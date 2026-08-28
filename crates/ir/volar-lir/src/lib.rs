// @reliability: normal
// @ai: assisted
//! `LirTarget` — low-level IR builder trait with block parameters.
//!
//! Backends receive a stream of builder calls and produce whatever output
//! format they target (C text, machine code, etc.).
//!
//! # Block-parameter SSA
//!
//! Control-flow join points are block parameters (Cranelift / MLIR style).
//! A phi-node `%v = phi [%a, bb0], [%b, bb1]` becomes: block `bb_join(p0)`
//! with `jump bb_join(%a)` from `bb0` and `jump bb_join(%b)` from `bb1`.
//!
//! # Types
//!
//! `LirType` is `Clone` (not `Copy`) because `Arr` contains a boxed element
//! type. Use `.clone()` when you need multiple copies.

#![no_std]
extern crate alloc;

use alloc::{boxed::Box, string::String, vec::Vec};
use volar_ir_common::{ReentryHint, Type as NativeType};

mod generated;
pub use generated::{ActionStoreTarget, FieldDef, IcmpPred, NameConfig, StructDef};

pub mod circuits;
pub use circuits::{
    BitCircuitBuilder, FrameLayout, PACK_W, StackPtr, StorageEmitter, n_packs, pack_bits,
    unpack_words,
};

// ============================================================================
// Name configuration
// ============================================================================

/// Controls how a backend maps logical names to emitted names.
///
/// Applied to every name a backend **defines or calls**:
/// - Functions registered via [`LirTarget::begin_function`]
/// - External functions declared via [`LirTarget::call_extern`]
/// - Oracle calls (`oracle_<name>`) and action calls (`action_<name>`)
///
/// The backend's `rng_fn` field (where present) is **not** subject to
/// `NameConfig`; it is always used verbatim as an absolute name.
///
/// # Resolution order
/// 1. If `remap` contains the original (un-prefixed) `name`, return `remap[name]`.
/// 2. Otherwise, prepend `prefix` to `name` and return the result.
///
/// An empty `prefix` and empty `remap` (the default) is the identity.
impl NameConfig {
    /// Apply this configuration to `name`.
    pub fn apply(&self, name: &str) -> String {
        if let Some(mapped) = self.remap.get(name) {
            return mapped.clone();
        }
        if self.prefix.is_empty() {
            String::from(name)
        } else {
            alloc::format!("{}{}", self.prefix, name)
        }
    }
}

// ============================================================================
// Types
// ============================================================================

/// Integer/boolean/aggregate types supported by LIR.
///
/// Note: `Clone`, not `Copy` — `Arr` boxes its element type.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
#[non_exhaustive]
#[cfg_attr(feature = "rkyv", rkyv(serialize_bounds(
    __S: rkyv::ser::Writer + rkyv::ser::Allocator,
    __S::Error: rkyv::rancor::Source,
)))]
#[cfg_attr(feature = "rkyv", rkyv(bytecheck(bounds(__C: rkyv::validation::ArchiveContext))))]
#[cfg_attr(feature = "rkyv", rkyv(deserialize_bounds(__D::Error: rkyv::rancor::Source)))]
pub enum LirType {
    // ---- Scalars ------------------------------------------------------------
    Bool,
    I8,
    U8,
    I16,
    U16,
    I32,
    U32,
    I64,
    U64,
    I128,
    U128,
    /// 256-bit signed integer. This is a packed, little-endian bitvector
    /// when converted from Volar IR's `_256` primitive.
    I256,
    /// 256-bit unsigned integer. This is a packed, little-endian bitvector
    /// when converted from Volar IR's `_256` primitive.
    U256,
    /// A lane-wise vector. Unlike [`Arr`](Self::Arr), this is a first-class
    /// arithmetic value for targets that have vector instructions (LLVM).
    /// Backends without vector registers expose the same value as flattened
    /// little-endian scalar lanes at their ABI boundary.
    Vector(
        #[cfg_attr(feature = "rkyv", rkyv(omit_bounds))] Box<LirType>,
        usize,
    ),
    // ---- Aggregates ---------------------------------------------------------
    /// Fixed-size homogeneous array: `[elem; len]`.
    Arr(
        #[cfg_attr(feature = "rkyv", rkyv(omit_bounds))] Box<LirType>,
        usize,
    ),
    /// Named struct registered via `LirTarget::define_struct`.
    Struct(StructId),
    /// An opaque Volar-IR-native field element, treated as a **single** value
    /// in both the LirTarget API and (for `VolarIrTarget`) the resulting IR.
    ///
    /// `VolarIrTarget` represents this as one `IRVarId` whose `IrType` is
    /// `IrType::Primitive(t)` — **not** as N individual `Bit` vars.  Other
    /// backends (e.g. C) can map it to the closest integer type or a custom
    /// field-element struct.
    ///
    /// Arithmetic on `Native` values in `VolarIrTarget` emits `IRStmt::Poly`
    /// with the native type, giving correct GF-field semantics automatically.
    Native(NativeType),
    /// A typed pointer to a value of the inner type.
    ///
    /// Only meaningful in backends that return `Some` from
    /// [`LirTarget::stack_alloc_ext`] (e.g. `CBackend`).  Circuit backends
    /// (`VolarIrTarget`, `VaffleTarget`) have no memory model and will panic
    /// if they encounter a `Ptr` type.
    Ptr(#[cfg_attr(feature = "rkyv", rkyv(omit_bounds))] Box<LirType>),
}

impl LirType {
    /// Bit width for scalar types. Panics on `Arr`/`Struct`/`Ptr`.
    pub fn bit_width(&self) -> u32 {
        match self {
            LirType::Bool => 1,
            LirType::I8 | LirType::U8 => 8,
            LirType::I16 | LirType::U16 => 16,
            LirType::I32 | LirType::U32 => 32,
            LirType::I64 | LirType::U64 => 64,
            LirType::I128 | LirType::U128 => 128,
            LirType::I256 | LirType::U256 => 256,
            LirType::Vector(elem, len) => elem.bit_width() * (*len as u32),
            LirType::Arr(elem, len) => elem.bit_width() * (*len as u32),
            LirType::Struct(_) => panic!("bit_width not defined for Struct"),
            LirType::Native(_) => panic!("bit_width not meaningful for Native field elements"),
            LirType::Ptr(_) => panic!("bit_width not meaningful for Ptr (target-dependent size)"),
        }
    }

