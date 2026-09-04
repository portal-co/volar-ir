# LLVM `noalias.scope.decl` / metadata-typed calls

**Status: landed.** rustc `-C opt-level=1` (and often `-O0`) emits

```llvm
declare void @llvm.experimental.noalias.scope.decl(metadata)
call void @llvm.experimental.noalias.scope.decl(metadata !12)
```

The importer skips this no-op alias-analysis hint before generic call-argument
handling. It also skips `llvm.lifetime.start.*`, `llvm.lifetime.end.*`,
`llvm.dbg.*`, `llvm.assume`, `llvm.donothing`, and `llvm.sideeffect`; none
becomes a `Value::Call` or a called-function worklist entry.

## Metadata safety

inkwell's `InstructionValue::get_operand` eagerly constructs a
`BasicValueEnum`, which panics for LLVM metadata. `call_value_operand` now
checks the raw operand type first. A metadata operand on a non-skipped call
returns a named `ImportError::Unsupported` instead. The check runs before
`func_id` reads the callee signature, because inkwell's
`FunctionValue::get_params` has the same BasicValue-only assumption.

This is semantic no-op handling only; the importer does not interpret
noalias/lifetime metadata for alias analysis.

## Tests

Importer fixture:

```llvm
define i32 @xor_one(i32 %x) {
entry:
  call void @llvm.experimental.noalias.scope.decl(metadata !0)
  %y = xor i32 %x, 1
  ret i32 %y
}
declare void @llvm.experimental.noalias.scope.decl(metadata)
!0 = !{!0}
```

`import_module(..., ["xor_one"])` succeeds without a panic or a
`Value::Call`; the fixture includes `llvm.lifetime.start/end` on an alloca so
lifetime skipping cannot regress stack tracking. A second fixture wraps a
non-skipped metadata-parameter call in `catch_unwind` and asserts that the
result is a named metadata error, not an inkwell panic.

Run:

```sh
cargo test -p volar-llvm-vaffle-import --test basic
cargo test -p volar-ir-build --features llvm --test llvm_frontends
```

## Still out of scope

- Interpreting noalias / lifetime for alias analysis (skip is enough).
- `invoke` / unwind.
- Identity `WebProofBackend::verify`.

## Where

`InstructionOpcode::Call` and `call_value_operand` in
`crates/frontends/volar-llvm-vaffle-import/src/lib.rs`.
