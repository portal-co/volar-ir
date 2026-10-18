# WARP-in-`volar-spec` + Field Acceleration: Plan for a WARP-Accumulated zkSNARK of the VOLE Verifier

> **Status:** plan for review. Nothing in this document is implemented unless a
> section explicitly says "landed". Commit references describe existing work
> only.
>
> **Scope:** two work items —
>
> 1. Move WARP into `volar-spec`, with the necessary code changes to compile
>    via IR.
> 2. Support, through LIR, extension/prime-field acceleration in the AST
>    (same marker system) compiled to VAFFLE.
>
> **Motivating end goal (context, planned here only as far as it constrains
> the two items):** run the VOLE verifier, woven, in a WARP reduction — with a
> PESAT emitter — which self-decides at the top of a batch of verifier cycles,
> producing a proper zkSNARK of the VOLE verifier. Prove-the-verifier, but on
> published prior art instead of a bespoke construction.
>
> **Explicit directive:** no preemptive optimization anywhere in this plan.
> Correctness and statement-binding first; performance is a later, separately
> reviewed concern.

---

## 1. Failure history this plan is written against

The plan exists because three successive prove-the-verifier constructions each
proved less than "our program's correlations hold". Each failure is evidence
for a design constraint in §6.

### 1.1 v0 — Nova fold with a `GF(2^k) → F_ℓ` lift (archived, deleted)

`docs/archive/prove-the-verifier.md`, `docs/archive/fold-lift-expansion.md`,
`docs/agent-context/gf2k-to-fell-embedding.md` (volar repo). The woven VOLE
verifier's per-gate check `K_a·K_b + V̂ = K_c·Δ` over the binary field was
embedded into the Nova scalar field `F_ℓ`. The one-scalar `FoldLift` was
**unsound** (pinned as a known-failing assert in `e2e_fold_verifier.rs`); the
spaced-packing expansion had a written soundness argument but stayed a Tier-3
cryptographic seam, and the whole path was deleted as subsumed.

The failure mode, stated plainly: the construction proved that **some** VOLE
correlation exists, not necessarily **our program's** correlations — the
embedding did not faithfully capture the actual `GF(2^k)` arithmetic the
verifier executed, so a satisfied folded instance did not imply the real
verifier accepted the real transcript.

### 1.2 v1 — bespoke native `GF(2^k)` IOP (live; `volar-iop`)

