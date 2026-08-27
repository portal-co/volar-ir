// @reliability: experimental
// @ai: assisted
//! Constant-folding and boolean-simplification pass for Boolar IR.

use alloc::collections::BTreeMap;
use volar_ir::boolar::{BIrBlock, BIrBlocks, BIrStmt, BIrTarget, BIrTerminator};
use volar_ir::circuit::BCircuit;
use volar_ir::ir::{IRBlockTargetId, IRVarId};

use crate::common::canon_alias;

// ============================================================================
// Public API
// ============================================================================

/// Simplify each block of `blocks` in place until no further changes occur.
///
/// Returns `true` if any block was modified.
pub fn fold_biir_blocks<P: Clone>(blocks: &mut BIrBlocks<P>) -> bool {
    let mut any_changed = false;
    for block in blocks.blocks.iter_mut() {
        loop {
            if !fold_biir_block_once(block) {
                break;
            }
            any_changed = true;
        }
    }
    any_changed
}

/// Simplify a fused Boolar circuit in place until no further changes occur.
///
/// Constants are propagated through Boolean statements and aliases are
/// rewritten in both later statements and circuit outputs. Returns `true` if
/// the circuit was modified.
pub fn fold_biir_circuit<P: Clone>(circuit: &mut BCircuit<P>) -> bool {
    let mut any_changed = false;
    loop {
        let state = fold_biir_stmts_once(circuit.params, &mut circuit.stmts);
        let mut changed = state.changed;
        for output in &mut circuit.outputs {
            let canonical = canon_alias(&state.alias_map, *output);
            if canonical != *output {
                *output = canonical;
                changed = true;
            }
        }
        if !changed {
            break;
        }
        any_changed = true;
    }
    any_changed
}

// ============================================================================
// Internal helpers
// ============================================================================

/// One forward simplification pass over a single block.
///
/// Returns `true` if any stmt or terminator operand was changed.
fn fold_biir_block_once<P: Clone>(block: &mut BIrBlock<P>) -> bool {
    let state = fold_biir_stmts_once(block.params, &mut block.stmts);
    let mut changed = state.changed;

    // Rewrite terminator operands through alias_map.
    changed |= apply_aliases_to_biir_terminator(&mut block.terminator, &state.alias_map);

    // Dead branch removal: fold CondJmp when condition is a known boolean.
    changed |= fold_biir_terminator_dead_branch(&mut block.terminator, &state.bool_map);

    changed
}

struct FoldState {
    bool_map: BTreeMap<IRVarId, bool>,
    alias_map: BTreeMap<IRVarId, IRVarId>,
    changed: bool,
}

fn fold_biir_stmts_once<P: Clone>(
    params: u32,
    stmts: &mut [volar_ir_common::Node<BIrStmt, P>],
) -> FoldState {
    let mut bool_map: BTreeMap<IRVarId, bool> = BTreeMap::new();
    let mut alias_map: BTreeMap<IRVarId, IRVarId> = BTreeMap::new();
    let mut changed = false;

    let base = params;

    for i in 0..stmts.len() {
        let rv = IRVarId(base + i as u32);

        // Step 1: apply alias substitutions to this stmt's operands.
        if apply_aliases_to_biir_stmt(&mut stmts[i].kind, &alias_map) {
            changed = true;
        }

        // Step 2: read (now-updated) operands and compute the simplification action.
        let action = {
            match &stmts[i].kind {
                BIrStmt::Zero => {
                    bool_map.insert(rv, false);
                    None
                }
                BIrStmt::One => {
                    bool_map.insert(rv, true);
                    None
                }
                BIrStmt::And(a, b) => {
                    let ca = canon_alias(&alias_map, *a);
                    let cb = canon_alias(&alias_map, *b);
                    let va = bool_map.get(&ca).copied();
                    let vb = bool_map.get(&cb).copied();
                    match (va, vb) {
                        (Some(false), _) | (_, Some(false)) => Some(Action::ToConst(false)),
                        (Some(true), Some(true)) => Some(Action::ToConst(true)),
                        (Some(true), None) => Some(Action::ToAlias(cb)),
                        (None, Some(true)) => Some(Action::ToAlias(ca)),
                        _ if ca == cb => Some(Action::ToAlias(ca)),
                        _ => None,
                    }
                }
                BIrStmt::Or(a, b) => {
                    let ca = canon_alias(&alias_map, *a);
                    let cb = canon_alias(&alias_map, *b);
                    let va = bool_map.get(&ca).copied();
                    let vb = bool_map.get(&cb).copied();
                    match (va, vb) {
                        (Some(true), _) | (_, Some(true)) => Some(Action::ToConst(true)),
                        (Some(false), Some(false)) => Some(Action::ToConst(false)),
                        (Some(false), None) => Some(Action::ToAlias(cb)),
                        (None, Some(false)) => Some(Action::ToAlias(ca)),
                        _ if ca == cb => Some(Action::ToAlias(ca)),
                        _ => None,
                    }
                }
                BIrStmt::Xor(a, b) => {
                    let ca = canon_alias(&alias_map, *a);
                    let cb = canon_alias(&alias_map, *b);
                    let va = bool_map.get(&ca).copied();
                    let vb = bool_map.get(&cb).copied();
                    match (va, vb) {
                        (Some(va_), Some(vb_)) => Some(Action::ToConst(va_ ^ vb_)),
                        (Some(false), None) => Some(Action::ToAlias(cb)),
                        (None, Some(false)) => Some(Action::ToAlias(ca)),
                        _ if ca == cb => Some(Action::ToConst(false)),
                        _ => None,
                    }
                }
                BIrStmt::Not(a) => {
                    let ca = canon_alias(&alias_map, *a);
                    bool_map.get(&ca).copied().map(|v| Action::ToConst(!v))
                }
                // External / storage stmts: no simplification.
                _ => None,
            }
        };

        // Step 3: apply the action.
        match action {
            Some(Action::ToConst(val)) => {
                let new_stmt = if val { BIrStmt::One } else { BIrStmt::Zero };
                if stmts[i].kind != new_stmt {
                    stmts[i].kind = new_stmt;
                    changed = true;
                }
                bool_map.insert(rv, val);
            }
            Some(Action::ToAlias(target)) => {
                // Record the alias so downstream operands are rewritten.
                // Do NOT tombstone the stmt: correctness is maintained because
                // any use of rv in subsequent stmts/terminator is rewritten to
                // target via alias_map.  The stmt at rv still produces the
                // correct value; rv just becomes a dead definition.
                alias_map.insert(rv, target);
                if let Some(&val) = bool_map.get(&target) {
                    bool_map.insert(rv, val);
                }
                // changed is set to true when downstream operands are actually
                // rewritten (detected in the next iteration's step 1).
            }
            None => {}
        }
    }

    FoldState {
        bool_map,
        alias_map,
        changed,
    }
}

