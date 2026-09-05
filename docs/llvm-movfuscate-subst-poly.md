# Movfuscate `subst_ir_owned` Poly rebuild on HKDF

**Status: landed.** `Stmt::Poly.coeffs` is `PolyCoeffs<V>`
(`Vec<(Vec<V>, u8)>`), not `BTreeMap<Vec<V>, u8>`.
`subst_ir_owned` calls `remap_monomials_in_place`: rewrite each monomial
`Vec` in place and re-sort the outer collection only when order broke.
Linked `site_keys::derive` therefore **finishes** `movfuscate` without
the `bulk_build_from_sorted_iter` rebuild (67 s → 32 s, peak ~7.5 GB
vs 14.6 GB).

The next leftover is the in-place remap walk itself
([`llvm-movfuscate-remap.md`](llvm-movfuscate-remap.md)).

## Tests

`from_llvm` → `lower_to_volar_ir` → `movfuscate` of the linked canaries
must return.

Run:

```sh
cargo test -p volar-ir-passes
unset RUSTFLAGS
export LLVM_SYS_221_PREFIX=/opt/homebrew/opt/llvm@22
export CARGO_TARGET_DIR=/Users/g/Code-local/portal-hot/site/target
cargo test -p site-proofs --features llvm --lib -- llvm_vaffle_site_keys_derive -- --nocapture --test-threads=1
```

## Still out of scope

- Identity `WebProofBackend::verify`, SLH-DSA in-circuit, putting HKDF in
  `site-proofs-guest`.
- `invoke` / unwind.
- Symbolic `memmove` length.
- Unconditional fuse of the movfuscated step circuit
  ([`llvm-fuse-unroll.md`](llvm-fuse-unroll.md)).
- `remap_monomials_in_place` walk
  ([`llvm-movfuscate-remap.md`](llvm-movfuscate-remap.md)).

## Where

`PolyCoeffs` in `crates/ir/volar-ir-common/src/lib.rs`.
`subst_ir_owned`'s `IRStmt::Poly` arm in
`crates/ir/volar-ir-passes/src/movfuscate.rs`.
