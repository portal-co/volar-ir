//! Lower Volar IR into one native field.
//!
//! `Bit` is the Boolar sink: [`lower_ir_to_boolar`](crate::lower_ir_to_boolar).
//! Any other native field (`PrimeField` or `ExtField`) is a Volar IR sink.
//! Each wire is one SSA value of that field. Addition and multiplication are
//! `Poly` statements, so evaluation of the lowered IR is the native operation.
//!
//! Values of the native field stay one wire. `Vec`, `Tuple`, and `ExtField`
//! unroll. A foreign prime is Solinas arithmetic on boolean digits. A bit
//! embedded in an odd prime uses `a+b-2ab` for XOR and multiplication for AND.

use alloc::collections::BTreeMap;
use alloc::vec;
use alloc::vec::Vec;

use volar_ir::boolar::BIrBlocks;
use volar_ir::ir::{
    IRBlock, IRBlockTargetId, IRBlocks, IRBranchTarget, IRStmt, IRTerminator, IRTypes, IRVarId,
};
use volar_ir_common::{
    BitRing, Constant, FieldSink, IrType, Node, PolyCoeffs, PrimeSpec, Stmt, Type, TypeId,
    embed_and, embed_xor, eval_prime_poly, monomial_product, mul_is_idempotent, prime_spec,
    repetition_residue,
};

use crate::lower_ir_to_boolar::lower_ir_to_boolar;

/// Result of [`lower_to_native`].
#[derive(Clone, Debug)]
pub enum NativeLowering<P: Clone> {
    /// Native field is `Bit`.
    Boolar(BIrBlocks<P>),
    /// Native field is a prime or an extension. The type table is the source
    /// table, which already contains `native`.
    Ir(IRBlocks<P>, IRTypes),
}

/// Lower `blocks` so every data value is wires of `native`.
///
/// # Panics
///
/// Panics when `native` is not `Bit`, `PrimeField`, or `ExtField`, or when a
/// statement has no lowering into that field (storage, oracles, and actions
/// stay on the Boolar sink).
pub fn lower_to_native<P: Clone>(
    blocks: &IRBlocks<P>,
    types: &IRTypes,
    native: TypeId,
) -> NativeLowering<P> {
    match types.0.get(native.0 as usize) {
        Some(IrType::Primitive(Type::Bit)) => {
            NativeLowering::Boolar(lower_ir_to_boolar(blocks, types))
        }
        Some(IrType::PrimeField { .. }) | Some(IrType::ExtField { .. }) => {
            let lowered = lower_blocks(blocks, types, native);
            NativeLowering::Ir(lowered, types.clone())
        }
        _ => panic!("lower_to_native: native type must be Bit, PrimeField, or ExtField"),
    }
}

fn lower_blocks<P: Clone>(blocks: &IRBlocks<P>, types: &IRTypes, native: TypeId) -> IRBlocks<P> {
    IRBlocks {
        oracles: blocks.oracles.clone(),
        actions: blocks.actions.clone(),
        rngs: blocks.rngs.clone(),
        pre_init: blocks.pre_init.clone(),
        blocks: blocks
            .blocks
            .iter()
            .map(|block| lower_block(block, types, native))
            .collect(),
    }
}

fn lower_block<P: Clone>(block: &IRBlock<P>, types: &IRTypes, native: TypeId) -> IRBlock<P> {
    let mut params = Vec::new();
    let mut var_wires: BTreeMap<u32, Vec<IRVarId>> = BTreeMap::new();
    let mut var_tys: BTreeMap<u32, TypeId> = BTreeMap::new();
    for (index, &ty) in block.params.iter().enumerate() {
        let mut wires = Vec::new();
        for _ in 0..native_wires(ty, native, types) {
            wires.push(IRVarId(params.len() as u32));
            params.push(native);
        }
        var_wires.insert(index as u32, wires);
        var_tys.insert(index as u32, ty);
    }
    let spec = prime_spec(native, types);
    let mut sink = IrSink {
        nparams: params.len() as u32,
        stmts: Vec::new(),
        native,
        characteristic_two: spec.is_none(),
        spec,
        prov: None,
        zero: None,
        one: None,
    };
    for (index, node) in block.stmts.iter().enumerate() {
        let src = (block.params.len() + index) as u32;
        sink.prov = Some(node.prov.clone());
        if let Some(ty) = stmt_ty(&node.kind) {
            var_tys.insert(src, ty);
        }
        let wires = lower_stmt(&node.kind, &var_wires, &var_tys, native, &mut sink, types);
        var_wires.insert(src, wires);
    }
    let terminator = lower_terminator(&block.terminator, &var_wires);
    IRBlock {
        params,
        stmts: sink.stmts,
        terminator,
    }
}