    /// Whether this scalar type is signed. Panics on aggregates.
    pub fn is_signed(&self) -> bool {
        matches!(
            self,
            LirType::I8
                | LirType::I16
                | LirType::I32
                | LirType::I64
                | LirType::I128
                | LirType::I256
        )
    }

    pub fn is_scalar(&self) -> bool {
        !matches!(self, LirType::Arr(_, _) | LirType::Struct(_))
    }
}

// ============================================================================
// ABI policy
// ============================================================================

/// ABI policy for a [`LirTarget`] backend.
///
/// Controls how values are passed between functions — inline vs. by-pointer,
/// aggregate packing thresholds, and whether the backend supports native
/// aggregate passing (structs/arrays as single C values).
///
/// Each [`LirTarget`] implementation returns its preferred ABI via
/// [`LirTarget::abi`].  The compiler codegen layer
/// (`volar-lir-codegen`) queries this to make passing-convention decisions.
///
/// # Variants
///
/// | Constant       | Backend          | Aggregates           | Pointer passing  |
/// |----------------|------------------|----------------------|------------------|
/// | `CIRCUIT`      | VolarIrTarget, VaffleTarget | Flattened to bits | N/A          |
/// | `C_NATIVE`     | CBackend         | Native C structs     | Above threshold  |
/// | `DEFAULT`      | Fallback         | Flattened to scalars | Disabled         |
#[derive(Clone, Debug)]
pub struct LirAbi {
    /// Maximum number of flat scalars to pass inline (by value) at a call
    /// site.  Aggregates whose [`flatten_count`] exceeds this are passed
    /// via `StackAllocExt` (alloca + pointer) when the backend supports it.
    ///
    /// Set to `usize::MAX` to disable pointer-passing entirely.
    pub aggregate_byval_limit: usize,

    /// Whether the backend can accept and return aggregate types directly
    /// in its native calling convention (e.g. C passes `struct` by value).
    ///
    /// When `true`, the codegen layer may skip flattening for types below
    /// the `aggregate_byval_limit` and rely on the backend's own
    /// pack/unpack logic (see `CBackend::pack_scalars`).
    pub native_aggregates: bool,
}

impl LirAbi {
    /// ABI for circuit backends (`VolarIrTarget`, `VaffleTarget`).
    ///
    /// Everything is decomposed to GF(2) bits; there is no concept of
    /// aggregate passing or pointer indirection.  `aggregate_byval_limit`
    /// is `usize::MAX` so the codegen layer never attempts pointer-passing.
    pub const CIRCUIT: LirAbi = LirAbi {
        aggregate_byval_limit: usize::MAX,
        native_aggregates: false,
    };

