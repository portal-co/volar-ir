// @reliability: normal
//! Lowering passes from the circuit IRs (`BIrBlocks`, `IRBlocks`) to `LirTarget`.

use alloc::{boxed::Box, collections::BTreeMap, vec, vec::Vec};
use volar_ir_config::IrLoweringConfig;
use volar_lir::{ActionStoreTarget, BranchTarget, LirTarget, LirType};
use volar_provenance::ProvenanceHandler;

use volar_ir::{
    boolar::{BIrBlocks, BIrStmt, BIrTarget, BIrTerminator},
    ir::{IRBlockTargetId, IRBlocks, IRBranchTarget, IRStmt, IRTerminator, IRType, IRTypes},
};
use volar_ir_common::Type;

// ============================================================================
// BIrBlocks → LirTarget
// ============================================================================

/// Lower a boolean circuit (`BIrBlocks<P>`) to any `LirTarget<P>`.
///
/// All values are `LirType::Bool`. The entry block's parameter count determines
/// the function signature. Multiple outputs are packed into a `U64` via
/// shl/or/zext; a single output is returned directly.
///
/// Before each statement, `target.set_prov(prov)` is called with that
/// statement's provenance from `block.stmt_provs`.
pub fn lower_biir<P: Clone, T: LirTarget<P>>(blocks: &BIrBlocks<P>, name: &str, target: &mut T) {
    lower_biir_with_handler(blocks, name, target, &volar_provenance::KeepProvenance)
}

/// Lower a boolean circuit with provenance mapping.
///
/// Like [`lower_biir`], but uses `handler` to convert the circuit's
/// provenance type `P` into the target's provenance type `H::Output`
/// before each `set_prov` call.
pub fn lower_biir_with_handler<P, T, H>(
    blocks: &BIrBlocks<P>,
    name: &str,
    target: &mut T,
    handler: &H,
) where
    P: Clone,
    H: ProvenanceHandler<P>,
    T: LirTarget<H::Output>,
{
    let entry_block = &blocks.blocks[0];
    let num_inputs = entry_block.params as usize;

    // Create all LIR block handles up front (forward declarations).
    let mut block_handles: Vec<T::Block> = Vec::with_capacity(blocks.blocks.len());
    // begin_function creates the entry block for us; create extra blocks here temporarily.
    // We'll call begin_function below, which gives us the true entry block.
    // For blocks beyond the first we pre-allocate after begin_function.

    let input_tys: Vec<LirType> = (0..num_inputs).map(|_| LirType::Bool).collect();
    let (entry_handle, entry_param_groups) =
        target.begin_function(name, &input_tys, Some(LirType::U64));
    // Each param is scalar (Bool), so each group has exactly one value.
    let entry_params: Vec<T::Value> = entry_param_groups
        .into_iter()
        .map(|g| g.into_iter().next().unwrap())
        .collect();
    block_handles.push(entry_handle);

    // Pre-create handles for all blocks after the entry.
    for _ in 1..blocks.blocks.len() {
        block_handles.push(target.create_block());
    }

    // Add block params for blocks beyond entry (entry params are function params).
    // vals_per_block[b] stores the values for block b: [param0, param1, ..., stmt0, stmt1, ...]
    let mut vals_per_block: Vec<Vec<T::Value>> = Vec::with_capacity(blocks.blocks.len());
    vals_per_block.push(entry_params); // entry block's params = function params

    for (bi, block) in blocks.blocks.iter().enumerate().skip(1) {
        let mut block_vals: Vec<T::Value> = Vec::with_capacity(block.params as usize);
        for _ in 0..block.params {
            let v = target.add_block_param(block_handles[bi].clone(), LirType::Bool);
            block_vals.push(v);
        }
        vals_per_block.push(block_vals);
    }

    // Emit each block.
    for (bi, block) in blocks.blocks.iter().enumerate() {
        target.switch_to_block(block_handles[bi].clone());

        let vals = &mut vals_per_block[bi];
        // Reserve space for stmt results.
        for stmt in block.stmts.iter() {
            target.set_prov(handler.map(&stmt.prov));
            let v = lower_biir_stmt(&stmt.kind, vals, target);
            vals.push(v);
        }

        // Emit terminator.
        // We need to look up vals immutably, but we already have &mut vals_per_block.
        // Clone the vals for this block to avoid borrow issues.
        let block_vals = vals_per_block[bi].clone();
        lower_biir_terminator(&block.terminator, &block_vals, &block_handles, target);
    }

    target.end_function();
}

