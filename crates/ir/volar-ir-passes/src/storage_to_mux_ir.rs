// @reliability: experimental
// @ai: assisted
//! Pass: promote a small, bounded Volar IR storage space into an explicit
//! MUX/demux register file, eliminating `Stmt::StorageRead` / `StorageWrite`
//! for one [`StorageId`] before Volar→Boolar lowering ever sees them.
//!
//! # Algorithm
//!
//! Walks the single block's statement list in program order, tracking one
//! SSA value per cell (`cells: Vec<u32>`, seeded from any matching
//! `PreInitSegment`, else a zero `Const`). Each `StorageRead { addr, .. }`
//! targeting this pass's storage id is replaced by an N-way MUX selecting
//! `cells[i]` where `addr == i`, built from the same linear-combination
//! primitives (`is_active · val`, `a + b` in the GF(2^n) field embedding)
//! movfuscation already uses for its own state-slot dispatch, and address
//! equality is built the same way movfuscation builds `pc == k` (`diff = addr
//! XOR k`, AND the NOT of every bit of `diff`). Each `StorageWrite { addr,
//! src, .. }` is replaced by an N-way demux updating every cell: `cells[i]' =
//! mux(addr == i, src, cells[i])`. All other statements are copied through
//! unchanged, with operand vars remapped through the running old-var →
//! new-var substitution.
//!
//! # Preconditions
//!
//! Requires already-fused, single-block, `Jmp(Return)`-terminated Volar IR
//! (`is_circuit()`) — run `movfuscate_ir` / `unroll_ir_everything` first.
//! Fails closed with [`StorageToMuxError::AddressTooNarrow`] if the address
//! type's bit width can't distinguish `num_cells` distinct indices — a real,
//! mechanically-checkable bound. It does **not** attempt to prove every
//! *runtime* address value stays in range: `num_cells` must be supplied by
//! the caller as (at least) the storage's true declared size, or
//! out-of-range addresses will silently read zero / drop writes.

use alloc::vec;
use alloc::vec::Vec;

use volar_ir::ir::{
    IRBlock, IRBlockTargetId, IRBlocks, IRBranchTarget, IRStmt, IRTerminator, IRType, IRTypeId,
    IRTypes, IRVarId, PrimType as Type,
};
use volar_ir_common::{Constant, PolyCoeffs, StorageAccess, StorageId, StorageTable};

/// Which storage id to eliminate, the value type of each cell, and the
/// declared cell count.
///
/// `num_cells` must cover every address the source program can ever compute
/// for this storage — this pass has no way to verify that from the SSA
/// alone (see module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageToMuxConfig {
    pub storage: StorageId,
    pub ty: IRTypeId,
    pub num_cells: usize,
}

/// Why a Volar IR storage-to-MUX promotion failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StorageToMuxError {
    /// The sidecar proves this storage immutable, but the input contains a
    /// write; do not lower an invalid immutable claim.
    ReadOnlyWrite { storage: StorageId },
    /// The input isn't a single `Jmp(Return)`-terminated block; run
    /// `movfuscate_ir` / `unroll_ir_everything` first.
    NotSingleBlockCircuit,
    /// `num_cells` was zero.
    ZeroCells,
    /// An address's type has no supported bit-width interpretation (must be
    /// a primitive scalar: `Bit`/`_8`/`_16`/`_32`/`_64`/`_128`/`_256`/`AES8`/
    /// `Galois64`).
    UnsupportedAddressType { ty: IRTypeId },
    /// The address type is too narrow to address `num_cells` distinct cells.
    AddressTooNarrow { addr_bits: usize, num_cells: usize },
}

impl core::fmt::Display for StorageToMuxError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            StorageToMuxError::ReadOnlyWrite { storage } => write!(
                f,
                "storage_to_mux_ir: read-only storage {storage:?} has a write"
            ),
            StorageToMuxError::NotSingleBlockCircuit => write!(
                f,
                "storage_to_mux_ir requires single-block circuit-shaped Volar IR; run movfuscate_ir or unroll_ir_everything first"
            ),
            StorageToMuxError::ZeroCells => {
                write!(f, "storage_to_mux_ir: num_cells must be nonzero")
            }
            StorageToMuxError::UnsupportedAddressType { ty } => write!(
                f,
                "storage_to_mux_ir: address type {ty:?} has no supported bit width"
            ),
            StorageToMuxError::AddressTooNarrow {
                addr_bits,
                num_cells,
            } => write!(
                f,
                "storage_to_mux_ir: address is {addr_bits} bits wide, too narrow to address {num_cells} cells"
            ),
        }
    }
}

