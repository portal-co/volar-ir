// @reliability: experimental
//! `vaffle::Module<P>` → `LirTarget<H::Output>` lowering.

use alloc::{collections::BTreeMap, format, string::String, vec::Vec};

use vaffle::{FuncBody, FuncDecl, FuncId, Module, Target as VTarget, Terminator, Value, ValueId};
use volar_ir_common::{IrType, Stmt, Type as NativeType, TypeId, TypeTable};
use volar_lir::{BranchTarget, LirTarget, LirType, assign_chunks};
use volar_lir_saved::{RecordingTarget, SavedLirModule};
use volar_provenance::{KeepProvenance, ProvenanceHandler};

/// Lower every `FuncDecl::Body` function in `module` into `target`.
///
/// Forwards provenance unchanged (equivalent to `KeepProvenance`).
/// `FuncDecl::Import` entries are not emitted directly — they surface only
/// as `call_extern` sites at their call sites, matching `call_extern`'s own
/// documented forward-declaration responsibility.
pub fn lower_vaffle_module<P: Clone, T: LirTarget<P>>(module: &Module<P>, target: &mut T) {
    lower_vaffle_module_with_handler(module, target, &KeepProvenance)
}

/// Like [`lower_vaffle_module`], but maps provenance through `handler`.
pub fn lower_vaffle_module_with_handler<P, T, H>(module: &Module<P>, target: &mut T, handler: &H)
where
    P: Clone,
    H: ProvenanceHandler<P>,
    T: LirTarget<H::Output>,
{
    let func_names = build_func_names(module);
    for (idx, decl) in module.funcs.iter().enumerate() {
        if let FuncDecl::Body(body) = decl {
            let name = func_names[idx]
                .as_deref()
                .expect("internal: every FuncDecl::Body must have an assigned name");
            lower_vaffle_func(body, module, &func_names, name, target, handler);
        }
    }
}

// ============================================================================
// Multi-translation-unit (chunked) lowering
// ============================================================================

/// Lower every `FuncDecl::Body` function in `module` into `n_chunks`
/// independent [`SavedLirModule`]s instead of one shared target.
///
/// Each function is assigned to exactly one chunk (via [`assign_chunks`],
/// weighted by each function's statement count, ignoring call-graph
/// locality). A call from one chunk's function to a function assigned to a
/// *different* chunk needs no special handling: [`LirTarget::call`] already
/// forward-declares a callee it hasn't locally defined (as a plain
/// external-linkage declaration in every backend), which is exactly the
/// right shape for a genuine cross-translation-unit call once each chunk is
/// independently compiled and the results linked together.
///
/// `FuncDecl::Import` entries need no chunk assignment — they never get a
/// `begin_function`, only `call_extern` forward declarations at their call
/// sites, same as the unchunked [`lower_vaffle_module`].
///
/// Forwards provenance unchanged (equivalent to [`KeepProvenance`]).
///
/// # Known limitation
///
/// This does not (and, given VAFFLE never produces `LirType::Struct`, does
/// not need to) handle a struct-typed value crossing a chunk boundary: a
/// `StructId` is assigned by a per-backend-instance monotonic counter, so
/// two independently-lowered chunks can disagree on what ID the "same"
/// struct has. `LirType::Arr` (the only aggregate VAFFLE can actually
/// produce, via [`results_to_lir`]'s homogeneous multi-result packing) is
/// unaffected — its C typedef name (e.g. `Arr_U8_16`) is structural, not
/// counter-based, so two chunks compiled independently always agree on it.
pub fn lower_vaffle_module_to_lir_chunks<P: Clone>(
    module: &Module<P>,
    n_chunks: usize,
) -> Vec<SavedLirModule>
where
    RecordingTarget: LirTarget<P>,
{
    lower_vaffle_module_to_lir_chunks_with_handler(module, n_chunks, &KeepProvenance)
}

