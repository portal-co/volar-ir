# LLVM `alloca` → VAFFLE stack

**Status: landed** for constant-size, scalar-integer `alloca` + load/store +
single-constant-index `getelementptr`, through the call-preserving
`import_module`/`Pipeline::from_llvm` path. See "Not this task" below for
what's still deliberately out of scope.

LLVM structural import (`volar-llvm-vaffle-import` / `Pipeline::from_llvm`)
used to hard-error on `alloca`. VAFFLE already has the stack ops. Rustc
guests (SLH-DSA verify, debug spills, many `async` frames) couldn't enter
LLVM→VAFFLE until this landed.

LLVM-direct (`volar-llvm-ir-import`, which wraps the sibling `llvm-bridge`
repo's `volar-llvm-import-core`) already executes concrete-count `alloca`
into a region (`max_alloca_bytes`, default 1 MiB, in `LoweringLimits`).
That path still rejects symbolic branches, so it is not a substitute for
`movfuscate`.

**What landed:** constant-size, scalar-integer-element `alloca` →
`Value::StackAlloc` with a per-function bump allocator (bit-granular,
matching `VaffleTarget`'s own `next_stack_slot`/`PTR_BITS` convention —
`StorageId::STACK`, compile-time `base_slot`). Load/store through the
tracked pointer, or through a single constant-index `getelementptr` off it,
become `Value::PtrLoad`/`PtrStore`/`PtrOffset` plus a per-bit
`StorageRead`/`StorageWrite`. A symbolic alloca count (VLA) is a named
`ImportError::Unsupported("alloca count is symbolic")`, not a panic. Global
load/store still goes through the separate, unrelated
`volar_llvm_constchain` path (per-global `StorageId`, byte-granular) — the
two are mutually exclusive by construction, so mixing a stack and a global
pointer at a load/store site fails closed via whichever path's own named
error fires (no special "mixing" error class was needed).

Implementation: `volar-llvm-vaffle-import`'s
`translate_instruction`/`FuncCtx` (`Alloca`, `Load`, `Store`,
`GetElementPtr` arms; `stack_load`/`stack_store`/`stack_addr_bits`
helpers). Tests: `alloca_spill_imports`,
`alloca_symbolic_count_is_named_unsupported`,
`stack_pointer_param_is_not_mistaken_for_alloca`,
`alloca_constant_gep_two_elements` (`volar-llvm-vaffle-import/tests/basic.rs`);
`llvm_alloca_spill_round_trips`,
`llvm_alloca_symbolic_count_is_named_unsupported`
(`volar-ir-build/tests/llvm_frontends.rs`, exercising the full
`lower_to_volar_ir()` + `unroll_ir()` + `is_circuit()` chain).

## Not this task

- **Symbolic/dynamic index into a `getelementptr` off a stack pointer.**
  Only a single constant-index GEP is supported. VAFFLE's `PtrOffset` +
  bit-circuit `bc_add` *could* support a symbolic index later, since STACK
  addressing is runtime bit arithmetic (unlike a global, which needs a
  compile-time-resolved identity) — a natural, cheap follow-up.
- **Non-integer alloca element type** (structs, arrays, floats) and
  **multi-index GEP** into a stack pointer — named errors. Next site
  rustc-guest handoff: [`llvm-array-alloca.md`](llvm-array-alloca.md).
- **phi/select of a pointer-typed SSA value** (merging two distinct
  pointers — e.g. two different allocas, or an alloca and a global — at a
  control-flow join). Falls through to `value_bits`'s existing
  `Unsupported` fallback.
- **`import_module_inlined` / `Pipeline::from_llvm_inlined` combined with
  `alloca` is unverified and likely broken** — do not rely on it yet.
  `inline_vaffle.rs`'s callee-splice rebases `Value::StackAlloc.base_slot`
  (`remap_callee_node`), but the pointer's *actual* address bits are
  separate, literal `Stmt::Const` nodes baked at VAFFLE-construction time
  from the pre-rebase `base_slot` — the generic per-`ValueId` shift in
  `remap_callee_node` does not (and structurally cannot) patch a `Const`'s
  embedded literal value. This looks like a latent mismatch between
  `inline_vaffle`'s stack-slot bookkeeping and `VaffleTarget`'s actual
  addressing convention, predating this change and orthogonal to it. Only
  the call-preserving path (`import_module`/`Pipeline::from_llvm`) is
  covered by the tests above. Investigate and fix `inline_vaffle.rs`
  separately before routing a real multi-function, alloca-using guest
  through `import_module_inlined`.
- Heap `malloc`. Identity `WebProofBackend::verify`. Proving full SLH-DSA
  verify (hash count, not alloca). `switch` ingest is landed.

## Corrections to the previous version of this doc

- `llvm_vaffle_xor` (previously cited as "must keep passing") does not
  exist in this repo. The in-repo equivalent is
  `llvm_register_xor_unrolls` in
  `volar-ir-build/tests/llvm_frontends.rs`. A test named `llvm_vaffle_xor`
  does exist, but in the sibling `site` repo
  (`site/crates/proofs/src/llvm_vaffle.rs`), alongside its own
  `llvm_vaffle_alloca_is_named_unsupported` mirroring the `spill` fixture
  here — that repo's test still needs a matching flip, not done as part
  of this change (different repo).
- "Site `llvm` guests that only needed stack spills can use `from_llvm` +
  `movfuscate` like WASM" was misleading: WASM's own pipeline
  (`waffle_lower.rs`) never uses `StackAlloc`/`PtrLoad`/`PtrStore`/
  `PtrOffset`/`StorageId::STACK` at all — WASM locals are already SSA'd by
  its frontend, and its linear memory uses `StorageId::memory(idx)`, a
  different mechanism. Only the *pipeline composition* (`from_llvm` +
  `movfuscate`) is analogous to WASM's, not any code.
- The concrete-count/`max_alloca_bytes` logic attributed to
  `volar-llvm-ir-import` actually lives in the sibling `llvm-bridge` repo's
  `volar-llvm-import-core::translate_instruction` — nothing under
  `volar-ir/crates/frontends/volar-llvm-ir-import` implements it directly.

## Where

`volar-llvm-vaffle-import`'s `translate_instruction` (`Alloca`, `Load`,
`Store`, `GetElementPtr` arms), `FuncCtx::next_stack_slot`/`stack_slot_of`,
`stack_load`/`stack_store`/`stack_addr_bits`. Tests listed above.