fn lower_biir_stmt<Q: Clone, T: LirTarget<Q>>(
    stmt: &BIrStmt,
    vals: &[T::Value],
    target: &mut T,
) -> T::Value {
    match stmt {
        BIrStmt::Zero => target.iconst(LirType::Bool, 0),
        BIrStmt::One => target.iconst(LirType::Bool, 1),
        BIrStmt::And(a, b) => {
            let va = vals[a.0 as usize].clone();
            let vb = vals[b.0 as usize].clone();
            target.and(va, vb)
        }
        BIrStmt::Or(a, b) => {
            let va = vals[a.0 as usize].clone();
            let vb = vals[b.0 as usize].clone();
            target.or(va, vb)
        }
        BIrStmt::Xor(a, b) => {
            let va = vals[a.0 as usize].clone();
            let vb = vals[b.0 as usize].clone();
            target.xor(va, vb)
        }
        BIrStmt::Not(a) => {
            let va = vals[a.0 as usize].clone();
            target.not(va)
        }
        BIrStmt::OracleBit {
            name,
            args,
            bit,
            occurrence,
        } => {
            let args = args
                .iter()
                .map(|id| vals[id.0 as usize].clone())
                .collect::<Vec<_>>();
            target.oracle_bit(name, &args, *bit, *occurrence)
        }
        BIrStmt::RngBit {
            name,
            bit,
            occurrence,
        } => target.rng_bit(name, *bit, *occurrence),
        BIrStmt::ActionStoreBit {
            name,
            guard,
            args,
            fallback,
            storage,
            lane,
            addr,
            bit,
            occurrence,
        } => {
            let args = args
                .iter()
                .map(|id| vals[id.0 as usize].clone())
                .collect::<Vec<_>>();
            let address = addr
                .iter()
                .map(|id| vals[id.0 as usize].clone())
                .collect::<Vec<_>>();
            let address = pack_bits_to_u64(&address, target);
            target.action_store_bit(
                name,
                vals[guard.0 as usize].clone(),
                &args,
                vals[fallback.0 as usize].clone(),
                storage.0 as u64,
                lane.0,
                address,
                *bit,
                *occurrence,
            );
            // Preserve Boolar's positional SSA convention for an effect-only
            // statement. Its value is intentionally unusable data.
            target.iconst(LirType::Bool, 0)
        }
        BIrStmt::OracleCall { .. }
        | BIrStmt::OracleProjectedBit { .. }
        | BIrStmt::ActionCall { .. }
        | BIrStmt::ActionBit { .. }
        | BIrStmt::Rng { .. }
        | BIrStmt::StorageRead { .. }
        | BIrStmt::StorageWrite { .. } => {
            unimplemented!(
                "lower_biir_stmt: extended BIrStmt variants not supported in plain LIR lowering"
            )
        }
        _ => panic!("lower_biir_stmt: unhandled BIrStmt variant — add lowering for this variant"),
    }
}

fn lower_biir_terminator<Q: Clone, T: LirTarget<Q>>(
    term: &BIrTerminator,
    vals: &[T::Value],
    block_handles: &[T::Block],
    target: &mut T,
) {
    match term {
        BIrTerminator::Jmp(tgt) => {
            lower_biir_jump(tgt, vals, block_handles, target);
        }
        BIrTerminator::CondJmp {
            val,
            then_target,
            else_target,
        } => {
            let cond = vals[val.0 as usize].clone();
            let (then_block, then_args) =
                resolve_biir_target::<Q, T>(then_target, vals, block_handles);
            let (else_block, else_args) =
                resolve_biir_target::<Q, T>(else_target, vals, block_handles);
            match (then_block, else_block) {
                (Some(tb), Some(eb)) => {
                    target.branch(
                        cond,
                        tb,
                        BranchTarget::args(then_args),
                        eb,
                        BranchTarget::args(else_args),
                    );
                }
                _ => unimplemented!("CondJmp with Return/Dyn target"),
            }
        }
        _ => panic!(
            "lower_biir_terminator: unhandled BIrTerminator variant — add lowering for this variant"
        ),
    }
}

fn lower_biir_jump<P: Clone, T: LirTarget<P>>(
    tgt: &BIrTarget,
    vals: &[T::Value],
    block_handles: &[T::Block],
    target: &mut T,
) {
    match &tgt.block {
        IRBlockTargetId::Return => {
            let args: Vec<T::Value> = tgt
                .args
                .iter()
                .map(|id| vals[id.0 as usize].clone())
                .collect();
            let ret_val = pack_bits_to_u64(&args, target);
            target.ret(&[ret_val]);
        }
        IRBlockTargetId::Block(id) => {
            let args: Vec<T::Value> = tgt
                .args
                .iter()
                .map(|id| vals[id.0 as usize].clone())
                .collect();
            target.jump(
                block_handles[id.0 as usize].clone(),
                BranchTarget::args(args.clone()),
            );
        }
        IRBlockTargetId::Dyn(_) => unimplemented!("dynamic jump target in BIrBlocks lowering"),
        _ => panic!(
            "lower_biir_jump: unhandled IRBlockTargetId variant — add lowering for this variant"
        ),
    }
}

