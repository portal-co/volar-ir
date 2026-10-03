// @reliability: experimental
// @ai: assisted
//! Pass: promote a small, bounded Boolar storage space into an explicit
//! MUX/demux bit register file, eliminating `BIrStmt::StorageRead` /
//! `StorageWrite` for one `(StorageId, LaneId)` pair before circuit fusion
//! or reversible lowering ever sees them.
//!
//! # Algorithm
//!
//! Walks the single block's statement list in program order, tracking one
//! SSA bit var per cell (`cells: Vec<u32>`, seeded from any matching
//! [`BIrPreInitSegment`], else `Zero`). Each `StorageRead { addr, .. }`
//! targeting this pass's `(storage, lane)` is replaced by an N-way MUX
//! selecting `cells[i]` where `addr == i`, using the same `MUX(cond, a, b) =
//! AND(cond, XOR(a, b)) XOR b` primitive already used elsewhere for
//! oblivious storage access (see `volar-weaver`'s VOLE-ORAM read/write).
//! Each `StorageWrite { addr, src, .. }` is replaced by an N-way demux
//! updating every cell: `cells[i]' = MUX(addr == i, src, cells[i])`. All
//! other statements are copied through unchanged, with operand vars remapped
//! through the running old-var → new-var substitution (statements are
//! replaced 1:1 by var id even when they expand into many new bit gates).
//!
//! # Preconditions
//!
//! Requires already-fused, single-block, `Jmp(Return)`-terminated Boolar IR
//! (`is_circuit()`) — run `movfuscate_biir` / `lower_to_circuit` first.
//! Fails closed with [`StorageToMuxBoolarError::AddressTooNarrow`] if any
//! read/write's address vector is too short to distinguish `num_cells`
//! distinct indices; this is a real, mechanically-checkable bound, unlike
//! proving that every *runtime* address value stays in range, which this
//! pass does not attempt — `num_cells` must be supplied by the caller as (at
//! least) the storage's true declared size.

use alloc::vec;
use alloc::vec::Vec;

use volar_ir::boolar::{BIrBlock, BIrBlocks, BIrStmt, BIrTerminator, LaneId};
use volar_ir::ir::IRVarId;
use volar_ir_common::{StorageAccess, StorageId, StorageTable};

/// Which `(StorageId, LaneId)` to eliminate, and its declared cell count.
///
/// `num_cells` must cover every address the source program can ever compute
/// for this storage — this pass has no way to verify that from the SSA
/// alone, so an under-declared bound silently reads zero / drops writes for
/// out-of-range addresses rather than erroring.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageToMuxBoolarConfig {
    pub storage: StorageId,
    pub lane: LaneId,
    pub num_cells: usize,
}

/// Why a Boolar storage-to-MUX promotion failed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StorageToMuxBoolarError {
    /// The sidecar proves this storage/lane is immutable, but the input has a
    /// write to that storage namespace.
    ReadOnlyWrite { storage: StorageId },
    /// The input isn't a single `Jmp(Return)`-terminated block; run
    /// `movfuscate_biir` / `lower_to_circuit` first.
    NotSingleBlockCircuit,
    /// `num_cells` was zero.
    ZeroCells,
    /// A read/write's address vector is too short to address `num_cells`
    /// distinct cells.
    AddressTooNarrow { addr_bits: usize, num_cells: usize },
    /// A static pre-init address cannot fit this numeric register-file
    /// selection pass's `usize` cell index.
    PreInitAddressTooWide { addr_bits: usize },
}

impl core::fmt::Display for StorageToMuxBoolarError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            StorageToMuxBoolarError::ReadOnlyWrite { storage } => write!(
                f,
                "storage_to_mux_boolar: read-only storage {storage:?} has a write"
            ),
            StorageToMuxBoolarError::NotSingleBlockCircuit => write!(
                f,
                "storage_to_mux_boolar requires single-block circuit-shaped Boolar IR; run movfuscate_biir or lower_to_circuit first"
            ),
            StorageToMuxBoolarError::ZeroCells => {
                write!(f, "storage_to_mux_boolar: num_cells must be nonzero")
            }
            StorageToMuxBoolarError::AddressTooNarrow {
                addr_bits,
                num_cells,
            } => write!(
                f,
                "storage_to_mux_boolar: address is {addr_bits} bits wide, too narrow to address {num_cells} cells"
            ),
            StorageToMuxBoolarError::PreInitAddressTooWide { addr_bits } => write!(
                f,
                "storage_to_mux_boolar: pre-init address is {addr_bits} bits wide and cannot be represented by a numeric cell index"
            ),
        }
    }
}

