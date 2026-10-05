//! Bristol Fashion text parsing and Boolar import.

use std::collections::BTreeSet;

use volar_ir::boolar::{BIrBlock, BIrBlocks, BIrStmt, BIrTarget, BIrTerminator};
use volar_ir::ir::{IRBlockTargetId, IRVarId};

use crate::InteropError;

/// Resource limits applied before Bristol header values control allocations or
/// loops. Larger workloads may opt into higher limits explicitly.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BristolLimits {
    pub max_gates: usize,
    pub max_wires: usize,
    pub max_io_wires: usize,
}

impl Default for BristolLimits {
    fn default() -> Self {
        Self {
            max_gates: 1_000_000,
            max_wires: 2_000_000,
            max_io_wires: 1_000_000,
        }
    }
}

/// Bristol input/output grouping, which is boundary metadata and not part of
/// Boolar's flat parameter/return representation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BristolIoLayout {
    pub input_groups: Vec<usize>,
    pub output_groups: Vec<usize>,
}

/// A validated Boolean Bristol Fashion circuit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BristolCircuit {
    pub wire_count: usize,
    pub input_groups: Vec<usize>,
    pub output_groups: Vec<usize>,
    pub gates: Vec<BristolGate>,
}

/// One member of an extended Bristol `MAND` operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BristolAnd {
    pub a: usize,
    pub b: usize,
    pub out: usize,
}

/// Supported Bristol operations. `AndBatch` retains the simultaneous `MAND`
/// write boundary so recycled wire IDs are interpreted correctly on import.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BristolGate {
    And { a: usize, b: usize, out: usize },
    AndBatch(Vec<BristolAnd>),
    Xor { a: usize, b: usize, out: usize },
    Not { input: usize, out: usize },
    Const { value: bool, out: usize },
    Copy { input: usize, out: usize },
}

#[derive(Clone, Copy)]
struct SourceLine<'a> {
    number: usize,
    text: &'a str,
}

/// Parse and validate a Bristol Fashion file using default resource limits.
pub fn parse_bristol_fashion(input: &str) -> Result<BristolCircuit, InteropError> {
    parse_bristol_fashion_with_limits(input, BristolLimits::default())
}

/// Parse and validate a Bristol Fashion file using caller-selected limits.
pub fn parse_bristol_fashion_with_limits(
    input: &str,
    limits: BristolLimits,
) -> Result<BristolCircuit, InteropError> {
    let lines = input
        .lines()
        .enumerate()
        .filter_map(|(index, text)| {
            (!text.trim().is_empty()).then_some(SourceLine {
                number: index + 1,
                text: text.trim(),
            })
        })
        .collect::<Vec<_>>();

    if lines.len() < 3 {
        return Err(parse_error(
            input.lines().count().saturating_add(1),
            "expected three Bristol Fashion header lines",
        ));
    }

    let first = tokens(lines[0]);
    if first.len() != 2 {
        return Err(parse_error(
            lines[0].number,
            "first header line must contain gate and wire counts",
        ));
    }
    let gate_count = parse_usize(first[0], lines[0].number, "gate count")?;
    let wire_count = parse_usize(first[1], lines[0].number, "wire count")?;
    enforce_limit(gate_count, limits.max_gates, "gate count", lines[0].number)?;
    enforce_limit(wire_count, limits.max_wires, "wire count", lines[0].number)?;

    let (input_groups, input_count) = parse_groups(lines[1], "input")?;
    let (output_groups, output_count) = parse_groups(lines[2], "output")?;
    enforce_limit(
        input_count,
        limits.max_io_wires,
        "input wire count",
        lines[1].number,
    )?;
    enforce_limit(
        output_count,
        limits.max_io_wires,
        "output wire count",
        lines[2].number,
    )?;
    if input_count > wire_count {
        return Err(parse_error(
            lines[1].number,
            "input wires exceed declared wire count",
        ));
    }
    if output_count > wire_count {
        return Err(parse_error(
            lines[2].number,
            "output wires exceed declared wire count",
        ));
    }

    let expected_lines = 3usize
        .checked_add(gate_count)
        .ok_or_else(|| parse_error(lines[0].number, "gate count overflows line count"))?;
    if lines.len() != expected_lines {
        let line = lines.get(expected_lines).map_or_else(
            || input.lines().count().saturating_add(1),
            |extra| extra.number,
        );
        return Err(parse_error(
            line,
            format!(
                "header declares {gate_count} gates but file contains {} gate records",
                lines.len().saturating_sub(3)
            ),
        ));
    }

    let mut gates = Vec::new();
    let mut produced = BTreeSet::new();
    for line in lines.iter().skip(3) {
        let parsed = parse_gate(*line, wire_count)?;
        let (inputs, outputs) = gate_wires(&parsed);

        // Gate inputs are read before any outputs are assigned, allowing
        // Bristol register recycling to overwrite a wire at its last use.
        for wire in inputs {
            if wire >= input_count && !produced.contains(&wire) {
                return Err(parse_error(
                    line.number,
                    format!("gate reads undefined wire {wire}"),
                ));
            }
        }
        // Reassignment is valid and expected when a Bristol writer recycles
        // dead registers.
        produced.extend(outputs);
        gates.push(parsed);
    }

    let first_output = wire_count - output_count;
    for wire in first_output..wire_count {
        if wire >= input_count && !produced.contains(&wire) {
            return Err(parse_error(
                lines[2].number,
                format!("output wire {wire} is never defined"),
            ));
        }
    }

    Ok(BristolCircuit {
        wire_count,
        input_groups,
        output_groups,
        gates,
    })
}