fn resolve_biir_target<Q: Clone, T: LirTarget<Q>>(
    tgt: &BIrTarget,
    vals: &[T::Value],
    block_handles: &[T::Block],
) -> (Option<T::Block>, Vec<T::Value>) {
    let args: Vec<T::Value> = tgt
        .args
        .iter()
        .map(|id| vals[id.0 as usize].clone())
        .collect();
    match &tgt.block {
        IRBlockTargetId::Block(id) => (Some(block_handles[id.0 as usize].clone()), args),
        IRBlockTargetId::Return => (None, args),
        IRBlockTargetId::Dyn(_) => unimplemented!("dynamic jump target in BIrBlocks lowering"),
        _ => panic!(
            "resolve_biir_target: unhandled IRBlockTargetId variant — add handling for this variant"
        ),
    }
}

/// Pack a slice of Bool values into a single U64 via shl/or/zext.
/// For a single value, just zext it.
fn pack_bits_to_u64<P: Clone, T: LirTarget<P>>(bits: &[T::Value], target: &mut T) -> T::Value {
    if bits.is_empty() {
        return target.iconst(LirType::U64, 0);
    }
    if bits.len() == 1 {
        return target.zext(bits[0].clone(), LirType::U64);
    }
    let mut acc = target.zext(bits[0].clone(), LirType::U64);
    for (i, bit) in bits.iter().enumerate().skip(1) {
        let ext = target.zext(bit.clone(), LirType::U64);
        let shift = target.iconst(LirType::U64, i as i64);
        let shifted = target.shl(ext, shift);
        acc = target.or(acc, shifted);
    }
    acc
}

// ============================================================================
// IRBlocks + IRTypes → LirTarget
// ============================================================================

/// Lower typed Volar IR (`IRBlocks`) to any `LirTarget`.
///
/// Forwards provenance unchanged (equivalent to `KeepProvenance` handler).
pub fn lower_ir<P: Clone, T: LirTarget<P>>(
    blocks: &IRBlocks<P>,
    types: &IRTypes,
    name: &str,
    target: &mut T,
) {
    lower_ir_with_handler(
        blocks,
        types,
        name,
        target,
        &volar_provenance::KeepProvenance,
        &IrLoweringConfig::default(),
    )
}

