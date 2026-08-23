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
//! [ x register (params) | stmt-result ancillas | y register (outputs) | scratch ]
//! ```
//!
//! Stmt-result ancillas are compacted: a statement whose result is produced
//! by a single-use XOR operand reuse (see gate cost below) allocates no
//! ancilla of its own.
//!
//! [`VarWireMap`] — the total map from Boolar var space to wire indices — is
//! a byproduct of allocation and is what consumers use for value→wire
//! watchlist translation.
//!
//! # Gate cost
//!
//! Per statement: `Xor` → 2 CNOT, or **1 CNOT with no new wire** when an
//! operand is a single-use non-input var whose only reader is this XOR (the
//! operand's wire is consumed in place); `And` → 1 Toffoli, `Not` → 1 CNOT +
//! 1 X, `Or` → 2 CNOT + 2 X + 1 Toffoli + 1 X (De Morgan on copied-not
//! operands). Output phase adds one CNOT per output bit.

use alloc::collections::BTreeSet;
use alloc::vec::Vec;
use volar_ir::boolar::BIrStmt;
#[cfg(test)]
use volar_ir::boolar::LaneId;
use volar_ir::circuit::BCircuit;
use volar_ir::ir::IRVarId;
use volar_ir::rcircuit::{RCircuit, RGate};
#[cfg(test)]
use volar_ir::rcircuit::StorageState;
#[cfg(test)]
use volar_ir_common::StorageId;

/// Why a Boolar circuit could not be converted to a reversible circuit.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum ToReversibleError {
    /// External primitives have no reversible-gate realization in the naive
    /// scheme (oracles, actions, RNG). Storage access *is* synthesized — via
    /// [`RGate::StorageSwap`] — but only for the fused single-block form.
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

