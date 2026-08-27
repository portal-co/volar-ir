// @reliability: experimental
// @experimental-status: design
// @ai: unreviewed
//! Lowering pass: Volar IR (`IRBlocks`) → Boolar IR (`BIrBlocks`).
//!
//! Expands each Volar IR statement into zero or more Boolar (boolean-gate)
//! statements.  Multi-bit values are decomposed into individual bit wires
//! (LSB-first) and tracked in a per-block `var_bits` map.
//!
//! # Type expansion
//!
//! | IR type            | Bit count      |
//! |--------------------|----------------|
//! | `Bit`              | 1              |
//! | `_8` / `AES8`      | 8              |
//! | `_16`              | 16             |
//! | `_32`              | 32             |
//! | `_64` / `Galois64` | 64             |
//! | `_128`             | 128            |
//! | `_256`             | 256            |
//! | `Vec(n, T)`        | n × bits(T)    |
//! | `Tuple(Ts)`        | Σ bits(Tᵢ)    |
//! | `Block` / `Func`   | 0              |
//!
//! # Statements that produce no new Boolar stmts
//!
//! `Transmute`, `Rol`, `Ror`, `Merge`, `Splat`, and `Shuffle` are pure
//! bit-permutation operations.  They update `var_bits` to alias or reorder
//! existing Boolar vars without emitting any new Boolar statement.
//!
//! # External primitives
//!
//! - `OracleCall` / `ActionCall`: emitted as an opaque Boolar call-handle
//!   var, immediately followed by one `OracleBit` / `ActionBit` per output
//!   bit.  The per-output bit groups are stashed for later `OracleOutput` /
//!   `ActionOutput` projections.
//! - `OracleOutput` / `ActionOutput`: resolved from the pre-projected bit
//!   stash; emit no new Boolar stmts.
//! - `Rng { name, ty }`: one `BIrStmt::Rng { name }` per output bit.
//! - `StorageRead` / `StorageWrite`: expanded one Boolar op **per bit** of
//!   the value. Every Boolar storage cell holds exactly one bit; the value's
//!   bit index `i` is appended to the address as high-order bits
//!   (`addr' = addr ++ bits_of(i)`, flat cell `base + (i << N)` where `N` is
//!   the lane's fixed element-address width). Lanes are dense `LaneId`s
//!   allocated by first use over the source type table; the total
//!   `LaneId → IRTypeId` side table is available from
//!   [`lower_ir_to_boolar_with_lane_table`].
//!
//! # Limitations
//!
//! - `IRTerminator::JumpTable` is not supported (panics).
//! - `IRTerminator::Jmp` with `IRBlockTargetId::Dyn` is not supported (panics).
//! - Within a `(StorageId, LaneId)` space every storage op must use the same
//!   element-address width; mixed widths panic (the appended-index layout
//!   would otherwise be ambiguous).

use alloc::{collections::BTreeMap, vec, vec::Vec};

use volar_ir::{
    boolar::{BIrBlock, BIrBlocks, BIrPreInitSegment, BIrStmt, BIrTarget, BIrTerminator, LaneId},
    ir::{
        IRBlock, IRBlockTargetId, IRBlocks, IRStmt, IRTerminator, IRType, IRTypeId, IRTypes,
        IRVarId, PrimType,
    },
};
use volar_ir_common::{Constant, StorageId};

/// A source call did not match the external declarations carried by its
/// containing [`IRBlocks`].  Lowering is deliberately fail-closed: a backend
/// never guesses a handler for an undeclared primitive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExternalLoweringError {
    UndeclaredSource {
        kind: &'static str,
        name: alloc::string::String,
    },
    SignatureMismatch {
        kind: &'static str,
        name: alloc::string::String,
    },
}

// ============================================================================
// Public API
// ============================================================================

/// Lower a Volar IR circuit (`IRBlocks`) to Boolar IR (`BIrBlocks`).
///
/// `types` is the type table that all [`IRTypeId`]s in `blocks` index into.
/// Each block is lowered independently; block arguments are expanded to their
/// flat bit lists in the order they appear in the IR terminator.
///
/// Storage lanes are allocated internally; use
/// [`lower_ir_to_boolar_with_lane_table`] when the `LaneId → IRTypeId`
/// mapping is needed.
///
/// # Panics
///
/// Panics on `JumpTable` terminators, `Dyn` jump targets (not representable
/// in `BIrTerminator`), and mismatched element-address widths within one
/// `(StorageId, LaneId)` space.
pub fn lower_ir_to_boolar<P: Clone>(blocks: &IRBlocks<P>, types: &IRTypes) -> BIrBlocks<P> {
    try_lower_ir_to_boolar(blocks, types)
        .unwrap_or_else(|error| panic!("lower_ir_to_boolar: invalid external primitive: {error:?}"))
}

/// Fallible counterpart to [`lower_ir_to_boolar`].
pub fn try_lower_ir_to_boolar<P: Clone>(
    blocks: &IRBlocks<P>,
    types: &IRTypes,
) -> Result<BIrBlocks<P>, ExternalLoweringError> {
    try_lower_ir_to_boolar_with_lane_table(blocks, types).map(|(blocks, _)| blocks)
}

/// Like [`lower_ir_to_boolar`], but also returns the watchlist-style side
/// table mapping every allocated [`LaneId`] to its source [`IRTypeId`]. The
/// table is a byproduct of the same lowering run that allocates the lanes, so
/// it cannot drift from them.
pub fn lower_ir_to_boolar_with_lane_table<P: Clone>(
    blocks: &IRBlocks<P>,
    types: &IRTypes,
) -> (BIrBlocks<P>, BTreeMap<LaneId, IRTypeId>) {
    try_lower_ir_to_boolar_with_lane_table(blocks, types)
        .unwrap_or_else(|error| panic!("lower_ir_to_boolar: invalid external primitive: {error:?}"))
}

