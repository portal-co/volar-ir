# VAFFLE→IR `collect_uses` on linked HKDF

**Status: landed.** `lower_function` counts operand uses once per block
(`collect_use_counts`) and consumes them as statements are translated.
A call site's `future_uses` is that remaining count, not a fresh
`BTreeSet` walk of every suffix `Stmt::Poly`.

Linked `site_keys::derive` / `derive_identity` therefore **finish**
`lower_to_volar_ir` and **movfuscate**. Boolar `lower_poly_bit` landed
([`llvm-boolar-poly.md`](llvm-boolar-poly.md)). Fuse `subst_stmt`
hash-cons landed ([`llvm-fuse-hkdf.md`](llvm-fuse-hkdf.md)). Fuse unroll
is a shape boundary ([`llvm-fuse-unroll.md`](llvm-fuse-unroll.md)).
Movfuscate `Poly` steal landed
([`llvm-movfuscate-poly.md`](llvm-movfuscate-poly.md)). Dense `val_map` landed
([`llvm-lower-val-map.md`](llvm-lower-val-map.md)). `PolyCoeffs` as a flat `Vec` landed
([`llvm-movfuscate-subst-poly.md`](llvm-movfuscate-subst-poly.md)). The
live leftover is `remap_monomials_in_place`
([`llvm-movfuscate-remap.md`](llvm-movfuscate-remap.md)).

## Tests

`from_llvm` → `lower_to_volar_ir` of the linked HKDF canaries must
return. Site canaries also require `movfuscate`.

Run:

```sh
cargo test -p volar-vaffle-target
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
- `remap_monomials_in_place` walk
  ([`llvm-movfuscate-remap.md`](llvm-movfuscate-remap.md)).

## Where

`collect_use_counts`, `UseSink`, and the incremental `future_uses`
consume in `LowerCtx::lower_function` in
`crates/ir/volar-vaffle-target/src/lower_to_ir.rs`.
