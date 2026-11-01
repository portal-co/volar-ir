# Unmodified-REVM import: status and open items

State after `dc0969e`. The unmodified revm driver (revm's own
`run_plain` loop over its real 256-entry `InstructionTable`, no steel
mirroring) **imports end-to-end**:

```text
revm unmodified import OK in 345.6s: 2597 funcs, 7799 blocks,
998,651,137 values      (REVM_UNMODIFIED=1 VLVI_DISABLE_WORD_CELLS=1)
```

Guarded by the 12GB RSS kill-switch; it never fired.

## What it took (commits ed30659..dc0969e)

- Indirect calls: signature-narrowed fn-pointer identity encodings +
  Table dispatch (ed30659).
- Spill-slot pointer provenance: tracked pointers stored to tracked
  slots re-register on load-back; -O0 spill round-trips keep
  specializations (a201ac4).
- Zero-return memo, hash-consed entry-hoisted constants (a201ac4).
- Jump-threaded candidate narrowing: constants thread through block
  params/phis (depth-capped); dispatch narrows by resolved tag/ID;
  fully-resolved single candidate -> direct access; unresolved-GEP
  preserves the base's tag/ID class bits, adder only on ADDR (9f0e030).
- Multi-pool carving + GLOBAL_ID_BITS 12->14 + i128 const fix
  (dc0969e).

## Lowering phase (this round)

- Static wasm32 module: imports 4.33M values, **lowers OK** — 1,091
  blocks / 5,552,117 stmts in 5.0s (`REVM_UNMOD_LOWER=1`).
- Unmodified module: import OK (341s), but whole-module lowering
  **exceeds 12GB inside ssa_ify** (~16.4GB peak; killed). The SSA pass
  holds input + output simultaneously (~1.3x growth), so ~1B-value
  imports cannot lower as one module under this budget. Path forward:
  shrink the import first (item 2), or build an incremental
  per-function lower (still output-bound).