// ============================================================================
// Action enum
// ============================================================================

enum Action {
    ToConst(bool),
    ToAlias(IRVarId),
}

// ============================================================================
// Alias application for Boolar stmts and terminators
// ============================================================================

pub(crate) fn apply_aliases_to_biir_stmt(
    stmt: &mut BIrStmt,
    alias_map: &BTreeMap<IRVarId, IRVarId>,
) -> bool {
    if alias_map.is_empty() {
        return false;
    }
    let mut changed = false;

    match stmt {
        BIrStmt::And(a, b) | BIrStmt::Or(a, b) | BIrStmt::Xor(a, b) => {
            let ca = canon_alias(alias_map, *a);
            let cb = canon_alias(alias_map, *b);
            if ca != *a {
                *a = ca;
                changed = true;
            }
            if cb != *b {
                *b = cb;
                changed = true;
            }
        }
        BIrStmt::Not(a) => {
            let ca = canon_alias(alias_map, *a);
            if ca != *a {
                *a = ca;
                changed = true;
            }
        }
        BIrStmt::OracleCall { args, .. } => {
            for v in args.iter_mut() {
                let c = canon_alias(alias_map, *v);
                if c != *v {
                    *v = c;
                    changed = true;
                }
            }
        }
        BIrStmt::OracleBit { args, .. } => {
            for v in args.iter_mut() {
                let c = canon_alias(alias_map, *v);
                if c != *v {
                    *v = c;
                    changed = true;
                }
            }
        }
        BIrStmt::OracleProjectedBit { call, .. } => {
            let c = canon_alias(alias_map, *call);
            if c != *call {
                *call = c;
                changed = true;
            }
        }
        BIrStmt::ActionCall {
            guard,
            args,
            fallback,
            ..
        } => {
            let cg = canon_alias(alias_map, *guard);
            if cg != *guard {
                *guard = cg;
                changed = true;
            }
            for v in args.iter_mut() {
                let c = canon_alias(alias_map, *v);
                if c != *v {
                    *v = c;
                    changed = true;
                }
            }
            for v in fallback.iter_mut() {
                let c = canon_alias(alias_map, *v);
                if c != *v {
                    *v = c;
                    changed = true;
                }
            }
        }
        BIrStmt::ActionBit { call, .. } => {
            let c = canon_alias(alias_map, *call);
            if c != *call {
                *call = c;
                changed = true;
            }
        }
        BIrStmt::ActionStoreBit {
            guard,
            args,
            fallback,
            addr,
            ..
        } => {
            let c = canon_alias(alias_map, *guard);
            if c != *guard {
                *guard = c;
                changed = true;
            }
            for v in args
                .iter_mut()
                .chain(core::iter::once(fallback))
                .chain(addr.iter_mut())
            {
                let c = canon_alias(alias_map, *v);
                if c != *v {
                    *v = c;
                    changed = true;
                }
            }
        }
        BIrStmt::StorageRead { addr, .. } => {
            for v in addr.iter_mut() {
                let c = canon_alias(alias_map, *v);
                if c != *v {
                    *v = c;
                    changed = true;
                }
            }
        }
        BIrStmt::StorageWrite { src, addr, .. } => {
            let cs = canon_alias(alias_map, *src);
            if cs != *src {
                *src = cs;
                changed = true;
            }
            for v in addr.iter_mut() {
                let c = canon_alias(alias_map, *v);
                if c != *v {
                    *v = c;
                    changed = true;
                }
            }
        }
        // Zero, One, Rng: no var references.
        _ => {}
    }

    changed
}