impl core::error::Error for StorageToMuxError {}

/// Eliminate `StorageRead`/`StorageWrite` for `cfg.storage` by promoting it
/// to an explicit MUX/demux register file. See the module docs for the
/// algorithm and its correctness contract. May intern a `Bit` type into
/// `types` if one isn't already present.
pub fn storage_to_mux_ir<P: Clone + Default>(
    blocks: &IRBlocks<P>,
    types: &mut IRTypes,
    cfg: &StorageToMuxConfig,
) -> Result<IRBlocks<P>, StorageToMuxError> {
    storage_to_mux_ir_with_access(blocks, types, cfg, None)
}

/// As [`storage_to_mux_ir`], with an optional mutability sidecar. A read-only
/// target may be promoted from its static `pre_init` image, but any write is
/// rejected rather than silently weakening the proof.
pub fn storage_to_mux_ir_with_access<P: Clone + Default>(
    blocks: &IRBlocks<P>,
    types: &mut IRTypes,
    cfg: &StorageToMuxConfig,
    storage_access: Option<&StorageTable>,
) -> Result<IRBlocks<P>, StorageToMuxError> {
    if storage_access.is_some_and(|table| table.access_of(cfg.storage) == StorageAccess::ReadOnly)
        && blocks.blocks.iter().flat_map(|block| block.stmts.iter()).any(|node| {
            matches!(node.kind, IRStmt::StorageWrite { storage, .. } if storage == cfg.storage)
        })
    {
        return Err(StorageToMuxError::ReadOnlyWrite { storage: cfg.storage });
    }
    if !blocks.is_circuit() {
        return Err(StorageToMuxError::NotSingleBlockCircuit);
    }
    if cfg.num_cells == 0 {
        return Err(StorageToMuxError::ZeroCells);
    }
    let bit_ty = types.bit();

    let old_block = &blocks.blocks[0];
    let mut new_block: IRBlock<P> = IRBlock {
        params: old_block.params.clone(),
        stmts: Vec::new(),
        terminator: old_block.terminator.clone(),
    };
    let mut remap: Vec<u32> = (0..old_block.params.len() as u32).collect();
    let mut var_types: Vec<IRTypeId> = old_block.params.clone();

    let mut cells = init_cells(&mut new_block, &mut var_types, blocks, cfg);

    for node in old_block.stmts.iter() {
        match &node.kind {
            IRStmt::StorageRead { storage, ty, addr } if *storage == cfg.storage => {
                let addr_new = remap[addr.0 as usize];
                let addr_ty = var_types[addr_new as usize];
                check_addr_width(types, addr_ty, cfg.num_cells)?;
                let result = mux_read(
                    &mut new_block,
                    &mut var_types,
                    types,
                    bit_ty,
                    &cells,
                    addr_new,
                    addr_ty,
                    *ty,
                    node.prov.clone(),
                )?;
                remap.push(result);
            }
            IRStmt::StorageWrite {
                storage,
                src,
                ty,
                addr,
            } if *storage == cfg.storage => {
                let addr_new = remap[addr.0 as usize];
                let addr_ty = var_types[addr_new as usize];
                check_addr_width(types, addr_ty, cfg.num_cells)?;
                let src_new = remap[src.0 as usize];
                cells = mux_write(
                    &mut new_block,
                    &mut var_types,
                    types,
                    bit_ty,
                    &cells,
                    addr_new,
                    addr_ty,
                    src_new,
                    *ty,
                    node.prov.clone(),
                )?;
                let dummy = push_typed(
                    &mut new_block,
                    &mut var_types,
                    IRStmt::Const(Constant { hi: 0, lo: 0 }, *ty),
                    *ty,
                    node.prov.clone(),
                );
                remap.push(dummy);
            }
            other => {
                let remapped = other
                    .clone()
                    .map_var(
                        &mut (),
                        &mut |_: &mut (), v: IRVarId| -> Result<IRVarId, core::convert::Infallible> {
                            Ok(IRVarId(remap[v.0 as usize]))
                        },
                        &mut |_: &mut (), t| Ok(t),
                        &mut |_: &mut (), s| Ok(s),
                    )
                    .unwrap();
                let ty = stmt_result_type(&remapped).unwrap_or(bit_ty);
                let id = new_block.push_stmt(remapped, node.prov.clone());
                var_types.push(ty);
                remap.push(id.0);
            }
        }
    }

    new_block.terminator = remap_terminator(&old_block.terminator, &remap);

    let pre_init = blocks
        .pre_init
        .iter()
        .filter(|seg| seg.storage != cfg.storage)
        .cloned()
        .collect();

    Ok(IRBlocks {
        oracles: blocks.oracles.clone(),
        actions: blocks.actions.clone(),
        rngs: blocks.rngs.clone(),
        blocks: vec![new_block],
        pre_init,
    })
}

