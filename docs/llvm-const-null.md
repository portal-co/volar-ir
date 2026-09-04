# LLVM constant `null` pointer values

**Status: landed.** LLVM `ConstantPointerNull` now has a uniform pointer
`Bits` value. `icmp eq ptr %p, null`, `select` with a `null` arm, and a `phi`
between `null` and another pointer import through the existing generic SSA
machinery.

This unblocks the empty-salt / `Option<&[u8]>` shape in `site-keys`
`derive`:

```llvm
%.not = icmp eq ptr %0, null
%spec.select = select i1 %.not, ptr %default_salt, ptr %0
```

Iterator `Option` uses the same sentinel through a pointer `phi`.

## Encoding and dispatch

`ptr_value_bits` uses tag `0` for stack and tag `1` for global
([`llvm-ptr-value-bits.md`](llvm-ptr-value-bits.md)). `null` takes the
otherwise-unassigned tagged-global pattern: tag `1`, global ID `0`, and
address `0`. Importer-created globals begin at `StorageId` 64, so no global
dispatch candidate can match it.

`dispatch_read` selects stack storage only when the tag is `0`; an unmatched
tagged-global pattern reads as zero. `dispatch_write` already writes a global
only through a matching candidate and writes the stack only for tag `0`, so a
write through `null` is a no-op. This avoids aliasing alloca slot zero without
changing stack allocation addresses.

`null` is materialized at each use block, like an immediate integer, rather
than function-wide cached: its `Stmt::Const` values must not cross sibling
control-flow arms and violate VAFFLE dominance.

## Tests

Importer fixtures confirm both a direct null comparison and a pointer `phi`
with one null incoming edge import without a residual failure. The end-to-end
LLVM test lowers `is_null` through unroll and evaluates both the null encoding
and all-zero stack address, proving they compare differently.

Run:

```sh
cargo test -p volar-llvm-vaffle-import --test basic
cargo test -p volar-ir-build --features llvm --test llvm_frontends
```

## Still out of scope

- Nonzero `inttoptr` constants (same ConstChain path if they appear).
- `invoke` / unwind.
- Identity `WebProofBackend::verify`, SLH-DSA in-circuit, putting HKDF in
  `site-proofs-guest`.

## Where

`Importer::value_bits`, `Importer::ptr_value_bits`, and runtime dispatch in
`crates/frontends/volar-llvm-vaffle-import/src/lib.rs`.
