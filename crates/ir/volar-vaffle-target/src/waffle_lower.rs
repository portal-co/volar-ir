// @reliability: experimental
// @ai: assisted
//! WAFFLE `FunctionBody` → VAFFLE lowering.
//!
//! Handles the subset relevant to program-related cryptography:
//!
//! **Types**: `I32`, `I64` only.  Any function or block parameter of type
//! `F32`, `F64`, `V128`, or any reference/heap type yields `UnsupportedOp`.
//!
//! **Operators** (supported):
//! - Constants: `I32Const`, `I64Const`
//! - Arithmetic: `I32/I64` add, sub, mul, divS, divU, and, or, xor, shl, shrS, shrU,
//!   remU, remS, clz, ctz, popcnt, rotl, rotr
//! - Sign-extend: `I32Extend8S`, `I32Extend16S`, `I64Extend8S`, `I64Extend16S`, `I64Extend32S`
//! - Comparisons: `I32/I64` eqz, eq, ne, ltS/U, gtS/U, leS/U, geS/U
//! - Conversions: `I32WrapI64`, `I64ExtendI32S`, `I64ExtendI32U`
//! - Select: `Select`, `TypedSelect` (I32 non-zero test via OR-reduce)
//! - Direct calls: `Call { function_index }` — multi-return supported; mutable globals
//!   are threaded as extra args/returns at every call site
//! - Nop: `Nop`
//! - Memory loads: `I32Load`, `I64Load`, `I32Load8U/S`, `I32Load16U/S`,
//!   `I64Load8U/S`, `I64Load16U/S`, `I64Load32U/S` (byte-addressed storage)
//! - Memory stores: `I32Store`, `I64Store`, `I32Store8/16`,
//!   `I64Store8/16/32`
//! - Memory stubs: `MemorySize` (always 0), `MemoryGrow` (always -1)
//! - Globals: `GlobalGet` and `GlobalSet` for mutable I32/I64 globals — threaded
//!   as extra function parameters and return values.  Immutable globals return
//!   their compile-time constant.
//!
//! **Operators** (not supported — returns `UnsupportedOp`):
//! - All F32/F64 ops and float memory ops
//! - All V128/SIMD ops
//! - All atomic/threads ops
//! - Trunc-sat conversions
//! - `CallIndirect`, `CallRef`
//! - Tables: `TableGet`, `TableSet`, `TableGrow`, `TableSize`
//! - Bulk memory: `MemoryCopy`, `MemoryFill`, `MemoryInit`, `DataDrop`
//! - Reference types: `RefNull`, `RefIsNull`, `RefFunc`
//! - GC proposal operators
//! - `Unreachable` operator (distinct from `Terminator::Unreachable`)
//!
//! **Terminators** (supported):
//! - `Br`, `CondBr`, `Return`, `ReturnCall`
//! - `Unreachable`, `UB`, `None` — stubbed as a return of zero
//!
//! **Terminators** (not supported — returns `UnsupportedOp`):
//! - `Select` (br_table)
//! - `ReturnCallIndirect`, `ReturnCallRef`
//!
//! All integer values are bit-decomposed via `BitCircuitBuilder`, sharing
//! the same GF(2) Poly circuits as `VolarIrTarget`.
//!
//! # Errors
//! `UnsupportedOp` is returned for any unhandled op, type, or terminator.
//! The module-level helper [`lower_waffle_module`] skips those functions and
//! collects errors rather than panicking.
//!
//! Opt-in vc-spec tagging: [`crate::lower_waffle_module_with_vc`].

use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    vec,
    vec::Vec,
};

use portal_pc_waffle_ir::{
    Func,
    FuncDecl,
    FunctionBody,
    MemoryArg,
    Module as WModule,
    Operator,
    SignatureData,
    Terminator,
    Type as WType,
    Value as WValue,
    ValueDef,
    entity::EntityRef, // for .index() on Func/Block/etc.
};

use volar_ir_common::{Constant, PreInitSegment, StorageId};
use volar_lir::circuits::{
    StorageEmitter, bc_clz, bc_ctz, bc_popcnt, bc_rotl, bc_rotr, bc_srem, bc_urem,
};
use volar_lir::{BitCircuitBuilder, BranchTarget, IcmpPred, LirTarget, LirType};

use crate::import_config::{WaffleImportConfig, WaffleImportKind};
use crate::target::{VaffleBlock, VaffleTarget, VaffleValue, bits_for_lir_type};
use crate::vc::{
    RevealedBits, VcArtifact, VcConfig, VcIds, VcLoweringState, VcVisibility, VciOp,
    build_vc_regions, resolve_call_func_name, validate_vc_regions, vci_op_for_func,
};
use vaffle::ValueId;

// ============================================================================
// Error
// ============================================================================

#[derive(Debug, Clone)]
pub struct UnsupportedOp(pub String);

impl core::fmt::Display for UnsupportedOp {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "unsupported WAFFLE op: {}", self.0)
    }
}

// ============================================================================
// Type mapping
// ============================================================================

fn waffle_ty(ty: WType) -> Result<LirType, UnsupportedOp> {
    match ty {
        WType::I32 | WType::F32 => Ok(LirType::U32),
        WType::I64 | WType::F64 => Ok(LirType::U64),
        other => Err(UnsupportedOp(alloc::format!("{other:?}"))),
    }
}

// ============================================================================
// Public entry points
// ============================================================================

/// Whether WAFFLE lowering consumes its source-neutral WSMM custom section.
///
/// `RespectUnstable` is the v0.1 opt-in: it allows layout declarations to
/// reduce emitted storage immediately, while keeping authentication a separate
/// caller choice.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum WasmMetadataMode {
    Ignore,
    #[default]
    RespectUnstable,
}

/// Lower all function bodies in a WAFFLE module into `target`, skipping
/// unsupported functions.  Returns a list of (name, error) for skipped functions.
///
/// `config` maps WAFFLE import names to oracle/action declarations. Matching
/// imports are pre-registered in `target.module.oracles` / `target.module.actions`
/// and routed through the oracle/action calling convention at every call site.
/// Pass `&WaffleImportConfig::default()` for the original behaviour.
pub fn lower_waffle_module(
    wasm: &WModule,
    target: &mut VaffleTarget,
    config: &WaffleImportConfig,
) -> Vec<(String, UnsupportedOp)> {
    lower_waffle_module_with_metadata(wasm, target, config, WasmMetadataMode::RespectUnstable)
}

/// Opt-in vc-spec lowering: tagged arguments, `mem_write`/`mem_reveal`, and
/// VCI `vc.reveal_*` identity-with-public-side. Default [`lower_waffle_module`]
/// is unchanged. The returned [`VcArtifact`] carries interned sides and a
/// [`volar_ir::typed_gadget::TypedRegionTable`].
pub fn lower_waffle_module_with_vc(
    wasm: &WModule,
    target: &mut VaffleTarget,
    config: &WaffleImportConfig,
    vc: &VcConfig,
) -> (Vec<(String, UnsupportedOp)>, VcArtifact) {
    let ids = VcIds::intern();
    let mut calls = BTreeMap::new();
    for (key, args) in &vc.calls {
        calls.insert(resolve_call_func_name(wasm, key), args.clone());
    }
    target.vc = Some(VcLoweringState::new(ids, calls));

    let errors =
        lower_waffle_module_with_metadata(wasm, target, config, WasmMetadataMode::RespectUnstable);
    apply_vc_public_mem_writes(target, vc);

    let byte_tid = target.byte_tid();
    let state = target.vc.take().expect("vc session");
    let regions = build_vc_regions(wasm, &target.module, vc, &state.ids, &state.calls, byte_tid);
    let _ = validate_vc_regions(&regions, &target.module, vc);
    let artifact = VcArtifact {
        regions,
        handler: state.ids.handler(),
        sides: state.ids.sides,
    };
    (errors, artifact)
}

fn apply_vc_public_mem_writes(target: &mut VaffleTarget, vc: &VcConfig) {
    let byte_tid = target.byte_tid();
    for w in &vc.mem_writes {
        if w.visibility != VcVisibility::Public {
            continue;
        }
        let Some(bytes) = w.bytes.as_ref() else {
            continue;
        };
        if bytes.is_empty() {
            continue;
        }
        target.module.pre_init.push(PreInitSegment {
            storage: StorageId::memory(w.memory),
            ty: byte_tid,
            offset: w.offset as usize,
            data: bytes
                .iter()
                .map(|&b| Constant {
                    hi: 0,
                    lo: b as u128,
                })
                .collect(),
        });
    }
}

fn configured_external_type(
    target: &mut VaffleTarget,
    import_name: &str,
    ty: WType,
) -> Result<volar_ir_common::TypeId, UnsupportedOp> {
    match ty {
        WType::I32 | WType::I64 => {
            Ok(target
                .lir_type_to_tid(&waffle_ty(ty).expect("scalar integer WAFFLE type maps to LIR")))
        }
        _ => Err(UnsupportedOp(alloc::format!(
            "configured external `{import_name}` requires scalar integer parameters and results"
        ))),
    }
}

/// Validate and register one explicitly configured imported external.
///
/// This happens before lowering any body so malformed registry entries cannot
/// become partial declarations or trigger slice indexing during call lowering.
fn register_configured_external(
    target: &mut VaffleTarget,
    wasm: &WModule,
    sig: portal_pc_waffle_ir::Signature,
    import_name: &str,
    kind: &WaffleImportKind,
) -> Result<(), UnsupportedOp> {
    let sig_data = &wasm.signatures[sig];
    let (wasm_params, wasm_results) = match sig_data {
        portal_pc_waffle_ir::SignatureData::Func {
            params, returns, ..
        } => (params.as_slice(), returns.as_slice()),
        _ => {
            return Err(UnsupportedOp(alloc::format!(
                "configured external `{import_name}` must have a function signature"
            )));
        }
    };
    let results: Vec<_> = wasm_results
        .iter()
        .copied()
        .map(|ty| configured_external_type(target, import_name, ty))
        .collect::<Result<_, _>>()?;
    if results.is_empty() {
        return Err(UnsupportedOp(alloc::format!(
            "configured external `{import_name}` must return at least one scalar integer"
        )));
    }
    match kind {
        WaffleImportKind::Oracle {
            name, execution, ..
        } => {
            let params = wasm_params
                .iter()
                .copied()
                .map(|ty| configured_external_type(target, import_name, ty))
                .collect::<Result<Vec<_>, _>>()?;
            if let Some(existing) = target.module.oracles.iter().find(|decl| decl.name == *name) {
                if existing.params != params
                    || existing.results != results
                    || existing.execution != *execution
                {
                    return Err(UnsupportedOp(alloc::format!(
                        "configured oracle `{name}` has inconsistent declarations"
                    )));
                }
            } else {
                target.register_oracle(volar_ir_common::OracleDecl {
                    name: name.clone(),
                    params,
                    results,
                    execution: *execution,
                });
            }
        }
        WaffleImportKind::Action {
            name,
            execution,
            n_args,
            ..
        } => {
            let expected = n_args
                .checked_add(1)
                .and_then(|count| count.checked_add(wasm_results.len()))
                .ok_or_else(|| {
                    UnsupportedOp(alloc::format!(
                        "configured action `{import_name}` argument count overflows its ABI"
                    ))
                })?;
            if wasm_params.len() != expected {
                return Err(UnsupportedOp(alloc::format!(
                    "configured action `{import_name}` has an incompatible parameter count"
                )));
            }
            if wasm_params[0] != WType::I32 {
                return Err(UnsupportedOp(alloc::format!(
                    "configured action `{import_name}` guard must be i32"
                )));
            }
            let fallback_count = wasm_params.len() - 1 - n_args;
            if fallback_count != wasm_results.len() {
                return Err(UnsupportedOp(alloc::format!(
                    "configured action `{import_name}` must have one fallback per result"
                )));
            }
            for (fallback, result) in wasm_params[1 + n_args..].iter().zip(wasm_results) {
                if fallback != result {
                    return Err(UnsupportedOp(alloc::format!(
                        "configured action `{import_name}` fallback types must match result types"
                    )));
                }
            }
            let params = wasm_params[1..1 + n_args]
                .iter()
                .copied()
                .map(|ty| configured_external_type(target, import_name, ty))
                .collect::<Result<Vec<_>, _>>()?;
            if let Some(existing) = target.module.actions.iter().find(|decl| decl.name == *name) {
                if existing.params != params
                    || existing.results != results
                    || existing.execution != *execution
                {
                    return Err(UnsupportedOp(alloc::format!(
                        "configured action `{name}` has inconsistent declarations"
                    )));
                }
            } else {
                target.register_action(volar_ir_common::ActionDecl {
                    name: name.clone(),
                    params,
                    results,
                    execution: *execution,
                });
            }
        }
    }
    Ok(())
}

