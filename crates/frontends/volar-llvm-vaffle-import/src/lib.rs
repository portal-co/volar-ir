//! Selective, structural LLVM IR import into VAFFLE.
//!
//! Unlike `volar-llvm-import-core`'s execution-mode walker (which inlines
//! every direct call by concretely executing it), this is a **structural**
//! translator: it walks LLVM basic blocks in declaration order and 1:1
//! translates each block/instruction/terminator into VAFFLE's own
//! `Block`/`Value`/`Terminator`, producing one `vaffle::Func` per LLVM
//! function reached — calls are preserved as `Value::Call`/
//! `Terminator::ReturnCall`, never inlined, matching VAFFLE's own
//! call-preserving design.
//!
//! Every integer value is represented the same way `VaffleTarget`
//! represents them: as a `Vec<ValueId>` of individual `Bit`-typed VAFFLE
//! values, LSB first ("bits"). Arithmetic is built via
//! [`volar_lir::circuits::BitCircuitBuilder`] — the same generic GF(2)
//! bit-circuit toolkit `VaffleTarget`/`VolarIrTarget` already use — so this
//! crate only implements the two required primitives (`bc_const`/`bc_poly`)
//! and gets add/sub/mul/div/shifts/comparisons/select for free.
//!
//! # Scope (v1)
//!
//! Supported: integer arithmetic (`add`/`sub`/`mul`/`udiv`/`sdiv`), bitwise
//! ops, shifts, `icmp` (all predicates), `select`, `trunc`/`zext`/`sext`,
//! `phi` (→ block params), direct `call`, `br`/conditional `br`/`switch`/`ret`,
//! and loads/stores through **two** distinct pointer provenances, resolved at
//! import time and never conflated:
//!
//! - a global variable (optionally behind a constant-index `getelementptr`)
//!   — each distinct LLVM global gets one `StorageId`, allocated the first
//!   time it's referenced and reused afterward, decided via
//!   [`volar_llvm_constchain`]'s constant-chain walker (the generic form of
//!   "the entire operation must be a constant load": the *storage identity*
//!   must resolve to a literal global at import time, even though the value
//!   read/written through it may be symbolic);
//! - a constant-size `alloca` (scalar integer element type only) — or a
//!   single constant-index `getelementptr` off one — tracked as a
//!   compile-time-constant `StorageId::STACK` address (`Value::StackAlloc`
//!   / `PtrLoad` / `PtrStore` / `PtrOffset`, one bit-level `StorageRead`/
//!   `StorageWrite` per bit, mirroring `VaffleTarget`'s own convention; see
//!   `docs/llvm-alloca.md`).
//!
//! Not yet supported (hard error): floats, vectors, aggregates, atomics,
//! `indirectbr`/`blockaddress` (VAFFLE's `Value::BlockAddr` already models
//! this — ingest is deferred), tail calls as a distinct form (currently
//! lowered the same as an ordinary `call`), any pointer arithmetic whose
//! base doesn't resolve to a literal global or a tracked stack pointer,
//! `alloca` with a symbolic count or a non-integer element type, a
//! multi-index or symbolic-index `getelementptr` into a stack pointer, and
//! phi/select of a pointer-typed SSA value (merging two distinct pointers
//! at a control-flow join). `switch` is supported: sparse case keys become
//! a dense positional `Terminator::Table` via the same `bc_eq` /
//! `bc_select_vec` selector cascade `VaffleTarget::switch` uses.

use std::collections::HashMap;

use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock as LlvmBlock;
use inkwell::llvm_sys::core::LLVMGetSwitchCaseValue;
use inkwell::module::Module as LlvmModule;
use inkwell::values::{
    AnyValue, AnyValueEnum, AsValueRef, BasicValueEnum, CallSiteValue, FunctionValue,
    InstructionOpcode, InstructionValue, IntValue, PhiValue, PointerValue,
};

use vaffle::{
    Block, BlockId, FuncBody, FuncDecl, FuncId, Module, SigDecl, SigId, Target, Terminator, Value,
    ValueId,
};
use volar_ir_common::{
    Constant, IrType, Node, Stmt, StorageAllocator, StorageId, Type, TypeId, TypeTable,
};
use volar_lir::circuits::{self, BitCircuitBuilder};
use volar_llvm_constchain::{ConstChainError, global_from_pointer, strip_pointer};

/// A structural-import failure.
#[derive(Debug)]
pub enum ImportError {
    Unsupported(String),
    ConstChain(ConstChainError),
}

impl core::fmt::Display for ImportError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ImportError::Unsupported(msg) => write!(f, "unsupported LLVM construct: {msg}"),
            ImportError::ConstChain(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for ImportError {}

impl From<ConstChainError> for ImportError {
    fn from(e: ConstChainError) -> Self {
        ImportError::ConstChain(e)
    }
}

type IResult<T> = Result<T, ImportError>;

/// Width of a stack pointer / alloca address, matching `VaffleTarget`'s own
/// `PTR_BITS` convention (and this crate's existing pointer-param fallback
/// in [`llvm_bit_width`]).
const PTR_BITS: usize = 32;

/// Bits needed to represent every integer in `0..=v` (at least 1). Matches
/// `VaffleTarget::switch`'s dense positional selector width.
fn bits_for_max_value(v: usize) -> usize {
    if v == 0 {
        1
    } else {
        (usize::BITS - v.leading_zeros()) as usize
    }
}

/// Import every reachable function transitively called from `entries` (by
/// LLVM name) into a fresh `vaffle::Module`. Each entry (and every function
/// it directly or indirectly calls) becomes its own `vaffle::Func`; direct
/// calls are preserved, never inlined.
pub fn import_module<'ctx>(llvm_module: &LlvmModule<'ctx>, entries: &[&str]) -> IResult<Module> {
    let mut importer = Importer::new();
    let mut worklist: Vec<FunctionValue<'ctx>> = Vec::new();
    for &name in entries {
        let f = llvm_module.get_function(name).ok_or_else(|| {
            ImportError::Unsupported(format!("entry function `{name}` does not exist"))
        })?;
        importer.func_id(f);
        worklist.push(f);
    }
    let mut done: std::collections::HashSet<PointerValue<'ctx>> = Default::default();
    while let Some(f) = worklist.pop() {
        let key = f.as_global_value().as_pointer_value();
        if done.contains(&key) {
            continue;
        }
        done.insert(key);
        let called = importer.import_function(f)?;
        worklist.extend(called);
    }
    Ok(importer.finish())
}

