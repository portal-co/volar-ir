# Plan: Runtime `StorageId` Registry (Packed Storages Without Fixed Ranges)

**Status:** Landed — Phases 0–5 implemented. `StorageRegistry` in
`crates/ir/volar-ir-common/src/storage_registry.rs`,
`StackFrameConvention` in `crates/ir/vaffle/src/lib.rs`, registry-mode
entry points in `volar-ir-virt` / `volar-llvm-vaffle-import` /
`volar-wasm-circuit-import` / `volar-vaffle-target` / `volar-ir-opt` /
`volar-ir-passes`, pipeline sidecar in `volar-ir-build`, and
`docs/agent-context/storage-registry.md` as the ongoing reference.
**Scope:** This worktree only — all paths below are relative to the repo root.
**Kind:** Coordination-layer refactor. No change to the serialized IR
representation, the `(StorageId, TypeId, addr)` slot semantics, or any
evaluator.

---

## 1. Problem statement

`StorageId` is a flat `pub struct StorageId(pub u32)`
(`crates/ir/volar-ir-common/src/generated.rs:580`). Ownership of the `u32`
space is today governed entirely by **module-level constants and
convention**, spread across four crates:

| ID / range | Meaning | Defined at | Consumed by |
|---|---|---|---|
| `0` (`DEFAULT`) | "main" storage | `volar-ir-common/src/lib.rs:433` | `substitute_ir.rs` (spill target), `to_reversible.rs`/`from_reversible.rs` (tests), `rcircuit.rs:569`, fuzz generators |
| `1` (`STACK`) | call stack | `lib.rs:435` | `vaffle` (`StackAlloc`/`PtrLoad`/`PtrStore` docs), `volar-vaffle-target/lower_to_ir.rs` (frame budget + ALLOCA rebase target), `volar-lir` ABI docs |
| `2` (`VIRT_BYTECODE`) | virt pass bytecode table | `lib.rs:439` | `volar-ir-virt` (`VirtualizeConfig::bytecode_storage` default) |
| `[3, 16)` (`VIRT_REGISTERS_BASE`) | virt per-type register files | `lib.rs:445` | `volar-ir-virt/ir.rs` (`storage_per_type` arithmetic) |
| `[16, ∞)` (`MEMORY_BASE + i`) | WASM linear memory `i` | `lib.rs:447` (`memory(i)`) | `volar-wasm-circuit-import` (reads, writes, `pre_init`) |
| `64+` (`GLOBAL_STORAGE_BASE`) | LLVM importer globals | `volar-llvm-vaffle-import/src/lib.rs:125` | same file (`storage_alloc`) |
| `≥ 64` (clamped) | "fresh" IDs | `StorageAllocator`, `volar-ir-common/src/lib.rs:1360` | `substitute_ir.rs`, `substitute_vaffle.rs` (scan-max + alloc) |
| `1_000_000` (`VAFFLE_SSA_SPILL`) | `vaffle_ssa` cross-block spills | `lib.rs:459` | `volar-vaffle-target/vaffle_ssa.rs` |
| `1_000_001` (`ALLOCA`) | frontend `alloca` marker | `lib.rs:477` | `volar-llvm-vaffle-import` (emits), `lower_to_ir.rs` (rebases ALLOCA→STACK) |

On top of the constants, three passes do **arithmetic** on IDs:

- `volar-ir-virt/ir.rs:790` and `bir.rs:209`: per-handler slot storages at
  `base.0 + 1 + k`; `next_free_storage_after_bytecode` (`ir.rs:806`) derives
  the register-file base from the bytecode base.
- `volar-ir-virt/preinit.rs:302,312`, `bir.rs:450,545,604`:
  `StorageId(bytecode_storage.0 + k)` for per-bit bytecode/slot lanes.
- `volar-ir-passes/movfuscate.rs:2223`: Block-typed vs non-Block
  `StorageWrite`s are split into **even/odd** IDs
  (`StorageId(storage.0 * 2)` / `2n+1`) — an implicit injective transform of
  the whole source namespace.

### Why this hurts

