# LLVM dynamic (symbolic-index) GEP into a global

**Status: landed.** Third stage of the ConstChain-fallback plan (see
`docs/llvm-global-gep-offset.md` for stage 0, `docs/llvm-dynamic-stack-gep.md`
for stage 1). A single-index `getelementptr` with a non-constant index into a
statically-known-identity global no longer defers — it emits real runtime
bit-circuit address arithmetic, mirroring stage 1's treatment of the stack
side. This is the exact `xs[i]`-shaped access the whole plan targets, minus
the one remaining piece: `xs` itself must still resolve to a *known* global
at import time (a genuinely unknown-provenance pointer parameter is stage 4).

## What changed

`FuncCtx::global_ptr_of`'s value type became an enum, `GlobalPtr`, instead of
`GlobalPointer` directly — the same shape as stage 1's `StackPtr`:

- `GlobalPtr::Const(GlobalPointer)` — the pre-existing, common case: a
  compile-time-constant byte offset. Unchanged behavior, including full
  multi-index/nested-array constant GEPs (stage 0's scope).
- `GlobalPtr::Symbolic { storage, offset_bits }` — a genuinely
  runtime-computed byte offset (`PTR_BITS`-wide `Bits`, LSB first), produced
  by a **single-index** GEP whose index isn't a compile-time constant (or
  whose base is itself already `Symbolic`), with a scalar-integer source
  element type.

The `GetElementPtr` non-stack arm now takes the original compile-time-constant
fast path (unchanged, still supports arbitrary constant multi-index/
nested-array GEPs) only when both the base is `GlobalPtr::Const` and the
GEP's own index operands are all constant. Any other combination — a
symbolic index, or a base that's already `Symbolic` — falls through to the
dynamic path: the index's own bits (normalized to `PTR_BITS`, sign-extended
or truncated to match) are multiplied by the element's byte width and added
to the base's offset bits, via the same `bc_mul`/`bc_add` primitives stage 1
uses. The dynamic path is restricted to a single index into a scalar-integer
element type (matching the stack arm's own restriction); a multi-index
dynamic GEP into a global remains deferred (left untracked, so a later
load/store through it still fails closed).

`mem_load_dynamic`/`mem_store_dynamic` are the `GlobalPtr::Symbolic`
counterparts of `mem_load`/`mem_store`: each byte needs its own
`addr = base_offset_bits + byte_i`, computed via `dynamic_addr` — the same
helper `stack_load_dynamic`/`stack_store_dynamic` already use (it was already
storage-agnostic, so no change was needed there).

Memory intrinsics are unaffected: `intrinsic_pointer` still only accepts a
base global with zero offset, unrelated to this change.

## Tests

- `global_gep_dynamic_index_imports` — a single-index symbolic GEP into a
  global now imports successfully, and every resulting global
  `StorageRead`/`StorageWrite` address is confirmed to be a computed value
  (`Stmt::Merge`), not a `Stmt::Const`.
- `global_gep_multi_index_symbolic_still_deferred` (renamed from
  `global_gep_symbolic_index_still_deferred`) — a *multi*-index symbolic GEP
  into a global is still deferred, confirming the dynamic path's scope
  restriction doesn't silently misresolve the unsupported shape.

Run:

```sh
cargo test -p volar-llvm-vaffle-import --test basic
cargo test -p volar-ir-build --features llvm --test llvm_frontends
```

## Out of scope (deferred to later stages)

- Multi-index dynamic GEP into a global (mixed-radix runtime array/nested-
  aggregate descent).
- Pointers with genuinely unknown provenance (parameters, `phi`/`select` of
  differently-provenanced pointers, pointers loaded from memory) — the
  `xs[i]` motivating case still needs stage 4's runtime storage-identity
  dispatch, since `xs` itself isn't statically resolvable to one global.
- Oversized-global paging, heap allocation, atomics/floats/vectors/
  aggregates, memory intrinsics through a dynamic global offset — unchanged,
  still out of scope.