/// Structural import followed by [`volar_ir_opt::inline_vaffle::inline_vaffle_everything`].
///
/// [`import_module`] itself stays call-preserving. This wrapper splices every
/// non-recursive intra-module call (including tail calls) so fewer calls
/// reach VAFFLE-to-IR's on-stack convention. Recursion and leftover
/// Body-to-Body calls fail closed.
pub fn import_module_inlined<'ctx>(
    llvm_module: &LlvmModule<'ctx>,
    entries: &[&str],
) -> IResult<Module> {
    let mut module = import_module(llvm_module, entries)?;
    let ids: Vec<FuncId> = entries
        .iter()
        .map(|name| {
            module.exports.get(*name).copied().ok_or_else(|| {
                ImportError::Unsupported(format!(
                    "entry function `{name}` was not exported after import"
                ))
            })
        })
        .collect::<IResult<Vec<FuncId>>>()?;
    volar_ir_opt::inline_vaffle::inline_vaffle_everything(&mut module, &ids).map_err(|e| {
        ImportError::Unsupported(e.to_string())
    })?;
    Ok(module)
}

/// One VAFFLE bit-typed value per LLVM bit, LSB first — mirrors
/// `VaffleTarget::VaffleValue.bits`.
type Bits = Vec<ValueId>;

struct Importer<'ctx> {
    types: TypeTable,
    funcs: Vec<FuncDecl>,
    sigs: Vec<SigDecl>,
    exports: std::collections::BTreeMap<String, FuncId>,
    func_ids: HashMap<PointerValue<'ctx>, FuncId>,
    storage_for_global: HashMap<PointerValue<'ctx>, StorageId>,
    storage_alloc: StorageAllocator,
    bit_tid: TypeId,
    byte_tid: TypeId,
}

impl<'ctx> Importer<'ctx> {
    fn new() -> Self {
        let mut types = TypeTable::new();
        let bit_tid = types.bit();
        let byte_tid = types.primitive(Type::_8);
        Importer {
            types,
            funcs: Vec::new(),
            sigs: Vec::new(),
            exports: Default::default(),
            func_ids: HashMap::new(),
            storage_for_global: HashMap::new(),
            // Start well above any reserved range; this importer never
            // touches StorageId::STACK/VIRT_*/memory(_) itself.
            storage_alloc: StorageAllocator::new(64),
            bit_tid,
            byte_tid,
        }
    }

    fn finish(self) -> Module {
        Module {
            types: self.types,
            oracles: Vec::new(),
            actions: Vec::new(),
            funcs: self.funcs,
            sigs: self.sigs,
            exports: self.exports,
            pre_init: Vec::new(),
        }
    }

    fn func_id(&mut self, f: FunctionValue<'ctx>) -> FuncId {
        let key = f.as_global_value().as_pointer_value();
        if let Some(&id) = self.func_ids.get(&key) {
            return id;
        }
        let id = FuncId(self.funcs.len());
        // Reserve a placeholder; `import_function` overwrites it once the
        // body (or import declaration) is known. Reserving up front lets
        // mutually/self-recursive call sites resolve a stable FuncId before
        // the callee itself has been walked.
        let params: Vec<TypeId> = f
            .get_params()
            .iter()
            .map(|p| self.llvm_type_id(p.get_type()))
            .collect();
        let results: Vec<TypeId> = match f.get_type().get_return_type() {
            Some(t) => vec![self.llvm_type_id(t)],
            None => vec![],
        };
        let sig = SigId(self.sigs.len());
        self.sigs.push(SigDecl { params, results });
        let name = f.get_name().to_string_lossy().into_owned();
        self.funcs.push(FuncDecl::Import {
            module: "llvm".into(),
            name: name.clone(),
            sig,
        });
        self.func_ids.insert(key, id);
        self.exports.entry(name).or_insert(id);
        id
    }

