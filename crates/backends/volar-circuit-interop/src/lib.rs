//! Interoperability between storage-free circuit-shaped Boolar IR, Bristol
//! Boolean circuits, and Summon source code.
//!
//! Conversion preserves the Boolean function and ordered input/output bits;
//! it does not preserve Boolar provenance, side annotations, or Bristol party
//! names. Storage and external effects are rejected.

use volar_ir::boolar::{BIrBlock, BIrBlocks, BIrStmt, BIrTerminator};
use volar_ir::ir::{IRBlockTargetId, IRVarId};
use volar_ir_common::Node;

mod bristol;
mod emit;
mod summon;

pub use bristol::{
    BristolAnd, BristolCircuit, BristolGate, BristolIoLayout, BristolLimits,
    import_bristol_fashion, import_bristol_fashion_with_limits, parse_bristol_fashion,
    parse_bristol_fashion_with_limits,
};
pub use emit::{export_bristol_fashion, export_bristol_fashion_with_layout};
pub use summon::emit_summon_source;

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
    /// A Bristol Fashion text record is malformed or invalid.
    BristolParse { line: usize, message: String },
    /// A Bristol gate opcode is outside the supported Boolean subset.
    UnsupportedBristolGate { line: usize, operation: String },
    /// Bristol input or output groups do not have the circuit's width.
    IoLayoutMismatch {
        kind: &'static str,
        expected: usize,
        got: usize,
    },
    /// A Bristol wire/gate count exceeds the host's representable range.
    BristolCountOverflow,
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
            Self::BristolParse { line, message } => {
                write!(f, "invalid Bristol Fashion input at line {line}: {message}")
            }
            Self::UnsupportedBristolGate { line, operation } => {
                write!(f, "unsupported Bristol gate `{operation}` at line {line}")
            }
            Self::IoLayoutMismatch {
                kind,
                expected,
                got,
            } => write!(
                f,
                "Bristol {kind} layout has width {got}, expected {expected}"
            ),
            Self::BristolCountOverflow => write!(f, "Bristol gate or wire count overflowed"),
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

#[cfg(test)]
mod bristol_tests {
    use volar_ir::boolar::{BIrStmt, BIrTerminator};
    use volar_ir::ir::IRVarId;

    use super::{
        BristolGate, BristolIoLayout, BristolLimits, InteropError, emit_summon_source,
        export_bristol_fashion, export_bristol_fashion_with_layout, import_bristol_fashion,
        parse_bristol_fashion, parse_bristol_fashion_with_limits,
    };

    #[test]
    fn parses_and_imports_a_basic_fashion_circuit() {
        let text = "1 3\n2 1 1\n1 1\n2 1 0 1 2 AND\n";

        let parsed = parse_bristol_fashion(text).unwrap();
        assert_eq!(parsed.wire_count, 3);
        assert_eq!(parsed.input_groups, vec![1, 1]);
        assert_eq!(parsed.output_groups, vec![1]);
        assert_eq!(parsed.gates, vec![BristolGate::And { a: 0, b: 1, out: 2 }]);

        let (blocks, io) = import_bristol_fashion(text).unwrap();
        assert_eq!(io.input_groups, vec![1, 1]);
        assert_eq!(io.output_groups, vec![1]);
        assert_eq!(blocks.blocks[0].params, 2);
        assert!(matches!(
            blocks.blocks[0].stmts[0].kind,
            BIrStmt::And(IRVarId(0), IRVarId(1))
        ));
        assert!(matches!(
            &blocks.blocks[0].terminator,
            BIrTerminator::Jmp(target) if target.args == vec![IRVarId(2)]
        ));
    }

    #[test]
    fn imports_xor_and_not_aliases() {
        let text = "2 4\n2 1 1\n1 1\n2 1 0 1 2 XOR\n1 1 2 3 NOT\n";
        let (blocks, _) = import_bristol_fashion(text).unwrap();
        assert_eq!(
            blocks.blocks[0].stmts[0].kind,
            BIrStmt::Xor(IRVarId(0), IRVarId(1))
        );
        assert_eq!(blocks.blocks[0].stmts[1].kind, BIrStmt::Not(IRVarId(2)));
        assert!(matches!(
            &blocks.blocks[0].terminator,
            BIrTerminator::Jmp(target) if target.args == vec![IRVarId(3)]
        ));
    }

    #[test]
    fn imports_constants_and_wire_copies() {
        let text = "2 3\n1 1\n1 2\n1 1 1 1 EQ\n1 1 1 2 EQW\n";
        let (blocks, _) = import_bristol_fashion(text).unwrap();

        assert_eq!(blocks.blocks[0].params, 1);
        assert_eq!(blocks.blocks[0].stmts.len(), 1);
        assert_eq!(blocks.blocks[0].stmts[0].kind, BIrStmt::One);
        assert!(matches!(
            &blocks.blocks[0].terminator,
            BIrTerminator::Jmp(target) if target.args == vec![IRVarId(1), IRVarId(1)]
        ));
    }

