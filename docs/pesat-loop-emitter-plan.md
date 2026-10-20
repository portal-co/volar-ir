# PESAT Loop Emitter: Plan

> **Status:** plan for review. Nothing in this document is implemented unless a
> section explicitly says "landed". Commit references describe existing work
> only.
>
> **Scope:** one work item — a **PESAT loop emitter**: compile **movfuscated
> Volar IR** (a self-looping step circuit) **directly to PESAT** — one sparse
> constant-degree instance per *step* — plus inter-step linking, over the
> marker-resolved extension field (`Galois128`). On top of that:
> **endless loops** (unbounded step streams) via WARP **accumulation across
> multiple calls**, and **in-circuit decision** (WARP `decide`/`verify` run
> in-circuit, their own loops lowered to the same step-circuit level).
>
> **The two capabilities it exists to exercise** (both landed in M4):
>
> 1. **Storage support.** Loop bodies read and write memory through the
>    cell-granular shared-stack model (volar-ir `4b5b3cc`). The emitter must
>    carry loop-carried *memory* across per-step instances, not just
>    loop-carried scalars.
> 2. **Volar IR support.** The emitter's input *is* Volar IR (the movfuscated
>    step circuit), and each emitted instance — and its *satisfaction
>    checker* — is itself a Volar IR / VAFFLE artifact via the M4
>    compile-via-IR path, so an instance's correctness is provable
>    in-circuit, standalone.
>
> **Why it matters — independent proofs alongside prove-the-verifier.** The
> full prove-the-verifier composition (WARP batch accumulation + batch-top
> self-decision, `docs/warp-vole-verifier-zksnark-plan.md` §3) is a long
> critical path. The emitter breaks it: **any single step's PESAT instance can
> be emitted, satisfied, and *proven* independently** — the per-step checker
> compiles via IR and runs as a self-contained proof — so emission correctness
> is validated step-by-step against the native trace, in parallel with and
> unblocked by the rest of the prove-the-verifier effort.
>
> **Explicit directive (carried over):** no preemptive optimization.
> Correctness and statement-binding first; performance is a later, separately
> reviewed concern.

---

## 1. Position in the larger plan

This is **D1** of `docs/warp-vole-verifier-zksnark-plan.md` §7, pulled forward
now that its prerequisites are green, and **re-scoped to sit directly on Volar
IR** rather than on the spec-source loop:

| Prerequisite | State | Evidence |
|---|---|---|
| M3: `volar-spec::warp` PESAT types | landed | `WarpIndex`/`WarpConstraint`/`WarpTerm`/`WarpWitness`, `warp_index_is_valid`, `warp_evaluate_bundled` (volar-spec, 347 native tests) |
| M4: compile-via-IR of the spec WARP path | **landed** | `warp_genesis_compiles_via_ir_and_matches_native` passes end-to-end (volar `b24f4fb`); cell-granular storage (volar-ir `4b5b3cc`) |
| F-series field markers (Galois128 native) | landed | field ops survive as field ops through LIR→VAFFLE |
| Movfuscated step circuit | landed | `volar-ir-passes::movfuscate::movfuscate_ir` — any multi-block IR CFG → one self-looping step block with `(pc, state)` params |
| WARP decide/verify in spec subset | landed | `warp_decide_spec` / `warp_verify_spec` (volar-spec `warp.rs`) — bounded loops over `ArraySize` shapes |

The upstream end-state (unchanged, context only): the woven VOLE verifier's
per-cycle step relation is emitted as PESAT and accumulated by WARP
(`acc_i = P_ACC(acc_{i-1}, cycle_i)`), with batch-top self-decision. This plan
delivers the *emitter* and the *accumulation/decision composition*; the
production code/parameter selection (D4) remains downstream.

### 1.1 Constraints inherited from the WARP plan

These bind the emitter's API shapes (from
`warp-vole-verifier-zksnark-plan.md` §1.4/§6 — the anti-v0 checklist):

- **C1/C2 — the constraints are the program's own arithmetic.** The emitter
  consumes the *movfuscated step circuit* — the same Volar IR the executor
  steps through — never a re-encoding. A PESAT variable *is* a trace cell; a
  PESAT term *is* an operation the step performed. No translated relation
  about different variables.
