// @reliability: normal
// @ai: assisted
//! LLVM backend for `LirTarget` (via `inkwell` / LLVM 20).
//!
//! Emits a native LLVM `Module` that can be compiled to object code, LLVM IR
//! text, or bitcode.  Each LIR function becomes an LLVM function; SSA values
//! map 1-to-1 to LLVM `BasicValueEnum` handles.
//!
//! # ABI
//!
//! Returns [`LirAbi::DEFAULT`], so the `volar-lir-codegen` layer decomposes
//! all `Arr`/`Struct` types to flat scalars before calling any backend method.
//! The backend therefore only receives scalar `LirType` variants (`Bool`,
//! `I8`–`U64`, `Native`, `Ptr`) from the codegen layer.  `define_struct` is
//! still called for bookkeeping, but the resulting LLVM struct types are only
//! used internally (e.g. as alloca element types).
//!
//! # Block-parameter / PHI strategy
//!
//! LIR uses block-parameter SSA (Cranelift-style).  LLVM uses PHI nodes.
//! The mapping:
//!
//! 1. `add_block_param(block, ty)` — temporarily positions the builder at the
//!    end of `block` (which is empty at this point), emits the PHI node, then
//!    restores the previous insertion position.  Returns the PHI's
//!    `BasicValueEnum` as an [`LlvmValue`].
//!
//! 2. `switch_to_block(block)` — positions the builder at the end of `block`.
//!    PHI nodes are already present from step 1.
//!
//! 3. `jump(target, args)` / `branch(...)` — adds incoming edges to the
//!    target block's PHI nodes, then emits an unconditional/conditional branch.
//!
//! # Pointer types (opaque pointers)
//!
//! LLVM 20 uses untyped `ptr` for all pointer values.  `ptr_load` and GEP
//! operations receive the pointee type explicitly (stored in `LlvmValue::ty`).

use std::vec::Vec;
use std::{collections::HashMap, num::NonZeroU32};

use inkwell::{
    AddressSpace,
    builder::Builder,
    context::Context,
    module::{Linkage, Module},
    types::{BasicMetadataTypeEnum, BasicType, BasicTypeEnum, FunctionType, StructType},
    values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue, PhiValue},
};
use volar_ir_common::Type as NativeType;
use volar_lir::{
    BranchTarget, IcmpPred, LirAbi, LirTarget, LirType, StackAllocExt, StructDef, StructId,
};

pub use volar_lir::NameConfig;

// ============================================================================
// Value and Block handle types
// ============================================================================

/// An SSA value produced by the LLVM backend.
///
/// Wraps an inkwell `BasicValueEnum` together with its `LirType` so that
/// [`LirTarget::value_scalar_type`] can be answered without a separate map.
#[derive(Clone, Debug)]
pub struct LlvmValue<'ctx> {
    pub(crate) inner: BasicValueEnum<'ctx>,
    pub(crate) ty: LirType,
}

impl PartialEq for LlvmValue<'_> {
    fn eq(&self, other: &Self) -> bool {
        // LLVM values are pointer-unique; compare by the underlying raw pointer.
        self.inner == other.inner
    }
}

impl Eq for LlvmValue<'_> {}

/// A basic block handle used by the LLVM backend.
#[derive(Clone, Copy, Debug)]
pub struct LlvmBlock<'ctx> {
    pub(crate) inner: inkwell::basic_block::BasicBlock<'ctx>,
    /// Index into `FunctionState::blocks`.
    pub(crate) id: u32,
}

impl PartialEq for LlvmBlock<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id && self.inner == other.inner
    }
}
impl Eq for LlvmBlock<'_> {}

// ============================================================================
// Per-block state
// ============================================================================

struct BlockState<'ctx> {
    /// PHI nodes created by `add_block_param`, in declaration order.
    /// Each entry is `(phi_value, lir_type)`.
    phi_values: Vec<(PhiValue<'ctx>, LirType)>,
}

// ============================================================================
// Per-function state
// ============================================================================

struct FunctionState<'ctx> {
    func: FunctionValue<'ctx>,
    ret_ty: Option<LirType>,
    /// One entry per block created via `begin_function` / `create_block`.
    blocks: Vec<BlockState<'ctx>>,
}

#[derive(Clone, Copy)]
enum IntBinOp {
    Add,
    Sub,
    Mul,
    Udiv,
    Sdiv,
    And,
    Or,
    Xor,
    Shl,
    Lshr,
    Ashr,
}

// ============================================================================
// The backend
// ============================================================================

/// LLVM backend implementing [`LirTarget`].
///
/// # Usage
///
/// ```ignore
/// let ctx = inkwell::context::Context::create();
/// let mut backend = LlvmBackend::new(&ctx, "my_module");
/// // drive via `lower_biir` / `lower_module` / etc.
/// let module = backend.finish();
/// module.print_to_file("out.ll").unwrap();
/// ```
pub struct LlvmBackend<'ctx> {
    context: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    current: Option<FunctionState<'ctx>>,

    // ---- Struct registry ----
    struct_defs: Vec<StructDef>,
    struct_names: Vec<String>,
    struct_llvm_types: Vec<StructType<'ctx>>,
    next_struct_id: StructId,

    // ---- Extern function cache ----
    /// Maps declared function names to their `FunctionValue` to avoid
    /// re-declaring the same extern multiple times.
    extern_cache: HashMap<String, FunctionValue<'ctx>>,

    /// Name configuration: prefix and per-name remaps applied to all defined
    /// and called function names.  See [`NameConfig`].
    pub name_config: NameConfig,

    /// Name of the C function called for `rng` stmts.
    /// Expected signature: `void volar_rng(void*, uint64_t)`.
    /// Default: `"volar_rng"`.
    pub rng_fn: String,
}

impl<'ctx> LlvmBackend<'ctx> {
    /// Create a new backend bound to `context`, producing a module named
    /// `module_name`.
    pub fn new(context: &'ctx Context, module_name: &str) -> Self {
        LlvmBackend {
            context,
            module: context.create_module(module_name),
            builder: context.create_builder(),
            current: None,
            struct_defs: Vec::new(),
            struct_names: Vec::new(),
            struct_llvm_types: Vec::new(),
            next_struct_id: 0,
            extern_cache: HashMap::new(),
            name_config: NameConfig::default(),
            rng_fn: "volar_rng".to_string(),
        }
    }

    /// Set the name configuration (prefix + per-name remaps).
    pub fn with_name_config(mut self, config: NameConfig) -> Self {
        self.name_config = config;
        self
    }