/// Lower typed Volar IR (`IRBlocks<P>`) to any `LirTarget<H::Output>`
/// with provenance mapping.
pub fn lower_ir_with_handler<P, T, H>(
    blocks: &IRBlocks<P>,
    types: &IRTypes,
    name: &str,
    target: &mut T,
    handler: &H,
    _config: &IrLoweringConfig,
) where
    P: Clone,
    H: ProvenanceHandler<P>,
    T: LirTarget<H::Output>,
{
    let entry = &blocks.blocks[0];

    // Compute the declared type of every SSA value before emitting anything.
    // External calls must preserve their real input ABI, rather than treating
    // every value as the historical U64 placeholder.
    let block_var_tys: Vec<Vec<Option<volar_ir::ir::IRTypeId>>> =
        blocks.blocks.iter().map(infer_block_var_types).collect();

    // Map entry block param types to LirType.
    let input_tys: Vec<LirType> = entry
        .params
        .iter()
        .map(|tid| ir_type_to_lir(&types.0[tid.0 as usize], types))
        .collect();

    // Preserve a single typed Volar return directly.  The legacy U64 packing
    // remains only for the older multi-bit/multi-output return convention;
    // native vector and packed-256 values must never flow through that path.
    let direct_return_ty =
        typed_return_type(blocks, &block_var_tys, types).filter(requires_native_return_abi);
    let ret_ty = Some(direct_return_ty.clone().unwrap_or(LirType::U64));

    let (entry_handle, entry_param_groups) = target.begin_function(name, &input_tys, ret_ty);
    // All params are scalar types, so each group has exactly one value.
    let entry_params: Vec<T::Value> = entry_param_groups
        .into_iter()
        .map(|g| g.into_iter().next().unwrap())
        .collect();

    let mut block_handles: Vec<T::Block> = vec![entry_handle];
    for _ in 1..blocks.blocks.len() {
        block_handles.push(target.create_block());
    }

    // Param arity per block, used to filter candidate destinations for
    // `IRBlockTargetId::Dyn` jumps (see `lower_ir_terminator`).
    let block_param_counts: Vec<usize> = blocks.blocks.iter().map(|b| b.params.len()).collect();

    // vals_per_block: for each block, vec of (var_id → value) in order
    let mut vals_per_block: Vec<Vec<T::Value>> = Vec::with_capacity(blocks.blocks.len());
    vals_per_block.push(entry_params);

    for (bi, block) in blocks.blocks.iter().enumerate().skip(1) {
        let mut bv: Vec<T::Value> = Vec::new();
        for tid in &block.params {
            let lir_ty = ir_type_to_lir(&types.0[tid.0 as usize], types);
            bv.push(target.add_block_param(block_handles[bi].clone(), lir_ty));
        }
        vals_per_block.push(bv);
    }

    for (bi, block) in blocks.blocks.iter().enumerate() {
        target.switch_to_block(block_handles[bi].clone());

        // Side table for multi-output call results (OracleCall / ActionCall).
        // Keyed by the *Call stmt's var index within this block.
        let mut multi_results: BTreeMap<usize, Vec<T::Value>> = BTreeMap::new();

        for stmt in block.stmts.iter() {
            target.set_prov(handler.map(&stmt.prov));
            let var_idx = vals_per_block[bi].len(); // index of this stmt's result
            match &stmt.kind {
                // ---- Oracle: emit target.oracle(), stash results ------------
                IRStmt::OracleCall {
                    name,
                    args,
                    output_tys,
                    ..
                } => {
                    let arg_vals: Vec<T::Value> = args
                        .iter()
                        .map(|id| vals_per_block[bi][id.0 as usize].clone())
                        .collect();
                    let arg_lir_tys = lir_types_for_vars(args, &block_var_tys[bi], types);
                    let ret_tys: Vec<LirType> = output_tys
                        .iter()
                        .map(|tid| ir_type_to_lir(&types.0[tid.0 as usize], types))
                        .collect();
                    let results = target.oracle(name, &arg_lir_tys, &arg_vals, &ret_tys);
                    multi_results.insert(var_idx, results);
                    // Push a dummy placeholder for the tuple-typed call var.
                    vals_per_block[bi].push(target.iconst(LirType::Bool, 0));
                }
                IRStmt::OracleOutput { call, idx, .. } => {
                    let call_idx = call.0 as usize;
                    let results = multi_results
                        .get(&call_idx)
                        .expect("OracleOutput: no stashed results for OracleCall");
                    vals_per_block[bi].push(results[*idx].clone());
                }
                // ---- Action: emit target.action(), stash results -----------
                IRStmt::ActionCall {
                    name,
                    guard,
                    args,
                    fallbacks,
                    output_tys,
                    ..
                } => {
                    let guard_val = vals_per_block[bi][guard.0 as usize].clone();
                    let arg_vals: Vec<T::Value> = args
                        .iter()
                        .map(|id| vals_per_block[bi][id.0 as usize].clone())
                        .collect();
                    let fallback_vals: Vec<T::Value> = fallbacks
                        .iter()
                        .map(|id| vals_per_block[bi][id.0 as usize].clone())
                        .collect();
                    let arg_lir_tys = lir_types_for_vars(args, &block_var_tys[bi], types);
                    let ret_tys: Vec<LirType> = output_tys
                        .iter()
                        .map(|tid| ir_type_to_lir(&types.0[tid.0 as usize], types))
                        .collect();
                    let results = target.action(
                        name,
                        guard_val,
                        &arg_lir_tys,
                        &arg_vals,
                        &fallback_vals,
                        &ret_tys,
                    );
                    multi_results.insert(var_idx, results);
                    vals_per_block[bi].push(target.iconst(LirType::Bool, 0));
                }
                IRStmt::ActionOutput { call, idx, .. } => {
                    let call_idx = call.0 as usize;
                    let results = multi_results
                        .get(&call_idx)
                        .expect("ActionOutput: no stashed results for ActionCall");
                    vals_per_block[bi].push(results[*idx].clone());
                }
                // ---- Direct action storage --------------------------------
                IRStmt::ActionStore {
                    name,
                    guard,
                    args,
                    fallbacks,
                    output_tys,
                    targets,
                } => {
                    let guard = vals_per_block[bi][guard.0 as usize].clone();
                    let args: Vec<T::Value> = args
                        .iter()
                        .map(|id| vals_per_block[bi][id.0 as usize].clone())
                        .collect();
                    let fallbacks: Vec<T::Value> = fallbacks
                        .iter()
                        .map(|id| vals_per_block[bi][id.0 as usize].clone())
                        .collect();
                    let arg_lir_tys = lir_types_for_vars(
                        match &stmt.kind {
                            IRStmt::ActionStore { args, .. } => args,
                            _ => unreachable!(),
                        },
                        &block_var_tys[bi],
                        types,
                    );
                    let ret_tys: Vec<LirType> = output_tys
                        .iter()
                        .map(|tid| ir_type_to_lir(&types.0[tid.0 as usize], types))
                        .collect();
                    let target_values: Vec<ActionStoreTarget<T::Value>> = targets
                        .iter()
                        .map(|target| {
                            let address_ty =
                                lir_type_for_var(target.addr, &block_var_tys[bi], types);
                            ActionStoreTarget {
                                storage: target.storage.0 as u64,
                                lane: 0,
                                address: vals_per_block[bi][target.addr.0 as usize].clone(),
                                address_ty,
                            }
                        })
                        .collect();
                    target.action_store(
                        name,
                        guard,
                        &arg_lir_tys,
                        &args,
                        &fallbacks,
                        &ret_tys,
                        &target_values,
                    );
                    // Effect-only statements have no valid SSA result.  Keep
                    // a private placeholder solely to retain the IR's stable
                    // variable numbering; type validation forbids consuming it.
                    vals_per_block[bi].push(target.iconst(LirType::Bool, 0));
                }
                // ---- Rng: emit target.rng() --------------------------------
                IRStmt::Rng { name, ty } => {
                    let lir_ty = ir_type_to_lir(&types.0[ty.0 as usize], types);
                    vals_per_block[bi].push(target.rng_named(name, lir_ty));
                }
                // ---- All other stmts: existing lowering --------------------
                other => {
                    let v = lower_ir_stmt(other, &vals_per_block[bi], types, target);
                    vals_per_block[bi].push(v);
                }
            }
        }

        let block_vals = vals_per_block[bi].clone();
        lower_ir_terminator(
            &block.terminator,
            &block_vals,
            &block_handles,
            &block_param_counts,
            direct_return_ty.is_some(),
            target,
        );
    }

    target.end_function();
}