pub(crate) fn apply_aliases_to_biir_target(
    target: &mut BIrTarget,
    alias_map: &BTreeMap<IRVarId, IRVarId>,
) -> bool {
    let mut changed = false;
    for v in target.args.iter_mut() {
        let c = canon_alias(alias_map, *v);
        if c != *v {
            *v = c;
            changed = true;
        }
    }
    if let IRBlockTargetId::Dyn(v) = &mut target.block {
        let c = canon_alias(alias_map, *v);
        if c != *v {
            *v = c;
            changed = true;
        }
    }
    changed
}

pub(crate) fn apply_aliases_to_biir_terminator(
    term: &mut BIrTerminator,
    alias_map: &BTreeMap<IRVarId, IRVarId>,
) -> bool {
    if alias_map.is_empty() {
        return false;
    }
    let mut changed = false;
    match term {
        BIrTerminator::Jmp(target) => {
            changed |= apply_aliases_to_biir_target(target, alias_map);
        }
        BIrTerminator::CondJmp {
            val,
            then_target,
            else_target,
        } => {
            let c = canon_alias(alias_map, *val);
            if c != *val {
                *val = c;
                changed = true;
            }
            changed |= apply_aliases_to_biir_target(then_target, alias_map);
            changed |= apply_aliases_to_biir_target(else_target, alias_map);
        }
        _ => {}
    }
    changed
}

// ============================================================================
// Dead branch removal
// ============================================================================

/// Fold `CondJmp` when the condition is a known boolean constant.
/// Returns `true` if the terminator was replaced.
fn fold_biir_terminator_dead_branch(
    term: &mut BIrTerminator,
    bool_map: &BTreeMap<IRVarId, bool>,
) -> bool {
    if let BIrTerminator::CondJmp {
        val,
        then_target,
        else_target,
    } = term
    {
        if let Some(&v) = bool_map.get(val) {
            let tgt = if v {
                then_target.clone()
            } else {
                else_target.clone()
            };
            *term = BIrTerminator::Jmp(tgt);
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use volar_ir_common::Node;

    fn node(stmt: BIrStmt) -> Node<BIrStmt> {
        Node::new(stmt, (), None)
    }

    #[test]
    fn folds_fused_circuit_constants_and_output_aliases() {
        let mut circuit = BCircuit {
            params: 1,
            stmts: vec![
                node(BIrStmt::Zero),
                node(BIrStmt::One),
                node(BIrStmt::And(IRVarId(0), IRVarId(1))),
                node(BIrStmt::Or(IRVarId(0), IRVarId(2))),
                node(BIrStmt::Xor(IRVarId(1), IRVarId(2))),
                node(BIrStmt::Not(IRVarId(1))),
                node(BIrStmt::And(IRVarId(3), IRVarId(4))),
                node(BIrStmt::And(IRVarId(0), IRVarId(2))),
                node(BIrStmt::Or(IRVarId(0), IRVarId(1))),
            ],
            pre_init: vec![],
            outputs: vec![
                IRVarId(3),
                IRVarId(4),
                IRVarId(5),
                IRVarId(6),
                IRVarId(7),
                IRVarId(8),
                IRVarId(9),
            ],
        };

        assert!(fold_biir_circuit(&mut circuit));
        assert!(!fold_biir_circuit(&mut circuit));
        assert_eq!(circuit.stmts[2].kind, BIrStmt::Zero);
        assert_eq!(circuit.stmts[3].kind, BIrStmt::One);
        assert_eq!(circuit.stmts[4].kind, BIrStmt::One);
        assert_eq!(circuit.stmts[5].kind, BIrStmt::One);
        assert_eq!(circuit.stmts[6].kind, BIrStmt::Zero);
        assert_eq!(
            circuit.outputs,
            vec![
                IRVarId(3),
                IRVarId(4),
                IRVarId(5),
                IRVarId(6),
                IRVarId(7),
                IRVarId(0),
                IRVarId(0),
            ]
        );
    }
}
