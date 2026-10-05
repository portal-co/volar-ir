# Boolar ↔ Bristol and Boolar → Summon interoperability plan

**Status:** implementation in progress — circuit validation and Bristol Fashion import landed
**Scope:** storage-free, circuit-shaped Boolar IR only.

## Goal and directions

Provide three explicit conversions:

| Direction | Result |
|---|---|
| Boolar IR → Bristol | Emit a Boolean Bristol Fashion circuit. |
| Bristol → Boolar IR | Parse a supported Boolean Bristol Fashion circuit into one circuit-shaped `BIrBlocks<()>`. |
| Boolar IR → Summon source | Emit a reusable TypeScript/Summon Boolean-circuit helper. |

There is deliberately **no Summon-source → Boolar** conversion in this plan. Bristol is the interchange format; Summon source is an additional one-way target.

Input Boolar must be a single block terminating in `Jmp(Return)`, have no `pre_init`, and contain only pure Boolean gates (`Zero`, `One`, `And`, `Or`, `Xor`, `Not`). Reject storage, oracle/action, RNG, conditional control flow, and other unsupported constructs with structured errors. The existing `BIrBlocks::is_circuit()` establishes the single-block/return shape, but does not itself guarantee storage-free contents.

## Research findings

### Bristol circuits (Nigel Smart's MPC-Circuits)

The linked site calls its current format **Bristol Fashion**, distinguishing it from the legacy Bristol Format. Fashion has a gate/wire-count header, grouped input widths, grouped output widths, then topologically ordered gates. Each gate declares input/output arities and wire IDs. Inputs occupy the first wire IDs; outputs occupy the final wire IDs. Basic Fashion supports `AND`, `XOR`, `INV`/`NOT`, `EQ` (constant zero/one), and `EQW` (wire copy); the extended dialect adds `MAND`, which batches independent ANDs. [1]

Implications:

- Make Bristol Fashion the canonical emitted format and the first parser target. Do not silently guess between Fashion and legacy headers. If legacy Bristol Format is needed, add it as an explicit parser mode after Fashion is stable. The site documents the old header separately. [1][2]
- Boolar `Not`, `And`, and `Xor` map directly to `INV`, `AND`, and `XOR`. Boolar `Or` has no basic Fashion gate; lower it as `(a XOR b) XOR (a AND b)` (three gates), or use another equivalent decomposition verified by tests.
- Encode Boolar constants using Fashion `EQ` constant gates. Import `EQW` as a wire alias/remap rather than adding a redundant Boolar operation. Accept `MAND` on import by expanding each input pair to one `And` result; it is optional on export.
- Bristol's terminal-output-wire convention differs from Boolar's return-argument convention. Always append `EQW` copies into a final, contiguous output-wire range when exporting. This also handles outputs that alias inputs, constants, or each other.
- Boolar has a flat parameter count and ordered return args, not Bristol party/value group metadata. Keep the parsed Bristol group widths in a small interface sidecar; default Boolar export to one input group and one output group unless the caller supplies grouping metadata. Do not claim that group names or parties survive conversion.

### Summon

Summon is a TypeScript dialect/compiler for arithmetic and Boolean circuits. Its README documents Bristol output (`output/circuit.txt`) plus a separate `circuit_info.json`; `--boolify-width` applies Boolify to produce Boolean circuits. This means ordinary Summon Bristol output is not necessarily a Boolar gate circuit: import must validate the actual gate operations and reject arithmetic/non-Boolean opcodes rather than infer their meaning. [3][4]

The repository's `bristol_to_summon` binary demonstrates the intended reusable-source shape: a generated TypeScript function accepts Boolean arrays, evaluates gates, and returns an output slice. Its source parser understands Fashion-style grouped headers, but its TypeScript renderer only supports unary `INV` and binary `XOR`/`AND`; it is a private CLI implementation rather than a public conversion API. It also performs wire recycling, which is unnecessary for Boolar-to-source generation and should not be copied blindly. [5]

For this project, emit static straight-line helper source (named temporaries / fixed numeric indexes), not a Summon import feature and not a run-time parser. Map Boolar `Or` directly to boolean `||` in source; keep Bristol's explicit basis expansion separate. Validate representative generated helpers by compiling them with Summon when that tool is available. [3][5]

### Existing repository context