    #[test]
    fn eq_constant_literals_are_not_wire_references() {
        let text = "1 1\n0\n1 1\n1 1 1 0 EQ\n";
        let (blocks, _) = import_bristol_fashion(text).unwrap();
        assert_eq!(blocks.blocks[0].stmts[0].kind, BIrStmt::One);
        assert!(matches!(
            &blocks.blocks[0].terminator,
            BIrTerminator::Jmp(target) if target.args == vec![IRVarId(0)]
        ));
    }

    #[test]
    fn expands_extended_mand_into_independent_and_gates() {
        let text = "1 6\n1 4\n1 2\n4 2 0 1 2 3 4 5 MAND\n";
        let (blocks, _) = import_bristol_fashion(text).unwrap();

        assert_eq!(blocks.blocks[0].stmts.len(), 2);
        assert_eq!(
            blocks.blocks[0].stmts[0].kind,
            BIrStmt::And(IRVarId(0), IRVarId(1))
        );
        assert_eq!(
            blocks.blocks[0].stmts[1].kind,
            BIrStmt::And(IRVarId(2), IRVarId(3))
        );
        assert!(matches!(
            &blocks.blocks[0].terminator,
            BIrTerminator::Jmp(target) if target.args == vec![IRVarId(4), IRVarId(5)]
        ));
    }

    #[test]
    fn mand_pairs_read_a_snapshot_before_any_output_wire_is_reassigned() {
        let text = "2 6\n1 4\n1 2\n4 2 0 1 2 3 2 4 MAND\n1 1 2 5 EQW\n";
        let (blocks, _) = import_bristol_fashion(text).unwrap();

        assert_eq!(
            blocks.blocks[0].stmts[1].kind,
            BIrStmt::And(IRVarId(2), IRVarId(3)),
        );
        assert!(matches!(
            &blocks.blocks[0].terminator,
            BIrTerminator::Jmp(target) if target.args == vec![IRVarId(5), IRVarId(4)]
        ));
    }

    #[test]
    fn later_gate_definitions_reuse_wire_ids_without_aliasing_old_ssa_values() {
        let text = "3 3\n1 1\n1 1\n1 1 0 1 EQ\n1 1 1 1 EQ\n1 1 1 2 EQW\n";
        let (blocks, _) = import_bristol_fashion(text).unwrap();

        assert_eq!(blocks.blocks[0].stmts.len(), 2);
        assert_eq!(blocks.blocks[0].stmts[0].kind, BIrStmt::Zero);
        assert_eq!(blocks.blocks[0].stmts[1].kind, BIrStmt::One);
        assert!(matches!(
            &blocks.blocks[0].terminator,
            BIrTerminator::Jmp(target) if target.args == vec![IRVarId(2)]
        ));
    }

    #[test]
    fn supports_a_wire_passthrough_circuit() {
        let text = "0 1\n1 1\n1 1\n";
        let (blocks, _) = import_bristol_fashion(text).unwrap();
        assert!(blocks.blocks[0].stmts.is_empty());
        assert!(matches!(
            &blocks.blocks[0].terminator,
            BIrTerminator::Jmp(target) if target.args == vec![IRVarId(0)]
        ));
    }

    #[test]
    fn rejects_non_boolean_gate_opcodes() {
        let text = "1 3\n2 1 1\n1 1\n2 1 0 1 2 AAdd\n";
        assert!(matches!(
            parse_bristol_fashion(text),
            Err(InteropError::UnsupportedBristolGate { line: 4, operation }) if operation == "AAdd"
        ));
    }

    #[test]
    fn exports_canonical_fashion_with_final_output_copies() {
        let blocks = circuit(
            2,
            vec![BIrStmt::And(IRVarId(0), IRVarId(1))],
            vec![IRVarId(2)],
        );
        let text = export_bristol_fashion(&blocks).unwrap();
        assert_eq!(text, "2 4\n1 2\n1 1\n\n2 1 0 1 2 AND\n1 1 2 3 EQW\n");

        let (imported, _) = import_bristol_fashion(&text).unwrap();
        assert!(matches!(
            &imported.blocks[0].terminator,
            BIrTerminator::Jmp(target) if target.args == vec![IRVarId(2)]
        ));
    }