/// Compute the bit-width of an IR type.  Recurses into `Vec` element types.
fn ir_type_bits(ty: &IRType, types: &IRTypes) -> u32 {
    match ty {
        IRType::Primitive(Type::Bit) => 1,
        IRType::Primitive(Type::_8) | IRType::Primitive(Type::AES8) => 8,
        IRType::Primitive(Type::_16) => 16,
        IRType::Primitive(Type::_32) => 32,
        IRType::Primitive(Type::_64) | IRType::Primitive(Type::Galois64) => 64,
        IRType::Primitive(Type::_128) => 128,
        IRType::Primitive(Type::_256) => 256,
        IRType::Vec(n, elem_tid) => {
            (*n as u32) * ir_type_bits(&types.0[elem_tid.0 as usize], types)
        }
        other => unimplemented!("ir_type_bits: unsupported type {:?}", other),
    }
}

fn ir_type_to_lir(ty: &IRType, types: &IRTypes) -> LirType {
    match ty {
        IRType::Primitive(Type::Bit) => LirType::Bool,
        IRType::Primitive(Type::_8) => LirType::U8,
        IRType::Primitive(Type::_16) => LirType::U16,
        IRType::Primitive(Type::_32) => LirType::U32,
        IRType::Primitive(Type::_64) => LirType::U64,
        IRType::Primitive(Type::_128) => LirType::U128,
        IRType::Primitive(Type::_256) => LirType::U256,
        IRType::Primitive(Type::AES8) => LirType::Native(Type::AES8),
        IRType::Primitive(Type::Galois64) => LirType::Native(Type::Galois64),
        IRType::Primitive(Type::Z3) => {
            panic!("ir_type_to_lir: Z3 values cannot lower through the GF(2) native targets")
        }
        IRType::Vec(lanes, element) => LirType::Vector(
            Box::new(ir_type_to_lir(&types.0[element.0 as usize], types)),
            *lanes,
        ),
        IRType::Tuple(_) => {
            panic!("ir_type_to_lir: tuple lowering requires target struct registration")
        }
        other => panic!("ir_type_to_lir: unsupported Volar IR type {other:?}"),
    }
}

/// Type of an SSA result when lowering directly to LIR.
///
/// Effect-only instructions deliberately return `None`: retaining that fact
/// here prevents a later external call from silently accepting their dummy
/// positional placeholder as a U64 argument.
fn lir_stmt_result_type(stmt: &IRStmt) -> Option<volar_ir::ir::IRTypeId> {
    match stmt {
        IRStmt::StorageRead { ty, .. }
        | IRStmt::Const(_, ty)
        | IRStmt::Rol { ty, .. }
        | IRStmt::Ror { ty, .. }
        | IRStmt::Merge { ty, .. }
        | IRStmt::Splat { ty, .. }
        | IRStmt::Shuffle { ty, .. }
        | IRStmt::OracleOutput { ty, .. }
        | IRStmt::ActionOutput { ty, .. }
        | IRStmt::Rng { ty, .. }
        | IRStmt::Poly { ty, .. } => Some(*ty),
        IRStmt::Transmute { dst_ty, .. } => Some(*dst_ty),
        IRStmt::OracleCall { result_ty, .. } | IRStmt::ActionCall { result_ty, .. } => {
            Some(*result_ty)
        }
        IRStmt::StorageWrite { .. } | IRStmt::ActionStore { .. } => None,
        _ => None,
    }
}

