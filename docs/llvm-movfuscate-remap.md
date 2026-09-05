# Movfuscate `remap_monomials_in_place` on HKDF

**Status: accepted bound.** `PolyCoeffs` as a flat `Vec` landed
([`llvm-movfuscate-subst-poly.md`](llvm-movfuscate-subst-poly.md)).
Linked `site_keys::derive` finishes `movfuscate` in ~32 s. The remaining
wall is still `emit_block_stmts`, not lower or fuse.

Sample of the site HKDF canary (process elapsed 0:22 of a 32 s run,
7.2 GB resident, 7.5 GB peak, 100% CPU) is 294/640 samples in:

```
Pipeline::movfuscate
  → movfuscate_ir_owned
    → IrCtx::emit_block_stmts
      → subst_ir_owned
        → PolyCoeffs::remap_monomials_in_place
          → remap each IRVarId via var_map, then vars.sort()
          → windows(2) lex check / occasional outer sort
```

A further 133 samples are `push_typed` → `Vec::push` `memmove` of the
combined-block statement list (copying already-emitted `Poly` nodes as
the vec grows). Do not wait it out. Do not treat fuse unroll as this
leftover.

## What to change

Per-monomial `sort` and the adjacent-key walk must not run on every SHA
`Poly`. Degree-1 monomials (the common bit-op shape) need no inner sort;
a monotonic `var_map` needs no outer reorder check. `from_llvm` of
linked `site_keys::derive` must finish `movfuscate` without walking
every monomial of the HMAC body on each remapped statement.

## Tests (when landing)

`from_llvm` → `lower_to_volar_ir` → `movfuscate` of the linked canaries
must return. Site canaries assert `Ok("movfuscate")` and do not start
Boolar or `fuse`.

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

## Where

`PolyCoeffs::remap_monomials_in_place` in
`crates/ir/volar-ir-common/src/lib.rs`. The always-`sort` closure is
`subst_ir_owned`'s `IRStmt::Poly` arm in
`crates/ir/volar-ir-passes/src/movfuscate.rs`.
