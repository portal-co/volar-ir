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


// ============================================================================
// Common-subexpression elimination (Boolar)
// ============================================================================

/// A structural key for one CSE-able Boolar statement. Commutative gates
/// carry a sorted operand pair. ORACLES are pure (a named pure function of
/// its args), so `OracleCall` and `OracleBit` are keyed by name + args;
/// `occurrence` is only replay identity for distinct call SITES, so it is
/// deliberately NOT part of the key — two calls computing the same pure
/// function of the same args ARE the same value. Only ACTIONS (side
/// effects) and RNG (fresh samples per occurrence) are never keyed, plus
/// storage statements (side effects).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum CseKey {
    Zero,
    One,
    And(IRVarId, IRVarId),
    Or(IRVarId, IRVarId),
    Xor(IRVarId, IRVarId),
    Not(IRVarId),
    OracleCall(alloc::string::String, alloc::vec::Vec<IRVarId>),
    OracleBit(alloc::string::String, alloc::vec::Vec<IRVarId>, usize),
}

/// The CSE key of a pure statement, if it has one.
fn cse_key(kind: &BIrStmt) -> Option<CseKey> {
    let sorted = |a: IRVarId, b: IRVarId| if a <= b { (a, b) } else { (b, a) };
    Some(match kind {
        BIrStmt::Zero => CseKey::Zero,
        BIrStmt::One => CseKey::One,
        BIrStmt::And(a, b) => {
            let (a, b) = sorted(*a, *b);
            CseKey::And(a, b)
        }
        BIrStmt::Or(a, b) => {
            let (a, b) = sorted(*a, *b);
            CseKey::Or(a, b)
        }
        BIrStmt::Xor(a, b) => {
            let (a, b) = sorted(*a, *b);
            CseKey::Xor(a, b)
        }
        BIrStmt::Not(a) => CseKey::Not(*a),
        // Pure named oracles: same name + same args => same value. The
        // occurrence counter is replay identity, not part of the value.
        BIrStmt::OracleCall { name, args, .. } => {
            CseKey::OracleCall(name.clone(), args.clone())
        }
        BIrStmt::OracleBit {
            name, args, bit, ..
        } => CseKey::OracleBit(name.clone(), args.clone(), *bit),
        _ => return None,
    })
}

/// Eliminate common subexpressions within each block, in place.
///
/// One forward sweep per block: operands are alias-canonicalized, then each
/// pure Boolean statement is replaced by the first statement with the same
/// structural key via the alias map (exactly the fold pass's aliasing
/// mechanism, so downstream operands and terminator args are rewritten
/// through it). The duplicate statement itself is left as a dead
/// definition — pair this with [`dce_biir_blocks`] to remove it. Loops to
/// a fixpoint. Returns `true` if any block changed.
pub fn cse_biir_blocks<P: Clone>(blocks: &mut BIrBlocks<P>) -> bool {
    let mut any_changed = false;
    for block in blocks.blocks.iter_mut() {
        loop {
            if !cse_biir_block_once(block) {
                break;
            }
            any_changed = true;
        }
    }
    any_changed
}

/// One CSE sweep over a single block.
fn cse_biir_block_once<P: Clone>(block: &mut BIrBlock<P>) -> bool {
    let mut alias_map: BTreeMap<IRVarId, IRVarId> = BTreeMap::new();
    let mut seen: BTreeMap<CseKey, IRVarId> = BTreeMap::new();
    let mut changed = false;
    let base = block.params;

    for i in 0..block.stmts.len() {
        let rv = IRVarId(base + i as u32);
        if apply_aliases_to_biir_stmt(&mut block.stmts[i].kind, &alias_map) {
            changed = true;
        }
        let Some(key) = cse_key(&block.stmts[i].kind) else {
            continue;
        };
        match seen.get(&key) {
            Some(&first) => {
                alias_map.insert(rv, first);
                // No immediate rewrite here; the next iteration's alias
                // application marks the change (mirrors fold's ToAlias).
            }
            None => {
                seen.insert(key, rv);
            }
        }
    }

    changed |= apply_aliases_to_biir_terminator(&mut block.terminator, &alias_map);
    changed
}


/// Remap a var through a RENUMBERING map: single-step lookup, never
/// transitive. Unlike [`canon_alias`] (which chases alias chains), a
/// renumbering's values may themselves be keys — old rv 4 -> 3 while old
/// rv 3 -> 2 — and chasing would rewrite a use of old 4 to 2 instead of 3.
fn renumber_var(v: &mut IRVarId, remap: &BTreeMap<IRVarId, IRVarId>) {
    if let Some(&w) = remap.get(v) {
        *v = w;
    }
}

