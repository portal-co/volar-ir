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
     │  optional inline_vaffle_everything  (volar-ir-opt)
     │  replace function calls with block jumps  (volar-vaffle-target)
     ▼
 Volar IR   (SSA; field-level; protocol-agnostic)
    │  constant folding, DCE, CSE  (volar-ir-opt)
    │
    ├─ unroll_ir_everything (concrete CF) → single is_circuit() block
    │
    │  replace conditional blocks with real-vs-fake flags  (movfuscate.rs)
    ▼
 Movfuscated Volar IR  (looping; arbitrary CF)
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
| `volar-ir-passes` | `movfuscate.rs` (looping circuit, arbitrary CF), `unroll_ir.rs` (combinational circuit, concrete CF), `lower_to_circuit.rs` (budgeted MUX unroll of a movfuscated loop), `fuse_to_circuit.rs`, `to_reversible.rs` |
| `volar-ir-opt` | DCE, CSE, constant folding, store-forwarding, budgeted VAFFLE inlining, `inline_vaffle_everything` |
| `volar-ir-build` | Build-time pipeline: WASM / structural LLVM / LLVM-direct / VAFFLE / Volar IR / LIR sources; inline-everything; unroll vs movfuscate; terminate at Volar IR or LIR |
| `volar-llvm-vaffle-import` | Structural LLVM → VAFFLE (calls preserved). `import_module_inlined` composes with inline-everything |
| `volar-llvm-ir-import` | LLVM-direct: execution-mode import to a single `is_circuit()` block (concrete CF required) |
| `volar-wasm-circuit-import` | Control-free WASM → `VCircuit` (source-level call expansion; not the builder's VAFFLE path) |
| `volar-ir-virt` | Virtualization (dispatch-mode block interpretation) |
| `volar-lir` / `volar-lir-text` / `volar-lir-saved` | Mid-level IR shared with backend consumers in `volar` |
| `volar-circuit-source` | Named-wire walker + `CircuitSourceBackend` for textual circuit libraries |
| `volar-noir-backend` | Noir `type = "lib"` emission (bool + Volar IR); see [`circuit-source.md`](circuit-source.md) |
| `volar-pod2-backend` | Podlang module emission (bool + Merkle array/dict storage) |
| `volar-ir` (`region.rs`, `gadget.rs`) | Input/output wire regions + gadget spec/binding types (companion metadata) |
| `volar-ir-passes` (`apply_gadgets.rs`) | Gadget application: splice gadgets onto region-tagged boundary wires |
| `volar-ir-text` (`regions.rs`) | Text sections for the region/gadget side tables |
| `volar-ir` (`typed_gadget.rs`) | Typed region/gadget authoring layer (typed anchors, `VCircuit` gadget bodies, validation) |
| `volar-ir-passes` (`region_lowering.rs`) | Typed→bit table/library lowering; region-table threading across movfuscation, unrolling, and reversible lowering |
| `volar-ir-text` (`typed_regions.rs`) | Text sections for the typed tables (spec stubs, not bodies) |
| `volar-vaffle-target` (`vaffle_regions.rs`) | VAFFLE-level table validation + lowering onto the Volar-IR level |

## Circuit-shape strategies and frontends

Three ways to get a single-block circuit, with different control-flow contracts:

| Transform | Input CF | Result | When it fails |
|---|---|---|---|
| `unroll_ir_everything` | Concrete (every branch/switch/address folds along the walked path; finite) | One **non-looped** block (`is_circuit()`, `Jmp Return`) | Symbolic branch/switch/address, non-finite loop, resource cap |
| `movfuscate_ir` | Arbitrary, including symbolic | One **self-looping** block (`is_movfuscated()`) | Does not fail closed on symbolic CF |
| `lower_to_circuit` | Already-movfuscated self-loop | Budgeted MUX-unroll of that loop | Trip may exceed the budget; the circuit still exists but may not have terminated |

Both `unroll_ir_everything` and `movfuscate_ir` now correctly handle a real,
non-inlined cross-function `Value::Call`/`Terminator::ReturnCall` — see
[`llvm-cross-function-calls.md`](llvm-cross-function-calls.md) for the four
independent calling-convention bugs this uncovered, none of which any
prior structural-only or inlined-path test had exercised.

`unroll_ir_everything` and `movfuscate_ir` are mutually alternative shape strategies on Volar IR. Folding may run before either. Do not use `lower_to_circuit` as a substitute for unroll-everything: it assumes a symbolic-PC movfuscated loop.

Frontend routes that feed those passes:

- **WASM (call-preserving):** `Pipeline::from_wasm` → WAFFLE → VAFFLE (`lower_waffle_module`). Calls stay as `Value::Call` until a later pass.
- **WASM (fully inlined):** `from_wasm_inlined` = the above plus `inline_vaffle_everything`, then `lower_vaffle_to_ir`, then optional `unroll_ir` or `movfuscate`. Distinct from `volar-wasm-circuit-import` (control-free `VCircuit`, source-level call expansion).
- **LLVM structural:** `volar-llvm-vaffle-import::import_module` preserves ordinary calls. `import_module_inlined` / `from_llvm_inlined` compose inline-everything on top. Then unroll (concrete CF) or movfuscate (symbolic CF). Constant-size scalar and array-of-integer `alloca`, typed-view GEP, and `switch` import are landed, including a fully general fix for the alloca/calling-convention frame collision (`StorageId::ALLOCA`, rebased onto the real runtime frame in `lower_to_ir.rs`) ([`llvm-alloca.md`](llvm-alloca.md), [`llvm-array-alloca.md`](llvm-array-alloca.md)); lowering through movfuscate to Boolar now round-trips too ([`llvm-stack-spill-boolar.md`](llvm-stack-spill-boolar.md)). Direct constant-size, nonvolatile `llvm.memset` / `llvm.memcpy` / `llvm.memmove` calls lower to storage operations; a symbolic `llvm.memcpy` becomes a VAFFLE CFG byte loop for movfuscation's step circuit, while symbolic `memset` / `memmove` remain fail-closed. Dead `landingpad` blocks are skipped by entry-CFG reachability, `llvm.*.with.overflow` aggregates lower to wrapping results plus overflow bits projected by `extractvalue`, direct `call; ret` / `call; unreachable` shapes lower to `ReturnCall`, and immediate integers and `null` pointers are materialized per use block so sibling arms remain dominance-safe ([`llvm-memset.md`](llvm-memset.md), [`llvm-landingpad.md`](llvm-landingpad.md), [`llvm-overflow-extractvalue.md`](llvm-overflow-extractvalue.md), [`llvm-unreachable.md`](llvm-unreachable.md), [`llvm-const-cache-dominance.md`](llvm-const-cache-dominance.md), [`llvm-const-null.md`](llvm-const-null.md)). Calls to declaration-only Imports take a typed abort sink, which returns zero-valued entry results rather than jumping out of range ([`llvm-import-returncall.md`](llvm-import-returncall.md)). Live exception handling, isolated reachable `unreachable`, unsupported symbolic memory operations, and general aggregate values remain fail-closed.
- **LLVM-direct:** `volar-llvm-ir-import` / `from_llvm_direct` is a specialized interpreter that already emits `is_circuit()` when control flow is concrete. It is not rewritten on top of unroll-everything. Structural + inline + unroll should agree with direct when both succeed; when CF is data-dependent, direct and unroll fail and structural + movfuscate still works.
- **LLVM LTO archive:** `from_llvm` / `from_llvm_direct` accept a static library (`.a`) of clang **full-LTO** bitcode members (raw `.bc` or ELF `.llvmbc` / Mach-O `__LLVM,__bitcode`). Members are `link_in_module`'d; native-only objects and ThinLTO-only summaries fail closed. GCC LTO is not LLVM bitcode.
- **LLVM LTO pre-build:** `from_cc` / `from_cc_inlined` / `from_cc_direct` (`cc` feature) run `cc::Build` with `-flto=full` at execute time and import the resulting `lib{name}.a`. `from_command` / `from_command_inlined` / `from_command_direct` run a user-specified command (no shell) that must write that archive.

`volar-ir-build` is the std builder for these sources and passes; it terminates at VAFFLE, Volar IR, or LIR. Object emit, weaving, and `cargo:rerun-if-changed` stay in `volar`'s `volar-build`, which thin-wraps this crate.

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
| LLVM `alloca` → VAFFLE stack | [`llvm-alloca.md`](llvm-alloca.md) |
| LLVM array/struct `alloca` | [`llvm-array-alloca.md`](llvm-array-alloca.md) |
| LLVM STACK spill → Boolar | [`llvm-stack-spill-boolar.md`](llvm-stack-spill-boolar.md) |
| Cross-function call numeric correctness | [`llvm-cross-function-calls.md`](llvm-cross-function-calls.md) |
| LLVM `memset` / `memcpy` / `memmove` | [`llvm-memset.md`](llvm-memset.md) |
| LLVM dead `landingpad` (rustc `-O0`) | [`llvm-landingpad.md`](llvm-landingpad.md) |
| LLVM overflow + `extractvalue` | [`llvm-overflow-extractvalue.md`](llvm-overflow-extractvalue.md) |
| LLVM reachable `unreachable` (rustc panic=abort) | [`llvm-unreachable.md`](llvm-unreachable.md) |
| LLVM `ReturnCall` to Import | [`llvm-import-returncall.md`](llvm-import-returncall.md) |
| LLVM `ConstantInt` cache vs dominance | [`llvm-const-cache-dominance.md`](llvm-const-cache-dominance.md) |
| LLVM pointer-param runtime dispatch | [`llvm-ptr-runtime-dispatch.md`](llvm-ptr-runtime-dispatch.md) |
| LLVM memcpy from a constant global | [`llvm-memcpy-global.md`](llvm-memcpy-global.md) |
| LLVM `noalias.scope.decl` metadata calls | [`llvm-noalias-scope-decl.md`](llvm-noalias-scope-decl.md) |
| LLVM constant `null` pointer | [`llvm-const-null.md`](llvm-const-null.md) |
| Full pipeline (weaving, backends, proving) | `volar` repo's `docs/pipeline.md` |
| Textual circuit libraries (Noir, POD2) | [`circuit-source.md`](circuit-source.md) |

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