    fn llvm_type_id<T: TryInto<inkwell::types::BasicTypeEnum<'ctx>>>(&mut self, ty: T) -> TypeId {
        use inkwell::types::BasicTypeEnum;
        let ty = ty
            .try_into()
            .unwrap_or_else(|_| panic!("expected a basic type"));
        match ty {
            BasicTypeEnum::IntType(i) => {
                let t = match i.get_bit_width() {
                    1 => Type::Bit,
                    8 => Type::_8,
                    16 => Type::_16,
                    32 => Type::_32,
                    64 => Type::_64,
                    128 => Type::_128,
                    _ => Type::_64, // best-effort fallback; conservative
                };
                self.types.primitive(t)
            }
            // Pointers/anything else are represented as 32-bit integers in
            // this importer (matching VaffleTarget's PTR_BITS convention),
            // not a distinct IrType.
            _ => self.types.primitive(Type::_32),
        }
    }

    /// Assign (allocating on first reference) the `StorageId` for a global
    /// variable, requiring the pointer operand to resolve to a literal
    /// global at import time — the generic "entire operation must be a
    /// constant load" rule, applied to every memory reference this importer
    /// emits.
    fn storage_for(&mut self, ptr: PointerValue<'ctx>) -> IResult<StorageId> {
        let raw = strip_pointer(ptr.as_value_ref(), "memory reference base")?;
        let global = global_from_pointer(raw, "memory reference base")?;
        let key = global.as_pointer_value();
        if let Some(&id) = self.storage_for_global.get(&key) {
            return Ok(id);
        }
        let id = self.storage_alloc.alloc();
        self.storage_for_global.insert(key, id);
        Ok(id)
    }

    fn import_function(&mut self, f: FunctionValue<'ctx>) -> IResult<Vec<FunctionValue<'ctx>>> {
        let id = self.func_id(f);
        if f.count_basic_blocks() == 0 {
            // Declaration only; already recorded as FuncDecl::Import above.
            return Ok(vec![]);
        }

        let blocks: Vec<LlvmBlock<'ctx>> = f.get_basic_blocks();
        let mut fctx = FuncCtx::new(self.bit_tid, blocks.len());

        // Pass A: allocate one VAFFLE BlockId per LLVM block and reserve
        // param slots for every phi at the top of it.
        for (i, bb) in blocks.iter().enumerate() {
            let vb = BlockId(i);
            fctx.block_of.insert(*bb, vb);
            fctx.phi_order[i] = phis_of(bb);
        }
        let entry_block = BlockId(0);

        // Function params become the entry block's params — bit-decomposed,
        // one Bit-typed `Value::Param` per bit (LSB first), matching
        // VaffleTarget's own integer representation: `Stmt::Poly`'s "at most
        // one non-Bit variable per monomial" rule means the generic
        // GF(2)-circuit arithmetic this crate builds on (`BitCircuitBuilder`)
        // needs every operand already split into individual bits, not one
        // wide-typed value.
        let mut next_bit_idx = 0usize;
        for param in f.get_params().iter() {
            let width = llvm_bit_width(param.get_type());
            let mut bits = Vec::with_capacity(width);
            for _ in 0..width {
                let vid = fctx.emit(
                    entry_block,
                    Value::Param {
                        block: entry_block,
                        ty: self.bit_tid,
                        idx: next_bit_idx,
                    },
                );
                fctx.params[entry_block.0].push((vid, self.bit_tid));
                bits.push(vid);
                next_bit_idx += 1;
            }
            fctx.cache.insert((*param).as_any_value_enum(), bits);
        }

        // Reserve one bit-decomposed VAFFLE param per phi, per block (after
        // any function-entry params on block 0).
        for (i, _bb) in blocks.iter().enumerate() {
            let vb = BlockId(i);
            let mut idx = if vb == entry_block { next_bit_idx } else { 0 };
            let phis = fctx.phi_order[i].clone();
            for phi in phis.iter() {
                let width = llvm_bit_width(phi.as_instruction().get_type());
                let mut bits = Vec::with_capacity(width);
                for _ in 0..width {
                    let vid = fctx.emit(
                        vb,
                        Value::Param {
                            block: vb,
                            ty: self.bit_tid,
                            idx,
                        },
                    );
                    fctx.params[vb.0].push((vid, self.bit_tid));
                    bits.push(vid);
                    idx += 1;
                }
                fctx.cache.insert(phi.as_any_value_enum(), bits);
            }
        }

        let mut called = Vec::new();

        // Pass B: translate every non-phi instruction, then the terminator.
        for (i, bb) in blocks.iter().enumerate() {
            let vb = BlockId(i);
            fctx.current = vb;
            let mut inst = bb.get_first_instruction();
            while let Some(instr) = inst {
                let next = instr.get_next_instruction();
                if instr.get_opcode() != InstructionOpcode::Phi {
                    if instr.is_terminator() {
                        self.translate_terminator(&mut fctx, instr, *bb)?;
                    } else {
                        let calls = self.translate_instruction(&mut fctx, instr)?;
                        called.extend(calls);
                    }
                }
                inst = next;
            }
        }

        // Reuse the `SigId` `func_id` already allocated for this function
        // (every function gets one up front, so recursive/mutually-recursive
        // call sites can resolve a stable `FuncId` before the callee itself
        // has been walked) rather than allocating a second, orphaned one.
        let sig = match &self.funcs[id.0] {
            FuncDecl::Import { sig, .. } => *sig,
            _ => unreachable!("import_function called twice for the same FuncId"),
        };

        let values = core::mem::take(&mut fctx.values);
        let body = FuncBody {
            sig,
            blocks: fctx.finish_blocks(),
            values,
            entry: entry_block,
        };
        self.funcs[id.0] = FuncDecl::Body(body);
        Ok(called)
    }

    /// Resolve an LLVM value (instruction result, constant, or already-cached
    /// param/phi) to its VAFFLE bits, materializing constants lazily.
    fn value_bits(&mut self, fctx: &mut FuncCtx<'ctx>, v: BasicValueEnum<'ctx>) -> IResult<Bits> {
        if let Some(bits) = fctx.cache.get(&v.as_any_value_enum()) {
            return Ok(bits.clone());
        }
        let bits = match v {
            BasicValueEnum::IntValue(i) => self.int_const_bits(fctx, i)?,
            _ => {
                return Err(ImportError::Unsupported(
                    "unsupported value kind (only integers and pointers-into-globals are supported)".into(),
                ));
            }
        };
        fctx.cache.insert(v.as_any_value_enum(), bits.clone());
        Ok(bits)
    }

    fn int_const_bits(&mut self, fctx: &mut FuncCtx<'ctx>, i: IntValue<'ctx>) -> IResult<Bits> {
        if !i.is_const() {
            return Err(ImportError::Unsupported(
                "non-constant int value with no producing instruction reached".into(),
            ));
        }
        let width = i.get_type().get_bit_width() as usize;
        let val = i.get_sign_extended_constant().unwrap_or(0) as u64;
        Ok((0..width)
            .map(|b| self.bc_const_at(fctx, fctx.current, (val >> b) & 1 != 0))
            .collect())
    }

    fn bc_const_at(&mut self, fctx: &mut FuncCtx<'ctx>, block: BlockId, val: bool) -> ValueId {
        fctx.emit(
            block,
            Value::Op(Stmt::Const(
                Constant {
                    hi: 0,
                    lo: val as u128,
                },
                self.bit_tid,
            )),
        )
    }

    fn translate_instruction(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        instr: InstructionValue<'ctx>,
    ) -> IResult<Vec<FunctionValue<'ctx>>> {
        let mut called = Vec::new();
        let opcode = instr.get_opcode();
        let cur = fctx.current;

        macro_rules! op {
            ($i:expr) => {{
                let v = instr
                    .get_operand($i)
                    .and_then(|o| o.value())
                    .ok_or_else(|| ImportError::Unsupported("expected value operand".into()))?;
                self.value_bits(fctx, v)?
            }};
        }

        let result_bits: Option<Bits> = match opcode {
            InstructionOpcode::Add => {
                let (a, b) = (op!(0), op!(1));
                Some(circuits::bc_add(
                    &mut Ctx {
                        fctx,
                        bit_tid: self.bit_tid,
                        block: cur,
                    },
                    &a,
                    &b,
                    false,
                ))
            }
            InstructionOpcode::Sub => {
                let (a, b) = (op!(0), op!(1));
                Some(circuits::bc_sub(
                    &mut Ctx {
                        fctx,
                        bit_tid: self.bit_tid,
                        block: cur,
                    },
                    &a,
                    &b,
                ))
            }
            InstructionOpcode::Mul => {
                let (a, b) = (op!(0), op!(1));
                Some(circuits::bc_mul(
                    &mut Ctx {
                        fctx,
                        bit_tid: self.bit_tid,
                        block: cur,
                    },
                    &a,
                    &b,
                ))
            }
            InstructionOpcode::UDiv => {
                let (a, b) = (op!(0), op!(1));
                Some(circuits::bc_udiv(
                    &mut Ctx {
                        fctx,
                        bit_tid: self.bit_tid,
                        block: cur,
                    },
                    &a,
                    &b,
                ))
            }
            InstructionOpcode::SDiv => {
                let (a, b) = (op!(0), op!(1));
                Some(circuits::bc_sdiv(
                    &mut Ctx {
                        fctx,
                        bit_tid: self.bit_tid,
                        block: cur,
                    },
                    &a,
                    &b,
                ))
            }
            InstructionOpcode::And => {
                let (a, b) = (op!(0), op!(1));
                Some(circuits::bc_and_vec(
                    &mut Ctx {
                        fctx,
                        bit_tid: self.bit_tid,
                        block: cur,
                    },
                    &a,
                    &b,
                ))
            }
            InstructionOpcode::Or => {
                let (a, b) = (op!(0), op!(1));
                Some(circuits::bc_or_vec(
                    &mut Ctx {
                        fctx,
                        bit_tid: self.bit_tid,
                        block: cur,
                    },
                    &a,
                    &b,
                ))
            }
            InstructionOpcode::Xor => {
                let (a, b) = (op!(0), op!(1));
                Some(circuits::bc_xor_vec(
                    &mut Ctx {
                        fctx,
                        bit_tid: self.bit_tid,
                        block: cur,
                    },
                    &a,
                    &b,
                ))
            }
            InstructionOpcode::Shl => {
                let (a, b) = (op!(0), op!(1));
                Some(circuits::bc_shl(
                    &mut Ctx {
                        fctx,
                        bit_tid: self.bit_tid,
                        block: cur,
                    },
                    &a,
                    &b,
                ))
            }
            InstructionOpcode::LShr => {
                let (a, b) = (op!(0), op!(1));
                Some(circuits::bc_lshr(
                    &mut Ctx {
                        fctx,
                        bit_tid: self.bit_tid,
                        block: cur,
                    },
                    &a,
                    &b,
                ))
            }
            InstructionOpcode::AShr => {
                let (a, b) = (op!(0), op!(1));
                Some(circuits::bc_ashr(
                    &mut Ctx {
                        fctx,
                        bit_tid: self.bit_tid,
                        block: cur,
                    },
                    &a,
                    &b,
                ))
            }
            InstructionOpcode::ICmp => {
                let pred = instr
                    .get_icmp_predicate()
                    .ok_or_else(|| ImportError::Unsupported("icmp without predicate".into()))?;
                let (a, b) = (op!(0), op!(1));
                let mut c = Ctx {
                    fctx,
                    bit_tid: self.bit_tid,
                    block: cur,
                };
                let bit = match pred {
                    IntPredicate::EQ => circuits::bc_eq(&mut c, &a, &b),
                    IntPredicate::NE => circuits::bc_ne(&mut c, &a, &b),
                    IntPredicate::ULT => circuits::bc_ult(&mut c, &a, &b),
                    IntPredicate::ULE => circuits::bc_ule(&mut c, &a, &b),
                    IntPredicate::UGT => circuits::bc_ult(&mut c, &b, &a),
                    IntPredicate::UGE => circuits::bc_ule(&mut c, &b, &a),
                    IntPredicate::SLT => circuits::bc_slt(&mut c, &a, &b),
                    IntPredicate::SLE => circuits::bc_sle(&mut c, &a, &b),
                    IntPredicate::SGT => circuits::bc_slt(&mut c, &b, &a),
                    IntPredicate::SGE => circuits::bc_sle(&mut c, &b, &a),
                };
                Some(vec![bit])
            }
            InstructionOpcode::Select => {
                let cond = op!(0)[0];
                let (t, e) = (op!(1), op!(2));
                Some(circuits::bc_select_vec(
                    &mut Ctx {
                        fctx,
                        bit_tid: self.bit_tid,
                        block: cur,
                    },
                    cond,
                    &t,
                    &e,
                ))
            }
            InstructionOpcode::Trunc => {
                let src = op!(0);
                let dst_n = int_result_width(instr)?;
                Some(src[..dst_n].to_vec())
            }
            InstructionOpcode::ZExt => {
                let mut src = op!(0);
                let dst_n = int_result_width(instr)?;
                while src.len() < dst_n {
                    src.push(self.bc_const_at(fctx, cur, false));
                }
                Some(src)
            }
            InstructionOpcode::SExt => {
                let mut src = op!(0);
                let dst_n = int_result_width(instr)?;
                let sign = *src
                    .last()
                    .ok_or_else(|| ImportError::Unsupported("sext of empty value".into()))?;
                while src.len() < dst_n {
                    src.push(sign);
                }
                Some(src)
            }
            InstructionOpcode::Load => {
                let ptr = load_store_pointer(instr, 0)?;
                if let Some(&base_slot) = fctx.stack_slot_of.get(&ptr.as_any_value_enum()) {
                    let ptr_bits0 = fctx
                        .cache
                        .get(&ptr.as_any_value_enum())
                        .and_then(|b| b.first().copied())
                        .ok_or_else(|| {
                            ImportError::Unsupported(
                                "stack pointer bits missing (internal)".into(),
                            )
                        })?;
                    let n_bits = int_result_width(instr)?;
                    let pointee_tid = self.llvm_type_id(instr.get_type());
                    Some(self.stack_load(fctx, ptr_bits0, base_slot, pointee_tid, n_bits))
                } else {
                    let n_bytes = int_result_width(instr)?.div_ceil(8);
                    Some(self.mem_load(fctx, ptr, n_bytes)?)
                }
            }
            InstructionOpcode::Store => {
                let val = op!(0);
                let ptr = load_store_pointer(instr, 1)?;
                if let Some(&base_slot) = fctx.stack_slot_of.get(&ptr.as_any_value_enum()) {
                    let ptr_bits0 = fctx
                        .cache
                        .get(&ptr.as_any_value_enum())
                        .and_then(|b| b.first().copied())
                        .ok_or_else(|| {
                            ImportError::Unsupported(
                                "stack pointer bits missing (internal)".into(),
                            )
                        })?;
                    self.stack_store(fctx, ptr_bits0, base_slot, &val);
                } else {
                    let n_bytes = val.len().div_ceil(8);
                    self.mem_store(fctx, ptr, &val, n_bytes)?;
                }
                None
            }
            InstructionOpcode::GetElementPtr => {
                let base = load_store_pointer(instr, 0)?;
                if let Some(&base_slot) = fctx.stack_slot_of.get(&base.as_any_value_enum()) {
                    // Constant-offset GEP off a tracked stack pointer. A
                    // symbolic index *could* be supported later (STACK
                    // addressing is runtime bit arithmetic, unlike a
                    // global's compile-time-resolved identity), but that's
                    // out of scope here.
                    if instr.get_num_operands() != 2 {
                        return Err(ImportError::Unsupported(
                            "multi-index GEP into stack pointer not supported".into(),
                        ));
                    }
                    let elem_ty = instr
                        .get_gep_source_element_type()
                        .map_err(|_| ImportError::Unsupported("malformed gep".into()))?;
                    let elem_bits = match elem_ty {
                        inkwell::types::BasicTypeEnum::IntType(t) => t.get_bit_width() as i64,
                        _ => {
                            return Err(ImportError::Unsupported(
                                "gep of non-integer element type into stack pointer not supported"
                                    .into(),
                            ));
                        }
                    };
                    let idx: i64 = match instr.get_operand(1).and_then(|o| o.value()) {
                        Some(BasicValueEnum::IntValue(n)) => {
                            n.get_sign_extended_constant().ok_or_else(|| {
                                ImportError::Unsupported(
                                    "symbolic index into stack pointer not supported".into(),
                                )
                            })?
                        }
                        _ => {
                            return Err(ImportError::Unsupported(
                                "expected an integer gep index".into(),
                            ));
                        }
                    };
                    let offset = idx
                        .checked_mul(elem_bits)
                        .ok_or_else(|| ImportError::Unsupported("gep offset overflow".into()))?;
                    let new_base_slot = base_slot.checked_add_signed(offset).ok_or_else(|| {
                        ImportError::Unsupported("gep offset out of range".into())
                    })?;

                    let addr_bits = self.stack_addr_bits(fctx, cur, new_base_slot);
                    let base_bits = fctx
                        .cache
                        .get(&base.as_any_value_enum())
                        .cloned()
                        .unwrap_or_else(|| addr_bits.clone());
                    fctx.emit(
                        cur,
                        Value::PtrOffset {
                            ptr: base_bits[0],
                            idx: addr_bits[0], // representative, matching `VaffleTarget::ptr_offset`
                            elem_bits: elem_bits as usize,
                        },
                    );
                    fctx.stack_slot_of
                        .insert(instr.as_any_value_enum(), new_base_slot);
                    Some(addr_bits)
                } else {
                    // Only a base global is supported; the byte offset a
                    // GEP chain would add is not yet folded in here (this
                    // validates the base resolves to a literal global so
                    // `Load`/`Store` through this pointer succeed).
                    self.storage_for(base)?;
                    None
                }
            }
            InstructionOpcode::Call => {
                let call = CallSiteValue::try_from(instr)
                    .map_err(|_| ImportError::Unsupported("malformed call instruction".into()))?;
                let callee_fn = call
                    .get_called_fn_value()
                    .ok_or_else(|| ImportError::Unsupported("indirect call".into()))?;
                let callee_id = self.func_id(callee_fn);
                called.push(callee_fn);
                let n_args = instr.get_num_operands().saturating_sub(1);
                let mut args = Vec::new();
                for i in 0..n_args {
                    let v = instr
                        .get_operand(i)
                        .and_then(|o| o.value())
                        .ok_or_else(|| {
                            ImportError::Unsupported("call argument must be a value".into())
                        })?;
                    args.extend(self.value_bits(fctx, v)?);
                }
                let vid = fctx.emit(
                    cur,
                    Value::Call {
                        func: callee_id,
                        args,
                    },
                );
                match instr.get_type().try_into() {
                    Ok(inkwell::types::BasicTypeEnum::IntType(t)) => {
                        let n = t.get_bit_width() as usize;
                        Some(
                            (0..n)
                                .map(|i| fctx.emit(cur, Value::Output { value: vid, idx: i }))
                                .collect(),
                        )
                    }
                    _ => None,
                }
            }
            InstructionOpcode::Alloca => {
                let elem_ty = instr
                    .get_allocated_type()
                    .map_err(|_| ImportError::Unsupported("malformed alloca".into()))?;
                let elem_bits = match elem_ty {
                    inkwell::types::BasicTypeEnum::IntType(t) => t.get_bit_width() as u64,
                    _ => {
                        return Err(ImportError::Unsupported(
                            "alloca of non-integer type not supported".into(),
                        ));
                    }
                };
                // The array-size operand is `1` unless the source used
                // `alloca <ty>, <n>`; either way it must be a compile-time
                // constant (VLAs are symbolic and fail closed here, not via
                // a panic).
                let count: u64 = match instr.get_operand(0).and_then(|o| o.value()) {
                    Some(BasicValueEnum::IntValue(n)) => n.get_zero_extended_constant().ok_or_else(
                        || ImportError::Unsupported("alloca count is symbolic".into()),
                    )?,
                    _ => 1,
                };
                let total_slots = elem_bits
                    .checked_mul(count)
                    .ok_or_else(|| ImportError::Unsupported("alloca size overflow".into()))?;
                let base_slot = fctx.next_stack_slot;
                fctx.next_stack_slot = fctx
                    .next_stack_slot
                    .checked_add(total_slots)
                    .ok_or_else(|| ImportError::Unsupported("alloca stack overflow".into()))?;

                // Bookkeeping marker (unused as an operand, matching
                // `VaffleTarget::alloca`'s own `_alloc_vid` convention) —
                // required so passes that pattern-match `Value::StackAlloc`
                // (e.g. `inline_vaffle`'s stack-slot rebase, `lower_to_ir`'s
                // spill-avoidance) see this allocation.
                let elem_tid = self.llvm_type_id(elem_ty);
                fctx.emit(
                    cur,
                    Value::StackAlloc {
                        elem_ty: elem_tid,
                        count: count as usize,
                        base_slot,
                    },
                );

                let addr_bits = self.stack_addr_bits(fctx, cur, base_slot);
                fctx.stack_slot_of
                    .insert(instr.as_any_value_enum(), base_slot);
                Some(addr_bits)
            }
            other => {
                return Err(ImportError::Unsupported(format!("{other:?}")));
            }
        };

        if let Some(bits) = result_bits {
            fctx.cache.insert(instr.as_any_value_enum(), bits);
        }
        Ok(called)
    }

    /// Bit-decompose a compile-time-constant `StorageId::STACK` address,
    /// `PTR_BITS` wide, LSB first — the pointer *value* for an alloca or a
    /// constant-index GEP off one. Mirrors `VaffleTarget::alloca`'s
    /// `addr_bits` construction exactly.
    fn stack_addr_bits(&mut self, fctx: &mut FuncCtx<'ctx>, block: BlockId, base_slot: u64) -> Bits {
        (0..PTR_BITS)
            .map(|i| self.bc_const_at(fctx, block, (base_slot >> i) & 1 != 0))
            .collect()
    }

    /// Read `n_bits` individual bits from `StorageId::STACK` starting at
    /// `base_slot`, one `StorageRead` per bit (matches `VaffleTarget::
    /// ptr_load`'s per-bit granularity, but with a compile-time-constant
    /// address per bit instead of a runtime-composed one, since this
    /// importer only tracks compile-time-constant stack pointers).
    fn stack_load(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        ptr_bits0: ValueId,
        base_slot: u64,
        pointee_ty: TypeId,
        n_bits: usize,
    ) -> Bits {
        let cur = fctx.current;
        let mut bits = Vec::with_capacity(n_bits);
        for i in 0..n_bits as u64 {
            let addr = fctx.emit(
                cur,
                Value::Op(Stmt::Const(
                    Constant {
                        hi: 0,
                        lo: (base_slot + i) as u128,
                    },
                    self.bit_tid,
                )),
            );
            let bit = fctx.emit(
                cur,
                Value::Op(Stmt::StorageRead {
                    storage: StorageId::STACK,
                    ty: self.bit_tid,
                    addr,
                }),
            );
            bits.push(bit);
        }
        // Bookkeeping marker (unused as an operand); the real read already
        // happened above, matching `VaffleTarget::ptr_load`'s `_load_vid`.
        fctx.emit(
            cur,
            Value::PtrLoad {
                ptr: ptr_bits0,
                pointee_ty,
            },
        );
        bits
    }

    /// Write `val` to `StorageId::STACK` starting at `base_slot`, one
    /// `StorageWrite` per bit. See [`Self::stack_load`].
    fn stack_store(&mut self, fctx: &mut FuncCtx<'ctx>, ptr_bits0: ValueId, base_slot: u64, val: &Bits) {
        let cur = fctx.current;
        for (i, &bit) in val.iter().enumerate() {
            let addr = fctx.emit(
                cur,
                Value::Op(Stmt::Const(
                    Constant {
                        hi: 0,
                        lo: (base_slot + i as u64) as u128,
                    },
                    self.bit_tid,
                )),
            );
            fctx.emit(
                cur,
                Value::Op(Stmt::StorageWrite {
                    storage: StorageId::STACK,
                    src: bit,
                    ty: self.bit_tid,
                    addr,
                }),
            );
        }
        let val_bits0 = val.first().copied().unwrap_or(ptr_bits0);
        fctx.emit(
            cur,
            Value::PtrStore {
                ptr: ptr_bits0,
                val: val_bits0,
            },
        );
    }

    fn mem_load(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        ptr: PointerValue<'ctx>,
        n_bytes: usize,
    ) -> IResult<Bits> {
        let storage = self.storage_for(ptr)?;
        let cur = fctx.current;
        let mut all_bits = Vec::with_capacity(n_bytes * 8);
        for byte_i in 0..n_bytes {
            let addr = fctx.emit(
                cur,
                Value::Op(Stmt::Const(
                    Constant {
                        hi: 0,
                        lo: byte_i as u128,
                    },
                    self.byte_tid,
                )),
            );
            let byte_var = fctx.emit(
                cur,
                Value::Op(Stmt::StorageRead {
                    storage,
                    ty: self.byte_tid,
                    addr,
                }),
            );
            for bit_j in 0..8u8 {
                let bit = fctx.emit(
                    cur,
                    Value::Op(Stmt::Shuffle {
                        result_bits: vec![(bit_j, byte_var)],
                        ty: self.bit_tid,
                    }),
                );
                all_bits.push(bit);
            }
        }
        Ok(all_bits)
    }

    fn mem_store(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        ptr: PointerValue<'ctx>,
        val: &Bits,
        n_bytes: usize,
    ) -> IResult<()> {
        let storage = self.storage_for(ptr)?;
        let cur = fctx.current;
        for byte_i in 0..n_bytes {
            let addr = fctx.emit(
                cur,
                Value::Op(Stmt::Const(
                    Constant {
                        hi: 0,
                        lo: byte_i as u128,
                    },
                    self.byte_tid,
                )),
            );
            let base = byte_i * 8;
            let zero = self.bc_const_at(fctx, cur, false);
            let bits: Vec<ValueId> = (0..8)
                .map(|j| *val.get(base + j).unwrap_or(&zero))
                .collect();
            let byte_var = fctx.emit(
                cur,
                Value::Op(Stmt::Merge {
                    parts: bits,
                    ty: self.byte_tid,
                }),
            );
            fctx.emit(
                cur,
                Value::Op(Stmt::StorageWrite {
                    storage,
                    src: byte_var,
                    ty: self.byte_tid,
                    addr,
                }),
            );
        }
        Ok(())
    }

    fn translate_terminator(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        instr: InstructionValue<'ctx>,
        bb: LlvmBlock<'ctx>,
    ) -> IResult<()> {
        let cur = fctx.current;
        let term = match instr.get_opcode() {
            InstructionOpcode::Return => {
                let values = match instr.get_operand(0) {
                    Some(op) => {
                        let v = op.value().ok_or_else(|| {
                            ImportError::Unsupported("ret operand must be a value".into())
                        })?;
                        self.value_bits(fctx, v)?
                    }
                    None => vec![],
                };
                Terminator::Return { values }
            }
            InstructionOpcode::Br => {
                if instr.get_num_operands() == 1 {
                    let dest = self.successor_target(fctx, bb, 0, instr)?;
                    Terminator::Jump(dest)
                } else {
                    let cond = instr
                        .get_operand(0)
                        .and_then(|o| o.value())
                        .ok_or_else(|| {
                            ImportError::Unsupported("br condition must be a value".into())
                        })?;
                    let cond_bit = self.value_bits(fctx, cond)?[0];
                    // LLVM's low-level operand list stores the false
                    // successor before the true successor (operand 1 = else,
                    // operand 2 = then) — the textual syntax's `label %then,
                    // label %else` order is reversed from this; verified
                    // against volar-llvm-import-core's own `branch` handling.
                    let then_target = self.successor_target(fctx, bb, 2, instr)?;
                    let else_target = self.successor_target(fctx, bb, 1, instr)?;
                    Terminator::IfNonzero {
                        cond: cond_bit,
                        then_target,
                        else_target,
                    }
                }
            }
            InstructionOpcode::Switch => self.translate_switch(fctx, instr, bb)?,
            other => {
                return Err(ImportError::Unsupported(format!(
                    "terminator {other:?} (indirectbr not yet supported)"
                )));
            }
        };
        fctx.terminators[cur.0] = Some(term);
        Ok(())
    }

    /// LLVM `switch`: sparse `(key, dest)` pairs plus a default. VAFFLE
    /// `Terminator::Table` is dense positional (`targets[idx]`, else
    /// default), so this builds the same GF(2) selector cascade as
    /// `VaffleTarget::switch`: the first matching case index, or
    /// `cases.len()` (out of `targets` bounds) when nothing matches.
    fn translate_switch(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        instr: InstructionValue<'ctx>,
        bb: LlvmBlock<'ctx>,
    ) -> IResult<Terminator> {
        let cur = fctx.current;
        let cond = instr
            .get_operand(0)
            .and_then(|o| o.value())
            .ok_or_else(|| ImportError::Unsupported("switch condition must be a value".into()))?;
        let cond_bits = self.value_bits(fctx, cond)?;
        let default_bb = instr
            .get_operand(1)
            .and_then(|o| o.block())
            .ok_or_else(|| ImportError::Unsupported("switch default must be a block".into()))?;
        let default_target = self.target_for_dest(fctx, bb, default_bb)?;

        let mut cases: Vec<(i64, Target)> = Vec::new();
        // Operands are `[cond, default_dest, case1_dest, …]`. Case *keys*
        // are not operands — `LLVMGetSwitchCaseValue(sw, successor_index)`
        // (successor 0 = default, so cases start at 1). See
        // `volar-llvm-jumpthread`'s `switch_target`.
        let n_ops = instr.get_num_operands();
        for i in 2..n_ops {
            let dest = instr
                .get_operand(i)
                .and_then(|o| o.block())
                .ok_or_else(|| ImportError::Unsupported("switch case dest must be a block".into()))?;
            let case_value = unsafe {
                IntValue::new(LLVMGetSwitchCaseValue(instr.as_value_ref(), i - 1))
            };
            let key = case_value.get_sign_extended_constant().ok_or_else(|| {
                ImportError::Unsupported("switch case key must be a constant".into())
            })?;
            cases.push((key, self.target_for_dest(fctx, bb, dest)?));
        }

        let n = cases.len();
        let sel_width = bits_for_max_value(n);
        let index_width = cond_bits.len();
        let selector = {
            let mut c = Ctx {
                fctx,
                bit_tid: self.bit_tid,
                block: cur,
            };
            let mut selector: Vec<ValueId> = (0..sel_width)
                .map(|b| c.bc_const((n >> b) & 1 != 0))
                .collect();
            for (case_i, (key, _)) in cases.iter().enumerate().rev() {
                let key_bits: Vec<ValueId> = (0..index_width)
                    .map(|b| c.bc_const((*key as u64 >> b) & 1 != 0))
                    .collect();
                let matched = circuits::bc_eq(&mut c, &cond_bits, &key_bits);
                let case_idx_bits: Vec<ValueId> = (0..sel_width)
                    .map(|b| c.bc_const((case_i >> b) & 1 != 0))
                    .collect();
                selector = circuits::bc_select_vec(&mut c, matched, &case_idx_bits, &selector);
            }
            selector
        };
        let selector_val = self.compose_address(fctx, cur, &selector);
        Ok(Terminator::Table {
            index: selector_val,
            targets: cases.into_iter().map(|(_, t)| t).collect(),
            default_target,
        })
    }

    /// Pack `bits` into a single VAFFLE value for use as a `Table` index,
    /// matching `VaffleTarget::compose_address`.
    fn compose_address(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        block: BlockId,
        bits: &[ValueId],
    ) -> ValueId {
        if bits.len() == 1 {
            return bits[0];
        }
        let vec_ty = self.types.intern(IrType::Vec(bits.len(), self.bit_tid));
        fctx.emit(
            block,
            Value::Op(Stmt::Merge {
                parts: bits.to_vec(),
                ty: vec_ty,
            }),
        )
    }

    /// Build the `Target` for successor block `succ_idx` of `bb`'s
    /// terminator: resolves that successor's VAFFLE `BlockId` and computes
    /// its `args` from every phi in the successor, in order, evaluated at
    /// `bb`'s incoming edge.
    fn successor_target(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        bb: LlvmBlock<'ctx>,
        succ_idx: u32,
        instr: InstructionValue<'ctx>,
    ) -> IResult<Target> {
        let succ_bb = instr
            .get_operand(succ_idx)
            .and_then(|o| o.block())
            .ok_or_else(|| ImportError::Unsupported("branch successor must be a block".into()))?;
        self.target_for_dest(fctx, bb, succ_bb)
    }

    fn target_for_dest(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        from: LlvmBlock<'ctx>,
        dest: LlvmBlock<'ctx>,
    ) -> IResult<Target> {
        let succ_vb = *fctx
            .block_of
            .get(&dest)
            .ok_or_else(|| ImportError::Unsupported("branch to unknown block".into()))?;
        let phis = fctx.phi_order[succ_vb.0].clone();
        let mut args = Vec::new();
        for phi in phis {
            let incoming = (0..phi.count_incoming())
                .find_map(|i| {
                    let (v, from_bb) = phi.get_incoming(i)?;
                    (from_bb == from).then_some(v)
                })
                .ok_or_else(|| {
                    ImportError::Unsupported(
                        "phi missing incoming value for this predecessor".into(),
                    )
                })?;
            args.extend(self.value_bits(fctx, incoming)?);
        }
        Ok(Target {
            block: succ_vb,
            args,
            reentry: None,
        })
    }
}

