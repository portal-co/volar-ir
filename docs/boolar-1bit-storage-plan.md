# Plan: Boolar IR storage is always 1 bit

> **Current circuit path:** movfuscated execution produces one typed step;
> [`pipeline.md`](pipeline.md) defines the external state driver and Boolar adapter.

**Status:** Landed — all six phases implemented and committed. The only
deliberate deviation from the draft: the Phase 4 storage round-trip property
(`prop_d2_lower_ir_storage_roundtrip_preserves_semantics`) compares full
program semantics of extended (storage-bearing) generated programs rather
than a dedicated write-then-read reassembly harness, and the lowering gained
a fail-closed 64-bit address-budget check (`element-address bits +
ceil(log2(value bits)) <= 64`) that the plan's Rule 3 implied but did not
spell out.
**Scope:** `crates/ir/volar-ir/src/boolar.rs`,
`crates/ir/volar-ir-common/src/lib.rs` (`PreInitSegment`),
`crates/ir/volar-ir-passes/src/lower_ir_to_boolar.rs` (producer),
`crates/ir/volar-ir-opt/src/{biir,store_forward,substitute_ir,common}.rs`,
`crates/ir/volar-ir-passes/src/{movfuscate,lower_to_circuit,lower_lir,to_reversible}.rs`,
`crates/ir/volar-ir-virt/src/bir.rs`, `crates/ir/volar-ir-text/src/boolar.rs`,
`crates/ir/volar-lir/src/circuits.rs`,
`crates/ir/volar-ssa-lir-replay/src/lower_vaffle.rs`,
`crates/fuzz/volar-fuzz/src/{interpreter,generators,arbitrary}/biir.rs`,
`docs/agent-context/ir-types-storage.md`.

---

## Motivation

Boolar IR's whole point is that **every value is exactly one bit**. Storage
breaks that promise today: `BIrStmt::StorageRead` has a `bit_width: usize`
field and returns a single result var that "represents the entire
`bit_width`-wide value as a single Boolar handle". One Boolar value pretends
to hold more than a bit — *bit-stuffing*. This infects everything downstream:

- **Interpretation**: the fuzzer evaluator must pack/unpack multi-bit handles
  around a map that is fundamentally `(storage, addr) → bool`
  (`crates/fuzz/volar-fuzz/src/interpreter/biir.rs`), and its keying disagrees
  with the documented `(StorageId, bit_width, addr)` lanes.
- **Passes**: DCE/CSE/store-forwarding/substitution in `volar-ir-opt` and the
  movfuscator must special-case `StorageRead`/`StorageWrite` shapes whose
  result var has a phantom width no other statement can see.
- **Usability**: consumers can't touch individual bits of a stored value at
  the Boolar level ("individual bits are not separately addressable"), so
  every real consumer re-invents expansion anyway.
- **Reversible pipeline readiness**: `RCircuit::RGate::StorageSwap` exchanges
  exactly one wire with exactly one addressed cell. As long as Boolar storage
  traffic is packed multi-bit handles, `to_reversible` cannot synthesize
  storage access — the main blocker flagged in
  `docs/circuit-fused-ir-plan.md`.

The fix: make the exception go away. **A Boolar storage cell holds 1 bit,
uniformly, everywhere.** Multi-bit Volar values are lowered to one Boolar
storage op *per bit*, with the value's bit index appended to the address.

## Current state (what changes)

| Site | Today |
|---|---|
| `BIrStmt::StorageRead { storage, bit_width, addr }` | Result var = packed `bit_width`-wide handle |
| `BIrStmt::StorageWrite { storage, src, bit_width, addr }` | `src` = packed handle; produces dummy bit |
| Fuzz interpreter | `BIrStorageMap = BTreeMap<(StorageId, u64), bool>`; `bit_width` ignored at eval time |
| Documented semantics (`docs/agent-context/ir-types-storage.md`) | Lanes keyed by `(StorageId, bit_width, addr)` |
| `lower_ir_to_boolar.rs` | Passes storage ops through opaquely; reads/writes only **bit 0** of the value (lossy) |
| `BIrBlocks.pre_init: Vec<PreInitSegment>` | Typed segments (`TypeId`, packed `Constant`s, element offsets) |
| `RCircuit` `StorageSwap` | 1-bit exchange keyed by `(StorageId, collapsed u64 addr)` — already uniform |

## Design

### Rule 1 — one bit per cell, no width field

```rust
// BEFORE                                          // AFTER
StorageRead {                                      StorageRead {
    storage: Stor,                                     storage: Stor,
    bit_width: usize,          // deleted              lane: LaneId,      // see Rule 2
    addr: Vec<Var>,   // N-bit address                addr: Vec<Var>,    // N-bit address
}                                                  }   // result: one bit, like every var

StorageWrite {
    storage: Stor,
    lane: LaneId,
    src: Var,         // one bit
    addr: Vec<Var>,
}
```

