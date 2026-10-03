# Code Pinnedness and Stability

Volar IR records two independent properties of source code and documentation. The tags are optional in this compiler-IR repository, but they give downstream cryptographic protocol consumers a consistent way to assess evidence and change risk:

- **Pinnedness** records the strength of the evidence tying a claim or
  implementation to an external specification, review, or formal proof.
- **Stability** records how suitable the code is for dependents and how likely
  its semantics, API, performance profile, or implementation are to change.

Neither axis follows from the other. In particular, a paper-pinned component can
still be very unstable, and a stable API is not thereby secure or reviewed. The
one intentional coupling is that **Forever** stability requires **Proven**
pinnedness.

This policy replaces the former single `@reliability:` classification. It does
not change the model-neutral contribution policy: all agents may contribute,
but a claim requires evidence appropriate to the claim and the human decisions
identified below.

## Pinnedness

Pinnedness is ordered by the evidence available for the precise version and
claim at issue.

| Pinnedness | Meaning | Minimum evidence |
|---|---|---|
| **Unpinned** | No complete binding from this code and its claims to a reviewed external artifact. This is the current state of most code. | Tests and ordinary review may exist, but they do not establish a paper, external-review, or formal-proof binding. |
| **Paper-pinned** | The implementation and claims are bound to a specific paper or published specification, but have not received the required independent external review. | Citation, relevant theorem/algorithm/section, assumptions, and a project binding artifact that maps them to the code version. |
| **Reviewed** | An independent external review has assessed the paper binding and implementation claim. | The paper-binding evidence plus a named reviewer/review record, scope, findings, and reviewed commit/version. |
| **Proven** | The claimed security or correctness property is formally proved in Lean or a comparable proof assistant, with a documented connection to this implementation. | Proof artifact, theorem name, artifact revision, assumptions, and an implementation/refinement or verified-code link. |

A proof about an abstract construction does not make an implementation Proven
unless the documented link covers the relevant implementation behavior. Likewise,
a paper citation alone does not make code Paper-pinned.

## Stability

Stability is ordered from the strongest long-term commitment to the weakest.
It is an intrinsic usability and change-expectation statement, not a security
claim.

| Stability | Meaning |
|---|---|
| **Forever** | The public semantics and dependency contract are intended to remain usable indefinitely. This tier requires **Proven** pinnedness. Breaking it requires a documented exceptional decision, not an ordinary release choice. |
| **Stable** | Suitable for ordinary dependents; changes are exceptional and come with a migration path. It has no implied paper, review, or proof claim. |
| **Semver** | The public API follows semantic versioning: compatible changes are made within a major version and breaking changes require a major version. Semver does not promise Forever or Stable semantics. |
| **Unstable** | The field, API, semantics, or performance profile is still evolving, or known inefficiencies make dependents likely to need adaptation. |
| **Very unstable** | A novel field, unvalidated approach, or impractical performance profile makes rework, replacement, or substantial dependent changes likely. Do not present it as a generally usable deployment component. |

Only the Forever → Proven implication is automatic. A maintainer must record
why any other stability tier is appropriate, especially when a cryptographic
construction is made Stable or Semver.

## Source markers and evidence records

New or reclassified source that makes a cryptographic, security, correctness,
or dependency-stability claim must record both axes near its module header:

```rust
// @pinnedness: unpinned
// @stability: very-unstable
```

Permitted values are exactly the table labels in kebab case:
`unpinned`, `paper-pinned`, `reviewed`, `proven`; and `forever`, `stable`,
`semver`, `unstable`, `very-unstable`.

A non-default pinnedness needs its evidence beside the marker or in a linked,
versioned review artifact:

```rust
// @pinnedness: paper-pinned
// @paper: Author et al., Title (year), §4 / Algorithm 2
// @paper-binding: docs/reviews/example-binding.md@<commit>
// @stability: unstable
```