/// Fallible variant of [`lower_ir_to_boolar_with_lane_table`].
pub fn try_lower_ir_to_boolar_with_lane_table<P: Clone>(
    blocks: &IRBlocks<P>,
    types: &IRTypes,
) -> Result<(BIrBlocks<P>, BTreeMap<LaneId, IRTypeId>), ExternalLoweringError> {
    validate_external_sources(blocks, types)?;
    // ---- 1. Allocate lanes: dense first-use renumbering -------------------
    let mut lane_of: BTreeMap<IRTypeId, LaneId> = BTreeMap::new();
    let mut next_lane: u32 = 0;
    {
        let mut alloc_lane = |ty: IRTypeId| {
            lane_of.entry(ty).or_insert_with(|| {
                let l = LaneId(next_lane);
                next_lane += 1;
                l
            });
        };
        for block in &blocks.blocks {
            for stmt in &block.stmts {
                match &stmt.kind {
                    IRStmt::StorageRead { ty, .. } | IRStmt::StorageWrite { ty, .. } => {
                        alloc_lane(*ty)
                    }
                    IRStmt::ActionStore { output_tys, .. } => {
                        for ty in output_tys {
                            alloc_lane(*ty);
                        }
                    }
                    _ => {}
                }
            }
        }
        for seg in &blocks.pre_init {
            alloc_lane(seg.ty);
        }
    }
    let lane_table: BTreeMap<LaneId, IRTypeId> =
        lane_of.iter().map(|(&ty, &lane)| (lane, ty)).collect();

    // ---- 2. Lower blocks, recording per-lane element-address widths -------
    let mut addr_widths: BTreeMap<(StorageId, LaneId), usize> = BTreeMap::new();
    let mut occurrence = 0u64;
    let out_blocks: Vec<BIrBlock<P>> = blocks
        .blocks
        .iter()
        .map(|block| lower_block(block, types, &lane_of, &mut addr_widths, &mut occurrence))
        .collect();

    // ---- 3. Expand typed pre-init to bit-granular segments ---------------
    let pre_init = blocks
        .pre_init
        .iter()
        .flat_map(|seg| expand_pre_init_segment(seg, types, &lane_of, &addr_widths))
        .collect();

    Ok((
        BIrBlocks {
            blocks: out_blocks,
            pre_init,
        },
        lane_table,
    ))
}

fn validate_external_sources<P: Clone>(
    blocks: &IRBlocks<P>,
    types: &IRTypes,
) -> Result<(), ExternalLoweringError> {
    for block in &blocks.blocks {
        let mut var_types: Vec<Option<IRTypeId>> = block.params.iter().copied().map(Some).collect();
        for node in &block.stmts {
            match &node.kind {
                IRStmt::OracleCall {
                    name,
                    args,
                    output_tys,
                    ..
                } => {
                    let decl = blocks
                        .oracles
                        .iter()
                        .find(|decl| decl.name == *name)
                        .ok_or_else(|| ExternalLoweringError::UndeclaredSource {
                            kind: "oracle",
                            name: name.clone(),
                        })?;
                    if decl.params.len() != args.len()
                        || decl.results.as_slice() != output_tys.as_slice()
                        || !external_arg_types_match(&var_types, args, &decl.params)
                    {
                        return Err(ExternalLoweringError::SignatureMismatch {
                            kind: "oracle",
                            name: name.clone(),
                        });
                    }
                }
                IRStmt::ActionCall {
                    name,
                    guard,
                    args,
                    fallbacks,
                    output_tys,
                    ..
                } => {
                    let decl = blocks
                        .actions
                        .iter()
                        .find(|decl| decl.name == *name)
                        .ok_or_else(|| ExternalLoweringError::UndeclaredSource {
                            kind: "action",
                            name: name.clone(),
                        })?;
                    if decl.params.len() != args.len()
                        || decl.results.as_slice() != output_tys.as_slice()
                        || fallbacks.len() != output_tys.len()
                        || !external_arg_types_match(&var_types, args, &decl.params)
                        || !external_arg_types_match(&var_types, fallbacks, &decl.results)
                        || !var_is_bit(&var_types, *guard, types)
                    {
                        return Err(ExternalLoweringError::SignatureMismatch {
                            kind: "action",
                            name: name.clone(),
                        });
                    }
                }
                IRStmt::ActionStore {
                    name,
                    guard,
                    args,
                    fallbacks,
                    output_tys,
                    targets,
                    ..
                } => {
                    let decl = blocks
                        .actions
                        .iter()
                        .find(|decl| decl.name == *name)
                        .ok_or_else(|| ExternalLoweringError::UndeclaredSource {
                            kind: "action",
                            name: name.clone(),
                        })?;
                    if decl.params.len() != args.len()
                        || decl.results.as_slice() != output_tys.as_slice()
                        || fallbacks.len() != output_tys.len()
                        || targets.len() != output_tys.len()
                        || !external_arg_types_match(&var_types, args, &decl.params)
                        || !external_arg_types_match(&var_types, fallbacks, &decl.results)
                        || !targets.iter().all(|target| {
                            var_types
                                .get(target.addr.0 as usize)
                                .and_then(|ty| *ty)
                                .is_some()
                        })
                        || !var_is_bit(&var_types, *guard, types)
                    {
                        return Err(ExternalLoweringError::SignatureMismatch {
                            kind: "action",
                            name: name.clone(),
                        });
                    }
                }
                IRStmt::Rng { name, ty } => {
                    let decl = blocks
                        .rngs
                        .iter()
                        .find(|decl| decl.name == *name)
                        .ok_or_else(|| ExternalLoweringError::UndeclaredSource {
                            kind: "rng",
                            name: name.clone(),
                        })?;
                    if decl.ty != *ty {
                        return Err(ExternalLoweringError::SignatureMismatch {
                            kind: "rng",
                            name: name.clone(),
                        });
                    }
                }
                _ => {}
            }
            var_types.push(stmt_result_type(&node.kind));
        }
    }
    Ok(())
}

fn external_arg_types_match(
    var_types: &[Option<IRTypeId>],
    args: &[IRVarId],
    expected: &[IRTypeId],
) -> bool {
    args.iter()
        .zip(expected)
        .all(|(arg, expected)| var_types.get(arg.0 as usize).and_then(|ty| *ty) == Some(*expected))
}

fn var_is_bit(var_types: &[Option<IRTypeId>], var: IRVarId, types: &IRTypes) -> bool {
    var_types
        .get(var.0 as usize)
        .and_then(|ty| *ty)
        .is_some_and(|ty| types.is_bit(ty))
}

/// Keep declaration validation independent of lowering's bit-expansion state.
/// Effect-only statements intentionally produce no type: any attempt to feed
/// one back into a source call is therefore rejected as a signature mismatch.
fn stmt_result_type(stmt: &IRStmt) -> Option<IRTypeId> {
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
        IRStmt::StorageWrite { .. } | IRStmt::ActionStore { .. } | _ => None,
    }
}

// ============================================================================
// Block lowering
// ============================================================================

