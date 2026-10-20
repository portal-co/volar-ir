# PESAT Loop Emitter: Plan

> **Status:** plan for review. Nothing in this document is implemented unless a
> section explicitly says "landed". Commit references describe existing work
> only.
>
> **Scope:** one work item — a **PESAT loop emitter**: turn the *compiled*
> body of a bounded loop (the linkable spec subset, the same artifact M4
> compiles via IR) into **one PESAT instance per loop iteration** plus
> inter-iteration linking constraints, over the marker-resolved extension
> field (`Galois128`).
>
> **The two capabilities it exists to exercise** (both landed in M4):
>
> 1. **Storage support.** The loop body reads and writes memory through the
>    cell-granular shared-stack model (volar-ir `4b5b3cc`). The emitter must
>    carry loop-carried *memory* across per-iteration instances, not just
>    loop-carried scalars.
> 2. **Volar IR support.** Each emitted instance — and, crucially, its
>    *satisfaction checker* — is itself a Volar IR / VAFFLE artifact via the
>    M4 compile-via-IR path, so an instance's correctness is provable
>    in-circuit, standalone.
>
> **Why it matters — independent proofs alongside prove-the-verifier.** The
> full prove-the-verifier composition (WARP batch accumulation + batch-top
> self-decision, `docs/warp-vole-verifier-zksnark-plan.md` §3) is a long
> critical path. The emitter breaks it: **any single loop iteration's PESAT
> instance can be emitted, satisfied, and *proven* independently** — the
> per-iteration checker compiles via IR and runs as a self-contained proof —
> so emission correctness is validated iteration-by-iteration against the
> native trace, in parallel with and unblocked by the rest of the
> prove-the-verifier effort.
>
> **Explicit directive (carried over):** no preemptive optimization.
> Correctness and statement-binding first; performance is a later, separately
> reviewed concern.

---

## 1. Position in the larger plan

This is **D1** of `docs/warp-vole-verifier-zksnark-plan.md` §7, pulled forward
now that its prerequisites are green:

| Prerequisite | State | Evidence |
|---|---|---|
| M3: `volar-spec::warp` PESAT types | landed | `WarpIndex`/`WarpConstraint`/`WarpTerm`/`WarpWitness`, `warp_index_is_valid`, `warp_evaluate_bundled` (volar-spec, 347 native tests) |
| M4: compile-via-IR of the spec WARP path | **landed** | `warp_genesis_compiles_via_ir_and_matches_native` passes end-to-end (volar `b24f4fb`); cell-granular storage (volar-ir `4b5b3cc`) |
| F-series field markers (Galois128 native) | landed | field ops survive as field ops through LIR→VAFFLE |

The upstream end-state (unchanged, context only): the woven VOLE verifier's
per-cycle step relation is emitted as PESAT and accumulated by WARP
(`acc_i = P_ACC(acc_{i-1}, cycle_i)`), with batch-top self-decision. This plan
delivers the *emitter*; the accumulation driver (D2) and batch-top
self-decision (D3) remain downstream.

### 1.1 Constraints inherited from the WARP plan

These bind the emitter's API shapes (from
`warp-vole-verifier-zksnark-plan.md` §1.4/§6 — the anti-v0 checklist):

- **C1/C2 — the constraints are the program's own arithmetic.** The emitter
  consumes the *compiled loop body* (the same IR the executor runs), never a
  re-encoding. A PESAT variable *is* a trace cell; a PESAT term *is* an
  operation the program performed. No translated relation about different
  variables.
- **B1 — program identity.** Each iteration's instance absorbs the loop
  artifact's digest (module/type-table/schedule digest) into its statement.
- **B2 — transcript binding.** The iteration's public input `x` carries the
  transcript values the body consumed; Fiat–Shamir challenges derive
  in-circuit from the same absorbed values.
- **B4 — I/O and boundary binding.** The iteration's accept bit and its
  memory boundary (§3.3) are part of the statement.
- **B5 — linking is literal.** Iteration `i`'s output state **is** iteration
  `i+1`'s input state, by equality constraints on shared boundary variables —
  not an implication through an unbound intermediate.

---

## 2. What the emitter is