Boolar is an actively supported bit-only IR. Circuit fusion provides the canonical shape validation but preserves storage, so the new interop layer must add its own storage/effect checks. Existing source emission operates on fused `BCircuit` / `VCircuit` and intentionally has different backend scope; the interoperability surface here starts at `BIrBlocks`. [6][7]

The repository's `volar-ir-opt` is `no_std`, while text/code-generation crates are `std`. Keep file parsing and source rendering out of `volar-ir-opt`; prefer an independent std-facing interop crate (working name `volar-circuit-interop`) or a narrowly scoped addition to an existing std circuit-source crate after reviewing its public API. Do not add a dependency on Summon itself. The pinned `bristol-circuit` Rust library used by Summon is a possible parsing/serialization adapter, but it expects a separate `CircuitInfo` sidecar and models generic string opcodes; verify that it meets this importer’s validation needs before adopting it. [8][9]

## Proposed API and format contract

Keep a format-neutral validated circuit representation between parsing/emission and Boolar lowering, for example:

```rust
struct BooleanCircuit {
    input_groups: Vec<usize>,
    output_groups: Vec<usize>,
    gates: Vec<BooleanGate>,
}

enum BooleanGate { And(usize, usize, usize), Xor(usize, usize, usize), Not(usize, usize), Const(bool, usize), Copy(usize, usize) }
```

Names are illustrative; internal representation may use a richer validated gate enum. Parse Bristol into this form, validate it once, then lower to Boolar. Emitters consume a validated storage-free circuit-shaped Boolar view. Keep parser and emitter errors explicit (line number, malformed header, arity/op mismatch, undefined or duplicate wire, out-of-range wire, non-topological use, inconsistent gate/wire counts, output-layout error, unsupported opcode, or unsupported Boolar statement).

**Bristol Fashion v1 contract**

- Parse exact Fashion headers and grouped widths; require declared counts to match parsed records and prevent integer overflow / unreasonable allocations.
- Validate gate arity and output arity per opcode; validate all wire IDs, unique definitions, and topological availability. Treat only the documented `EQ` literal-zero/literal-one cases as constants; reject other `EQ` forms.
- Require supported operations only: `AND`, `XOR`, `INV`/`NOT`, `EQ`, `EQW`, and optionally `MAND`. Unknown arithmetic or protocol gates fail closed.
- Lower gates in source order to Boolar SSA variables. Inputs map to the initial parameter IDs; constants and actual Boolean gates append statements; copies alias the referenced value. Return the final contiguous output wires in order.
- On emission, assign all Boolar params to initial Bristol wires; allocate fresh unique wires for gates; emit constants and gates in topological order; append output copies last; compute accurate gate and wire counts. Default to one input/output group, or validate caller-provided group widths against total params/outputs.

**Summon source v1 contract**

- Emit a documented reusable exported function with Boolean-array inputs and Boolean-array outputs, compatible with Summon's demonstrated helper pattern. Keep argument lengths explicit and checked.
- Generate only static, deterministic gate statements. No `eval`, dynamic wire indexing driven by signals, storage, IO party policy, or source parsing.
- Map `Zero`/`One` to boolean literals, `And`/`Or`/`Xor`/`Not` to `&&`/`||`/`!==`/`!`. Preserve ordered outputs, including repeated and passthrough outputs.
- Use deterministic identifier allocation and safe escaping/naming; source generation must fail on unsupported IR rather than emit partial source.
- This is a source target only. Do not make the Summon compiler or CLI a required runtime dependency.

## Work plan

### 1. Lock the boundary and errors

- Add a validated circuit-shaped Boolar view or reusable validation routine: exactly one return block, no conditional terminator, no pre-initialized storage, no storage/effect statements, and valid variable references.
- Establish whether `BIrBlocks::is_circuit()` / fused `BCircuit::try_from_ir` can be reused without losing provenance or side data; retain the explicit storage-free check either way.
- Define structured interop errors and the Bristol IO-layout sidecar. Document that input/output group boundaries are metadata, not part of Boolar semantics.

### 2. Bristol Fashion parser and Boolar importer

- Implement Fashion headers and the basic gate subset first; add `MAND` expansion if the format-neutral gate representation makes it straightforward.
- Validate counts, arities, definitions, references, ordering, constants, and final output layout before constructing Boolar.
- Add legacy Bristol Format only behind an explicit format selection and only if a concrete fixture/use case justifies it. Never infer a dialect from a partially parsed file.

### 3. Bristol Fashion emitter