impl core::error::Error for StorageToMuxBoolarError {}

/// Eliminate `StorageRead`/`StorageWrite` for `cfg.storage`/`cfg.lane` by
/// promoting it to an explicit MUX/demux register file. See the module docs
/// for the algorithm and its correctness contract.
pub fn storage_to_mux_boolar<P: Clone + Default>(
    blocks: &BIrBlocks<P>,
    cfg: &StorageToMuxBoolarConfig,
) -> Result<BIrBlocks<P>, StorageToMuxBoolarError> {
    storage_to_mux_boolar_with_access(blocks, cfg, None)
}

/// As [`storage_to_mux_boolar`], validating an optional immutable-storage
/// sidecar before lowering the selected storage namespace.
pub fn storage_to_mux_boolar_with_access<P: Clone + Default>(
    blocks: &BIrBlocks<P>,
    cfg: &StorageToMuxBoolarConfig,
    storage_access: Option<&StorageTable>,
) -> Result<BIrBlocks<P>, StorageToMuxBoolarError> {
    if storage_access.is_some_and(|table| table.access_of(cfg.storage) == StorageAccess::ReadOnly)
        && blocks.blocks.iter().flat_map(|block| block.stmts.iter()).any(|node| {
            matches!(node.kind, BIrStmt::StorageWrite { storage, .. } if storage == cfg.storage)
        })
    {
        return Err(StorageToMuxBoolarError::ReadOnlyWrite { storage: cfg.storage });
    }
    if !blocks.is_circuit() {
        return Err(StorageToMuxBoolarError::NotSingleBlockCircuit);
    }
    if cfg.num_cells == 0 {
        return Err(StorageToMuxBoolarError::ZeroCells);
    }

    let old_block = &blocks.blocks[0];
    let mut new_block = BIrBlock {
        params: old_block.params,
        stmts: Vec::new(),
        terminator: old_block.terminator.clone(),
    };
    let mut remap: Vec<u32> = (0..old_block.params).collect();

    let mut cells = init_cells(&mut new_block, blocks, cfg)?;

    for node in old_block.stmts.iter() {
        match &node.kind {
            BIrStmt::StorageRead {
                storage,
                lane,
                addr,
            } if *storage == cfg.storage && *lane == cfg.lane => {
                if (1usize << addr.len().min(usize::BITS as usize - 1)) < cfg.num_cells
                    || (addr.is_empty() && cfg.num_cells > 1)
                {
                    return Err(StorageToMuxBoolarError::AddressTooNarrow {
                        addr_bits: addr.len(),
                        num_cells: cfg.num_cells,
                    });
                }
                let addr_new: Vec<u32> = addr.iter().map(|v| remap[v.0 as usize]).collect();
                let result = mux_read(&mut new_block, &cells, &addr_new, node.prov.clone());
                remap.push(result);
            }
            BIrStmt::StorageWrite {
                storage,
                lane,
                src,
                addr,
            } if *storage == cfg.storage && *lane == cfg.lane => {
                if (1usize << addr.len().min(usize::BITS as usize - 1)) < cfg.num_cells
                    || (addr.is_empty() && cfg.num_cells > 1)
                {
                    return Err(StorageToMuxBoolarError::AddressTooNarrow {
                        addr_bits: addr.len(),
                        num_cells: cfg.num_cells,
                    });
                }
                let addr_new: Vec<u32> = addr.iter().map(|v| remap[v.0 as usize]).collect();
                let src_new = remap[src.0 as usize];
                cells = mux_write(
                    &mut new_block,
                    &cells,
                    &addr_new,
                    src_new,
                    node.prov.clone(),
                );
                // `StorageWrite` "produces a dummy zero bit (no useful value)".
                let dummy = push(&mut new_block, BIrStmt::Zero, node.prov.clone());
                remap.push(dummy.0);
            }
            other => {
                let remapped = other
                    .clone()
                    .map(
                        &mut (),
                        &mut |_: &mut (), v: IRVarId| -> Result<IRVarId, core::convert::Infallible> {
                            Ok(IRVarId(remap[v.0 as usize]))
                        },
                        &mut |_: &mut (), s| Ok(s),
                    )
                    .unwrap();
                let id = push(&mut new_block, remapped, node.prov.clone());
                remap.push(id.0);
            }
        }
    }

    new_block.terminator = remap_terminator(&old_block.terminator, &remap);

    let pre_init = blocks
        .pre_init
        .iter()
        .filter(|seg| !(seg.storage == cfg.storage && seg.lane == cfg.lane))
        .cloned()
        .collect();

    Ok(BIrBlocks {
        blocks: vec![new_block],
        pre_init,
    })
}

