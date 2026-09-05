# Movfuscate `Poly` clone and drop on HKDF

**Status: landed.** Linked `site_keys::derive` / `derive_identity` use
the consuming Volar-IR movfuscation path. The builder moves `Poly`
monomial buffers into the step circuit block-by-block rather than cloning
the whole source program and then dropping it. Dense `val_map` landed
([`llvm-lower-val-map.md`](llvm-lower-val-map.md)). `PolyCoeffs` as a flat `Vec` landed
([`llvm-movfuscate-subst-poly.md`](llvm-movfuscate-subst-poly.md)). The
live leftover is `remap_monomials_in_place`
([`llvm-movfuscate-remap.md`](llvm-movfuscate-remap.md)).

Sample of the site HKDF canary (process elapsed 3:06 of a 203 s run,
13.9 GB resident, 19.5 GB peak, 100% CPU) is entirely in:

```
Pipeline::movfuscate
  → Movfuscate::apply
    → drop_in_place<IRBlocks>
      → drop Stmt::Poly
        → drop BTreeMap<Vec<IRVarId>, u8>
```

`Movfuscate::apply` owns its lowered multi-block IR and calls
`movfuscate_ir_owned`. As each source block is visited, `subst_ir_owned`
takes a `Poly`'s coefficient map, rewrites each monomial's `Vec<IRVarId>`
in place, and inserts that same allocation into the remapped `BTreeMap`.
The tree nodes are rebuilt because remapping can change sort order, but the
dominant monomial-vector buffers are not cloned. The visited source
statement is replaced by a tiny placeholder, so its old map does not wait
for a whole-program `drop_in_place` after the output is complete.

The legacy borrowed `movfuscate_ir` and Boolar entry points remain
compatibility shims. Large compiler pipelines must use the owned API; the
standard `Pipeline::movfuscate` does so automatically. Unconditional fuse
of the resulting step circuit remains a separate shape boundary
([`llvm-fuse-unroll.md`](llvm-fuse-unroll.md)) and is not started by these
canaries.

## Result

The linked `site_keys::derive` canary measured:

```text
import       3.40 s
lower      137.95 s
movfuscate  27.50 s
total      176.66 s
```

This replaces the prior roughly 200-second movfuscation phase whose peak
held both large `Poly` populations. The canary did not sample a new exact
RSS peak, so it must not be cited as a precise memory measurement; the
whole-program clone-then-drop source of that peak is nevertheless removed
from the pipeline path.

## Implementation

`movfuscate_ir_owned(IRBlocks, &mut IRTypes)` is the consuming API.
`MovfuscCtx::emit_block_stmts` receives mutable source blocks so the
Volar-IR implementation can transfer a statement payload while the generic
movfuscation algorithm still obtains its source terminator afterward. The
borrowed API clones into a private working copy before using the same
algorithm, preserving its established public contract. A regression test
asserts byte-for-byte equality of borrowed and owned outputs for a
multi-block `Poly` program.

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
- `remap_monomials_in_place` walk
  ([`llvm-movfuscate-remap.md`](llvm-movfuscate-remap.md)).

## Where

`Stmt::Poly.coeffs` in `crates/ir/volar-ir-common/src/lib.rs`.
`subst_ir_owned` and `movfuscate_ir_owned` in
`crates/ir/volar-ir-passes/src/movfuscate.rs`; pipeline entry is
`Movfuscate::apply` in
`crates/frontends/volar-ir-build/src/pipeline.rs`.