/// As [`lower_waffle_module`], with an explicit source-neutral metadata mode.
pub fn lower_waffle_module_with_metadata(
    wasm: &WModule,
    target: &mut VaffleTarget,
    config: &WaffleImportConfig,
    metadata_mode: WasmMetadataMode,
) -> Vec<(String, UnsupportedOp)> {
    let unused_ranges = if metadata_mode == WasmMetadataMode::RespectUnstable {
        wasm.wsmm_manifest()
            .ok()
            .flatten()
            .map(unused_memory_ranges)
            .unwrap_or_default()
    } else {
        BTreeMap::new()
    };
    // Pre-register OracleDecl / ActionDecl for imports named in config.
    // Real-WASM imports carry their names in the module's import table
    // ("<module>.<field>"), not in `FuncDecl::Import`'s (empty) name field —
    // resolve through `waffle_import_func_names`.
    let import_names = waffle_import_func_names(wasm);
    // Registration must be atomic at the declaration-table level: an invalid
    // later mapping cannot leave an earlier external usable in a target whose
    // lowering returned errors.
    let initial_oracles = target.module.oracles.len();
    let initial_actions = target.module.actions.len();
    let mut errors = Vec::new();
    for import_name in config.imports.keys() {
        if !import_names.values().any(|name| name == import_name) {
            errors.push((
                import_name.clone(),
                UnsupportedOp(alloc::format!(
                    "configured external `{import_name}` is not imported by the WASM module"
                )),
            ));
        }
    }
    if !errors.is_empty() {
        return errors;
    }
    for (func_ref, decl) in wasm.funcs.entries() {
        if let FuncDecl::Import(sig, decl_name) = decl {
            let import_name = import_names.get(&func_ref.index()).unwrap_or(decl_name);
            let Some(kind) = config.imports.get(import_name) else {
                continue;
            };
            if let Err(error) = register_configured_external(target, wasm, *sig, import_name, kind)
            {
                errors.push((import_name.clone(), error));
            }
        }
    }
    if !errors.is_empty() {
        target.module.oracles.truncate(initial_oracles);
        target.module.actions.truncate(initial_actions);
        return errors;
    }

    // The compatibility materializer deliberately resolves every non-import
    // function through the same single-function lazy boundary.
    for (func_ref, decl) in wasm.funcs.entries() {
        if !matches!(decl, FuncDecl::Import(..)) {
            let name = decl.name().to_string();
            if let Err(error) = lower_waffle_function_lazy(wasm, func_ref, target, config) {
                errors.push((name, error));
            }
        }
    }

    // Collect WASM active data-segment pre-initialisations.
    // WASM linear memories use 8-bit byte cells, addressed by
    // `mem_load_bytes`/`mem_store_bytes`'s own `StorageId::memory(..)` +
    // `byte_tid()` (interns `IrType::Vec(8, Bit)`) -- pre_init segments
    // must be typed identically, or every runtime StorageRead permanently
    // misses them (the interpreter's storage map is keyed by
    // `(StorageId, TypeId, addr)`; a `TypeId` mismatch alone makes a
    // correctly-populated entry unreachable, silently reading back as
    // the default/zero value). Previously used `lir_type_to_tid(U8)`,
    // which interns the *structurally different* `IrType::Primitive(_8)`
    // -- a distinct TypeTable entry despite representing the same "one
    // byte" concept, since `TypeTable::intern` interns structurally.
    let byte_tid = target.byte_tid();
    for (mem_ref, mem_data) in wasm.memories.entries() {
        let storage = StorageId::memory(mem_ref.index() as u32);
        for seg in &mem_data.segments {
            // Under the explicit unstable manifest mode, an entire segment in
            // a declared-unused range has no represented storage to initialise.
            // The `Ignore` mode and malformed/absent manifests retain the
            // legacy complete materialization path.
            if unused_ranges
                .get(&(mem_ref.index() as u32))
                .is_some_and(|ranges| {
                    ranges.iter().any(|&(start, end)| {
                        seg.offset >= start && seg.offset.saturating_add(seg.data.len()) <= end
                    })
                })
            {
                continue;
            }
            target.module.pre_init.push(PreInitSegment {
                storage,
                ty: byte_tid,
                offset: seg.offset,
                data: seg
                    .data
                    .iter()
                    .map(|&b| Constant {
                        hi: 0,
                        lo: b as u128,
                    })
                    .collect(),
            });
        }
    }

    errors
}

fn unused_memory_ranges(manifest: wax_meta::Manifest) -> BTreeMap<u32, Vec<(usize, usize)>> {
    let mut ranges = BTreeMap::new();
    for (key, value) in manifest.entries() {
        let Some(index) = key
            .strip_prefix("memory/")
            .and_then(|tail| tail.strip_suffix("/unused"))
        else {
            continue;
        };
        let Ok(memory) = index.parse::<u32>() else {
            continue;
        };
        let wax_meta::Value::List(items) = value else {
            continue;
        };
        let mut parsed = Vec::new();
        for item in items {
            let wax_meta::Value::Map(fields) = item else {
                continue;
            };
            let (Some(wax_meta::Value::U64(start)), Some(wax_meta::Value::U64(length))) =
                (fields.get("start"), fields.get("length"))
            else {
                continue;
            };
            let Ok(start) = usize::try_from(*start) else {
                continue;
            };
            let Ok(length) = usize::try_from(*length) else {
                continue;
            };
            if let Some(end) = start.checked_add(length) {
                parsed.push((start, end));
            }
        }
        ranges.insert(memory, parsed);
    }
    ranges
}

/// Expand and lower one selected WAFFLE function.
///
/// This is the lazy frontend boundary used by demand planners. For a
/// `FuncDecl::Lazy`, only `function` is parsed into a body; sibling function
/// bodies remain deferred. Synthetic module providers can use the same route
/// because they produce a normal WAFFLE module without serializing WASM bytes.
pub fn lower_waffle_function_lazy(
    wasm: &WModule,
    function: Func,
    target: &mut VaffleTarget,
    config: &WaffleImportConfig,
) -> Result<(), UnsupportedOp> {
    let declaration = &wasm.funcs[function];
    if matches!(declaration, FuncDecl::Import(..)) {
        return Err(UnsupportedOp(alloc::format!(
            "cannot lower imported function {}",
            declaration.name()
        )));
    }
    let name = declaration.name().to_string();
    let body = portal_pc_waffle_frontend::clone_and_expand_body(wasm, function)
        .map_err(|error| UnsupportedOp(alloc::format!("failed to expand {name}: {error}")))?;
    lower_waffle_function(&body, &name, wasm, target, config)
}

/// Lower a single WAFFLE `FunctionBody` into `target`.
///
/// Parameter types come from `body.locals` (first `body.n_params` locals);
/// return types from `body.rets`.
///
/// Mutable globals are threaded through every function as extra parameters
/// and return values: `params = [orig_params, g0, g1, …]`,
/// `rets = [orig_rets, g0', g1', …]`.
pub fn lower_waffle_function(
    body: &FunctionBody,
    name: &str,
    wasm: &WModule,
    target: &mut VaffleTarget,
    config: &WaffleImportConfig,
) -> Result<(), UnsupportedOp> {
    // ---- Collect mutable globals for threading ---------------------------
    let mut global_lir_tys: Vec<LirType> = Vec::new();
    let mut global_idx_map: BTreeMap<usize, usize> = BTreeMap::new();
    for (g_ref, g_data) in wasm.globals.entries() {
        if g_data.mutable {
            let lir_ty = waffle_ty(g_data.ty)?;
            global_idx_map.insert(g_ref.index(), global_lir_tys.len());
            global_lir_tys.push(lir_ty);
        }
    }

    // ---- Parameter / return types from FunctionBody directly ----------------
    let param_tys: Vec<WType> = body.locals.values().take(body.n_params).copied().collect();
    let ret_tys: &[WType] = &body.rets;

    let param_lir: Vec<LirType> = param_tys
        .iter()
        .map(|&t| waffle_ty(t))
        .collect::<Result<_, _>>()?;
    let ret_lir: Vec<LirType> = ret_tys
        .iter()
        .map(|&t| waffle_ty(t))
        .collect::<Result<_, _>>()?;

    // Append global types to function params for threading.
    let mut all_param_lir = param_lir.clone();
    all_param_lir.extend_from_slice(&global_lir_tys);

    let ret_hint = ret_lir.first().cloned();
    let (entry_block, param_groups) = target.begin_function(name, &all_param_lir, ret_hint);
    target.switch_to_block(entry_block);
    tag_vc_entry_params(target, name, &param_groups, param_lir.len());

    // ---- Map WAFFLE blocks → VAFFLE blocks ----------------------------------
    let mut block_map: BTreeMap<portal_pc_waffle_ir::Block, VaffleBlock> = BTreeMap::new();
    block_map.insert(body.entry, entry_block);
    for (wblock, _) in body.blocks.entries() {
        if wblock == body.entry {
            continue;
        }
        block_map.insert(wblock, target.create_block());
    }

    // ---- Seed value map with entry-block params (= function parameters) -----
    let mut val_map: BTreeMap<WValue, VaffleValue> = BTreeMap::new();
    for ((_, entry_wval), group) in body.blocks[body.entry]
        .params
        .iter()
        .zip(param_groups.iter())
    {
        if let Some(vv) = group.first() {
            val_map.insert(*entry_wval, vv.clone());
        }
    }

    // ---- Initialize current_globals from the trailing function param groups -
    let n_orig_params = param_lir.len();
    let mut current_globals: Vec<VaffleValue> = (0..global_lir_tys.len())
        .map(|i| {
            param_groups
                .get(n_orig_params + i)
                .and_then(|g| g.first())
                .cloned()
                .unwrap_or_else(|| VaffleValue {
                    bits: vec![],
                    ty: LirType::Bool,
                })
        })
        .collect();

    // ---- Pre-map every non-entry block's value params ----------------------
    // A block's params must be resolvable before *any* block that references
    // them is lowered, but `body.blocks.entries()` iterates in block-id order
    // and a loop-exit block can reference its (dominating) loop header's
    // params via an alias while carrying a *lower* block id than the header.
    // Lowering in id order then maps the header's params only after the exit
    // block is lowered, surfacing `UnsupportedOp("undefined v…")` on a branch
    // arg that aliases a not-yet-processed block param. Block params are pure
    // introduction points (they don't depend on other blocks) and
    // `add_block_param` restores the current block, so mapping them all up
    // front in a dedicated pass is safe and order-independent. The threaded
    // globals stay in the main loop (they append after the value params).
    for (wblock, block_def) in body.blocks.entries() {
        if wblock == body.entry {
            continue;
        }
        let vblock = block_map[&wblock];
        for &(ty, wval) in &block_def.params {
            let lir_ty = waffle_ty(ty)?;
            let vv = target.add_block_param(vblock, lir_ty);
            val_map.insert(wval, vv);
        }
    }

    // ---- Emit each WAFFLE block ---------------------------------------------
    for (wblock, block_def) in body.blocks.entries() {
        let vblock = block_map[&wblock];
        target.switch_to_block(vblock);

        // Non-entry block value params were pre-mapped above; here we only add
        // the extra block params carrying the threaded globals for this block.
        if wblock != body.entry {
            current_globals = global_lir_tys
                .iter()
                .map(|ty| target.add_block_param(vblock, ty.clone()))
                .collect();
        }

        // Instructions: each ValueRecord wraps a Value index.
        for record in &block_def.insts {
            let wval = record.value;
            match &body.values[wval] {
                ValueDef::Operator(op, args_ref, tys_ref) => {
                    let args: Vec<WValue> = body.arg_pool[*args_ref].to_vec();
                    let result_tys: Vec<WType> = body.type_pool[*tys_ref].to_vec();
                    if let Some(vv) = lower_op(
                        op,
                        &args,
                        &result_tys,
                        &val_map,
                        &mut current_globals,
                        &global_idx_map,
                        &global_lir_tys,
                        target,
                        wasm,
                        config,
                        body,
                    )? {
                        val_map.insert(wval, vv);
                    }
                }
                ValueDef::PickOutput(from_val, idx, ty) => {
                    if let Some(call_vv) = resolve_wval(body, &val_map, *from_val) {
                        let lir_ty = waffle_ty(*ty)?;
                        let n = bits_for_lir_type(&lir_ty, &[], target.pointer_width().bits());
                        // Compute correct bit offset using the source op's result types.
                        let start = compute_pick_offset(body, from_val, *idx as usize);
                        let end = (start + n).min(call_vv.bits.len());
                        val_map.insert(
                            wval,
                            VaffleValue {
                                bits: call_vv.bits[start..end].to_vec(),
                                ty: lir_ty,
                            },
                        );
                    }
                }
                ValueDef::Alias(target_val) => {
                    if let Some(vv) = resolve_wval(body, &val_map, *target_val) {
                        val_map.insert(wval, vv);
                    }
                }
                // BlockParam and Placeholder are handled during block-param setup.
                _ => {}
            }
        }

        // Terminator.
        lower_term(
            &block_def.terminator.terminator,
            &val_map,
            &block_map,
            &ret_lir,
            &current_globals,
            target,
            body,
        )?;
    }

    target.end_function();
    Ok(())
}

// ============================================================================
// Operator lowering
// ============================================================================