fn push<P: Clone>(block: &mut BIrBlock<P>, stmt: BIrStmt, prov: P) -> IRVarId {
    let id = IRVarId(block.params + block.stmts.len() as u32);
    block.push_stmt(stmt, prov);
    id
}

/// `1` iff the bits of `addr` equal the bits of `index` (LSB-first),
/// `AND`-folding a per-bit XNOR.
fn eq_const<P: Clone>(block: &mut BIrBlock<P>, addr: &[u32], index: usize, prov: P) -> u32 {
    let mut acc: Option<u32> = None;
    for (j, &bit_var) in addr.iter().enumerate() {
        let bit_set = (index >> j) & 1 == 1;
        let term = if bit_set {
            bit_var
        } else {
            push(block, BIrStmt::Not(IRVarId(bit_var)), prov.clone()).0
        };
        acc = Some(match acc {
            None => term,
            Some(a) => push(block, BIrStmt::And(IRVarId(a), IRVarId(term)), prov.clone()).0,
        });
    }
    acc.unwrap_or_else(|| push(block, BIrStmt::One, prov.clone()).0)
}

/// `MUX(cond, a, b) = AND(cond, XOR(a, b)) XOR b`.
fn mux<P: Clone>(block: &mut BIrBlock<P>, cond: u32, a: u32, b: u32, prov: P) -> u32 {
    let xor_ab = push(block, BIrStmt::Xor(IRVarId(a), IRVarId(b)), prov.clone()).0;
    let and_c = push(
        block,
        BIrStmt::And(IRVarId(cond), IRVarId(xor_ab)),
        prov.clone(),
    )
    .0;
    push(block, BIrStmt::Xor(IRVarId(and_c), IRVarId(b)), prov).0
}

/// Oblivious read: fold a MUX-tree selection over every cell.
fn mux_read<P: Clone>(block: &mut BIrBlock<P>, cells: &[u32], addr: &[u32], prov: P) -> u32 {
    let mut acc = push(block, BIrStmt::Zero, prov.clone()).0;
    for (i, &cell) in cells.iter().enumerate() {
        let eq_i = eq_const(block, addr, i, prov.clone());
        acc = mux(block, eq_i, cell, acc, prov.clone());
    }
    acc
}

/// Oblivious write: demux `src` into every cell via a per-cell MUX.
fn mux_write<P: Clone>(
    block: &mut BIrBlock<P>,
    cells: &[u32],
    addr: &[u32],
    src: u32,
    prov: P,
) -> Vec<u32> {
    cells
        .iter()
        .enumerate()
        .map(|(i, &cell)| {
            let eq_i = eq_const(block, addr, i, prov.clone());
            mux(block, eq_i, src, cell, prov.clone())
        })
        .collect()
}

fn init_cells<P: Clone + Default>(
    block: &mut BIrBlock<P>,
    blocks: &BIrBlocks<P>,
    cfg: &StorageToMuxBoolarConfig,
) -> Result<Vec<u32>, StorageToMuxBoolarError> {
    let mut values = vec![false; cfg.num_cells];
    let mut set = vec![false; cfg.num_cells];
    for seg in &blocks.pre_init {
        if seg.storage != cfg.storage || seg.lane != cfg.lane {
            continue;
        }
        for (k, &bit) in seg.data.iter().enumerate() {
            let addr = add_to_address(&seg.addr, k);
            let idx =
                address_to_usize(&addr).ok_or(StorageToMuxBoolarError::PreInitAddressTooWide {
                    addr_bits: addr.len(),
                })?;
            if idx < cfg.num_cells {
                values[idx] = bit;
                set[idx] = true;
            }
        }
    }
    Ok(values
        .iter()
        .map(|&bit| {
            let stmt = if bit { BIrStmt::One } else { BIrStmt::Zero };
            push(block, stmt, P::default()).0
        })
        .collect())
}