```text
 compiled loop body (linkable spec subset; the M4 artifact)
   for iter in 0..TRIP { <body> }        <- one bounded loop, concrete trip count
      │
      │  emit (this plan)
      ▼
 per-iteration PESAT instances  I_0 … I_{TRIP-1}
      each: WarpIndex { constraints, public_len, witness_len, degree_bound }
            + WarpWitness { public, witness }     <- from the native trace
      │
      │  linking constraints (boundary equalities, §3.2)
      ▼
 a chain: I_0 → I_1 → … → I_{TRIP-1}
      each link checkable standalone (§4) and foldable pairwise (D2, later)
```

The emitter is a **trace-and-constrain** pass: it runs the compiled body one
iteration at a time (nativaly, on the same interpreter the M4 differential
uses), records the per-iteration trace (scalar carries, memory boundary), and
emits the body's step relation as a `WarpIndex` whose satisfying assignment is
exactly that trace. It does **not** execute the loop differently from the
executor — C2 forbids a parallel semantics.

### 2.1 Input

A compiled loop in the linkable spec subset, as already handled by
`volar-lir-codegen`'s `LoopLowering::Native` path: a header/body/latch CFG
with loop-carried scalars as block params and loop-carried aggregates
alloca-promoted through the cell-granular memory model. Concretely, the
emitter's input is the *same* parsed/lowered artifact M4 feeds to
`VaffleTarget` — the emitter is an additional consumer of that artifact, not a
new front end.

### 2.2 Output

Per iteration `i`:

- `WarpIndex<M, TERMS, DEG>` — the body's constraints (fixed across
  iterations; the *shape* is the loop body's, the iteration index enters only
  through the public input). `M` a power of two (paper requirement), with
  fail-closed validation by the existing `warp_index_is_valid`.
- `WarpWitness<K, X>` — the iteration's public `x` (transcript-absorbed
  values, boundary-in) and witness `w` (intermediate wires, boundary-out).

And between iterations: the linking constraint set (§3.2).

---

## 3. Design

### 3.1 From body operations to PESAT terms

The step relation is read off the body's operations directly (C2). Over
`Galois128` (characteristic 2), per the existing `warp_eval_constraint`
algebra:

| Body op | PESAT term(s) | Degree |
|---|---|---|
| field/bit add, xor | `out + a + b = 0` (char 2: `−` is `+`) | 1 |
| field mul, bit and | `out + a·b = 0` | 2 |
| field const | `out + c = 0` | 0 |
| select/mux | `out + b + s·(a + b) = 0` | 2 |
| booleanity of a bit wire `s` | `s·s + s = 0` | 2 |
| equality/icmp feeding a branch | the branch is *not* emitted — see below | — |

Control flow is **flattened by the trace, not encoded**: the emitter follows
the taken branch and emits only the executed path's constraints (the loop body
is straight-line per iteration by construction — `LoopLowering::Native` has
already resolved the CFG). A conditional update becomes a select term (above)
only when the compiled body itself uses a select; a taken/not-taken branch
emits the taken arm. This is the honest reading of C2: the proven statement is
the *executed* relation, with the branch condition's value in the witness.

The `degree_bound` is the body's actual maximum term degree (2 for the
boolean/field ops above; recorded, fail-closed, by `warp_index_is_valid`).

### 3.2 Linking constraints (loop-carried scalars)

Each loop-carried scalar (a native-loop block param in the compiled artifact)
is a boundary variable shared by adjacent instances: iteration `i`'s
body writes `carry_out`, iteration `i+1` reads `carry_in`. The link is a
linear equality per carried scalar:

```text
 carry_in(i+1)  +  carry_out(i)  =  0        (char 2: equality)
```

`carry_in(i+1)` is a *public* variable of instance `I_{i+1}` and
`carry_out(i)` a *witness* variable of instance `I_i`; the link is checked by
whoever consumes the chain (the D2 driver / an independent checker). The
emitter records the pairing `(carry_out var in I_i) ↔ (carry_in var in
I_{i+1})` in a per-chain manifest. B5: the link is this equality, nothing
weaker.

### 3.3 Storage support (loop-carried memory) — the point of capability 1