fn lower_block<P: Clone>(
    block: &IRBlock<P>,
    types: &IRTypes,
    lane_of: &BTreeMap<IRTypeId, LaneId>,
    addr_widths: &mut BTreeMap<(StorageId, LaneId), usize>,
    occurrence: &mut u64,
) -> BIrBlock<P> {
    // ---- 1. Expand params --------------------------------------------------
    // Each IR param of type T becomes ir_type_bits(T) consecutive Boolar params.
    // var_bits[param_idx] = slice of Boolar param IRVarIds for that param.
    let mut var_bits: BTreeMap<u32, Vec<IRVarId>> = BTreeMap::new();
    let mut next_param: u32 = 0;
    for (i, ty_id) in block.params.iter().enumerate() {
        let w = ir_type_bits(&types.0[ty_id.0 as usize], types);
        let bits: Vec<IRVarId> = (next_param..next_param + w as u32).map(IRVarId).collect();
        var_bits.insert(i as u32, bits);
        next_param += w as u32;
    }
    let total_params = next_param;

    // ---- 2. Emit stmts -----------------------------------------------------
    let mut emitter = Emitter::new(total_params);

    // Stash for OracleOutput / ActionOutput resolution.
    // Key: IR var index of the OracleCall / ActionCall.
    // Value: per-output bit lists (Vec<Vec<IRVarId>>), one inner Vec per output.
    let mut call_output_bits: BTreeMap<u32, Vec<Vec<IRVarId>>> = BTreeMap::new();

    for (si, stmt) in block.stmts.iter().enumerate() {
        let prov = stmt.prov.clone();
        let ir_var_idx = block.params.len() as u32 + si as u32;
        lower_stmt(
            &stmt.kind,
            prov,
            ir_var_idx,
            &mut var_bits,
            &mut call_output_bits,
            &mut emitter,
            types,
            lane_of,
            addr_widths,
            occurrence,
        );
    }

    // ---- 3. Convert terminator --------------------------------------------
    let terminator = lower_terminator(&block.terminator, &var_bits);

    BIrBlock {
        params: total_params,
        stmts: emitter.stmts,
        terminator,
    }
}

// ============================================================================
// Statement lowering
// ============================================================================

