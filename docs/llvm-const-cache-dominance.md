# LLVM constant cache vs VAFFLE dominance

**Status: landed.** rustc `-O0` `Option` / enum layouts can store the same
integer literal in sibling blocks:

```llvm
bb2:
  store i32 1, ptr %p
  br label %bb3
bb1:
  store i32 1, ptr %p
  br label %bb3
```

The structural importer materializes each immediate integer in the LLVM block
that uses it. Sibling blocks therefore receive distinct VAFFLE `Stmt::Const`
values, and `lower_to_volar_ir` preserves VAFFLE dominance rather than
panicking in `vaffle_ssa`.

## Resolution

`Importer::value_bits` recognizes a constant `IntValue` before consulting
`FuncCtx::cache`, then calls `int_const_bits` directly. Each use emits fresh
bit constants through `bc_const_at(..., fctx.current, ...)`.

The function-wide cache remains for parameters, phis, and instruction
results. LLVM SSA requires those definitions to dominate every use, unlike a
constant first materialized lazily in a control-flow arm.

## Root cause

Previously, `value_bits` cached LLVM `ConstantInt` bits in `FuncCtx::cache`,
keyed only by `AnyValueEnum`. The `Stmt::Const` nodes were emitted into
whichever block first used the literal. A later sibling block reused those
`ValueId`s, but sibling diamond arms do not dominate one another:

```text
vaffle_ssa: value 229 (owned by block 1) is used at block 2
but block 1 does not dominate block 2
```

`bc_const_at` itself always emitted a fresh Const; the leak was the
function-wide cache hit for non-instruction constants.

## Measured (site, 2026-09-04)

Real rustc 1.96.0 `-C opt-level=0 -C panic=abort` of
`if flag { Some(a) } else { Some(b) }`:

| Guest | Result |
|-------|--------|
| `pick` (parameter stores, no shared literal) | movfuscate + fuse **ok** |
| `opt_join` (`store i32 1` in both arms) | import → lower → movfuscate → fuse **ok** |
| rustc `-O0` debug `stack_spill` (`x + 1`) | movfuscate + fuse **ok** |
| rustc `-O0` `xs[i]` pointer-param GEP | named ConstChain (`llvm_vaffle_rustc_o0_slice_get`) |

The site canary `llvm_vaffle_rustc_o0_option_join` can add lowering,
movfuscation, fuse, and value checks as follow-up work outside this repository.

## Tests

- `const_literal_in_sibling_blocks_is_not_shared` verifies structurally that
  each sibling has its own `Stmt::Const(1)` `ValueId`.
- `llvm_const_literal_in_sibling_blocks_lowers_movfuscates_and_fuses` takes
  the `opt_join` fixture through `from_llvm` → `lower_to_volar_ir` →
  `movfuscate` → Boolar → `fuse`, then evaluates both the original and
  movfuscated IR: `opt_join(true, 5, 9) = 4` and
  `opt_join(false, 5, 9) = 8`.
- Existing `pick`, `poll_fsm`, and overflow-panic tests remain green.

Run:

```sh
cargo test -p volar-llvm-vaffle-import --test basic
cargo test -p volar-ir-build --features llvm --test llvm_frontends
```

## Out of scope

- Symbolic GEP / pointer-param loads (`xs[i]`) — already named ConstChain.
- `invoke` / unwind.
- Constraining overflow bits.
- Identity `WebProofBackend::verify`, SLH-DSA verify.

## Where

`crates/frontends/volar-llvm-vaffle-import/src/lib.rs` handles the immediate
before its cache lookup. Coverage is in
`crates/frontends/volar-llvm-vaffle-import/tests/basic.rs` and
`crates/frontends/volar-ir-build/tests/llvm_frontends.rs`.