/// Effect-only statements (matching [`lower_ir_to_boolar`](crate::lower_ir_to_boolar)'s
/// own convention) produce no type.
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

fn bit_width_for_ty(types: &IRTypes, ty: IRTypeId) -> Result<usize, StorageToMuxError> {
    match &types.0[ty.0 as usize] {
        IRType::Primitive(p) => Ok(match p {
            Type::Bit => 1,
            Type::_8 => 8,
            Type::_16 => 16,
            Type::_32 => 32,
            Type::_64 => 64,
            Type::_128 => 128,
            Type::_256 => 256,
            Type::AES8 => 8,
            Type::Galois64 => 64,
            Type::Z3 => return Err(StorageToMuxError::UnsupportedAddressType { ty }),
            _ => return Err(StorageToMuxError::UnsupportedAddressType { ty }),
        }),
        _ => Err(StorageToMuxError::UnsupportedAddressType { ty }),
    }
}

fn check_addr_width(
    types: &IRTypes,
    addr_ty: IRTypeId,
    num_cells: usize,
) -> Result<(), StorageToMuxError> {
    let bits = bit_width_for_ty(types, addr_ty)?;
    let capacity = 1u128 << bits.min(127);
    if (num_cells as u128) > capacity {
        return Err(StorageToMuxError::AddressTooNarrow {
            addr_bits: bits,
            num_cells,
        });
    }
    Ok(())
}

fn push_typed<P: Clone>(
    block: &mut IRBlock<P>,
    var_types: &mut Vec<IRTypeId>,
    stmt: IRStmt,
    ty: IRTypeId,
    prov: P,
) -> u32 {
    let id = block.push_stmt(stmt, prov);
    var_types.push(ty);
    id.0
}

fn emit_poly<P: Clone>(
    block: &mut IRBlock<P>,
    var_types: &mut Vec<IRTypeId>,
    coeffs: PolyCoeffs<IRVarId>,
    constant: Constant,
    ty: IRTypeId,
    prov: P,
) -> u32 {
    push_typed(
        block,
        var_types,
        IRStmt::Poly {
            ty,
            coeffs,
            constant,
        },
        ty,
        prov,
    )
}

/// `a AND b` (both Bit-typed).
fn emit_and_bit<P: Clone>(
    block: &mut IRBlock<P>,
    var_types: &mut Vec<IRTypeId>,
    bit_ty: IRTypeId,
    a: u32,
    b: u32,
    prov: P,
) -> u32 {
    let mut key = vec![IRVarId(a), IRVarId(b)];
    key.sort();
    let mut coeffs = PolyCoeffs::new();
    coeffs.insert(key, 1u8);
    emit_poly(
        block,
        var_types,
        coeffs,
        Constant { hi: 0, lo: 0 },
        bit_ty,
        prov,
    )
}

/// `NOT a` (Bit-typed) = `1 + a` in GF(2).
fn emit_not_bit<P: Clone>(
    block: &mut IRBlock<P>,
    var_types: &mut Vec<IRTypeId>,
    bit_ty: IRTypeId,
    a: u32,
    prov: P,
) -> u32 {
    let mut coeffs = PolyCoeffs::new();
    coeffs.insert(vec![IRVarId(a)], 1);
    emit_poly(
        block,
        var_types,
        coeffs,
        Constant { hi: 0, lo: 1 },
        bit_ty,
        prov,
    )
}

