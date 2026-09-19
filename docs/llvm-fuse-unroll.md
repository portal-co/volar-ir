# Unconditional fuse unroll of boolar HKDF

**Status: resolved as an intentional circuit-shape boundary; Cirrus
consumes the one-step circuit.** The avoidable `subst_stmt` overhead was
removed by the bounded pure-gate cache in
[`llvm-fuse-hkdf.md`](llvm-fuse-hkdf.md). The remaining cost of
`fuse(..., Unconditional)` on linked `site_keys::derive` /
`derive_identity` is inherent to materializing this program as the current
acyclic Boolar circuit representation; it is not another substitution or
hash-consing bug. These entries import, lower, movfuscate, and lower to
Boolar, but must remain as step circuits rather than be unconditionally
fused.

Sample of the site HKDF canary (~9 min, 26.2 GB resident, 77% CPU) is
entirely in:

```
fuse
  → lower_to_circuit_fused
    → lower_to_circuit_impl
      → subst_stmt / emit_substituted
```

`lower_to_circuit_impl` copies the movfuscated SHA-256 / HMAC Boolar
body once per `limit` (64) and MUX-gates the exits. The result must be a
single acyclic `BCircuit`: it has neither a PC register nor a repeat/loop
node. For arbitrary input data, the PC and carried state after step `k + 1`
are functions of the work performed at step `k`; consequently the gates in
the next copy generally have different operands and cannot be shared with
the prior copy. Storage traffic is ordered and therefore cannot be
hash-consed at all. The first-exit MUX cascade is likewise necessary to
preserve the bounded-loop semantics.

For this representation, the materialized output is necessarily
`O(limit * step_body + limit * return_width)`, with the first term
dominating HKDF. The cache can and does retain exact repeated pure gates,
but it cannot change that bound. A smaller *program* representation exists:
the already-movfuscated Boolar CFG is the universal, reusable one-step
circuit. It is not interchangeable with the requested acyclic circuit.
Do not wait for the full unroll and do not treat Boolar of `compress256` as
HKDF fusing.

## Result and pipeline rule

No further generic `lower_to_circuit` optimization is warranted here.
`Pipeline::fuse(..., LoweringMode::Unconditional)` is for callers that know
the finite circuit they request is small enough to materialize. It must not
be used as a universal "finish compiling" step after movfuscation.

Supported Cirrus path for a movfuscated self-loop:

1. `lower_to_circuit_ir(1, WithTerminationFlag)` then Boolar — one copy
   of the step body. Outputs are `[done] ++ state ++ return` (the IR
   layout). Boolar `fuse(1, WithTerminationFlag)` is the wrong shape:
   it truncates carried state to the return width.
2. `initialize_storage` once, then loop `execute_initialized`, feeding
   the `state` slice (width = `params`) back as the next params until
   `done` or a step budget. Latch `return` on the first done.

That is what `cirrus-volar-boolar` already exposes for transition circuits.
Producing one flat `BCircuit` for HKDF would still require either a huge
Unconditional budget or a new circuit IR with an explicit bounded-repeat
node; neither is a drop-in replacement for the present `BCircuit` ABI.

Linked HKDF canaries still stop after `movfuscate`. Do not start
`fuse(64, Unconditional)` on HKDF. `lower_to_boolar` of linked HKDF has
been observed at ~8 min / 18 GB; `fuse(1)` is one copy of that body, not
64, but Boolar itself may still be too large for default CI.

## Tests

`poll_fsm` (small enough to Unconditional-fuse) proves the host-side
equivalence: `fuse(64, Unconditional)` return bits match 64
`execute_initialized` steps of the IR one-step circuit. Cirrus VOLE
then proves that one-step circuit across those persistent storage
steps (`llvm_looped_circuit_step_repeat_matches_unroll`,
`wasm_looped_circuit_step_repeat_vole`). Identity compile
coexistence uses the one-step path
(`web_proof_cirrus_vole_after_identity_compile`).

`from_llvm` → `lower_to_volar_ir` → `movfuscate` of the linked HKDF
canaries must return. Those canaries deliberately fail closed before
`fuse`.

Run:

```sh
cargo test -p volar-ir-passes
cargo test -p volar-ir-build --features llvm --test llvm_frontends
unset RUSTFLAGS
export LLVM_SYS_221_PREFIX=/opt/homebrew/opt/llvm@22
export CARGO_TARGET_DIR=/Users/g/Code-local/portal-hot/site/target
cargo test -p site-proofs --features llvm-loop --lib -- llvm_looped -- --nocapture --test-threads=1
cargo test -p site-proofs --features llvm --lib -- llvm_vaffle -- --nocapture --test-threads=1
```

The live compile leftover on the supported HKDF path (stop after
`movfuscate`) is `remap_monomials_in_place`
([`llvm-movfuscate-remap.md`](llvm-movfuscate-remap.md)).

## Still out of scope

- Identity `WebProofBackend::verify`, SLH-DSA in-circuit, putting HKDF in
  `site-proofs-guest`.
- `invoke` / unwind.
- Symbolic `memmove` length.
- `fuse(1)` / Boolar of linked HKDF in default CI (memory).

## Where

`lower_to_circuit_impl`'s `for _k in 0..limit` unroll in
`crates/ir/volar-ir-passes/src/lower_to_circuit.rs` is the materialization
boundary. Pipeline entry is `Pipeline::fuse`. Site driver is
`crates/proofs/src/vole_storage.rs` (`run_step_loop` /
`prove_and_verify_vole_step_loop`). Linked HKDF must not cross
Unconditional fuse.
