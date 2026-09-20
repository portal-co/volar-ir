# Plan: virtualization fixtures and read-only storage

**Status:** in progress. The compatibility sidecar, virtualization producer,
structural validation, static IR/Boolar read folding, storage-to-MUX routing,
and the opt-in M6502/Z80 fixture harness are landed. Caller-sidecar
propagation through virtualization is landed. Broader transform propagation,
protocol selection, and the resource-gated movfuscation fixture chain remain.

**Goal:** make storage access explicit enough that a producer can prove a
storage namespace is read-only, then use virtualized bytecode as the first
such producer. Establish the retro-CPU Boolar fixtures as an opt-in,
large-program regression/measurement harness for virtualization. This is a
compiler-IR optimization and representation plan; it does not prescribe a
particular cryptographic protocol.

**Scope:** `volar-ir-common`, `volar-ir`, `vaffle`, `volar-ir-virt`,
`volar-ir-passes`, `volar-ir-opt`, `volar-ir-text`, the concrete evaluators,
and their callers. The fixture source remains in the sibling `retrop`
repository; generated circuit blobs are not copied into this repository.

## Why this is needed

Today a `StorageId` is only a flat storage namespace. `PreInitSegment` establishes
initial values, but says nothing about whether a program can later write that
namespace. A storage read is therefore state-dependent to every generic pass
and every consumer, even when the storage is actually a static table. There is
currently **no read-only/read-write storage distinction** in Volar IR, Boolar,
or VAFFLE.

That loses several sound optimizations and target choices:

- storage reads cannot generally be commoned or folded beyond local
  store-to-load forwarding;
- an initialized static table cannot be recognized as a ROM/constant source;
- consumers cannot choose a cheaper read-only-storage protocol, rather than a
  read-write protocol, from an IR guarantee; and
- virtualization emits bytecode entirely through `pre_init`, yet its bytecode
  and handler-slot storages look indistinguishable from mutable guest memory.

The large `retrop-emit-volar` fixtures are a useful real-world harness. They
are deliberately serialized Boolar circuits rather than generated native Rust:
currently `m6502_step.biir` is about 6.6 MiB and `z80_step.biir` about 47 MiB.
They exercise a large gate graph with bit-granular RAM/port traffic without
requiring native code generation or compilation just to test an IR transform.

## Facts and constraints discovered before implementation

1. Volar storage cells are keyed by `(StorageId, TypeId, address)`; Boolar
   cells by `((StorageId, LaneId), address)`. Mutability is a property of the
   whole `StorageId`, not an individual type or Boolar lane. A read-only
   declaration consequently covers every typed/lane namespace under that ID.
2. `pre_init` is an initializer, **not** a mutability declaration. WASM memory
   and stack-like stores may be initialized and subsequently written.
3. The retro fixtures are one-block Boolar circuits. Virtualizing one block is
   still useful for deserialization, representation, and semantic-regression
   coverage, but it cannot demonstrate handler deduplication: it has exactly
   one source block and therefore one handler. Metrics must report this rather
   than claim a size win.
4. `virtualize_bir` returns a public-dispatch multi-block program. A native
   consumer that requires `BIrBlocks::is_circuit()` cannot ingest that
   intermediate directly. The fixture harness tests the transform and, only
   where resources permit, the subsequent movfuscation back to a single-block
   circuit; it does not compile the native target.
5. Adding fields to the generated IR module structs changes the rkyv layout.
   rkyv blobs are intentionally opaque and unversioned here, so fixtures must
   be regenerated from source at the same revision. This is particularly
   relevant to the retrop blobs; do not promise old blobs remain decodable.

## Design decisions

### A module-owned storage declaration table

Add shared sidecar types in `volar-ir-common`:

```rust
pub enum StorageAccess {
    ReadOnly,
    ReadWrite,
}

pub struct StorageDecl {
    pub storage: StorageId,
    pub access: StorageAccess,
}

pub struct StorageTable {
    // Canonical, unique by StorageId; lookup defaults to ReadWrite.
    pub entries: Vec<StorageDecl>,
}
```

The exact container may be a canonical vector rather than a map so it remains
portable and schema-friendly. It must expose at least:

- `access_of(StorageId) -> StorageAccess`, returning `ReadWrite` when absent;
- insertion/merge APIs which reject duplicate/conflicting declarations;
- a validator for canonical ordering and uniqueness; and
- explicit storage-ID remapping/retention helpers used by transforms.

