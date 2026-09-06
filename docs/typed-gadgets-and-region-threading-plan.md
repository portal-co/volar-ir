# Plan: Typed Gadgets, Higher-Level IR Gadgets, and Region Threading Through Passes

> **Historical note:** typed gadget regions now cross `terminated` and
> `next_state` through the one-step boundary in [`pipeline.md`](pipeline.md).

**Status:** Landed — Phases 1–4 implemented (Phase 5 doc updates: this file).
Deviations from the draft below:

- **Typed gadgets/lowerings live in `volar-ir/src/typed_gadget.rs`
  (`TypedAnchor`/`TypedRegionTable`/`TypedGadgetSpec`/`TypedAuxSource`/
  `validate_typed_gadget`) and
  `volar-ir-passes/src/region_lowering.rs` (`lower_typed_region_table`,
  `lower_gadget_library`, `lower_typed_bindings`, `vcircuit_output_widths`,
  `movfuscate_region_layout`, `translate_regions_movfuscate`,
  `translate_typed_aux_movfuscate`, `movfuscate_state_regions`,
  `translate_regions_termination_flag`, `translate_regions_to_reversible`).
- **Var-bit allocation exposure:** `lower_ir_to_boolar_with_tables` returns
  a `LoweredTables` bundle (`lanes`, `VarBitMap`, `addr_widths`) instead of a
  separate opt-pass side table.
- **Movfuscation layout:** `movfuscate_region_layout` re-runs
  movfuscate's own `(position, type)` slot analysis
  (`compute_static_slot_classes` + `compute_return_slot_types`, now
  `pub(crate)`) without running the pass; `movfuscate`'s signature is
  unchanged as planned.
- **Property G′ generator restricted to Bit-typed block params** — this
  exposed a **pre-existing `movfuscate_ir` bug with `Vec`-typed state
  slots**: a 2-block identity swap over `Vec(2, Bit)` params loses slot 1
  through the movfuscated step circuit (reproduced at both the typed-step
  and Boolar levels, while the pre-movfuscation program and its Boolar
  lowering are both correct). Properties A/M don't cover it (BIr slots are
  1-bit; Property M's generators use 1-slot scalars). Widening G′ to word
  params is blocked on that fix; typed word-width coverage lives in
  `region_lowering::tests::typed_table_and_gadget_lower_and_splice`
  (8-bit gadget ports on a `VCircuit` host) and the per-plane storage
  fan-out test.
- **VAFFLE tables** live in `volar-vaffle-target/src/vaffle_regions.rs`
  (`validate_vaffle_regions`, `translate_vaffle_regions`): entry-function
  `FuncInput` anchors lower to IR `Input` anchors; `FuncOutput` anchors are
  rejected at the IR boundary (no module-level outputs — author them
  against the unrolled circuit); non-entry-function anchors fail closed
  (call edges dissolve during lowering).
- **Text sections** in `volar-ir-text/src/typed_regions.rs`
  (`typed_regions {}`, `typed_gadgets {}`, `typed_gadget_specs {}`):
  gadget *bodies* are not text-serialized — specs round-trip as
  port-signature stubs (`TypedGadgetSpecStub`).
**Prerequisite:** [`wire-regions-gadgets-plan.md`](wire-regions-gadgets-plan.md)
(landed: bit-level `RegionTable`/`RegionSelector` in `volar-ir/src/region.rs`,
`GadgetSpec`/`GadgetBinding` in `gadget.rs`, the `apply_gadgets` splicer,
text sections, Property G). This plan extends that layer upward.
**Scope:** `crates/ir/volar-ir` (typed region/gadget types), new
`region_lowering.rs` + region-translation helpers in
`crates/ir/volar-ir-passes`, typed-table lowering support in
`crates/ir/volar-ir-opt` (expose var-bit maps), VAFFLE-side tables in
`crates/ir/vaffle`-adjacent code, fuzz properties in `crates/fuzz/volar-fuzz`.
**Kind:** New companion-metadata types + pure translation functions + one new
lowering step. No changes to existing pass semantics; Properties A–G remain
unmodified except where a property is extended (noted per phase).

---

## Motivation

The landed v1 anchors regions on the **fused `BCircuit` boundary in raw bit
positions**, and gadget ports carry bare bit widths. That is the right
splicing substrate, but the wrong *authoring* substrate:

1. **Callers think in typed terms.** "The `_32` word at param 1", "the
   `(STACK, _8)` stack slots", "the `_128` AES block on output 0" — not
   "input bits 37..69". Hand-computing bit offsets from `var_bits`
   allocation rules is error-prone and must be redone whenever a width
   changes.