    /// ABI for the C backend.
    ///
    /// Aggregates up to 64 flat scalars are passed by value as C structs.
    /// Larger aggregates are passed by pointer via `StackAllocExt` when
    /// available.
    pub const C_NATIVE: LirAbi = LirAbi {
        aggregate_byval_limit: 64,
        native_aggregates: true,
    };

    /// ABI for the VAFFLE target with stack-based aggregate passing.
    ///
    /// Parameters whose bit-decomposition exceeds 64 bits are passed via
    /// `StorageId::STACK`: the caller writes the bits to a stack slot and
    /// passes a 32-bit address; the callee reads the bits back.  This
    /// dramatically reduces block-parameter counts (and therefore
    /// spill/reload cost in the `lower_to_ir` pass) for functions that
    /// accept AES-sized or larger aggregates.
    ///
    /// Enable via [`VaffleTarget::with_optimized_abi`].
    pub const VAFFLE_OPTIMIZED: LirAbi = LirAbi {
        aggregate_byval_limit: 64,
        native_aggregates: false,
    };

    /// Default ABI: unlimited inline passing, no native aggregates.
    pub const DEFAULT: LirAbi = LirAbi {
        aggregate_byval_limit: usize::MAX,
        native_aggregates: false,
    };

    /// Whether `scalar_count` exceeds the inline-passing limit.
    ///
    /// Returns `true` when the aggregate should be passed by pointer
    /// (assuming the backend has `StackAllocExt` support).
    #[inline]
    pub fn pass_by_ptr(&self, scalar_count: usize) -> bool {
        scalar_count > self.aggregate_byval_limit
    }
}

// ============================================================================
// Stack allocation extension trait
// ============================================================================

/// Extension trait for backends that support stack allocation and pointer
/// operations.
///
/// Access via [`LirTarget::stack_alloc_ext`], which returns `None` for
/// backends without a memory model (e.g. `VolarIrTarget`, `VaffleTarget`).
/// `CBackend` returns `Some(self)`.
///
/// # Pointer type
///
/// `alloca` returns a [`LirType::Ptr`] value.  Pointer arithmetic and
/// dereferencing use element indices, not byte offsets, so callers do not need
/// to know the element size.
pub trait StackAllocExt {
    type Value: Clone + Eq + core::fmt::Debug;

    /// Allocate a stack region for `count` elements of `elem_ty`.
    ///
    /// Returns a value of type `LirType::Ptr(Box::new(elem_ty))`.
    /// The region is live for the duration of the enclosing function.
    fn alloca(&mut self, elem_ty: LirType, count: usize) -> Self::Value;

    /// Load through a typed pointer. `ty` must match the pointee type.
    fn ptr_load(&mut self, ptr: Self::Value, ty: LirType) -> Self::Value;

    /// Store `val` through `ptr`. Returns `()` — emitted as a bare statement.
    fn ptr_store(&mut self, ptr: Self::Value, val: Self::Value);

    /// Element-wise pointer offset: `ptr + idx` elements (not bytes).
    ///
    /// Returns a pointer of the same type as `ptr`.
    fn ptr_offset(&mut self, ptr: Self::Value, idx: Self::Value) -> Self::Value;
}

// ============================================================================
// Heap allocation extension trait
// ============================================================================

/// Extension trait for backends with a real heap-allocation primitive —
/// the `Box<T>` counterpart of [`StackAllocExt`].
///
/// Access via [`LirTarget::heap_alloc_ext`], which returns `None` for
/// backends without one. `CBackend` returns `Some(self)`, backed by `malloc`.
///
/// # Pointer type
///
/// Like [`StackAllocExt::alloca`], `heap_alloc` returns a [`LirType::Ptr`]
/// value — [`StackAllocExt::ptr_load`]/`ptr_store`/`ptr_offset` and
/// [`LirTarget::ptr_index_load`]/`ptr_index_store` all work identically
/// regardless of whether a pointer came from `alloca` or `heap_alloc`; only
/// the allocation site itself differs. A backend implementing
/// `HeapAllocExt` is expected to also implement `StackAllocExt`, so callers
/// needing to read/write through the resulting pointer reuse that trait's
/// methods rather than duplicating them here.
///
/// # Lifetime
///
/// Unlike a stack allocation, a heap allocation is never implicitly freed —
/// this trait deliberately has no `free`/`drop` method. For this codebase's
/// current use (function-scoped proof-generation pools, freed in bulk when
/// the enclosing single-shot process exits) that's an acceptable, explicit
/// simplification, not an oversight; add explicit freeing here if a future
/// caller needs it.
pub trait HeapAllocExt {
    type Value: Clone + Eq + core::fmt::Debug;

