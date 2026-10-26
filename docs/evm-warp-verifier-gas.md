# WARP verifier on EVM: codegen-path gas analysis

**Question.** Does WARP have a *direct* benefit on EVM, and which codegen path
(boolean-circuit IR → EVM with 256-lane SIMD, vs. a higher-level AST/IR →
Solidity with native opcodes) gives a deployable verifier?

**Short answer.** The 256-lane boolean batching is real but is the *wrong
decomposition* for a WARP verifier: it boolean-ifies Keccak (which should be
the native `KECCAK256` opcode) and the raw backend's one-memory-word-per-slot
model has **quadratic** memory-expansion gas, so it is feasible only for small
circuits. A higher-level **AST-to-Solidity** (or Volar IR-to-Solidity) path that
keeps Keccak native and does GF(2^128) arithmetic at the word level is cheaper
*per operation* than even the batched boolean path, and scales *linearly* — it
is the deployable path. WARP's genuine EVM edge is that its field is the binary
field GF(2^128), whose arithmetic is cheap in both regimes.

## Measured: boolean-circuit path (volar-evm-backend, 256-lane SIMD)

Circuits compiled from spec source (VAFFLE → fully-inlined Volar IR → unroll →
boolar → BCircuit) and measured with a faithful gas interpreter (YP appendix G:
3 gas/word-op, PUSH0 = 2, memory expansion `C(a) = 3a + a²/512` charged
incrementally). Per-lane outputs validated against native `Galois128`.

| circuit | gates | packed 256-lane gas | per-proof (packed) | scalar (1 lane) |
|---|---|---|---|---|
| `xor128` | 2,065 | — | — | — |
| `add128` | 3,082 | — | — | — |
| `mul128` (**u128 integer**) | 2,787,255 | — | — | — |
| `mulg128` (**GF(2^128)**) | 79,037 | 13,499,972 | 52,734 | 13,499,972 |
| `g128_mul x4` | 464,659 | 428,117,497 | 1,672,333 | 428,117,497 |
| `g128_dot4` | 1,004,993 | 1,977,570,163 | 7,724,883 | 1,977,570,163 |

Findings:

- **Binary-field advantage.** A GF(2^128) mul is 79,037 boolean gates vs.
  2,787,255 for a u128 integer mul — **~35× cheaper**, because carry-less
  multiply has no carry-propagation chain. This is a real WARP edge for any
  circuit-based target.
- **256× batching confirmed.** Packed (256 lanes) gas == scalar (1 lane) gas,
  so 256 instances verify for the price of one bytecode run.
- **Quadratic memory.** Gas ≈ `3·G + G²/512`: 79K gates → 13.5M, 465K → 428M,
  1M → 1.98B (a 30M-gas block is exceeded at ~100–150K gates). The raw backend
  allocates one memory word per live slot with no reuse, so memory expansion
  dominates. The full verifier (Keccak is ~10⁵–10⁶ boolean gates per hash ×
  many hashes) is far past this boundary → **infeasible** on this path.

## Measured: native Solidity path (forge, `docs/native-evm/Gf128.sol`)

Hand-written native Solidity for the WARP field + decider primitives, measured
with forge. `Gf128.mul` is verified bit-exact against
`volar_primitives::Galois128` (GCM polynomial `0x87`, interleaved
shift-and-reduce).

| op | gas (native) | gas (boolean, 256× packed) | gas (boolean, scalar) |
|---|---|---|---|
| GF(2^128) mul | ~23,000 | 52,734 | 13,499,972 |
| GF(2^128) add (XOR) | ~68 | — | — |
| Keccak-256 (64-byte node) | ~519 | infeasible | infeasible |
| MLE eval, N=8 | 349,321 | — | — |
| MLE eval, N=256 | 11,895,127 | — | — |

Findings:

- **Native is cheaper per-op than even the batched boolean path.** A native
  GF(2^128) mul (~23K gas, a 128-iteration shift-and-reduce loop) beats the
  boolean *packed* mul (52.7K) — the 256× SIMD batching does not overcome the
  boolean per-gate + quadratic-memory cost. And native scales *linearly*.
