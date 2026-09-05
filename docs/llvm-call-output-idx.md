# VAFFLE `Output` bit index vs `SigDecl` result index

**Status: landed.** `vaffle_value_vtid` types `Value::Output { .. }` as
`TypeId(0)` (Bit). `idx` is the importer's per-bit projection, not a
`SigDecl::results` index. SSA-ify can spill `Output { idx: 1 }` of a
64-bit pointer or `i32` call result without

```
index out of bounds: the len is 1 but the index is 1
```

Linked `site_keys::derive` therefore **starts** `lower_to_volar_ir`.
Incremental `collect_uses` landed
([`llvm-lower-collect-uses.md`](llvm-lower-collect-uses.md)). The next
leftover is Unconditional fuse unroll of boolar HKDF
([`llvm-fuse-unroll.md`](llvm-fuse-unroll.md)).

Cross-function call numeric correctness for a single `i32` return that
never spills is independent and landed
([`llvm-cross-function-calls.md`](llvm-cross-function-calls.md)).

## Tests

A two-function fixture: callee returns `i32` or `ptr`, caller uses the
result in a successor block so SSA-ify must spill an `Output { idx: 1 }`.
`from_llvm` → `lower_to_volar_ir` must not panic on that typing.

Run:

```sh
cargo test -p volar-vaffle-target
cargo test -p volar-ir-build --features llvm --test llvm_frontends
```

## Still out of scope

- Identity `WebProofBackend::verify`, SLH-DSA in-circuit, putting HKDF in
  `site-proofs-guest`.
- `invoke` / unwind.
- Symbolic `memmove` length.
- Finishing `fuse` of linked HKDF
  ([`llvm-fuse-unroll.md`](llvm-fuse-unroll.md)).

## Where

`vaffle_value_vtid` in
`crates/ir/volar-vaffle-target/src/lower_to_ir.rs` (the
`Value::Output` / `Value::Call` arm). Importer emission is
`translate_instruction`'s `Call` arm in
`crates/frontends/volar-llvm-vaffle-import/src/lib.rs`. SSA-ify spill
is `ssa_ify_function` in `crates/ir/volar-vaffle-target/src/vaffle_ssa.rs`.