    /// Allocate heap storage for `count` elements of `elem_ty`, returning a
    /// value of type `LirType::Ptr(Box::new(elem_ty))`.
    fn heap_alloc(&mut self, elem_ty: LirType, count: usize) -> Self::Value;
}

// ============================================================================
// Struct definitions
// ============================================================================

pub type StructId = u32;

// ============================================================================
// Comparison predicates
// ============================================================================

// ============================================================================
// Branch targets (terminators)
// ============================================================================

/// A jump/branch destination: block arguments plus optional reentry hint.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BranchTarget<V> {
    pub args: Vec<V>,
    pub reentry: Option<ReentryHint>,
}

impl<V> BranchTarget<V> {
    /// Branch target with arguments and no reentry hint.
    pub fn args(args: impl Into<Vec<V>>) -> Self {
        BranchTarget {
            args: args.into(),
            reentry: None,
        }
    }

    /// Attach a reentry hint to this target.
    pub fn with_reentry(mut self, hint: ReentryHint) -> Self {
        self.reentry = Some(hint);
        self
    }
}

// ============================================================================
// The trait
// ============================================================================

/// Builder trait for a low-level SSA IR with block parameters.
///
/// All methods take `&mut self`. The caller must maintain well-formedness:
/// - Call `switch_to_block` before emitting instructions or a terminator.
/// - Every block must end with exactly one terminator.
/// - Call `end_function` after the last terminator.
///
/// # Provenance
///
/// The optional `Prov` type parameter allows callers to attach provenance
/// annotations to emitted instructions.  Call [`set_prov`](LirTarget::set_prov)
/// before emitting one or more instructions; each instruction inherits the
/// most recently set provenance.  Backends that do not track provenance use
/// the default `Prov = ()` and the no-op default impl of `set_prov`.
///
/// # Side
///
/// Independently of provenance, callers may also attach a [`SideId`] naming
/// which actor/party/role subsequently emitted instructions belong to (see
/// `volar-side`).  Call [`set_side`](LirTarget::set_side) the same way as
/// `set_prov` — each instruction inherits the most recently set side.  The
/// default implementation is a no-op, exactly like `set_prov`'s default.
pub trait LirTarget<Prov: Clone = ()> {
    type Value: Clone + Eq + core::fmt::Debug;
    type Block: Clone + Eq + core::fmt::Debug;

    /// Set the provenance context for subsequently emitted instructions.
    ///
    /// Each call overrides the previous value.  Instructions emitted after
    /// this call (and before the next `set_prov`) are tagged with `prov`.
    ///
    /// The default implementation is a no-op — backends that do not track
    /// provenance need not override this.
    fn set_prov(&mut self, _prov: Prov) {}

    /// Set the side context for subsequently emitted instructions.
    ///
    /// Each call overrides the previous value.  Instructions emitted after
    /// this call (and before the next `set_side`) are tagged with `side`.
    ///
    /// The default implementation is a no-op — backends that do not track
    /// sides need not override this.
    fn set_side(&mut self, _side: Option<volar_side::SideId>) {}

    // ---- Type registration --------------------------------------------------

    /// Register a struct definition. Must be called before any use of the
    /// returned `StructId` in `LirType::Struct(id)` or `struct_new`.
    /// Can be called at any point (before or during functions).
    fn define_struct(&mut self, def: StructDef) -> StructId;

    // ---- Function management ------------------------------------------------

    /// Begin a new function.
    ///
    /// `params` holds the ABI type for each parameter (may be aggregate).
    /// Returns the entry block and one `Vec<Self::Value>` per parameter — each
    /// inner vec is the flat scalar values that represent that parameter.
    fn begin_function(
        &mut self,
        name: &str,
        params: &[LirType],
        ret: Option<LirType>,
    ) -> (Self::Block, Vec<Vec<Self::Value>>);

    fn end_function(&mut self);

    // ---- Block management ---------------------------------------------------

    fn create_block(&mut self) -> Self::Block;
    fn add_block_param(&mut self, block: Self::Block, ty: LirType) -> Self::Value;
    fn switch_to_block(&mut self, block: Self::Block);

    // ---- Constants ----------------------------------------------------------

    fn iconst(&mut self, ty: LirType, val: i64) -> Self::Value;

    // ---- Arithmetic ---------------------------------------------------------