- `bit_width` is **deleted** from both variants. Every Boolar var involved is
  a bit; nothing about these statements differs from `And`/`Xor` anymore.
- Reads return one bit; writes take one bit and still produce a dummy zero.

### Rule 2 — the type ID disambiguates, it does not specify

Different Volar types may legally share one `StorageId` (e.g. a `u32` slot and
an `f32` slot in the same space). With widths gone, something must keep those
lanes from colliding — but that something carries **no width semantics**: it
only names a lane.

- New opaque `pub struct LaneId(pub u32)` (in `boolar.rs`; rkyv-derived).
  Produced by `lower_ir_to_boolar` from the Volar `TypeId` via dense
  first-use renumbering, with an emitted `LaneId → TypeId` side table from the
  same pass run (resolved decision 1 below).
- The interpreter's storage key becomes
  `BTreeMap<((StorageId, LaneId), u64), bool>` — restoring agreement between
  code and `docs/agent-context/ir-types-storage.md`.
- `LaneId` is never inspected for a width anywhere in the codebase. That's the
  "disambiguating, not specifying" contract; a debug assertion-free rule, not
  a runtime check.

### Rule 3 — lowering appends the bit index as address bits

For a k-bit Volar value at Volar address bits `a[0..N]` (LSB-first), bit `i`
of the value lowers to a Boolar op addressed by:

```text
addr' = a[0..N] ++ bits_of(i)        // appended ⇒ high-order above a[N-1]
```

i.e. the effective flat cell index is `base + (i << N)` within the
`(StorageId, LaneId)` space. Concretely, `IRStmt::StorageRead{..}` of a 32-bit
type emits 32 `BIrStmt::StorageRead`s (one per `var_bits` slot of the result),
and `IRStmt::StorageWrite` emits 32 writes fed from the source value's bit
wires. This also fixes today's silent lossiness (only bit 0 moved).

Layout consequences, stated up front:
- Each typed value occupies a strided region (stride `2^N` cells). Contiguous
  packing was never observable through the old API either, because reads were
  opaque whole-value handles.
- Total address width grows by `ceil(log2(k))` bits per op. Acceptable: Boolar
  is an end-stage representation and addresses are wires, not heap.

### Rule 4 — pre-init goes bit-granular

`BIrBlocks.pre_init` switches from the shared typed `PreInitSegment` to a
Boolar-local segment:

```rust
pub struct BIrPreInitSegment {
    pub storage: StorageId,
    pub lane: LaneId,
    /// Flat bit-cell offset within the (storage, lane) space, using the same
    /// appended-address layout as Rule 3.
    pub offset: u64,
    pub data: alloc::vec::Vec<bool>,
}
```

`volar-ir-common::PreInitSegment` stays untouched (VAFFLE/Volar still need
typed constants); `lower_ir_to_boolar` expands each typed segment into bit
cells via the same `base + (i << N)` arithmetic. `apply_pre_init` in the fuzz
interpreter moves to the new type.

### What does NOT change

- `addr: Vec<Var>` stays a `Vec` of bit vars, LSB-first, index 0 = LSB.
- `StorageId` semantics, oracle/action/RNG statements, block structure.
- `RCircuit`/`StorageSwap`: already 1-bit-uniform. Only its storage key gains
  the `LaneId` (Phase 5) so reversible circuits can address the same spaces.

## Phases

### Phase 1 — core types (`volar-ir`, `volar-ir-text`)

Work items:

- [x] Add `LaneId`; rewrite both storage variants without `bit_width`.
- [x] `map`/`as_ref`/`as_mut` arms updated per
      `docs/agent-context/ir-map-conventions.md`.
- [x] New `BIrPreInitSegment`; `BIrBlocks.pre_init` retyped.
      Catch-all arms stay catch-alls (repo rule 1).
- [x] Text format (`volar-ir-text/src/boolar.rs`): parse/print the new shapes;
      round-trip tests updated. Old `bit_width` syntax rejected with a clear
      error.
- [x] rkyv derives still compile under `--features rkyv`.

### Phase 2 — producer (`lower_ir_to_boolar`)

Work items:

- [x] Lane allocation: build the `TypeId → LaneId` map for a lowering run
      (dense renumbering over the module's type table; deterministic order =
      first-use order). Emit the watchlist-style side table mapping every
      allocated `LaneId` back to its source `TypeId` alongside the output —
      same-pass-byproduct rule from the fused-IR plan applies (no drift).
- [x] Per-bit expansion for reads and writes per Rule 3, wiring results into
      `var_bits[..]` slots (this replaces the lossy bit-0-only plumbing).