#[allow(clippy::too_many_arguments)]
fn lower_stmt<P: Clone>(
    stmt: &IRStmt,
    prov: P,
    ir_var_idx: u32,
    var_bits: &mut BTreeMap<u32, Vec<IRVarId>>,
    call_output_bits: &mut BTreeMap<u32, Vec<Vec<IRVarId>>>,
    emitter: &mut Emitter<P>,
    types: &IRTypes,
    lane_of: &BTreeMap<IRTypeId, LaneId>,
    addr_widths: &mut BTreeMap<(StorageId, LaneId), usize>,
    occurrence: &mut u64,
) {
    match stmt {
        // ---- Constant ------------------------------------------------------
        IRStmt::Const(c, ty_id) => {
            let w = ir_type_bits(&types.0[ty_id.0 as usize], types);
            let bits: Vec<IRVarId> = (0..w)
                .map(|j| {
                    if constant_bit(c, j) {
                        emitter.emit(BIrStmt::One, prov.clone())
                    } else {
                        emitter.emit(BIrStmt::Zero, prov.clone())
                    }
                })
                .collect();
            var_bits.insert(ir_var_idx, bits);
        }

        // ---- Transmute (bit-identity) --------------------------------------
        // Same bits, different type label.  No new Boolar stmts.
        IRStmt::Transmute { src, .. } => {
            let bits = var_bits[&src.0].clone();
            var_bits.insert(ir_var_idx, bits);
        }

        // ---- GF(2) polynomial ----------------------------------------------
        IRStmt::Poly {
            coeffs, constant, ..
        } => {
            let w = infer_poly_width(coeffs, var_bits);
            let bits: Vec<IRVarId> = (0..w)
                .map(|j| lower_poly_bit(coeffs, constant, j, var_bits, emitter, prov.clone()))
                .collect();
            var_bits.insert(ir_var_idx, bits);
        }

        // ---- Rotate left ---------------------------------------------------
        // result[j] = src[(j + w - n) % w]   (matches vole.rs vec_parts convention)
        IRStmt::Rol { src, n, .. } => {
            let src_bits = var_bits[&src.0].clone();
            let w = src_bits.len();
            let rotated: Vec<IRVarId> = (0..w)
                .map(|j| src_bits[(j + w - n % w.max(1)) % w])
                .collect();
            var_bits.insert(ir_var_idx, rotated);
        }

        // ---- Rotate right --------------------------------------------------
        // result[j] = src[(j + n) % w]   (matches vole.rs vec_parts convention)
        IRStmt::Ror { src, n, .. } => {
            let src_bits = var_bits[&src.0].clone();
            let w = src_bits.len();
            let rotated: Vec<IRVarId> = (0..w).map(|j| src_bits[(j + n % w.max(1)) % w]).collect();
            var_bits.insert(ir_var_idx, rotated);
        }

        // ---- Merge ---------------------------------------------------------
        // Concatenate bit lists of all parts (LSB-first order preserved).
        IRStmt::Merge { parts, .. } => {
            let bits: Vec<IRVarId> = parts
                .iter()
                .flat_map(|p| var_bits[&p.0].iter().cloned())
                .collect();
            var_bits.insert(ir_var_idx, bits);
        }

        // ---- Splat ---------------------------------------------------------
        // Broadcast the single bit of `src` to all positions of `ty`.
        IRStmt::Splat { src, ty } => {
            let src_bit = var_bits[&src.0][0];
            let w = ir_type_bits(&types.0[ty.0 as usize], types);
            var_bits.insert(ir_var_idx, vec![src_bit; w]);
        }

        // ---- Shuffle -------------------------------------------------------
        // Arbitrary bit selection: result[i] = src[bit_idx].
        IRStmt::Shuffle { result_bits, .. } => {
            let bits: Vec<IRVarId> = result_bits
                .iter()
                .map(|(bit_idx, v)| var_bits[&v.0][*bit_idx as usize])
                .collect();
            var_bits.insert(ir_var_idx, bits);
        }

        // ---- StorageRead ---------------------------------------------------
        // One Boolar read per bit of the value; bit i's address is the
        // element address with bits_of(i) appended as high-order bits.
        IRStmt::StorageRead { storage, ty, addr } => {
            let k = ir_type_bits(&types.0[ty.0 as usize], types);
            let lane = *lane_of.get(ty).expect("lane allocated for storage type");
            let base_addr: Vec<IRVarId> = var_bits[&addr.0].clone();
            check_addr_budget(base_addr.len(), k);
            record_addr_width(*storage, lane, base_addr.len(), addr_widths);
            let mut bits = Vec::with_capacity(k);
            for i in 0..k {
                let mut full_addr = base_addr.clone();
                full_addr.extend(const_index_bits(emitter, i, k, &prov));
                let h = emitter.emit(
                    BIrStmt::StorageRead {
                        storage: *storage,
                        lane,
                        addr: full_addr,
                    },
                    prov.clone(),
                );
                bits.push(h);
            }
            var_bits.insert(ir_var_idx, bits);
        }

        // ---- StorageWrite --------------------------------------------------
        // One Boolar write per bit of the value; same address layout as
        // reads. The result slot carries the per-bit dummy sentinels.
        IRStmt::StorageWrite {
            storage,
            src,
            ty,
            addr,
        } => {
            let k = ir_type_bits(&types.0[ty.0 as usize], types);
            let lane = *lane_of.get(ty).expect("lane allocated for storage type");
            let base_addr: Vec<IRVarId> = var_bits[&addr.0].clone();
            let src_bits = &var_bits[&src.0];
            assert_eq!(
                src_bits.len(),
                k,
                "StorageWrite source width mismatch: {} bits vs type width {k}",
                src_bits.len()
            );
            check_addr_budget(base_addr.len(), k);
            record_addr_width(*storage, lane, base_addr.len(), addr_widths);
            let mut sentinels = Vec::with_capacity(k);
            for (i, &b) in src_bits.iter().enumerate() {
                let mut full_addr = base_addr.clone();
                full_addr.extend(const_index_bits(emitter, i, k, &prov));
                let h = emitter.emit(
                    BIrStmt::StorageWrite {
                        storage: *storage,
                        lane,
                        src: b,
                        addr: full_addr,
                    },
                    prov.clone(),
                );
                sentinels.push(h);
            }
            var_bits.insert(ir_var_idx, sentinels);
        }

        // ---- OracleCall ----------------------------------------------------
        IRStmt::OracleCall {
            name,
            args,
            output_tys,
            ..
        } => {
            let flat_args: Vec<IRVarId> = args
                .iter()
                .flat_map(|a| var_bits[&a.0].iter().cloned())
                .collect();
            let total_bits: usize = output_tys
                .iter()
                .map(|tid| ir_type_bits(&types.0[tid.0 as usize], types))
                .sum();
            // Emit every result bit as its own source invocation.  The
            // legacy aggregate is only retained in the high IR so existing
            // frontends can still use `OracleOutput` projections.
            let all_bit_vars: Vec<IRVarId> = (0..total_bits)
                .map(|bit| {
                    let current = *occurrence;
                    *occurrence += 1;
                    emitter.emit(
                        BIrStmt::OracleBit {
                            name: name.clone(),
                            args: flat_args.clone(),
                            bit,
                            occurrence: current,
                        },
                        prov.clone(),
                    )
                })
                .collect();
            // Partition into per-output bit lists for OracleOutput resolution.
            let mut output_bit_lists: Vec<Vec<IRVarId>> = Vec::new();
            let mut offset = 0;
            for tid in output_tys {
                let w = ir_type_bits(&types.0[tid.0 as usize], types);
                output_bit_lists.push(all_bit_vars[offset..offset + w].to_vec());
                offset += w;
            }
            var_bits.insert(ir_var_idx, vec![]);
            call_output_bits.insert(ir_var_idx, output_bit_lists);
        }

        // ---- OracleOutput --------------------------------------------------
        // Resolved from the pre-projected bit stash; no new Boolar stmts.
        IRStmt::OracleOutput { call, idx, .. } => {
            let bits = call_output_bits.get(&call.0).unwrap_or_else(|| {
                panic!(
                    "lower_ir_to_boolar: OracleOutput references unknown call var {}",
                    call.0
                )
            })[*idx]
                .clone();
            var_bits.insert(ir_var_idx, bits);
        }

        // ---- ActionCall ----------------------------------------------------
        IRStmt::ActionCall {
            name,
            guard,
            args,
            fallbacks,
            output_tys,
            ..
        } => {
            let guard_bit = var_bits[&guard.0][0];
            let flat_args: Vec<IRVarId> = args
                .iter()
                .flat_map(|a| var_bits[&a.0].iter().cloned())
                .collect();
            let flat_fallback: Vec<IRVarId> = fallbacks
                .iter()
                .flat_map(|f| var_bits[&f.0].iter().cloned())
                .collect();
            let total_bits: usize = output_tys
                .iter()
                .map(|tid| ir_type_bits(&types.0[tid.0 as usize], types))
                .sum();
            let handle = emitter.emit(
                BIrStmt::ActionCall {
                    name: name.clone(),
                    guard: guard_bit,
                    args: flat_args,
                    fallback: flat_fallback,
                    num_bits: total_bits,
                },
                prov.clone(),
            );
            let all_bit_vars: Vec<IRVarId> = (0..total_bits)
                .map(|bit| emitter.emit(BIrStmt::ActionBit { call: handle, bit }, prov.clone()))
                .collect();
            let mut output_bit_lists: Vec<Vec<IRVarId>> = Vec::new();
            let mut offset = 0;
            for tid in output_tys {
                let w = ir_type_bits(&types.0[tid.0 as usize], types);
                output_bit_lists.push(all_bit_vars[offset..offset + w].to_vec());
                offset += w;
            }
            var_bits.insert(ir_var_idx, vec![handle]);
            call_output_bits.insert(ir_var_idx, output_bit_lists);
        }

        // ---- ActionOutput --------------------------------------------------
        IRStmt::ActionOutput { call, idx, .. } => {
            let bits = call_output_bits.get(&call.0).unwrap_or_else(|| {
                panic!(
                    "lower_ir_to_boolar: ActionOutput references unknown call var {}",
                    call.0
                )
            })[*idx]
                .clone();
            var_bits.insert(ir_var_idx, bits);
        }

        // ---- ActionStore ---------------------------------------------------
        IRStmt::ActionStore {
            name,
            guard,
            args,
            fallbacks,
            output_tys,
            targets,
        } => {
            let guard_bit = var_bits[&guard.0][0];
            let flat_args: Vec<IRVarId> = args
                .iter()
                .flat_map(|a| var_bits[&a.0].iter().cloned())
                .collect();
            let mut flat_bit = 0usize;
            for ((fallback, ty), target) in fallbacks.iter().zip(output_tys).zip(targets) {
                let lane = *lane_of
                    .get(ty)
                    .expect("lane allocated for action result type");
                let base_addr = var_bits[&target.addr.0].clone();
                let fallback_bits = &var_bits[&fallback.0];
                let width = ir_type_bits(&types.0[ty.0 as usize], types);
                assert_eq!(
                    fallback_bits.len(),
                    width,
                    "ActionStore fallback width mismatch"
                );
                check_addr_budget(base_addr.len(), width);
                record_addr_width(target.storage, lane, base_addr.len(), addr_widths);
                for (result_bit, fallback) in fallback_bits.iter().copied().enumerate() {
                    let mut addr = base_addr.clone();
                    addr.extend(const_index_bits(emitter, result_bit, width, &prov));
                    let current = *occurrence;
                    *occurrence += 1;
                    emitter.emit(
                        BIrStmt::ActionStoreBit {
                            name: name.clone(),
                            guard: guard_bit,
                            args: flat_args.clone(),
                            fallback,
                            storage: target.storage,
                            lane,
                            addr,
                            bit: flat_bit,
                            occurrence: current,
                        },
                        prov.clone(),
                    );
                    flat_bit += 1;
                }
            }
            // Effects have no data result.  Keep the statement position in
            // the SSA numbering while making accidental consumption obvious.
            var_bits.insert(ir_var_idx, vec![]);
        }

        // ---- Rng -----------------------------------------------------------
        // One fresh `BIrStmt::Rng` per output bit.
        IRStmt::Rng { name, ty } => {
            let w = ir_type_bits(&types.0[ty.0 as usize], types);
            let bits: Vec<IRVarId> = (0..w)
                .map(|bit| {
                    let current = *occurrence;
                    *occurrence += 1;
                    emitter.emit(
                        BIrStmt::RngBit {
                            name: name.clone(),
                            bit,
                            occurrence: current,
                        },
                        prov.clone(),
                    )
                })
                .collect();
            var_bits.insert(ir_var_idx, bits);
        }
        _ => panic!("lower_ir_to_boolar: unhandled IRStmt variant — add lowering for this variant"),
    }
}