fn address_to_usize(addr: &[bool]) -> Option<usize> {
    // This pass encodes the selected storage cell as a host `usize`.  An
    // exact Boolar address wider than that is not representable here even
    // when its high bits happen to be zero: accepting it would silently
    // collapse distinct exact storage keys.
    if addr.len() > usize::BITS as usize {
        return None;
    }
    Some(
        addr.iter()
            .take(usize::BITS as usize)
            .enumerate()
            .fold(0usize, |value, (bit, set)| value | ((*set as usize) << bit)),
    )
}

fn add_to_address(addr: &[bool], mut addend: usize) -> Vec<bool> {
    let mut out = addr.to_vec();
    let mut bit = 0usize;
    while addend != 0 {
        if bit == out.len() {
            out.push(false);
        }
        if addend & 1 != 0 {
            let mut carry = true;
            let mut at = bit;
            while carry {
                if at == out.len() {
                    out.push(false);
                }
                let next = out[at] ^ carry;
                carry &= out[at];
                out[at] = next;
                at += 1;
            }
        }
        addend >>= 1;
        bit += 1;
    }
    out
}

fn remap_terminator(term: &BIrTerminator, remap: &[u32]) -> BIrTerminator {
    let remap_target = |t: &volar_ir::boolar::BIrTarget| volar_ir::boolar::BIrTarget {
        block: t.block.clone(),
        args: t
            .args
            .iter()
            .map(|v| IRVarId(remap[v.0 as usize]))
            .collect(),
    };
    match term {
        BIrTerminator::Jmp(target) => BIrTerminator::Jmp(remap_target(target)),
        BIrTerminator::CondJmp {
            val,
            then_target,
            else_target,
        } => BIrTerminator::CondJmp {
            val: IRVarId(remap[val.0 as usize]),
            then_target: remap_target(then_target),
            else_target: remap_target(else_target),
        },
        _ => panic!("storage_to_mux_boolar: unsupported BIrTerminator variant"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;
    use volar_ir::boolar::{BIrPreInitSegment, BIrTarget};
    use volar_ir::ir::IRBlockTargetId;
    use volar_ir_common::Node;

    /// Minimal interpreter for the boolean-primitive + storage subset this
    /// pass's own test fixtures use.
    fn eval(
        stmts: &[Node<BIrStmt, ()>],
        params: &[bool],
        storage: &mut BTreeMap<(u32, u64), bool>,
    ) -> Vec<bool> {
        let mut vals = params.to_vec();
        for node in stmts {
            let v = match &node.kind {
                BIrStmt::Zero => false,
                BIrStmt::One => true,
                BIrStmt::And(a, b) => vals[a.0 as usize] && vals[b.0 as usize],
                BIrStmt::Or(a, b) => vals[a.0 as usize] || vals[b.0 as usize],
                BIrStmt::Xor(a, b) => vals[a.0 as usize] ^ vals[b.0 as usize],
                BIrStmt::Not(a) => !vals[a.0 as usize],
                BIrStmt::StorageRead { lane, addr, .. } => {
                    let key = addr_key(lane.0, addr, &vals);
                    *storage.get(&key).unwrap_or(&false)
                }
                BIrStmt::StorageWrite {
                    lane, src, addr, ..
                } => {
                    let key = addr_key(lane.0, addr, &vals);
                    storage.insert(key, vals[src.0 as usize]);
                    false
                }
                other => panic!("test interpreter: unsupported stmt {other:?}"),
            };
            vals.push(v);
        }
        vals
    }

    fn addr_key(lane: u32, addr: &[IRVarId], vals: &[bool]) -> (u32, u64) {
        let mut a = 0u64;
        for (j, v) in addr.iter().enumerate() {
            if vals[v.0 as usize] {
                a |= 1 << j;
            }
        }
        (lane, a)
    }

    fn return_values(term: &BIrTerminator, vals: &[bool]) -> Vec<bool> {
        match term {
            BIrTerminator::Jmp(BIrTarget {
                block: IRBlockTargetId::Return,
                args,
            }) => args.iter().map(|v| vals[v.0 as usize]).collect(),
            other => panic!("expected Jmp(Return), got {other:?}"),
        }
    }

    /// Build: write(addr=1, val=1), then return [read(addr=0), read(addr=1)].
    /// Expect [false, true] — write only touches cell 1.
    fn build_fixture() -> BIrBlocks<()> {
        let storage = StorageId(0);
        let lane = LaneId(0);
        let mut block: BIrBlock<()> = BIrBlock {
            params: 0,
            stmts: alloc::vec![],
            terminator: BIrTerminator::Jmp(BIrTarget {
                block: IRBlockTargetId::Return,
                args: alloc::vec![],
            }),
        };
        let addr1 = push(&mut block, BIrStmt::One, ());
        let val1 = push(&mut block, BIrStmt::One, ());
        push(
            &mut block,
            BIrStmt::StorageWrite {
                storage,
                lane,
                src: val1,
                addr: alloc::vec![addr1],
            },
            (),
        );
        let addr0 = push(&mut block, BIrStmt::Zero, ());
        let read0 = push(
            &mut block,
            BIrStmt::StorageRead {
                storage,
                lane,
                addr: alloc::vec![addr0],
            },
            (),
        );
        let read1 = push(
            &mut block,
            BIrStmt::StorageRead {
                storage,
                lane,
                addr: alloc::vec![addr1],
            },
            (),
        );
        block.terminator = BIrTerminator::Jmp(BIrTarget {
            block: IRBlockTargetId::Return,
            args: alloc::vec![read0, read1],
        });
        BIrBlocks {
            blocks: alloc::vec![block],
            pre_init: alloc::vec![],
        }
    }

    #[test]
    fn eliminates_storage_and_preserves_semantics() {
        let blocks = build_fixture();
        let mut storage = BTreeMap::new();
        let before = return_values(
            &blocks.blocks[0].terminator,
            &eval(&blocks.blocks[0].stmts, &[], &mut storage),
        );
        assert_eq!(before, vec![false, true]);

        let cfg = StorageToMuxBoolarConfig {
            storage: StorageId(0),
            lane: LaneId(0),
            num_cells: 2,
        };
        let rewritten = storage_to_mux_boolar(&blocks, &cfg).expect("pass should succeed");
        assert!(rewritten.is_circuit());
        for node in &rewritten.blocks[0].stmts {
            assert!(
                !matches!(
                    node.kind,
                    BIrStmt::StorageRead { .. } | BIrStmt::StorageWrite { .. }
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
        let blocks = build_fixture();
        let mut sidecar = StorageTable::new();
        sidecar.set(StorageId(0), StorageAccess::ReadOnly);
        assert_eq!(
            storage_to_mux_boolar_with_access(
                &blocks,
                &StorageToMuxBoolarConfig {
                    storage: StorageId(0),
                    lane: LaneId(0),
                    num_cells: 2,
                },
                Some(&sidecar),
            ),
            Err(StorageToMuxBoolarError::ReadOnlyWrite {
                storage: StorageId(0)
            })
        );
    }

    #[test]
    fn rejects_zero_cells() {
        let blocks = build_fixture();
        let cfg = StorageToMuxBoolarConfig {
            storage: StorageId(0),
            lane: LaneId(0),
            num_cells: 0,
        };
        assert_eq!(
            storage_to_mux_boolar(&blocks, &cfg).unwrap_err(),
            StorageToMuxBoolarError::ZeroCells
        );
    }

    #[test]
    fn rejects_wide_exact_pre_init_address() {
        let mut blocks = build_fixture();
        let addr_bits = usize::BITS as usize + 1;
        blocks.pre_init = alloc::vec![BIrPreInitSegment {
            storage: StorageId(0),
            lane: LaneId(0),
            addr: alloc::vec![false; addr_bits],
            data: alloc::vec![true],
        }];
        let cfg = StorageToMuxBoolarConfig {
            storage: StorageId(0),
            lane: LaneId(0),
            num_cells: 2,
        };
        assert_eq!(
            storage_to_mux_boolar(&blocks, &cfg).unwrap_err(),
            StorageToMuxBoolarError::PreInitAddressTooWide { addr_bits }
        );
    }
}
