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
//! `phi` (→ block params), direct `call`, direct tail calls (→
//! `Terminator::ReturnCall`), `br`/conditional `br`/`switch`/`ret`,
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
//!   compile-time-constant `StorageId::ALLOCA` address (`Value::StackAlloc`
//!   / `PtrLoad` / `PtrStore` / `PtrOffset`, one bit-level `StorageRead`/
//!   `StorageWrite` per bit, mirroring `VaffleTarget`'s own convention; see
//!   `docs/llvm-alloca.md`).
//!
//! Not yet supported (hard error): floats, vectors, aggregates, atomics,
//! `indirectbr`/`blockaddress` (VAFFLE's `Value::BlockAddr` already models
//! this — ingest is deferred), reachable `unreachable` not immediately
//! preceded by a direct call, any pointer arithmetic whose
//! base doesn't resolve to a literal global or a tracked stack pointer,
//! `alloca` with a symbolic count or a non-integer element type, a
//! multi-index or symbolic-index `getelementptr` into a stack pointer, and
//! phi/select of a pointer-typed SSA value (merging two distinct pointers
//! at a control-flow join). `switch` is supported: sparse case keys become
//! a dense positional `Terminator::Table` via the same `bc_eq` /
//! `bc_select_vec` selector cascade `VaffleTarget::switch` uses.

use std::collections::{HashMap, HashSet};

use inkwell::AddressSpace;
use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock as LlvmBlock;
use inkwell::llvm_sys::core::{
    LLVMGetConstOpcode, LLVMGetGEPSourceElementType, LLVMGetNumOperands, LLVMGetNumSuccessors,
    LLVMGetOperand, LLVMGetSuccessor, LLVMGetSwitchCaseValue, LLVMGetTypeKind, LLVMIsAConstantExpr,
    LLVMTypeOf,
};
use inkwell::llvm_sys::{LLVMOpcode, LLVMTypeKind};
use inkwell::module::Module as LlvmModule;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{
    AnyValue, AnyValueEnum, AsValueRef, BasicValueEnum, CallSiteValue, FunctionValue,
    InstructionOpcode, InstructionValue, IntValue, PhiValue, PointerValue,
};

use vaffle::{
    Block, BlockId, FuncBody, FuncDecl, FuncId, Module, PointerWidth, SigDecl, SigId,
    StackFrameConvention, Target, Terminator, Value, ValueId,
};
use volar_ir_common::{
    Constant, IrType, Node, PolyCoeffs, Stmt, StorageAllocator, StorageId, StoragePurpose,
    StorageRegistry, Type, TypeId, TypeTable,
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

/// Bits reserved for a global's `StorageId` within `Importer::ptr_value_bits`'s
/// tagged pointer-*value* encoding (most-significant bit = provenance tag, 0 = stack;
/// remaining bits for a global split `GLOBAL_ID_BITS`:`GLOBAL_ADDR_BITS`,
/// high:low). Reused directly as a compact, already-dense candidate index
/// rather than building a separate table -- `StorageAllocator::new(64)`
/// already hands out small sequential ids. 12 bits comfortably covers any
/// realistic module's global count (up to ~4000).
const GLOBAL_ID_BITS: usize = 12;
/// Bits reserved for a global's byte offset within the same encoding.

/// First `StorageId` this importer's own `storage_alloc` hands out to a
/// global -- below any reserved range (`StorageId::ALLOCA`/`STACK`/
/// `VIRT_*`/`memory(_)`).
const GLOBAL_STORAGE_BASE: u32 = 64;

/// Cap on the number of distinct globals a single runtime pointer-dispatch
/// site (`Importer::dispatch_read`/`dispatch_write`) will build a mux/demux
/// cascade over. Each candidate costs a full `StorageRead` (and, for a
/// write, a paired `StorageRead`+`StorageWrite`) per accessed bit, so this
/// bounds worst-case circuit blowup -- fail closed with a named error past
/// it rather than silently emitting an enormous circuit. Well under
/// `2^GLOBAL_ID_BITS` (the encoding's own, much larger, capacity limit).
const MAX_DISPATCH_CANDIDATES: usize = 64;

/// Bits needed to represent every integer in `0..=v` (at least 1). Matches
/// `VaffleTarget::switch`'s dense positional selector width.
fn bits_for_max_value(v: usize) -> usize {
    if v == 0 {
        1
    } else {
        (usize::BITS - v.leading_zeros()) as usize
    }
}

/// Optional ABI constraints for structural LLVM import.
///
/// When unset, the default-address-space pointer width is read from the LLVM
/// data layout (or LLVM's 64-bit default when the module has no layout).
/// Supplying a width is useful to make a caller's ABI expectation explicit;
/// it never overrides an incompatible LLVM layout.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct LlvmImportConfig {
    pub pointer_width: Option<PointerWidth>,
}

fn pointer_width_from_layout<'ctx>(
    llvm_module: &LlvmModule<'ctx>,
    config: LlvmImportConfig,
) -> IResult<PointerWidth> {
    let data_layout = llvm_module.get_data_layout();
    let layout = data_layout
        .as_str()
        .to_str()
        .map_err(|_| ImportError::Unsupported("LLVM data layout is not UTF-8".into()))?;
    // LLVM uses a 64-bit default pointer layout when no default `p` entry is
    // present. Address-space-specific entries (`p270`, etc.) do not affect
    // this choice.
    let declared_bits = layout.split('-').find_map(|entry| {
        let rest = entry
            .strip_prefix("p:")
            .or_else(|| entry.strip_prefix("p0:"))?;
        rest.split(':').next()?.parse::<usize>().ok()
    });
    let width = match declared_bits.unwrap_or(64) {
        32 => PointerWidth::Bits32,
        64 => PointerWidth::Bits64,
        bits => {
            return Err(ImportError::Unsupported(format!(
                "default LLVM pointer width {bits} is unsupported (only 32 and 64 bits are supported)"
            )));
        }
    };
    if let Some(explicit) = config.pointer_width {
        if explicit != width {
            return Err(ImportError::Unsupported(format!(
                "configured pointer width {} does not match LLVM data layout width {}",
                explicit.bits(),
                width.bits()
            )));
        }
    }
    Ok(width)
}

/// Import every reachable function transitively called from `entries` (by
/// LLVM name) into a fresh `vaffle::Module`. Each entry (and every function
/// it directly or indirectly calls) becomes its own `vaffle::Func`; direct
/// calls are preserved, never inlined.
pub fn import_module<'ctx>(llvm_module: &LlvmModule<'ctx>, entries: &[&str]) -> IResult<Module> {
    import_module_with_config(llvm_module, entries, LlvmImportConfig::default())
}