/// Compute the bit offset of the `idx`-th output in a multi-result `ValueDef::Operator`.
///
/// Sums the widths of outputs `0..idx` by reading the operator's result type pool.
/// Returns 0 if `from_val` is not an `Operator` node or any type is unsupported.
fn compute_pick_offset(body: &FunctionBody, from_val: &WValue, idx: usize) -> usize {
    match &body.values[*from_val] {
        ValueDef::Operator(_, _, tys_ref) => body.type_pool[*tys_ref]
            .iter()
            .take(idx)
            .filter_map(|&t| waffle_ty(t).ok())
            // Waffle's own values do not currently include pointers; retain
            // its 32-bit ABI if that ever changes before this helper gains a
            // target argument.
            .map(|ty| bits_for_lir_type(&ty, &[], 32))
            .sum(),
        _ => 0,
    }
}

/// Resolve a WAFFLE value to its already-lowered `VaffleValue`, following
/// `ValueDef::Alias` chains that were never independently scheduled into
/// any block's `insts` list.
///
/// WAFFLE's frontend can produce "loose" aliases this way — e.g. a pure
/// rename of an existing value, such as re-reading a local that hasn't
/// been written since its zero-initialization — without ever listing them
/// in an `insts` array (aliases don't need scheduling; they're a pure
/// indirection meant to be resolved transparently at read time). A plain
/// `val_map` lookup alone misses these even though the value is perfectly
/// well-defined, which previously surfaced as a spurious
/// `UnsupportedOp("undefined value ...")` on any real program complex
/// enough to trigger WAFFLE's alias-based local handling (never hit by
/// the hand-built `FunctionBody` fixtures in this crate's own tests, since
/// those never produce bare aliases).
fn resolve_wval(
    body: &FunctionBody,
    val_map: &BTreeMap<WValue, VaffleValue>,
    mut wval: WValue,
) -> Option<VaffleValue> {
    // Bounded, not a `while let` over a `HashSet`-tracked visited set: a
    // well-formed alias chain is only ever a few hops; this guards against
    // a malformed cycle without paying for cycle bookkeeping on every call.
    for _ in 0..10_000 {
        if let Some(vv) = val_map.get(&wval) {
            return Some(vv.clone());
        }
        match &body.values[wval] {
            ValueDef::Alias(target) => wval = *target,
            _ => return None,
        }
    }
    None
}

fn lower_op(
    op: &Operator,
    args: &[WValue],
    result_tys: &[WType],
    val_map: &BTreeMap<WValue, VaffleValue>,
    current_globals: &mut Vec<VaffleValue>,
    global_idx_map: &BTreeMap<usize, usize>,
    global_lir_tys: &[LirType],
    tgt: &mut VaffleTarget,
    wasm: &WModule,
    config: &WaffleImportConfig,
    body: &FunctionBody,
) -> Result<Option<VaffleValue>, UnsupportedOp> {
    let get = |i: usize| -> Result<VaffleValue, UnsupportedOp> {
        resolve_wval(body, val_map, args[i])
            .ok_or_else(|| UnsupportedOp(alloc::format!("undefined value {:?}", args[i])))
    };

    Ok(Some(match op {
        // ---- Constants -------------------------------------------------
        Operator::I32Const { value } => vc_iconst(tgt, LirType::U32, *value as i32 as i64),
        Operator::I64Const { value } => vc_iconst(tgt, LirType::U64, *value as i64),

        // ---- I32 arithmetic --------------------------------------------
        Operator::I32Add => tgt.add(get(0)?, get(1)?),
        Operator::I32Sub => tgt.sub(get(0)?, get(1)?),
        Operator::I32Mul => tgt.mul(get(0)?, get(1)?),
        Operator::I32DivS => tgt.sdiv(get(0)?, get(1)?),
        Operator::I32DivU => tgt.udiv(get(0)?, get(1)?),
        Operator::I32And => tgt.and(get(0)?, get(1)?),
        Operator::I32Or => tgt.or(get(0)?, get(1)?),
        Operator::I32Xor => tgt.xor(get(0)?, get(1)?),
        Operator::I32Shl => tgt.shl(get(0)?, get(1)?),
        Operator::I32ShrS => tgt.ashr(get(0)?, get(1)?),
        Operator::I32ShrU => tgt.lshr(get(0)?, get(1)?),
        Operator::I32RemU => {
            let a = get(0)?;
            let x = get(1)?;
            let ty = a.ty.clone();
            VaffleValue {
                bits: bc_urem(tgt, &a.bits, &x.bits),
                ty,
            }
        }
        Operator::I32RemS => {
            let a = get(0)?;
            let x = get(1)?;
            let ty = a.ty.clone();
            VaffleValue {
                bits: bc_srem(tgt, &a.bits, &x.bits),
                ty,
            }
        }
        Operator::I32Clz => {
            let a = get(0)?;
            let ty = a.ty.clone();
            VaffleValue {
                bits: bc_clz(tgt, &a.bits),
                ty,
            }
        }
        Operator::I32Ctz => {
            let a = get(0)?;
            let ty = a.ty.clone();
            VaffleValue {
                bits: bc_ctz(tgt, &a.bits),
                ty,
            }
        }
        Operator::I32Popcnt => {
            let a = get(0)?;
            let ty = a.ty.clone();
            VaffleValue {
                bits: bc_popcnt(tgt, &a.bits),
                ty,
            }
        }
        Operator::I32Rotl => {
            let a = get(0)?;
            let x = get(1)?;
            let ty = a.ty.clone();
            VaffleValue {
                bits: bc_rotl(tgt, &a.bits, &x.bits),
                ty,
            }
        }
        Operator::I32Rotr => {
            let a = get(0)?;
            let x = get(1)?;
            let ty = a.ty.clone();
            VaffleValue {
                bits: bc_rotr(tgt, &a.bits, &x.bits),
                ty,
            }
        }

        // ---- I32 comparisons (result is i32: 0 or 1) -------------------
        Operator::I32Eqz => {
            let z = tgt.iconst(LirType::U32, 0);
            let c = tgt.icmp(IcmpPred::Eq, get(0)?, z);
            tgt.zext(c, LirType::U32)
        }
        Operator::I32Eq => {
            let c = tgt.icmp(IcmpPred::Eq, get(0)?, get(1)?);
            tgt.zext(c, LirType::U32)
        }
        Operator::I32Ne => {
            let c = tgt.icmp(IcmpPred::Ne, get(0)?, get(1)?);
            tgt.zext(c, LirType::U32)
        }
        Operator::I32LtS => {
            let c = tgt.icmp(IcmpPred::Slt, get(0)?, get(1)?);
            tgt.zext(c, LirType::U32)
        }
        Operator::I32LtU => {
            let c = tgt.icmp(IcmpPred::Ult, get(0)?, get(1)?);
            tgt.zext(c, LirType::U32)
        }
        Operator::I32GtS => {
            let c = tgt.icmp(IcmpPred::Sgt, get(0)?, get(1)?);
            tgt.zext(c, LirType::U32)
        }
        Operator::I32GtU => {
            let c = tgt.icmp(IcmpPred::Ugt, get(0)?, get(1)?);
            tgt.zext(c, LirType::U32)
        }
        Operator::I32LeS => {
            let c = tgt.icmp(IcmpPred::Sle, get(0)?, get(1)?);
            tgt.zext(c, LirType::U32)
        }
        Operator::I32LeU => {
            let c = tgt.icmp(IcmpPred::Ule, get(0)?, get(1)?);
            tgt.zext(c, LirType::U32)
        }
        Operator::I32GeS => {
            let c = tgt.icmp(IcmpPred::Sge, get(0)?, get(1)?);
            tgt.zext(c, LirType::U32)
        }
        Operator::I32GeU => {
            let c = tgt.icmp(IcmpPred::Uge, get(0)?, get(1)?);
            tgt.zext(c, LirType::U32)
        }

        // ---- I64 arithmetic --------------------------------------------
        Operator::I64Add => tgt.add(get(0)?, get(1)?),
        Operator::I64Sub => tgt.sub(get(0)?, get(1)?),
        Operator::I64Mul => tgt.mul(get(0)?, get(1)?),
        Operator::I64DivS => tgt.sdiv(get(0)?, get(1)?),
        Operator::I64DivU => tgt.udiv(get(0)?, get(1)?),
        Operator::I64And => tgt.and(get(0)?, get(1)?),
        Operator::I64Or => tgt.or(get(0)?, get(1)?),
        Operator::I64Xor => tgt.xor(get(0)?, get(1)?),
        Operator::I64Shl => tgt.shl(get(0)?, get(1)?),
        Operator::I64ShrS => tgt.ashr(get(0)?, get(1)?),
        Operator::I64ShrU => tgt.lshr(get(0)?, get(1)?),
        Operator::I64RemU => {
            let a = get(0)?;
            let x = get(1)?;
            let ty = a.ty.clone();
            VaffleValue {
                bits: bc_urem(tgt, &a.bits, &x.bits),
                ty,
            }
        }
        Operator::I64RemS => {
            let a = get(0)?;
            let x = get(1)?;
            let ty = a.ty.clone();
            VaffleValue {
                bits: bc_srem(tgt, &a.bits, &x.bits),
                ty,
            }
        }
        Operator::I64Clz => {
            let a = get(0)?;
            let ty = a.ty.clone();
            VaffleValue {
                bits: bc_clz(tgt, &a.bits),
                ty,
            }
        }
        Operator::I64Ctz => {
            let a = get(0)?;
            let ty = a.ty.clone();
            VaffleValue {
                bits: bc_ctz(tgt, &a.bits),
                ty,
            }
        }
        Operator::I64Popcnt => {
            let a = get(0)?;
            let ty = a.ty.clone();
            VaffleValue {
                bits: bc_popcnt(tgt, &a.bits),
                ty,
            }
        }
        Operator::I64Rotl => {
            let a = get(0)?;
            let x = get(1)?;
            let ty = a.ty.clone();
            VaffleValue {
                bits: bc_rotl(tgt, &a.bits, &x.bits),
                ty,
            }
        }
        Operator::I64Rotr => {
            let a = get(0)?;
            let x = get(1)?;
            let ty = a.ty.clone();
            VaffleValue {
                bits: bc_rotr(tgt, &a.bits, &x.bits),
                ty,
            }
        }

        // ---- I64 comparisons -------------------------------------------
        Operator::I64Eqz => {
            let z = tgt.iconst(LirType::U64, 0);
            let c = tgt.icmp(IcmpPred::Eq, get(0)?, z);
            tgt.zext(c, LirType::U64)
        }
        Operator::I64Eq => {
            let c = tgt.icmp(IcmpPred::Eq, get(0)?, get(1)?);
            tgt.zext(c, LirType::U64)
        }
        Operator::I64Ne => {
            let c = tgt.icmp(IcmpPred::Ne, get(0)?, get(1)?);
            tgt.zext(c, LirType::U64)
        }
        Operator::I64LtS => {
            let c = tgt.icmp(IcmpPred::Slt, get(0)?, get(1)?);
            tgt.zext(c, LirType::U64)
        }
        Operator::I64LtU => {
            let c = tgt.icmp(IcmpPred::Ult, get(0)?, get(1)?);
            tgt.zext(c, LirType::U64)
        }
        Operator::I64GtS => {
            let c = tgt.icmp(IcmpPred::Sgt, get(0)?, get(1)?);
            tgt.zext(c, LirType::U64)
        }
        Operator::I64GtU => {
            let c = tgt.icmp(IcmpPred::Ugt, get(0)?, get(1)?);
            tgt.zext(c, LirType::U64)
        }
        Operator::I64LeS => {
            let c = tgt.icmp(IcmpPred::Sle, get(0)?, get(1)?);
            tgt.zext(c, LirType::U64)
        }
        Operator::I64LeU => {
            let c = tgt.icmp(IcmpPred::Ule, get(0)?, get(1)?);
            tgt.zext(c, LirType::U64)
        }
        Operator::I64GeS => {
            let c = tgt.icmp(IcmpPred::Sge, get(0)?, get(1)?);
            tgt.zext(c, LirType::U64)
        }
        Operator::I64GeU => {
            let c = tgt.icmp(IcmpPred::Uge, get(0)?, get(1)?);
            tgt.zext(c, LirType::U64)
        }

        // ---- Conversions -----------------------------------------------
        Operator::I32WrapI64 => tgt.trunc(get(0)?, LirType::U32),
        Operator::I64ExtendI32S => tgt.sext(get(0)?, LirType::U64),
        Operator::I64ExtendI32U => tgt.zext(get(0)?, LirType::U64),
        // Sign-extend variants: truncate to the narrow width, then sign-extend.
        Operator::I32Extend8S => {
            let t = tgt.trunc(get(0)?, LirType::U8);
            tgt.sext(t, LirType::U32)
        }
        Operator::I32Extend16S => {
            let t = tgt.trunc(get(0)?, LirType::U16);
            tgt.sext(t, LirType::U32)
        }
        Operator::I64Extend8S => {
            let t = tgt.trunc(get(0)?, LirType::U8);
            tgt.sext(t, LirType::U64)
        }
        Operator::I64Extend16S => {
            let t = tgt.trunc(get(0)?, LirType::U16);
            tgt.sext(t, LirType::U64)
        }
        Operator::I64Extend32S => {
            let t = tgt.trunc(get(0)?, LirType::U32);
            tgt.sext(t, LirType::U64)
        }

        // ---- Select (WAFFLE: args = [val_true, val_false, cond]) --------
        Operator::Select | Operator::TypedSelect { .. } => {
            let if_t = get(0)?;
            let if_f = get(1)?;
            let cond = get(2)?;
            // vc-spec select rule: a *concrete* public condition means the
            // result takes the selected operand's taint. Check the whole i32
            // cond for a public constant before OR-reducing (the reduce would
            // build a fresh, untagged bit and lose the const-ness).
            let width = cond.bits.len();
            let public = tgt.vc_public_side();
            let cond_const = (width <= 64).then(|| tgt.const_u64(&cond.bits)).flatten();
            let cond_is_public = cond.bits.iter().all(|&b| tgt.side_of(b) == public);
            if let (Some(v), true) = (cond_const, cond_is_public) {
                return Ok(Some(if v != 0 { if_t } else { if_f }));
            }
            // cond is I32; treat as bool via OR-reduce (non-zero = true).
            let cond_bit = or_bits(tgt, &cond.bits);
            let cond_bool = VaffleValue {
                bits: vec![cond_bit],
                ty: LirType::Bool,
            };
            tgt.select(cond_bool, if_t, if_f)
        }

        // ---- Globals ---------------------------------------------------
        Operator::GlobalGet { global_index } => {
            let g_ref = *global_index;
            let g_data = &wasm.globals[g_ref];
            if g_data.mutable {
                let local_idx = *global_idx_map.get(&g_ref.index()).ok_or_else(|| {
                    UnsupportedOp(alloc::format!("unknown mutable global {}", g_ref.index()))
                })?;
                return Ok(Some(current_globals[local_idx].clone()));
            } else {
                let lir_ty = waffle_ty(g_data.ty)?;
                let val = g_data.value.unwrap_or(0) as i64;
                return Ok(Some(vc_iconst(tgt, lir_ty, val)));
            }
        }
        Operator::GlobalSet { global_index } => {
            let g_ref = *global_index;
            let g_data = &wasm.globals[g_ref];
            if g_data.mutable {
                let local_idx = *global_idx_map.get(&g_ref.index()).ok_or_else(|| {
                    UnsupportedOp(alloc::format!("unknown mutable global {}", g_ref.index()))
                })?;
                current_globals[local_idx] = get(0)?;
            }
            return Ok(None);
        }

        // ---- Direct call (multi-result, globals threaded) --------------
        Operator::Call { function_index } => {
            let fid = *function_index;
            if tgt.vc.is_some() {
                if let Some(op) = vci_op_for_func(wasm, fid) {
                    return lower_vci_reveal(op, args, val_map, tgt, body);
                }
            }
            let name = callee_name(wasm, fid);

            // Oracle / action dispatch: bypass globals threading. Config
            // lookup uses the canonical import-table name for imports.
            let import_names = waffle_import_func_names(wasm);
            let config_key = callee_config_key(wasm, &import_names, fid);
            if let Some(kind) = config.imports.get(&config_key) {
                let all_arg_vals: Vec<VaffleValue> =
                    args.iter()
                        .map(|wv| {
                            val_map.get(wv).cloned().ok_or_else(|| {
                                UnsupportedOp(alloc::format!("undefined arg {:?}", wv))
                            })
                        })
                        .collect::<Result<_, _>>()?;
                let orig_ret_tys: Vec<LirType> = result_tys
                    .iter()
                    .map(|&t| waffle_ty(t))
                    .collect::<Result<_, _>>()?;

                let results = match kind {
                    WaffleImportKind::Oracle {
                        name: oracle_name,
                        side,
                        ..
                    } => {
                        tgt.set_side(*side);
                        // Emit the oracle as an IR-level `OracleCall` (not a
                        // `Value::Call` to an env import), so it survives the
                        // vaffle→IR lowering as a real `IRStmt::OracleCall`
                        // validated against `module.oracles`.
                        let r = tgt.oracle_call_multi(oracle_name, &all_arg_vals, &orig_ret_tys);
                        tgt.set_side(None);
                        r
                    }
                    WaffleImportKind::Action {
                        name: action_name,
                        n_args,
                        side,
                        ..
                    } => {
                        let expected = n_args
                            .checked_add(1)
                            .and_then(|count| count.checked_add(orig_ret_tys.len()))
                            .ok_or_else(|| {
                                UnsupportedOp(alloc::format!(
                                    "configured action `{action_name}` argument count overflows its ABI"
                                ))
                            })?;
                        if all_arg_vals.len() != expected {
                            return Err(UnsupportedOp(alloc::format!(
                                "configured action `{action_name}` call has an incompatible ABI"
                            )));
                        }
                        let guard_vv = all_arg_vals[0].clone();
                        if guard_vv.bits.len() != 32 {
                            return Err(UnsupportedOp(alloc::format!(
                                "configured action `{action_name}` guard must be i32"
                            )));
                        }
                        let guard_bit = or_bits(tgt, &guard_vv.bits);
                        let real_args = &all_arg_vals[1..=*n_args];
                        let fallbacks = &all_arg_vals[*n_args + 1..];
                        if fallbacks
                            .iter()
                            .zip(&orig_ret_tys)
                            .any(|(fallback, result)| fallback.ty != *result)
                        {
                            return Err(UnsupportedOp(alloc::format!(
                                "configured action `{action_name}` fallback types must match result types"
                            )));
                        }
                        tgt.set_side(*side);
                        // Emit a real `Stmt::ActionCall` (not a call to an
                        // env import): the evaluator-hosted action extern
                        // (e.g. a network socket) survives lowering as an
                        // `IRStmt::ActionCall`.
                        let r = tgt.action_call_multi(
                            action_name,
                            guard_bit,
                            real_args,
                            fallbacks,
                            &orig_ret_tys,
                        );
                        tgt.set_side(None);
                        r
                    }
                };

                return Ok(match results.len() {
                    0 => None,
                    1 => Some(results.into_iter().next().unwrap()),
                    _ => {
                        let bits: Vec<ValueId> = results
                            .iter()
                            .flat_map(|vv| vv.bits.iter().copied())
                            .collect();
                        let ty = results[0].ty.clone();
                        Some(VaffleValue { bits, ty })
                    }
                });
            }

            let arg_vals: Vec<VaffleValue> = args
                .iter()
                .map(|wv| {
                    val_map
                        .get(wv)
                        .cloned()
                        .ok_or_else(|| UnsupportedOp(alloc::format!("undefined arg {:?}", wv)))
                })
                .collect::<Result<_, _>>()?;
            let orig_ret_tys: Vec<LirType> = result_tys
                .iter()
                .map(|&t| waffle_ty(t))
                .collect::<Result<_, _>>()?;
            // Append current globals to args and return types for threading.
            let mut all_args = arg_vals;
            all_args.extend_from_slice(current_globals);
            let mut all_ret_tys = orig_ret_tys.clone();
            all_ret_tys.extend_from_slice(global_lir_tys);
            let mut all_results = tgt.call_extern_multi(&name, &all_args, &all_ret_tys);
            let n_orig = orig_ret_tys.len();
            let new_globals = all_results.split_off(n_orig);
            *current_globals = new_globals;
            return Ok(match all_results.len() {
                0 => None,
                1 => Some(all_results.into_iter().next().unwrap()),
                _ => {
                    // Concatenate return bits so PickOutput can slice with compute_pick_offset.
                    let bits: Vec<ValueId> = all_results
                        .iter()
                        .flat_map(|vv| vv.bits.iter().copied())
                        .collect();
                    let ty = all_results[0].ty.clone();
                    Some(VaffleValue { bits, ty })
                }
            });
        }

        Operator::Nop => return Ok(None),

        // ---- Memory loads (byte-addressed storage) ---------------------
        Operator::I32Load { memory } => {
            lower_mem_load(tgt, memory, &get(0)?, 4, LirType::U32, false, config)
        }
        Operator::I64Load { memory } => {
            lower_mem_load(tgt, memory, &get(0)?, 8, LirType::U64, false, config)
        }
        Operator::I32Load8U { memory } => {
            lower_mem_load(tgt, memory, &get(0)?, 1, LirType::U32, false, config)
        }
        Operator::I32Load8S { memory } => {
            lower_mem_load(tgt, memory, &get(0)?, 1, LirType::U32, true, config)
        }
        Operator::I32Load16U { memory } => {
            lower_mem_load(tgt, memory, &get(0)?, 2, LirType::U32, false, config)
        }
        Operator::I32Load16S { memory } => {
            lower_mem_load(tgt, memory, &get(0)?, 2, LirType::U32, true, config)
        }
        Operator::I64Load8U { memory } => {
            lower_mem_load(tgt, memory, &get(0)?, 1, LirType::U64, false, config)
        }
        Operator::I64Load8S { memory } => {
            lower_mem_load(tgt, memory, &get(0)?, 1, LirType::U64, true, config)
        }
        Operator::I64Load16U { memory } => {
            lower_mem_load(tgt, memory, &get(0)?, 2, LirType::U64, false, config)
        }
        Operator::I64Load16S { memory } => {
            lower_mem_load(tgt, memory, &get(0)?, 2, LirType::U64, true, config)
        }
        Operator::I64Load32U { memory } => {
            lower_mem_load(tgt, memory, &get(0)?, 4, LirType::U64, false, config)
        }
        Operator::I64Load32S { memory } => {
            lower_mem_load(tgt, memory, &get(0)?, 4, LirType::U64, true, config)
        }

        // ---- Memory stores ---------------------------------------------
        Operator::I32Store { memory } => {
            lower_mem_store(tgt, memory, &get(0)?, &get(1)?, 4, config);
            return Ok(None);
        }
        Operator::I64Store { memory } => {
            lower_mem_store(tgt, memory, &get(0)?, &get(1)?, 8, config);
            return Ok(None);
        }
        Operator::I32Store8 { memory } => {
            lower_mem_store(tgt, memory, &get(0)?, &get(1)?, 1, config);
            return Ok(None);
        }
        Operator::I32Store16 { memory } => {
            lower_mem_store(tgt, memory, &get(0)?, &get(1)?, 2, config);
            return Ok(None);
        }
        Operator::I64Store8 { memory } => {
            lower_mem_store(tgt, memory, &get(0)?, &get(1)?, 1, config);
            return Ok(None);
        }
        Operator::I64Store16 { memory } => {
            lower_mem_store(tgt, memory, &get(0)?, &get(1)?, 2, config);
            return Ok(None);
        }
        Operator::I64Store32 { memory } => {
            lower_mem_store(tgt, memory, &get(0)?, &get(1)?, 4, config);
            return Ok(None);
        }

        // ---- Memory size/grow (stubs returning constant) ----------------
        Operator::MemorySize { .. } => {
            // In the ZK/MPC context, memory size is fixed at compile time.
            // Return 0 pages as a placeholder; real programs should not
            // depend on dynamic memory growth.
            tgt.iconst(LirType::U32, 0)
        }
        Operator::MemoryGrow { .. } => {
            // memory.grow always fails (returns -1) in the circuit model.
            tgt.iconst(LirType::U32, -1i32 as i64)
        }

        _ => return Err(UnsupportedOp(alloc::format!("{op:?}"))),
    }))
}

