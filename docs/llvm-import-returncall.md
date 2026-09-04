# LLVM `ReturnCall` to a declaration-only import

**Status: landed.** rustc `-O0` `panic=abort` overflow imports:
`call void @panic_const_add_overflow(); unreachable` becomes VAFFLE
`Terminator::ReturnCall` to a `FuncDecl::Import` ([`llvm-unreachable.md`](llvm-unreachable.md)).
`lower_to_volar_ir` succeeds. `unroll_ir` stays fail-closed on the symbolic
overflow `br`. `movfuscate` now completes through a well-typed abort sink.

## Resolution

`plan_functions` records no body entry block for a `FuncDecl::Import` and
reserves one actual abort-sink block instead. Both `Value::Call` and
`Terminator::ReturnCall` select that sink rather than constructing an
`IRBlockId` from an absent entry:

```rust
FuncInfo {
    entry_block: None,
    abort_block: Some(reserved_block),
    ..
}
```

The sink accepts the normal packed SP and import-argument words, followed by
one zero bit for every result bit of the module entry function. It jumps to
`Return` with those zero bits. Thus an imported declaration is not treated as
an executable implementation and it never resumes its caller; its reachable
path terminates the modeled program with zero-valued entry results. This is a
defined abort model, not an assertion that an overflow path is impossible.

## Measured (site, 2026-09-03)

| Guest | Result |
|-------|--------|
| overflow `extractvalue` 0, no panic edge | unroll **ok** |
| `call panic; unreachable` → ReturnCall | `from_llvm` + `lower_to_volar_ir` **ok**; `unroll_ir` fail-closed **ok** |
| same + `movfuscate` | **ok** through the import abort sink |
| `panic=unwind` `invoke` + `landingpad` | named `Invoke` (`llvm_vaffle_rustc_o0_overflow_invoke_is_named_unsupported`) |
| symbolic `gep i32, ptr %xs, i64 %i` | named unsupported (`llvm_vaffle_symbolic_gep_is_named_unsupported`) |

Site canary `llvm_vaffle_rustc_o0_overflow_panic_unreachable` can now add
movfuscation as a follow-up update outside this repository.

## Supported contract

- `FuncInfo::entry_block` is `None` for a declaration-only import; its
  reserved abort sink is always an in-range IR block.
- Both direct `ReturnCall` and ordinary `Value::Call` lower to that sink.
- The sink's `Return` arity and bit types match the module entry function's
  results, satisfying movfuscation's shared return-slot contract.
- The overflow branch and its panic edge remain in the IR. `unroll_ir`
  remains fail-closed when that branch is symbolic.
- Isolated reachable `unreachable` without a preceding direct call remains
  the existing named unsupported construct.

Tests (this repo):

- `llvm_import_return_call_movfuscates_via_abort_sink` verifies that the
  panic fixture movfuscates, computes `add_one(5) = 6`, and yields zero
  results on the overflow path.
- `llvm_import_call_movfuscates_via_abort_sink` covers the non-tail
  `Value::Call` path with an imported `i32(i32)` declaration.
- Existing `llvm_overflow_panic_unreachable_imports_but_unroll_fails_closed`
  stays green (symbolic unroll still fail-closed).
- Body-to-body `ReturnCall` (`caller(5)=6` tail call) stays green.

After this lands, add `movfuscate` (and fuse if it works) to the site
canary. Keep unroll fail-closed on the panic edge.

## Not this task

- `invoke` / unwind / reachable `landingpad` (already named).
- Constraining the overflow bit to 0 and deleting `%panic`.
- Symbolic GEP / `xs[i]` (already named).
- Adding `IRTerminator::Unreachable`.
- Identity `WebProofBackend::verify`, SLH-DSA verify.

## Where

`volar-vaffle-target/src/lower_to_ir.rs` plans and emits the import abort
sink, then uses it from both `Terminator::ReturnCall` and `Value::Call`.
Coverage is in `volar-ir-build/tests/llvm_frontends.rs`.