An absent declaration is deliberately conservative: existing hand-built IR,
old source constructors, and unknown imported storage all remain read-write.
Only an explicit `ReadOnly` declaration establishes the optimization and
protocol-selection guarantee. An explicit `ReadWrite` entry is permitted for
explanatory producer metadata but has the same semantics as the default.

`StorageTable` is a compatibility sidecar owned by a producing transform or
consumer API rather than a field in persisted module/circuit carriers. The
first carrier is `VirtOutput`; consumers that preserve the fact across their
own APIs must carry that same sidecar explicitly. This keeps existing
rkyv/text formats and old blobs compatible. Do not put it on individual
storage statements, `TypeId`s, or `LaneId`s.

### Meaning of read-only storage

`ReadOnly(S)` means this IR program and all declared action targets will never
perform an IR-visible write to *any* cell whose storage ID is `S`. It does not
mean the values are public, small, bounded, authenticated, or free to read.
Those are independent consumer/protocol decisions.

The initial contents remain the module's `pre_init` image plus the standard
zero value for cells absent from that image. A static read can become a
constant only when the optimizer can resolve its typed/lane address and image
value. A symbolic address is not silently treated as constant; it can be
considered a ROM/table only by a consumer that has an explicit finite-domain
and cost model.

This distinction deliberately makes three progressively stronger facts
available:

| Fact | Enables |
|---|---|
| `ReadOnly` | reads may be treated as non-mutating state dependencies; read-only-storage protocol selection |
| `ReadOnly` + a resolved address + `pre_init`/zero image | replacement of that read with a constant |
| `ReadOnly` + a consumer-proved finite address domain | ROM/mux/table lowering, CSE, or a specialized read-only protocol |

### Validation is semantic, metadata is not decorative

Provide a fallible `validate_storage_access` walk for each representation.
It must reject a read-only declaration targeted by:

- `Stmt::StorageWrite` in VAFFLE or Volar IR;
- `BIrStmt::StorageWrite` in Boolar;
- `Stmt::ActionStore` and every declared `ActionTarget`;
- `BIrStmt::ActionStoreBit`; and
- any later storage-mutating circuit representation, including reversible
  storage-swap lowering, before that representation claims the same table.

Pure reads are allowed. The validator must use catch-all match arms as the IR
families evolve, and must return a useful error identifying the storage ID and
operation rather than panic.

Run validation at all public transform/lowering boundaries and after text/rkyv
loading APIs expose a module. rkyv itself has no semantic validation hook, so
callers must not gain an optimization merely by deserializing unchecked bytes.
All transforms must preserve declarations when IDs are preserved and remap
them when IDs are remapped. A transform which merges two IDs may merge their
facts only if the result is no more permissive (`ReadOnly` only when every
merged source is read-only); otherwise it must emit `ReadWrite` or reject the
merge.

## Implementation phases

### Phase 0 — establish fixture and mutability baselines

1. Add an ignored, opt-in `volar-ir-virt` integration harness that obtains the
   retrop fixture directory from a required environment variable, for example
   `VOLAR_IR_RETROP_FIXTURE_DIR`. It must not guess a sibling checkout path,
   download artifacts, or vendor the blobs.
2. Load the files into an aligned rkyv buffer exactly as retrop's own
   `tests/equiv.rs` does. Report fixture byte size, blocks, params,
   statements, pre-init segments, storage IDs/lanes, virtualization output
   blocks, and handler count.
3. Record a checked-in Markdown/JSON baseline only for stable structural
   metrics. Put elapsed time and peak RSS in the manual test output rather
   than asserting machine-dependent limits.
4. Test both zero-initialized storage and a small deterministic set of
   synthetic bit-granular RAM/port pre-init overlays. Compare the original
   fixture with its Public-dispatch virtualized form using `eval_biir` and a
   bounded deterministic input sample. Keep the full Z80 semantic run
   manually invoked/ignored until its resource profile is known.
5. Add a separate, small multi-block fixture for handler-dedup assertions.
   It is the test that must assert `n_handlers < blocks_in`; do not infer that
   assertion from a one-block retro CPU circuit.

Initial manual command shape (the exact test name may change):

```sh
VOLAR_IR_RETROP_FIXTURE_DIR="$(cd ../retrop/crates/retrop-emit-volar/fixtures && pwd)" \
  cargo test -p volar-ir-virt --test retrop_fixtures -- --ignored --nocapture
```

