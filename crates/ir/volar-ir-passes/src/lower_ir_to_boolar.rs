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
use volar_ir_common::{Constant, PolyCoeffs, StorageId};

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
    try_lower_ir_to_boolar_with_tables(blocks, types).map(|(blocks, tables)| (blocks, tables.lanes))
}

/// All side tables of one [`lower_ir_to_boolar`] run: the `LaneId → IRTypeId`
/// lane table, the per-block typed-var → bit-var allocation ([`VarBitMap`]),
/// and the per-`(StorageId, LaneId)` element-address widths. Returned as one
/// bundle so companion metadata (region tables) is lowered from the *same*
/// allocation the lowering itself produced.
#[derive(Clone, Debug, Default)]
pub struct LoweredTables {
    /// Dense `LaneId → IRTypeId` table (same as [`lower_ir_to_boolar_with_lane_table`]).
    pub lanes: BTreeMap<LaneId, IRTypeId>,
    /// Per-host-block typed-var → bit-var lists (LSB-first). Params are
    /// allocated contiguously in param order; statement `i` of block `b`
    /// defines the var keyed at `block.params.len() + i`.
    pub var_bits: VarBitMap,
    /// Element-address bit width per `(StorageId, LaneId)` space, as recorded
    /// from the lowered traffic.
    pub addr_widths: BTreeMap<(StorageId, LaneId), usize>,
}

/// Per-block typed-var → bit-var allocation from one lowering run.
#[derive(Clone, Debug, Default)]
pub struct VarBitMap {
    /// One map per host block (block `b` = `blocks[b]`).
    pub blocks: Vec<BTreeMap<u32, Vec<IRVarId>>>,
}

impl VarBitMap {
    /// Bit-var list of one var of one block (LSB-first), or `None`.
    pub fn bits(&self, block: usize, var: u32) -> Option<&[IRVarId]> {
        self.blocks.get(block)?.get(&var).map(|v| v.as_slice())
    }

    /// The Boolar param position of bit `start` of one var, or `None`.
    /// Valid because params are allocated as contiguous ascending var ids.
    pub fn bit_start(&self, block: usize, var: u32, start: usize) -> Option<u32> {
        self.bits(block, var)?.get(start).map(|v| v.0)
    }
}

/// Like [`lower_ir_to_boolar_with_lane_table`], returning the full side-table
/// bundle ([`LoweredTables`]).
pub fn lower_ir_to_boolar_with_tables<P: Clone>(
    blocks: &IRBlocks<P>,
    types: &IRTypes,
) -> (BIrBlocks<P>, LoweredTables) {
    try_lower_ir_to_boolar_with_tables(blocks, types)
        .unwrap_or_else(|error| panic!("lower_ir_to_boolar: invalid external primitive: {error:?}"))
}

/// Fallible variant of [`lower_ir_to_boolar_with_tables`].
pub fn try_lower_ir_to_boolar_with_tables<P: Clone>(
    blocks: &IRBlocks<P>,
    types: &IRTypes,
) -> Result<(BIrBlocks<P>, LoweredTables), ExternalLoweringError> {
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
    let mut var_bits_per_block: Vec<BTreeMap<u32, Vec<IRVarId>>> =
        Vec::with_capacity(blocks.blocks.len());
    let out_blocks: Vec<BIrBlock<P>> = blocks
        .blocks
        .iter()
        .map(|block| {
            let (b, vb) = lower_block_with_var_bits(
                block,
                types,
                &lane_of,
                &mut addr_widths,
                &mut occurrence,
                None,
            );
            var_bits_per_block.push(vb);
            b
        })
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
        LoweredTables {
            lanes: lane_table,
            var_bits: VarBitMap {
                blocks: var_bits_per_block,
            },
            addr_widths,
        },
    ))
}

/// Per-block side inputs for [`lower_ir_to_boolar_with_sides`].
///
/// `param_sides[b][i]` is the side of Boolar param bit `i` of block `b`
/// (one entry per *bit*, not per typed param — expand a typed param's side
/// across its `ir_type_bits` bits). Blocks without an entry default to all
/// `None` (untracked) params. The side of every other wire is derived by the
/// lowering: derived gates take the `volar_side::propagate` join of their
/// operands' sides, and introduction points (constants, oracle/action
/// outputs) inherit the side stamped on the source IR stmt node.
#[derive(Clone, Debug, Default)]
pub struct SideInputs {
    /// `param_sides[b]` = per-param-bit sides for block `b`.
    pub param_sides: BTreeMap<usize, Vec<Option<volar_side::SideId>>>,
}