fn infer_block_var_types<P: Clone>(
    block: &volar_ir::ir::IRBlock<P>,
) -> Vec<Option<volar_ir::ir::IRTypeId>> {
    let mut result = block.params.iter().copied().map(Some).collect::<Vec<_>>();
    result.extend(
        block
            .stmts
            .iter()
            .map(|stmt| lir_stmt_result_type(&stmt.kind)),
    );
    result
}

fn lir_type_for_var(
    var: volar_ir::ir::IRVarId,
    var_tys: &[Option<volar_ir::ir::IRTypeId>],
    types: &IRTypes,
) -> LirType {
    let tid = var_tys
        .get(var.0 as usize)
        .and_then(|ty| *ty)
        .unwrap_or_else(|| panic!("LIR lowering: SSA value {} has no data type", var.0));
    ir_type_to_lir(&types.0[tid.0 as usize], types)
}

fn lir_types_for_vars(
    vars: &[volar_ir::ir::IRVarId],
    var_tys: &[Option<volar_ir::ir::IRTypeId>],
    types: &IRTypes,
) -> Vec<LirType> {
    vars.iter()
        .copied()
        .map(|var| lir_type_for_var(var, var_tys, types))
        .collect()
}

fn typed_single_return<P: Clone>(
    block: &volar_ir::ir::IRBlock<P>,
    var_tys: &[Option<volar_ir::ir::IRTypeId>],
    types: &IRTypes,
) -> Option<LirType> {
    let IRTerminator::Jmp { target } = &block.terminator else {
        return None;
    };
    if !matches!(target.dest, IRBlockTargetId::Return) || target.args.len() != 1 {
        return None;
    }
    Some(lir_type_for_var(target.args[0], var_tys, types))
}

/// Return a native LIR type only when every `return` edge has exactly one
/// value of that same type.  A function has one ABI return declaration, so it
/// would be invalid to decide per basic block (and would accidentally make a
/// multi-block U64 function emit `ret i8`, for example).
fn typed_return_type<P: Clone>(
    blocks: &IRBlocks<P>,
    block_var_tys: &[Vec<Option<volar_ir::ir::IRTypeId>>],
    types: &IRTypes,
) -> Option<LirType> {
    let mut result: Option<LirType> = None;
    for (block, var_tys) in blocks.blocks.iter().zip(block_var_tys) {
        let Some(ty) = typed_single_return(block, var_tys, types) else {
            if matches!(
                block.terminator,
                IRTerminator::Jmp {
                    target: IRBranchTarget {
                        dest: IRBlockTargetId::Return,
                        ..
                    }
                }
            ) {
                return None;
            }
            continue;
        };
        match &result {
            Some(existing) if existing != &ty => return None,
            Some(_) => {}
            None => result = Some(ty),
        }
    }
    result
}

/// The historical direct-IR entry-point ABI returns narrow scalar values in
/// a zero-extended `U64`.  Keep that compatibility for existing hosts while
/// using a real native return for values that cannot be represented there.
fn requires_native_return_abi(ty: &LirType) -> bool {
    matches!(
        ty,
        LirType::I128 | LirType::U128 | LirType::I256 | LirType::U256 | LirType::Vector(_, _)
    )
}