This phase is successful when the harness makes fixture provenance and cost
observable, passes semantic equivalence for the selected samples, and clearly
reports the expected one-handler result.

### Phase 1 — add and propagate the storage table

1. Add `StorageAccess`, `StorageDecl`, and `StorageTable` as ordinary shared
   Rust types, outside generated persisted schemas. The table starts empty and
   treats absent IDs as read-write.
2. Add explicit sidecar-bearing result/view types at APIs that can prove the
   fact. Start with virtualization; later frontends and consumers carry a
   `StorageTable` alongside the legacy IR value instead of changing that
   value's serialized layout.
3. Thread sidecars deliberately through VAFFLE → Volar IR → Boolar only where
   the caller needs the fact. ID remappers use a common table helper rather
   than rebuilding ad hoc maps.
4. Existing retrop rkyv fixtures stay decodable unchanged; run retrop's own
   golden-vector equivalence suite only when changing its generator, not for
   this sidecar addition.
5. Update `docs/agent-context/ir-types-storage.md` with the access contract,
   default, image semantics, and validation rule. Keep it separate from the
   existing typed-slot aliasing/invalidation rule, which does not change.

Success criterion: a read-only sidecar survives its owning transform API,
while every undeclared legacy storage behaves exactly as read-write storage did
before this work.

### Phase 2 — enforce declared mutability

1. Implement the validators described above and invoke them at every public
   producer/consumer boundary before an optimization relies on `ReadOnly`.
2. Teach all generic statement walkers and map/as-ref/as-mut implementations
   about the table-preserving module fields. Add explicit tests for storage
   used only by `pre_init`, used only by action targets, and used under
   multiple `TypeId`/`LaneId` spaces.
3. Verify remapping behavior in `movfuscate`, `substitute_ir`,
   `substitute_vaffle`, storage-to-mux lowering, and circuit/reversible
   lowering. In particular, a source read-only storage must not accidentally
   gain a write-capable split lane; any generated writable scratch/register
   lane must be declared `ReadWrite`.
4. Reject invalid direct constructions in unit tests, including a read-only
   storage written only on a dead branch. The property is structural, not
   reachability-dependent: a module claiming read-only must contain no write
   operation to that namespace.

Success criterion: no public optimizer or backend can receive a validated
module that declares `ReadOnly(S)` yet contains an IR-visible write to `S`.

### Phase 3 — make virtualization the first producer

**Status: landed.** Virtualization now returns a validated sidecar covering
its complete generated table layout. Caller declarations are carried into the
result and merged conservatively: any conflicting read-write fact weakens the
result to read-write, while undeclared caller storage remains read-write by
default.

Virtualization already creates its bytecode and handler-slot contents with
`pre_init`; classify those storage IDs explicitly instead of relying on that
fact implicitly.

1. Have IR `GlobalLayout` and Boolar `BirSlotLayout` return the exact complete
   set of bytecode/handler-slot storage IDs they allocate. Mark every one
   `ReadOnly` in the virtualized output's table.
2. Preserve and merge the input module's declarations, rejecting collisions
   rather than silently overwriting facts. The virtual storage allocator must
   allocate facts together with IDs; do not classify only
   `cfg.bytecode_storage` and omit its derived slot range.
3. Mark typed register files `ReadWrite`: setup and handler arms write them.
   Mark keyed-commitment `key_storage` `ReadWrite`, because setup writes the
   key parameters. Mark `commitment_storage` `ReadOnly`, because the current
   implementation seeds its per-handler values in `pre_init` and handlers only
   read them.
4. Correct `docs/virt.md` and the `virtualize_ir_committed` API documentation
   while landing this: they currently say setup writes every commitment hash,
   but the implementation writes only optional keys in setup and initializes
   commitment hashes through `pre_init`.
5. Add IR and Boolar tests which collect every output `StorageWrite`/action
   target and prove it is disjoint from the virtualization-produced read-only
   set. Run the generic validator over each output, including direct dispatch,
   commitments, and adaptive-split configurations which are currently
   supported.

Success criterion: `virtualize_ir` and `virtualize_bir` are the first public
APIs that return modules with nonempty, validated `ReadOnly` storage facts;
all virtual bytecode reads retain those facts through the next lowering.

### Phase 4 — exploit the fact conservatively