1. **Collision risk is real, not theoretical.**
   `StorageId::memory(i) = 16 + i` is an *unbounded* range, while the LLVM
   importer hands out globals from `GLOBAL_STORAGE_BASE = 64`. A WASM module
   with ≥ 48 memories collides silently with LLVM-imported globals if the
   two frontends' outputs are ever combined (or a multi-memory module's IDs
   land on `VAFFLE_SSA_SPILL` at i = 999_984). Nothing checks this today.
2. **Consumers must know each other's constants.** `lower_to_ir.rs` rebases
   by *numeric value* (`ALLOCA`); the ALLOCA doc comment
   (`lib.rs:461–476`) exists precisely because numeric-value rebasing is
   fragile — it already burned the fuzzer (extended-block generator picks
   `StorageId(a % 4)`, which includes `STACK`).
3. **Composition requires whole-program scans.** `substitute_ir`/`substitute_vaffle`
   scan every statement and pre-init segment for max-ID
   (`substitute_ir.rs:842`, `substitute_vaffle.rs:643`) before every
   substitution, then pray the guest module didn't also use the allocator's
   output range.
4. **Movfuscate's doubling** assumes the entire doubled range is free — it
   silently remaps *any* source ID, including another consumer's freshly
   allocated one.
5. **No introspection.** Backends that want to *name* storages
   (`volar-circuit-source/names.rs`) get raw numbers; there is no record of
   "which consumer created this space and why".

### Goal

A **runtime registry** in which each consumer *registers* the storage
spaces it needs — stack, WASM memory `i`, LLVM alloca marker, LLVM globals,
virt bytecode/register-file/slots, spill scratch, substitution-remapped
guest spaces — and receives back its own `StorageId`s to hold onto, with
the registry guaranteeing uniqueness and remembering purpose. Producers
stop depending on magic numbers; the IR itself stays exactly as flat
`u32`s.

---

## 2. Consumer inventory (what must keep working)

### Frontends
- **`volar-wasm-circuit-import`** (`src/lib.rs:1135,1186,1242`): emits
  `StorageRead/Write` and `PreInitSegment`s against `StorageId::memory(i)`.
  External indexing convention: "WASM memory index" is a meaningful,
  user-facing key.
- **`volar-llvm-vaffle-import`**: two distinct uses —
  (a) `ALLOCA` marker space for `Value::StackAlloc`/`PtrLoad`/`PtrStore`/
  `PtrOffset` (`lib.rs:317–339`, emit sites ~`2352–2613`), a **cross-crate
  protocol** with `lower_to_ir.rs`'s rebase;
  (b) per-global spaces from its own `StorageAllocator::new(64)`
  (`lib.rs:125,519`), whose dense sequential IDs are **reused** as the
  `GLOBAL_ID_BITS`-wide tag in `ptr_value_bits`'s pointer-value encoding
  (`lib.rs:110`) — so globals need *small, dense* IDs (≤ 2¹²), plus a mux
  cascade keyed on the global list (`MAX_DISPATCH_CANDIDATES = 64`).
- **`volar-llvm-ir-import`** (`src/lib.rs:268`): storage-opaque; passes
  `StorageId` through `map_var`'s identity `stor_fn`. Registry-transparent.
- **`volar-ir-build`** (`src/pipeline.rs:297–330,447–490`): public pipeline
  API takes `StorageToMuxConfig { storage, ty, num_cells }` /
  `StorageToMuxBoolarConfig { storage, lane, .. }` — callers name a space
  to eliminate. Registry must give them a handle to *refer to*.

### Middle / lowering
- **`vaffle`** (`src/lib.rs:251`): `StackAlloc`/`PtrLoad`/`PtrStore` docs
  bake `STACK` into the value-model contract.
- **`volar-vaffle-target/lower_to_ir.rs`**: STACK frame budgeting
  (`compute_alloca_budget`, `own_layout.size` interplay, `:388`), and the
  `ALLOCA → STACK` rebase in `rebase_stack_addr` (`:2280`, call sites
  `:1003–1065`, emitted stmts `:520,622,643,1010,1017`).