    fn add(&mut self, lhs: Self::Value, rhs: Self::Value) -> Self::Value;
    fn sub(&mut self, lhs: Self::Value, rhs: Self::Value) -> Self::Value;
    fn mul(&mut self, lhs: Self::Value, rhs: Self::Value) -> Self::Value;
    fn udiv(&mut self, lhs: Self::Value, rhs: Self::Value) -> Self::Value;
    fn sdiv(&mut self, lhs: Self::Value, rhs: Self::Value) -> Self::Value;

    // ---- Bitwise ------------------------------------------------------------

    fn and(&mut self, lhs: Self::Value, rhs: Self::Value) -> Self::Value;
    fn or(&mut self, lhs: Self::Value, rhs: Self::Value) -> Self::Value;
    fn xor(&mut self, lhs: Self::Value, rhs: Self::Value) -> Self::Value;
    fn not(&mut self, val: Self::Value) -> Self::Value;
    fn shl(&mut self, val: Self::Value, shift: Self::Value) -> Self::Value;
    fn lshr(&mut self, val: Self::Value, shift: Self::Value) -> Self::Value;
    fn ashr(&mut self, val: Self::Value, shift: Self::Value) -> Self::Value;

    // ---- Comparison ---------------------------------------------------------

    /// Integer compare — result type is always `LirType::Bool`.
    fn icmp(&mut self, pred: IcmpPred, lhs: Self::Value, rhs: Self::Value) -> Self::Value;

    // ---- Conversions --------------------------------------------------------

    fn zext(&mut self, val: Self::Value, dst_ty: LirType) -> Self::Value;
    fn sext(&mut self, val: Self::Value, dst_ty: LirType) -> Self::Value;
    fn trunc(&mut self, val: Self::Value, dst_ty: LirType) -> Self::Value;

    // ---- Select -------------------------------------------------------------

    /// `cond ? then_val : else_val`. `cond` must be `LirType::Bool`.
    fn select(
        &mut self,
        cond: Self::Value,
        then_val: Self::Value,
        else_val: Self::Value,
    ) -> Self::Value;

    // ---- Value type query ---------------------------------------------------

    /// Return the scalar `LirType` of a previously-emitted value.
    ///
    /// Used by the lowering pass to recover types for join-block parameters
    /// (e.g. in `lower_if`) without threading type information through every
    /// `lower_expr` return.  Panics if `val` was not produced by this target.
    fn value_scalar_type(&self, val: &Self::Value) -> LirType;

    // ---- Extern calls -------------------------------------------------------

    /// Call an external function by name.
    ///
    /// `arg_tys` holds the ABI type for each *logical* argument (may be
    /// `Arr`/`Struct`).  `args` is the flat list of scalars whose total count
    /// equals `sum(flatten_count(arg_tys[i]))`.  `ret_ty` is the ABI return
    /// type (may be aggregate).  Returns the flat scalar list for the return
    /// value (empty for void).
    fn call_extern(
        &mut self,
        name: &str,
        arg_tys: &[LirType],
        args: &[Self::Value],
        ret_ty: Option<LirType>,
    ) -> Vec<Self::Value>;

    // ---- Sibling (intra-module) calls ----------------------------------------

    /// Call another function *defined in this same module* — as opposed to
    /// [`call_extern`](Self::call_extern), which is for named external/
    /// oracle-style symbols the backend never itself emits a body for.
    ///
    /// `name` is the callee's logical name, exactly as passed to a
    /// (possibly not-yet-emitted) [`begin_function`](Self::begin_function)
    /// call. Implementations must support forward references and mutual
    /// recursion: `name` may be called before its own `begin_function`/
    /// `end_function` pair has run. Argument/return marshalling matches
    /// `call_extern` exactly (flat scalar lists, ABI types from `arg_tys`/
    /// `ret_ty`).
    fn call(
        &mut self,
        name: &str,
        arg_tys: &[LirType],
        args: &[Self::Value],
        ret_ty: Option<LirType>,
    ) -> Vec<Self::Value>;

    // ---- Terminators --------------------------------------------------------

    fn jump(&mut self, target: Self::Block, branch: BranchTarget<Self::Value>);

    fn branch(
        &mut self,
        cond: Self::Value,
        then_block: Self::Block,
        then_branch: BranchTarget<Self::Value>,
        else_block: Self::Block,
        else_branch: BranchTarget<Self::Value>,
    );

    /// Emit a return.  `vals` is the flat scalar list for the return value
    /// (empty slice for void functions).
    fn ret(&mut self, vals: &[Self::Value]);

