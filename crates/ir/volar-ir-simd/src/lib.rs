#![no_std]
// @pinnedness: unpinned
// @stability: very-unstable
// @ai: assisted

//! Width-agnostic lane scheduling (native-word SIMD) for Boolar circuits.
//!
//! [`plan_lanes`] schedules a storage-free Boolean circuit ([`BCircuit`])
//! as lane-wise word operations over abstract slots: every bit of a target
//! word is an independent Boolean lane (lane zero is the least-significant
//! bit), so a 256-bit EVM word evaluates 256 instances at once and a
//! 6502/Z80 byte evaluates 8. Lane width is a renderer and ABI property,
//! never planner data, so one schedule drives every packed-word backend.
//!
//! Renderers live with their targets: the 6502/Z80 byte renderers in
//! retrop's `retrop-emit-native`, and the EVM emitters. Slot assignment is
//! deterministic: packed inputs occupy slots `0..num_params`, then each live
//! gate statement allocates its destination slot in circuit order; dead
//! pure-gate statements (no later use, not an output) are dropped.

extern crate alloc;

use alloc::vec::Vec;

use volar_ir::boolar::{BIrBlocks, BIrStmt, BIrTarget, BIrTerminator};
use volar_ir::circuit::BCircuit;
use volar_ir::ir::{IRBlockTargetId, IRVarId};

/// One lane-wise word operation over abstract slots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaneOp {
    /// All lanes false (`Zero`) or all lanes true (`One`).
    Const(bool),
    /// Lane-wise AND of two slots.
    And(u32, u32),
    /// Lane-wise OR of two slots.
    Or(u32, u32),
    /// Lane-wise XOR of two slots.
    Xor(u32, u32),
    /// Lane-wise inversion of one slot.
    Not(u32),
}

/// A scheduled lane statement: `dst = op`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaneStmt {
    /// Destination word slot.
    pub dst: u32,
    /// The lane-wise operation producing it.
    pub op: LaneOp,
}

/// A width-agnostic lane schedule for a storage-free Boolean circuit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanePlan {
    /// Number of packed input words. Inputs occupy slots `0..num_params`.
    pub num_params: u32,
    /// Live gate statements in circuit order.
    pub stmts: Vec<LaneStmt>,
    /// One slot per circuit output, in circuit-defined order.
    pub outputs: Vec<u32>,
    /// Total word slots allocated.
    pub num_slots: u32,
}

impl LanePlan {
    /// The packed input word slots.
    pub fn input_slots(&self) -> core::ops::Range<u32> {
        0..self.num_params
    }
}

/// Ingest a single-block `BIrBlocks` value (the `is_circuit()` shape:
/// params -> gates -> `Jmp(Return)`) into a [`BCircuit`]. Returns `None`
/// for any other shape.
pub fn ingest_circuit<P: Clone>(blocks: BIrBlocks<P>) -> Option<BCircuit<P>> {
    if !blocks.is_circuit() {
        return None;
    }
    let BIrBlocks {
        mut blocks,
        pre_init,
    } = blocks;
    let block = blocks.pop()?;
    let outputs = match block.terminator {
        BIrTerminator::Jmp(BIrTarget {
            block: IRBlockTargetId::Return,
            args,
        }) => args,
        _ => return None,
    };
    Some(BCircuit {
        params: block.params,
        stmts: block.stmts,
        pre_init,
        outputs,
    })
}