fn lower_stmt<P: Clone>(
    stmt: &IRStmt,
    var_wires: &BTreeMap<u32, Vec<IRVarId>>,
    var_tys: &BTreeMap<u32, TypeId>,
    native: TypeId,
    sink: &mut IrSink<P>,
    types: &IRTypes,
) -> Vec<IRVarId> {
    match stmt {
        IRStmt::Const(c, ty) => const_wires(c, *ty, native, sink, types),
        IRStmt::Transmute { src, dst_ty, .. } => {
            let wires = var_wires[&src.0].clone();
            let expect = native_wires(*dst_ty, native, types);
            assert_eq!(
                wires.len(),
                expect,
                "lower_to_native: transmute wire count does not match the destination"
            );
            wires
        }
        IRStmt::Poly {
            ty,
            coeffs,
            constant,
        } => lower_poly(
            *ty, coeffs, constant, var_wires, var_tys, native, sink, types,
        ),
        IRStmt::Rol { src, ty, n } => rotate(&var_wires[&src.0], *ty, *n, true, types),
        IRStmt::Ror { src, ty, n } => rotate(&var_wires[&src.0], *ty, *n, false, types),
        IRStmt::Merge { parts, ty } => {
            let wires: Vec<IRVarId> = parts
                .iter()
                .flat_map(|part| var_wires[&part.0].iter().copied())
                .collect();
            assert_eq!(
                wires.len(),
                native_wires(*ty, native, types),
                "lower_to_native: merge wire count does not match the result"
            );
            wires
        }
        IRStmt::Splat { src, ty } => {
            let bit = var_wires[&src.0][0];
            let count = native_wires(*ty, native, types);
            vec![bit; count]
        }
        IRStmt::Shuffle { result_bits, .. } => result_bits
            .iter()
            .map(|(bit, var)| var_wires[&var.0][*bit as usize])
            .collect(),
        _ => panic!(
            "lower_to_native: unhandled statement — storage, oracles, and actions lower through the Boolar sink"
        ),
    }
}

fn lower_terminator(
    term: &IRTerminator,
    var_wires: &BTreeMap<u32, Vec<IRVarId>>,
) -> IRTerminator {
    match term {
        IRTerminator::Jmp { target } => IRTerminator::Jmp {
            target: flatten_target(target, var_wires),
        },
        IRTerminator::JumpCond {
            condition,
            then_target,
            else_target,
        } => {
            let cond = var_wires[&condition.0][0];
            IRTerminator::JumpCond {
                condition: cond,
                then_target: flatten_target(then_target, var_wires),
                else_target: flatten_target(else_target, var_wires),
            }
        }
        _ => panic!("lower_to_native: JumpTable is not a native-field terminator"),
    }
}

fn flatten_target(
    target: &IRBranchTarget<IRVarId>,
    var_wires: &BTreeMap<u32, Vec<IRVarId>>,
) -> IRBranchTarget<IRVarId> {
    let dest = match &target.dest {
        IRBlockTargetId::Block(id) => IRBlockTargetId::Block(*id),
        IRBlockTargetId::Return => IRBlockTargetId::Return,
        IRBlockTargetId::Dyn(_) => {
            panic!("lower_to_native: Dyn jump targets are not representable")
        }
        _ => panic!("lower_to_native: unhandled jump target"),
    };
    IRBranchTarget {
        dest,
        args: target
            .args
            .iter()
            .flat_map(|arg| var_wires[&arg.0].iter().copied())
            .collect(),
        reentry: target.reentry.clone(),
    }
}

