# Volar IR compiler context

This repository defines a reusable compiler IR family and the transformations between typed SSA, Boolean SSA, circuits, and reversible circuits.

## IR family

**Volar IR**:
The typed, field-level SSA control-flow representation in `volar-ir::ir`. It is one member of the Volar IR family, not a name for every representation in the repository.

**Boolar IR**:
The Boolean SSA control-flow representation in `volar-ir::boolar`, where values and storage cells are one bit. **BIR** is the accepted API abbreviation for Boolar IR.

**Volar IR family**:
The representation family containing Volar IR, Boolar IR, Boolean circuits, and reversible circuits.

**VAFFLE**:
The Volar-aware function/module IR that feeds lowering into Volar IR.

## Circuit representations and transformations

**Circuit-shaped Volar IR**:
An `IRBlocks` value with one return-terminated block, satisfying `is_circuit()`.
_Avoid_: circuit, when the distinction from a concrete circuit representation matters.

**Boolean circuit**:
The semantic notion of a Boolean computation represented as gates and wires.

**`BCircuit`**:
The concrete fused Boolean-circuit representation in the Volar IR family.

**Circuit lowering**:
Any transformation that creates circuit-shaped IR from non-circuit-shaped IR. This includes transformations that produce circuit-shaped Volar IR; it is not limited to Boolar IR → `BCircuit` conversion.

**Movfuscation**:
The transformation that converts arbitrary Volar-IR control flow into a single self-looping step program using real/fake execution flags.

**Movfuscated Volar IR**:
The self-looping Volar-IR program produced by movfuscation. The same term may describe the corresponding Boolar transformation when the representation is clear from context.

**Unrolling**:
The finite control-flow transformation that follows concrete execution paths to produce a non-looped circuit-shaped program.

## Virtualization

**IR virtualization**:
The transformation that replaces source blocks with deduplicated instruction handlers and a dispatcher backed by initialized bytecode storage.

**Virtualized program**:
An IR program produced by IR virtualization, containing a dispatcher, virtualization handlers, and initialized bytecode storage.

**Virtualization handler**:
A deduplicated instruction body emitted by IR virtualization. Use the qualified term when documentation crosses virtualization and provenance or backend terminology.

## Storage

**Storage namespace**:
A complete storage identity denoted by a `StorageId`.

**Typed storage cell**:
A Volar-IR or VAFFLE cell identified by `(StorageId, TypeId, address)`.

**Boolar storage cell**:
A Boolar cell identified by `(StorageId, LaneId, address)`.

**Storage image**:
The initial storage contents supplied by `pre_init`, together with the implicit zero value for cells absent from that initialization.

**Storage-access sidecar**:
The compatibility metadata held in a `StorageTable` that declares whether a storage namespace is `ReadOnly` or `ReadWrite` without changing persisted IR carrier layouts.

## Pipeline and metadata

**Lowering**:
A representation-changing transformation between IR layers. A lowering may also be a circuit lowering when its result is circuit-shaped.

**Circuit lowering**:
A lowering or other transformation whose result is circuit-shaped IR, including creation of circuit-shaped Volar IR from non-circuit-shaped Volar IR.

**Pass**:
A compiler transformation or analysis that consumes an IR and produces a changed IR or metadata result. Validators and analyses may be passes when they participate in the compiler pipeline.

**VAFFLE module**:
A module in the VAFFLE representation, containing functions and their control-flow bodies.

**IR program**:
An `IRBlocks` value representing executable Volar IR. A program has entry control flow and results but does not contain a function collection.

**Boolar program**:
A `BIrBlocks` value representing executable Boolar IR. A program has entry control flow and results but does not contain a function collection.

**Pre-initialization**:
The mechanism and data supplied through a representation's `pre_init` field to establish initial storage contents.

**Pre-init segment**:
One typed or Boolar segment in pre-initialization, such as `PreInitSegment` or `BIrPreInitSegment`.

**Node provenance**:
The source-attribution value attached to an IR node and preserved through transformations.

**Provenance handler**:
The mapping or combining mechanism that supplies, transforms, or preserves node provenance.

**Provenance pipeline**:
The end-to-end preservation of node provenance across lowering and optimization transformations.

