# Shared-storage scheduling for VOLE and lockstep MPC

**Status:** implementation in progress; Phases 1–3 landed in volar-ir; Phase 4 adapter seam started
**Scope:** Volar's VOLE proving path and main-branch MPC path across Volar and
Cirrus  
**Related work:** parallel communication reduction (#14160)

**Implementation checkpoints:**

- Phase 1: canonical shared-storage identities, claims, bounded manifests, and
  adapter-selected digest input — landed in `f4cf67d`.
- Phase 2: conservative fixed-circuit storage-claim extraction for typed Volar
  IR and Boolar IR, including read-only policy checks and serial fallback for
  unsupported control flow — landed in the follow-up implementation commit.
- Phase 3: deterministic transaction planning and a `no_std` runtime core with
  atomic claim reservation, buffered writes, actual-access validation, abort-on-
  conflict/failure behavior, and canonical trace fragments — landed in the
  follow-up implementation commit. Host-thread/VOLE/MPC adapters remain later
  phases.
- Phase 4 checkpoint: Cirrus now exposes manifest-bound
  `VoleScheduleTicket`/`VoleScheduleCursor` replay plumbing in `6ca60b5`,
  plus ticket-bound prover hat sinks/verifier hat sources in `99cbe52`,
  deterministic fragment merging in `7dfe1bc`, hardened range validation
  in `071369c`, rejection-path coverage in `673dae4`, manifest-checked
  fragment merging in `84884f6`, bounded explicit-limit consumers in
  Cirrus `e2d08c5`, fail-closed incomplete and over-consumed handling in
  `462fda4`, next-ticket cursor access in `9d3bda3`, and a toy
  role-separated StackIo schedule exercise in Cirrus
  `fc6754a`, using a canonical SHA3 schedule digest in `56724db`;
  existing ideal-functionality contexts remain
  unchanged until the real role-separated VOLE path is ready.

This document specifies a deterministic scheduler for executions in which
multiple host threads act as virtual cores over a set of shared typed storages.
It is deliberately a runtime/scheduling design, not a cryptographic claim.
The first implementation must preserve the existing sequential semantics and
must fail closed when an access cannot be represented in the public schedule.

The same schedule artifact is used by two different execution modes:

- **VOLE:** the prover may execute independent iterations concurrently. The
  verifier does not need to reproduce the host thread interleaving; it checks
  the canonical schedule and replays the resulting storage/proof events in
  schedule order.
- **Main-branch MPC:** both parties execute the same public schedule in
  lockstep. Host threads may prepare independent work, but neither role may
  advance the protocol or expose a storage result out of schedule order.

The central rule is that an OS mutex acquisition order is never the execution
order. The compiler/runtime extracts a canonical order first; locks, worker
lanes, communication packing, traces, and verifier replay all refer to that
order.

---

## 1. Goals and non-goals

### 1.1 Goals

1. Share storage banks safely between host workers without serializing every
   access behind the current whole-bank mutex.
2. Let one logical iteration acquire only the storage namespaces it needs.
3. Treat a small group of iterations as a **consistency epoch**: all members
   use the epoch's declared snapshot/lock contract, then writes become visible
   at the epoch barrier in canonical order.
4. Make the order extractable before execution, or produce a bounded per-run
   manifest when a loop requires runtime iteration material.
5. Permit VOLE prover-side parallelism without making hats, correlations,
   memory timestamps, or fold state depend on OS scheduling.
6. Keep main-branch MPC synchronous: both parties consume identical batch and
   iteration boundaries and reject mismatched manifests or frame ordinals.
7. Provide a natural input to #14160's communication reduction: independent
   operations can be packed into deterministic batches without changing the
   logical transcript.
8. Preserve a serial reference implementation and make every parallel result
   differentially testable against it.

### 1.2 Non-goals

- This does not make a semi-honest MPC implementation malicious-secure. The
  current `volar-mpc` documentation describes a semi-honest session layer, and
  Cirrus's current VOLE context has explicit ideal/simulation caveats.
- This does not hide a schedule that is intentionally public. The manifest,
  batch sizes, lock classes, and communication shape are public protocol
  metadata. Secret addresses and secret values must not be converted into
  public scheduling decisions.
- This does not permit arbitrary data races and call them eventual
  consistency. A read/write or write/write conflict must be ordered, excluded
  from a common epoch, or cause an abort.
- This does not use arrival order, `Mutex` fairness, an atomic counter, or
  thread timing as a replayable proof fact.
- This does not universally hoist cross-region values into one shared prefix.
  The interpreter-scale investigation found that doing so duplicates work and
  can create a catastrophic footprint. Cross-region values must remain local
  or be threaded only between their producer, intervening, and consumer
  chunks.
- This does not replace oblivious RAM, the existing VOLE memory-checking
  relation, or the MPC GRAM/ORAM semantics. It schedules those mechanisms.

---

## 2. Existing seams and constraints

The plan is intentionally grounded in the current three-repository layout.
Paths below are relative to this repository unless stated otherwise.

### 2.1 IR and storage identity

The IR context defines a typed storage cell as
`(StorageId, TypeId, address)` and distinguishes a storage namespace from an
individual cell. Boolar uses the analogous `(StorageId, LaneId, address)`
identity. The scheduler must retain this distinction:

- the **lock key** is normally a typed storage namespace, not a secret address;
- the **trace event** contains the full typed-cell identity and canonical
  operation ordinal;
- an address may refine a lock only when its value/range is public and the
  compiler has proved that the refinement is safe;
- `StorageTable::ReadOnly` is an explicit assertion. An absent storage-table
  entry is conservatively `ReadWrite`, as required by the IR storage policy.

Relevant local definitions are in [`CONTEXT.md`](../CONTEXT.md),
`volar-ir`'s storage sidecar, and the storage analyses in the Volar worktree.

### 2.2 VOLE weaver and memory trace

The existing VOLE weaver supports `StorageMode::Tree` and
`StorageMode::Commitment` in
[`volar/docs/vole-weaving.md`](../../volar/docs/vole-weaving.md). In commitment
mode, `volar-weaver/src/vole.rs` returns a `MemoryTrace` whose entries contain
`storage_id`, `type_id`, variable identities, read/write kind, and a
`timestamp`. The timestamp currently follows circuit order, which is the right
semantic baseline but is not yet a cross-thread scheduling contract.

The external memory-checking design in
[`volar/docs/memory-checking.md`](../../volar/docs/memory-checking.md) requires
canonical produce/consume accounting. Parallel execution must therefore merge
per-worker trace fragments by manifest ordinal before building the memory proof;
merging by completion time would change the meaning of timestamps.

The split VOLE weaver already has per-block functions, chunked accumulator
functions, explicit `all_ok`/`fold_state` threading, and array-batched hats.
The scheduler should align with those boundaries rather than introduce a
second, conflicting chunk model. The `all_ok`/`fold_state` chain remains
logically ordered even if independent block work is prepared in parallel.

### 2.3 Current Cirrus storage wrappers

`cirrus-volar-vole/src/locked.rs` currently exposes
`LockedVoleProverStorage` and `LockedVoleVerifierStorage`. Each wraps the
entire `Vole{Prover,Verifier}Storage` in one `spin::Mutex`; this is a useful
correctness baseline, but it serializes unrelated storage namespaces and makes
mutex arrival order observable to the host.

The replacement must not simply add more mutexes around individual methods.
A logical iteration needs a transaction/lock token that covers its declared
accesses, read observations, local writes, trace entries, and commit. Otherwise
one iteration can observe another iteration halfway through its update and the
trace still has no deterministic order.

### 2.4 Main-branch MPC

`volar-mpc` is `no_std + alloc` by default and exposes a role-separated
`GateSchedule`, garbled execution, strict cursors, and GRAM storage hooks. The
schedule already has concrete storage operation ordinals for GRAM accesses.
The scheduler metadata must remain protocol-neutral enough for this crate and
must not introduce a mandatory `std` thread runtime into the core session API.

Host-side threaded execution can live in an adapter. The core API receives a
public manifest and a ticket/ordinal; it does not infer order from a host
thread. Both roles validate the same manifest and frame sequence.

### 2.5 Related Volar plans

This plan composes with, but does not supersede:

- [`volar/docs/vole-weaving.md`](../../volar/docs/vole-weaving.md) — VOLE
  weaver API and gate/hats ordering;
- [`volar/docs/memory-checking.md`](../../volar/docs/memory-checking.md) —
  external storage consistency relation;
- [`volar/docs/vole-continuation-bridge.md`](../../volar/docs/vole-continuation-bridge.md)
  — continuation boundaries and state commitment;
- [`volar/docs/mpc-external-actions-oracles-plan.md`](../../volar/docs/mpc-external-actions-oracles-plan.md)
  — dependency-aware external batches and explicit action ordering;
- [`volar/docs/interpreter-honest-e2e-zk-plan.md`](../../volar/docs/interpreter-honest-e2e-zk-plan.md)
  — split weaving, cross-region locality, and packed-parameter threading;
- [`volar/docs/secret-shared-volar-mpc-plan.md`](../../volar/docs/secret-shared-volar-mpc-plan.md)
  — a separate future secret-shared storage topology. That plan must not be
  treated as evidence that this lockstep/VOLE scheduler is implemented.

---

## 3. Semantic model

### 3.1 Virtual cores and logical iterations

A **virtual core** is a public worker lane in the schedule, not a promise that
one particular OS thread will run it. An **iteration** is the smallest unit
that may acquire storage locks and produce a trace/protocol fragment. Depending
on the backend, an iteration may be:

- one movfuscated circuit step;
- one original IR block or split-weave region;
- one bounded group of gate operations in a `GateSchedule`; or
- one explicit storage transaction around a GRAM/VOLE access sequence.

The compiler chooses the iteration boundary. It must not be inferred after
threads start running.

### 3.2 Consistency epochs (small batches)

A **consistency epoch** is a bounded, ordered batch of iterations. It has:

1. a public epoch ordinal;
2. a fixed list of iteration ordinals;
3. a declared lock/access summary for every iteration;
4. an optional public worker-lane assignment;
5. a communication group and resource cap; and
6. a commit barrier.

Within an epoch:

- read-only iterations may run concurrently;
- iterations touching disjoint lock keys may run concurrently;
- a write/write or read/write conflict is never placed in the same epoch unless
  an explicitly proven public address partition makes the accesses disjoint;
- each iteration sees a stable epoch snapshot plus its own buffered writes;
- writes are committed in manifest order, not completion order;
- no later epoch starts until all commits and protocol acknowledgements for
  the current epoch have completed.

This is the intended meaning of "storage only needs to be consistent within
small batches": a batch has a well-defined snapshot and a well-defined merge,
not an unconstrained race. The initial implementation should use whole typed
storage namespaces as the conflict unit. Public address/range sharding is an
optimization after the conservative implementation is correct.

### 3.3 Lock modes

For each typed storage key, an iteration claims one of:

| Claim | Meaning | Same-epoch compatibility |
|---|---|---|
| `Read` | no IR-visible write to this key | compatible with other `Read` claims |
| `Write` | iteration may write, or needs read/write atomicity | exclusive |
| `Unknown` | analysis could not prove the access set | treated as `Write` over the whole key |

A `ReadOnly` storage declaration may reject a `Write` claim as a compiler
error. An absent declaration is `ReadWrite`, so even a statically read-looking
operation does not receive unsafe read-only treatment without policy metadata.

A single iteration that both reads and writes a key claims `Write` for the
whole iteration. This prevents a second iteration from observing an
intermediate value. A future implementation may add public address partitions,
but the partition proof and its version must be in the manifest.

### 3.4 Sequential reference semantics

The reference semantics are the original program order:

```text
for iteration in canonical_iteration_order:
    execute iteration against the current storage image
    commit its writes immediately
```

The parallel scheduler is correct only if its epoch execution is equivalent to
this reference. For a valid epoch, disjoint operations commute and the
canonical commit order handles any remaining write order. Every adapter gets a
serial mode, and all new tests compare parallel mode with serial mode on the
same initial storage image and witness inputs.

---

## 4. Extractable schedule artifact

### 4.1 Schedule manifest

Introduce a versioned, canonical `ScheduleManifest` in a protocol-neutral
layer. The exact crate location is a phase-1 decision, but the preferred split
is:

- identity, access summaries, canonical encoding, and validation in a small
  `volar-ir-common`/shared scheduling module usable by `no_std + alloc`;
- IR extraction in `volar-ir-passes`/`volar-weaver`;
- protocol adapters in Cirrus and `volar-mpc`.

The manifest is conceptually:

```text
ScheduleManifest {
    format_version,
    program_digest,
    storage_policy_digest,
    protocol_profile,
    epoch_size,
    worker_lane_count,
    epochs: [EpochManifest],
}

EpochManifest {
    epoch_ordinal,
    iterations: [IterationManifest],
    communication_group,
    resource_caps,
}

IterationManifest {
    iteration_ordinal,
    logical_step_id,
    worker_lane,
    dependency_ordinals,
    lock_claims: [StorageClaim],
    expected_access_classes,
    correlation_range,
    communication_ordinal_range,
}

StorageClaim {
    storage_id,
    type_or_lane_id,
    mode,                 // Read, Write, Unknown
    address_partition,    // Whole by default; public proven refinement only
}
```

All arrays are sorted in canonical order before encoding. Duplicate claims are
coalesced. The manifest digest covers the format version, program/circuit
identity, storage policy, protocol profile, epoch boundaries, lock claims,
correlation ranges, and communication grouping. It is bound to the session
transcript before any worker begins.

`worker_lane` is a deterministic placement hint and a useful replay/debug
field. Correctness must not depend on the physical thread that happens to run
the lane. An implementation may derive it from ordinal modulo lane count, or
use a deterministic greedy assignment; both roles must use the same rule.

### 4.2 Static and per-run extraction

There are three accepted extraction modes:

1. **Static manifest.** For fixed-shape circuits and movfuscated steps, the
   compiler emits the complete manifest. This is the preferred VOLE and GRAM
   path.
2. **Bounded per-run manifest.** A runtime loop emits a public iteration
   descriptor from a fixed public loop bound. Secret data may choose values,
   but it may not choose a different public lock or communication schedule.
3. **Serial fallback.** If the access set or order cannot be conservatively
   extracted without a secret-dependent scheduling decision, run that region
   as one-iteration epochs. Do not silently guess a lock set.

A dynamic branch can affect an iteration's values, but it cannot cause one
role to schedule a different set of storage namespaces than the other role.
Movfuscation already provides a useful route for secret control flow: fake
iterations remain in the public circuit schedule and claim the conservative
union of possible storage effects.

### 4.3 Runtime access validation

The manifest is conservative, but it is still checked. Each transaction
records the actual typed storage keys and operation classes it touches.
At commit:

- an actual key absent from the iteration's manifest claim aborts;
- an actual write under a `Read` claim aborts;
- a `ReadOnly` violation aborts;
- an iteration that fails to consume its reserved protocol/correlation range
  aborts or returns unused material through a defined, canonical path;
- duplicate, missing, or out-of-order iteration ordinals abort.

An access address remains a trace/protocol value. It is not a new scheduler key
unless the manifest explicitly contains a public partition proof.

---

## 5. Shared-storage runtime

### 5.1 Storage bank and lock manager

Add a `SharedStorageBank` abstraction with a backend-independent interface:

```text
SharedStorageBank {
    begin_epoch(epoch_manifest) -> EpochHandle
}

EpochHandle {
    begin_iteration(iteration_manifest) -> StorageTxn
    finish_iteration(txn) -> IterationResult
    commit_epoch() -> OrderedEpochResult
}

StorageTxn {
    read(key, address) -> AuthenticatedValue
    write(key, address, value)
    trace_fragment() -> StorageTraceFragment
    commit_or_abort()
}
```

The first host implementation may use one sharded lock table and one
copy-on-write/write-buffer per iteration:

- one lock shard per typed storage key (or a deterministic hash shard);
- `Read` claims take shared access, `Write` claims take exclusive access;
- all keys are acquired in canonical `(StorageId, type/lane)` order;
- the scheduler grants a ticket only after the whole declared lock set is
  available, so a worker does not hold key A while waiting for key B;
- reads come from the epoch snapshot plus the transaction's own write buffer;
- writes are applied at the epoch barrier in iteration-ordinal order;
- a failed or cancelled iteration publishes no partial writes.

The lock manager is an implementation detail. A future backend may replace
host locks with immutable snapshots and version checks, but it must retain the
same manifest and commit semantics. A simple `Arc<Mutex<HashMap<...>>>` per
cell is not an acceptable first design: it makes the unit of consistency too
small, leaks arrival order into traces, and is difficult to audit.

### 5.2 Scheduler algorithm

The planner receives iterations in canonical program order and greedily builds
bounded epochs:

```text
for iteration in program_order:
    claims = conservative_access_summary(iteration)
    predecessors = data_and_storage_predecessors(iteration)
    if adding iteration would:
         exceed an epoch/resource cap, or
         conflict with an existing claim, or
         violate a predecessor/barrier,
       close the current epoch
    add iteration with the next canonical ordinal
assign deterministic worker lanes
canonicalize and hash the manifest
```

The runtime then does:

```text
for epoch in manifest.epochs:
    announce_or_validate(epoch)
    grant tickets in manifest order
    workers execute granted iterations using StorageTxn
    collect results by iteration ordinal
    validate actual accesses and protocol ranges
    commit writes and trace fragments in ordinal order
    barrier / acknowledge epoch
```

The scheduler may grant non-conflicting iterations to different workers before
an earlier iteration finishes, but it must never let completion order define a
later iteration's input. If a dependency was omitted, validation must abort;
it must not repair the manifest by dynamically reordering work.

### 5.3 Storage trace ordering

Every storage event gets a structured logical time, for example:

```text
ExecutionOrdinal {
    epoch_ordinal,
    iteration_ordinal,
    intra_iteration_access,
}
```

The serialized trace order is lexicographic by this ordinal. The existing
`MemoryTraceEntry::timestamp` can remain as a compatibility projection for
small traces, but a versioned trace must avoid truncating a large ordinal into
a `u32` or reusing a host-time counter.

For commitment-mode memory checking, `produce`/`consume` events are merged in
canonical order before the multiset accumulator is finalized. For a tree-mode
storage proof, the same order controls which authenticated state is presented
to each operation. Parallelism must therefore happen above the storage
semantics, not inside an unaudited mutable accumulator.

---

## 6. VOLE execution path

### 6.1 Prover side

The prover is the only path allowed to use host-side parallelism initially.
The required flow is:

1. Weave/extract the public manifest from the movfuscated or split circuit.
2. Reserve deterministic VOLE correlation ranges and output ordinals per
   iteration. A global atomic "next correlation" counter is forbidden because
   worker arrival order is nondeterministic.
3. Start an epoch and give each worker a ticket plus its declared lock set.
4. Execute the block/chunk, accumulating reads, writes, hats, and local proof
   state in worker-owned buffers.
5. Validate the actual storage/protocol footprint against the ticket.
6. Merge hats, `MemoryTrace` fragments, outputs, and any `all_ok` inputs by
   iteration ordinal. Fold `all_ok`/`fold_state` in canonical order (possibly
   with a deterministic chunk tree whose root order is fixed).
7. Commit the storage transaction and close the epoch.

The current `LockedVoleProverStorage` can be retained as the serial adapter
for phase 0. The parallel adapter must not call its per-method `read`/`write`
from unrelated threads and then sort only the final trace: storage values and
VOLE memory accumulators would already have observed the wrong order.

### 6.2 Verifier side

The verifier need not create host threads to validate a prover schedule. It:

- validates the manifest digest and protocol/circuit identity;
- checks the prover's iteration outputs and hats against reserved ordinals;
- replays storage state in manifest order;
- checks memory commitments/permutations with manifest-derived logical times;
- folds `all_ok`/`fold_state` in the same order; and
- rejects missing, duplicated, skipped, or out-of-order fragments.

A verifier may parallelize pure checks in a later phase, but the result must be
reduced in canonical order and the feature must not alter the transcript.

### 6.3 Correlation and continuation boundaries

VOLE correlations, hats, and continuation state are finite protocol resources.
The manifest therefore reserves ranges before execution and includes them in
the schedule digest. A continuation/reconnect boundary from
[`vole-continuation-bridge.md`](../../volar/docs/vole-continuation-bridge.md)
must coincide with an epoch boundary or carry an explicit parent epoch/ordinal
binding. A reconnect may not resume from whichever worker happened to finish
last.

The continuation bridge's boundary state, storage image digest, and schedule
digest should be treated as one checkpoint identity. If a bridge implementation
cannot represent an in-flight epoch, it must finish or discard that epoch
before reconnecting.

### 6.4 Cross-region locality constraint

The split weaver's functions and accumulator chunks are already a natural
scheduler unit. Do not add a universal `shared_prefix` containing every value
used in any region. The implementation should instead:

- identify producer, intervening, and consumer chunks for each synthetic value;
- thread packed values only across that local chain in driver order;
- keep the manifest's data dependencies aligned with those parameters; and
- allow a worker to retain only its local chunk material.

This keeps the scheduling plan compatible with the measured cross-region
footprint constraints and avoids turning parallelism into a source-size
regression.

---

## 7. Main-branch MPC / Cirrus execution path

### 7.1 Lockstep rule

Main-branch MPC has two protocol roles. Both roles receive or derive the same
public `ScheduleManifest`, verify its digest, and advance through:

```text
epoch-open -> iteration/batch material -> epoch-commit -> epoch-ack
```

A host thread may prepare a table, label operation, authenticated storage
read, or local arithmetic result early, but it may not send or consume a
protocol frame with a future logical ordinal. The transport frame carries at
least:

```text
session_id, schedule_digest, epoch_ordinal, iteration_ordinal,
phase, message_ordinal, payload
```

The receiver rejects a wrong digest, role, phase, epoch, ordinal, or payload
width. This is a synchronization and replay check, not a malicious-security
proof by itself.

### 7.2 Gate and GRAM mapping

For the current `volar-mpc::GateSchedule` shape:

- pure gates retain their canonical wire indices;
- `StorageRead` and `StorageWrite` retain their public storage/access ordinals;
- the manifest groups independent gate ranges and storage transactions;
- the evaluator-side GRAM driver, where that adapter owns the physical ORAM,
  still follows the logical transaction/epoch order; the abstraction does not
  assume both processes share an address-space mutex;
- any future multi-party storage driver implements the same
  `SharedStorageBank`/`StorageTxn` contract on each role's authenticated view.

The garbler and evaluator may use different local worker counts, but they must
produce the same logical manifest and communication grouping. A worker count
must not change wire numbering, access ordinals, label bases, OT indices, or
output decoding.

### 7.3 External actions and oracles

The external-boundary plan already distinguishes value dependencies, action
chains, storage chains, and boundary batches. This scheduler adds storage
claims to that dependency graph; it does not make actions reorderable.

- A storage transaction may share a communication frame with a pure oracle
  only when the external-boundary plan says so.
- An action-chain predecessor remains a hard barrier.
- A secret guard never selects a worker lane or changes public batch shape.
- A deferred result is not visible to an iteration until its declared
  reinsertion ordinal has completed.

### 7.4 Cirrus VOLE adapters

Cirrus's current `LockedVoleProverStorage` and
`LockedVoleVerifierStorage` become serial/reference implementations behind a
common scheduler adapter. The parallel host runtime can use `std` threads
where available, while the core storage transaction and manifest types remain
usable by Cirrus's `no_std` target.

The adapter must preserve the role distinction:

- prover-side workers may compute independent authenticated values;
- verifier-side storage checks and Q-state updates are consumed in canonical
  order unless a separately validated parallel reduction is used;
- same-process ideal functionality remains a test fixture, not evidence for a
  real communication path;
- the real role-separated ML-KEM/Ferret route and its session/transport
  boundaries use the same manifest binding once integrated.

---

## 8. Composition with parallel communication reduction (#14160)

The scheduler is the semantic substrate for communication reduction, not a
second optimization that guesses which messages are independent.

### 8.1 Three layers

1. **Dependency planning:** extract data, storage, action, and protocol
   dependencies; form conflict-free epochs.
2. **Deterministic execution:** execute virtual-core work in an epoch, but
   merge outputs, storage events, correlations, and fold inputs by ordinal.
3. **Transport packing:** concatenate independent messages from one epoch into
   a bounded frame, in ordinal order, with one manifest/frame digest.

Communication reduction may change packet boundaries and buffering. It may not
change logical iteration order, memory timestamps, correlation assignment, or
the point at which a dependent value becomes visible.

### 8.2 Batch eligibility

A candidate message can join an epoch/frame only if all of the following hold:

- its iteration is in the same manifest epoch;
- its data dependencies are complete;
- its storage claims are compatible with all other epoch claims;
- it is not behind an action, external, continuation, or explicit host
  barrier;
- its protocol/correlation range and output width are within the public caps;
- its backend permits buffering it without changing observable state; and
- the packed frame has a deterministic ordinal and bounded size.

A cap split is deterministic: the planner walks canonical ordinals and closes a
frame at the same item on both roles. An OS scheduler cannot cause a different
split.

### 8.3 Protocol transcript

The transport should expose both:

- a logical transcript of `(epoch, iteration, message ordinal, message kind)`;
  and
- physical frame metadata (frame count, byte lengths, compression/packing
  version).

The proof/verifier binds the logical transcript. The deployment may choose
whether physical frame sizes are public; if they are public, they belong in the
manifest/resource policy. Tests must prove that packing and unpacking are a
bijection over logical messages.

### 8.4 Interaction with cross-region value threading

Communication packing must use the same local producer-to-consumer chains as
split weaving. It must not encourage a global shared prefix or transmit every
synthetic value to every worker. A packed parameter is sent only to the chunks
that consume it, in driver order, and its availability becomes an explicit
manifest dependency.

### 8.5 Measurements

For every reduction experiment report:

- sequential and parallel wall time;
- worker count and epoch/frame caps;
- number of logical messages and physical frames;
- bytes before and after packing;
- peak storage, trace, hat, correlation, and frame-buffer memory;
- lock wait and epoch barrier time;
- proof/verification time; and
- whether the schedule was static, bounded per-run, or serial fallback.

A smaller packet count without a semantic-equivalence and replay result is not
a successful optimization.

---

## 9. Verifier and soundness invariants

These are acceptance conditions for the first implementation, not claims that
the current code already satisfies them.

### 9.1 Manifest binding

The verifier rejects if any of these differ from its locally derived values:

- program/circuit digest;
- schedule format version;
- storage policy and type/lane map;
- epoch/iteration count or order;
- lock mode or public address partition;
- worker-lane/communication grouping rule;
- VOLE correlation range or gate/access ordinal;
- MPC protocol profile, session, or transport epoch.

### 9.2 Replay order

The verifier checks that iteration ordinals are exactly contiguous in the
manifest order. It derives memory event times from those ordinals, not from
arrival order. A fragment that arrives early may be buffered; it may not be
applied early.

For each iteration it checks:

1. declared inputs and dependencies are available;
2. authenticated gate/label/share values have the expected width;
3. actual storage accesses are within the manifest's conservative claim;
4. reads observe the storage image produced by the preceding canonical commits;
5. writes are committed once, in ordinal order;
6. all hats/labels/correlation slots for the iteration are consumed exactly
   once; and
7. the iteration output is connected to the declared next-state/consumer.

### 9.3 Memory relation

For `StorageMode::Commitment`, the schedule-ordered trace feeds the existing
produce/consume multiset check. For transparent Boolar/Mode-A audit paths,
the equivalent public `MemoryAccess::time` is the schedule ordinal. For tree
mode, the same order determines the authenticated storage transition.

The schedule does not replace the memory relation: a manifest that says a
read is legal does not prove that the returned value is correct. Both the
schedule and the value/authentication checks are required.

### 9.4 Secret-dependent scheduling prohibition

No schedule field may depend on a secret address, secret branch result, private
input bit, or OS timing. If a compiler cannot prove a public conservative
claim, it emits a serial/fixed-union schedule or rejects the program. In
particular, a worker must not skip a lock because a secret condition is false;
the public schedule must already account for the possible effect.

---

## 10. Implementation order

The order below is intentionally extractable: each phase leaves a usable
artifact and a validation gate before the next phase changes a protocol path.

### Phase 0 — Freeze the reference contract

**Deliverables**

- Record current sequential semantics for VOLE Tree and Commitment storage,
  main-branch `GateSchedule`/GRAM, and Cirrus locked storage.
- Add no parallel behavior yet. Name the serial adapters explicitly in tests.
- Inventory every storage operation and current trace/access ordinal producer.
- Decide the first supported iteration boundary for each path: split-weave
  block/chunk for VOLE, gate-range/GRAM transaction for MPC.

**Exit gate**

Existing targeted VOLE, Cirrus, Volar, and `volar-mpc` tests remain green; a
serial reference test can replay a complete trace from an initial storage
image.

### Phase 1 — Shared manifest and identity types

**Deliverables**

- Add versioned `StorageKey`, `StorageClaim`, `IterationManifest`,
  `EpochManifest`, and `ScheduleManifest` types in the smallest shared
  `no_std + alloc` layer.
- Implement canonical sorting, duplicate coalescing, bounded decoding, and a
  domain-separated manifest digest API. Do not call it a security proof or
  silently choose a hash profile; the protocol adapter supplies the approved
  digest.
- Represent typed storage lanes without flattening their identity into a
  textual name. Preserve `StorageId`/`TypeId` (or `LaneId`) as structured
  fields.
- Encode absent `StorageTable` policy as `ReadWrite`.

**Tests**

- permutation of input vectors yields byte-identical canonical manifests;
- duplicate/unsorted claims normalize identically;
- wrong version, oversized epoch, duplicate ordinal, and malformed range are
  rejected;
- two distinct typed lanes never collide in a lock key;
- manifest digest changes when any protocol-relevant field changes.

### Phase 2 — IR access analysis and extraction

**Deliverables**

- Extract conservative per-iteration storage summaries from Volar IR,
  Boolar/VCircuit schedules, and split-weave metadata.
- Add explicit analysis results for `Read`, `Write`, and `Unknown`.
- Thread storage-policy sidecar lookup through the analysis; missing entries
  are read/write.
- Emit static manifests for fixed circuits and a bounded per-run API for
  loops. Emit a serial fallback when extraction is incomplete.
- Add dependency edges for state/data flow, storage conflicts, actions,
  external boundaries, and continuation checkpoints.

**Tests**

- storage-free programs produce an empty lock set;
- read/read claims batch;
- read/write and write/write claims do not batch;
- same storage with different types/lane IDs batches only when independently
  safe;
- absent policy is conservative;
- secret-dependent address/branch produces a whole-key claim or serial
  fallback, never a secret-dependent public schedule;
- extracted order equals the existing circuit/IR order.

### Phase 3 — Generic transaction scheduler

**Deliverables**

- Implement the serial scheduler and a host-threaded scheduler over the same
  manifest.
- Add `SharedStorageBank`, epoch handles, transaction buffers, deterministic
  lock acquisition, actual-access validation, canonical write merge, and
  ordered trace fragments.
- Keep the scheduler core independent from Vope/Q, garbled labels, and
  concrete field types.
- Include a deterministic worker-lane simulator that randomizes physical
  completion order while retaining the manifest.

**Tests**

- one, two, and many virtual cores produce the same final storage image,
  outputs, and ordered trace;
- randomized host completion order produces byte-identical logical results;
- conflict attempts abort rather than retrying in a new order;
- a transaction failure leaves no partial writes;
- lock acquisition for multi-key iterations cannot deadlock;
- epoch caps and lane assignments are deterministic.

### Phase 4 — VOLE prover/verifier integration

**Current checkpoint:** the ticket/replay seam, per-ticket hat
buffer/source, canonical fragment merge, range validation, manifest-checked
merge path, and fail-closed completeness/consumption checks are landed in
Cirrus. A toy StackIo test drives independent
role-separated ML-KEM/Ferret sessions through identical cursors, but this is
only schedule/replay evidence: it does not wire schedule tickets into real
VOLE circuit execution, storage, or trace adapters. The remaining integration
must do that without using host completion order or reviving the deferred
closure path.

**Deliverables**

- Adapt split VOLE block/chunk functions to receive manifest tickets and
  deterministic correlation/hats ranges.
- Replace direct shared calls to whole-bank `LockedVole*Storage` with
  transaction-scoped adapters; retain the current wrappers as serial mode.
- Merge `MemoryTrace`, hats, outputs, and fold state by manifest ordinal.
- Extend the trace format with structured logical time and schedule digest,
  preserving a compatibility conversion for small existing fixtures.
- Bind continuation checkpoints to epoch and storage-image digests.

**Exit gates**

- VOLE serial and parallel prover outputs verify identically;
- verifier accepts only canonical schedule order;
- Commitment-mode memory checking passes for reads/writes across multiple
  independent storages;
- Tree-mode storage transitions match the serial reference;
- wrong correlation range, missing trace fragment, swapped worker result,
  duplicate ordinal, and manifest mismatch all fail closed;
- prover thread count does not change hats, trace order, or verifier result.

### Phase 5 — Main-branch lockstep MPC integration

**Deliverables**

- Add manifest validation and epoch/iteration cursor checks to the
  `volar-mpc` execution seam.
- Wrap GateSchedule/GRAM storage operations in transactions. Preserve the
  existing evaluator-side storage topology where applicable; the schedule is
  logical and does not assume shared address-space RAM across parties.
- Require both roles to acknowledge epoch boundaries and reject out-of-order
  frames.
- Permit local preparation on virtual cores only when wire IDs, OT/access
  ordinals, label bases, and frame ordinals remain manifest-derived.
- Keep action/oracle boundary ordering from the existing external plan.

**Exit gates**

- garbler and evaluator accept the same manifest and reject altered copies;
- one and many host workers produce the same outputs and logical transcript;
- a role that skips, duplicates, or reorders an iteration aborts;
- storage read-after-write, write conflicts, and independent storages match
  the serial GRAM reference;
- two simultaneous sessions cannot share a storage lock, transcript counter,
  or manifest digest accidentally.

### Phase 6 — Communication reduction integration (#14160)

**Deliverables**

- Add deterministic per-epoch message grouping and bounded frame packing.
- Preserve logical message ordinals inside packed frames.
- Add schedule-aware buffering to VOLE hats/correlation traffic and MPC
  transport/external batches.
- Measure the local cross-region parameter-threading path rather than adding a
  universal shared prefix.

**Exit gates**

- unpacking is a bijection over logical messages;
- packed and unpacked runs have identical schedule/transcript digests and
  outputs;
- resource-cap splits are identical on both roles;
- no dependency or action/storage ordering test regresses;
- communication bytes/frames improve on a representative workload without an
  unbounded memory increase.

### Phase 7 — Stress, review, and rollout

**Deliverables**

- Differential fuzzing of serial vs parallel scheduler and trace replay.
- Failure-injection corpus for malformed manifests, frame reorder, partial
  epoch, stale continuation, access-policy violation, and resource exhaustion.
- Thread sanitizer/loom-style interleaving coverage where supported by the
  host; deterministic simulator coverage remains mandatory for no-std code.
- Independent review of schedule leakage, lock granularity, correlation
  reservation, memory-trace binding, and protocol frame binding.
- Reliability/status updates before enabling parallel mode by default.

**Rollout**

1. serial mode remains the default;
2. parallel VOLE is opt-in behind a measured feature/configuration;
3. main-branch MPC requires manifest negotiation and lockstep validation;
4. communication packing is enabled only after logical-transcript tests pass;
5. unsupported/dynamic access shapes select serial fallback or abort, never a
   best-effort unscheduled path.

---

## 11. Validation matrix

| Area | Required property | Minimal fixture |
|---|---|---|
| Determinism | physical worker order does not change logical output/trace | two independent typed storages, randomized completion |
| Read sharing | concurrent reads see one epoch snapshot | many read-only iterations |
| Write exclusion | conflicting writes are ordered or rejected | same key, same/different addresses |
| Read/write | no stale or half-written observation | producer then consumer iterations |
| Policy | absent metadata is read/write | undeclared storage sidecar |
| Typed identity | lanes do not alias accidentally | same `StorageId`, two `TypeId`s |
| Address privacy | secret addresses do not affect schedule shape | symbolic/oblivious storage access |
| VOLE Tree | authenticated tree state matches serial | multiple `StorageId`/type pairs |
| VOLE Commitment | canonical memory multiset verifies | reads, overwrites, final drains |
| VOLE proof order | hats/correlations/fold state are canonical | split block/chunk fixture |
| MPC lockstep | both roles reject cursor/frame divergence | garbler/evaluator local transport |
| GRAM | storage operations retain access ordinals | `GateSchedule` read/write fixture |
| Packing | packed/unpacked logical transcript is equal | bounded epoch with mixed messages |
| Continuation | checkpoint resumes only at a committed epoch | disconnect before/after barrier |
| Sessions | no cross-session state or lock reuse | two concurrent sessions |
| Robustness | malformed manifests fail closed | fuzzed decode and reorder corpus |

Every semantic test must compare against the serial reference. Statement counts,
number of locks, or packet counts are secondary structural assertions; they do
not establish correctness by themselves.

---

## 12. Risks and decisions to record before implementation

1. **Lock granularity.** Start at `(StorageId, TypeId/LaneId)`. Address-level
   locking is allowed only with public partition proofs and explicit manifest
   versioning.
2. **Snapshot mechanism.** The first implementation may use lock-held
   transactions and write buffers. Copy-on-write/versioned snapshots are a
   performance follow-up, not a semantic change.
3. **Epoch size.** Select from measured working-set, correlation, and frame
   limits. A larger epoch is not automatically faster if it increases trace or
   buffer memory.
4. **Correlation reservation.** Prefer compiler-known per-iteration ranges;
   otherwise use deterministic per-worker ranges plus canonical merge. Never
   allocate by completion order.
5. **Dynamic control flow.** Movfuscate/fixed-union schedules are preferred.
   General secret-dependent scheduling falls back to serial execution.
6. **MPC transport ownership.** Keep manifest/cursor validation in the
   `no_std + alloc` session layer and put socket/thread orchestration in the
   `std` adapter.
7. **Trace compatibility.** Version any change from `u32 timestamp` to a
   structured ordinal. Do not reinterpret old timestamps silently.
8. **Failure policy.** An unexpected access, protocol range, or frame ordinal
   aborts the epoch/session. Retrying with a different schedule is not safe
   without a new manifest and protocol epoch.
9. **Security profile.** The schedule is public metadata. Any deployment that
   needs contention/access-pattern privacy requires a separate oblivious
   scheduling design and review.
10. **Cross-repository ownership.** Agree on the shared manifest crate/API
    before modifying Volar main or Cirrus so their adapters do not fork the
    schedule identity or digest encoding.

---

## 13. Prior art and what we borrow

This section records the external designs consulted for the scheduling model.
They are analogies and engineering inputs, not security reductions for Volar.

### 13.1 Bulk synchronous parallel (BSP)

Leslie Valiant's *A Bridging Model for Parallel Computation* introduces the
BSP model: bounded local computation followed by communication/synchronization
supersteps. This maps directly to our consistency epochs: worker-local
transactions run within an epoch, and the barrier is where writes, traces, and
communication become ordered.

**Borrow:** explicit superstep boundaries and accounting for communication and
synchronization rather than treating them as free.  
**Do not borrow:** BSP's ordinary shared-memory assumptions; our values may be
VOLE-authenticated or MPC-encoded, and the schedule must be protocol-bound.

Sources: [ACM DOI](https://dl.acm.org/doi/10.1145/79173.79181),
[author/paper copy](https://www.cs.ucdavis.edu/~amenta/f15/bridging.pdf).

### 13.2 Determinator / deterministic parallelism

Aviram et al.'s OSDI paper describes deterministic execution using
shared-nothing address spaces, deterministic communication, and private work
that is reconciled at synchronization points. Its private-workspace model is
a strong precedent for our per-iteration write buffers and epoch merge: workers
can calculate independently, but only the deterministic merge changes the
shared state.

**Borrow:** isolate worker-local state, make synchronization a semantic merge,
and distinguish deterministic program order from host timing.  
**Do not borrow:** an OS-level runtime or a claim that a private workspace
alone authenticates a cryptographic storage transition.

Sources: [USENIX paper](https://www.usenix.org/event/osdi10/tech/full_papers/Aviram.pdf),
[Yale project page](https://dedis.cs.yale.edu/2010/det/).

### 13.3 Calvin deterministic transaction scheduling

Calvin separates a globally agreed serial input sequence from deterministic
locking and concurrent execution. It is especially relevant to the requirement
that a verifier/replayed replica know the order without observing which worker
won a race.

**Borrow:** public sequence/manifest, declared transaction access sets,
deterministic lock grants, and execution that is concurrent only when the
serial order is preserved.  
**Difference:** our manifest is compiler/circuit-derived and may need
conservative whole-storage claims when logical addresses are secret. We cannot
require source programs to reveal secret address sets to the scheduler.

Sources: [ACM DOI](https://dl.acm.org/doi/10.1145/2213836.2213838),
[CMU paper copy](https://15799.courses.cs.cmu.edu/fall2013/static/papers/p1-thomson.pdf).

### 13.4 DuORAM / distributed oblivious RAM

DuORAM uses distributed point-function based queries and explicitly targets
bandwidth-efficient secure two- and three-party computation. It demonstrates
that independent reads and updates can be represented and transported in
batches, reducing communication without turning logical accesses into a
host-thread race.

**Borrow:** separate logical access semantics from compact/batched transport;
benchmark communication and online work rather than assuming one message per
access.  
**Do not borrow:** DuORAM's storage topology or DPF construction as the Volar
storage design. The current plan schedules the existing VOLE memory relation
and main-branch GRAM/ORAM adapters.

Sources: [IACR ePrint 2022/1747](https://eprint.iacr.org/2022/1747),
[USENIX Security 2023 paper](https://www.usenix.org/system/files/usenixsecurity23-vadapalli.pdf).

### 13.5 MASCOT and offline/online MPC

MASCOT separates input-independent preprocessing (authenticated multiplication
triples generated using OT) from the online arithmetic computation. The
analogy matters for VOLE correlation reservation and MPC batching: material
can be prepared ahead of the online schedule, but online consumption still
needs fixed ordinals and consistency checks.

**Borrow:** explicit preprocessing/online boundary, resource accounting, and
batch consistency checks.  
**Do not borrow:** its arithmetic-MPC security theorem or preprocessing
provider as a theorem for the current BitGC/VOLE/MPC code. Provider and
security-profile choices remain separately pinned work.

Sources: [IACR ePrint 2016/505](https://eprint.iacr.org/2016/505),
[ACM DOI](https://dl.acm.org/doi/10.1145/2976749.2978357).

### 13.6 Local precedent: Volar's own storage and weaving plans

The strongest implementation precedent is local: Volar's VOLE memory trace
already defines a canonical operation order; the Boolar trace audit already
models a public `time` and an execution/address-sorted memory permutation; the
split weaver already threads state through chunk functions; and the external
boundary plan already has deterministic dependency-aware batches.

**Borrow:** extend these existing ordinals and dependency contracts rather than
creating a parallel storage semantics. The new scheduler should be a common
execution-plan layer consumed by these paths.

---

## 14. Acceptance checklist

The design is ready for implementation only when all of the following are
explicitly agreed:

- [ ] The shared manifest crate/API and canonical digest encoding are owned by
      one repository boundary.
- [ ] The first iteration boundaries for VOLE and MPC are named and tested.
- [ ] `StorageKey` preserves typed IR identity; absent policy means read/write.
- [ ] The manifest is public, versioned, bounded, and binds program/storage/
      protocol identity.
- [ ] Secret addresses and secret branch results cannot alter public schedule
      shape.
- [ ] The serial reference path is retained and used in differential tests.
- [ ] Per-iteration storage transactions validate actual accesses and publish
      no partial writes on failure.
- [ ] Worker completion order cannot alter trace, hats, correlations, wire
      indices, labels, or fold order.
- [ ] VOLE verifier replay and memory checking consume manifest order.
- [ ] Both MPC roles validate epoch/frame/iteration ordinals and synchronize at
      epoch boundaries.
- [ ] Communication packing preserves a logical transcript and deterministic
      cap splits.
- [ ] Cross-region values use local packed-parameter threading, not a global
      shared prefix.
- [ ] Thread-count, randomized-interleaving, malformed-manifest, and
      continuation-boundary tests exist before enabling parallel mode.
- [ ] Security/reliability labels remain conservative until the protocol and
      transport reviews are complete.
