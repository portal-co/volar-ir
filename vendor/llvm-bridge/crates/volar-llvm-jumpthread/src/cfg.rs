//! CFG utilities: successor/predecessor maps, back-edge detection, and
//! natural-loop-body computation.
//!
//! Adapted from `cirrus-llvm-pass`'s `deloopify.rs`, which proved this exact
//! approach (DFS back-edge detection + predecessor-closure natural loop
//! body) safe and correct for a real LLVM-IR-mutating pass. Generalized here
//! to be reusable by any loop found in a function, not just single-latch
//! early-exit candidates — callers still enforce their own single-latch/
//! no-nested-loop restrictions on top of this.

use std::collections::{HashMap, HashSet};

use inkwell::basic_block::BasicBlock;
use inkwell::values::{FunctionValue, InstructionValue, Operand};

/// The successors of `block`'s terminator, in operand order. Only `br`
/// (unconditional/conditional) is decoded; anything else (switch,
/// indirectbr, ret, unreachable, invoke, ...) returns an empty list here —
/// callers needing switch/other terminator successors inspect the
/// terminator directly.
pub fn br_successors(block: BasicBlock<'_>) -> Vec<BasicBlock<'_>> {
    let Some(terminator) = block.get_terminator() else {
        return Vec::new();
    };
    match terminator.get_num_operands() {
        1 => terminator
            .get_operand(0)
            .and_then(Operand::block)
            .into_iter()
            .collect(),
        3 => [terminator.get_operand(1), terminator.get_operand(2)]
            .into_iter()
            .filter_map(|operand| operand.and_then(Operand::block))
            .collect(),
        _ => Vec::new(),
    }
}

/// All successors of `block`'s terminator, covering `br` and `switch`
/// (every case target plus the default), for CFG-shape purposes (back-edge
/// detection, natural loop body, external-use legality). Does not
/// distinguish which case selects which target — callers needing that
/// resolve the terminator themselves.
pub fn all_successors<'ctx>(block: BasicBlock<'ctx>) -> Vec<BasicBlock<'ctx>> {
    let Some(terminator) = block.get_terminator() else {
        return Vec::new();
    };
    if terminator.get_opcode() == inkwell::values::InstructionOpcode::Switch {
        // A switch's case *values* aren't tracked operands — only
        // `[condition, successor_0 (default), successor_1, successor_2, ...]`
        // are, contiguously from index 1 (see `LLVMGetSwitchCaseValue`'s doc:
        // "the first successor is the default destination"). No stride-2
        // skipping: every operand from 1 onward is a successor block.
        let n = terminator.get_num_operands();
        let mut out = Vec::new();
        for i in 1..n {
            if let Some(b) = terminator.get_operand(i).and_then(Operand::block) {
                out.push(b);
            }
        }
        return out;
    }
    br_successors(block)
}

pub fn build_cfg_maps<'ctx>(
    function: FunctionValue<'ctx>,
) -> (
    HashMap<BasicBlock<'ctx>, Vec<BasicBlock<'ctx>>>,
    HashMap<BasicBlock<'ctx>, Vec<BasicBlock<'ctx>>>,
) {
    let mut successors = HashMap::new();
    let mut predecessors: HashMap<BasicBlock<'ctx>, Vec<BasicBlock<'ctx>>> = HashMap::new();
    for block in function.get_basic_blocks() {
        let s = all_successors(block);
        for &target in &s {
            predecessors.entry(target).or_default().push(block);
        }
        successors.insert(block, s);
    }
    (successors, predecessors)
}

/// DFS back-edge detection. Returns `(latch, header)` pairs.
pub fn find_back_edges<'ctx>(
    entry: BasicBlock<'ctx>,
    successors: &HashMap<BasicBlock<'ctx>, Vec<BasicBlock<'ctx>>>,
) -> Vec<(BasicBlock<'ctx>, BasicBlock<'ctx>)> {
    let mut visited = HashSet::new();
    let mut on_stack = HashSet::new();
    let mut back_edges = Vec::new();
    let empty = Vec::new();
    let mut stack: Vec<(BasicBlock<'ctx>, std::slice::Iter<'_, BasicBlock<'ctx>>)> = Vec::new();

    visited.insert(entry);
    on_stack.insert(entry);
    stack.push((entry, successors.get(&entry).unwrap_or(&empty).iter()));

    while let Some((node, iter)) = stack.last_mut() {
        let node = *node;
        if let Some(&successor) = iter.next() {
            if on_stack.contains(&successor) {
                back_edges.push((node, successor));
            } else if visited.insert(successor) {
                on_stack.insert(successor);
                stack.push((
                    successor,
                    successors.get(&successor).unwrap_or(&empty).iter(),
                ));
            }
        } else {
            on_stack.remove(&node);
            stack.pop();
        }
    }
    back_edges
}

/// The standard natural-loop node set for back edge `latch -> header`:
/// `header` plus every block that can reach `latch` without passing back
/// through `header`.
pub fn natural_loop_body<'ctx>(
    header: BasicBlock<'ctx>,
    latch: BasicBlock<'ctx>,
    predecessors: &HashMap<BasicBlock<'ctx>, Vec<BasicBlock<'ctx>>>,
) -> HashSet<BasicBlock<'ctx>> {
    let mut body = HashSet::new();
    body.insert(header);
    body.insert(latch);
    let mut stack = vec![latch];
    while let Some(block) = stack.pop() {
        if block == header {
            continue;
        }
        for &predecessor in predecessors.get(&block).into_iter().flatten() {
            if body.insert(predecessor) {
                stack.push(predecessor);
            }
        }
    }
    body
}

/// Decode a 3-operand conditional `br`'s `(condition, false_target, true_target)`.
/// LLVM's low-level operand list stores the false successor before the true
/// successor.
pub fn conditional_branch_parts(
    terminator: InstructionValue<'_>,
) -> Option<(
    inkwell::values::BasicValueEnum<'_>,
    BasicBlock<'_>,
    BasicBlock<'_>,
)> {
    if terminator.get_num_operands() != 3 {
        return None;
    }
    let condition = terminator.get_operand(0).and_then(Operand::value)?;
    let false_target = terminator.get_operand(1).and_then(Operand::block)?;
    let true_target = terminator.get_operand(2).and_then(Operand::block)?;
    Some((condition, false_target, true_target))
}
