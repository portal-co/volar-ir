# LLVM array/struct `alloca` → VAFFLE stack

**Status: landed** for constant-size alloca of an array of integers (including
`[N x i8]` as a byte blob), plus a single-index, differently-typed constant
`getelementptr` used as a "typed view" into it (the rustc `-O0` `stack_spill`
shape). Struct allocas are a named error (the "or names a clear error" branch
of item 3 below — not implemented, deliberately).

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
`Value::StackAlloc` forwards its address-bit `Stmt::Const` nodes verbatim,
with no rebasing against that frame at all. Once the `bit_tid` masking bug
above was fixed (making 32 *genuinely distinct* addresses reach the
interpreter instead of collapsing to 2), this became directly observable:
address 0 aliased the function's own packed return-value slot. Worked
around locally (not fixed generally) via `STACK_ALLOCA_RESERVE`: this
importer's own alloca bump allocator now starts at a large, fixed offset
(`1 << 24`) instead of 0, comfortably clear of any realistic
`own_layout.size` while staying well inside `SP_BITS` (32) so `StackPtr`
arithmetic elsewhere never wraps around it. A fully general fix (rebasing
every `StorageId::STACK` access by that function's own `own_layout.size`
inside `lower_to_ir.rs`) is still open — see `llvm-alloca.md`'s "Known
risks" for the sibling gap this compounds (`inline_vaffle.rs` + `alloca`).
Nothing else currently reaches this path with a real, non-trivial
`own_layout.size` (`VaffleTarget`/`CBackend` never go through
`lower_to_ir.rs` for `StackAlloc` at all), so the reservation is safe today,
but it is a heuristic, not a proof.

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
- A general fix for the `StorageId::STACK` frame/alloca address-space
  collision described above (belongs in `lower_to_ir.rs`, affects any
  future producer of `Value::StackAlloc` that reaches it with a non-trivial
  `own_layout.size`, not just large allocas from this importer).
- `import_module_inlined` + `alloca` (`llvm-alloca.md`'s existing "Not this
  task" — orthogonal, still unverified).
- Symbolic/dynamic GEP index, pointer `phi`/`select`, heap `malloc`,
  identity `WebProofBackend::verify`, full SLH-DSA verify — all unchanged
  from `llvm-alloca.md`.
- Site `llvm`/`llvm-loop` canaries for rustc `-O0` `stack_spill` (item 5) —
  cross-repo (`site`), not touched here; the fixture and its correctness are
  now verified in *this* repo via `llvm_array_alloca_stack_spill_computes_x_xor_x_plus_1`.

## Where

`volar-llvm-vaffle-import`'s `translate_instruction` `Alloca` arm
(`flatten_alloca_type`), `Importer::addr_tid`, `stack_load`/`stack_store`
(the `bit_tid` → `addr_tid` fix), `FuncCtx::new`/`STACK_ALLOCA_RESERVE`
(the frame-collision workaround). Tests listed above.