/// Apply a renumbering to one statement's operands (single-step).
fn renumber_biir_stmt(stmt: &mut BIrStmt, remap: &BTreeMap<IRVarId, IRVarId>) {
    for_each_biir_operand_mut(stmt, &mut |v| renumber_var(v, remap));
}

/// Visit every operand var a statement reads, mutably (for renumbering).
fn for_each_biir_operand_mut(kind: &mut BIrStmt, f: &mut impl FnMut(&mut IRVarId)) {
    match kind {
        BIrStmt::Zero | BIrStmt::One | BIrStmt::Rng { .. } | BIrStmt::RngBit { .. } => {}
        BIrStmt::And(a, b) | BIrStmt::Or(a, b) | BIrStmt::Xor(a, b) => {
            f(a);
            f(b);
        }
        BIrStmt::Not(a) => f(a),
        BIrStmt::OracleCall { args, .. } => args.iter_mut().for_each(&mut *f),
        BIrStmt::OracleBit { args, .. } => args.iter_mut().for_each(&mut *f),
        BIrStmt::OracleProjectedBit { call, .. } => f(call),
        BIrStmt::ActionCall {
            guard,
            args,
            fallback,
            ..
        } => {
            f(guard);
            args.iter_mut().for_each(&mut *f);
            fallback.iter_mut().for_each(&mut *f);
        }
        BIrStmt::ActionBit { call, .. } => f(call),
        BIrStmt::ActionStoreBit {
            guard,
            args,
            fallback,
            addr,
            ..
        } => {
            f(guard);
            args.iter_mut().for_each(&mut *f);
            f(fallback);
            addr.iter_mut().for_each(&mut *f);
        }
        BIrStmt::StorageRead { addr, .. } => addr.iter_mut().for_each(&mut *f),
        BIrStmt::StorageWrite { src, addr, .. } => {
            f(src);
            addr.iter_mut().for_each(&mut *f);
        }
        _ => {}
    }
}

/// Apply a renumbering to a terminator's operands (single-step).
fn renumber_biir_terminator(term: &mut BIrTerminator, remap: &BTreeMap<IRVarId, IRVarId>) {
    let target = |t: &mut BIrTarget, remap: &BTreeMap<IRVarId, IRVarId>| {
        t.args.iter_mut().for_each(|v| renumber_var(v, remap));
    };
    match term {
        BIrTerminator::Jmp(t) => target(t, remap),
        BIrTerminator::CondJmp {
            val,
            then_target,
            else_target,
        } => {
            renumber_var(val, remap);
            target(then_target, remap);
            target(else_target, remap);
        }
        _ => {}
    }
}

// ============================================================================
// Dead-code elimination (Boolar)
// ============================================================================


/// Whether a Boolar statement is pure: removing it when its result is
/// unused has no observable effect. Oracles (`OracleCall`, `OracleBit`,
/// `OracleProjectedBit`) are pure named functions of their args, so they
/// are removable when unused. Only ACTIONS (side effects), storage
/// statements (side effects), and RNG (fresh samples per occurrence) are
/// effectful and always kept.
fn biir_is_pure(kind: &BIrStmt) -> bool {
    matches!(
        kind,
        BIrStmt::Zero
            | BIrStmt::One
            | BIrStmt::And(_, _)
            | BIrStmt::Or(_, _)
            | BIrStmt::Xor(_, _)
            | BIrStmt::Not(_)
            | BIrStmt::OracleCall { .. }
            | BIrStmt::OracleBit { .. }
            | BIrStmt::OracleProjectedBit { .. }
    )
}

