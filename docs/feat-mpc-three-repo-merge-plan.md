# `feat/mpc` Three-Repository Merge Plan

**Status: approved for execution by the user; integration in progress. Use the current local `feat/mpc` tips, not stale `origin/feat/mpc` tips.**

> [!IMPORTANT]
> **Keep `feat/mpc` after merging.** This branch is expected to receive more protocol-development commits. Do **not** delete or rename the local or remote `feat/mpc` branch, do not use GitHub's delete-branch cleanup, and do not squash/rebase away its ancestry. Integrate it with ordinary merge commits; retain the branch as the continuing MPC workstream and use it for future commits.

This plan covers the requested integration into the current `main` branches of `volar-ir` (this repository), `../volar`, and `../cirrus`. It deliberately separates source integration from security approval: a successful merge or test run is not evidence that a cryptographic construction is secure or deployment-ready.

## 1. Snapshot and scope

The following is a point-in-time inventory of local refs and worktrees checked on **2026-10-05**. Refresh every ref and worktree status before acting; the feature branches and working trees may advance. Each `feat/mpc` branch is currently checked out in a linked worktree under `../mpc/`; preserve those worktrees and operate on the explicitly selected main/feature worktree rather than moving branch pointers from the wrong checkout.

| Repository | Target `main` | Local `feat/mpc` / `origin/feat/mpc` | Merge base | `main...feat/mpc` unique commits | Current worktree state |
|---|---|---|---|---:|---|
| `volar-ir` (here) | `3f951c3` (also `origin/main`) | `7b2c19a` (also `origin/feat/mpc`) | `4634615` | 5 main-only / 36 feature-only | Main worktree has an existing modified `Cargo.lock`; feature worktree `../mpc/volar-ir` also has a modified `Cargo.lock`. |
| `volar` (`../volar`) | `943b5a9`; local main is 19 commits ahead of `origin/main` (`bdb544e`) | `2bc75c2`; local feature is 46 commits ahead of `origin/feat/mpc` (`a205189`) | `bdb544e` | 19 main-only / 196 feature-only | Main has a modified `Cargo.lock`. The feature worktree `../mpc/volar` has tracked and untracked work described in §2. |
| `cirrus` (`../cirrus`) | `05cfd86` (also `origin/main`) | `4708087` (also `origin/feat/mpc`) | `d5bb1e3` | 127 main-only / 19 feature-only | Main and feature worktrees were clean at inspection. |

Approximate three-dot change sizes at that snapshot:

- `volar-ir`: 44 files, 5,489 insertions / 639 deletions.
- `volar`: 196 files, 52,419 insertions / 1,241 deletions.
- `cirrus`: 28 files, 3,593 insertions / 258 deletions.

These are not merge estimates. They include generated files, documentation, tests, and changes already in prior merge commits.

### Branches and local state that need an explicit decision

- **The local `volar/feat/mpc` is ahead of its remote by 46 commits.** The proposal assumes the intended source is the local tip `2bc75c2`, not merely the remote tip `a205189`. Confirm that before merging or pushing anything.
- **`volar/main` is itself 19 commits ahead of its remote.** This plan targets the checked-out local `main` at `943b5a9`; it does not authorize publishing those 19 commits. Confirm that they are intended to be part of the eventual target and preserve them during the integration.
- **The Volar feature worktree is not clean.** Tracked changes include `Cargo.lock`, `crates/mpc/volar-mpc/Cargo.toml`, `docs/adr/0002-semi-honest-pq-cot-ferret.md`, `docs/reviews/ferret-paper-binding.md`, and `docs/secret-shared-volar-mpc-plan.md`. It also has untracked `crates/mpc/volar-mpc/examples/ot_kem_candidate_rank.rs` and `crates/vc/volar-vc/examples/field_circuit_bench.rs`.
- The main worktrees for `volar-ir` and `volar`, plus the `volar-ir/feat/mpc` and `volar/feat/mpc` worktrees, have local `Cargo.lock` changes. Treat them as existing user state: inspect and preserve them; do not reset, checkout, or overwrite them as routine merge cleanup.
- `cirrus/feat/mpc` is much older relative to its current main than the other feature tips: its last main merge is `a8477f0` from 2026-09-14, while current main is at 2026-10-04. The 127 main-only commits make this a substantive reconciliation, not a small catch-up.

