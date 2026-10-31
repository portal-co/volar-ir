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
