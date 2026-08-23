# Plan: Wire Reuse for Single-Use XOR Operands in `to_reversible`

**Status:** Landed — implemented in `to_reversible.rs` with structural unit
 tests (reuse, multi-use fallback, param exclusion, chain collapse,
 deterministic tiebreak) and an unmodified Property E. `VarWireMap` gained a
 `y_base()` accessor; it is injective among live values only (consumed vars
 alias their consumer's wire).
**Scope:** `crates/ir/volar-ir-passes/src/to_reversible.rs` (+ tests).
**Kind:** Simple, local optimization. No IR changes, no new passes.

## Motivation

`to_reversible` allocates one zero-initialized ancilla wire per statement
result and synthesizes `Xor(a, b)` as two CNOTs into that fresh wire:

```
w_r := 0 (implicit);  CNOT(w_a → w_r);  CNOT(w_b → w_r)
```

When one operand — say `b` — is a **non-input** var whose *only* use is this
XOR, the fresh ancilla is pure waste. Reversible semantics let us consume
`b`'s wire in place:

```
CNOT(w_a → w_b)        // w_b now holds a ⊕ b == r
```

Savings per hit: **1 wire** (the unused ancilla) and **1 gate** (two CNOTs
become one). Fused/movfuscated circuits are full of extension chains
(`t1 = x ⊕ c1; t2 = t1 ⊕ c2; …`) where every intermediate except the last is
single-use, so hits are common, and reuse **cascades** along chains because
each reused result is itself a candidate for the next XOR.

## Current state (what makes this more than a two-line change)

Wire layout today: `[params | stmt-result ancillas | y register | scratch…]`,
and the synthesis loop addresses operands through `wire_of(v) = v.0 as
usize` — an *identity* assumption. `VarWireMap { map, num_wires }` exists and
is returned to callers (watchlist translation, callers seeding inputs), but
it is currently filled in as identity too.

Reuse breaks identity: if `r = a ⊕ b` consumes `b`, then **r lives at b's old
wire**, and every subsequent reference to `r` must resolve through the map.
So this optimization requires de-identitying the addressing — which is also
what makes it a clean prerequisite for any future reuse scheme (Not-folding,
dead-ancilla reclamation).

## Design

### Rule

For each `Xor(a, b)` producing `r`: let `cand` be an operand such that

1. `cand` is not an input param (the x register is immutable by the `(x, ·) ↦
   (x, ·)` contract), and
2. `cand` occurs exactly once across all operand positions in the circuit
   (other stmts' operands, storage addr/src lists) **plus** `circ.outputs`.

If exactly one operand qualifies, use it (deterministic tiebreak: prefer
`a`). If both qualify, still pick just one — the other simply remains a
garbage ancilla, same as today. If none qualifies, emit the existing
two-CNOT form into the fresh ancilla.

### Use-count analysis

A pre-pass over `circ` builds `uses: Vec<u32>` (indexed by var id):

- `Xor`/`And`/`Or`: +1 for each operand var
- `Not`: +1
- `StorageRead`: +1 per addr var; `StorageWrite`: +1 for src and per addr var
- `outputs`: +1 per output var
- `Zero`/`One`/`Rng`: nothing

Params start at 0 uses from statements by construction (SSA order); they are
excluded from candidacy by rule 1 regardless.

### Synthesis change

Track the live wire of each var in `map_vec` **during** the loop (params:
identity; stmt results: filled at their defining stmt — either the fresh
ancilla or the reused wire). Replace the free function `wire_of(v) = v.0 as
usize` with a closure over `map_vec`. At an eligible `Xor(a, cand)`:

```
let wc = map_vec[cand];          // cand's live wire (may itself be a reuse)
gates.push(Cnot { ctrl: map_vec[other], target: wc });
map_vec[r] = wc;                 // r now lives there
// do NOT allocate/zero the stmt-i ancilla; it is never emitted
```

The final `VarWireMap` is the accumulated `map_vec`; consumed vars' entries
alias their consumer's wire (they are dead by construction — see
invariants). `num_wires` accounting: the skipped ancilla is simply not
counted; scratch/y bases computed off the post-loop `next_scratch` as today,
with `num_wires_total = next_scratch.max(...)` adjusted so the y register
still sits above the highest allocated wire.

### Invariants & edge cases

- **Consumed vars are unreadable-after.** Use count == 1 and that use is
  this XOR ⇒ no later stmt, storage op, or output reads the operand again.
  Its map entry may safely alias. Callers must not query the wire of a
  consumed var and expect the operand's value; document this on
  `VarWireMap`.
- **Watchlists stay sound.** Watchlists are derived from things that read
  the value (outputs / downstream uses). A consumed var has none, so no
  well-formed watchlist references it. Property E only resolves var 0 (a
  param) — unaffected.
- **Chains cascade.** Inner XOR results defined before outer ones (SSA order
  already enforced by `check_operand`), so a reused result is visible with
  correct count when the outer XOR is synthesized.
- **No self-reuse hazards.** `a ≠ b ≠ r` in well-formed SSA;
  `check_operand` already rejects out-of-order refs.
- **Output phase and storage ops go through the map.** The output CNOTs'
  `ctrl` and storage addr/src wire lookups must use the map-based lookup —
  mechanical but easy to miss.
- **Semantics unchanged elsewhere.** `Zero/One/And/Or/Not/storage` arms are
  untouched apart from switching to map-based wire lookup.

### Non-goals (noted as follow-ups, not implemented here)

- `Not(a)` folding for single-use non-input `a` (`X(w_a)`, saves a wire).
- General dead-wire reclamation / scratch pool reuse.
- Applying reuse at the VCircuit level.

## Testing

1. **Unit (structural, justified as a specific structural invariant):**
   - single-use XOR consumes operand: 1 CNOT, no extra wire, `map[r] ==
     old wire of operand`;
   - multi-use operand keeps the 2-CNOT form;
   - param operand is never consumed even when single-use;
   - chain `x⊕c1 ⊕ c2` collapses to 2 CNOTs total;
   - both-operands-eligible picks `a` deterministically;
   - existing tests (`xor_and_or_not_gates`, storage synthesis, inverse
     restores all wires) keep passing unchanged.
2. **Property E rerun** (`prop_e_to_reversible_implements_xor_embedding`):
   must pass unmodified — outputs and joint reversibility are reuse-
   agnostic since all observable reads go through `VarWireMap`/outputs.
3. **Workspace + rkyv** feature check.

## Phases

### Phase 1 — implementation (`to_reversible.rs`)

- [ ] Use-count pre-pass.
- [ ] Map-based wire lookup replacing `wire_of`; fill `map_vec` at defs.
- [ ] Xor arm reuse rule; skip ancilla allocation on hit.
- [ ] Doc comment on `VarWireMap` describing consumed-var aliasing.

### Phase 2 — tests & docs

- [ ] New unit tests listed above.
- [ ] Rerun Property E; workspace green incl. `--features rkyv`.
- [ ] Note the optimization (and the de-identitying) in
      `docs/circuit-fused-ir-plan.md` status and this file's Status line.
