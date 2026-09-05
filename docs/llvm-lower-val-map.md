# VAFFLE→IR `val_map` lookup on linked HKDF

**Status: landed.** `val_map` in `lower_function` is a dense
`ValueMap` (`Vec<Option<IRVarId>>` keyed by VAFFLE `ValueId`). Call-site
`contains_key` / `insert` / `required` are O(1). Linked
`site_keys::derive` therefore **finishes** `lower_to_volar_ir` without
the multi-minute `BTreeMap` walk.

`PolyCoeffs` as a flat `Vec` landed
([`llvm-movfuscate-subst-poly.md`](llvm-movfuscate-subst-poly.md)). The
next leftover is `remap_monomials_in_place`
([`llvm-movfuscate-remap.md`](llvm-movfuscate-remap.md)).

## Tests

`from_llvm` → `lower_to_volar_ir` → `movfuscate` of the linked canaries
must return.

Run:

```sh
cargo test -p volar-vaffle-target
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

`ValueMap` and `LowerCtx::lower_function` in
`crates/ir/volar-vaffle-target/src/lower_to_ir.rs`.