/// `val XOR const_k` (field addition in GF(2^n)), result typed `ty`.
fn emit_poly_xor_const<P: Clone>(
    block: &mut IRBlock<P>,
    var_types: &mut Vec<IRTypeId>,
    val: u32,
    const_k: Constant,
    ty: IRTypeId,
    prov: P,
) -> u32 {
    let mut coeffs = PolyCoeffs::new();
    coeffs.insert(vec![IRVarId(val)], 1u8);
    emit_poly(block, var_types, coeffs, const_k, ty, prov)
}

/// Extract bit `bit_j` of `src` as a fresh Bit-typed var.
fn emit_shuffle_bit<P: Clone>(
    block: &mut IRBlock<P>,
    var_types: &mut Vec<IRTypeId>,
    bit_ty: IRTypeId,
    src: u32,
    bit_j: u8,
    prov: P,
) -> u32 {
    push_typed(
        block,
        var_types,
        IRStmt::Shuffle {
            result_bits: vec![(bit_j, IRVarId(src))],
            ty: bit_ty,
        },
        bit_ty,
        prov,
    )
}

/// `is_active · val` — scalar multiplication of `val: ty` by a Bit.
fn emit_gate<P: Clone>(
    block: &mut IRBlock<P>,
    var_types: &mut Vec<IRTypeId>,
    is_active: u32,
    val: u32,
    ty: IRTypeId,
    prov: P,
) -> u32 {
    let mut key = vec![IRVarId(is_active), IRVarId(val)];
    key.sort();
    let mut coeffs = PolyCoeffs::new();
    coeffs.insert(key, 1u8);
    emit_poly(
        block,
        var_types,
        coeffs,
        Constant { hi: 0, lo: 0 },
        ty,
        prov,
    )
}

/// `a + b` — field addition, both operands typed `ty`.
fn emit_field_add<P: Clone>(
    block: &mut IRBlock<P>,
    var_types: &mut Vec<IRTypeId>,
    a: u32,
    b: u32,
    ty: IRTypeId,
    prov: P,
) -> u32 {
    let mut coeffs = PolyCoeffs::new();
    coeffs.insert(vec![IRVarId(a)], 1);
    coeffs.insert(vec![IRVarId(b)], 1);
    emit_poly(
        block,
        var_types,
        coeffs,
        Constant { hi: 0, lo: 0 },
        ty,
        prov,
    )
}

/// `1` iff `val == const_k`.
fn emit_eq_const<P: Clone>(
    block: &mut IRBlock<P>,
    var_types: &mut Vec<IRTypeId>,
    types: &IRTypes,
    bit_ty: IRTypeId,
    val: u32,
    const_k: Constant,
    ty: IRTypeId,
    prov: P,
) -> Result<u32, StorageToMuxError> {
    if matches!(types.0[ty.0 as usize], IRType::Primitive(Type::Bit)) {
        return Ok(if const_k.lo == 0 {
            emit_not_bit(block, var_types, bit_ty, val, prov)
        } else {
            val
        });
    }
    let diff = emit_poly_xor_const(block, var_types, val, const_k, ty, prov.clone());
    let width = bit_width_for_ty(types, ty)?;
    let mut is_zero = push_typed(
        block,
        var_types,
        IRStmt::Const(Constant { hi: 0, lo: 1 }, bit_ty),
        bit_ty,
        prov.clone(),
    );
    for j in 0..width as u8 {
        let bit_j = emit_shuffle_bit(block, var_types, bit_ty, diff, j, prov.clone());
        let not_j = emit_not_bit(block, var_types, bit_ty, bit_j, prov.clone());
        is_zero = emit_and_bit(block, var_types, bit_ty, is_zero, not_j, prov.clone());
    }
    Ok(is_zero)
}

