//! Bristol Fashion emission from validated Boolar circuits.

use std::fmt::Write as _;

use volar_ir::boolar::{BIrBlocks, BIrStmt};
use volar_ir::ir::IRVarId;

use crate::{BristolIoLayout, InteropError, validate_bool_circuit};

/// Emit a Boolean Bristol Fashion circuit using one input group and one output
/// group.
pub fn export_bristol_fashion<P: Clone>(blocks: &BIrBlocks<P>) -> Result<String, InteropError> {
    let view = validate_bool_circuit(blocks)?;
    let io = BristolIoLayout {
        input_groups: vec![view.params() as usize],
        output_groups: vec![view.outputs().len()],
    };
    emit_validated(&view, &io)
}

/// Emit a Boolean Bristol Fashion circuit with explicit input/output grouping.
pub fn export_bristol_fashion_with_layout<P: Clone>(
    blocks: &BIrBlocks<P>,
    io: &BristolIoLayout,
) -> Result<String, InteropError> {
    let view = validate_bool_circuit(blocks)?;
    emit_validated(&view, io)
}

fn emit_validated<P: Clone>(
    circuit: &crate::ValidatedBoolCircuit<'_, P>,
    io: &BristolIoLayout,
) -> Result<String, InteropError> {
    let input_width = sum_widths(&io.input_groups)?;
    let output_width = sum_widths(&io.output_groups)?;
    if input_width != circuit.params() as usize {
        return Err(InteropError::IoLayoutMismatch {
            kind: "input",
            expected: circuit.params() as usize,
            got: input_width,
        });
    }
    if output_width != circuit.outputs().len() {
        return Err(InteropError::IoLayoutMismatch {
            kind: "output",
            expected: circuit.outputs().len(),
            got: output_width,
        });
    }

    let mut next_wire = circuit.params() as usize;
    let mut gate_count = 0usize;
    let mut gate_lines = String::new();
    let mut statement_wires = Vec::with_capacity(circuit.stmts().len());

    for (index, node) in circuit.stmts().iter().enumerate() {
        let result_wire = match &node.kind {
            BIrStmt::Zero => {
                let out = fresh_wire(&mut next_wire)?;
                write_gate(
                    &mut gate_lines,
                    &mut gate_count,
                    format_args!("1 1 0 {out} EQ"),
                )?;
                out
            }
            BIrStmt::One => {
                let out = fresh_wire(&mut next_wire)?;
                write_gate(
                    &mut gate_lines,
                    &mut gate_count,
                    format_args!("1 1 1 {out} EQ"),
                )?;
                out
            }
            BIrStmt::And(a, b) => {
                let a = wire_for(*a, circuit.params(), &statement_wires, index)?;
                let b = wire_for(*b, circuit.params(), &statement_wires, index)?;
                let out = fresh_wire(&mut next_wire)?;
                write_gate(
                    &mut gate_lines,
                    &mut gate_count,
                    format_args!("2 1 {a} {b} {out} AND"),
                )?;
                out
            }
            BIrStmt::Xor(a, b) => {
                let a = wire_for(*a, circuit.params(), &statement_wires, index)?;
                let b = wire_for(*b, circuit.params(), &statement_wires, index)?;
                let out = fresh_wire(&mut next_wire)?;
                write_gate(
                    &mut gate_lines,
                    &mut gate_count,
                    format_args!("2 1 {a} {b} {out} XOR"),
                )?;
                out
            }
            BIrStmt::Not(value) => {
                let input = wire_for(*value, circuit.params(), &statement_wires, index)?;
                let out = fresh_wire(&mut next_wire)?;
                write_gate(
                    &mut gate_lines,
                    &mut gate_count,
                    format_args!("1 1 {input} {out} INV"),
                )?;
                out
            }
            BIrStmt::Or(a, b) => {
                let a = wire_for(*a, circuit.params(), &statement_wires, index)?;
                let b = wire_for(*b, circuit.params(), &statement_wires, index)?;
                let and_out = fresh_wire(&mut next_wire)?;
                write_gate(
                    &mut gate_lines,
                    &mut gate_count,
                    format_args!("2 1 {a} {b} {and_out} AND"),
                )?;
                let xor_out = fresh_wire(&mut next_wire)?;
                write_gate(
                    &mut gate_lines,
                    &mut gate_count,
                    format_args!("2 1 {a} {b} {xor_out} XOR"),
                )?;
                let out = fresh_wire(&mut next_wire)?;
                write_gate(
                    &mut gate_lines,
                    &mut gate_count,
                    format_args!("2 1 {and_out} {xor_out} {out} XOR"),
                )?;
                out
            }
            _ => return Err(InteropError::UnsupportedStatement { index }),
        };
        statement_wires.push(result_wire);
    }

    for output in circuit.outputs() {
        let input = wire_for(
            *output,
            circuit.params(),
            &statement_wires,
            circuit.stmts().len(),
        )?;
        let out = fresh_wire(&mut next_wire)?;
        write_gate(
            &mut gate_lines,
            &mut gate_count,
            format_args!("1 1 {input} {out} EQW"),
        )?;
    }

    let mut text = String::new();
    writeln!(&mut text, "{gate_count} {next_wire}").expect("writing to String cannot fail");
    write_groups(&mut text, &io.input_groups);
    write_groups(&mut text, &io.output_groups);
    text.push('\n');
    text.push_str(&gate_lines);
    Ok(text)
}

fn wire_for(
    var: IRVarId,
    params: u32,
    statement_wires: &[usize],
    current_statement: usize,
) -> Result<usize, InteropError> {
    if var.0 < params {
        return Ok(var.0 as usize);
    }
    let index = (var.0 - params) as usize;
    if index >= current_statement {
        return Err(InteropError::InvalidStatementInput {
            statement: current_statement,
            var,
        });
    }
    statement_wires
        .get(index)
        .copied()
        .ok_or(InteropError::InvalidStatementInput {
            statement: current_statement,
            var,
        })
}

fn fresh_wire(next_wire: &mut usize) -> Result<usize, InteropError> {
    let out = *next_wire;
    *next_wire = next_wire
        .checked_add(1)
        .ok_or(InteropError::BristolCountOverflow)?;
    Ok(out)
}

fn write_gate(
    output: &mut String,
    gate_count: &mut usize,
    line: std::fmt::Arguments<'_>,
) -> Result<(), InteropError> {
    writeln!(output, "{line}").expect("writing to String cannot fail");
    *gate_count = gate_count
        .checked_add(1)
        .ok_or(InteropError::BristolCountOverflow)?;
    Ok(())
}

fn write_groups(output: &mut String, groups: &[usize]) {
    write!(output, "{}", groups.len()).expect("writing to String cannot fail");
    for width in groups {
        write!(output, " {width}").expect("writing to String cannot fail");
    }
    output.push('\n');
}

fn sum_widths(widths: &[usize]) -> Result<usize, InteropError> {
    widths.iter().try_fold(0usize, |sum, width| {
        sum.checked_add(*width)
            .ok_or(InteropError::BristolCountOverflow)
    })
}