- **`volar-vaffle-target/vaffle_ssa.rs`**: `VAFFLE_SSA_SPILL` scratch space.
- **`volar-ir-virt`**: `bytecode_storage` (config, default `VIRT_BYTECODE`),
  per-type register files (`ir.rs:526–537`), per-handler slot files
  (`ir.rs:788–815`, `bir.rs:198–216`), commitment storage
  (`hash.rs`, `virtualize_ir_committed`), pre-init lanes (`preinit.rs`).
  `VirtualizeConfig` docs already say "must not collide with any StorageId
  the input module uses" — an unenforced precondition the registry can
  enforce.
- **`volar-ir-opt/substitute_{ir,vaffle}.rs`**: scan-max + bump-allocate
  above (`ir_storage_allocator`/`vaffle_storage_allocator`, clamped ≥ 64),
  then remap *every* guest storage into fresh host IDs via
  `remap_storage_in_stmt` (`substitute_ir.rs:775`).
- **`volar-ir-passes/movfuscate.rs:2198–2223`**: even/odd doubling (see
  §1).
- **`volar-ir-passes/storage_to_mux_{ir,boolar}.rs`**: take an explicit
  `storage` to eliminate; registry-transparent, but the config struct is
  the natural place to accept a handle. MUX trees are instantiated **per
  storage**, and tree shape depends on cell count/address width — never on
  the numeric ID — so the registry neither helps nor hurts tree shape; what
  it *does* simplify is tree management: each tree is built at a site that
  already holds an explicit per-storage handle from the registry (see
  Q5 in §6).
- **`volar-ir-passes/{to,from}_reversible.rs`**: test-only `DEFAULT` uses.
- **`volar-ir-opt/store_forward.rs`**: caches keyed by `(StorageId, …)`;
  fully transparent to ID provenance.

### Backends / observation
- **`volar-circuit-source`**: `walk.rs` collects `BTreeSet<StorageId>`;
  `names.rs` has `name_storage(StorageId, String)` — a registry with
  purpose strings can *feed* this automatically.
- **`volar-noir-backend`**: rejects storage outright; transparent.
- **`volar-fuzz` interpreters**: `StorageMap = BTreeMap<(StorageId, TypeId,
  u64), …>` (`interpreter/ir.rs:38`), `BIrStorageMap` keyed by
  `((StorageId, LaneId), addr)` (`interpreter/biir.rs:27`). Transparent —
  and this is a *requirement*: evaluation must not need a registry.

### Formats
- **Text** (`volar-ir-text`): `storage_read storage=0 …` raw u32
  (`parse/ir.rs:34`, `ir.rs:99`); typed regions print/parse `S<n>`
  (`typed_regions.rs:93,358`). No symbolic names.
- **rkyv**: `StorageId(u32)` is `Portable` (`generated.rs:580–617`) —
  changing the *type* is a breaking archive-format change; the plan avoids
  it.
- **Schema**: `schema/volar-ir.schema.json` + `schema/GENERATED.md`
  generate `StorageId` — untouched.

---

## 3. Design

### 3.1 Core principle: registry at production time, flat IDs in the IR

The registry is a **producer-side coordination layer**, not a runtime
indirection and not an IR format change. Statements keep `storage:
StorageId` (flat `u32`); evaluators, store-forward, text/rkyv formats,
region anchors, and backends are untouched. What changes is *who decides
the number*: consumers ask the registry instead of hard-coding a constant
or scanning for max.

This is the only design that satisfies all of: no rkyv breakage, no
evaluator key changes, fuzzer's deliberate unregistered `StorageId(a % 4)`
usage stays legal, and `no_std + alloc` (both `volar-ir-common` and
`volar-ir-opt` are `#![no_std]`).

### 3.2 The type — generic over the purpose vocabulary

New module `crates/ir/volar-ir-common/src/storage_registry.rs`,
re-exported from `lib.rs`. The registry is **generic over the purpose
type** so downstream consumers (e.g. the `volar` repo) can define their
own purpose vocabulary without forking the container:

```rust
/// Generic storage-space allocator + ownership record. `P` is the
/// consumer's purpose vocabulary: this repo uses [`StoragePurpose`];
/// downstreams define their own.
pub struct StorageRegistry<P> {
    /// Dense bump cursor for `register()`.
    next: u32,
    /// Every ID handed out or claimed → its purpose. The map *is* the
    /// uniqueness guarantee.
    by_id: BTreeMap<StorageId, P>,
}

impl<P> StorageRegistry<P> {
    /// Empty registry; `register()` starts from ID 0, skipping claimed IDs.
    pub fn new() -> Self;

    /// Allocate the smallest never-before-issued ID for `purpose`.
    pub fn register(&mut self, purpose: P) -> StorageId;

    /// Claim a *specific* ID (external convention, adopting pre-existing
    /// IR). **Fails** on collision instead of stomping.
    pub fn claim(&mut self, id: StorageId, purpose: P)
        -> Result<(), StorageClaimError>;

    /// Claim every storage referenced by a statement/pre-init walk
    /// (adopting foreign IR). Returns the colliding IDs, if any.
    pub fn claim_all_in_use(&mut self, ids: impl IntoIterator<Item = StorageId>,
                            purpose: impl Fn(StorageId) -> P)
        -> Result<(), StorageClaimError>;

    /// Allocate a contiguous block of `n` IDs sharing one purpose (virt
    /// slot files, per-bit lanes) — keeps today's "base + k" access
    /// patterns working without unreservable arithmetic.
    pub fn register_block(&mut self, purpose: P, n: u32) -> StorageBlock
    where P: Clone;  // { base: StorageId, len: u32 }

    pub fn purpose_of(&self, id: StorageId) -> Option<&P>;
    pub fn iter(&self) -> impl Iterator<Item = (StorageId, &P)>;
}

/// In-repo purpose vocabulary. Extensible; the registry treats it as
/// opaque metadata + a human-readable label. Downstream consumers use
/// `StorageRegistry<TheirPurpose>` instead of extending this enum.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoragePurpose {
    /// The "main" space (today's `DEFAULT`).
    Default,
    /// Call-stack frame space (today's `STACK`; see §3.3).
    Stack,
    /// WASM linear memory `i` (external indexing convention).
    WasmMemory { index: u32 },
    /// Frontend alloca marker (today's `ALLOCA`; see §3.3).
    AllocaMarker,
    /// One LLVM-imported global.
    LlvmGlobal { name: alloc::string::String },
    /// Virt pass: bytecode table / register file / handler slot / commitment.
    Virt { role: VirtStorageRole, detail: u32 },
    /// `vaffle_ssa` cross-block spill scratch.
    VaffleSsaSpill,
    /// Substitution-remapped guest space (records the guest's original ID).
    Remapped { from: StorageId },
    /// Anything else; free-form label for diagnostics/naming.
    Other(alloc::string::String),
}
```

Contracts:

- **Dense, small IDs.** `register()` hands out the smallest free ID, which
  keeps `volar-llvm-vaffle-import`'s `GLOBAL_ID_BITS = 12` pointer-value
  encoding and `MAX_DISPATCH_CANDIDATES` mux sizes valid — registry-issued
  globals stay ≪ 2¹² by construction (the importer still asserts
  `id.0 < (1 << GLOBAL_ID_BITS)` fail-closed where the tag is built).
  Density is a stated contract.
- **`StorageBlock`** covers the legitimate uses of ID arithmetic
  (virt `base.0 + k` lane fans, per-type register files): arithmetic is
  fine when the *whole block* is reserved up front.
- **Strictly ephemeral.** The registry is never serialized: **no rkyv /
  serde derives, no text-format section, no schema entry** (Q4 in §6).
  Purpose metadata exists for allocation-time uniqueness, diagnostics, and
  optional name-feeding into `volar-circuit-source` — nothing else.
- **No garbage collection in v1.** Storage spaces live as long as the
  module; no consumer frees one today. `unregister` is an explicit
  non-goal (see §7).
- `Default`/`Clone` derived; `Clone` enables speculative pass pipelines
  (clone registry, run pass, discard on failure).
- **One registry per module** (Q2 in §6): each module being built/lowered
  has exactly one registry governing its storage namespace. Cross-module
  composition goes through `claim_all_in_use` + the substitute path's
  rename-on-collision — never two registries handing out IDs into the same
  module.

### 3.3 Well-known spaces: typed handles, not magic numbers (Q1)