    /// Convenience: set a prefix applied to all emitted function names.
    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.name_config.prefix = prefix.into();
        self
    }

    /// Set the RNG helper function name.
    pub fn with_rng_fn(mut self, name: impl Into<String>) -> Self {
        self.rng_fn = name.into();
        self
    }

    /// Consume the backend and return the finished LLVM module.
    pub fn finish(self) -> Module<'ctx> {
        self.module
    }

    /// Borrow the LLVM module (e.g. to verify or print before consuming).
    pub fn module(&self) -> &Module<'ctx> {
        &self.module
    }

    // ---- Internal helpers ---------------------------------------------------

    fn state(&mut self) -> &mut FunctionState<'ctx> {
        self.current
            .as_mut()
            .expect("LlvmBackend: not inside a function")
    }

    /// Map a scalar `LirType` to its LLVM `BasicTypeEnum`.
    ///
    /// Panics on `Arr`/`Struct` — `LirAbi::DEFAULT` ensures the codegen layer
    /// flattens these before they reach the backend.
    fn lir_type_to_llvm(&self, ty: &LirType) -> BasicTypeEnum<'ctx> {
        match ty {
            LirType::Bool => self.context.bool_type().into(),
            LirType::I8 | LirType::U8 => self.context.i8_type().into(),
            LirType::I16 | LirType::U16 => self.context.i16_type().into(),
            LirType::I32 | LirType::U32 => self.context.i32_type().into(),
            LirType::I64 | LirType::U64 => self.context.i64_type().into(),
            LirType::I128 | LirType::U128 => self.context.i128_type().into(),
            LirType::I256 | LirType::U256 => self
                .context
                .custom_width_int_type(NonZeroU32::new(256).unwrap())
                .expect("256 is a valid LLVM integer width")
                .into(),
            LirType::Vector(elem, lanes) => self
                .lir_type_to_llvm(elem)
                .into_int_type()
                .vec_type(*lanes as u32)
                .into(),
            LirType::Native(t) => self.native_type_to_llvm(*t).into(),
            // LLVM 21 opaque pointer.
            LirType::Ptr(_) => self.context.ptr_type(AddressSpace::default()).into(),
            LirType::Arr(_, _) | LirType::Struct(_) => {
                panic!(
                    "LlvmBackend: aggregate LirType {:?} passed to lir_type_to_llvm — \
                     backend uses LirAbi::DEFAULT; codegen should have flattened this",
                    ty
                )
            }
            _ => panic!("LlvmBackend: unsupported LirType {:?}", ty),
        }
    }

    /// Map a `NativeType` (GF-field element) to the closest LLVM integer type.
    fn native_type_to_llvm(&self, t: NativeType) -> inkwell::types::IntType<'ctx> {
        match t {
            NativeType::Bit => self.context.bool_type(),
            NativeType::_8 | NativeType::AES8 => self.context.i8_type(),
            NativeType::_16 => self.context.i16_type(),
            NativeType::_32 => self.context.i32_type(),
            NativeType::_64 | NativeType::Galois64 => self.context.i64_type(),
            NativeType::_128 => self.context.i128_type(),
            NativeType::_256 => self
                .context
                .custom_width_int_type(NonZeroU32::new(256).unwrap())
                .expect("256 is a valid LLVM integer width"),
            _ => self.context.i64_type(),
        }
    }

    /// Declare an external function in the module (idempotent).
    fn declare_extern(&mut self, name: &str, fn_type: FunctionType<'ctx>) -> FunctionValue<'ctx> {
        if let Some(&fv) = self.extern_cache.get(name) {
            return fv;
        }
        let fv = self
            .module
            .add_function(name, fn_type, Some(Linkage::External));
        self.extern_cache.insert(name.to_string(), fv);
        fv
    }

    /// Call an external whose logical result list is represented as a native
    /// LLVM struct when it contains more than one value.  This keeps each
    /// result's exact scalar/vector type at the ABI boundary instead of
    /// silently discarding every result after the first.
    fn call_extern_results(
        &mut self,
        name: &str,
        arg_tys: &[LirType],
        args: &[LlvmValue<'ctx>],
        ret_tys: &[LirType],
    ) -> Vec<LlvmValue<'ctx>> {
        assert!(
            !ret_tys.is_empty(),
            "external result list must be non-empty"
        );
        if ret_tys.len() == 1 {
            return self.call_extern(name, arg_tys, args, Some(ret_tys[0].clone()));
        }

        let params = arg_tys
            .iter()
            .map(|ty| self.lir_type_to_llvm(ty).into())
            .collect::<Vec<BasicMetadataTypeEnum<'ctx>>>();
        let fields = ret_tys
            .iter()
            .map(|ty| self.lir_type_to_llvm(ty))
            .collect::<Vec<BasicTypeEnum<'ctx>>>();
        let result_ty = self.context.struct_type(&fields, false);
        let function = self.declare_extern(
            &self.name_config.apply(name),
            result_ty.fn_type(&params, false),
        );
        let args = args
            .iter()
            .map(|value| value.inner.into())
            .collect::<Vec<BasicMetadataValueEnum<'ctx>>>();
        let result = self
            .builder
            .build_call(function, &args, "")
            .unwrap()
            .try_as_basic_value()
            .unwrap_basic()
            .into_struct_value();
        ret_tys
            .iter()
            .enumerate()
            .map(|(index, ty)| LlvmValue {
                inner: self
                    .builder
                    .build_extract_value(result, index as u32, "")
                    .unwrap(),
                ty: ty.clone(),
            })
            .collect()
    }

    /// Build a binary integer operation, returning a fresh value.
    fn int_binop(
        &mut self,
        lhs: LlvmValue<'ctx>,
        rhs: LlvmValue<'ctx>,
        op: IntBinOp,
    ) -> LlvmValue<'ctx> {
        let ty = lhs.ty.clone();
        let result = match (lhs.inner, rhs.inner) {
            (BasicValueEnum::IntValue(l), BasicValueEnum::IntValue(r)) => {
                let value = match op {
                    IntBinOp::Add => self.builder.build_int_add(l, r, ""),
                    IntBinOp::Sub => self.builder.build_int_sub(l, r, ""),
                    IntBinOp::Mul => self.builder.build_int_mul(l, r, ""),
                    IntBinOp::Udiv => self.builder.build_int_unsigned_div(l, r, ""),
                    IntBinOp::Sdiv => self.builder.build_int_signed_div(l, r, ""),
                    IntBinOp::And => self.builder.build_and(l, r, ""),
                    IntBinOp::Or => self.builder.build_or(l, r, ""),
                    IntBinOp::Xor => self.builder.build_xor(l, r, ""),
                    IntBinOp::Shl => self.builder.build_left_shift(l, r, ""),
                    IntBinOp::Lshr => self.builder.build_right_shift(l, r, false, ""),
                    IntBinOp::Ashr => self.builder.build_right_shift(l, r, true, ""),
                }
                .unwrap();
                value.into()
            }
            (BasicValueEnum::VectorValue(l), BasicValueEnum::VectorValue(r)) => {
                let value = match op {
                    IntBinOp::Add => self.builder.build_int_add(l, r, ""),
                    IntBinOp::Sub => self.builder.build_int_sub(l, r, ""),
                    IntBinOp::Mul => self.builder.build_int_mul(l, r, ""),
                    IntBinOp::Udiv => self.builder.build_int_unsigned_div(l, r, ""),
                    IntBinOp::Sdiv => self.builder.build_int_signed_div(l, r, ""),
                    IntBinOp::And => self.builder.build_and(l, r, ""),
                    IntBinOp::Or => self.builder.build_or(l, r, ""),
                    IntBinOp::Xor => self.builder.build_xor(l, r, ""),
                    IntBinOp::Shl => self.builder.build_left_shift(l, r, ""),
                    IntBinOp::Lshr => self.builder.build_right_shift(l, r, false, ""),
                    IntBinOp::Ashr => self.builder.build_right_shift(l, r, true, ""),
                }
                .unwrap();
                value.into()
            }
            _ => panic!("LLVM integer operation requires matching scalar or vector integer values"),
        };
        LlvmValue { inner: result, ty }
    }

    /// Return the current insertion block (panics if builder is not positioned).
    fn current_block(&self) -> inkwell::basic_block::BasicBlock<'ctx> {
        self.builder
            .get_insert_block()
            .expect("LlvmBackend: builder has no current block")
    }

    /// Add one incoming edge (from `pred_block`) to every PHI in `block`,
    /// matching `args` positionally. Shared by `switch`/`dyn_jump`, which
    /// each wire N successor blocks from a single predecessor — the same
    /// per-block logic `jump`/`branch` already inline for their one/two
    /// successors.
    fn wire_phis(
        &mut self,
        block: LlvmBlock<'ctx>,
        args: &[LlvmValue<'ctx>],
        pred_block: inkwell::basic_block::BasicBlock<'ctx>,
    ) {
        let phi_count = self
            .current
            .as_ref()
            .map(|s| s.blocks[block.id as usize].phi_values.len())
            .unwrap_or(0);
        assert_eq!(
            args.len(),
            phi_count,
            "wire_phis: arg count ({}) != block param count ({}) for block{}",
            args.len(),
            phi_count,
            block.id
        );
        for i in 0..phi_count {
            let phi = self.current.as_ref().unwrap().blocks[block.id as usize].phi_values[i].0;
            phi.add_incoming(&[(&args[i].inner, pred_block)]);
        }
    }

    /// Resolve `name` to a `FunctionValue`, reusing an existing declaration
    /// (from a prior sibling `call`, or the function's own earlier
    /// `begin_function`) if one exists, otherwise forward-declaring one from
    /// `param_tys`/`ret`. This is what makes forward references and mutual
    /// recursion between module-local functions safe: whichever of
    /// `call`/`begin_function` runs first creates the `FunctionValue`; the
    /// other reuses it rather than risking LLVM auto-uniquifying a
    /// name-colliding second function.
    fn resolve_or_declare_sibling(
        &mut self,
        name: &str,
        param_tys: &[LirType],
        ret: &Option<LirType>,
    ) -> FunctionValue<'ctx> {
        if let Some(existing) = self.module.get_function(name) {
            return existing;
        }
        let param_llvm_tys: Vec<BasicMetadataTypeEnum<'ctx>> = param_tys
            .iter()
            .map(|ty| self.lir_type_to_llvm(ty).into())
            .collect();
        let fn_type = match ret.as_ref().map(|ty| self.lir_type_to_llvm(ty)) {
            Some(r) => r.fn_type(&param_llvm_tys, false),
            None => self.context.void_type().fn_type(&param_llvm_tys, false),
        };
        self.module.add_function(name, fn_type, None)
    }
}