## 2. What the branches contain

This inventory is to guide review, not to assert every feature is finished or approved.

### `volar-ir`: compiler and Boolar support

The feature branch adds or extends:

- Boolar optimization and evaluator work, plus CFG layout and partial unrolling before movfuscation.
- Side propagation from VAFFLE into Volar IR/Boolar and circuit-building APIs.
- Explicit configured external/oracle policies for WASM and LLVM imports, including registration/ABI validation, rejection of invalid declarations, and lowering coverage.
- Shared AES and TLS external-operation contracts, schema/text-format updates, and generated artifacts.
- Fuzzer/evaluator changes and tests for these lowerings.

The current `main` has five later commits not in the feature tip: the storage-free Boolar interoperability boundary, Bristol Fashion import, Bristol Fashion export, Summon source emission, and semantic tests. These must remain in the merged tree. In particular, do not resolve a Boolar, circuit, or generated-schema conflict by replacing whole files with one side; review the semantic/API deltas and regenerate generated artifacts through their documented generator.

### `volar`: MPC, protocol, compiler, and provider work

The feature branch is a large workstream, not a single protocol patch. Its changed areas include:

- `volar-mpc` correlation providers, OT/Ferret, strict sessions, authenticated actions, external batching, TCP framing, transcripts, and cut-and-choose-related code.
- `volar-vc` circuit providers, external boundaries/executors, ORAM and held-material storage, AES/TLS integrations, and end-to-end tests.
- `volar-spec` field/OT/BinFHE primitives and parameter/plan code; `volar-weaver` garbling, hybrid, FHE, and ORAM paths; workspace/dependency integration; extensive plans and evidence documents.
- Compiler/IR integration for the feature, including the external action/oracle policies provided by `volar-ir`.

The latest committed feature tip is `2bc75c2` (“checkpoint KEM-seeded Ferret provider prototype”). The 19 local-main-only commits include newer parser/lowering fixes, role-separated Ferret seed APIs, and official Ferret-Reg setup intervals. These are especially relevant to the feature's current KEM/Ferret prototype and must be reconciled before treating its benchmarks or setup claims as current.

The uncommitted feature-worktree edits are substantive, not disposable build output. Decide whether to checkpoint and include them, keep them as subsequent feature work, or leave them untouched for a later commit. Do not silently include untracked benchmark/example files in a merge, and do not discard them. Re-run and review their measurements against the post-merge Ferret parameters/APIs; the current notes explicitly label candidate measurements and security claims as unapproved.

### `cirrus`: MiniOT, garbling, GRAM, and ERT integration

The feature branch adds:

- A `miniot` crate with framed transport, a Ferret/OT-extension bridge, and transport tests.
- Cirrus Volar garbling and evaluator work, including GRAM storage/hosts, encrypted ORAM support, and cut-and-choose coverage.
- Changes to the Boolar/garbling integration and removal of the old `cirrus-vole` wrapper.

Current `main` has advanced substantially since the branch's 2026-09-14 merge. It includes continued ERT/ARM/RISC-V development and a newer MiniOT/Ferret path; in particular, `05cfd86` replaces the earlier IKNP setup with a direct MiniOT Ferret bootstrap, preceded by `53af9b5` adding ML-KEM Ferret setup. Several edited Cirrus files overlap with newer main work, including the workspace manifest, `cirrus-vole`, ARM/RISC-V ERT, the garbled-circuit library, and Volar Boolar/garbling crates.

