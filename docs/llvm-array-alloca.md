# LLVM array/struct `alloca` → VAFFLE stack

**Status: landed** for constant-size alloca of an array of integers (including
`[N x i8]` as a byte blob), plus a single-index, differently-typed constant
`getelementptr` used as a "typed view" into it (the rustc `-O0` `stack_spill`
shape). Struct allocas are a named error (the "or names a clear error" branch
of item 3 below — not implemented, deliberately). The `StorageId::STACK`
address collision this uncovered (below) is now fixed in general, not just
worked around.

## Re-triage: the real blocker was not array support

Re-checking this doc's own "Do" items against the code found that item 1
(array alloca) was a reasonably small, contained addition — but building the
end-to-end numeric test item 2 asks for (`stack_spill` evaluates
`x ^ (x+1)`, not just imports/unrolls) surfaced a **much more severe,
pre-existing bug that predates this doc entirely**: every stack `alloca`
landed by `llvm-alloca.md` — including the *original, simplest possible*
`spill(i32 %x)` fixture already committed and passing — was silently
computing the **wrong value**, and had been since that work landed. Nothing
caught it because every existing alloca test checked circuit *shape*
(`is_circuit()`, `StackAlloc` presence, `StorageRead`/`StorageWrite` op
*counts*) and never the actual *computed value* — the exact same class of
gap `docs/fuzz-property-test-report.md`'s n_params bug was.

**Root cause:** `volar-llvm-vaffle-import`'s `stack_load`/`stack_store`
stamped each `StorageId::STACK` address `Stmt::Const` with `self.bit_tid` —
a **1-bit** declared type — even though the constant's own numeric value
(`base_slot + i`) is not a single bit. An interpreter evaluating
`Stmt::Const` masks the literal down to its *declared* type's width, so a
1-bit-typed address constant silently collapses **every** address to just
its own low bit — e.g. address 5 (`0b101`) and address 4 (`0b100`) both
become bit 0 (`1` and `0` respectively... but address 6 and address 4 both
become `0`). Confirmed directly: `spill(5)` evaluated (via
`volar_fuzz::interpreter::ir::eval_ir`) to `0`, not `5`, and the
interpreter's own storage map showed only two live addresses (`0` and `1`)
for a function whose alloca needed 32 distinct ones. Fixed by giving
`Importer` a dedicated `addr_tid` (`_32`, matching the `PTR_BITS`/`SP_BITS`
convention already used elsewhere) and using it for the address `Const`
instead of `bit_tid` — `stack_addr_bits` (which builds a pointer's own
*bit-decomposed* representation, where each individual bit-value's numeric
value genuinely is 0 or 1) was already correct and untouched.

**A second, independent bug** surfaced by the same numeric test: `StorageId::STACK`
is shared, uncoordinated, between this importer's own per-function alloca
bump allocator (`FuncCtx::next_stack_slot`, starting at 0) and
`volar-vaffle-target/src/lower_to_ir.rs`'s calling-convention frame layout
(params/return/spill/cross-block-value regions, *also* starting at absolute
address 0 for a top-level function) — `lower_to_ir.rs`'s translation of
`Value::StackAlloc` forwarded its address-bit `Stmt::Const` nodes verbatim,
with no rebasing against that frame at all. Initially worked around with a
fixed `STACK_ALLOCA_RESERVE` offset (comfortably clear of any realistic
frame size); a follow-up pass replaced that heuristic with the fully
general fix below. **The reserve constant is gone.**

## Fully-principled frame rebasing (StorageId::ALLOCA)