// ============================================================================
// LirTarget implementation
// ============================================================================

impl<'ctx> LirTarget for LlvmBackend<'ctx> {
    type Value = LlvmValue<'ctx>;
    type Block = LlvmBlock<'ctx>;

    // ---- ABI ----------------------------------------------------------------

    fn abi(&self) -> LirAbi {
        LirAbi::DEFAULT
    }

    // ---- Type registration --------------------------------------------------

    fn define_struct(&mut self, def: StructDef) -> StructId {
        let id = self.next_struct_id;
        self.next_struct_id += 1;

        // Build LLVM struct type from field types.
        // With LirAbi::DEFAULT, fields should all be scalars (no nested Arr/Struct
        // in practice), but we handle them gracefully anyway.
        let field_types: Vec<BasicTypeEnum<'ctx>> = def
            .fields
            .iter()
            .map(|f| self.lir_type_to_llvm(&f.ty))
            .collect();
        // Use opaque_struct_type + set_body to get a named struct in the IR.
        let struct_ty = self.context.opaque_struct_type(&def.name);
        struct_ty.set_body(&field_types, false);

        self.struct_defs.push(def.clone());
        self.struct_names.push(def.name.clone());
        self.struct_llvm_types.push(struct_ty);
        id
    }

    // ---- Value type query ---------------------------------------------------

    fn value_scalar_type(&self, val: &LlvmValue<'ctx>) -> LirType {
        val.ty.clone()
    }

    // ---- Function management ------------------------------------------------

    fn begin_function(
        &mut self,
        name: &str,
        params: &[LirType],
        ret: Option<LirType>,
    ) -> (LlvmBlock<'ctx>, Vec<Vec<LlvmValue<'ctx>>>) {
        assert!(
            self.current.is_none(),
            "begin_function called while already inside a function"
        );

        // Reuse an existing forward declaration (from a sibling `call` that
        // ran before this function's own body) if one exists, rather than
        // declaring a second, name-colliding function.
        let resolved_name = self.name_config.apply(name);
        let func = self.resolve_or_declare_sibling(&resolved_name, params, &ret);

        // Create the entry block.
        let entry_llvm = self.context.append_basic_block(func, "block0");

        self.current = Some(FunctionState {
            func,
            ret_ty: ret,
            blocks: vec![BlockState {
                phi_values: Vec::new(),
            }],
        });

        // Wrap each LLVM function parameter as LlvmValue.
        let param_vals: Vec<Vec<LlvmValue<'ctx>>> = func
            .get_params()
            .iter()
            .zip(params.iter())
            .map(|(param, ty)| {
                vec![LlvmValue {
                    inner: *param,
                    ty: ty.clone(),
                }]
            })
            .collect();

        (
            LlvmBlock {
                inner: entry_llvm,
                id: 0,
            },
            param_vals,
        )
    }