- [x] Typed pre-init → `BIrPreInitSegment` expansion.
- [x] Unit tests: write-then-read round-trip of a multi-bit constant through
      the lowered form; distinct lanes don't collide for same-`StorageId`
      different-type slots; appended-address layout asserted explicitly.

### Phase 3 — mechanical consumers (passes, opt, virt, lir)

All of these currently thread `bit_width` through clone/map arms or match on
the storage variants; they become *simpler* (one fewer field, no special
result-shape reasoning):

- [x] `movfuscate.rs`, `lower_to_circuit.rs`, `lower_lir.rs`,
      `to_reversible.rs` — arm updates only.
- [x] `volar-ir-opt/src/{biir,common,store_forward,substitute_ir}.rs` +
      `src/ir.rs` — arm updates; revisit any store-forwarding logic that
      reasoned about packed handles (expected deletions).
- [x] `volar-ir-virt/src/bir.rs`, `volar-lir/src/circuits.rs`,
      `volar-ssa-lir-replay/src/lower_vaffle.rs` — arm updates.
- [x] Semantics-preservation signal: existing Properties A–D keep passing
      (evaluate before/after; nothing here asserts IR shape).

### Phase 4 — fuzz harness

Work items:

- [x] Interpreter: `BIrStorageMap` key `((StorageId, LaneId), u64)`;
      `apply_pre_init` on the new segment type; drop pack/unpack paths.
- [x] Generators/arbitrary (`generators/biir.rs`, `arbitrary/*`): generate
      storage ops without widths; keep address widths small.
- [x] New property: **storage round-trip** — a generated program writing a
      computed k-bit value then reading all k bits back reassembles the value
      (per-bit, exercising Rule 3 end to end).
- [x] `docs/fuzzing.md` notes if property numbering shifts.

### Phase 5 — reversible readiness (`rcircuit.rs`, `to_reversible.rs`)

Work items:

- [x] `RCircuit` storage model: `StorageState` key gains `LaneId`;
      `RGate::StorageSwap` addr validation unchanged otherwise.
- [x] Extend `to_reversible` to synthesize per-bit storage access: Boolar
      `StorageRead` → copy-out via `StorageSwap` into a fresh ancilla (then
      swap back after last use — Bennett style); `StorageWrite` → compute into
      ancilla, `StorageSwap` in. Remove the `UnsupportedStmt` rejection for
      storage ops; keep rejecting oracles/actions/RNG.
- [x] Property E extended: programs with storage traffic embed reversibly;
      inverse∘circuit identity now includes random initial storage contents.

### Phase 6 — docs & backlog

Work items:

- [x] `docs/agent-context/ir-types-storage.md`: replace the
      `(StorageId, bit_width, addr)` lane description with
      `((StorageId, LaneId), addr)` and document the appended-address layout.
- [x] `docs/pipeline.md` Boolar row mention if needed.
- [x] Circuit-size backlog (`circuit-size-optimization-backlog.md`): strike
      any entry blocked on "Boolar pretends values are wider than bits".

## Risks

| Risk | Mitigation |
|---|---|
| Address-space growth (`k · 2^N` cells per typed value) | Bounded by generator/frontend address widths; document the stride; revisit only if a consumer shows real blowup |
| Silent semantic drift in consumers during the mechanical sweep | Phase 3 is arm-updates only; Properties A–D are the correctness signal, not shape assertions |
| Fuzz regression files pinned to old shapes | Regenerate `proptest-regressions` entries; generators change in the same commit |
| Lane collision bugs (two TypeIds mapping to one `LaneId`) | Dense renumbering in one place (Phase 2); dedicated unit test for same-`StorageId` different-type slots |
| rkyv archive format break | Internal crate; no persisted archives; bump is a non-event |
| `to_reversible` storage synthesis unbounded garbage ancillas | Same accepted trade-off as the rest of the naive Bennett transform; documented, not solved |

## Resolved decisions (formerly open questions)

1. **Lane table visibility** — resolved: dense `LaneId` renumbering **plus a
   watchlist-style side table**. As with the `VarWireMap`/watchlist pattern in
   the circuit-fused plan, the lowering emits an explicit, total
   `LaneId → TypeId` table alongside the lowered block set: compact in the IR
   (dense ids), inspectable by tooling (the table), and produced by the same
   pass run that allocates the lanes so it can't drift. Phase 2 work items
   updated accordingly.
2. **Appended index bits confirmed** — bit index occupies the high-order bits
   above the base address; flat cell index is `base + (i << N)` within the
   `(StorageId, LaneId)` space. No layout change.
3. **`StorageWrite` keeps producing a dummy zero** — behavior preserved; no
   result-shape change rides along with this refactor.