// ============================================================================
// Terminator lowering
// ============================================================================

fn lower_terminator(term: &IRTerminator, var_bits: &BTreeMap<u32, Vec<IRVarId>>) -> BIrTerminator {
    match term {
        IRTerminator::Jmp { target } => {
            assert!(
                !matches!(target.dest, IRBlockTargetId::Dyn(_)),
                "lower_ir_to_boolar: Dyn jump targets are not representable in BIrTerminator"
            );
            BIrTerminator::Jmp(BIrTarget {
                block: target.dest.clone(),
                args: flatten_bits(&target.args, var_bits),
            })
        }

        IRTerminator::JumpCond {
            condition,
            then_target,
            else_target,
        } => {
            let cond_bits = &var_bits[&condition.0];
            assert_eq!(
                cond_bits.len(),
                1,
                "lower_ir_to_boolar: JumpCond condition var {} has {} bits; expected 1 (Bit type)",
                condition.0,
                cond_bits.len()
            );
            BIrTerminator::CondJmp {
                val: cond_bits[0],
                then_target: BIrTarget {
                    block: then_target.dest.clone(),
                    args: flatten_bits(&then_target.args, var_bits),
                },
                else_target: BIrTarget {
                    block: else_target.dest.clone(),
                    args: flatten_bits(&else_target.args, var_bits),
                },
            }
        }

        IRTerminator::JumpTable { .. } => {
            panic!(
                "lower_ir_to_boolar: JumpTable terminators are not supported; \
                 convert to nested JumpCond first"
            )
        }
        _ => panic!(
            "lower_ir_to_boolar: unhandled IRTerminator variant — add lowering for this variant"
        ),
    }
}

fn flatten_bits(args: &[IRVarId], var_bits: &BTreeMap<u32, Vec<IRVarId>>) -> Vec<IRVarId> {
    args.iter()
        .flat_map(|a| var_bits[&a.0].iter().cloned())
        .collect()
}

// ============================================================================
// Polynomial lowering helpers
// ============================================================================

/// Return the bit-width of the output of a `Poly` stmt.
///
/// The width equals the maximum bit-count of any variable appearing in any
/// monomial.  If the polynomial has no variables (pure constant), the width
/// is 1 (a single GF(2) bit).
fn infer_poly_width(
    coeffs: &alloc::collections::BTreeMap<Vec<IRVarId>, u8>,
    var_bits: &BTreeMap<u32, Vec<IRVarId>>,
) -> usize {
    let mut w = 1usize;
    for (mono, _) in coeffs {
        for v in mono {
            if let Some(bits) = var_bits.get(&v.0) {
                w = w.max(bits.len());
            }
        }
    }
    w
}

/// Lower the `j`-th output bit of a `Poly` stmt.
///
/// Implements: `result[j] = constant[j] ⊕ ⊕{(mono,coeff): coeff odd} ∧(vars[j])`.
fn lower_poly_bit<P: Clone>(
    coeffs: &alloc::collections::BTreeMap<Vec<IRVarId>, u8>,
    constant: &Constant,
    bit: usize,
    var_bits: &BTreeMap<u32, Vec<IRVarId>>,
    emitter: &mut Emitter<P>,
    prov: P,
) -> IRVarId {
    // Accumulator: None means "0 so far".
    let mut acc: Option<IRVarId> = if constant_bit(constant, bit) {
        Some(emitter.emit(BIrStmt::One, prov.clone()))
    } else {
        None
    };

    for (mono, &coeff) in coeffs {
        if coeff % 2 == 0 {
            continue;
        }
        let mono_var: Option<IRVarId> = if mono.is_empty() {
            // Empty product = 1; contributes a constant One term.
            Some(emitter.emit(BIrStmt::One, prov.clone()))
        } else {
            // AND of all variable bits at position `bit`.
            let mut and_acc: Option<IRVarId> = None;
            for v in mono {
                if let Some(bit_var) = var_bits.get(&v.0).and_then(|bs| bs.get(bit)).cloned() {
                    and_acc = Some(match and_acc {
                        None => bit_var,
                        Some(prev) => emitter.emit(BIrStmt::And(prev, bit_var), prov.clone()),
                    });
                }
                // If the variable has fewer bits than `bit`, its high bits are
                // implicitly 0 — the monomial contributes 0 for this position.
            }
            and_acc
        };

        if let Some(mv) = mono_var {
            acc = Some(match acc {
                None => mv,
                Some(prev) => emitter.emit(BIrStmt::Xor(prev, mv), prov.clone()),
            });
        }
    }

    // If no terms contributed, result is 0.
    acc.unwrap_or_else(|| emitter.emit(BIrStmt::Zero, prov))
}

// ============================================================================
// Type utilities
// ============================================================================

/// Return the number of GF(2) bits that `ty` expands to.
///
/// `Block` and `Func` types are not data types and expand to 0 bits.
pub fn ir_type_bits(ty: &IRType, types: &IRTypes) -> usize {
    match ty {
        IRType::Primitive(PrimType::Bit) => 1,
        IRType::Primitive(PrimType::_8) | IRType::Primitive(PrimType::AES8) => 8,
        IRType::Primitive(PrimType::_16) => 16,
        IRType::Primitive(PrimType::_32) => 32,
        IRType::Primitive(PrimType::_64) | IRType::Primitive(PrimType::Galois64) => 64,
        IRType::Primitive(PrimType::_128) => 128,
        IRType::Primitive(PrimType::_256) => 256,
        IRType::Primitive(PrimType::Z3) => {
            panic!(
                "ir_type_bits: Z3 (GF(3)) cannot be lowered to GF(2) bits. \
                 Z3 values are only valid in the TFHE backend. \
                 Use raise_to_z3 before the TFHE weaver, not before lower_ir_to_boolar."
            );
        }
        IRType::Vec(n, elem_id) => n * ir_type_bits(&types.0[elem_id.0 as usize], types),
        IRType::Tuple(ids) => ids
            .iter()
            .map(|id| ir_type_bits(&types.0[id.0 as usize], types))
            .sum(),
        IRType::Block { .. } | IRType::Func { .. } => 0,
        IRType::Primitive(_) => unimplemented!("ir_type_bits: unknown PrimType variant"),
        _ => panic!("ir_type_bits: unhandled IrType variant — add bit-width calculation"),
    }
}

