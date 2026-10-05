//! Interoperability between storage-free circuit-shaped Boolar IR, Bristol
//! Boolean circuits, and Summon source code.
//!
//! Conversion preserves the Boolean function and ordered input/output bits;
//! it does not preserve Boolar provenance, side annotations, or Bristol party
//! names. Storage and external effects are rejected.

use volar_ir::boolar::{BIrBlock, BIrBlocks, BIrStmt, BIrTerminator};
use volar_ir::ir::{IRBlockTargetId, IRVarId};
use volar_ir_common::Node;

/// Why a Boolar program cannot be used by this interoperability layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InteropError {
    /// The program does not contain exactly one block.
    NotSingleBlock { found: usize },
    /// The single block does not return normally.
    NotReturnTerminator,
    /// This format cannot represent pre-initialized storage.
    PreInitUnsupported,
    /// A Boolar statement is not a pure Boolean gate supported by the format.
    UnsupportedStatement { index: usize },
    /// An instruction refers to a value not yet defined at that point.
    InvalidStatementInput { statement: usize, var: IRVarId },
    /// A return value does not refer to a parameter or statement result.
    InvalidOutput { output: usize, var: IRVarId },
    /// The number of parameters and statements exceeds the Boolar variable-ID
    /// range.
    VarSpaceOverflow,
}

impl core::fmt::Display for InteropError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::NotSingleBlock { found } => {
                write!(f, "expected one Boolar block, found {found}")
            }
            Self::NotReturnTerminator => write!(f, "expected a Boolar Jmp(Return) terminator"),
            Self::PreInitUnsupported => write!(f, "pre-initialized storage is unsupported"),
            Self::UnsupportedStatement { index } => {
                write!(f, "Boolar statement {index} is not a supported pure gate")
            }
            Self::InvalidStatementInput { statement, var } => write!(
                f,
                "Boolar statement {statement} uses undefined or forward variable {}",
                var.0
            ),
            Self::InvalidOutput { output, var } => {
                write!(
                    f,
                    "Boolar output {output} references undefined variable {}",
                    var.0
                )
            }
            Self::VarSpaceOverflow => write!(f, "Boolar variable space exceeds the u32 ID range"),
        }
    }
}

impl std::error::Error for InteropError {}

/// A validated view of a storage-free, circuit-shaped Boolar block.
///
/// Provenance and side annotations remain on the underlying statements but are
/// not part of the conversion contract.
#[derive(Debug)]
pub struct ValidatedBoolCircuit<'a, P: Clone> {
    block: &'a BIrBlock<P>,
    outputs: &'a [IRVarId],
}

impl<P: Clone> ValidatedBoolCircuit<'_, P> {
    /// Number of Boolean input wires.
    pub fn params(&self) -> u32 {
        self.block.params
    }

    /// Pure gate statements in SSA order.
    pub fn stmts(&self) -> &[Node<BIrStmt, P>] {
        &self.block.stmts
    }

    /// Ordered return wires.
    pub fn outputs(&self) -> &[IRVarId] {
        self.outputs
    }
}

/// Validate the circuit shape, statement subset, and SSA references required
/// by the Bristol and Summon converters.
pub fn validate_bool_circuit<P: Clone>(
    blocks: &BIrBlocks<P>,
) -> Result<ValidatedBoolCircuit<'_, P>, InteropError> {
    let [block] = blocks.blocks.as_slice() else {
        return Err(InteropError::NotSingleBlock {
            found: blocks.blocks.len(),
        });
    };

    if !blocks.pre_init.is_empty() {
        return Err(InteropError::PreInitUnsupported);
    }

    let outputs = match &block.terminator {
        BIrTerminator::Jmp(target) if target.block == IRBlockTargetId::Return => {
            target.args.as_slice()
        }
        _ => return Err(InteropError::NotReturnTerminator),
    };

    for (index, node) in block.stmts.iter().enumerate() {
        let index_u32 = u32::try_from(index).map_err(|_| InteropError::VarSpaceOverflow)?;
        let result_var = block
            .params
            .checked_add(index_u32)
            .ok_or(InteropError::VarSpaceOverflow)?;

        let check_input = |var: IRVarId| {
            if var.0 < result_var {
                Ok(())
            } else {
                Err(InteropError::InvalidStatementInput {
                    statement: index,
                    var,
                })
            }
        };

        match &node.kind {
            BIrStmt::Zero | BIrStmt::One => {}
            BIrStmt::And(a, b) | BIrStmt::Or(a, b) | BIrStmt::Xor(a, b) => {
                check_input(*a)?;
                check_input(*b)?;
            }
            BIrStmt::Not(value) => check_input(*value)?,
            _ => return Err(InteropError::UnsupportedStatement { index }),
        }
    }

    let statement_count =
        u32::try_from(block.stmts.len()).map_err(|_| InteropError::VarSpaceOverflow)?;
    let var_space = block
        .params
        .checked_add(statement_count)
        .ok_or(InteropError::VarSpaceOverflow)?;

    for (index, var) in outputs.iter().enumerate() {
        if var.0 >= var_space {
            return Err(InteropError::InvalidOutput {
                output: index,
                var: *var,
            });
        }
    }

    Ok(ValidatedBoolCircuit { block, outputs })
}

