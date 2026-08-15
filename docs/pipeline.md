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
 Boolean circuit
```

Consumers outside this repo (in `volar`) take the boolean circuit (or the
movfuscated Volar IR directly) into ZK proof weaving or garbled-circuit
weaving — see `volar`'s `docs/garbling-pipeline.md` and `docs/vole-weaving.md`.

## Crates

| Crate | Role |
|---|---|
| `vaffle` | VAFFLE module representation, mirrors `portal-pc-waffle-ir::Module` |
| `volar-vaffle-target` | Lowers VAFFLE (and WAFFLE, via `portal-pc-waffle-frontend`) into Volar IR |
| `volar-ir` | Volar IR and Boolar IR types |
| `volar-ir-passes` | `movfuscate.rs` (movfuscation), `lower_to_circuit.rs` (circuit lowering) |
| `volar-ir-opt` | DCE, CSE, constant folding, store-forwarding — across Volar IR, Boolar IR, and VAFFLE |
| `volar-ir-virt` | Virtualization (dispatch-mode block interpretation) |
| `volar-lir` / `volar-lir-text` / `volar-lir-saved` | Mid-level IR shared with backend consumers in `volar` |

## Related documents

| Topic | File |
|---|---|
| Low-level circuit IRs (Volar IR, Boolar IR) in depth | [`ir-lowering.md`](ir-lowering.md) |
| LIR target trait | [`lir.md`](lir.md), [`lir-abi.md`](lir-abi.md) |
| Virtualization | [`virt.md`](virt.md) |
| Provenance | [`provenance.md`](provenance.md) |
| VAFFLE lowering detail | [`waffle-lowering.md`](waffle-lowering.md) |
| Fuzzing | [`fuzzing.md`](fuzzing.md) |
| Full pipeline (weaving, backends, proving) | `volar` repo's `docs/pipeline.md` |