`STACK` and `ALLOCA` are not merely allocation conveniences — they form a
**semantic protocol**: `STACK` is written into `vaffle`'s value model
(`StackAlloc` doc) and is the rebase *target* of `lower_to_ir.rs`'s calling
convention; `ALLOCA` is a marker produced by `volar-llvm-vaffle-import` and
consumed by `volar-vaffle-target` — two crates agreeing on a number.

**Decision (Q1): make that protocol a typed handle** threaded from the
producer to the consumer, instead of a well-known numeric registration:

```rust
/// In `vaffle` (it owns the value model whose `StackAlloc` points into the
/// stack space; both the importer and `volar-vaffle-target` depend on it):
pub struct StackFrameConvention {
    /// Marker space frontends tag alloca accesses with (legacy: `ALLOCA`).
    pub alloca_marker: StorageId,
    /// Runtime frame space the marker rebases onto (legacy: `STACK`).
    pub stack: StorageId,
}

impl StackFrameConvention {
    /// The legacy numeric values — the default, preserving validity of
    /// hand-built modules, fuzzer output, and existing text fixtures.
    pub const LEGACY: StackFrameConvention = ...; // { ALLOCA, STACK }

    /// Register both spaces in `registry` and return the handle.
    /// `legacy_values: true` *claims* the legacy numbers (failing loudly
    /// on collision); `false` registers fresh ones.
    pub fn registered(
        registry: &mut StorageRegistry<StoragePurpose>,
        legacy_values: bool,
    ) -> Result<Self, StorageClaimError>;
}
```

- `volar-llvm-vaffle-import`'s `Importer` holds a `StackFrameConvention`
  (default `LEGACY`); every `storage: StorageId::ALLOCA` emit site uses
  `self.convention.alloca_marker`.
- `lower_to_ir.rs` takes the same handle (config field / parameter) and
  `rebase_stack_addr` matches `convention.alloca_marker → convention.stack`
  instead of the raw constants. The rebase *logic* (SP advance past
  `own_layout.size` + alloca budget, `sp_bits + local_offset`) is unchanged.
- `DEFAULT` stays a plain constant: it is a default, not a protocol —
  nothing pattern-matches on it the way the rebase matches ALLOCA.
  Substitute's internal `DEFAULT → fresh spill` remap is unaffected.
- The `StorageId::{STACK, ALLOCA}` constants remain as the values of
  `StackFrameConvention::LEGACY` (docs updated to say so). Fuzzers and
  hand-written VAFFLE are unaffected: unregistered IDs stay legal (§3.5),
  and the convention's *default* is the legacy numbers.
- `VAFFLE_SSA_SPILL`, `VIRT_BYTECODE`, `VIRT_REGISTERS_BASE`,
  `MEMORY_BASE`, `GLOBAL_STORAGE_BASE` lose their reason to exist and are
  phased out per §4 (kept as deprecated aliases until the last in-repo
  user migrates).

### 3.4 Pass-by-pass adoption design

#### `volar-ir-virt` (largest internal arithmetic user)
- `VirtualizeConfig` gains an optional `&mut StorageRegistry<P>` (or the
  pass takes it alongside `cfg` — decide at implementation; the
  config-field route preserves call-site shape). The pass is purpose-type
  agnostic: `P` need only carry what the caller wants recorded.
- When present: bytecode table, per-type register files, per-handler slot
  files, and commitment storage are all `register()`/`register_block()`ed;
  `RegAlloc::build` and `GlobalLayout`/`BirSlotLayout` stop taking raw
  `base: u32` + doing `next_sid += 1` and instead pull from the registry.
- `VirtOutput` already surfaces layout information; extend it (or its
  layout struct) with the list of `(StorageId, VirtStorageRole)` consumed,
  so downstream `storage_to_mux` / region tagging / `names.rs` can find
  them without re-deriving.
- When absent: current behavior (`VIRT_BYTECODE` default +
  `next_free_storage_after_bytecode` arithmetic) — full backcompat, and the
  doc-only precondition "must not collide" becomes an *enforced* claim when
  a registry is supplied.

#### `volar-ir-passes/movfuscate.rs`
- Replace the `2n`/`2n+1` even-odd transform (`:2198–2223`) with
  on-demand pairs from the registry (one `register()` per side per distinct
  source ID seen; the source-ID → (block-typed, plain) pair map lives in
  the movfuscator state). **This removes the silent full-namespace remap.**