**Side**:
The actor or party tag attached to a value or computation in a multi-actor program.

**Side tracking**:
Preserving side tags through IR construction and transformations.

**Region**:
A generic set of IR items given a shared meaning. A region may contain control-flow items, wire items, or another representation-specific class of IR items.

**Wire region**:
A region whose items describe a tagged input/output wire boundary in a circuit.

**Control-flow region**:
A region whose items describe a control-flow or execution boundary.

**Gadget**:
Code or a circuit fragment attached to a region to implement behavior at that region.

**Typed gadget**:
A gadget authored through the typed region/gadget API, with typed anchors and circuit bodies.

**Looping**:
Repeatedly executing a circuit-shaped IR instance to conceptually reobtain a program in that same IR representation. Looping is the semantic idea behind self-looping and bounded repeated circuit execution.

**Reentry hint**:
Legacy metadata on a control-flow edge describing a measure that strictly decreases when control re-enters a block. It supports bounded-loop reasoning and virtualization planning; it is not executable behavior.

**Read-only storage**:
A storage namespace for which the producer guarantees that no IR-visible write targets any typed or Boolar lane under its `StorageId`.

**Read-write storage**:
A storage namespace that may be written, or whose mutability is not explicitly proven otherwise. An absent storage-access declaration is conservatively read-write.

## Virtualization and metadata flow

**Producer**:
A transform or frontend that establishes metadata or creates an IR result for downstream use.

**Consumer**:
A pass, backend, or API that reads a representation or metadata established by a producer.

**Dispatcher**:
The control-flow component that selects the active virtualization handler from the current bytecode row.

**Handler index**:
The bytecode-selected identifier of a virtualization handler.

**Bytecode table**:
The logical sequence of handler and immediate entries used by a virtualized program.

**Bytecode row**:
One logical entry in a bytecode table.

**Bytecode storage**:
The storage namespace holding a virtualized program's bytecode table.

**Setup block**:
The runtime initialization and control block emitted by virtualization.

**Setup write**:
An IR-visible storage write performed by a setup block. It is distinct from pre-initialization.

**Commitment storage**:
Read-only storage holding preinitialized per-handler commitment values.

**Key storage**:
Read-write storage holding optional runtime key parameters used by keyed virtualization.

**Commitment configuration**:
The algorithm, storage, and key settings controlling per-handler commitments.

**Keyed virtualization**:
Virtualization configured with per-handler commitments and the corresponding key parameters.

**Per-handler commitment**:
A commitment value associated with a deduplicated virtualization handler and initialized for handler lookup; it is not a generic per-PC name.

**Adaptive split**:
Virtualization planning that extracts reusable regions before handler emission.

**Shared-core region**:
A cross-block reusable computation identified by adaptive split planning.

**Reroll-loop region**:
An intra-block repeated computation represented as a loop by adaptive split planning.

**Split plan**:
The analysis output consumed by adaptive virtualization to emit selected shared-core and reroll-loop regions.

## Testing vocabulary

**Semantic preservation**:
Equality of evaluator outputs before and after a transformation for the same inputs and initial storage image.

**Property test**:
A generated test that asserts an invariant over many inputs or IR instances.

**Fixture**:
A concrete IR artifact supplied for repeatable testing, whether checked in or obtained from an explicitly configured external location.

**Structural regression**:
A test of representation shape, counts, or layout rather than evaluator semantics.

**Resource-gated test**:
An explicitly opt-in test whose runtime or memory cost is reported instead of enforced in normal CI.

**Robustness property**:
A generated-input property asserting that a transform completes without panicking, independently of semantic preservation.

## Values, state, and interfaces

**Typed IR**:
Volar IR whose values have `IRTypeId`-described types. Typed storage is distinguished by `TypeId`, while typed gadgets use the typed region/gadget authoring API.

**Scalar value**:
One non-`Vec` IR value.

**Wide value**:
A scalar value wider than one bit, including `_32`, `_64`, and field values wider than `Bit`.

**Bit-granular**:
Boolar's one-bit wire and storage-cell model.

**Execution state**:
Values carried between execution steps or blocks, including parameters and virtualized registers.

