# LLVM uniform pointer-value representation

**Status: landed.** Fourth stage of the ConstChain-fallback plan (see
`docs/llvm-global-gep-offset.md` for stage 0, `docs/llvm-dynamic-stack-gep.md`
for stage 1, `docs/llvm-dynamic-global-gep.md` for stage 2). Pure plumbing:
gives every statically-resolvable pointer a real `Bits` *value* so it can
flow through generic SSA machinery (`phi`, `select`) without hard-erroring.
Does not change how `Load`/`Store` resolve a pointer — that's unchanged, and
still requires `stack_slot_of`/`global_ptr_of` tracking. This stage exists to
give stage 4 (runtime storage-identity dispatch for genuinely
unknown-provenance pointers) something to decode.

## The gap this closes

A tracked stack pointer (from `alloca`, or a GEP off one) already had a real
`Bits` value — `stack_addr_bits`, cached from its own instruction translation.
A global did not: `mem_load`/`mem_store` never produced a `Bits` *for the
pointer itself* (only for the loaded/stored data), and a bare global
reference (`@g` used directly, not through a `load`/`store`/GEP) isn't an
LLVM instruction, so it never went through the opcode-dispatch caching
mechanism at all. `value_bits`'s fallback only handled `IntValue`; any other
`BasicValueEnum` — including a bare pointer — hit `Unsupported("unsupported
value kind")`. Concretely: `select i1 %c, ptr %alloca_ptr, ptr @g` failed to
import, because resolving `@g`'s operand inside `Select`'s existing
`op!(2)` → `value_bits` call had no path to succeed.

## What changed

`Importer::ptr_value_bits` computes a uniform, `PTR_BITS`-wide tagged bit
pattern for any pointer resolvable via `stack_slot_of` or
`global_ptr_of`/`storage_for_with_offset`:

- Bit 31 (MSB) = provenance tag. `0` = stack: bits `[30:0]` are the
  ALLOCA-local address, identical to `stack_ptr_addr_bits`. `Alloca`'s own
  bump allocator now explicitly refuses to ever set bit 31 (a new check
  alongside its existing overflow check), so a cached stack pointer's bits
  are always compatible with this tag convention without needing to pass
  through `ptr_value_bits` at all.
- `1` = global: bits `[30:19]` (`GLOBAL_ID_BITS` = 12) are the global's own
  `StorageId` value, reused directly as a compact candidate index rather
  than building a separate table (`StorageAllocator` already hands out
  small sequential ids). ID 0 is reserved for LLVM `null`, while importer
  globals begin at `StorageId` 64; bits `[18:0]` (`GLOBAL_ADDR_BITS` = 19)
  are the byte offset within it. A *constant* offset or `StorageId` that
  doesn't fit is a checked, named error; a *dynamic* (`GlobalPtr::Symbolic`)
  offset that doesn't fit silently truncates instead — matching this
  codebase's existing convention for `StorageId::STACK` addresses
  ("addresses wrap modulo 2^SP_BITS and alias unrelated storage" per
  `lower_to_ir.rs`), not a new deviation from the fail-closed norm.

Wired into `value_bits`'s fallback (`BasicValueEnum::PointerValue(p) =>
self.ptr_value_bits(fctx, block, p)?`). No other code changed:
`Select`'s existing `bc_select_vec` handling and `phi`'s existing generic
block-param mechanism both already compose correctly with same-width `Bits`
from either provenance — the only gap was producing that `Bits` in the first
place.

LLVM `ConstantPointerNull` takes the reserved tag-1 / ID-0 / address-0
pattern. It is materialized at each use block rather than cached across the
function, and runtime dispatch reads it as zero and ignores writes; see
[`llvm-const-null.md`](llvm-const-null.md).

## Tests

- `select_between_stack_and_global_pointer_imports` — `select` between a
  stack-alloca'd pointer and a global now imports.
- `phi_between_stack_and_global_pointer_imports` — same shape via a
  control-flow-join `phi` instead of `select`.
- `stack_pointer_param_is_not_mistaken_for_alloca` (pre-existing) still
  fails closed: a genuinely unknown-provenance pointer *parameter* isn't
  resolvable via `stack_slot_of` or `storage_for_with_offset`, so
  `ptr_value_bits` still propagates the ConstChain error. Confirms this
  stage doesn't accidentally reach ahead into stage 4's scope.

Run:

```sh
cargo test -p volar-llvm-vaffle-import --test basic
cargo test -p volar-ir-build --features llvm --test llvm_frontends
```

## Out of scope (deferred to stage 4)

- Loading/storing *through* a `phi`/`select`-merged pointer whose
  provenance is genuinely uncertain at that point — the pointer *value* now
  exists, but `Load`/`Store` still only consult `stack_slot_of`/
  `global_ptr_of`, keyed by LLVM value identity, not by decoding an
  arbitrary `Bits` pattern. That decode-and-dispatch step (a mux cascade
  for reads, a read-modify-write demux for writes) is stage 4.
- Pointer function parameters and other genuinely unknown-provenance
  pointers — unchanged, still fail closed.
- Oversized-global paging beyond `GLOBAL_ADDR_BITS`, more than
  `2^GLOBAL_ID_BITS` distinct globals — unchanged, still a named error for
  the constant case.