    /// A multi-way branch keyed on a compile-time-enumerable set of integer
    /// case values, falling through to `default_block` when `index` matches
    /// none of `cases`. The `LirTarget` analogue of VAFFLE's
    /// `Terminator::Table`, LLVM's `switch`, and WASM's `br_table`. Unlike
    /// [`dyn_jump`](Self::dyn_jump), each case carries its own
    /// [`BranchTarget`] — args may differ per destination.
    fn switch(
        &mut self,
        index: Self::Value,
        cases: &[(i64, Self::Block, BranchTarget<Self::Value>)],
        default_block: Self::Block,
        default_branch: BranchTarget<Self::Value>,
    );

    /// A backend-defined integer ordinal for `block`, stable within one
    /// function. Only used by the default [`block_addr`](Self::block_addr)
    /// and [`dyn_jump`](Self::dyn_jump) implementations to build matching
    /// [`switch`](Self::switch) case keys; backends that override both of
    /// those (e.g. `LlvmBackend`, using native `blockaddress`/`indirectbr`)
    /// need not implement this.
    fn block_ordinal(&self, _block: &Self::Block) -> i64 {
        unimplemented!(
            "block_ordinal: not supported by this backend (only used by the default \
             block_addr/dyn_jump implementations)"
        )
    }

    /// Materialize a first-class reference to `block`, usable later as the
    /// `index` of a [`dyn_jump`](Self::dyn_jump). Mirrors LLVM's
    /// `blockaddress` constant.
    ///
    /// LLVM cannot take the address of a function's *entry* block
    /// (`BasicBlock::get_address` returns `None` there) — implementations
    /// should treat `block_addr(entry_block)` as a caller error. Producers
    /// of `LirTarget` calls (e.g. VAFFLE's `Value::BlockAddr`) are expected
    /// to never target the entry block for exactly this reason.
    ///
    /// The default implementation encodes `block_ordinal(block)` as a
    /// `LirType::U32` constant; it is correct for any backend that also uses
    /// the default [`dyn_jump`](Self::dyn_jump), since both agree on the
    /// same per-block ordinal.
    fn block_addr(&mut self, block: Self::Block) -> Self::Value {
        let ordinal = self.block_ordinal(&block);
        self.iconst(LirType::U32, ordinal)
    }

    /// Jump to a runtime block reference produced by
    /// [`block_addr`](Self::block_addr), given the closed set of blocks it
    /// could possibly resolve to (mirroring LLVM's `indirectbr`, which
    /// always carries its own destination list — never truly open-ended).
    ///
    /// `branch`'s args apply *uniformly* to every possible destination: like
    /// LLVM's `indirectbr`/PHI model (only one incoming value per
    /// predecessor edge) and Volar IR's own `Dyn` continuation-jump
    /// convention, a dynamic jump cannot pass different args down different
    /// destinations — route per-destination data through storage instead if
    /// that's needed.
    ///
    /// The default implementation lowers to [`switch`](Self::switch), keying
    /// each destination on its own [`block_ordinal`](Self::block_ordinal) —
    /// matching the default [`block_addr`](Self::block_addr) regardless of
    /// `destinations`' order, so C and WASM backends need no bespoke code
    /// here; only `LlvmBackend` overrides this (native `indirectbr`).
    fn dyn_jump(
        &mut self,
        index: Self::Value,
        destinations: &[Self::Block],
        branch: BranchTarget<Self::Value>,
    ) {
        let mut cases = Vec::with_capacity(destinations.len().saturating_sub(1));
        for block in destinations.iter().skip(1) {
            let ordinal = self.block_ordinal(block);
            cases.push((ordinal, block.clone(), branch.clone()));
        }
        self.switch(index, &cases, destinations[0].clone(), branch);
    }

    // ---- External access primitives ----------------------------------------

    /// Invoke a named pure oracle, returning all outputs as a flat scalar list.
    ///
    /// `ret_tys` holds the [`LirType`] of each output (length ≥ 1).
    /// Returns the concatenation of `flatten(output_i)` for each `i`.
    /// Callers split using `flatten_count(&ret_tys[i])`.
    fn oracle(
        &mut self,
        name: &str,
        arg_tys: &[LirType],
        args: &[Self::Value],
        ret_tys: &[LirType],
    ) -> Vec<Self::Value>;