/// Parse a Boolean Bristol Fashion file into circuit-shaped Boolar IR and
/// return the original grouped IO widths alongside it.
pub fn import_bristol_fashion(
    input: &str,
) -> Result<(BIrBlocks<()>, BristolIoLayout), InteropError> {
    import_bristol_fashion_with_limits(input, BristolLimits::default())
}

/// Parse a Boolean Bristol Fashion file into Boolar IR with caller-selected
/// resource limits.
pub fn import_bristol_fashion_with_limits(
    input: &str,
    limits: BristolLimits,
) -> Result<(BIrBlocks<()>, BristolIoLayout), InteropError> {
    let circuit = parse_bristol_fashion_with_limits(input, limits)?;
    let input_count = sum_groups(&circuit.input_groups)?;
    let output_count = sum_groups(&circuit.output_groups)?;
    let params = u32::try_from(input_count).map_err(|_| InteropError::VarSpaceOverflow)?;

    let mut block = BIrBlock {
        params,
        stmts: Vec::new(),
        terminator: BIrTerminator::Jmp(BIrTarget {
            block: IRBlockTargetId::Return,
            args: Vec::new(),
        }),
    };
    // Only computed/reassigned wires need entries. Inputs otherwise map
    // directly to their initial Boolar parameter IDs.
    let mut current_values = std::collections::BTreeMap::<usize, IRVarId>::new();

    for gate in circuit.gates {
        match gate {
            BristolGate::And { a, b, out } => {
                let lhs = current_value(a, input_count, &current_values)?;
                let rhs = current_value(b, input_count, &current_values)?;
                let id = push_stmt(&mut block, BIrStmt::And(lhs, rhs))?;
                current_values.insert(out, id);
            }
            BristolGate::AndBatch(batch) => {
                // MAND outputs are concurrent: resolve all operands against
                // the pre-gate wire map before binding any output wire.
                let resolved = batch
                    .iter()
                    .map(|gate| {
                        Ok((
                            current_value(gate.a, input_count, &current_values)?,
                            current_value(gate.b, input_count, &current_values)?,
                            gate.out,
                        ))
                    })
                    .collect::<Result<Vec<_>, InteropError>>()?;
                for (a, b, out) in resolved {
                    let id = push_stmt(&mut block, BIrStmt::And(a, b))?;
                    current_values.insert(out, id);
                }
            }
            BristolGate::Xor { a, b, out } => {
                let lhs = current_value(a, input_count, &current_values)?;
                let rhs = current_value(b, input_count, &current_values)?;
                let id = push_stmt(&mut block, BIrStmt::Xor(lhs, rhs))?;
                current_values.insert(out, id);
            }
            BristolGate::Not { input, out } => {
                let value = current_value(input, input_count, &current_values)?;
                let id = push_stmt(&mut block, BIrStmt::Not(value))?;
                current_values.insert(out, id);
            }
            BristolGate::Const { value, out } => {
                let stmt = if value { BIrStmt::One } else { BIrStmt::Zero };
                let id = push_stmt(&mut block, stmt)?;
                current_values.insert(out, id);
            }
            BristolGate::Copy { input, out } => {
                let value = current_value(input, input_count, &current_values)?;
                current_values.insert(out, value);
            }
        }
    }

    let first_output = circuit.wire_count - output_count;
    let mut outputs = Vec::with_capacity(output_count);
    for wire in first_output..circuit.wire_count {
        outputs.push(current_value(wire, input_count, &current_values)?);
    }
    if let BIrTerminator::Jmp(target) = &mut block.terminator {
        target.args = outputs;
    }

    let io = BristolIoLayout {
        input_groups: circuit.input_groups,
        output_groups: circuit.output_groups,
    };
    Ok((
        BIrBlocks {
            blocks: vec![block],
            pre_init: Vec::new(),
        },
        io,
    ))
}