`docs/prove-the-verifier-iop.md`; `crates/iop/volar-iop/src/{fold,ligero}.rs`.
Native-field per-gate relaxed-R1CS fold plus a Merkle+Fiat–Shamir
finalization. This fixed the field-embedding problem (no lift; the tower field
of `field.rs` contains the VOLE field canonically) but it is a **bespoke
construction** — the fold schedule, the sizing formula (`n` = next power of
two ≥ 2k, `q = k + 10` is "a reasonable first choice, not a derived-and-proven
soundness bound" per its own doc), and the one-shot proximity test have no
published analysis behind them.

### 1.3 v1.5 — `spartan-whir` adapter over KoalaBear (cirrus WIP)

`cirrus-volar-vole/src/{vole_verifier_relation.rs,spartan_whir_adapter.rs}`.
Two disqualifying properties, per its own module docs:

- **Field mismatch.** "characteristic-two field arithmetic cannot be embedded
  into KoalaBear" — the relation does not support the binary-field `GF(2^k)`
  VOLE traces we actually produce. We use binary extension fields; a prime
  field that cannot represent them is the wrong field, full stop.
- **Ideal setup.** "It assumes an ideal/native VOLE setup transcript for
  `V_w`; proving that setup is outside this slice." Our system must support
  **actual interactive proofs** — the ML-KEM/Ferret setup is a real
  interactive protocol whose verifier-side execution must be part of the
  proven computation, explicitly **not** an ideal assumption.

### 1.4 Constraints extracted from the failures

| # | Constraint | Failed by |
|---|---|---|
| C1 | The proven statement must range over the **native** binary extension field — no cross-field embedding, no field the trace cannot live in. | v0, v1.5 |
| C2 | The accumulated statement must be **the program's own step relation**, bound to program identity, inputs, and transcript — not a generic correlation shape. | v0 |
| C3 | The construction must be **published prior art** with a written soundness analysis, not bespoke. | v1 |
| C4 | **Interactive phases are part of the proven computation** — VOLE setup is woven and proven, never assumed ideal. | v1.5 |
| C5 | ZK discipline: the outer proof only needs soundness over public/committed data (`docs/prove-the-verifier-iop.md` §1), but the discipline boundary stays compile-time enforced (`volar-discipline`). | — (keep) |

---

## 2. Why WARP is the right prior art

WARP — Bünz, Chiesa, Fenzi, Wang, *Linear-Time Accumulation Schemes*, IACR
ePrint 2025/753 (TCC 2025) — is pinned in `volar/docs/research/
warp-paper-pinning.md` (paper + full text archived, sha256 recorded) and
implemented as a construction-faithful algebraic reference in
`crates/iop/volar-iop/src/warp.rs` (volar commits `82b23ae`, `417320a`,
`acc21f0`; 9 focused WARP tests + 49/49 crate suite, unlocked runs).

Fit against §1.4:

- **C1.** WARP works with any linear code over any sufficiently large field
  (paper, Theorem-level requirement: `n`, `M`, `ℓ`, `1+s+t` powers of two;
  field size enters only the soundness bounds). Our tower `Gf128` is a native
  binary extension field — the same field family the VOLE runs over
  (`Galois128` = GF(2^128)). VOLE-over-extension-field fits directly.
- **C2.** WARP accumulates **PESAT** instances — sparse constant-degree
  polynomial constraints. The VOLE verifier's per-cycle checks are degree-2
  (`K_a·K_b + V̂ = K_c·Δ` in characteristic 2) plus boolean/memory-boundary
  constraints — natively PESAT-shaped, no R1CS translation gap, no embedding.
- **C3.** Published, peer-reviewed-venue paper with explicit soundness
  theorems (incl. the mutual-correlated-agreement analysis our implementation
  note records). The AGPL-3.0 `willow-network/warp-folding` reference repo was
  used for API/test understanding only; our port is written from the CC-BY
  paper text (licensing boundary recorded in the pinning doc).
- **C4.** WARP is itself an interactive IOR compiled by Fiat–Shamir. The
  accumulated computation can include the verifier's view of the interactive
  VOLE setup (ML-KEM/Ferret) as ordinary PESAT constraints — transcripts are
  inputs, challenges are derived in-circuit from the same Fiat–Shamir
  schedule, nothing is ideal.

Deliberate non-goals carried over from the paper and the reference review
(**not** to be "fixed" preemptively): no parallel-repetition soundness hacks,
no heuristic small fields, no production code/parameter selection in this
plan. The current port's `IdentityCode`/`RepetitionCode` fixtures carry no
proximity-soundness claim; choosing a reviewed code (e.g. an Orion-class
Spielman code) and parameters `s`, `t` against the paper's bounds is a
separate, later, explicitly-reviewed step.

---

## 3. End-state architecture (why the two items exist)

```text
 VOLE verifier (volar-spec source: ML-KEM/Ferret setup verification
                + per-cycle hat checks + memory/trace boundary)
      │  volar-compiler parse → IrModule AST
      │  field-accelerated lowering (item 2): AST → LIR → VAFFLE/Volar IR
      ▼
 woven verifier (volar-weaver), Transparent discipline, cycles unrolled
      │  PESAT emitter (downstream phase, §7): each cycle's step relation
      │  → sparse constant-degree constraints over the extension field,
      │    statement bound to program digest + transcript + I/O
      ▼
 WARP accumulation (volar-spec WARP, item 1): one PESAT instance per cycle,
      acc_i = P_ACC(acc_{i-1}, cycle_i)
      │  at the top of a batch of cycles: D_ACC self-decision — the batch's
      │  opening checks are themselves expressed in the accumulated relation
      │  (requires the WARP decider/verifier path to compile via IR: item 1)
      ▼
 zkSNARK of the VOLE verifier: "this verifier program accepted this
 transcript" — succinct, re-checkable, soundness-only (C5), no bespoke fold.
```

Both work items are prerequisites on this path:

- **Item 1** puts WARP where it can be both the native reference *and* an
  IR-compilable artifact (single source of truth), which is what makes the
  batch-top self-decision expressible in the accumulated relation.
- **Item 2** is what makes the woven verifier (and the WARP decider, when
  woven) cheap enough to accumulate honestly: extension/prime-field arithmetic
  rides through the pipeline as native field operations instead of
  bit-decomposed circuits — and it keeps the arithmetic *identical* to the
  native spec arithmetic (C2), because the same marker-resolved field type
  drives both.

---

## 4. Item 1 — Move WARP into `volar-spec`, compiling via IR

### 4.1 Current state

`crates/iop/volar-iop/src/warp.rs` (~1.7k lines): generic Rust over
`field::Field`, `LinearCode<F>`, `transcript::FromBytes`; `alloc::vec::Vec`
everywhere; `Gf128` tower from `field.rs`; SHAKE128 `IopTranscript`;
SHA3-256 domain-separated Merkle. Implements Construction 10.4 with
Constructions 5.10/6.3/7.2/8.2 inlined; power-of-two enforcement for `M`,
`ℓ`, `n`, `r = 1+s+t`; the documented transcript-order pinning
(`rt₀ → τᵢ → μᵢ → (τ, ω)`, verifier's printed order; `beta = x || tau`).
This module **stays** as the native oracle; the spec module is cross-tested
against it (the `volar-spec/src/fold/mod.rs` ↔ `volar-fold` precedent:
spec written in the linkable subset, "cross-tested against the oracle").

### 4.2 Target

`volar-spec/src/warp.rs` (plus `pub mod warp;` in `volar-spec/src/lib.rs`),
written in the **linkable spec subset** that `volar-compiler`'s parser accepts
and `volar-lir-codegen` lowers: total, bounded loops; fixed shapes where the
dialect requires; generics only in the supported form (operator-bounded type
parameters, per the `fold/mod.rs` precedent: `S: Clone + Add + Mul + Sub`);
no unsupported macros/constructs (`parser.rs` rejects with
`CompilerError::Unsupported` — the port is driven by compiling, not by
reading the rejection list).

### 4.3 Workstreams

**W1. Spec-dialect field layer.** Re-express the tower field and the PESAT
arithmetic over the marker-resolved field types of item 2 (§5), so the same
type drives native execution and IR compilation. The existing
`volar-iop::field::{BetaOf, Ext}` zero-sized marker pattern is the model
(each tower level names a marker carrying its verified `β`). Where the
dialect cannot express the marker generics directly, the spec module
monomorphizes concrete field modules (`gf128`-style), still marker-named, so
the IR side resolves the same field spec.

**W2. Spec-dialect transcript and Merkle.** SHAKE128 transcript and SHA3-256
Merkle against `volar-spec`'s existing `sha3`/`digest` usage
(`faest::transcript`'s `FaestTranscript` is the in-crate precedent the IOP
transcript was already modeled on). In-circuit hashing cost is a **known,
accepted cost** — not to be optimized in this plan. The transcript-order
pinning from the IOP port carries over verbatim, with the same doc note.

**W3. PESAT index types.** Sparse constant-degree `PesatIndex` /
`PesatConstraint` / `PesatTerm` in the spec subset, with the same fail-closed
validation (`MalformedIndex`, `NonPowerOfTwo`, degree-bound checks) and the
same witness shape (`x || w`).

**W4. Accumulation functions.** `warp_genesis`, `warp_prove`, `warp_verify`,
`warp_decide` in the spec subset. `warp_verify` and `warp_decide` are the
pieces that must compile via IR for batch-top self-decision; `warp_prove`
must *also* live in the spec module (single source of truth, and the
decider-at-batch-top composition reuses its combine/eval algebra) even though
the prover normally runs natively. All four keep the paper's shape; no
asymptotic table-maintenance optimizations (the module already declares this
bias).

**W5. Linear code.** Keep the generic `LinearCode` trait in the native
oracle; in the spec subset, start with the same construction-faithful
identity/repetition fixtures so the differential tests are apples-to-apples.
A reviewed production code is out of scope here (§2 non-goals); the trait
seam is what the later step plugs into.

**W6. Compile-via-IR validation.** Drive the spec module through
`volar-compiler` parse → `volar-lir-codegen` → LIR → Volar IR/VAFFLE (the
`VolarIrTarget` path) and execute both natively and via IR on identical
inputs; per this repo's design rule, the correctness signal is
**semantics preservation** — evaluate before/after and compare — never IR
shape assertions except where a structural invariant is the point (e.g.
"field ops survived as field ops", §5 F5).

### 4.4 Ported test floor (from the IOP oracle)

Two-fresh accumulation; prior-accumulator folding; public inputs +
multi-constraint bundling; identity and repetition codes; unsatisfied-PESAT
rejection; tampered sumcheck rejection; tampered Merkle path rejection;
tampered output-root rejection; decider witness/codeword mismatch. Plus, in
the spec module: native-vs-oracle differential runs of all of the above on
shared fixed seeds.

### 4.5 Explicitly not in item 1

Production code selection; parameter selection `(s, t)` against the paper's
soundness bounds; proof serialization format; any performance work. Each is a
later reviewed step with its own note.

---

## 5. Item 2 — Extension/prime-field acceleration in the AST (marker system), through LIR, to VAFFLE

### 5.1 Layer-by-layer current state

| Layer | Where | Today |
|---|---|---|
| AST | `volar-compiler/src/ir.rs` `PrimitiveType` | Hardcoded `Bit`, `Galois` (GF(2^8)), `Galois64`, `Galois128`, `Galois256`, `Z3` (GF(3)); no general extension field, no general prime field. |
| LIR codegen | `volar-lir-codegen/src/structs.rs` | Maps the hardcoded primitives to `LirType::ExtField` (hardcoded irreducibles `[0,1,3,4,8]`, `[0,1,3,4,64]`, `[0,1,2,7,128]`, `[0,2,5,10,256]`) and `LirType::PrimeField` (`Z3` → `2^2−1`). |
| LIR types | `volar-lir/src/lib.rs` `LirType` | **Already general:** `Native(NativeType)`, `ExtField { wrapped, degree, irreducible }`, `PrimeField { k, n }`. |
| IR types | `volar-ir-common` `IrType` | **Already general:** `ExtField { wrapped: TypeId, degree, irreducible }` with fail-closed interning (`field.rs`: monic, irreducible, coefficient-range checks; `ExtFieldError`), `PrimeField { k, n }` with canonical-`2^k−n` primality interning (`prime.rs`: `PrimeFieldError`). |
| IR field ops | `volar-ir-common/field.rs` | `FieldOp::{Add, Mul, Pack, Unpack}`, `FieldSpec`, `parse_field_symbol` (`volar.field.<op>.d<deg>.<wrapped>.p<hex>`), `embed_xor`/`embed_and`. |

So the general field machinery exists from LIR downward; the gap is the AST
and its lowering: only the six hardcoded primitives can name a field.

### 5.2 The marker system (the "same marker system")

Three existing marker patterns define the house style:

- `volar-discipline`: zero-sized `Zk` / `Transparent` markers carried in the
  type (`Tagged<Z, T>`), with `NonZk` bounds enforced at compile time and a
  runtime mirror enum for dynamic boundaries.
- `volar-iop::field::BetaOf<F>`: one zero-sized marker per tower level
  carrying a verified constant, precisely because "each level can name a
  distinct zero-sized marker type carrying its own verified constant".
- `volar-ir-build` pipeline stages: `PipelineStage` / `PipelinePass` marker
  traits making stage mismatches compile errors.

Item 2 applies the same pattern to field identity:

**F1. Field-marker trait family (volar-spec or volar-compiler-adjacent).**
A sealed-style trait pair — `ExtFieldMarker` (assoc consts: `DEGREE`,
`IRREDUCIBLE: &[u64]`, base marker) and `PrimeFieldMarker` (assoc consts:
`K`, `N: &[u64]`, i.e. `p = 2^K − N`) — implemented only by zero-sized marker
structs (e.g. `Aes8`, `Gcm128`, the tower levels, `Koala`-family primes if
ever needed downstream). Provided markers for at least: the existing six
primitives' fields (back-compat), the VOLE field GF(2^128), and the tower
levels WARP uses. Marker definitions are the *single* place an irreducible or
modulus is written; everything else resolves through them.

**F2. AST support.** Extend the AST so a field type is named by its marker
(type parameter or path resolved by the parser) and resolves to the concrete
field spec at compile time — fail-closed: unknown marker, reducible
polynomial, non-monic, non-canonical/composite modulus all error at type
resolution, reusing the `field.rs`/`prime.rs` interning rules rather than
re-implementing them. The hardcoded `PrimitiveType` variants remain as
aliases of their markers (no breakage); new fields arrive only as markers.

**F3. LIR lowering.** `volar-lir-codegen` maps marker-resolved AST field
types to the existing `LirType::ExtField` / `LirType::PrimeField` (extending
the current six-arm match in `structs.rs` to a marker-driven resolution), and
field arithmetic expressions on marked values lower to native field ops.

**F4. VAFFLE/Volar IR emission.** The `VolarIrTarget` path interns the same
spec via `TypeTable::ext_field` / `prime_field` (single validation point),
so a marked value is one typed field value end-to-end — never exploded into
bits at any stage boundary.

**F5. Acceleration invariance (the point of the item).** Add/mul on marked
field values compile to `FieldOp::Add`/`FieldOp::Mul` (or the Volar-IR
equivalent) instead of booleanized gate networks. This is the *only* place
this plan permits a structural assertion: a compile-through test may assert
"the field op survived as a field op" (and that no `Bit`-level expansion of
that operation exists), because that structural fact is the feature. All
*value-level* correctness still uses semantics preservation (evaluate IR
before/after; differential vs native spec arithmetic on fixed seeds).

**F6. Op coverage audit.** Enumerate the field ops the woven VOLE verifier
and the spec WARP decider actually need (add, mul, neg/sub, equality,
pack/unpack at I/O boundaries; inversion only if the decider's MLE path needs
it — record the finding, don't add speculative ops), and close exactly those
in the VAFFLE/Volar-IR lowering. Catch-all `_ =>` arms on IR matches per the
repo rule, so parallel pass development keeps working.

### 5.3 Why this is correctness work, not just speed

C2 (§1.4) demands the accumulated statement be the program's own arithmetic.
Bit-decomposing field arithmetic re-opens exactly the v0 class of gap (a
translated relation about different variables). Native field ops mean the
compiled verifier's arithmetic is *the same operation* the spec describes —
the marker guarantees both sides resolve one field spec — so the PESAT
emitter (§7) emits constraints over the actual trace values, not over a
re-encoding.

### 5.4 Field-basis decision — RESOLVED: accumulate over the VOLE field

`volar_primitives::Galois128` (the VOLE field; irreducible
`x^128+x^7+x^2+x+1`) and the IOP tower's `Gf128` (quadratic tower from
GF(2^8), `β`-chain) are isomorphic copies of GF(2^128) with **different
bases**. **Decision (reviewer-directed): WARP accumulates over the VOLE
field `Galois128` directly — no tower lift.** The `Gf128` tower remains only
as oracle-side test fixture in `volar-iop`. Consequences:

- The spec WARP port's field marker is the VOLE field's marker
  (`x^128+x^7+x^2+x+1`, coefficient set `{0,1,2,7,128}`), defined once under
  F1 and resolved everywhere else.
- Oracle-vs-spec differentials run the `volar-iop` oracle over the *same*
  field: `volar-iop`'s `Field` trait gains (or already has) an impl for
  `volar_primitives::Galois128`, so oracle and spec are compared on identical
  arithmetic, not through an isomorphism.
- No `IopLift`-style boundary embedding exists on this path; the v0-class
  basis-mixing risk is removed by construction rather than mitigated.

---

## 6. Statement-binding requirements (the anti-`v0` checklist)

These constrain item 1's API shapes now, so the downstream emitter doesn't
have to retrofit them:

- **B1. Program identity.** Every accumulated PESAT instance is emitted from
  a specific woven artifact; the artifact's digest (module/type-table/
  schedule digest, per the existing manifest-digest discipline) is absorbed
  into the WARP transcript domain for that batch. No digest, no accumulation.
- **B2. Transcript binding.** The VOLE transcript values the verifier
  consumed (hats, openings, setup messages) enter the PESAT public input `x`;
  Fiat–Shamir challenges are derived in-circuit from the same absorbed
  values. The prover cannot substitute a different transcript that happens to
  satisfy a generic correlation.
- **B3. Full execution, including setup.** The ML-KEM/Ferret setup's
  verifier-side execution is woven and accumulated like any other phase
  (C4) — the relation covers "setup accepted AND each cycle's checks held",
  never "assume valid `V_w`".
- **B4. I/O and boundary binding.** The verifier's accept/reject bit and the
  memory/trace boundary (`mem_acc_in`/`mem_acc_out` precedent) are part of
  the accumulated statement and of the final decided claim.
- **B5. Batch-top self-decision decides *the* batch.** The decision at the
  top of a batch binds the batch's first and last accumulator instances
  (alpha/beta/mu/eta chain), so "the verifier accepted this transcript" is
  the literal decided statement — not an implication through an unbound
  intermediate.

---

## 7. Downstream phases (context only — not part of the two items)

Recorded so reviewers can see the items are sufficient and so the items don't
smuggle this work in unreviewed:

- **D1. PESAT emitter.** Woven-verifier cycle trace → sparse constant-degree
  PESAT over the marked extension field, per-cycle instance + inter-cycle
  linking constraints, satisfying B1–B5. Emitter correctness is differential:
  native verifier run vs emitted-instance satisfaction on the same trace.
- **D2. Cycle accumulation driver.** One `warp_prove` step per cycle;
  manifest/epoch discipline per the existing scheduling plans where cycles
  are produced in parallel.
- **D3. Batch-top self-decision.** The spec WARP decider, compiled via IR
  (item 1 + item 2), accumulated as the final cycle(s) of the batch so the
  SNARK's decided statement is B5-complete.
- **D4. Reviewed code/parameter selection** (§2 non-goals), soundness
  parameter derivation from the paper's bounds, serialization, and only
  then any performance work.

---

## 8. Milestones and validation

Per repo convention: commit after each milestone and after intra-phase
checkpoints; `volar-ir-opt` stays `#![no_std]` + `alloc`; catch-all arms on
IR matches; never specialize on test cases; semantics preservation is the
correctness signal; unlocked/offline validation is reported as such (the
protected dirty `Cargo.lock` discipline applies).

| M | Content | Validation gate |
|---|---|---|
| M1 | F1+F2: marker traits, provided markers, AST resolution, fail-closed errors | Marker-resolution unit tests; rejection tests (reducible, non-monic, composite, non-canonical, unknown marker); back-compat: the six primitives resolve identically through their markers. |
| M2 | F3+F4+F5: LIR lowering + VAFFLE emission + acceleration invariance | Compile-through tests asserting field ops survive structurally; differential value tests AST→LIR→VAFFLE vs native spec arithmetic for every provided marker; `prime.rs`/`field.rs` interning reused (single validation point demonstrated by test). |
| M3 | W1–W5: `volar-spec::warp` module | The §4.4 test floor ported + native-vs-oracle differentials against `volar-iop::warp` on fixed seeds; power-of-two and malformed-index rejection preserved. |
| M4 | W6: compile-via-IR of the spec WARP | Parse → lower → Volar IR/VAFFLE round-trip of the spec module's verify/decide path; native-vs-IR differential execution; LSP-clean, `cargo test` suites green in both repos touched. |
| M5 (review checkpoint) | ~~Basis decision~~ **resolved** (§5.4: VOLE field) + D1 design doc | Reviewed note; no code required. |

Each milestone's tests extend, never special-case (repo rule 2).

### M4 status (2026-10-17): substantially complete

The compile-via-IR machinery is proven: `volar-lir-codegen/tests/warp_compile.rs`
(volar repo) runs the spec module through `lower_module_seeded` → `VaffleTarget`
and evaluates differentially against the native spec. Five probes pass (field
identity, ref-array index, array elem-sum, doubling loop with a mutated array,
and `warp_mle_eval` vs native `Galois128` arithmetic).

Key M4 work (volar-lir-codegen, volar-compiler, volar-vaffle-target, volar-fuzz):
- Native-loop lowering (`LoopLowering::Native`): per-iteration temporaries are
  excluded from loop-carried block params; the WARP spec code destructures
  struct fields into locals around native-loop array writes. The earlier
  all-concrete-bounds-for-Unroll pressure is superseded for this path.
- The spec module was refactored to free functions (methods on generic types
  mistype receivers in the planner).
- Planner/lowering fixes: type-alias resolution, DefaultValue/ArrayGenerate/
  Closure call discovery, narrowing casts, `from_fn` element-width
  reconciliation, field constructors, `is_power_of_two`, forward-reference-safe
  sibling calls, leaf-aware VaffleTarget params/returns, field bit-view/select/
  icmp, and the volar-fuzz VAFFLE interpreter's stack-memory model.

Cross-block threading gap: **resolved** (2026-10-17). `lower_if` now
SSA-merges outer variables reassigned in either branch through join-block
params (validated by `probe_native_loop_if_assign`); the instance-key
canonicalization (`canonical_type`) was normalized so planning and lowering
agree on monomorphized call targets. The full `warp_genesis_spec` probe now
lowers AND threads correctly.

Eval scale: **resolved** (2026-10-17). The volar-fuzz VAFFLE interpreter's
value tables are now dense Vec-indexed (O(1)) with in-place single-bit reads,
~15x faster, enough to run the genesis eval to completion in a test budget.
Instance duplication was deduplicated in the mono planner (non-generic callees
no longer inherit the caller's generic env). A unified shared-stack memory
model (all allocas on `StorageId::STACK` + one global stack pointer) makes
cross-call mutable references alias correctly; the spec-Keccak-f1600
differential probe (compiled permutation == known zero-state answer) passes.
Struct field *construction* now reconciles each field value's scalar width to
the declared field width (a U64 literal in a u32 field was shifting every
later field's offset).

Remaining gap: a `match` on a runtime index whose arms return *captured*
(let-bound) struct values miscompiles (wrong result + a block blowup) — the
`from_fn(|i| match i { 0 => mk0, ... })` shape used to build the toy PESAT
index in the genesis probe. That makes the genesis constraint eval read
`n_vars` as 0, so `warp_genesis_spec` returns `None`. Fixing
`lower_match_arm_chain`'s multi-scalar (struct) result threading is the next
correctness step; see the `#[ignore]`d `probe_match_captured_struct` minimal
repro.

---

## 9. Risks and open questions

1. **Dialect friction.** `warp.rs` is `Vec`-heavy generic Rust; the spec
   subset is fixed-shape and bounded. Expected pressure points: `Vec` usage
   (see the `vec_lint` pass), trait generics beyond operator bounds, iterator
   chains (the AST's `IrIterChain` is the supported form), and recursion
   (none allowed — the port must be explicit-loop). Mitigation: W4 is driven
   by compiling early skeletons, not by porting everything and then fighting
   the parser. Open: exact supported-generics envelope — M3 checkpoint
   records what the parser accepted.
2. **Hash-in-circuit cost.** W2's transcript/Merkle hashing will be large
   when woven. Accepted (no preemptive optimization); recorded so D3's batch
   sizing treats it as a known quantity rather than discovering it late.
3. **Soundness boundary.** Until D4, all WARP runs use proximity-fixture
   codes — completeness evidence only, zero production-soundness claim. Every
   doc/test must keep saying so (the current port's test docs already do).
4. **Transcript pinning deviation.** The verifier-order choice
   (`rt₀ → τᵢ → μᵢ → (τ, ω)`) is already documented in the pinning doc and
   must be carried into the spec port unchanged; any future re-derivation
   from the paper re-checks against that note first.
5. **Basis mixing (§5.4).** The highest-severity silent-failure risk in the
   whole plan; mitigated by F1 single-point marker resolution plus B1 digest
   binding, and by making the §5.4 decision an explicit review gate (M5).
6. **Oracle retention — RESOLVED (reviewer-directed):** `volar-iop::warp`
   remains the oracle **until production parameters deploy** (D4). This is
   deliberately more feasible here than in the prior attempts: WARP's
   code-agnostic design (any linear code over a sufficiently large field)
   plus the native-field decision (§5.4) mean production parameter/code
   selection does not redesign the port — the oracle stays the paper-pinned
   reference until that reviewed step lands. Retiring it earlier is not an
   option.

---

## 10. References

- Paper pinning: `volar/docs/research/warp-paper-pinning.md`; archived paper
  `volar/docs/research/papers/2025-753.{pdf,txt}` (sha256 in the note).
- WARP algebraic reference: `volar/crates/iop/volar-iop/src/warp.rs`
  (commits `82b23ae`, `417320a`, `acc21f0`).
- Prior prove-the-verifier docs: `volar/docs/prove-the-verifier-iop.md`
  (live bespoke path); `volar/docs/archive/prove-the-verifier.md`,
  `volar/docs/archive/fold-lift-expansion.md`,
  `volar/docs/agent-context/gf2k-to-fell-embedding.md` (archived v0);
  `cirrus-volar-vole/src/{vole_verifier_relation,spartan_whir_adapter}.rs`
  module docs (v1.5 WIP critique).
- Marker-system precedents: `volar/crates/ir/volar-discipline/src/lib.rs`,
  `volar/crates/iop/volar-iop/src/field.rs` (`BetaOf`),
  `volar-ir/crates/frontends/volar-ir-build/src/pipeline.rs`.
- Field-type machinery: `volar-ir/crates/ir/volar-ir-common/src/{field,prime}.rs`,
  `volar-ir/crates/ir/volar-lir/src/lib.rs` (`LirType`),
  `volar/crates/compiler/volar-lir-codegen/src/structs.rs`.
- Spec-dialect precedent: `volar/crates/spec/volar-spec/src/fold/mod.rs`.
- Cross-repo plan precedent: `docs/vole-mpc-shared-storage-scheduling-plan.md`.