/// Side-aware variant of [`lower_ir_to_boolar`].
///
/// Unlike the legacy entry point, every emitted Boolar node carries a side:
/// param bits are seeded from `sides`, and each derived/introduction wire is
/// stamped as described on [`SideInputs`]. This is the pass that lets a real
/// program's side annotations (vc visibilities, MPC party ownership) flow
/// through the bit-level lowering to the weaver, so the input partition for
/// an MPC session is derived from the program rather than hand-specified.
pub fn lower_ir_to_boolar_with_sides<P: Clone>(
    blocks: &IRBlocks<P>,
    types: &IRTypes,
    sides: &SideInputs,
) -> BIrBlocks<P> {
    try_lower_ir_to_boolar_with_sides(blocks, types, sides).unwrap_or_else(|error| {
        panic!("lower_ir_to_boolar_with_sides: invalid external primitive: {error:?}")
    })
}

/// Fallible variant of [`lower_ir_to_boolar_with_sides`].
pub fn try_lower_ir_to_boolar_with_sides<P: Clone>(
    blocks: &IRBlocks<P>,
    types: &IRTypes,
    sides: &SideInputs,
) -> Result<BIrBlocks<P>, ExternalLoweringError> {
    try_lower_ir_to_boolar_side_inner(blocks, types, sides).map(|(blocks, _)| blocks)
}

/// Side-aware variant of [`lower_ir_to_boolar_with_tables`]: the full
/// [`LoweredTables`] bundle plus the side-carrying blocks.
pub fn lower_ir_to_boolar_with_tables_and_sides<P: Clone>(
    blocks: &IRBlocks<P>,
    types: &IRTypes,
    sides: &SideInputs,
) -> (BIrBlocks<P>, LoweredTables) {
    try_lower_ir_to_boolar_side_inner(blocks, types, sides).unwrap_or_else(|error| {
        panic!("lower_ir_to_boolar_with_sides: invalid external primitive: {error:?}")
    })
}

