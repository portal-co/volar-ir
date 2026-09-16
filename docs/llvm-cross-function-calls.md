# Cross-function call numeric correctness (`lower_to_ir.rs`)

> **Current circuit path:** concrete CFGs may unroll; movfuscated CFGs produce
> one typed step as specified in [`pipeline.md`](pipeline.md).

**Status: landed.** `unroll_ir_everything` and `movfuscate_ir` both used to
reject *any* call-preserving (`Pipeline::from_llvm`, not `_inlined`)
cross-function call as "not statically finite" / a param-count mismatch,
independent of `alloca` or any other feature — confirmed reproducible with
a plain two-function call chain and zero allocas. Four real, independent
bugs in `volar-vaffle-target/src/lower_to_ir.rs`'s calling convention were
root-caused and fixed; a non-inlined call with a real multi-bit argument
and return value now unrolls to `is_circuit()` **and** movfuscates
correctly, evaluating to the right numeric value in both cases.

None of this needed a fuzzer or a new pass — every bug was found by
building the most minimal possible reproducer (`caller` calls `helper`
with an `i32` argument, adds 1 to the result) and pushing it end to end
through the *existing* pipeline, one root-cause at a time. Each fix
uncovered the next: fixing the control-flow crash surfaced a silent
wrong-value bug; fixing that surfaced a second, alloca-specific address
collision that a previous session's own fix (`FuncInfo::alloca_budget`,
see `docs/llvm-array-alloca.md`) had introduced without ever being
numerically exercised.

## Why this was invisible until now

Every existing test exercising a non-inlined cross-function call
(`test_packed_spill_reload`, `test_selective_spill`,
`llvm_alloca_survives_nested_call_lowers_without_panicking`, and this
crate's own property tests for `lower_vaffle_to_ir`) checked either
*structure* (block count, op count) or ran the *inlined* path
(`import_module_inlined`/`prop_k_*`), which sidesteps this calling
convention entirely by splicing calls away before lowering. Nothing had
ever driven a real, non-inlined call through `unroll_ir`/`eval_ir` far
enough to notice the return value was wrong -- the same "structural tests
pass, computed value is silently wrong" pattern this whole line of work
keeps finding (see `docs/llvm-array-alloca.md`, `docs/llvm-alloca.md`).

## The four bugs, in the order they were found

### 1. `cont_block_idx` predicted the wrong absolute block index

A call site computes where its continuation block will land
(`cont_block_idx`, baked into the continuation const written before the
call and read back via `Dyn` after the callee returns) using
`self.blocks.len() + self.extra_blocks.len() + 1`. `self.blocks.len()`
only reflects how much of the module's reserved block range has been
*consumed so far* -- functions lower in sequence, so a call from an
earlier function has no way to see that a later, not-yet-processed
function's own reserved entry-block slot still sits ahead of it in the
final block array. For a two-function module where `caller` (processed
first) calls `helper` (processed second), the computed index landed
exactly on `helper`'s own entry block -- the callee's return jumped back
into its own entry instead of the caller's continuation, which
`unroll_ir`'s walker correctly reported as `NonFiniteControl` (revisiting
a block with the same fingerprint).

Fixed by adding `LowerCtx::total_blocks` -- `plan_functions`'s own
`block_offset` counter's *final* value, i.e. the true total reserved range
-- and using `total_blocks + extra_blocks.len() + (1 if this call's own
jump block also lands in extra_blocks else 0)` instead.

### 2. `n_params` read the wrong source for the callee's own entry-param count

`plan_functions` computed a callee's `n_params`/`n_param_words` (used both
to size its own entry block's declared params and to size how many packed
words a *caller* must pack per call) from `sig.params` -- the function's
declared signature. But `lower_vaffle_to_ir` always runs
`vaffle_ssa::ssa_ify_module` first, and for any non-entry function (i.e.
any function reachable via an internal call) that pass threads an extra
64-bit (`SPILL_ADDR_BITS`) SP parameter onto the entry block's own
`params` -- appended, not reflected in `module.sigs` at all (deliberately;
see `vaffle_ssa.rs`'s own module doc -- that SP is a lowering-internal
detail, not a real logical parameter). `wire_call_sites` correspondingly
appends that SP's own bits to every `Value::Call`/`Terminator::ReturnCall`'s
`args`. Sizing from `sig.params` desyncs by exactly `SPILL_ADDR_BITS` bits
for any such function: the caller packs more argument words than the
callee's entry block declares, and unpacking on the other side reads
garbage.

Fixed by deriving `n_params` from `body.blocks[body.entry.0].params`
directly (bit-width-summed) instead of `sig.params` -- correct for the
entry function too (no SP threading there, so the two sources already
agreed), and correct regardless of what any future producer does to widen
an entry block beyond its declared signature.

### 3. A multi-bit call result never reached its own uses

Once (1) and (2) were fixed, `unroll_ir`/`eval_ir` completed successfully
but computed the wrong *value*: `caller(123)` (`helper(123) + 1`) returned
`1`, not `124`. Two compounding bugs:

- `n_ret_bits_orig` (used to size how many bits `unpack_words` reconstructs
  from the callee's packed return words) was computed as `if n_ret > 0 {
  1 } else { 0 }` -- `n_ret` being the packed *word* count (always 1 for
  any return up to 64 bits), so this was really a presence flag, not a
  bit width. Every existing test's callee happened to return exactly 1
  bit, so this was never wrong in practice until now. Fixed by using
  `callee_info.total_ret_bits` (already computed correctly by
  `plan_functions` from `sig.results`) directly.
- Even with the right bit count unpacked, nothing stored those bits where
  the caller's own body actually looks for them. `volar-llvm-vaffle-import`'s
  own convention (matching any multi-bit VAFFLE value) represents a call's
  result as one `Value::Output { value: call_vid, idx }` node per result
  bit, *not* as a single value keyed by `call_vid` itself -- but the
  per-statement dispatch loop had no match arm for `Value::Output` at all,
  so it fell into the generic `_ => {}` and those nodes silently kept
  whatever `val_map` entry they didn't have (defaulting to `IRVarId(0)`,
  an unrelated wire).

  Fixed by recording each call's own `ret_bits` in a new
  `call_ret_bits: BTreeMap<usize, Vec<IRVarId>>` (keyed by the call's own
  `ValueId`), and adding an explicit `Value::Output { value, idx }` match
  arm that looks its own bit up there. `val_map.insert(call_vid.0,
  ret_bits[0])` is kept alongside for any producer (e.g. `VaffleTarget`)
  that references a 1-bit call result directly without an `Output`
  wrapper.

### 4. `alloca_budget`'s own frame-write/frame-read address mismatch

Testing (1)-(3) against `llvm_alloca_survives_nested_call` (a caller with
*both* an `alloca` and a nested call -- the scenario `FuncInfo::alloca_budget`
was built for, see `docs/llvm-array-alloca.md`) surfaced a fourth, distinct
bug in that earlier fix itself: the call site writes the continuation at
`callee_frame_sp = current_sp_bits` but advances the callee's own received
SP by `alloca_budget + callee_info.own_layout.size` -- so the callee's
*actual* frame base is `current_sp_bits + alloca_budget`, not
`current_sp_bits`. The continuation write and the callee's own
`frame_read_cont` (which retreats from its received SP by its own
`own_layout.size` alone -- it has no way to know this specific caller's
`alloca_budget`) disagreed by exactly that amount, surfacing as
`SymbolicBranch` resolving the callee's own return `Dyn` jump (the address
`unroll_ir`'s walker needed to fold didn't match any write it had seen).

A second instance of the same shift affected the caller's own post-call
state: the callee correctly returns `current_sp_bits + alloca_budget` as
the "SP to resume with" (since that's what its own retreat computes from
what it received), but the caller's continuation set `current_sp_bits`
to that value directly, which is off by `alloca_budget` from the caller's
own real, pre-call SP -- corrupting both the caller's own subsequent
alloca-relative addressing and its own eventual `frame_read_cont`.

Fixed by adding `alloca_budget` to `callee_frame_sp` before writing the
continuation, and retreating the callee's returned SP by `alloca_budget`
on the caller's own side before resuming (the callee cannot do this
itself -- a different caller could have a different `alloca_budget` for
the same callee). Both fixes are strictly on the caller's own side of the
protocol, since `alloca_budget` is caller-specific and the callee must
stay agnostic to it.

## What's still open

- `movfuscate_ir` itself now succeeds structurally on a non-inlined
  cross-function call (`is_movfuscated()` true, confirmed) and a further
  budgeted `lower_to_circuit_ir` unroll of that loop also reaches
  `is_circuit()`. Numerically evaluating *that* combination hit a
  pre-existing, narrower gap: `volar_fuzz::interpreter::ir::eval_ir`
  assumes the single-packed-word-per-input calling convention
  `unroll_ir_everything`'s own output uses, and panics on
  `lower_to_circuit_ir`'s own (differently-shaped) block signature. This
  is a test-harness gap in driving that specific downstream stage
  numerically, not evidence that movfuscation of calls is itself broken --
  out of scope here.
- Recursive (self- or mutually-recursive) VAFFLE calls are not covered by
  any test in this pass. `vaffle_ssa.rs`'s own SP-threading is explicitly
  designed for recursion safety (`advance_sp`/`compute_sp_step`), and nothing
  found here contradicts that design, but no end-to-end recursive-call
  fixture has been numerically verified end to end through `unroll_ir`
  (recursion is inherently unbounded-depth from a *static* unroll's
  perspective in general, though a fixed, small recursion depth folds
  fine under the same "every branch/address is a compile-time constant"
  contract as any other finite loop).
- `Terminator::ReturnCall` (tail calls) was inspected for the same
  `alloca_budget` address-mismatch risk (bug 4) and found *not* to need
  the fix: a tail call reuses the caller's own, already-written
  continuation and its own frame becomes dead once the tail call happens,
  so the callee's frame safely overlapping the tail-calling function's own
  now-dead alloca region is correct, not a bug.
  `llvm_direct_tail_call_return_computes_correct_value` exercises the
  importer-generated form end to end.

## Tests

`llvm_alloca_survives_nested_call` and
`llvm_register_xor_call_computes_correct_value`
(`volar-ir-build/tests/llvm_frontends.rs`) -- both now assert the actual
computed value via `unroll_ir` + `volar_fuzz::interpreter::ir::eval_ir`,
not just that lowering succeeds. The tail-call test additionally checks a
direct `call; ret` pair that imports as `ReturnCall`. Full `volar-fuzz`
property suite (78 tests) and the broader `vaffle`/`volar-ir-opt`/`volar-ir-passes`/
`volar-c-backend`/`volar-wasm-backend`/`volar-llvm-backend` regression
suites re-verified green after each of the four fixes.

## Where

`volar-vaffle-target/src/lower_to_ir.rs`: `LowerCtx::total_blocks`
(bug 1), `plan_functions`'s `n_params` computation (bug 2), the
`Value::Output` match arm and `call_ret_bits` map plus `n_ret_bits_orig`
(bug 3), `callee_frame_sp`/the post-call SP-restoration in the `at_call`
handling (bug 4).