`reviewed` additionally names the independent review record and reviewed
revision. `proven` additionally names the proof artifact, theorem, revision,
and implementation/refinement link. `forever` additionally names the owner
commitment and the Proven evidence it relies on. Do not use a URL, an AI marker,
or passing tests as a substitute for these records.

New cryptographic constructions start **Unpinned** and **Very unstable** unless
a maintainer documents stronger evidence and a less volatile intended use. They
must have an executable main-use-case test and a paper-bound review plan before
they can be Paper-pinned. Promotion on either axis is a human decision; a
stability promotion never upgrades pinnedness.

## AI markers

Files meaningfully shaped by an AI assistant carry a module-level `//! @ai:`
marker. It records authorship and review history, not authority, pinnedness,
stability, or correctness.

| Marker | Meaning |
|---|---|
| `none` | No AI involvement. |
| `supervised` | AI drafted work that a human reviewed line by line. |
| `assisted` | AI and human collaborated; the human reviewed intent and output. |
| `generated` | AI produced most content; review was high-level. |
| `unreviewed` | AI-produced content without meaningful review. |

An AI marker never supplies the independent review required for Reviewed or the
formal artifact required for Proven.

## Evidence-based contribution and review

All agents may contribute. Model identity, capability tiers, and sub-threshold
source tags are not policy mechanisms. The required guardrail is evidence
appropriate to the claim:

- Compiler and backend changes need focused tests and generated-code
  compile-and-run evidence where applicable.
- Cryptographic changes need the paper-binding, review, and human decision
  required by their pinnedness claim; they may not use a stability label to
  imply a parameter, noise, security, or deployment result.
- Generated files are changed only through their generator pipeline. A missing
  environment dependency is recorded separately from a source failure.
- A review or handoff records the exact reproducer, source evidence, invariants,
  unresolved blockers, and next smallest safe action.

## Legacy-marker migration

`// @reliability: normal`, `hazmat`, and `experimental` are deprecated
migration markers. They are not a pinnedness or stability claim and must not be
used to infer one. No project-wide mechanical conversion is performed because
that would make unsupported classifications.

Until a file is reclassified with both new markers:

- unmarked and legacy-marked compiled files are treated as **Unpinned** and
  **Unstable**;
- legacy `experimental` is additionally a conservative signal to treat the
  work as **Very unstable** until assessed;
- legacy `hazmat` remains a use-safety obligation only; and
- `.rs.insecure` remains quarantined as described above.

When touching a legacy-marked file for a substantive change, replace its
`@reliability:` marker with explicit axis markers if its current evidence and
intended dependent contract can be stated honestly. Otherwise leave the legacy
marker in place and record the missing classification in the handoff; do not
invent a paper binding, review, proof, or stability commitment merely to finish
the migration.

## Reclassification protocol

1. Record the current commit, evidence, exact claim, and intended dependents in
   a review artifact or handoff.
2. Add or update both source markers and the supporting paper/review/proof or
   stability record.
3. Run the applicable tests, generated-code execution, and target checks; keep
   environment blockers distinct.
4. Obtain the required human decision for any non-default pinnedness, Forever
   stability, deployment claim, parameter/security claim, or policy change.
5. Update the relevant plan, documentation index, and this policy's inventory
   only when the evidence is current.

Pinnedness can advance without changing stability, and stability can be revised
downward without changing the evidence record. A fundamental correctness flaw
must be recorded in the relevant issue or handoff and fixed or removed before a
dependent-facing claim can remain in place.

## Current classification inventory

No project-wide source reclassification has been performed under this policy.
Existing reliability markers are governed by the legacy migration rules above.
The first classifications must be evidence-led, not inferred from crate names,
old labels, or model identity.

## Policy adoption

This policy replaces the former single reliability ladder with independent
pinnedness and stability axes. It preserves evidence-led claims, ordinary
compiler testing, typed IR, provenance, deterministic generation where
applicable, and downstream consumers' ability to assess a component without
mistaking a stability commitment for a security or correctness proof.