Do not blindly restore the feature branch's older MiniOT/Ferret or `cirrus-vole` state over current main. Decide which abstractions remain useful, preserve the newer mainline bootstrap/ERT behavior unless evidence shows otherwise, and add integration tests proving the resulting stack works end-to-end.

## 3. Dependency order and integration principles

Recommended order is **`volar-ir` → `volar` → `cirrus`**:

1. `volar` builds on `volar-ir`'s IR, side, lowering, and external-call contracts.
2. `cirrus` consumes Volar's `volar-spec`/`volar-oram` and also uses Volar IR crates in its integration/test graph.
3. This order gives each downstream merge a concrete, already-integrated dependency revision to test against.

There are two distinct Cargo overlays in the local checkout layout:

- `../.cargo/config.toml` patches dependencies to the ordinary sibling `volar-ir` and `volar` worktrees.
- `../mpc/.cargo/config.toml` patches to the feature worktrees under `../mpc/{volar-ir,volar,cirrus}`.

The Volar feature manifest also carries local patches, and Cirrus's feature manifest patches `volar-spec` to its sibling Volar checkout. Tests can therefore compile against a different source tree than a remote Git dependency or `Cargo.lock` appears to indicate. At each phase, record `cargo metadata`/`cargo tree` source resolution and prove there is only one compatible copy of each shared IR crate/type identity. Do not change shared `.cargo/config.toml` files to make a merge appear green; any temporary integration overlay must be local, explicit, and excluded from commits.

Use normal merge commits, not squash or rebase, so feature ancestry and later follow-up commits remain visible. Do not force-push. Do not update or publish remote refs until the target/source commits and local worktree changes have been explicitly reviewed.

## 4. Proposed phases and review gates

### Phase 0 — Freeze and decide the inputs

1. Refresh all hashes, merge bases, upstream tracking states, and `git status` output. This plan's snapshot is only a guide.
2. Preserve exact `main` and `feat/mpc` tips in each repo with local backup refs or equivalent recorded refs before any merge.
3. Preserve the dirty Volar feature worktree, including both untracked examples. Review it as a separate proposed checkpoint; decide whether its code/docs should be included in this integration or deferred. Do not run cleanup commands against it.
4. Decide explicitly whether to include the 46 local-only Volar commits and 19 local-only Volar-main commits in the integration/publish sequence.
5. Agree the merge method and whether any upstream push is authorized. This plan itself grants no push/delete-branch permission.

**Gate:** do not start Phase 1 until the source tips, dirty changes, and inclusion scope are confirmed.

### Phase 1 — Integrate `volar-ir`

1. Review the 36 feature-only commits by workstream and the five current-main-only commits; inspect conflicts in the external import, Boolar optimization/lowering, `BIrStmt`/schema, and build-pipeline surfaces.
2. Resolve on an isolated integration worktree/branch based on current `main`. Preserve the current mainline interop crate and all pre-existing local `Cargo.lock` content.
3. Confirm generated schema/text artifacts match their generator output; do not hand-edit generated files to resolve the merge.
4. Run focused tests for `volar-ir-opt`, `volar-ir-passes`, `volar-fuzz`, `volar-vaffle-target`, `volar-llvm-vaffle-import`, `volar-ir-build`, and `volar-ir-text`, then the repository's normal workspace checks.
5. Exercise the contract boundaries: malformed/duplicate external declarations, ABI mismatch, unsupported/defined/variadic declarations, WASM and LLVM import paths, Boolar semantic equivalence, side propagation, and the new storage-free Bristol/Summon interoperability tests.

**Gate:** the merged `volar-ir` main must build/test independently before updating or merging either dependent.

### Phase 2 — Integrate `volar`