- **Native Keccak is the decisive win.** `KECCAK256` is ~519 gas for a 64-byte
  node; the boolean path cannot represent the verifier's Merkle hashing at all
  within a block's gas.
- **The decider is linear in codeword size N.** The MLE eval (the field
  core) is ~23K gas/element. Feasible for small N (≲1000); a real proof
  (N ~ 2²⁰) is ~10¹⁰ gas → on-chain verification must be the *succinct final
  verifier* (post-accumulation), not the linear decider, at real sizes.

### Toy decider cost model (N=8 codeword, M=16 constraints), native

| component | est. gas |
|---|---|
| index validity (`warp_index_is_valid`) | ~1K |
| Merkle root + `commit_codeword` (~15 `KECCAK256`) | ~8K |
| code encode (identity) | ~0 |
| `warp_mle_eval` (N=8) | 349K |
| `warp_evaluate_bundled` (~128 muls) | ~2.9M |
| **total** | **~3.3M gas** — deployable in one block |

The same toy decider on the boolean path boolean-ifies the Keccak → millions of
gates → quadratic memory → infeasible.

## Recommendation

Build a **native AST-to-Solidity** (or Volar IR-to-Solidity) backend for the
verifier, on these principles:

1. **Native Keccak.** Re-anchor WARP's transcript/Merkle hashing to
   `KECCAK256` (EVM-native) instead of the spec's bit-level SHAKE128 sponge.
   The spec currently lowers Keccak through LIR to VAFFLE with *no* extern
   boundary (see the note at `volar-spec/src/warp.rs`); the native path needs a
   hashing extern that the Solidity backend maps to `KECCAK256`. This is a spec
   re-anchoring decision (SHAKE128 XOF ≠ Keccak-256 padding), so it changes the
   Fiat-Shamir transcript bytes — fine for a deployment target, but a distinct
   parameter set.
2. **Word-level GF(2^128).** Keep field elements as `uint256` and use the
   measured `Gf128` library (mul ~23K, add ~68). A windowed/table mul can cut
   this further (the current mul is the straightforward 128-iteration form).
3. **Linear memory.** Emit Solidity `let`/locals (the `--via-ir` optimizer
   stack-allocates) or a reusing-slot arena — *not* one memory word per slot —
   to avoid the boolean backend's quadratic memory expansion.
4. **Scope to the succinct verifier.** The linear decider is deployable only at
   toy sizes; the on-chain artifact at real sizes is the succinct final
   verifier. The per-step / accumulation proofs (PESAT emitter) are decided
   off-chain.

The boolean 256-lane path (`volar-evm-backend`) remains the right tool for
*many small independent circuits* (its intended regime), not for one large
uniform verifier.

## Status: codegen bootstrapped

`volar-compiler::printer_sol` (an AST-to-Solidity printer on the compiler IR)
is the seed of the native path. It keeps operations at the word level —
`Galois128` → `uint256`, `a * b` → `Gf128.mul`, `a + b` → `^` — and emits the
measured `Gf128` library inline. Validated end-to-end: a spec-source
dot-product and the full `eq_evals` + `mle_eval` kernel compile to native
Solidity that (a) is bit-exact against the native reference and (b) costs
~103K gas (dot4) / ~362K gas (MLE N=8) — within 4% of the hand-written native,
versus infeasible on the boolean path.

Remaining to reach the full decider: spec **structs** (`WarpIndex`/`WarpAcc`),
the **Keccak re-anchoring** (SHAKE128 sponge → a `KECCAK256` extern the printer
maps to the native opcode), and the Merkle / bundled-eval logic.

## Reproducing

- Boolean path: `cargo test -p volar-lir-codegen --test warp_compile --
  --nocapture probe_evm_mulg128_gas probe_evm_gate_counts` (and the
  `#[ignore]`d `probe_evm_verifier_kernels` for the scaling series).
- Native path: `docs/native-evm/` is a foundry project fragment; `forge test
  -vvv` reports the per-op gas. `Gf128.mul` is differentially validated against
  `volar_primitives::Galois128`.
