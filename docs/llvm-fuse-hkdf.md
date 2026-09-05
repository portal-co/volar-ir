# Fuse `subst_stmt` hash-cons on boolar HKDF

**Status: landed.** `Emitter::emit_substituted` hash-conses pure Boolean
gates re-emitted while unrolling (`SubstitutedGateCache`). Linked
`site_keys::derive` / `derive_identity` therefore **start** `fuse`
without the earlier unbounded `subst_stmt` walk.

Unconditional fuse unroll is a shape boundary
([`llvm-fuse-unroll.md`](llvm-fuse-unroll.md)). The live leftover on the
supported path is `remap_monomials_in_place`
([`llvm-movfuscate-remap.md`](llvm-movfuscate-remap.md)).

## Tests

`from_llvm` → `lower_to_volar_ir` → `movfuscate` → `lower_to_boolar` of
the linked HKDF canaries must return. Site canaries fail-close before
`fuse`.

Run:

```sh
cargo test -p volar-ir-passes
cargo test -p volar-ir-build --features llvm --test llvm_frontends
unset RUSTFLAGS
export LLVM_SYS_221_PREFIX=/opt/homebrew/opt/llvm@22
export CARGO_TARGET_DIR=/Users/g/Code-local/portal-hot/site/target
cargo test -p site-proofs --features llvm --lib -- llvm_vaffle -- --nocapture --test-threads=1
```

## Still out of scope

- Identity `WebProofBackend::verify`, SLH-DSA in-circuit, putting HKDF in
  `site-proofs-guest`.
- `invoke` / unwind.
- Symbolic `memmove` length.
- Finishing `fuse` of linked HKDF
  ([`llvm-fuse-unroll.md`](llvm-fuse-unroll.md)).

## Where

`Emitter::emit_substituted`, `SubstitutedGateKey`, and
`SubstitutedGateCache` in
`crates/ir/volar-ir-passes/src/lower_to_circuit.rs`.
