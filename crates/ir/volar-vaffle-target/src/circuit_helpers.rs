// @reliability: experimental
// @ai: assisted
//! Extract GF(2) circuit helpers into ordinary VAFFLE functions.
//!
//! Producers can keep calling [`volar_lir::circuits`] in the parent block, or
//! intern one function per `(op, operand widths)` and `Value::Call` it. The
//! helper body always runs the circuit inline, so a multiply's internal add
//! does not become another call. The helper is appended after whatever
//! functions already exist; callers must reserve `funcs[0]` first so the
//! module entry is not a helper (`vaffle_ssa` does not thread a spill pointer
//! into the entry).

extern crate alloc;

use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;

use vaffle::{Block, BlockId, FuncBody, FuncDecl, FuncId, SigDecl, SigId, Terminator, Value, ValueId};
use volar_ir_common::{Constant, IrType, Node, PolyCoeffs, Stmt, Type, TypeId, TypeTable};
use volar_lir::circuits::{self, BitCircuitBuilder};

/// Whether a VAFFLE producer inlines circuit helpers or extracts them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CircuitHelperMode {
    /// Splice `bc_*` into the parent block.
    #[default]
    Inline,
    /// Intern a typed function and call it.
    Extract,
}

/// One integer circuit, keyed with its operand widths.
#[derive(Clone, Copy, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub enum HelperOp {
    Add,
    Sub,
    Mul,
    UDiv,
    SDiv,
    URem,
    SRem,
    Shl,
    LShr,
    AShr,
    And,
    Or,
    Xor,
    Not,
    Eq,
    Ne,
    Ult,
    Ule,
    Slt,
    Sle,
}

/// Cache key: the operation and each operand's bit width.
#[derive(Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
pub struct HelperKey {
    pub op: HelperOp,
    pub widths: Vec<usize>,
}

/// Bit width of a helper's single result.
pub fn helper_result_width(op: HelperOp, widths: &[usize]) -> usize {
    match op {
        HelperOp::Eq | HelperOp::Ne | HelperOp::Ult | HelperOp::Ule | HelperOp::Slt | HelperOp::Sle => {
            1
        }
        _ => widths[0],
    }
}

/// Intern `op` at `widths` into `funcs`, reusing `cache`.
///
/// The function has one entry parameter per operand and one return value.
/// Primitive `_8`/`_16`/`_32`/`_64`/`_128` are used when the width matches;
/// every other width is `Vec(n, Bit)`.
pub fn intern_circuit_helper(
    types: &mut TypeTable,
    sigs: &mut Vec<SigDecl>,
    funcs: &mut Vec<FuncDecl>,
    cache: &mut BTreeMap<HelperKey, FuncId>,
    op: HelperOp,
    widths: &[usize],
) -> FuncId {
    let key = HelperKey {
        op,
        widths: widths.to_vec(),
    };
    if let Some(&id) = cache.get(&key) {
        return id;
    }
    let param_tys: Vec<TypeId> = widths.iter().copied().map(|w| width_type(types, w)).collect();
    let result_ty = width_type(types, helper_result_width(op, widths));
    let sig_id = SigId(sigs.len());
    sigs.push(SigDecl {
        params: param_tys.clone(),
        results: vec![result_ty],
    });
    let body = build_helper_body(types, sig_id, op, widths, &param_tys, result_ty);
    let id = FuncId(funcs.len());
    funcs.push(FuncDecl::Body(body));
    cache.insert(key, id);
    id
}

/// Type id for an integer of `n` bits. Interns into `types`.
pub fn width_type(types: &mut TypeTable, n: usize) -> TypeId {
    match n {
        1 => types.bit(),
        8 => types.primitive(Type::_8),
        16 => types.primitive(Type::_16),
        32 => types.primitive(Type::_32),
        64 => types.primitive(Type::_64),
        128 => types.primitive(Type::_128),
        n => {
            let bit = types.bit();
            types.intern(IrType::Vec(n, bit))
        }
    }
}

