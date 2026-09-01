# Volar IR / VAFFLE Lowering Pipeline

> This document covers the low-level circuit-IR slice of the pipeline that
> lives in this repo: VAFFLE, Volar IR, Boolar IR, and the passes between
> them. It does **not** cover the high-level `IrModule`/weaving/backend
> layers (`volar-compiler`, `volar-weaver`, `volar-c-backend`, etc.) — those
> live in the [`volar`](https://github.com/portal-co/volar) repo, whose
> `docs/pipeline.md` describes the full graph this slice plugs into.

---

## The IR family and the transforms between them

```
 VAFFLE (volar-aware WAFFLE; optimizations, inlining)
     │
     │  replace function calls with block jumps  (volar-vaffle-target)
     ▼
 Volar IR   (SSA; field-level; protocol-agnostic)
     │  constant folding, DCE, CSE  (volar-ir-opt)
     │
     │  replace conditional blocks with real-vs-fake flags  (movfuscate.rs)
     ▼
 Movfuscated Volar IR
     │  constant folding, SSA  (volar-ir-opt)
     │
     │  booleanize
     ▼
 Boolar IR  (boolean SSA)
     │  peephole opts, DCE, CSE  (volar-ir-opt)
     │
     │  circuit lowering  (lower_to_circuit.rs)
     ▼
 Boolean circuit ── fuse_to_circuit.rs ──▶ circuit-fused BCircuit/VCircuit
                                              │
                                              │ to_reversible.rs
                                              │  naive: (x,y) ↦ (x,y ⊕ f(x))
                                              │  hardened: zero workspace runs
                                              │  f; nonzero workspace is identity
                                              ▼
                                       Reversible circuit (RCircuit:
                                       X/CNOT/Toffoli/XorLut2/StorageSwap)
                                              │
                                              │ apply_gadgets.rs
                                              │  splice gadget circuits onto
                                              │  region-tagged boundary wires
                                              ▼
                                       Wrapped BCircuit (ciphertext boundary)
                                              │
                                              │ to_boolar_circuit
                                              │ full wire-state transition
                                              ▼
                                   BCircuit (all wires as inputs + outputs)
                                              │
                                              │ hardcode inputs / remove outputs
                                              ▼
                                      caller-selected Boolar projection
```

`to_reversible` preserves the original naive behavior. Library consumers opt
into the clean, total-workspace transform with
`to_reversible_with_mode(..., ReversibleMode::Hardened)`. Hardened mode follows
the reversible embedding invariant in Appendix A of Canetti, Chamon, Mucciolo,
and Ruckenstein, *Towards general-purpose program obfuscation via local mixing*
(IACR ePrint 2024/006): it restores both synthesized workspace and arbitrary
dirty borrowed wires, and becomes identity for every nonzero workspace input.
Stateful storage statements remain available in naive mode and are rejected by
hardened mode because they do not have the required pure wire-function contract.

`to_boolar_circuit` exposes an `RCircuit` as an ordinary `BCircuit` without
guessing its logical ABI: reversible wire `i` becomes Boolean parameter and
output `i`. Consumers may hardcode known workspace/register inputs and remove
unneeded output positions to obtain a conventional projection. These helpers
do not remove dead statements or effects; run the normal optimization passes
afterward when appropriate. Action-backed storage-XOR gates lower through
Boolar's legacy action-call form, so their explicit reversible replay
`occurrence` becomes ordinary Boolar action ordering.

Consumers outside this repo (in `volar`) take the boolean circuit (or the
movfuscated Volar IR directly) into ZK proof weaving or garbled-circuit
weaving — see `volar`'s `docs/garbling-pipeline.md` and `docs/vole-weaving.md`.

## Crates

| Crate | Role |
|---|---|
| `vaffle` | VAFFLE module representation, mirrors `portal-pc-waffle-ir::Module` |
| `volar-vaffle-target` | Lowers VAFFLE (and WAFFLE, via `portal-pc-waffle-frontend`) into Volar IR |
| `volar-ir` | Volar IR and Boolar IR types |
| `volar-ir-passes` | `movfuscate.rs` (movfuscation), `lower_to_circuit.rs` (circuit lowering), `fuse_to_circuit.rs` (circuit-fused forms), `to_reversible.rs` (reversible lowering) |
| `volar-ir-opt` | DCE, CSE, constant folding, store-forwarding — across Volar IR, Boolar IR, and VAFFLE |
| `volar-ir-virt` | Virtualization (dispatch-mode block interpretation) |
| `volar-lir` / `volar-lir-text` / `volar-lir-saved` | Mid-level IR shared with backend consumers in `volar` |
| `volar-ir` (`region.rs`, `gadget.rs`) | Input/output wire regions + gadget spec/binding types (companion metadata) |
| `volar-ir-passes` (`apply_gadgets.rs`) | Gadget application: splice gadgets onto region-tagged boundary wires |
| `volar-ir-text` (`regions.rs`) | Text sections for the region/gadget side tables |
| `volar-ir` (`typed_gadget.rs`) | Typed region/gadget authoring layer (typed anchors, `VCircuit` gadget bodies, validation) |
| `volar-ir-passes` (`region_lowering.rs`) | Typed→bit table/library lowering; region-table threading across movfuscation, unrolling, and reversible lowering |
| `volar-ir-text` (`typed_regions.rs`) | Text sections for the typed tables (spec stubs, not bodies) |
| `volar-vaffle-target` (`vaffle_regions.rs`) | VAFFLE-level table validation + lowering onto the Volar-IR level |

## Related documents

| Topic | File |
|---|---|
| Low-level circuit IRs (Volar IR, Boolar IR) in depth | [`ir-lowering.md`](ir-lowering.md) |
| LIR target trait | [`lir.md`](lir.md), [`lir-abi.md`](lir-abi.md) |
| Virtualization | [`virt.md`](virt.md) |
| Provenance | [`provenance.md`](provenance.md) |
| VAFFLE lowering detail | [`waffle-lowering.md`](waffle-lowering.md) |
| Verifiable Compute (opt-in WAFFLE tagging) | [`waffle-lowering.md`](waffle-lowering.md#verifiable-compute-opt-in) |
| Fuzzing | [`fuzzing.md`](fuzzing.md) |
| Wire regions & gadgets (plan) | [`wire-regions-gadgets-plan.md`](wire-regions-gadgets-plan.md) |
| Typed gadgets, higher-level IR gadgets, region threading through passes (plan) | [`typed-gadgets-and-region-threading-plan.md`](typed-gadgets-and-region-threading-plan.md) |
| Full pipeline (weaving, backends, proving) | `volar` repo's `docs/pipeline.md` |

## Retro CPU Boolar circuits (fixtures)

Full-CPU single-step Boolar circuits for retro CPUs (currently the WDC 65C02
and Z80) are generated as rkyv-serialized `BIrBlocks` fixture files by the
[`retrop`](../../../retrop) repository's `retrop-emit-volar` generator; no
generated code is vendored in this repository. Each fixture is one oblivious
single-block circuit per CPU step (state bits as entry params, RAM as
bit-granular storage with indicator-guarded writes, XOR one-hot state
selection across patterns). Fixtures deserialize with the volar-ir `rkyv`
feature and can be lowered via `movfuscate`/`lower_to_circuit` or evaluated
concretely with `volar_fuzz::interpreter::biir::eval_biir`. Equivalence tests
against retrop's decoder + semantics interpreter live in retrop.