A loop body that writes memory (`state[pos] ^= …`, the absorb-loop shape) has
loop-carried state that is *not* a scalar. Carrying the whole memory image
across instances is quadratic and re-opens exactly the blowup M4 removed.
Instead the emitter carries a **memory boundary**, reusing the existing
in-circuit multiset-accumulator precedent
(`volar-weaver/src/storage_loop.rs`, `docs/vole-continuation-bridge.md`'s
`mem_acc` / `ResumeToken.mem_acc`):

- Per memory **write** `(addr, old, new)` in the iteration: the body already
  performs the store through cell-granular storage; the emitter adds the
  accumulator absorption as constraints (the `mem_acc_absorb_vope`
  field-arithmetic shape, expressed as PESAT terms over the trace cells).
- Per memory **read** `(addr, val)`: the read value is a witness variable;
  its consistency is enforced by the multiset equality at the boundary, not
  by re-proving the read.
- The boundary carried into the next instance is the O(1)-field-element
  accumulator pair `(mem_prod, mem_cons)` — loop-carried *scalars*, so they
  link exactly like §3.2.

This is deliberately the *carry* mechanism, with the same honest-soundness
note as the storage_loop weaver: full read-consistency additionally needs
last-write timestamps absorbed per access and a final drain at loop exit.
That bookkeeping layers on top of the accumulator carry without changing the
chain's shape; it is recorded here as a requirement (B4 makes the boundary
part of the statement) and is milestone-gated (§6, E3), not smuggled in.

The M4 storage model is what makes this *expressible*: addresses are
cell-granular and deterministic, so `(addr, value)` tuples are well-defined
field values the absorber can bind.

### 3.4 Volar IR support — the point of capability 2

Two distinct IR touchpoints:

1. **The instance checker compiles via IR.** `warp_evaluate_bundled` /
   `warp_index_is_valid` already live in `volar-spec` and compile through the
   M4 path. So "instance `I_i` is satisfied by trace `T_i`" is a statement
   whose *checker* is IR-compilable — the satisfaction proof can itself be
   run in-circuit. This is what makes the independent proof of a single
   iteration possible (§4).
2. **The emitted chain is a Volar IR artifact.** The manifest (per-iteration
   instance digests + link pairings) is a plain data structure over the same
   `volar-ir-common` types, so the chain can be serialized, diffed, and
   re-checked by the existing IR tooling (text format, provenance) rather
   than living only in emitter memory.

---

## 4. Independent proofs alongside prove-the-verifier

The composition this enables, explicitly:

```text
 prove-the-verifier (batch WARP accumulation, D2/D3)   <- long critical path
      ▲                                                      (unchanged)
      │  consumes a validated per-iteration chain
      │
 PESAT loop emitter (this plan)
      │  per-iteration instance I_i + trace T_i
      ▼
 independent per-iteration proof:
   "warp_evaluate_bundled(I_i, beta, T_i) == 0  AND  link(I_{i-1}, I_i) holds"
   — its checker compiles via IR (§3.4), so this is a self-contained
     in-circuit proof of ONE iteration, producible and verifiable on its own
```

So a single iteration — the genesis iteration, a mid-loop absorb iteration, a
finalization iteration — can be proven correct **without** the batch
accumulation existing yet. The emitter's own validation is the same property
run natively (§6 test floor): native trace vs emitted-instance satisfaction,
per iteration. Emission bugs are therefore caught at iteration granularity,
in parallel with the prove-the-verifier work, instead of surfacing only as a
batch-top mismatch at the very end.

This is the emitter's reason to exist now: it converts a monolithic
end-to-end gate into a per-iteration gate.

---

## 5. Non-goals (explicitly not this plan)

