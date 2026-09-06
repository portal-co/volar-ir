# Plan: Input/Output Wire Regions and Gadgets

> **Current circuit path:** region anchors cross a typed step boundary as
> documented in [`pipeline.md`](pipeline.md), then explicitly flatten for consumers.

**Status:** Landed — Phases 1–5 implemented. `RegionTable`/`RegionSelector`
 live in `crates/ir/volar-ir/src/region.rs`, gadget types in `gadget.rs`,
 the splicer in `crates/ir/volar-ir-passes/src/apply_gadgets.rs`, text
 sections in `crates/ir/volar-ir-text/src/regions.rs`, Property G in
 `crates/fuzz/volar-fuzz/src/properties/gadgets.rs`, rkyv round-trips in
 feature-gated `rkyv_tests` modules.

Implementation refinements vs. the draft below (all fail-closed, none
semantics-changing):

- **Gadget bodies carry both directions**: `GadgetSpec { encrypt,
  decrypt: Option<…> }` — `decrypt: None` means self-inverse. Bodies must
  be pure gates; storage/oracle/action/RNG inside a body is rejected.
- **Wider selections chunk**: a binding whose selection exceeds the data
  port width instantiates the gadget once per data-port-width chunk
  (inputs/outputs), instead of requiring an exact single-instantiation fit.
- **Storage wrapping is static-address only** (dynamic addresses in a
  wrapped `(StorageId, LaneId)` space → `DynamicStorageAddress`), and
  **synthetic pre-init** is emitted for wrapped cells with no host segment
  (default-zero cells must hold ciphertext-of-zero, not plaintext zero).
- **`AuxSource::Rng` / `Rng` ports are rejected by the splicer**
  (`UnsupportedRng`): wrap/unwrap keystream consistency needs an out-of-band
  channel (gadget side outputs) — follow-up work.
- **`pre_init` re-encryption requires all-`Const` aux** for the owning
  binding (`PreInitNeedsConstantAux`).
**Scope:** `crates/ir/volar-ir` (new hand-written region/gadget types), new
 `apply_gadgets.rs` pass in `crates/ir/volar-ir-passes`, printer/parser in
 `crates/ir/volar-ir-text`, fuzz generators in `crates/fuzz/volar-fuzz`.
**Kind:** New boundary-metadata layer + one new lowering pass. No changes to
 existing pass semantics; existing properties A–E remain unmodified.
**First target level:** `BCircuit` (bit-level, storage cells, `pre_init`).
 `VCircuit` (typed) is the second target; field-level `IRBlocks`/VAFFLE are
 out of scope for v1 (see Non-goals).

---

## Motivation

Circuit consumers in `volar` (weaving, garbling, FHE) increasingly need to
reason about **which boundary wires play which role** — "these input bits are
the public message", "these storage cells hold the key", "these outputs are
the ciphertext face" — and to **wrap whole families of boundary wires with a
sub-circuit** without touching the core. The motivating example:

> Encrypt/decrypt all *public-facing* wires **outside** a designated
> *plaintext* region. Wires tagged `public` that are not also tagged
> `plaintext` cross the circuit boundary in ciphertext form; the plaintext
> region (and, say, a `key` region) stays untouched.

Today this can only be done by hand-editing a circuit or by special-casing in
each consumer. There is no way to say, declaratively:

1. **(Regions)** "this function input / this function output / this storage
   value / this temp-or-state slot belongs to region(s) R₁, R₂, …", and
2. **(Gadgets)** "attach this sub-circuit to every wire in region set S of
   that circuit's inputs/outputs, applying `E` on the way out and `E⁻¹` on
   the way in".

This plan adds both, as a side-table metadata layer plus one splicing pass,
so the core IR, its optimization passes, and its evaluators stay untouched.

## Background: where boundaries live today

| Boundary | `BCircuit<P>` / `BIrBlocks<P>` shape |
|---|---|
| Function inputs | `params: u32` — a bare bit count, no per-param structure |
| Function outputs | `outputs: Vec<IRVarId>` |
| Storage values / temp / state slots | `BIrStmt::StorageRead`/`StorageWrite { storage: StorageId, lane: LaneId, addr: Vec<Var> }`; every cell is one bit, keyed by `(StorageId, LaneId, flat-addr)`; `pre_init: Vec<BIrPreInitSegment>` initializes cells with constants |
| Internal temps | ordinary SSA vars (not a boundary; explicitly out of scope) |