fn const_wires<P: Clone>(
    constant: &Constant,
    ty: TypeId,
    native: TypeId,
    sink: &mut IrSink<P>,
    types: &IRTypes,
) -> Vec<IRVarId> {
    if same_type(ty, native, types) {
        return vec![sink.const_value(*constant)];
    }
    let width = types
        .value_bit_width(ty)
        .unwrap_or_else(|| panic!("lower_to_native: constant has no bit width"));
    let bits = constant_bits(constant, width);
    const_from_bits(ty, &bits, native, sink, types)
}

fn const_from_bits<P: Clone>(
    ty: TypeId,
    bits: &[bool],
    native: TypeId,
    sink: &mut IrSink<P>,
    types: &IRTypes,
) -> Vec<IRVarId> {
    if same_type(ty, native, types) {
        return vec![sink.const_value(bits_to_constant(bits))];
    }
    match types.0.get(ty.0 as usize) {
        Some(IrType::Vec(count, elem)) => {
            let lane = types.value_bit_width(*elem).unwrap_or(0);
            let mut out = Vec::new();
            for index in 0..*count {
                let start = index * lane;
                out.extend(const_from_bits(
                    *elem,
                    &bits[start..start + lane],
                    native,
                    sink,
                    types,
                ));
            }
            out
        }
        Some(IrType::Tuple(parts)) => {
            let mut out = Vec::new();
            let mut offset = 0usize;
            for &part in parts {
                let width = types.value_bit_width(part).unwrap_or(0);
                out.extend(const_from_bits(
                    part,
                    &bits[offset..offset + width],
                    native,
                    sink,
                    types,
                ));
                offset += width;
            }
            out
        }
        Some(IrType::ExtField { wrapped, degree, .. }) => {
            let coeff = types.value_bit_width(*wrapped).unwrap_or(0);
            let mut out = Vec::new();
            for index in 0..*degree as usize {
                let start = index * coeff;
                out.extend(const_from_bits(
                    *wrapped,
                    &bits[start..start + coeff],
                    native,
                    sink,
                    types,
                ));
            }
            out
        }
        _ => bits.iter().map(|bit| sink.bit_wire(*bit)).collect(),
    }
}

fn lower_poly<P: Clone>(
    ty: TypeId,
    coeffs: &PolyCoeffs<IRVarId>,
    constant: &Constant,
    var_wires: &BTreeMap<u32, Vec<IRVarId>>,
    var_tys: &BTreeMap<u32, TypeId>,
    native: TypeId,
    sink: &mut IrSink<P>,
    types: &IRTypes,
) -> Vec<IRVarId> {
    let width = types.value_bit_width(ty).unwrap_or_else(|| {
        panic!("lower_to_native: polynomial output has no bit width")
    });
    let bits = constant_bits(constant, width);
    let mut terms = Vec::new();
    for (mono, coeff) in coeffs.iter() {
        let mut factors = Vec::with_capacity(mono.len());
        for var in mono {
            let factor_ty = var_tys.get(&var.0).copied().unwrap_or_else(|| {
                panic!("lower_to_native: factor v{} has no type", var.0)
            });
            let wires = var_wires.get(&var.0).cloned().unwrap_or_else(|| {
                panic!("lower_to_native: factor v{} has no wires", var.0)
            });
            factors.push((factor_ty, wires));
        }
        terms.push((factors, *coeff));
    }
    lower_poly_rec(ty, &bits, &terms, native, sink, types)
}

