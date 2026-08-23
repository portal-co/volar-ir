// @reliability: normal
//! @ai: assisted
//! Transform: circuit-fused Boolar (`BCircuit`) → reversible circuit
//! ([`RCircuit`]), implementing the naive Bennett-style embedding
//!
//! ```text
//! (x, y) ↦ (x, y ⊕ f(x))
//! ```
//!
//! for a Boolar circuit `f`. This is the **naive** scheme: internal wires are
//! fresh ancillas initialized to 0 and left as garbage at the end — no Bennett
//! uncomputation. The `x` register is provably untouched and the `y` register
//! accumulates `f(x)` via CNOTs.
//!
//! # Wire layout
//!
//! ```text
//! [ x register (params) | per-stmt result ancillas | y register (outputs) | scratch ]
//! ```
//!
//! [`VarWireMap`] — the total, injective map from Boolar var space to wire
//! indices — is a byproduct of allocation and is what consumers use for
//! value→wire watchlist translation.
//!
//! # Gate cost
//!
//! Per statement: `Xor` → 2 CNOT, `And` → 1 Toffoli, `Not` → 1 CNOT + 1 X,
//! `Or` → 2 CNOT + 2 X + 1 Toffoli + 1 X (De Morgan on copied-not operands).
//! Output phase adds one CNOT per output bit.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;
use volar_ir::boolar::BIrStmt;
use volar_ir::circuit::BCircuit;
use volar_ir::ir::IRVarId;
use volar_ir::rcircuit::{RCircuit, RGate};

/// Why a Boolar circuit could not be converted to a reversible circuit.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ToReversibleError {
    /// External primitives have no reversible-gate realization in the naive
    /// scheme. Note `StorageRead`/`StorageWrite` are rejected here even
    /// though `RCircuit` can *represent* storage ops (`RGate::StorageSwap`) —
    /// synthesizing reversible storage access from Boolar storage traffic is
    /// future work.
    UnsupportedStmt {
        /// Var id of the offending statement result.
        var: u32,
    },
    /// An operand was defined later in the statement list than its user.
    /// Synthesis relies on SSA order within the single fused block.
    UseBeforeDef {
        /// Var id of the using statement's result.
        user: u32,
        /// Var id of the operand defined too late.
        operand: u32,
    },
}

impl core::fmt::Display for ToReversibleError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ToReversibleError::UnsupportedStmt { var } => {
                write!(f, "stmt var {var} is an external primitive with no reversible lowering")
            }
            ToReversibleError::UseBeforeDef { user, operand } => {
                write!(f, "stmt var {user} uses var {operand} before it is defined")
            }
        }
    }
}

/// Total, injective map from a `BCircuit`'s var space (params followed by stmt
/// results) to `RCircuit` wire indices.
///
/// Produced by the same [`to_reversible`] run that emits the [`RCircuit`] —
/// never reconstructed after the fact, so it cannot drift from the allocator's
/// layout.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct VarWireMap {
    /// `map[var_id] = wire_index`; total over `0..var_space`.
    map: Vec<usize>,
    /// Total wires in the produced circuit (including scratch).
    num_wires: usize,
}

impl VarWireMap {
    /// Wire index carrying the value of Boolar var `v`.
    pub fn wire(&self, v: IRVarId) -> Option<usize> {
        self.map.get(v.0 as usize).copied()
    }

    /// Total wires in the produced reversible circuit.
    pub fn num_wires(&self) -> usize {
        self.num_wires
    }

    /// Number of mapped vars (params + stmt results).
    pub fn var_space(&self) -> u32 {
        self.map.len() as u32
    }
}

/// A user-provided watchlist over the Boolar **value space**: a set of var ids
/// (params or statement results) whose values a consumer wants to track
/// through the reversible circuit.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ValueWatchlist {
    pub vars: BTreeSet<u32>,
}

impl ValueWatchlist {
    pub fn from_vars(vars: impl IntoIterator<Item = u32>) -> Self {
        ValueWatchlist { vars: vars.into_iter().collect() }
    }
}

/// A watchlist over the reversible circuit's **wire space**: sorted,
/// deduplicated wire indices, each annotated with the var id it came from.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WireWatchlist {
    pub entries: Vec<WireWatchEntry>,
}

/// One translated watchlist entry.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WireWatchEntry {
    pub wire: usize,
    pub var: u32,
}

/// Why watchlist translation failed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct UnknownVar {
    pub var: u32,
}

