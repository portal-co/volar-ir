// @reliability: normal
//! @ai: assisted
//! Transform: ordinary (movfuscated / circuit-shaped) Volar IR and Boolar IR →
//! their circuit-fused variants ([`VCircuit`] / [`BCircuit`]).
//!
//! Fusion **validates**; it does not produce the single-block shape itself.
//! Inputs that are not already movfuscated with a `Jmp { Return }` terminator
//! are rejected with a precise error directing callers to run movfuscation or
//! loop lowering first. All invariant checking is delegated to the fused
//! types' constructors in `volar-ir` (`VCircuit::try_from_ir` /
//! `BCircuit::try_from_ir`) — the single implementations of the circuit
//! invariant.

use volar_ir::boolar::BIrBlocks;
use volar_ir::circuit::{BCircuit, CircuitFusionError, VCircuit};
use volar_ir::ir::IRBlocks;

/// Fuse a movfuscated Volar program into a [`VCircuit`].
///
/// Errors if `blocks` is not a single block terminating in `Jmp { Return }`
/// (run `movfuscate_ir` first), or if it carries module-level declarations
/// (see [`CircuitFusionError::ModuleLevelStateUnsupported`]).
pub fn to_circuit_fused_volar<P: Clone>(
    blocks: &IRBlocks<P>,
) -> Result<VCircuit<P>, CircuitFusionError> {
    VCircuit::try_from_ir(blocks)
}

/// Fuse a movfuscated Boolar program into a [`BCircuit`].
///
/// Errors if `blocks` is not a single block terminating in `Jmp(Return)`
/// (run `movfuscate_biir` / `lower_to_circuit` first). Bit-granular static
/// storage is preserved in the resulting circuit.
pub fn to_circuit_fused_boolar<P: Clone>(
    blocks: &BIrBlocks<P>,
) -> Result<BCircuit<P>, CircuitFusionError> {
    BCircuit::try_from_ir(blocks)
}