/// `mux(cond, a, b) = cond·a + (NOT cond)·b`.
#[allow(clippy::too_many_arguments)]
fn mux<P: Clone>(
    block: &mut IRBlock<P>,
    var_types: &mut Vec<IRTypeId>,
    bit_ty: IRTypeId,
    cond: u32,
    a: u32,
    b: u32,
    ty: IRTypeId,
    prov: P,
) -> u32 {
    let not_cond = emit_not_bit(block, var_types, bit_ty, cond, prov.clone());
    let gated_a = emit_gate(block, var_types, cond, a, ty, prov.clone());
    let gated_b = emit_gate(block, var_types, not_cond, b, ty, prov.clone());
    emit_field_add(block, var_types, gated_a, gated_b, ty, prov)
}

#[allow(clippy::too_many_arguments)]
fn mux_read<P: Clone>(
    block: &mut IRBlock<P>,
    var_types: &mut Vec<IRTypeId>,
    types: &IRTypes,
    bit_ty: IRTypeId,
    cells: &[u32],
    addr: u32,
    addr_ty: IRTypeId,
    val_ty: IRTypeId,
    prov: P,
) -> Result<u32, StorageToMuxError> {
    let mut acc = push_typed(
        block,
        var_types,
        IRStmt::Const(Constant { hi: 0, lo: 0 }, val_ty),
        val_ty,
        prov.clone(),
    );
    for (i, &cell) in cells.iter().enumerate() {
        let const_i = Constant {
            hi: 0,
            lo: i as u128,
        };
        let eq_i = emit_eq_const(
            block,
            var_types,
            types,
            bit_ty,
            addr,
            const_i,
            addr_ty,
            prov.clone(),
        )?;
        acc = mux(
            block,
            var_types,
            bit_ty,
            eq_i,
            cell,
            acc,
            val_ty,
            prov.clone(),
        );
    }
    Ok(acc)
}

#[allow(clippy::too_many_arguments)]
fn mux_write<P: Clone>(
    block: &mut IRBlock<P>,
    var_types: &mut Vec<IRTypeId>,
    types: &IRTypes,
    bit_ty: IRTypeId,
    cells: &[u32],
    addr: u32,
    addr_ty: IRTypeId,
    src: u32,
    val_ty: IRTypeId,
    prov: P,
) -> Result<Vec<u32>, StorageToMuxError> {
    cells
        .iter()
        .enumerate()
        .map(|(i, &cell)| {
            let const_i = Constant {
                hi: 0,
                lo: i as u128,
            };
            let eq_i = emit_eq_const(
                block,
                var_types,
                types,
                bit_ty,
                addr,
                const_i,
                addr_ty,
                prov.clone(),
            )?;
            Ok(mux(
                block,
                var_types,
                bit_ty,
                eq_i,
                src,
                cell,
                val_ty,
                prov.clone(),
            ))
        })
        .collect()
}

fn init_cells<P: Clone>(
    block: &mut IRBlock<P>,
    var_types: &mut Vec<IRTypeId>,
    blocks: &IRBlocks<P>,
    cfg: &StorageToMuxConfig,
) -> Vec<u32>
where
    P: Default,
{
    let mut values = vec![Constant { hi: 0, lo: 0 }; cfg.num_cells];
    for seg in &blocks.pre_init {
        if seg.storage != cfg.storage {
            continue;
        }
        for (k, &c) in seg.data.iter().enumerate() {
            let idx = seg.offset + k;
            if idx < cfg.num_cells {
                values[idx] = c;
            }
        }
    }
    values
        .into_iter()
        .map(|c| {
            push_typed(
                block,
                var_types,
                IRStmt::Const(c, cfg.ty),
                cfg.ty,
                P::default(),
            )
        })
        .collect()
}

