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
- **OPEN (Table path only)**: one join shape remains — a token-0
  (program-lowering) composition at a 64-pred join that vaffle_ssa's
  post-SP idom rejects while the import-time LLVM-dominance model
  accepts it. The disagreement is between the import-time CFG model
  and vaffle_ssa's post-SP CFG; next step is a pre/post-SP pred-set
  comparison on the failing function (the DOM PROBE in the revm
  probe, VLVI_DOM_TRACE=1, shows the surviving cross-block Merges:
  f122-style token-0 program compositions).

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