**Storage state**:
The current contents of storage namespaces after applying pre-initialization and IR-visible writes.

**Workspace**:
Temporary circuit wires or registers that are not part of the logical program state.

**Block parameter**:
A value supplied when control enters an IR block.

**Entry parameter**:
A parameter supplied to the program's entry block.

**Return value**:
A value supplied by a return terminator to the program or caller.

**Program input/output**:
Values crossing an evaluator or program API boundary. Use block parameter and return value for internal IR control flow.

**Wire input/output**:
Values crossing a circuit wire boundary.

## Dispatch and effects

**Public dispatch**:
Dispatch where the handler index and control-flow choice are assumed visible or agreed by the participating parties.

**Oblivious dispatch**:
Dispatch where every handler executes and results are selected without revealing which handler is active.

**Action**:
An externally declared stateful or effectful operation.

**Action target**:
A storage namespace or cell that an action may mutate.

**Action store**:
The IR representation of action-mediated storage mutation.

**Oracle call**:
A pure, deterministic external value-producing operation represented by the IR. Purity distinguishes an oracle from an action.

**RNG operation**:
A nondeterministic or random value-producing operation. It is distinct from storage mutation and from a pure oracle call.

**Optimization**:
A semantics-preserving transformation whose purpose is to reduce representation size, execution cost, or downstream compilation cost. Virtualization is an optimization because it reduces code size through handler deduplication.

**Canonicalization**:
A normalization that gives equivalent IR a stable representation for comparison or deduplication; it is supporting analysis unless it also reduces downstream cost.

**Calling convention**:
The mapping of arguments and results across blocks or functions.

**ABI**:
An externally stable calling convention at a frontend or backend boundary.

## IR mechanics

**Control-flow graph (CFG)**:
The directed graph formed by blocks and their control-flow edges.

**Block**:
A sequence of SSA statements entered through block parameters and exited by one terminator.

**Terminator**:
A block's control-flow exit.

**Branch target**:
The destination block or return plus the arguments carried across that control-flow edge.

**SSA value**:
A block parameter or statement result, defined once within its IR scope.

**IR variable**:
An `IRVarId` reference to an SSA value. Variable is acceptable narrative shorthand for an SSA value when no distinction is needed.

**Alias**:
A replacement mapping from one SSA value to an equivalent value without changing the represented computation.

**Polynomial statement**:
A `Stmt::Poly` operation over the field associated with its result type.
_Avoid_: Poly, except for the API variant or type name.

**Monomial**:
A product of SSA values in a polynomial statement.

**Coefficient**:
The field coefficient of a monomial in a polynomial statement.

**Constant term**:
The part of a polynomial statement independent of SSA operands.

**IR type**:
An `IRType` describing a value representation.

**Type ID**:
An `IRTypeId` or `TypeId` handle naming an interned IR type; both handle types denote the same kind of concept at their respective IR layers.

**Type table**:
The canonical collection mapping type IDs to IR types.

**Boolar lane**:
A `LaneId` namespace beneath one storage namespace, used to distinguish bit-granular storage domains that share a `StorageId`.

**Fusion**:
The transformation that converts a self-looping program into its step circuit. It is the inverse of looping at the representation level.

**Fused circuit**:
The step-circuit result of fusion, represented by `BCircuit` or `VCircuit` as appropriate.

**Reversible lowering**:
Lowering a Boolean circuit into an `RCircuit`.

**Reversible circuit**:
An `RCircuit` whose operations preserve enough information to be inverted. Reversible circuits may support storage; storage support adds no separate reversibility requirement.

**Reversible workspace**:
Temporary wires required by a reversible embedding that are not logical program state.

**MUX / demux**:
A multiplexer selects one of multiple values; a demultiplexer routes a value or update to one selected destination. MUX and demux are accepted shorthand.

**MUX lowering**:
Replacing bounded storage or control choices with explicit MUX and demux logic.

## LIR boundary

**LIR**:
The mid-level IR intended for lowering to native code outside this project, using standard integer operations and backends such as LLVM or C.

**Cryptography-friendly representation**:
Volar IR or Boolar IR when retained as an internal representation because its form is suitable for cryptographic circuit/protocol consumers.