/// Like [`lower_vaffle_module_to_lir_chunks`], but maps provenance through
/// `handler`.
pub fn lower_vaffle_module_to_lir_chunks_with_handler<P, H>(
    module: &Module<P>,
    n_chunks: usize,
    handler: &H,
) -> Vec<SavedLirModule>
where
    P: Clone,
    H: ProvenanceHandler<P>,
    RecordingTarget: LirTarget<H::Output>,
{
    let func_names = build_func_names(module);

    // Weight each `FuncDecl::Body` function by its statement count (a proxy
    // for lowered-LIR size); `FuncDecl::Import` entries carry no weight
    // since they never get a `begin_function`.
    let mut body_indices: Vec<usize> = Vec::new();
    let mut weights: Vec<u64> = Vec::new();
    for (idx, decl) in module.funcs.iter().enumerate() {
        if let FuncDecl::Body(body) = decl {
            body_indices.push(idx);
            let stmt_count: usize = body.blocks.iter().map(|b| b.stmts.len()).sum();
            weights.push((stmt_count as u64).max(1));
        }
    }

    if body_indices.is_empty() {
        return Vec::new();
    }

    let assignment = assign_chunks(&weights, n_chunks);
    let actual_chunks = assignment.iter().copied().max().map_or(0, |m| m + 1);

    let mut targets: Vec<RecordingTarget> = (0..actual_chunks).map(|_| RecordingTarget::new()).collect();

    for (i, &func_idx) in body_indices.iter().enumerate() {
        let chunk = assignment[i];
        let FuncDecl::Body(body) = &module.funcs[func_idx] else {
            unreachable!("body_indices only contains FuncDecl::Body indices");
        };
        let name = func_names[func_idx]
            .as_deref()
            .expect("internal: every FuncDecl::Body must have an assigned name");
        lower_vaffle_func(body, module, &func_names, name, &mut targets[chunk], handler);
    }

    targets.into_iter().map(RecordingTarget::finish).collect()
}

/// Assign a stable name to every `FuncDecl::Body` function: its export name
/// if one exists (checking `module.exports` in insertion/key order — the
/// first match wins if a `FuncId` has multiple export aliases), otherwise a
/// synthesized `__vaffle_func_<index>` name. `FuncDecl::Import` entries are
/// left `None` — callers use their own declared `name` field directly.
///
/// `vaffle::FuncBody` carries no name field of its own (only `exports` does),
/// so this table is the single source of truth used consistently at both a
/// function's `begin_function` site and every `call` site referencing it —
/// including across a chunk boundary (see [`lower_vaffle_module_to_lir_chunks`]),
/// since it is built once and shared by every chunk's lowering pass.
pub fn build_func_names<P: Clone>(module: &Module<P>) -> Vec<Option<String>> {
    let mut names: Vec<Option<String>> = alloc::vec![None; module.funcs.len()];
    for (export_name, fid) in &module.exports {
        if matches!(module.funcs.get(fid.0), Some(FuncDecl::Body(_))) {
            names[fid.0].get_or_insert_with(|| export_name.clone());
        }
    }
    for (idx, decl) in module.funcs.iter().enumerate() {
        if matches!(decl, FuncDecl::Body(_)) && names[idx].is_none() {
            names[idx] = Some(format!("__vaffle_func_{idx}"));
        }
    }
    names
}

// ============================================================================
// Type mapping
// ============================================================================

fn type_bits(tid: TypeId, types: &TypeTable) -> u32 {
    match &types.0[tid.0 as usize] {
        IrType::Primitive(NativeType::Bit) => 1,
        IrType::Primitive(NativeType::_8) | IrType::Primitive(NativeType::AES8) => 8,
        IrType::Primitive(NativeType::_16) => 16,
        IrType::Primitive(NativeType::_32) => 32,
        IrType::Primitive(NativeType::_64) | IrType::Primitive(NativeType::Galois64) => 64,
        IrType::Primitive(NativeType::_128) => 128,
        IrType::Primitive(NativeType::_256) => 256,
        IrType::Vec(n, elem) => (*n as u32) * type_bits(*elem, types),
        other => unimplemented!("type_bits: unsupported VAFFLE type {:?}", other),
    }
}

fn type_to_lir(tid: TypeId, types: &TypeTable) -> LirType {
    let bits = type_bits(tid, types);
    match bits {
        1 => LirType::Bool,
        2..=8 => LirType::U8,
        9..=16 => LirType::U16,
        17..=32 => LirType::U32,
        33..=64 => LirType::U64,
        w => unimplemented!(
            "type_to_lir: {w}-bit VAFFLE type exceeds 64-bit word; \
             multi-word lowering is not yet implemented",
        ),
    }
}