fn push_stmt(block: &mut BIrBlock<()>, stmt: BIrStmt) -> Result<IRVarId, InteropError> {
    let index = u32::try_from(block.stmts.len()).map_err(|_| InteropError::VarSpaceOverflow)?;
    let id = block
        .params
        .checked_add(index)
        .ok_or(InteropError::VarSpaceOverflow)?;
    block.push_stmt(stmt, ());
    Ok(IRVarId(id))
}

fn current_value(
    wire: usize,
    input_count: usize,
    current_values: &std::collections::BTreeMap<usize, IRVarId>,
) -> Result<IRVarId, InteropError> {
    if let Some(value) = current_values.get(&wire) {
        return Ok(*value);
    }
    if wire < input_count {
        return u32::try_from(wire)
            .map(IRVarId)
            .map_err(|_| InteropError::VarSpaceOverflow);
    }
    // The parser guarantees all gate inputs and final outputs are defined.
    Err(parse_error(
        0,
        format!("wire {wire} lost its validated definition"),
    ))
}

fn parse_gate(line: SourceLine<'_>, wire_count: usize) -> Result<BristolGate, InteropError> {
    let parts = tokens(line);
    if parts.len() < 3 {
        return Err(parse_error(line.number, "gate record is too short"));
    }
    let input_count = parse_usize(parts[0], line.number, "gate input count")?;
    let output_count = parse_usize(parts[1], line.number, "gate output count")?;
    let expected = input_count
        .checked_add(output_count)
        .and_then(|count| count.checked_add(3))
        .ok_or_else(|| parse_error(line.number, "gate arity overflows record length"))?;
    if parts.len() != expected {
        return Err(parse_error(
            line.number,
            format!(
                "gate record has {} fields, expected {expected}",
                parts.len()
            ),
        ));
    }

    let inputs_end = 2 + input_count;
    let outputs_end = inputs_end + output_count;
    let inputs = parts[2..inputs_end]
        .iter()
        .map(|token| parse_usize(token, line.number, "input wire"))
        .collect::<Result<Vec<_>, _>>()?;
    let outputs = parts[inputs_end..outputs_end]
        .iter()
        .map(|token| parse_usize(token, line.number, "output wire"))
        .collect::<Result<Vec<_>, _>>()?;
    let operation = parts[outputs_end];
    for wire in &outputs {
        if *wire >= wire_count {
            return Err(parse_error(
                line.number,
                format!("wire {wire} exceeds declared wire count {wire_count}"),
            ));
        }
    }
    // `EQ`'s input field is a literal constant selector, not a wire ID.
    if operation != "EQ" {
        for wire in &inputs {
            if *wire >= wire_count {
                return Err(parse_error(
                    line.number,
                    format!("wire {wire} exceeds declared wire count {wire_count}"),
                ));
            }
        }
    }
    if outputs
        .iter()
        .enumerate()
        .any(|(i, wire)| outputs[..i].contains(wire))
    {
        return Err(parse_error(
            line.number,
            "one gate assigns the same output wire more than once",
        ));
    }
    let bad_arity = || {
        parse_error(
            line.number,
            format!("invalid input/output arity for {operation}"),
        )
    };
    match operation {
        "AND" if input_count == 2 && output_count == 1 => Ok(BristolGate::And {
            a: inputs[0],
            b: inputs[1],
            out: outputs[0],
        }),
        "XOR" if input_count == 2 && output_count == 1 => Ok(BristolGate::Xor {
            a: inputs[0],
            b: inputs[1],
            out: outputs[0],
        }),
        "INV" | "NOT" if input_count == 1 && output_count == 1 => Ok(BristolGate::Not {
            input: inputs[0],
            out: outputs[0],
        }),
        "EQ" if input_count == 1 && output_count == 1 => {
            let value = match inputs[0] {
                0 => false,
                1 => true,
                _ => {
                    return Err(parse_error(
                        line.number,
                        "EQ constant source must be literal 0 or 1",
                    ));
                }
            };
            Ok(BristolGate::Const {
                value,
                out: outputs[0],
            })
        }
        "EQW" if input_count == 1 && output_count == 1 => Ok(BristolGate::Copy {
            input: inputs[0],
            out: outputs[0],
        }),
        "MAND"
            if output_count > 0
                && input_count.is_multiple_of(2)
                && input_count / 2 == output_count =>
        {
            Ok(BristolGate::AndBatch(
                inputs
                    .chunks(2)
                    .zip(outputs.iter())
                    .map(|(pair, out)| BristolAnd {
                        a: pair[0],
                        b: pair[1],
                        out: *out,
                    })
                    .collect(),
            ))
        }
        "AND" | "XOR" | "INV" | "NOT" | "EQ" | "EQW" | "MAND" => Err(bad_arity()),
        _ => Err(InteropError::UnsupportedBristolGate {
            line: line.number,
            operation: operation.to_string(),
        }),
    }
}