    /// Invoke a named conditional action, returning all outputs as a flat
    /// scalar list.
    ///
    /// `guard` must be `LirType::Bool`.  `fallbacks` is the flat concatenation
    /// of all outputs' fallback scalar values.  `ret_tys` holds the [`LirType`]
    /// of each output (length ≥ 1).  The action executes iff `guard = 1`.
    fn action(
        &mut self,
        name: &str,
        guard: Self::Value,
        arg_tys: &[LirType],
        args: &[Self::Value],
        fallbacks: &[Self::Value],
        ret_tys: &[LirType],
    ) -> Vec<Self::Value>;

    /// Generate a fresh random value of `ty`.  Each call is an independent
    /// sample; implementations must not alias results.
    fn rng(&mut self, ty: LirType) -> Self::Value;

    /// Generate a fresh random value from a named source.
    ///
    /// The default preserves the original anonymous-runtime ABI.  Native
    /// targets override it to expose `rng_<name>` as a configurable external
    /// symbol/import, while circuit targets can retain their existing source
    /// handling without a second implementation.
    fn rng_named(&mut self, _name: &str, ty: LirType) -> Self::Value {
        self.rng(ty)
    }

    /// Invoke one bit of a named oracle through the portable external-bit
    /// ABI. The default symbol is `oracle_<name>` and is still routed through
    /// [`NameConfig`] by native backends, so a deployment can remap every
    /// source without rewriting its IR.
    ///
    /// ABI: `bool oracle_<name>(bool... args, u32 bit, u64 occurrence)`.
    fn oracle_bit(
        &mut self,
        name: &str,
        args: &[Self::Value],
        bit: usize,
        occurrence: u64,
    ) -> Self::Value {
        let mut arg_tys = alloc::vec![LirType::Bool; args.len()];
        let mut values = args.to_vec();
        arg_tys.push(LirType::U32);
        values.push(self.iconst(LirType::U32, bit as i64));
        arg_tys.push(LirType::U64);
        values.push(self.iconst(LirType::U64, occurrence as i64));
        self.call_extern(
            &alloc::format!("oracle_{name}"),
            &arg_tys,
            &values,
            Some(LirType::Bool),
        )
        .into_iter()
        .next()
        .expect("external oracle bit must return exactly one Bool")
    }

    /// Invoke one fresh bit from a named RNG source.
    ///
    /// ABI: `bool rng_<name>(u32 bit, u64 occurrence)`.
    fn rng_bit(&mut self, name: &str, bit: usize, occurrence: u64) -> Self::Value {
        let values = [
            self.iconst(LirType::U32, bit as i64),
            self.iconst(LirType::U64, occurrence as i64),
        ];
        self.call_extern(
            &alloc::format!("rng_{name}"),
            &[LirType::U32, LirType::U64],
            &values,
            Some(LirType::Bool),
        )
        .into_iter()
        .next()
        .expect("external RNG bit must return exactly one Bool")
    }

    /// Emit one direct action-storage side effect through the portable
    /// external-bit ABI. The action owns the storage write and chooses
    /// `fallback` when `guard` is false.
    ///
    /// ABI: `void action_<name>(bool... args, bool guard, bool fallback,
    /// u64 storage, u32 lane, u64 address, u32 bit, u64 occurrence)`.
    #[allow(clippy::too_many_arguments)]
    fn action_store_bit(
        &mut self,
        name: &str,
        guard: Self::Value,
        args: &[Self::Value],
        fallback: Self::Value,
        storage: u64,
        lane: u32,
        address: Self::Value,
        bit: usize,
        occurrence: u64,
    ) {
        let mut arg_tys = alloc::vec![LirType::Bool; args.len()];
        let mut values = args.to_vec();
        arg_tys.extend([
            LirType::Bool,
            LirType::Bool,
            LirType::U64,
            LirType::U32,
            LirType::U64,
            LirType::U32,
            LirType::U64,
        ]);
        values.extend([
            guard,
            fallback,
            self.iconst(LirType::U64, storage as i64),
            self.iconst(LirType::U32, lane as i64),
            address,
            self.iconst(LirType::U32, bit as i64),
            self.iconst(LirType::U64, occurrence as i64),
        ]);
        self.call_extern(&alloc::format!("action_{name}"), &arg_tys, &values, None);
    }