/// Shared driver for the side-aware entry points: identical to
/// [`try_lower_ir_to_boolar_with_tables`] except each block is lowered with
/// its param sides, enabling side propagation.
fn try_lower_ir_to_boolar_side_inner<P: Clone>(
    blocks: &IRBlocks<P>,
    types: &IRTypes,
    sides: &SideInputs,
) -> Result<(BIrBlocks<P>, LoweredTables), ExternalLoweringError> {
    validate_external_sources(blocks, types)?;
    // Lane allocation is identical to the side-agnostic driver.
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

    let mut addr_widths: BTreeMap<(StorageId, LaneId), usize> = BTreeMap::new();
    let mut occurrence = 0u64;
    let mut var_bits_per_block: Vec<BTreeMap<u32, Vec<IRVarId>>> =
        Vec::with_capacity(blocks.blocks.len());
    let out_blocks: Vec<BIrBlock<P>> = blocks
        .blocks
        .iter()
        .enumerate()
        .map(|(bi, block)| {
            // Enable side tracking for every block in the side-aware driver,
            // even blocks with no explicit param sides (empty slice seeds
            // all-None params but still propagates stmt-node sides).
            static EMPTY: &[Option<volar_side::SideId>] = &[];
            let param_sides = sides
                .param_sides
                .get(&bi)
                .map(|v| v.as_slice())
                .or(Some(EMPTY));
            let (b, vb) = lower_block_with_var_bits(
                block,
                types,
                &lane_of,
                &mut addr_widths,
                &mut occurrence,
                param_sides,
            );
            var_bits_per_block.push(vb);
            b
        })
        .collect();

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
        LoweredTables {
            lanes: lane_table,
            var_bits: VarBitMap {
                blocks: var_bits_per_block,
            },
            addr_widths,
        },
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

/// Lower one block, returning the lowered Boolar block together with the
/// block's typed-var → bit-var allocation (see [`VarBitMap`]).
///
/// When `param_sides` is `Some`, side tracking is enabled: param wire `i`
/// is seeded with `param_sides[i]` (one side per *Boolar param bit*, padded
/// with `None`), each statement's emitted wires derive their side from the
/// join of their operands' sides, and introduction points (constants,
/// oracle/action calls) inherit the side stamped on the source IR stmt node.
/// When `None`, every emitted node carries `side: None` (legacy behaviour).
fn lower_block_with_var_bits<P: Clone>(
    block: &IRBlock<P>,
    types: &IRTypes,
    lane_of: &BTreeMap<IRTypeId, LaneId>,
    addr_widths: &mut BTreeMap<(StorageId, LaneId), usize>,
    occurrence: &mut u64,
    param_sides: Option<&[Option<volar_side::SideId>]>,
) -> (BIrBlock<P>, BTreeMap<u32, Vec<IRVarId>>) {
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
    if let Some(sides) = param_sides {
        emitter.enable_side_tracking(total_params, sides);
    }

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
            stmt.side,
        );
    }

    // ---- 3. Convert terminator --------------------------------------------
    let terminator = lower_terminator(&block.terminator, &var_bits);

    (
        BIrBlock {
            params: total_params,
            stmts: emitter.stmts,
            terminator,
        },
        var_bits,
    )
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
    stmt_side: Option<volar_side::SideId>,
) {
    match stmt {
        // ---- Constant ------------------------------------------------------
        IRStmt::Const(c, ty_id) => {
            let w = ir_type_bits(&types.0[ty_id.0 as usize], types);
            let bits: Vec<IRVarId> = (0..w)
                .map(|j| {
                    if constant_bit(c, j) {
                        emitter.emit_with_side(BIrStmt::One, prov.clone(), stmt_side)
                    } else {
                        emitter.emit_with_side(BIrStmt::Zero, prov.clone(), stmt_side)
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
            ty,
            coeffs,
            constant,
        } => {
            // The result type is authoritative. In particular, a pure
            // constant polynomial has no operands from which to infer a
            // width, but still needs one Boolar wire per bit of `ty`.
            let w = ir_type_bits(&types.0[ty.0 as usize], types);
            let bits: Vec<IRVarId> = (0..w)
                .map(|j| {
                    lower_poly_bit(
                        coeffs,
                        constant,
                        j,
                        var_bits,
                        emitter,
                        prov.clone(),
                        stmt_side,
                    )
                })
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

/// Lower the `j`-th output bit of a `Poly` stmt.
///
/// Implements: `result[j] = constant[j] ⊕ ⊕{(mono,coeff): coeff odd} ∧(vars[j])`.
fn lower_poly_bit<P: Clone>(
    coeffs: &PolyCoeffs<IRVarId>,
    constant: &Constant,
    bit: usize,
    var_bits: &BTreeMap<u32, Vec<IRVarId>>,
    emitter: &mut Emitter<P>,
    prov: P,
    stmt_side: Option<volar_side::SideId>,
) -> IRVarId {
    // Accumulator: None means "0 so far".
    let mut acc: Option<IRVarId> = if constant_bit(constant, bit) {
        Some(emitter.emit_with_side(BIrStmt::One, prov.clone(), stmt_side))
    } else {
        None
    };

    for (mono, &coeff) in coeffs {
        if coeff % 2 == 0 {
            continue;
        }
        let mono_var: Option<IRVarId> = if mono.is_empty() {
            // Empty product = 1; contributes a constant One term.
            Some(emitter.emit_with_side(BIrStmt::One, prov.clone(), stmt_side))
        } else {
            // AND of all variable bits at position `bit`.
            let mut and_acc: Option<IRVarId> = None;
            let mut is_zero = false;
            for v in mono {
                let Some(bits) = var_bits.get(&v.0) else {
                    is_zero = true;
                    break;
                };
                let bit_var = match bits.as_slice() {
                    // A Bit operand is a scalar selector and broadcasts to
                    // every lane of the wider polynomial result.
                    [scalar] => *scalar,
                    _ => match bits.get(bit) {
                        Some(bit_var) => *bit_var,
                        // A non-scalar operand with no lane here is a
                        // zero-extended value, so this whole product is 0.
                        None => {
                            is_zero = true;
                            break;
                        }
                    },
                };
                and_acc = Some(match and_acc {
                    None => bit_var,
                    Some(prev) => emitter.emit_poly_and(prev, bit_var, prov.clone()),
                });
            }
            (!is_zero).then_some(and_acc).flatten()
        };

        if let Some(mv) = mono_var {
            acc = Some(match acc {
                None => mv,
                Some(prev) => emitter.emit_poly_xor(prev, mv, prov.clone()),
            });
        }
    }

    // If no terms contributed, result is 0.
    acc.unwrap_or_else(|| emitter.emit_with_side(BIrStmt::Zero, prov, stmt_side))
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

/// Operand wires a [`BIrStmt`] reads, for side propagation.
///
/// This is deliberately conservative: the side of a derived wire is the join
/// of *every* wire the stmt's result can depend on — value operands, plus the
/// guard of a conditional action and the address bits of a storage access
/// (a read's result depends on which cell the address selects). `Zero`/`One`
/// and the introduction handles (`OracleCall`, `Rng`, `ActionCall`) have
/// no value operands here; their side is assigned by the caller via
/// [`Emitter::emit_with_side`], not derived.
fn stmt_operands(stmt: &BIrStmt) -> Vec<IRVarId> {
    match stmt {
        BIrStmt::Zero | BIrStmt::One => vec![],
        BIrStmt::And(a, b) | BIrStmt::Or(a, b) | BIrStmt::Xor(a, b) => vec![*a, *b],
        BIrStmt::Not(v) => vec![*v],
        // Call handles and RNG are introduction points, not derivations:
        // their side comes from the caller, not a join.
        BIrStmt::OracleCall { .. } | BIrStmt::ActionCall { .. } => vec![],
        BIrStmt::Rng { .. } | BIrStmt::RngBit { .. } => vec![],
        // A projected bit derives from the call handle.
        BIrStmt::OracleProjectedBit { call, .. } | BIrStmt::ActionBit { call, .. } => vec![*call],
        // A direct oracle/action bit derives from its argument wires.
        BIrStmt::OracleBit { args, .. } => args.clone(),
        BIrStmt::ActionStoreBit {
            guard,
            args,
            fallback,
            addr,
            ..
        } => {
            let mut v = vec![*guard, *fallback];
            v.extend_from_slice(args);
            v.extend_from_slice(addr);
            v
        }
        BIrStmt::StorageRead { addr, .. } => addr.clone(),
        BIrStmt::StorageWrite { src, addr, .. } => {
            let mut v = vec![*src];
            v.extend_from_slice(addr);
            v
        }
        _ => vec![],
    }
}

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
    /// Bounded hash-cons table for Boolean gates emitted while expanding
    /// `IRStmt::Poly`. This is intentionally direct-mapped rather than a
    /// full unbounded CSE table: linked LLVM programs can contain millions of
    /// distinct gates, while repeated local subexpressions are the useful
    /// sharing opportunity here.
    poly_gates: Option<PolyGateCache>,
    /// Side-propagation state, active only when `side_tracking` is on.
    /// `var_side[v]` is the side of the Boolar wire with var id `v`. The
    /// side of an emitted gate is the `volar_side::propagate` join of its
    /// operand wires' sides; introduction points (params, constants,
    /// oracle/action outputs) carry whatever side the caller stamped.
    side_tracking: bool,
    var_side: Vec<Option<volar_side::SideId>>,
}

impl<P: Clone> Emitter<P> {
    fn new(params: u32) -> Self {
        Emitter {
            stmts: vec![],
            next_var: params,
            const_wires: BTreeMap::new(),
            poly_gates: None,
            side_tracking: false,
            var_side: vec![],
        }
    }

    /// Enable side propagation, seeding the param wires `[0, params)` with
    /// `param_sides` (padded with `None` if shorter). Must be called before
    /// any `emit`.
    fn enable_side_tracking(&mut self, params: u32, param_sides: &[Option<volar_side::SideId>]) {
        self.side_tracking = true;
        self.var_side = (0..params)
            .map(|i| param_sides.get(i as usize).copied().flatten())
            .collect();
    }

    /// Side of an already-allocated wire (param or emitted stmt).
    fn side_of(&self, var: IRVarId) -> Option<volar_side::SideId> {
        self.var_side.get(var.0 as usize).copied().flatten()
    }

    /// Join of the operand wires' sides via `volar_side::propagate`.
    fn joined_side(&self, operands: &[IRVarId]) -> Option<volar_side::SideId> {
        let sides: Vec<Option<volar_side::SideId>> =
            operands.iter().map(|&v| self.side_of(v)).collect();
        volar_side::propagate(&sides)
    }

    fn emit(&mut self, stmt: BIrStmt, prov: P) -> IRVarId {
        let id = IRVarId(self.next_var);
        // Derive the result side from the operands before pushing, while the
        // stmt still names them. `BIrStmt` operands are `IRVarId`s.
        let side = if self.side_tracking {
            self.joined_side(&stmt_operands(&stmt))
        } else {
            None
        };
        self.stmts
            .push(volar_ir_common::Node::new(stmt, prov, side));
        if self.side_tracking {
            self.var_side.push(side);
        }
        self.next_var += 1;
        id
    }

    /// Emit a constant/introduction wire with an explicit side, overriding
    /// the (vacuous, no-operand) join. Used for `Zero`/`One`/introduction
    /// points whose side the caller knows.
    fn emit_with_side(
        &mut self,
        stmt: BIrStmt,
        prov: P,
        side: Option<volar_side::SideId>,
    ) -> IRVarId {
        let id = IRVarId(self.next_var);
        let side = if self.side_tracking { side } else { None };
        self.stmts
            .push(volar_ir_common::Node::new(stmt, prov, side));
        if self.side_tracking {
            self.var_side.push(side);
        }
        self.next_var += 1;
        id
    }

    /// Emit or reuse an AND introduced by `lower_poly_bit`.
    fn emit_poly_and(&mut self, a: IRVarId, b: IRVarId, prov: P) -> IRVarId {
        self.emit_poly_gate(PolyGateKind::And, a, b, prov)
    }

    /// Emit or reuse an XOR introduced by `lower_poly_bit`.
    fn emit_poly_xor(&mut self, a: IRVarId, b: IRVarId, prov: P) -> IRVarId {
        self.emit_poly_gate(PolyGateKind::Xor, a, b, prov)
    }

    fn emit_poly_gate(&mut self, kind: PolyGateKind, a: IRVarId, b: IRVarId, prov: P) -> IRVarId {
        let key = PolyGateKey::new(kind, a, b);
        if let Some(existing) = self.poly_gates.as_ref().and_then(|cache| cache.get(key)) {
            return existing;
        }

        let stmt = match kind {
            PolyGateKind::And => BIrStmt::And(a, b),
            PolyGateKind::Xor => BIrStmt::Xor(a, b),
        };
        let result = self.emit(stmt, prov);
        self.poly_gates
            .get_or_insert_with(PolyGateCache::new)
            .insert(key, result);
        result
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

/// Boolean gate kinds that `lower_poly_bit` can introduce. Both are
/// commutative, so their cache keys use ascending operand IDs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum PolyGateKind {
    And,
    Xor,
}

/// One exact, commutative Boolean-gate key.
#[derive(Clone, Copy, PartialEq, Eq)]
struct PolyGateKey {
    kind: PolyGateKind,
    lo: IRVarId,
    hi: IRVarId,
}

impl PolyGateKey {
    fn new(kind: PolyGateKind, a: IRVarId, b: IRVarId) -> Self {
        if a.0 <= b.0 {
            Self { kind, lo: a, hi: b }
        } else {
            Self { kind, lo: b, hi: a }
        }
    }
}

/// A bounded, direct-mapped cache. Collision replacement can only miss a
/// sharing opportunity; it can never alias two different gates. Keeping this
/// fixed-size prevents the lowerer from retaining every unique SHA gate while
/// still capturing the dense repeated subexpressions movfuscation creates.
struct PolyGateCache {
    slots: Vec<Option<(PolyGateKey, IRVarId)>>,
}

impl PolyGateCache {
    const CAPACITY: usize = 1 << 16;

    fn new() -> Self {
        Self {
            slots: vec![None; Self::CAPACITY],
        }
    }

    fn get(&self, key: PolyGateKey) -> Option<IRVarId> {
        let (stored, value) = self.slots[Self::slot(key)]?;
        (stored == key).then_some(value)
    }

    fn insert(&mut self, key: PolyGateKey, value: IRVarId) {
        self.slots[Self::slot(key)] = Some((key, value));
    }

    fn slot(key: PolyGateKey) -> usize {
        let tag = match key.kind {
            PolyGateKind::And => 0u64,
            PolyGateKind::Xor => 1u64,
        };
        let mut hash = ((key.lo.0 as u64) << 32) | key.hi.0 as u64;
        hash ^= tag.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        hash ^= hash >> 30;
        hash = hash.wrapping_mul(0xbf58_476d_1ce4_e5b9);
        hash ^= hash >> 27;
        hash = hash.wrapping_mul(0x94d0_49bb_1331_11eb);
        hash ^= hash >> 31;
        hash as usize & (Self::CAPACITY - 1)
    }
}

/// Record the element-address width of a storage space; all ops in one
/// `(StorageId, LaneId)` must agree, since the appended-index cell layout is
/// defined relative to it.
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
/// Under the appended-address layout, element `e`, bit `i` lives at the
/// LSB-first address bits for `offset + e`, followed by the bits for `i`.
/// Bits of one element are therefore strided, so one [`BIrPreInitSegment`]
/// is emitted per bit index, each covering the contiguous run of elements at
/// that bit position.
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
    let recorded_addr_bits = *addr_widths.get(&(seg.storage, lane)).unwrap_or(&0);
    let end = seg.offset.saturating_add(seg.data.len().saturating_sub(1));
    let static_addr_bits = if end == 0 {
        0
    } else {
        usize::BITS as usize - end.leading_zeros() as usize
    };
    let n_addr = recorded_addr_bits.max(static_addr_bits);
    let index_bits = if k <= 1 {
        0
    } else {
        (usize::BITS - (k - 1).leading_zeros()) as usize
    };
    (0..k)
        .map(|i| BIrPreInitSegment {
            storage: seg.storage,
            lane,
            addr: (0..n_addr)
                .map(|bit| bit < usize::BITS as usize && (seg.offset >> bit) & 1 != 0)
                .chain((0..index_bits).map(|bit| (i >> bit) & 1 != 0))
                .collect(),
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
    use volar_ir_common::{Constant, Node, StorageId, TypeTable};

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

    #[test]
    fn wide_storage_address_expands_to_exact_70_bit_reversible_cells() {
        // A 64-bit pointer address plus the six-bit suffix that selects one
        // bit of a 64-bit loaded value must survive the Boolar and reversible
        // boundary intact. In particular, it must not be packed into u64.
        let mut types = TypeTable::new();
        let bit = types.bit();
        let addr = types.intern(IRType::Vec(64, bit));
        let word = types.primitive(PrimType::_64);
        let mut block = IRBlock::<()> {
            params: std::vec![addr],
            stmts: std::vec![],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, std::vec![]),
            },
        };
        let read = block.push_stmt(
            volar_ir::ir::IRStmt::StorageRead {
                storage: StorageId::ALLOCA,
                ty: word,
                addr: IRVarId(0),
            },
            (),
        );
        block.terminator = IRTerminator::Jmp {
            target: IRBranchTarget::new(IRBlockTargetId::Return, std::vec![read]),
        };

        let lowered = lower_ir_to_boolar(&IRBlocks::new(std::vec![block]), &types);
        let reads: std::vec::Vec<_> = lowered.blocks[0]
            .stmts
            .iter()
            .filter_map(|node| match &node.kind {
                BIrStmt::StorageRead { addr, .. } => Some(addr),
                _ => None,
            })
            .collect();
        assert_eq!(reads.len(), 64);
        assert!(reads.iter().all(|addr| addr.len() == 70));

        let circuit = crate::to_circuit_fused_boolar(&lowered).expect("single block fuses");
        let (reversible, _) = crate::to_reversible(&circuit).expect("storage circuit lowers");
        let swap_addrs: std::vec::Vec<_> = reversible
            .gates()
            .iter()
            .filter_map(|gate| match gate {
                volar_ir::rcircuit::RGate::StorageSwap { addr, .. } => Some(addr),
                _ => None,
            })
            .collect();
        assert_eq!(
            swap_addrs.len(),
            128,
            "read swaps out and restores each cell"
        );
        assert!(swap_addrs.iter().all(|addr| addr.len() == 70));
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

        let mut coeffs = PolyCoeffs::new();
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

    #[test]
    fn pure_constant_poly_uses_its_declared_width() {
        let mut types = TypeTable::new();
        let byte = types.primitive(PrimType::_8);
        let block = IRBlock::<()> {
            params: std::vec![],
            stmts: std::vec![Node::new(
                volar_ir::ir::IRStmt::Poly {
                    ty: byte,
                    coeffs: PolyCoeffs::new(),
                    constant: Constant {
                        hi: 0,
                        lo: 0b1010_0101
                    },
                },
                (),
                None,
            )],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, std::vec![IRVarId(0)]),
            },
        };

        let lowered = lower_ir_to_boolar(&IRBlocks::new(std::vec![block]), &types);
        let block = &lowered.blocks[0];
        assert_eq!(block.stmts.len(), 8);
        assert_eq!(
            block.terminator,
            BIrTerminator::Jmp(BIrTarget {
                block: IRBlockTargetId::Return,
                args: (0..8).map(IRVarId).collect(),
            }),
        );
        for (bit, node) in block.stmts.iter().enumerate() {
            let expected = if (0b1010_0101 >> bit) & 1 == 1 {
                BIrStmt::One
            } else {
                BIrStmt::Zero
            };
            assert_eq!(node.kind, expected, "incorrect constant bit {bit}");
        }
    }

    #[test]
    fn poly_broadcasts_bit_selectors_across_wider_results() {
        let mut types = TypeTable::new();
        let bit = types.bit();
        let byte = types.primitive(PrimType::_8);
        let mut coeffs = PolyCoeffs::new();
        coeffs.insert(std::vec![IRVarId(0), IRVarId(1)], 1);
        let block = IRBlock::<()> {
            params: std::vec![bit, byte],
            stmts: std::vec![Node::new(
                volar_ir::ir::IRStmt::Poly {
                    ty: byte,
                    coeffs,
                    constant: zero_const(),
                },
                (),
                None,
            )],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, std::vec![IRVarId(2)]),
            },
        };

        let lowered = lower_ir_to_boolar(&IRBlocks::new(std::vec![block]), &types);
        let block = &lowered.blocks[0];
        assert_eq!(block.stmts.len(), 8);
        for (bit, node) in block.stmts.iter().enumerate() {
            assert_eq!(
                node.kind,
                BIrStmt::And(IRVarId(0), IRVarId((bit + 1) as u32))
            );
        }
    }

    #[test]
    fn repeated_wide_polys_reuse_their_boolar_gate_dag() {
        let mut types = TypeTable::new();
        let bit = types.bit();
        let byte = types.primitive(PrimType::_8);
        let mut coeffs = PolyCoeffs::new();
        // Per output bit this is
        // selector XOR (selector AND value_bit) XOR value_bit.
        coeffs.insert(std::vec![IRVarId(0)], 1);
        coeffs.insert(std::vec![IRVarId(0), IRVarId(1)], 1);
        coeffs.insert(std::vec![IRVarId(1)], 1);
        let block = IRBlock::<()> {
            params: std::vec![bit, byte],
            stmts: std::vec![
                Node::new(
                    IRStmt::Poly {
                        ty: byte,
                        coeffs: coeffs.clone(),
                        constant: zero_const(),
                    },
                    (),
                    None,
                ),
                Node::new(
                    IRStmt::Poly {
                        ty: byte,
                        coeffs,
                        constant: zero_const(),
                    },
                    (),
                    None,
                ),
            ],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(
                    IRBlockTargetId::Return,
                    std::vec![IRVarId(2), IRVarId(3)],
                ),
            },
        };

        let lowered = lower_ir_to_boolar(&IRBlocks::new(std::vec![block]), &types);
        let block = &lowered.blocks[0];
        // The first byte emits one AND and two XOR gates per lane. The
        // identical second polynomial aliases all 24 emitted gates rather
        // than recreating another DAG.
        assert_eq!(block.stmts.len(), 24);
        let BIrTerminator::Jmp(BIrTarget { args, .. }) = &block.terminator else {
            panic!("expected return jump");
        };
        assert_eq!(args.len(), 16);
        assert_eq!(&args[..8], &args[8..]);
    }

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

            execution: volar_ir_common::ActionExecutionPolicy::legacy_evaluator(),
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

    // -- Side propagation (lower_ir_to_boolar_with_sides) -------------------

    use volar_side::SideId;

    /// Two AES8 params XORed bit-wise via Poly; returns the lowered block so
    /// tests can inspect per-wire sides.
    fn xor_two_params_block() -> (IRBlocks<()>, IRTypes) {
        let mut types = TypeTable::new();
        let aes8_id = types.primitive(PrimType::AES8);
        let mut coeffs = PolyCoeffs::new();
        coeffs.insert(std::vec![IRVarId(0)], 1);
        coeffs.insert(std::vec![IRVarId(1)], 1);
        let block = IRBlock::<()> {
            params: std::vec![aes8_id, aes8_id],
            stmts: std::vec![Node::new(
                volar_ir::ir::IRStmt::Poly {
                    ty: aes8_id,
                    coeffs,
                    constant: zero_const(),
                },
                (),
                None,
            )],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, std::vec![IRVarId(2)]),
            },
        };
        (IRBlocks::new(std::vec![block]), types)
    }

    #[test]
    fn sides_propagate_through_derived_xor() {
        let (blocks, types) = xor_two_params_block();
        let side_a = SideId(1);
        // Param 0 (bits 0..8) on side A; param 1 (bits 8..16) untracked.
        let mut param_sides = vec![None; 16];
        for s in param_sides.iter_mut().take(8) {
            *s = Some(side_a);
        }
        let sides = SideInputs {
            param_sides: [(0, param_sides)].into_iter().collect(),
        };
        let lowered = lower_ir_to_boolar_with_sides(&blocks, &types, &sides);
        let b = &lowered.blocks[0];
        // Each output bit is Xor(param0_bit, param1_bit); the join of
        // Some(A) and None is Some(A), so every derived Xor carries side A.
        assert_eq!(b.stmts.len(), 8);
        for node in &b.stmts {
            assert!(matches!(&node.kind, BIrStmt::Xor(_, _)));
            assert_eq!(node.side, Some(side_a), "derived Xor must inherit side A");
        }
    }

    #[test]
    fn conflicting_operand_sides_yield_none() {
        let (blocks, types) = xor_two_params_block();
        let side_a = SideId(1);
        let side_b = SideId(2);
        // Param 0 on side A, param 1 on side B — a mixed wire is not
        // attributable to a single side, so the join is None.
        let mut param_sides = vec![None; 16];
        for s in param_sides.iter_mut().take(8) {
            *s = Some(side_a);
        }
        for s in param_sides.iter_mut().skip(8) {
            *s = Some(side_b);
        }
        let sides = SideInputs {
            param_sides: [(0, param_sides)].into_iter().collect(),
        };
        let lowered = lower_ir_to_boolar_with_sides(&blocks, &types, &sides);
        for node in &lowered.blocks[0].stmts {
            assert_eq!(node.side, None, "mixed-side Xor must be unattributable");
        }
    }

    #[test]
    fn legacy_lowering_stamps_no_sides() {
        // The side-agnostic entry point must be unchanged: all sides None.
        let (blocks, types) = xor_two_params_block();
        let lowered = lower_ir_to_boolar::<()>(&blocks, &types);
        for node in &lowered.blocks[0].stmts {
            assert_eq!(node.side, None);
        }
    }

    #[test]
    fn constant_inherits_stmt_side() {
        // A pure-constant Poly introduction point inherits the side stamped
        // on its source IR stmt node.
        let mut types = TypeTable::new();
        let u8_id = types.primitive(PrimType::_8);
        let const_side = SideId(7);
        let block = IRBlock::<()> {
            params: std::vec![],
            stmts: std::vec![Node::new(
                volar_ir::ir::IRStmt::Poly {
                    ty: u8_id,
                    coeffs: PolyCoeffs::new(),
                    constant: Constant { lo: 0b1010, hi: 0 },
                },
                (),
                Some(const_side),
            )],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, std::vec![IRVarId(0)]),
            },
        };
        let blocks = IRBlocks::new(std::vec![block]);
        let sides = SideInputs::default();
        let lowered = lower_ir_to_boolar_with_sides(&blocks, &types, &sides);
        let b = &lowered.blocks[0];
        assert_eq!(b.stmts.len(), 8);
        for node in &b.stmts {
            assert!(matches!(&node.kind, BIrStmt::Zero | BIrStmt::One));
            assert_eq!(
                node.side,
                Some(const_side),
                "constant bit must inherit stmt side"
            );
        }
    }
}