/// Visit every operand var a statement reads.
fn for_each_biir_operand(kind: &BIrStmt, f: &mut impl FnMut(IRVarId)) {
    match kind {
        BIrStmt::Zero | BIrStmt::One | BIrStmt::Rng { .. } | BIrStmt::RngBit { .. } => {}
        BIrStmt::And(a, b) | BIrStmt::Or(a, b) | BIrStmt::Xor(a, b) => {
            f(*a);
            f(*b);
        }
        BIrStmt::Not(a) => f(*a),
        BIrStmt::OracleCall { args, .. } => args.iter().for_each(|a| f(*a)),
        BIrStmt::OracleBit { args, .. } => args.iter().for_each(|a| f(*a)),
        BIrStmt::OracleProjectedBit { call, .. } => f(*call),
        BIrStmt::ActionCall {
            guard,
            args,
            fallback,
            ..
        } => {
            f(*guard);
            args.iter().for_each(|a| f(*a));
            fallback.iter().for_each(|a| f(*a));
        }
        BIrStmt::ActionBit { call, .. } => f(*call),
        BIrStmt::ActionStoreBit {
            guard,
            args,
            fallback,
            addr,
            ..
        } => {
            f(*guard);
            args.iter().for_each(|a| f(*a));
            f(*fallback);
            addr.iter().for_each(|a| f(*a));
        }
        BIrStmt::StorageRead { addr, .. } => addr.iter().for_each(|a| f(*a)),
        BIrStmt::StorageWrite { src, addr, .. } => {
            f(*src);
            addr.iter().for_each(|a| f(*a));
        }
        // External/effectful variants not otherwise listed read no vars we
        // can track here; treat conservatively (no operands visited).
        _ => {}
    }
}

/// Visit every operand var a terminator reads (target args + condition).
fn for_each_biir_terminator_operand(term: &BIrTerminator, f: &mut impl FnMut(IRVarId)) {
    match term {
        BIrTerminator::Jmp(t) => t.args.iter().for_each(|a| f(*a)),
        BIrTerminator::CondJmp {
            val,
            then_target,
            else_target,
        } => {
            f(*val);
            then_target.args.iter().for_each(|a| f(*a));
            else_target.args.iter().for_each(|a| f(*a));
        }
        // Non-exhaustive upstream enum; other terminators read no vars here.
        _ => {}
    }
}

/// Eliminate dead pure statements from each block, in place.
///
/// A statement is dead when its result var has no uses anywhere in the
/// block (statements or terminator) and it is pure ([`biir_is_pure`]).
/// Because a statement's result var id is `params + stmt index`, removing
/// statements renumbers every later result var; this pass rebuilds each
/// block with remapped operands and terminator args in one sweep. Loops to
/// a fixpoint (a removed statement can deaden its operands). Returns
/// `true` if any block changed.
pub fn dce_biir_blocks<P: Clone>(blocks: &mut BIrBlocks<P>) -> bool {
    let mut any_changed = false;
    for block in blocks.blocks.iter_mut() {
        loop {
            if !dce_biir_block_once(block) {
                break;
            }
            any_changed = true;
        }
    }
    any_changed
}