/// Resolve a callee's ABI return type from its signature's `results` list.
///
/// `LirTarget::begin_function`/`call`/`call_extern` all take a single
/// `Option<LirType>`, so multiple logical results must be packed into one
/// ABI type. Homogeneous multi-results pack into `LirType::Arr`; genuinely
/// heterogeneous multi-results are not yet supported (no producer in this
/// codebase emits them today — see `Value::Output`'s single-level-call doc).
fn results_to_lir(results: &[TypeId], types: &TypeTable) -> Option<LirType> {
    match results {
        [] => None,
        [only] => Some(type_to_lir(*only, types)),
        many => {
            let first = type_to_lir(many[0], types);
            if many.iter().all(|r| type_to_lir(*r, types) == first) {
                Some(LirType::Arr(alloc::boxed::Box::new(first), many.len()))
            } else {
                unimplemented!(
                    "results_to_lir: heterogeneous multi-result VAFFLE function \
                     ({} results) — not yet supported",
                    many.len(),
                )
            }
        }
    }
}

// ============================================================================
// Per-function lowering
// ============================================================================

/// Lower a single function's body into `target`.
///
/// Each function must be lowered into exactly one target instance; a
/// [`Value::Call`]/[`Terminator::ReturnCall`] site resolves its callee's name
/// via the shared `func_names` table regardless of which target instance
/// that callee itself was (or will be) lowered into — [`LirTarget::call`]
/// already forward-declares a name it hasn't locally defined, so calling
/// functions into *different* target instances (multi-translation-unit
/// chunking) needs no special handling here. See
/// [`lower_vaffle_module_to_lir_chunks`].
pub fn lower_vaffle_func<P, T, H>(
    body: &FuncBody<P>,
    module: &Module<P>,
    func_names: &[Option<String>],
    name: &str,
    target: &mut T,
    handler: &H,
) where
    P: Clone,
    H: ProvenanceHandler<P>,
    T: LirTarget<H::Output>,
{
    let sig = &module.sigs[body.sig.0];
    let input_tys: Vec<LirType> = sig
        .params
        .iter()
        .map(|tid| type_to_lir(*tid, &module.types))
        .collect();
    let ret_ty = results_to_lir(&sig.results, &module.types);

    let (entry_handle, entry_param_groups) = target.begin_function(name, &input_tys, ret_ty);

    let mut block_handles: Vec<Option<T::Block>> = alloc::vec![None; body.blocks.len()];
    block_handles[body.entry.0] = Some(entry_handle);
    for bi in 0..body.blocks.len() {
        if bi != body.entry.0 {
            block_handles[bi] = Some(target.create_block());
        }
    }
    let block_handles: Vec<T::Block> = block_handles
        .into_iter()
        .map(|b| b.expect("every block handle assigned"))
        .collect();

    let mut vals: Vec<Option<T::Value>> = alloc::vec![None; body.values.len()];

    // Seed the entry block's params from `begin_function`'s flat scalar groups.
    {
        let entry_block = &body.blocks[body.entry.0];
        for ((vid, _tid), group) in entry_block
            .params
            .iter()
            .zip(entry_param_groups.into_iter())
        {
            vals[vid.0] = Some(group.into_iter().next().expect("empty param scalar group"));
        }
    }
    // Add block params for every other block.
    for (bi, block) in body.blocks.iter().enumerate() {
        if bi == body.entry.0 {
            continue;
        }
        for (vid, tid) in &block.params {
            let lir_ty = type_to_lir(*tid, &module.types);
            let v = target.add_block_param(block_handles[bi].clone(), lir_ty);
            vals[vid.0] = Some(v);
        }
    }

    // Side table for multi-result `Value::Call` aggregates, keyed by the
    // call's own `ValueId` — projected via `Value::Output`.
    let mut call_agg: BTreeMap<usize, Vec<T::Value>> = BTreeMap::new();
    // Side table for multi-result `Stmt::OracleCall`/`ActionCall` aggregates
    // (inside `Value::Op`), keyed the same way but projected via
    // `Stmt::OracleOutput`/`ActionOutput` — kept separate from `call_agg`
    // since the two projection mechanisms are distinct in VAFFLE's data
    // model (see `Value::Output`'s doc: it only ever projects `Value::Call`).
    let mut multi_results: BTreeMap<usize, Vec<T::Value>> = BTreeMap::new();

    for (bi, block) in body.blocks.iter().enumerate() {
        target.switch_to_block(block_handles[bi].clone());
        for &vid in &block.stmts {
            let node = &body.values[vid.0];
            target.set_prov(handler.map(&node.prov));
            lower_vaffle_value(
                vid,
                &node.kind,
                module,
                func_names,
                &block_handles,
                &mut vals,
                &mut call_agg,
                &mut multi_results,
                target,
            );
        }
        lower_vaffle_terminator(
            &block.terminator,
            body,
            module,
            func_names,
            &vals,
            &block_handles,
            target,
        );
    }

    target.end_function();
}

