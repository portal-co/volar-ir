# LLVM `memset` with a symbolic length

**Status: landed.** A non-constant `llvm.memset` length lowers to the same
three-block VAFFLE byte-loop CFG as symbolic `memcpy`
([`llvm-memset.md`](llvm-memset.md)): header (`index < length`), body
(one fill byte through `dispatch_write`), continue. Volatile calls and
symbolic `memmove` stay fail-closed.

`load ptr` of a 64-bit fat-slice field is independent and landed
([`llvm-load-ptr.md`](llvm-load-ptr.md)).

## SHA-256 finalize padding

Linked `-C opt-level=1` IR of `site_keys::derive`. After `load ptr` of
`info_components`, `Hkdf::expand_multi_info` reaches digest finalize. The
block buffer writes the `0x80` pad byte, then zeros the rest of the 64-byte
block. The zero-fill length is `pos XOR 63`:

```llvm
%_7.i.i = zext nneg i8 %_29.i.i to i64
store i8 -128, ptr %34, align 1
%_50.i.i.i = getelementptr i8, ptr %34, i64 1
%_46.i.i.i = xor i64 %_7.i.i, 63
call void @llvm.memset.p0.i64(
    ptr align 1 %_50.i.i.i, i8 0, i64 %_46.i.i.i, i1 false)
```

That `memset` now imports. The same pad appears in `Hkdf::extract` / HMAC
finalize. Linked `site_keys::derive` therefore finishes structural import.
Call-`Output` bit-index typing landed
([`llvm-call-output-idx.md`](llvm-call-output-idx.md)). The next leftover
is Unconditional fuse unroll of boolar HKDF
([`llvm-fuse-unroll.md`](llvm-fuse-unroll.md)).

## Tests

`symbolic_memset_lowers_to_cfg_loop_without_residual_call` imports an
unbounded `i64 %n` fill, asserts no residual `Value::Call`, and checks the
header/body/continue `IfNonzero` shape.

Run:

```sh
cargo test -p volar-llvm-vaffle-import --test basic
cargo test -p volar-ir-build --features llvm --test llvm_frontends
```

## Still out of scope

- Symbolic `memmove` length.
- Volatile memory intrinsics.
- `invoke` / unwind.
- Identity `WebProofBackend::verify`, SLH-DSA in-circuit, putting HKDF in
  `site-proofs-guest`.

## Where

`Importer::translate_memory_intrinsic` (`MemoryIntrinsic::Memset`,
`MemoryIntrinsicLength::Symbolic`) and `Importer::symbolic_memory_loop` in
`crates/frontends/volar-llvm-vaffle-import/src/lib.rs`.