2. **Gadget bodies are authored typed.** A real block cipher gadget is
   naturally a typed `VCircuit`/`IRBlocks` (Poly-typed words, shuffles,
   rotates) — writing it as raw Boolean gates by hand defeats the compiler
   stack above the splice point.
3. **The boundary moves under the table.** Every pass between the typed
   module and the fused circuit reshapes the boundary — movfuscation
   *replaces* it with `[pc_bits, state_slots…]` + `[done, ret…]`, IR→Boolar
   widening one typed var into many bit vars, unrolling prepends `done`.
   A region table authored at one level is invalid at the next unless some
   component translates it. Today nothing does.
4. **Internal state slots need regions.** After movfuscation, the combined
   block's parameters *are* the machine's internal state: the `⌈log₂N⌉` PC
   bits and the threaded state slots. Callers weaving proofs or garbling
   need to name these wires ("this slot is secret state", "wrap the PC with
   this gadget") — i.e. movfuscation's output boundary must be
   region-addressable, including the slots it invents.

Three workstreams address this: **(A)** typed region/gadget tables and typed
gadget bodies at the `VCircuit` level; **(B)** the same for field-level
`IRBlocks` and VAFFLE modules (function-scoped boundaries); **(C)** explicit,
fail-closed region-table translation for every pass that moves the boundary —
the "region threading" contract.

---

## Background: what exists at each level

| Level | Type | Boundary shape | Storage |
|---|---|---|---|
| VAFFLE | `vaffle::Module` | functions (`SigDecl { params, results: Vec<TypeId> }`), typed block params (`Value::Param { block, ty, idx }`), calls between functions | `StackAlloc { elem_ty, count, base_slot }` → `StorageId::STACK` slot ranges; typed `PreInitSegment` |
| Volar IR | `IRBlocks` | one function, blocks with `params: Vec<TypeId>`, terminators | `(StorageId, TypeId, addr)`; typed `PreInitSegment` |
| Typed fused | `VCircuit` | `params: Vec<TypeId>`, straight-line typed stmts, `outputs: Vec<IRVarId>` | same as IR |
| Boolar | `BIrBlocks` → fused `BCircuit` | bit params/outputs, single block | `((StorageId, LaneId), flat cell)`; bit `pre_init` |
| Reversible | `RCircuit` | wire `i` ↔ Boolean wire `i` (via `VarWireMap`) | — |

Key mechanics the translations rely on (all verified in-tree):

- `lower_ir_to_boolar` allocates **lanes** by dense first-use over
  `IRTypeId` (`LaneId → IRTypeId` side table) and expands each typed var
  into a `var_bits: BTreeMap<u32, Vec<IRVarId>>` bit list — param `i`'s bits
  are allocated contiguously in param order. A multi-bit value at element
  address `A`, bit plane `i`, lives in flat cell `base + A + (i << N)` where
  `N` is the lane's element-address width. Typed `pre_init` segments expand
  to one strided bit segment per plane.
- `movfuscate_ir(blocks, types)` produces a single block with
  `params = [pc_bit_0 … pc_bit_{k-1}, state_0 … state_{w-1}]`
  (`k = ⌈log₂ N⌉`; state slots carry the original block params' types) and
  terminator `JumpCond(done, Return(ret), Block(0, [next_pc…, next_state…]))`.
  It already emits rich boundary metadata (`MovfuscBlockBoundary`,
  `MovfuscAccumInfo`, `remap_movfusc_*`) threaded through later passes.
- `lower_to_circuit` unrolls the movfuscated self-loop and **prepends a
  single `done` bit to the return values**; `lower_to_circuit_fused` /
  `to_circuit_fused_boolar` give the fused `BCircuit` the landed region
  table anchors on.
- `to_reversible` returns a `VarWireMap` and already ships the
  value→wire `translate_watchlist` precedent for exactly this kind of
  companion-metadata translation.

---

## Design

### Guiding rules

1. **One splicer.** All workloads still bottom out in the landed bit-level
   `apply_gadgets`. Typed layers *author and translate*; they do not splice.
   This keeps Property G as the single semantics gate and avoids a second
   correctness story for typed `Poly` splicing.
2. **Companion tables are translated, never silently mutated.** Every pass
   that moves the boundary gets an explicit region-table contract (below):
   either the table provably survives unchanged, or a pure translation
   function maps it, or the pass refuses the table (fail-closed). No
   pass discovers regions by pattern-matching the IR.
3. **Fail-closed on ambiguity.** A translation that cannot name where an
   anchor went (slot type disagreement, function inlined away, storage
   space whose traffic vanished) errors; it never guesses. This matches the
   landed v1's posture and `remap_movfusc_boundary`'s panic-on-missing-entry
   precedent.
4. **Positions over identities.** Anchors stay positional (bit ranges,
   slot ranges, output positions) so translations are arithmetic, not
   symbolic. Where a pass is identity-on-positions, the table passes
   through untouched after revalidation.

### Workstream A — Typed gadgets at the `VCircuit` level

New types (hand-written, rkyv-gated like the landed ones; suggest
`crates/ir/volar-ir/src/typed_gadget.rs`):

```rust
/// A typed boundary anchor. Bit ranges are *within the typed var's bit
/// layout* (LSB-first, `ir_type_bits`), not global wire positions.
pub enum TypedAnchor {
    /// Bit range `[start, start+len)` of input param `param`.
    Input { param: u32, start: u16, len: u16 },
    /// Bit range of output position `out` (`VCircuit::outputs[out]`).
    Output { out: u32, start: u16, len: u16 },
    /// Typed storage value range: addresses `[addr_start, addr_start+addr_len)`
    /// of `(storage, ty)` — one anchor covers *all bit planes* of the cell.
    Storage { storage: StorageId, ty: IRTypeId, addr_start: u64, addr_len: u64 },
}

pub struct TypedPort { pub name: String, pub kind: PortKind, pub ty: IRTypeId, pub count: usize }

pub struct TypedGadgetSpec {
    pub name: String,
    pub ports: Vec<TypedPort>,          // exactly one PortKind::Data, as in v1
    pub encrypt: VCircuit,              // typed body; params = flattened port words
    pub decrypt: Option<VCircuit>,      // None = self-inverse
}

pub struct TypedGadgetLibrary { pub gadgets: Vec<TypedGadgetSpec> }

/// Aux at typed granularity; lowers to the landed bit-level `AuxSource`.
pub enum TypedAuxSource {
    /// Bits `[start, start + needed)` of input param `param`.
    InputRange { param: u32, start: u16 },
    /// One constant per needed bit, from typed `Constant`s flattened LSB-first.
    Const(Vec<Constant>),
    /// Still rejected downstream in v1 (see landed plan).
    Rng(String),
}
```

Body conventions mirror the landed bit-level ones exactly: gadget body
params are the port words flattened in declaration order (Data, then Aux,
then Rng), outputs are the Data words; bodies must be pure (no storage,
oracle, action, RNG; `Poly` is fine — it's an ordinary typed stmt and
booleanizes through the normal lowering). `Data` port bit width is
`count × ir_type_bits(ty)`; selection chunking at bit level works per
landed rules after lowering.

Typed region tables (`TypedRegionTable`) reuse the landed
`RegionSelector`/`RegionEntry` shape with `TypedAnchor` in place of
`WireAnchor`, plus the same validation posture (bounds via `ir_type_bits`,
known-storage checks against the host's traffic and `pre_init`).

**Application is lowering, not splicing.** One new module
(`volar-ir-passes/src/region_lowering.rs`) provides:

- `lower_typed_region_table(host: &VCircuit, table: &TypedRegionTable,
  types: &IRTypes, var_bits: &VarBitMap) -> Result<RegionTable, …>` —
  maps each typed anchor to landed bit anchors: `Input`/`Output` anchors
  shift by the param/output base offsets from the var-bit allocation;
  one `Storage` anchor fans out to one landed `WireAnchor::Storage` per
  bit plane `i ∈ [0, bits(ty))`, each covering flat cells
  `[addr_start + (i << N), addr_start + addr_len + (i << N))`.
- `lower_gadget_library(lib: &TypedGadgetLibrary, types: &IRTypes)
  -> Result<GadgetLibrary, …>` — booleanizes each typed body through the
  standard typed→Boolar→fused pipeline and checks the flattened bit shape
  matches the declared ports.

Enabler: `lower_ir_to_boolar` currently keeps `var_bits` internal. Add
`lower_ir_to_boolar_with_var_map` (or a `VarBitMap` side table in the
spirit of the existing `LaneId → IRTypeId` table) so table lowering
consumes the *same* allocation the lowering produced — computed, never
re-derived.

### Workstream B — Gadgets for `IRBlocks` and VAFFLE

Same typed vocabulary, anchored at the richer boundaries:

- **`IRBlocks`**: the `TypedRegionTable` above, with anchors additionally
  scoped to a block: `Input { block: IRBlockId, param: u32, … }` (block
  params are typed: `IRBlock.params: Vec<TypeId>`). Storage anchors and
  `pre_init` are program-scoped. After `movfuscate_ir`, block-param
  anchors are what Workstream C's movfuscation translation consumes.
- **VAFFLE**: anchors gain a function scope —
  `Param { func: FuncId, idx, start, len }`,
  `Result { func: FuncId, idx, start, len }`, plus program-scoped
  `Storage { storage, ty, addr_start, addr_len }` (VAFFLE storage is
  `StorageId::STACK` slot ranges from `StackAlloc.base_slot` and the typed
  module `pre_init`). Gadget bodies at this level are single-function
  VAFFLE modules (no calls after inlining, no oracle/action/RNG/storage)
  booleanized by the same `lower_gadget_library` path.

Like Workstream A, **application is deferred to the bit level**: a VAFFLE
table lowers through `volar-vaffle-target`'s deterministic call→jump
lowering into an `IRBlocks` table, which lowers through movfuscation
translation + booleanization into the landed `RegionTable`, and the splice
happens once. Native typed splicing (rewriting VAFFLE function boundaries
with call edges to gadget functions before lowering) is explicitly a
non-goal for now — see Non-goals.

Interaction with opt passes: `inline_vaffle` / `substitute_vaffle` remove
or clone functions; anchors on a function that was inlined away must fail
closed (the caller re-authors against the post-inline module), while
anchors on surviving functions follow the module's own param/result
reordering only if such a pass ever introduces one (none does today).

### Workstream C — Region threading through passes

The contract, per pass. "Translation" means a pure function
`(old_table, pass_metadata) -> Result<new_table, RegionThreadError>`;
translation functions live next to their passes and are unit-tested
against hand-built tables plus covered by a property (Phase 3).

| Pass | Boundary effect | Region contract |
|---|---|---|
| `fold_ir_blocks` / `fold_biir_blocks` / `batch_ir_blocks` / CSE / store-forward | stmt-level only; params, outputs, storage spaces preserved | Table revalidated against the post-pass host. Edge case: store-forward can erase *all* traffic in an anchored `(StorageId, TypeId)` space — the anchor is then vacuous; policy: drop with a reported warning (fail-closed-lite, mirroring landed `UnknownStorageSpace`). |
| `dce_ir_blocks_with_remap` | returns a var remap | New `translate_regions_remap(&remap, table)` — boundary anchors don't reference interior vars, so this validates rather than rewrites; provided for tables that later gain var-scoped anchors. |
| `movfuscate_ir` / `movfuscate_biir` | **redefines** params (`[pc, state…]`) and outputs (`done` + ret accumulators) | `translate_regions_movfuscate(pre_table, boundaries: &[MovfuscBlockBoundary], layout) -> Result<TypedRegionTable, …>`: block-param anchor `(b, i)` → the state-slot range hosting that param; output anchors → ret accumulator positions; storage anchors pass through. Plus `movfuscate_state_regions(layout, slot_regions) -> TypedRegionTable` emitting anchors for the **invented internal slots**: PC bits under a conventional `pc` region id and each state slot under caller-supplied region sets (a `BTreeSet<RegionId>` per slot, positionally). Both are pure functions over movfuscation's already-emitted metadata — `movfuscate`'s signature does not change. |
| `lower_ir_to_boolar` | typed vars → bit vars; typed storage → lanes | `lower_typed_region_table` (Workstream A), driven by the newly exposed `VarBitMap` + lane table. Input anchors map through contiguous param allocation; `Storage` anchors fan out per bit plane via the `base + A + (i << N)` layout. |
| `lower_to_circuit` (unrolling) | params preserved; outputs = `[done] ++ ret` | Output anchors shift by `+1` output position (the prepended `done` bit); the never-terminated fallback exposes final state in the ret positions — anchors stay positional, and the `done`-flag position itself gets no anchor unless the caller adds one. Documented + unit-tested, not a separate function. |
| `to_circuit_fused_*` / `fuse_to_circuit` | single-block fuse; params/outputs preserved | Identity; revalidate only. |
| `to_reversible` | var → wire (`VarWireMap`) | `translate_regions_watchlist`-style helper over the existing map (Input anchors are wire positions on both sides; the map disambiguates). Follows `translate_watchlist` exactly. |
| `volar-ir-virt` (virtualization) | wholesale restructure (handler module + bytecode) | **Reject** tables in v1: virtualized outputs are not the original boundary. Explicit non-goal to auto-translate. |
| `apply_gadgets` | consumes the table; wraps the boundary | Already fail-closed on mismatch. Follow-up (non-blocking): emit a *post-application* table marking the newly ciphertext faces with fresh region ids, so downstream consumers can name them. |

Two related notes:

- **Naming collision.** `movfuscate::thread_synthetic_slots` already uses
  the word "regions" (`region_sets_final`) for *split-driver chunk
  locality ids* — an unrelated concept. This plan's doc comments and the
  new APIs must disambiguate explicitly; a follow-up rename of
  `region_sets_final` → `chunk_origin_sets` is desirable but out of scope.
- **Provenance precedent.** `ProvenanceHandler` / `map_prov_with_handler`
  is the established pattern for threading companion annotations through
  passes. We deliberately do *not* fold regions into provenance: regions
  are validated structural metadata (intervals over boundaries), not
  per-statement annotations, and keeping them as explicit side tables with
  pure translations preserves the fail-closed posture.

---

## Testing

1. **Unit tests per translation function** (each fail-closed path
   exercised): typed→bit fan-out for multi-plane storage anchors;
   movfuscation translation incl. slot-type-disagreement and
   single-block-passthrough cases; unroll `+1` output shift; reversible
   wire translation via `VarWireMap`.
2. **Property G′ — translation preserves the splice.** Extend the
   Property G generator: author the table at the typed level, then
   `lower → (translate through movfuscate/unroll) → apply_gadgets → eval`
   on ciphertext inputs must equal `eval(host)` on plaintexts, exactly as
   Property G states at bit level. Fails loudly if any translation
   misplaces an anchor.
3. **Property H — typed gadget bodies.** Random typed XOR/word gadgets
   (`_8`/`_32` Data ports, `Const` aux) authored as `VCircuit` bodies;
   `lower_gadget_library` output spliced by the landed pass must satisfy
   the same round-trip as bit-level Property G.
4. **Existing suites untouched.** All translation entry points are new
   functions; no pass signature changes. Properties A–G pass unmodified
   except G′, which is additive.

## Phases

### Phase 1 — typed region/gadget types (`volar-ir/src/typed_gadget.rs`)
- [x] `TypedAnchor`, `TypedRegionTable`, `TypedPort`, `TypedGadgetSpec`,
      `TypedAuxSource`, `TypedGadgetLibrary`, thread-error type.
- [x] Validation (bounds via `ir_type_bits`, known-storage) + rkyv
      round-trips + text-format sections (printer/parser mirroring
      `volar-ir-text/src/regions.rs`).

### Phase 2 — table & library lowering (`volar-ir-passes/src/region_lowering.rs`)
- [x] Expose the var-bit allocation from `lower_ir_to_boolar`
      (`VarBitMap` side table; `volar-ir-opt` unchanged).
- [x] `lower_typed_region_table` (incl. per-plane storage fan-out) and
      `lower_gadget_library` (typed body booleanization + shape check).
- [x] Unit tests from §Testing.1.

### Phase 3 — movfuscation + pipeline translations
- [x] `translate_regions_movfuscate` + `movfuscate_state_regions`
      (internal PC/state-slot anchors).
- [x] Unroll output-shift helper (or documented constant), reversible
      `VarWireMap` translation.
- [x] **Property G′** in `volar-fuzz`.

### Phase 4 — VAFFLE-level tables
- [x] Function-scoped anchors; VAFFLE→IR table lowering through
      `volar-vaffle-target`; inline/substitute fail-closed rules.
- [x] **Property H** + text/rkyv coverage for VAFFLE tables.

### Phase 5 — docs
- [x] `docs/pipeline.md`: diagram gains the typed-table lane;
      `docs/wire-regions-gadgets-plan.md`: point here for typed/upper
      layers; this file's Status → Landed with deviations.

## Non-goals (explicit follow-ups, not this plan)

- **Native typed splicing** (gadget calls rewritten into VAFFLE/IR before
  booleanization). Lowering-then-splicing subsumes it; revisit only if a
  consumer needs typed gadgets to survive *typed-level* optimization.
- **Virtualization region translation.** The virt output boundary is a
  different machine; tables are rejected there in v1.
- **Stacked bindings, RNG keystream channel, side-tag↔region bridge** —
  unchanged from the landed plan's non-goals; the typed layer inherits
  them as-is.
- **Renaming `region_sets_final`** — noted, deferred.