1. Confirm whether the feature input is the local `2bc75c2` tip or the remote `a205189` tip. Do not accidentally omit the 46 local-only commits.
2. Account for the local 19 main-only commits, especially the role-separated Ferret seed API and official Ferret-Reg intervals. Test the merged code against the integrated `volar-ir` source selected by the intended Cargo overlay.
3. Resolve feature/current-main API and Cargo lockfile conflicts without losing the current main-only work. Re-run lockfile resolution from a clean, recorded state; keep pre-existing dirty lock changes recoverable.
4. Separately review the dirty benchmark/docs/examples listed in §1. If accepted, checkpoint them in an explicit commit before merge; otherwise preserve them as follow-up work. Re-evaluate their results against the merged parameter/API state and label single-run or exploratory measurements accurately.
5. Run targeted tests for `volar-spec`, `volar-mpc`, `volar-vc`, `volar-weaver`, and relevant compiler/build crates; build feature-gated examples; then run the agreed workspace/CI suite. Include two-party/socket/action-batch/external-boundary tests, ORAM/provider tests, circuit/garbling tests, and LLVM/WASM-to-external pipeline tests.
6. Check resource-heavy tests/benchmarks separately and record timeout, memory, features, and environment. Do not present a local test pass as a controlled performance result.

**Cryptographic gate:** retain Unpinned/Very unstable or the repository's applicable conservative status unless the existing paper-binding, exact construction/parameter analysis, independent review, and human approval are complete. In particular, a KEM name, passing correctness tests, or benchmark ranking does not establish a post-quantum OT/MPC security claim. This integration may preserve experimental code without promoting its security or deployment status.

**Gate:** the merged Volar tree must consume the integrated `volar-ir` revision, pass the agreed tests, and preserve explicit evidence blockers before beginning Cirrus integration.

### Phase 3 — Integrate `cirrus`

1. Re-evaluate the 127-main-only / 19-feature-only divergence and all overlapping files. Resolve in an isolated integration worktree based on current Cirrus main.
2. Reconcile the branch's `miniot` transport/extension with current main's direct MiniOT Ferret bootstrap; preserve a single intentional provider/transport path and test it. Do not keep duplicate or conflicting Ferret setup stacks by accident.
3. Review the feature branch's removal of `cirrus-vole` against current main's usage and direct-Ferret setup. Decide whether to retain, adapt, or remove it based on current callers and tests—not on the old branch diff alone.
4. Preserve current main's newer ERT and garbling improvements while integrating GRAM/ORAM/cut-and-choose and Boolar behavior. Add explicit tests where both branches changed the same interface.
5. Ensure the Cirrus tests resolve `volar-spec`, `volar-oram`, and Volar IR to the newly integrated Volar and Volar-IR revisions. Check lockfile/source identity; test with both the intended local-overlay path and the normal dependency-resolution path as appropriate.
6. Run focused tests for `miniot`, `cirrus-volar-garble`, `cirrus-volar-boolar`, `cirrus-volar-vole`, and changed ERT crates, followed by Cirrus workspace/CI checks. Run existing QEMU fixture tests only under their documented, bounded, software-emulated setup; unavailable test prerequisites are blockers, not evidence of success.

**Gate:** no remaining duplicate/legacy protocol path, unresolved dependency-source ambiguity, or regression in current main's ERT/MiniOT behavior.

### Phase 4 — Final cross-repository verification and publication decision

1. Record the final merge commits and exact source revisions in all three repositories; verify ancestry confirms the full feature commit history was retained.
2. Run a small end-to-end dependency smoke test from Cirrus through the intended Volar/IR APIs, in addition to each repository's focused suite.
3. Verify generated artifacts, manifests, lockfiles, and Cargo source resolution; inspect each final `git status` and ensure no pre-existing WIP or lockfile change was lost or accidentally included.
4. Obtain explicit approval before pushing any main/feature ref. In particular, decide how to publish Volar's 19 main-only commits and 46 feature-only commits.

## 5. Post-merge branch-retention procedure

**The `feat/mpc` branch remains an active development branch after the integration.** On all three repositories:

- Keep the local branch and `origin/feat/mpc`; disable/avoid automatic merged-branch deletion.
- Preserve the merge commit(s); do not squash/rebase the branch's published/shared history or force-push it.
- After integration, the feature pointer may remain at the last integrated tip. Before the next protocol-development batch, merge the then-current `main` into `feat/mpc` (a normal merge, not rebase) so new work starts from the integrated baseline.
- Later, merge only the new feature commits back into `main` with a normal merge commit. Repeat this cycle; branch retention is intentional, not a temporary cleanup exception.
- If one repository's feature branch is intentionally paused, keep its ref anyway. Future work can resume from the preserved integration ancestry.

## 6. Approved execution choices and checkpoint log

The user approved proceeding and explicitly selected **local branch tips throughout**. The plan's recommended dependency order and the requirement to keep `feat/mpc` in all three repositories remain in force. No remote push is implied.

The Volar feature worktree's uncommitted source/docs/examples were reviewed and recorded in the separate checkpoint commit before beginning the Volar merge. Existing `Cargo.lock` modifications remain protected user state and are not to be staged as part of these checkpoints unless specifically identified as required merge output.

| Checkpoint | Status | Commit / evidence |
|---|---|---|
| Approved plan and branch/input snapshot | Complete | `ff91ad2`; user approved local tips and dependency order. Pre-existing `Cargo.lock` state remains unstaged and preserved. |
| `volar-ir` feature integration | Complete | `579274c` merges local `feat/mpc` (`7b2c19a`) into `main`; `feat/mpc` ancestry confirmed retained. Focused suites plus `cargo test --offline --quiet --workspace` passed (86 successful test-result groups; ignored tests remain ignored). `volar-ir-common` Poly remap regression fixed by restoring generic map-style collision behavior; 17 common tests pass. No remote refs updated. |
| Volar feature-worktree checkpoint | Complete | `../mpc/volar` commit `5306e25` records the reviewed benchmark examples/docs; the pre-existing Volar feature `Cargo.lock` remains dirty and unstaged. Rebuilt and ran all three ML-KEM profiles and all four field candidates (three process runs each); field IR/Boolar results matched the independent reference. The `volar-mpc` library's 23 tests and `ferret_ot_tcp` passed; the 600-second `volar-mpc` integration-suite invocation timed out while later TCP/MPC tests were still running, so those gates remain for Phase 2. Cargo used a temporary explicit overlay selecting the local feature worktrees to avoid mixing the parent and `../mpc` patches. |
| Volar merge/compiler checkpoint | In progress; not a Phase 2 pass | `../volar/main` has an uncommitted merge of its local `feat/mpc` tip `5306e25`; staged merge changes and unstaged reconciliations remain, while its pre-existing dirty `Cargo.lock` is preserved separately. The merged Ferret suite passes (25 tests), `volar-compiler --lib` passes (33 tests), and `cargo xtask check-specs` passes after regeneration with zero unresolved wrapping-width warnings. The full `volar-mpc --features std` suite now passes after fixing its `OutputDecodes`/`VerdictBits` receive ordering and adding opaque-output and evaluator-reveal TCP regressions. Added TypeScript regressions for tuple-field positions, bit-count result types, checked/custom method return widths, and transitive seeded impl-method emission; the seeded-method regression and related focused tests pass. However, the full TypeScript component suite remains 17/19: `faest_core` has 19 diagnostics, matching the pre-merge-main baseline, while `vole_setup` has 26 diagnostics versus 13 on baseline. The new `vole_setup` failures concern feature-added APIs and unsupported generic/associated-function, collection, and error-propagation emission. The earlier `volar-vc`/`volar-weaver` compile failure from obsolete `volar_ir_common::Type::{AES8,Z3}` references was resolved by the canonical field migration recorded below. The TypeScript component failures remain open integration blockers, not passing gates. Tests used the explicit local volar-ir overlay; subsequent Cargo validation is isolated from the source Cargo.lock. No Volar merge commit or remote update has occurred. |
| Volar canonical field migration checkpoint | Component-validated; not a Phase 2 pass | `volar-ir/main` commit `8e20d7d` adds explicit LIR `ExtField`/`PrimeField` scalar forms, text round-tripping, documented widths/ABI, and field-aware C/LLVM arithmetic. `volar-lir` tests pass (1), `volar-lir-text --features parse` passes (35), all `volar-c-backend` tests pass (13), all `volar-llvm-backend` tests pass (70; one doctest ignored), and `cargo check --offline --workspace --all-targets` succeeds. The C/LLVM regressions include exhaustive GF(2^8) multiplication, FIPS 197 AES multiplication, GF(3)/GF(5) modular arithmetic and constant normalization, and extension-field shifts with differing shift widths. Against the corrected local volar-ir overlay, Volar `volar-ir-lir-target` (30) and `volar-lir-codegen` (1) library tests pass. The uncommitted Volar weaver migration now removes obsolete `PrimType::AES8`/`Galois64` assumptions from CFG bit-width handling and explicitly rejects typed `ExtField`/`PrimeField` values rather than silently treating them as packed integers; two `TfheScheme::wire_type_for_ir` rejection regressions pass. Added tests confirm the codegen descriptors for Galois8/64/128/256 and that all four polynomial descriptors are accepted and interned by the volar-ir LIR target; both focused tests pass. In an isolated temporary Volar source copy with the local overlay paths made explicit and a disposable lockfile, `cargo check --offline -p volar-vc -p volar-weaver --all-targets` passes; this resolves the earlier compile blocker for that package pair. The first full weaver test run then exposed stale BinFHE test-fixture imports and a retired `LutInputs` API use; the test harness was aligned to the current `volar-spec` API, and `cargo test --offline -p volar-weaver --lib` passes all 168 tests. These BinFHE edits update tests only, not cryptographic behavior. The source Volar and volar-ir Cargo.lock checksums remained unchanged. These are component gates only: the Volar merge remains uncommitted, and the existing TypeScript component and broader integration gates remain open. No remote refs updated. |
| Volar TypeScript emitter/reachability checkpoint | Progress; gate still failing | In the uncommitted `../volar/main` merge, qualified tuple-struct constructors now emit `new Class(...)`; seeded reachability now traverses every source/step/terminal in `IterPipeline`, including nested calls and impl methods; TS lowering also handles checked `u32::try_from`, `Vec::first`, and `Vec::as_slice` (including function-item use). Focused TypeScript compile regressions pass for qualified constructors, checked integer/Vec helpers, and free/impl calls inside iterator pipelines. A full isolated `ts_backend_components` run passes 19/21 tests; `faest_core` remains at its 19-error pre-merge baseline, and `vole_setup` is down to 17 diagnostics from 26 at the previous merge checkpoint but remains above its 13-diagnostic baseline. `cargo test` was run from the temporary Volar copy with the explicit local volar-ir overlay and available `tsc`; the source Cargo.lock files were not used/changed by this run. This is not a Phase 2 pass; broader integration and the remaining TypeScript errors are still open. No Volar merge commit or remote update has occurred. |
| Volar TS emitter/codegen follow-up | Progress; gate still failing | Added the `@volar-codegen-exclude` marker to Rust-only `volar-primitives/src/backend.rs` and made the component parser honor that marker, aligning component coverage with xtask generation. Updated TS emission so `enumerate()` produces Rust-width bigint indices, digest `finalize()` results map byte values to bigint, and byte-string literals preserve their bytes; regenerated all four spec artifacts. In the isolated Volar copy, `cargo run --offline -p xtask -- gen-specs` and `check-specs` both pass. Focused TypeScript regressions pass for tuple constructors, pipeline reachability, Vec/integer helpers, enumeration, and digest output. The full isolated `ts_backend_components` suite passes 19/21; `faest_core` remains at its 19-diagnostic pre-merge baseline, while `vole_setup` is now at 8 diagnostics (down from 26 at the earlier checkpoint and the 13-diagnostic baseline), but still fails on Result/error propagation and associated-method/type handling. Tests used the explicit local volar-ir overlay and available `tsc` in the temporary copy; source Cargo.lock files were not used/changed. This remains a progress checkpoint, not a Phase 2 pass; broader integration and remaining compiler diagnostics are open. No Volar merge commit or remote update has occurred. |
| Volar checked arithmetic, range access, and associated-method checkpoint | Progress; Phase 2 gate still failing | In the isolated Volar copy with the explicit local volar-ir overlay, `volar-compiler --lib` passes 33 tests and `volar-compiler-passes --lib` passes 61. Focused TypeScript regressions now cover checked/saturating arithmetic, `Result` error-marker propagation, safe optional `get(range)` including inclusive ranges, and qualified static/associated-method reachability with transitive free-function calls. Fixes also prevent `lower_module_dyn` from mistaking every lowercase zero-argument function call (for example `helper()`) for a length variable, infer static associated-call return types, propagate array element types through slice patterns, and erase the generic-array wrapper `.0` field transparently. The missing `AesCtrLengthDoubler::double`/`vole_setup` diagnostics are resolved: that component now passes. A newly reachable FAEST BAVC method exposed its nested local `walk` helper as unsupported TS output; the helper was moved to module scope without changing traversal logic, and all four focused `volar-spec::faest::bavc` tests pass. The full `ts_backend_components` suite still fails 1/24: `faest_core` reports 14 TypeScript diagnostics; this is not a Phase 2 pass. Two wrapping-width warnings in that component still default to 32-bit, so arithmetic correctness remains open. No Volar merge commit or remote update has occurred; source Cargo.lock state remains separate from temporary-copy validation. |
| Volar loop-body tail-expression checkpoint | Progress; Phase 2 gate still failing | In the uncommitted `../volar/main` merge, a TS component regression first reproduced a Rust loop-body tail `if let` being emitted as a function return (`void` returned from a `u32` function). The parser now converts trailing loop-body values into statements for `for`, `while`, and `loop`, matching Rust's discarded loop-body value semantics. The regression passes; `volar-compiler --lib` passes 33 tests. The full isolated `ts_backend_components` suite passes 24/25, with `vole_setup` and all other components passing; `faest_core` remains failing with 12 TypeScript diagnostics (down from 14 before this fix). Two unresolved wrapping-width warnings in the component still default to 32-bit, so generated arithmetic correctness remains an open gate. Validation used the explicit local volar-ir overlay in the temporary copy; source Cargo.lock files were not changed. The Volar merge remains uncommitted; no remote refs updated. |
| Volar TypeScript byte-vector conversion checkpoint | Progress; Phase 2 gate still failing | The uncommitted Volar TypeScript emitter now lowers `Vec::from(slice)` to a copying `Array.from(slice)` and converts SHA3 `Digest::finalize(...).to_vec()` output bytes from JS numbers to Rust `u8` bigints. Red/green regressions in the focused integer/Vec helpers test pass. The full isolated `ts_backend_components` suite passes 24/25; `vole_setup` and all other components pass, while `faest_core` is down to 9 TypeScript diagnostics (from 11). Remaining `faest_core` errors concern enum-backed `Sponge` method emission, SHAKE XOF modeling, missing `ctx` arguments, and Option fallback narrowing; two wrapping-width warnings still default to 32-bit. Tests used the explicit local volar-ir overlay in the temporary copy; source Cargo.lock files were not changed. The Volar merge remains uncommitted, with no remote refs updated. |
| `cirrus` feature integration | Pending | |
| Cross-repository validation and retained-branch sync | Pending | |