- **D2/D3** (accumulation driver, batch-top self-decision) — downstream.
- **Code/parameter selection** (reviewed linear code, `s`/`t` against the
  paper's bounds) — the WARP plan's §2 non-goal, unchanged.
- **Any optimization of the emitted constraint count** beyond reading the
  body honestly — no preemptive optimization, per the directive.
- **Timestamp/drain completeness** of the memory accumulator beyond the
  carry mechanism — recorded as E3 scope, with the soundness note carried.
- **Parallel/epoch scheduling of iteration emission** — the
  manifest/scheduling discipline exists (`vole-mpc-shared-storage-scheduling-
  plan.md`); the emitter plugs into it later, unchanged.

---

## 6. Workstreams and milestones

Per repo convention: commit after each milestone and after intra-phase
checkpoints; `volar-ir-opt` stays `#![no_std]` + `alloc`; catch-all arms on IR
matches; never specialize on test cases; **semantics preservation is the
correctness signal** (native trace vs emitted-instance satisfaction), never IR
shape assertions except where a structural invariant is the point (the
link-pairing manifest's well-formedness is such an invariant).

| M | Content | Validation gate |
|---|---|---|
| E1 | Op→term emission for the straight-line body (§3.1), scalar carries only | A fixed tiny loop (e.g. the `probe_runtime_index_write_size` absorb shape) emits an instance whose native trace satisfies it (`check_instance`), and a tampered trace is rejected. |
| E2 | Linking constraints + chain manifest (§3.2, §3.4.2) | A multi-iteration run: each link equality holds on the native trace and fails on a perturbed boundary value; the manifest round-trips through the IR text format. |
| E3 | Storage boundary: memory accumulator absorption + carry (§3.3) | A memory-writing loop (the absorb loop): the carried `(mem_prod, mem_cons)` match across every iteration boundary on the native trace; a flipped read value is caught at its boundary. Soundness note (timestamps + drain) recorded in the module doc. |
| E4 | Independent per-iteration proof (§3.4.1, §4) | The satisfaction checker for one emitted instance compiles via IR (M4 path) and evaluates to acceptance on the native trace in-circuit — a standalone in-circuit proof of one iteration, differentially against native `check_instance`. |
| E5 (review checkpoint) | Emitter design review + D2 handoff note | Reviewed note; no code required. |

Each milestone's tests extend, never special-case (repo rule 2). Each gate's
differential runs on fixed seeds, mirroring the M4 probe discipline
(`volar-lir-codegen/tests/warp_compile.rs` is the precedent for the
compile-via-IR differential harness).

---

## 7. Test floor

- **Satisfaction soundness**: honest trace accepted; trace with one flipped
  wire rejected; trace with one perturbed boundary carry rejected at the link.
- **Validation fail-closed**: non-power-of-two `M`, over-degree term,
  out-of-range variable index all rejected by `warp_index_is_valid` on
  emitter output (the emitter must never produce an invalid index — a bug
  here is a bug in the emitter, surfaced by the gate).
- **Chain integrity**: the manifest's link pairings are exactly the shared
  boundary variables, in iteration order (structural invariant — the one
  place shape assertions are permitted).
- **Independent proof**: one iteration's in-circuit satisfaction proof
  evaluates to accept (E4), on a mid-loop iteration *and* on the genesis
  iteration (the two shapes that differ most: cold start vs steady state).
- **Regression**: the M4 suite (`warp_compile.rs`) stays green; the emitter
  adds no special cases to `volar-lir-codegen`.

---

## 8. Risks and open questions

1. **Constraint-count scale.** An honest per-iteration instance of a real
   verifier cycle is large (the body is a Keccak-scale computation). This is
   the accepted in-circuit cost (WARP plan §4 W2); the emitter records counts
   but does not optimize them in this plan. The risk is eval budget in the
   E4 in-circuit proof, mitigated by the M4 interpreter throughput work.
2. **Memory-accumulator completeness.** The carry mechanism is sound; full
   read-consistency needs the timestamp/drain bookkeeping (§3.3). If E3 shows
   the bookkeeping changes the chain's shape, that is a design finding to
   bring back, not to absorb silently.
3. **Branch-flattening faithfulness.** Emitting only the executed path (§3.1)
   is correct only if the branch condition's value is bound in the witness
   and the condition's own constraints are emitted. The E1 gate must include
   a body with a data-dependent branch to pin this.
4. **Boundary-variable ordering.** The link manifest's correctness depends on
   a canonical ordering of carried scalars and boundary cells. Pin the
   ordering rule in E2 (iteration order, then declaration order) and assert
   it structurally.