/// Schedule `circuit` into lane operations.
///
/// Returns `None` when the circuit is not lane-compatible — any statement
/// outside the pure Boolean subset (`Zero`/`One`/`And`/`Or`/`Xor`/`Not`),
/// e.g. storage, oracle, action, or RNG statements. Callers decide the
/// fallback (scalar lowering, rejection, ...); the planner does not pick one.
pub fn plan_lanes<P: Clone>(circuit: &BCircuit<P>) -> Option<LanePlan> {
    let compatible = circuit.stmts.iter().all(|node| {
        matches!(
            &node.kind,
            BIrStmt::Zero
                | BIrStmt::One
                | BIrStmt::And(..)
                | BIrStmt::Or(..)
                | BIrStmt::Xor(..)
                | BIrStmt::Not(..)
        )
    });
    if !compatible {
        return None;
    }

    // Circuit vars are dense: params are `IRVarId(0)..IRVarId(num_params)`
    // and statement `i` defines `IRVarId(num_params + i)`.
    let num_params = circuit.params as usize;
    let var_for_stmt = |index: usize| num_params + index;
    let mut last_use: Vec<usize> = (0..num_params)
        .map(|_| 0)
        .chain(0..circuit.stmts.len())
        .collect();
    let mut record_use = |var: IRVarId, use_idx: usize| {
        let entry = &mut last_use[var.0 as usize];
        *entry = (*entry).max(use_idx);
    };
    for (index, node) in circuit.stmts.iter().enumerate() {
        match &node.kind {
            BIrStmt::Not(a) => record_use(*a, index),
            BIrStmt::And(a, b) | BIrStmt::Or(a, b) | BIrStmt::Xor(a, b) => {
                record_use(*a, index);
                record_use(*b, index);
            }
            _ => {}
        }
    }
    for &var in &circuit.outputs {
        last_use[var.0 as usize] = usize::MAX;
    }

    let mut slot_of = alloc::vec![u32::MAX; num_params + circuit.stmts.len()];
    let mut next = 0u32;
    let mut slot_for = |var: IRVarId| -> u32 {
        let entry = &mut slot_of[var.0 as usize];
        if *entry == u32::MAX {
            *entry = next;
            next += 1;
        }
        *entry
    };
    // Inputs first, in parameter order.
    for param in 0..circuit.params {
        slot_for(IRVarId(param));
    }

    let mut stmts = Vec::new();
    for (index, node) in circuit.stmts.iter().enumerate() {
        // Dead pure-gate statement: no later use and not an output.
        if last_use[var_for_stmt(index)] == index {
            continue;
        }
        let dst = slot_for(IRVarId(var_for_stmt(index) as u32));
        let op = match &node.kind {
            BIrStmt::Zero => LaneOp::Const(false),
            BIrStmt::One => LaneOp::Const(true),
            BIrStmt::And(a, b) => LaneOp::And(slot_for(*a), slot_for(*b)),
            BIrStmt::Or(a, b) => LaneOp::Or(slot_for(*a), slot_for(*b)),
            BIrStmt::Xor(a, b) => LaneOp::Xor(slot_for(*a), slot_for(*b)),
            BIrStmt::Not(a) => LaneOp::Not(slot_for(*a)),
            _ => unreachable!("lane compatibility was checked above"),
        };
        stmts.push(LaneStmt { dst, op });
    }

    let outputs = circuit.outputs.iter().map(|&var| slot_for(var)).collect();
    Some(LanePlan {
        num_params: circuit.params,
        stmts,
        outputs,
        num_slots: next,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    use volar_ir::boolar::LaneId;
    use volar_ir::boolar::{BIrBlock, BIrPreInitSegment};
    use volar_ir::ir::StorageId;
    use volar_ir_common::Node;

    fn block(params: u32, stmts: Vec<BIrStmt>, outputs: Vec<IRVarId>) -> BIrBlocks<()> {
        BIrBlocks {
            blocks: alloc::vec![BIrBlock {
                params,
                stmts: stmts.into_iter().map(|s| Node::new(s, (), None)).collect(),
                terminator: BIrTerminator::Jmp(BIrTarget {
                    block: IRBlockTargetId::Return,
                    args: outputs,
                }),
            }],
            pre_init: alloc::vec![] as Vec<BIrPreInitSegment>,
        }
    }

    /// `sum = a ^ b ^ cin`, `cout = (a & b) | (cin & (a ^ b))`.
    fn full_adder() -> BIrBlocks<()> {
        let (a, b, cin) = (IRVarId(0), IRVarId(1), IRVarId(2));
        let ab_xor = IRVarId(3);
        let sum = IRVarId(4);
        let ab_and = IRVarId(5);
        let cin_and = IRVarId(6);
        let cout = IRVarId(7);
        block(
            3,
            alloc::vec![
                BIrStmt::Xor(a, b),
                BIrStmt::Xor(ab_xor, cin),
                BIrStmt::And(a, b),
                BIrStmt::And(cin, ab_xor),
                BIrStmt::Or(ab_and, cin_and),
            ],
            alloc::vec![sum, cout],
        )
    }

    /// Execute a plan over 64 lanes packed into `u64` words.
    fn eval_plan(plan: &LanePlan, inputs: &[u64]) -> Vec<u64> {
        let mut slots = alloc::vec![0u64; plan.num_slots as usize];
        for (index, &value) in inputs.iter().enumerate() {
            slots[index] = value;
        }
        for stmt in &plan.stmts {
            slots[stmt.dst as usize] = match stmt.op {
                LaneOp::Const(value) => {
                    if value {
                        u64::MAX
                    } else {
                        0
                    }
                }
                LaneOp::And(a, b) => slots[a as usize] & slots[b as usize],
                LaneOp::Or(a, b) => slots[a as usize] | slots[b as usize],
                LaneOp::Xor(a, b) => slots[a as usize] ^ slots[b as usize],
                LaneOp::Not(a) => !slots[a as usize],
            };
        }
        plan.outputs
            .iter()
            .map(|&slot| slots[slot as usize])
            .collect()
    }

    #[test]
    fn full_adder_plan_layout() {
        let circuit = ingest_circuit(full_adder()).expect("circuit shape");
        let plan = plan_lanes(&circuit).expect("pure-Boolean circuit plans");
        assert_eq!(plan.num_params, 3);
        assert_eq!(plan.input_slots().collect::<Vec<_>>(), alloc::vec![0, 1, 2]);
        let kinds: Vec<(u32, LaneOp)> = plan.stmts.iter().map(|s| (s.dst, s.op)).collect();
        assert_eq!(
            kinds,
            alloc::vec![
                (3, LaneOp::Xor(0, 1)),
                (4, LaneOp::Xor(3, 2)),
                (5, LaneOp::And(0, 1)),
                (6, LaneOp::And(2, 3)),
                (7, LaneOp::Or(5, 6)),
            ]
        );
        assert_eq!(plan.outputs, alloc::vec![4, 7]);
        assert_eq!(plan.num_slots, 8);
    }

    #[test]
    fn dead_pure_gates_are_dropped() {
        let (a, b) = (IRVarId(0), IRVarId(1));
        let used = IRVarId(2);
        let blocks = block(
            2,
            alloc::vec![
                BIrStmt::And(a, b),       // var 2: the output
                BIrStmt::Or(a, b),        // var 3: dead — never used
                BIrStmt::Xor(used, used), // var 4: live but after a dead one
            ],
            alloc::vec![IRVarId(4)],
        );
        let circuit = ingest_circuit(blocks).expect("circuit shape");
        let plan = plan_lanes(&circuit).expect("plans");
        let kinds: Vec<(u32, LaneOp)> = plan.stmts.iter().map(|s| (s.dst, s.op)).collect();
        // Slots compact over live statements only.
        assert_eq!(
            kinds,
            alloc::vec![(2, LaneOp::And(0, 1)), (3, LaneOp::Xor(2, 2))]
        );
        assert_eq!(plan.outputs, alloc::vec![3]);
        assert_eq!(plan.num_slots, 4);
    }

    #[test]
    fn storage_circuits_do_not_plan() {
        let blocks = block(
            1,
            alloc::vec![BIrStmt::StorageRead {
                storage: StorageId(0),
                lane: LaneId(0),
                addr: alloc::vec![IRVarId(0)],
            }],
            alloc::vec![IRVarId(1)],
        );
        let circuit = ingest_circuit(blocks).expect("circuit shape");
        assert!(plan_lanes(&circuit).is_none());
    }

    #[test]
    fn packed_evaluation_matches_biir() {
        let blocks = full_adder();
        let circuit = ingest_circuit(blocks.clone()).expect("circuit shape");
        let plan = plan_lanes(&circuit).expect("plans");
        // Exhaustive: all 2^3 input combinations in lanes 0..8.
        let mut inputs = [0u64; 3];
        for combo in 0..8usize {
            for param in 0..3 {
                if (combo >> param) & 1 != 0 {
                    inputs[param] |= 1 << combo;
                }
            }
        }
        let outputs = eval_plan(&plan, &inputs);
        for lane in 0..8 {
            let params: alloc::vec::Vec<bool> =
                (0..3).map(|param| (lane >> param) & 1 != 0).collect();
            let expected = volar_fuzz::interpreter::biir::eval_biir(&blocks, &params)
                .expect("circuit terminates");
            for (packed, expected) in outputs.iter().zip(&expected) {
                assert_eq!((packed >> lane) & 1 != 0, *expected, "lane {lane}");
            }
        }
    }

    #[test]
    fn multi_block_blocks_do_not_ingest() {
        let mut blocks = full_adder();
        blocks.blocks.push(BIrBlock {
            params: 0,
            stmts: alloc::vec![],
            terminator: BIrTerminator::Jmp(BIrTarget {
                block: IRBlockTargetId::Return,
                args: alloc::vec![],
            }),
        });
        assert!(ingest_circuit(blocks).is_none());
    }
}
