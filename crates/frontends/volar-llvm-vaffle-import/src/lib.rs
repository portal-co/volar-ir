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
//! - a constant-size `alloca` (integer, pointer, or array of those) — or a
//!   `getelementptr` off one — tracked as its own bit-addressed data
//!   `StorageId` plus a pointer-width stack-pointer global. `Value::StackAlloc`
//!   yields the pointer (the stack pointer before the bump); loads and stores
//!   are ordinary `StorageRead`/`StorageWrite` of that storage at those bits.
//!   Function exit writes the entry stack-pointer value back. See
//!   `docs/llvm-alloca.md`.
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

use std::collections::{BTreeMap, HashMap, HashSet};

use inkwell::AddressSpace;
use inkwell::IntPredicate;
use inkwell::basic_block::BasicBlock as LlvmBlock;
use inkwell::llvm_sys::core::{
    LLVMGetConstOpcode, LLVMGetFirstUse, LLVMGetGEPSourceElementType, LLVMGetNextUse,
    LLVMGetNumOperands, LLVMGetNumSuccessors, LLVMGetOperand, LLVMGetSuccessor,
    LLVMGetSwitchCaseValue, LLVMGetTypeKind, LLVMGetUser, LLVMIsAConstantExpr,
    LLVMIsAGlobalVariable, LLVMIsAInstruction, LLVMTypeOf,
};
use inkwell::llvm_sys::{LLVMOpcode, LLVMTypeKind};
use inkwell::module::Module as LlvmModule;
use inkwell::types::{BasicMetadataTypeEnum, BasicTypeEnum};
use inkwell::values::{
    AnyValue, AnyValueEnum, AsValueRef, BasicValueEnum, CallSiteValue, FunctionValue,
    InstructionOpcode, InstructionValue, IntValue, PhiValue, PointerValue,
};

use vaffle::{
    Block, BlockId, FuncBody, FuncDecl, FuncId, Module, PointerWidth, SigDecl, SigId, Target,
    Terminator, Value, ValueId,
};
use volar_ir_common::{
    ActionDecl, ActionExecutionPolicy, Constant, IrType, Node, OracleDecl, OracleExecutionPolicy,
    PolyCoeffs, PreInitSegment, Stmt, StorageAllocator, StorageId, TypeId, TypeTable, aes_extern,
};
use volar_lir::circuits::{self, BitCircuitBuilder};
use volar_llvm_constchain::{ConstChainError, global_from_pointer, strip_pointer};
use volar_vaffle_target::circuit_helpers::{
    HelperKey, HelperOp, divrem_split, helper_result_width, intern_circuit_helper, width_type,
};

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

impl ImportError {
    /// The word-cell freeze fixpoint's retry marker: the attempt found an
    /// unaligned access on a word-frozen storage and tainted it.
    pub fn is_word_violation(&self) -> bool {
        matches!(self, ImportError::Unsupported(m)
            if m.starts_with("unaligned access through a word-cell storage"))
    }
}

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

/// First `StorageId` this importer's allocator hands out. Globals, per-alloca
/// data stacks, and per-alloca stack-pointer cells all come from this
/// allocator, above `StorageId::STACK`.
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
/// Explicit LLVM declaration mapping for a direct external call symbol.
///
/// These mappings are opt-in: an unmapped LLVM declaration retains ordinary
/// call semantics and is never silently treated as an MPC external.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LlvmExternalImportKind {
    Oracle {
        execution: OracleExecutionPolicy,
    },
    /// LLVM action ABI: `[guard, args (n_args), fallbacks (one per result)]`.
    Action {
        n_args: usize,
        execution: ActionExecutionPolicy,
    },
}

/// Optional ABI and explicit external-declaration constraints for structural
/// LLVM import.
pub use volar_vaffle_target::circuit_helpers::CircuitHelperMode;

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LlvmImportConfig {
    pub pointer_width: Option<PointerWidth>,
    pub externals: BTreeMap<String, LlvmExternalImportKind>,
    /// Inline `bc_*` into the parent block, or intern one typed function per
    /// `(op, operand widths)` and call it. Address math and stack-pointer
    /// bumps stay inline in both modes.
    pub circuit_helpers: CircuitHelperMode,
    /// Per-site and module-wide cap on runtime pointer-dispatch candidate
    /// globals. `None` uses [`MAX_DISPATCH_CANDIDATES`] (the fail-closed
    /// default). Whole real-world modules (e.g. unmodified revm at -O0,
    /// S6c) carry hundreds of legitimate globals, so callers importing such
    /// modules raise this deliberately and accept the wider dispatch
    /// cascades at whichever sites actually need runtime dispatch.
    pub max_dispatch_candidates: Option<usize>,
    /// Lower reachable `unreachable` instructions to a zero return instead
    /// of failing the import. Compiler-proven-dead arms in real-world
    /// modules (e.g. `core::ptr::swap_nonoverlapping`'s len-0 paths in
    /// revm's dependency closure at -O0) are runtime-unreachable by
    /// construction; the knob is opt-in and the default stays fail-closed.
    pub lower_reachable_unreachable_to_zeros: bool,
}

impl LlvmImportConfig {
    pub fn with_oracle_execution(
        mut self,
        llvm_symbol: impl Into<String>,
        execution: OracleExecutionPolicy,
    ) -> Self {
        self.externals.insert(
            llvm_symbol.into(),
            LlvmExternalImportKind::Oracle { execution },
        );
        self
    }

    pub fn with_action_execution(
        mut self,
        llvm_symbol: impl Into<String>,
        n_args: usize,
        execution: ActionExecutionPolicy,
    ) -> Self {
        self.externals.insert(
            llvm_symbol.into(),
            LlvmExternalImportKind::Action { n_args, execution },
        );
        self
    }

    /// Reject contradictory duplicate registrations at the public import
    /// boundary. The builder methods retain replacement semantics for
    /// ergonomic construction, while this validator makes import admission
    /// explicit and fail-closed.
    fn validate(&self) -> IResult<()> {
        for (symbol, external) in &self.externals {
            if symbol.is_empty() {
                return Err(ImportError::Unsupported(
                    "configured external symbol must not be empty".into(),
                ));
            }
            if let LlvmExternalImportKind::Action { n_args, .. } = external
                && n_args.checked_add(2).is_none()
            {
                return Err(ImportError::Unsupported(format!(
                    "configured action `{symbol}` argument count overflows its ABI"
                )));
            }
        }
        if self.max_dispatch_candidates.is_some_and(|c| c == 0) {
            return Err(ImportError::Unsupported(
                "configured max_dispatch_candidates must be nonzero".into(),
            ));
        }
        Ok(())
    }

    pub fn with_circuit_helpers(mut self, mode: CircuitHelperMode) -> Self {
        self.circuit_helpers = mode;
        self
    }

    /// Raise (or lower) the runtime pointer-dispatch candidate cap; see the
    /// field docs.
    pub fn with_max_dispatch_candidates(mut self, cap: usize) -> Self {
        self.max_dispatch_candidates = Some(cap);
        self
    }

    /// Opt in to zero-return lowering of reachable `unreachable`
    /// instructions; see the field docs.
    pub fn with_reachable_unreachable_zeros(mut self) -> Self {
        self.lower_reachable_unreachable_to_zeros = true;
        self
    }
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
    if let Some(explicit) = config.pointer_width
        && explicit != width
    {
        return Err(ImportError::Unsupported(format!(
            "configured pointer width {} does not match LLVM data layout width {}",
            explicit.bits(),
            width.bits()
        )));
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
    config.validate()?;
    // Word-cell freeze fixpoint: a storage is word-celled only if EVERY
    // access to it (any function, tracked or dispatch) is word-aligned.
    // The alloca's own function is proven by the scan; specialized callees
    // are checked at their access arms, which taints the storage and fails
    // the attempt. The whole import reruns with the grown taint set (fresh
    // Importer, deterministic); each pass freezes a monotone-smaller set,
    // so the loop terminates. Attempts that make no progress return the
    // original error.
    let mut tainted: std::collections::HashSet<StorageId> = Default::default();
    const MAX_WORD_FREEZE_PASSES: usize = 4;
    for pass in 0..MAX_WORD_FREEZE_PASSES {
        match import_module_with_config_attempt(llvm_module, entries, config.clone(), &tainted) {
            Ok((module, _)) => return Ok(module),
            Err((e, new_taints)) => {
                let grew = new_taints.len() > tainted.len();
                tainted = new_taints;
                if !grew || !e.is_word_violation() {
                    return Err(e);
                }
                if pass + 1 == MAX_WORD_FREEZE_PASSES {
                    return Err(ImportError::Unsupported(format!(
                        "word-cell freeze did not converge in {MAX_WORD_FREEZE_PASSES} passes"
                    )));
                }
            }
        }
    }
    unreachable!("fixpoint loop always returns")
}

fn import_module_with_config_attempt<'ctx>(
    llvm_module: &LlvmModule<'ctx>,
    entries: &[&str],
    config: LlvmImportConfig,
    tainted: &std::collections::HashSet<StorageId>,
) -> Result<
    (Module, std::collections::HashSet<StorageId>),
    (ImportError, std::collections::HashSet<StorageId>),