    fn end_function(&mut self) {
        let _ = self
            .current
            .take()
            .expect("end_function called outside a function");
        // Builder is left wherever the last instruction placed it; the caller
        // must have emitted a terminator in every block.
    }

    // ---- Block management ---------------------------------------------------

    fn create_block(&mut self) -> LlvmBlock<'ctx> {
        let (id, func) = {
            let state = self.state();
            let id = state.blocks.len() as u32;
            (id, state.func)
        };
        let name = format!("block{id}");
        let llvm_block = self.context.append_basic_block(func, &name);
        self.state().blocks.push(BlockState {
            phi_values: Vec::new(),
        });
        LlvmBlock {
            inner: llvm_block,
            id,
        }
    }

    /// Create a PHI node at the *start* of `block` and return its value.
    ///
    /// The builder is temporarily repositioned to the end of `block`
    /// (which is empty at this point — params are declared before any
    /// instructions are emitted into the block) to emit the PHI, then
    /// restored to the previous insertion point.
    fn add_block_param(&mut self, block: LlvmBlock<'ctx>, ty: LirType) -> LlvmValue<'ctx> {
        // Save current insertion position.
        let saved = self.builder.get_insert_block();

        // Move to the target block to emit the PHI at its head.
        self.builder.position_at_end(block.inner);

        let llvm_ty = self.lir_type_to_llvm(&ty);
        let phi = self.builder.build_phi(llvm_ty, "").unwrap();

        // Restore insertion position.
        if let Some(prev) = saved {
            self.builder.position_at_end(prev);
        }

        // Record the PHI for incoming-edge wiring in jump/branch.
        let state = self.state();
        state.blocks[block.id as usize]
            .phi_values
            .push((phi, ty.clone()));

        LlvmValue {
            inner: phi.as_basic_value(),
            ty,
        }
    }

    fn switch_to_block(&mut self, block: LlvmBlock<'ctx>) {
        self.builder.position_at_end(block.inner);
    }

    // ---- Constants ----------------------------------------------------------

    fn iconst(&mut self, ty: LirType, val: i64) -> LlvmValue<'ctx> {
        let llvm_ty = self.lir_type_to_llvm(&ty);
        let int_ty = llvm_ty.into_int_type();
        // sign_extend=true so negative i64 values are represented correctly
        // in narrower types (the truncation to the correct bit width is
        // implicit in `const_int`'s bit pattern).
        let int_val = int_ty.const_int(val as u64, true);
        LlvmValue {
            inner: int_val.into(),
            ty,
        }
    }

    // ---- Arithmetic ---------------------------------------------------------

    fn add(&mut self, lhs: LlvmValue<'ctx>, rhs: LlvmValue<'ctx>) -> LlvmValue<'ctx> {
        self.int_binop(lhs, rhs, IntBinOp::Add)
    }

    fn sub(&mut self, lhs: LlvmValue<'ctx>, rhs: LlvmValue<'ctx>) -> LlvmValue<'ctx> {
        self.int_binop(lhs, rhs, IntBinOp::Sub)
    }

    fn mul(&mut self, lhs: LlvmValue<'ctx>, rhs: LlvmValue<'ctx>) -> LlvmValue<'ctx> {
        self.int_binop(lhs, rhs, IntBinOp::Mul)
    }

    fn udiv(&mut self, lhs: LlvmValue<'ctx>, rhs: LlvmValue<'ctx>) -> LlvmValue<'ctx> {
        self.int_binop(lhs, rhs, IntBinOp::Udiv)
    }

    fn sdiv(&mut self, lhs: LlvmValue<'ctx>, rhs: LlvmValue<'ctx>) -> LlvmValue<'ctx> {
        self.int_binop(lhs, rhs, IntBinOp::Sdiv)
    }

    // ---- Bitwise ------------------------------------------------------------

    fn and(&mut self, lhs: LlvmValue<'ctx>, rhs: LlvmValue<'ctx>) -> LlvmValue<'ctx> {
        self.int_binop(lhs, rhs, IntBinOp::And)
    }

    fn or(&mut self, lhs: LlvmValue<'ctx>, rhs: LlvmValue<'ctx>) -> LlvmValue<'ctx> {
        self.int_binop(lhs, rhs, IntBinOp::Or)
    }

    fn xor(&mut self, lhs: LlvmValue<'ctx>, rhs: LlvmValue<'ctx>) -> LlvmValue<'ctx> {
        self.int_binop(lhs, rhs, IntBinOp::Xor)
    }

    fn not(&mut self, val: LlvmValue<'ctx>) -> LlvmValue<'ctx> {
        let ty = val.ty.clone();
        let v = val.inner.into_int_value();
        let result = if ty == LirType::Bool {
            // LLVM has no logical NOT; XOR with true (1) is correct for i1.
            let true_val = self.context.bool_type().const_int(1, false);
            self.builder.build_xor(v, true_val, "").unwrap()
        } else {
            self.builder.build_not(v, "").unwrap()
        };
        LlvmValue {
            inner: result.into(),
            ty,
        }
    }

    fn shl(&mut self, val: LlvmValue<'ctx>, shift: LlvmValue<'ctx>) -> LlvmValue<'ctx> {
        self.int_binop(val, shift, IntBinOp::Shl)
    }

    fn lshr(&mut self, val: LlvmValue<'ctx>, shift: LlvmValue<'ctx>) -> LlvmValue<'ctx> {
        // sign_extend = false → logical (zero-fill) right shift
        self.int_binop(val, shift, IntBinOp::Lshr)
    }

    fn ashr(&mut self, val: LlvmValue<'ctx>, shift: LlvmValue<'ctx>) -> LlvmValue<'ctx> {
        // sign_extend = true → arithmetic (sign-fill) right shift
        self.int_binop(val, shift, IntBinOp::Ashr)
    }

    // ---- Comparison ---------------------------------------------------------

    fn icmp(
        &mut self,
        pred: IcmpPred,
        lhs: LlvmValue<'ctx>,
        rhs: LlvmValue<'ctx>,
    ) -> LlvmValue<'ctx> {
        use inkwell::IntPredicate;
        let llvm_pred = match pred {
            IcmpPred::Eq => IntPredicate::EQ,
            IcmpPred::Ne => IntPredicate::NE,
            IcmpPred::Ult => IntPredicate::ULT,
            IcmpPred::Ule => IntPredicate::ULE,
            IcmpPred::Ugt => IntPredicate::UGT,
            IcmpPred::Uge => IntPredicate::UGE,
            IcmpPred::Slt => IntPredicate::SLT,
            IcmpPred::Sle => IntPredicate::SLE,
            IcmpPred::Sgt => IntPredicate::SGT,
            IcmpPred::Sge => IntPredicate::SGE,
        };
        let l = lhs.inner.into_int_value();
        let r = rhs.inner.into_int_value();
        let result = self.builder.build_int_compare(llvm_pred, l, r, "").unwrap();
        LlvmValue {
            inner: result.into(),
            ty: LirType::Bool,
        }
    }

    // ---- Conversions --------------------------------------------------------

    fn zext(&mut self, val: LlvmValue<'ctx>, dst_ty: LirType) -> LlvmValue<'ctx> {
        let llvm_dst = self.lir_type_to_llvm(&dst_ty).into_int_type();
        let src = val.inner.into_int_value();
        let result = self.builder.build_int_z_extend(src, llvm_dst, "").unwrap();
        LlvmValue {
            inner: result.into(),
            ty: dst_ty,
        }
    }

    fn sext(&mut self, val: LlvmValue<'ctx>, dst_ty: LirType) -> LlvmValue<'ctx> {
        let llvm_dst = self.lir_type_to_llvm(&dst_ty).into_int_type();
        let src = val.inner.into_int_value();
        let result = self.builder.build_int_s_extend(src, llvm_dst, "").unwrap();
        LlvmValue {
            inner: result.into(),
            ty: dst_ty,
        }
    }

    fn trunc(&mut self, val: LlvmValue<'ctx>, dst_ty: LirType) -> LlvmValue<'ctx> {
        let llvm_dst = self.lir_type_to_llvm(&dst_ty).into_int_type();
        let src = val.inner.into_int_value();
        let result = self.builder.build_int_truncate(src, llvm_dst, "").unwrap();
        LlvmValue {
            inner: result.into(),
            ty: dst_ty,
        }
    }

    // ---- Select -------------------------------------------------------------

    fn select(
        &mut self,
        cond: LlvmValue<'ctx>,
        then_val: LlvmValue<'ctx>,
        else_val: LlvmValue<'ctx>,
    ) -> LlvmValue<'ctx> {
        let ty = then_val.ty.clone();
        let c = cond.inner.into_int_value();
        let result = self
            .builder
            .build_select(c, then_val.inner, else_val.inner, "")
            .unwrap();
        LlvmValue { inner: result, ty }
    }

    // ---- Terminators --------------------------------------------------------

    fn jump(&mut self, target: LlvmBlock<'ctx>, branch: BranchTarget<LlvmValue<'ctx>>) {
        let args = &branch.args[..];
        let pred_block = self.current_block();

        // Wire incoming edges into the target block's PHI nodes.
        // Safety: we hold a shared reference to `self.current.blocks` via the
        // block id, so we read phi values first before calling the builder.
        let phi_count = self
            .current
            .as_ref()
            .map(|s| s.blocks[target.id as usize].phi_values.len())
            .unwrap_or(0);

        assert_eq!(
            args.len(),
            phi_count,
            "jump: arg count ({}) != block param count ({})",
            args.len(),
            phi_count
        );

        for i in 0..phi_count {
            let phi = self.current.as_ref().unwrap().blocks[target.id as usize].phi_values[i].0;
            phi.add_incoming(&[(&args[i].inner, pred_block)]);
        }

        self.builder
            .build_unconditional_branch(target.inner)
            .unwrap();
    }

    fn branch(
        &mut self,
        cond: LlvmValue<'ctx>,
        then_block: LlvmBlock<'ctx>,
        then_branch: BranchTarget<LlvmValue<'ctx>>,
        else_block: LlvmBlock<'ctx>,
        else_branch: BranchTarget<LlvmValue<'ctx>>,
    ) {
        let then_args = &then_branch.args[..];
        let else_args = &else_branch.args[..];

        // When both targets are the same block, emitting a conditional branch
        // with identical predecessors would produce a PHI node with two entries
        // from the same predecessor, which LLVM forbids.  Convert to `select`
        // per argument + unconditional branch instead.
        if then_block.id == else_block.id {
            let selected: Vec<LlvmValue<'ctx>> = then_args
                .iter()
                .zip(else_args.iter())
                .map(|(t, e)| self.select(cond.clone(), t.clone(), e.clone()))
                .collect();
            self.jump(then_block, BranchTarget::args(selected));
            return;
        }

        let pred_block = self.current_block();

        // Wire then-block PHIs.
        let then_phi_count = self
            .current
            .as_ref()
            .map(|s| s.blocks[then_block.id as usize].phi_values.len())
            .unwrap_or(0);
        assert_eq!(then_args.len(), then_phi_count);
        for i in 0..then_phi_count {
            let phi = self.current.as_ref().unwrap().blocks[then_block.id as usize].phi_values[i].0;
            phi.add_incoming(&[(&then_args[i].inner, pred_block)]);
        }

        // Wire else-block PHIs.
        let else_phi_count = self
            .current
            .as_ref()
            .map(|s| s.blocks[else_block.id as usize].phi_values.len())
            .unwrap_or(0);
        assert_eq!(else_args.len(), else_phi_count);
        for i in 0..else_phi_count {
            let phi = self.current.as_ref().unwrap().blocks[else_block.id as usize].phi_values[i].0;
            phi.add_incoming(&[(&else_args[i].inner, pred_block)]);
        }

        let cond_val = cond.inner.into_int_value();
        self.builder
            .build_conditional_branch(cond_val, then_block.inner, else_block.inner)
            .unwrap();
    }

    fn ret(&mut self, vals: &[LlvmValue<'ctx>]) {
        match vals {
            [] => {
                self.builder.build_return(None).unwrap();
            }
            [single] => {
                self.builder.build_return(Some(&single.inner)).unwrap();
            }
            _ => {
                // Multiple return values should not appear with LirAbi::DEFAULT
                // (aggregates are flattened to single scalars). Panic defensively.
                panic!(
                    "LlvmBackend::ret: {} values — LirAbi::DEFAULT \
                     should produce at most one return scalar",
                    vals.len()
                );
            }
        }
    }

    /// Native LLVM `switch`. Each case's (and the default's) target block
    /// gets exactly one incoming PHI edge from the current block, matching
    /// LLVM's own model — a `switch` has one predecessor block but many
    /// successor edges, just like `branch`'s then/else pair generalized to
    /// N+1 successors.
    fn switch(
        &mut self,
        index: LlvmValue<'ctx>,
        cases: &[(i64, LlvmBlock<'ctx>, BranchTarget<LlvmValue<'ctx>>)],
        default_block: LlvmBlock<'ctx>,
        default_branch: BranchTarget<LlvmValue<'ctx>>,
    ) {
        let pred_block = self.current_block();
        let index_ty = index.ty.clone();
        let idx_val = index.inner.into_int_value();

        self.wire_phis(default_block, &default_branch.args, pred_block);

        let llvm_int_ty = self.lir_type_to_llvm(&index_ty).into_int_type();
        let mut llvm_cases = Vec::with_capacity(cases.len());
        for (key, block, branch) in cases {
            self.wire_phis(*block, &branch.args, pred_block);
            llvm_cases.push((llvm_int_ty.const_int(*key as u64, true), block.inner));
        }

        self.builder
            .build_switch(idx_val, default_block.inner, &llvm_cases)
            .unwrap();
    }

    /// Native LLVM `blockaddress` constant.
    ///
    /// Represented as an opaque `LirType::Ptr(I8)` value — it's never
    /// dereferenced, only compared/passed through to `dyn_jump`.
    ///
    /// # Panics
    ///
    /// LLVM cannot take the address of a function's *entry* block
    /// (`BasicBlock::get_address` returns `None` there) — panics with a
    /// clear message rather than silently producing a null pointer.
    fn block_addr(&mut self, block: LlvmBlock<'ctx>) -> LlvmValue<'ctx> {
        let addr = unsafe { block.inner.get_address() }.unwrap_or_else(|| {
            panic!(
                "LlvmBackend::block_addr: cannot take the address of a function's entry block \
                 (block{} is block0)",
                block.id
            )
        });
        LlvmValue {
            inner: addr.into(),
            ty: LirType::Ptr(Box::new(LirType::I8)),
        }
    }

    /// Native LLVM `indirectbr`.
    fn dyn_jump(
        &mut self,
        index: LlvmValue<'ctx>,
        destinations: &[LlvmBlock<'ctx>],
        branch: BranchTarget<LlvmValue<'ctx>>,
    ) {
        let pred_block = self.current_block();
        for block in destinations {
            self.wire_phis(*block, &branch.args, pred_block);
        }
        let dest_blocks: Vec<_> = destinations.iter().map(|b| b.inner).collect();
        self.builder
            .build_indirect_branch(index.inner, &dest_blocks)
            .unwrap();
    }

    // ---- Extern calls -------------------------------------------------------

    fn call_extern(
        &mut self,
        name: &str,
        arg_tys: &[LirType],
        args: &[LlvmValue<'ctx>],
        ret_ty: Option<LirType>,
    ) -> Vec<LlvmValue<'ctx>> {
        // Build the LLVM function type for the extern declaration.
        let param_llvm_tys: Vec<BasicMetadataTypeEnum<'ctx>> = arg_tys
            .iter()
            .map(|ty| self.lir_type_to_llvm(ty).into())
            .collect();

        let fn_type = match ret_ty.as_ref().map(|ty| self.lir_type_to_llvm(ty)) {
            Some(r) => r.fn_type(&param_llvm_tys, false),
            None => self.context.void_type().fn_type(&param_llvm_tys, false),
        };

        let func = self.declare_extern(&self.name_config.apply(name), fn_type);

        // Build the call arguments.
        let call_args: Vec<BasicMetadataValueEnum<'ctx>> =
            args.iter().map(|v| v.inner.into()).collect();

        let call = self.builder.build_call(func, &call_args, "").unwrap();

        match ret_ty {
            None => vec![],
            Some(ty) => {
                let ret_val = call.try_as_basic_value().unwrap_basic();
                vec![LlvmValue { inner: ret_val, ty }]
            }
        }
    }

    // ---- Sibling (intra-module) calls ----------------------------------------

    /// Call another function defined in this same module.
    ///
    /// Reuses [`resolve_or_declare_sibling`](LlvmBackend::resolve_or_declare_sibling)
    /// so the callee resolves correctly whether its own `begin_function` has
    /// already run or not (forward references / mutual recursion). The
    /// actual `call` instruction is identical to
    /// [`call_extern`](Self::call_extern)'s.
    fn call(
        &mut self,
        name: &str,
        arg_tys: &[LirType],
        args: &[LlvmValue<'ctx>],
        ret_ty: Option<LirType>,
    ) -> Vec<LlvmValue<'ctx>> {
        let resolved_name = self.name_config.apply(name);
        let func = self.resolve_or_declare_sibling(&resolved_name, arg_tys, &ret_ty);

        let call_args: Vec<BasicMetadataValueEnum<'ctx>> =
            args.iter().map(|v| v.inner.into()).collect();
        let call = self.builder.build_call(func, &call_args, "").unwrap();

        match ret_ty {
            None => vec![],
            Some(ty) => {
                let ret_val = call.try_as_basic_value().unwrap_basic();
                vec![LlvmValue { inner: ret_val, ty }]
            }
        }
    }

    // ---- External access primitives ----------------------------------------

    fn oracle(
        &mut self,
        name: &str,
        arg_tys: &[LirType],
        args: &[LlvmValue<'ctx>],
        ret_tys: &[LirType],
    ) -> Vec<LlvmValue<'ctx>> {
        self.call_extern_results(&format!("oracle_{name}"), arg_tys, args, ret_tys)
    }

    fn action(
        &mut self,
        name: &str,
        guard: LlvmValue<'ctx>,
        arg_tys: &[LirType],
        args: &[LlvmValue<'ctx>],
        fallbacks: &[LlvmValue<'ctx>],
        ret_tys: &[LirType],
    ) -> Vec<LlvmValue<'ctx>> {
        // Legacy ActionCall returns values; ActionStore is the side-effecting
        // ABI. Preserve this historical form by selecting every native result
        // against its fallback after one struct-returning external call.
        let action_result =
            self.call_extern_results(&format!("action_{name}"), arg_tys, args, ret_tys);
        action_result
            .iter()
            .zip(fallbacks.iter())
            .map(|(r, f)| self.select(guard.clone(), r.clone(), f.clone()))
            .collect()
    }

    fn rng(&mut self, ty: LirType) -> LlvmValue<'ctx> {
        // Strategy: alloca a slot of the target type, call volar_rng(ptr, size),
        // load and return.
        //
        // Expected C signature: void volar_rng(void* out, uint64_t len);
        // We approximate: declare as (ptr, i64) -> void.

        let llvm_ty = self.lir_type_to_llvm(&ty);
        let size_bytes = (llvm_ty
            .size_of()
            .unwrap()
            .get_zero_extended_constant()
            .unwrap_or(8)) as u64;

        // Alloca the slot.
        let slot = self.builder.build_alloca(llvm_ty, "rng_slot").unwrap();

        // Build the volar_rng call.
        let i64_ty = self.context.i64_type();
        let ptr_ty: BasicTypeEnum<'ctx> = self.context.ptr_type(AddressSpace::default()).into();
        let rng_fn_type = self
            .context
            .void_type()
            .fn_type(&[ptr_ty.into(), i64_ty.into()], false);
        let rng_fn_name = self.rng_fn.clone();
        let rng_func = self.declare_extern(&rng_fn_name, rng_fn_type);

        let size_val = i64_ty.const_int(size_bytes, false);
        self.builder
            .build_call(rng_func, &[slot.into(), size_val.into()], "")
            .unwrap();

        // Load and return.
        let loaded = self.builder.build_load(llvm_ty, slot, "").unwrap();
        LlvmValue { inner: loaded, ty }
    }

    fn rng_named(&mut self, name: &str, ty: LirType) -> LlvmValue<'ctx> {
        self.call_extern(&format!("rng_{name}"), &[], &[], Some(ty))
            .into_iter()
            .next()
            .expect("named RNG must return exactly one value")
    }

    fn stack_alloc_ext(&mut self) -> Option<&mut dyn StackAllocExt<Value = LlvmValue<'ctx>>> {
        Some(self)
    }

    fn ptr_index_load(
        &mut self,
        ptr: LlvmValue<'ctx>,
        idx: LlvmValue<'ctx>,
        pointee_ty: &LirType,
    ) -> Vec<LlvmValue<'ctx>> {
        let ext = self.stack_alloc_ext().unwrap();
        let offset_ptr = ext.ptr_offset(ptr, idx);
        let loaded = ext.ptr_load(offset_ptr, pointee_ty.clone());
        vec![loaded]
    }

    fn ptr_index_store(
        &mut self,
        ptr: LlvmValue<'ctx>,
        idx: LlvmValue<'ctx>,
        vals: &[LlvmValue<'ctx>],
        _pointee_ty: &LirType,
    ) {
        assert_eq!(
            vals.len(),
            1,
            "ptr_index_store: expected 1 scalar (DEFAULT ABI)"
        );
        let val = vals[0].clone();
        let ext = self.stack_alloc_ext().unwrap();
        let offset_ptr = ext.ptr_offset(ptr, idx);
        ext.ptr_store(offset_ptr, val);
    }
}

// ============================================================================
// StackAllocExt implementation
// ============================================================================

impl<'ctx> StackAllocExt for LlvmBackend<'ctx> {
    type Value = LlvmValue<'ctx>;

    /// Allocate a stack region for `count` elements of `elem_ty`.
    ///
    /// Emits `alloca elem_ty, count` at the current insertion point.
    /// Returns a `LirType::Ptr(elem_ty)` value.
    ///
    /// Note: For canonical SSA form, callers should ensure alloca is emitted
    /// in the function entry block.  `volar-lir-codegen` does this by default
    /// when using the `C_NATIVE`-style ABI; with `DEFAULT` ABI alloca is
    /// emitted wherever the builder currently points.
    fn alloca(&mut self, elem_ty: LirType, count: usize) -> LlvmValue<'ctx> {
        let llvm_elem_ty = self.lir_type_to_llvm(&elem_ty);
        let count_val = self.context.i64_type().const_int(count as u64, false);
        let ptr = self
            .builder
            .build_array_alloca(llvm_elem_ty, count_val, "")
            .unwrap();
        LlvmValue {
            inner: ptr.into(),
            ty: LirType::Ptr(Box::new(elem_ty)),
        }
    }

    /// Load through a typed pointer.
    ///
    /// LLVM 21 uses opaque pointers; the pointee type must be provided
    /// explicitly via `ty`.
    fn ptr_load(&mut self, ptr: LlvmValue<'ctx>, ty: LirType) -> LlvmValue<'ctx> {
        let llvm_ty = self.lir_type_to_llvm(&ty);
        let ptr_val = ptr.inner.into_pointer_value();
        let loaded = self.builder.build_load(llvm_ty, ptr_val, "").unwrap();
        LlvmValue { inner: loaded, ty }
    }

    /// Store `val` through `ptr`.
    fn ptr_store(&mut self, ptr: LlvmValue<'ctx>, val: LlvmValue<'ctx>) {
        let ptr_val = ptr.inner.into_pointer_value();
        self.builder.build_store(ptr_val, val.inner).unwrap();
    }

    /// Element-wise pointer offset: `ptr + idx` elements.
    ///
    /// Uses GEP with the pointee type inferred from `ptr.ty`.
    fn ptr_offset(&mut self, ptr: LlvmValue<'ctx>, idx: LlvmValue<'ctx>) -> LlvmValue<'ctx> {
        let pointee_ty = match &ptr.ty {
            LirType::Ptr(inner) => self.lir_type_to_llvm(inner),
            other => panic!("ptr_offset: expected Ptr type, got {:?}", other),
        };
        let ptr_val = ptr.inner.into_pointer_value();
        let idx_val = idx.inner.into_int_value();
        let gep = unsafe {
            self.builder
                .build_gep(pointee_ty, ptr_val, &[idx_val], "")
                .unwrap()
        };
        LlvmValue {
            inner: gep.into(),
            ty: ptr.ty,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use volar_ir::{
        boolar::{BIrBlock, BIrBlocks, BIrStmt, BIrTarget, BIrTerminator, LaneId},
        ir::{
            IRBlock, IRBlockTargetId, IRBlocks, IRBranchTarget, IRStmt, IRTerminator, IRType,
            IRTypes, IRVarId,
        },
    };
    use volar_ir_common::{ActionTarget, Node, StorageId, Type};
    use volar_ir_passes::lower_lir::{lower_biir, lower_ir};

    fn external_bit_fixture() -> BIrBlocks {
        BIrBlocks {
            blocks: vec![BIrBlock {
                params: 4,
                stmts: vec![
                    Node::new(
                        BIrStmt::OracleBit {
                            name: "lookup".into(),
                            args: vec![IRVarId(1)],
                            bit: 2,
                            occurrence: 7,
                        },
                        (),
                        None,
                    ),
                    Node::new(
                        BIrStmt::RngBit {
                            name: "nonce".into(),
                            bit: 3,
                            occurrence: 8,
                        },
                        (),
                        None,
                    ),
                    Node::new(
                        BIrStmt::ActionStoreBit {
                            name: "commit".into(),
                            guard: IRVarId(0),
                            args: vec![IRVarId(4), IRVarId(5)],
                            fallback: IRVarId(3),
                            storage: StorageId(9),
                            lane: LaneId(4),
                            addr: vec![IRVarId(2)],
                            bit: 1,
                            occurrence: 9,
                        },
                        (),
                        None,
                    ),
                ],
                terminator: BIrTerminator::Jmp(BIrTarget {
                    block: IRBlockTargetId::Return,
                    args: vec![IRVarId(4), IRVarId(5)],
                }),
            }],
            pre_init: vec![],
        }
    }

    #[test]
    fn lowers_direct_external_bits_to_configured_llvm_externs() {
        let mut remap = BTreeMap::new();
        remap.insert("oracle_lookup".into(), "host_lookup_bit".into());
        remap.insert("rng_nonce".into(), "host_nonce_bit".into());
        remap.insert("action_commit".into(), "host_commit_bit".into());
        let context = Context::create();
        let mut backend =
            LlvmBackend::new(&context, "external_bits").with_name_config(NameConfig {
                prefix: String::new(),
                remap,
            });
        lower_biir(&external_bit_fixture(), "run", &mut backend);
        let ir = backend.finish().print_to_string().to_string();
        assert!(
            ir.contains("declare i1 @host_lookup_bit(i1, i32, i64)"),
            "{ir}"
        );
        assert!(ir.contains("declare i1 @host_nonce_bit(i32, i64)"), "{ir}");
        assert!(
            ir.contains("declare void @host_commit_bit(i1, i1, i1, i1, i64, i32, i64, i32, i64)"),
            "{ir}"
        );
    }

    #[test]
    fn preserves_packed_i256_and_lane_vectors_in_native_llvm_ir() {
        let context = Context::create();
        let mut backend = LlvmBackend::new(&context, "wide_vectors");
        let vector = LirType::Vector(Box::new(LirType::U32), 4);
        let (entry, params) = backend.begin_function(
            "vector_add",
            &[vector.clone(), vector.clone()],
            Some(vector),
        );
        backend.switch_to_block(entry);
        let sum = backend.add(params[0][0].clone(), params[1][0].clone());
        backend.ret(&[sum]);
        backend.end_function();

        let (entry, params) = backend.begin_function(
            "wide_xor",
            &[LirType::U256, LirType::U256],
            Some(LirType::U256),
        );
        backend.switch_to_block(entry);
        let value = backend.xor(params[0][0].clone(), params[1][0].clone());
        backend.ret(&[value]);
        backend.end_function();

        let ir = backend.finish().print_to_string().to_string();
        assert!(ir.contains("<4 x i32>"), "{ir}");
        assert!(ir.contains("i256"), "{ir}");
        assert!(ir.contains("add <4 x i32>"), "{ir}");
        assert!(ir.contains("xor i256"), "{ir}");
    }

    #[test]
    fn high_volar_ir_keeps_wide_and_vector_return_abi() {
        fn rng_program(ty: volar_ir::ir::IRTypeId, name: &str) -> IRBlocks {
            let mut block = IRBlock {
                params: vec![],
                stmts: vec![],
                terminator: IRTerminator::Jmp {
                    target: IRBranchTarget::new(IRBlockTargetId::Return, vec![IRVarId(0)]),
                },
            };
            block.push_stmt(
                IRStmt::Rng {
                    name: name.into(),
                    ty,
                },
                (),
            );
            IRBlocks::new(vec![block])
        }

        let mut types = IRTypes::new();
        let wide = types.primitive(Type::_256);
        let word = types.primitive(Type::_32);
        let vector = types.push(IRType::Vec(4, word));
        let context = Context::create();
        let mut backend = LlvmBackend::new(&context, "typed_high_ir");
        lower_ir(&rng_program(wide, "wide"), &types, "run_wide", &mut backend);
        lower_ir(
            &rng_program(vector, "vector"),
            &types,
            "run_vector",
            &mut backend,
        );

        let ir = backend.finish().print_to_string().to_string();
        assert!(ir.contains("define i256 @run_wide()"), "{ir}");
        assert!(ir.contains("define <4 x i32> @run_vector()"), "{ir}");
        assert!(ir.contains("declare i256 @rng_wide()"), "{ir}");
        assert!(ir.contains("declare <4 x i32> @rng_vector()"), "{ir}");
        assert!(!ir.contains("define i64 @run_wide()"), "{ir}");
    }

    #[test]
    fn high_volar_externals_keep_native_arguments_results_and_action_storage() {
        let mut types = IRTypes::new();
        let bit = types.bit();
        let wide = types.primitive(Type::_256);
        let word = types.primitive(Type::_32);
        let lanes = types.push(IRType::Vec(4, word));
        let pair = types.push(IRType::Tuple(vec![wide, lanes]));
        let address = types.primitive(Type::_64);

        let mut oracle_block = IRBlock {
            params: vec![wide],
            stmts: vec![],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, vec![IRVarId(2)]),
            },
        };
        oracle_block.push_stmt(
            IRStmt::OracleCall {
                name: "pair".into(),
                args: vec![IRVarId(0)],
                output_tys: vec![wide, lanes],
                result_ty: pair,
            },
            (),
        );
        oracle_block.push_stmt(
            IRStmt::OracleOutput {
                call: IRVarId(1),
                idx: 0,
                ty: wide,
            },
            (),
        );

        let mut action_block = IRBlock {
            params: vec![bit, wide, wide, address],
            stmts: vec![],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, vec![IRVarId(1)]),
            },
        };
        action_block.push_stmt(
            IRStmt::ActionStore {
                name: "commit".into(),
                guard: IRVarId(0),
                args: vec![IRVarId(1)],
                fallbacks: vec![IRVarId(2)],
                output_tys: vec![wide],
                targets: vec![ActionTarget {
                    storage: StorageId(7),
                    addr: IRVarId(3),
                }],
            },
            (),
        );

        let context = Context::create();
        let mut backend = LlvmBackend::new(&context, "typed_externals");
        lower_ir(
            &IRBlocks::new(vec![oracle_block]),
            &types,
            "oracle_wrapper",
            &mut backend,
        );
        lower_ir(
            &IRBlocks::new(vec![action_block]),
            &types,
            "action_wrapper",
            &mut backend,
        );
        let ir = backend.finish().print_to_string().to_string();
        assert!(
            ir.contains("declare { i256, <4 x i32> } @oracle_pair(i256)"),
            "{ir}"
        );
        assert!(ir.contains("extractvalue { i256, <4 x i32> }"), "{ir}");
        assert!(
            ir.contains("declare void @action_commit(i256, i1, i256, i64, i32, i64)"),
            "{ir}"
        );
    }
}