fn remap_terminator(term: &IRTerminator, remap: &[u32]) -> IRTerminator {
    let remap_target = |t: &IRBranchTarget| IRBranchTarget {
        dest: match &t.dest {
            IRBlockTargetId::Dyn(v) => IRBlockTargetId::Dyn(IRVarId(remap[v.0 as usize])),
            other => other.clone(),
        },
        args: t
            .args
            .iter()
            .map(|v| IRVarId(remap[v.0 as usize]))
            .collect(),
        reentry: t.reentry.clone(),
    };
    match term {
        IRTerminator::Jmp { target } => IRTerminator::Jmp {
            target: remap_target(target),
        },
        IRTerminator::JumpCond {
            condition,
            then_target,
            else_target,
        } => IRTerminator::JumpCond {
            condition: IRVarId(remap[condition.0 as usize]),
            then_target: remap_target(then_target),
            else_target: remap_target(else_target),
        },
        IRTerminator::JumpTable { index, cases } => IRTerminator::JumpTable {
            index: IRVarId(remap[index.0 as usize]),
            cases: cases.iter().map(|(c, t)| (*c, remap_target(t))).collect(),
        },
        _ => panic!("storage_to_mux_ir: unsupported IRTerminator variant"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;
    use volar_ir::ir::{IRBlockId, IRBranchTarget};
    use volar_ir_common::Node;

    /// Minimal interpreter for the Bit-only-fixture subset this pass's own
    /// tests use: `Const`, `Poly` (GF(2) semantics), single-bit `Shuffle`,
    /// and stateful storage read/write.
    fn eval(
        stmts: &[Node<IRStmt, ()>],
        params: &[bool],
        storage: &mut BTreeMap<u64, bool>,
    ) -> Vec<bool> {
        let mut vals = params.to_vec();
        for node in stmts {
            let v = match &node.kind {
                IRStmt::Const(c, _) => (c.lo & 1) == 1,
                IRStmt::Poly {
                    coeffs, constant, ..
                } => {
                    let mut acc = (constant.lo & 1) == 1;
                    for (mono, coeff) in coeffs {
                        if coeff % 2 == 0 {
                            continue;
                        }
                        acc ^= mono.iter().all(|v| vals[v.0 as usize]);
                    }
                    acc
                }
                IRStmt::Shuffle { result_bits, .. } => {
                    assert_eq!(result_bits.len(), 1, "test fixture is Bit-only");
                    let (bit_j, src) = &result_bits[0];
                    assert_eq!(*bit_j, 0, "test fixture is Bit-only");
                    vals[src.0 as usize]
                }
                IRStmt::StorageRead { addr, .. } => {
                    let key = vals[addr.0 as usize] as u64;
                    *storage.get(&key).unwrap_or(&false)
                }
                IRStmt::StorageWrite { src, addr, .. } => {
                    let key = vals[addr.0 as usize] as u64;
                    storage.insert(key, vals[src.0 as usize]);
                    false
                }
                other => panic!("test interpreter: unsupported stmt {other:?}"),
            };
            vals.push(v);
        }
        vals
    }

    fn return_values(term: &IRTerminator, vals: &[bool]) -> Vec<bool> {
        match term {
            IRTerminator::Jmp {
                target:
                    IRBranchTarget {
                        dest: IRBlockTargetId::Return,
                        args,
                        ..
                    },
            } => args.iter().map(|v| vals[v.0 as usize]).collect(),
            other => panic!("expected Jmp(Return), got {other:?}"),
        }
    }

    fn bit_types() -> IRTypes {
        IRTypes(vec![IRType::Primitive(Type::Bit)])
    }

    /// Build: write(addr=1, val=1), then return [read(addr=0), read(addr=1)].
    /// Expect [false, true] — write only touches cell 1.
    fn build_fixture(bit_ty: IRTypeId) -> IRBlocks<()> {
        let storage = StorageId(0);
        let mut block: IRBlock<()> = IRBlock {
            params: vec![],
            stmts: vec![],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, vec![]),
            },
        };
        let addr1 = block.push_stmt(IRStmt::Const(Constant { hi: 0, lo: 1 }, bit_ty), ());
        let val1 = block.push_stmt(IRStmt::Const(Constant { hi: 0, lo: 1 }, bit_ty), ());
        block.push_stmt(
            IRStmt::StorageWrite {
                storage,
                src: val1,
                ty: bit_ty,
                addr: addr1,
            },
            (),
        );
        let addr0 = block.push_stmt(IRStmt::Const(Constant { hi: 0, lo: 0 }, bit_ty), ());
        let read0 = block.push_stmt(
            IRStmt::StorageRead {
                storage,
                ty: bit_ty,
                addr: addr0,
            },
            (),
        );
        let read1 = block.push_stmt(
            IRStmt::StorageRead {
                storage,
                ty: bit_ty,
                addr: addr1,
            },
            (),
        );
        block.terminator = IRTerminator::Jmp {
            target: IRBranchTarget::new(IRBlockTargetId::Return, vec![read0, read1]),
        };
        IRBlocks::new(vec![block])
    }

    #[test]
    fn eliminates_storage_and_preserves_semantics() {
        let mut types = bit_types();
        let bit_ty = types.bit();
        let blocks = build_fixture(bit_ty);

        let mut storage = BTreeMap::new();
        let before = return_values(
            &blocks.blocks[0].terminator,
            &eval(&blocks.blocks[0].stmts, &[], &mut storage),
        );
        assert_eq!(before, vec![false, true]);

        let cfg = StorageToMuxConfig {
            storage: StorageId(0),
            ty: bit_ty,
            num_cells: 2,
        };
        let rewritten = storage_to_mux_ir(&blocks, &mut types, &cfg).expect("pass should succeed");
        assert!(rewritten.is_circuit());
        for node in &rewritten.blocks[0].stmts {
            assert!(
                !matches!(
                    node.kind,
                    IRStmt::StorageRead { .. } | IRStmt::StorageWrite { .. }
                ),
                "storage op survived: {:?}",
                node.kind
            );
        }

        let mut no_storage = BTreeMap::new();
        let after = return_values(
            &rewritten.blocks[0].terminator,
            &eval(&rewritten.blocks[0].stmts, &[], &mut no_storage),
        );
        assert_eq!(after, before);
    }

    #[test]
    fn readonly_sidecar_rejects_writes() {
        let storage = StorageId(0);
        let mut types = bit_types();
        let blocks = build_fixture(IRTypeId(0));
        let mut sidecar = StorageTable::new();
        sidecar.set(storage, StorageAccess::ReadOnly);
        assert_eq!(
            storage_to_mux_ir_with_access(
                &blocks,
                &mut types,
                &StorageToMuxConfig {
                    storage,
                    ty: IRTypeId(0),
                    num_cells: 2,
                },
                Some(&sidecar),
            ),
            Err(StorageToMuxError::ReadOnlyWrite { storage })
        );
    }

    #[test]
    fn readonly_sidecar_allows_read_only_promotion() {
        let storage = StorageId(0);
        let mut types = bit_types();
        let mut block: IRBlock<()> = IRBlock {
            params: vec![],
            stmts: vec![],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, vec![]),
            },
        };
        let address = block.push_stmt(IRStmt::Const(Constant { hi: 0, lo: 1 }, IRTypeId(0)), ());
        let value = block.push_stmt(
            IRStmt::StorageRead {
                storage,
                ty: IRTypeId(0),
                addr: address,
            },
            (),
        );
        block.terminator = IRTerminator::Jmp {
            target: IRBranchTarget::new(IRBlockTargetId::Return, vec![value]),
        };
        let blocks = IRBlocks::new(vec![block]);
        let mut sidecar = StorageTable::new();
        sidecar.set(storage, StorageAccess::ReadOnly);
        let rewritten = storage_to_mux_ir_with_access(
            &blocks,
            &mut types,
            &StorageToMuxConfig {
                storage,
                ty: IRTypeId(0),
                num_cells: 2,
            },
            Some(&sidecar),
        )
        .expect("read-only storage can be promoted");
        assert!(rewritten.is_circuit());
        assert!(!rewritten.blocks[0].stmts.iter().any(
            |node| matches!(node.kind, IRStmt::StorageRead { storage: s, .. } if s == storage)
        ));
    }

    #[test]
    fn rejects_non_circuit_shape() {
        let mut types = bit_types();
        let bit_ty = types.bit();
        let block: IRBlock<()> = IRBlock {
            params: vec![],
            stmts: vec![],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Block(IRBlockId(1)), vec![]),
            },
        };
        let blocks = IRBlocks::new(vec![
            block,
            IRBlock {
                params: vec![],
                stmts: vec![],
                terminator: IRTerminator::Jmp {
                    target: IRBranchTarget::new(IRBlockTargetId::Return, vec![]),
                },
            },
        ]);
        let cfg = StorageToMuxConfig {
            storage: StorageId(0),
            ty: bit_ty,
            num_cells: 2,
        };
        assert_eq!(
            storage_to_mux_ir(&blocks, &mut types, &cfg).unwrap_err(),
            StorageToMuxError::NotSingleBlockCircuit
        );
    }
}