> {
    let pointer_width = match pointer_width_from_layout(llvm_module, config.clone()) {
        Ok(w) => w,
        Err(e) => return Err((e, Default::default())),
    };
    let mut importer = Importer::new(
        pointer_width,
        config.externals.clone(),
        config.circuit_helpers,
        config.max_dispatch_candidates,
        config.lower_reachable_unreachable_to_zeros,
    );
    importer.set_data_layout(inkwell::targets::TargetData::create(
        &llvm_module.get_data_layout().as_str().to_string_lossy(),
    ));
    importer.tainted_word = std::rc::Rc::new(std::cell::RefCell::new(tainted.clone()));
    let tainted_out = importer.tainted_word.clone();
    let result: IResult<Module> = (|| {
        importer.validate_configured_external_symbols(llvm_module)?;
    // Eagerly assign every module global its `StorageId` before walking any
    // function body, so `dispatch_read`/`dispatch_write` (runtime
    // storage-identity dispatch for a pointer whose provenance isn't
    // statically resolvable) always sees the complete, stable candidate set
    // -- never just whichever globals happened to be referenced by earlier
    // functions' own direct loads/stores/GEPs.
    importer.register_all_globals(llvm_module)?;
    importer.collect_fnptr_candidates(llvm_module);
    let mut worklist: Vec<(FunctionValue<'ctx>, Vec<Prov>)> = Vec::new();
    for &name in entries {
        let f = llvm_module.get_function(name).ok_or_else(|| {
            ImportError::Unsupported(format!("entry function `{name}` does not exist"))
        })?;
        // Entry functions have no caller: their pointer parameters carry no
        // static provenance (empty vector, never seeded).
        importer.func_id(f, &[])?;
        worklist.push((f, Vec::new()));
    }
    let mut done: std::collections::HashSet<(PointerValue<'ctx>, Vec<Prov>)> = Default::default();
    let mut imported_count = 0usize;
    while let Some((f, prov)) = worklist.pop() {
        let key = (f.as_global_value().as_pointer_value(), prov.clone());
        if done.contains(&key) {
            continue;
        }
        done.insert(key);
        if std::env::var("VLVI_IMPORT_NEXT").is_ok() {
            eprintln!("next: {}", f.get_name().to_string_lossy());
        }
        let before = importer.total_values();
        let called = importer.import_function(f, &prov)?;
        worklist.extend(called);
        imported_count += 1;
        if std::env::var("VLVI_IMPORT_PROGRESS").is_ok() {
            eprintln!(
                "import: #{imported_count} {} (worklist {}, values {})",
                f.get_name().to_string_lossy(),
                worklist.len(),
                importer.total_values() - before
            );
        }
        let _ = before;
    }
    Ok(importer.finish())
    })();
    match result {
        Ok(module) => Ok((module, tainted_out.borrow().clone())),
        Err(e) => Err((e, tainted_out.borrow().clone())),
    }
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

/// One alloca site's data stack. The address is the `Value::StackAlloc`
/// result (the site's stack pointer before the bump), plus any later GEP.
/// `const_offset` is the bit offset from that allocation's base when it is
/// still a constant; a symbolic GEP clears it. Range checks use the offset,
/// and the address bits stay the full pointer width so a known-provenance
/// load does not go through the tagged encoding.
#[derive(Clone, Debug)]
struct StackPtr {
    storage: StorageId,
    allocation_bits: u64,
    const_offset: Option<u64>,
    addr_bits: Bits,
}

impl StackPtr {
    fn intrinsic_range(&self, n_bytes: usize) -> IResult<(u64, u64)> {
        let start = self.const_offset.ok_or_else(|| {
            ImportError::Unsupported(
                "memory intrinsic range is not a constant offset of its alloca".into(),
            )
        })?;
        let n_bits = u64::try_from(n_bytes)
            .ok()
            .and_then(|n| n.checked_mul(8))
            .ok_or_else(|| {
                ImportError::Unsupported("memory intrinsic length is too large".into())
            })?;
        let end = start
            .checked_add(n_bits)
            .ok_or_else(|| ImportError::Unsupported("memory intrinsic range overflow".into()))?;
        if start > self.allocation_bits || end > self.allocation_bits {
            return Err(ImportError::Unsupported(
                "memory intrinsic range escapes its alloca provenance".into(),
            ));
        }
        Ok((start, end))
    }
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

impl GlobalPtr {
    fn storage(&self) -> StorageId {
        match self {
            GlobalPtr::Const(g) => g.storage,
            GlobalPtr::Symbolic { storage, .. } => *storage,
        }
    }
}

/// Static provenance class of a pointer-typed call argument, driving
/// call-site specialization (issue: `&mut`-parameter state access otherwise
/// imports as per-bit runtime dispatch). A callee is re-imported per
/// distinct provenance vector of its pointer arguments; a copy seeded with
/// `Stack`/`Global` provenance resolves every access through those
/// parameters statically against the named storage, while `Any` keeps the
/// generic runtime-dispatch body.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
enum Prov {
    /// The argument names a specific alloca data stack (a tracked
    /// `StackPtr`, possibly at a runtime offset within it).
    Stack(StorageId),
    /// The argument names a specific global-class storage (a tracked or
    /// constant global; with pool packing that is the single pool storage,
    /// at a possibly-runtime byte offset).
    Global(StorageId),
    /// No static provenance: the generic dispatching body.
    Any,
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

/// The once-per-access byte base of a dynamic byte-cell access: either a
/// constant byte offset (pool-backed, all-const bit address) or the base
/// bits shifted right 3, computed once and shared across every byte.
enum ByteBase {
    Const(u64),
    Runtime(Bits),
}

struct Importer<'ctx> {
    pointer_width: PointerWidth,
    pointer_bits: usize,
    types: TypeTable,
    funcs: Vec<FuncDecl>,
    sigs: Vec<SigDecl>,
    exports: std::collections::BTreeMap<String, FuncId>,
    /// Per (LLVM function, pointer-argument provenance) — the
    /// specialization key. Two call sites whose pointer arguments name the
    /// same storages share one imported copy; distinct storages produce
    /// distinct copies in which parameter-provenance state access resolves
    /// statically (see `Prov`).
    func_ids: HashMap<(PointerValue<'ctx>, Vec<Prov>), FuncId>,
    storage_for_global: HashMap<PointerValue<'ctx>, StorageId>,
    /// All module globals pack into ONE pool storage: a global has exactly
    /// one possible location, so its identity is a constant byte offset
    /// within the pool (`global_pool_offset`) rather than a distinct
    /// `StorageId`. Non-escaping allocas carve from the same pool tail
    /// (`pool_next_byte`); a runtime-dispatched pointer can then only name
    /// this one global-class storage plus escaping per-alloca stacks.
    pool_storage: Option<StorageId>,
    global_pool_offset: HashMap<PointerValue<'ctx>, u64>,
    pool_next_byte: u64,
    /// The one SigId per LLVM function. Signature shape is
    /// provenance-independent (param/result types don't change across
    /// specialized copies); only the FuncId specializes.
    sig_of: HashMap<PointerValue<'ctx>, SigId>,
    storage_alloc: StorageAllocator,
    /// Data stacks created for `alloca`, in allocation order. Dispatch
    /// muxes these separately from byte-addressed globals.
    alloca_data: Vec<StorageId>,
    /// Each data stack's allocation span in bits, parallel to
    /// `alloca_data`. An in-bounds access within one alloca never
    /// addresses at or beyond this bound, so its address adder only needs
    /// the bound's bit length (the trimmed-address optimization).
    alloca_sizes: Vec<u64>,
    /// Stack-pointer globals paired with `alloca_data`. Excluded from
    /// global dispatch; each holds one pointer-width cell at address 0.
    sp_globals: Vec<StorageId>,
    pre_init: Vec<PreInitSegment>,
    bit_tid: TypeId,
    byte_tid: TypeId,
    /// Target word size in bytes (pointer width / 8): wasm32 -> 4,
    /// 64-bit targets -> 8. Drives word-cell addressing.
    word_bytes: u64,
    /// Cell type for one target word.
    word_tid: TypeId,
    /// Per-storage word-cell flag (frozen at the alloca): true iff every
    /// LLVM access to the storage carries `align >= word_bytes`, so no
    /// access ever straddles a word boundary and every access sits at
    /// lane 0. Storages not in the map are byte-celled.
    storage_word: HashMap<StorageId, bool>,
    /// Tainted (byte-frozen by interprocedural evidence) storages for the
    /// current fixpoint attempt; consulted with `storage_word`. Shared
    /// behind Rc<RefCell> so the taint set survives `finish(self)`'s
    /// consumption of the Importer on the success path.
    tainted_word: std::rc::Rc<std::cell::RefCell<HashSet<StorageId>>>,
    /// Function-pointer constants STORED somewhere in the module (the
    /// members of fn-pointer tables). The indirect-call dispatch compares
    /// the loaded callee bits against each candidate's encoding and routes
    /// through a dense positional `Terminator::Table`.
    fnptr_candidates: Vec<PointerValue<'ctx>>,
    /// Stable per-function identity indexes for function-pointer constant
    /// encodings (ADDR field), allocated on first sight of each function.
    fn_symbols: HashMap<usize, u64>,
    /// Oracle declarations registered by circuit-extern lowering (today:
    /// `aes128_encrypt_block`, see [`translate_aes_extern`]), carried into
    /// the finished module so the vaffle→IR and IR→boolar lowerings can
    /// validate the emitted `Stmt::OracleCall`s against them.
    oracles: Vec<OracleDecl>,
    /// Action declarations registered by configured LLVM extern lowering.
    actions: Vec<ActionDecl>,
    /// Explicit direct-call mappings supplied by [`LlvmImportConfig`].
    externals: BTreeMap<String, LlvmExternalImportKind>,
    /// Pointer-width type for stack-pointer cells and merged addresses.
    /// A 1-bit address constant would collapse every address to its low bit.
    addr_tid: TypeId,
    helper_mode: CircuitHelperMode,
    helper_cache: BTreeMap<HelperKey, FuncId>,
    /// Effective per-site/module-wide dispatch candidate cap (resolved from
    /// the config; see `LlvmImportConfig::max_dispatch_candidates`).
    max_dispatch_candidates: usize,
    /// The module's LLVM data layout (for struct field offsets and aggregate
    /// sizes in GEP lowering). Set once at import entry, before any body is
    /// walked.
    data_layout: Option<inkwell::targets::TargetData>,
    /// Zero-return lowering of reachable `unreachable` instructions.
    lower_reachable_unreachable_to_zeros: bool,
}

impl<'ctx> Importer<'ctx> {
    fn new(
        pointer_width: PointerWidth,
        externals: BTreeMap<String, LlvmExternalImportKind>,
        helper_mode: CircuitHelperMode,
        max_dispatch_candidates: Option<usize>,
        lower_reachable_unreachable_to_zeros: bool,
    ) -> Self {
        let pointer_bits = pointer_width.bits();
        let mut types = TypeTable::new();
        let bit_tid = types.bit();
        // The `Type::_n` cutover: an n-bit value is `Vec(n, Bit)`.
        let byte_tid = types.intern(IrType::Vec(8, bit_tid));
        let addr_tid = types.intern(IrType::Vec(pointer_width.bits(), bit_tid));
        // The target word size for word-cell addressing: the pointer
        // width's byte size — 4 for wasm32, 8 for 64-bit targets (a
        // memory64 layout parses as a 64-bit pointer width here, since
        // `pointer_width` comes from the module's target info).
        let word_bytes = ((pointer_bits / 8).max(1)) as u64;
        let word_tid = types.intern(IrType::Vec((word_bytes * 8) as usize, bit_tid));
        Importer {
            pointer_width,
            pointer_bits,
            types,
            funcs: Vec::new(),
            sigs: Vec::new(),
            exports: Default::default(),
            func_ids: HashMap::new(),
            storage_for_global: HashMap::new(),
            pool_storage: None,
            global_pool_offset: HashMap::new(),
            pool_next_byte: 0,
            sig_of: HashMap::new(),
            storage_alloc: StorageAllocator::new(GLOBAL_STORAGE_BASE),
            alloca_data: Vec::new(),
            alloca_sizes: Vec::new(),
            sp_globals: Vec::new(),
            pre_init: Vec::new(),
            bit_tid,
            byte_tid,
            word_bytes,
            word_tid,
            storage_word: HashMap::new(),
            tainted_word: std::rc::Rc::new(std::cell::RefCell::new(HashSet::new())),
            fnptr_candidates: Vec::new(),
            fn_symbols: HashMap::new(),
            oracles: Vec::new(),
            actions: Vec::new(),
            externals,
            addr_tid,
            helper_mode,
            helper_cache: BTreeMap::new(),
            max_dispatch_candidates: max_dispatch_candidates.unwrap_or(MAX_DISPATCH_CANDIDATES),
            data_layout: None,
            lower_reachable_unreachable_to_zeros,
        }
    }

    /// Install the module's data layout before walking any function body
    /// (struct GEP lowering needs field offsets).
    fn set_data_layout(&mut self, layout: inkwell::targets::TargetData) {
        self.data_layout = Some(layout);
    }

    /// Total VAFFLE values emitted so far (diagnostics).
    fn total_values(&self) -> usize {
        self.funcs
            .iter()
            .map(|f| match f {
                FuncDecl::Body(b) => b.values.len(),
                _ => 0,
            })
            .sum()
    }

    /// Validate every configured mapping against the module before walking
    /// reachable bodies. A misspelled mapping must not silently succeed just
    /// because no imported call happened to reference it.
    fn validate_configured_external_symbols(&self, llvm_module: &LlvmModule<'ctx>) -> IResult<()> {
        for (symbol, external) in &self.externals {
            let callee = llvm_module.get_function(symbol).ok_or_else(|| {
                ImportError::Unsupported(format!(
                    "configured external `{symbol}` is not declared by the LLVM module"
                ))
            })?;
            self.validate_configured_external_declaration(callee, symbol, external)?;
        }
        Ok(())
    }

    fn finish(self) -> Module {
        Module {
            pointer_width: self.pointer_width,
            types: self.types,
            oracles: self.oracles,
            actions: self.actions,
            funcs: self.funcs,
            sigs: self.sigs,
            exports: self.exports,
            pre_init: self.pre_init,
        }
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
            BasicTypeEnum::StructType(struct_ty) => {
                // Structs: ABI size from the module data layout (includes
                // padding).
                let layout = self.data_layout.as_ref().ok_or_else(|| {
                    ImportError::Unsupported(
                        "struct size query before the data layout was installed (internal)".into(),
                    )
                })?;
                Ok(layout.get_abi_size(&struct_ty))
            }
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
            match cur_ty {
                BasicTypeEnum::ArrayType(array) => {
                    let elem_ty = array.get_element_type();
                    let step = idx
                        .checked_mul(self.memory_byte_size(elem_ty)? as i64)
                        .ok_or_else(|| ImportError::Unsupported("gep offset overflow".into()))?;
                    offset = offset
                        .checked_add(step)
                        .ok_or_else(|| ImportError::Unsupported("gep offset overflow".into()))?;
                    cur_ty = elem_ty;
                }
                BasicTypeEnum::StructType(struct_ty) => {
                    // Struct field descent: ABI field offset from the module
                    // data layout (padding included).
                    let layout = self.data_layout.as_ref().ok_or_else(|| {
                        ImportError::Unsupported(
                            "struct gep before the data layout was installed (internal)".into(),
                        )
                    })?;
                    let field = u32::try_from(idx).map_err(|_| {
                        ImportError::Unsupported("gep struct field index out of range".into())
                    })?;
                    let field_offset =
                        layout.offset_of_element(&struct_ty, field).ok_or_else(|| {
                            ImportError::Unsupported(format!(
                                "gep struct field index {idx} out of range"
                            ))
                        })?;
                    offset = offset
                        .checked_add(field_offset as i64)
                        .ok_or_else(|| ImportError::Unsupported("gep offset overflow".into()))?;
                    cur_ty = struct_ty.get_field_type_at_index(field).ok_or_else(|| {
                        ImportError::Unsupported(format!(
                            "gep struct field index {idx} out of range"
                        ))
                    })?;
                }
                _ => {
                    return Err(ImportError::Unsupported(
                        "gep of non-array/struct aggregate type not supported".into(),
                    ));
                }
            }
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

    /// Flattened leaf-field widths of a first-class type, in declaration
    /// order, recursing into nested structs/arrays; scalars (integers and
    /// default-address-space pointers) are single-field. This is the
    /// importer's own SSA-level value layout: supported code never loads or
    /// stores aggregates as whole-memory values (struct field access goes
    /// through tracked allocas via GEPs; struct *values* exist only as call
    /// results and `insertvalue` chains), so only `extractvalue`'s and
    /// `insertvalue`'s index decomposition has to agree with this layout.
    fn aggregate_field_layout(
        &self,
        ty: inkwell::types::BasicTypeEnum<'ctx>,
    ) -> IResult<Vec<usize>> {
        Ok(match ty {
            inkwell::types::BasicTypeEnum::ArrayType(a) => {
                let mut out = Vec::new();
                for _ in 0..a.len() {
                    out.extend(self.aggregate_field_layout(a.get_element_type())?);
                }
                out
            }
            inkwell::types::BasicTypeEnum::StructType(s) => {
                let mut out = Vec::new();
                for field in s.get_field_types() {
                    out.extend(self.aggregate_field_layout(field)?);
                }
                out
            }
            scalar => vec![self.llvm_bit_width(scalar).map_err(|_| {
                ImportError::Unsupported(format!(
                    "aggregate field type {scalar:?} is not an integer/pointer scalar"
                ))
            })?],
        })
    }

    /// Total flattened bit width of a first-class type.
    fn llvm_flatten_width(&self, ty: inkwell::types::BasicTypeEnum<'ctx>) -> IResult<usize> {
        let width: usize = self.aggregate_field_layout(ty)?.iter().sum();
        // Profile alarm (VLVI_IMPORT_STATS=1): a monstrous flattened width
        // means a call site emits one per-bit `Output` per result bit — the
        // fn#64-shaped 17M-Output hotspots. Logged at this single choke
        // point so cached (already-interned) signatures are covered too.
        if std::env::var("VLVI_IMPORT_STATS").is_ok() && width > 65536 {
            eprintln!(
                "stats: flatten width {width} for {}",
                ty.print_to_string().to_string_lossy()
            );
        }
        Ok(width)
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

    fn func_id(&mut self, f: FunctionValue<'ctx>, prov: &[Prov]) -> IResult<FuncId> {
        let key = f.as_global_value().as_pointer_value();
        // Intern the signature once per LLVM function: the param/result
        // shape is provenance-independent, so every specialized copy of the
        // function shares it.
        let sig = match self.sig_of.get(&key) {
            Some(&sig) => sig,
            None => {
                let params: Vec<TypeId> = f
                    .get_params()
                    .iter()
                    .map(|p| self.llvm_type_id(p.get_type()))
                    .collect::<IResult<_>>()?;
                let results: Vec<TypeId> = match f.get_type().get_return_type() {
                    Some(t) => match t {
                        inkwell::types::BasicTypeEnum::StructType(_)
                        | inkwell::types::BasicTypeEnum::ArrayType(_) => {
                            // Aggregate returns flatten to ONE wide Bit-vector
                            // result whose fields occupy declaration-order bit
                            // ranges — matching the per-bit `Value::Output`s the
                            // call site emits and the concatenated bits a `ret`
                            // produces.
                            let width = self.llvm_flatten_width(t)?;
                            vec![self.types.intern(IrType::Vec(width, self.bit_tid))]
                        }
                        _ => vec![self.llvm_type_id(t)?],
                    },
                    None => vec![],
                };
                let sig = SigId(self.sigs.len());
                self.sigs.push(SigDecl { params, results });
                self.sig_of.insert(key, sig);
                sig
            }
        };
        // Reserve a placeholder per (function, provenance); `import_function`
        // overwrites it once the body (or import declaration) is known.
        // Reserving up front lets mutually/self-recursive call sites resolve
        // a stable FuncId before the callee itself has been walked.
        let fkey = (key, prov.to_vec());
        if let Some(&id) = self.func_ids.get(&fkey) {
            return Ok(id);
        }
        let id = FuncId(self.funcs.len());
        let name = f.get_name().to_string_lossy().into_owned();
        self.funcs.push(FuncDecl::Import {
            module: "llvm".into(),
            name: name.clone(),
            sig,
        });
        self.func_ids.insert(fkey, id);
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
                let w = i.get_bit_width() as usize;
                if w == 1 {
                    self.types.bit()
                } else {
                    // The `Type::_n` cutover: an n-bit value is `Vec(n, Bit)`.
                    let bit = self.types.bit();
                    self.types.intern(IrType::Vec(w, bit))
                }
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
            // The `Type::_n` cutover: an n-bit value is `Vec(n, Bit)`.
            _ => {
                let bit = self.types.bit();
                self.types.intern(IrType::Vec(32, bit))
            }
        })
    }

    /// Assign (allocating on first reference) the `StorageId` for a global
    /// variable, requiring the pointer operand to resolve to a literal
    /// global at import time — the generic "entire operation must be a
    /// constant load" rule, applied to every memory reference this importer
    /// emits.
    fn storage_for(&mut self, ptr: PointerValue<'ctx>) -> IResult<StorageId> {
        self.ensure_default_address_space(ptr)?;
        use inkwell::values::AnyValue;
        let raw = strip_pointer(ptr.as_value_ref(), "memory reference base").map_err(|e| {
            ImportError::Unsupported(format!(
                "{} (pointer: {})",
                e,
                ptr.print_to_string().to_string_lossy()
            ))
        })?;
        let global = global_from_pointer(raw, "memory reference base")?;
        let key = global.as_pointer_value();
        if let Some(&id) = self.storage_for_global.get(&key) {
            return Ok(id);
        }
        let id = self.storage_alloc.alloc();
        self.storage_for_global.insert(key, id);
        Ok(id)
    }

    /// Assign every global *variable* its identity up front, packed into
    /// ONE pool storage. Unlike general allocas (any of whose per-site data
    /// stacks a runtime-dispatched pointer may name, so dispatch must read
    /// them all), a global has exactly one possible location: its identity
    /// is the constant byte offset within the pool. `storage_for_global`
    /// maps every global to the same pool `StorageId` and
    /// `global_pool_offset` records its aligned byte offset, so the runtime
    /// dispatch candidate set collapses to the single pool storage instead
    /// of one candidate per global.
    ///
    /// The whole module's globals register before any body is walked (not
    /// just ones reachable from the entry points being imported): the pool's
    /// total extent must be fixed before any function body carves
    /// non-escaping allocas from its tail, and a runtime-dispatched pointer's
    /// caller may pass any global's address regardless of which function
    /// directly references it syntactically.
    fn register_all_globals(&mut self, llvm_module: &LlvmModule<'ctx>) -> IResult<()> {
        let mut count = 0usize;
        for global in llvm_module.get_globals() {
            let key = global.as_pointer_value();
            if self.storage_for_global.contains_key(&key) {
                continue;
            }
            let pool = self.pool_ensure();
            let ty: BasicTypeEnum = global
                .get_value_type()
                .try_into()
                .map_err(|_| ImportError::Unsupported("global of non-first-class type".into()))?;
            let size = self.memory_byte_size(ty)?;
            let base = self.pool_carve(self.pool_next_byte, size)?;
            self.storage_for_global.insert(key, pool);
            self.global_pool_offset.insert(key, base);
            count += 1;
        }
        if count > self.max_dispatch_candidates {
            return Err(ImportError::Unsupported(format!(
                "module has {count} globals, exceeding the {}-candidate \
                 limit for runtime pointer dispatch",
                self.max_dispatch_candidates
            )));
        }
        Ok(())
    }

    /// The static pool storage, allocating it on first use. Globals register
    /// from it eagerly in `register_all_globals`; a module with no globals
    /// allocates it lazily when its first non-escaping alloca carves from
    /// the tail (pool storages are neither per-alloca data stacks nor SP
    /// globals, so the dispatch candidate enumeration picks them up
    /// whenever they appear).
    fn pool_ensure(&mut self) -> StorageId {
        match self.pool_storage {
            Some(p) => p,
            None => {
                let p = self.storage_alloc.alloc();
                self.pool_storage = Some(p);
                p
            }
        }
    }

    /// Carve `n_bytes` from the static pool's tail at 16-byte alignment,
    /// returning the byte offset. Globals register from the pool head;
    /// non-escaping allocas carve from the tail as function bodies are
    /// walked. Fails closed when the pool outgrows the pointer-value
    /// encoding's address field.
    fn pool_carve(&mut self, base: u64, n_bytes: u64) -> IResult<u64> {
        let base = base.next_multiple_of(16);
        let end = base
            .checked_add(n_bytes)
            .ok_or_else(|| ImportError::Unsupported("static pool size overflow".into()))?;
        if end >= (1u64 << self.global_addr_bits()) {
            return Err(ImportError::Unsupported(
                "static pool exhausted: globals plus non-escaping allocas \
                 exceed the pointer-value address field"
                    .into(),
            ));
        }
        self.pool_next_byte = end;
        Ok(base)
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
            let storage = self.storage_for(ptr)?;
            // A bare global sits at its packed offset within the pool.
            let off = self.global_pool_offset.get(&ptr).copied().unwrap_or(0);
            return Ok((storage, off));
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

    fn import_function(
        &mut self,
        f: FunctionValue<'ctx>,
        prov: &[Prov],
    ) -> IResult<Vec<(FunctionValue<'ctx>, Vec<Prov>)>> {
        let id = self.func_id(f, prov)?;
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

        // Call-site specialization: seed pointer parameters with the
        // caller-observed provenance so their accesses resolve statically
        // instead of dispatching over every storage. The tagged pointer
        // encoding puts the offset in the low `global_addr_bits` of the
        // value bits, so the seeded tracking reuses the caller's runtime
        // offset while fixing the storage identity. Parameters of the
        // generic (`Any`) body keep the dispatching behavior; entry
        // functions receive an empty provenance vector and are never
        // seeded.
        if prov.iter().any(|p| !matches!(p, Prov::Any)) {
            let global_addr_bits = self.global_addr_bits();
            let zero_bit = self.bc_const_at(&mut fctx, entry_block, false);
            for (i, param) in f.get_params().iter().enumerate() {
                let BasicValueEnum::PointerValue(pv) = param else {
                    continue;
                };
                let bits = match fctx.cache.get(&param.as_any_value_enum()) {
                    Some(b) if b.len() >= global_addr_bits => b.clone(),
                    _ => continue,
                };
                match prov.get(i) {
                    Some(Prov::Stack(storage)) => {
                        let mut addr_bits = bits[..global_addr_bits].to_vec();
                        addr_bits.resize(self.pointer_bits, zero_bit);
                        fctx.stack_slot_of.insert(
                            pv.as_any_value_enum(),
                            StackPtr {
                                storage: *storage,
                                // The offset is runtime, so range-checking
                                // against a compile-time extent is
                                // meaningless: `const_offset: None` fails
                                // memory intrinsics closed while ordinary
                                // loads/stores/GEPs proceed on the fixed
                                // storage.
                                allocation_bits: u64::MAX,
                                const_offset: None,
                                addr_bits,
                            },
                        );
                    }
                    Some(Prov::Global(storage)) => {
                        fctx.global_ptr_of.insert(
                            pv.as_any_value_enum(),
                            GlobalPtr::Symbolic {
                                storage: *storage,
                                offset_bits: bits[..global_addr_bits].to_vec(),
                            },
                        );
                    }
                    _ => {}
                }
            }
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

        // The enclosing function's flattened return width (the same width as
        // the single wide result `func_id` interns). Computed before Pass B
        // so reachable-`unreachable` zero-return lowering can size its
        // terminator.
        let return_bits = match f.get_type().get_return_type() {
            Some(ty) => match ty {
                inkwell::types::BasicTypeEnum::StructType(_)
                | inkwell::types::BasicTypeEnum::ArrayType(_) => self.llvm_flatten_width(ty)?,
                _ => self.llvm_bit_width(ty)?,
            },
            None => 0,
        };
        fctx.return_bits = return_bits;

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
            if std::env::var("VLVI_INSTR_TRACE").is_ok() {
                eprintln!("[trace] bb{i} start values={}", fctx.values.len());
            }
            let mut inst = bb.get_first_instruction();
            let mut trace_i = 0u32;
            while let Some(instr) = inst {
                let next = instr.get_next_instruction();
                if std::env::var("VLVI_INSTR_TRACE").is_ok() {
                    trace_i += 1;
                    eprintln!(
                        "[trace] bb{i} instr{trace_i} opcode={:?} values={}",
                        instr.get_opcode(),
                        fctx.values.len()
                    );
                    // Explosion probe: die EARLY with a backtrace instead of
                    // RSS-ing to 14GB — names the exact emitting path. The
                    // zero-return memo makes ~160k a legitimate one-time
                    // cost for wide-return functions; explosions are
                    // multi-million.
                    if fctx.values.len() > 1_000_000 {
                        eprintln!(
                            "exploding instruction: {}",
                            instr
                                .print_to_string()
                                .to_string()
                                .chars()
                                .take(600)
                                .collect::<String>()
                        );
                        panic!(
                            "value explosion: bb{i} instr{trace_i} opcode={:?} values={}",
                            instr.get_opcode(),
                            fctx.values.len()
                        );
                    }
                }
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

        let mut fallback_return_values: Vec<Vec<ValueId>> = blocks
            .iter()
            .enumerate()
            .map(|(i, _)| {
                if !reachable.contains(&blocks[i]) {
                    {
                        (0..return_bits)
                            .map(|_| self.bc_const_at(&mut fctx, BlockId(i), false))
                            .collect()
                    }
                } else {
                    Default::default()
                }
            })
            .collect();
        // Symbolic memcpy loops append their own fully-terminated VAFFLE
        // blocks. Keep the fallback vector aligned with every block so
        // `finish_blocks` does not truncate them when zipping its vectors.
        fallback_return_values.resize_with(fctx.stmts.len(), Vec::new);
        self.install_sp_reverts(&mut fctx);
        // Per-function statement histogram (VLVI_IMPORT_STATS=1): what the
        // emitted values are made of — dispatch/stack traffic vs arithmetic
        // vs call plumbing — for profiling aggregate-traffic hotspots.
        if std::env::var("VLVI_IMPORT_STATS").is_ok() {
            let mut hist: BTreeMap<&'static str, usize> = Default::default();
            for v in fctx.values.iter() {
                let k = match &v.kind {
                    Value::Op(s) => match s {
                        Stmt::StorageRead { .. } => "op:StorageRead",
                        Stmt::StorageWrite { .. } => "op:StorageWrite",
                        Stmt::Poly { coeffs, .. } => {
                            let max_vars =
                                coeffs.iter().map(|(vars, _)| vars.len()).max().unwrap_or(0);
                            match (coeffs.len(), max_vars) {
                                (1, 1) => "poly:xor1var",
                                (1, 2) => "poly:and2var",
                                _ => "poly:other",
                            }
                        }
                        Stmt::Const(..) => "op:Const",
                        Stmt::Merge { .. } => "op:Merge",
                        Stmt::Shuffle { .. } => "op:Shuffle",
                        Stmt::Splat { .. } => "op:Splat",
                        Stmt::Transmute { .. } => "op:Transmute",
                        _ => "op:other",
                    },
                    Value::Param { .. } => "param",
                    Value::Call { .. } => "call",
                    Value::Output { .. } => "output",
                    Value::StackAlloc { .. } => "stack_alloc",
                    Value::PtrLoad { .. } => "ptr_load",
                    Value::PtrStore { .. } => "ptr_store",
                    Value::PtrOffset { .. } => "ptr_offset",
                    _ => "other",
                };
                *hist.entry(k).or_insert(0) += 1;
            }
            eprintln!(
                "stats: hist {}",
                hist.iter()
                    .map(|(k, n)| format!("{k}={n}"))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        }
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
        if let BasicValueEnum::IntValue(i) = v
            && i.is_const()
        {
            return self.int_const_bits(fctx, i);
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

    fn bc_const_at(&mut self, fctx: &mut FuncCtx<'ctx>, _block: BlockId, val: bool) -> ValueId {
        // Hash-consed constants, hoisted to the entry block: constants are
        // pure and function-scoped, so ONE node per distinct value serves
        // every use — huge constant aggregates (drop-glue zero fills, wide
        // zero-return vectors, exploded aggregate stores) collapse from
        // per-bit nodes to two shared entries. Block order is irrelevant
        // for pure constants and the entry block is always executed, so
        // references from any block resolve.
        let key = val as u128;
        if let Some(&vid) = fctx.const_cache.get(&key) {
            return vid;
        }
        let vid = fctx.emit(
            BlockId(0),
            Value::Op(Stmt::Const(
                Constant {
                    hi: 0,
                    lo: key,
                },
                self.bit_tid,
            )),
        );
        fctx.const_cache.insert(key, vid);
        vid
    }

    fn translate_instruction(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        instr: InstructionValue<'ctx>,
    ) -> IResult<Vec<(FunctionValue<'ctx>, Vec<Prov>)>> {
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
                Some(self.alu(fctx, cur, HelperOp::Add, &[a, b]))
            }
            InstructionOpcode::Sub => {
                let (a, b) = (op!(0), op!(1));
                Some(self.alu(fctx, cur, HelperOp::Sub, &[a, b]))
            }
            InstructionOpcode::Mul => {
                let (a, b) = (op!(0), op!(1));
                Some(self.alu(fctx, cur, HelperOp::Mul, &[a, b]))
            }
            InstructionOpcode::UDiv => {
                let (a, b) = (op!(0), op!(1));
                Some(self.alu(fctx, cur, HelperOp::UDiv, &[a, b]))
            }
            InstructionOpcode::URem => {
                let (a, b) = (op!(0), op!(1));
                Some(self.alu(fctx, cur, HelperOp::URem, &[a, b]))
            }
            InstructionOpcode::SRem => {
                let (a, b) = (op!(0), op!(1));
                Some(self.alu(fctx, cur, HelperOp::SRem, &[a, b]))
            }
            InstructionOpcode::SDiv => {
                let (a, b) = (op!(0), op!(1));
                Some(self.alu(fctx, cur, HelperOp::SDiv, &[a, b]))
            }
            InstructionOpcode::And => {
                let (a, b) = (op!(0), op!(1));
                Some(self.alu(fctx, cur, HelperOp::And, &[a, b]))
            }
            InstructionOpcode::Or => {
                let (a, b) = (op!(0), op!(1));
                Some(self.alu(fctx, cur, HelperOp::Or, &[a, b]))
            }
            InstructionOpcode::Xor => {
                let (a, b) = (op!(0), op!(1));
                Some(self.alu(fctx, cur, HelperOp::Xor, &[a, b]))
            }
            InstructionOpcode::Shl => {
                let (a, b) = (op!(0), op!(1));
                Some(self.alu(fctx, cur, HelperOp::Shl, &[a, b]))
            }
            InstructionOpcode::LShr => {
                let (a, b) = (op!(0), op!(1));
                Some(self.alu(fctx, cur, HelperOp::LShr, &[a, b]))
            }
            InstructionOpcode::AShr => {
                let (a, b) = (op!(0), op!(1));
                Some(self.alu(fctx, cur, HelperOp::AShr, &[a, b]))
            }
            InstructionOpcode::ICmp => {
                let pred = instr
                    .get_icmp_predicate()
                    .ok_or_else(|| ImportError::Unsupported("icmp without predicate".into()))?;
                let (a, b) = (op!(0), op!(1));
                let (op, swap) = match pred {
                    IntPredicate::EQ => (HelperOp::Eq, false),
                    IntPredicate::NE => (HelperOp::Ne, false),
                    IntPredicate::ULT => (HelperOp::Ult, false),
                    IntPredicate::ULE => (HelperOp::Ule, false),
                    IntPredicate::UGT => (HelperOp::Ult, true),
                    IntPredicate::UGE => (HelperOp::Ule, true),
                    IntPredicate::SLT => (HelperOp::Slt, false),
                    IntPredicate::SLE => (HelperOp::Sle, false),
                    IntPredicate::SGT => (HelperOp::Slt, true),
                    IntPredicate::SGE => (HelperOp::Sle, true),
                };
                let (x, y) = if swap { (b, a) } else { (a, b) };
                Some(self.alu(fctx, cur, op, &[x, y]))
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
            InstructionOpcode::InsertValue => {
                // Aggregate construction: fields = the base aggregate's
                // fields with the indexed component replaced. Poison/undef
                // bases start fully untracked; every leaf field must be
                // inserted before the aggregate itself is usable (fail
                // closed on partially-initialized reads via the missing
                // cache entry + untracked-field errors). Pure SSA
                // re-arrangement: the aggregate's bits are other values'
                // outputs; no new statements are emitted.
                let layout_ty: inkwell::types::BasicTypeEnum<'ctx> =
                    instr.get_type().try_into().map_err(|_| {
                        ImportError::Unsupported(
                            "insertvalue result must be a first-class type".into(),
                        )
                    })?;
                let layout = self.aggregate_field_layout(layout_ty)?;
                let inserted = op!(1);
                let indices = instr.get_indices();
                let [field] = indices.as_slice() else {
                    return Err(ImportError::Unsupported(
                        "insertvalue must target exactly one aggregate field".into(),
                    ));
                };
                let field = *field as usize;
                let width = *layout.get(field).ok_or_else(|| {
                    ImportError::Unsupported(format!(
                        "insertvalue field {field} is outside the aggregate layout"
                    ))
                })?;
                let base = instr
                    .get_operand(0)
                    .and_then(|o| o.value())
                    .ok_or_else(|| {
                        ImportError::Unsupported("insertvalue base must be a value".into())
                    })?;
                let mut fields: Vec<Option<Bits>> =
                    match fctx.aggregate_fields.get(&base.as_any_value_enum()) {
                        Some(base_fields) => {
                            if base_fields.len() != layout.len() {
                                return Err(ImportError::Unsupported(
                                    "insertvalue base aggregate layout mismatch".into(),
                                ));
                            }
                            base_fields.clone()
                        }
                        // Poison/undef base: an aggregate with every field still
                        // untracked.
                        None => vec![None; layout.len()],
                    };
                if inserted.len() != width {
                    return Err(ImportError::Unsupported(
                        "insertvalue operand width does not match the aggregate field".into(),
                    ));
                }
                fields[field] = Some(inserted);
                if fields.iter().all(|f| f.is_some()) {
                    // Record the (complete) field decomposition even for the
                    // completing insertvalue, so later extractvalues and
                    // further insertvalue chains can consume it.
                    fctx.aggregate_fields
                        .insert(instr.as_any_value_enum(), fields.clone());
                    let bits: Bits = fields
                        .iter()
                        .flatten()
                        .flat_map(|f| f.iter().copied())
                        .collect();
                    Some(bits)
                } else {
                    // Still partially uninitialized: no value is cached, so
                    // any read of the aggregate itself fails closed.
                    fctx.aggregate_fields
                        .insert(instr.as_any_value_enum(), fields);
                    None
                }
            }
            InstructionOpcode::PtrToInt => {
                // Pointers ARE integers in the tagged value encoding
                // (docs/llvm-ptr-value-bits.md): the cast is an identity on
                // the bit representation, width-adjusted for the result
                // type. Model note: the encoding's global provenance tag
                // (high bit) means a global-class pointer does not integer-
                // compare to zero — null checks via `ptrtoint` on
                // global-class pointers misreport; stack-class nulls
                // (all-zero address, tag 0) compare correctly.
                let mut src = op!(0);
                let dst_n = int_result_width(instr)?;
                while src.len() < dst_n {
                    src.push(self.bc_const_at(fctx, cur, false));
                }
                src.truncate(dst_n);
                Some(src)
            }
            InstructionOpcode::IntToPtr => {
                // Same identity, but the RESULT is pointer-typed: the
                // destination width is the tagged pointer encoding's width
                // (an IntToPtr instruction appeared with the fn-pointer
                // identity encodings; the constant form was already
                // handled in `ptr_value_bits`).
                let mut src = op!(0);
                let dst_n = self.pointer_bits;
                while src.len() < dst_n {
                    src.push(self.bc_const_at(fctx, cur, false));
                }
                src.truncate(dst_n);
                Some(src)
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
                self.ensure_default_address_space(ptr)?;
                let n_bits = self.llvm_bit_width(instr.get_type())?;
                let n_bytes = n_bits.div_ceil(8);
                if let Some(stack_ptr) = fctx.stack_slot_of.get(&ptr.as_any_value_enum()).cloned() {
                    // Word-frozen storage invariant: every access is
                    // word-aligned. The alloca's own function was scanned;
                    // this check closes the interprocedural hole (a
                    // specialized callee receiving the storage and doing
                    // an unaligned access) with a loud error instead of
                    // silent cell-key corruption.
                    if self.storage_is_word(stack_ptr.storage)
                        && (instr.get_alignment().unwrap_or(1) as u64) < self.word_bytes
                    {
                        self.tainted_word.borrow_mut().insert(stack_ptr.storage);
                        return Err(ImportError::Unsupported(
                            "unaligned access through a word-cell storage".into(),
                        ));
                    }
                    // Pointer-VALUE provenance through memory: a load of a
                    // pointer-typed value from a slot holding a tracked
                    // pointer returns that pointer's encoding directly
                    // (bit-identical to the stored bytes) and RE-REGISTERS
                    // the tracked identity, so -O0 spill round-trips keep
                    // downstream accesses specialized instead of
                    // dispatching over every candidate storage.
                    let load_ty_kind = unsafe {
                        inkwell::llvm_sys::core::LLVMGetTypeKind(
                            inkwell::llvm_sys::core::LLVMTypeOf(instr.as_value_ref()),
                        )
                    };
                    if load_ty_kind
                        == inkwell::llvm_sys::LLVMTypeKind::LLVMPointerTypeKind
                    {
                        if let Some(spilled) =
                            fctx.slot_ptr_of.get(&ptr.as_any_value_enum()).cloned()
                        {
                            fctx.stack_slot_of.insert(instr.as_any_value_enum(), spilled.clone());
                            let bits = self.encode_stack_ptr(
                                fctx,
                                cur,
                                spilled.storage,
                                &spilled.addr_bits,
                            )?;
                            fctx.cache.insert(instr.as_any_value_enum(), bits);
                            return Ok(called);
                        }
                    }
                    let ptr_bits0 = fctx
                        .cache
                        .get(&ptr.as_any_value_enum())
                        .and_then(|b| b.first().copied())
                        .ok_or_else(|| {
                            ImportError::Unsupported("stack pointer bits missing (internal)".into())
                        })?;
                    let pointee_tid = self.llvm_type_id(instr.get_type())?;
                    let addr_bound = self.stack_addr_bound(&stack_ptr);
                    Some(self.stack_load_dynamic(
                        fctx,
                        stack_ptr.storage,
                        ptr_bits0,
                        &stack_ptr.addr_bits,
                        pointee_tid,
                        n_bits,
                        addr_bound,
                    ))
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
                    // Mirrors the Load arm's word-frozen invariant check.
                    if self.storage_is_word(stack_ptr.storage)
                        && (instr.get_alignment().unwrap_or(1) as u64) < self.word_bytes
                    {
                        self.tainted_word.borrow_mut().insert(stack_ptr.storage);
                        return Err(ImportError::Unsupported(
                            "unaligned access through a word-cell storage".into(),
                        ));
                    }
                    // Pointer-VALUE provenance pairing: a tracked pointer
                    // stored to a tracked slot teaches the slot's identity;
                    // a non-tracked value stored over it invalidates the
                    // pairing (the slot may then hold an untracked pointer).
                    if let Some(val_ev) = instr
                        .get_operand(0)
                        .and_then(|o| o.value())
                        .map(|v| v.as_any_value_enum())
                    {
                        match fctx.stack_slot_of.get(&val_ev).cloned() {
                            Some(spilled) => {
                                fctx.slot_ptr_of.insert(ptr.as_any_value_enum(), spilled);
                            }
                            None => {
                                fctx.slot_ptr_of.remove(&ptr.as_any_value_enum());
                            }
                        }
                    }
                    let ptr_bits0 = fctx
                        .cache
                        .get(&ptr.as_any_value_enum())
                        .and_then(|b| b.first().copied())
                        .ok_or_else(|| {
                            ImportError::Unsupported("stack pointer bits missing (internal)".into())
                        })?;
                    let addr_bound = self.stack_addr_bound(&stack_ptr);
                    self.stack_store_dynamic(
                        fctx,
                        stack_ptr.storage,
                        ptr_bits0,
                        &stack_ptr.addr_bits,
                        &val,
                        addr_bound,
                    );
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
                    // Offset GEP off a tracked stack pointer. Constant-index
                    // GEPs (single or multi-index; arrays and structs, via
                    // the module data layout) fold through
                    // `gep_instr_constant_offset`. A symbolic index is
                    // permitted only as a GEP's single index, stepping by
                    // the source element type's width (scalars: bit width;
                    // aggregates: whole byte size ×8 for the bit-addressed
                    // stack).
                    let elem_ty = instr
                        .get_gep_source_element_type()
                        .map_err(|_| ImportError::Unsupported("malformed gep".into()))?;
                    let step_bits: i64 = match elem_ty {
                        BasicTypeEnum::StructType(_) | BasicTypeEnum::ArrayType(_) => {
                            self.memory_byte_size(elem_ty)? as i64 * 8
                        }
                        _ => self.llvm_bit_width(elem_ty).map_err(|_| {
                            ImportError::Unsupported(
                                "gep of non-integer/pointer element type into stack pointer not supported"
                                    .into(),
                            )
                        })? as i64,
                    };
                    let folded = self.gep_instr_constant_offset(instr)?;
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

                    let new_stack_ptr = if let Some(byte_offset) = folded {
                        // All indices constant (any count): one byte offset
                        // (data-layout aware) converted to bits.
                        let offset = i64::try_from(byte_offset)
                            .ok()
                            .and_then(|bytes| bytes.checked_mul(8))
                            .ok_or_else(|| {
                                ImportError::Unsupported("gep offset overflow".into())
                            })?;
                        let addr = match base_ptr.const_offset {
                            Some(base_off) => {
                                Some(base_off.checked_add_signed(offset).ok_or_else(|| {
                                    ImportError::Unsupported("gep offset out of range".into())
                                })?)
                            }
                            // A runtime base (a specialized parameter's provenance
                            // is `const_offset: None`) plus a constant delta stays
                            // runtime: `addr_bits` below carries the folded offset;
                            // there is simply no compile-time offset to extend.
                            None => None,
                        };
                        let addr_bits =
                            self.add_signed_delta(fctx, cur, &base_ptr.addr_bits, offset);
                        StackPtr {
                            storage: base_ptr.storage,
                            allocation_bits: base_ptr.allocation_bits,
                            const_offset: addr,
                            addr_bits,
                        }
                    } else {
                        // A symbolic index: single-index GEPs only.
                        if instr.get_num_operands() != 2 {
                            return Err(ImportError::Unsupported(
                                "multi-index symbolic GEP into stack pointer not supported".into(),
                            ));
                        }
                        match (base_ptr.const_offset, idx_int.get_sign_extended_constant()) {
                            (Some(base_off), Some(idx)) => {
                                let offset = idx.checked_mul(step_bits).ok_or_else(|| {
                                    ImportError::Unsupported("gep offset overflow".into())
                                })?;
                                let addr =
                                    base_off.checked_add_signed(offset).ok_or_else(|| {
                                        ImportError::Unsupported("gep offset out of range".into())
                                    })?;
                                let addr_bits =
                                    self.add_signed_delta(fctx, cur, &base_ptr.addr_bits, offset);
                                StackPtr {
                                    storage: base_ptr.storage,
                                    allocation_bits: base_ptr.allocation_bits,
                                    const_offset: Some(addr),
                                    addr_bits,
                                }
                            }
                            (_, _) => {
                                let idx_bits = self.value_bits(fctx, idx_val)?;
                                let idx_bits = resize_bits_signed(&idx_bits, self.pointer_bits);
                                let new_addr_bits = {
                                    let mut c = Ctx {
                                        fctx,
                                        bit_tid: self.bit_tid,
                                        block: cur,
                                    };
                                    circuits::bc_add_scaled_const(
                                        &mut c,
                                        &base_ptr.addr_bits,
                                        &idx_bits,
                                        step_bits as u64,
                                    )
                                };
                                StackPtr {
                                    storage: base_ptr.storage,
                                    allocation_bits: base_ptr.allocation_bits,
                                    const_offset: None,
                                    addr_bits: new_addr_bits,
                                }
                            }
                        }
                    };

                    let base_bits = fctx
                        .cache
                        .get(&base.as_any_value_enum())
                        .cloned()
                        .unwrap_or_else(|| new_stack_ptr.addr_bits.clone());
                    fctx.emit(
                        cur,
                        Value::PtrOffset {
                            ptr: base_bits[0],
                            idx: new_stack_ptr.addr_bits[0],
                            elem_bits: step_bits as usize,
                        },
                    );
                    let tagged = self.encode_stack_ptr(
                        fctx,
                        cur,
                        new_stack_ptr.storage,
                        &new_stack_ptr.addr_bits,
                    )?;
                    fctx.stack_slot_of
                        .insert(instr.as_any_value_enum(), new_stack_ptr);
                    Some(tagged)
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
                                            circuits::bc_add_scaled_const(
                                                &mut c,
                                                &base_offset_bits,
                                                &idx_bits,
                                                elem_bytes as u64,
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
                    // Aggregates (struct/array element types) step by their
                    // whole ABI byte size (module data layout); scalars keep
                    // the existing bit-width/8 step. The GEP itself is pure
                    // address arithmetic — the load/store through the
                    // result dispatches on its own (scalar) type.
                    let elem_bytes = match elem_ty {
                        inkwell::types::BasicTypeEnum::IntType(_)
                        | inkwell::types::BasicTypeEnum::PointerType(_) => {
                            (self.llvm_bit_width(elem_ty)? as u64).div_ceil(8)
                        }
                        inkwell::types::BasicTypeEnum::StructType(_)
                        | inkwell::types::BasicTypeEnum::ArrayType(_) => {
                            self.memory_byte_size(elem_ty)?
                        }
                        _ => {
                            return Err(ImportError::Unsupported(
                                "gep of non-integer/pointer aggregate element type into an \
                                 unresolved pointer not supported"
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
                        circuits::bc_add_scaled_const(
                            &mut c,
                            &base_bits,
                            &idx_bits,
                            elem_bytes as u64,
                        )
                    };
                    Some(new_bits)
                }
            }
            InstructionOpcode::Call => {
                let call = CallSiteValue::try_from(instr)
                    .map_err(|_| ImportError::Unsupported("malformed call instruction".into()))?;
                let callee_fn = match call.get_called_fn_value() {
                    Some(f) => f,
                    // INDIRECT CALL: the callee is a loaded function
                    // pointer (e.g. revm's InstructionTable dispatch).
                    // Dispatch over the module's stored fn-pointer
                    // constants via an equality cascade + a dense
                    // positional `Terminator::Table`.
                    None => return self.translate_indirect_call(fctx, instr),
                };
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
                } else if callee_fn.get_first_basic_block().is_none()
                    && volar_ir_common::parse_field_symbol(&callee_name).is_some()
                {
                    let symbol = volar_ir_common::parse_field_symbol(&callee_name).expect("parsed");
                    Some(self.lower_field_call(fctx, cur, instr, &symbol)?)
                } else if let Some(intrinsic) = OverflowIntrinsic::from_name(&callee_name) {
                    let fields = self.translate_overflow_intrinsic(fctx, instr, intrinsic)?;
                    fctx.aggregate_fields
                        .insert(instr.as_any_value_enum(), fields);
                    None
                } else if callee_name == aes_extern::LLVM_SYMBOL {
                    // Circuit extern: `__portal_aes128_encrypt_block`
                    // lowers to an IR-level `Stmt::OracleCall`, never a
                    // declaration-only `Value::Call`.
                    self.translate_aes_extern(fctx, instr)?;
                    None
                } else if let Some(external) = self.externals.get(callee_name.as_ref()).cloned() {
                    // Explicitly configured external declarations are the
                    // only generic LLVM calls admitted as external MPC work.
                    // Every other direct call retains ordinary VAFFLE call
                    // semantics below; there is no name-based inference.
                    self.translate_configured_external(
                        fctx,
                        instr,
                        callee_fn,
                        &callee_name,
                        external,
                    )?
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
                    // Call-site specialization: classify each pointer
                    // argument's static provenance so the callee imports
                    // (or reuses) a copy whose parameter accesses resolve
                    // statically. `call_arg_bits` validated the operands
                    // above; call operands are the arguments followed by
                    // the callee itself.
                    let prov: Vec<Prov> = callee_fn
                        .get_params()
                        .iter()
                        .enumerate()
                        .map(|(i, pty)| {
                            if !matches!(pty.get_type(), BasicTypeEnum::PointerType(_)) {
                                return Prov::Any;
                            }
                            let arg_ref = unsafe { LLVMGetOperand(instr.as_value_ref(), i as u32) };
                            if arg_ref.is_null() {
                                return Prov::Any;
                            }
                            let arg = unsafe { BasicValueEnum::new(arg_ref) };
                            self.arg_provenance(fctx, arg)
                        })
                        .collect();
                    let callee_id = self.func_id(callee_fn, &prov)?;
                    called.push((callee_fn, prov));
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
                        let struct_bits: Option<Option<Bits>> = match instr.get_type().try_into() {
                            Ok(inkwell::types::BasicTypeEnum::StructType(_))
                            | Ok(inkwell::types::BasicTypeEnum::ArrayType(_)) => {
                                // Struct/array returns flatten to per-bit
                                // `Value::Output`s across the callee's single
                                // wide result; leaf-field boundaries are
                                // recorded so `extractvalue` can slice them.
                                let layout_ty: inkwell::types::BasicTypeEnum<'ctx> =
                                    instr.get_type().try_into().map_err(|_| {
                                        ImportError::Unsupported(
                                            "call result must be a first-class type".into(),
                                        )
                                    })?;
                                let layout = self.aggregate_field_layout(layout_ty)?;
                                let mut bits: Bits = Vec::with_capacity(layout.iter().sum());
                                let mut fields: Vec<Option<Bits>> =
                                    Vec::with_capacity(layout.len());
                                for width in layout {
                                    let mut field: Bits = Vec::with_capacity(width);
                                    for _ in 0..width {
                                        let idx = bits.len();
                                        field.push(
                                            fctx.emit(cur, Value::Output { value: vid, idx }),
                                        );
                                    }
                                    bits.extend_from_slice(&field);
                                    fields.push(Some(field));
                                }
                                fctx.aggregate_fields
                                    .insert(instr.as_any_value_enum(), fields);
                                Some(Some(bits))
                            }
                            _ => None,
                        };
                        match struct_bits {
                            Some(bits) => bits,
                            None => match instr.get_type().try_into() {
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
                            },
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
                // Pool packing: a NON-ESCAPING alloca has exactly one
                // possible location — carve its bytes from the static pool
                // (the single storage globals pack into) instead of
                // allocating a per-site data stack. Every access through it
                // resolves statically (same tracked-`StackPtr` machinery,
                // different backing storage), and runtime dispatch never
                // needs to read its cells. Escaping allocas keep per-site
                // stacks: a runtime-dispatched pointer may name any of
                // them, so dispatch must read them all.
                if !self.alloca_escapes(instr) {
                    let pool = self.pool_ensure();
                    let n_bytes = total_slots.div_ceil(8);
                    let base = self.pool_carve(self.pool_next_byte, n_bytes)?;
                    let base_bit_off = base.checked_mul(8).ok_or_else(|| {
                        ImportError::Unsupported("alloca pool offset overflow".into())
                    })?;
                    if base_bit_off >= (1u64 << self.global_addr_bits()) {
                        return Err(ImportError::Unsupported(
                            "alloca pool offset exceeds the pointer-value encoding".into(),
                        ));
                    }
                    let addr_bits: Bits = (0..self.pointer_bits)
                        .map(|b| self.bc_const_at(fctx, cur, (base_bit_off >> b) & 1 != 0))
                        .collect();
                    fctx.stack_slot_of.insert(
                        instr.as_any_value_enum(),
                        StackPtr {
                            storage: pool,
                            allocation_bits: total_slots,
                            const_offset: Some(0),
                            addr_bits: addr_bits.clone(),
                        },
                    );
                    Some(self.encode_stack_ptr(fctx, cur, pool, &addr_bits)?)
                } else {
                    // The tagged pointer encoding keeps the bit offset in the
                    // low `global_addr_bits` field. One site's allocation has to
                    // fit there; the calling-convention frame is a different storage.
                    if total_slots >= (1u64 << self.global_addr_bits()) {
                        return Err(ImportError::Unsupported(
                            "alloca stack region too large for the pointer-value encoding".into(),
                        ));
                    }

                    let storage = self.storage_alloc.alloc();
                    let sp = self.storage_alloc.alloc();
                    self.alloca_data.push(storage);
                    self.alloca_sizes.push(total_slots);
                    // Freeze the cell granularity for this storage: word
                    // cells only when every access is provably word-aligned
                    // (see `alloca_word_aligned`). Dispatch coherence holds
                    // because the dispatch path routes candidates through
                    // the same stack_load/store_dynamic this flag switches.
                    self.storage_word.insert(
                        storage,
                        self.alloca_word_aligned(instr, self.word_bytes),
                    );
                    self.sp_globals.push(sp);
                    self.pre_init.push(PreInitSegment {
                        storage: sp,
                        ty: self.addr_tid,
                        offset: 0,
                        data: vec![Constant { hi: 0, lo: 0 }],
                    });
                    fctx.sp_globals.push(sp);

                    let elem_tid = self.llvm_type_id(scalar_ty)?;
                    let alloc_vid = fctx.emit(
                        cur,
                        Value::StackAlloc {
                            elem_ty: elem_tid,
                            count: count as usize,
                            storage,
                            sp,
                        },
                    );
                    let addr_bits = self.explode_pointer(fctx, cur, alloc_vid);
                    fctx.stack_slot_of.insert(
                        instr.as_any_value_enum(),
                        StackPtr {
                            storage,
                            allocation_bits: total_slots,
                            const_offset: Some(0),
                            addr_bits: addr_bits.clone(),
                        },
                    );
                    Some(self.encode_stack_ptr(fctx, cur, storage, &addr_bits)?)
                }
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

    fn validate_configured_external_declaration(
        &self,
        callee: FunctionValue<'ctx>,
        symbol: &str,
        external: &LlvmExternalImportKind,
    ) -> IResult<()> {
        if callee.get_first_basic_block().is_some() {
            return Err(ImportError::Unsupported(format!(
                "configured external `{symbol}` must target a declaration, not a defined LLVM function"
            )));
        }
        let signature = callee.get_type();
        if signature.is_var_arg() {
            return Err(ImportError::Unsupported(format!(
                "configured external `{symbol}` must not be variadic"
            )));
        }
        let expected_params = match external {
            LlvmExternalImportKind::Oracle { .. } => signature.count_param_types() as usize,
            LlvmExternalImportKind::Action { n_args, .. } => {
                n_args.checked_add(2).ok_or_else(|| {
                    ImportError::Unsupported(format!(
                        "configured action `{symbol}` argument count overflows its ABI"
                    ))
                })?
            }
        };
        if signature.count_param_types() as usize != expected_params {
            return Err(ImportError::Unsupported(format!(
                "configured external `{symbol}` declaration has an incompatible parameter count"
            )));
        }
        for ty in signature.get_param_types() {
            if !matches!(ty, BasicMetadataTypeEnum::IntType(_)) {
                return Err(ImportError::Unsupported(format!(
                    "configured external `{symbol}` declaration requires scalar integer parameters"
                )));
            }
        }
        let Some(BasicTypeEnum::IntType(_)) = signature.get_return_type() else {
            return Err(ImportError::Unsupported(format!(
                "configured external `{symbol}` declaration must return one scalar integer"
            )));
        };
        if matches!(external, LlvmExternalImportKind::Action { .. }) {
            let types = signature.get_param_types();
            let BasicMetadataTypeEnum::IntType(guard) = types[0] else {
                unreachable!("integer parameters checked above");
            };
            if guard.get_bit_width() != 1 {
                return Err(ImportError::Unsupported(format!(
                    "configured action `{symbol}` declaration guard must be i1"
                )));
            }
            let BasicMetadataTypeEnum::IntType(fallback) = types[types.len() - 1] else {
                unreachable!("integer parameters checked above");
            };
            let Some(BasicTypeEnum::IntType(result)) = signature.get_return_type() else {
                unreachable!("integer result checked above");
            };
            if fallback.get_bit_width() != result.get_bit_width() {
                return Err(ImportError::Unsupported(format!(
                    "configured action `{symbol}` declaration fallback type must match its result type"
                )));
            }
        }
        Ok(())
    }

    fn register_configured_oracle(
        &mut self,
        symbol: &str,
        params: &[TypeId],
        results: &[TypeId],
        execution: OracleExecutionPolicy,
    ) -> IResult<()> {
        if let Some(existing) = self.oracles.iter().find(|decl| decl.name == symbol) {
            if existing.params != params
                || existing.results != results
                || existing.execution != execution
            {
                return Err(ImportError::Unsupported(format!(
                    "configured oracle `{symbol}` has inconsistent call signatures or execution policy"
                )));
            }
            return Ok(());
        }
        self.oracles.push(OracleDecl {
            name: symbol.into(),
            params: params.to_vec(),
            results: results.to_vec(),
            execution,
        });
        Ok(())
    }

    fn register_configured_action(
        &mut self,
        symbol: &str,
        params: &[TypeId],
        results: &[TypeId],
        execution: ActionExecutionPolicy,
    ) -> IResult<()> {
        if let Some(existing) = self.actions.iter().find(|decl| decl.name == symbol) {
            if existing.params != params
                || existing.results != results
                || existing.execution != execution
            {
                return Err(ImportError::Unsupported(format!(
                    "configured action `{symbol}` has inconsistent call signatures or execution policy"
                )));
            }
            return Ok(());
        }
        self.actions.push(ActionDecl {
            name: symbol.into(),
            params: params.to_vec(),
            results: results.to_vec(),
            execution,
        });
        Ok(())
    }

    /// Lower a direct configured LLVM declaration to a real Volar external
    /// primitive, preserving its explicit execution policy in the module
    /// declaration table. The present ABI supports scalar integer inputs and
    /// one scalar integer result; aggregate/pointer host ABI adaptation must
    /// use a separately reviewed lowering instead of being guessed here.
    fn translate_configured_external(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        instr: InstructionValue<'ctx>,
        callee: FunctionValue<'ctx>,
        symbol: &str,
        external: LlvmExternalImportKind,
    ) -> IResult<Option<Bits>> {
        self.validate_configured_external_declaration(callee, symbol, &external)?;
        let n_args = instr.get_num_operands().saturating_sub(1) as usize;
        let mut args = Vec::with_capacity(n_args);
        let mut params = Vec::with_capacity(n_args);
        for index in 0..n_args as u32 {
            let value = call_value_operand(instr, index, "configured external argument")?;
            let BasicTypeEnum::IntType(_) = value.get_type() else {
                return Err(ImportError::Unsupported(format!(
                    "configured external `{symbol}` requires scalar integer arguments"
                )));
            };
            let bits = self.value_bits(fctx, value)?;
            let ty = self.llvm_type_id(value.get_type())?;
            let merged = if bits.len() == 1 {
                bits[0]
            } else {
                fctx.emit(fctx.current, Value::Op(Stmt::Merge { parts: bits, ty }))
            };
            args.push(merged);
            params.push(ty);
        }
        let result_type: Result<BasicTypeEnum<'ctx>, _> = instr.get_type().try_into();
        let result_type = result_type.map_err(|_| {
            ImportError::Unsupported(format!(
                "configured external `{symbol}` must return one scalar integer"
            ))
        })?;
        let BasicTypeEnum::IntType(_) = result_type else {
            return Err(ImportError::Unsupported(format!(
                "configured external `{symbol}` must return one scalar integer"
            )));
        };
        let result_tid = self.llvm_type_id(result_type)?;
        let output_tys = vec![result_tid];
        let result_ty = self.types.intern(IrType::Tuple(output_tys.clone()));
        let cur = fctx.current;
        let (call, output) = match external {
            LlvmExternalImportKind::Oracle { execution } => {
                self.register_configured_oracle(symbol, &params, &output_tys, execution)?;
                let call = fctx.emit(
                    cur,
                    Value::Op(Stmt::OracleCall {
                        name: symbol.into(),
                        args,
                        output_tys: output_tys.clone(),
                        result_ty,
                    }),
                );
                let output = fctx.emit(
                    cur,
                    Value::Op(Stmt::OracleOutput {
                        call,
                        idx: 0,
                        ty: result_tid,
                    }),
                );
                (call, output)
            }
            LlvmExternalImportKind::Action { n_args, execution } => {
                let expected_args = n_args.checked_add(2).ok_or_else(|| {
                    ImportError::Unsupported("configured action argument count overflow".into())
                })?;
                if args.len() != expected_args {
                    return Err(ImportError::Unsupported(format!(
                        "configured action `{symbol}` expects guard + {n_args} args + 1 fallback"
                    )));
                }
                let guard_value = call_value_operand(instr, 0, "configured action guard")?;
                if self.llvm_bit_width(guard_value.get_type())? != 1 {
                    return Err(ImportError::Unsupported(format!(
                        "configured action `{symbol}` guard must be i1"
                    )));
                }
                let fallback_value =
                    call_value_operand(instr, (n_args + 1) as u32, "configured action fallback")?;
                if fallback_value.get_type() != result_type {
                    return Err(ImportError::Unsupported(format!(
                        "configured action `{symbol}` fallback type must match its result type"
                    )));
                }
                let guard = args[0];
                let action_params = params[1..1 + n_args].to_vec();
                let fallback = args[n_args + 1];
                self.register_configured_action(symbol, &action_params, &output_tys, execution)?;
                let call = fctx.emit(
                    cur,
                    Value::Op(Stmt::ActionCall {
                        name: symbol.into(),
                        guard,
                        args: args[1..1 + n_args].to_vec(),
                        fallbacks: vec![fallback],
                        output_tys: output_tys.clone(),
                        result_ty,
                    }),
                );
                let output = fctx.emit(
                    cur,
                    Value::Op(Stmt::ActionOutput {
                        call,
                        idx: 0,
                        ty: result_tid,
                    }),
                );
                (call, output)
            }
        };
        let bits = (0..self.llvm_bit_width(instr.get_type())?)
            .map(|bit| {
                fctx.emit(
                    cur,
                    Value::Op(Stmt::Shuffle {
                        result_bits: vec![(bit as u8, output)],
                        ty: self.bit_tid,
                    }),
                )
            })
            .collect();
        let _ = call;
        Ok(Some(bits))
    }

    /// Lower the `__portal_aes128_encrypt_block` circuit extern (the
    /// `volar_ir_common::aes_extern` contract): a declaration-only
    /// `void (u64 out[2], const u64 key[2], const u64 pt[2])` call becomes
    /// an IR-level `Stmt::OracleCall` against the `aes128_encrypt_block`
    /// oracle, reading key/plaintext through the memory-intrinsic pointer
    /// machinery and storing the ciphertext back through `out`.
    ///
    /// The oracle is registered in the module's oracle table on first use so
    /// the vaffle→IR lowering validates every emitted call against it.
    fn translate_aes_extern(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        instr: InstructionValue<'ctx>,
    ) -> IResult<()> {
        let n_args = instr.get_num_operands().saturating_sub(1);
        if n_args != 3 {
            return Err(ImportError::Unsupported(format!(
                "{} takes exactly 3 pointer args (out, key, pt)",
                aes_extern::LLVM_SYMBOL
            )));
        }
        let operand_ptr = |index: u32| -> IResult<PointerValue<'ctx>> {
            match call_value_operand(instr, index, "aes128 extern pointer arg")? {
                BasicValueEnum::PointerValue(p) => Ok(p),
                _ => Err(ImportError::Unsupported(format!(
                    "{} arg {index} must be a pointer",
                    aes_extern::LLVM_SYMBOL
                ))),
            }
        };
        let out = self.intrinsic_pointer(fctx, operand_ptr(0)?)?;
        let key = self.intrinsic_pointer(fctx, operand_ptr(1)?)?;
        let pt = self.intrinsic_pointer(fctx, operand_ptr(2)?)?;
        self.validate_intrinsic_pointer(&out, aes_extern::CT_BITS / 8)?;
        self.validate_intrinsic_pointer(&key, aes_extern::KEY_BITS / 8)?;
        self.validate_intrinsic_pointer(&pt, aes_extern::PT_BITS / 8)?;

        let key_bits = self.intrinsic_load(fctx, &key, aes_extern::KEY_BITS / 8)?;
        let pt_bits = self.intrinsic_load(fctx, &pt, aes_extern::PT_BITS / 8)?;

        // Oracle signature: params [u64; 4] = (key_lo, key_hi, pt_lo, pt_hi),
        // results [u64; 2] = (ct_lo, ct_hi), each u64 little-endian
        // byte-packed (matching the contract's byte-major, LSB-first-in-byte
        // bit layout).
        // The `Type::_n` cutover: a 64-bit word is `Vec(64, Bit)`.
        let u64_tid = self.types.intern(IrType::Vec(64, self.bit_tid));
        if !self
            .oracles
            .iter()
            .any(|o| o.name == aes_extern::ORACLE_NAME)
        {
            self.oracles.push(OracleDecl {
                name: aes_extern::ORACLE_NAME.into(),
                params: vec![u64_tid; 4],
                results: vec![u64_tid; 2],

                execution: volar_ir_common::OracleExecutionPolicy::legacy_evaluator(),
            });
        }
        let output_tys = vec![u64_tid; 2];
        let result_ty = self.types.intern(IrType::Tuple(output_tys.clone()));

        let cur = fctx.current;
        let arg_vars: Vec<ValueId> = key_bits
            .iter()
            .chain(pt_bits.iter())
            .copied()
            .collect::<Vec<_>>()
            .chunks(64)
            .map(|chunk| {
                fctx.emit(
                    cur,
                    Value::Op(Stmt::Merge {
                        parts: chunk.to_vec(),
                        ty: u64_tid,
                    }),
                )
            })
            .collect();
        let call = fctx.emit(
            cur,
            Value::Op(Stmt::OracleCall {
                name: aes_extern::ORACLE_NAME.into(),
                args: arg_vars,
                output_tys,
                result_ty,
            }),
        );
        let mut ct_bits: Bits = Vec::with_capacity(aes_extern::CT_BITS);
        for idx in 0..2usize {
            let word = fctx.emit(
                cur,
                Value::Op(Stmt::OracleOutput {
                    call,
                    idx,
                    ty: u64_tid,
                }),
            );
            for b in 0..64u8 {
                ct_bits.push(fctx.emit(
                    cur,
                    Value::Op(Stmt::Shuffle {
                        result_bits: vec![(b, word)],
                        ty: self.bit_tid,
                    }),
                ));
            }
        }
        self.intrinsic_store(fctx, &out, &ct_bits)?;
        Ok(())
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
            circuits::bc_add_const(&mut c, &index_bits, 1)
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
        let stack_offset = {
            let mut c = Ctx {
                fctx,
                bit_tid: self.bit_tid,
                block,
            };
            circuits::bc_shl_const(&mut c, &index, 3)
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

    fn lower_field_call(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        cur: BlockId,
        instr: InstructionValue<'ctx>,
        symbol: &volar_ir_common::FieldSymbol,
    ) -> IResult<Bits> {
        let n_args = instr.get_num_operands().saturating_sub(1);
        let mut field_args = Vec::with_capacity(n_args as usize);
        for i in 0..n_args {
            let value = call_value_operand(instr, i, "field argument")?;
            let constant = match value {
                inkwell::values::BasicValueEnum::IntValue(int) => {
                    int.get_zero_extended_constant().map(|bits| bits as u128)
                }
                _ => None,
            };
            field_args.push(volar_ir_common::FieldArg {
                bits: self.value_bits(fctx, value)?,
                constant,
            });
        }
        let ret_bits = self.llvm_bit_width(instr.get_type())?;
        volar_ir_common::lower_field_call(symbol, &field_args, ret_bits, &mut self.types, |stmt| {
            fctx.emit(cur, Value::Op(stmt))
        })
        .map_err(|err| ImportError::Unsupported(format!("volar.field: {err:?}")))
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
    ) -> IResult<Vec<Option<Bits>>> {
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

        Ok(vec![Some(result), Some(vec![overflow])])
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
        fields
            .get(*field as usize)
            .cloned()
            .flatten()
            .ok_or_else(|| {
                ImportError::Unsupported(format!(
                    "extractvalue field {field} is not an initialized tracked aggregate field"
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
        if let IntrinsicPointer::Stack { ptr, .. } = ptr
            && ptr.const_offset.is_some()
        {
            ptr.intrinsic_range(n_bytes)?;
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
            IntrinsicPointer::Stack { ptr, ptr_bits0 } => {
                let n_bits = n_bytes.checked_mul(8).ok_or_else(|| {
                    ImportError::Unsupported("memory intrinsic length is too large".into())
                })?;
                Ok(self.stack_load_dynamic(
                    fctx,
                    ptr.storage,
                    *ptr_bits0,
                    &ptr.addr_bits,
                    self.byte_tid,
                    n_bits,
                    self.stack_addr_bound(ptr),
                ))
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
            IntrinsicPointer::Stack { ptr, ptr_bits0 } => {
                self.stack_store_dynamic(
                    fctx,
                    ptr.storage,
                    *ptr_bits0,
                    &ptr.addr_bits,
                    bytes,
                    self.stack_addr_bound(ptr),
                );
            }
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

    /// Bit-decompose a compile-time-constant `u64` (a global's constant byte
    /// offset), `PTR_BITS` wide, LSB first.
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

    /// Project `src` into `n` bit values. `src` is the `StackAlloc` result.
    fn explode_pointer(&mut self, fctx: &mut FuncCtx<'ctx>, block: BlockId, src: ValueId) -> Bits {
        (0..self.pointer_bits)
            .map(|i| {
                fctx.emit(
                    block,
                    Value::Op(Stmt::Shuffle {
                        result_bits: vec![(i as u8, src)],
                        ty: self.bit_tid,
                    }),
                )
            })
            .collect()
    }

    /// `base + delta`, with `delta` a signed bit count in two's complement.
    fn add_signed_delta(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        block: BlockId,
        base: &Bits,
        delta: i64,
    ) -> Bits {
        // Sub-storage arithmetic: a constant base (every pool-backed
        // alloca) stays constant through the fold — Const bits out, no
        // adder circuits — so every later access through the folded pointer
        // hits `dynamic_addr`'s zero-adder constant fold. Without this the
        // gep fold's `bc_add` demoted constant addresses to runtime adder
        // outputs and every access paid a full pointer-width adder.
        let mut base_val = 0u64;
        let mut is_const = true;
        for (b, &vid) in base.iter().enumerate() {
            match &fctx.values[vid.0].kind {
                Value::Op(Stmt::Const(c, _)) => {
                    if c.lo & 1 != 0 {
                        base_val |= 1u64 << b;
                    }
                }
                _ => {
                    is_const = false;
                    break;
                }
            }
        }
        if is_const {
            let sum = (base_val as i64).wrapping_add(delta);
            let mut c = Ctx {
                fctx,
                bit_tid: self.bit_tid,
                block,
            };
            return (0..self.pointer_bits)
                .map(|b| c.bc_const(((sum as u64) >> b) & 1 != 0))
                .collect();
        }
        let mut c = Ctx {
            fctx,
            bit_tid: self.bit_tid,
            block,
        };
        circuits::bc_add_const(&mut c, base, delta as u64)
    }

    /// Tagged pointer value for one alloca site: low `global_addr_bits` of
    /// the absolute bit address, then `GLOBAL_ID_BITS` of `storage`, tag 0.
    fn encode_stack_ptr(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        block: BlockId,
        storage: StorageId,
        addr_bits: &Bits,
    ) -> IResult<Bits> {
        if storage.0 >= (1u32 << GLOBAL_ID_BITS) {
            return Err(ImportError::Unsupported(
                "too many storages for the pointer-value encoding".into(),
            ));
        }
        let global_addr_bits = self.global_addr_bits();
        let mut bits = Vec::with_capacity(self.pointer_bits);
        bits.extend_from_slice(&addr_bits[..global_addr_bits]);
        for b in 0..GLOBAL_ID_BITS {
            bits.push(self.bc_const_at(fctx, block, (storage.0 >> b) & 1 != 0));
        }
        bits.push(self.bc_const_at(fctx, block, false));
        Ok(bits)
    }

    /// Read each stack-pointer global this function bumps, at the start of
    /// the entry block, and write those values back before every return and
    /// tail call. Unreachable fallback returns do not restore.
    fn install_sp_reverts(&mut self, fctx: &mut FuncCtx<'ctx>) {
        if fctx.sp_globals.is_empty() {
            return;
        }
        let sps = fctx.sp_globals.clone();
        let entry = BlockId(0);
        let before = fctx.stmts[entry.0].len();
        let mut saved = Vec::with_capacity(sps.len());
        for sp in sps {
            let addr = fctx.emit(
                entry,
                Value::Op(Stmt::Const(Constant { hi: 0, lo: 0 }, self.addr_tid)),
            );
            let val = fctx.emit(
                entry,
                Value::Op(Stmt::StorageRead {
                    storage: sp,
                    ty: self.addr_tid,
                    addr,
                }),
            );
            saved.push((sp, val));
        }
        let stmts = &mut fctx.stmts[entry.0];
        let mut reads = stmts.split_off(before);
        reads.append(stmts);
        *stmts = reads;

        let exits: Vec<usize> = fctx
            .terminators
            .iter()
            .enumerate()
            .filter_map(|(i, term)| match term {
                Some(Terminator::Return { .. }) | Some(Terminator::ReturnCall { .. }) => Some(i),
                _ => None,
            })
            .collect();
        for i in exits {
            for &(sp, val) in &saved {
                let addr = fctx.emit(
                    BlockId(i),
                    Value::Op(Stmt::Const(Constant { hi: 0, lo: 0 }, self.addr_tid)),
                );
                fctx.emit(
                    BlockId(i),
                    Value::Op(Stmt::StorageWrite {
                        storage: sp,
                        src: val,
                        ty: self.addr_tid,
                        addr,
                    }),
                );
            }
        }
    }

    /// Inline `bc_*`, or intern one typed helper and call it.
    fn alu(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        block: BlockId,
        op: HelperOp,
        operands: &[Bits],
    ) -> Bits {
        // DIV/REM pair sharing: route all four division ops through the
        // combined div/rem helpers so a DIV + MOD pair on the same operand
        // widths interns and references ONE circuit body (and a lone REM
        // no longer pays an extra multiply to re-derive its result).
        if let Some((shared, want_quotient)) = divrem_split(op) {
            let n = operands[0].len();
            let full = self.alu(fctx, block, shared, operands);
            return full
                .into_iter()
                .skip(if want_quotient { 0 } else { n })
                .take(n)
                .collect();
        }
        if self.helper_mode != CircuitHelperMode::Extract {
            return self.alu_inline(fctx, block, op, operands);
        }
        let widths: Vec<usize> = operands.iter().map(|bits| bits.len()).collect();
        let before_helpers = self.total_values();
        let func = intern_circuit_helper(
            &mut self.types,
            &mut self.sigs,
            &mut self.funcs,
            &mut self.helper_cache,
            op,
            &widths,
        );
        if std::env::var("VLVI_IMPORT_STATS").is_ok() {
            let body = self.total_values() - before_helpers;
            if body > 50000 {
                eprintln!("stats: helper {op:?} widths={widths:?} body={body}");
            }
        }
        let result_width = helper_result_width(op, &widths);
        let args: Vec<ValueId> = operands
            .iter()
            .map(|bits| self.merge_operand(fctx, block, bits))
            .collect();
        let call = fctx.emit(block, Value::Call { func, args });
        (0..result_width)
            .map(|i| {
                fctx.emit(
                    block,
                    Value::Output {
                        value: call,
                        idx: i,
                    },
                )
            })
            .collect()
    }

    fn merge_operand(&mut self, fctx: &mut FuncCtx<'ctx>, block: BlockId, bits: &Bits) -> ValueId {
        if bits.len() <= 1 {
            return bits[0];
        }
        let ty = width_type(&mut self.types, bits.len());
        fctx.emit(
            block,
            Value::Op(Stmt::Merge {
                parts: bits.to_vec(),
                ty,
            }),
        )
    }

    fn alu_inline(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        block: BlockId,
        op: HelperOp,
        operands: &[Bits],
    ) -> Bits {
        let mut c = Ctx {
            fctx,
            bit_tid: self.bit_tid,
            block,
        };
        match op {
            HelperOp::Add => circuits::bc_add(&mut c, &operands[0], &operands[1], false),
            HelperOp::Sub => circuits::bc_sub(&mut c, &operands[0], &operands[1]),
            HelperOp::Mul => circuits::bc_mul(&mut c, &operands[0], &operands[1]),
            HelperOp::UDiv => circuits::bc_udiv(&mut c, &operands[0], &operands[1]),
            HelperOp::SDiv => circuits::bc_sdiv(&mut c, &operands[0], &operands[1]),
            HelperOp::URem => circuits::bc_urem(&mut c, &operands[0], &operands[1]),
            HelperOp::SRem => circuits::bc_srem(&mut c, &operands[0], &operands[1]),
            HelperOp::UDivRem => {
                let (q, r) = circuits::bc_udivrem(&mut c, &operands[0], &operands[1]);
                q.into_iter().chain(r).collect()
            }
            HelperOp::SDivRem => {
                let (q, r) = circuits::bc_sdivrem(&mut c, &operands[0], &operands[1]);
                q.into_iter().chain(r).collect()
            }
            HelperOp::Shl => circuits::bc_shl(&mut c, &operands[0], &operands[1]),
            HelperOp::LShr => circuits::bc_lshr(&mut c, &operands[0], &operands[1]),
            HelperOp::AShr => circuits::bc_ashr(&mut c, &operands[0], &operands[1]),
            HelperOp::And => circuits::bc_and_vec(&mut c, &operands[0], &operands[1]),
            HelperOp::Or => circuits::bc_or_vec(&mut c, &operands[0], &operands[1]),
            HelperOp::Xor => circuits::bc_xor_vec(&mut c, &operands[0], &operands[1]),
            HelperOp::Not => circuits::bc_not_vec(&mut c, &operands[0]),
            HelperOp::Eq => vec![circuits::bc_eq(&mut c, &operands[0], &operands[1])],
            HelperOp::Ne => vec![circuits::bc_ne(&mut c, &operands[0], &operands[1])],
            HelperOp::Ult => vec![circuits::bc_ult(&mut c, &operands[0], &operands[1])],
            HelperOp::Ule => vec![circuits::bc_ule(&mut c, &operands[0], &operands[1])],
            HelperOp::Slt => vec![circuits::bc_slt(&mut c, &operands[0], &operands[1])],
            HelperOp::Sle => vec![circuits::bc_sle(&mut c, &operands[0], &operands[1])],
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
    /// The MSB is the provenance tag: `0` = stack (low `global_addr_bits`
    /// are the absolute bit offset, and the next `GLOBAL_ID_BITS` are that
    /// alloca's data `StorageId`); `1` = global (the same split, with the
    /// global's `StorageId` and a byte offset).
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
        // A function-value pointer constant: an opaque per-function
        // identity — the same family as `inttoptr(N)` (tag = 1, ID = 0,
        // ADDR = the function's stable symbol index, allocated on first
        // sight). Loads through it read zero, writes drop, and equality
        // distinguishes functions — the invariant the indirect-call
        // dispatch (`translate_indirect_call`) compares on. Function
        // pointers must NOT reach `resolve_global_ptr`: the const-chain
        // evaluator rejects them as memory-reference bases.
        {
            let raw = ptr.as_value_ref();
            if !unsafe { inkwell::llvm_sys::core::LLVMIsAFunction(raw) }.is_null() {
                let next = self.fn_symbols.len() as u64 + 1; // 0 reserved
                let idx = *self
                    .fn_symbols
                    .entry(raw as usize)
                    .or_insert(next);
                let global_addr_bits = self.global_addr_bits();
                if idx >= (1u64 << global_addr_bits) {
                    return Err(ImportError::Unsupported(
                        "function-pointer identity exceeds the pointer-value ADDR field".into(),
                    ));
                }
                let mut bits = Vec::with_capacity(self.pointer_bits);
                for b in 0..global_addr_bits {
                    bits.push(self.bc_const_at(fctx, block, (idx >> b) & 1 != 0));
                }
                for _ in 0..GLOBAL_ID_BITS {
                    bits.push(self.bc_const_at(fctx, block, false));
                }
                bits.push(self.bc_const_at(fctx, block, true)); // tag = 1
                return Ok(bits);
            }
        }
        // An `inttoptr (N to ptr)` constant (rustc's promoted ZST-slice and
        // dangling-pointer constants — e.g. `&[]`'s `inttoptr (i64 1)`, seen
        // in `Bytes::new()` at -O0): an integer-valued pointer naming no
        // storage. Encode N into the ADDR field with the unmatched-global
        // identity (tag = 1, ID = 0) — the same family as `null` — so loads
        // through it read zero, writes are dropped, and equality against the
        // same constant compares equal.
        {
            let raw = ptr.as_value_ref();
            let is_inttoptr_const = !unsafe { LLVMIsAConstantExpr(raw) }.is_null()
                && unsafe { LLVMGetConstOpcode(raw) } == LLVMOpcode::LLVMIntToPtr;
            if is_inttoptr_const {
                let operand = unsafe { LLVMGetOperand(raw, 0) };
                let iv = unsafe { IntValue::new(operand) };
                let n = iv.get_zero_extended_constant().ok_or_else(|| {
                    ImportError::Unsupported(
                        "non-constant inttoptr pointer value is unsupported".into(),
                    )
                })?;
                let global_addr_bits = self.global_addr_bits();
                if n >= (1u64 << global_addr_bits) {
                    return Err(ImportError::Unsupported(
                        "inttoptr pointer constant exceeds the pointer-value ADDR field".into(),
                    ));
                }
                let mut bits = Vec::with_capacity(self.pointer_bits);
                for b in 0..global_addr_bits {
                    bits.push(self.bc_const_at(fctx, block, (n >> b) & 1 != 0));
                }
                for _ in 0..GLOBAL_ID_BITS {
                    bits.push(self.bc_const_at(fctx, block, false));
                }
                bits.push(self.bc_const_at(fctx, block, true)); // tag = 1 (global)
                return Ok(bits);
            }
        }
        if let Some(sp) = fctx.stack_slot_of.get(&ptr.as_any_value_enum()).cloned() {
            return self.encode_stack_ptr(fctx, block, sp.storage, &sp.addr_bits);
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
        if let GlobalPtr::Const(g) = &gp
            && g.byte_offset >= (1u64 << self.global_addr_bits())
        {
            return Err(ImportError::Unsupported(
                "global byte offset too large for the pointer-value encoding".into(),
            ));
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
    /// tag = 1, global ID = 0, address = 0. Importer-created globals begin
    /// at [`GLOBAL_STORAGE_BASE`], so dispatch never matches this identity.
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
        addr_bound: u64,
    ) -> ValueId {
        // Constant-base fold: a pool-backed alloca's address bits are all
        // `Const` nodes, so `base + i` is a single constant with zero gate
        // circuits — the profile's dominant cost (a full pointer-width
        // adder per accessed bit, ~192 polys on a 55k-access function)
        // disappears entirely for the common local-variable case.
        let mut base = 0u64;
        let mut is_const = true;
        for (b, &vid) in base_addr_bits.iter().enumerate() {
            match &fctx.values[vid.0].kind {
                Value::Op(Stmt::Const(c, _)) => {
                    if c.lo & 1 != 0 {
                        base |= 1u64 << b;
                    }
                }
                _ => {
                    is_const = false;
                    break;
                }
            }
        }
        if is_const {
            return fctx.emit(
                block,
                Value::Op(Stmt::Const(
                    Constant {
                        hi: 0,
                        lo: (base + i) as u128,
                    },
                    self.addr_tid,
                )),
            );
        }
        // `i == 0` needs no adder: the address is the base itself.
        if i == 0 {
            return fctx.emit(
                block,
                Value::Op(Stmt::Merge {
                    parts: base_addr_bits.to_vec(),
                    ty: self.addr_tid,
                }),
            );
        }
        // Trimmed-width runtime adder: an access within one alloca never
        // addresses at or beyond its allocation bound, so the sum fits in
        // the bound's bit length — the low bits carry the whole adder and
        // the high bits reassemble as constants (the base's high wires are
        // semantically zero there and are dropped; out-of-bounds programs
        // already read undefined cells, so nothing sound is lost). A
        // 32-bit scalar on a runtime stack drops its address adder from
        // ~192 gate polys to ~15.
        let bound = if addr_bound == 0 || addr_bound == u64::MAX {
            0
        } else {
            addr_bound
        };
        let width = if bound > 0 {
            ((u64::BITS - (bound - 1).leading_zeros()) as usize).min(base_addr_bits.len())
        } else {
            base_addr_bits.len()
        };
        let sum = {
            let mut c = Ctx {
                fctx,
                bit_tid: self.bit_tid,
                block,
            };
            let low = &base_addr_bits[..width];
            circuits::bc_add_const(&mut c, low, i)
        };
        fctx.emit(
            block,
            Value::Op(Stmt::Merge {
                parts: sum,
                ty: self.addr_tid,
            }),
        )
    }

    /// The trimmed-address bound for one tracked stack pointer: its
    /// allocation span, except pool-backed pointers (a pool offset is
    /// bounded by the whole pool, not the alloca) and seeded parameter
    /// pointers (offset unknown) get no bound.
    fn stack_addr_bound(&self, ptr: &StackPtr) -> u64 {
        if Some(ptr.storage) == self.pool_storage || ptr.allocation_bits == u64::MAX {
            0
        } else {
            ptr.allocation_bits
        }
    }

    /// Read `n_bits` from `storage` at `base_addr_bits`, one bit per address.
    /// The shifted byte base of a dynamic access, computed ONCE per
    /// access: `Const(byte_base)` when the bit address folds, else the
    /// 61-bit `byte_bits` (the base shifted right 3 via Shuffles).
    /// Computing this per BYTE instead of per access multiplied the
    /// shift cost by the byte count — the import profile showed
    /// `op:Shuffle` at 50% of all values from exactly this redundancy.
    fn byte_base_of(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        block: BlockId,
        base_addr_bits: &Bits,
    ) -> ByteBase {
        let mut byte_base = 0u64;
        for (b, &vid) in base_addr_bits.iter().enumerate() {
            match &fctx.values[vid.0].kind {
                Value::Op(Stmt::Const(c, _)) => {
                    if c.lo & 1 != 0 {
                        byte_base |= 1u64 << b;
                    }
                }
                _ => {
                    // Runtime base: shift right 3 once, sharing the result
                    // across every byte of this access.
                    let mut byte_bits: Bits =
                        Vec::with_capacity(base_addr_bits.len().saturating_sub(3));
                    for b in 3..base_addr_bits.len() {
                        // Each `base_addr_bits[b]` is itself a 1-bit value,
                        // so the Shuffle must extract BIT 0 of it.
                        let bit = fctx.emit(
                            block,
                            Value::Op(Stmt::Shuffle {
                                result_bits: vec![(0, base_addr_bits[b])],
                                ty: self.bit_tid,
                            }),
                        );
                        byte_bits.push(bit);
                    }
                    return ByteBase::Runtime(byte_bits);
                }
            }
        }
        ByteBase::Const(byte_base >> 3)
    }

    /// The byte-granular cell address of byte `byte_i` of a dynamic access
    /// whose (once-computed) byte base is `base` — the same byte-cell
    /// convention as the constant-offset `mem_load` path.
    fn byte_addr(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        block: BlockId,
        base: &ByteBase,
        byte_i: u64,
        addr_bound: u64,
    ) -> ValueId {
        match base {
            ByteBase::Const(byte_base) => {
                // Carved pool offsets are 16-byte aligned, so the bit base
                // divides by 8 exactly.
                fctx.emit(
                    block,
                    Value::Op(Stmt::Const(
                        Constant {
                            hi: 0,
                            lo: (byte_base + byte_i) as u128,
                        },
                        self.addr_tid,
                    )),
                )
            }
            ByteBase::Runtime(byte_bits) => {
                // byte_i == 0 must NOT fall into dynamic_addr's `i == 0`
                // shortcut: that returns the base unshifted, i.e. the raw
                // BIT address where the byte-cell convention wants the
                // divided one — the exact read-path miss this layer
                // originally shipped with.
                if byte_i == 0 {
                    return fctx.emit(
                        block,
                        Value::Op(Stmt::Merge {
                            parts: byte_bits.clone(),
                            ty: self.addr_tid,
                        }),
                    );
                }
                let byte_bound = if addr_bound == 0 || addr_bound == u64::MAX {
                    0
                } else {
                    addr_bound.div_ceil(8)
                };
                self.dynamic_addr(fctx, block, byte_bits, byte_i, byte_bound)
            }
        }
    }

    fn stack_load_dynamic(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        storage: StorageId,
        ptr_bits0: ValueId,
        base_addr_bits: &Bits,
        pointee_ty: TypeId,
        n_bits: usize,
        addr_bound: u64,
    ) -> Bits {
        let cur = fctx.current;
        // Cell granularity: word-frozen storages (every access provably
        // word-aligned, so lane 0 always) read whole target-word cells —
        // one address computation per W bytes instead of per byte; byte
        // cells everywhere else (and for dispatch candidates whose
        // storage never froze). Coherent because dispatch routes
        // candidates through this same function.
        let word = self.storage_is_word(storage);
        let cell_bytes = if word { self.word_bytes as usize } else { 1 };
        let cell_ty = if word { self.word_tid } else { self.byte_tid };
        let cell_bits = cell_bytes * 8;
        let n_cells = n_bits.div_ceil(cell_bits);
        let byte_base = self.byte_base_of(fctx, cur, base_addr_bits);
        let mut bits = Vec::with_capacity(n_bits);
        for cell_i in 0..n_cells as u64 {
            let addr = self.byte_addr(fctx, cur, &byte_base, cell_i * cell_bytes as u64, addr_bound);
            let cell_var = fctx.emit(
                cur,
                Value::Op(Stmt::StorageRead {
                    storage,
                    ty: cell_ty,
                    addr,
                }),
            );
            for bit_j in 0..cell_bits as u8 {
                let bit_idx = (cell_i as usize) * cell_bits + bit_j as usize;
                if bit_idx >= n_bits {
                    break;
                }
                let bit = fctx.emit(
                    cur,
                    Value::Op(Stmt::Shuffle {
                        result_bits: vec![(bit_j, cell_var)],
                        ty: self.bit_tid,
                    }),
                );
                bits.push(bit);
            }
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

    /// Write `val` to `storage` at `base_addr_bits`, one bit per address.
    fn stack_store_dynamic(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        storage: StorageId,
        ptr_bits0: ValueId,
        base_addr_bits: &Bits,
        val: &Bits,
        addr_bound: u64,
    ) {
        let cur = fctx.current;
        let word = self.storage_is_word(storage);
        let cell_bytes = if word { self.word_bytes as usize } else { 1 };
        let cell_ty = if word { self.word_tid } else { self.byte_tid };
        let cell_bits = cell_bytes * 8;
        let n_cells = val.len().div_ceil(cell_bits);
        let byte_base = self.byte_base_of(fctx, cur, base_addr_bits);
        for cell_i in 0..n_cells {
            let addr = self.byte_addr(fctx, cur, &byte_base, cell_i as u64 * cell_bytes as u64, addr_bound);
            let lo = cell_i * cell_bits;
            let have = (lo + cell_bits).min(val.len()) - lo;
            let mut parts: Vec<ValueId> = val[lo..lo + have].to_vec();
            if have < cell_bits {
                // Partial tail cell: word-frozen storages can see
                // sub-word accesses when an element is over-aligned
                // (e.g. an align-8 i32). Read-modify-write keeps the
                // old cell's high bits instead of clobbering them.
                let old = fctx.emit(
                    cur,
                    Value::Op(Stmt::StorageRead {
                        storage,
                        ty: cell_ty,
                        addr,
                    }),
                );
                for j in have..cell_bits {
                    let old_bit = fctx.emit(
                        cur,
                        Value::Op(Stmt::Shuffle {
                            result_bits: vec![(j as u8, old)],
                            ty: self.bit_tid,
                        }),
                    );
                    parts.push(old_bit);
                }
            }
            let cell_val = fctx.emit(
                cur,
                Value::Op(Stmt::Merge {
                    parts,
                    ty: cell_ty,
                }),
            );
            fctx.emit(
                cur,
                Value::Op(Stmt::StorageWrite {
                    storage,
                    src: cell_val,
                    ty: cell_ty,
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
            let addr = self.dynamic_addr(fctx, cur, base_offset_bits, byte_i, 0);
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
            let addr = self.dynamic_addr(fctx, cur, base_offset_bits, byte_i, 0);
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

    /// Conservative escape check for an `alloca`: does its address value
    /// (transitively through GEP instructions) stay within this function's
    /// loads and stores? Any other use — a call argument (other than the
    /// `llvm.lifetime.*`/`llvm.dbg.*` markers, which take the address
    /// without dereferencing or storing it), storing the pointer as a
    /// value, phi/select/return, ptrtoint — escapes. Unknown user shapes
    /// escape. Only non-escaping allocas may carve from the static pool:
    /// their address values never flow anywhere a runtime-dispatched load
    /// could dereference, so no dispatch ever needs to read their cells.
    /// True when EVERY load/store that transitively uses this alloca
    /// (through gep/bitcast chains) carries LLVM `align >= w_bytes` — such
    /// accesses never straddle a word boundary and always sit at lane 0,
    /// which is exactly the precondition for word-cell addressing. Opaque
    /// users (calls other than lifetime/dbg intrinsics, phis, anything
    /// unmodeled) fail closed to byte cells, keeping word-frozen storages
    /// coherent for the dispatch path (which calls the same
    /// stack_load/store_dynamic this flag switches).
    /// Recursively collect function-pointer constants out of a constant
    /// tree (aggregate initializers, constant expressions).
    fn collect_const_fns(
        &mut self,
        raw: inkwell::llvm_sys::prelude::LLVMValueRef,
        depth: usize,
        seen: &mut HashSet<usize>,
    ) {
        use inkwell::llvm_sys::core::{
            LLVMIsAConstantArray, LLVMIsAConstantExpr, LLVMIsAConstantStruct,
            LLVMIsAConstantVector, LLVMIsAFunction,
        };
        if depth > 6 || !seen.insert(raw as usize) {
            return;
        }
        if unsafe { LLVMIsAFunction(raw) }.is_null() {
            // Not a function: recurse through aggregates and expressions.
            let is_expr = !unsafe { LLVMIsAConstantExpr(raw) }.is_null();
            let is_agg = !unsafe { LLVMIsAConstantArray(raw) }.is_null()
                || !unsafe { LLVMIsAConstantStruct(raw) }.is_null()
                || !unsafe { LLVMIsAConstantVector(raw) }.is_null();
            if is_expr || is_agg {
                let n = unsafe { LLVMGetNumOperands(raw) };
                for i in 0..n {
                    let op = unsafe { LLVMGetOperand(raw, i as u32) };
                    if !op.is_null() {
                        self.collect_const_fns(op, depth + 1, seen);
                    }
                }
            }
            return;
        }
        let pv = unsafe { PointerValue::new(raw) };
        if !pv
            .as_value_ref()
            .is_null()
            && !self
                .fnptr_candidates
                .iter()
                .any(|c| c.as_value_ref() == raw)
        {
            self.fnptr_candidates.push(pv);
        }
    }

    /// Collect the module's function-pointer constants that are STORED
    /// somewhere (the members of fn-pointer tables like revm's
    /// `InstructionTable`). Walks each function value's uses; a Store whose
    /// value operand is the function pointer makes it a dispatch candidate.
    fn collect_fnptr_candidates(&mut self, llvm_module: &LlvmModule<'ctx>) {
        // Global-constant initializers: revm's `instruction_table()` at -O0
        // lowers to ONE `llvm.memcpy` from a global constant array of fn
        // pointers — the candidates live in initializers, not stores.
        for g in llvm_module.get_globals() {
            if let Some(init) = g.get_initializer() {
                let mut seen: HashSet<usize> = Default::default();
                self.collect_const_fns(init.as_value_ref(), 0, &mut seen);
            }
        }
        for f in llvm_module.get_functions() {
            let fptr = f.as_global_value().as_pointer_value();
            let vref = fptr.as_value_ref();
            let mut use_ref = unsafe { LLVMGetFirstUse(vref) };
            let mut is_candidate = false;
            while !use_ref.is_null() {
                let user = unsafe { LLVMGetUser(use_ref) };
                use_ref = unsafe { LLVMGetNextUse(use_ref) };
                if unsafe { LLVMIsAInstruction(user) }.is_null() {
                    continue;
                }
                let opcode = unsafe { InstructionValue::new(user) }.get_opcode();
                if opcode == InstructionOpcode::Store
                    && unsafe { LLVMGetOperand(user, 0) } == vref
                {
                    is_candidate = true;
                    break;
                }
            }
            if is_candidate
                && !self
                    .fnptr_candidates
                    .iter()
                    .any(|c| c.as_value_ref() == vref)
            {
                self.fnptr_candidates.push(fptr);
            }
        }
    }

    /// The effective cell granularity for a storage: word-celled iff it
    /// froze as word-aligned AND no interprocedural evidence (a specialized
    /// callee's unaligned access) has tainted it this fixpoint.
    fn storage_is_word(&self, storage: StorageId) -> bool {
        // VLVI_DISABLE_WORD_CELLS=1: byte cells everywhere. The word-freeze
        // fixpoint re-imports per taint, and its RSS ratchets across
        // passes; dispatch-heavy inputs (indirect calls) run byte-celled
        // until that cost is tuned.
        if std::env::var("VLVI_DISABLE_WORD_CELLS").is_ok() {
            return false;
        }
        self.storage_word.get(&storage).copied().unwrap_or(false)
            && !self.tainted_word.borrow().contains(&storage)
    }

    fn alloca_word_aligned(&self, alloc: InstructionValue<'ctx>, w_bytes: u64) -> bool {
        let mut work = vec![alloc.as_value_ref()];
        let mut seen: std::collections::HashSet<usize> = Default::default();
        while let Some(vref) = work.pop() {
            if !seen.insert(vref as usize) {
                continue;
            }
            let mut use_ref = unsafe { LLVMGetFirstUse(vref) };
            while !use_ref.is_null() {
                let user = unsafe { LLVMGetUser(use_ref) };
                use_ref = unsafe { LLVMGetNextUse(use_ref) };
                if unsafe { LLVMIsAInstruction(user) }.is_null() {
                    return false;
                }
                let instr = unsafe { InstructionValue::new(user) };
                match instr.get_opcode() {
                    InstructionOpcode::GetElementPtr
                    | InstructionOpcode::BitCast
                    | InstructionOpcode::AddrSpaceCast => {
                        if unsafe { LLVMGetOperand(user, 0) } == vref {
                            // Pointer-value flow: follow it.
                            work.push(user);
                        } else {
                            return false;
                        }
                    }
                    InstructionOpcode::Load => {
                        if unsafe { LLVMGetOperand(user, 0) } != vref {
                            return false;
                        }
                        if (instr.get_alignment().unwrap_or(1) as u64) < w_bytes {
                            return false;
                        }
                    }
                    InstructionOpcode::Store => {
                        // Store operands are (value, pointer). A store of
                        // the pointer VALUE (vref as operand 0) is value
                        // flow, not a memory access through it.
                        if unsafe { LLVMGetOperand(user, 1) } == vref {
                            if (instr.get_alignment().unwrap_or(1) as u64) < w_bytes {
                                return false;
                            }
                        }
                    }
                    InstructionOpcode::Phi
                    | InstructionOpcode::Select
                    | InstructionOpcode::Call
                    | InstructionOpcode::Return => {
                        // Pure pointer-value flow (a call argument, a phi
                        // arm, ...). The callee's actual accesses are
                        // separate LLVM instructions: any function that
                        // receives this storage checks its own accesses'
                        // alignment at the Load/Store arms (fail-closed
                        // there), so pointer flow needs no scan verdict.
                    }
                    _ => return false,
                }
            }
        }
        true
    }

    fn alloca_escapes(&self, alloc: InstructionValue<'ctx>) -> bool {
        let mut work = vec![alloc.as_value_ref()];
        let mut seen: std::collections::HashSet<usize> = Default::default();
        while let Some(vref) = work.pop() {
            if !seen.insert(vref as usize) {
                continue;
            }
            let mut use_ref = unsafe { LLVMGetFirstUse(vref) };
            while !use_ref.is_null() {
                let user = unsafe { LLVMGetUser(use_ref) };
                use_ref = unsafe { LLVMGetNextUse(use_ref) };
                if unsafe { LLVMIsAInstruction(user) }.is_null() {
                    // Allocas are not constants, so a constant-expression
                    // user cannot occur; treat anything unexpected as an
                    // escape.
                    return true;
                }
                let opcode = unsafe { InstructionValue::new(user) }.get_opcode();
                match opcode {
                    InstructionOpcode::GetElementPtr => {
                        if unsafe { LLVMGetOperand(user, 0) } == vref {
                            work.push(user);
                        } else {
                            return true;
                        }
                    }
                    InstructionOpcode::Load => {
                        if unsafe { LLVMGetOperand(user, 0) } != vref {
                            return true;
                        }
                    }
                    InstructionOpcode::Store => {
                        // Store operands are (value, pointer).
                        if unsafe { LLVMGetOperand(user, 1) } != vref {
                            return true;
                        }
                    }
                    InstructionOpcode::Call => {
                        // Call operands are the arguments followed by the
                        // callee itself.
                        let n = unsafe { LLVMGetNumOperands(user) };
                        let callee_ref = unsafe { LLVMGetOperand(user, (n - 1) as u32) };
                        let callee = unsafe { FunctionValue::new(callee_ref) };
                        let name = callee
                            .map(|f| f.get_name().to_string_lossy().into_owned())
                            .unwrap_or_default();
                        if name.starts_with("llvm.lifetime") || name.starts_with("llvm.dbg") {
                            continue;
                        }
                        return true;
                    }
                    _ => return true,
                }
            }
        }
        false
    }

    /// Statically classify a pointer-typed call argument for call-site
    /// specialization (see `Prov`). Tracked stack pointers and globals
    /// name their storage; bare constant globals (and constant-expression
    /// GEP chains rooted at one) name the pool; everything else — pointers
    /// loaded at runtime, merged values, unseeded parameters of a generic
    /// body — stays `Any`.
    fn arg_provenance(&mut self, fctx: &FuncCtx<'ctx>, arg: BasicValueEnum<'ctx>) -> Prov {
        let BasicValueEnum::PointerValue(pv) = arg else {
            return Prov::Any;
        };
        if let Some(sp) = fctx.stack_slot_of.get(&pv.as_any_value_enum()) {
            return Prov::Stack(sp.storage);
        }
        if let Some(gp) = fctx.global_ptr_of.get(&pv.as_any_value_enum()) {
            return Prov::Global(gp.storage());
        }
        let raw = pv.as_value_ref();
        let is_global = !unsafe { LLVMIsAGlobalVariable(raw) }.is_null();
        let is_const_gep = !unsafe { LLVMIsAConstantExpr(raw) }.is_null()
            && unsafe { LLVMGetConstOpcode(raw) } == LLVMOpcode::LLVMGetElementPtr;
        if !is_global && !is_const_gep {
            return Prov::Any;
        }
        match self.storage_for_with_offset(pv) {
            Ok((storage, _)) => Prov::Global(storage),
            Err(_) => Prov::Any,
        }
    }

    /// Byte-addressed globals. Per-alloca data stacks and their stack-pointer
    /// cells are allocated from the same counter and are not globals.
    fn dispatch_candidates(&self) -> IResult<Vec<StorageId>> {
        let skip: HashSet<StorageId> = self
            .alloca_data
            .iter()
            .copied()
            .chain(self.sp_globals.iter().copied())
            .collect();
        let candidates: Vec<StorageId> = (GLOBAL_STORAGE_BASE..self.storage_alloc.next)
            .map(StorageId)
            .filter(|id| !skip.contains(id))
            .collect();
        if candidates.len() > self.max_dispatch_candidates {
            return Err(ImportError::Unsupported(format!(
                "{} candidate globals exceeds the {}-candidate limit for \
                 runtime pointer dispatch",
                candidates.len(),
                self.max_dispatch_candidates
            )));
        }
        Ok(candidates)
    }

    /// Per-candidate `gate AND (id_bits == candidate's StorageId)`.
    fn dispatch_id_matches(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        block: BlockId,
        ptr_bits: &Bits,
        candidates: &[StorageId],
        gate: ValueId,
    ) -> Vec<ValueId> {
        let global_addr_bits = self.global_addr_bits();
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
                c.bc_and(gate, id_eq)
            })
            .collect()
    }

    /// Global candidates: tag bit AND storage-id equality.
    fn dispatch_matches(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        block: BlockId,
        ptr_bits: &Bits,
        candidates: &[StorageId],
    ) -> Vec<ValueId> {
        let tag_bit = ptr_bits[self.pointer_bits - 1];
        self.dispatch_id_matches(fctx, block, ptr_bits, candidates, tag_bit)
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
    /// constant). Reads every per-alloca data stack (tag 0, bit address) and
    /// every module global (tag 1, byte address) and muxes the one
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
        let allocas = self.alloca_data.clone();
        let candidates = self.dispatch_candidates()?;
        let cur = fctx.current;
        let offset_bits = self.dispatch_global_addr_bits(fctx, cur, ptr_bits);
        let tag_bit = ptr_bits[self.pointer_bits - 1];
        let not_tag = {
            let mut c = Ctx {
                fctx,
                bit_tid: self.bit_tid,
                block: cur,
            };
            c.bc_not(tag_bit)
        };
        let stack_matches = self.dispatch_id_matches(fctx, cur, ptr_bits, &allocas, not_tag);
        let zero = self.bc_const_at(fctx, cur, false);
        let mut result = vec![zero; n_bits];
        for (candidate_idx, &sid) in allocas.iter().enumerate() {
            let loaded = self.stack_load_dynamic(
                fctx,
                sid,
                ptr_bits[0],
                &offset_bits,
                pointee_ty,
                n_bits,
                self.alloca_sizes[candidate_idx],
            );
            result = mux_bits(
                self,
                fctx,
                cur,
                stack_matches[candidate_idx],
                &loaded,
                &result,
            );
        }

        let matches = self.dispatch_matches(fctx, cur, ptr_bits, &candidates);
        for (candidate_idx, &sid) in candidates.iter().enumerate() {
            let global_bits = self.mem_load_dynamic(fctx, sid, &offset_bits, n_bytes);
            result = mux_bits(
                self,
                fctx,
                cur,
                matches[candidate_idx],
                &global_bits,
                &result,
            );
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
        let allocas = self.alloca_data.clone();
        let candidates = self.dispatch_candidates()?;
        let cur = fctx.current;
        let offset_bits = self.dispatch_global_addr_bits(fctx, cur, ptr_bits);
        let tag_bit = ptr_bits[self.pointer_bits - 1];
        let not_tag = {
            let mut c = Ctx {
                fctx,
                bit_tid: self.bit_tid,
                block: cur,
            };
            c.bc_not(tag_bit)
        };
        let stack_matches = self.dispatch_id_matches(fctx, cur, ptr_bits, &allocas, not_tag);
        let bit_tid = self.bit_tid;
        for (candidate_idx, &sid) in allocas.iter().enumerate() {
            let bound = self.alloca_sizes[candidate_idx];
            let old = self.stack_load_dynamic(
                fctx,
                sid,
                ptr_bits[0],
                &offset_bits,
                bit_tid,
                n_bits,
                bound,
            );
            let new_val = mux_bits(self, fctx, cur, stack_matches[candidate_idx], val, &old);
            self.stack_store_dynamic(fctx, sid, ptr_bits[0], &offset_bits, &new_val, bound);
        }

        let matches = self.dispatch_matches(fctx, cur, ptr_bits, &candidates);
        for (candidate_idx, &sid) in candidates.iter().enumerate() {
            let old = self.mem_load_dynamic(fctx, sid, &offset_bits, n_bytes);
            let new_val = mux_bits(self, fctx, cur, matches[candidate_idx], val, &old);
            self.mem_store_dynamic(fctx, sid, &offset_bits, &new_val, n_bytes);
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
                if !self.lower_reachable_unreachable_to_zeros {
                    return Err(ImportError::Unsupported("reachable unreachable".into()));
                }
                // Opt-in lowering for compiler-proven-dead arms: return
                // zeros sized to the enclosing function's return width.
                // Runtime-unreachable by construction. The all-zero vector
                // is identical for every terminator of a given width —
                // emit once, share the ValueIds (a wide-return function
                // otherwise pays return_bits Const nodes PER BLOCK).
                let width = fctx.return_bits;
                let values = if let Some(shared) = fctx.zero_returns.get(&width) {
                    shared.clone()
                } else {
                    let shared: Vec<ValueId> = (0..width)
                        .map(|_| self.bc_const_at(fctx, fctx.current, false))
                        .collect();
                    fctx.zero_returns.insert(width, shared.clone());
                    shared
                };
                Terminator::Return { values }
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

    /// Indirect call: the callee operand's bits are a runtime select over
    /// the module's stored function-pointer encodings. Build an
    /// equality-selector cascade (the same GF(2) shape as
    /// `translate_switch`) mapping those bits to a dense case index, split
    /// the current block on a positional `Terminator::Table` with one call
    /// block per candidate, and resume translation in a shared
    /// continuation block. Void callees only (revm's instruction
    /// functions are all void); non-void fails closed.
    fn translate_indirect_call(
        &mut self,
        fctx: &mut FuncCtx<'ctx>,
        instr: InstructionValue<'ctx>,
    ) -> IResult<Vec<(FunctionValue<'ctx>, Vec<Prov>)>> {
        // Non-void indirect calls would need per-candidate output
        // selection; revm's table callees are all void.
        if instr
            .get_type()
            .try_into()
            .map(|t: BasicTypeEnum| {
                matches!(
                    t,
                    BasicTypeEnum::IntType(_) | BasicTypeEnum::PointerType(_)
                )
            })
            .unwrap_or(false)
        {
            return Err(ImportError::Unsupported(
                "non-void indirect call is unsupported".into(),
            ));
        }
        if self.fnptr_candidates.is_empty() {
            return Err(ImportError::Unsupported(
                "indirect call with no stored function-pointer candidates".into(),
            ));
        }
        if std::env::var("VLVI_IMPORT_NEXT").is_ok() {
            eprintln!(
                "[indirect] enter site ({} candidates, values so far {})",
                self.fnptr_candidates.len(),
                fctx.values.len()
            );
        }
        let cur = fctx.current;
        let n_ops = instr.get_num_operands();
        // Call operands are [arguments..., callee]. The callee's runtime
        // bits come from the load's cache (a dispatch over the table
        // storage returns exactly the encodings stored, so the selector's
        // equality keys below are consistent by construction).
        let callee_ref = unsafe { LLVMGetOperand(instr.as_value_ref(), (n_ops - 1) as u32) };
        let callee_val = unsafe { BasicValueEnum::new(callee_ref) };
        let callee_bits = self.value_bits(fctx, callee_val)?;
        // Argument conversions land in the pre-split block.
        let args = self.call_arg_bits(fctx, instr)?;
        // Provenance per pointer argument (mirrors the direct-call arm,
        // driven by operand types instead of a known signature).
        let prov: Vec<Prov> = (0..(n_ops - 1) as u32)
            .map(|i| {
                let arg_ref = unsafe { LLVMGetOperand(instr.as_value_ref(), i) };
                if arg_ref.is_null() {
                    return Prov::Any;
                }
                let arg = unsafe { BasicValueEnum::new(arg_ref) };
                if !matches!(arg.get_type(), BasicTypeEnum::PointerType(_)) {
                    Prov::Any
                } else {
                    self.arg_provenance(fctx, arg)
                }
            })
            .collect();
        let mut candidate_encodings: Vec<(FunctionValue<'ctx>, Vec<ValueId>)> = Vec::new();
        // Snapshot: the encoding emission below needs &mut self. Narrow by
        // function type: a site only dispatches to callees with its exact
        // signature (revm's table site keeps its 256; a closure-pointer
        // site keeps a handful instead of importing every table member).
        // LLVM types are uniqued per context: signature identity is raw
        // pointer equality of the function types (wasm32's opaque `ptr`
        // carries no signature — it lives on the call site and the
        // function global).
        let site_fn_ty =
            unsafe { inkwell::llvm_sys::core::LLVMGetCalledFunctionType(instr.as_value_ref()) };
        let candidates: Vec<PointerValue<'ctx>> = self
            .fnptr_candidates
            .clone()
            .into_iter()
            .filter(|cand| {
                let cand_ty =
                    unsafe { inkwell::llvm_sys::core::LLVMGlobalGetValueType(cand.as_value_ref()) };
                !cand_ty.is_null() && !site_fn_ty.is_null() && cand_ty == site_fn_ty
            })
            .collect();
        // Selector cascade: default = candidates.len() (out of bounds ->
        // default target); the last matching candidate wins, mirroring
        // `translate_switch`'s reverse iteration.
        let n = candidates.len();
        let sel_width = bits_for_max_value(n);
        let mut selector: Vec<ValueId> = {
            let mut c = Ctx {
                fctx,
                bit_tid: self.bit_tid,
                block: cur,
            };
            (0..sel_width)
                .map(|b| c.bc_const((n >> b) & 1 != 0))
                .collect()
        };
        for (case_i, cand) in candidates.iter().enumerate() {
            let cand_bits = self.ptr_value_bits(fctx, cur, *cand)?;
            let callee_fn = unsafe { FunctionValue::new(cand.as_value_ref()) };
            let Some(callee_fn) = callee_fn else {
                return Err(ImportError::Unsupported(
                    "fn-pointer candidate is not a function".into(),
                ));
            };
            candidate_encodings.push((callee_fn, cand_bits));
        }
        for (case_i, (_, cand_bits)) in candidate_encodings.iter().enumerate().rev() {
            let matched = {
                let mut c = Ctx {
                    fctx,
                    bit_tid: self.bit_tid,
                    block: cur,
                };
                circuits::bc_eq(&mut c, &callee_bits, cand_bits)
            };
            let case_idx_bits: Vec<ValueId> = {
                let mut c = Ctx {
                    fctx,
                    bit_tid: self.bit_tid,
                    block: cur,
                };
                (0..sel_width)
                    .map(|b| c.bc_const((case_i >> b) & 1 != 0))
                    .collect()
            };
            selector = {
                let mut c = Ctx {
                    fctx,
                    bit_tid: self.bit_tid,
                    block: cur,
                };
                circuits::bc_select_vec(&mut c, matched, &case_idx_bits, &selector)
            };
        }
        let selector_val = self.compose_address(fctx, cur, &selector);
        // Split: one call block per candidate, all resuming at the
        // continuation block; translation continues there.
        let cont = fctx.append_block();
        let mut targets: Vec<Target> = Vec::with_capacity(n);
        let mut called: Vec<(FunctionValue<'ctx>, Vec<Prov>)> = Vec::with_capacity(n);
        for (callee_fn, _) in &candidate_encodings {
            let case_b = fctx.append_block();
            let callee_id = self.func_id(*callee_fn, &prov)?;
            called.push((*callee_fn, prov.clone()));
            fctx.emit(
                case_b,
                Value::Call {
                    func: callee_id,
                    args: args.clone(),
                },
            );
            fctx.terminators[case_b.0] = Some(Terminator::Jump(Target {
                block: cont,
                args: vec![],
                reentry: None,
            }));
            targets.push(Target {
                block: case_b,
                args: vec![],
                reentry: None,
            });
        }
        let n_cases = targets.len();
        fctx.terminators[cur.0] = Some(Terminator::Table {
            index: selector_val,
            targets,
            default_target: Target {
                block: cont,
                args: vec![],
                reentry: None,
            },
        });
        fctx.current = cont;
        if std::env::var("VLVI_IMPORT_NEXT").is_ok() {
            eprintln!(
                "[indirect] exit site ({} cases, values now {})",
                n_cases,
                fctx.values.len()
            );
        }
        Ok(called)
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

fn mux_bits<'ctx>(
    importer: &mut Importer<'ctx>,
    fctx: &mut FuncCtx<'ctx>,
    block: BlockId,
    cond: ValueId,
    taken: &[ValueId],
    fallback: &[ValueId],
) -> Bits {
    let mut c = Ctx {
        fctx,
        bit_tid: importer.bit_tid,
        block,
    };
    taken
        .iter()
        .zip(fallback.iter())
        .map(|(&bit, &keep)| c.bc_select(cond, bit, keep))
        .collect()
}

fn intrinsic_ranges_overlap(
    dest: &IntrinsicPointer,
    src: &IntrinsicPointer,
    n_bytes: usize,
) -> IResult<bool> {
    match (dest, src) {
        (IntrinsicPointer::Stack { ptr: dest, .. }, IntrinsicPointer::Stack { ptr: src, .. })
            if dest.storage == src.storage
                && dest.const_offset.is_some()
                && src.const_offset.is_some() =>
        {
            let (dest_start, dest_end) = dest.intrinsic_range(n_bytes)?;
            let (src_start, src_end) = src.intrinsic_range(n_bytes)?;
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
    aggregate_fields: HashMap<AnyValueEnum<'ctx>, Vec<Option<Bits>>>,
    current: BlockId,
    /// Stack-pointer globals this function's `alloca` sites bump. Exit
    /// restores the values read from these cells at entry.
    sp_globals: Vec<StorageId>,
    /// Pointer-typed LLVM values (alloca results, or a GEP off one) whose
    /// data stack and absolute bit address are known. `cache` holds the
    /// tagged encoding used when the pointer escapes; this map holds the
    /// full address used by a known-provenance load or store.
    stack_slot_of: HashMap<AnyValueEnum<'ctx>, StackPtr>,
    /// Pointer-typed LLVM values that are the result of a constant-index
    /// `getelementptr` *instruction* off a global (directly, or chained off
    /// another tracked entry here). See `GlobalPointer`.
    global_ptr_of: HashMap<AnyValueEnum<'ctx>, GlobalPtr>,
    /// The enclosing function's flattened return width (bits), for
    /// zero-return lowering of reachable `unreachable` instructions.
    return_bits: usize,
    /// Hash-consed bit constants (value -> ValueId), hoisted to the entry
    /// block. See `bc_const_at`.
    const_cache: HashMap<u128, ValueId>,
    /// Pointer-VALUE provenance through memory: slot -> the tracked
    /// pointer stored there. -O0 spills every parameter
    /// (`store ptr %ctx, %spill` ... `%r = load %spill`), and a bare load
    /// loses tracked provenance, degenerating every downstream access into
    /// full storage dispatch. Pairs tracked store/load slots so spilled
    /// pointers stay tracked.
    slot_ptr_of: HashMap<inkwell::values::AnyValueEnum<'ctx>, StackPtr>,
    /// Shared all-zero return vectors per width: the
    /// reachable-`unreachable` zero-return lowering emits the same Const
    /// vector for every such terminator in a function — a wide-return
    /// function (158k-bit wasm multi-value returns) paid it PER BLOCK.
    zero_returns: HashMap<usize, Vec<ValueId>>,
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
            sp_globals: Vec::new(),
            stack_slot_of: HashMap::new(),
            global_ptr_of: HashMap::new(),
            return_bits: 0,
            const_cache: HashMap::new(),
            slot_ptr_of: HashMap::new(),
            zero_returns: HashMap::new(),
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

    fn bc_poly(&mut self, coeffs: PolyCoeffs<ValueId>, constant: u128) -> ValueId {
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
