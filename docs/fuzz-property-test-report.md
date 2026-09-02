# Fuzz Property Test Repair Report

## Scope

This change re-enables the three property tests that were marked `#[ignore]`
in `crates/fuzz/volar-fuzz/src/properties/ir_passes.rs`:

- `prop_k_lower_vaffle_to_ir_with_inlining_preserves_semantics`
- `prop_p_ir_multiblock_movfuscate_one_step_agrees`
- `prop_p_ir_diamond_movfuscate_one_step_agrees`

## Reproduction

Before the repair, the deterministic regression-backed command below failed
all three tests:

```text
cargo test -p volar-fuzz --lib -- --ignored --test-threads=1
```

The movfuscation properties produced a direct IR-step versus lowered-Boolar
step disagreement. The inlining property either disagreed or reached an IR
block with an inconsistent parameter count, depending on the generated
counterexample.

## Root causes and fixes

### Boolar polynomial lowering

`lower_ir_to_boolar` derived a `Poly` result width from its operand vectors
instead of the statement's declared type. A pure constant `Poly { ty: _8,
.. }` therefore emitted one Boolar wire rather than eight. The
movfuscation-plus-circuit-lowering path can synthesize this form.

The lowerer now expands every `Poly` to the bit width of its declared `ty`.
`pure_constant_poly_uses_its_declared_width` locks down the regression.

The same lowering also treated a one-bit selector as missing above bit zero,
rather than broadcasting it over a wide `Poly` result. This broke the
`is_active * value` expressions emitted by movfuscation. The lowerer now
broadcasts scalar operands and treats a missing lane of a non-scalar operand
as a zero contribution. `poly_broadcasts_bit_selectors_across_wider_results`
covers this behavior.

### Inlining property domain

The inlining property generated modules that returned `StorageWrite` results
or opaque `OracleCall` aggregates directly. The IR evaluator intentionally
models those as non-value handles, so their result ABI cannot be compared to
the VAFFLE value ABI. That made the property report a false inliner
parameter-count/semantic failure.

The property now excludes those documented non-value return shapes in every
body involved in the generated two-function module. Inlining remains tested
with non-empty, mixed-width data parameters and projected oracle outputs.

### Unsupported Boolar storage layouts

The Boolar storage model intentionally rejects a flat-cell address whose
element-address widths conflict within one `(StorageId, LaneId)` space, or
whose address plus bit suffix exceeds 64 bits. The fuzz properties now treat
both fail-closed diagnostics as non-representable inputs, matching the
existing handling for oversized flat-cell addresses.

## Verification

The following checks passed after the fix:

```text
cargo test -p volar-ir-passes
cargo test -p volar-fuzz --lib prop_k_lower_vaffle_to_ir_with_inlining_preserves_semantics -- --test-threads=1
cargo test -p volar-fuzz --lib prop_p_ir_ -- --test-threads=1
```

The first command includes both new focused regression tests; the latter two
run the formerly ignored properties at their normal proptest case counts.