// ============================================================================
// Value lowering
// ============================================================================

fn get<V: Clone>(vals: &[Option<V>], v: &ValueId) -> V {
    vals[v.0]
        .clone()
        .expect("VAFFLE value used before it was defined")
}

#[allow(clippy::too_many_arguments)]
fn lower_vaffle_value<P, T, Q>(
    vid: ValueId,
    value: &Value,
    module: &Module<P>,
    func_names: &[Option<String>],
    block_handles: &[T::Block],
    vals: &mut [Option<T::Value>],
    call_agg: &mut BTreeMap<usize, Vec<T::Value>>,
    multi_results: &mut BTreeMap<usize, Vec<T::Value>>,
    target: &mut T,
) where
    P: Clone,
    Q: Clone,
    T: LirTarget<Q>,
{
    match value {
        Value::Param { .. } => {
            // Params are seeded up front from block-param handles; a `Param`
            // entry should never appear in `Block::stmts` (mirroring the
            // VAFFLE fuzz interpreter's own evaluator convention).
            assert!(
                vals[vid.0].is_some(),
                "Value::Param encountered in Block::stmts without a pre-seeded value"
            );
        }
        Value::Op(Stmt::OracleCall {
            name,
            args,
            output_tys,
            ..
        }) => {
            let arg_vals: Vec<T::Value> = args.iter().map(|a| get(vals, a)).collect();
            let arg_lir_tys: Vec<LirType> = args.iter().map(|_| LirType::U64).collect();
            let ret_tys: Vec<LirType> = output_tys
                .iter()
                .map(|tid| type_to_lir(*tid, &module.types))
                .collect();
            let results = target.oracle(name, &arg_lir_tys, &arg_vals, &ret_tys);
            multi_results.insert(vid.0, results);
            vals[vid.0] = Some(target.iconst(LirType::Bool, 0));
        }
        Value::Op(Stmt::OracleOutput { call, idx, .. }) => {
            let results = multi_results
                .get(&call.0)
                .expect("OracleOutput: no stashed results for OracleCall");
            vals[vid.0] = Some(results[*idx].clone());
        }
        Value::Op(Stmt::ActionCall {
            name,
            guard,
            args,
            fallbacks,
            output_tys,
            ..
        }) => {
            let guard_val = get(vals, guard);
            let arg_vals: Vec<T::Value> = args.iter().map(|a| get(vals, a)).collect();
            let fallback_vals: Vec<T::Value> = fallbacks.iter().map(|a| get(vals, a)).collect();
            let arg_lir_tys: Vec<LirType> = args.iter().map(|_| LirType::U64).collect();
            let ret_tys: Vec<LirType> = output_tys
                .iter()
                .map(|tid| type_to_lir(*tid, &module.types))
                .collect();
            let results = target.action(
                name,
                guard_val,
                &arg_lir_tys,
                &arg_vals,
                &fallback_vals,
                &ret_tys,
            );
            multi_results.insert(vid.0, results);
            vals[vid.0] = Some(target.iconst(LirType::Bool, 0));
        }
        Value::Op(Stmt::ActionOutput { call, idx, .. }) => {
            let results = multi_results
                .get(&call.0)
                .expect("ActionOutput: no stashed results for ActionCall");
            vals[vid.0] = Some(results[*idx].clone());
        }
        Value::Op(Stmt::Rng { ty, .. }) => {
            let lir_ty = type_to_lir(*ty, &module.types);
            vals[vid.0] = Some(target.rng(lir_ty));
        }
        Value::Op(stmt) => {
            let v = lower_vaffle_stmt::<T, Q>(stmt, vals, &module.types, target);
            vals[vid.0] = Some(v);
        }
        Value::Call { func, args } => {
            let arg_vals: Vec<T::Value> = args.iter().map(|a| get(vals, a)).collect();
            let callee_sig = &module.sigs[module.funcs[func.0].sig().0];
            let arg_lir_tys: Vec<LirType> = callee_sig
                .params
                .iter()
                .map(|tid| type_to_lir(*tid, &module.types))
                .collect();
            let ret_ty = results_to_lir(&callee_sig.results, &module.types);
            let results = emit_callee_call(
                *func,
                module,
                func_names,
                &arg_lir_tys,
                &arg_vals,
                ret_ty,
                target,
            );
            call_agg.insert(vid.0, results);
            // Unused placeholder: VAFFLE always projects call results via
            // `Value::Output`, never the call's own `ValueId` directly (see
            // the fuzz interpreter's `eval_vaffle_value`, which inserts an
            // empty value for the call site itself).
            vals[vid.0] = Some(target.iconst(LirType::Bool, 0));
        }
        Value::Output {
            value: call_vid,
            idx,
        } => {
            let results = call_agg
                .get(&call_vid.0)
                .expect("Output: no stashed results for Call");
            vals[vid.0] = Some(results[*idx].clone());
        }
        Value::StackAlloc { elem_ty, count, .. } => {
            let lir_ty = type_to_lir(*elem_ty, &module.types);
            let ext = target
                .stack_alloc_ext()
                .expect("StackAlloc: target has no memory model");
            vals[vid.0] = Some(ext.alloca(lir_ty, *count));
        }
        Value::PtrLoad { ptr, pointee_ty } => {
            let lir_ty = type_to_lir(*pointee_ty, &module.types);
            let ptr_val = get(vals, ptr);
            let ext = target
                .stack_alloc_ext()
                .expect("PtrLoad: target has no memory model");
            vals[vid.0] = Some(ext.ptr_load(ptr_val, lir_ty));
        }
        Value::PtrStore { ptr, val } => {
            let ptr_val = get(vals, ptr);
            let val_val = get(vals, val);
            let ext = target
                .stack_alloc_ext()
                .expect("PtrStore: target has no memory model");
            ext.ptr_store(ptr_val, val_val);
            // No result value — nothing else may reference this `ValueId`.
        }
        Value::PtrOffset { ptr, idx, .. } => {
            let ptr_val = get(vals, ptr);
            let idx_val = get(vals, idx);
            let ext = target
                .stack_alloc_ext()
                .expect("PtrOffset: target has no memory model");
            vals[vid.0] = Some(ext.ptr_offset(ptr_val, idx_val));
        }
        Value::BlockAddr { block } => {
            vals[vid.0] = Some(target.block_addr(block_handles[block.0].clone()));
        }
        _ => panic!("lower_vaffle_value: unhandled Value variant — add lowering for this variant"),
    }
}

