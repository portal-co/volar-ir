//! CFG trace layout and unreachable-block pruning before movfuscation.
//!
//! Movfuscation emits one PC-equality predicate per retained block. A block
//! unreachable from entry can never become active, so retaining it creates a
//! provably-dead equality test and a gated body. This pass removes such blocks
//! and puts the remaining CFG in deterministic depth-first trace order: the
//! first successor is adjacent to its predecessor, which improves locality for
//! concrete-prefix unrolling, region splitting, and post-movfuscation CSE.
//! It does **not** claim machine-code fall-through changes movfuscation's
//! semantics; control is still selected by PC predicates.

use alloc::{collections::BTreeSet, vec, vec::Vec};

use volar_ir::ir::{IRBlockId, IRBlockTargetId, IRBlocks, IRBranchTarget, IRTerminator};

/// Measurements returned by [`layout_cfg_for_movfuscation`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CfgLayoutReport {
    /// Blocks before pruning.
    pub input_blocks: usize,
    /// Blocks retained in the laid-out CFG.
    pub output_blocks: usize,
    /// Number of unreachable PC equality predicates eliminated.
    pub pruned_blocks: usize,
}

/// Prune statically unreachable blocks and place retained blocks in trace
/// order. Direct targets are followed in source successor order; a dynamic
/// target conservatively makes every block reachable.
///
/// Block ids in every direct target are remapped atomically with the layout.
/// Arguments and `reentry` metadata are preserved unchanged.
pub fn layout_cfg_for_movfuscation<P: Clone>(blocks: &mut IRBlocks<P>) -> CfgLayoutReport {
    let input_blocks = blocks.blocks.len();
    if input_blocks <= 1 {
        return CfgLayoutReport {
            input_blocks,
            output_blocks: input_blocks,
            pruned_blocks: 0,
        };
    }

    let mut seen = BTreeSet::new();
    let mut order = Vec::new();
    visit(0, blocks, &mut seen, &mut order);
    let mut old_to_new = vec![None; input_blocks];
    for (new, &old) in order.iter().enumerate() {
        old_to_new[old] = Some(new as u32);
    }

    let mut laid_out = Vec::with_capacity(order.len());
    for old in order {
        let mut block = blocks.blocks[old].clone();
        block.terminator = remap_terminator(block.terminator, &old_to_new);
        laid_out.push(block);
    }
    blocks.blocks = laid_out;
    CfgLayoutReport {
        input_blocks,
        output_blocks: blocks.blocks.len(),
        pruned_blocks: input_blocks - blocks.blocks.len(),
    }
}

fn visit<P: Clone>(
    block: usize,
    blocks: &IRBlocks<P>,
    seen: &mut BTreeSet<usize>,
    order: &mut Vec<usize>,
) {
    if block >= blocks.blocks.len() || !seen.insert(block) {
        return;
    }
    order.push(block);
    let mut dynamic = false;
    for target in successors(&blocks.blocks[block].terminator) {
        match target.dest {
            IRBlockTargetId::Block(id) => visit(id.0 as usize, blocks, seen, order),
            IRBlockTargetId::Dyn(_) => dynamic = true,
            IRBlockTargetId::Return => {}
            _ => dynamic = true,
        }
    }
    if dynamic {
        for id in 0..blocks.blocks.len() {
            visit(id, blocks, seen, order);
        }
    }
}

fn successors(term: &IRTerminator) -> Vec<&IRBranchTarget> {
    match term {
        IRTerminator::Jmp { target } => vec![target],
        IRTerminator::JumpCond {
            then_target,
            else_target,
            ..
        } => vec![then_target, else_target],
        IRTerminator::JumpTable { cases, .. } => cases.values().collect(),
        _ => Vec::new(),
    }
}

fn remap_target(mut target: IRBranchTarget, map: &[Option<u32>]) -> IRBranchTarget {
    if let IRBlockTargetId::Block(old) = target.dest {
        target.dest = IRBlockTargetId::Block(IRBlockId(
            map.get(old.0 as usize)
                .and_then(|id| *id)
                .expect("reachable target retained"),
        ));
    }
    target
}

fn remap_terminator(term: IRTerminator, map: &[Option<u32>]) -> IRTerminator {
    match term {
        IRTerminator::Jmp { target } => IRTerminator::Jmp {
            target: remap_target(target, map),
        },
        IRTerminator::JumpCond {
            condition,
            then_target,
            else_target,
        } => IRTerminator::JumpCond {
            condition,
            then_target: remap_target(then_target, map),
            else_target: remap_target(else_target, map),
        },
        IRTerminator::JumpTable { index, cases } => IRTerminator::JumpTable {
            index,
            cases: cases
                .into_iter()
                .map(|(key, target)| (key, remap_target(target, map)))
                .collect(),
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use volar_ir::ir::{IRBlock, IRBranchTarget};

    fn jump(dest: u32) -> IRTerminator {
        IRTerminator::Jmp {
            target: IRBranchTarget::new(IRBlockTargetId::Block(IRBlockId(dest)), vec![]),
        }
    }

    #[test]
    fn prunes_dead_pc_equality_and_remaps_targets() {
        let mut blocks: IRBlocks<()> = IRBlocks::new(vec![
            IRBlock {
                params: vec![],
                stmts: vec![],
                terminator: jump(2),
            },
            IRBlock {
                params: vec![],
                stmts: vec![],
                terminator: IRTerminator::Jmp {
                    target: IRBranchTarget::new(IRBlockTargetId::Return, vec![]),
                },
            },
            IRBlock {
                params: vec![],
                stmts: vec![],
                terminator: jump(3),
            },
            IRBlock {
                params: vec![],
                stmts: vec![],
                terminator: IRTerminator::Jmp {
                    target: IRBranchTarget::new(IRBlockTargetId::Return, vec![]),
                },
            },
        ]);
        let report = layout_cfg_for_movfuscation(&mut blocks);
        assert_eq!(
            report,
            CfgLayoutReport {
                input_blocks: 4,
                output_blocks: 3,
                pruned_blocks: 1
            }
        );
        match &blocks.blocks[0].terminator {
            IRTerminator::Jmp { target } => {
                assert_eq!(target.dest, IRBlockTargetId::Block(IRBlockId(1)))
            }
            other => panic!("unexpected terminator {other:?}"),
        }
    }
}
