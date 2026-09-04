# LLVM dynamic (symbolic-index) GEP into a stack pointer

**Status: landed.** Second stage of the ConstChain-fallback plan (see
`docs/llvm-global-gep-offset.md` for stage 0). A `getelementptr` with a
non-constant index into a tracked `alloca` pointer no longer hard-errors —
it emits real runtime bit-circuit address arithmetic instead.

## What changed

`FuncCtx::stack_slot_of`'s value type became an enum,
`StackPtr`, instead of `StackPointer` directly:

- `StackPtr::Const(StackPointer)` — the pre-existing, common case: a
  compile-time-constant `StorageId::ALLOCA` address. Unchanged behavior.
- `StackPtr::Symbolic { allocation_base, allocation_bits, addr_bits }` — a
  genuinely runtime-computed address (`PTR_BITS`-wide `Bits`, LSB first),
  produced when a GEP's index isn't a compile-time constant, or when its
  base is itself already `Symbolic`.

This was always sound at the storage level: `Stmt::StorageRead`/
`StorageWrite`'s `addr` field is a plain `Var`, and
`rebase_stack_addr` (`volar-vaffle-target/src/lower_to_ir.rs`) already treats
every ALLOCA address as an opaque runtime value with no dependency on it
being a compile-time constant — real bit-circuit addition against the live
stack-pointer register, used for the calling convention's own frame
addressing. The importer's requirement that a GEP index resolve to a literal
integer was self-imposed, not something downstream needed.

The `GetElementPtr` stack arm now takes the compile-time-constant fast path
(unchanged arithmetic) only when both the base is `StackPtr::Const` **and**
the index is a compile-time constant. Any other combination computes a
runtime address: the index's own bits (normalized to `PTR_BITS`, sign-
extended or truncated to match — mirroring the `SExt` opcode's own idiom)
are multiplied by the element's bit width and added to the base's address
bits, all via the existing `bc_mul`/`bc_add` bit-circuit primitives — the
same primitives `translate_switch`'s dispatch cascade already uses.

Reads/writes through a `Symbolic` pointer go through new
`stack_load_dynamic`/`stack_store_dynamic` helpers: since stack storage is
bit-granular, each of the `n_bits` individual bits needs its own address
(`base + i`), computed by `dynamic_addr` (bit-circuit add, then merged into
one `addr_tid`-typed value via `Stmt::Merge` — the same "compose many bits
into one typed value" idiom `compose_address`/`mem_store`'s byte-merge
already use).

Memory intrinsics (`memset`/`memcpy`/`memmove`) do **not** support a
`Symbolic` stack pointer — `intrinsic_pointer` now returns a named
`Unsupported` error for one, since `StackPointer::intrinsic_range`'s bounds
check has no defined behavior for a runtime address.

## Tests

- `dynamic_gep_index_into_alloca_imports` — a symbolic-index GEP into an
  `alloca` now imports successfully, and every resulting ALLOCA
  `StorageRead`/`StorageWrite` address is confirmed to be a computed value
  (`Stmt::Merge`), not a `Stmt::Const`.

Run:

```sh
cargo test -p volar-llvm-vaffle-import --test basic
cargo test -p volar-ir-build --features llvm --test llvm_frontends
```

## Out of scope (deferred to later stages)

- Dynamic GEP offsets into a *global* (statically-known identity, dynamic
  offset) — stage 2.
- Pointers with genuinely unknown provenance (parameters, `phi`/`select` of
  differently-provenanced pointers) — needs runtime storage-identity
  dispatch (stages 3-4), not just address arithmetic.
- Multi-index GEP into a stack pointer, and non-integer element types —
  unchanged, still a named error.
- Memory intrinsics through a symbolically-addressed stack pointer.
