# LLVM `load` of a pointer-typed value

**Status: landed.** `Load` takes its result width from `llvm_bit_width`,
which accepts a default-address-space pointer and uses the module's pointer
width. That width comes from the LLVM data layout (`p:` / `p0:`), or LLVM's
64-bit default when the module has no layout. Host rustc IR (`e-p:64:64`)
therefore loads a real 64-bit pointer, not a 32-bit tagged word.

The previous `int_result_width` path required `IntType` and failed closed on
`load ptr` with `expected an integer-typed instruction result`. `Store` of a
pointer, function parameters, and `phi`s already used the layout width.

Constant-length `memcpy` through a symbolic stack GEP is independent
([`llvm-memcpy-symbolic-stack.md`](llvm-memcpy-symbolic-stack.md)).

## What this unblocks

Linked `-C opt-level=1` IR of `site_keys::derive`. `derive` stores a
`KeyPath` info `&str` into an alloca and calls `Hkdf::expand_multi_info`.
That define walks `info_components: &[&[u8]]` as `{ptr, i64}` pairs:

```llvm
%_42.0 = load ptr, ptr %iter1.sroa.0.0129, align 8
%22 = getelementptr inbounds nuw i8, ptr %iter1.sroa.0.0129, i64 8
%_42.1 = load i64, ptr %22, align 8
```

`%info_components.0` is a function parameter. Both fields now import: the
`load ptr` uses the 64-bit layout width and the unknown-provenance
`dispatch_read` arm. Symbolic `memset` in SHA-256 finalize padding is independent and landed
([`llvm-memset-symbolic.md`](llvm-memset-symbolic.md)). Call-`Output` bit
index landed ([`llvm-call-output-idx.md`](llvm-call-output-idx.md)). The
next HKDF leftover is Unconditional fuse unroll of boolar HKDF
([`llvm-fuse-unroll.md`](llvm-fuse-unroll.md)).

## Tests

Importer fixture `pointer_load_uses_the_64_bit_layout_width` stores an
`i64` address in an `alloca ptr`, reloads it, and dereferences. The
end-to-end fixture `llvm_64_bit_pointer_spill_loads_through_boolar_and_reversible`
unrolls, evaluates a concrete 64-bit value, and checks Boolar/reversible
storage keep the 64-bit address plus the 6-bit cell suffix.

Run:

```sh
cargo test -p volar-llvm-vaffle-import --test basic
cargo test -p volar-ir-build --features llvm --test llvm_frontends
```

## Still out of scope

- Unbounded fuse of linked HKDF
  ([`llvm-fuse-unroll.md`](llvm-fuse-unroll.md)).
- Symbolic `memmove` length.
- `ptrtoint` / `inttoptr`.
- `insertvalue` / `extractvalue` of `{ ptr, i64 }` aggregates.
- Non-default LLVM pointer address spaces.
- `invoke` / unwind.
- Identity `WebProofBackend::verify`, SLH-DSA in-circuit, putting HKDF in
  `site-proofs-guest`.

## Where

`pointer_width_from_layout`, `Importer::llvm_bit_width`, and the `Load` arm
of `translate_instruction` in
`crates/frontends/volar-llvm-vaffle-import/src/lib.rs`. Pointer-parameter
dispatch stays in [`llvm-ptr-runtime-dispatch.md`](llvm-ptr-runtime-dispatch.md).
Pointer `Bits` encoding stays in [`llvm-ptr-value-bits.md`](llvm-ptr-value-bits.md).