fn lower_poly_rec<P: Clone>(
    ty: TypeId,
    const_bits: &[bool],
    terms: &[(Vec<(TypeId, Vec<IRVarId>)>, u8)],
    native: TypeId,
    sink: &mut IrSink<P>,
    types: &IRTypes,
) -> Vec<IRVarId> {
    match types.0.get(ty.0 as usize) {
        Some(IrType::Vec(count, elem)) => {
            let lane_bits = types.value_bit_width(*elem).unwrap_or(0);
            let lane_wires = native_wires(*elem, native, types);
            let mut out = Vec::new();
            for lane in 0..*count {
                let start = lane * lane_bits;
                let mut lane_terms = Vec::with_capacity(terms.len());
                for (factors, coeff) in terms {
                    let mut lane_factors = Vec::with_capacity(factors.len());
                    for (factor_ty, wires) in factors {
                        if same_type(*factor_ty, ty, types) {
                            let at = lane * lane_wires;
                            lane_factors.push((
                                *elem,
                                wires[at..at + lane_wires].to_vec(),
                            ));
                        } else if same_type(*factor_ty, *elem, types) || is_bit(*factor_ty, types)
                        {
                            lane_factors.push((*factor_ty, wires.clone()));
                        } else {
                            panic!("lower_to_native: vector factor type does not match the lane");
                        }
                    }
                    lane_terms.push((lane_factors, *coeff));
                }
                out.extend(lower_poly_rec(
                    *elem,
                    &const_bits[start..start + lane_bits],
                    &lane_terms,
                    native,
                    sink,
                    types,
                ));
            }
            out
        }
        Some(IrType::Tuple(parts)) => {
            let mut out = Vec::new();
            let mut bit_at = 0usize;
            let mut wire_at = 0usize;
            for &part in parts {
                let part_bits = types.value_bit_width(part).unwrap_or(0);
                let part_wires = native_wires(part, native, types);
                let mut part_terms = Vec::with_capacity(terms.len());
                for (factors, coeff) in terms {
                    let mut part_factors = Vec::new();
                    for (factor_ty, wires) in factors {
                        if same_type(*factor_ty, ty, types) {
                            part_factors.push((
                                part,
                                wires[wire_at..wire_at + part_wires].to_vec(),
                            ));
                        } else if same_type(*factor_ty, part, types) || is_bit(*factor_ty, types)
                        {
                            part_factors.push((*factor_ty, wires.clone()));
                        } else {
                            panic!("lower_to_native: tuple factor type does not match this part");
                        }
                    }
                    part_terms.push((part_factors, *coeff));
                }
                out.extend(lower_poly_rec(
                    part,
                    &const_bits[bit_at..bit_at + part_bits],
                    &part_terms,
                    native,
                    sink,
                    types,
                ));
                bit_at += part_bits;
                wire_at += part_wires;
            }
            out
        }
        _ if same_type(ty, native, types) => {
            vec![field_wire(ty, const_bits, terms, sink, types)]
        }
        Some(IrType::PrimeField { .. }) => {
            let constant_wires: Vec<IRVarId> = const_bits
                .iter()
                .map(|bit| sink.bit_wire(*bit))
                .collect();
            eval_prime_poly(ty, &constant_wires, terms, types, sink)
        }
        _ if mul_is_idempotent(ty, types) => (0..const_bits.len())
            .map(|lane| gf2_lane(lane, const_bits[lane], terms, sink))
            .collect(),
        Some(IrType::ExtField { .. }) => ext_wires(ty, const_bits, terms, sink, types),
        _ => panic!("lower_to_native: polynomial output type cannot be lowered"),
    }
}