/// Translate the shared `Stmt` vocabulary (identical to
/// `volar-ir-passes::lower_lir`'s `lower_ir_stmt`, parameterized over
/// `ValueId` instead of `IRVarId`) into `LirTarget` primitive calls.
fn lower_vaffle_stmt<T: LirTarget<Q>, Q: Clone>(
    stmt: &Stmt<ValueId>,
    vals: &[Option<T::Value>],
    types: &TypeTable,
    target: &mut T,
) -> T::Value {
    match stmt {
        Stmt::Const(c, tid) => {
            let lir_ty = type_to_lir(*tid, types);
            target.iconst(lir_ty, c.lo as i64)
        }
        Stmt::Transmute {
            src,
            src_ty,
            dst_ty,
        } => {
            let sv = get(vals, src);
            let src_lir = type_to_lir(*src_ty, types);
            let dst_lir = type_to_lir(*dst_ty, types);
            if dst_lir.bit_width() > src_lir.bit_width() {
                target.zext(sv, dst_lir)
            } else if dst_lir.bit_width() < src_lir.bit_width() {
                target.trunc(sv, dst_lir)
            } else {
                sv
            }
        }
        Stmt::Poly {
            ty,
            coeffs,
            constant,
        } => {
            let lir_ty = type_to_lir(*ty, types);
            let mut acc = target.iconst(lir_ty, (constant.lo & 1) as i64);
            for (varset, &coeff) in coeffs {
                if coeff == 0 {
                    continue;
                }
                let product = varset
                    .iter()
                    .map(|id| get(vals, id))
                    .reduce(|a, b| target.and(a, b))
                    .unwrap_or_else(|| target.iconst(LirType::Bool, 1));
                acc = target.xor(acc, product);
            }
            acc
        }
        Stmt::Rol { src, ty, n } => {
            let sv = get(vals, src);
            let lir_ty = type_to_lir(*ty, types);
            let width = lir_ty.bit_width();
            let n_mod = (*n as u32) % width;
            if n_mod == 0 {
                return sv;
            }
            let shift_l = target.iconst(lir_ty.clone(), n_mod as i64);
            let shift_r = target.iconst(lir_ty, (width - n_mod) as i64);
            let left = target.shl(sv.clone(), shift_l);
            let right = target.lshr(sv, shift_r);
            target.or(left, right)
        }
        Stmt::Ror { src, ty, n } => {
            let sv = get(vals, src);
            let lir_ty = type_to_lir(*ty, types);
            let width = lir_ty.bit_width();
            let n_mod = (*n as u32) % width;
            if n_mod == 0 {
                return sv;
            }
            let shift_r = target.iconst(lir_ty.clone(), n_mod as i64);
            let shift_l = target.iconst(lir_ty, (width - n_mod) as i64);
            let right = target.lshr(sv.clone(), shift_r);
            let left = target.shl(sv, shift_l);
            target.or(right, left)
        }
        Stmt::Merge { parts, ty } => {
            let dst_lir = type_to_lir(*ty, types);
            let elem_bits = dst_lir.bit_width() / parts.len() as u32;
            let mut acc = target.iconst(dst_lir.clone(), 0);
            for (i, part_id) in parts.iter().enumerate() {
                let part = get(vals, part_id);
                let ext = target.zext(part, dst_lir.clone());
                let shift = target.iconst(dst_lir.clone(), (i as u32 * elem_bits) as i64);
                let shifted = target.shl(ext, shift);
                acc = target.or(acc, shifted);
            }
            acc
        }
        Stmt::Splat { src, ty } => {
            let dst_lir = type_to_lir(*ty, types);
            let sv = get(vals, src);
            let src_bits = match &types.0[ty.0 as usize] {
                IrType::Vec(_, elem_tid) => type_bits(*elem_tid, types),
                _ => 1,
            };
            let total_bits = dst_lir.bit_width();
            let ext = target.zext(sv, dst_lir.clone());
            let mut acc = ext.clone();
            let mut pos = src_bits;
            while pos < total_bits {
                let shift = target.iconst(dst_lir.clone(), pos as i64);
                let shifted = target.shl(ext.clone(), shift);
                acc = target.or(acc, shifted);
                pos += src_bits;
            }
            acc
        }
        Stmt::StorageRead { .. } | Stmt::StorageWrite { .. } => {
            unimplemented!("StorageRead/Write require memory ops not yet in LirTarget")
        }
        Stmt::Shuffle { .. } => {
            unimplemented!("Shuffle lowering to LirTarget is not yet implemented")
        }
        _ => panic!("lower_vaffle_stmt: unhandled Stmt variant — add lowering for this variant"),
    }
}

