# Boolar `lower_poly_bit` on movfuscated HKDF

**Status: landed.** `Emitter` hash-conses AND/XOR gates introduced by
`lower_poly_bit` (`emit_poly_and` / `emit_poly_xor`, bounded
`PolyGateCache`). Linked `site_keys::derive` / `derive_identity`
therefore **finish** `lower_to_boolar`.

Fuse `subst_stmt` hash-cons landed
([`llvm-fuse-hkdf.md`](llvm-fuse-hkdf.md)). Fuse unroll is a shape
boundary ([`llvm-fuse-unroll.md`](llvm-fuse-unroll.md)). Movfuscate
`Poly` steal landed
([`llvm-movfuscate-poly.md`](llvm-movfuscate-poly.md)). Dense `val_map` landed
([`llvm-lower-val-map.md`](llvm-lower-val-map.md)). `PolyCoeffs` as a flat `Vec` landed
([`llvm-movfuscate-subst-poly.md`](llvm-movfuscate-subst-poly.md)). The
live leftover is `remap_monomials_in_place`
([`llvm-movfuscate-remap.md`](llvm-movfuscate-remap.md)).

## Tests

`from_llvm` → `lower_to_volar_ir` → `movfuscate` → `lower_to_boolar` of
the linked HKDF canaries must return.

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
- Finishing `fuse` of linked HKDF ([`llvm-fuse-unroll.md`](llvm-fuse-unroll.md)).

## Where

`lower_poly_bit`, `emit_poly_and` / `emit_poly_xor`, and `PolyGateCache`
in `crates/ir/volar-ir-passes/src/lower_ir_to_boolar.rs`.
