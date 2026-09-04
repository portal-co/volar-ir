# LLVM memory intrinsic through a symbolic stack GEP

**Status: landed.** Constant-length `llvm.memcpy`, `memset`, and `memmove`
now accept a `StackPtr::Symbolic` source or destination. They reuse the
ordinary dynamic ALLOCA access helpers already used by
[`llvm-dynamic-stack-gep.md`](llvm-dynamic-stack-gep.md), so they do not need
runtime storage-identity dispatch or a new bounds-checking primitive.

Constant `null` imports independently
([`llvm-const-null.md`](llvm-const-null.md)). Symbolic-length `memcpy` still
uses its own byte-loop CFG; its tagged pointer representation already covers
symbolic stack addresses.

## SHA block-buffer case

`cargo rustc -p site-keys --no-default-features --lib --release -- -C
overflow-checks=off -C panic=abort -C opt-level=1 --emit=llvm-ir` of
`site_keys::derive`. When the SHA-256 block buffer still has room for a
full 32-byte chunk (`pos < 32`):

```llvm
%pos.i = zext nneg i8 %_33.i to i64
%_43.i = getelementptr inbounds nuw i8, ptr %buf, i64 %pos.i
call void @llvm.memcpy.p0.p0.i64(
    ptr %_43.i, ptr %src, i64 32, i1 false)
```

`%pos.i` is not a compile-time constant, so the GEP is `StackPtr::Symbolic`.
The length `32` takes the direct-storage intrinsic path, which reads or writes
each byte through `stack_load_dynamic` / `stack_store_dynamic`. The sibling
remainder copy (`i64 %rem.i`) is a symbolic-length `memcpy` and takes the
separate byte-loop CFG path.

`StackPointer::intrinsic_range` remains the compile-time range check for a
`StackPtr::Const`. A symbolic address has no import-time range proof, so its
accesses have the same defined-execution requirement as ordinary dynamic GEP
loads and stores: the concrete address must be within the originating alloca.
This matches LLVM's own behavior for an out-of-bounds `inbounds` GEP rather
than imposing an arbitrary importer cap. As before, constant `memmove` reads
the full source before its writes; an overlapping symbolic `memcpy` is LLVM
undefined behavior.

## Tests (when landing)

Importer fixture, not the full `site-keys` crate:

```llvm
define void @copy_at(i64 %i, ptr %src) {
entry:
  %buf = alloca [64 x i8], align 1
  %p = getelementptr inbounds i8, ptr %buf, i64 %i
  call void @llvm.memcpy.p0.p0.i64(ptr %p, ptr %src, i64 32, i1 false)
  ret void
}
declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1 immarg)
```

The importer fixture covers this `memcpy` shape and a matching `memset`; each
has 32 dynamically addressed stack writes and no residual `Value::Call`. The
end-to-end fixture copies four bytes at dynamic offsets 0, 17, and 60, checks
the original CFG's result, then checks it matches the movfuscated step
circuit.

Run:

```sh
cargo test -p volar-llvm-vaffle-import --test basic
cargo test -p volar-ir-build --features llvm --test llvm_frontends
```

## Still out of scope

- Symbolic `memset` / `memmove` length.
- `invoke` / unwind.
- Identity `WebProofBackend::verify`, SLH-DSA in-circuit, putting HKDF in
  `site-proofs-guest`.

## Where

`IntrinsicPointer`, `Importer::intrinsic_pointer`,
`Importer::intrinsic_load`, and `Importer::intrinsic_store` in
`crates/frontends/volar-llvm-vaffle-import/src/lib.rs`. Symbolic-length
`memcpy` stays in [`llvm-memset.md`](llvm-memset.md). Dynamic stack GEP
load/store stays in [`llvm-dynamic-stack-gep.md`](llvm-dynamic-stack-gep.md).