- Emit `AND` / `XOR` / `INV`, constants and output copies; lower `Or` to a verified basis network. Consider `MAND` batching only as an optional later optimization.
- Guarantee deterministic output, contiguous final outputs, and valid headers even for zero-input, zero-output, passthrough, repeated-output, and constant-only circuits.

### 4. Summon source emitter

- Emit static straight-line source from validated Boolar circuits, including readable stable names and boundary checks.
- Keep source generation independent from Bristol emission so each target has direct tests and diagnostics.
- Add an optional integration check that runs Summon's compiler on generated sources; do not make local test success depend on network access or an installed Summon binary.

### 5. Semantic validation and hardening

- For small circuits, exhaustively enumerate inputs and compare the Boolar interpreter against both emitted-then-imported Bristol and generated Summon behavior.
- For larger circuits, use deterministic random vectors and existing property/fuzz infrastructure; compare output vectors, not statement counts or textual shape.
- Add fixtures covering constants, all primitive gates, `Or`, multi-input/output groups, `MAND`, `EQW`, repeated/passthrough outputs, empty boundaries, malformed records, use-before-definition, invalid IDs, unsupported gates, and nonempty Boolar storage/effects.
- Fuzz the text parser with bounded inputs; make all malformed/unsupported cases return errors rather than panic or allocate from unchecked counts.

## Acceptance criteria

- A storage-free circuit-shaped Boolar program round-trips through the supported Bristol dialect with identical outputs for exhaustive/random test inputs.
- Exported Fashion files satisfy the documented gate syntax and input/output wire layout; imported valid Boolean Fashion circuits lower to equivalent Boolar programs.
- Summon helpers are deterministic, preserve their Boolean function and ordered IO, and compile in the optional Summon integration test.
- Non-circuit shape, storage/pre-init, external effects, malformed files, unsupported gates, and unsupported Boolar variants fail explicitly and without panic.
- No Summon reverse importer, cryptographic/protocol behavior, wire-party semantics, or storage support is implied by the API.

## Primary sources

1. Nigel Smart, [Bristol Fashion circuits](https://nigelsmart.github.io/MPC-Circuits/) — current dialect, headers, gate order, IO wire layout, basic/extended gates. [Legacy Bristol Format](https://nigelsmart.github.io/MPC-Circuits/old-circuits.html).
2. [Bristol Fashion gate examples](https://nigelsmart.github.io/MPC-Circuits/) — `EQ` constants, `EQW` copies, and `MAND` expansion semantics (same source as [1]).
3. Summon [README](https://github.com/privacy-ethereum/summon/blob/main/README.md) — compiler, Bristol output, `--boolify-width`, and external Bristol-to-source helper example.
4. Summon [`cli/src/main.rs`](https://github.com/privacy-ethereum/summon/blob/main/cli/src/main.rs) — `--boolify-width` invokes Boolify before writing Bristol output.
5. Summon [`cli/src/bin/bristol_to_summon.rs`](https://github.com/privacy-ethereum/summon/blob/main/cli/src/bin/bristol_to_summon.rs) — parser, wire recycling, and TypeScript support for `INV`/`XOR`/`AND`.
6. Repository [`docs/agent-context/boolar-ir-conflicts.md`](agent-context/boolar-ir-conflicts.md) — active Boolar IR status and bit-only boundary.
7. Repository [`docs/circuit-source.md`](circuit-source.md) and [`crates/ir/volar-ir-passes/src/fuse_to_circuit.rs`](../crates/ir/volar-ir-passes/src/fuse_to_circuit.rs) — current source-emission and circuit-fusion boundaries.
8. Summon [`vm/src/circuit.rs`](https://github.com/privacy-ethereum/summon/blob/main/vm/src/circuit.rs) — its Bristol adapter emits generic gate names and per-wire metadata.
9. Summon [`Cargo.toml`](https://github.com/privacy-ethereum/summon/blob/main/Cargo.toml) pins `bristol-circuit` to [`voltrevo/bristol-circuit` revision `10ee9c7`](https://github.com/voltrevo/bristol-circuit/tree/10ee9c7); its parser/writer requires separate `CircuitInfo` metadata (`BristolCircuit::from_info_and_bristol_string`).
10. Repository [`docs/pipeline.md`](pipeline.md) and [`docs/ir-lowering.md`](ir-lowering.md) — Boolar's pipeline, representation, and crate constraints.