/// Total map from a `BCircuit`'s var space (params followed by stmt results)
/// to `RCircuit` wire indices.
///
/// Produced by the same [`to_reversible`] run that emits the [`RCircuit`] —
/// never reconstructed after the fact, so it cannot drift from the allocator's
/// layout.
///
/// The map is injective among **live** values. As a wire-reuse optimization,
/// a Boolar var whose only use feeds an `Xor` may have its wire consumed in
/// place: the XOR's result then lives at the consumed operand's old wire, and
/// the consumed var's own entry aliases it. A consumed var has no remaining
/// readers by construction, so well-formed consumers (which only observe
/// vars via outputs or downstream uses) never query a stale entry.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct VarWireMap {
    /// `map[var_id] = wire_index`; total over `0..var_space`. Injective
    /// among live values; entries of wire-consumed vars alias their
    /// consumer's wire (see type doc).
    map: Vec<usize>,
    /// Total wires in the produced circuit (including scratch).
    num_wires: usize,
    /// First wire of the y register (one wire per output, in output order).
    y_base: usize,
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

    /// First wire of the y register; output `k` lives at `y_base() + k` and
    /// accumulates `f(x)` under XOR.
    pub fn y_base(&self) -> usize {
        self.y_base
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

/// Convert a circuit-fused Boolar program computing `y_out = f(x_in)` into a
/// reversible circuit over `(x, y)` implementing `(x, y) ↦ (x, y ⊕ f(x))`,
/// plus the var→wire map.
///
/// Returns `(RCircuit, VarWireMap)`. The y-register wires are the last
/// `circ.outputs.len()` wires below any scratch; use the returned map plus
/// [`translate_watchlist`] to locate them symbolically.
pub fn to_reversible(circ: &BCircuit) -> Result<(RCircuit, VarWireMap), ToReversibleError> {
    // ---- Use-count analysis -------------------------------------------------
    // Counts of each var in every *reader* position: stmt operands, storage
    // addr/src lists, and outputs. A var with count == 1 that is not an input
    // param is a wire-reuse candidate for the single statement that reads it.
    let params = circ.params;
    let n_stmts = circ.stmts.len();
    let mut uses = alloc::vec![0u32; params as usize + n_stmts];
    {
        let mut bump = |v: IRVarId| uses[v.0 as usize] += 1;
        for node in &circ.stmts {
            match &node.kind {
                BIrStmt::Xor(a, b) | BIrStmt::And(a, b) | BIrStmt::Or(a, b) => {
                    bump(*a);
                    bump(*b);
                }
                BIrStmt::Not(a) => bump(*a),
                BIrStmt::StorageRead { addr, .. } => {
                    for v in addr {
                        bump(*v);
                    }
                }
                BIrStmt::StorageWrite { src, addr, .. } => {
                    bump(*src);
                    for v in addr {
                        bump(*v);
                    }
                }
                _ => {}
            }
        }
        for out in &circ.outputs {
            bump(*out);
        }
    }

    // ---- Wire allocation ---------------------------------------------------
    // x register: one wire per param bit (wire i carries param i).
    // stmt ancillas: allocated on demand below; a stmt whose result consumes
    // a single-use operand's wire allocates none (see the Xor arm).
    let mut map_vec: Vec<usize> = Vec::with_capacity(params as usize + n_stmts);
    for w in 0..params as usize {
        map_vec.push(w);
    }

    // Provisional y-register base (an upper bound; compacted after synthesis).
    let y_base_prov = params as usize + n_stmts;

    let mut gates: Vec<RGate> = Vec::new();

    // Scratch allocator (appended after the provisional y register).
    let mut next_scratch = y_base_prov + circ.outputs.len();

    // On-demand stmt-result ancilla allocator.
    let mut next_anc = params as usize;


    // SSA order check + synthesis per statement.
    for (i, node) in circ.stmts.iter().enumerate() {
        let r = IRVarId(params + i as u32); // this stmt's result var
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
        let check_operand_list = |vs: &[IRVarId]| -> Result<(), ToReversibleError> {
            vs.iter().try_for_each(|&v| check_operand(v))
        };
        match &node.kind {
            BIrStmt::Zero => {
                // No gates: a fresh ancilla is already 0.
                let wr = next_anc;
                next_anc += 1;
                map_vec.push(wr);
            }
            BIrStmt::One => {
                let wr = next_anc;
                next_anc += 1;
                gates.push(RGate::X(wr));
                map_vec.push(wr);
            }
            BIrStmt::Xor(a, b) => {
                check_operand(*a)?;
                check_operand(*b)?;
                let wa = map_vec[a.0 as usize];
                let wb = map_vec[b.0 as usize];
                // Wire reuse: an operand whose only reader is this XOR and
                // which is not an input param can be consumed in place —
                // XOR the other operand into its wire instead of copying
                // both into a fresh ancilla (saves one wire and one gate).
                // Prefer `a` when both qualify. Guards: `a != b` avoids a
                // self-CNOT; distinct live wires avoid clobbering the other
                // operand through prior aliasing.
                let is_candidate = |v: IRVarId| v.0 >= params && uses[v.0 as usize] == 1;
                let sa = is_candidate(*a);
                let sb = is_candidate(*b);
                let cand = if sa && !sb {
                    Some(*a)
                } else if sb && !sa {
                    Some(*b)
                } else if sa && sb && a != b && wa != wb {
                    Some(*a)
                } else {
                    None
                };
                if let Some(c) = cand {
                    let wc = map_vec[c.0 as usize];
                    let other_wire = if c == *a { wb } else { wa };
                    gates.push(RGate::Cnot { ctrl: other_wire, target: wc });
                    // The result now lives at the consumed operand's wire.
                    map_vec.push(wc);
                } else {
                    // w_r starts 0; XOR-copy both operands into it.
                    let wr = next_anc;
                    next_anc += 1;
                    gates.push(RGate::Cnot { ctrl: wa, target: wr });
                    gates.push(RGate::Cnot { ctrl: wb, target: wr });
                    map_vec.push(wr);
                }
            }
            BIrStmt::And(a, b) => {
                check_operand(*a)?;
                check_operand(*b)?;
                let wr = next_anc;
                next_anc += 1;
                gates.push(RGate::Ccnot {
                    c1: map_vec[a.0 as usize],
                    c2: map_vec[b.0 as usize],
                    target: wr,
                });
                map_vec.push(wr);
            }
            BIrStmt::Not(a) => {
                check_operand(*a)?;
                let wr = next_anc;
                next_anc += 1;
                // Copy-then-invert: never invert an input wire (would break
                // the `(x, ·) ↦ (x, ·)` contract).
                gates.push(RGate::Cnot { ctrl: map_vec[a.0 as usize], target: wr });
                gates.push(RGate::X(wr));
                map_vec.push(wr);
            }
            BIrStmt::Or(a, b) => {
                check_operand(*a)?;
                check_operand(*b)?;
                let wr = next_anc;
                next_anc += 1;
                // De Morgan on copied-not operands: res := ¬(¬a ∧ ¬b).
                let na = next_scratch;
                next_scratch += 1;
                let nb = next_scratch;
                next_scratch += 1;
                gates.push(RGate::Cnot { ctrl: map_vec[a.0 as usize], target: na });
                gates.push(RGate::X(na));
                gates.push(RGate::Cnot { ctrl: map_vec[b.0 as usize], target: nb });
                gates.push(RGate::X(nb));
                gates.push(RGate::Ccnot { c1: na, c2: nb, target: wr });
                gates.push(RGate::X(wr));
                map_vec.push(wr);
            }
            BIrStmt::StorageRead { storage, lane, addr } => {
                check_operand_list(addr)?;
                // Bennett-style non-destructive read:
                //   swap cell ↔ scratch (cell value now in scratch, cell = 0)
                //   CNOT scratch → result wire (copy the bit out)
                //   swap again (restore the cell, clear the scratch)
                let s = next_scratch;
                next_scratch += 1;
                let addr_wires: Vec<usize> =
                    addr.iter().map(|v| map_vec[v.0 as usize]).collect();
                let swap = |target| RGate::StorageSwap {
                    storage: *storage,
                    lane: *lane,
                    addr: addr_wires.clone(),
                    target,
                };
                gates.push(swap(s));
                let wr = next_anc;
                next_anc += 1;
                gates.push(RGate::Cnot { ctrl: s, target: wr });
                gates.push(swap(s));
                map_vec.push(wr);
            }
            BIrStmt::StorageWrite { storage, lane, src, addr } => {
                check_operand(*src)?;
                check_operand_list(addr)?;
                // Copy the source bit into a fresh scratch wire, then swap it
                // into the cell. The old cell value is left in the scratch as
                // expected Bennett garbage (documented, not uncomputed). The
                // stmt's own result var is void — it gets a fresh ancilla
                // that stays 0 and is never read.
                let t = next_scratch;
                next_scratch += 1;
                gates.push(RGate::Cnot { ctrl: map_vec[src.0 as usize], target: t });
                gates.push(RGate::StorageSwap {
                    storage: *storage,
                    lane: *lane,
                    addr: addr.iter().map(|v| map_vec[v.0 as usize]).collect(),
                    target: t,
                });
                let wr = next_anc;
                next_anc += 1;
                map_vec.push(wr);
            }
            // Catch-all over the `#[non_exhaustive]` statement enum: every
            // external primitive falls here with no reversible lowering.
            _ => {
                return Err(ToReversibleError::UnsupportedStmt { var: r.0 });
            }
        }
        // Each arm records this stmt's wire entry only after validation.
    }

    // ---- Compaction ---------------------------------------------------------
    // Reuse left holes where consumed stmt results skipped their ancillas.
    // Shift every wire at or above the provisional y base down to close them,
    // across gates and map entries alike. All stmt wires (the only possible
    // holes) are strictly below the provisional base, and all shifted wires
    // are at or above it, so one linear pass suffices.
    let y_base = next_anc;
    let delta = y_base_prov - y_base;
    if delta > 0 {
        let mut shift = |w: &mut usize| {
            if *w >= y_base_prov {
                *w -= delta;
            }
        };
        for g in &mut gates {
            match g {
                RGate::X(w) => shift(w),
                RGate::Cnot { ctrl, target } => {
                    shift(ctrl);
                    shift(target);
                }
                RGate::Ccnot { c1, c2, target } => {
                    shift(c1);
                    shift(c2);
                    shift(target);
                }
                RGate::StorageSwap { addr, target, .. } => {
                    for w in addr.iter_mut() {
                        shift(w);
                    }
                    shift(target);
                }
                // Fail closed on future gate variants rather than silently
                // leaving stale wire indices behind.
                _ => panic!("to_reversible compaction: unknown RGate variant"),
            }
        }
        for w in map_vec.iter_mut() {
            shift(w);
        }
    }

    // ---- Output phase: y_i ^= out_i ----------------------------------------
    for (k, out) in circ.outputs.iter().enumerate() {
        gates.push(RGate::Cnot {
            ctrl: map_vec[out.0 as usize],
            target: y_base + k,
        });
    }

    let num_wires_total = next_scratch - delta;
    let circuit = RCircuit::new(num_wires_total, gates).expect(
        "to_reversible synthesis produces validated gates by construction",
    );
    Ok((
        circuit,
        VarWireMap { map: map_vec, num_wires: num_wires_total, y_base },
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
            // Locate the y register through the map (reuse may compact it).
            let y_base = map.y_base();
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

    // ---- Wire reuse ---------------------------------------------------------

    #[test]
    fn xor_reuses_single_use_operand() {
        // r = a ⊕ b where a = x0 ⊕ x1 is used only here: its wire is
        // consumed in place — one CNOT, no fresh ancilla.
        let mut circ = BCircuit::new(2);
        let a = build(&mut circ, BIrStmt::Xor(IRVarId(0), IRVarId(1)));
        let r = build(&mut circ, BIrStmt::Xor(a, IRVarId(0)));
        circ.outputs = vec![r];

        let (rc, map) = to_reversible(&circ).expect("converts");
        // Gate budget: the first XOR synthesizes normally into its ancilla
        // (2 CNOTs), then the second consumes `a`'s wire in place (1 CNOT),
        // plus one output-phase CNOT.
        let g = rc.gates();
        assert_eq!(g.len(), 4);
        assert_eq!(g[0], RGate::Cnot { ctrl: 0, target: 2 });
        assert_eq!(g[1], RGate::Cnot { ctrl: 1, target: 2 });
        assert_eq!(
            g[2],
            RGate::Cnot { ctrl: 0, target: 2 },
            "x0 XORed into a's old wire instead of a fresh ancilla"
        );
        // The result lives at the consumed operand's wire; no ancilla was
        // allocated for it.
        assert_eq!(map.wire(a), Some(2));
        assert_eq!(map.wire(r), Some(2));
        check_semantics(&circ);
    }

    #[test]
    fn xor_keeps_two_cnot_form_for_multi_use_operand() {
        // Same as above but the intermediate also feeds an output: it has
        // two uses, so the classic 2-CNOT form must be kept.
        let mut circ = BCircuit::new(2);
        let a = build(&mut circ, BIrStmt::Xor(IRVarId(0), IRVarId(1)));
        let r = build(&mut circ, BIrStmt::Xor(a, IRVarId(0)));
        circ.outputs = vec![a, r];

        let (rc, _map) = to_reversible(&circ).expect("converts");
        // 2 CNOT per XOR plus 2 output-phase CNOTs: no reuse happened.
        assert_eq!(rc.gates().len(), 6);
        check_semantics(&circ);
    }

    #[test]
    fn params_are_never_consumed() {
        // x1 is single-use here but is an input: the XOR must copy into a
        // fresh ancilla and leave the x register untouched.
        let mut circ = BCircuit::new(2);
        let r = build(&mut circ, BIrStmt::Xor(IRVarId(0), IRVarId(1)));
        circ.outputs = vec![r];

        let (rc, map) = to_reversible(&circ).expect("converts");
        assert_eq!(rc.gates().len(), 3, "2 CNOT + output phase");
        assert_ne!(map.wire(r), Some(1), "must not live on the param wire");
        check_semantics(&circ);
    }

    #[test]
    fn xor_chain_collapses() {
        // t1 = x ⊕ c1; t2 = t1 ⊕ c2; out = t2 ⊕ x: t1 and t2 are single-use
        // non-inputs, so both are consumed — three XORs, three CNOTs total.
        let mut circ = BCircuit::new(1);
        let c1 = build(&mut circ, BIrStmt::One);
        let t1 = build(&mut circ, BIrStmt::Xor(IRVarId(0), c1));
        let c2 = build(&mut circ, BIrStmt::Zero);
        let t2 = build(&mut circ, BIrStmt::Xor(t1, c2));
        let out = build(&mut circ, BIrStmt::Xor(t2, IRVarId(0)));
        circ.outputs = vec![out];

        let (rc, map) = to_reversible(&circ).expect("converts");
        // Synthesis gates: One's X, then one reused CNOT per XOR (the
        // constants c1/c2 and intermediate t1 all get consumed), plus the
        // output-phase CNOT.
        let g = rc.gates();
        assert_eq!(g.len(), 5);
        assert_eq!(g[1], RGate::Cnot { ctrl: 0, target: 1 }, "x ⊕ c1 into c1's wire");
        assert_eq!(g[2], RGate::Cnot { ctrl: 2, target: 1 }, "⊕ c2 into the chain wire");
        assert_eq!(g[3], RGate::Cnot { ctrl: 0, target: 1 }, "⊕ x into the chain wire");
        // All three results alias the same consumed chain wire.
        assert_eq!(map.wire(t1), Some(1));
        assert_eq!(map.wire(t2), Some(1));
        assert_eq!(map.wire(out), Some(1));
        // Layout compacted: [x | c1/chain wire | c2's zero wire | y] = 4 wires,
        // not the 7 an uncompacted layout would need.
        assert_eq!(rc.num_wires, 4);
        assert_eq!(map.y_base(), 3);
        check_semantics(&circ);
    }

    #[test]
    fn both_operands_eligible_prefers_a() {
        // Both operands of the outer XOR are single-use non-inputs: `a`
        // must be the consumed one (deterministic tiebreak).
        let mut circ = BCircuit::new(2);
        let a = build(&mut circ, BIrStmt::Not(IRVarId(0)));
        let b = build(&mut circ, BIrStmt::Not(IRVarId(1)));
        let r = build(&mut circ, BIrStmt::Xor(a, b));
        circ.outputs = vec![r];

        let (_rc, map) = to_reversible(&circ).expect("converts");
        assert_eq!(map.wire(r), Some(2), "consumed a's wire, not b's");
        check_semantics(&circ);
    }

    #[test]
    fn storage_read_write_synthesis() {
        // f(x0) = cell[x0] (one-bit storage read), then a second circuit that
        // writes x0 into cell[0]. Both must evaluate correctly through the
        // storage model, and reads must leave storage restored.
        let sid = StorageId::DEFAULT;
        let lane = LaneId(0);

        let mut read_circ = BCircuit::new(1);
        let r = read_circ.push_stmt(
            BIrStmt::StorageRead { storage: sid, lane, addr: vec![IRVarId(0)] },
            (),
        );
        read_circ.outputs = vec![r];
        let (rc, map) = to_reversible(&read_circ).expect("storage read converts");
        assert!(rc.uses_storage());

        for addr_bit in [false, true] {
            for stored in [false, true] {
                let mut storage = StorageState::new();
                if stored {
                    storage.insert(((sid, lane), addr_bit as u64), true);
                }
                let mut wires = vec![false; rc.num_wires];
                wires[map.wire(IRVarId(0)).unwrap()] = addr_bit;
                rc.apply(&mut wires, &mut storage);
                // y register (last output wire region): y ^= f(x).
                let y_base = map.y_base();
                assert_eq!(wires[y_base], stored, "read at addr={addr_bit}");
                // Cell restored by the swap-back pair.
                let got = storage.get(&((sid, lane), addr_bit as u64)).copied().unwrap_or(false);
                assert_eq!(got, stored, "cell must hold its original value");
            }
        }

        // Write: cell[0] <- x0 (address wire is a Zero constant).
        let mut write_circ = BCircuit::new(1);
        let zero = write_circ.push_stmt(BIrStmt::Zero, ());
        let _w = write_circ.push_stmt(
            BIrStmt::StorageWrite {
                storage: sid,
                lane,
                src: IRVarId(0),
                addr: vec![zero],
            },
            (),
        );
        write_circ.outputs = vec![];
        let (rcw, _) = to_reversible(&write_circ).expect("storage write converts");
        for x in [false, true] {
            let mut storage = StorageState::new();
            let mut wires = vec![false; rcw.num_wires];
            wires[0] = x;
            rcw.apply(&mut wires, &mut storage);
            assert_eq!(
                storage.get(&((sid, lane), 0)).copied().unwrap_or(false),
                x,
                "cell[0] must hold written bit"
            );
        }
    }
}
