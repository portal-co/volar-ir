# Plan: Circuit-Fused IR Variants, Reversible Circuits, and Lowering Transforms

> **Historical note:** movfuscated programs now enter the typed one-step
> boundary in [`pipeline.md`](pipeline.md); this plan's fuse/unroll seam is retired.

**Status:** Phases 1–3 landed (fused types, `RCircuit`, fusion + reversible
transforms, watchlist translation, exhaustive unit tests, and fuzz Property E
in `crates/fuzz/volar-fuzz/src/properties/reversible.rs`).
Storage synthesis in `to_reversible` has since **landed** as part of the
Boolar 1-bit-storage refactor (see `docs/boolar-1bit-storage-plan.md`):
`RGate::StorageSwap` now carries a `LaneId`, and `to_reversible` synthesizes
non-destructive reads and swap-in writes instead of rejecting storage ops.
An opt-in `ReversibleMode::Hardened` has also **landed**. It reuses the naive
synthesis as a compute phase, uncomputes every intermediate, and applies the
intended output XOR only when the full synthesized workspace starts at zero;
all nonzero workspace inputs map to identity even for arbitrary borrowed-wire
values. This is the hardened-Toffoli invariant from Appendix A of IACR ePrint
2024/006.
**Deferred:** text-format (`volar-ir-text`) print/parse for the fused and
reversible forms.
**Optimization:** `to_reversible` reuses the wire of any single-use non-input
XOR operand in place (1 CNOT, no fresh ancilla; chains collapse); see
`docs/to-reversible-wire-reuse-plan.md`.
**Scope:** `crates/ir/volar-ir` (new types), `crates/ir/volar-ir-passes`
(new transforms), `crates/ir/volar-ir-opt` (opt support),
`crates/ir/volar-ir-text` (text format), `crates/fuzz/volar-fuzz`
(semantics-preservation tests).

## Goal

Today the "circuit" shape of Volar IR and Boolar IR is a *predicate*, not a
*type*: `IRBlocks::is_circuit()` / `BIrBlocks::is_circuit()` check at runtime
that there happens to be exactly one block whose only exit is
`Jmp { target: Return }`. Every backend consumer must re-establish or trust
this invariant, and nothing prevents later edits from breaking it.

This plan introduces three things:

1. **Circuit-fused variants** of Volar IR and Boolar IR — dedicated types that
   *structurally* represent exactly one block with no control flow, making the
   circuit invariant unrepresentable to violate.
2. **A reversible circuit-fused Boolar IR** — a gate-level representation of
   *reversible* circuits (bijective boolean maps), using a reversible gate
   basis (X / CNOT / Toffoli), an atomic two-input target-XOR lookup gate,
   plus a reversible storage-exchange gate.
3. **Two transforms:**
   - `to_circuit_fused`: ordinary (movfuscated/circuit-shaped) Volar IR and
     Boolar IR → their circuit-fused variants.
   - `to_reversible` (naive): a Boolar circuit computing `f(x)` → a reversible
     circuit computing `(x, y) ↦ (x, y ⊕ f(x))`.

This mirrors how `lir-lowering-monomorphization-plan.md` moved an implicit
whole-module precondition into explicit structure: here we move the
"circuitness" precondition from runtime predicates into the type system.

---

## Phase 1 — Circuit-fused Volar IR and Boolar IR

### Current state

- `crates/ir/volar-ir/src/ir.rs`: `IRBlocks { blocks: Vec<IRBlock> }`,
  `is_movfuscated() == blocks.len() == 1`, `is_circuit()` additionally
  requires the sole block's terminator to be `Jmp { dest: Return }`.
- `crates/ir/volar-ir/src/boolar.rs`: same shape for `BIrBlocks` /
  `BIrBlock { params: u32, stmts, terminator }`.
- `lower_to_circuit.rs` (volar-ir-passes) produces this shape by unrolling
  self-loops and MUX-gating conditional exits, but returns it as a plain
  `BIrBlocks`/`IRBlocks`, so the guarantee is invisible downstream.

### Proposed types

New module `crates/ir/volar-ir/src/circuit.rs` (shared scaffolding) plus
extensions near the existing definitions:

```rust
/// A circuit-fused Volar program: exactly one block, no control flow.
pub struct VCircuit<P: Clone = ()> {
    /// Input parameters (block params), typed as today.
    pub params: Vec<IRTypeId>,
    pub stmts: Vec<Node<IRStmt, P>>,
    /// The variables returned to the caller (former `Jmp { Return }` args).
    pub outputs: Vec<IRVarId>,
}

/// A circuit-fused Boolar program: exactly one bit-block, no control flow.
pub struct BCircuit<P: Clone = ()> {
    pub params: u32,
    pub stmts: Vec<Node<BIrStmt, P>>,
    pub outputs: Vec<IRVarId>,
}
```

Design decisions:

1. **No terminator field.** A circuit has no terminator; its former
   `Jmp { Return, args }` becomes the `outputs` list. This deletes the last
   place a hidden jump could hide.
2. **Statement subset.** All current non-control-flow statements are allowed:
   - Volar: `StorageRead/Write`, `Const`, `Transmute`, `Poly`, `Rol`, `Ror`,
     `Merge`, `Splat`.
   - Boolar: `Zero`, `One`, `And`, `Or`, `Xor`, `Not`, plus the external
     primitives (`OracleCall`/`OracleBit`, `ActionCall`/`ActionBit`, `Rng`,
     `StorageRead`/`StorageWrite`).
   - Forbidden *by construction*: any statement producing or consuming a
     first-class `Block { .. }` value, and any `Dyn` target (there is no place
     to put one). Enforce with a validation constructor, not by duplicating
     the enums (see decision 4).
3. **Round-trip, not replacement.** `From<VCircuit> for IRBlocks` and
   `TryFrom<IRBlocks> for VCircuit` (fallible, validating single-block +
   `Jmp { Return }` + no block-typed values) so all existing passes, text
   format, rkyv serialization, and backends keep working while new code can
   require the fused type. `IRBlocks::is_circuit()` stays but becomes a thin
   helper used by `TryFrom`.
4. **Validation lives in one constructor.**
   `VCircuit::try_from_ir(blocks: &IRBlocks) -> Result<Self, CircuitFusionError>`
   (and the Boolar analogue) is the only entry point; it performs the checks
   once and reports precise errors (`NotSingleBlock`, `NotReturnTerminator`,
   `BlockTypedValue`, …). Catch-all arms on IR-type matches elsewhere must not
   silently accept circuit-illegal shapes (per repo design rules).
5. **Provenance and side tagging preserved** via `Node<Stmt, P>` exactly as in
   the unfused IRs; `map_prov_with_handler` gets a fused counterpart (see
   `docs/agent-context/provenance-pipeline.md`).
6. **rkyv/text parity.** Derive rkyv archives like sibling types; add
   parse/print support in `volar-ir-text` behind the existing conventions in
   `docs/text-format-spec.md` (e.g. a `circuit` keyword distinguishing fused
   forms).

### Work items

- [x] `circuit.rs` with `VCircuit`, `BCircuit`, error enum, `TryFrom`/`From`.
      Note: `BIrBlocks` has no oracle/action/RNG tables (only `pre_init`), so
      `ModuleLevelStateUnsupported` fires for Volar declarations and Boolar
      pre-init segments. Block-typed-value rejection is enforced by
      producers, not the fuser: no types table is reachable from `IRBlocks`
      alone; revisit if a validating constructor with a types table is needed.
- [x] Builder API mirroring `IRBlock::push_stmt` (var ids continue the
      param-index numbering).
- [x] Fused types round-trip through DCE/CSE-compatible general form;
      opt passes need no changes because fusion validates rather than
      transforms (opt code paths operate on the general form).
- [ ] `volar-ir-text` round-trip tests (**deferred**).
- [x] Unit tests: valid fusion of single-block shapes; rejection cases
      (`NotSingleBlock`, `NotReturnTerminator`, out-of-range outputs).

---

## Phase 2 — Reversible circuit-fused Boolar IR

### Semantics

A reversible circuit is a bijection on `{0,1}^N` given as a sequence of
self-inverse or easily-invertible gates over a fixed wire vector. Unlike
`BCircuit`, there is no notion of SSA values — gates mutate wires in place,
which is the natural representation for garbled/reversible-circuit backends.