- Fixed en route: vaffle_ssa's spill postcondition now skips
  unreachable blocks (the spill pass's own contract) — the zero-return
  memo's shared entry consts in lowered-unreachable terminators were
  false positives; zero-value bodies (trivial traps) get one unused
  entry Const so SP seeding has provenance to clone; the postcondition
  now reports owner + exact use sites (`cross_block_use_sites`).

## Table-on-ID + incremental lowering (this round)

- **Table-on-ID dispatch** (`VLVI_TABLE_DISPATCH=1`): at unresolved
  sites, an equality-selector cascade over the ID field feeds a dense
  `Terminator::Table` of per-candidate DIRECT-access case blocks (no
  gates, no per-bit mux, no RMW). Static wasm32 module: 4.33M ->
  2.50M values, 849ms -> 467ms; with bulk loop 1.96M / 363ms.
- **Incremental lowering** (`lower_vaffle_to_ir_incremental`): one
  function at a time — SSA-ify with the ORIGINAL entry flag, lower,
  rebase terminator block ids (vars are block-scoped), drop both
  representations. Static module output matches the whole-module
  lowering's block count exactly (1,091) with +0.27% stmts, and the
  bulk differential gates its EVALUATION. Enablers: `lower_all_with`
  knobs (scaffold/import-sink), reachability-correct spill
  postcondition, sig-preserving placeholder sub-modules, cont-shape
  pre-interning (lazy per-call-mix interns diverged per-function type
  tables), empty-body Const fixup.
- **Unmodified module stays open**: its import hovers AT the 12GB
  budget (~999M values; one run completed at 341s, later runs die to
  macOS jetsam at ~11.8GB around function ~970-1330 — the budget, not
  a specific function). Table-on-ID at THIS module's scale explodes
  block count (thousands of dispatch sites x ~430 case blocks) and
  dies earlier — the per-site case blocks need sharing across sites
  (same candidate set + width -> same access blocks) before Table can
  ship at this scale. Cascade-based import remains the working path.

## Shared case blocks + jump threading (this round)

- **Shared dispatch classes**: Table-on-ID's per-site case blocks are
  now SHARED — one dispatch block D (params [ptr, tag]) + selector +
  per-candidate case blocks + result router R per (candidate-set hash,
  width, read/write) class; sites jump into D with a fresh dense tag
  and R's Table routes [loaded, tag] straight to the site's own
  continuation (zero extra blocks per site). Static module with bulk
  loop: 6,606 -> 4,523 blocks, 2.50M -> 1.68M values, 332ms.
  Cascade fallback on candidate-set collision or tag exhaustion.
- **Jump threading** (VLVI_JUMP_THREAD=1, experimental): splices
  statement-empty pure-Jmp non-entry blocks with jump-arg
  substitution, bounded sweeps, single compaction — the old attempt's
  hang is fixed (works suite 19s, no hang). KNOWN BUG: after
  threading, a callee's non-entry block param can go unbound
  (eval_vaffle "value N not in table"; evidence: callee v303 =
  Param{block 6, idx 0} never seeded). The dominator-availability
  guard (jump args must be the block's own params or Consts) did NOT
  close it — the leak is elsewhere in the edge substitution. Off by
  default; debug tooling under VLVI_THREAD_DEBUG / VLVI_SSA_TRACE.

## Threading fix + check-once dispatch (this round)

- **Jump threading FIXED**: the pass substituted the predecessor's edge
  args IN PLACE and installed them under the splice target — but those
  args bind the spliced block's params positionally, so edges landed
  misaligned with dropped const args (the "value N not in table" eval
  failure). The construction now rewrites `targs` with each of the
  spliced block's params replaced by that edge's own arg. Validated:
  bulk differential passes with VLVI_JUMP_THREAD=1 (14.8s), works
  19.5s (no hang), pre_movfuscate/fuzz/lir/vaffle-target green.
- **Check-once dispatch reuse**: the shared class builds D (selector)
  -> RD (candidate Table); a site whose pointer's ID+TAG bits are
  ValueId-identical to the last full dispatch's, in the directly
  continuing block, jumps straight to RD with the recorded selector
  value — the tag/ID check runs once per derived-pointer chain
  (GEPs preserve class bits, so derived pointers match by
  construction). Trace: VLVI_DISPATCH_TRACE=1.
- **Two latent Table-path bugs fixed**: write classes jumped 3 args at
  a 2-param D (value param never threaded; the write Table path had
  never been exercised), and case bodies were built with the site's
  fctx.current, so their storage accesses landed in the site's block
  (dominance violation at lowering). New regression test
  dispatch_table_read_write_reuse exercises write+read+reuse with a
  pointer param and a GEP off it.
- **OPEN (Table path only, env-gated)**: lowering a Table-dispatched
  revm module still trips the dominance assert — a value defined in a
  dispatch CONT block (bit extracts of the cont's packed param) is
  packed by a Merge in an early program block (64-param join): the
  value cache serves bits across dispatch conts without dominance
  guards. Default (cascade) path lowers clean end-to-end (import 615ms
  -> lower 3.9s, 1172 blocks / 4.27M stmts). Next step: make
  site ptr_bits composition re-derive (or dominance-check) cached
  pointer bits per block instead of trusting the cache across conts.

## Dominance guards (this round)

- **Value-cache dominance guard**: cache entries record their defining
  vaffle block; `cache_get_dominant` serves a hit only when def==cur,
  def is the entry, the def block dominates cur via the parent chain,
  the bits are function-global (recursively: Const / entry Param /
  Merge / Shuffle / Poly / Splat / Transmute over global bits), or —
  on rejection — the defining instruction is re-run in the current
  chain (sound for every value-producing opcode; Store/Call/terminators
  excluded). translate_instruction's `op!` re-syncs `cur` after each
  operand fetch because a re-run can split the block.
- **Dominance chain**: `block_parent` per block — `extend_block` links
  dispatch/loop conts linearly; program blocks are seeded at walk entry
  with `exit(idom(LLVM block))` from a Cooper-Harvey-Kennedy idom over
  the supported-terminator CFG. Debug: VLVI_DOM_TRACE=1.
- Validated: dispatch_table_read_write_reuse (write/read/reuse through
  a GEP'd pointer param), the bulk differential, cascade revm import +
  lowering clean (1172 blocks / 4.27M stmts), works with
  VLVI_JUMP_THREAD=1, volar-fuzz 91/91.
- **Site-token routing (this round)**: dispatch sites mint a token;
  conts inherit it; cache entries record it; lookups accept only
  tokens in the using block's ancestor lineage — cross-site cache
  leaks through the shared D/R web are refused and re-derived (the
  f41/f110/f136 web-leak shapes now re-run soundly). The linear
  parent-chain dominance check is gone; the walk-entry resets the
  active token. The old LLVM-idom seeding was removed as dead code.
- **Pre/post-SP comparison DONE (finding)**: the scanner shows f0
  pre=3359 -> post=0 violations and f13 pre=489 -> post=0 — the
  importer emits direct cross-block references BY DESIGN and
  vaffle_ssa's spilling is what makes them sound. The dominance
  panic sat in the PRE-spill collection phase, firing on exactly the
  violations the spiller was about to fix (the cascade path only ever
  passed because its imports happen to compose no foreign bits). The
  panic is removed; the line-676 postcondition ("cross-block values
  remain after spilling") governs residuals. The unit test was
  repurposed: test_cross_block_use_is_spilled_not_rejected (75/75).
- **frame_write_ret FIXED (this round)**: the failing Return was a
  reachable-but-instruction-less block's fallback `Return{[]}` under a
  non-void sig (f164 b264: sig_results=1 stmts=0). The Return arm now
  forwards the zero-return convention: short/empty ret_bits are padded
  with zero Const bits to FrameLayout.ret's slot count before
  frame_write_ret. **THE TABLE-DISPATCHED REVM MODULE NOW LOWERS
  END-TO-END FOR THE FIRST TIME**: import OK (1.68M values) -> lower
  OK in 8.45s: 5092 blocks / 3.35M stmts / 15 types (the Table
  trade-off vs cascade: more blocks, fewer stmts — 1172/4.27M).
  Suites green: dispatch test, differential, cascade import+lower,
  vaffle-target 75/75, fuzz 91/91, lir 7/7, pre_movfuscate 9/9,
  works with VLVI_JUMP_THREAD=1.

## Open items

1. **Word-cell freeze does not converge on this module** — needs
   `VLVI_DISABLE_WORD_CELLS=1`. The spill-provenance recovery exposes
   accesses the pre-import freeze scan cannot see (it walks value flow,
   not store/load slot pairs), so word-frozen storages keep tainting.
   Fail-closed (cap names its storages: `[81, 2359, 2365, 2375]`).
   Fix shape: make the freeze scan treat spill-paired slots as opaque,
   or thread alignment knowledge through the slot-ptr pairing.
2. **~999M values is not yet proving-sized**, and the import hovers at
   the 12GB budget (flaky under memory pressure). Table-on-ID exists
   and wins on the static module, but needs SHARED CASE BLOCKS across
   sites before it helps here (per-site case blocks explode block
   count at thousands of sites).
3. **Lowering**: the incremental per-function lower exists and is
   eval-gated (`lower_vaffle_to_ir_incremental`); at 1B import values
   the OUTPUT alone still approaches the budget, so item 2 (shrink the
   import) remains the lever. Static-module lowering: 4.33M -> 5.57M
   stmts (whole-module 5.55M) in ~4.7s.
4. `bulk_mem` differential needs `-C debug-assertions=off`:
   debug-assertion panic machinery at -O0 trips a lowering/eval
   param-threading off-by-one (35 args into 34 params). Not a bulk
   issue; document or fix the glint shape separately.

## Incremental-lowering redesign (2026-10-31, call terminators)

**Root cause of the movfuscation elision has TWO layers.**

Layer 1 (FIXED, this change): `lower_vaffle_to_ir_incremental` lowered
each function in a throwaway sub-module where every other function was
an empty-name Import placeholder, so every `Value::Call`'s static jump
targeted a GHOST abort id that was never emitted (empty-name
placeholders reserve no sink slot), and continuation `Const`s carried
un-rebased local block ids. Real callee entries were orphaned
(reachable=3/5092, no `Dyn` ever reached) — invisible to
block/stmt-count differentials, only visible to evaluation. Redesigned
as a persistent single-module driver: one `LowerCtx` owning the module
mutably, functions appended in sequence with dynamically assigned
(final) block ids; a throwaway per-function SSA pre-pass records frame
sizes up front (callee sizes exact before callees lower); a persistent
function->entry map gives direct edges to already-lowered callees and
records pending edges for the rest, resolved at the end (real entry,
zero-return args truncated; deduped on-demand abort sinks for
declaration-only imports).

Layer 1b (FIXED, same change): cross-call spill collapsed WIDE values
to one bit — `pack_bits` treats each var as a single bit position, and
the call arm packed `val_map.required(key)` wires directly, so any
multi-bit value spilled across a call reloaded as bit 0 (+ the reload
wrote 1-bit wires back into `val_map`). The call arm now explodes each
spilled key to its full bit width and re-merges on reload.

Validated by `incremental_multifunction_call_eval_matches_whole`
(volar-steel): a multi-function module (forward callee, chained calls,
a call inside a branch) is FULLY statically reachable through both
lowerings, block counts match whole-module lowering, and VAFFLE
reference == whole-module IR == incremental IR on all vectors.
Suites: vaffle-target 75/75, fuzz 91/91, lir 7/7, replay 4/4,
ir-passes 177/177.

Layer 2 (RESOLVED 2026-10-31): the 73 unresolved callees were NOT
declare/define aliasing inside one module — the pre-link `.ll` carries
cross-CGU calls as `declare`s whose DEFINES live in other CGUs (only
177 defines / 90 declares; `overflowing_add` etc. are declare-only).
Fix: build the driver whole-program —
`CARGO_PROFILE_DEV_LTO=fat CARGO_PROFILE_DEV_CODEGEN_UNITS=1 cargo
rustc --target wasm32-unknown-unknown -- --emit=llvm-ir -C opt-level=0
-C panic=abort` — merging every dependency CGU into one module
(12,025 defines; only `strlen`/`memcmp` remain external). The emitted
module needs `programs/sanitize_wasm_ll.py` (newest float spellings ->
accepted hex forms, `target_mem:` attribute tokens stripped, float
parse/format machinery neutered `unreachable` — revm is integer-only).

Importer extensions for the LTO module (all fail-closed -> lowered):
AtomicRMW (load->op->store, old value returned; single-threaded wasm
ordering ignored), AtomicCmpXchg (load, Eq, select-store, aggregate
{old, success} result registered for extractvalue), Fence (no-op),
constant-aggregate flattening in `value_bits` (StructValue fields /
ArrayValue elements, including niche-encoded `inttoptr` enum
constants), unused-global pool skip (`get_first_use` — safe
over-approximation), dedicated GLOBAL spillover pools (globals were
pool[0]-only; LTO rodata overflows the ADDR field — global pools are
byte-addressed and never alloca-carved so the two address domains
cannot collide), and the stale global-count dispatch gate replaced by
a pool-storage-count gate (per-site candidates are per-STORAGE since
pool packing; the global count does not widen any mux).

RESULT (static wasm32 probe, REVM_UNMOD_LOWER VLVI_TABLE_DISPATCH
VLVI_DISABLE_WORD_CELLS VLVI_BULK_LOOP REVM_MOVFUSCATE): import OK
1,121 funcs / 54,545 blocks / 15.3M values in 3.2s (was 308 funcs /
1.68M values with 73 unresolved imports); lower OK 56,734 blocks /
26.5M stmts in 160s; **movfuscation 26.5M stmts -> 1 block / 81.7M
stmts of guarded mov-iterations in 67s** — StorageRead 270,917 ->
564,763 (guarded, PRESERVED), StorageWrite 293,846 -> 293,846
(exact), Poly 8.8M -> 54.3M (guard algebra). The former 99.7% elision
is gone: operations are carried as iterations as intended. Probe
facts: 1,186 real call sites with proper Dyn returns, 8 remaining
import sinks (strlen/memcmp-family call sites inside neutered/leaf
contexts). Note: the probe's own reachability REPLICA still
under-reports if it does not expand Dyn (the movfuscation layout
visits all on Dyn; the replica was aligned to match).


## IR-level optimization + unmodified-LTO status (2026-10-31)

The opt pipeline (`volar_ir_opt`) is wired into both revm probes behind
env gates, with per-stage timings measured at the static LTO module's
scale (26.5M lowered stmts / 56,734 blocks):

- **DCE** (`dce_ir_blocks_with_remap`): 50s round 0, converges round 1
  (13.8s round 1, unchanged round 2). Alone: 26.5M -> 25.0M stmts.
- **CSE** (`cse_ir_blocks_with_remap`): 65-71s round 0, converged by
  round 1. With DCE: 26.5M -> 12.68M stmts (2.1x).
- **fold** (`fold_ir_blocks`): 1438s at 26.5M stmts (54us/stmt — a
  pass-performance problem; gated behind REVM_IR_FOLD, default off).
- **VAFFLE-level `fold_vaffle_module`**: 987s at 15.3M values with no
  value-count reduction (gated behind REVM_VAFFLE_OPT, default off).
- **Batch** (Poly widening): unmeasured at scale (fold-gated).
- Correctness: the bulk differential re-evaluates the optimized lowered
  modules — 60,081 -> 14,865 stmts (4.0x) with eval equality on all
  vectors (DYN-OK contract re-checked pre/post opt at revm scale).

Pipeline traps found and fixed in the probe wiring:
1. `dce/cse_ir_blocks_with_remap` rewrite each block's terminator
   INTERNALLY; the returned var remaps exist for external metadata
   holders only. Applying them AGAIN to the terminators double-remaps
   and corrupts Dyn targets (every Dyn target became a computed
   Shuffle/Poly). `check_dyn_targets` (probe-side, mirrors the
   movfuscator's per-block Block-typed Const/StorageRead/param
   contract) now guards pre/post opt.
2. CSE's allocator churn raises the RSS high-water mark; the
   subsequent movfuscation then lands on top and gets killed
   (jetsam ~12GB). `malloc_zone_pressure_relief` between stages helps
   but does not remove the wall; REVM_NO_CSE skips CSE.

Memory envelope at LTO scale: movfuscation of the static LTO module
(26.5M stmts / 15.3M values / 54,545 blocks) completed once at ~12GB
(the 81.7M-stmt guarded result) but sits AT the ceiling — the same
baseline SIGKILLed on a re-run with less free system memory, and
DCE-only/CSE'd variants (25.0M/12.68M inputs) also died after the opt
stage's churn. Movfuscation needs either a quieter machine, a
two-process pipeline (serialize the module between stages), or its own
memory work before whole-program LTO-scale movfuscation is reliable.

Unmodified-revm LTO import: the unmodified entry pulls revm's FULL
state machinery (BTreeMap/quicksort/account-load paths) through the
interpreter's indirect dispatch — the import SIGKILLed at ~12GB around
function #587/1200+ (~900M values at that point), the same wall as the
pre-LTO 999M-value import. Symbolic memmove length is now lowered
(byte loop, forward-copy = exact for dest <= src). The unmod import
shrink (shared Table-on-ID dispatch at this scale, open item #2)
remains the lever.