    /// Invoke an action and give its host implementation every result's
    /// storage destination directly.
    ///
    /// This is the typed counterpart of [`action_store_bit`](Self::action_store_bit).
    /// The portable argument order is:
    ///
    /// `args..., guard, fallbacks..., (storage, lane, address)...`
    ///
    /// `fallbacks`, `ret_tys`, and `targets` have one item per action result.
    /// The action runtime writes either its result (when `guard` is true) or
    /// the corresponding fallback (when false); it returns no SSA value.
    /// Native targets retain each value's type, while targets with a flat ABI
    /// lower individual values according to their normal call ABI.
    fn action_store(
        &mut self,
        name: &str,
        guard: Self::Value,
        arg_tys: &[LirType],
        args: &[Self::Value],
        fallbacks: &[Self::Value],
        ret_tys: &[LirType],
        targets: &[ActionStoreTarget<Self::Value>],
    ) {
        assert_eq!(
            fallbacks.len(),
            ret_tys.len(),
            "one fallback per action result"
        );
        assert_eq!(targets.len(), ret_tys.len(), "one target per action result");

        let mut abi_tys = arg_tys.to_vec();
        let mut values = args.to_vec();
        abi_tys.push(LirType::Bool);
        values.push(guard);
        abi_tys.extend_from_slice(ret_tys);
        values.extend_from_slice(fallbacks);
        for target in targets {
            abi_tys.extend([LirType::U64, LirType::U32, target.address_ty.clone()]);
            values.push(self.iconst(LirType::U64, target.storage as i64));
            values.push(self.iconst(LirType::U32, target.lane as i64));
            values.push(target.address.clone());
        }
        self.call_extern(&alloc::format!("action_{name}"), &abi_tys, &values, None);
    }

    /// Return a mutable reference to the [`StackAllocExt`] implementation for
    /// this backend, if it supports stack allocation and pointer operations.
    ///
    /// Circuit backends (`VolarIrTarget`, `VaffleTarget`) return `None`.
    /// `CBackend` returns `Some(self)`.
    ///
    /// Callers should check for `Some` before using pointer-based patterns
    /// (e.g. passing large structs by pointer in the C ABI).
    fn stack_alloc_ext(&mut self) -> Option<&mut dyn StackAllocExt<Value = Self::Value>> {
        None
    }

    /// Return a mutable reference to the [`HeapAllocExt`] implementation for
    /// this backend, if it supports heap allocation.
    ///
    /// The `Box<T>` abstraction (see `volar_compiler::ir::box_type`/
    /// `box_new_expr`) lowers through this — unlike [`Self::stack_alloc_ext`],
    /// a heap allocation's storage outlives the call that produced it and is
    /// never implicitly freed at function exit, which is exactly what a
    /// large, statically-sized-but-too-big-for-the-stack buffer (e.g. a
    /// pool with thousands of slots) needs.
    ///
    /// Circuit backends (`VolarIrTarget`, `VaffleTarget`) return `None`.
    /// `CBackend` returns `Some(self)` (backed by `malloc`).
    fn heap_alloc_ext(&mut self) -> Option<&mut dyn HeapAllocExt<Value = Self::Value>> {
        None
    }

    /// Return the ABI policy for this target.
    ///
    /// The compiler codegen layer (`volar-lir-codegen`) queries this to
    /// decide aggregate passing conventions, pointer-passing thresholds,
    /// and other target-specific calling convention details.
    ///
    /// The default returns [`LirAbi::DEFAULT`].  Override in backends with
    /// specialised ABIs (e.g. `CBackend` returns [`LirAbi::C_NATIVE`]).
    fn abi(&self) -> LirAbi {
        LirAbi::DEFAULT
    }

    // ---- Pointer-indexed memory operations ----------------------------------

    /// Load an element from a pointer at the given index, returning flat
    /// scalar values.
    ///
    /// Semantics: `ptr[idx]` — read the element at offset `idx` and
    /// decompose the aggregate result into flat scalar values.
    ///
    /// Only backends with a memory model (e.g. `CBackend`) need to implement
    /// this.  Circuit backends can leave the default, which panics.
    fn ptr_index_load(
        &mut self,
        _ptr: Self::Value,
        _idx: Self::Value,
        _pointee_ty: &LirType,
    ) -> Vec<Self::Value> {
        unimplemented!("ptr_index_load: not supported by this backend")
    }

    /// Store flat scalar values to a pointer at the given index.
    ///
    /// Semantics: `ptr[idx] = pack(vals)` — pack the flat scalars into an
    /// aggregate and write it at offset `idx`.
    ///
    /// Only backends with a memory model need to implement this.
    fn ptr_index_store(
        &mut self,
        _ptr: Self::Value,
        _idx: Self::Value,
        _vals: &[Self::Value],
        _pointee_ty: &LirType,
    ) {
        unimplemented!("ptr_index_store: not supported by this backend")
    }
}