// ============================================================================
// Terminator lowering
// ============================================================================

fn lower_term(
    term: &Terminator,
    val_map: &BTreeMap<WValue, VaffleValue>,
    block_map: &BTreeMap<portal_pc_waffle_ir::Block, VaffleBlock>,
    ret_lir: &[LirType],
    current_globals: &[VaffleValue],
    tgt: &mut VaffleTarget,
    body: &FunctionBody,
) -> Result<(), UnsupportedOp> {
    let get = |wv: &WValue| -> Result<VaffleValue, UnsupportedOp> {
        resolve_wval(body, val_map, *wv)
            .ok_or_else(|| UnsupportedOp(alloc::format!("undefined {:?}", wv)))
    };
    let get_block = |wb: portal_pc_waffle_ir::Block| -> Result<VaffleBlock, UnsupportedOp> {
        block_map
            .get(&wb)
            .copied()
            .ok_or_else(|| UnsupportedOp(alloc::format!("unknown block {:?}", wb)))
    };
    let get_args = |wargs: &[WValue]| -> Result<Vec<VaffleValue>, UnsupportedOp> {
        wargs
            .iter()
            .map(|wv| {
                resolve_wval(body, val_map, *wv)
                    .ok_or_else(|| UnsupportedOp(alloc::format!("undefined {:?}", wv)))
            })
            .collect()
    };

    match term {
        Terminator::Br { target: bt } => {
            let vb = get_block(bt.block)?;
            let mut args = get_args(&bt.args)?;
            args.extend_from_slice(current_globals);
            tgt.jump(vb, BranchTarget::args(args));
        }

        Terminator::CondBr {
            cond,
            if_true,
            if_false,
        } => {
            let cond_vv = get(cond)?;
            // WAFFLE cond is I32; use OR-reduce for non-zero test.
            let cond_bit = or_bits(tgt, &cond_vv.bits);
            let cond_bool = VaffleValue {
                bits: vec![cond_bit],
                ty: LirType::Bool,
            };
            let then_b = get_block(if_true.block)?;
            let else_b = get_block(if_false.block)?;
            let mut then_args = get_args(&if_true.args)?;
            then_args.extend_from_slice(current_globals);
            let mut else_args = get_args(&if_false.args)?;
            else_args.extend_from_slice(current_globals);
            tgt.branch(
                cond_bool,
                then_b,
                BranchTarget::args(then_args),
                else_b,
                BranchTarget::args(else_args),
            );
        }

        Terminator::Return { values } => {
            let mut vals: Vec<VaffleValue> =
                values.iter().map(|v| get(v)).collect::<Result<_, _>>()?;
            vals.extend_from_slice(current_globals);
            tgt.ret(&vals);
        }

        Terminator::Unreachable | Terminator::UB | Terminator::None => {
            // Stub: emit zeros for all original return values + global slots.
            let mut zero_vals: Vec<VaffleValue> =
                ret_lir.iter().map(|ty| tgt.iconst(ty.clone(), 0)).collect();
            let global_zeros: Vec<VaffleValue> = current_globals
                .iter()
                .map(|vv| tgt.iconst(vv.ty.clone(), 0))
                .collect();
            zero_vals.extend(global_zeros);
            tgt.ret(&zero_vals);
        }

        Terminator::ReturnCall { func, args } => {
            let name = alloc::format!("func_{}", func.index());
            let mut arg_vals: Vec<VaffleValue> =
                args.iter().map(|a| get(a)).collect::<Result<_, _>>()?;
            arg_vals.extend_from_slice(current_globals);
            tgt.ret_call(&name, &arg_vals);
        }

        other => return Err(UnsupportedOp(alloc::format!("terminator {other:?}"))),
    }
    Ok(())
}

