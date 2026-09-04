# LLVM memory-intrinsic global / untracked pointers

**Status: landed.** Constant-size, nonvolatile `llvm.memcpy`, `memset`, and
`memmove` now use the importer's regular pointer resolution. A tracked alloca
uses the direct stack path (and keeps its range check); a global resolved by
`global_ptr_of` or `storage_for_with_offset` uses direct global storage with
its folded byte offset; a symbolic-global or otherwise unresolved pointer
uses tagged runtime storage-identity dispatch. Symbolic lengths remain
unsupported.

## HMAC IV case

`cargo rustc -p site-keys --no-default-features --lib --release -- -C
overflow-checks=off -C panic=abort -C opt-level=1 --emit=llvm-ir` of
`site_keys::derive` (HKDF-SHA256).

HMAC copies the SHA-256 IV from a module constant onto a 40-byte alloca:

```llvm
@anon.…0 = private unnamed_addr constant [32 x i8] c"g\E6…", align 8
%digest.i = alloca [40 x i8], align 8
call void @llvm.memcpy.p0.p0.i64(
    ptr %digest.i,
    ptr @anon.…0,
    i64 32, i1 false)
```

The constant IV copy imports as 32 global-byte reads followed by alloca
writes. SHA remainder copies through slice pointers with a symbolic length
(`ptr %dest.0, ptr %src.0, i64 %rem.i`) still fail closed with the existing
named length error.

## Implementation

`Importer::intrinsic_pointer` now resolves globals through
`resolve_global_ptr`, rather than `LLVMIsAGlobalVariable`'s bare-global
recognition. `IntrinsicPointer::Global` carries the resulting byte offset and
passes it to `mem_load` / `mem_store`; a nonzero global GEP therefore cannot
silently access offset zero. Unresolved pointer values reuse the same
`dispatch_read` / `dispatch_write` paths as ordinary loads and stores.

Copy source reads remain fully materialized before destination writes.
Statically known overlapping `memcpy` remains a named error; `memmove` keeps
its temporary-buffer behavior. Importing the IV copy alone does not make
HKDF-SHA256 a circuit.

## Tests

Importer fixture, not the full `site-keys` crate:

```llvm
@iv = private unnamed_addr constant [32 x i8] zeroinitializer
define void @copy_iv(ptr %out) {
entry:
  %buf = alloca [40 x i8], align 8
  call void @llvm.memcpy.p0.p0.i64(ptr %buf, ptr @iv, i64 32, i1 false)
  ret void
}
declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1 immarg)
```

Import `copy_iv`. The fixture sees 32 global storage reads at offset zero into
the alloca and no residual `Value::Call`.

The second fixture copies through a nonzero GEP into `@iv` and asserts its
offsetted read range, preventing an offset-zero regression.

A pointer-parameter fixture additionally confirms that a constant-size copy
uses runtime dispatch rather than remaining an imported call.

Run:

```sh
cargo test -p volar-llvm-vaffle-import --test basic
cargo test -p volar-ir-build --features llvm --test llvm_frontends
```

## Still out of scope

- Symbolic memcpy/memset length (SHA remainder `copy_from_slice`).
- `invoke` / unwind.
- Identity `WebProofBackend::verify`, SLH-DSA in-circuit, putting HKDF in
  `site-proofs-guest`.

## Where

`Importer::intrinsic_pointer` in
`crates/frontends/volar-llvm-vaffle-import/src/lib.rs`. Stack memset/memcpy
coverage stays in [`llvm-memset.md`](llvm-memset.md).