/// Convenience: lower a Boolar program to a circuit and fuse it in one step.
///
/// Dual API for [`crate::lower_to_circuit`]: same unrolling budget and MUX
/// gating policy, but returns the strongly-typed fused form so downstream
/// consumers get the single-block guarantee from the canonical producer.
pub fn lower_to_circuit_fused<P: Clone>(
    blocks: &BIrBlocks<P>,
    limit: u32,
    mode: crate::LoweringMode,
) -> Result<BCircuit<P>, CircuitFusionError>
where
    P: Default,
{
    // Use the control-provenance variant (with a default annotation) so that
    // statement-free movfuscated blocks — legal circuits — don't panic.
    to_circuit_fused_boolar(&crate::lower_to_circuit_with_control_provenance(
        blocks,
        limit,
        mode,
        &P::default(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;
    use volar_ir::boolar::{BIrBlock, BIrPreInitSegment, BIrTarget, BIrTerminator};
    use volar_ir::ir::{
        IRBlock, IRBlockId, IRBlockTargetId, IRBranchTarget, IRTerminator, IRTypeId, IRVarId,
    };
    use volar_ir_common::{Constant, Stmt, StorageId};

    fn empty_pre_init() -> Vec<BIrPreInitSegment> {
        Vec::new()
    }

    fn bir_block_return(params: u32, outputs: Vec<IRVarId>) -> BIrBlock {
        BIrBlock {
            params,
            stmts: vec![],
            terminator: BIrTerminator::Jmp(BIrTarget {
                block: IRBlockTargetId::Return,
                args: outputs,
            }),
        }
    }

    #[test]
    fn fuses_single_block_boolar() {
        let block = bir_block_return(2, vec![IRVarId(1)]);
        let blocks = BIrBlocks {
            blocks: vec![block],
            pre_init: empty_pre_init(),
        };
        let fused = to_circuit_fused_boolar(&blocks).expect("should fuse");
        assert_eq!(fused.params, 2);
        assert_eq!(fused.outputs, vec![IRVarId(1)]);
        // Round-trip back to general form.
        let general = fused.to_bir_blocks();
        assert!(general.is_circuit());
        assert_eq!(
            BCircuit::try_from_ir(&general).expect("re-fuse"),
            BCircuit {
                params: 2,
                stmts: vec![],
                pre_init: vec![],
                outputs: vec![IRVarId(1)]
            }
        );
    }

    #[test]
    fn fusing_boolar_retains_static_storage() {
        let pre_init = vec![BIrPreInitSegment {
            storage: StorageId(9),
            lane: volar_ir::boolar::LaneId(2),
            offset: 3,
            data: vec![true, false, true],
        }];
        let blocks = BIrBlocks {
            blocks: vec![bir_block_return(0, vec![])],
            pre_init: pre_init.clone(),
        };
        let fused = to_circuit_fused_boolar(&blocks).expect("data-bearing circuit should fuse");
        assert_eq!(fused.pre_init, pre_init);
        assert_eq!(fused.to_bir_blocks().pre_init, pre_init);
    }

    #[test]
    fn rejects_multi_block_boolar() {
        let b0 = bir_block_return(1, vec![IRVarId(0)]);
        let mut b1 = bir_block_return(1, vec![IRVarId(0)]);
        b1.terminator = BIrTerminator::Jmp(BIrTarget {
            block: IRBlockTargetId::Block(IRBlockId(0)),
            args: vec![],
        });
        let blocks = BIrBlocks {
            blocks: vec![b0, b1],
            pre_init: empty_pre_init(),
        };
        let err = to_circuit_fused_boolar(&blocks).unwrap_err();
        assert_eq!(err, CircuitFusionError::NotSingleBlock { found: 2 });
    }

    #[test]
    fn rejects_conditional_terminator() {
        let mut b0 = bir_block_return(2, vec![volar_ir::ir::IRVarId(1)]);
        b0.terminator = BIrTerminator::CondJmp {
            val: IRVarId(0),
            then_target: BIrTarget {
                block: IRBlockTargetId::Return,
                args: vec![],
            },
            else_target: BIrTarget {
                block: IRBlockTargetId::Return,
                args: vec![],
            },
        };
        let blocks = BIrBlocks {
            blocks: vec![b0],
            pre_init: empty_pre_init(),
        };
        assert_eq!(
            to_circuit_fused_boolar(&blocks).unwrap_err(),
            CircuitFusionError::NotReturnTerminator
        );
    }

    #[test]
    fn rejects_out_of_range_output() {
        let b0 = bir_block_return(1, vec![IRVarId(7)]);
        let blocks = BIrBlocks {
            blocks: vec![b0],
            pre_init: empty_pre_init(),
        };
        assert_eq!(
            to_circuit_fused_boolar(&blocks).unwrap_err(),
            CircuitFusionError::OutputVarOutOfRange {
                var: 7,
                var_space: 1
            }
        );
    }

    #[test]
    fn fuses_movfuscated_volar_and_rejects_cfg() {
        use volar_ir_common::Node;
        // Single movfuscated-style block: Const + Jmp{Return}.
        let block = IRBlock {
            params: vec![IRTypeId(0)],
            stmts: vec![Node::new(
                Stmt::Const(Constant { hi: 0, lo: 1 }, IRTypeId(0)),
                (),
                None,
            )],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget {
                    dest: IRBlockTargetId::Return,
                    args: vec![IRVarId(1)],
                    reentry: None,
                },
            },
        };
        let blocks = IRBlocks::new(vec![block]);
        let fused = to_circuit_fused_volar(&blocks).expect("should fuse");
        assert_eq!(fused.outputs.len(), 1);
        let round_trip = fused.to_ir_blocks();
        assert!(round_trip.is_circuit());

        // Two blocks -> rejected.
        let blocks2: IRBlocks<()> = IRBlocks::new(vec![
            IRBlock {
                params: vec![],
                stmts: vec![],
                terminator: IRTerminator::Jmp {
                    target: IRBranchTarget {
                        dest: IRBlockTargetId::Block(IRBlockId(1)),
                        args: vec![],
                        reentry: None,
                    },
                },
            },
            IRBlock {
                params: vec![],
                stmts: vec![],
                terminator: IRTerminator::Jmp {
                    target: IRBranchTarget {
                        dest: IRBlockTargetId::Return,
                        args: vec![],
                        reentry: None,
                    },
                },
            },
        ]);
        assert!(to_circuit_fused_volar(&blocks2).is_err());
    }
}
