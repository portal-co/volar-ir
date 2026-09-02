# LLVM cross-block STACK spill → Boolar

**Status: resolved.** `alloca` and `switch` import landed first (see
`llvm-alloca.md`); movfuscate accepts `JumpTable` indices typed as
`Vec(PACK_W, Bit)` (see `bit_width_for_eq` in `movfuscate.rs`). This doc's
own original diagnosis of the LLVM→VAFFLE→movfuscate→Boolar blocker was
**wrong** — re-triaging it surfaced a different, much more fundamental bug,
fixed below.

## What this doc originally claimed (wrong)

> `lower_vaffle_to_ir` spills cross-block values at block entry via
> `frame_spill(..., BIT_TID)` even when the SSA value is a multi-bit packed
> word... Entry/reload for `cross_block_values` does not [pack].

This diagnosis doesn't hold up: `lower_to_ir.rs`'s own `compute_cross_block_values`/`frame_spill`/`frame_reload` mechanism (the thing this doc blamed)
**never fires** for the `poll_fsm` fixture — its spill trace is empty. That
mechanism was already superseded by `vaffle_ssa.rs`'s dominator-verified
spill/reload pass (`StorageId::VAFFLE_SSA_SPILL`), which runs *before*
`lower_to_ir.rs` even sees the module and already computes each spilled
value's real width correctly via `vaffle_value_vtid` (not a hardcoded
`BIT_TID`). All 97 of its spill/reload ops for `poll_fsm` were internally
consistent (1-bit value, 1-bit declared type) — no width mismatch there at
all.

## Actual root cause

`crates/ir/volar-vaffle-target/src/lower_to_ir.rs`'s `plan_functions`
(`LowerCtx::plan_functions`) computed a function's entry-block parameter
bit-count as:

```rust
let n_params = sig.params.len();
```

`sig.params: Vec<TypeId>` is one entry **per logical parameter** (e.g. `[i8,
i32]` → length 2 for `poll_fsm(i8, i32)`), not its bit width. `n_params` then
sized `n_param_words = n_packs(n_params)` and fed `unpack_words(&mut em,
&param_word_ids, n_params, PACK_W)` when materializing the entry block's
params — both of which need the **total bit width** (40 for `poll_fsm`), not
the parameter *count* (2). Every parameter bit past index `n_params` (i.e.
past index 2) was silently left out of the entry block's `val_map`.

Every later reference to one of those un-mapped bits — including
`vaffle_ssa.rs`'s spill writes for `%acc`'s bits (indices 8–39, since
`%acc`'s bits are the ones actually used cross-block) — hit
`translate_stmt`'s documented fallback: *"such a cross-block reference
silently resolves to `IRVarId(0)` ... instead of the real value."*
`IRVarId(0)` in the entry block happens to be the packed **SP** word
(`PACK_TID`, 64 bits) — hence the observed panic, `StorageWrite source width
mismatch: 64 bits vs type width 1` (a 1-bit-typed write silently fed the
64-bit SP word as its source).

**This bug predates and is unrelated to `alloca`/`switch` import.** It
affects *every* function whose parameters total more than 1 bit — i.e.
virtually all of them — and was never caught because every prior LLVM
frontend test (`llvm_register_xor_unrolls`, `simple_add`, etc.) only checked
circuit *shape* (`is_circuit()` / block counts), never the *computed value*.
Confirmed directly: before the fix, `xor_one(5)` (`%y = xor i32 %x, 1`)
evaluated (via `volar_fuzz::interpreter::ir::eval_ir`) to `0` instead of `4`
— a silent wrong-answer bug, not merely a panic.

**Fix** (`lower_to_ir.rs`, `plan_functions`): compute `n_params` as the sum
of each parameter type's bit width, exactly mirroring how `total_ret_bits`
was already computed a few lines below:

```rust
let n_params: usize = sig.params.iter()
    .map(|&vtid| ir_type_bit_width(&self.types, self.type_map[vtid.0 as usize]))
    .sum();
```

Fixing this alone resolves both this doc's original item 1 (`StorageWrite`
width mismatch in `lower_to_boolar`) and item 2 (`fuse` round-trip) with no
further changes needed to `frame_spill`/`frame_reload`/`FrameLayout` — those
were never the problem.

## Verified

- `llvm_switch_movfuscated_lowers_to_boolar_and_fuses` (was
  `llvm_switch_movfuscated_lower_to_boolar_blocked`,
  `volar-ir-build/tests/llvm_frontends.rs`): `poll_fsm` now lowers to Boolar
  and round-trips through `fuse` without panicking.
- `llvm_multi_bit_params_compute_correct_value` (same file): regression test
  pinning the actual bug — evaluates `xor_one(5)` via
  `volar_fuzz::interpreter::ir::eval_ir` and asserts the result is `4`, not
  a silently-wrong value. `volar-fuzz` is now a dev-dependency of
  `volar-ir-build` for this.
- Full `volar-ir-build`, `volar-vaffle-target`, `vaffle`, `volar-ir-opt`,
  `volar-ir-passes`, `volar-llvm-vaffle-import`, `volar-c-backend`,
  `volar-wasm-backend`, `volar-llvm-backend` suites pass with no
  regressions.

## Not this task

`import_module_inlined` + `alloca` (see `llvm-alloca.md` — a separate,
still-unverified `inline_vaffle.rs` stack-slot rebase concern). Symbolic GEP
off stack pointers. Identity `WebProofBackend::verify`. Full SLH-DSA verify
(thousands of hashes + 43 symbolic `br i1`s — separate cost/CF question).
Site `llvm-loop` (`site/crates/proofs/src/llvm_loop.rs`) reaching fused
`BCircuit` + Cirrus VOLE smoke (`llvm_looped_circuit`) — different repo, not
touched here, but should now be unblocked by this fix; worth re-running.
A separate, pre-existing, apparently unrelated fuzz-property failure,
`volar-fuzz`'s `properties::ir_passes::prop_d2_lower_ir_storage_roundtrip_preserves_semantics`,
was noticed during regression testing — confirmed to fail identically
without this change too, so it's a distinct, already-existing issue, not
touched here.

## Where

`crates/ir/volar-vaffle-target/src/lower_to_ir.rs`, `LowerCtx::plan_functions`
(the `n_params` fix). Tests:
`llvm_switch_movfuscated_lowers_to_boolar_and_fuses`,
`llvm_multi_bit_params_compute_correct_value`
(`volar-ir-build/tests/llvm_frontends.rs`).