fn field_wire<P: Clone>(
    ty: TypeId,
    const_bits: &[bool],
    terms: &[(Vec<(TypeId, Vec<IRVarId>)>, u8)],
    sink: &mut IrSink<P>,
    types: &IRTypes,
) -> IRVarId {
    let spec = prime_spec(ty, types);
    let mut acc = {
        let constant = bits_to_constant(const_bits);
        if constant.hi == 0 && constant.lo == 0 {
            None
        } else {
            Some(sink.const_value(constant))
        }
    };
    for (factors, coeff) in terms {
        let times = match &spec {
            Some(spec) => repetition_residue(*coeff, spec),
            None => coeff & 1,
        };
        if times == 0 {
            continue;
        }
        let mut prod = None;
        for (_, wires) in factors {
            let wire = *wires.first().unwrap_or_else(|| {
                panic!("lower_to_native: native-field factor has no wire")
            });
            prod = Some(match prod {
                None => wire,
                Some(prev) => sink.mul(prev, wire),
            });
        }
        let prod = prod.unwrap_or_else(|| sink.one());
        for _ in 0..times {
            acc = Some(match acc {
                None => prod,
                Some(prev) => sink.add(prev, prod),
            });
        }
    }
    acc.unwrap_or_else(|| sink.zero())
}

fn gf2_lane<P: Clone>(
    lane: usize,
    const_bit: bool,
    terms: &[(Vec<(TypeId, Vec<IRVarId>)>, u8)],
    sink: &mut IrSink<P>,
) -> IRVarId {
    let mut acc = sink.bit_wire(const_bit);
    for (factors, coeff) in terms {
        if coeff & 1 == 0 {
            continue;
        }
        let mut prod = sink.one();
        let mut is_zero = false;
        for (_, wires) in factors {
            let bit = if wires.len() == 1 {
                wires[0]
            } else {
                match wires.get(lane) {
                    Some(bit) => *bit,
                    None => {
                        is_zero = true;
                        break;
                    }
                }
            };
            prod = embed_and(sink, prod, bit);
        }
        if !is_zero {
            acc = embed_xor(sink, acc, prod);
        }
    }
    acc
}

fn ext_wires<P: Clone>(
    ty: TypeId,
    const_bits: &[bool],
    terms: &[(Vec<(TypeId, Vec<IRVarId>)>, u8)],
    sink: &mut IrSink<P>,
    types: &IRTypes,
) -> Vec<IRVarId> {
    let mut acc: Vec<IRVarId> = const_bits.iter().map(|bit| sink.bit_wire(*bit)).collect();
    for (factors, coeff) in terms {
        if coeff & 1 == 0 {
            continue;
        }
        let product = {
            let refs: Vec<(TypeId, &[IRVarId])> = factors
                .iter()
                .map(|(factor_ty, wires)| (*factor_ty, wires.as_slice()))
                .collect();
            let mut ring = EmbedRing { sink };
            monomial_product(ty, &refs, types, &mut ring)
        };
        for (index, wire) in product.into_iter().enumerate() {
            if index < acc.len() {
                let prev = acc[index];
                acc[index] = embed_xor(sink, prev, wire);
            }
        }
    }
    acc
}

fn rotate(
    wires: &[IRVarId],
    ty: TypeId,
    n: usize,
    left: bool,
    types: &IRTypes,
) -> Vec<IRVarId> {
    let bits = types.value_bit_width(ty).unwrap_or(wires.len());
    assert_eq!(
        wires.len(),
        bits,
        "lower_to_native: rotate applies to boolean digits"
    );
    let width = wires.len().max(1);
    let n = if wires.is_empty() { 0 } else { n % width };
    (0..wires.len())
        .map(|index| {
            let src = if left {
                (index + width - n) % width
            } else {
                (index + n) % width
            };
            wires[src]
        })
        .collect()
}

fn native_wires(ty: TypeId, native: TypeId, types: &IRTypes) -> usize {
    if same_type(ty, native, types) {
        return 1;
    }
    match types.0.get(ty.0 as usize) {
        Some(IrType::Primitive(prim)) => match prim {
            Type::Bit => 1,
            Type::_8 => 8,
            Type::_16 => 16,
            Type::_32 => 32,
            Type::_64 => 64,
            Type::_128 => 128,
            Type::_256 => 256,
            _ => panic!("lower_to_native: primitive has no wire count"),
        },
        Some(IrType::PrimeField { k, .. }) => *k as usize,
        Some(IrType::Vec(count, elem)) => count * native_wires(*elem, native, types),
        Some(IrType::Tuple(parts)) => parts
            .iter()
            .map(|part| native_wires(*part, native, types))
            .sum(),
        Some(IrType::ExtField {
            wrapped, degree, ..
        }) => *degree as usize * native_wires(*wrapped, native, types),
        _ => panic!("lower_to_native: control types have no data wires"),
    }
}