// ============================================================================
// Memory helpers
// ============================================================================

/// Address width for WASM memory byte addresses (i32 addresses = 32 bits).
#[allow(dead_code)]
const MEM_ADDR_BITS: usize = 32;

/// Compute effective byte address = base_addr + static_offset.
///
/// `base` is the i32 address from the WASM operand stack (32 bits).
/// `offset` is the static offset from the `MemoryArg`.
/// Returns the 32-bit effective address as a bit vector.
fn effective_addr(tgt: &mut VaffleTarget, base: &VaffleValue, offset: u64) -> VaffleValue {
    if offset == 0 {
        return base.clone();
    }
    let off = tgt.iconst(LirType::U32, offset as i64);
    tgt.add(base.clone(), off)
}

/// Read `n_bytes` consecutive bytes from memory storage starting at
/// `byte_addr`, returning a flat bit vector (LSB first, little-endian).
///
/// Each byte is a separate `StorageRead` from `StorageId::memory(mem_idx)`.
/// The returned `VaffleValue` has `n_bytes * 8` bits.
fn mem_load_bytes(
    tgt: &mut VaffleTarget,
    mem_idx: u32,
    byte_addr: &VaffleValue,
    n_bytes: usize,
    config: &WaffleImportConfig,
) -> VaffleValue {
    let storage = StorageId::memory(mem_idx);
    let byte_tid = tgt.byte_tid();
    let bit_tid = tgt.bit_tid();
    let mut all_bits: Vec<ValueId> = Vec::with_capacity(n_bytes * 8);

    for byte_i in 0..n_bytes {
        // Compute address for this byte.
        let addr_val = if byte_i == 0 {
            byte_addr.clone()
        } else {
            let off = tgt.iconst(LirType::U32, byte_i as i64);
            tgt.add(byte_addr.clone(), off)
        };

        // StorageRead: reads one byte (Vec(8, Bit)) from memory.
        let byte_var = tgt.emit_read(
            storage,
            byte_tid,
            memory_address_bits(&addr_val.bits, config),
        );

        // Decompose the byte into 8 individual bits via Shuffle.
        for bit_j in 0..8u8 {
            let bit_var = {
                let v = vaffle::Value::Op(volar_ir_common::Stmt::Shuffle {
                    result_bits: vec![(bit_j, byte_var)],
                    ty: bit_tid,
                });
                tgt.fb().emit_value(v)
            };
            all_bits.push(bit_var);
        }
    }

    let ty = match n_bytes {
        1 => LirType::U8,
        2 => LirType::U16,
        4 => LirType::U32,
        8 => LirType::U64,
        _ => LirType::U32,
    };
    VaffleValue { bits: all_bits, ty }
}

/// Write `n_bytes` bytes of `value` (little-endian, LSB first) to memory
/// storage starting at `byte_addr`.
fn mem_store_bytes(
    tgt: &mut VaffleTarget,
    mem_idx: u32,
    byte_addr: &VaffleValue,
    value: &VaffleValue,
    n_bytes: usize,
    config: &WaffleImportConfig,
) {
    let storage = StorageId::memory(mem_idx);
    let byte_tid = tgt.byte_tid();

    for byte_i in 0..n_bytes {
        // Compute address for this byte.
        let addr_val = if byte_i == 0 {
            byte_addr.clone()
        } else {
            let off = tgt.iconst(LirType::U32, byte_i as i64);
            tgt.add(byte_addr.clone(), off)
        };

        // Extract 8 bits for this byte.
        let base = byte_i * 8;
        let bits: Vec<ValueId> = (0..8)
            .map(|j| {
                if base + j < value.bits.len() {
                    value.bits[base + j]
                } else {
                    tgt.bc_const(false) // zero-pad
                }
            })
            .collect();

        // Merge 8 bits into a byte-typed value.
        let byte_var = tgt.compose_address(&bits); // compose_address creates Merge → Vec(8, Bit)
        // Actually compose_address creates Vec(N, Bit) where N = bits.len().
        // For 8 bits this gives us Vec(8, Bit) = byte_tid. Perfect.

        tgt.emit_write(
            storage,
            byte_var,
            byte_tid,
            memory_address_bits(&addr_val.bits, config),
        );
    }
}

/// Lower a WAFFLE memory load operator.
///
/// Returns the loaded value as a `VaffleValue` with the appropriate type.
fn lower_mem_load(
    tgt: &mut VaffleTarget,
    memory: &MemoryArg,
    base: &VaffleValue,
    load_bytes: usize,
    result_ty: LirType,
    sign_extend: bool,
    config: &WaffleImportConfig,
) -> VaffleValue {
    let mem_idx = memory.memory.index() as u32;
    let addr = effective_addr(tgt, base, memory.offset);
    let loaded = mem_load_bytes(tgt, mem_idx, &addr, load_bytes, config);

    // Extend to the target width if needed.
    let target_bits = bits_for_lir_type(&result_ty, &[], tgt.pointer_width().bits());
    if loaded.bits.len() == target_bits {
        VaffleValue {
            bits: loaded.bits,
            ty: result_ty,
        }
    } else if sign_extend {
        tgt.sext(loaded, result_ty)
    } else {
        tgt.zext(loaded, result_ty)
    }
}

/// Lower a WAFFLE memory store operator.
fn lower_mem_store(
    tgt: &mut VaffleTarget,
    memory: &MemoryArg,
    base: &VaffleValue,
    value: &VaffleValue,
    store_bytes: usize,
    config: &WaffleImportConfig,
) {
    let mem_idx = memory.memory.index() as u32;
    let addr = effective_addr(tgt, base, memory.offset);
    // Truncate to the store width if needed.
    let store_bits = store_bytes * 8;
    let truncated = if value.bits.len() > store_bits {
        VaffleValue {
            bits: value.bits[..store_bits].to_vec(),
            ty: value.ty.clone(),
        }
    } else {
        value.clone()
    };
    mem_store_bytes(tgt, mem_idx, &addr, &truncated, store_bytes, config);
}