fn build_helper_body(
    types: &mut TypeTable,
    sig: SigId,
    op: HelperOp,
    widths: &[usize],
    param_tys: &[TypeId],
    result_ty: TypeId,
) -> FuncBody {
    let bit_tid = types.bit();
    let mut b = HelperBuilder {
        values: Vec::new(),
        stmts: Vec::new(),
        params: Vec::new(),
        bit_tid,
    };
    let mut param_ids = Vec::new();
    for (idx, &ty) in param_tys.iter().enumerate() {
        let id = ValueId(b.values.len());
        b.values.push(Node::new(
            Value::Param {
                block: BlockId(0),
                ty,
                idx,
            },
            (),
            None,
        ));
        b.params.push((id, ty));
        param_ids.push(id);
    }
    let operands: Vec<Vec<ValueId>> = param_ids
        .iter()
        .zip(widths.iter())
        .map(|(&id, &width)| explode(&mut b, id, width))
        .collect();
    let result_bits = eval_op(&mut b, op, &operands);
    let result = if result_bits.len() == 1 {
        result_bits[0]
    } else {
        b.emit(Value::Op(Stmt::Merge {
            parts: result_bits,
            ty: result_ty,
        }))
    };
    FuncBody {
        sig,
        blocks: vec![Block {
            params: b.params,
            stmts: b.stmts,
            terminator: Terminator::Return {
                values: vec![result],
            },
        }],
        values: b.values,
        entry: BlockId(0),
    }
}

fn explode(b: &mut HelperBuilder, src: ValueId, n: usize) -> Vec<ValueId> {
    if n <= 1 {
        return vec![src];
    }
    (0..n)
        .map(|i| {
            b.emit(Value::Op(Stmt::Shuffle {
                result_bits: vec![(i as u8, src)],
                ty: b.bit_tid,
            }))
        })
        .collect()
}

fn eval_op(b: &mut HelperBuilder, op: HelperOp, operands: &[Vec<ValueId>]) -> Vec<ValueId> {
    let bit = |b: &mut HelperBuilder, op: HelperOp, operands: &[Vec<ValueId>]| -> ValueId {
        match op {
            HelperOp::Eq => circuits::bc_eq(b, &operands[0], &operands[1]),
            HelperOp::Ne => circuits::bc_ne(b, &operands[0], &operands[1]),
            HelperOp::Ult => circuits::bc_ult(b, &operands[0], &operands[1]),
            HelperOp::Ule => circuits::bc_ule(b, &operands[0], &operands[1]),
            HelperOp::Slt => circuits::bc_slt(b, &operands[0], &operands[1]),
            HelperOp::Sle => circuits::bc_sle(b, &operands[0], &operands[1]),
            _ => unreachable!("comparison helper"),
        }
    };
    match op {
        HelperOp::Add => circuits::bc_add(b, &operands[0], &operands[1], false),
        HelperOp::Sub => circuits::bc_sub(b, &operands[0], &operands[1]),
        HelperOp::Mul => circuits::bc_mul(b, &operands[0], &operands[1]),
        HelperOp::UDiv => circuits::bc_udiv(b, &operands[0], &operands[1]),
        HelperOp::SDiv => circuits::bc_sdiv(b, &operands[0], &operands[1]),
        HelperOp::URem => circuits::bc_urem(b, &operands[0], &operands[1]),
        HelperOp::SRem => circuits::bc_srem(b, &operands[0], &operands[1]),
        HelperOp::Shl => circuits::bc_shl(b, &operands[0], &operands[1]),
        HelperOp::LShr => circuits::bc_lshr(b, &operands[0], &operands[1]),
        HelperOp::AShr => circuits::bc_ashr(b, &operands[0], &operands[1]),
        HelperOp::And => circuits::bc_and_vec(b, &operands[0], &operands[1]),
        HelperOp::Or => circuits::bc_or_vec(b, &operands[0], &operands[1]),
        HelperOp::Xor => circuits::bc_xor_vec(b, &operands[0], &operands[1]),
        HelperOp::Not => circuits::bc_not_vec(b, &operands[0]),
        HelperOp::Eq
        | HelperOp::Ne
        | HelperOp::Ult
        | HelperOp::Ule
        | HelperOp::Slt
        | HelperOp::Sle => vec![bit(b, op, operands)],
    }
}

struct HelperBuilder {
    values: Vec<Node<Value, ()>>,
    stmts: Vec<ValueId>,
    params: Vec<(ValueId, TypeId)>,
    bit_tid: TypeId,
}

impl HelperBuilder {
    fn emit(&mut self, val: Value) -> ValueId {
        let id = ValueId(self.values.len());
        self.values.push(Node::new(val, (), None));
        self.stmts.push(id);
        id
    }
}

impl BitCircuitBuilder for HelperBuilder {
    type Bit = ValueId;

    fn bc_const(&mut self, val: bool) -> ValueId {
        self.emit(Value::Op(Stmt::Const(
            Constant {
                hi: 0,
                lo: val as u128,
            },
            self.bit_tid,
        )))
    }

    fn bc_poly(&mut self, coeffs: PolyCoeffs<ValueId>, constant: u128) -> ValueId {
        self.emit(Value::Op(Stmt::Poly {
            ty: self.bit_tid,
            coeffs,
            constant: Constant {
                hi: 0,
                lo: constant,
            },
        }))
    }
}
