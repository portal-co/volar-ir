# volar-ir Agent Context

## Project Mission

This repo is the reusable IR/circuit-compilation layer split out of
[`volar`](https://github.com/portal-co/volar): Volar IR, VAFFLE, their
lowerings (VAFFLE→Volar IR, movfuscation, virtualization, circuit lowering),
and their optimization passes (DCE, CSE, constant folding, store-forwarding).
It's generic compiler engineering — SSA IRs, CFG transforms, peephole
optimizations — that happens to be designed to eventually feed a
cryptographic proving/garbling backend, but has no cryptography of its own.

Treat this as an ordinary compiler-IR codebase: no ZK/non-ZK discipline
boundary, no `.insecure` quarantine convention, no pinnedness/stability
tagging — those are `volar`'s conventions for its cryptographic spec layer,
not this repo's.

## Crate Constraints

| Crate | `std` | Notes |
|---|---|---|
| `volar-ir-opt` | `#![no_std]` + `extern crate alloc` | Use `alloc::vec`, `alloc::vec::Vec`, `alloc::collections::BTreeMap` |
| `volar-fuzz` | `std` | Full standard library available |

## Design Rules

1. **Catch-all arms**: Use `_ =>` catch-alls on IR type matches to support
   parallel development across passes.
2. **Never specialize on test cases**: Extend tests instead of special-casing
   pass behavior to make a specific test pass.
3. **Semantics-preservation is the correctness signal for passes.** DCE, CSE,
   constant folding, movfuscation, and circuit lowering must be tested by
   evaluating IR before and after a pass and comparing results (see
   `crates/fuzz/volar-fuzz` and `docs/fuzzing.md`'s Properties A–D) — not by
   asserting on IR shape/statement counts unless verifying a specific
   structural invariant (e.g. "output is movfuscated").

## Topic Context Files

| Topic | File | When to load |
|---|---|---|
| Pipeline overview | `docs/pipeline.md` | Always — first thing before editing |
| IR types, storage, Poly semantics | `docs/agent-context/ir-types-storage.md` | Working on IR, lowering, evaluators, store-forward, fuzzer generators |
| IR `map`/`as_ref`/`as_mut` conventions | `docs/agent-context/ir-map-conventions.md` | Adding IR variants, writing IR transformations |
| Boolar IR pass conflicts | `docs/agent-context/boolar-ir-conflicts.md` | Touching more than one Boolar IR pass in the same change |
| Provenance pipeline | `docs/agent-context/provenance-pipeline.md` | Adding provenance to passes, writing `ProvenanceHandler` impls |
| Virtualization adaptive-split ADR | `docs/agent-context/virt-adaptive-split-adr.md` | Working on `volar-ir-virt` dispatch modes |
| Circuit-size optimization backlog | `docs/agent-context/circuit-size-optimization-backlog.md` | Working on `volar-ir-opt` or circuit lowering with an eye on output size |
| Complexity hints | `docs/agent-context/complexity-hints.md` | Estimating pass cost or circuit size |
| `u128` support in LIR (deferred) | `docs/agent-context/lir-u128-support.md` | Touching `LirType`/`primitive_to_lir` for wide integers |
| Side tagging | `docs/agent-context/side.md`, `docs/side.md` | Working on multi-actor/multi-party value tagging |
| WASM feature coverage | `docs/wasm-feature-support.md` | Extending `volar-vaffle-target`'s WASM operator support |
| LLVM `alloca` import | `docs/llvm-alloca.md` | Extending `volar-llvm-vaffle-import` `alloca` / stack `load`/`store`/`gep` |
| LLVM array/struct `alloca` | `docs/llvm-array-alloca.md` | rustc `-O0` / SLH-DSA `[N x i8]` and aggregate stack frames |
| LLVM STACK spill → Boolar | `docs/llvm-stack-spill-boolar.md` | `lower_vaffle_to_ir` cross-block spill vs `lower_ir_to_boolar` width |
| Cross-function call numeric correctness | `docs/llvm-cross-function-calls.md` | `unroll_ir`/`movfuscate` on a non-inlined multi-function call; `lower_to_ir.rs`'s calling convention |
| LLVM memcpy from a constant global | `docs/llvm-memcpy-global.md` | `llvm.memcpy`/`memset` of `@global` or untracked slice pointers (HMAC IV) |
| LLVM `noalias.scope.decl` | `docs/llvm-noalias-scope-decl.md` | inkwell ICE on metadata-typed `llvm.experimental.noalias.scope.decl` (rustc guests) |
| LLVM constant `null` pointer | `docs/llvm-const-null.md` | `icmp eq ptr %p, null` / `phi` of `null` |
| LLVM memcpy through a symbolic stack GEP | `docs/llvm-memcpy-symbolic-stack.md` | constant-length `llvm.memcpy` of `gep i8, ptr %alloca, i64 %i` (SHA block buffer) |
| LLVM `load ptr` | `docs/llvm-load-ptr.md` | 64-bit `load ptr` from the LLVM data layout |
| LLVM symbolic `memset` length | `docs/llvm-memset-symbolic.md` | `llvm.memset` of `xor i64 %pos, 63` (SHA-256 finalize padding) |
| VAFFLE call `Output` bit index | `docs/llvm-call-output-idx.md` | landed: `Output` types as Bit, not `sig.results[idx]` |
| VAFFLE→IR `collect_uses` on HKDF | `docs/llvm-lower-collect-uses.md` | landed: incremental `future_uses` counts, not a per-call suffix walk |
| Boolar `lower_poly_bit` on HKDF | `docs/llvm-boolar-poly.md` | landed: hash-cons AND/XOR in `lower_poly_bit` |
| Movfuscated step circuit on HKDF | `docs/llvm-fuse-unroll.md` | typed one-step boundary; external driver owns iteration |
| Movfuscate `Poly` on HKDF | `docs/llvm-movfuscate-poly.md` | landed: owned `subst_ir` steals monomial `Vec`s |
| VAFFLE→IR `val_map` on HKDF | `docs/llvm-lower-val-map.md` | landed: dense `ValueMap` |
| Movfuscate `subst_ir_owned` Poly | `docs/llvm-movfuscate-subst-poly.md` | landed: `PolyCoeffs` flat `Vec` |
| Movfuscate remap on HKDF | `docs/llvm-movfuscate-remap.md` | accepted ~32s `remap_monomials_in_place` |
| Text format | `docs/text-format-spec.md` | Working on `volar-lir-text`/`volar-ir-text` |

## Compression-aware logging

Token compression proxies can sit between this tool and an LLM provider,
compressing output before it reaches the model. When a proxy is active, MORE
verbose structured output is net-cheaper than terse plaintext.

| Variable | Effect |
|---|---|
| `PORTAL_LOG_JSON=1` | Structured NDJSON output; routes `log::` calls through the sink when subscriber is installed. |
| `PORTAL_LOG_BATCH=1` | Group events by phase into single JSON arrays. |

These variables have no effect when unset and do not change program correctness.