// ============================================================================
// Calls (shared by `Value::Call` and `Terminator::ReturnCall`)
// ============================================================================

fn emit_callee_call<P, T, Q>(
    func: FuncId,
    module: &Module<P>,
    func_names: &[Option<String>],
    arg_lir_tys: &[LirType],
    arg_vals: &[T::Value],
    ret_ty: Option<LirType>,
    target: &mut T,
) -> Vec<T::Value>
where
    P: Clone,
    Q: Clone,
    T: LirTarget<Q>,
{
    match &module.funcs[func.0] {
        FuncDecl::Import { name, .. } => target.call_extern(name, arg_lir_tys, arg_vals, ret_ty),
        FuncDecl::Body(_) => {
            let name = func_names[func.0]
                .as_deref()
                .expect("Body func must have an assigned name");
            target.call(name, arg_lir_tys, arg_vals, ret_ty)
        }
        _ => panic!("emit_callee_call: unhandled FuncDecl variant — add lowering for this variant"),
    }
}

trait SigOf {
    fn sig(&self) -> vaffle::SigId;
}
impl<P: Clone> SigOf for FuncDecl<P> {
    fn sig(&self) -> vaffle::SigId {
        match self {
            FuncDecl::Import { sig, .. } => *sig,
            FuncDecl::Body(b) => b.sig,
            _ => panic!("SigOf: unhandled FuncDecl variant — add handling for this variant"),
        }
    }
}

