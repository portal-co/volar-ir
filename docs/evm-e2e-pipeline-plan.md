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

## Parameters: there are no official ones — derivation from Theorem 9.1

**Finding (searched the paper, ePrint 2025/753, in-repo at
`volar/docs/research/papers/2025-753.txt`).** WARP has **no official concrete
parameter set**. The "Choice of parameters" section (§, p.23) discusses
trade-offs qualitatively and explicitly defers selection: *"Carefully
navigating these tradeoffs requires an implementation (and suitable
optimizations). We leave such efforts to future work."* So we derive the set
from **Theorem 9.1** (p.51), which gives exact formulas.

Theorem 9.1 (for a code `C: F^k→F^n`, fold arity `ℓ`, degree `d`, `κ=N−k`):

- **Rounds**: `log ℓ + log n + 3`.
- **Query complexity (Merkle openings checked by the verifier)**: `ℓ·t`.
- **Verifier field ops**: `O(ℓ·(log n + log M + κ + t) + log ℓ·d + (t+s)·log n)`.
- **Prover time**: `O(ℓ·(n + d·(|p̂|+M)) + s·n + t·log n)`.
- **Soundness ≤ 2^−λ** requires (Eq. 13) **`t ≥ λ / −log(1−δ)`** and (Eq. 12)
  **`|F| ≥ 2^λ · max{ (log ℓ+1)·|Λ(C^ℓ,δ)|,
  (1+max{log n+1, log M+d})·|Λ(C^ℓ,δ)| + (ℓ/2)·err_PG·|F|,
  2^(λ/s−1)·|Λ(C,δ)|^(2/s)·log n,  |Λ(C,δ)|·max{2, log(1+s+t)} }`**.

`n, M, ℓ, 1+s+t` all powers of two (Remark 9.3). For `δ ≤ δ(C)/2` (unique
decoding radius) out-of-domain samples are unnecessary → **`s = 0`** (§,
p.23), which we take.

### Derived set (documented assumptions, not a certified claim)

- **Field**: the soundness field-size term `err_PG·|F| ≲ |F|/2^λ` with
  `err_PG ≈ poly(n)/|F|` forces roughly `|F| ≳ 2^(λ + O(log n))`. **So
  GF(2^128) reaches only ~`128 − O(log n)`-bit security** at codeword size
  `n` — e.g. ~88-bit at `n = 2^20`. True 128-bit at real sizes needs the
  larger `Galois256` (which the spec already has) or the Section-D
  extension-field variant (small base field, challenges from a large `F`).
  **This is a genuine deployment finding.**
- **Code**: the spec's seeded two-cascade **Spielman** code (linear-time
  encoding — required for linear-time accumulation) is the production target,
  but its distance/proximity-radius is uncertified (C3 follow-on). For the
  analysis we assume a constant relative distance `δ(C)` and use the
  Reed–Solomon-like proximity bound as a stand-in; the *shape* of the gas
  profile is robust to this.
- **Proximity bound** `δ = δ(C)/2` (unique decoding → `s = 0`).
- **`t ≥ λ / −log2(1−δ)`**; with `δ = 1/4`, `−log2(3/4) ≈ 0.415`, so
  `t ≈ 2.41·λ`. For `λ = 100`: `t = 255` (so `1+0+255 = 256 = 2^8`). For
  `λ = 128`: `t = 511` (`1+511 = 512 = 2^9`).
- **`ℓ = 2`** (one fresh instance + one running accumulator per step, as in the
  PESAT stream fold).

### Verifier gas drivers (what the analysis measures)

- **Merkle openings**: `ℓ·t` paths × `log n` hashes each (native `KECCAK256`,
  ~519 gas/hash measured).
- **Field arithmetic**: `(t+s)·log n + ℓ·(...)` field ops, dominated by
  `Gf128.mul` (~23K gas measured) — **the dominant verifier cost and the top
  optimization target** (a windowed/table carry-less mul can cut it ~5–10×).

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
