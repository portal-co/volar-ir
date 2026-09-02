# Circuit source emission

Textual library emission for fused circuits. This is **not** `LirTarget`
(C / LLVM / WASM). It walks [`BCircuit`] / [`VCircuit`] and renders a
reusable package for a language backend.

| Crate | Role |
|---|---|
| `volar-circuit-source` | Namer, named DAG, `CircuitSourceBackend`, `SourcePackage` |
| `volar-noir-backend` | Noir `type = "lib"` (bool + Volar IR) |
| `volar-pod2-backend` | Podlang module (bool + Merkle array/dict storage) |

Entry points: `emit_bool_circuit` / `emit_volar_circuit`, or
`Pipeline<BoolarCircuitStage>::emit_source` /
`Pipeline<VolarIrStage>::emit_source` (the latter fuses to `VCircuit` first).

## Wire names

Default names are `in{i}` for parameters and `w{id}` for statement results.
`EmitOptions::wires` overrides individual `IRVarId`s and `StorageId`s.
`name_all_wires` forces a binding for every wire; otherwise unnamed
single-use wires may be inlined (Noir) or kept as private wildcards (POD2).

Colliding sanitized names and language keywords fail closed.

## ABI

Generated artefacts are **libraries**. The embedder owns `main` (Noir) or
`REQUEST` (POD2) and therefore public/private witness flags.

- **Noir bool:** `pub fn eval_circuit(w: [bool; N]) -> [bool; M]`
- **Noir Volar:** `pub fn eval_circuit(in0: T0, …) -> R`
- **POD2:** `eval_circuit(wires, …)` verifies a Merkle `Dictionary` of wire
  values. It does not compute the circuit.

## Storage

Noir has no Merkle/maps. `StorageRead` / `StorageWrite` fail with a hint to
run `StorageToMux` or switch to POD2.

POD2 maps Boolar `(StorageId, LaneId, addr_bits)` onto Merkle arrays:
`ArrayContains` / `ArrayUpdate`. Constant addresses fold to integers;
symbolic addresses are reconstructed as `Σ bit_i · 2^i`.

## v1 exclusions

Oracles, actions, RNG, `RCircuit`, full CFG `IRBlocks` (unroll or
movfuscate first), Noir Merkle / Aztec `Map`, POD2 IntroductionPods, POD2
Volar-IR `Poly`. Unknown `BIrStmt` / `Stmt` variants panic with “add
lowering”.