// ============================================================================
// Terminator lowering
// ============================================================================

fn lower_vaffle_terminator<P, T, Q>(
    term: &Terminator,
    body: &FuncBody<P>,
    module: &Module<P>,
    func_names: &[Option<String>],
    vals: &[Option<T::Value>],
    block_handles: &[T::Block],
    target: &mut T,
) where
    P: Clone,
    Q: Clone,
    T: LirTarget<Q>,
{
    let branch = |t: &VTarget<ValueId>, vals: &[Option<T::Value>]| -> BranchTarget<T::Value> {
        BranchTarget {
            args: t.args.iter().map(|v| get(vals, v)).collect(),
            reentry: t.reentry.clone(),
        }
    };

    match term {
        Terminator::Return { values } => {
            let rets: Vec<T::Value> = values.iter().map(|v| get(vals, v)).collect();
            target.ret(&rets);
        }
        Terminator::Jump(t) => {
            target.jump(block_handles[t.block.0].clone(), branch(t, vals));
        }
        Terminator::IfNonzero {
            cond,
            then_target,
            else_target,
        } => {
            let cond_val = get(vals, cond);
            target.branch(
                cond_val,
                block_handles[then_target.block.0].clone(),
                branch(then_target, vals),
                block_handles[else_target.block.0].clone(),
                branch(else_target, vals),
            );
        }
        Terminator::ReturnCall { func, args } => {
            let arg_vals: Vec<T::Value> = args.iter().map(|v| get(vals, v)).collect();
            let callee_sig = &module.sigs[module.funcs[func.0].sig().0];
            let arg_lir_tys: Vec<LirType> = callee_sig
                .params
                .iter()
                .map(|tid| type_to_lir(*tid, &module.types))
                .collect();
            let ret_ty = results_to_lir(&callee_sig.results, &module.types);
            let results = emit_callee_call(
                *func,
                module,
                func_names,
                &arg_lir_tys,
                &arg_vals,
                ret_ty,
                target,
            );
            target.ret(&results);
        }
        Terminator::Table {
            index,
            targets,
            default_target,
        } => {
            let index_val = get(vals, index);
            let is_block_addr = matches!(&body.values[index.0].kind, Value::BlockAddr { .. });
            if is_block_addr {
                // VAFFLE's own convention (see `Value::BlockAddr`'s doc):
                // a dynamic jump is a `Table` whose index traces back to a
                // `BlockAddr` value. `LirTarget::dyn_jump` requires one
                // uniform `BranchTarget` for every destination (matching
                // LLVM's `indirectbr`/PHI model), whereas `Table` allows
                // per-target args — require them to agree here, which any
                // real `blockaddress`-style producer naturally satisfies.
                let default_args = branch(default_target, vals);
                let mut destinations = alloc::vec![block_handles[default_target.block.0].clone()];
                for t in targets {
                    let this_args = branch(t, vals);
                    assert_eq!(
                        this_args, default_args,
                        "dyn_jump: VAFFLE Table args must be uniform across all destinations \
                         when index derives from BlockAddr",
                    );
                    destinations.push(block_handles[t.block.0].clone());
                }
                target.dyn_jump(index_val, &destinations, default_args);
            } else {
                // Ordinary data-driven dispatch: `Table` is POSITIONAL
                // (`targets[idx]`, or `default_target` when `idx` is out of
                // range — VAFFLE's `Terminator::Table` fuzz-interpreter
                // semantics), unlike `LirTarget::switch`'s KEYED cases, so
                // key each case on its own position.
                let mut cases = Vec::with_capacity(targets.len());
                for (i, t) in targets.iter().enumerate() {
                    cases.push((i as i64, block_handles[t.block.0].clone(), branch(t, vals)));
                }
                target.switch(
                    index_val,
                    &cases,
                    block_handles[default_target.block.0].clone(),
                    branch(default_target, vals),
                );
            }
        }
        _ => panic!(
            "lower_vaffle_terminator: unhandled Terminator variant — add lowering for this variant"
        ),
    }
}