fn phis_of<'ctx>(bb: &LlvmBlock<'ctx>) -> Vec<PhiValue<'ctx>> {
    let mut out = Vec::new();
    let mut inst = bb.get_first_instruction();
    while let Some(instr) = inst {
        if instr.get_opcode() != InstructionOpcode::Phi {
            break;
        }
        if let Ok(phi) = PhiValue::try_from(instr) {
            out.push(phi);
        }
        inst = instr.get_next_instruction();
    }
    out
}

fn int_result_width(instr: InstructionValue<'_>) -> IResult<usize> {
    match instr.get_type().try_into() {
        Ok(inkwell::types::BasicTypeEnum::IntType(t)) => Ok(t.get_bit_width() as usize),
        _ => Err(ImportError::Unsupported(
            "expected an integer-typed instruction result".into(),
        )),
    }
}

/// Bit width used for this importer's per-bit `Value::Param` decomposition.
/// Integers use their real width; pointers (and anything else) use the
/// PTR_BITS(=32) convention `VaffleTarget` also uses.
fn llvm_bit_width<'ctx, T: TryInto<inkwell::types::BasicTypeEnum<'ctx>>>(ty: T) -> usize {
    use inkwell::types::BasicTypeEnum;
    match ty.try_into() {
        Ok(BasicTypeEnum::IntType(i)) => i.get_bit_width() as usize,
        _ => 32,
    }
}

