# LLVM reachable `unreachable` (rustc panic=abort)

**Status: landed.** rustc `-O0` debug overflow (and bounds checks) emit a
reachable panic block:

```llvm
  br i1 %overflow, label %panic, label %ok
ok:
  ret i32 %value
panic:
  call void @panic_const_add_overflow(...)
  unreachable
```

Dead `landingpad` skip ([`llvm-landingpad.md`](llvm-landingpad.md)) does not
apply: `%panic` is a `br` successor of entry. Overflow
`extractvalue` ([`llvm-overflow-extractvalue.md`](llvm-overflow-extractvalue.md))
lowers the `{ i32, i1 }` pair, and the direct `call; unreachable` pair lowers
to VAFFLE's existing `Terminator::ReturnCall`. No IR-wide `Unreachable`
variant is needed.

VAFFLE `Terminator` has `Return` / `Jump` / `ReturnCall` / `IfNonzero` /
`Table` only. `IRTerminator` is `Jmp` / `JumpCond` / `JumpTable` only.
Adding a new terminator variant would fan out through every exhaustive match
(movfuscate, unroll, Boolar, virt, text), so the importer does not add one.

## Measured (site, 2026-09-03)

Real rustc 1.96.0 `-C opt-level=0 -C panic=abort` of `x + 1`:

| Guest | Result |
|-------|--------|
| overflow + `extractvalue` 0, **no** panic edge | unroll **ok** (`llvm_vaffle_rustc_o0_overflow_extractvalue`) |
| same + reachable `call panic; unreachable` | `from_llvm` / structural lowering succeeds; symbolic unrolling remains fail-closed |
| wrapping `stack_spill` + memset | unroll **ok** |
| rustc `-O0` packed `{i32,i32}` as `i64` + memcpy + `gep i8, i64 4` | unroll **ok** (`llvm_vaffle_rustc_o0_pair_xor_memcpy`) |
| rustc `-O0` `if cond { a } else { b }` via alloca join | movfuscate + fuse **ok** (`llvm_vaffle_rustc_o0_pick_movfuscates`) |
| rustc `-O0` `xs[i]` bounds check | same `call panic_bounds_check; unreachable` shape |

Site canary: `llvm_vaffle_rustc_o0_overflow_panic_unreachable`.

`panic=unwind` replaces the abort `call` with a reachable `invoke` +
`landingpad`. That is **not** this task.

## Supported contract

The importer recognizes two direct-call tail positions before it emits a
`Value::Call`:

- `call; ret %call_result` (or `call void; ret void`) becomes `ReturnCall`
  when the callee has a body. This is generic tail-call optimization: no call
  result or continuation is materialized, and the callee reuses the caller's
  continuation.
- `call; unreachable` becomes `ReturnCall` even for a declaration-only
  callee. This covers rustc's aborting panic call without pretending that the
  panic block returns normally.

The subsequent LLVM `ret` or `unreachable` is skipped because the
`ReturnCall` already terminates the VAFFLE block. A reachable `unreachable`
without that immediately preceding direct call remains the named error
`reachable unreachable`.

The overflow branch itself is retained. Therefore import and structural
lowering succeed for a rustc `panic=abort` fixture, but `unroll_ir` remains
fail-closed when the overflow condition is symbolic. Wrapping behavior is a
separate opt-in policy (`-C overflow-checks=off`).

## Tests

- Structural tests cover generic `call; ret` conversion, aborting
  `call; unreachable` conversion, and a standalone reachable
  `unreachable` rejection.
- End-to-end coverage evaluates an internal tail call (`caller(5) = 6`) and
  verifies that an overflow panic fixture imports and lowers but does not
  statically unroll under symbolic control flow.

Run:

```sh
cargo test -p volar-llvm-vaffle-import --test basic
cargo test -p volar-ir-build --features llvm --test llvm_frontends
```

Site canary `llvm_vaffle_rustc_o0_overflow_panic_unreachable` now expects
import + unroll fail-closed. `movfuscate` of that fixture is
[`llvm-import-returncall.md`](llvm-import-returncall.md).

## Not this task

- `invoke` / unwind / reachable `landingpad`.
- Adding `Terminator::Unreachable` / `IRTerminator::Unreachable`; standalone
  reachable `unreachable` remains a named unsupported construct.
- Constraining the overflow bit to 0 and deleting `%panic` (opt-in wrap).
- Symbolic GEP / `xs[i]` as a circuit (bounds-check unreachable is the
  same terminator; the GEP index is a separate leftover).
- Struct alloca, pointer `phi`/`select`, identity
  `WebProofBackend::verify`, SLH-DSA verify.

## Where

`crates/frontends/volar-llvm-vaffle-import/src/lib.rs` detects tail call
shapes in its `Call` arm and recognizes standalone `Unreachable` in
`translate_terminator`. Coverage is in
`crates/frontends/volar-llvm-vaffle-import/tests/basic.rs` and
`crates/frontends/volar-ir-build/tests/llvm_frontends.rs`.