fn same_type(lhs: TypeId, rhs: TypeId, types: &IRTypes) -> bool {
    lhs == rhs
        || types
            .0
            .get(lhs.0 as usize)
            .zip(types.0.get(rhs.0 as usize))
            .is_some_and(|(a, b)| a == b)
}

fn is_bit(ty: TypeId, types: &IRTypes) -> bool {
    matches!(
        types.0.get(ty.0 as usize),
        Some(IrType::Primitive(Type::Bit))
    )
}

fn stmt_ty(stmt: &IRStmt) -> Option<TypeId> {
    match stmt {
        IRStmt::Const(_, ty)
        | IRStmt::Poly { ty, .. }
        | IRStmt::Rol { ty, .. }
        | IRStmt::Ror { ty, .. }
        | IRStmt::Merge { ty, .. }
        | IRStmt::Splat { ty, .. }
        | IRStmt::Shuffle { ty, .. }
        | IRStmt::OracleOutput { ty, .. }
        | IRStmt::ActionOutput { ty, .. }
        | IRStmt::Rng { ty, .. }
        | IRStmt::StorageRead { ty, .. } => Some(*ty),
        IRStmt::Transmute { dst_ty, .. } => Some(*dst_ty),
        _ => None,
    }
}

fn constant_bits(constant: &Constant, width: usize) -> Vec<bool> {
    (0..width)
        .map(|bit| {
            if bit < 128 {
                (constant.lo >> bit) & 1 == 1
            } else if bit < 256 {
                (constant.hi >> (bit - 128)) & 1 == 1
            } else {
                false
            }
        })
        .collect()
}

fn bits_to_constant(bits: &[bool]) -> Constant {
    let mut lo = 0u128;
    let mut hi = 0u128;
    for (bit, set) in bits.iter().enumerate() {
        if !*set {
            continue;
        }
        if bit < 128 {
            lo |= 1u128 << bit;
        } else if bit < 256 {
            hi |= 1u128 << (bit - 128);
        }
    }
    Constant { hi, lo }
}

struct IrSink<P: Clone> {
    nparams: u32,
    stmts: Vec<Node<IRStmt, P>>,
    native: TypeId,
    spec: Option<PrimeSpec>,
    characteristic_two: bool,
    prov: Option<P>,
    zero: Option<IRVarId>,
    one: Option<IRVarId>,
}

impl<P: Clone> IrSink<P> {
    fn emit(&mut self, stmt: IRStmt) -> IRVarId {
        let id = IRVarId(self.nparams + self.stmts.len() as u32);
        let prov = self
            .prov
            .clone()
            .expect("lower_to_native: statement emitted without provenance");
        self.stmts.push(Node::new(stmt, prov, None));
        id
    }

    fn bit_wire(&mut self, bit: bool) -> IRVarId {
        if bit { self.one() } else { self.zero() }
    }

    fn const_value(&mut self, constant: Constant) -> IRVarId {
        if constant.hi == 0 && constant.lo == 0 {
            self.zero()
        } else if constant.hi == 0 && constant.lo == 1 {
            self.one()
        } else {
            self.emit(Stmt::Const(constant, self.native))
        }
    }

    fn poly(&mut self, coeffs: PolyCoeffs<IRVarId>, constant: u128) -> IRVarId {
        self.emit(Stmt::Poly {
            ty: self.native,
            coeffs,
            constant: Constant {
                hi: 0,
                lo: constant,
            },
        })
    }

    fn insert_term(&self, coeffs: &mut PolyCoeffs<IRVarId>, mut key: Vec<IRVarId>, add: u8) {
        key.sort();
        let prev = coeffs.get(&key).copied().unwrap_or(0);
        let sum = self.combine(prev, add);
        if sum == 0 {
            coeffs.remove(&key);
        } else {
            coeffs.insert(key, sum);
        }
    }