- Backcompat: the registry-less entry point keeps the doubling (documented
  as collision-prone, deprecated).

#### `volar-ir-opt/substitute_{ir,vaffle}.rs`
- `ir_storage_allocator`/`vaffle_storage_allocator`'s scan-max-then-bump
  becomes: `claim_all_in_use` of the *host* module, then one
  `register(Remapped { from })` per distinct guest storage (the existing
  `storage_map` construction at `substitute_ir.rs:123–147` — including its
  special-casing of `DEFAULT`→spill and `STACK`→fresh — becomes registry
  calls; `state_storage_guest` handling likewise).
- `StorageAllocator` itself stays (public API, used by llvm-vaffle-import
  pre-migration) but is documented as the low-level escape hatch; the
  `≥ 64` clamp remains in the registry-less path for backcompat.

#### Frontends
- **`volar-wasm-circuit-import`**: per memory, `claim(StorageId::memory(i),
  WasmMemory { index: i })` during transition (preserves the external
  convention and existing text-format fixtures), upgrading to plain
  `register(WasmMemory { index: i })` + an `index → StorageId` side table
  once nothing else needs the formula. **Claim-mode immediately fixes the
  latent 48+-memories vs. LLVM-globals overlap** by failing loudly instead
  of corrupting.
- **`volar-llvm-vaffle-import`**: `storage_alloc: StorageAllocator::new(GLOBAL_STORAGE_BASE)`
  → `register(LlvmGlobal { name })` per global. Density contract (§3.2)
  keeps `ptr_value_bits` valid; the `id.0 < (1 << GLOBAL_ID_BITS)` check is
  added fail-closed where the tag is built (`lib.rs:110` region). Alloca
  accesses use the threaded `StackFrameConvention` (§3.3).
- **`vaffle_ssa`**: `register(VaffleSsaSpill)` at pass entry, ID threaded
  through instead of the constant.
- **`lower_to_ir.rs`**: rebase logic unchanged, but the marker/stack IDs it
  matches come from the threaded `StackFrameConvention` handle (§3.3).

#### `volar-ir-build` pipeline (integration point)
- `Pipeline<Stage>` carries the module's `StorageRegistry<StoragePurpose>`
  as sidecar metadata alongside `(blocks, types)` — one per module (§3.2),
  created at `Pipeline::start`, threaded through passes, queryable by
  callers.
- `storage_to_mux` keeps accepting an explicit `StorageId`; callers obtain
  that ID from the registry (or from `VirtOutput`). Convenience:
  `StorageToMuxConfig::for_purpose(&registry, predicate)`. MUX trees are
  instantiated per storage, so the handle lookup is exactly the point where
  the tree's metadata (cell count, address width) can be recorded alongside
  — tree management gets simpler, and power-of-two considerations stay
  where they belong: on *offsets/addresses within* a storage, not on IDs
  (Q5 in §6).
- Terminal stages (`emit_source`) can auto-populate
  `volar-circuit-source`'s `WireNames`/`storage_names` from
  `registry.iter()` — human-readable backend output for free.

### 3.5 What the registry is *not*

- Not consulted by evaluators, store-forward, mux promotion, or any
  semantics-bearing pass. IDs remain self-validating flat numbers.
- Not serialized — strictly ephemeral, no rkyv/serde representation
  anywhere (§3.2). Two pipelines exchanging IR exchange flat IDs; the
  receiving pipeline `claim_all_in_use`s them (collision ⇒ rename via the
  substitute path, which is exactly what substitute already does).
- Not required for correctness of hand-built IR: fuzz generators
  (`generators/{ir,vaffle,biir}.rs` using `StorageId(a % 4)`) keep working
  unchanged — the registry governs *allocation*, never *validity*. This is
  called out in the registry's crate docs so future consumers don't add a
  "storage must be registered" check anywhere semantic.

---

## 4. Migration phases

Each phase is independently landable, `cargo test` green, and
semantics-preserving per the repo rule: **evaluate before/after and compare
(volar-fuzz Properties A–D)**, not IR-shape assertions, except where a
structural invariant *is* the point (e.g. "no two purposes share an ID").