- **B1 — program identity.** Each step instance absorbs the step circuit's
  digest (module/type-table digest) into its statement.
- **B2 — transcript binding.** The step's public input `x` carries the
  transcript values the step consumed; Fiat–Shamir challenges derive
  in-circuit from the same absorbed values.
- **B4 — I/O and boundary binding.** The step's memory boundary (§3.3) and the
  batch's final accept bit are part of the statement.
- **B5 — linking is literal.** Step `i`'s output state **is** step `i+1`'s
  input state, by equality constraints on the shared `(pc, state)` boundary
  variables — not an implication through an unbound intermediate.

---

## 2. What the emitter is

```text
 source loop / arbitrary CFG (linkable spec subset)
      │  volar-compiler → volar-lir-codegen → Volar IR (multi-block CFG)
      │  movfuscate_ir  (volar-ir-passes)     <- DIRECT compile, this plan
      ▼
 movfuscated step circuit:  one self-looping block
      step(pc, state, mem_boundary, ext_in) -> (pc', state', mem_boundary', out)
      │
      │  emit (this plan): read the step relation off the step circuit's ops
      ▼
 per-step PESAT instances  I_0 … I_{t-1}        <- one per EXECUTED step
      each: WarpIndex { constraints, public_len, witness_len, degree_bound }
            + WarpWitness { public, witness }     <- from the native trace
      │
      │  linking constraints on the (pc, state) boundary (§3.2)
      ▼
 a chain: I_0 → I_1 → … → I_{t-1}
      │  endless streams: segments accumulated across calls (§4)
      │  batch-top: decide/verify in-circuit as more steps (§5)
      ▼
 acc_t  — a running WARP accumulator over the whole (possibly unbounded) stream
```

Two deliberate choices distinguish this from a naive "unroll the loop, emit
constraints" approach:

1. **Direct from movfuscated Volar IR.** The emitter's input is the step
   circuit produced by `movfuscate_ir`, not the source-level loop. The step
   circuit is the *universal* step relation — any control flow (nested loops,
   data-dependent branches, calls already inlined) is flattened to
   `(pc, state) → (pc', state')` steps by the existing, tested movfuscation
   pass. The emitter therefore handles *arbitrary* loops, not only clean
   `for` loops, and never re-implements control flow. This is C2 made
   structural: the step circuit is what the executor runs.
2. **Per-step, not per-source-iteration.** A step is one CFG transition. A
   source-level loop iteration is a short sequence of steps. The granularity
   is finer, uniform, and independent of how the source was written — which
   is exactly what makes endless streams (§4) and in-circuit decision (§5)
   compose: everything is steps of one step circuit.

### 2.1 Input

A `Volar IR` module (`IRBlocks`) — any shape `movfuscate_ir` accepts (any
scalar block-param types; the pass adds a `Bit` type if absent). The emitter
first runs `movfuscate_ir_owned` to obtain the single self-looping step block
plus the `MovfuscAccumInfo`/`MovfuscBlockBoundary` metadata (which maps each
step's executed source block back out for tracing). The emitter consumes that
step block.

### 2.2 Output

Per executed step `i`:

- `WarpIndex<M, TERMS, DEG>` — the step circuit's constraint set (fixed shape
  across steps; the step's identity enters through the public input — the
  current `pc` selects which block's relation is active). `M` a power of two
  (paper requirement), validated fail-closed by `warp_index_is_valid`.
- `WarpWitness<K, X>` — the step's public `x` (current `pc`, transcript-
  absorbed external inputs, boundary-in) and witness `w` (the step's
  intermediate wires, boundary-out state).

And across steps: the `(pc, state)` linking constraints (§3.2) and the memory
boundary (§3.3). The whole chain folds into a running accumulator (§4).

---

## 3. Design

### 3.1 From step-circuit operations to PESAT terms

The step circuit is a single straight-line block over the flattened
`(pc, state, ext_in)` wires, with the active block's relation selected by
`pc`. Reading it off directly (C2), over `Galois128` (char 2):