fn gate_wires(gate: &BristolGate) -> (Vec<usize>, Vec<usize>) {
    match gate {
        BristolGate::And { a, b, out } | BristolGate::Xor { a, b, out } => {
            (vec![*a, *b], vec![*out])
        }
        BristolGate::AndBatch(batch) => {
            let mut inputs = Vec::with_capacity(batch.len() * 2);
            let mut outputs = Vec::with_capacity(batch.len());
            for gate in batch {
                inputs.extend([gate.a, gate.b]);
                outputs.push(gate.out);
            }
            (inputs, outputs)
        }
        BristolGate::Not { input, out } | BristolGate::Copy { input, out } => {
            (vec![*input], vec![*out])
        }
        BristolGate::Const { out, .. } => (Vec::new(), vec![*out]),
    }
}

fn parse_groups(line: SourceLine<'_>, kind: &str) -> Result<(Vec<usize>, usize), InteropError> {
    let parts = tokens(line);
    let Some(count_token) = parts.first() else {
        return Err(parse_error(
            line.number,
            format!("missing {kind} group count"),
        ));
    };
    let count = parse_usize(count_token, line.number, &format!("{kind} group count"))?;
    let expected = count
        .checked_add(1)
        .ok_or_else(|| parse_error(line.number, format!("{kind} group count overflows")))?;
    if parts.len() != expected {
        return Err(parse_error(
            line.number,
            format!("{kind} group count does not match widths"),
        ));
    }
    let widths = parts[1..]
        .iter()
        .map(|token| parse_usize(token, line.number, &format!("{kind} group width")))
        .collect::<Result<Vec<_>, _>>()?;
    let total = widths
        .iter()
        .try_fold(0usize, |sum, width| sum.checked_add(*width));
    let total = total.ok_or_else(|| parse_error(line.number, format!("{kind} widths overflow")))?;
    Ok((widths, total))
}

fn sum_groups(groups: &[usize]) -> Result<usize, InteropError> {
    groups.iter().try_fold(0usize, |sum, width| {
        sum.checked_add(*width)
            .ok_or(InteropError::VarSpaceOverflow)
    })
}

fn tokens(line: SourceLine<'_>) -> Vec<&str> {
    line.text.split_whitespace().collect()
}

fn parse_usize(token: &str, line: usize, field: &str) -> Result<usize, InteropError> {
    token
        .parse()
        .map_err(|_| parse_error(line, format!("invalid {field}: {token}")))
}

fn enforce_limit(
    actual: usize,
    limit: usize,
    field: &'static str,
    line: usize,
) -> Result<(), InteropError> {
    if actual > limit {
        return Err(parse_error(
            line,
            format!("{field} {actual} exceeds configured limit {limit}"),
        ));
    }
    Ok(())
}

fn parse_error(line: usize, message: impl Into<String>) -> InteropError {
    InteropError::BristolParse {
        line,
        message: message.into(),
    }
}