### Phase 0 — the type (no call-site changes)
- New `storage_registry.rs` in `volar-ir-common` (respect `#![no_std]` +
  `alloc`; `BTreeMap`, no `std` collections), generic `StorageRegistry<P>`
  + in-repo `StoragePurpose` + `StackFrameConvention` in `vaffle`
  (handle type only; threading lands in Phase 2).
- Unit tests: alloc uniqueness, density (smallest-free-ID contract), claim
  collision failure, block allocation, clone independence, generic-`P`
  instantiation with a downstream-style custom purpose type.
- Add the topic to `AGENTS.md`'s context-file table *at the end of the
  work* (new `docs/agent-context/storage-registry.md` once behavior lands).

### Phase 1 — `volar-ir-virt`
- Registry-optional config; `register_block` for slot files and register
  files; `VirtOutput` reports consumed `(StorageId, role)` pairs.
- Tests: existing virt tests unchanged (backcompat path); new tests that a
  registry seeded with a colliding claim makes virtualize fail closed, and
  that `VirtOutput`'s reported set matches the storages actually read/written
  (structural invariant — legitimately shape-checked).
- Update `docs/agent-context/virt-adaptive-split-adr.md` cross-references if
  dispatch layout docs mention bases.

### Phase 2 — frontends + the typed convention (Q1)
- wasm-circuit-import (claim-mode + collision test: synthetic module whose
  memory formula collides with a pre-claimed ID fails loudly).
- llvm-vaffle-import: globals via registry; density assertion; the
  `StackFrameConvention` handle threaded through `Importer` and
  `lower_to_ir`, replacing the constant-matching in `rebase_stack_addr`.
  Default `LEGACY` keeps `tests/basic.rs`'s ALLOCA assertions passing
  unchanged (the values are identical).
- vaffle_ssa spill registration.
- **Fixture impact (Q3 — checked): none.** No goldens depend on numeric
  global IDs: `volar-llvm-vaffle-import/tests/basic.rs` asserts the
  ALLOCA-vs-global *distinction* and computed-address behavior, never raw
  numbers (`:650–675` matches `StorageId(id)` only to exclude ALLOCA);
  `volar-ir-text` fixtures use hand-written `storage=0`/`storage=1`;
  `crates/pass/volar-llvm-plugin/tests/fixtures` is a C source. Dense
  registry-issued global IDs (2, 3, … instead of 64, 65, …) break nothing —
  no renumbering needed.
- Tests: import-and-eval equivalence on the existing LLVM/WASM test corpus.

### Phase 3 — pass composition
- substitute_ir/substitute_vaffle registry path (deprecate scan-max seeding;
  keep `StorageAllocator` public).
- movfuscate registry path (kill even/odd doubling for registry users).
- Tests: volar-fuzz property runs (substitution = Property D territory;
  movfuscation = Property B/C) before/after on identical seeds; a targeted
  regression test where a guest module uses an ID inside what the old
  doubling transform would have clobbered.

### Phase 4 — pipeline integration
- `volar-ir-build` sidecar registry (one per module, §3.2);
  `StorageToMuxConfig::for_purpose` convenience; `WireNames` auto-naming
  from purposes.
- Optional follow-on: symbolic storage names in `volar-ir-text`
  (`storage %name` with an alias table section), gated on demand — the
  registry's purpose strings are the obvious name source.

### Phase 5 — cleanup
- Deprecate `MEMORY_BASE`/`VIRT_REGISTERS_BASE`/`GLOBAL_STORAGE_BASE` once
  no in-repo caller remains; keep `DEFAULT`/`STACK`/`ALLOCA` as the values
  of `StackFrameConvention::LEGACY` / the default space (documented as
  such).
- Sweep `docs/agent-context/ir-types-storage.md` and `docs/pipeline.md` for
  fixed-range language.

---

## 5. File-by-file change list