/// Select the low address bits used by a bounded memory image after full
/// 32-bit WASM effective-address arithmetic has completed.
fn memory_address_bits<'a>(bits: &'a [ValueId], config: &WaffleImportConfig) -> &'a [ValueId] {
    match config.memory_address_bits() {
        Some(width) => &bits[..width],
        None => bits,
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// OR-reduce a bit vector: 1 iff any input bit is 1 (non-zero test).
fn or_bits(tgt: &mut VaffleTarget, bits: &[ValueId]) -> ValueId {
    if bits.is_empty() {
        return tgt.bc_const(false);
    }
    let mut acc = bits[0];
    for &b in &bits[1..] {
        acc = tgt.bc_or(acc, b);
    }
    acc
}

/// Derive a callee name from the WAFFLE module's function declaration.
fn callee_name(wasm: &WModule, fid: portal_pc_waffle_ir::Func) -> String {
    match &wasm.funcs[fid] {
        FuncDecl::Body(_, name, _) => name.clone(),
        FuncDecl::Import(_, name) => name.clone(),
        _ => alloc::format!("func_{}", fid.index()),
    }
}

/// Canonical `"<module>.<field>"` names for every *imported* function in
/// `wasm`, keyed by func index.
///
/// The WAFFLE frontend leaves `FuncDecl::Import`'s name field empty for real
/// WASM binaries (the two-level `(import "mod" "field")` name lives in the
/// module's import table, keyed by [`ImportKind::Func`]); synthetic modules
/// (tests, producers) instead fill that name field with a single-segment
/// name. Config lookups ([`WaffleImportConfig`]) are keyed on the canonical
/// `"mod.field"` form, falling back to the name field so synthetic modules
/// keep working.
pub fn waffle_import_func_names(wasm: &WModule) -> BTreeMap<usize, String> {
    let mut out = BTreeMap::new();
    for import in &wasm.imports {
        if let portal_pc_waffle_ir::ImportKind::Func(func) = import.kind {
            out.insert(
                func.index(),
                alloc::format!("{}.{}", import.module, import.name),
            );
        }
    }
    for (func_ref, decl) in wasm.funcs.entries() {
        if let FuncDecl::Import(_, name) = decl {
            if !name.is_empty() {
                out.entry(func_ref.index()).or_insert_with(|| name.clone());
            }
        }
    }
    out
}

/// The config lookup key for `fid`: the canonical import-table name for
/// imports, the declaration name otherwise.
fn callee_config_key(
    wasm: &WModule,
    import_names: &BTreeMap<usize, String>,
    fid: portal_pc_waffle_ir::Func,
) -> String {
    if let FuncDecl::Import(..) = &wasm.funcs[fid] {
        if let Some(name) = import_names.get(&fid.index()) {
            return name.clone();
        }
    }
    callee_name(wasm, fid)
}

fn vc_iconst(tgt: &mut VaffleTarget, ty: LirType, val: i64) -> VaffleValue {
    if let Some(side) = tgt.vc_public_side() {
        tgt.set_side(Some(side));
        let v = tgt.iconst(ty, val);
        tgt.set_side(None);
        v
    } else {
        tgt.iconst(ty, val)
    }
}

fn tag_vc_entry_params(
    target: &mut VaffleTarget,
    name: &str,
    param_groups: &[Vec<VaffleValue>],
    n_orig_params: usize,
) {
    let (args, public) = {
        let Some(vc) = target.vc.as_mut() else {
            return;
        };
        vc.reveals.clear();
        (vc.calls.get(name).cloned(), vc.ids.public)
    };
    let sides: Vec<Option<volar_side::SideId>> = {
        let Some(vc) = target.vc.as_ref() else {
            return;
        };
        (0..n_orig_params)
            .map(|i| {
                args.as_ref()
                    .and_then(|a| a.get(i))
                    .map(|arg| vc.ids.side_of(arg.visibility()))
            })
            .collect()
    };

    for (i, group) in param_groups.iter().enumerate() {
        let side = if i < n_orig_params {
            sides[i]
        } else {
            Some(public)
        };
        let Some(side) = side else {
            continue;
        };
        for vv in group {
            for &bit in &vv.bits {
                target.set_node_side(bit, Some(side));
            }
        }
    }
}

fn lower_vci_reveal(
    op: VciOp,
    args: &[WValue],
    val_map: &BTreeMap<WValue, VaffleValue>,
    tgt: &mut VaffleTarget,
    body: &FunctionBody,
) -> Result<Option<VaffleValue>, UnsupportedOp> {
    let get = |i: usize| -> Result<VaffleValue, UnsupportedOp> {
        resolve_wval(body, val_map, args[i])
            .ok_or_else(|| UnsupportedOp(alloc::format!("undefined value {:?}", args[i])))
    };
    match op {
        VciOp::UnsupportedFloat => Err(UnsupportedOp(
            "VCI float reveal is unsupported (no f32/f64 circuit)".into(),
        )),
        VciOp::RevealI32 | VciOp::RevealI64 => {
            let x = get(0)?;
            let public = tgt
                .vc_public_side()
                .expect("VCI reveal requires an active VC session");
            let n = {
                let vc = tgt.vc.as_mut().expect("VC session");
                vc.next_handle = vc.next_handle.saturating_add(1);
                let n = vc.next_handle;
                vc.reveals.insert(
                    n,
                    RevealedBits {
                        bits: x.bits.clone(),
                        ty: x.ty.clone(),
                    },
                );
                n
            };
            tgt.set_side(Some(public));
            let handle = tgt.iconst(LirType::U32, n as i64);
            tgt.set_side(None);
            Ok(Some(handle))
        }
        VciOp::WaitI32 | VciOp::WaitI64 => {
            let h = get(0)?;
            let n = tgt.const_u64(&h.bits).ok_or_else(|| {
                UnsupportedOp("VCI reveal handle is not a concrete constant".into())
            })?;
            if n == 0 || n > u32::MAX as u64 {
                return Err(UnsupportedOp("VCI reveal handle is not valid".into()));
            }
            let revealed = {
                let vc = tgt.vc.as_mut().expect("VC session");
                vc.reveals.remove(&(n as u32)).ok_or_else(|| {
                    UnsupportedOp("VCI reveal handle is not valid and unconsumed".into())
                })?
            };
            let public = tgt
                .vc_public_side()
                .expect("VCI wait requires an active VC session");
            tgt.set_side(Some(public));
            let zero = tgt.iconst(revealed.ty.clone(), 0);
            let src = VaffleValue {
                bits: revealed.bits,
                ty: revealed.ty,
            };
            let out = tgt.xor(src, zero);
            tgt.set_side(None);
            Ok(Some(out))
        }
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    extern crate std;
    use std::vec;

    use super::*;
    use portal_pc_waffle_ir::entity::EntityVec;
    use portal_pc_waffle_ir::{
        BlockTarget, Global, GlobalData, Memory, MemoryData, MemorySegment, Module as WModule,
        Operator, Signature, SignatureData, Terminator as WTerminator, Type as WType, ValueDef,
    };
    use volar_ir_common::Stmt;

    #[test]
    fn respect_unstable_skips_declared_unused_data_storage() {
        let mut wasm = WModule::empty();
        wasm.memories.push(MemoryData {
            initial_pages: 1,
            maximum_pages: Some(1),
            segments: vec![MemorySegment {
                offset: 32,
                data: vec![1, 2, 3],
            }],
            memory64: false,
            shared: false,
            page_size_log2: None,
        });
        let mut range = BTreeMap::new();
        range.insert("length".into(), wax_meta::Value::U64(3));
        range.insert("start".into(), wax_meta::Value::U64(32));
        let mut manifest = wax_meta::Manifest::new();
        manifest
            .insert(
                "memory/0/unused".into(),
                wax_meta::Value::List(vec![wax_meta::Value::Map(range)]),
            )
            .unwrap();
        wasm.set_wsmm_manifest(&manifest).unwrap();

        let mut respected = VaffleTarget::new();
        lower_waffle_module_with_metadata(
            &wasm,
            &mut respected,
            &WaffleImportConfig::default(),
            WasmMetadataMode::RespectUnstable,
        );
        assert!(respected.module.pre_init.is_empty());

        let mut ignored = VaffleTarget::new();
        lower_waffle_module_with_metadata(
            &wasm,
            &mut ignored,
            &WaffleImportConfig::default(),
            WasmMetadataMode::Ignore,
        );
        assert_eq!(ignored.module.pre_init.len(), 1);
    }

    /// Build a minimal WAFFLE module with one memory and a function that
    /// does `i32.store(addr=param0, val=param1)` then `i32.load(addr=param0) → return`.
    fn build_store_load_module() -> WModule<'static> {
        let mut sigs: EntityVec<Signature, SignatureData> = EntityVec::default();
        let sig = sigs.push(SignatureData::Func {
            params: vec![WType::I32, WType::I32],
            returns: vec![WType::I32],
            shared: false,
        });

        let mut memories: EntityVec<Memory, MemoryData> = EntityVec::default();
        memories.push(MemoryData {
            initial_pages: 1,
            maximum_pages: None,
            segments: vec![],
            memory64: false,
            shared: false,
            page_size_log2: None,
        });

        let mut module = WModule {
            orig_bytes: None,
            funcs: EntityVec::default(),
            signatures: sigs,
            globals: EntityVec::default(),
            tables: EntityVec::default(),
            imports: vec![],
            exports: vec![],
            memories,
            control_tags: EntityVec::default(),
            start_func: None,
            debug: Default::default(),
            debug_map: Default::default(),
            custom_sections: Default::default(),
        };

        let mut body = portal_pc_waffle_ir::FunctionBody::new(&module, sig);
        let entry = body.entry;

        // param0 = address, param1 = value
        let param0 = body.blocks[entry].params[0].1;
        let param1 = body.blocks[entry].params[1].1;

        let mem_arg = MemoryArg {
            align: 2,
            offset: 0,
            memory: Memory::from(0u32),
        };

        // i32.store param0, param1
        body.add_op(
            entry,
            Operator::I32Store {
                memory: mem_arg.clone(),
            },
            &[param0, param1],
            &[],
        );

        // loaded = i32.load param0
        let loaded = body.add_op(
            entry,
            Operator::I32Load { memory: mem_arg },
            &[param0],
            &[WType::I32],
        );

        // return loaded
        body.set_terminator(
            entry,
            WTerminator::Return {
                values: vec![loaded],
            },
        );

        module.funcs.push(portal_pc_waffle_ir::FuncDecl::Body(
            sig,
            "store_load".into(),
            body,
        ));

        module
    }

    #[test]
    fn test_memory_store_load_lowering() {
        let wasm = build_store_load_module();
        let mut target = VaffleTarget::new();

        let errors = lower_waffle_module(&wasm, &mut target, &WaffleImportConfig::default());
        assert!(errors.is_empty(), "unexpected errors: {:?}", errors);

        // Should have one function in the module.
        assert_eq!(target.module.funcs.len(), 1);

        // Check that StorageRead and StorageWrite ops are present in the VAFFLE.
        let func = &target.module.funcs[0];
        let body = match func {
            vaffle::FuncDecl::Body(b) => b,
            _ => panic!("expected function body"),
        };

        let mut has_read = false;
        let mut has_write = false;
        for val in &body.values {
            if let vaffle::Value::Op(stmt) = &val.kind {
                match stmt {
                    Stmt::StorageRead { storage, .. } => {
                        assert_eq!(storage.0, StorageId::MEMORY_BASE);
                        has_read = true;
                    }
                    Stmt::StorageWrite { storage, .. } => {
                        assert_eq!(storage.0, StorageId::MEMORY_BASE);
                        has_write = true;
                    }
                    _ => {}
                }
            }
        }
        assert!(
            has_write,
            "VAFFLE should contain StorageWrite for i32.store"
        );
        assert!(has_read, "VAFFLE should contain StorageRead for i32.load");
    }

    #[test]
    fn bounded_memory_uses_only_configured_address_bits() {
        let wasm = build_store_load_module();
        let mut bounded = VaffleTarget::new();
        let errors = lower_waffle_module(
            &wasm,
            &mut bounded,
            &WaffleImportConfig::default().with_memory_address_bits(5),
        );
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        let body = match &bounded.module.funcs[0] {
            vaffle::FuncDecl::Body(body) => body,
            _ => panic!(),
        };
        let address_width = |addr: ValueId| match &body.values[addr.0].kind {
            vaffle::Value::Op(Stmt::Merge { parts, .. }) => parts.len(),
            other => panic!("memory address should be a merged bit vector, got {other:?}"),
        };
        let widths: Vec<_> = body
            .values
            .iter()
            .filter_map(|value| match &value.kind {
                vaffle::Value::Op(Stmt::StorageRead { addr, .. })
                | vaffle::Value::Op(Stmt::StorageWrite { addr, .. }) => Some(address_width(*addr)),
                _ => None,
            })
            .collect();
        assert!(!widths.is_empty());
        assert!(widths.iter().all(|&width| width == 5));

        let mut full = VaffleTarget::new();
        let errors = lower_waffle_module(&wasm, &mut full, &WaffleImportConfig::default());
        assert!(errors.is_empty(), "unexpected errors: {errors:?}");
        let body = match &full.module.funcs[0] {
            vaffle::FuncDecl::Body(body) => body,
            _ => panic!(),
        };
        let full_address_width = |addr: ValueId| match &body.values[addr.0].kind {
            vaffle::Value::Op(Stmt::Merge { parts, .. }) => parts.len(),
            other => panic!("memory address should be a merged bit vector, got {other:?}"),
        };
        assert!(body.values.iter().any(|value| match &value.kind {
            vaffle::Value::Op(Stmt::StorageRead { addr, .. })
            | vaffle::Value::Op(Stmt::StorageWrite { addr, .. }) =>
                full_address_width(*addr) == MEM_ADDR_BITS,
            _ => false,
        }));
    }

    /// Build a module with i32.store8 + i32.load8_u to test sub-word memory access.
    fn build_byte_store_load_module() -> WModule<'static> {
        let mut sigs: EntityVec<Signature, SignatureData> = EntityVec::default();
        let sig = sigs.push(SignatureData::Func {
            params: vec![WType::I32, WType::I32],
            returns: vec![WType::I32],
            shared: false,
        });

        let mut memories: EntityVec<Memory, MemoryData> = EntityVec::default();
        memories.push(MemoryData {
            initial_pages: 1,
            maximum_pages: None,
            segments: vec![],
            memory64: false,
            shared: false,
            page_size_log2: None,
        });

        let mut module = WModule {
            orig_bytes: None,
            funcs: EntityVec::default(),
            signatures: sigs,
            globals: EntityVec::default(),
            tables: EntityVec::default(),
            imports: vec![],
            exports: vec![],
            memories,
            control_tags: EntityVec::default(),
            start_func: None,
            debug: Default::default(),
            debug_map: Default::default(),
            custom_sections: Default::default(),
        };

        let mut body = portal_pc_waffle_ir::FunctionBody::new(&module, sig);
        let entry = body.entry;
        let param0 = body.blocks[entry].params[0].1;
        let param1 = body.blocks[entry].params[1].1;

        let mem_arg = MemoryArg {
            align: 0,
            offset: 0,
            memory: Memory::from(0u32),
        };

        // i32.store8 param0, param1
        body.add_op(
            entry,
            Operator::I32Store8 {
                memory: mem_arg.clone(),
            },
            &[param0, param1],
            &[],
        );

        // loaded = i32.load8_u param0
        let loaded = body.add_op(
            entry,
            Operator::I32Load8U { memory: mem_arg },
            &[param0],
            &[WType::I32],
        );

        body.set_terminator(
            entry,
            WTerminator::Return {
                values: vec![loaded],
            },
        );

        module.funcs.push(portal_pc_waffle_ir::FuncDecl::Body(
            sig,
            "byte_store_load".into(),
            body,
        ));

        module
    }

    #[test]
    fn test_byte_memory_access() {
        let wasm = build_byte_store_load_module();
        let mut target = VaffleTarget::new();
        let errors = lower_waffle_module(&wasm, &mut target, &WaffleImportConfig::default());
        assert!(errors.is_empty(), "unexpected errors: {:?}", errors);

        let func = &target.module.funcs[0];
        let body = match func {
            vaffle::FuncDecl::Body(b) => b,
            _ => panic!("expected function body"),
        };

        // i32.store8 writes 1 byte, i32.load8_u reads 1 byte.
        // Each should produce exactly 1 StorageWrite / 1 StorageRead.
        let writes: Vec<_> = body
            .values
            .iter()
            .filter(|v| matches!(&v.kind, vaffle::Value::Op(Stmt::StorageWrite { .. })))
            .collect();
        let reads: Vec<_> = body
            .values
            .iter()
            .filter(|v| matches!(&v.kind, vaffle::Value::Op(Stmt::StorageRead { .. })))
            .collect();
        assert_eq!(
            writes.len(),
            1,
            "store8 should produce exactly 1 StorageWrite"
        );
        assert_eq!(
            reads.len(),
            1,
            "load8_u should produce exactly 1 StorageRead"
        );
    }

    #[test]
    fn test_i32_store_produces_4_byte_writes() {
        let wasm = build_store_load_module();
        let mut target = VaffleTarget::new();
        let errors = lower_waffle_module(&wasm, &mut target, &WaffleImportConfig::default());
        assert!(errors.is_empty());

        let body = match &target.module.funcs[0] {
            vaffle::FuncDecl::Body(b) => b,
            _ => panic!(),
        };

        // i32.store writes 4 bytes → 4 StorageWrite ops.
        let writes: Vec<_> = body
            .values
            .iter()
            .filter(|v| matches!(&v.kind, vaffle::Value::Op(Stmt::StorageWrite { .. })))
            .collect();
        assert_eq!(
            writes.len(),
            4,
            "i32.store should produce 4 StorageWrite ops (one per byte)"
        );

        // i32.load reads 4 bytes → 4 StorageRead ops.
        let reads: Vec<_> = body
            .values
            .iter()
            .filter(|v| matches!(&v.kind, vaffle::Value::Op(Stmt::StorageRead { .. })))
            .collect();
        assert_eq!(
            reads.len(),
            4,
            "i32.load should produce 4 StorageRead ops (one per byte)"
        );
    }

    /// Test that MemoryArg.offset is applied correctly.
    fn build_offset_load_module() -> WModule<'static> {
        let mut sigs: EntityVec<Signature, SignatureData> = EntityVec::default();
        let sig = sigs.push(SignatureData::Func {
            params: vec![WType::I32],
            returns: vec![WType::I32],
            shared: false,
        });

        let mut memories: EntityVec<Memory, MemoryData> = EntityVec::default();
        memories.push(MemoryData {
            initial_pages: 1,
            maximum_pages: None,
            segments: vec![],
            memory64: false,
            shared: false,
            page_size_log2: None,
        });

        let mut module = WModule {
            orig_bytes: None,
            funcs: EntityVec::default(),
            signatures: sigs,
            globals: EntityVec::default(),
            tables: EntityVec::default(),
            imports: vec![],
            exports: vec![],
            memories,
            control_tags: EntityVec::default(),
            start_func: None,
            debug: Default::default(),
            debug_map: Default::default(),
            custom_sections: Default::default(),
        };

        let mut body = portal_pc_waffle_ir::FunctionBody::new(&module, sig);
        let entry = body.entry;
        let param0 = body.blocks[entry].params[0].1;

        let mem_arg = MemoryArg {
            align: 2,
            offset: 16, // static offset of 16 bytes
            memory: Memory::from(0u32),
        };

        let loaded = body.add_op(
            entry,
            Operator::I32Load { memory: mem_arg },
            &[param0],
            &[WType::I32],
        );

        body.set_terminator(
            entry,
            WTerminator::Return {
                values: vec![loaded],
            },
        );

        module.funcs.push(portal_pc_waffle_ir::FuncDecl::Body(
            sig,
            "offset_load".into(),
            body,
        ));

        module
    }

    #[test]
    fn test_offset_load_lowering() {
        let wasm = build_offset_load_module();
        let mut target = VaffleTarget::new();
        let errors = lower_waffle_module(&wasm, &mut target, &WaffleImportConfig::default());
        assert!(errors.is_empty(), "unexpected errors: {:?}", errors);

        // The function should have produced VAFFLE with storage reads.
        // The offset computation happens via add circuits, but the key
        // property is that it lowers without error.
        let body = match &target.module.funcs[0] {
            vaffle::FuncDecl::Body(b) => b,
            _ => panic!(),
        };
        let reads: Vec<_> = body
            .values
            .iter()
            .filter(|v| matches!(&v.kind, vaffle::Value::Op(Stmt::StorageRead { .. })))
            .collect();
        assert_eq!(
            reads.len(),
            4,
            "i32.load with offset should produce 4 reads"
        );
    }

    // ── helpers shared by integer-op tests ───────────────────────────────────

    /// Build a minimal module (no memory) with the given signature and body.
    fn build_simple_module(
        params: Vec<WType>,
        returns: Vec<WType>,
        build_body: impl FnOnce(
            &mut portal_pc_waffle_ir::FunctionBody,
            portal_pc_waffle_ir::Block,
            Vec<portal_pc_waffle_ir::Value>,
        ) -> portal_pc_waffle_ir::Value,
    ) -> WModule<'static> {
        let mut sigs: EntityVec<Signature, SignatureData> = EntityVec::default();
        let sig = sigs.push(SignatureData::Func {
            params: params.clone(),
            returns: returns.clone(),
            shared: false,
        });
        let mut module = WModule {
            orig_bytes: None,
            funcs: EntityVec::default(),
            signatures: sigs,
            globals: EntityVec::default(),
            tables: EntityVec::default(),
            imports: vec![],
            exports: vec![],
            memories: EntityVec::default(),
            control_tags: EntityVec::default(),
            start_func: None,
            debug: Default::default(),
            debug_map: Default::default(),
            custom_sections: Default::default(),
        };
        let mut body = portal_pc_waffle_ir::FunctionBody::new(&module, sig);
        let entry = body.entry;
        let ps: Vec<_> = body.blocks[entry]
            .params
            .iter()
            .map(|&(_ty, v)| v)
            .collect();
        let result = build_body(&mut body, entry, ps);
        body.set_terminator(
            entry,
            WTerminator::Return {
                values: vec![result],
            },
        );
        module
            .funcs
            .push(portal_pc_waffle_ir::FuncDecl::Body(sig, "f".into(), body));
        module
    }

    // ── I32RemU ──────────────────────────────────────────────────────────────

    #[test]
    fn test_i32_remu_lowers() {
        let wasm = build_simple_module(
            vec![WType::I32, WType::I32],
            vec![WType::I32],
            |body, entry, ps| body.add_op(entry, Operator::I32RemU, &[ps[0], ps[1]], &[WType::I32]),
        );
        let mut target = VaffleTarget::new();
        let errors = lower_waffle_module(&wasm, &mut target, &WaffleImportConfig::default());
        assert!(errors.is_empty(), "i32.rem_u lowering failed: {:?}", errors);
        assert_eq!(target.module.funcs.len(), 1);
    }

    // ── I32RemS ──────────────────────────────────────────────────────────────

    #[test]
    fn test_i32_rems_lowers() {
        let wasm = build_simple_module(
            vec![WType::I32, WType::I32],
            vec![WType::I32],
            |body, entry, ps| body.add_op(entry, Operator::I32RemS, &[ps[0], ps[1]], &[WType::I32]),
        );
        let mut target = VaffleTarget::new();
        let errors = lower_waffle_module(&wasm, &mut target, &WaffleImportConfig::default());
        assert!(errors.is_empty(), "i32.rem_s lowering failed: {:?}", errors);
    }

    // ── I32Clz ───────────────────────────────────────────────────────────────

    #[test]
    fn test_i32_clz_lowers() {
        let wasm = build_simple_module(vec![WType::I32], vec![WType::I32], |body, entry, ps| {
            body.add_op(entry, Operator::I32Clz, &[ps[0]], &[WType::I32])
        });
        let mut target = VaffleTarget::new();
        let errors = lower_waffle_module(&wasm, &mut target, &WaffleImportConfig::default());
        assert!(errors.is_empty(), "i32.clz lowering failed: {:?}", errors);
    }

    // ── I32Ctz ───────────────────────────────────────────────────────────────

    #[test]
    fn test_i32_ctz_lowers() {
        let wasm = build_simple_module(vec![WType::I32], vec![WType::I32], |body, entry, ps| {
            body.add_op(entry, Operator::I32Ctz, &[ps[0]], &[WType::I32])
        });
        let mut target = VaffleTarget::new();
        let errors = lower_waffle_module(&wasm, &mut target, &WaffleImportConfig::default());
        assert!(errors.is_empty(), "i32.ctz lowering failed: {:?}", errors);
    }

    // ── I32Popcnt ────────────────────────────────────────────────────────────

    #[test]
    fn test_i32_popcnt_lowers() {
        let wasm = build_simple_module(vec![WType::I32], vec![WType::I32], |body, entry, ps| {
            body.add_op(entry, Operator::I32Popcnt, &[ps[0]], &[WType::I32])
        });
        let mut target = VaffleTarget::new();
        let errors = lower_waffle_module(&wasm, &mut target, &WaffleImportConfig::default());
        assert!(
            errors.is_empty(),
            "i32.popcnt lowering failed: {:?}",
            errors
        );
    }

    // ── I32Rotl ──────────────────────────────────────────────────────────────

    #[test]
    fn test_i32_rotl_lowers() {
        let wasm = build_simple_module(
            vec![WType::I32, WType::I32],
            vec![WType::I32],
            |body, entry, ps| body.add_op(entry, Operator::I32Rotl, &[ps[0], ps[1]], &[WType::I32]),
        );
        let mut target = VaffleTarget::new();
        let errors = lower_waffle_module(&wasm, &mut target, &WaffleImportConfig::default());
        assert!(errors.is_empty(), "i32.rotl lowering failed: {:?}", errors);
    }

    // ── I32Rotr ──────────────────────────────────────────────────────────────

    #[test]
    fn test_i32_rotr_lowers() {
        let wasm = build_simple_module(
            vec![WType::I32, WType::I32],
            vec![WType::I32],
            |body, entry, ps| body.add_op(entry, Operator::I32Rotr, &[ps[0], ps[1]], &[WType::I32]),
        );
        let mut target = VaffleTarget::new();
        let errors = lower_waffle_module(&wasm, &mut target, &WaffleImportConfig::default());
        assert!(errors.is_empty(), "i32.rotr lowering failed: {:?}", errors);
    }

    // ── I32Extend8S ──────────────────────────────────────────────────────────

    #[test]
    fn test_i32_extend8s_lowers() {
        let wasm = build_simple_module(vec![WType::I32], vec![WType::I32], |body, entry, ps| {
            body.add_op(entry, Operator::I32Extend8S, &[ps[0]], &[WType::I32])
        });
        let mut target = VaffleTarget::new();
        let errors = lower_waffle_module(&wasm, &mut target, &WaffleImportConfig::default());
        assert!(
            errors.is_empty(),
            "i32.extend8_s lowering failed: {:?}",
            errors
        );
    }

    // ── I32Extend16S ─────────────────────────────────────────────────────────

    #[test]
    fn test_i32_extend16s_lowers() {
        let wasm = build_simple_module(vec![WType::I32], vec![WType::I32], |body, entry, ps| {
            body.add_op(entry, Operator::I32Extend16S, &[ps[0]], &[WType::I32])
        });
        let mut target = VaffleTarget::new();
        let errors = lower_waffle_module(&wasm, &mut target, &WaffleImportConfig::default());
        assert!(
            errors.is_empty(),
            "i32.extend16_s lowering failed: {:?}",
            errors
        );
    }

    // ── I64Extend32S ─────────────────────────────────────────────────────────

    #[test]
    fn test_i64_extend32s_lowers() {
        let wasm = build_simple_module(vec![WType::I64], vec![WType::I64], |body, entry, ps| {
            body.add_op(entry, Operator::I64Extend32S, &[ps[0]], &[WType::I64])
        });
        let mut target = VaffleTarget::new();
        let errors = lower_waffle_module(&wasm, &mut target, &WaffleImportConfig::default());
        assert!(
            errors.is_empty(),
            "i64.extend32_s lowering failed: {:?}",
            errors
        );
    }

    // ── Mutable global threading ──────────────────────────────────────────────
    //
    // A WASM function with sig () → (i32) that reads then sets a mutable i32
    // global (g0=42) should lower to a VAFFLE function whose signature has an
    // extra i32 param (the incoming global value) and an extra i32 result (the
    // updated global value).

    fn build_global_get_set_module() -> WModule<'static> {
        let mut sigs: EntityVec<Signature, SignatureData> = EntityVec::default();
        // WASM: () → (i32)
        let sig = sigs.push(SignatureData::Func {
            params: vec![],
            returns: vec![WType::I32],
            shared: false,
        });

        let mut globals: EntityVec<Global, GlobalData> = EntityVec::default();
        globals.push(GlobalData {
            ty: WType::I32,
            value: Some(42),
            mutable: true,
        });

        let mut module = WModule {
            orig_bytes: None,
            funcs: EntityVec::default(),
            signatures: sigs,
            globals,
            tables: EntityVec::default(),
            imports: vec![],
            exports: vec![],
            memories: EntityVec::default(),
            control_tags: EntityVec::default(),
            start_func: None,
            debug: Default::default(),
            debug_map: Default::default(),
            custom_sections: Default::default(),
        };

        let mut body = portal_pc_waffle_ir::FunctionBody::new(&module, sig);
        let entry = body.entry;
        let g = Global::from(0u32);

        // old_val = global.get g0
        let old_val = body.add_op(
            entry,
            Operator::GlobalGet { global_index: g },
            &[],
            &[WType::I32],
        );

        // global.set g0, old_val  (write back same value — just to exercise GlobalSet)
        body.add_op(
            entry,
            Operator::GlobalSet { global_index: g },
            &[old_val],
            &[],
        );

        // return old_val
        body.set_terminator(
            entry,
            WTerminator::Return {
                values: vec![old_val],
            },
        );
        module.funcs.push(portal_pc_waffle_ir::FuncDecl::Body(
            sig,
            "global_rw".into(),
            body,
        ));
        module
    }

    #[test]
    fn test_mutable_global_threading() {
        let wasm = build_global_get_set_module();
        let mut target = VaffleTarget::new();
        let errors = lower_waffle_module(&wasm, &mut target, &WaffleImportConfig::default());
        assert!(errors.is_empty(), "global threading failed: {:?}", errors);
        assert_eq!(target.module.funcs.len(), 1);

        // The VAFFLE function's signature should have 1 extra param (the incoming
        // global value) and 1 extra result (the outgoing global value) compared
        // to the original WASM signature (() → i32).
        let func_body = match &target.module.funcs[0] {
            vaffle::FuncDecl::Body(b) => b,
            _ => panic!("expected function body"),
        };
        let sig = &target.module.sigs[func_body.sig.0];

        // Original WASM: 0 params, 1 i32 return.
        // After threading 1 mutable i32 global: 32 bit-params (the global bits),
        // and the Return terminator should carry 32 (orig i32) + 32 (global out) = 64 bits.
        assert_eq!(
            sig.params.len(),
            32,
            "VAFFLE sig should have 32 bit-params (the mutable i32 global), got {:?}",
            sig.params.len()
        );

        let entry_block = &func_body.blocks[func_body.entry.0];
        let ret_bits = match &entry_block.terminator {
            vaffle::Terminator::Return { values } => values.len(),
            other => panic!("expected Return terminator, got {:?}", other),
        };
        assert_eq!(
            ret_bits, 64,
            "Return should carry 64 bits (32 orig + 32 global_out), got {ret_bits}"
        );
    }

    // ── Multi-value return lowers without error ───────────────────────────────
    //
    // A WASM function that returns 2 values should lower without error.

    #[test]
    fn test_multi_value_return_lowers() {
        let mut sigs: EntityVec<Signature, SignatureData> = EntityVec::default();
        // (i32) → (i32, i32)
        let sig = sigs.push(SignatureData::Func {
            params: vec![WType::I32],
            returns: vec![WType::I32, WType::I32],
            shared: false,
        });
        let mut module = WModule {
            orig_bytes: None,
            funcs: EntityVec::default(),
            signatures: sigs,
            globals: EntityVec::default(),
            tables: EntityVec::default(),
            imports: vec![],
            exports: vec![],
            memories: EntityVec::default(),
            control_tags: EntityVec::default(),
            start_func: None,
            debug: Default::default(),
            debug_map: Default::default(),
            custom_sections: Default::default(),
        };
        let mut body = portal_pc_waffle_ir::FunctionBody::new(&module, sig);
        let entry = body.entry;
        let p = body.blocks[entry].params[0].1;
        // return (p, p)
        body.set_terminator(entry, WTerminator::Return { values: vec![p, p] });
        module.funcs.push(portal_pc_waffle_ir::FuncDecl::Body(
            sig,
            "multi_ret".into(),
            body,
        ));

        let mut target = VaffleTarget::new();
        let errors = lower_waffle_module(&module, &mut target, &WaffleImportConfig::default());
        assert!(
            errors.is_empty(),
            "multi-value return lowering failed: {:?}",
            errors
        );
        assert_eq!(target.module.funcs.len(), 1);
        // VAFFLE sig should return 2 * 32 = 64 bits (two i32s concatenated).
        let func_body = match &target.module.funcs[0] {
            vaffle::FuncDecl::Body(b) => b,
            _ => panic!("expected function body"),
        };
        let sig = &target.module.sigs[func_body.sig.0];
        // WASM (i32) → (i32, i32): one 32-bit param → 64-bit return.
        // VAFFLE sig carries 32 bit-params. Results are in the Return terminator.
        assert_eq!(
            sig.params.len(),
            32,
            "VAFFLE sig should have 32 bit-params for the i32 input"
        );

        // The Return terminator should carry 64 ValueIds (32+32 bits for the two i32s).
        let entry_block = &func_body.blocks[func_body.entry.0];
        let ret_bits = match &entry_block.terminator {
            vaffle::Terminator::Return { values } => values.len(),
            other => panic!("expected Return terminator, got {:?}", other),
        };
        assert_eq!(
            ret_bits, 64,
            "Return should carry 64 bits for two i32 returns, got {ret_bits}"
        );
    }

    // ── PickOutput: multi-value call with per-output extraction ───────────────

    #[test]
    fn test_pick_output_from_multi_return_call() {
        // Callee: () → (i32, i32) returning two constants.
        let mut sigs: EntityVec<Signature, SignatureData> = EntityVec::default();
        let callee_sig = sigs.push(SignatureData::Func {
            params: vec![],
            returns: vec![WType::I32, WType::I32],
            shared: false,
        });
        // Caller: () → (i32) that calls the callee and returns the second output.
        let caller_sig = sigs.push(SignatureData::Func {
            params: vec![],
            returns: vec![WType::I32],
            shared: false,
        });

        let mut module = WModule {
            orig_bytes: None,
            funcs: EntityVec::default(),
            signatures: sigs,
            globals: EntityVec::default(),
            tables: EntityVec::default(),
            imports: vec![],
            exports: vec![],
            memories: EntityVec::default(),
            control_tags: EntityVec::default(),
            start_func: None,
            debug: Default::default(),
            debug_map: Default::default(),
            custom_sections: Default::default(),
        };

        // Build callee body: return (1, 2).
        let mut callee_body = portal_pc_waffle_ir::FunctionBody::new(&module, callee_sig);
        let callee_entry = callee_body.entry;
        let c1 = callee_body.add_op(
            callee_entry,
            Operator::I32Const { value: 1 },
            &[],
            &[WType::I32],
        );
        let c2 = callee_body.add_op(
            callee_entry,
            Operator::I32Const { value: 2 },
            &[],
            &[WType::I32],
        );
        callee_body.set_terminator(
            callee_entry,
            WTerminator::Return {
                values: vec![c1, c2],
            },
        );
        let callee_func = module.funcs.push(portal_pc_waffle_ir::FuncDecl::Body(
            callee_sig,
            "callee".into(),
            callee_body,
        ));

        // Build caller body: call callee, pick output[1], return it.
        let mut caller_body = portal_pc_waffle_ir::FunctionBody::new(&module, caller_sig);
        let caller_entry = caller_body.entry;
        // The Call op produces a tuple value with both returns.
        let call_val = caller_body.add_op(
            caller_entry,
            Operator::Call {
                function_index: callee_func,
            },
            &[],
            &[WType::I32, WType::I32],
        );
        // PickOutput(call_val, 1, I32) extracts the second return.
        let picked = caller_body.add_value(ValueDef::PickOutput(call_val, 1, WType::I32));
        caller_body.append_to_block(caller_entry, picked);
        caller_body.set_terminator(
            caller_entry,
            WTerminator::Return {
                values: vec![picked],
            },
        );
        module.funcs.push(portal_pc_waffle_ir::FuncDecl::Body(
            caller_sig,
            "caller".into(),
            caller_body,
        ));

        let mut target = VaffleTarget::new();
        let errors = lower_waffle_module(&module, &mut target, &WaffleImportConfig::default());
        assert!(
            errors.is_empty(),
            "PickOutput lowering failed: {:?}",
            errors
        );
        // Both functions should have been lowered.
        assert_eq!(target.module.funcs.len(), 2);
    }

    // ── Oracle / action import registration ──────────────────────────────────
    //
    // A WAFFLE module with:
    //   - import "oracle_hash"  : (i32, i32) → i32   (oracle)
    //   - import "action_send"  : (i32, i32, i32) → i32  (action: guard, 1 arg, 1 fallback)
    //   - a caller function that calls both
    //
    // After lowering with the appropriate WaffleImportConfig the VAFFLE module
    // must contain one OracleDecl with name "hash" and one ActionDecl with name "send".

    fn build_oracle_action_module() -> WModule<'static> {
        let mut sigs: EntityVec<Signature, SignatureData> = EntityVec::default();
        // oracle_hash: (i32, i32) → i32
        let oracle_sig = sigs.push(SignatureData::Func {
            params: vec![WType::I32, WType::I32],
            returns: vec![WType::I32],
            shared: false,
        });
        // action_send: (i32, i32, i32) → i32  [guard, arg, fallback]
        let action_sig = sigs.push(SignatureData::Func {
            params: vec![WType::I32, WType::I32, WType::I32],
            returns: vec![WType::I32],
            shared: false,
        });
        // caller: (i32, i32, i32) → i32  [two hash inputs + guard]
        let caller_sig = sigs.push(SignatureData::Func {
            params: vec![WType::I32, WType::I32, WType::I32],
            returns: vec![WType::I32],
            shared: false,
        });

        let mut module = WModule {
            orig_bytes: None,
            funcs: EntityVec::default(),
            signatures: sigs,
            globals: EntityVec::default(),
            tables: EntityVec::default(),
            imports: vec![],
            exports: vec![],
            memories: EntityVec::default(),
            control_tags: EntityVec::default(),
            start_func: None,
            debug: Default::default(),
            debug_map: Default::default(),
            custom_sections: Default::default(),
        };

        // Push the two imports.
        let oracle_func = module.funcs.push(portal_pc_waffle_ir::FuncDecl::Import(
            oracle_sig,
            "oracle_hash".into(),
        ));
        let action_func = module.funcs.push(portal_pc_waffle_ir::FuncDecl::Import(
            action_sig,
            "action_send".into(),
        ));

        // Build caller body: hash(p0, p1) → h; send(p2, h, 0) → result; return result.
        let mut body = portal_pc_waffle_ir::FunctionBody::new(&module, caller_sig);
        let entry = body.entry;
        let p0 = body.blocks[entry].params[0].1;
        let p1 = body.blocks[entry].params[1].1;
        let p2 = body.blocks[entry].params[2].1; // guard

        // h = oracle_hash(p0, p1)
        let h = body.add_op(
            entry,
            Operator::Call {
                function_index: oracle_func,
            },
            &[p0, p1],
            &[WType::I32],
        );

        // fallback = 0
        let fallback = body.add_op(entry, Operator::I32Const { value: 0 }, &[], &[WType::I32]);

        // result = action_send(p2, h, fallback)
        let result = body.add_op(
            entry,
            Operator::Call {
                function_index: action_func,
            },
            &[p2, h, fallback],
            &[WType::I32],
        );

        body.set_terminator(
            entry,
            WTerminator::Return {
                values: vec![result],
            },
        );

        module.funcs.push(portal_pc_waffle_ir::FuncDecl::Body(
            caller_sig,
            "caller".into(),
            body,
        ));

        module
    }

    #[test]
    fn test_oracle_and_action_registration_with_explicit_policy() {
        use volar_ir_common::{
            ActionExecutionPolicy, ExternalExecutor, ExternalRevealPolicy, OracleExecutionKind,
            OracleExecutionPolicy,
        };

        let wasm = build_oracle_action_module();
        let oracle_policy = OracleExecutionPolicy {
            execution: OracleExecutionKind::Assigned,
            executor: ExternalExecutor::Garbler,
            reveal: ExternalRevealPolicy::BothRoles,
            fingerprint: [0xA1; 32],
        };
        let action_policy = ActionExecutionPolicy {
            executor: ExternalExecutor::Garbler,
            reveal: ExternalRevealPolicy::BothRoles,
            fingerprint: [0xB2; 32],
        };
        let config = WaffleImportConfig::new()
            .with_oracle_execution("oracle_hash", "hash", oracle_policy)
            .with_action_execution("action_send", "send", 1, action_policy);
        let mut target = VaffleTarget::new();
        let errors = lower_waffle_module(&wasm, &mut target, &config);
        assert!(errors.is_empty(), "unexpected errors: {:?}", errors);
        assert_eq!(target.module.oracles[0].execution, oracle_policy);
        assert_eq!(target.module.actions[0].execution, action_policy);
    }

    #[test]
    fn test_oracle_and_action_registration() {
        let wasm = build_oracle_action_module();
        let config = WaffleImportConfig::new()
            .with_oracle("oracle_hash", "hash")
            .with_action("action_send", "send", 1);

        let mut target = VaffleTarget::new();
        let errors = lower_waffle_module(&wasm, &mut target, &config);
        assert!(errors.is_empty(), "unexpected errors: {:?}", errors);

        // OracleDecl for "hash" should be registered.
        assert_eq!(target.module.oracles.len(), 1, "expected one oracle");
        assert_eq!(target.module.oracles[0].name, "hash");
        assert_eq!(
            target.module.oracles[0].params.len(),
            2,
            "oracle has 2 params"
        );
        assert_eq!(
            target.module.oracles[0].results.len(),
            1,
            "oracle has 1 result"
        );

        // ActionDecl for "send" should be registered.
        assert_eq!(target.module.actions.len(), 1, "expected one action");
        assert_eq!(target.module.actions[0].name, "send");
        assert_eq!(
            target.module.actions[0].params.len(),
            1,
            "action has 1 real arg"
        );
        assert_eq!(
            target.module.actions[0].results.len(),
            1,
            "action has 1 result"
        );

        // The default convenience builder remains explicitly legacy.
        assert_eq!(
            target.module.oracles[0].execution,
            volar_ir_common::OracleExecutionPolicy::legacy_evaluator()
        );
        assert_eq!(
            target.module.actions[0].execution,
            volar_ir_common::ActionExecutionPolicy::legacy_evaluator()
        );

        // The caller function should have lowered successfully.
        let caller = target
            .module
            .funcs
            .iter()
            .find(|f| matches!(f, vaffle::FuncDecl::Body(_)));
        assert!(caller.is_some(), "caller function body should be present");
    }

    /// Regression: a loop whose exit block references the (dominating) loop
    /// header's block params via an alias, while the exit block carries a
    /// *lower* block id than the header, must lower without
    /// `UnsupportedOp("undefined v…")`. The lowering pre-maps every non-entry
    /// block's value params before emitting any block, so forward references
    /// resolve regardless of block-id order. (This is the `(block (loop …))`
    /// counting loop, which previously failed with `undefined v11`.)
    #[test]
    fn loop_exit_referencing_header_param_lowers() {
        let wat_src = r#"(module
          (func $f (export "f") (param $n i32) (result i32)
            (local $acc i32)
            (block $done
              (loop $l
                (br_if $done (i32.eqz (local.get $n)))
                (local.set $acc (i32.add (local.get $acc) (local.get $n)))
                (local.set $n (i32.sub (local.get $n) (i32.const 1)))
                (br $l)))
            (local.get $acc)))"#;
        let bytes = wat::parse_str(wat_src).expect("wat assembles");
        let mut wasm = portal_pc_waffle_frontend::from_wasm_bytes(
            &bytes,
            &portal_pc_waffle_frontend::FrontendOptions::default(),
        )
        .expect("wasm parses");
        portal_pc_waffle_frontend::expand_all_funcs(&mut wasm).expect("expand");

        let mut target = VaffleTarget::new();
        let errors = lower_waffle_module(&wasm, &mut target, &WaffleImportConfig::default());
        assert!(
            errors.is_empty(),
            "loop with forward block-param reference must lower: {errors:?}"
        );
        assert_eq!(target.module.funcs.len(), 1);
    }
}