**LIR retargeting**:
Lowering LIR back to Volar IR or Boolar IR when needed. This does not make LIR an acceptable intermediate representation for an internal pass.

**Function-capable intermediate representation**:
VAFFLE, the required internal intermediate representation when a transformation needs function-call support.

## Execution and virtualization mechanics

**Program counter (PC)**:
The current original-block or bytecode-row index used by a virtualized program to select its next handler. Outside virtualization, use the representation's own control-flow terminology rather than assuming a PC exists.

**Next PC**:
The row index selected by the current virtualization handler's terminator arm.

**Virtualization register**:
A typed, storage-backed location used to carry execution state between virtualization handlers.

**Register file**:
The complete set of virtualization registers allocated for one IR type.

**Block skeleton**:
A block's structure after row-varying immediates and concrete target IDs have been lifted out.

**Canonical handler key**:
The canonical representation of a block skeleton used as a handler-deduplication key. Code-level names such as `IrHandlerKey` and `BirHandlerKey` may be used when referring to APIs.

**Handler deduplication**:
Reusing one virtualization handler for source blocks with the same canonical handler key.

**Concrete control flow**:
Control-flow choices and relevant addresses that resolve at transformation time.

**Symbolic control flow**:
Control-flow choices that depend on runtime values and therefore remain represented in the output.

**Concrete address / symbolic address**:
The same transformation-time versus runtime distinction applied to storage addresses.

**Step program**:
A self-looping, circuit-shaped Volar-IR or Boolar-IR instance that advances execution state once.

**Step circuit**:
The fused circuit representation of one step program.

**Static data**:
Data known and installed through pre-initialization at compile or transformation time. Static data is public by default. The narrow exception is data hardcoded into indistinguishability obfuscation, where it may be witness-private; that cryptographic exception must be documented explicitly.

**Dynamic data**:
Data computed or written during program execution. Static bytecode rows and per-handler commitments are distinct from dynamic entry routing and key writes.

## Optimization and semantic vocabulary

**Common-subexpression elimination (CSE)**:
A semantics-preserving optimization that replaces repeated equivalent pure computations with one result.

**Dead-code elimination (DCE)**:
A semantics-preserving optimization that removes results whose computation has no required effect or live use.

**Store forwarding**:
Replacing a storage read with a known preceding write under the typed- or lane-aware aliasing policy. Use this term rather than storage-read forwarding.

**Structural**:
Describes an IR's representation, layout, or shape independently of evaluated behavior.

**Semantic**:
Describes behavior observed by evaluation for specified inputs and storage state.

**Structural invariant**:
An intentionally tested shape property, such as the requirement that output is movfuscated.

## Publicness and consumer policy

**Public dispatch**:
A virtualization dispatch mode that assumes the handler index and control-flow choice are visible or agreed by participating parties. It is not a `SideId` assignment.

**Witness-side / public-side**:
Consumer-defined side meanings. ZK consumers may use witness-side and public-side values; public-side is the generic term because this IR also supports MPC and other protocols where ZK-specific terminology is not applicable.

**Protection**:
Consumer-owned policy for interpreting side tags. Protection policy is not embedded in the generic IR.

## Reliability, pinnedness, and stability

**Pinnedness**:
The strength of evidence tying a claim or implementation to an external specification, review, or formal proof. The levels are Unpinned, Paper-pinned, Reviewed, and Proven.

**Stability**:
How suitable code is for dependents and how likely its semantics, API, performance profile, or implementation are to change. The levels are Forever, Stable, Semver, Unstable, and Very unstable. Stability is not a security claim; Forever requires Proven pinnedness.

**Legacy reliability marker**:
The deprecated single `@reliability:` marker. It is a migration signal, not a pinnedness or stability claim; legacy `experimental` additionally conservatively implies Very unstable treatment until assessed.

**Evidence-led classification**:
Pinnedness and stability are assigned from documented evidence and intended dependent contracts, not inferred from crate names, old labels, model identity, or passing tests. These tags are useful to downstream dependents even though this repository is not required to mark every source file.

See [`docs/reliability.md`](docs/reliability.md) for the complete policy, marker syntax, migration rules, and reclassification protocol.