| File | Change |
|---|---|
| `crates/ir/volar-ir-common/src/storage_registry.rs` | **New.** Generic `StorageRegistry<P>`, `StoragePurpose`, `StorageBlock`, errors. No rkyv/serde. |
| `crates/ir/volar-ir-common/src/lib.rs` | `mod` + re-exports; deprecate range constants (doc-level). |
| `crates/ir/vaffle/src/lib.rs` | **New `StackFrameConvention`** handle (§3.3) next to the `StackAlloc` value-model docs. |
| `crates/ir/volar-ir-virt/src/{lib,ir,bir,preinit,adaptive_emit,hash}.rs` | Optional registry in `VirtualizeConfig`; replace base arithmetic (`ir.rs:529–537,788–815`, `bir.rs:209–215`, `preinit.rs:302,312`); `VirtOutput` consumed-set reporting. |
| `crates/ir/volar-ir-passes/src/movfuscate.rs` | Registry path replacing `:2223` even/odd split. |
| `crates/ir/volar-ir-opt/src/substitute_{ir,vaffle}.rs` | Registry-based guest remap; deprecate `scan_max_storage` seeding. |
| `crates/frontends/volar-wasm-circuit-import/src/lib.rs` | Memory claims/registrations at `:1135,1186,1242` + `pre_init`. |
| `crates/frontends/volar-llvm-vaffle-import/src/lib.rs` | Globals via registry (`:519`); `GLOBAL_ID_BITS` density check; `Importer` holds `StackFrameConvention`; ALLOCA emit sites use `convention.alloca_marker`. |
| `crates/ir/volar-vaffle-target/src/lower_to_ir.rs` | Take `StackFrameConvention`; `rebase_stack_addr` matches the handle, not the constants. |
| `crates/ir/volar-vaffle-target/src/vaffle_ssa.rs` | Spill space registration instead of `VAFFLE_SSA_SPILL` constant. |
| `crates/frontends/volar-ir-build/src/pipeline.rs` | Per-module sidecar registry threading; optional `WireNames` auto-population. |
| `crates/backends/volar-circuit-source/src/names.rs` | Accept bulk names from registry purposes (additive API). |
| `docs/agent-context/ir-types-storage.md` | Storage-semantics doc updated post-landing. |
| Fuzzers, interpreters, `store_forward`, `storage_to_mux_*`, `to/from_reversible`, text format, schema, `volar-llvm-ir-import`, noir backend | **No change** (verified registry-transparent). |

---

## 6. Review decisions (resolved)

- **Q1 — ALLOCA/STACK protocol:** **typed.** The rebase protocol becomes a
  `StackFrameConvention` handle threaded importer → `lower_to_ir` (§3.3);
  the constants survive only as `LEGACY` values/backcompat.
- **Q2 — registry lifetime:** **per module.** One registry governs one
  module's namespace; cross-module composition goes through
  `claim_all_in_use` + substitute's rename (§3.2).
- **Q3 — fixture stability:** **checked; no impact.** No goldens depend on
  numeric global IDs — `basic.rs` asserts ALLOCA-vs-global distinction
  only, text fixtures use hand-written small IDs, llvm-plugin fixtures are
  C sources. Dense registry-issued global IDs break nothing (Phase 2).
- **Q4 — serialized purposes:** **no; strictly ephemeral.** No rkyv/serde
  derives, no format sections — purposes live only in-memory for
  uniqueness, diagnostics, and optional name-feeding (§3.2).
- **Q5 — power-of-two bases:** **no alignment work.** Backends benefit from
  power-of-two *offsets within* a storage, not power-of-two IDs; MUX trees
  are instantiated per storage, and explicit per-storage handles simplify
  tree management (§2, §3.4).
- **Purpose type:** **generic.** `StorageRegistry<P>` is parameterized over
  the purpose vocabulary so downstream consumers (e.g. `volar`) can define
  their own without forking the container; this repo's enum is
  `StoragePurpose` (§3.2).

## 7. Non-goals

- Changing `StorageId`'s `u32` representation, the
  `(StorageId, TypeId, addr)` slot model, BIR lane semantics, or any
  evaluator.
- `unregister`/ID reuse (no consumer needs it; adds use-after-free hazard
  for held IDs).
- Enforcing registration at *evaluation* or *parse* time (fuzzers and
  hand-written text fixtures intentionally use unregistered IDs).
- Serializing the registry or its purposes in any format (rkyv, text,
  schema) — it is strictly ephemeral by decision (Q4).
- Region/gadget (`WireAnchor::Storage`) changes — anchors already carry an
  explicit `StorageId` and work unchanged.