/// Extract bit `bit` from a 256-bit `Constant` (LSB-first).
fn constant_bit(c: &Constant, bit: usize) -> bool {
    if bit < 128 {
        (c.lo >> bit) & 1 == 1
    } else {
        (c.hi >> (bit - 128)) & 1 == 1
    }
}

// ============================================================================
// Emitter
// ============================================================================

/// Sequential Boolar var-ID allocator and stmt accumulator.
///
/// Boolar var IDs start at `params` (the number of input bit params for the
/// block) and increment by one for each emitted stmt.
struct Emitter<P: Clone> {
    stmts: Vec<volar_ir_common::Node<BIrStmt, P>>,
    next_var: u32,
    /// Cache of constant index-bit wires used as appended address suffixes:
    /// `(bit_position, bit_value) → wire`.
    const_wires: BTreeMap<(u32, bool), IRVarId>,
}

impl<P: Clone> Emitter<P> {
    fn new(params: u32) -> Self {
        Emitter {
            stmts: vec![],
            next_var: params,
            const_wires: BTreeMap::new(),
        }
    }

    fn emit(&mut self, stmt: BIrStmt, prov: P) -> IRVarId {
        let id = IRVarId(self.next_var);
        self.stmts
            .push(volar_ir_common::Node::new(stmt, prov, None));
        self.next_var += 1;
        id
    }

    /// Wire that is constantly `(i >> j) & 1`, emitting a `Zero`/`One`
    /// statement once per (position, value) pair.
    fn const_bit_wire(&mut self, i: usize, j: u32, prov: &P) -> IRVarId {
        let bit_val = ((i >> j) & 1) == 1;
        if let Some(&w) = self.const_wires.get(&(j, bit_val)) {
            return w;
        }
        let w = self.emit(
            if bit_val { BIrStmt::One } else { BIrStmt::Zero },
            prov.clone(),
        );
        self.const_wires.insert((j, bit_val), w);
        w
    }
}

/// Record the element-address width of a storage space; all ops in one
/// `(StorageId, LaneId)` must agree, since the appended-index cell layout is
/// defined relative to it.
/// Fail closed when an appended-index address cannot fit the `u64` flat cell
/// space: element-address bits + ceil(log2(value bits)) must stay within 64.
///
/// The Boolar storage model keys cells by `u64`; silently truncating wider
/// addresses would alias distinct cells and corrupt read/write semantics.
fn check_addr_budget(addr_bits: usize, value_bits: usize) {
    // Same minimal bit count `const_index_bits` emits for indices 0..k.
    let index_bits = if value_bits <= 1 {
        0
    } else {
        (u32::BITS - (value_bits as u32 - 1).leading_zeros()) as usize
    };
    let total = addr_bits + index_bits;
    assert!(
        total <= 64,
        "lower_ir_to_boolar: storage address of {addr_bits} element bits + \
         {index_bits} appended bit-index bits exceeds the 64-bit flat cell space \
         (value width {value_bits})"
    );
}

fn record_addr_width(
    storage: StorageId,
    lane: LaneId,
    width: usize,
    addr_widths: &mut BTreeMap<(StorageId, LaneId), usize>,
) {
    match addr_widths.entry((storage, lane)) {
        alloc::collections::btree_map::Entry::Occupied(e) => {
            assert_eq!(
                *e.get(),
                width,
                "mixed element-address widths within one (StorageId, LaneId) \
                 storage space: {} vs {width}",
                e.get()
            );
        }
        alloc::collections::btree_map::Entry::Vacant(e) => {
            e.insert(width);
        }
    }
}

/// Appended address suffix wires for bit `i` of a `k`-bit value:
/// `ceil(log2(k))` constant wires, LSB-first (empty when `k <= 1`).
fn const_index_bits<P: Clone>(
    emitter: &mut Emitter<P>,
    i: usize,
    k: usize,
    prov: &P,
) -> Vec<IRVarId> {
    let s = if k <= 1 {
        0
    } else {
        (usize::BITS - (k - 1).leading_zeros()) as usize
    };
    (0..s as u32)
        .map(|j| emitter.const_bit_wire(i, j, prov))
        .collect()
}