#[cfg(test)]
mod tests {
    use volar_ir::boolar::{
        BIrBlock, BIrBlocks, BIrPreInitSegment, BIrStmt, BIrTarget, BIrTerminator, LaneId,
    };
    use volar_ir::ir::{IRBlockTargetId, IRVarId, StorageId};

    use super::{InteropError, validate_bool_circuit};

    fn block(params: u32, stmts: Vec<BIrStmt>, outputs: Vec<IRVarId>) -> BIrBlocks<()> {
        BIrBlocks {
            blocks: vec![BIrBlock {
                params,
                stmts: stmts
                    .into_iter()
                    .map(|kind| volar_ir_common::Node::new(kind, (), None))
                    .collect(),
                terminator: BIrTerminator::Jmp(BIrTarget {
                    block: IRBlockTargetId::Return,
                    args: outputs,
                }),
            }],
            pre_init: vec![],
        }
    }

    #[test]
    fn accepts_a_well_formed_storage_free_circuit() {
        let circuit = block(
            2,
            vec![BIrStmt::And(IRVarId(0), IRVarId(1))],
            vec![IRVarId(2)],
        );

        let validated = validate_bool_circuit(&circuit).unwrap();
        assert_eq!(validated.params(), 2);
        assert_eq!(validated.outputs(), &[IRVarId(2)]);
        assert_eq!(validated.stmts().len(), 1);
    }

    #[test]
    fn rejects_preinitialized_storage() {
        let mut circuit = block(0, vec![], vec![]);
        circuit.pre_init.push(BIrPreInitSegment {
            storage: StorageId(0),
            lane: LaneId(0),
            addr: vec![],
            data: vec![true],
        });

        assert_eq!(
            validate_bool_circuit(&circuit).unwrap_err(),
            InteropError::PreInitUnsupported,
        );
    }

    #[test]
    fn rejects_storage_statements() {
        let circuit = block(
            0,
            vec![BIrStmt::StorageRead {
                storage: StorageId(0),
                lane: LaneId(0),
                addr: vec![],
            }],
            vec![IRVarId(0)],
        );

        assert_eq!(
            validate_bool_circuit(&circuit).unwrap_err(),
            InteropError::UnsupportedStatement { index: 0 },
        );
    }

    #[test]
    fn rejects_non_circuit_control_flow() {
        let mut circuit = block(1, vec![], vec![]);
        circuit.blocks[0].terminator = BIrTerminator::CondJmp {
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

        assert_eq!(
            validate_bool_circuit(&circuit).unwrap_err(),
            InteropError::NotReturnTerminator,
        );
    }

    #[test]
    fn rejects_forward_references() {
        let circuit = block(1, vec![BIrStmt::Not(IRVarId(1))], vec![IRVarId(1)]);

        assert_eq!(
            validate_bool_circuit(&circuit).unwrap_err(),
            InteropError::InvalidStatementInput {
                statement: 0,
                var: IRVarId(1),
            },
        );
    }

    #[test]
    fn rejects_out_of_range_outputs() {
        let circuit = block(1, vec![], vec![IRVarId(1)]);

        assert_eq!(
            validate_bool_circuit(&circuit).unwrap_err(),
            InteropError::InvalidOutput {
                output: 0,
                var: IRVarId(1),
            },
        );
    }
}