/// Configured variant of [`import_module`].
pub fn import_module_with_config<'ctx>(
    llvm_module: &LlvmModule<'ctx>,
    entries: &[&str],
    config: LlvmImportConfig,
) -> IResult<Module> {
    let mut importer = Importer::new(pointer_width_from_layout(llvm_module, config)?);
    // Eagerly assign every module global its `StorageId` before walking any
    // function body, so `dispatch_read`/`dispatch_write` (runtime
    // storage-identity dispatch for a pointer whose provenance isn't
    // statically resolvable) always sees the complete, stable candidate set
    // -- never just whichever globals happened to be referenced by earlier
    // functions' own direct loads/stores/GEPs.
    importer.register_all_globals(llvm_module)?;
    let mut worklist: Vec<FunctionValue<'ctx>> = Vec::new();
    for &name in entries {
        let f = llvm_module.get_function(name).ok_or_else(|| {
            ImportError::Unsupported(format!("entry function `{name}` does not exist"))
        })?;
        importer.func_id(f)?;
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

/// Registry-mode variant of [`import_module_with_config`].
///
/// The module's storage spaces are coordinated through `registry` (one
/// registry per module, see `volar_ir_common::storage_registry`):
///
/// * the alloca-marker / stack spaces of [`StackFrameConvention::LEGACY`]
///   are claimed (failing closed if another consumer already owns them);
/// * `StorageId(0)` is reserved — the `null` pointer's tagged encoding is
///   global-ID 0, so no global may receive it;
/// * every global gets a dense, purpose-tagged [`StoragePurpose::LlvmGlobal`]
///   space (keeping the `GLOBAL_ID_BITS` pointer-value encoding valid by
///   construction).
///
/// Returns the module together with the registry so the caller keeps the
/// complete storage ownership record.
pub fn import_module_with_registry<'ctx>(
    llvm_module: &LlvmModule<'ctx>,
    entries: &[&str],
    config: LlvmImportConfig,
    registry: StorageRegistry<StoragePurpose>,
) -> IResult<(Module, StorageRegistry<StoragePurpose>)> {
    let mut importer = Importer::new(pointer_width_from_layout(llvm_module, config)?)
        .with_storage_registry(registry)?;
    importer.register_all_globals(llvm_module)?;
    let mut worklist: Vec<FunctionValue<'ctx>> = Vec::new();
    for &name in entries {
        let f = llvm_module.get_function(name).ok_or_else(|| {
            ImportError::Unsupported(format!("entry function `{name}` does not exist"))
        })?;
        importer.func_id(f)?;
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
    Ok(importer.finish_registry())
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
    import_module_inlined_with_config(llvm_module, entries, LlvmImportConfig::default())
}

/// Configured variant of [`import_module_inlined`].
pub fn import_module_inlined_with_config<'ctx>(
    llvm_module: &LlvmModule<'ctx>,
    entries: &[&str],
    config: LlvmImportConfig,
) -> IResult<Module> {
    let mut module = import_module_with_config(llvm_module, entries, config)?;
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
    volar_ir_opt::inline_vaffle::inline_vaffle_everything(&mut module, &ids)
        .map_err(|e| ImportError::Unsupported(e.to_string()))?;
    Ok(module)
}

/// One VAFFLE bit-typed value per LLVM bit, LSB first — mirrors
/// `VaffleTarget::VaffleValue.bits`.
type Bits = Vec<ValueId>;

#[derive(Clone, Copy, Debug)]
struct StackPointer {
    /// Identity and bounds of the originating alloca, in bit-addressed
    /// `StorageId::ALLOCA` slots. Used by `StackPtr::Const`'s import-time
    /// memory-intrinsic range check; symbolic stack addresses retain their
    /// normal runtime defined-execution requirement instead.
    allocation_base: u64,
    allocation_bits: u64,
    /// Current pointer position within (or potentially beyond) that alloca.
    /// Ordinary loads/stores retain their pre-existing behavior; memory
    /// intrinsics validate this range before emitting accesses.
    addr: u64,
}

impl StackPointer {
    fn intrinsic_range(self, n_bytes: usize) -> IResult<(u64, u64)> {
        let n_bits = u64::try_from(n_bytes)
            .ok()
            .and_then(|n| n.checked_mul(8))
            .ok_or_else(|| {
                ImportError::Unsupported("memory intrinsic length is too large".into())
            })?;
        let allocation_end = self
            .allocation_base
            .checked_add(self.allocation_bits)
            .ok_or_else(|| ImportError::Unsupported("alloca range overflow".into()))?;
        let end = self
            .addr
            .checked_add(n_bits)
            .ok_or_else(|| ImportError::Unsupported("memory intrinsic range overflow".into()))?;
        if self.addr < self.allocation_base || self.addr > allocation_end || end > allocation_end {
            return Err(ImportError::Unsupported(
                "memory intrinsic range escapes its alloca provenance".into(),
            ));
        }
        Ok((self.addr, end))
    }
}

/// Tracking for a pointer-typed LLVM value known (at import time) to be
/// stack-provenance. `Const` is the pre-existing, common case: a
/// compile-time-constant `StorageId::ALLOCA` address. `Symbolic` is produced
/// by a `getelementptr` whose index isn't a compile-time constant (or whose
/// base is itself already `Symbolic`): a genuinely runtime-computed address,
/// `PTR_BITS` wide, LSB first -- proven safe by `rebase_stack_addr`
/// (`volar-vaffle-target/src/lower_to_ir.rs`), which already treats every
/// ALLOCA address as an opaque runtime value with no dependency on it being
/// a compile-time constant.
#[derive(Clone, Debug)]
enum StackPtr {
    Const(StackPointer),
    Symbolic {
        allocation_base: u64,
        allocation_bits: u64,
        addr_bits: Bits,
    },
}

/// Identity and constant byte offset of a global-provenance pointer tracked
/// across a chain of constant-index `getelementptr` *instructions* off a
/// global (or off another already-tracked `GlobalPointer`). Populated by the
/// `GetElementPtr` non-stack arm, consulted by `Load`/`Store` before falling
/// back to `Importer::storage_for_with_offset` -- mirrors `StackPointer`'s
/// role for the ALLOCA side.
#[derive(Clone, Copy, Debug)]
struct GlobalPointer {
    storage: StorageId,
    byte_offset: u64,
}

/// Tracking for a pointer-typed LLVM value known (at import time) to be
/// global-provenance. `Const` is the pre-existing, common case: a
/// compile-time-constant byte offset (`GlobalPointer`). `Symbolic` is
/// produced by a single-index `getelementptr` whose index isn't a
/// compile-time constant (or whose base is itself already `Symbolic`): a
/// genuinely runtime-computed byte offset, `PTR_BITS` wide, LSB first --
/// the same shape as `StackPtr::Symbolic`, and safe for the same reason:
/// `Stmt::StorageRead`/`StorageWrite`'s `addr` is a plain `Var` with no
/// dependency on being a compile-time constant.
#[derive(Clone, Debug)]
enum GlobalPtr {
    Const(GlobalPointer),
    Symbolic {
        storage: StorageId,
        offset_bits: Bits,
    },
}

#[derive(Clone, Debug)]
enum IntrinsicPointer {
    Stack {
        ptr: StackPtr,
        ptr_bits0: ValueId,
    },
    Global {
        storage: StorageId,
        byte_offset: u64,
    },
    /// A pointer whose storage identity is only available in the importer's
    /// tagged runtime representation. Constant-size memory intrinsics can
    /// lower through the same complete candidate dispatch used by ordinary
    /// loads and stores.
    Dispatch {
        ptr_bits: Bits,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MemoryIntrinsic {
    Memset,
    Memcpy,
    Memmove,
}

/// A memory-intrinsic length after import-time validation. A dynamic length
/// carries the value bits consumed by an importer-generated loop.
#[derive(Clone, Debug)]
enum MemoryIntrinsicLength {
    Constant(usize),
    Symbolic { bits: Bits },
}

/// The byte produced by one iteration of a symbolic memory-intrinsic loop.
/// A copy reads it from a runtime-dispatched source pointer, while a memset
/// reuses one normalized, eight-bit fill value.
enum SymbolicByteOperation<'a> {
    Copy { src_base: &'a Bits },
    Fill { byte: &'a Bits },
}

impl MemoryIntrinsic {
    fn from_name(name: &str) -> Option<Self> {
        if name.starts_with("llvm.memset.") {
            Some(Self::Memset)
        } else if name.starts_with("llvm.memcpy.") {
            Some(Self::Memcpy)
        } else if name.starts_with("llvm.memmove.") {
            Some(Self::Memmove)
        } else {
            None
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Memset => "memset",
            Self::Memcpy => "memcpy",
            Self::Memmove => "memmove",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OverflowIntrinsic {
    SAdd,
    UAdd,
    SSub,
    USub,
    SMul,
    UMul,
}

impl OverflowIntrinsic {
    fn from_name(name: &str) -> Option<Self> {
        [
            ("llvm.sadd.with.overflow.", Self::SAdd),
            ("llvm.uadd.with.overflow.", Self::UAdd),
            ("llvm.ssub.with.overflow.", Self::SSub),
            ("llvm.usub.with.overflow.", Self::USub),
            ("llvm.smul.with.overflow.", Self::SMul),
            ("llvm.umul.with.overflow.", Self::UMul),
        ]
        .into_iter()
        .find_map(|(prefix, intrinsic)| name.starts_with(prefix).then_some(intrinsic))
    }

    fn name(self) -> &'static str {
        match self {
            Self::SAdd => "llvm.sadd.with.overflow",
            Self::UAdd => "llvm.uadd.with.overflow",
            Self::SSub => "llvm.ssub.with.overflow",
            Self::USub => "llvm.usub.with.overflow",
            Self::SMul => "llvm.smul.with.overflow",
            Self::UMul => "llvm.umul.with.overflow",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TailCallEnd {
    Return,
    Unreachable,
}

struct Importer<'ctx> {
    pointer_width: PointerWidth,
    pointer_bits: usize,
    types: TypeTable,
    funcs: Vec<FuncDecl>,
    sigs: Vec<SigDecl>,
    exports: std::collections::BTreeMap<String, FuncId>,
    func_ids: HashMap<PointerValue<'ctx>, FuncId>,
    storage_for_global: HashMap<PointerValue<'ctx>, StorageId>,
    storage_alloc: StorageAllocator,
    /// The alloca-marker → stack-frame protocol handle: alloca accesses are
    /// tagged `convention.alloca_marker` (legacy `StorageId::ALLOCA`) for
    /// `volar-vaffle-target`'s lowering to rebase onto `convention.stack`.
    convention: StackFrameConvention,
    /// Registry mode: when set, per-global storage spaces are registered
    /// here (dense, purpose-tagged) instead of bump-allocated from
    /// [`GLOBAL_STORAGE_BASE`].
    storage_registry: Option<StorageRegistry<StoragePurpose>>,
    bit_tid: TypeId,
    byte_tid: TypeId,
    /// Type stamped on `StorageId::ALLOCA` address `Stmt::Const`s
    /// (`stack_load`/`stack_store`). Must be wide enough to hold the
    /// *numeric value* of a stack bit-address (this function's own
    /// allocated bits, zero-based -- see `FuncCtx::next_stack_slot`;
    /// `volar-vaffle-target/src/lower_to_ir.rs` rebases this local offset
    /// onto the real runtime frame at lowering time, so nothing here needs
    /// to reserve headroom against a collision) -- `self.bit_tid` (1 bit)
    /// is NOT wide enough: an interpreter evaluating `Stmt::Const` masks
    /// the literal down to its *declared* type's width, so a 1-bit-typed
    /// address constant silently collapses every address to just its own
    /// low bit, aliasing almost everything onto addresses 0/1 (confirmed
    /// root cause of `spill(5)` computing `0` instead of `5` -- every one
    /// of `spill`'s 32 distinct bit addresses collapsed to 0 or 1 this
    /// way). Its integer width matches the LLVM module's pointer ABI.
    addr_tid: TypeId,
}

impl<'ctx> Importer<'ctx> {
    fn new(pointer_width: PointerWidth) -> Self {
        let pointer_bits = pointer_width.bits();
        let mut types = TypeTable::new();
        let bit_tid = types.bit();
        let byte_tid = types.primitive(Type::_8);
        let addr_tid = types.primitive(match pointer_width {
            PointerWidth::Bits32 => Type::_32,
            PointerWidth::Bits64 => Type::_64,
        });
        Importer {
            pointer_width,
            pointer_bits,
            types,
            funcs: Vec::new(),
            sigs: Vec::new(),
            exports: Default::default(),
            func_ids: HashMap::new(),
            storage_for_global: HashMap::new(),
            // Start well above any reserved range; this importer's own
            // *global* StorageIds never touch StorageId::ALLOCA/STACK/
            // VIRT_*/memory(_) (stack-alloca'd data uses StorageId::ALLOCA
            // directly, via `stack_load`/`stack_store`, not this allocator).
            storage_alloc: StorageAllocator::new(GLOBAL_STORAGE_BASE),
            convention: StackFrameConvention::LEGACY,
            storage_registry: None,
            bit_tid,
            byte_tid,
            addr_tid,
        }
    }

    /// Registry-mode constructor: claims the well-known alloca/stack
    /// spaces and reserves `StorageId(0)` (the `null` pointer's tagged
    /// encoding is global-ID 0 — a global must never receive it), then
    /// hands out dense, purpose-tagged global spaces from `registry`.
    /// The registry is returned by [`Importer::finish_registry`] so the
    /// caller keeps the module's storage ownership record.
    fn with_storage_registry(
        mut self,
        mut registry: StorageRegistry<StoragePurpose>,
    ) -> IResult<Self> {
        StackFrameConvention::registered(&mut registry, true).map_err(|e| {
            ImportError::Unsupported(format!("stack frame convention spaces: {e}"))
        })?;
        registry
            .claim(StorageId::DEFAULT, StoragePurpose::Default)
            .map_err(|e| ImportError::Unsupported(format!("null-tag reservation: {e}")))?;
        self.storage_registry = Some(registry);
        Ok(self)
    }

    fn finish(self) -> Module {
        Module {
            pointer_width: self.pointer_width,
            types: self.types,
            oracles: Vec::new(),
            actions: Vec::new(),
            funcs: self.funcs,
            sigs: self.sigs,
            exports: self.exports,
            pre_init: Vec::new(),
        }
    }

    /// Registry-mode counterpart to [`Importer::finish`]: also returns the
    /// module's storage registry.
    fn finish_registry(mut self) -> (Module, StorageRegistry<StoragePurpose>) {
        let registry = self
            .storage_registry
            .take()
            .expect("finish_registry requires registry mode");
        (self.finish(), registry)
    }

    fn global_addr_bits(&self) -> usize {
        self.pointer_bits - 1 - GLOBAL_ID_BITS
    }

    /// The byte size of an LLVM element type the importer can address. Pointer
    /// elements use the module ABI, rather than the host's pointer size.
    fn memory_byte_size(&self, ty: BasicTypeEnum<'ctx>) -> IResult<u64> {
        match ty {
            BasicTypeEnum::IntType(int_ty) => Ok((int_ty.get_bit_width() as u64).div_ceil(8)),
            BasicTypeEnum::PointerType(ptr_ty) => self
                .llvm_bit_width(ptr_ty)
                .map(|bits| (bits as u64).div_ceil(8)),
            BasicTypeEnum::ArrayType(array) => self
                .memory_byte_size(array.get_element_type())?
                .checked_mul(array.len() as u64)
                .ok_or_else(|| ImportError::Unsupported("LLVM aggregate size overflow".into())),
            _ => Err(ImportError::Unsupported(
                "expected an integer, pointer, or array element type".into(),
            )),
        }
    }

    /// Fold constant GEP indices into a byte offset. The first index steps
    /// through the source element type; later indices descend arrays.
    fn constant_gep_byte_offset(
        &self,
        source_ty: BasicTypeEnum<'ctx>,
        indices: &[i64],
    ) -> IResult<u64> {
        let [first, rest @ ..] = indices else {
            return Err(ImportError::Unsupported("gep has no index operands".into()));
        };
        let whole_size = self.memory_byte_size(source_ty)?;
        let mut offset = first
            .checked_mul(whole_size as i64)
            .ok_or_else(|| ImportError::Unsupported("gep offset overflow".into()))?;
        let mut cur_ty = source_ty;
        for &idx in rest {
            let BasicTypeEnum::ArrayType(array) = cur_ty else {
                return Err(ImportError::Unsupported(
                    "gep of non-array aggregate type not supported".into(),
                ));
            };
            let elem_ty = array.get_element_type();
            let step = idx
                .checked_mul(self.memory_byte_size(elem_ty)? as i64)
                .ok_or_else(|| ImportError::Unsupported("gep offset overflow".into()))?;
            offset = offset
                .checked_add(step)
                .ok_or_else(|| ImportError::Unsupported("gep offset overflow".into()))?;
            cur_ty = elem_ty;
        }
        u64::try_from(offset)
            .map_err(|_| ImportError::Unsupported("gep offset out of range".into()))
    }

    /// As [`Self::constant_gep_byte_offset`], but leave a symbolic GEP index
    /// for the dynamic GEP path instead of prematurely rejecting it.
    fn gep_instr_constant_offset(&self, instr: InstructionValue<'ctx>) -> IResult<Option<u64>> {
        let source_ty = instr
            .get_gep_source_element_type()
            .map_err(|_| ImportError::Unsupported("malformed gep".into()))?;
        let n_ops = instr.get_num_operands();
        let mut indices = Vec::with_capacity(n_ops.saturating_sub(1) as usize);
        for i in 1..n_ops {
            match instr.get_operand(i).and_then(|o| o.value()) {
                Some(BasicValueEnum::IntValue(n)) => match n.get_sign_extended_constant() {
                    Some(v) => indices.push(v),
                    None => return Ok(None),
                },
                _ => {
                    return Err(ImportError::Unsupported(
                        "expected an integer gep index".into(),
                    ));
                }
            }
        }
        self.constant_gep_byte_offset(source_ty, &indices).map(Some)
    }

    fn llvm_bit_width<T: TryInto<BasicTypeEnum<'ctx>>>(&self, ty: T) -> IResult<usize> {
        let ty = ty.try_into().map_err(|_| {
            ImportError::Unsupported(
                "expected an integer or default-address-space pointer value".into(),
            )
        })?;
        match ty {
            BasicTypeEnum::IntType(ty) => Ok(ty.get_bit_width() as usize),
            BasicTypeEnum::PointerType(ty) => {
                if ty.get_address_space() != AddressSpace::default() {
                    return Err(ImportError::Unsupported(
                        "non-default LLVM pointer address spaces are unsupported".into(),
                    ));
                }
                Ok(self.pointer_bits)
            }
            _ => Err(ImportError::Unsupported(
                "expected an integer or default-address-space pointer value".into(),
            )),
        }
    }

    fn ensure_default_address_space(&self, ptr: PointerValue<'ctx>) -> IResult<()> {
        if ptr.get_type().get_address_space() == AddressSpace::default() {
            Ok(())
        } else {
            Err(ImportError::Unsupported(
                "non-default LLVM pointer address spaces are unsupported".into(),
            ))
        }
    }

    fn func_id(&mut self, f: FunctionValue<'ctx>) -> IResult<FuncId> {
        let key = f.as_global_value().as_pointer_value();
        if let Some(&id) = self.func_ids.get(&key) {
            return Ok(id);
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
            .collect::<IResult<_>>()?;
        let results: Vec<TypeId> = match f.get_type().get_return_type() {
            Some(t) => vec![self.llvm_type_id(t)?],
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
        Ok(id)
    }

    fn llvm_type_id<T: TryInto<inkwell::types::BasicTypeEnum<'ctx>>>(
        &mut self,
        ty: T,
    ) -> IResult<TypeId> {
        use inkwell::types::BasicTypeEnum;
        let ty = ty
            .try_into()
            .unwrap_or_else(|_| panic!("expected a basic type"));
        Ok(match ty {
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
            // Pointer values use the module ABI width. Other unsupported
            // types retain the historical best-effort integer fallback.
            BasicTypeEnum::PointerType(ptr) => {
                if ptr.get_address_space() != AddressSpace::default() {
                    return Err(ImportError::Unsupported(
                        "non-default LLVM pointer address spaces are unsupported".into(),
                    ));
                }
                self.addr_tid
            }
            _ => self.types.primitive(Type::_32),
        })
    }

    /// Assign (allocating on first reference) the `StorageId` for a global
    /// variable, requiring the pointer operand to resolve to a literal
    /// global at import time — the generic "entire operation must be a
    /// constant load" rule, applied to every memory reference this importer
    /// emits.
    fn storage_for(&mut self, ptr: PointerValue<'ctx>) -> IResult<StorageId> {
        self.ensure_default_address_space(ptr)?;
        let raw = strip_pointer(ptr.as_value_ref(), "memory reference base")?;
        let global = global_from_pointer(raw, "memory reference base")?;
        let key = global.as_pointer_value();
        if let Some(&id) = self.storage_for_global.get(&key) {
            return Ok(id);
        }
        let id = match &mut self.storage_registry {
            Some(registry) => registry.register(StoragePurpose::LlvmGlobal {
                name: global.get_name().to_string_lossy().into_owned(),
            }),
            None => self.storage_alloc.alloc(),
        };
        self.storage_for_global.insert(key, id);
        Ok(id)
    }

    /// Assign every global *variable* in the module its `StorageId` up
    /// front (via `storage_for`, trivially resolved -- `strip_pointer`
    /// succeeds immediately on a bare global). Registers the whole module's
    /// globals, not just ones reachable from the entry points being
    /// imported: a runtime-dispatched pointer's caller may pass any
    /// address-taken global regardless of which function directly
    /// references it syntactically, so the dispatch candidate set must be
    /// conservative, not just "whatever's been seen so far."
    fn register_all_globals(&mut self, llvm_module: &LlvmModule<'ctx>) -> IResult<()> {
        let mut count = 0usize;
        for global in llvm_module.get_globals() {
            self.storage_for(global.as_pointer_value())?;
            count += 1;
        }
        if count > MAX_DISPATCH_CANDIDATES {
            return Err(ImportError::Unsupported(format!(
                "module has {count} globals, exceeding the {MAX_DISPATCH_CANDIDATES}-candidate \
                 limit for runtime pointer dispatch"
            )));
        }
        Ok(())
    }

    /// Like `storage_for`, but also folds in a constant-index `getelementptr`
    /// *constant expression* wrapping a global, walked to arbitrary depth.
    /// LLVM constant-folds `getelementptr (T, ptr @g, ...)` embedded
    /// directly in a Load/Store pointer operand into exactly this shape --
    /// `storage_for`/`strip_pointer` alone resolves straight through such a
    /// chain to `@g` at offset 0, silently discarding the index (`strip_pointer`
    /// always takes operand 0, which is the GEP's *base* pointer, never its
    /// indices). A GEP *instruction* (as opposed to a folded constant
    /// expression) is handled separately via `FuncCtx::global_ptr_of`,
    /// populated by the `GetElementPtr` opcode arm -- by construction, every
    /// index inside a `ConstantExpr` is itself already a compile-time
    /// constant (a symbolic index anywhere in the chain would have forced
    /// LLVM to represent this as an instruction instead), so this never
    /// needs to defer the way `gep_instr_constant_offset` does.
    fn storage_for_with_offset(&mut self, ptr: PointerValue<'ctx>) -> IResult<(StorageId, u64)> {
        let raw = ptr.as_value_ref();
        let is_gep_const_expr = !unsafe { LLVMIsAConstantExpr(raw) }.is_null()
            && unsafe { LLVMGetConstOpcode(raw) } == LLVMOpcode::LLVMGetElementPtr;
        if !is_gep_const_expr {
            return Ok((self.storage_for(ptr)?, 0));
        }
        let base_raw = unsafe { LLVMGetOperand(raw, 0) };
        let base_ptr = unsafe { PointerValue::new(base_raw) };
        let (storage, base_offset) = self.storage_for_with_offset(base_ptr)?;
        let source_ty = unsafe { BasicTypeEnum::new(LLVMGetGEPSourceElementType(raw)) };
        let n_ops = unsafe { LLVMGetNumOperands(raw) };
        let mut indices = Vec::with_capacity(usize::try_from(n_ops.saturating_sub(1)).unwrap_or(0));
        for i in 1..n_ops {
            let op = unsafe { LLVMGetOperand(raw, i as u32) };
            let iv = unsafe { IntValue::new(op) };
            let v = iv
                .get_sign_extended_constant()
                .ok_or_else(|| ImportError::Unsupported("expected a constant gep index".into()))?;
            indices.push(v);
        }
        let gep_offset = self.constant_gep_byte_offset(source_ty, &indices)?;
        let offset = base_offset
            .checked_add(gep_offset)
            .ok_or_else(|| ImportError::Unsupported("gep offset overflow".into()))?;
        Ok((storage, offset))
    }

    /// Resolve a pointer to its `GlobalPtr` (constant or already-dynamic),
    /// checking `FuncCtx::global_ptr_of` (a previously-tracked GEP
    /// instruction result -- constant or symbolic offset alike) before
    /// falling back to `storage_for_with_offset` (a bare global or a
    /// constant-index GEP constant expression, always constant).
    fn resolve_global_ptr(
        &mut self,
        fctx: &FuncCtx<'ctx>,
        ptr: PointerValue<'ctx>,
    ) -> IResult<GlobalPtr> {
        if let Some(gp) = fctx.global_ptr_of.get(&ptr.as_any_value_enum()) {
            return Ok(gp.clone());
        }
        let (storage, byte_offset) = self.storage_for_with_offset(ptr)?;
        Ok(GlobalPtr::Const(GlobalPointer {
            storage,
            byte_offset,
        }))
    }

    fn import_function(&mut self, f: FunctionValue<'ctx>) -> IResult<Vec<FunctionValue<'ctx>>> {
        let id = self.func_id(f)?;
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
            let width = self.llvm_bit_width(param.get_type())?;
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
                let width = self.llvm_bit_width(phi.as_instruction().get_type())?;
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

        let reachable = reachable_blocks(f)?;

        // Pass B: translate every non-phi instruction, then the terminator,
        // but only for blocks reachable through the terminators this importer
        // supports. Pass A intentionally still assigned all block IDs and
        // phi slots, so reachable targets retain their original identity.
        for (i, bb) in blocks.iter().enumerate() {
            if !reachable.contains(bb) {
                continue;
            }
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

        // Unreachable LLVM blocks retain their Pass-A placeholder IDs so no
        // reachable branch or phi needs remapping. They must nevertheless
        // have a well-typed terminator when later VAFFLE lowerings inspect
        // every block, rather than the old empty `Return` fallback (which is
        // ill-typed for a non-void enclosing function).
        let return_bits = match f.get_type().get_return_type() {
            Some(ty) => self.llvm_bit_width(ty)?,
            None => 0,
        };
        let mut fallback_return_values: Vec<Vec<ValueId>> = blocks
            .iter()
            .enumerate()
            .map(|(i, _)| {
                (!reachable.contains(&blocks[i]))
                    .then(|| {
                        (0..return_bits)
                            .map(|_| self.bc_const_at(&mut fctx, BlockId(i), false))
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .collect();
        // Symbolic memcpy loops append their own fully-terminated VAFFLE
        // blocks. Keep the fallback vector aligned with every block so
        // `finish_blocks` does not truncate them when zipping its vectors.
        fallback_return_values.resize_with(fctx.stmts.len(), Vec::new);
        let values = core::mem::take(&mut fctx.values);
        let body = FuncBody {
            sig,
            blocks: fctx.finish_blocks(fallback_return_values),
            values,
            entry: entry_block,
        };
        self.funcs[id.0] = FuncDecl::Body(body);
        Ok(called)
    }

    /// Resolve an LLVM value (instruction result, immediate, or already-cached
    /// param/phi) to its VAFFLE bits.
    ///
    /// Immediate integers deliberately bypass the function-wide cache. A
    /// `Stmt::Const` is emitted into the current block, so reusing its
    /// `ValueId`s from a sibling block violates VAFFLE's dominance invariant.
    /// Parameters, phis, and instruction results remain cached: LLVM SSA
    /// guarantees their definitions dominate their uses.
    fn value_bits(&mut self, fctx: &mut FuncCtx<'ctx>, v: BasicValueEnum<'ctx>) -> IResult<Bits> {
        if let BasicValueEnum::IntValue(i) = v {
            if i.is_const() {
                return self.int_const_bits(fctx, i);
            }
        }
        // Like immediate integers, LLVM's singleton `null` is a constant
        // that must be materialized in each use block rather than cached
        // function-wide. Caching a block-local `Stmt::Const` for a sibling
        // arm would violate VAFFLE dominance.
        if let BasicValueEnum::PointerValue(p) = v {
            self.ensure_default_address_space(p)?;
            if p.is_null() {
                return Ok(self.null_ptr_bits(fctx, fctx.current));
            }
        }
        if let Some(bits) = fctx.cache.get(&v.as_any_value_enum()) {
            return Ok(bits.clone());
        }
        let bits = match v {
            BasicValueEnum::IntValue(i) => self.int_const_bits(fctx, i)?,
            BasicValueEnum::PointerValue(p) => {
                let block = fctx.current;
                self.ptr_value_bits(fctx, block, p)?
            }
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
            InstructionOpcode::ExtractValue => Some(self.extract_overflow_field(fctx, instr)?),
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
                self.ensure_default_address_space(ptr)?;
                let n_bits = self.llvm_bit_width(instr.get_type())?;
                let n_bytes = n_bits.div_ceil(8);
                if let Some(stack_ptr) = fctx.stack_slot_of.get(&ptr.as_any_value_enum()).cloned() {
                    let ptr_bits0 = fctx
                        .cache
                        .get(&ptr.as_any_value_enum())
                        .and_then(|b| b.first().copied())
                        .ok_or_else(|| {
                            ImportError::Unsupported("stack pointer bits missing (internal)".into())
                        })?;
                    let pointee_tid = self.llvm_type_id(instr.get_type())?;
                    Some(match stack_ptr {
                        StackPtr::Const(sp) => {
                            self.stack_load(fctx, ptr_bits0, sp.addr, pointee_tid, n_bits)
                        }
                        StackPtr::Symbolic { addr_bits, .. } => self.stack_load_dynamic(
                            fctx,
                            ptr_bits0,
                            &addr_bits,
                            pointee_tid,
                            n_bits,
                        ),
                    })
                } else {
                    match self.resolve_global_ptr(fctx, ptr) {
                        Ok(GlobalPtr::Const(g)) => {
                            Some(self.mem_load(fctx, g.storage, g.byte_offset, n_bytes))
                        }
                        Ok(GlobalPtr::Symbolic {
                            storage,
                            offset_bits,
                        }) => Some(self.mem_load_dynamic(fctx, storage, &offset_bits, n_bytes)),
                        Err(_) => {
                            // Neither a tracked stack pointer nor a
                            // statically resolvable global -- e.g. a
                            // pointer function parameter, or a
                            // `phi`/`select`-merged value whose tag isn't a
                            // compile-time constant. Fall back to runtime
                            // storage-identity dispatch instead of failing
                            // closed.
                            let pointee_tid = self.llvm_type_id(instr.get_type())?;
                            let ptr_bits =
                                self.value_bits(fctx, BasicValueEnum::PointerValue(ptr))?;
                            Some(self.dispatch_read(fctx, &ptr_bits, pointee_tid, n_bytes)?)
                        }
                    }
                }
            }
            InstructionOpcode::Store => {
                let val = op!(0);
                let ptr = load_store_pointer(instr, 1)?;
                self.ensure_default_address_space(ptr)?;
                if let Some(stack_ptr) = fctx.stack_slot_of.get(&ptr.as_any_value_enum()).cloned() {
                    let ptr_bits0 = fctx
                        .cache
                        .get(&ptr.as_any_value_enum())
                        .and_then(|b| b.first().copied())
                        .ok_or_else(|| {
                            ImportError::Unsupported("stack pointer bits missing (internal)".into())
                        })?;
                    match stack_ptr {
                        StackPtr::Const(sp) => self.stack_store(fctx, ptr_bits0, sp.addr, &val),
                        StackPtr::Symbolic { addr_bits, .. } => {
                            self.stack_store_dynamic(fctx, ptr_bits0, &addr_bits, &val)
                        }
                    }
                } else {
                    let n_bytes = val.len().div_ceil(8);
                    match self.resolve_global_ptr(fctx, ptr) {
                        Ok(GlobalPtr::Const(g)) => {
                            self.mem_store(fctx, g.storage, g.byte_offset, &val, n_bytes)
                        }
                        Ok(GlobalPtr::Symbolic {
                            storage,
                            offset_bits,
                        }) => self.mem_store_dynamic(fctx, storage, &offset_bits, &val, n_bytes),
                        Err(_) => {
                            let ptr_bits =
                                self.value_bits(fctx, BasicValueEnum::PointerValue(ptr))?;
                            self.dispatch_write(fctx, &ptr_bits, &val)?;
                        }
                    }
                }
                None
            }
            InstructionOpcode::GetElementPtr => {
                let base = load_store_pointer(instr, 0)?;
                self.ensure_default_address_space(base)?;
                if let Some(base_ptr) = fctx.stack_slot_of.get(&base.as_any_value_enum()).cloned() {
                    // Offset GEP off a tracked stack pointer. A constant
                    // index against a `Const` base takes the original
                    // compile-time-constant fast path; anything else (a
                    // symbolic index, or a base that's already
                    // `Symbolic` from an earlier dynamic GEP) computes a
                    // genuinely runtime address via real bit-circuit
                    // multiply-and-add -- `rebase_stack_addr` already
                    // proves ALLOCA addressing tolerates this.
                    if instr.get_num_operands() != 2 {
                        return Err(ImportError::Unsupported(
                            "multi-index GEP into stack pointer not supported".into(),
                        ));
                    }
                    let elem_ty = instr
                        .get_gep_source_element_type()
                        .map_err(|_| ImportError::Unsupported("malformed gep".into()))?;
                    let elem_bits = self.llvm_bit_width(elem_ty).map_err(|_| {
                        ImportError::Unsupported(
                            "gep of non-integer/pointer element type into stack pointer not supported"
                                .into(),
                        )
                    })? as i64;
                    let idx_val =
                        instr
                            .get_operand(1)
                            .and_then(|o| o.value())
                            .ok_or_else(|| {
                                ImportError::Unsupported("expected an integer gep index".into())
                            })?;
                    let BasicValueEnum::IntValue(idx_int) = idx_val else {
                        return Err(ImportError::Unsupported(
                            "expected an integer gep index".into(),
                        ));
                    };

                    let new_stack_ptr = match (&base_ptr, idx_int.get_sign_extended_constant()) {
                        (StackPtr::Const(sp), Some(idx)) => {
                            let offset = idx.checked_mul(elem_bits).ok_or_else(|| {
                                ImportError::Unsupported("gep offset overflow".into())
                            })?;
                            let addr = sp.addr.checked_add_signed(offset).ok_or_else(|| {
                                ImportError::Unsupported("gep offset out of range".into())
                            })?;
                            StackPtr::Const(StackPointer { addr, ..*sp })
                        }
                        (_, _) => {
                            let (allocation_base, allocation_bits) = match &base_ptr {
                                StackPtr::Const(sp) => (sp.allocation_base, sp.allocation_bits),
                                StackPtr::Symbolic {
                                    allocation_base,
                                    allocation_bits,
                                    ..
                                } => (*allocation_base, *allocation_bits),
                            };
                            let base_bits = self.stack_ptr_addr_bits(fctx, cur, &base_ptr);
                            let idx_bits = self.value_bits(fctx, idx_val)?;
                            let idx_bits = resize_bits_signed(&idx_bits, self.pointer_bits);
                            let new_addr_bits = {
                                let mut c = Ctx {
                                    fctx,
                                    bit_tid: self.bit_tid,
                                    block: cur,
                                };
                                let elem_bits_const: Vec<ValueId> = (0..self.pointer_bits)
                                    .map(|b| c.bc_const((elem_bits as u64 >> b) & 1 != 0))
                                    .collect();
                                let scaled = circuits::bc_mul(&mut c, &idx_bits, &elem_bits_const);
                                circuits::bc_add(&mut c, &base_bits, &scaled, false)
                            };
                            StackPtr::Symbolic {
                                allocation_base,
                                allocation_bits,
                                addr_bits: new_addr_bits,
                            }
                        }
                    };

                    let addr_bits = self.stack_ptr_addr_bits(fctx, cur, &new_stack_ptr);
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
                        .insert(instr.as_any_value_enum(), new_stack_ptr);
                    Some(addr_bits)
                } else if let Ok(base_gp) = self.resolve_global_ptr(fctx, base) {
                    // Base is a global (directly, a constant-index GEP
                    // constant expression, or a previously-tracked
                    // `GlobalPtr` from a chained GEP instruction -- constant
                    // or already-dynamic offset alike).
                    let const_offset = self.gep_instr_constant_offset(instr)?;

                    let new_gp = if let (GlobalPtr::Const(g), Some(gep_offset)) =
                        (&base_gp, const_offset)
                    {
                        // Fast path: everything resolves at compile time
                        // (multi-index, nested-array GEPs included).
                        let byte_offset =
                            g.byte_offset.checked_add(gep_offset).ok_or_else(|| {
                                ImportError::Unsupported("gep offset overflow".into())
                            })?;
                        Some(GlobalPtr::Const(GlobalPointer {
                            storage: g.storage,
                            byte_offset,
                        }))
                    } else if instr.get_num_operands() == 2 {
                        // Dynamic path: a single index (constant or
                        // symbolic) into a scalar-integer element type --
                        // reached when the base is already `Symbolic`, or
                        // this GEP's own index is symbolic. The same shape
                        // the stack arm supports, and what the `xs[i]`
                        // motivating case needs. Multi-index dynamic GEPs
                        // remain deferred (left untracked below).
                        let elem_ty = instr
                            .get_gep_source_element_type()
                            .map_err(|_| ImportError::Unsupported("malformed gep".into()))?;
                        match elem_ty {
                            inkwell::types::BasicTypeEnum::IntType(_)
                            | inkwell::types::BasicTypeEnum::PointerType(_) => {
                                let elem_bytes = (self.llvm_bit_width(elem_ty)? as u64).div_ceil(8);
                                match instr.get_operand(1).and_then(|o| o.value()) {
                                    Some(idx_bv @ BasicValueEnum::IntValue(_)) => {
                                        let storage = match &base_gp {
                                            GlobalPtr::Const(g) => g.storage,
                                            GlobalPtr::Symbolic { storage, .. } => *storage,
                                        };
                                        let base_offset_bits =
                                            self.global_ptr_offset_bits(fctx, cur, &base_gp);
                                        let idx_bits = self.value_bits(fctx, idx_bv)?;
                                        let idx_bits =
                                            resize_bits_signed(&idx_bits, self.pointer_bits);
                                        let new_offset_bits = {
                                            let mut c = Ctx {
                                                fctx,
                                                bit_tid: self.bit_tid,
                                                block: cur,
                                            };
                                            let elem_bytes_const: Vec<ValueId> = (0..self
                                                .pointer_bits)
                                                .map(|b| c.bc_const((elem_bytes >> b) & 1 != 0))
                                                .collect();
                                            let scaled = circuits::bc_mul(
                                                &mut c,
                                                &idx_bits,
                                                &elem_bytes_const,
                                            );
                                            circuits::bc_add(
                                                &mut c,
                                                &base_offset_bits,
                                                &scaled,
                                                false,
                                            )
                                        };
                                        Some(GlobalPtr::Symbolic {
                                            storage,
                                            offset_bits: new_offset_bits,
                                        })
                                    }
                                    _ => None,
                                }
                            }
                            _ => None,
                        }
                    } else {
                        None
                    };

                    if let Some(gp) = new_gp {
                        fctx.global_ptr_of.insert(instr.as_any_value_enum(), gp);
                    }
                    None
                } else {
                    // Base has genuinely unknown provenance (e.g. a pointer
                    // function parameter) -- neither a tracked stack
                    // pointer nor resolvable to any known global. Compute
                    // the offset as ordinary bit-circuit arithmetic
                    // directly on the base's own uniform tagged `Bits`
                    // (`ptr_value_bits`'s encoding, already available for
                    // any pointer via `value_bits` -- a parameter's raw
                    // bits are cached from entry-block setup regardless of
                    // provenance). This is correct for whichever concrete
                    // storage the pointer turns out to name at runtime: the
                    // offset lives in the low ADDR bits either way (every
                    // bit, for a stack destination; the low
                    // `GLOBAL_ADDR_BITS`, for a global one), with the
                    // tag+ID bits above untouched by an in-bounds add. The
                    // result stays provenance-unresolved; a later
                    // `Load`/`Store` decodes and dispatches on it at
                    // runtime (`dispatch_read`/`dispatch_write`).
                    if instr.get_num_operands() != 2 {
                        return Err(ImportError::Unsupported(
                            "multi-index GEP into an unresolved pointer not supported".into(),
                        ));
                    }
                    let elem_ty = instr
                        .get_gep_source_element_type()
                        .map_err(|_| ImportError::Unsupported("malformed gep".into()))?;
                    let elem_bytes = match elem_ty {
                        inkwell::types::BasicTypeEnum::IntType(_)
                        | inkwell::types::BasicTypeEnum::PointerType(_) => {
                            (self.llvm_bit_width(elem_ty)? as u64).div_ceil(8)
                        }
                        _ => {
                            return Err(ImportError::Unsupported(
                                "gep of non-integer/pointer element type into an unresolved pointer not \
                                 supported"
                                    .into(),
                            ));
                        }
                    };
                    let idx_val =
                        instr
                            .get_operand(1)
                            .and_then(|o| o.value())
                            .ok_or_else(|| {
                                ImportError::Unsupported("expected an integer gep index".into())
                            })?;
                    if !matches!(idx_val, BasicValueEnum::IntValue(_)) {
                        return Err(ImportError::Unsupported(
                            "expected an integer gep index".into(),
                        ));
                    }
                    let base_bits = self.value_bits(fctx, BasicValueEnum::PointerValue(base))?;
                    let idx_bits = self.value_bits(fctx, idx_val)?;
                    let idx_bits = resize_bits_signed(&idx_bits, self.pointer_bits);
                    let new_bits = {
                        let mut c = Ctx {
                            fctx,
                            bit_tid: self.bit_tid,
                            block: cur,
                        };
                        let elem_bytes_const: Vec<ValueId> = (0..self.pointer_bits)
                            .map(|b| c.bc_const((elem_bytes >> b) & 1 != 0))
                            .collect();
                        let scaled = circuits::bc_mul(&mut c, &idx_bits, &elem_bytes_const);
                        circuits::bc_add(&mut c, &base_bits, &scaled, false)
                    };
                    Some(new_bits)
                }
            }
            InstructionOpcode::Call => {
                let call = CallSiteValue::try_from(instr)
                    .map_err(|_| ImportError::Unsupported("malformed call instruction".into()))?;
                let callee_fn = call
                    .get_called_fn_value()
                    .ok_or_else(|| ImportError::Unsupported("indirect call".into()))?;
                let callee_name = callee_fn.get_name().to_string_lossy();
                if is_ignored_llvm_intrinsic(&callee_name) {
                    // Debug, lifetime, and alias-analysis intrinsics carry
                    // no circuit dataflow. In particular,
                    // `noalias.scope.decl` has a metadata operand that
                    // inkwell cannot represent as a `BasicValueEnum`; skip
                    // it before generic call-argument conversion.
                    None
                } else if let Some(intrinsic) = MemoryIntrinsic::from_name(&callee_name) {
                    self.translate_memory_intrinsic(fctx, instr, intrinsic)?;
                    None
                } else if let Some(intrinsic) = OverflowIntrinsic::from_name(&callee_name) {
                    let fields = self.translate_overflow_intrinsic(fctx, instr, intrinsic)?;
                    fctx.aggregate_fields
                        .insert(instr.as_any_value_enum(), fields);
                    None
                } else {
                    // Validate arguments before asking `func_id` to inspect
                    // the callee signature. `FunctionValue::get_params`
                    // itself assumes every parameter is a BasicValue and
                    // would otherwise hit inkwell's metadata panic before
                    // `call_arg_bits` can turn this into ImportError.
                    let args = self.call_arg_bits(fctx, instr)?;
                    let tail_end = tail_call_end(instr);
                    // A `ReturnCall` has no continuation, so it can only
                    // target a defined function when it represents LLVM's
                    // ordinary `call; ret` shape. A body-less declaration is
                    // nevertheless meaningful for `call; unreachable`: it
                    // models an aborting direct call and never returns into
                    // the enclosing function.
                    let is_tail_call = matches!(tail_end, Some(TailCallEnd::Unreachable))
                        || (matches!(tail_end, Some(TailCallEnd::Return))
                            && callee_fn.get_first_basic_block().is_some());
                    let callee_id = self.func_id(callee_fn)?;
                    called.push(callee_fn);
                    if is_tail_call {
                        fctx.terminators[cur.0] = Some(Terminator::ReturnCall {
                            func: callee_id,
                            args,
                        });
                        None
                    } else {
                        let vid = fctx.emit(
                            cur,
                            Value::Call {
                                func: callee_id,
                                args,
                            },
                        );
                        match instr.get_type().try_into() {
                            Ok(inkwell::types::BasicTypeEnum::IntType(_))
                            | Ok(inkwell::types::BasicTypeEnum::PointerType(_)) => {
                                let n = self.llvm_bit_width(instr.get_type())?;
                                Some(
                                    (0..n)
                                        .map(|i| {
                                            fctx.emit(cur, Value::Output { value: vid, idx: i })
                                        })
                                        .collect(),
                                )
                            }
                            _ => None,
                        }
                    }
                }
            }
            InstructionOpcode::Alloca => {
                // LLVM permits an explicit alloca address space. This
                // importer only models the default pointer address space.
                self.llvm_bit_width(instr.get_type())?;
                let elem_ty = instr
                    .get_allocated_type()
                    .map_err(|_| ImportError::Unsupported("malformed alloca".into()))?;
                // Flatten a (possibly nested) array type down to its
                // innermost scalar integer or pointer element type and total
                // element count -- `[16 x i8]` becomes (i8, 16), `[4 x [4 x
                // i8]]` becomes (i8, 16), and a bare scalar is (ty, 1). Structs
                // (and anything else) are a named error, not a panic --
                // see docs/llvm-array-alloca.md item 3.
                let (scalar_ty, array_count) = flatten_alloca_type(elem_ty).ok_or_else(|| {
                    ImportError::Unsupported(
                        "alloca of non-integer/pointer, non-array-of-integer/pointer type not supported".into(),
                    )
                })?;
                let elem_bits = self.llvm_bit_width(scalar_ty)? as u64;
                // The array-size operand is `1` unless the source used
                // `alloca <ty>, <n>`; either way it must be a compile-time
                // constant (VLAs are symbolic and fail closed here, not via
                // a panic).
                let alloca_count: u64 = match instr.get_operand(0).and_then(|o| o.value()) {
                    Some(BasicValueEnum::IntValue(n)) => {
                        n.get_zero_extended_constant().ok_or_else(|| {
                            ImportError::Unsupported("alloca count is symbolic".into())
                        })?
                    }
                    _ => 1,
                };
                let count = array_count
                    .checked_mul(alloca_count)
                    .ok_or_else(|| ImportError::Unsupported("alloca size overflow".into()))?;
                let total_slots = elem_bits
                    .checked_mul(count)
                    .ok_or_else(|| ImportError::Unsupported("alloca size overflow".into()))?;
                let base_slot = fctx.next_stack_slot;
                fctx.next_stack_slot = fctx
                    .next_stack_slot
                    .checked_add(total_slots)
                    .ok_or_else(|| ImportError::Unsupported("alloca stack overflow".into()))?;
                // `ptr_value_bits` reserves bit 31 of a pointer *value*'s
                // encoding as the stack-vs-global tag (0 = stack); a local
                // ALLOCA offset that set it would be indistinguishable from
                // a global-provenance value.
                if fctx.next_stack_slot >= (1u64 << (self.pointer_bits - 1)) {
                    return Err(ImportError::Unsupported(
                        "alloca stack region too large for the pointer-value encoding".into(),
                    ));
                }

                // Bookkeeping marker (unused as an operand, matching
                // `VaffleTarget::alloca`'s own `_alloc_vid` convention) —
                // required so passes that pattern-match `Value::StackAlloc`
                // (e.g. `inline_vaffle`'s stack-slot rebase, `lower_to_ir`'s
                // spill-avoidance) see this allocation.
                let elem_tid = self.llvm_type_id(scalar_ty)?;
                fctx.emit(
                    cur,
                    Value::StackAlloc {
                        elem_ty: elem_tid,
                        count: count as usize,
                        base_slot,
                    },
                );

                let addr_bits = self.stack_addr_bits(fctx, cur, base_slot);
                fctx.stack_slot_of.insert(
                    instr.as_any_value_enum(),
                    StackPtr::Const(StackPointer {
                        allocation_base: base_slot,
                        allocation_bits: total_slots,
                        addr: base_slot,
                    }),
                );
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

    /// Lower supported LLVM memory intrinsics before they can become a
    /// declaration-only `Value::Call`. Constant lengths use direct storage
    /// operations. A symbolic `memcpy` or `memset` is an importer-generated
    /// byte loop: its dynamic condition remains real VAFFLE CFG for
    /// movfuscation's step circuit instead of being expanded to a fixed
    /// maximum byte count.
    fn translate_memory_intrinsic(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        instr: InstructionValue<'ctx>,
        intrinsic: MemoryIntrinsic,
    ) -> IResult<()> {
        let n_args = instr.get_num_operands().saturating_sub(1);
        let expected_args = match intrinsic {
            MemoryIntrinsic::Memset => 4,
            MemoryIntrinsic::Memcpy | MemoryIntrinsic::Memmove => 4,
        };
        if n_args != expected_args {
            return Err(ImportError::Unsupported(format!(
                "llvm.{} has unexpected operand count {n_args}",
                intrinsic.name()
            )));
        }

        match intrinsic {
            MemoryIntrinsic::Memset => {
                let dest_ptr = load_store_pointer(instr, 0)?;
                let length = self.memory_intrinsic_length(fctx, instr, 2, intrinsic)?;
                memory_intrinsic_nonvolatile(instr, 3, intrinsic)?;

                let fill = call_value_operand(instr, 1, "memset fill byte")?;
                let mut fill_bits = self.value_bits(fctx, fill)?;
                fill_bits.truncate(8);
                while fill_bits.len() < 8 {
                    fill_bits.push(self.bc_const_at(fctx, fctx.current, false));
                }
                match length {
                    MemoryIntrinsicLength::Constant(n_bytes) => {
                        let dest = self.intrinsic_pointer(fctx, dest_ptr)?;
                        self.validate_intrinsic_pointer(&dest, n_bytes)?;
                        let n_bits = n_bytes.checked_mul(8).ok_or_else(|| {
                            ImportError::Unsupported("memory intrinsic length is too large".into())
                        })?;
                        let mut bytes = Vec::with_capacity(n_bits);
                        for _ in 0..n_bytes {
                            bytes.extend_from_slice(&fill_bits);
                        }
                        self.intrinsic_store(fctx, &dest, &bytes)
                    }
                    MemoryIntrinsicLength::Symbolic { bits } => {
                        let dest_bits =
                            self.value_bits(fctx, BasicValueEnum::PointerValue(dest_ptr))?;
                        self.symbolic_memory_loop(
                            fctx,
                            &dest_bits,
                            &bits,
                            SymbolicByteOperation::Fill { byte: &fill_bits },
                        )
                    }
                }
            }
            MemoryIntrinsic::Memcpy | MemoryIntrinsic::Memmove => {
                let dest_ptr = load_store_pointer(instr, 0)?;
                let src_ptr = load_store_pointer(instr, 1)?;
                let length = self.memory_intrinsic_length(fctx, instr, 2, intrinsic)?;
                memory_intrinsic_nonvolatile(instr, 3, intrinsic)?;
                match length {
                    MemoryIntrinsicLength::Constant(n_bytes) => {
                        let dest = self.intrinsic_pointer(fctx, dest_ptr)?;
                        let src = self.intrinsic_pointer(fctx, src_ptr)?;
                        self.validate_intrinsic_pointer(&dest, n_bytes)?;
                        self.validate_intrinsic_pointer(&src, n_bytes)?;
                        if intrinsic == MemoryIntrinsic::Memcpy
                            && intrinsic_ranges_overlap(&dest, &src, n_bytes)?
                        {
                            return Err(ImportError::Unsupported(
                                "memcpy source and destination overlap".into(),
                            ));
                        }

                        let bytes = self.intrinsic_load(fctx, &src, n_bytes)?;
                        self.intrinsic_store(fctx, &dest, &bytes)
                    }
                    MemoryIntrinsicLength::Symbolic { bits } => {
                        let dest_bits =
                            self.value_bits(fctx, BasicValueEnum::PointerValue(dest_ptr))?;
                        let src_bits =
                            self.value_bits(fctx, BasicValueEnum::PointerValue(src_ptr))?;
                        self.symbolic_memory_loop(
                            fctx,
                            &dest_bits,
                            &bits,
                            SymbolicByteOperation::Copy {
                                src_base: &src_bits,
                            },
                        )
                    }
                }
            }
        }
    }

    /// Insert a three-block byte loop at the current source position:
    /// `current → header(index) → body → header`, with `header → continue`
    /// once `index == length`. Both original pointers dominate the new CFG;
    /// the header's bit-decomposed block parameters carry only the loop index.
    fn symbolic_memory_loop(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        dest_base: &Bits,
        length_bits: &Bits,
        operation: SymbolicByteOperation<'_>,
    ) -> IResult<()> {
        if length_bits.is_empty() {
            return Err(ImportError::Unsupported(
                "symbolic memory intrinsic length must not be empty".into(),
            ));
        }
        let preheader = fctx.current;
        let header = fctx.append_block();
        let body = fctx.append_block();
        let continuation = fctx.append_block();

        let zero_index: Bits = (0..length_bits.len())
            .map(|_| self.bc_const_at(fctx, preheader, false))
            .collect();
        fctx.terminators[preheader.0] = Some(Terminator::Jump(Target {
            block: header,
            args: zero_index,
            reentry: None,
        }));

        fctx.current = header;
        let index_bits: Bits = (0..length_bits.len())
            .map(|index| fctx.add_param(header, self.bit_tid, index))
            .collect();
        let active = {
            let mut c = Ctx {
                fctx,
                bit_tid: self.bit_tid,
                block: header,
            };
            circuits::bc_ult(&mut c, &index_bits, length_bits)
        };
        fctx.terminators[header.0] = Some(Terminator::IfNonzero {
            cond: active,
            then_target: Target {
                block: body,
                args: Vec::new(),
                reentry: None,
            },
            else_target: Target {
                block: continuation,
                args: Vec::new(),
                reentry: None,
            },
        });

        fctx.current = body;
        let dest = self.pointer_with_byte_offset(fctx, body, dest_base, &index_bits);
        let byte = match operation {
            SymbolicByteOperation::Copy { src_base } => {
                let src = self.pointer_with_byte_offset(fctx, body, src_base, &index_bits);
                self.dispatch_read(fctx, &src, self.byte_tid, 1)?
            }
            SymbolicByteOperation::Fill { byte } => byte.clone(),
        };
        self.dispatch_write(fctx, &dest, &byte)?;
        let next_index = {
            let mut c = Ctx {
                fctx,
                bit_tid: self.bit_tid,
                block: body,
            };
            let one: Bits = (0..index_bits.len())
                .map(|bit| c.bc_const(bit == 0))
                .collect();
            circuits::bc_add(&mut c, &index_bits, &one, false)
        };
        fctx.terminators[body.0] = Some(Terminator::Jump(Target {
            block: header,
            args: next_index,
            reentry: None,
        }));

        // The source block's remaining LLVM instructions and terminator are
        // translated into this continuation. It is deliberately left without
        // a terminator until that normal walk resumes.
        fctx.current = continuation;
        Ok(())
    }

    /// Offset a uniform pointer value by a byte count. Stack pointers are
    /// bit-addressed while globals are byte-addressed, so select `index * 8`
    /// for tag 0 and `index` for tag 1 before adding. As with ordinary GEP
    /// lowering, defined executions must keep the addition within the
    /// encoding's address sub-field so it cannot carry into tag/ID bits.
    fn pointer_with_byte_offset(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        block: BlockId,
        base: &Bits,
        index: &Bits,
    ) -> Bits {
        let mut index = index.clone();
        index.truncate(self.pointer_bits);
        while index.len() < self.pointer_bits {
            index.push(self.bc_const_at(fctx, block, false));
        }
        let stack_scale: Bits = (0..self.pointer_bits)
            .map(|bit| self.bc_const_at(fctx, block, bit == 3))
            .collect();
        let stack_offset = {
            let mut c = Ctx {
                fctx,
                bit_tid: self.bit_tid,
                block,
            };
            circuits::bc_mul(&mut c, &index, &stack_scale)
        };
        let tag = base[self.pointer_bits - 1];
        let offset = {
            let mut c = Ctx {
                fctx,
                bit_tid: self.bit_tid,
                block,
            };
            circuits::bc_select_vec(&mut c, tag, &index, &stack_offset)
        };
        let mut c = Ctx {
            fctx,
            bit_tid: self.bit_tid,
            block,
        };
        circuits::bc_add(&mut c, base, &offset, false)
    }

    fn memory_intrinsic_length(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        instr: InstructionValue<'ctx>,
        index: u32,
        intrinsic: MemoryIntrinsic,
    ) -> IResult<MemoryIntrinsicLength> {
        let BasicValueEnum::IntValue(length) =
            call_value_operand(instr, index, "memory intrinsic length")?
        else {
            return Err(ImportError::Unsupported(format!(
                "llvm.{} length must be an integer",
                intrinsic.name()
            )));
        };
        if let Some(length) = length.get_zero_extended_constant() {
            return usize::try_from(length)
                .map(MemoryIntrinsicLength::Constant)
                .map_err(|_| {
                    ImportError::Unsupported(format!(
                        "llvm.{} length does not fit usize",
                        intrinsic.name()
                    ))
                });
        }
        if !matches!(intrinsic, MemoryIntrinsic::Memcpy | MemoryIntrinsic::Memset) {
            return Err(ImportError::Unsupported(format!(
                "symbolic llvm.{} length is not supported",
                intrinsic.name()
            )));
        }
        let bits = self.value_bits(fctx, BasicValueEnum::IntValue(length))?;
        Ok(MemoryIntrinsicLength::Symbolic { bits })
    }

    fn call_arg_bits(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        instr: InstructionValue<'ctx>,
    ) -> IResult<Bits> {
        let n_args = instr.get_num_operands().saturating_sub(1);
        let mut args = Vec::new();
        for i in 0..n_args {
            let value = call_value_operand(instr, i, "call argument")?;
            args.extend(self.value_bits(fctx, value)?);
        }
        Ok(args)
    }

    /// Lower LLVM's fixed two-field arithmetic-overflow aggregates. They are
    /// never materialized as first-class aggregate values: the result and its
    /// overflow bit are kept under the call instruction until an
    /// `extractvalue` projects one of them.
    fn translate_overflow_intrinsic(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        instr: InstructionValue<'ctx>,
        intrinsic: OverflowIntrinsic,
    ) -> IResult<Vec<Bits>> {
        let n_args = instr.get_num_operands().saturating_sub(1);
        if n_args != 2 {
            return Err(ImportError::Unsupported(format!(
                "{} has unexpected operand count {n_args}",
                intrinsic.name()
            )));
        }

        let a = overflow_integer_operand(instr, 0, intrinsic)?;
        let b = overflow_integer_operand(instr, 1, intrinsic)?;
        if a.get_type().get_bit_width() != b.get_type().get_bit_width() {
            return Err(ImportError::Unsupported(format!(
                "{} operands must have the same integer width",
                intrinsic.name()
            )));
        }

        let a = self.value_bits(fctx, BasicValueEnum::IntValue(a))?;
        let b = self.value_bits(fctx, BasicValueEnum::IntValue(b))?;
        let n_bits = a.len();
        if n_bits == 0 {
            return Err(ImportError::Unsupported(format!(
                "{} operands must not be empty",
                intrinsic.name()
            )));
        }
        let cur = fctx.current;
        let mut c = Ctx {
            fctx,
            bit_tid: self.bit_tid,
            block: cur,
        };

        let (result, overflow) = match intrinsic {
            OverflowIntrinsic::SAdd => {
                let result = circuits::bc_add(&mut c, &a, &b, false);
                let input_signs_differ = c.bc_xor(a[n_bits - 1], b[n_bits - 1]);
                let input_signs_match = c.bc_not(input_signs_differ);
                let result_sign_changed = c.bc_xor(result[n_bits - 1], a[n_bits - 1]);
                let overflow = c.bc_and(input_signs_match, result_sign_changed);
                (result, overflow)
            }
            OverflowIntrinsic::UAdd => {
                let result = circuits::bc_add(&mut c, &a, &b, false);
                let overflow = circuits::bc_ult(&mut c, &result, &a);
                (result, overflow)
            }
            OverflowIntrinsic::SSub => {
                let result = circuits::bc_sub(&mut c, &a, &b);
                let input_signs_differ = c.bc_xor(a[n_bits - 1], b[n_bits - 1]);
                let result_sign_changed = c.bc_xor(result[n_bits - 1], a[n_bits - 1]);
                let overflow = c.bc_and(input_signs_differ, result_sign_changed);
                (result, overflow)
            }
            OverflowIntrinsic::USub => {
                let result = circuits::bc_sub(&mut c, &a, &b);
                let overflow = circuits::bc_ult(&mut c, &a, &b);
                (result, overflow)
            }
            OverflowIntrinsic::SMul => {
                let result = circuits::bc_mul(&mut c, &a, &b);
                let wide_bits = n_bits.checked_mul(2).ok_or_else(|| {
                    ImportError::Unsupported(format!(
                        "{} operand width is too large",
                        intrinsic.name()
                    ))
                })?;
                let a_sign = a[n_bits - 1];
                let b_sign = b[n_bits - 1];
                let result_sign = result[n_bits - 1];
                let mut a_wide = a.clone();
                let mut b_wide = b.clone();
                let mut result_wide = result.clone();
                while a_wide.len() < wide_bits {
                    a_wide.push(a_sign);
                    b_wide.push(b_sign);
                    result_wide.push(result_sign);
                }
                let full = circuits::bc_mul(&mut c, &a_wide, &b_wide);
                let overflow = circuits::bc_ne(&mut c, &full, &result_wide);
                (result, overflow)
            }
            OverflowIntrinsic::UMul => {
                let wide_bits = n_bits.checked_mul(2).ok_or_else(|| {
                    ImportError::Unsupported(format!(
                        "{} operand width is too large",
                        intrinsic.name()
                    ))
                })?;
                let mut a_wide = a.clone();
                let mut b_wide = b.clone();
                while a_wide.len() < wide_bits {
                    a_wide.push(c.bc_const(false));
                    b_wide.push(c.bc_const(false));
                }
                let full = circuits::bc_mul(&mut c, &a_wide, &b_wide);
                let result = full[..n_bits].to_vec();
                let mut overflow = c.bc_const(false);
                for bit in &full[n_bits..] {
                    overflow = c.bc_or(overflow, *bit);
                }
                (result, overflow)
            }
        };

        Ok(vec![result, vec![overflow]])
    }

    fn extract_overflow_field(
        &self,
        fctx: &FuncCtx<'ctx>,
        instr: InstructionValue<'ctx>,
    ) -> IResult<Bits> {
        let aggregate = call_value_operand(instr, 0, "extractvalue aggregate")?;
        let fields = fctx
            .aggregate_fields
            .get(&aggregate.as_any_value_enum())
            .ok_or_else(|| {
                ImportError::Unsupported(
                    "extractvalue of an untracked aggregate is not supported".into(),
                )
            })?;
        let indices = instr.get_indices();
        let [field] = indices.as_slice() else {
            return Err(ImportError::Unsupported(
                "extractvalue must select exactly one aggregate field".into(),
            ));
        };
        fields.get(*field as usize).cloned().ok_or_else(|| {
            ImportError::Unsupported(format!(
                "extractvalue field {field} is outside the tracked aggregate"
            ))
        })
    }

    fn intrinsic_pointer(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        ptr: PointerValue<'ctx>,
    ) -> IResult<IntrinsicPointer> {
        if let Some(stack_ptr) = fctx.stack_slot_of.get(&ptr.as_any_value_enum()).cloned() {
            let ptr_bits0 = fctx
                .cache
                .get(&ptr.as_any_value_enum())
                .and_then(|bits| bits.first().copied())
                .ok_or_else(|| {
                    ImportError::Unsupported("stack pointer bits missing (internal)".into())
                })?;
            Ok(IntrinsicPointer::Stack {
                ptr: stack_ptr,
                ptr_bits0,
            })
        } else {
            match self.resolve_global_ptr(fctx, ptr) {
                // Unlike the former bare-global check, this keeps the byte
                // offset folded by `global_ptr_of` or
                // `storage_for_with_offset`. A constant-expression GEP
                // must never silently become a byte-zero access.
                Ok(GlobalPtr::Const(global)) => Ok(IntrinsicPointer::Global {
                    storage: global.storage,
                    byte_offset: global.byte_offset,
                }),
                // A symbolic global offset and a pointer of genuinely
                // unknown provenance both already have a uniform tagged
                // `Bits` encoding. Reuse the ordinary load/store runtime
                // dispatch rather than treating a constant-size intrinsic
                // as a declaration-only call or rejecting a slice pointer
                // solely for its provenance.
                Ok(GlobalPtr::Symbolic { .. }) | Err(_) => Ok(IntrinsicPointer::Dispatch {
                    ptr_bits: self.value_bits(fctx, BasicValueEnum::PointerValue(ptr))?,
                }),
            }
        }
    }

    fn validate_intrinsic_pointer(&self, ptr: &IntrinsicPointer, n_bytes: usize) -> IResult<()> {
        if let IntrinsicPointer::Stack {
            ptr: StackPtr::Const(ptr),
            ..
        } = ptr
        {
            (*ptr).intrinsic_range(n_bytes)?;
        }
        Ok(())
    }

    fn intrinsic_load(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        ptr: &IntrinsicPointer,
        n_bytes: usize,
    ) -> IResult<Bits> {
        match ptr {
            IntrinsicPointer::Stack {
                ptr: StackPtr::Const(ptr),
                ptr_bits0,
            } => {
                let n_bits = n_bytes.checked_mul(8).ok_or_else(|| {
                    ImportError::Unsupported("memory intrinsic length is too large".into())
                })?;
                Ok(self.stack_load(fctx, *ptr_bits0, ptr.addr, self.byte_tid, n_bits))
            }
            IntrinsicPointer::Stack {
                ptr: StackPtr::Symbolic { addr_bits, .. },
                ptr_bits0,
            } => {
                let n_bits = n_bytes.checked_mul(8).ok_or_else(|| {
                    ImportError::Unsupported("memory intrinsic length is too large".into())
                })?;
                Ok(self.stack_load_dynamic(fctx, *ptr_bits0, addr_bits, self.byte_tid, n_bits))
            }
            IntrinsicPointer::Global {
                storage,
                byte_offset,
            } => Ok(self.mem_load(fctx, *storage, *byte_offset, n_bytes)),
            IntrinsicPointer::Dispatch { ptr_bits } => {
                self.dispatch_read(fctx, ptr_bits, self.byte_tid, n_bytes)
            }
        }
    }

    fn intrinsic_store(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        ptr: &IntrinsicPointer,
        bytes: &Bits,
    ) -> IResult<()> {
        match ptr {
            IntrinsicPointer::Stack {
                ptr: StackPtr::Const(ptr),
                ptr_bits0,
            } => {
                self.stack_store(fctx, *ptr_bits0, ptr.addr, bytes);
            }
            IntrinsicPointer::Stack {
                ptr: StackPtr::Symbolic { addr_bits, .. },
                ptr_bits0,
            } => self.stack_store_dynamic(fctx, *ptr_bits0, addr_bits, bytes),
            IntrinsicPointer::Global {
                storage,
                byte_offset,
            } => {
                self.mem_store(fctx, *storage, *byte_offset, bytes, bytes.len().div_ceil(8));
            }
            IntrinsicPointer::Dispatch { ptr_bits } => {
                self.dispatch_write(fctx, ptr_bits, bytes)?;
            }
        }
        Ok(())
    }

    /// Bit-decompose a compile-time-constant `u64` (a `StorageId::ALLOCA`
    /// address, *or* a global's constant byte offset — the encoding is
    /// identical, just a plain unsigned integer), `PTR_BITS` wide, LSB
    /// first. Mirrors `VaffleTarget::alloca`'s `addr_bits` construction
    /// exactly.
    fn stack_addr_bits(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        block: BlockId,
        base_slot: u64,
    ) -> Bits {
        (0..self.pointer_bits)
            .map(|i| self.bc_const_at(fctx, block, (base_slot >> i) & 1 != 0))
            .collect()
    }

    /// Read `n_bits` individual bits from `StorageId::ALLOCA` starting at
    /// `base_slot`, one `StorageRead` per bit (matches `VaffleTarget::
    /// ptr_load`'s per-bit granularity, but with a compile-time-constant
    /// address per bit instead of a runtime-composed one, since this
    /// importer only tracks compile-time-constant stack pointers).
    /// `StorageId::ALLOCA`, not `StorageId::STACK` -- see that constant's
    /// own doc comment for why sharing `STACK` is unsafe.
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
                    self.addr_tid,
                )),
            );
            let bit = fctx.emit(
                cur,
                Value::Op(Stmt::StorageRead {
                    storage: self.convention.alloca_marker,
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

    /// Write `val` to `StorageId::ALLOCA` starting at `base_slot`, one
    /// `StorageWrite` per bit. See [`Self::stack_load`].
    fn stack_store(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        ptr_bits0: ValueId,
        base_slot: u64,
        val: &Bits,
    ) {
        let cur = fctx.current;
        for (i, &bit) in val.iter().enumerate() {
            let addr = fctx.emit(
                cur,
                Value::Op(Stmt::Const(
                    Constant {
                        hi: 0,
                        lo: (base_slot + i as u64) as u128,
                    },
                    self.addr_tid,
                )),
            );
            fctx.emit(
                cur,
                Value::Op(Stmt::StorageWrite {
                    storage: self.convention.alloca_marker,
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

    /// Bit-decompose a `StackPtr`'s current address into a `PTR_BITS`-wide
    /// `Bits`, LSB first, regardless of whether it's a compile-time constant
    /// (`StackPtr::Const`, via `stack_addr_bits`) or already
    /// runtime-computed (`StackPtr::Symbolic`, returned as-is).
    fn stack_ptr_addr_bits(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        block: BlockId,
        ptr: &StackPtr,
    ) -> Bits {
        match ptr {
            StackPtr::Const(sp) => self.stack_addr_bits(fctx, block, sp.addr),
            StackPtr::Symbolic { addr_bits, .. } => addr_bits.clone(),
        }
    }

    /// Bit-decompose a `GlobalPtr`'s current byte offset into a
    /// `PTR_BITS`-wide `Bits`, LSB first — the `GlobalPtr` counterpart of
    /// `stack_ptr_addr_bits`.
    fn global_ptr_offset_bits(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        block: BlockId,
        ptr: &GlobalPtr,
    ) -> Bits {
        match ptr {
            GlobalPtr::Const(g) => self.stack_addr_bits(fctx, block, g.byte_offset),
            GlobalPtr::Symbolic { offset_bits, .. } => offset_bits.clone(),
        }
    }

    /// Compute a uniform, `PTR_BITS`-wide tagged bit pattern for any LLVM
    /// pointer this importer can resolve to a known provenance at import
    /// time (a tracked stack pointer, or a global -- directly, via a
    /// constant-index GEP constant expression, or via a previously-tracked
    /// `GlobalPtr`, constant or already-dynamic offset alike).
    ///
    /// Bit 31 (MSB) is the provenance tag: `0` = stack (bits `[30:0]` are
    /// the ALLOCA-local address, matching `stack_ptr_addr_bits` exactly --
    /// `Alloca`'s own bump allocator refuses to ever set this bit, see its
    /// `next_stack_slot` check); `1` = global (bits `[GLOBAL_ADDR_BITS+
    /// GLOBAL_ID_BITS-1 : GLOBAL_ADDR_BITS]` are this global's own
    /// `StorageId` value, bits `[GLOBAL_ADDR_BITS-1:0]` are the byte offset
    /// within it).
    ///
    /// This is purely a *value* representation: it doesn't change how
    /// `Load`/`Store` resolve a pointer (still `stack_slot_of`/
    /// `global_ptr_of`-tracked, unchanged) -- only how a pointer *value*
    /// used generically (a `phi`/`select` operand, a function argument,
    /// anything not immediately dereferenced) is represented, instead of
    /// hard-erroring. `Select`'s existing generic `bc_select_vec` handling
    /// and `phi`'s existing generic block-param mechanism both already
    /// compose correctly with same-width `Bits` from either provenance, so
    /// wiring this into `value_bits`'s fallback is the only change needed
    /// to let a `phi`/`select` merging two differently-provenanced (or
    /// distinct same-provenance) pointers import successfully.
    ///
    /// A dynamic (`GlobalPtr::Symbolic`) byte offset that doesn't fit
    /// `GLOBAL_ADDR_BITS` truncates rather than erroring -- silent
    /// wraparound on overflow, matching this codebase's existing convention
    /// for `StorageId::STACK` addresses (`lower_to_ir.rs`'s own doc:
    /// "addresses wrap modulo 2^SP_BITS and alias unrelated storage"), not
    /// a new deviation from the fail-closed norm; a *constant* offset that
    /// doesn't fit is checked and errors, since that case is always
    /// statically decidable.
    fn ptr_value_bits(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        block: BlockId,
        ptr: PointerValue<'ctx>,
    ) -> IResult<Bits> {
        if ptr.is_null() {
            return Ok(self.null_ptr_bits(fctx, block));
        }
        if let Some(sp) = fctx.stack_slot_of.get(&ptr.as_any_value_enum()).cloned() {
            return Ok(self.stack_ptr_addr_bits(fctx, block, &sp));
        }
        let gp = self.resolve_global_ptr(fctx, ptr)?;
        let storage = match &gp {
            GlobalPtr::Const(g) => g.storage,
            GlobalPtr::Symbolic { storage, .. } => *storage,
        };
        if storage.0 >= (1u32 << GLOBAL_ID_BITS) {
            return Err(ImportError::Unsupported(
                "too many distinct globals for the pointer-value encoding".into(),
            ));
        }
        if let GlobalPtr::Const(g) = &gp {
            if g.byte_offset >= (1u64 << self.global_addr_bits()) {
                return Err(ImportError::Unsupported(
                    "global byte offset too large for the pointer-value encoding".into(),
                ));
            }
        }
        let offset_bits = self.global_ptr_offset_bits(fctx, block, &gp);
        let global_addr_bits = self.global_addr_bits();
        let mut bits = Vec::with_capacity(self.pointer_bits);
        bits.extend_from_slice(&offset_bits[..global_addr_bits]);
        for b in 0..GLOBAL_ID_BITS {
            bits.push(self.bc_const_at(fctx, block, (storage.0 >> b) & 1 != 0));
        }
        bits.push(self.bc_const_at(fctx, block, true)); // tag = 1 (global)
        Ok(bits)
    }

    /// LLVM `null` uses the otherwise-unassigned tagged-global pattern:
    /// tag = 1, global ID = 0, address = 0. Importer-created globals never
    /// receive id 0 — they begin at [`GLOBAL_STORAGE_BASE`] in legacy mode,
    /// and registry mode reserves `StorageId(0)` up front (see
    /// [`Importer::with_storage_registry`]) — so dispatch never matches
    /// this identity.
    /// `dispatch_read` maps such an unmatched tagged pointer to zeros and
    /// `dispatch_write` leaves every candidate untouched; it can therefore
    /// never alias stack slot zero.
    fn null_ptr_bits(&mut self, fctx: &mut FuncCtx<'ctx>, block: BlockId) -> Bits {
        let mut bits: Bits = (0..self.pointer_bits - 1)
            .map(|_| self.bc_const_at(fctx, block, false))
            .collect();
        bits.push(self.bc_const_at(fctx, block, true));
        bits
    }

    /// Compute `base_addr_bits + i` as a single `addr_tid`-typed value, via
    /// real bit-circuit addition, for use as a `StorageRead`/`StorageWrite`
    /// `addr` operand. `i` is small (bounded by the load/store's own bit
    /// width) and always fits well within `PTR_BITS`.
    fn dynamic_addr(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        block: BlockId,
        base_addr_bits: &Bits,
        i: u64,
    ) -> ValueId {
        let sum = {
            let mut c = Ctx {
                fctx,
                bit_tid: self.bit_tid,
                block,
            };
            let i_bits: Vec<ValueId> = (0..base_addr_bits.len())
                .map(|b| c.bc_const((i >> b) & 1 != 0))
                .collect();
            circuits::bc_add(&mut c, base_addr_bits, &i_bits, false)
        };
        fctx.emit(
            block,
            Value::Op(Stmt::Merge {
                parts: sum,
                ty: self.addr_tid,
            }),
        )
    }

    /// Like `stack_load`, but the base address is a runtime-computed `Bits`
    /// (`StackPtr::Symbolic`) rather than a compile-time-constant slot: each
    /// of the `n_bits` individual bit reads needs its own `addr = base + i`,
    /// via [`Self::dynamic_addr`].
    fn stack_load_dynamic(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        ptr_bits0: ValueId,
        base_addr_bits: &Bits,
        pointee_ty: TypeId,
        n_bits: usize,
    ) -> Bits {
        let cur = fctx.current;
        let mut bits = Vec::with_capacity(n_bits);
        for i in 0..n_bits as u64 {
            let addr = self.dynamic_addr(fctx, cur, base_addr_bits, i);
            let bit = fctx.emit(
                cur,
                Value::Op(Stmt::StorageRead {
                    storage: self.convention.alloca_marker,
                    ty: self.bit_tid,
                    addr,
                }),
            );
            bits.push(bit);
        }
        fctx.emit(
            cur,
            Value::PtrLoad {
                ptr: ptr_bits0,
                pointee_ty,
            },
        );
        bits
    }

    /// Write `val` to `StorageId::ALLOCA` at a runtime-computed base
    /// address. See [`Self::stack_load_dynamic`].
    fn stack_store_dynamic(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        ptr_bits0: ValueId,
        base_addr_bits: &Bits,
        val: &Bits,
    ) {
        let cur = fctx.current;
        for (i, &bit) in val.iter().enumerate() {
            let addr = self.dynamic_addr(fctx, cur, base_addr_bits, i as u64);
            fctx.emit(
                cur,
                Value::Op(Stmt::StorageWrite {
                    storage: self.convention.alloca_marker,
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

    /// Read `n_bytes` bytes from `storage` starting at `base_offset`
    /// (compile-time-constant byte offset; `storage_for_with_offset`/
    /// `FuncCtx::global_ptr_of` already folded in any GEP offset). The
    /// address `Const` is stamped `addr_tid` (32-bit), not `byte_tid` (8-bit)
    /// -- a too-narrow address type silently truncates the address instead of
    /// erroring (see `addr_tid`'s own doc comment for the `spill(5)`
    /// regression this exact mistake caused for the ALLOCA path).
    fn mem_load(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        storage: StorageId,
        base_offset: u64,
        n_bytes: usize,
    ) -> Bits {
        let cur = fctx.current;
        let mut all_bits = Vec::with_capacity(n_bytes * 8);
        for byte_i in 0..n_bytes as u64 {
            let addr = fctx.emit(
                cur,
                Value::Op(Stmt::Const(
                    Constant {
                        hi: 0,
                        lo: (base_offset + byte_i) as u128,
                    },
                    self.addr_tid,
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
        all_bits
    }

    /// Write `n_bytes` bytes of `val` to `storage` starting at
    /// `base_offset`. See `mem_load`'s doc comment for why the address
    /// `Const` is `addr_tid`, not `byte_tid`.
    fn mem_store(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        storage: StorageId,
        base_offset: u64,
        val: &Bits,
        n_bytes: usize,
    ) {
        let cur = fctx.current;
        for byte_i in 0..n_bytes as u64 {
            let addr = fctx.emit(
                cur,
                Value::Op(Stmt::Const(
                    Constant {
                        hi: 0,
                        lo: (base_offset + byte_i) as u128,
                    },
                    self.addr_tid,
                )),
            );
            let base = (byte_i * 8) as usize;
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
    }

    /// Like `mem_load`, but the base byte offset is a runtime-computed
    /// `Bits` (`GlobalPtr::Symbolic`) rather than a compile-time constant:
    /// each byte needs its own `addr = base_offset_bits + byte_i`, via
    /// [`Self::dynamic_addr`] (the same helper `stack_load_dynamic` uses).
    fn mem_load_dynamic(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        storage: StorageId,
        base_offset_bits: &Bits,
        n_bytes: usize,
    ) -> Bits {
        let cur = fctx.current;
        let mut all_bits = Vec::with_capacity(n_bytes * 8);
        for byte_i in 0..n_bytes as u64 {
            let addr = self.dynamic_addr(fctx, cur, base_offset_bits, byte_i);
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
        all_bits
    }

    /// Write `n_bytes` bytes of `val` to `storage` at a runtime-computed
    /// base byte offset. See [`Self::mem_load_dynamic`].
    fn mem_store_dynamic(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        storage: StorageId,
        base_offset_bits: &Bits,
        val: &Bits,
        n_bytes: usize,
    ) {
        let cur = fctx.current;
        for byte_i in 0..n_bytes as u64 {
            let addr = self.dynamic_addr(fctx, cur, base_offset_bits, byte_i);
            let base = (byte_i * 8) as usize;
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
    }

    /// The closed candidate set every runtime pointer dispatch considers:
    /// every `StorageId` `register_all_globals` (or any later lazy
    /// `storage_for` call) has handed out to a global so far. Always
    /// complete by the time any function body is walked, since
    /// `import_module` registers every module global up front.
    fn dispatch_candidates(&self) -> IResult<Vec<StorageId>> {
        // The assigned set itself, sorted for deterministic dispatch order
        // -- NOT a contiguity assumption over the allocator's range, which
        // registry mode (dense, skipping claimed ids) deliberately breaks.
        let candidates: Vec<StorageId> = self
            .storage_for_global
            .values()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        if candidates.len() > MAX_DISPATCH_CANDIDATES {
            return Err(ImportError::Unsupported(format!(
                "{} candidate globals exceeds the {MAX_DISPATCH_CANDIDATES}-candidate limit for \
                 runtime pointer dispatch",
                candidates.len()
            )));
        }
        Ok(candidates)
    }

    /// Per-candidate `matched` flags for `ptr_bits` against `candidates`:
    /// `tag_bit AND (id_bits == candidate's StorageId)` — `false` whenever
    /// `tag_bit` is 0 (a stack pointer), which is exactly what lets
    /// `dispatch_read` use the stack read as a bare default with no
    /// separate `NOT tag_bit` case of its own.
    fn dispatch_matches(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        block: BlockId,
        ptr_bits: &Bits,
        candidates: &[StorageId],
    ) -> Vec<ValueId> {
        let global_addr_bits = self.global_addr_bits();
        let tag_bit = ptr_bits[self.pointer_bits - 1];
        let id_bits = &ptr_bits[global_addr_bits..global_addr_bits + GLOBAL_ID_BITS];
        let mut c = Ctx {
            fctx,
            bit_tid: self.bit_tid,
            block,
        };
        candidates
            .iter()
            .map(|sid| {
                let id_const: Vec<ValueId> = (0..GLOBAL_ID_BITS)
                    .map(|b| c.bc_const((sid.0 >> b) & 1 != 0))
                    .collect();
                let id_eq = circuits::bc_eq(&mut c, id_bits, &id_const);
                c.bc_and(tag_bit, id_eq)
            })
            .collect()
    }

    /// Zero-extend `ptr_bits`'s low `GLOBAL_ADDR_BITS` (the ADDR sub-field
    /// of `ptr_value_bits`'s encoding) to a full `PTR_BITS`-wide `Bits`, for
    /// use as `mem_load_dynamic`/`mem_store_dynamic`'s base-offset operand.
    fn dispatch_global_addr_bits(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        block: BlockId,
        ptr_bits: &Bits,
    ) -> Bits {
        let zero = self.bc_const_at(fctx, block, false);
        let mut bits: Bits = ptr_bits[..self.global_addr_bits()].to_vec();
        bits.resize(self.pointer_bits, zero);
        bits
    }

    /// Runtime storage-identity dispatch for a `Load` through a pointer
    /// whose provenance isn't statically resolvable (neither
    /// `stack_slot_of` nor `global_ptr_of`/`storage_for_with_offset` could
    /// pin it down -- e.g. a pointer function parameter, or a
    /// `phi`/`select`-merged value whose tag isn't a compile-time
    /// constant). Reads *every* candidate in the closed set (the stack,
    /// `StorageId::ALLOCA`, plus every module global) and muxes the one
    /// `ptr_bits` actually names, decoding `ptr_bits` per
    /// `ptr_value_bits`'s own tag+ID+ADDR encoding -- the uniform encoding
    /// every pointer *value* this importer produces already uses (see
    /// `docs/llvm-ptr-value-bits.md`), which is what makes decode-by-bits
    /// sufficient here without any provenance analysis. Mirrors
    /// `translate_switch`'s `bc_eq`/`bc_select_vec` cascade, applied to
    /// data bits instead of a jump-table index.
    fn dispatch_read(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        ptr_bits: &Bits,
        pointee_ty: TypeId,
        n_bytes: usize,
    ) -> IResult<Bits> {
        let n_bits = n_bytes
            .checked_mul(8)
            .ok_or_else(|| ImportError::Unsupported("dispatch read length too large".into()))?;
        let candidates = self.dispatch_candidates()?;
        let cur = fctx.current;
        let matches = self.dispatch_matches(fctx, cur, ptr_bits, &candidates);
        let global_addr_bits = self.dispatch_global_addr_bits(fctx, cur, ptr_bits);

        // The stack candidate is selected only for tag = 0. An unmatched
        // tagged-global pattern (LLVM `null` uses ID 0, while real globals
        // begin at `GLOBAL_STORAGE_BASE`) must read as zero rather than
        // accidentally falling through to stack address zero.
        let stack = self.stack_load_dynamic(fctx, ptr_bits[0], ptr_bits, pointee_ty, n_bits);
        let tag_bit = ptr_bits[self.pointer_bits - 1];
        let not_tag = {
            let mut c = Ctx {
                fctx,
                bit_tid: self.bit_tid,
                block: cur,
            };
            c.bc_not(tag_bit)
        };
        let zero = self.bc_const_at(fctx, cur, false);
        let mut result: Bits = {
            let mut c = Ctx {
                fctx,
                bit_tid: self.bit_tid,
                block: cur,
            };
            stack
                .iter()
                .map(|bit| c.bc_select(not_tag, *bit, zero))
                .collect()
        };

        for (candidate_idx, &sid) in candidates.iter().enumerate() {
            let global_bits = self.mem_load_dynamic(fctx, sid, &global_addr_bits, n_bytes);
            let matched = matches[candidate_idx];
            let mut new_result = Vec::with_capacity(n_bits);
            {
                let mut c = Ctx {
                    fctx,
                    bit_tid: self.bit_tid,
                    block: cur,
                };
                for i in 0..n_bits {
                    new_result.push(c.bc_select(matched, global_bits[i], result[i]));
                }
            }
            result = new_result;
        }
        Ok(result)
    }

    /// Runtime storage-identity dispatch for a `Store` through a pointer
    /// whose provenance isn't statically resolvable. Since
    /// `Stmt::StorageWrite`'s `storage` field isn't itself
    /// runtime-selectable, every candidate is written unconditionally on
    /// every call -- read-modify-write, muxing each candidate's *old* value
    /// against `val` by its own `matched` flag, so an unmatched candidate's
    /// write is a semantic no-op. Direct generalization of
    /// `storage_to_mux_ir::mux_write`'s existing "N addresses in one
    /// storage" technique to "N storages, one address."
    fn dispatch_write(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        ptr_bits: &Bits,
        val: &Bits,
    ) -> IResult<()> {
        let n_bits = val.len();
        let n_bytes = n_bits.div_ceil(8);
        let candidates = self.dispatch_candidates()?;
        let cur = fctx.current;
        let matches = self.dispatch_matches(fctx, cur, ptr_bits, &candidates);
        let global_addr_bits = self.dispatch_global_addr_bits(fctx, cur, ptr_bits);
        let tag_bit = ptr_bits[self.pointer_bits - 1];
        let not_tag = {
            let mut c = Ctx {
                fctx,
                bit_tid: self.bit_tid,
                block: cur,
            };
            c.bc_not(tag_bit)
        };

        // Stack candidate.
        let bit_tid = self.bit_tid;
        let stack_old = self.stack_load_dynamic(fctx, ptr_bits[0], ptr_bits, bit_tid, n_bits);
        let mut stack_new = Vec::with_capacity(n_bits);
        {
            let mut c = Ctx {
                fctx,
                bit_tid: self.bit_tid,
                block: cur,
            };
            for i in 0..n_bits {
                stack_new.push(c.bc_select(not_tag, val[i], stack_old[i]));
            }
        }
        self.stack_store_dynamic(fctx, ptr_bits[0], ptr_bits, &stack_new);

        // Every global candidate.
        for (candidate_idx, &sid) in candidates.iter().enumerate() {
            let old = self.mem_load_dynamic(fctx, sid, &global_addr_bits, n_bytes);
            let matched = matches[candidate_idx];
            let mut new_val = Vec::with_capacity(n_bits);
            {
                let mut c = Ctx {
                    fctx,
                    bit_tid: self.bit_tid,
                    block: cur,
                };
                for i in 0..n_bits {
                    new_val.push(c.bc_select(matched, val[i], old[i]));
                }
            }
            self.mem_store_dynamic(fctx, sid, &global_addr_bits, &new_val, n_bytes);
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
        // A preceding direct call may already have consumed this LLVM
        // terminator as a VAFFLE `ReturnCall`. This is how both ordinary
        // `call; ret` TCO and noreturn-style `call; unreachable` avoid
        // introducing an IR-wide Unreachable variant.
        if fctx.terminators[cur.0].is_some() {
            return Ok(());
        }
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
            InstructionOpcode::Unreachable => {
                return Err(ImportError::Unsupported("reachable unreachable".into()));
            }
            other => {
                return Err(ImportError::Unsupported(format!(
                    "terminator {other:?} is not supported"
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
                .ok_or_else(|| {
                    ImportError::Unsupported("switch case dest must be a block".into())
                })?;
            let case_value =
                unsafe { IntValue::new(LLVMGetSwitchCaseValue(instr.as_value_ref(), i - 1)) };
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

/// Recursively flatten a (possibly nested-array) alloca'd type down to its
/// innermost scalar integer element type and total element count —
/// `[16 x i8]` -> `(i8, 16)`, `[4 x [4 x i8]]` -> `(i8, 16)`, a bare scalar
/// -> `(ty, 1)`. `None` for anything else (struct, float, vector, pointer)
/// — callers turn that into a named "not supported" error, never a panic
/// (see docs/llvm-array-alloca.md item 3: a struct alloca either flattens
/// or names a clear error, both are an acceptable outcome).
/// Truncate or sign-extend `bits` (LSB first) to exactly `width` bits,
/// matching the `SExt` opcode's own idiom elsewhere in this file (repeat the
/// MSB when widening). Used to normalize a GEP index of arbitrary LLVM width
/// to `PTR_BITS` before feeding it into `bc_mul`/`bc_add`, which require
/// equal-width operands.
fn resize_bits_signed(bits: &[ValueId], width: usize) -> Bits {
    let mut out: Bits = bits.to_vec();
    if out.len() > width {
        out.truncate(width);
    } else if let Some(&sign) = out.last() {
        while out.len() < width {
            out.push(sign);
        }
    }
    out
}

fn flatten_alloca_type(
    ty: inkwell::types::BasicTypeEnum<'_>,
) -> Option<(inkwell::types::BasicTypeEnum<'_>, u64)> {
    match ty {
        ty @ (inkwell::types::BasicTypeEnum::IntType(_)
        | inkwell::types::BasicTypeEnum::PointerType(_)) => Some((ty, 1)),
        inkwell::types::BasicTypeEnum::ArrayType(a) => {
            let (inner_ty, inner_count) = flatten_alloca_type(a.get_element_type())?;
            inner_count
                .checked_mul(a.len() as u64)
                .map(|c| (inner_ty, c))
        }
        _ => None,
    }
}

/// Find the blocks whose instructions the structural importer may translate.
/// Only `br` and `switch` are traversed because they are the only LLVM
/// terminators this frontend supports; any other reachable terminator remains
/// visible to Pass B and therefore fails closed through `translate_terminator`.
fn reachable_blocks<'ctx>(f: FunctionValue<'ctx>) -> IResult<HashSet<LlvmBlock<'ctx>>> {
    let entry = f
        .get_first_basic_block()
        .ok_or_else(|| ImportError::Unsupported("function has no entry block".into()))?;
    let mut reachable = HashSet::new();
    let mut pending = vec![entry];

    while let Some(bb) = pending.pop() {
        if !reachable.insert(bb) {
            continue;
        }
        let terminator = bb
            .get_terminator()
            .ok_or_else(|| ImportError::Unsupported("reachable block has no terminator".into()))?;
        if !matches!(
            terminator.get_opcode(),
            InstructionOpcode::Br | InstructionOpcode::Switch
        ) {
            continue;
        }
        let n_successors = unsafe { LLVMGetNumSuccessors(terminator.as_value_ref()) };
        for i in 0..n_successors {
            let successor =
                unsafe { LlvmBlock::new(LLVMGetSuccessor(terminator.as_value_ref(), i)) }
                    .ok_or_else(|| {
                        ImportError::Unsupported("terminator successor is not a basic block".into())
                    })?;
            pending.push(successor);
        }
    }

    Ok(reachable)
}

fn call_value_operand<'ctx>(
    instr: InstructionValue<'ctx>,
    index: u32,
    description: &str,
) -> IResult<BasicValueEnum<'ctx>> {
    // `InstructionValue::get_operand` eagerly constructs a
    // `BasicValueEnum`. That is an inkwell panic for LLVM metadata values,
    // so inspect the raw operand type first. This guard deliberately sits
    // below the ignored-intrinsic check: those metadata-only hints should be
    // skipped, while a metadata operand on any ordinary call must fail closed
    // as an ImportError instead of crashing the importer.
    let raw = unsafe { LLVMGetOperand(instr.as_value_ref(), index) };
    if raw.is_null() {
        return Err(ImportError::Unsupported(format!(
            "{description} is missing"
        )));
    }
    if unsafe { LLVMGetTypeKind(LLVMTypeOf(raw)) } == LLVMTypeKind::LLVMMetadataTypeKind {
        return Err(ImportError::Unsupported(format!(
            "{description} must not be metadata"
        )));
    }
    instr
        .get_operand(index)
        .and_then(|operand| operand.value())
        .ok_or_else(|| ImportError::Unsupported(format!("{description} must be a value")))
}

/// LLVM intrinsics that are semantic no-ops for this structural circuit
/// importer. These must be recognized by callee name before generic call
/// handling because several take metadata operands, which inkwell cannot
/// convert to `BasicValueEnum`.
fn is_ignored_llvm_intrinsic(name: &str) -> bool {
    name == "llvm.experimental.noalias.scope.decl"
        || name.starts_with("llvm.lifetime.start.")
        || name.starts_with("llvm.lifetime.end.")
        || name.starts_with("llvm.dbg.")
        || matches!(name, "llvm.assume" | "llvm.donothing" | "llvm.sideeffect")
}

/// Detect the exact LLVM shapes that can use VAFFLE's `ReturnCall` without
/// materializing a call result or creating a continuation. The caller has
/// already established that this is a direct call.
fn tail_call_end(instr: InstructionValue<'_>) -> Option<TailCallEnd> {
    let terminator = instr.get_next_instruction()?;
    match terminator.get_opcode() {
        InstructionOpcode::Unreachable => Some(TailCallEnd::Unreachable),
        InstructionOpcode::Return => match terminator.get_operand(0).and_then(|op| op.value()) {
            Some(value) if value.as_any_value_enum() == instr.as_any_value_enum() => {
                Some(TailCallEnd::Return)
            }
            None if instr.get_type().is_void_type() => Some(TailCallEnd::Return),
            _ => None,
        },
        _ => None,
    }
}

fn memory_intrinsic_nonvolatile<'ctx>(
    instr: InstructionValue<'ctx>,
    index: u32,
    intrinsic: MemoryIntrinsic,
) -> IResult<()> {
    let BasicValueEnum::IntValue(volatile) =
        call_value_operand(instr, index, "memory intrinsic volatile flag")?
    else {
        return Err(ImportError::Unsupported(format!(
            "llvm.{} volatile flag must be constant false",
            intrinsic.name()
        )));
    };
    if volatile.get_zero_extended_constant() != Some(0) {
        return Err(ImportError::Unsupported(format!(
            "llvm.{} volatile flag must be constant false",
            intrinsic.name()
        )));
    }
    Ok(())
}

fn overflow_integer_operand<'ctx>(
    instr: InstructionValue<'ctx>,
    index: u32,
    intrinsic: OverflowIntrinsic,
) -> IResult<IntValue<'ctx>> {
    match call_value_operand(instr, index, "overflow intrinsic operand")? {
        BasicValueEnum::IntValue(value) => Ok(value),
        _ => Err(ImportError::Unsupported(format!(
            "{} operands must be integers",
            intrinsic.name()
        ))),
    }
}

fn intrinsic_ranges_overlap(
    dest: &IntrinsicPointer,
    src: &IntrinsicPointer,
    n_bytes: usize,
) -> IResult<bool> {
    match (dest, src) {
        (
            IntrinsicPointer::Stack {
                ptr: StackPtr::Const(dest),
                ..
            },
            IntrinsicPointer::Stack {
                ptr: StackPtr::Const(src),
                ..
            },
        ) if dest.allocation_base == src.allocation_base
            && dest.allocation_bits == src.allocation_bits =>
        {
            let (dest_start, dest_end) = (*dest).intrinsic_range(n_bytes)?;
            let (src_start, src_end) = (*src).intrinsic_range(n_bytes)?;
            Ok(dest_start < src_end && src_start < dest_end)
        }
        (
            IntrinsicPointer::Global {
                storage: dest_storage,
                byte_offset: dest_offset,
            },
            IntrinsicPointer::Global {
                storage: src_storage,
                byte_offset: src_offset,
            },
        ) if dest_storage == src_storage => {
            let n_bytes = u64::try_from(n_bytes).map_err(|_| {
                ImportError::Unsupported("memory intrinsic length is too large".into())
            })?;
            let dest_end = dest_offset.checked_add(n_bytes).ok_or_else(|| {
                ImportError::Unsupported("memory intrinsic range overflow".into())
            })?;
            let src_end = src_offset.checked_add(n_bytes).ok_or_else(|| {
                ImportError::Unsupported("memory intrinsic range overflow".into())
            })?;
            Ok(*dest_offset < src_end && *src_offset < dest_end)
        }
        _ => Ok(false),
    }
}

fn int_result_width(instr: InstructionValue<'_>) -> IResult<usize> {
    match instr.get_type().try_into() {
        Ok(inkwell::types::BasicTypeEnum::IntType(t)) => Ok(t.get_bit_width() as usize),
        _ => Err(ImportError::Unsupported(
            "expected an integer-typed instruction result".into(),
        )),
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
    /// Fields of the fixed `{ integer, i1 }` values returned by LLVM's
    /// arithmetic-overflow intrinsics. LLVM only lets the importer observe
    /// these through `extractvalue`, so they stay out of the general value
    /// cache and arbitrary aggregate handling remains unsupported.
    aggregate_fields: HashMap<AnyValueEnum<'ctx>, Vec<Bits>>,
    current: BlockId,
    /// Per-function bump allocator for `StorageId::ALLOCA`, in *bits*, zero-
    /// based (matches `VaffleTarget`'s own `next_stack_slot`/`PTR_BITS`
    /// convention) — not bytes like the global `storage_for`/`mem_load`/
    /// `mem_store` path, which is a distinct storage identity and
    /// addressing convention. These are *local* offsets within this
    /// function's own alloca region: `volar-vaffle-target/src/
    /// lower_to_ir.rs` rebases each one onto the real runtime frame
    /// (`sp_bits + local_offset`) at lowering time, so this bump allocator
    /// never needs to know — or reserve headroom against — the calling
    /// convention's own frame layout.
    next_stack_slot: u64,
    /// Pointer-typed LLVM values (alloca results, or a constant-index GEP
    /// off one) that are tracked as `StorageId::ALLOCA` addresses. This is
    /// the sole source of truth for "is this a stack pointer" — `cache`
    /// alone is not enough, since pointer-typed function *parameters* are
    /// also cached there as plain (meaningless-as-an-address) bits.
    stack_slot_of: HashMap<AnyValueEnum<'ctx>, StackPtr>,
    /// Pointer-typed LLVM values that are the result of a constant-index
    /// `getelementptr` *instruction* off a global (directly, or chained off
    /// another tracked entry here). See `GlobalPointer`.
    global_ptr_of: HashMap<AnyValueEnum<'ctx>, GlobalPtr>,
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
            aggregate_fields: HashMap::new(),
            current: BlockId(0),
            next_stack_slot: 0,
            stack_slot_of: HashMap::new(),
            global_ptr_of: HashMap::new(),
        }
    }

    /// Append an importer-synthesized control-flow block. These blocks do not
    /// correspond to LLVM basic blocks, so they have no phi metadata or
    /// `block_of` entry; normal source-block translation can still resume in
    /// one after a lowered intrinsic loop.
    fn append_block(&mut self) -> BlockId {
        let block = BlockId(self.stmts.len());
        self.phi_order.push(Vec::new());
        self.params.push(Vec::new());
        self.stmts.push(Vec::new());
        self.terminators.push(None);
        block
    }

    fn add_param(&mut self, block: BlockId, ty: TypeId, idx: usize) -> ValueId {
        let value = self.emit(block, Value::Param { block, ty, idx });
        self.params[block.0].push((value, ty));
        value
    }

    fn emit(&mut self, block: BlockId, v: Value) -> ValueId {
        let id = ValueId(self.values.len());
        self.values.push(Node::new(v, (), None));
        self.stmts[block.0].push(id);
        id
    }

    fn finish_blocks(self, fallback_return_values: Vec<Vec<ValueId>>) -> Vec<Block> {
        self.stmts
            .into_iter()
            .zip(self.params)
            .zip(self.terminators)
            .zip(fallback_return_values)
            .map(|(((stmts, params), term), fallback_return_values)| Block {
                params,
                stmts,
                terminator: term.unwrap_or_else(|| Terminator::Return {
                    values: fallback_return_values.clone(),
                }),
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
        coeffs: PolyCoeffs<ValueId>,
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
