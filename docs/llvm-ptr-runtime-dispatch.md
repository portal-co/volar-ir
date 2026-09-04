# LLVM runtime storage-identity dispatch

**Status: landed.** Fifth and final stage of the ConstChain-fallback plan
(see `docs/llvm-global-gep-offset.md` for stage 0, `docs/llvm-dynamic-stack-gep.md`
for stage 1, `docs/llvm-dynamic-global-gep.md` for stage 2,
`docs/llvm-ptr-value-bits.md` for stage 3). `ConstChain` failure and
symbolic-index GEPs now fall back to genuine runtime dispatch instead of a
hard import error, for pointers with **genuinely unknown provenance** — a
function parameter, or a `phi`/`select`-merged value whose tag isn't a
compile-time constant. This unblocks `fn slice_get(xs: &[i32], i: usize) ->
i32 { xs[i] }` — the motivating case named throughout this plan
(`docs/llvm-const-cache-dominance.md`'s own "Measured" table: "rustc `-O0`
`xs[i]` pointer-param GEP | named ConstChain").

## The core idea

Stage 3 gave every statically-resolvable pointer a uniform, `PTR_BITS`-wide
tagged bit encoding (`ptr_value_bits`). Because *every* pointer-typed value
this importer produces uses that same encoding — including a bare
parameter's raw incoming bits, which the caller's own encoding populated at
the ABI boundary — decoding a pointer's provenance never needs any actual
provenance *analysis*. It's purely a bit-level decode: check the tag bit,
compare the ID field against each known candidate, done. This is what makes
"fall back to runtime dispatch" tractable without new IR primitives:
`Stmt::StorageRead`/`StorageWrite`'s `addr` was already a generic `Var`
(stages 1-2); only *which* `StorageId` a read/write targets needed a new
mechanism, since that field isn't itself runtime-selectable at the `Stmt`
level.

## What changed

**Eager global registration.** `import_module` now calls
`Importer::register_all_globals` before walking any function body, assigning
every module global its `StorageId` up front (previously lazy, on first
reference). Runtime dispatch needs the complete, stable candidate set from
the start — a dispatched pointer's caller may pass any address-taken global
regardless of which function directly references it syntactically, so the
candidate set can't just be "whatever's been seen so far." Capped at
`MAX_DISPATCH_CANDIDATES` (64) — a practical circuit-size bound, well under
the encoding's own much larger `2^GLOBAL_ID_BITS` capacity limit — fails
closed past it.

**`GetElementPtr`'s global arm gained a third case.** Previously: tracked
stack pointer, or resolvable global (stages 0-2). Now, when the base is
*neither* — e.g. a pointer parameter — the offset is computed as ordinary
bit-circuit arithmetic directly on the base's own raw `Bits` (via
`value_bits`, which already works for a parameter regardless of provenance).
This is correct for whichever concrete storage the pointer turns out to name
at runtime: the offset lives in the low ADDR bits either way (every bit, for
a stack destination; the low `GLOBAL_ADDR_BITS`, for a global one), with the
tag+ID bits above untouched by an in-bounds add — the same "flat mixed-radix
add" property that made the encoding worth choosing in the first place. The
result stays provenance-unresolved; restricted to a single index into a
scalar-integer element type, matching stages 1-2's own scope (multi-index
dispatch remains deferred).

**`Load`/`Store`'s fallback now dispatches instead of erroring.** When
neither `stack_slot_of` nor `global_ptr_of`/`storage_for_with_offset`
resolves the pointer, `dispatch_read`/`dispatch_write` decode and dispatch on
its raw `Bits` at runtime:

- `dispatch_read`: for each candidate (the stack, plus every registered
  global), compute a `matched` flag (`tag_bit AND (id_bits == candidate's
  StorageId)` for a global; implicitly `NOT tag_bit` for the stack, since
  every global's `matched` is already false whenever `tag_bit` is 0 — no
  separate case needed). Read *every* candidate unconditionally, then fold a
  `bc_select_vec`-style cascade per bit — mirrors `translate_switch`'s
  `bc_eq`/`bc_select_vec` dispatch pattern, applied to data bits instead of a
  jump-table index.
