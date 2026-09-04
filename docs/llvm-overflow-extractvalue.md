# LLVM overflow intrinsics + `extractvalue`

**Status: landed.** The structural LLVM importer lowers rustc's fixed
two-field arithmetic-overflow aggregates before they can become ordinary
`Value::Call`s:

```llvm
%1 = call { i32, i1 } @llvm.sadd.with.overflow.i32(i32 %x, i32 1)
%value = extractvalue { i32, i1 } %1, 0
%overflow = extractvalue { i32, i1 } %1, 1
```

This covers the rustc `-O0` shape for checked scalar arithmetic without
adding a public aggregate type to VAFFLE.

## Supported contract

The importer recognizes direct calls named:

- `llvm.sadd.with.overflow.*` and `llvm.uadd.with.overflow.*`
- `llvm.ssub.with.overflow.*` and `llvm.usub.with.overflow.*`
- `llvm.smul.with.overflow.*` and `llvm.umul.with.overflow.*`

Each call must have two integer operands of the same width. It emits the
wrapping add, subtract, or multiply result as field 0 and a one-bit overflow
flag as field 1. Unsigned add and subtract use the usual unsigned comparisons;
unsigned multiply checks the widened high half. Signed add and subtract use
sign-bit formulas, while signed multiply compares the doubled-width product
with the sign-extended wrapping result.

The aggregate is importer-local metadata, keyed by the intrinsic call; it is
not a general VAFFLE value. `extractvalue` supports exactly one index, `0` or
`1`, from one of those tracked calls. Thus neither a residual intrinsic call
nor an imported declaration is created for supported operations.

## Tests

- Structural coverage imports all six families without a residual
  `Value::Call`, and verifies that `extractvalue` of an untracked aggregate is
  a named unsupported error.
- End-to-end coverage unrolls the result field of signed add (`add_one(5) =
  6`) and evaluates representative overflowing signed/unsigned add,
  subtract, and multiply boundaries.

Run:

```sh
cargo test -p volar-llvm-vaffle-import --test basic
cargo test -p volar-ir-build --features llvm --test llvm_frontends
```

## Out of scope

- The debug-overflow panic CFG. A branch on the overflow flag followed by a
  reachable panic call and `unreachable` is
  [`llvm-unreachable.md`](llvm-unreachable.md). Circuit guests that need
  wrapping behavior should use wrapping operations or
  `-C overflow-checks=off`.
- Real exception handling (`invoke` unwind destinations). Dead landingpad
  skip is covered separately in [`llvm-landingpad.md`](llvm-landingpad.md).
- General aggregates: `insertvalue`, nested aggregates, landingpad values,
  aggregate loads, and `extractvalue` from anything other than the tracked
  overflow intrinsic result.
- The external site canary update; that is follow-up work outside this
  repository. Site `llvm_vaffle_rustc_o0_overflow_extractvalue` now expects unroll.

## Where

`crates/frontends/volar-llvm-vaffle-import/src/lib.rs` handles the intrinsic
in its `InstructionOpcode::Call` arm and projects its fields in the restricted
`ExtractValue` arm. Coverage is in
`crates/frontends/volar-llvm-vaffle-import/tests/basic.rs` and
`crates/frontends/volar-ir-build/tests/llvm_frontends.rs`.
