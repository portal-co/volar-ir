# Steel-in-Volar reference artifacts

S1 (frontend selection) artifacts for `docs/steel-in-volar-plan.md`.

## The reference program

`reference.rs` — a minimal Steel-subset view call: the state-access Merkle
proof for a single storage read. `key`/`value` form the leaf; the 4-deep
`path` folds up to a root. A stand-in multiply-xor hash keeps S1 about the
*frontend*, not the in-circuit Keccak (deferred to S6).

## Regenerating the artifacts (one build, both forms)

```sh
cd docs/steel
# WASM (the wasm32 frontend's input):
rustc --crate-type=cdylib --target wasm32-unknown-unknown -O -o reference.wasm reference.rs
# LLVM IR (the LLVM frontend's input) — from the SAME source:
rustc --crate-type=lib --emit=llvm-ir -O -o reference.ll reference.rs
```

## S1 measurements (the frontend decision input)

Driven by `volar-steel/tests/frontend_probe.rs`:

| Frontend | Crate | Output | Size |
|---|---|---|---|
| WASM | `volar-wasm-circuit-import` | `WasmCircuitArtifact` (a control-free `VCircuit`) | **66,625** circuit stmts (384 params, 64 outputs) |
| LLVM | `volar-llvm-vaffle-import` | `vaffle::Module` (VAFFLE) | **67,008** values (1 body) |

## S1 finding / decision

- **Both frontends import the reference program** and produce nearly identical
  sizes (~66.6K vs ~67.0K ops) — a good cross-check that they agree on the
  program's semantics. (The size is dominated by the four u64 `wrapping_mul`s;
  a u64 multiply is ~thousands of bit-gates.)
- **Decision: dual-support, with the LLVM frontend as the pipeline input for
  S2+.** The Steel pipeline (virtualize → movfuscate → PESAT) operates on
  VAFFLE/Volar IR, which is exactly what `volar-llvm-vaffle-import` produces.
  The WASM frontend's `VCircuit` output is control-free *already* (it bypasses
  virtualization/movfuscation), so it serves as (a) a cross-check oracle and
  (b) the path for simpler programs that don't need the step circuit. When the
  Steel stepper grows pointer/memory-heavy (S6), the LLVM frontend's
  `alloca`/`memcpy` support (the `docs/llvm-*.md` series) is the load-bearing
  piece.