/// Expand one typed pre-init segment into bit-granular Boolar segments.
///
/// Under the appended-address layout, element `e`, bit `i` lives in flat cell
/// `offset + e + (i << N)` where `N` is the lane's recorded element-address
/// width (0 when the lane has no runtime storage ops). Bits of one element
/// are therefore strided, so one [`BIrPreInitSegment`] is emitted per bit
/// index, each covering the contiguous run of elements at that bit position.
fn expand_pre_init_segment(
    seg: &volar_ir_common::PreInitSegment,
    types: &IRTypes,
    lane_of: &BTreeMap<IRTypeId, LaneId>,
    addr_widths: &BTreeMap<(StorageId, LaneId), usize>,
) -> alloc::vec::Vec<BIrPreInitSegment> {
    let k = ir_type_bits(&types.0[seg.ty.0 as usize], types);
    let lane = *lane_of
        .get(&seg.ty)
        .expect("lane allocated for pre-init type");
    let n_addr = *addr_widths.get(&(seg.storage, lane)).unwrap_or(&0);
    check_addr_budget(n_addr, k);
    (0..k)
        .map(|i| BIrPreInitSegment {
            storage: seg.storage,
            lane,
            offset: seg.offset as u64 + ((i as u64) << n_addr),
            data: (0..seg.data.len())
                .map(|e| constant_bit(&seg.data[e], i))
                .collect(),
        })
        .collect()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use volar_ir::ir::{
        IRBlock, IRBlockTargetId, IRBlocks, IRBranchTarget, IRTerminator, IRType, IRTypes, IRVarId,
        PrimType,
    };
    use volar_ir_common::{Constant, Node, TypeTable};

    // -- Helpers -------------------------------------------------------------

    /// Zero constant (all bits 0).
    fn zero_const() -> Constant {
        Constant { lo: 0, hi: 0 }
    }

    /// Build a minimal single-block `IRBlocks` that takes `n_params` Bit params
    /// and immediately returns them, alongside an `IRTypes` table that only
    /// contains `Bit`.
    fn make_passthrough(n_params: usize) -> (IRBlocks<()>, IRTypes) {
        let mut types = TypeTable::new();
        let bit_id = types.bit();

        let params: std::vec::Vec<_> = (0..n_params).map(|_| bit_id).collect();
        let args: std::vec::Vec<IRVarId> = (0..n_params as u32).map(IRVarId).collect();

        let block = IRBlock {
            params,
            stmts: std::vec![],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, args),
            },
        };
        (IRBlocks::new(std::vec![block]), types)
    }

    // -- ir_type_bits ---------------------------------------------------------

    #[test]
    fn type_bits_bit() {
        let types = TypeTable::new();
        assert_eq!(ir_type_bits(&IRType::Primitive(PrimType::Bit), &types), 1);
    }

    #[test]
    fn type_bits_primitives() {
        let types = TypeTable::new();
        assert_eq!(ir_type_bits(&IRType::Primitive(PrimType::_8), &types), 8);
        assert_eq!(ir_type_bits(&IRType::Primitive(PrimType::_16), &types), 16);
        assert_eq!(ir_type_bits(&IRType::Primitive(PrimType::_32), &types), 32);
        assert_eq!(ir_type_bits(&IRType::Primitive(PrimType::_64), &types), 64);
        assert_eq!(
            ir_type_bits(&IRType::Primitive(PrimType::_128), &types),
            128
        );
        assert_eq!(
            ir_type_bits(&IRType::Primitive(PrimType::_256), &types),
            256
        );
        assert_eq!(ir_type_bits(&IRType::Primitive(PrimType::AES8), &types), 8);
        assert_eq!(
            ir_type_bits(&IRType::Primitive(PrimType::Galois64), &types),
            64
        );
    }

    #[test]
    fn type_bits_vec() {
        let mut types = TypeTable::new();
        let bit_id = types.bit();
        let vec4 = IRType::Vec(4, bit_id);
        assert_eq!(ir_type_bits(&vec4, &types), 4);
        let vec8 = IRType::Vec(8, bit_id);
        assert_eq!(ir_type_bits(&vec8, &types), 8);
    }

    #[test]
    fn type_bits_tuple() {
        let mut types = TypeTable::new();
        let bit_id = types.bit();
        let u8_id = types.primitive(PrimType::_8);
        // Tuple(Bit, _8) = 1 + 8 = 9
        let tuple = IRType::Tuple(std::vec![bit_id, u8_id]);
        assert_eq!(ir_type_bits(&tuple, &types), 9);
    }

    // -- Param expansion ------------------------------------------------------

    #[test]
    fn passthrough_zero_params() {
        let (blocks, types) = make_passthrough(0);
        let lowered = lower_ir_to_boolar::<()>(&blocks, &types);
        assert_eq!(lowered.blocks.len(), 1);
        let b = &lowered.blocks[0];
        assert_eq!(b.params, 0);
        assert_eq!(b.stmts.len(), 0);
    }

    #[test]
    fn passthrough_three_bit_params() {
        let (blocks, types) = make_passthrough(3);
        let lowered = lower_ir_to_boolar::<()>(&blocks, &types);
        let b = &lowered.blocks[0];
        // 3 Bit params → 3 Boolar params.
        assert_eq!(b.params, 3);
        assert_eq!(b.stmts.len(), 0);
    }

    #[test]
    fn u8_param_expands_to_eight_bits() {
        let mut types = TypeTable::new();
        let u8_id = types.primitive(PrimType::_8);

        let block = IRBlock {
            params: std::vec![u8_id],
            stmts: std::vec![],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, std::vec![IRVarId(0)]),
            },
        };
        let blocks = IRBlocks::<()>::new(std::vec![block]);
        let lowered = lower_ir_to_boolar::<()>(&blocks, &types);
        // One u8 param → 8 Boolar bit params.
        assert_eq!(lowered.blocks[0].params, 8);
    }

    // -- Const statement ------------------------------------------------------

    #[test]
    fn const_zero_bit_emits_zero_stmt() {
        let mut types = TypeTable::new();
        let bit_id = types.bit();

        let mut block = IRBlock::<()> {
            params: std::vec![],
            stmts: std::vec![],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, std::vec![IRVarId(0)]),
            },
        };
        block.push_stmt(volar_ir::ir::IRStmt::Const(zero_const(), bit_id), ());

        let blocks = IRBlocks::new(std::vec![block]);
        let lowered = lower_ir_to_boolar::<()>(&blocks, &types);
        let b = &lowered.blocks[0];
        // 1-bit const zero → exactly one BIrStmt::Zero.
        assert_eq!(b.stmts.len(), 1);
        assert_eq!(b.stmts[0].kind, BIrStmt::Zero);
    }

    #[test]
    fn const_u8_emits_eight_stmts() {
        let mut types = TypeTable::new();
        let u8_id = types.primitive(PrimType::_8);

        let mut block = IRBlock::<()> {
            params: std::vec![],
            stmts: std::vec![],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, std::vec![IRVarId(0)]),
            },
        };
        // Const = 0b00000001 (value 1, bit 0 = One, rest = Zero).
        let c = Constant { lo: 1, hi: 0 };
        block.push_stmt(volar_ir::ir::IRStmt::Const(c, u8_id), ());

        let blocks = IRBlocks::new(std::vec![block]);
        let lowered = lower_ir_to_boolar::<()>(&blocks, &types);
        let b = &lowered.blocks[0];
        assert_eq!(b.stmts.len(), 8);
        // LSB first: bit 0 = 1 → One.
        assert_eq!(b.stmts[0].kind, BIrStmt::One);
        // All remaining bits are 0 → Zero.
        for node in &b.stmts[1..] {
            assert_eq!(node.kind, BIrStmt::Zero);
        }
    }

    // -- Transmute (identity) -------------------------------------------------

    #[test]
    fn transmute_aliases_existing_bits() {
        let mut types = TypeTable::new();
        let bit_id = types.bit();
        let u8_src = types.primitive(PrimType::_8);
        let u8_dst = types.primitive(PrimType::_8);

        // Block: param0 = _8, stmt0 = Const(1, _8), stmt1 = Transmute(stmt0).
        let c = Constant { lo: 1, hi: 0 };
        let block = IRBlock::<()> {
            params: std::vec![bit_id], // 1 bit param so var ids start right
            stmts: std::vec![
                volar_ir::ir::IRStmt::Const(c, u8_src),
                volar_ir::ir::IRStmt::Transmute {
                    src: IRVarId(1), // var 1 = the Const above (params=1 so param is var 0)
                    src_ty: u8_src,
                    dst_ty: u8_dst,
                },
            ]
            .into_iter()
            .map(|s| Node::new(s, (), None))
            .collect(),
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, std::vec![IRVarId(2)]),
            }, // return the transmuted value
        };
        let blocks = IRBlocks::new(std::vec![block]);
        let lowered = lower_ir_to_boolar::<()>(&blocks, &types);
        let b = &lowered.blocks[0];
        // Transmute emits zero new stmts; only the 8 from the Const.
        assert_eq!(b.stmts.len(), 8);
    }

    // -- AES8 Poly lowering ---------------------------------------------------
    //
    // FAEST relies on `PrimType::AES8` (GF(2^8) under the AES polynomial) being
    // lowerable. The IR's `Poly` semantics permit linear combinations over a
    // bitvector/field type with Bit selectors — addition in GF(2^k) of
    // characteristic 2 is bitwise XOR, so the existing per-bit-position
    // `lower_poly_bit` produces correct output for AES8-typed linear combos.
    //
    // GF(2^8) *multiplication* (i.e. `gf_mul_u8`) is not a `Poly` statement —
    // it parses from the spec total-Rust subset as an extern function call.
    // The lowering responsibility for that path lives in the spec-call
    // expansion pass, not here.

    #[test]
    fn poly_aes8_linear_combo_xors_per_bit() {
        // Build: param0 = AES8, param1 = AES8 (each becomes 8 boolar bits),
        // then stmt0 = Poly { ty: AES8, coeffs: {[param0] -> 1, [param1] -> 1},
        // constant = 0 } — i.e. param0 XOR param1.
        // Verify: 8 output bits, each is XOR of the corresponding param bits.
        let mut types = TypeTable::new();
        let aes8_id = types.primitive(PrimType::AES8);

        let mut coeffs: alloc::collections::BTreeMap<std::vec::Vec<IRVarId>, u8> =
            alloc::collections::BTreeMap::new();
        coeffs.insert(std::vec![IRVarId(0)], 1);
        coeffs.insert(std::vec![IRVarId(1)], 1);

        let block = IRBlock::<()> {
            params: std::vec![aes8_id, aes8_id],
            stmts: std::vec![volar_ir::ir::IRStmt::Poly {
                ty: aes8_id,
                coeffs,
                constant: zero_const(),
            }]
            .into_iter()
            .map(|s| Node::new(s, (), None))
            .collect(),
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, std::vec![IRVarId(2)]),
            },
        };
        let blocks = IRBlocks::new(std::vec![block]);
        let lowered = lower_ir_to_boolar::<()>(&blocks, &types);
        let b = &lowered.blocks[0];
        // 2 AES8 params = 16 boolar param bits. Each of the 8 output bits is
        // one Xor — so 8 stmts emitted.
        assert_eq!(b.params, 16);
        assert_eq!(b.stmts.len(), 8);
        for node in &b.stmts {
            assert!(
                matches!(&node.kind, BIrStmt::Xor(_, _)),
                "expected per-bit Xor for AES8 linear combination, got {:?}",
                node.kind
            );
        }
    }

    // Latent limitation worth recording for the FAEST work:
    // `infer_poly_width` derives the output width from referenced variables'
    // bit-counts, *not* from the Poly's `ty`. Consequently a pure-constant
    // `Poly { ty: AES8, coeffs: {}, constant: c }` lowers to a single bit,
    // not 8. This isn't a blocker for FAEST — pure constants always go
    // through `IRStmt::Const` instead — but if a future weaver pass emits
    // constant-only Polys at field types, `infer_poly_width` will need to
    // fall back to `ir_type_bits(ty)` when `coeffs` is empty. Tracked
    // adjacent to AES8 work; no test asserts the current (wrong-for-empty)
    // behaviour because no code path produces it today.

    // -- Provenance threading -------------------------------------------------

    #[test]
    fn provenance_is_threaded_through() {
        // Use u32 as provenance.
        let mut types = TypeTable::new();
        let bit_id = types.bit();

        let c = Constant { lo: 0, hi: 0 };
        let block = IRBlock::<u32> {
            params: std::vec![bit_id],
            stmts: std::vec![volar_ir::ir::IRStmt::Const(c, bit_id)]
                .into_iter()
                .map(|s| Node::new(s, 42u32, None))
                .collect(),
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, std::vec![IRVarId(0)]),
            },
        };
        let blocks = IRBlocks::new(std::vec![block]);
        let lowered = lower_ir_to_boolar::<u32>(&blocks, &types);
        let b = &lowered.blocks[0];
        // The single Const(Bit, 0) emits one Zero stmt with provenance 42.
        assert_eq!(b.stmts.len(), 1);
        assert_eq!(b.stmts[0].prov, 42u32);
    }

    #[test]
    fn action_store_lowers_each_result_bit_to_its_storage_target() {
        use volar_ir::ir::{ActionDecl, ActionTarget};

        let mut types = TypeTable::new();
        let bit = types.bit();
        let byte = types.primitive(PrimType::_8);
        let block = IRBlock::<()> {
            // guard, argument, fallback byte, and element address.
            params: std::vec![bit, bit, byte, bit],
            stmts: std::vec![Node::new(
                IRStmt::ActionStore {
                    name: "write_byte".into(),
                    guard: IRVarId(0),
                    args: std::vec![IRVarId(1)],
                    fallbacks: std::vec![IRVarId(2)],
                    output_tys: std::vec![byte],
                    targets: std::vec![ActionTarget {
                        storage: StorageId::DEFAULT,
                        addr: IRVarId(3),
                    }],
                },
                (),
                None,
            )],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, std::vec![]),
            },
        };
        let mut blocks = IRBlocks::new(std::vec![block]);
        blocks.actions.push(ActionDecl {
            name: "write_byte".into(),
            params: std::vec![bit],
            results: std::vec![byte],
        });

        let lowered = try_lower_ir_to_boolar(&blocks, &types).unwrap();
        let effects: std::vec::Vec<_> = lowered.blocks[0]
            .stmts
            .iter()
            .filter_map(|node| match &node.kind {
                BIrStmt::ActionStoreBit {
                    name,
                    storage,
                    lane,
                    bit,
                    occurrence,
                    ..
                } => Some((name, storage, lane, bit, occurrence)),
                _ => None,
            })
            .collect();
        assert_eq!(effects.len(), 8);
        for (index, (name, storage, lane, bit, occurrence)) in effects.into_iter().enumerate() {
            assert_eq!(name, "write_byte");
            assert_eq!(*storage, StorageId::DEFAULT);
            assert_eq!(*lane, LaneId(0));
            assert_eq!(*bit, index);
            assert_eq!(*occurrence, index as u64);
        }
    }
}