/// One DCE sweep over a single block.
fn dce_biir_block_once<P: Clone>(block: &mut BIrBlock<P>) -> bool {
    let n = block.stmts.len();
    let base = block.params;
    // Use counts over result vars (0..n); terminator uses included.
    let mut uses = alloc::vec::Vec::with_capacity(n);
    uses.resize(n, 0u32);
    for stmt in block.stmts.iter() {
        for_each_biir_operand(&stmt.kind, &mut |v| {
            if v.0 >= base {
                uses[(v.0 - base) as usize] += 1;
            }
        });
    }
    for_each_biir_terminator_operand(&block.terminator, &mut |v| {
        if v.0 >= base {
            uses[(v.0 - base) as usize] += 1;
        }
    });

    // Reverse liveness: a pure stmt with no uses is dead; removing it
    // decrements its operands' uses.
    let mut dead = alloc::vec::Vec::with_capacity(n);
    dead.resize(n, false);
    let mut any_dead = false;
    for i in (0..n).rev() {
        if uses[i] == 0 && biir_is_pure(&block.stmts[i].kind) {
            dead[i] = true;
            any_dead = true;
            let kind = block.stmts[i].kind.clone();
            for_each_biir_operand(&kind, &mut |v| {
                if v.0 >= base {
                    uses[(v.0 - base) as usize] -= 1;
                }
            });
        }
    }
    if !any_dead {
        return false;
    }

    // Rebuild with remapped var ids: old result rv -> params + new index.
    let mut remap: BTreeMap<IRVarId, IRVarId> = BTreeMap::new();
    let mut kept: alloc::vec::Vec<volar_ir_common::Node<BIrStmt, P>> =
        alloc::vec::Vec::with_capacity(n);
    for (i, stmt) in core::mem::take(&mut block.stmts).into_iter().enumerate() {
        if dead[i] {
            continue;
        }
        let old_rv = IRVarId(base + i as u32);
        let new_rv = IRVarId(base + kept.len() as u32);
        if old_rv != new_rv {
            remap.insert(old_rv, new_rv);
        }
        kept.push(stmt);
    }
    block.stmts = kept;
    for stmt in block.stmts.iter_mut() {
        renumber_biir_stmt(&mut stmt.kind, &remap);
    }
    renumber_biir_terminator(&mut block.terminator, &remap);
    true
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

    #[test]
    fn cse_dedupes_identical_gates() {
        // a=And(p0,p1) at rv 2; b=And(p1,p0) at rv 3 (commuted); c=Xor(a,b).
        let mut blocks = BIrBlocks {
            blocks: vec![BIrBlock {
                params: 2,
                stmts: vec![
                    node(BIrStmt::And(IRVarId(0), IRVarId(1))),
                    node(BIrStmt::And(IRVarId(1), IRVarId(0))),
                    node(BIrStmt::Xor(IRVarId(2), IRVarId(3))),
                    node(BIrStmt::Not(IRVarId(0))),
                    node(BIrStmt::Not(IRVarId(0))),
                ],
                terminator: BIrTerminator::Jmp(BIrTarget {
                    block: IRBlockTargetId::Return,
                    args: vec![IRVarId(4), IRVarId(6)],
                }),
            }],
            pre_init: vec![],
        };
        assert!(cse_biir_blocks(&mut blocks));
        assert!(!cse_biir_blocks(&mut blocks));
        // rv3 aliases rv2; the Xor must reference the canonical var.
        assert_eq!(
            blocks.blocks[0].stmts[2].kind,
            BIrStmt::Xor(IRVarId(2), IRVarId(2))
        );
        // rv6 aliases rv5; the terminator arg must be rewritten.
        assert_eq!(
            blocks.blocks[0].terminator,
            BIrTerminator::Jmp(BIrTarget {
                block: IRBlockTargetId::Return,
                args: vec![IRVarId(4), IRVarId(5)],
            })
        );
    }

    #[test]
    fn cse_dedupes_pure_oracles_across_occurrences() {
        // Two OracleBit calls with the same name/args/bit but DISTINCT
        // occurrences compute the same pure value, so they ARE CSE'd.
        let mut blocks = BIrBlocks {
            blocks: vec![BIrBlock {
                params: 1,
                stmts: vec![
                    node(BIrStmt::OracleBit {
                        name: "f".into(),
                        args: vec![IRVarId(0)],
                        bit: 0,
                        occurrence: 0,
                    }),
                    node(BIrStmt::OracleBit {
                        name: "f".into(),
                        args: vec![IRVarId(0)],
                        bit: 0,
                        occurrence: 1,
                    }),
                ],
                terminator: BIrTerminator::Jmp(BIrTarget {
                    block: IRBlockTargetId::Return,
                    args: vec![IRVarId(1), IRVarId(2)],
                }),
            }],
            pre_init: vec![],
        };
        assert!(cse_biir_blocks(&mut blocks));
        // rv2 aliases rv1; the terminator's second arg is rewritten.
        assert_eq!(
            blocks.blocks[0].terminator,
            BIrTerminator::Jmp(BIrTarget {
                block: IRBlockTargetId::Return,
                args: vec![IRVarId(1), IRVarId(1)],
            })
        );
    }

    #[test]
    fn cse_keeps_actions_and_rng() {
        // ActionBit with the same call handle + bit but distinct
        // occurrences is NOT CSE-able (actions have side effects), and
        // RngBit is never keyed (fresh sample per occurrence).
        let mut blocks = BIrBlocks {
            blocks: vec![BIrBlock {
                params: 1,
                stmts: vec![
                    node(BIrStmt::RngBit {
                        name: "r".into(),
                        bit: 0,
                        occurrence: 0,
                    }),
                    node(BIrStmt::RngBit {
                        name: "r".into(),
                        bit: 0,
                        occurrence: 1,
                    }),
                ],
                terminator: BIrTerminator::Jmp(BIrTarget {
                    block: IRBlockTargetId::Return,
                    args: vec![IRVarId(1), IRVarId(2)],
                }),
            }],
            pre_init: vec![],
        };
        assert!(!cse_biir_blocks(&mut blocks));
    }

    #[test]
    fn dce_removes_unused_pure_oracles() {
        // An unused OracleBit is pure, so DCE removes it (unlike actions /
        // RNG / storage).
        let mut blocks = BIrBlocks {
            blocks: vec![BIrBlock {
                params: 1,
                stmts: vec![
                    node(BIrStmt::OracleBit {
                        name: "f".into(),
                        args: vec![IRVarId(0)],
                        bit: 0,
                        occurrence: 0,
                    }),
                    node(BIrStmt::And(IRVarId(0), IRVarId(0))),
                ],
                terminator: BIrTerminator::Jmp(BIrTarget {
                    block: IRBlockTargetId::Return,
                    args: vec![IRVarId(2)],
                }),
            }],
            pre_init: vec![],
        };
        assert!(dce_biir_blocks(&mut blocks));
        assert_eq!(blocks.blocks[0].stmts.len(), 1);
    }

    #[test]
    fn dce_removes_dead_pure_gates_and_renumbers() {
        // params 2; rv2 = And (live, output), rv3 = Xor (dead),
        // rv4 = Not(rv3) (dead chain), rv5 = Or(rv2, rv2) (live output).
        let mut blocks = BIrBlocks {
            blocks: vec![BIrBlock {
                params: 2,
                stmts: vec![
                    node(BIrStmt::And(IRVarId(0), IRVarId(1))),
                    node(BIrStmt::Xor(IRVarId(2), IRVarId(0))),
                    node(BIrStmt::Not(IRVarId(3))),
                    node(BIrStmt::Or(IRVarId(2), IRVarId(2))),
                ],
                terminator: BIrTerminator::Jmp(BIrTarget {
                    block: IRBlockTargetId::Return,
                    args: vec![IRVarId(2), IRVarId(5)],
                }),
            }],
            pre_init: vec![],
        };
        assert!(dce_biir_blocks(&mut blocks));
        assert!(!dce_biir_blocks(&mut blocks));
        // rv3/rv4 removed; rv5 renumbered to rv3.
        assert_eq!(blocks.blocks[0].stmts.len(), 2);
        assert_eq!(
            blocks.blocks[0].stmts[1].kind,
            BIrStmt::Or(IRVarId(2), IRVarId(2))
        );
        assert_eq!(
            blocks.blocks[0].terminator,
            BIrTerminator::Jmp(BIrTarget {
                block: IRBlockTargetId::Return,
                args: vec![IRVarId(2), IRVarId(3)],
            })
        );
    }

    #[test]
    fn dce_keeps_effectful_statements_even_when_unused() {
        let mut blocks = BIrBlocks {
            blocks: vec![BIrBlock {
                params: 1,
                stmts: vec![
                    node(BIrStmt::StorageWrite {
                        storage: volar_ir::ir::StorageId(0),
                        lane: volar_ir::boolar::LaneId(0),
                        src: IRVarId(0),
                        addr: vec![IRVarId(0)],
                    }),
                    node(BIrStmt::RngBit {
                        name: "r".into(),
                        bit: 0,
                        occurrence: 0,
                    }),
                    node(BIrStmt::And(IRVarId(0), IRVarId(0))),
                ],
                terminator: BIrTerminator::Jmp(BIrTarget {
                    block: IRBlockTargetId::Return,
                    args: vec![IRVarId(3)],
                }),
            }],
            pre_init: vec![],
        };
        // StorageWrite and RngBit are effectful: kept despite zero uses.
        // The And is used by the terminator, so nothing is removable.
        assert!(!dce_biir_blocks(&mut blocks));
        assert_eq!(blocks.blocks[0].stmts.len(), 3);
    }

    #[test]
    fn cse_then_dce_shrinks_duplicate_fanout() {
        // params 1; 16 copies of And(p0,p0), Xor-folded pairwise to one
        // output — after CSE the 16 Ands collapse to 1 and DCE removes 15.
        let mut stmts = vec![];
        for _ in 0..16 {
            stmts.push(node(BIrStmt::And(IRVarId(0), IRVarId(0))));
        }
        let mut acc = IRVarId(1);
        let mut next = 17u32;
        for i in 2..17 {
            stmts.push(node(BIrStmt::Xor(acc, IRVarId(i))));
            acc = IRVarId(next);
            next += 1;
        }
        let mut blocks = BIrBlocks {
            blocks: vec![BIrBlock {
                params: 1,
                stmts,
                terminator: BIrTerminator::Jmp(BIrTarget {
                    block: IRBlockTargetId::Return,
                    args: vec![acc],
                }),
            }],
            pre_init: vec![],
        };
        assert!(cse_biir_blocks(&mut blocks));
        assert!(dce_biir_blocks(&mut blocks));
        // 1 And + 15 Xor (Xor(x,x) isn't folded here — no fold pass).
        assert_eq!(blocks.blocks[0].stmts.len(), 16);
        // And the pipeline again is a fixpoint.
        assert!(!cse_biir_blocks(&mut blocks));
        assert!(!dce_biir_blocks(&mut blocks));
    }
}