    fn combine(&self, prev: u8, add: u8) -> u8 {
        if self.characteristic_two {
            return prev ^ add;
        }
        let spec = self
            .spec
            .as_ref()
            .expect("lower_to_native: odd prime sink");
        let sum = prev as u16 + add as u16;
        if spec.k > 8 {
            assert!(sum <= 255, "lower_to_native: repetition does not fit in u8");
            return sum as u8;
        }
        let n = spec.n.first().copied().unwrap_or(0) as u16;
        let prime = (1u16 << spec.k) - n;
        (sum % prime) as u8
    }
}

impl<P: Clone> FieldSink for IrSink<P> {
    type Wire = IRVarId;

    fn zero(&mut self) -> IRVarId {
        if let Some(zero) = self.zero {
            return zero;
        }
        let zero = self.emit(Stmt::Const(
            Constant { hi: 0, lo: 0 },
            self.native,
        ));
        self.zero = Some(zero);
        zero
    }

    fn one(&mut self) -> IRVarId {
        if let Some(one) = self.one {
            return one;
        }
        let one = self.emit(Stmt::Const(
            Constant { hi: 0, lo: 1 },
            self.native,
        ));
        self.one = Some(one);
        one
    }

    fn add(&mut self, lhs: IRVarId, rhs: IRVarId) -> IRVarId {
        let mut coeffs = PolyCoeffs::new();
        self.insert_term(&mut coeffs, vec![lhs], 1);
        self.insert_term(&mut coeffs, vec![rhs], 1);
        if coeffs.is_empty() {
            self.zero()
        } else {
            self.poly(coeffs, 0)
        }
    }

    fn sub(&mut self, lhs: IRVarId, rhs: IRVarId) -> IRVarId {
        if self.characteristic_two {
            return self.add(lhs, rhs);
        }
        let spec = self
            .spec
            .as_ref()
            .expect("lower_to_native: odd prime sink")
            .clone();
        let minus = modulus_minus_one(&spec);
        if minus < 256 {
            let mut coeffs = PolyCoeffs::new();
            self.insert_term(&mut coeffs, vec![lhs], 1);
            self.insert_term(&mut coeffs, vec![rhs], minus as u8);
            self.poly(coeffs, 0)
        } else {
            let scale = self.const_value(Constant { hi: 0, lo: minus });
            let scaled = self.mul(scale, rhs);
            self.add(lhs, scaled)
        }
    }

    fn mul(&mut self, lhs: IRVarId, rhs: IRVarId) -> IRVarId {
        let mut key = vec![lhs, rhs];
        key.sort();
        let mut coeffs = PolyCoeffs::new();
        coeffs.insert(key, 1);
        self.poly(coeffs, 0)
    }

    fn char_two(&self) -> bool {
        self.characteristic_two
    }
}

fn modulus_minus_one(spec: &PrimeSpec) -> u128 {
    let n = spec.n.first().copied().unwrap_or(0) as u128;
    if spec.k < 128 {
        (1u128 << spec.k) - n - 1
    } else if spec.k == 128 {
        u128::MAX - n
    } else {
        panic!("lower_to_native: p-1 does not fit in one u128 limb")
    }
}

struct EmbedRing<'a, P: Clone> {
    sink: &'a mut IrSink<P>,
}

impl<P: Clone> BitRing for EmbedRing<'_, P> {
    type Bit = IRVarId;

    fn bit_and(&mut self, lhs: IRVarId, rhs: IRVarId) -> IRVarId {
        embed_and(self.sink, lhs, rhs)
    }

    fn bit_xor(&mut self, lhs: IRVarId, rhs: IRVarId) -> IRVarId {
        embed_xor(self.sink, lhs, rhs)
    }

    fn bit_zero(&mut self) -> IRVarId {
        self.sink.zero()
    }

    fn bit_one(&mut self) -> IRVarId {
        self.sink.one()
    }
}