Two structural facts drive the design:

- **There is no node to hang metadata on at the boundary.** `params` is a
  count; storage cells are `(StorageId, LaneId, addr)` triples, not `Node`-
  wrapped statements. This is the same situation as circuit input sides in
  `volar-weaver`, which are solved with an explicit assignment *side table*
  (`VoleSideAssignments`) rather than IR-node metadata. Regions follow the
  same pattern: a side table carried alongside the circuit, not inside it.
- **Generated rkyv types are off-limits for hand edits.** `boolar/generated.rs`
  is produced by `volar-ir-schema-gen`; adding fields there means a schema
  change for every consumer. A companion table avoids that entirely.

Precedents this plan reuses: opaque numeric ids (`StorageId`, `SideId` =
"never invented, always explicitly supplied"), hand-written metadata modules
in `volar-ir` (`public.rs`'s `PublicSet`), and table-driven attachment at
introduction points (`WaffleImportConfig`, `VoleSideAssignments`).

---

## Part 1 — Input/output wire regions

### Types (new module `crates/ir/volar-ir/src/region.rs`, no_std, hand-written)

```rust
/// Opaque region identifier. Never synthesized; supplied explicitly by the
/// frontend/consumer, exactly like `SideId`/`StorageId`.
pub struct RegionId(pub u32);

/// One anchored set of wires. A wire may belong to several regions, so the
/// payload is a set, not a single id — matching the request "given a *set*
/// of regions".
pub struct RegionEntry {
    pub anchor: WireAnchor,
    pub regions: BTreeSet<RegionId>,
}

/// Where a region-annotated group of wires is anchored on the boundary.
/// `bit_range` selects a sub-range of the anchor's wires: `[start, start+len)`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum WireAnchor {
    /// Function input: bit range within the `params: u32` bit sequence.
    Input { bit_range: (u32, u32) },
    /// Function output: bit range within the flattened output wire sequence.
    Output { bit_range: (u32, u32) },
    /// Storage cells: a contiguous flat-address range within one
    /// `(StorageId, LaneId)` space. Covers storage values, stack/temp slots
    /// (`StorageId::STACK`), and state slots alike — they differ only in
    /// which `StorageId`/address range they occupy.
    Storage { storage: StorageId, lane: LaneId, addr_range: (u64, u64) },
}

/// The full region assignment for one circuit.
pub struct RegionTable {
    pub entries: Vec<RegionEntry>,   // sorted, deduplicated by anchor
}
```

Design points:

- **Bit ranges, not per-bit entries.** Regions almost always cover contiguous
  runs (a 128-bit message, a 256-byte stack frame). Interval anchors keep the
  table small and make coverage validation exact. Non-contiguous sets are
  expressed as multiple entries with the same `RegionId`s.
- **Overlapping regions are allowed** (a wire can be both `public` and
  `output`); disjointness is *not* an invariant. What must hold is width
  accounting: an anchor's bit range must fit inside the anchor's actual wire
  count, checked fail-closed against the host circuit.
- **"Temp/state slot" is a storage anchor.** In Boolar, stack spill slots and
  state live in storage spaces already (`StorageId::STACK` etc., 1-bit cells
  with flat addresses), so a range of cells is the uniform way to tag them.
  No separate anchor variant is needed.
- **Why not `Node`-level metadata (next to `side`)?** Sides are per-SSA-value
  and per-statement; regions are per-*boundary-wire-group*, and the boundary
  (param count, storage cells) has no nodes. A node-level field also couldn't
  express storage-cell tagging without touching every `StorageRead/Write`
  site in every pass. The table is cheaper, additive, and optional.

### Validation invariants (`RegionTable::validate`)

1. Every `Input`/`Output` range satisfies `start + len ≤` the host's input /
   output wire count; every `Storage` range fits the `(StorageId, LaneId)`
   cell count implied by the circuit's storage traffic.
2. Entries are sorted and anchor-disjoint *as intervals per anchor kind* —
   two entries may overlap in regions but a single (anchor, wire) position
   must be claimed by at most one entry, so "the set of regions of wire w"
   is a well-defined lookup. (Multiple regions on one wire = one entry whose
   `regions` set has several ids.)
3. Region ids referenced anywhere (table, gadget bindings) are registered in
   a module-level `RegionDecls { names: BTreeMap<RegionId, String> }` —
   names are printer-only; semantics are by id.

### Lookup helper

```rust
impl RegionTable {
    /// Regions of bit `i` of an input; `None` = untagged.
    pub fn input_regions(&self, bit: u32) -> impl Iterator<Item = RegionId>;
    pub fn output_regions(&self, bit: u32) -> impl Iterator<Item = RegionId>;
    /// Regions of one storage cell.
    pub fn cell_regions(&self, storage: StorageId, lane: LaneId, addr: u64)
        -> impl Iterator<Item = RegionId>;
    /// Every wire position matching a selector (see Part 2).
    pub fn select(&self, sel: &RegionSelector) -> Vec<BoundaryWire>;
}
```

```rust
/// Positive + negative membership, enough for "public minus plaintext".
pub struct RegionSelector {
    pub all_of: BTreeSet<RegionId>,      // wire must carry every id here
    pub none_of: BTreeSet<RegionId>,     // wire must carry none of these
}
```

---

## Part 2 — Gadgets

### Concepts

A **gadget** is a named sub-circuit with a declared *port signature*, stored
in a `GadgetLibrary` keyed by name (same resolution model as oracle/action
names: `String` in the IR, resolved by the environment). A **gadget binding**
attaches a gadget to a selector over another circuit's regions.

```rust
/// Port signature of a gadget (a BCircuit plus a declaration of which of its
/// boundary bits are data ports vs. aux/key ports).
pub struct GadgetSpec {
    pub name: String,
    /// Wires-per-port; port i occupies `[offset(i), offset(i)+width(i))` of
    /// the gadget BCircuit's input (resp. output) bit sequence.
    pub ports: Vec<Port>,
}

pub struct Port {
    pub name: String,        // "data_in", "data_out", "key", "iv", ...
    pub kind: PortKind,
    pub width: u64,          // bits
}

pub enum PortKind {
    /// The plaintext-side data the host circuit actually reads/writes.
    /// `width` must equal the bound region's wire count (fail-closed).
    Data,
    /// Aux material supplied from outside (key, IV, tweak). Never itself
    /// gadget-wrapped by the same binding.
    Aux,
    /// Fresh randomness: lowered to `BIrStmt::Rng`/`RngBit` occurrences
    /// inside the spliced gadget body.
    Rng,
}

/// One attachment: "wrap the wires selected by `sel` with gadget `gadget`".
pub struct GadgetBinding {
    pub gadget: String,              // key into GadgetLibrary
    pub selector: RegionSelector,
    /// Source for each `Aux` port, in declaration order.
    pub aux_sources: Vec<AuxSource>,
}

pub enum AuxSource {
    /// An input-region bit range (e.g. the `key` region of the host's params).
    InputRange(WireAnchor),
    /// A constant bit string (spliced as Zero/One statements).
    Const(Vec<bool>),
    /// A named RNG source (lowered to Rng statements).
    Rng(String),
}
```

### Direction semantics

For a host circuit `H` and a binding with encryption gadget `E` (data ports
`plain ↔ cipher`, `E` maps plain→cipher):

| Boundary kind | Core-facing side | Boundary-facing side | Spliced form |
|---|---|---|---|
| Input bit(s) in `sel` | plaintext | ciphertext | `E⁻¹` (decrypt) applied to the incoming param wires; gadget's plaintext outputs become the wires consumed by `H`'s statements |
| Output bit(s) in `sel` | plaintext | ciphertext | `E` applied to `H`'s output wires; gadget's ciphertext outputs become the new circuit outputs |
| Storage cell in `sel` (read) | plaintext | ciphertext stored | `StorageRead` → `E⁻¹` → plaintext var |
| Storage cell in `sel` (write) | plaintext | ciphertext stored | `E` → `StorageWrite` |
| `pre_init` segment in `sel` | plaintext | ciphertext stored | segment data re-emitted through `E` (constant encryption, computed at application time — a free constant-folding win) |

The example from the motivation is then one binding:

```text
gadget = "stream-enc128"
selector = { all_of: {public}, none_of: {plaintext} }
aux = [ Input(key region), Rng("nonce") ]
```

Inputs arriving in ciphertext get decrypted on entry, outputs get encrypted
on exit, tagged storage cells transparently hold ciphertext — and the core
`H`, all its passes, and its evaluator are completely unaware.

### The application pass (`volar-ir-passes/src/apply_gadgets.rs`)

```rust
pub fn apply_gadgets<P: Clone>(
    host: &BCircuit<P>,
    regions: &RegionTable,
    bindings: &[GadgetBinding],
    lib: &GadgetLibrary,
) -> Result<BCircuit<P>, GadgetError>;
```

Mechanics (BCircuit level, single straight-line body — the fused form):

1. **Validate** the table against `host` (Part 1 invariants) and each
   binding: gadget exists; every `Data` port width equals the selected wire
   count (per anchor, per bit — the selector's expansion must tile the data
   port exactly); aux sources exist and have matching widths; **selector
   disjointness across bindings** — two bindings may not claim the same wire
   in v1 (fail-closed; stacking is a follow-up, see Non-goals).
2. **Splice**: for each binding, iterate its selected wires grouped by
   anchor, instantiate the gadget `BCircuit` body (renumbered vars), and
   wire it per the direction table above. Storage anchors expand one gadget
   instantiation per cell address in the range (or per cell-group if the
   gadget declares block width — v1: per cell, simplest and always correct).
3. **Renumber**: params and outputs keep their positions and count. Gadget
   aux inputs coming from `InputRange` stay ordinary params (they are *not*
   transformed); RNG ports become `Rng`/`RngBit` stmts in the host with the
   occurrence counter continuing past the host's own occurrences.
4. **Idempotence guard**: a `GadgetApplied` marker (name list) is carried in
   the returned circuit's companion metadata so double application is a
   detectable error, not silent double-encryption.

### Pipeline placement

```
 Volar IR → movfuscate → Boolar IR → opt passes (DCE/CSE/fold/store-forward)
     → lower_to_circuit / fuse_to_circuit
     → to_reversible (optional)
     → **apply_gadgets**          ← last lowering step, right before consumers
     → weaving / garbling / rkyv fixtures
```

Rationale:

- **After the opt passes.** Gadget boundaries must act as *barriers*:
  store-forwarding must never forward a plaintext load across a
  decrypt splice, and CSE must never merge through `E`/`E⁻¹`. Running the
  passes first, then splicing, sidesteps every such hazard instead of
  teaching five passes about barriers.
- **After `to_reversible` (when used).** Hardened `to_reversible` requires a
  pure wire-function contract and rejects stateful storage; encryption
  splices — especially storage wrapping — must not be in scope for it. The
  reversible form is produced from the plaintext-core contract, then the
  ciphertext boundary is wrapped on top.
- **Consumers see one flat circuit.** The output of `apply_gadgets` is an
  ordinary `BCircuit`: evaluable by `volar_fuzz::interpreter::biir`,
  serializable with rkyv, weavable — no new statement kinds, no evaluator
  changes.

### Interaction with side tagging (note, not a dependency)

Regions and `SideId` are orthogonal and neither derives from the other:
a side answers "who owns this value", a region answers "which named group of
boundary wires is this". In practice, a `SideHandler`-based protection
decision can be *informed by* regions (e.g. "region `public` ⇒
`FheProtection::Encrypted`"), and a follow-up may add a bridge that derives
side assignments from region tables at weaving time. v1 keeps them separate.

---

## Serialization and text format

- **rkyv / `SavedLirModule`-style fixtures:** `RegionTable`, `GadgetSpec`,
  and bindings are plain no_std types in `volar-ir`. For v1 they are
  serialized as a *companion* structure (circuit, regions, bindings) rather
  than new fields on `BCircuit`, keeping `generated.rs` and
  `volar-ir-schema-gen` untouched. If a schema change is later warranted,
  it goes through schema-gen — never hand edits.
- **Text format (`volar-ir-text`):** extend the Boolar printer/parser with
  two side-table sections printed after the circuit body:

  ```text
  regions {
    input  [0, 128)  -> {plaintext, public}
    input  [128, 32) -> {key}
    storage S1 L0 [0, 256) -> {public}
  }
  gadgets {
    stream-enc128 on {all_of: public, none_of: plaintext}
      aux key = input [128, 32)
      aux iv  = rng "nonce"
  }
  ```

  Round-trip property: print → parse → identical table.

---

## Testing

Per Design Rule 3, the correctness signal is **evaluate-before/evaluate-after
equivalence**, not IR shape.

1. **Unit (structural, justified as specific invariants):**
   - `RegionTable::validate` rejects out-of-range anchors, duplicate claims
     of one wire, and unregistered region ids;
   - splice on a toy circuit: input decrypt / output encrypt / storage
     read-decrypt + write-encrypt / `pre_init` re-encryption each verified
     by evaluation against a hand-computed expectation;
   - width mismatch between data port and selected region → `GadgetError`;
   - overlapping selectors across two bindings → `GadgetError`;
   - aux `InputRange` wires pass through untransformed (the `key` region is
     not encrypted by its own binding — the motivating example's exact trap);
   - double application detected via the applied-gadget marker.
2. **Property G — gadget round-trip preserves semantics** (new, in
   `crates/fuzz/volar-fuzz`, alongside Properties A–E): for randomly
   generated circuits + region tables + one self-inverse gadget binding
   (XOR-pad: `E = E⁻¹ = XOR with RNG key`),
   `eval(apply_gadgets(C, regions, bindings))` fed ciphertext-boundary
   inputs equals `eval(C)` fed the corresponding plaintext inputs, and the
   ciphertext outputs unapply to `C`'s outputs. Generation lives in
   `generators/biir.rs` (regions: random interval tables; gadget: fixed
   XOR-pad library entry), following the existing generator style.
3. **Text + rkyv round-trips** for the companion metadata.
4. **Existing suites untouched:** Properties A–E and all optimizer tests
   pass unmodified — the pass adds a post-pipeline step, nothing upstream
   changes.

## Phases

### Phase 1 — region types (`volar-ir/src/region.rs`)
- [ ] `RegionId`, `WireAnchor`, `RegionEntry`, `RegionTable`,
      `RegionSelector`, `RegionDecls` (no_std, `alloc` only).
- [ ] `validate` + lookup/select helpers + unit tests.

### Phase 2 — gadget types + library shape
- [ ] `GadgetSpec`, `Port`/`PortKind`, `GadgetBinding`, `AuxSource`,
      `GadgetLibrary`, `GadgetError`.
- [ ] Binding/table consistency checking (no circuit rewriting yet).

### Phase 3 — `apply_gadgets` pass (`volar-ir-passes/src/apply_gadgets.rs`)
- [ ] Splice for Input/Output anchors (decrypt-on-entry, encrypt-on-exit).
- [ ] Storage anchors: read/write wrapping + `pre_init` re-encryption.
- [ ] RNG-port lowering with occurrence continuity; applied-marker guard.
- [ ] Unit tests from §Testing.1.

### Phase 4 — fuzz + format
- [ ] Property G + generator support (`generators/biir.rs`).
- [ ] `volar-ir-text` printer/parser sections + round-trip tests.
- [ ] rkyv round-trip for the companion metadata.

### Phase 5 — docs
- [ ] `docs/pipeline.md`: add `apply_gadgets` to the diagram + related-docs
      row; mark this file's Status as Landed.
- [ ] `docs/fuzzing.md`: document Property G.
- [ ] `docs/text-format-spec.md`: the two new sections.

## Non-goals (explicit follow-ups, not v1)

- **Stacked bindings on one wire** (encrypt-then-MAC). The selector model
  already supports overlap detection; ordering/associativity rules are
  deferred until a concrete consumer needs them. v1 rejects overlap
  fail-closed.
- **Typed gadget ports, gadgets at the typed `VCircuit` level, gadgets on
  field-level `IRBlocks`/VAFFLE, and region-table threading through the
  passes between these levels** — now planned in
  [`typed-gadgets-and-region-threading-plan.md`](typed-gadgets-and-region-threading-plan.md).
  Bit-level splicing subsumes typed splicing; a typed layer can be added
  later by lowering `VCircuit → BCircuit` first.
- **Regions/gadgets on field-level `IRBlocks`/VAFFLE.** Boundary structure
  there is richer (typed params, `Dyn` calls); revisit after the circuit
  level is proven.
- **Gadget-aware optimization** (e.g. constant-propagating known plaintexts
  through `E⁻¹`). The pre_init constant-encryption in Phase 3 covers the
  only case that matters for size today.
- **Side↔region bridge** (`SideHandler` informed by regions) and garbler-
  side gadget tables (garbled-circuit label handling) — `volar`-repo work.