fn load_store_pointer<'ctx>(
    instr: InstructionValue<'ctx>,
    operand: u32,
) -> IResult<PointerValue<'ctx>> {
    match instr.get_operand(operand).and_then(|o| o.value()) {
        Some(BasicValueEnum::PointerValue(p)) => Ok(p),
        _ => Err(ImportError::Unsupported(
            "expected a pointer operand".into(),
        )),
    }
}

struct FuncCtx<'ctx> {
    values: Vec<Node<Value, ()>>,
    block_of: HashMap<LlvmBlock<'ctx>, BlockId>,
    phi_order: Vec<Vec<PhiValue<'ctx>>>,
    params: Vec<Vec<(ValueId, TypeId)>>,
    stmts: Vec<Vec<ValueId>>,
    terminators: Vec<Option<Terminator>>,
    cache: HashMap<AnyValueEnum<'ctx>, Bits>,
    current: BlockId,
    /// Per-function bump allocator for `StorageId::STACK`, in *bits* (matches
    /// `VaffleTarget`'s own `next_stack_slot`/`PTR_BITS` convention) — not
    /// bytes like the global `storage_for`/`mem_load`/`mem_store` path,
    /// which is a distinct storage identity and addressing convention.
    next_stack_slot: u64,
    /// Pointer-typed LLVM values (alloca results, or a constant-index GEP
    /// off one) that are tracked as `StorageId::STACK` addresses, mapped to
    /// their resolved compile-time-constant `base_slot`. This is the sole
    /// source of truth for "is this a stack pointer" — `cache` alone is not
    /// enough, since pointer-typed function *parameters* are also cached
    /// there as plain (meaningless-as-an-address) bits.
    stack_slot_of: HashMap<AnyValueEnum<'ctx>, u64>,
}