fn lower_ir_stmt<Q: Clone, T: LirTarget<Q>>(
    stmt: &IRStmt,
    vals: &[T::Value],
    types: &IRTypes,
    target: &mut T,
) -> T::Value {
    match stmt {
        IRStmt::Const(c, tid) => {
            let lir_ty = ir_type_to_lir(&types.0[tid.0 as usize], types);
            target.iconst(lir_ty, c.lo as i64)
        }
        IRStmt::Transmute {
            src,
            src_ty,
            dst_ty,
        } => {
            let sv = vals[src.0 as usize].clone();
            let src_lir = ir_type_to_lir(&types.0[src_ty.0 as usize], types);
            let dst_lir = ir_type_to_lir(&types.0[dst_ty.0 as usize], types);
            if dst_lir.bit_width() > src_lir.bit_width() {
                target.zext(sv, dst_lir)
            } else if dst_lir.bit_width() < src_lir.bit_width() {
                target.trunc(sv, dst_lir)
            } else {
                sv
            }
        }
        IRStmt::Poly {
            ty,
            coeffs,
            constant,
        } => {
            // Use the declared output type to determine LIR type.
            let lir_ty = ir_type_to_lir(&types.0[ty.0 as usize], types);
            let mut acc = target.iconst(lir_ty, (constant.lo & 1) as i64);
            for (vars, &coeff) in coeffs {
                if coeff == 0 {
                    continue;
                }
                let product = vars
                    .iter()
                    .map(|id| vals[id.0 as usize].clone())
                    .reduce(|a, b| target.and(a, b))
                    .unwrap_or_else(|| target.iconst(LirType::Bool, 1));
                acc = target.xor(acc, product);
            }
            acc
        }
        IRStmt::Rol { src, ty, n } => {
            let sv = vals[src.0 as usize].clone();
            let lir_ty = ir_type_to_lir(&types.0[ty.0 as usize], types);
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
        IRStmt::Ror { src, ty, n } => {
            let sv = vals[src.0 as usize].clone();
            let lir_ty = ir_type_to_lir(&types.0[ty.0 as usize], types);
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
        IRStmt::Merge { parts, ty } => {
            let dst_lir = ir_type_to_lir(&types.0[ty.0 as usize], types);
            let elem_bits = dst_lir.bit_width() / parts.len() as u32;
            let mut acc = target.iconst(dst_lir.clone(), 0);
            for (i, part_id) in parts.iter().enumerate() {
                let part = vals[part_id.0 as usize].clone();
                let ext = target.zext(part, dst_lir.clone());
                let shift = target.iconst(dst_lir.clone(), (i as u32 * elem_bits) as i64);
                let shifted = target.shl(ext, shift);
                acc = target.or(acc, shifted);
            }
            acc
        }
        IRStmt::Splat { src, ty } => {
            let dst_lir = ir_type_to_lir(&types.0[ty.0 as usize], types);
            let sv = vals[src.0 as usize].clone();
            // Compute source element bit-width from the Vec's element type.
            // For Vec(n, elem_ty): src_bits = bits(elem_ty).
            // For non-Vec dst, fall back to 1-bit broadcast.
            let src_bits = match &types.0[ty.0 as usize] {
                IRType::Vec(_, elem_tid) => ir_type_bits(&types.0[elem_tid.0 as usize], types),
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
        IRStmt::StorageRead { .. } | IRStmt::StorageWrite { .. } => {
            unimplemented!("StorageRead/Write require memory ops not yet in LirTarget")
        }
        IRStmt::Shuffle { .. } => {
            unimplemented!("Shuffle lowering to LirTarget is not yet implemented")
        }
        // ---- External access primitives (stubs) ----------------------------
        // Full implementation requires the deferred-value strategy described in
        // the external-primitives plan (docs/external-primitives-plan.md §7.2).
        IRStmt::OracleCall { .. } | IRStmt::OracleOutput { .. } => {
            unimplemented!("OracleCall/OracleOutput lowering to LirTarget not yet implemented")
        }
        IRStmt::ActionCall { .. } | IRStmt::ActionOutput { .. } => {
            unimplemented!("ActionCall/ActionOutput lowering to LirTarget not yet implemented")
        }
        IRStmt::Rng { .. } => {
            unimplemented!("Rng lowering to LirTarget not yet implemented")
        }
        _ => panic!("lower_ir_stmt: unhandled IRStmt variant — add lowering for this variant"),
    }
}

fn lower_ir_terminator<Q: Clone, T: LirTarget<Q>>(
    term: &IRTerminator,
    vals: &[T::Value],
    block_handles: &[T::Block],
    block_param_counts: &[usize],
    direct_typed_return: bool,
    target: &mut T,
) {
    match term {
        IRTerminator::Jmp {
            target: jump_target,
        } => {
            let arg_vals: Vec<T::Value> = jump_target
                .args
                .iter()
                .map(|id| vals[id.0 as usize].clone())
                .collect();
            match &jump_target.dest {
                IRBlockTargetId::Return => {
                    if direct_typed_return {
                        target.ret(&arg_vals);
                    } else {
                        let ret = pack_bits_to_u64(&arg_vals, target);
                        target.ret(&[ret]);
                    }
                }
                IRBlockTargetId::Block(id) => {
                    target.jump(
                        block_handles[id.0 as usize].clone(),
                        BranchTarget::args(arg_vals),
                    );
                }
                IRBlockTargetId::Dyn(v) => {
                    lower_dyn_jump(
                        vals[v.0 as usize].clone(),
                        arg_vals,
                        block_handles,
                        block_param_counts,
                        target,
                    );
                }
                _ => panic!(
                    "lower_ir_terminator: unhandled IRBlockTargetId variant — add lowering for this variant"
                ),
            }
        }
        IRTerminator::JumpCond {
            condition,
            then_target,
            else_target,
        } => {
            let cond = vals[condition.0 as usize].clone();
            let true_vals: Vec<T::Value> = then_target
                .args
                .iter()
                .map(|id| vals[id.0 as usize].clone())
                .collect();
            let false_vals: Vec<T::Value> = else_target
                .args
                .iter()
                .map(|id| vals[id.0 as usize].clone())
                .collect();
            match (&then_target.dest, &else_target.dest) {
                (IRBlockTargetId::Block(t), IRBlockTargetId::Block(f)) => {
                    target.branch(
                        cond,
                        block_handles[t.0 as usize].clone(),
                        BranchTarget::args(true_vals),
                        block_handles[f.0 as usize].clone(),
                        BranchTarget::args(false_vals),
                    );
                }
                _ => unimplemented!("JumpCond with Return/Dyn target"),
            }
        }
        IRTerminator::JumpTable { index, cases } => {
            let idx_val = vals[index.0 as usize].clone();
            let mut iter = cases.iter();
            let (_, default_target) = iter
                .next()
                .unwrap_or_else(|| panic!("JumpTable with no cases: no valid destination"));
            let (default_block, default_args) =
                resolve_ir_block_target::<Q, T>(default_target, vals, block_handles);
            let mut switch_cases = Vec::with_capacity(cases.len().saturating_sub(1));
            for (key, t) in iter {
                let (blk, args) = resolve_ir_block_target::<Q, T>(t, vals, block_handles);
                switch_cases.push((key.lo as i64, blk, BranchTarget::args(args)));
            }
            target.switch(
                idx_val,
                &switch_cases,
                default_block,
                BranchTarget::args(default_args),
            );
        }
        _ => panic!(
            "lower_ir_terminator: unhandled IRTerminator variant — add lowering for this variant"
        ),
    }
}

fn resolve_ir_block_target<Q: Clone, T: LirTarget<Q>>(
    t: &IRBranchTarget,
    vals: &[T::Value],
    block_handles: &[T::Block],
) -> (T::Block, Vec<T::Value>) {
    let args: Vec<T::Value> = t
        .args
        .iter()
        .map(|id| vals[id.0 as usize].clone())
        .collect();
    match &t.dest {
        IRBlockTargetId::Block(id) => (block_handles[id.0 as usize].clone(), args),
        _ => unimplemented!("JumpTable case with Return/Dyn target"),
    }
}

/// Lower an `IRBlockTargetId::Dyn` jump to `LirTarget::switch`.
///
/// Volar IR has no `blockaddress`-equivalent value producer (unlike VAFFLE's
/// `Value::BlockAddr`), so a `Dyn` jump's underlying value carries no static
/// destination list — it is typically reconstructed via a `StorageRead` from
/// the software call-stack (see `volar-vaffle-target`'s CPS lowering), which
/// pure dataflow tracing cannot see through. Rather than a real points-to
/// analysis (out of scope here — see Phase 5's DFA jump threading for that
/// kind of precise recovery), this conservatively treats every non-entry
/// block whose parameter arity matches the jump's argument count as a
/// possible destination. This is sound (Volar IR block ids are
/// function-scoped, so any valid Dyn jump target lives in this same
/// function's block list) but not tight — a function with many
/// same-arity blocks gets a proportionally large `switch`.
fn lower_dyn_jump<Q: Clone, T: LirTarget<Q>>(
    index: T::Value,
    args: Vec<T::Value>,
    block_handles: &[T::Block],
    block_param_counts: &[usize],
    target: &mut T,
) {
    let arg_count = args.len();
    // Block 0 is the function's entry block, which LLVM (and this crate's
    // own convention) never allows as a jump target.
    let candidates: Vec<usize> = block_param_counts
        .iter()
        .enumerate()
        .skip(1)
        .filter(|&(_, &n)| n == arg_count)
        .map(|(i, _)| i)
        .collect();
    assert!(
        !candidates.is_empty(),
        "dynamic jump: no non-entry block in this function has {} params to match the jump's argument count",
        arg_count,
    );
    let default_bi = candidates[0];
    let mut switch_cases = Vec::with_capacity(candidates.len().saturating_sub(1));
    for &bi in &candidates[1..] {
        switch_cases.push((
            bi as i64,
            block_handles[bi].clone(),
            BranchTarget::args(args.clone()),
        ));
    }
    target.switch(
        index,
        &switch_cases,
        block_handles[default_bi].clone(),
        BranchTarget::args(args),
    );
}
