# Unconditional fuse unroll of boolar HKDF

**Status: resolved as an intentional circuit-shape boundary.** The
avoidable `subst_stmt` overhead was removed by the bounded pure-gate cache
in [`llvm-fuse-hkdf.md`](llvm-fuse-hkdf.md). The remaining cost of
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

For linked HKDF, stop after `lower_to_boolar` and retain the movfuscated
one-step circuit. A future backend that can consume a repeated/step-circuit
form may use it directly. Producing one flat `BCircuit` would require either
a caller-approved resource budget large enough for the expanded circuit or a
new circuit IR with an explicit bounded-repeat operation; neither is a
drop-in replacement for the present `BCircuit` ABI.

## Tests (when landing)

`from_llvm` → `lower_to_volar_ir` → `movfuscate` → `lower_to_boolar` of the
linked canaries must return. The site canaries deliberately fail closed
before `fuse`: success through Boolean lowering is the supported milestone;
full unconditional fusion is intentionally not started.

Run:

```sh
cargo test -p volar-ir-passes
cargo test -p volar-ir-build --features llvm --test llvm_frontends
unset RUSTFLAGS
export LLVM_SYS_221_PREFIX=/opt/homebrew/opt/llvm@22
export CARGO_TARGET_DIR=/Users/g/Code-local/portal-hot/site/target
cargo test -p site-proofs --features llvm --lib -- llvm_vaffle -- --nocapture --test-threads=1
```

The live compile leftover on the supported path (stop after
`movfuscate`) is `remap_monomials_in_place`
([`llvm-movfuscate-remap.md`](llvm-movfuscate-remap.md)).

## Still out of scope

- Identity `WebProofBackend::verify`, SLH-DSA in-circuit, putting HKDF in
  `site-proofs-guest`.
- `invoke` / unwind.
- Symbolic `memmove` length.

## Where

`lower_to_circuit_impl`'s `for _k in 0..limit` unroll in
`crates/ir/volar-ir-passes/src/lower_to_circuit.rs` is the materialization
boundary. Pipeline entry is `Pipeline::fuse` with
`LoweringMode::Unconditional`; linked HKDF must not cross it until a
repeat-capable target is introduced.