impl<'ctx> FuncCtx<'ctx> {
    fn new(_bit_tid: TypeId, n_blocks: usize) -> Self {
        FuncCtx {
            values: Vec::new(),
            block_of: HashMap::new(),
            phi_order: vec![Vec::new(); n_blocks],
            params: vec![Vec::new(); n_blocks],
            stmts: vec![Vec::new(); n_blocks],
            terminators: vec![None; n_blocks],
            cache: HashMap::new(),
            current: BlockId(0),
            next_stack_slot: 0,
            stack_slot_of: HashMap::new(),
        }
    }

    fn emit(&mut self, block: BlockId, v: Value) -> ValueId {
        let id = ValueId(self.values.len());
        self.values.push(Node::new(v, (), None));
        self.stmts[block.0].push(id);
        id
    }

    fn finish_blocks(self) -> Vec<Block> {
        self.stmts
            .into_iter()
            .zip(self.params)
            .zip(self.terminators)
            .map(|((stmts, params), term)| Block {
                params,
                stmts,
                terminator: term.unwrap_or(Terminator::Return { values: vec![] }),
            })
            .collect()
    }
}

/// Adapter implementing [`BitCircuitBuilder`] over the per-function builder
/// state, targeting a specific block.
struct Ctx<'a, 'ctx> {
    fctx: &'a mut FuncCtx<'ctx>,
    bit_tid: TypeId,
    block: BlockId,
}

impl<'a, 'ctx> BitCircuitBuilder for Ctx<'a, 'ctx> {
    type Bit = ValueId;

    fn bc_const(&mut self, val: bool) -> ValueId {
        self.fctx.emit(
            self.block,
            Value::Op(Stmt::Const(
                Constant {
                    hi: 0,
                    lo: val as u128,
                },
                self.bit_tid,
            )),
        )
    }

    fn bc_poly(
        &mut self,
        coeffs: std::collections::BTreeMap<Vec<ValueId>, u8>,
        constant: u128,
    ) -> ValueId {
        let ty = self.bit_tid;
        self.fctx.emit(
            self.block,
            Value::Op(Stmt::Poly {
                ty,
                coeffs,
                constant: Constant {
                    hi: 0,
                    lo: constant,
                },
            }),
        )
    }
}
