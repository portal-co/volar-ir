# Storage Registry (Packed Storages)

> Load when allocating `StorageId`s, adding a pass/frontend that needs its
> own storage space, or composing modules from multiple producers.

## What it is

`volar_ir_common::storage_registry` — a **producer-side coordination
layer**, not a runtime indirection and not an IR format change. Statements
keep `storage: StorageId` (flat `u32`); evaluators, store-forward, text/rkyv
formats, region anchors, and backends are untouched. What the registry
changes is *who decides the number*: consumers register the spaces they
need instead of hard-coding a constant or scan-max-bumping.

Core invariants:

- **Allocation, never validity.** The registry governs which IDs a
  *producer* picks; it is never consulted to decide whether an existing
  storage access is meaningful. Fuzz generators and hand-built IR use
  unregistered IDs legally. Do **not** add "storage must be registered"
  checks to any semantics-bearing pass.
- **Strictly ephemeral.** No rkyv/serde, no text-format section, no schema
  entry. Cross-module exchange is flat IDs; the receiver `adopt_in_use`s or
  `claim`s them.
- **One registry per module.**
- **Dense, small IDs** from `register()` (smallest free). Consumers packing
  IDs into bit-limited encodings (`volar-llvm-vaffle-import`'s
  `GLOBAL_ID_BITS = 12` pointer tags) rely on this.

## API sketch

```rust
let mut reg = StorageRegistry::<StoragePurpose>::new();
let id = reg.register(StoragePurpose::Stack);            // dense fresh id
reg.claim(StorageId::memory(0), StoragePurpose::WasmMemory { index: 0 })?; // specific id, fails on collision
let blk = reg.register_block(purpose, 8);                // contiguous [base, base+8)
let blk2 = reg.register_block_with(|off| purpose_for(off), 8); // per-offset purposes
reg.adopt_in_use(foreign_ids, |from| StoragePurpose::Remapped { from }); // idempotent gap-fill
reg.purpose_of(id); reg.iter();
```

`StorageRegistry<P>` is **generic over the purpose vocabulary** —
downstream consumers define their own `P`; this repo uses
`StoragePurpose`. Registry-taking pass entry points are likewise generic
(take a `purpose: impl FnMut(...) -> RP` constructor).

## Registry-mode entry points (legacy paths all still work)

| Consumer | Entry point | Behavior |
|---|---|---|
| `volar-ir-virt` | `virtualize_ir_with_registry`, `virtualize_ir_committed_with_registry`, `virtualize_bir_with_registry` | Bytecode region (one `register_block_with`), per-type register files, commitment/key spaces from the registry; `cfg.bytecode_storage` numeric value **ignored**; `VirtOutput::consumed_storages` reports `(StorageId, VirtStorageRole)` in **both** modes |
| `volar-llvm-vaffle-import` | `import_module_with_registry` | Claims `StackFrameConvention::LEGACY` spaces, reserves `StorageId(0)` (null's tag = global-ID 0!), registers dense `LlvmGlobal` spaces; returns the registry |
| `volar-wasm-circuit-import` | `import_module_with_registry` | Claim mode: `StorageId::memory(i)` convention preserved, fails closed on collision |
| `lower_to_ir` | `lower_vaffle_to_ir_with_registry` | Claims convention, registers a fresh `VaffleSsaSpill` (dense, **not** `1_000_000`) |
| `vaffle_ssa` | `ssa_ify_module*_with_spill` | Caller-chosen spill space |
| `volar-ir-opt` | `substitute_ir_blocks_with_registry` | Adopts host, remaps every guest storage to registered `Remapped { from }` spaces |
| `volar-ir-opt` | `substitute_vaffle_with_registry` | **Adopt-only** — VAFFLE spaces are shared protocols, never remapped |
| `volar-ir-passes` | `movfuscate_ir_with_registry` | (Block, plain) lane pairs from the registry instead of `2n`/`2n+1` doubling |
| `volar-ir-build` | `Pipeline` sidecar | `from_data_with_registry` / `storage_registry()` / `into_parts`; threaded through `apply`; `emit_source` auto-fills `WireNames` from purposes; `StorageToMuxConfig::for_purpose` finds spaces by query |

## The typed convention

`vaffle::StackFrameConvention { alloca_marker, stack }` is the
alloca→frame protocol as a value, replacing numeric `ALLOCA`/`STACK`
matching. `LEGACY` preserves the historical values (`1_000_001`/`1`), so
hand-built modules and existing tests are unaffected. `lower_to_ir`
rebases by the handle; the LLVM importer tags by the handle. New pipelines
can `StackFrameConvention::registered(registry, false)` for fresh spaces.

## Hazards this fixed (don't reintroduce)

- `StorageId::memory(i) = 16 + i` is unbounded; LLVM globals started at
  64 — a ≥48-memory module collides silently. Claim mode now fails loudly.
- Movfuscate's `2n`/`2n+1` doubling overflows `u32` for source IDs > 2³¹
  and rewrites ranges other consumers may own.
- `dispatch_candidates` used to assume global IDs are contiguous from 64 —
  it now iterates the assigned set.
- `virtualize_bir` pre-init lanes must use **full-width** base addresses
  (`vec![false; pc_bits]`, fixed in Phase 1): element 0 at the empty
  address caused an infinite dispatch loop for multi-block inputs.

## Constants status

`DEFAULT`/`STACK`/`ALLOCA` stay (values of the default space and
`StackFrameConvention::LEGACY`). `VIRT_REGISTERS_BASE` is `#[deprecated]`.
`StorageAllocator` is the documented low-level escape hatch.
`Memory(i)` stays as the WASM external indexing convention (claim mode).