```rust
/// Gate indices refer to positions in `0..num_wires`.
#[non_exhaustive]
pub enum RGate {
    /// Pauli-X (NOT) on one wire.
    X(usize),
    /// Controlled-NOT: target ^= ctrl.
    Cnot { ctrl: usize, target: usize },
    /// Toffoli (CCNOT): target ^= c1 & c2.
    Ccnot { c1: usize, c2: usize, target: usize },
    /// Reversible storage exchange: atomically SWAP the target wire with the
    /// bit stored at `((storage, lane), addr)`.
    ///
    /// This is the reversible analogue of Boolar's `StorageRead`/
    /// `StorageWrite`: because it *exchanges* rather than copies, the joint
    /// map over (wires, storage contents) is a bijection regardless of what
    /// the cell held. Reading is therefore destructive-but-restorable by
    /// applying the same gate again (it is an involution).
    ///
    /// `addr` is a list of wire indices forming the address, most-significant
    /// first or LSB-first per the Boolar storage convention (`bit 0` = index 0
    /// = least-significant; match `BIrStmt::StorageRead`'s convention).
    StorageSwap {
        storage: StorageId,
        lane: LaneId,
        addr: Vec<usize>,
        target: usize,
    },
    /// Arbitrary two-input reversible lookup: target ^= lut(c0, c1).
    /// Appended so the original variants retain their archive discriminants.
    XorLut2 { controls: [usize; 2], target: usize, table: u8 },
}

pub struct RCircuit {
    pub num_wires: usize,
    pub gates: Vec<RGate>,
}
```

Decision record:

- **No `Fredkin` in v1.** Controlled-SWAP waits until an actual consumer
  needs it; the `#[non_exhaustive]` enum admits it later without breaking
  matches. Note `StorageSwap` is *un*controlled by design — controlled
  variants of it would require the Fredkin machinery we are deferring.
- **Reversible storage ships in v1.** `StorageSwap` gives backends lossless
  spill/fetch of circuit state without a copy-and-zero discipline; it is
  self-inverse and validated like any other gate.

Invariants (validated on construction):

- Every index `< num_wires`; `target ≠ ctrl`; `Ccnot` targets distinct from
  controls; `StorageSwap` address wires in range and `target` not among them.
- `RGate` is `#[non_exhaustive]` so future gates (multi-controlled Toffoli,
  SW-by-basis-change sugar) can be added without breaking matches — but every
  added gate must be reversible *by construction*; that is the type's reason
  to exist.
- No provenance initially (wire-mutation form doesn't fit `Node<Stmt, P>`
  cleanly). If provenance is needed later, attach it per gate as a parallel
  `Vec<P>` keyed by gate index — decide when a consumer appears.

Provided operations:

- `RCircuit::apply(&self, wires: &mut [bool], storage: &mut StorageState)` —
  pure gates touch only `wires`; `StorageSwap` exchanges against the storage
  model. A storage-free convenience wrapper asserts no `StorageSwap` gates
  are present.
- `inverse(&self)` — reverse gate order (every v1 gate is an involution;
  assert this in tests, including double-application of `StorageSwap`).
- Wire-count-aware composition `then(&self, next: &RCircuit)`.

Placement: `crates/ir/volar-ir/src/rcircuit.rs`. It depends only on the
storage-id type shared with `boolar.rs`, not on `BIrStmt` itself.

### Work items

- [x] Types + validation + `apply` / `inverse` / `then` (with the storage
      model threaded through `apply`).
- [x] Property test: `c.inverse().then(&c)` is identity on random wire *and*
      random storage assignments (unit-level in `rcircuit.rs`; circuit-level
      via fuzz Property E).
- [ ] Optional: text-format rendering (`x w`, `cx c t`, `ccx ...` lines) —
      defer until a consumer needs it.

---

## Phase 3 — Transforms

### 3a. `to_circuit_fused` (normal → circuit-fused)

Location: `crates/ir/volar-ir-passes/src/fuse_to_circuit.rs`.