    #[test]
    fn export_expands_or_and_preserves_custom_io_groups() {
        let blocks = circuit(
            3,
            vec![
                BIrStmt::Or(IRVarId(0), IRVarId(1)),
                BIrStmt::Xor(IRVarId(0), IRVarId(2)),
            ],
            vec![IRVarId(3), IRVarId(4)],
        );
        let layout = BristolIoLayout {
            input_groups: vec![1, 2],
            output_groups: vec![1, 1],
        };
        let text = export_bristol_fashion_with_layout(&blocks, &layout).unwrap();
        let (imported, imported_layout) = import_bristol_fashion(&text).unwrap();
        assert_eq!(imported_layout, layout);
        assert_eq!(imported.blocks[0].stmts.len(), 4);
        assert!(matches!(
            &imported.blocks[0].terminator,
            BIrTerminator::Jmp(target) if target.args.len() == 2
        ));
    }

    #[test]
    fn export_rejects_io_group_width_mismatches() {
        let blocks = circuit(2, vec![], vec![IRVarId(0)]);
        let layout = BristolIoLayout {
            input_groups: vec![1],
            output_groups: vec![1],
        };
        assert_eq!(
            export_bristol_fashion_with_layout(&blocks, &layout).unwrap_err(),
            InteropError::IoLayoutMismatch {
                kind: "input",
                expected: 2,
                got: 1,
            }
        );
    }

    fn circuit(
        params: u32,
        stmts: Vec<BIrStmt>,
        outputs: Vec<IRVarId>,
    ) -> volar_ir::boolar::BIrBlocks<()> {
        let mut block = volar_ir::boolar::BIrBlock {
            params,
            stmts: vec![],
            terminator: BIrTerminator::Jmp(volar_ir::boolar::BIrTarget {
                block: volar_ir::ir::IRBlockTargetId::Return,
                args: outputs,
            }),
        };
        for stmt in stmts {
            block.push_stmt(stmt, ());
        }
        volar_ir::boolar::BIrBlocks {
            blocks: vec![block],
            pre_init: vec![],
        }
    }

    #[test]
    fn emits_straight_line_summon_source_for_boolean_gates() {
        let blocks = circuit(
            2,
            vec![
                BIrStmt::Zero,
                BIrStmt::One,
                BIrStmt::Or(IRVarId(0), IRVarId(1)),
                BIrStmt::Xor(IRVarId(2), IRVarId(3)),
                BIrStmt::Not(IRVarId(4)),
            ],
            vec![IRVarId(5), IRVarId(6)],
        );
        let source = emit_summon_source(&blocks).unwrap();
        assert_eq!(
            source,
            concat!(
                "/** generated by volar-circuit-interop */\n",
                "export default function evalCircuit(input: boolean[]): boolean[] {\n",
                "  if (input.length !== 2) throw new Error(\"input length\");\n",
                "\n",
                "  const w2 = false;\n",
                "  const w3 = true;\n",
                "  const w4 = input[0] || input[1];\n",
                "  const w5 = w2 !== w3;\n",
                "  const w6 = !w4;\n",
                "  return [w5, w6];\n",
                "}\n"
            )
        );
    }

    #[test]
    fn summon_source_uses_checked_static_inputs_and_passthrough_outputs() {
        let blocks = circuit(1, vec![], vec![IRVarId(0), IRVarId(0)]);
        let source = emit_summon_source(&blocks).unwrap();
        assert!(source.contains("if (input.length !== 1)"));
        assert!(source.contains("return [input[0], input[0]];"));
        assert!(!source.contains("for ("));
    }

    #[test]
    fn rejects_bad_arity_and_configured_resource_overruns() {
        let wrong_arity = "1 3\n1 1\n1 1\n1 1 0 2 AND\n";
        assert!(matches!(
            parse_bristol_fashion(wrong_arity),
            Err(InteropError::BristolParse { line: 4, .. })
        ));

        let circuit = "1 3\n2 1 1\n1 1\n2 1 0 1 2 AND\n";
        assert!(matches!(
            parse_bristol_fashion_with_limits(
                circuit,
                BristolLimits {
                    max_gates: 0,
                    ..BristolLimits::default()
                }
            ),
            Err(InteropError::BristolParse { line: 1, .. })
        ));
    }

    #[test]
    fn rejects_undefined_wires_and_malformed_gate_counts() {
        let undefined = "1 3\n1 1\n1 1\n2 1 0 2 1 AND\n";
        assert!(matches!(
            parse_bristol_fashion(undefined),
            Err(InteropError::BristolParse { line: 4, .. })
        ));

        let truncated = "2 3\n1 2\n1 1\n2 1 0 1 2 AND\n";
        assert!(matches!(
            parse_bristol_fashion(truncated),
            Err(InteropError::BristolParse { .. })
        ));
    }
}