**Status: substantially landed.** Typed IR and Boolar constant folding,
consumer route queries, and sidecar-aware bounded storage-to-MUX entry points
are implemented and tested. The remaining end-to-end work is to thread these
APIs through more callers and add the resource-gated virtual-bytecode chain.

1. Add a read-only storage folding pass (or a clearly named extension of the
   existing folding pass), initially limited to a read whose address resolves
   to a concrete value. Look up the typed `(StorageId, TypeId, address)` or
   Boolar `((StorageId, LaneId), address)` image in `pre_init`, falling back
   to zero, and replace the read with the correctly typed constant.
2. Keep the existing store-forwarding invalidation policy unchanged for
   read-write storage. For read-only storage there are no writes to invalidate
   a cache, but do not duplicate expensive expressions: reuse normal CSE and
   use-count rules rather than substituting a read's full producer at every
   use.
3. Give consumers a small `StorageAccessView`/table query API so they can
   deliberately select a ROM/read-only-storage implementation or a different
   protocol for `ReadOnly`. The API exposes a proof of immutability only; the
   consumer remains responsible for visibility, address-domain bounds,
   authentication, and cost/security analysis.
4. Extend bounded `storage_to_mux_ir`/Boolar lowering to accept a read-only
   image when its existing finite-address preconditions hold. It may build a
   mux/ROM table there; it must reject or leave symbolic/unbounded accesses
   untouched rather than materializing an unbounded table.
5. Use virtual bytecode as the first end-to-end consumer proof: after
   virtualizing a small multi-block module, fold a constant bytecode read and
   show the result is semantically equal before/after. Add a separate test
   proving a read-write pre-initialized storage does not receive this fold.

Success criterion: the optimization removes only reads justified by both a
validated `ReadOnly` declaration and a resolvable static image, and consumer
selection tests demonstrate that read-write and read-only routes remain
observably distinct.

### Phase 5 — scale and regression-test

1. Re-run the retrop fixture harness after Phase 3. It should verify that
   unrelated guest RAM/port storages remain read-write by default, while only
   virtualization-generated table spaces are read-only.
2. Add a manual transform chain for the M6502 fixture:

   ```text
   deserialize → virtualize_bir(Public) → validate facts
               → [resource-gated] movfuscate_biir → validate facts/evaluate
   ```

   The test compares output bits for the same sampled inputs and storage image
   at each evaluable boundary. It must report, not hide, any phase whose size
   exceeds the declared manual resource envelope.
3. Exercise the Z80 fixture only under the same explicit opt-in gate first.
   Promote structural decode/virtualization checks to normal CI only if their
   measured resource use is suitable; keep whole-fixture semantic and
   movfuscation tests manually scheduled otherwise.
4. Run the normal semantic property suites before and after each pass:
   `volar-fuzz` evaluator equivalence, store-forward/folding properties,
   virtualization properties, text/rkyv round-trips, and retrop golden
   vectors after fixture regeneration.

## Test matrix

| Area | Required evidence |
|---|---|
| Data model | defaults are read-write; duplicate/conflicting entries fail; sidecar APIs preserve declarations without changing schema/text/rkyv layouts |
| Validation | direct writes, action stores/targets, Boolar action-store bits, and reversible mutations to read-only storage fail with actionable errors |
| Semantics | evaluate IR/Boolar before and after metadata-preserving transforms; declarations alone never change output |
| Folding | concrete read-only initialized reads fold; read-write and unresolved-address reads do not |
| Remapping | substitutions, movfuscation lane splitting, and IR→Boolar lane assignment preserve or conservatively weaken facts |
| Virtualization | all allocated bytecode/slot IDs are read-only; register/key IDs are read-write; emitted writes are disjoint from read-only IDs |
| Large fixture | aligned rkyv decode, structural metrics, sampled semantic equality, explicit resource-gated M6502 then Z80 chain |

## Non-goals

- Inferring read-only status from an absence of writes in an arbitrary foreign
  module. Producers must declare the fact; inference can be a separate,
  conservative diagnostic later.
- Changing typed-slot aliasing or store-forwarding invalidation semantics.
- Treating read-only as public, trusted, authenticated, bounded, or cheap.
- Making the one-block retro fixtures a handler-dedup benchmark. They are
  realistic scale tests and native-code-avoidance fixtures; a small dedicated
  multi-block program establishes dedup behavior.
- Introducing a hidden fixed `StorageId` range. Access facts follow actual
  allocated/remapped IDs and must remain compatible with the storage-registry
  work when that branch is integrated.