- For Volar IR: input must satisfy `is_movfuscated()`; fuse the single block.
  If the terminator isn't `Jmp { Return }`, return an error directing callers
  to run movfuscation / loop unrolling first — do **not** silently unroll
  here (that is `lower_to_circuit`'s job, with its own budget policy).
- For Boolar IR: same, delegating validation entirely to the Phase 1
  constructor (single implementation of the invariant).
- Signature sketch:

  ```rust
  pub fn to_circuit_fused_volar(blocks: &IRBlocks) -> Result<VCircuit, CircuitFusionError>;
  pub fn to_circuit_fused_boolar(blocks: &BIrBlocks) -> Result<BCircuit, CircuitFusionError>;
  ```

- Update `lower_to_circuit.rs` to optionally return fused types directly
  (feature-gated or dual API) so the canonical producer emits the strong type
  from day one.

### 3b. `to_reversible` (naive Bennett-style embedding)

Location: `crates/ir/volar-ir-passes/src/to_reversible.rs`.

Input: a `BCircuit` computing `y_out = f(x_in)`. Output: an `RCircuit` on
`(x, y)` wires implementing `(x, y) ↦ (x, y ⊕ f(x))`.

Naive scheme (no uncomputation — garbage ancillas are expected and documented):

1. **Wire allocation.** One wire per input bit (the `x` register), one per
   output bit (the `y` register), plus one ancilla per internal Boolar var,
   initialized to 0.
2. **Gate synthesis per statement**, processing statements in order (SSA defs
   precede uses inside a single block):
   - `Xor(a, b)` → `Cnot { ctrl: w(b), target: w(a) }` (result wire of `a`
     accumulates the XOR; map each Boolar var to its wire).
   - `And(a, b)` → `Ccnot { c1: w(a), c2: w(b), target: fresh_ancilla }`
     (AND against a zero-initialized ancilla = compute-and-write).
   - `Not(a)` → `X(w(a))` on a fresh ancilla after `Cnot` from `w(a)`
     (copy-then-invert; never invert an input wire, which would break the
     `(x, ·) ↦ (x, ·)` contract).
   - `Or(a, b)` → De Morgan: `¬a ∧ ¬b` then final `Not` — synthesized as
     copy-not both operands to fresh ancillas, Toffoli, X on result.
   - `Zero`/`One` → leave ancilla at 0 / apply `X` to it.
   - External primitives (`OracleCall`, `ActionCall`, `Rng`,
     `StorageRead/Write`) have no reversible-gate realization: reject with a
     dedicated error listing offending statements (extend later if a
     reversible oracle convention is designed).
3. **Output phase.** For each output var `o_i`:
   `Cnot { ctrl: w(o_i), target: y_i }` — this is precisely the
   `⊕= f(x)` step of the contract.
4. **Result:** `x` register untouched (verified), `y` register holds
   `y ⊕ f(x)`, internal ancillas hold garbage intermediates.
5. **Watchlist translation.** Consumers cross-reference the reversible
   circuit with its Boolar origin via a *watchlist*, not embedded wire names:

   - A **value-space watchlist** is a user-provided set of Boolar var ids
     (`BCircuit` value space: params + stmt results).
   - `to_reversible` builds the var→wire allocation table as a byproduct of
     step 1; expose it as `VarWireMap` (a total, injective map from every
     `BCircuit` var id to its RCircuit wire index).
   - `translate_watchlist(&ValueWatchlist, &VarWireMap) -> Result<WireWatchlist,
     UnknownVar>` resolves the user's value ids into a **wire-space
     watchlist** (sorted wire indices + provenance of each). This is what
     garbling/observation consumers consume.
   - Translation fails closed on unknown or out-of-scope var ids; it never
     silently drops entries.

   Rationale: keeping names out of `RCircuit` keeps the gate list compact and
   rkyv-friendly while still giving consumers an exact, validated
   correspondence between the two spaces. Watchlists are per-consumption,
   not baked into the artifact.

#### Hardened mode

`to_reversible_with_mode(circ, ReversibleMode::Hardened)` strengthens the
initialized-ancilla contract to a total function over `(x, y, z, u)`:

```text
(x, y, z, u) ↦ (x, y ⊕ f(x), z, u)  if z = 0
(x, y, z, u) ↦ (x, y,        z, u)  otherwise
```

The construction follows Appendix A of Canetti et al., *Towards
general-purpose program obfuscation via local mixing* (IACR ePrint 2024/006),
while reusing this pass's existing synthesis. If `U` is the naive compute phase,
the controlled clean copy is `S = U; controlled-output-copy; U^-1`; only the
output copy needs the dirty selector as an added control. A linear-size
multiple-control ladder implements `T`, which toggles the selector exactly when
all workspace wires are zero while restoring every borrowed wire. The final
gate sequence is `S; T; S; T`.

The `VarWireMap` reports both workspace and borrowed-wire sets so callers do
not reconstruct layout assumptions. Pure Boolean statements are supported;
storage statements fail closed in hardened mode and retain their existing
semantics in naive mode.

Cost notes (see `docs/agent-context/complexity-hints.md` conventions):
gate count ≈ (#XOR)·1 + (#AND + #OR·5-ish)·Toffoli-equivalents + #outputs;
wire count ≈ #vars + #outputs. Record actuals in the complexity-hints doc
when landed.

Correctness signal (repo rule: semantics preservation by evaluation, not IR
shape):

- Fuzz property: for random `f` and random `(x, y)`,
  `rc.apply(x ‖ y ‖ zeros) == (x, y ⊕ f(x), _)`, evaluated by running the
  original `BCircuit` on `x` and the `RCircuit` on the padded wire vector
  (new volar-fuzz property alongside Properties A–D in `docs/fuzzing.md`).
- Inverse property: applying `inverse()` then `to_reversible`'s output
  restores the original wire vector.
- Round-trip property: `BCircuit → fused → reversible` preserves behavior for
  arbitrary inputs even when the source went through DCE/CSE first.

### Work items

- [x] `fuse_to_circuit.rs` + errors + tests (3a).
- [x] Dual-API return of fused types from `lower_to_circuit`
      (`lower_to_circuit_fused`; uses the control-provenance variant so
      statement-free movfuscated blocks don't panic).
- [x] `to_reversible.rs` + wire allocator + rejection of external primitives
      (note: `StorageRead` stays rejected at synthesis time even though
      `RCircuit` can *represent* storage ops — synthesizing reversible
      storage access from Boolar storage traffic is future work).
- [x] `VarWireMap` + value→wire watchlist translation with fail-closed
      unknown-id handling.
- [x] Fuzzer property: `prop_e_to_reversible_implements_xor_embedding` in
      `crates/fuzz/volar-fuzz/src/properties/reversible.rs` (exhaustive small
      unit tests cover the same property at fixed widths; includes
      watchlist-translation checks).
- [ ] Docs: update `docs/pipeline.md` diagram tail with
      `Boolean circuit → circuit-fused → reversible` stages; add rows to the
      topic-context table in `AGENTS.md` if a new agent-context file appears
      (optional).

---

## Non-goals

- **No default behavior change.** `to_reversible` remains the low-cost naive
  transform; Bennett uncomputation and total-workspace hardening are explicit
  opt-in behavior through `ReversibleMode::Hardened`.
- **No reversible Volar-IR variant.** Field-level arithmetic has no natural
  reversible gate basis at this layer; reversibility starts at the bit level.
- **No change to movfuscation or loop-unrolling policies.** Fusion validates;
  it does not produce the single-block shape itself.
- **No weaving/garbling backend changes** (those live in the `volar` repo);
  this plan only hands them stronger types.
- **No `u128`/wide-int concerns** — orthogonal (see
  `docs/agent-context/lir-u128-support.md`).

## Risks and mitigations

| Risk | Mitigation |
|---|---|
| Duplicate single-block invariants drift between `TryFrom` impls and passes | One validator per IR; passes call it, never re-check inline |
| `Node<Stmt, P>` wrapper vs wire-mutation `RCircuit` mismatch | Keep provenance out of Phase 2; revisit on consumer demand |
| Opt passes accidentally reintroducing control flow into fused values | Fused types expose no terminator field; opt code operates through builders |
| Multiple concurrent edits to Boolar passes during Phases 1–3 | Coordinate per `docs/agent-context/boolar-ir-conflicts.md` |
| Text format churn | Ship fused/reversible printing behind the existing spec process; round-trip tests gate it |
| Watchlists drifting from the allocator's layout | `VarWireMap` is produced by the same pass run that emits the `RCircuit`; never reconstructed after the fact |
| `StorageSwap` surprising garbling consumers unaware of storage | Convenience wrapper rejects circuits containing `StorageSwap`; consumers opt in explicitly |

## Resolved decisions (formerly open questions)

1. **`Fredkin`: deferred** until a consumer needs it; **reversible storage
   (`StorageSwap`) ships in v1** instead, since spill/fetch is the concrete
   near-term need.
2. **Cross-referencing uses a watchlist system**, not per-wire names:
   user-provided value-space watchlists are translated to wire indices via
   the transform's `VarWireMap`, fail-closed on unknown ids (see Phase 3b,
   step 5).
3. **No pre-check diagnostics in `to_circuit_fused`.** Suggesting which
   missing pass produced a non-circuit input would be fragile (passes get
   reordered, new producers appear); errors report only what was structurally
   observed.

## Suggested landing order

1. Phase 1 fused types + validators + text round-trip (no behavior change).
2. Phase 3a transform; switch `lower_to_circuit` to emit fused types.
3. Phase 2 `RCircuit` with apply/inverse properties.
4. Phase 3b naive `to_reversible` + fuzz property.
