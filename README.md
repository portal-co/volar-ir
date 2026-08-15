# volar-ir

Volar IR and VAFFLE: a reusable compiler IR, lowering pipeline, and
optimization passes for WASM-derived circuits.

This repo holds the IR/circuit-compilation layer that was split out of
[`volar`](https://github.com/portal-co/volar), Portal's ZK-proof / garbled-circuit
/ MPC toolkit. `volar` still owns everything downstream of a boolean circuit —
ZK proof weaving, garbled-circuit weaving — and the spec-integration side of
compiling `volar-spec` through to C/LLVM/WASM, and consumes the crates here as
a regular dependency.

## What's here

- **VAFFLE** (`vaffle`, `volar-vaffle-target`) — a Volar-aware WASM-IR dialect,
  and its lowering from WAFFLE/VAFFLE into Volar IR.
- **Volar IR / Boolar IR** (`volar-ir`, `volar-ir-common`, `volar-ir-config`,
  `volar-ir-text`) — the field-level and boolean-circuit-level SSA IRs.
- **Lowerings** (`volar-ir-passes`): movfuscation (collapsing branching CFGs
  into a single oblivious dispatch block) and circuit lowering.
- **Virtualization** (`volar-ir-virt`): dispatch-mode block interpretation.
- **Optimizations** (`volar-ir-opt`): DCE, CSE, constant folding, and
  store-forwarding, across Volar IR, Boolar IR, and VAFFLE.
- **LIR** (`volar-lir`, `volar-lir-text`, `volar-lir-saved`,
  `volar-lir-test-corpus`) — the mid-level IR that this pipeline and the
  backends below build on.
- **Backends** (`crates/backends/volar-c-backend`, `volar-llvm-backend`,
  `volar-wasm-backend`) — the `LirTarget` implementations that lower LIR to
  C99, LLVM IR, and WASM, plus their pure-LIR-level tests. The
  spec-integration tests (compiling real `volar-spec` source through
  `volar-compiler` + `volar-lir-codegen` into these backends) stay in
  `volar` as `volar-c-backend-spec-tests`, since they exercise crates that
  are out of scope here.
- **Fuzz/property tests** (`crates/fuzz/volar-fuzz`, `fuzz/`) for the passes
  above.

See `docs/pipeline.md` for how these fit together, and `docs/` generally for
per-crate design docs carried over from `volar`.

## Building

```
cargo check --workspace
cargo test --workspace
```

Building inside a `portal-hot`-style checkout (as a sibling of `volar/`,
`waffle-/`, `wax/`) picks up the root `.cargo/config.toml` patches for
`portal-pc-waffle-ir`/`portal-pc-waffle-frontend`/`wax-meta`. Building this
repo fully standalone requires those as real git dependencies (already the
default in `Cargo.toml`).

## Relationship to `volar`

`volar` depends on the crates here via a `[patch]` entry when built inside
the same checkout, or a normal git dependency otherwise. Two things stay in
`volar` deliberately, not here:

- `volar-discipline` — the ZK/non-ZK proving-discipline typestate, which is
  specific to `volar`'s cryptographic pipeline, not to IR mechanics.
- `volar-ir-lir-target` — the Volar-IR→LIR-for-backends lowering, since it
  depends on `volar-compiler`/`volar-lir-codegen`, which are backend/codegen
  infrastructure out of scope here.
- `volar-lir-codegen` itself — the `IrModule`/`IrCfgModule` → `LirTarget`
  lowering, since it depends on `volar-compiler`.
- `volar-c-backend-spec-tests` — the spec-integration and
  `volar-lir-codegen`-dependent tests for the C backend (parses real
  `volar-spec` source, exercises `volar-weaver`).
- `fuzz/fuzz_targets/fuzz_vole_circuit_completeness.rs` — it also exercises
  `volar-spec`, so it's an integration test of the crypto pipeline's
  consumption of this IR, not a test of the IR itself.
