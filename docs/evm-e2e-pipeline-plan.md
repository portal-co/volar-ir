# End-to-end WARP → on-chain verifier pipeline + gas analysis

**Goal.** Run the full pipeline — a program → Volar IR → virtualization →
PESAT instances → WARP accumulation proof → the on-chain verifier/decider —
with a documented, official-style parameter set, measure the on-chain gas, and
identify what needs optimizing (SIMD lanes, multi-transaction proofs,
pre-Solidity IR optimizations, native-Keccak externs) on L1 vs L2.

## Architecture

```text
program (spec source)
  │  volar-lir-codegen
  ▼
Volar IR (IRBlocks)                    ── volar-ir
  │  volar-ir-virt::virtualize_ir       (DispatchMode, dedup)
  ▼
virtualized step circuit
  │  movfuscate + lower_to_circuit      (E1 step granularity)
  ▼
per-step PESAT instances               ── volar-pesat-emit (E1–E5)
  │  volar-iop::warp::warp_prove        (Construction 10.4 fold)
  ▼
WARP accumulation proof
  │  on-chain:
  ▼
warp_verify (succinct, on-chain)  +  warp_decide (linear, off-chain or proven)
```

The on-chain artifact is **`warp_verify`** (the succinct verifier: Merkle paths
+ twin-constraint & multilinear sumchecks + a few MLE/eq evals — logarithmic in
the codeword size `N`), not `warp_decide` (linear — off-chain or itself proven
per the prove-the-verifier plan). The prior `printer_sol` work compiles the
field arithmetic + Keccak cores; this extends it to the full succinct verifier.

## Official-style parameters

WARP over GF(2^128) (the VOLE field). Theorem 9.1 needs `n`, `M`, `ell`, and
`1 + s + t` powers of two. Per the WARP plan (§2), *production* code/parameter
selection against the paper's bounds is a separate reviewed step; for this
analysis we use a documented **illustrative** set and record the soundness
reasoning, not a certified claim:

- field `GF(2^128)`; code = the seeded two-cascade **Spielman** code
  (`volar-spec`'s `SpielmanCode`), distance uncertified (C3 follow-on).
- `log_codeword_len` (`n`) sized to the statement's witness.
- `s` out-of-domain + `t` shift queries, `1 + s + t` a power of two; `t`
  drives the `(1-δ)^t` proximity soundness, `s` the out-of-domain term.
- `ell` (fold arity) a power of two.

The gas profile's *shape* (which ops dominate, how it scales) is
parameter-robust; the exact constants move with `(s, t, code distance)`.

## Milestones

| M | Workstream | Gate |
|---|---|---|
| P1 | Official-style parameter set + a real proof (native) for a small program | `warp_prove`/`warp_verify`/`warp_decide` round-trip on pipeline-emitted instances; sizes recorded |
| P2 | Succinct-verifier codegen to native Solidity (`printer_sol` extension: structs, transcript→KECCAK256, sumchecks, Merkle-verify) | `warp_verify` compiles, correct vs native, gas measured |
| P3 | Gas analysis: L1 (30M) vs L2 limits; the binding constraints | measured gas vs limits; bottleneck breakdown |
| P4 | Optimization analysis: SIMD lanes, multi-tx proofs, pre-Solidity IR opts, Keccak externs | a costed recommendation per lever |

## Optimization levers to evaluate (P4)

1. **SIMD lanes (256×)** — only helps many *independent* small verifications;
   the single succinct verifier is already logarithmic, so this is for batch
   verification of many proofs, not one.
2. **Multi-transaction proofs** — split the verifier across transactions
   (resumable), if a single tx exceeds the block gas limit.
3. **Pre-Solidity IR optimizations** — reduce the constraint/gate count before
   lowering (CSE, DCE, the circuit-size backlog).
4. **Native-Keccak externs** — already the native path's win; keep the
   transcript + Merkle on `KECCAK256`.
5. **Field-mul strength** — the 128-iteration shift-and-reduce `Gf128.mul` is
   ~23K gas; a windowed/table mul or a `CLMUL`-style trick could cut it.