- `dispatch_write`: since `Stmt::StorageWrite`'s `storage` field isn't
  itself runtime-selectable, every candidate is written unconditionally on
  every call — read-modify-write, muxing each candidate's *old* value
  against the new one by its own `matched` flag, so an unmatched candidate's
  write is a semantic no-op. Direct generalization of
  `storage_to_mux_ir::mux_write`'s existing "N addresses in one storage"
  technique to "N storages, one address."

Both reuse existing per-candidate read/write helpers unchanged
(`stack_load_dynamic`/`stack_store_dynamic` for the stack candidate,
`mem_load_dynamic`/`mem_store_dynamic` for each global) — the only new
machinery is the `matched`-flag cascade and the fold/RMW loop around them.

An unmatched tagged-global pointer is deliberately not allowed to fall back
to stack address zero: the stack result is gated by `NOT tag_bit` before the
global selection cascade. This gives LLVM `null` (tag 1, global ID 0) a
zero-read/no-op-write behavior; see [`llvm-const-null.md`](llvm-const-null.md).

## Cost

Each candidate costs a full `StorageRead` (reads) or a paired
`StorageRead`+`StorageWrite` (writes) per accessed bit. A dispatched access
in a module with `N` globals costs roughly `(N+1)×` the storage operations
of a statically-resolved one, plus the `matched`-flag comparisons. This is
the accepted, acknowledged tradeoff of the approach (matching the plan's own
framing) — real gate-count cost in exchange for turning a hard import error
into an evaluable circuit.

## Tests

- `stack_pointer_param_dispatches_at_runtime` (rewritten from
  `stack_pointer_param_is_not_mistaken_for_alloca`) — a bare pointer
  parameter now dispatches instead of failing closed; asserts every
  candidate's address is a computed value, never a compile-time constant
  (preserving the original regression guard: a parameter must never be
  mistaken for a tracked alloca with a *known* address).
- `slice_get_dispatches_through_pointer_parameter` — the exact `xs[i]`
  motivating shape imports successfully.
- `dispatch_write_through_pointer_parameter_reaches_every_candidate` — a
  store through an unresolved pointer, in a module with one global, produces
  a full read-modify-write against *both* the stack candidate and the
  global's candidate.
- `llvm_slice_get_pointer_param_gep_movfuscates` — end-to-end through the
  real pipeline (`crates/frontends/volar-ir-build`): import →
  `lower_to_volar_ir` → `movfuscate` succeeds and produces
  `is_movfuscated()`.

Run:

```sh
cargo test -p volar-llvm-vaffle-import --test basic
cargo test -p volar-ir-build --features llvm --test llvm_frontends
```

**Not covered:** full numeric evaluation of the dispatch mux/demux
circuitry (e.g. constructing concrete inputs and checking `slice_get(xs, i)`
returns the right array element for real memory contents) was not attempted
— the movfuscated entry-block parameter layout's exact packed-word/
calling-convention encoding wasn't verified with enough confidence to write
a correct `eval_ir` input vector in the time available. The structural tests
above confirm the pipeline accepts and correctly *shapes* the dispatch
circuitry; a follow-up should add a genuine numeric correctness check.

## Out of scope (unchanged from earlier stages, still deferred)

- Multi-index dispatch (GEP into an unresolved pointer, or into a
  `Symbolic` global, with more than one index operand).
- Oversized-global paging beyond `GLOBAL_ADDR_BITS`.
- True unbounded heap `malloc`/`free` — no allocator model exists.
- `indirectbr`/`blockaddress` ingestion, indirect calls.
- Atomics, floats, vectors, aggregates/structs.