The importer's stack addresses are now genuinely *local, per-function
offsets* starting at 0 (`FuncCtx::next_stack_slot`'s original scheme, no
reserved headroom needed), and `lower_to_ir.rs` rebases each one onto the
real runtime frame at lowering time: `sp_bits + local_offset`, via the same
`StackPtr`/`bc_add` machinery the calling convention's own spill/reload/
param/return addressing already uses (`rebase_stack_addr` in
`lower_to_ir.rs`). `sp_bits` is a block's own incoming SP — exactly where
that function's own `own_layout.size` register region ends — so alloca
storage starts right past it. Critically this is the *actual runtime* SP,
not a compile-time literal: a recursive call's nested activation gets its
own alloca storage automatically, rather than every recursion depth
aliasing the same fixed address (which no fixed-offset scheme, however
large, could ever prevent — only defer).

A second, independent collision surfaced once this was implemented: a
function that both uses `alloca` *and* makes a nested call needs its own
call-site SP advancement to skip past its own live alloca storage, not
just the callee's own register region — otherwise the callee's frame lands
on top of it. Fixed via `FuncInfo::alloca_budget` (computed once in
`plan_functions`, straight off every `Value::StackAlloc`'s own
`base_slot`/`count`/`elem_ty` fields), added alongside `own_layout.size` at
every call site's SP advance.

**`StorageId::ALLOCA`, not `StorageId::STACK`, is the marker that gets
rebased.** The first implementation matched on `storage: StorageId::STACK`
directly and immediately broke `volar-fuzz`'s
`prop_n_lower_vaffle_to_boolar_extended_preserves_semantics`: its extended
block generator (`crates/fuzz/volar-fuzz/src/generators/vaffle.rs`) picks a
`StorageId` from `0..4` for arbitrary storage ops, including `STACK`'s
numeric value (1) as *just another id* with plain, unrebased semantics —
exactly the kind of unrelated code the module doc already warned
`StorageId::STACK`'s sharing could hit. Rebasing based on `STACK`'s numeric
value silently shifted those addresses, breaking equivalence between the
VAFFLE-level interpreter (no rebasing) and the lowered-to-Boolar pipeline
(rebased). Fixed by giving alloca a dedicated, disjoint id —
`StorageId::ALLOCA = StorageId(1_000_001)`, following `VAFFLE_SSA_SPILL`'s
existing "far away, deliberately reserved" pattern — so `StorageId::STACK`
stays exactly what it always was: a plain, unrebased storage space free for
the calling convention's own frame *and* for arbitrary/fuzzer-generated
code, with zero special handling. `lower_to_ir.rs` matches `ALLOCA` on the
way in and re-tags the rebased result `STACK` on the way out (that's
genuinely where the data ends up living); `StorageId::ALLOCA` never
survives into the lowered output. `VaffleTarget::alloca` (`target.rs`, the
`CBackend`/LIR-level producer) deliberately keeps using `StorageId::STACK`
directly and is *not* rebased — it never goes through `lower_to_ir.rs` in
its real pipeline, so there's nothing to rebase.

Tests: `volar-vaffle-target::lower_to_ir`'s own unit test
`test_alloca_budget_reserved_across_nested_call` (hand-built two-function
VAFFLE module; checks the computed `alloca_budget` directly and that the
lowered output re-tags `ALLOCA` as `STACK` with no leftover `ALLOCA`);
`llvm_alloca_survives_nested_call_lowers_without_panicking`
(`volar-ir-build/tests/llvm_frontends.rs`) — this one can only check that
lowering succeeds, not the computed value: `unroll_ir`/`movfuscate` both
reject *any* call-preserving cross-function call as "not statically
finite," confirmed reproducible with zero allocas involved. That gap in
numeric-evaluation support for cross-function calls is pre-existing and
orthogonal to this work — noted under "Not this task" below, not fixed
here. Full `volar-fuzz` property suite (78 tests, 0 ignored) re-verified
green after this change, including the one it broke along the way.

## What landed

