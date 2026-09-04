# LLVM constant-index GEP offsets into globals

**Status: landed.** First stage of a larger plan (see
`docs/agent-context/` planning notes) to let `ConstChain` failures fall back
to runtime dispatch instead of a hard import error, unblocking non-unrolled
LLVM frontend usage. This stage only fixes constant-offset handling; dynamic
(symbolic-index) offsets are still deferred.

## What changed

Two pre-existing gaps in `Importer::storage_for`'s callers
(`crates/frontends/volar-llvm-vaffle-import/src/lib.rs`):

1. A constant-index `getelementptr` **instruction** off a global (`%p =
   getelementptr [4 x i8], ptr @arr, i64 0, i64 2`) was never tracked past the
   `GetElementPtr` opcode arm — it only validated that the *base* resolved to
   a literal global, then discarded the index. A later `load`/`store` through
   `%p` then failed closed (`%p` itself isn't a global, so `storage_for`'s
   `ConstChain` walk failed).
2. The same offset expressed as a `getelementptr` **constant expression**
   embedded directly in a load/store's pointer operand (`load i8, ptr
   getelementptr inbounds ([4 x i8], ptr @arr, i64 0, i64 2)` — LLVM
   constant-folds this shape instead of emitting a separate instruction) was
   *silently wrong*, not an error: `ConstChain`'s `strip_pointer` always walks
   to a constant expression's operand 0 (a GEP's base pointer), discarding any
   index operand, so this resolved straight to `@arr` at byte offset 0.

Both are now folded correctly:

- `FuncCtx::global_ptr_of` tracks `(StorageId, byte_offset)` for a GEP
  instruction's result, mirroring `stack_slot_of`'s role for `alloca`s. It's
  consulted before falling back to `storage_for`.
- `Importer::storage_for_with_offset` extends `storage_for` to also walk a
  chain of GEP constant expressions (arbitrary depth, matching
  `strip_pointer`'s own walk), accumulating their byte offsets.
- `constant_gep_byte_offset` computes the byte offset from a GEP's source
  element type and constant index operands: the first index steps through
  "array of the source element type," each subsequent index descends one
  level into a nested array. Only scalar-integer and (possibly nested)
  array-of-integer types are supported — the same shape
  `flatten_alloca_type` already supports for `alloca`; struct/aggregate
  element types are a named error, not a panic.
- `mem_load`/`mem_store` now take an explicit `base_offset: u64` and stamp
  their address `Const`s with `addr_tid` (32-bit) instead of `byte_tid`
  (8-bit) — the same too-narrow-address-type mistake already root-caused once
  for the `ALLOCA` path (see `addr_tid`'s doc comment and the `spill(5)`
  regression it cites in `docs/llvm-array-alloca.md`) would otherwise
  resurface the moment a real (potentially large) offset is folded in.

A symbolic (non-constant) GEP index into a global is still deferred, not
supported: `gep_instr_constant_offset` returns `Ok(None)` for it, so the
GEP's result is simply left untracked and a later load/store through it still
fails closed via `storage_for`, exactly as before — no silent misresolution.
(A GEP constant expression can never carry a symbolic index by construction:
LLVM only constant-folds a GEP into a `ConstantExpr` when every index is
already a compile-time constant.)

The memory-intrinsic pointer path (`Importer::intrinsic_pointer`,
[`llvm-memset.md`](llvm-memset.md)) now reuses the same folded global offset,
so a constant-size intrinsic through this GEP accesses its actual byte range
rather than rejecting the pointer or silently using byte zero.

## Tests

- `global_gep_instruction_offset_folds_into_storage_addr` — constant-index
  GEP instruction off a global now imports and produces the correct byte
  offset (previously failed closed).
- `global_gep_constant_expr_offset_folds_into_storage_addr` — the same
  offset via an embedded GEP constant expression now produces the correct
  byte offset (previously silently produced offset 0).
- `global_gep_symbolic_index_still_deferred` — a symbolic index into a global
  still fails closed, confirming no silent misresolution was introduced.

Run:

```sh
cargo test -p volar-llvm-vaffle-import --test basic
cargo test -p volar-ir-build --features llvm --test llvm_frontends
```

## Out of scope (deferred to later stages)

- Dynamic (symbolic-index) GEP offsets into a global, and into a stack
  pointer — real runtime bit-circuit addition, not compile-time folding.
- Pointers with genuinely unknown provenance (function parameters, `phi`/
  `select` of differently-provenanced pointers, pointers loaded from memory)
  — needs runtime storage-identity dispatch, not just offset folding.
- Oversized-global paging, heap allocation, `indirectbr`/`blockaddress`,
  atomics/floats/vectors/aggregates — unchanged, still out of scope.