| Step-circuit op | PESAT term(s) | Degree |
|---|---|---|
| `Const` | `wire + c = 0` | 0 |
| `Poly` (GF(2) add/mul — the movfuscated step's arithmetic) | one term per monomial: `… + ∏(factor bits) = 0` | ≤ poly degree |
| `Merge`/`Shuffle`/`Splat`/`Transmute` (bit moves) | wire equalities | 1 |
| `Rol`/`Ror` | wire equalities after rotation | 1 |
| select on `pc` (the block dispatch) | `out + b + sel·(a + b) = 0`, plus booleanity `sel·sel + sel = 0` | 2 |
| field ops (marker-resolved `Galois128`) | native field terms — never bit-exploded (F5) | ≤ 2 |
| `StorageRead`/`StorageWrite` | memory-boundary absorption (§3.3) | ≤ 2 |

The step circuit is straight-line by construction (movfuscation resolved the
CFG into `pc`-selection), so there is no control-flow encoding problem: the
dispatch is a `pc`-indexed select, whose selector's booleanity is constrained
and whose selected value is bound in the witness. The `degree_bound` is the
step circuit's actual maximum term degree, recorded and checked fail-closed by
`warp_index_is_valid`.

### 3.2 Linking constraints (the `(pc, state)` boundary)

The step circuit's loop-carried state is exactly its `(pc, state)` params and
`(pc', state')` outputs. The link between consecutive steps is a linear
equality per state wire:

```text
 state_in(i+1)  +  state_out(i)  =  0      (char 2: equality)
 pc_in(i+1)     +  pc_out(i)     =  0
```

`state_in`/`pc_in` are *public* in instance `I_{i+1}`; `state_out`/`pc_out`
are *witness* in `I_i`. The emitter records the pairing in a per-chain
manifest (§3.4). B5: the link is this equality on the shared boundary, nothing
weaker. Because the state register is the *entire* loop-carried state
(movfuscation's whole point), this boundary is complete — there is no
loop-carried scalar that escapes it.

### 3.3 Storage support (loop-carried memory) — capability 1

A step that touches memory has loop-carried state that is not a scalar. The
emitter carries a **memory boundary**, reusing the in-circuit multiset-
accumulator precedent (`volar-weaver/src/storage_loop.rs`,
`docs/vole-continuation-bridge.md`'s `mem_acc` / `ResumeToken.mem_acc`):

- Per `StorageWrite` `(addr, old, new)`: the step's store through
  cell-granular storage is recorded; the emitter adds the accumulator
  absorption as PESAT terms over the trace cells (the `mem_acc_absorb_vope`
  field-arithmetic shape).
- Per `StorageRead` `(addr, val)`: the read value is a witness variable; its
  consistency is enforced by the multiset equality at the boundary, not by
  re-proving the read.
- The boundary carried into the next instance is the O(1)-field-element
  accumulator pair `(mem_prod, mem_cons)` — loop-carried *scalars*, linking
  exactly like §3.2.

Honest-soundness note (carried from the storage_loop weaver): full
read-consistency additionally needs last-write timestamps absorbed per access
and a final drain at stream end. That bookkeeping layers on the carry without
changing the chain's shape; it is milestone-gated (E3), not smuggled in. The
M4 cell-granular addressing is what makes the `(addr, value)` tuples
well-defined field values the absorber can bind.

### 3.4 Volar IR support — capability 2

1. **The step's satisfaction checker compiles via IR.**
   `warp_evaluate_bundled` / `warp_index_is_valid` live in `volar-spec` and
   compile through the M4 path, so "instance `I_i` is satisfied by trace
   `T_i`" is a statement whose checker is IR-compilable — the satisfaction
   proof runs in-circuit. This is what makes a single step's independent proof
   possible (§4/§6 E5).
2. **The emitted chain is a Volar IR artifact.** The manifest (per-step
   instance digests + link pairings) is plain data over `volar-ir-common`
   types, serializable and re-checkable by existing IR tooling (text format,
   provenance), not only in emitter memory.
3. **The step circuit is Volar IR.** The emitter never leaves the IR ecosystem
   — the input CFG, the step circuit, and the checker are all the same
   artifact family, so the differential harness (evaluate before/after,
   compare) applies uniformly.

---

## 4. Endless loops via accumulation (multiple calls)

An endless loop is an **unbounded step stream**. WARP's accumulation is exactly
the tool for folding a stream into a constant-size running statement, and the
continuation model (`docs/vole-continuation-bridge.md`) is what lets it span
multiple calls:

```text
 call 1: steps 0..s1      -> acc_{s1},   ResumeToken(acc, pc, state, mem_acc)
 call 2: steps s1..s2     -> acc_{s2},   ResumeToken(acc, pc, state, mem_acc)
   ...        (the stream may be genuinely unbounded)
```

- **Multiple verifier calls = segments.** Each call runs a segment of steps,
  folds each step's instance into the running accumulator
  (`acc = P_ACC(acc, step)`), and returns a continuation carrying
  `(acc, pc, state, mem_acc)`. A later call resumes from the token — the
  `(pc, state)` boundary (§3.2) and the memory accumulator (§3.3) are
  precisely the continuation's payload, so resumption binds the new segment to
  the old without re-execution.
- **No a-priori trip count.** Because the per-step instance shape is fixed and
  the linking is step-local, the stream length need not be known in advance.
  This is what "endless loop" means here: not an unrolled finite loop but a
  foldable stream.
- **Accumulator validity at every boundary.** After each call, the running
  accumulator is a well-formed statement about the stream so far — so a
  *partial* stream is already independently checkable (§4's independent-proof
  property holds at every segment boundary, not just at a final halt).

---

## 5. Decision on top: Decide and Verify in-circuit

Batch-top self-decision (`warp_decide_spec` / `warp_verify_spec`) closes the
stream: it checks the final accumulator against the committed statement. Per
this plan's directive, decision runs **in-circuit**, and its own loops lower
to the **same step-circuit level**:

- **Decide/verify compile to Volar IR and movfuscate like any other
  program.** `warp_decide_spec`'s internals — the Merkle-root recomputation
  loops (`warp_merkle_root`/`warp_commit_codeword`), the MLE evaluations
  (`warp_mle_eval`), the `eq`-weight loop (`warp_eq_evals`), and the
  `for i in 0..ELL { for j in 0..LEN { … } }` double loop — are bounded loops
  over `ArraySize` shapes in the spec subset. They compile via the M4 path to
  a multi-block Volar IR CFG, then `movfuscate_ir` lowers them to *their own*
  step circuit. Their loops thus **translate to lower-level loops** — the
  same step relation the emitter already handles — rather than needing a
  bespoke constraint encoding.
- **Decision steps join the same stream.** The decide/verify computation is
  emitted as steps and accumulated like any other, so the batch-top decision
  is *part of* the accumulated relation (B5-complete: the decided statement
  is literally "the verifier accepted this transcript"). This is the WARP
  plan's D3, expressed in the step-circuit frame.
- **Fiat–Shamir in-circuit.** The challenges the decider re-derives are
  computed in-circuit from the absorbed transcript (B2), so the decision binds
  the actual transcript, not an ideal one.

---

## 6. Independent proofs alongside prove-the-verifier

The composition this enables, explicitly:

```text
 prove-the-verifier (batch WARP accumulation + self-decision)  <- long path
      ▲                                                            (§4/§5)
      │  consumes a validated step chain
      │
 PESAT loop emitter (this plan)
      │  per-step instance I_i + trace T_i
      ▼
 independent per-step proof:
   "warp_evaluate_bundled(I_i, beta, T_i) == 0  AND  link(I_{i-1}, I_i) holds"
   — the checker compiles via IR (§3.4), so this is a self-contained
     in-circuit proof of ONE step, producible and verifiable on its own
```

So a single step — the genesis step, a mid-stream absorb step, a decider step —
can be proven correct **without** the batch accumulation existing yet. The
emitter's own validation is the same property run natively (§8 test floor):
native trace vs emitted-instance satisfaction, per step. Emission bugs are
caught at step granularity, in parallel with the prove-the-verifier work,
instead of surfacing only as a batch-top mismatch at the very end.

This is the emitter's reason to exist now: it converts a monolithic
end-to-end gate into a per-step gate.

---

## 7. Non-goals (explicitly not this plan)

- **D4** (reviewed code/parameter selection, soundness parameter derivation,
  serialization) — downstream.
- **Any optimization of the emitted constraint count** beyond reading the step
  circuit honestly — no preemptive optimization, per the directive.
- **Timestamp/drain completeness** of the memory accumulator beyond the carry
  mechanism — recorded as E3 scope, with the soundness note carried.
- **Parallel/epoch scheduling of segment emission** — the
  manifest/scheduling discipline exists
  (`vole-mpc-shared-storage-scheduling-plan.md`); the emitter plugs into it
  later, unchanged.
- **Changing `movfuscate_ir` itself** — the emitter consumes its existing,
  tested output; any movfuscation enhancement is a separate reviewed change.

---

## 8. Workstreams and milestones

Per repo convention: commit after each milestone and after intra-phase
checkpoints; `volar-ir-opt` stays `#![no_std]` + `alloc`; catch-all arms on IR
matches; never specialize on test cases; **semantics preservation is the
correctness signal** (native trace vs emitted-instance satisfaction), never IR
shape assertions except where a structural invariant is the point (the
link-pairing manifest's well-formedness is such an invariant).

| M | Content | Validation gate |
|---|---|---|
| E1 | Step-circuit → PESAT emission (§3.1): movfuscate a small CFG, read the step relation off the step block | **landed** (volar `8a0bd06`): a movfuscated 2-block pipeline emits per-step instances; honest traces satisfy (`check_instance`), tampered rejected, C2 differential vs the reference interpreter. |
| E2 | `(pc, state)` linking + chain manifest (§3.2, §3.4.2) | **landed** (volar `faf05ea`): a 4-step chain links per-wire; a perturbed boundary wire breaks the link; the manifest round-trips its canonical text form (the manifest is emitter metadata, not an IRBlocks, so the IR text format is not a fit — canonical text instead). |
| E3 | Storage boundary: memory-accumulator absorption + carry (§3.3) | **landed** (volar `5cf7041`): a write+read memory loop carries `(mem_prod, mem_cons)` across the boundary; a flipped read is caught by the absorption constraint; a perturbed accumulator boundary breaks the chain. Timestamp/drain simplification documented in `emit.rs`. |
| E4 | Endless stream via accumulation across calls (§4) | **landed** (volar `507a6c2`): a 6-step stream accumulates segment-transparently (single-shot == two-call resumed) via the `StreamAccumulator` token; a 3-step prefix's accumulator is well-formed. Uses the Vec-based native oracle (`volar-iop::warp`). |
| E5 | Independent per-step proof (§3.4.1, §6) | **landed** (volar `ea992e1`): a minimal self-loop step instance is emitted, flattened (`flatten_index`), and its satisfaction relation checked in-circuit via the M4 path — honest trace accepted, tampered rejected, differential vs native `check_instance`. Caveat: drives the relation over flat arrays, not the canonical `warp_is_instance_satisfied` — see the registration finding below. |
| E6 | In-circuit decision (§5): decide/verify loops → step circuit | **landed** (volar `d086856`): a toy accumulator (genesis over the emitted self-loop instance) is marshalled as flat input arrays, reconstructed in-circuit into a const-generic `WarpAcc`, and decided by `warp_decide_spec` compiled via the M4 path — in-circuit verdict matches native, differentially. Needed a real codegen fix: aggregate `==`/`!=` now reduce to a scalar bool (AND of element-wise Eq / OR of element-wise Ne). The test is #[ignore]d as correct-but-slow (~25 min: full warp.rs parse + native-loop lowering + in-circuit Merkle/Keccak eval). |
| E7 (review checkpoint) | Emitter design review + D4 handoff note | Reviewed note; no code required. |

### The E5/E6 const-generic bridge (finding from E4/E5)

The emitter produces **runtime-sized** instances (`PesatIndex` with `Vec`
constraints; the step block's wire count fixes the sizes at emit time). The
in-circuit WARP checker and decider (`volar-spec`'s `warp_*_spec`) are
**const-generic** (`WarpIndex<M, TERMS, DEG>`, `Array<Galois128, N>`), so
running an emitted instance's satisfaction check or decision in-circuit
requires its sizes as compile-time typenum constants. Two consequences:

- The emitted instance's `M` is already a power of two (the emitter pads), and
  its `witness_len` is padded to a power of two (E4, for the linear code), so
  the sizes are well-defined per step circuit — but they must be read off the
  step block and instantiated as typenum constants, and the constraint data
  (coeffs + variable indices) must reach the in-circuit driver.
- The clean path is for the emitter to also emit a **spec-source driver** with
  the (fixed) step relation baked in as constant data and the witness as the
  only input — the step relation is identical across steps, so one generated
  driver serves the whole stream. This is the E5/E6 enabling work.

The in-circuit *machinery* is not in question (the M4 genesis gate compiles
and runs `check_instance`/`warp_evaluate_bundled` via IR); the bridge is.

**Correction (E5 follow-up)**: there is no const-generic registration bug.
The E5 "struct not in registry" failure was a *harness* issue — the driver was
parsed with `parse_driver_only` (driver source only), so the canonical checker
`warp_is_instance_satisfied` was never in-module, never planned, and its
`WarpIndex` never registered. Parsed with `parse_with_driver` (which includes
the full `warp.rs`), the canonical checker compiles and accepts the honest
step trace in-circuit (proven by the #[ignore]d `probe_e5_canonical_check`,
~14 min). E5's committed gate uses the flat-array relation for speed (~110s).

So **E6 is not blocked on a compiler bug** — it needs (a) `parse_with_driver`,
and (b) marshalling a toy `WarpAcc` (instance + witness + Merkle tree) as
in-circuit input. The remaining cost is compile/eval time of the large decider
(`warp_decide_spec`: Merkle root recompute, MLE, eq-evals, nested loops) —
sizeable but on the proven M4 native-loop path.

Each milestone's tests extend, never special-case (repo rule 2). Each gate's
differential runs on fixed seeds, mirroring the M4 probe discipline
(`volar-lir-codegen/tests/warp_compile.rs` is the precedent for the
compile-via-IR differential harness, and the movfuscation differential tests
in `volar-ir-passes` for the step-circuit shape).

---

## 9. Test floor

- **Satisfaction soundness**: honest trace accepted; one flipped wire
  rejected; one perturbed boundary wire rejected at the link.
- **Validation fail-closed**: non-power-of-two `M`, over-degree term,
  out-of-range variable index all rejected by `warp_index_is_valid` on emitter
  output (an invalid index is an emitter bug, surfaced by the gate).
- **Chain integrity**: the manifest's link pairings are exactly the shared
  `(pc, state)` boundary wires, in step order (structural invariant — the one
  place shape assertions are permitted).
- **Segment associativity (endless)**: accumulating `steps a..b` then `b..c`
  with a ResumeToken equals accumulating `a..c` in one shot (E4's core
  property).
- **Independent proof**: one step's in-circuit satisfaction proof evaluates to
  accept (E5), on a mid-stream step *and* on the genesis step (cold start vs
  steady state differ most).
- **Decision**: the in-circuit decider's verdict matches the native
  `warp_decide_spec` verdict on a shared toy chain, accept *and* reject
  (tampered) cases (E6).
- **Regression**: the M4 suite (`warp_compile.rs`) and the `volar-ir-passes`
  movfuscation differential suites stay green; the emitter adds no special
  cases to `volar-lir-codegen` or `movfuscate`.

---

## 10. Risks and open questions

1. **Constraint-count scale.** A step of a Keccak-scale body is large; the
   step circuit's `pc`-dispatch select adds a per-step mux over all blocks.
   This is the accepted in-circuit cost (WARP plan §4 W2); counts are recorded,
   not optimized, in this plan. The E5 in-circuit proof's eval budget is the
   real risk, mitigated by the M4 interpreter throughput work.
2. **Memory-accumulator completeness.** The carry mechanism is sound; full
   read-consistency needs the timestamp/drain bookkeeping (§3.3). If E3 shows
   it changes the chain's shape, that is a design finding to bring back, not
   to absorb silently.
3. **`pc`-dispatch faithfulness.** The step circuit selects the active block's
   relation by `pc`; the emitter must constrain the selector's booleanity and
   bind the selected value in the witness, or a step could claim a different
   block executed. E1 must include a multi-block CFG (a data-dependent branch)
   to pin this.
4. **Segment-boundary canonical ordering.** The continuation's
   `(pc, state, mem_acc)` payload ordering must be canonical (step order, then
   slot order) or resumed segments misalign. Pin the ordering rule in E2/E4
   and assert it structurally.
5. **Decide-loop blowup.** The decider's own loops (Merkle over `NODES`, MLE
   over `N`) are the largest loops in scope; their step-circuit translation
   (E6) is where constraint counts peak. If E6 shows the in-circuit decider is
   infeasible at toy scale, that gates the whole self-decision composition and
   must be reported before further investment.

---

## 11. E7 review + D4 handoff

All six build milestones (E1–E6) are landed and differentially validated; this
section is the E7 design review and the handoff to D4 (production
code/parameter selection, soundness derivation, serialization).

### 11.1 What the emitter delivers

- **E1** — a movfuscated Volar IR step circuit emits one PESAT instance per
  executed step over `Galois128` (one relational constraint per output bit +
  booleanity per wire, padded to a power of two).
- **E2** — `(pc, state)` inter-step linking with a canonical manifest.
- **E3** — a storage boundary: `StorageRead`/`StorageWrite` absorbed into a
  `(mem_prod, mem_cons)` multiset accumulator carried across the boundary.
- **E4** — endless streams via resumable WARP accumulation (native oracle),
  segment-transparent.
- **E5** — an independent per-step satisfaction proof, checked in-circuit.
- **E6** — in-circuit decision: `warp_decide_spec` compiles via the M4 path and
  its verdict matches the native decider, differentially.

### 11.2 Status of the §10 risks

1. **Constraint-count scale** — recorded, not optimized (per the directive).
   E6's in-circuit decider runs in ~25 min at toy scale; the cost driver is the
   Merkle/Keccak eval, which scales with `N`/`NODES`/`M`. Feasible; production
   sizing is a D4 concern.
2. **Memory-accumulator completeness** — the carry mechanism is landed and
   differentially validated, but the challenges are **deterministic
   placeholders, not Fiat-Shamir-bound to a program digest**, and full
   read-consistency (per-access timestamps + final drain) is not implemented.
   This is the emitter's one real soundness gap; D4 must close it before any
   real proof (see §11.3).
3. **`pc`-dispatch faithfulness** — retired: E1/E2 use multi-block CFGs with
   data-dependent branches, and the emission constrains the selector's
   booleanity and binds the selected value in the witness.
4. **Segment-boundary canonical ordering** — retired: the boundary is derived
   at emission time (ordered `pc`, then `state`, then the accumulators) and
   packaged by `chain_manifest`; E4's segment-transparency gate validates it.
5. **Decide-loop blowup** — retired at toy scale: E6 shows the in-circuit
   decider is feasible. Production scale remains a D4 sizing concern, not a
   correctness blocker.

### 11.3 D4 handoff — what remains before real proofs

1. **Fiat-Shamir binding.** Replace the E3 placeholder challenges with
   challenges derived from a Fiat-Shamir transcript over the program digest and
   the public boundary, so a cheating prover cannot choose the accumulator
   challenges.
2. **Memory read-consistency.** Add per-access timestamps and the final drain
   so the multiset accumulator actually enforces that every read returns the
   last write to that address (§3.3). The carry shape is in place; the
   bookkeeping is not.
3. **Parameter/soundness derivation.** Derive soundness parameters and pick
   production sizes (`M`, `N`, `NODES`, code parameters) — the in-circuit
   decider cost scales with these, and the toy-scale numbers do not transfer
   directly.
4. **Serialization.** A canonical, versioned encoding of the emitted instance,
   manifest, and accumulator for transport between prover and verifier.

None of these change the emitter's architecture; they are the production
hardening D4 owns. The emitter's correctness signal throughout was
**differential validation against the native spec/oracle** (C2), which every
milestone gate exercised.