`instr.get_allocated_type()` is now recursively flattened
(`flatten_alloca_type`) down to an innermost scalar integer type and total
element count — `[16 x i8]` → `(i8, 16)`, `[4 x [4 x i8]]` → `(i8, 16)`
(nested arrays flatten too), a bare scalar → `(ty, 1)` (unchanged from
`llvm-alloca.md`). `Value::StackAlloc`'s `elem_ty`/`count` fields keep their
natural LLVM meaning (element type and total flattened element count),
matching the existing convention rather than inventing a "flat blob" type.
Struct types (and anything else) return `None` from `flatten_alloca_type`,
producing the same kind of named `ImportError::Unsupported` as before — the
"or names a clear error" branch of item 3, chosen over implementing struct
field layout/GEP.

The existing single-constant-index `getelementptr` path
(`docs/llvm-alloca.md`) needed **no changes** for the "typed view" case
(`getelementptr i32, ptr %buf, i64 k` over a `[16 x i8]`-typed `%buf`): its
element width already comes from the GEP's own
`get_gep_source_element_type()`, independent of the base pointer's
declared alloca type. A genuinely `[N x T]`-typed, two-index GEP
(`getelementptr [16 x i8], ptr %buf, i64 0, i64 k`) is still rejected as
"multi-index GEP into stack pointer not supported" — out of scope here,
matching `llvm-alloca.md`'s existing single-constant-index-only design.

Tests: `array_alloca_imports`, `array_alloca_gep_typed_view_two_i32_slots`,
`struct_alloca_is_named_unsupported`
(`volar-llvm-vaffle-import/tests/basic.rs`, structural checks only, matching
that file's existing convention); `llvm_array_alloca_stack_spill_computes_x_xor_x_plus_1`
(unrolls to `is_circuit()` **and** evaluates `x ^ (x+1)` via
`volar_fuzz::interpreter::ir::eval_ir` — the doc's actual item 2 bar),
`llvm_struct_alloca_is_named_unsupported`
(`volar-ir-build/tests/llvm_frontends.rs`).

## Not this task

- Struct element types with constant GEP indices (item 3's *other* branch —
  this handoff took the named-error branch instead). Would need field
  layout (size/alignment) from LLVM's `TargetData` and a genuine two-index
  GEP path (`[0, field_idx]`), not just the single-index one this crate has.
- A general numeric evaluator for call-preserving, cross-function Volar IR:
  `unroll_ir`/`movfuscate`/`eval_ir` all reject *any* real inter-function
  call as "not statically finite," independent of alloca (confirmed with a
  plain two-function call chain and zero allocas). This means the
  `alloca`-survives-a-nested-call fix above is verified structurally
  (`volar-vaffle-target`'s own hand-built-module unit test) but not via a
  full LLVM→circuit→numeric round trip — that round trip doesn't exist yet
  for *any* non-inlined multi-function program, alloca or not.
- `import_module_inlined` + `alloca` (`llvm-alloca.md`'s existing "Not this
  task" — orthogonal, still unverified; `inline_vaffle.rs`'s `StackAlloc`
  rebase looks disconnected from `VaffleTarget`'s actual addressing
  convention, per that doc's "Known risks").
- Symbolic/dynamic GEP index, pointer `phi`/`select`, heap `malloc`,
  identity `WebProofBackend::verify`, full SLH-DSA verify — all unchanged
  from `llvm-alloca.md`.
- Site `llvm`/`llvm-loop` canaries for rustc `-O0` `stack_spill` (item 5) —
  cross-repo (`site`), not touched here; the fixture and its correctness are
  now verified in *this* repo via `llvm_array_alloca_stack_spill_computes_x_xor_x_plus_1`.

## Where

`volar-llvm-vaffle-import`'s `translate_instruction` `Alloca` arm
(`flatten_alloca_type`), `Importer::addr_tid`, `stack_load`/`stack_store`
(the `bit_tid` → `addr_tid` fix, and their `StorageId::ALLOCA` tag).
`volar-ir-common::StorageId::ALLOCA` (the dedicated marker). `volar-vaffle-
target/src/lower_to_ir.rs`'s `rebase_stack_addr`, `compute_alloca_budget`,
`FuncInfo::alloca_budget`, and the call-site SP-advance that consults it.
Tests listed above.