/// Translate a value-space watchlist into a wire-space watchlist.
///
/// Fails closed on any var id outside the mapped var space; it never silently
/// drops entries. Duplicate ids collapse to one entry (wires are unique per
/// var).
pub fn translate_watchlist(
    watchlist: &ValueWatchlist,
    map: &VarWireMap,
) -> Result<WireWatchlist, UnknownVar> {
    let mut entries: Vec<WireWatchEntry> = Vec::with_capacity(watchlist.vars.len());
    for &var in &watchlist.vars {
        let wire = map
            .wire(IRVarId(var))
            .ok_or(UnknownVar { var })?;
        entries.push(WireWatchEntry { wire, var });
    }
    entries.sort_by_key(|e| e.wire);
    Ok(WireWatchlist { entries })
}

// ============================================================================
// The transform
// ============================================================================
/// Wire index of Boolar var `v`: by construction the layout is
/// `[params | stmt-ancillas | y | scratch]`, so every mapped var's wire index
/// equals its var id.
fn wire_of(v: &IRVarId) -> usize {
    v.0 as usize
}


/// Convert a circuit-fused Boolar program computing `y_out = f(x_in)` into a
/// reversible circuit over `(x, y)` implementing `(x, y) ↦ (x, y ⊕ f(x))`,
/// plus the var→wire map.
///
/// Returns `(RCircuit, VarWireMap)`. The y-register wires are the last
/// `circ.outputs.len()` wires below any scratch; use the returned map plus
/// [`translate_watchlist`] to locate them symbolically.
pub fn to_reversible(circ: &BCircuit) -> Result<(RCircuit, VarWireMap), ToReversibleError> {
    // ---- Wire allocation ---------------------------------------------------
    // x register: one wire per param bit (wire i carries param i).
    // stmt ancillas: one zero-initialized wire per stmt result var.
    let params = circ.params;
    let n_stmts = circ.stmts.len();
    let mut num_wires = params as usize + n_stmts;

    let mut map_vec: Vec<usize> = Vec::with_capacity(params as usize + n_stmts);
    for w in 0..params as usize {
        map_vec.push(w);
    }

    // y register comes after the stmt ancillas so scratch can grow past it.
    let y_base = num_wires;
    num_wires += circ.outputs.len();

    let mut gates: Vec<RGate> = Vec::new();

    // Scratch allocator (appended after the y register).
    let mut next_scratch = num_wires;


    // SSA order check + synthesis per statement.
    for (i, node) in circ.stmts.iter().enumerate() {
        let r = IRVarId(params + i as u32); // this stmt's result var
        // Stmt i's ancilla is its own dedicated zero-initialized wire.
        let wr = params as usize + i;
        let check_operand = |v: IRVarId| -> Result<(), ToReversibleError> {
            if v.0 > r.0 {
                Err(ToReversibleError::UseBeforeDef { user: r.0, operand: v.0 })
            } else if v.0 >= params + i as u32 && v.0 != r.0 {
                // Operand is a *later* stmt's result.
                Err(ToReversibleError::UseBeforeDef { user: r.0, operand: v.0 })
            } else {
                Ok(())
            }
        };
        match &node.kind {
            BIrStmt::Zero => {}
            BIrStmt::One => gates.push(RGate::X(wr)),
            BIrStmt::Xor(a, b) => {
                check_operand(*a)?;
                check_operand(*b)?;
                // w_r starts 0; XOR-copy both operands into it.
                gates.push(RGate::Cnot { ctrl: wire_of(a), target: wr });
                gates.push(RGate::Cnot { ctrl: wire_of(b), target: wr });
            }
            BIrStmt::And(a, b) => {
                check_operand(*a)?;
                check_operand(*b)?;
                gates.push(RGate::Ccnot { c1: wire_of(a), c2: wire_of(b), target: wr });
            }
            BIrStmt::Not(a) => {
                check_operand(*a)?;
                // Copy-then-invert: never invert an input wire (would break
                // the `(x, ·) ↦ (x, ·)` contract).
                gates.push(RGate::Cnot { ctrl: wire_of(a), target: wr });
                gates.push(RGate::X(wr));
            }
            BIrStmt::Or(a, b) => {
                check_operand(*a)?;
                check_operand(*b)?;
                // De Morgan on copied-not operands: res := ¬(¬a ∧ ¬b).
                let na = next_scratch;
                next_scratch += 1;
                let nb = next_scratch;
                next_scratch += 1;
                gates.push(RGate::Cnot { ctrl: wire_of(a), target: na });
                gates.push(RGate::X(na));
                gates.push(RGate::Cnot { ctrl: wire_of(b), target: nb });
                gates.push(RGate::X(nb));
                gates.push(RGate::Ccnot { c1: na, c2: nb, target: wr });
                gates.push(RGate::X(wr));
            }
            // Catch-all over the `#[non_exhaustive]` statement enum: every
            // external primitive falls here with no reversible lowering.
            _ => {
                return Err(ToReversibleError::UnsupportedStmt { var: r.0 });
            }
        }
        // Record this stmt's wire only after validation of the whole stmt.
    }

    // Fill the map for all stmt vars (params already recorded).
    debug_assert_eq!(map_vec.len(), params as usize);
    for i in 0..n_stmts {
        map_vec.push(params as usize + i);
    }

    // ---- Output phase: y_i ^= out_i ----------------------------------------
    for (k, out) in circ.outputs.iter().enumerate() {
        gates.push(RGate::Cnot { ctrl: wire_of(out), target: y_base + k });
    }

    let num_wires_total = next_scratch.max(num_wires);
    let circuit = RCircuit::new(num_wires_total, gates).expect(
        "to_reversible synthesis produces validated gates by construction",
    );
    Ok((
        circuit,
        VarWireMap { map: map_vec, num_wires: num_wires_total },
    ))
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;
    use volar_ir::circuit::BCircuit;

    /// Evaluate a pure-gate `BCircuit` (Zero/One/And/Or/Xor/Not only) on the
    /// given parameter bits; returns the full var-space values.
    fn eval_bir(circ: &BCircuit, params: &[bool]) -> Vec<bool> {
        let mut vals = vec![false; circ.var_space() as usize];
        vals[..params.len()].copy_from_slice(params);
        for (i, node) in circ.stmts.iter().enumerate() {
            let v = params.len() + i;
            vals[v] = match &node.kind {
                BIrStmt::Zero => false,
                BIrStmt::One => true,
                BIrStmt::And(a, b) => vals[a.0 as usize] & vals[b.0 as usize],
                BIrStmt::Or(a, b) => vals[a.0 as usize] | vals[b.0 as usize],
                BIrStmt::Xor(a, b) => vals[a.0 as usize] ^ vals[b.0 as usize],
                BIrStmt::Not(a) => !vals[a.0 as usize],
                _ => panic!("eval_bir only supports pure boolean gates"),
            };
        }
        vals
    }

    fn build(circ: &mut BCircuit, kind: BIrStmt) -> IRVarId {
        circ.push_stmt(kind, ())
    }

    /// Full semantics property: applying the reversible circuit to
    /// (x, y, zeros…) yields (x, y ⊕ f(x), garbage…), for every input pair
    /// over the given bit width.
    fn check_semantics(circ: &BCircuit) {
        let (rc, map) = to_reversible(circ).expect("converts");
        let n_params = circ.params as usize;
        let n_out = circ.outputs.len();
        assert_eq!(map.var_space(), circ.var_space());
        assert!(map.num_wires() <= rc.num_wires);

        for x in 0u32..(1u32 << n_params.min(8)) {
            let px: Vec<bool> = (0..n_params).map(|i| (x >> i) & 1 == 1).collect();
            let f = {
                let vals = eval_bir(circ, &px);
                circ.outputs.iter().map(|o| vals[o.0 as usize]).collect::<Vec<_>>()
            };
            // Wire layout mirrors the transform:
            // [x ‖ stmt-ancillas ‖ y-register ‖ scratch].
            let y_base = n_params + circ.stmts.len();
            for ymask in 0..(1u32 << n_out.min(6)) {
                let py: Vec<bool> =
                    (0..n_out).map(|i| (ymask >> i) & 1 == 1).collect();
                let mut wires = vec![false; rc.num_wires];
                for (i, b) in px.iter().enumerate() {
                    wires[map.wire(IRVarId(i as u32)).unwrap()] = *b;
                }
                for (k, b) in py.iter().enumerate() {
                    wires[y_base + k] = *b;
                }
                rc.apply_pure(&mut wires).expect("pure circuit");
                // x untouched:
                for (i, b) in px.iter().enumerate() {
                    assert_eq!(
                        wires[map.wire(IRVarId(i as u32)).unwrap()],
                        *b,
                        "x wire {i} disturbed"
                    );
                }
                // y ^= f(x):
                for (k, b) in f.iter().enumerate() {
                    let expected = py[k] ^ b;
                    assert_eq!(wires[y_base + k], expected, "x={x:?} y={py:?}");
                }
            }
        }
    }

    #[test]
    fn identity_function() {
        // f(x0,x1) = (x0, x1): just pass-through outputs.
        let mut circ = BCircuit::new(2);
        circ.outputs = vec![IRVarId(0), IRVarId(1)];
        check_semantics(&circ);
    }

    #[test]
    fn xor_and_or_not_gates() {
        // f(x0,x1) = (¬x0, x0|x1, x0^x1)
        let mut circ = BCircuit::new(2);
        let a = build(&mut circ, BIrStmt::Not(IRVarId(0)));
        let o = build(&mut circ, BIrStmt::Or(IRVarId(0), IRVarId(1)));
        let x = build(&mut circ, BIrStmt::Xor(IRVarId(0), IRVarId(1)));
        let _and = build(&mut circ, BIrStmt::And(x, o));
        circ.outputs = vec![a, o, x];
        check_semantics(&circ);
    }

    #[test]
    fn constants_and_chained_ands() {
        // f() over 3 params with constants mixed in.
        let mut circ = BCircuit::new(3);
        let z = build(&mut circ, BIrStmt::Zero);
        let one = build(&mut circ, BIrStmt::One);
        let a1 = build(&mut circ, BIrStmt::And(IRVarId(0), IRVarId(1)));
        let a2 = build(&mut circ, BIrStmt::And(a1, IRVarId(2)));
        let a3 = build(&mut circ, BIrStmt::Xor(one, a2));
        let a4 = build(&mut circ, BIrStmt::Xor(z, a3));
        circ.outputs = vec![z, a2, a3, a4];
        check_semantics(&circ);
    }

    #[test]
    fn inverse_restores_all_wires() {
        let mut circ = BCircuit::new(2);
        let o = build(&mut circ, BIrStmt::Or(IRVarId(0), IRVarId(1)));
        let a = build(&mut circ, BIrStmt::And(o, IRVarId(1)));
        circ.outputs = vec![a];
        let (rc, _map) = to_reversible(&circ).expect("converts");
        let inv = rc.inverse();
        let composed = rc.then(&inv).expect("compose");
        // For arbitrary initial wires, composed must be identity.
        for mask in 0..(1u32 << rc.num_wires.min(10)) {
            let mut wires: Vec<bool> =
                (0..rc.num_wires).map(|i| (mask >> i) & 1 == 1).collect();
            let orig = wires.clone();
            composed.apply_pure(&mut wires).expect("pure");
            assert_eq!(wires, orig, "inverse∘circuit ≠ identity for mask {mask}");
        }
    }

    #[test]
    fn watchlist_translation() {
        let mut circ = BCircuit::new(2);
        let o = build(&mut circ, BIrStmt::Or(IRVarId(0), IRVarId(1)));
        let a = build(&mut circ, BIrStmt::And(o, IRVarId(1)));
        circ.outputs = vec![a];
        let (_rc, map) = to_reversible(&circ).expect("converts");

        // Param 0 lives on wire 0; stmt vars live on their allocated wires.
        assert_eq!(map.wire(IRVarId(0)), Some(0));
        assert_eq!(map.wire(IRVarId(1)), Some(1));
        assert_eq!(map.wire(IRVarId(o.0)), Some(2));
        assert_eq!(map.wire(IRVarId(a.0)), Some(3));

        let wl = ValueWatchlist::from_vars([0u32, a.0]);
        let translated = translate_watchlist(&wl, &map).expect("translates");
        assert_eq!(
            translated.entries,
            vec![WireWatchEntry { wire: 0, var: 0 }, WireWatchEntry { wire: 3, var: a.0 }]
        );

        // Unknown id fails closed.
        let bad = ValueWatchlist::from_vars([99]);
        assert_eq!(
            translate_watchlist(&bad, &map).unwrap_err(),
            UnknownVar { var: 99 }
        );
    }

    #[test]
    fn rejects_external_primitives() {
        let mut circ = BCircuit::new(1);
        let rng = circ.push_stmt(BIrStmt::Rng { name: alloc::string::String::from("r") }, ());
        circ.outputs = vec![rng];
        assert_eq!(
            to_reversible(&circ).unwrap_err(),
            ToReversibleError::UnsupportedStmt { var: 1 }
        );
    }

    #[test]
    fn rejects_use_before_def() {
        let mut circ = BCircuit::new(1);
        // Stmt 0 uses stmt 1's result (var 2) — out of SSA order.
        let s0 = circ.push_stmt(BIrStmt::Not(IRVarId(2)), ());
        let _s1 = circ.push_stmt(BIrStmt::Not(s0), ());
        circ.outputs = vec![s0];
        assert_eq!(
            to_reversible(&circ).unwrap_err(),
            ToReversibleError::UseBeforeDef { user: 1, operand: 2 }
        );
    }
}
