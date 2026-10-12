# volar-evm-backend

EVM 256-lane SIMD backends for Boolar circuits. Each 256-bit EVM word
carries 256 independent Boolean lanes — lane zero is the least-significant
bit — so one lane-wise `AND`/`OR`/`XOR`/`NOT` evaluates 256 circuit
instances in a single 3-gas operation. Lane scheduling comes from
`volar-ir-simd`.

This crate is the destination of the EVM work staged in retrop's
`retrop-emit-evm` (see retrop's `docs/PLAN-EVM-SIMD.md`, phases 2–5).

## Emitters

Emission is generator-agnostic over the `EvmEmit` trait: slot-based methods
with no Yul syntax or byte vocabulary, so any EVM generator can implement
it. Emitters needing a whole-program view can consume `volar-ir-simd`'s
`LanePlan` directly.

- **`SolidityAsm`** — a complete `.sol` library source:
  `function eval(uint256[] memory inputs) internal pure returns (uint256[]
  memory outputs)` whose `assembly { ... }` body is one `let` binding per
  lane slot in whitelisted Yul (`let`, `:=`, `and`, `or`, `xor`, `not`,
  `mload`, `mstore`, `add`, numeric literals). No loops, no jumps; every
  array offset is a compile-time literal. Compile with
  `--via-ir --optimize`: the Yul optimizer then owns stack allocation, CSE,
  and dead-code elimination. **The legacy pipeline does not optimize inline
  assembly.** Array bounds are the caller's responsibility.
- **`EvmSolidityBackend`** — the same emitter behind the standard
  `volar-circuit-source` `CircuitSourceBackend` trait, yielding a
  `SourcePackage` with a `{library}.sol` file. `EmitOptions::circuit_name`
  becomes the Solidity function name.
- **`RawEvm`** — straight-line runtime bytecode with no jumps: inputs via
  `CALLDATALOAD` at calldata offset `32 * index` (no function selector),
  one scratch memory word per slot at `scratch_base + 32 * slot`, outputs
  staged to memory and returned via `RETURN`. Constants are `PUSH0` (zero,
  2 gas) and `PUSH32` of all-ones (3 gas; `PUSH0; NOT` would cost 5 gas but
  save 33 bytes of code — runtime cost wins here).

Storage-bearing or otherwise non-lane-compatible circuits are rejected with
`EvmEmitError::NotLaneCompatible`; there is no scalar EVM fallback.

## Gas model and measured amortization

Static gas per the [opcode reference](https://ethereum.org/developers/docs/evm/opcodes/)
and the Yellow Paper, appendix G: `AND`/`OR`/`XOR`/`NOT`/`PUSH1..32`/
`MLOAD`/`MSTORE`/`CALLDATALOAD` = 3 gas, `PUSH0` = 2 gas, `RETURN` = 0 gas,
plus memory expansion `C(a) = 3a + a²/512` on the touched word count,
charged incrementally. The test suite's bytecode-subset interpreter
(`tests/support/evm.rs`) implements exactly this model.

Fixture (`tests/raw.rs::packed_gas_amortizes_over_256_scalar_runs`):
`xor_reduce(64)` — 64 inputs, 63 gates — run once packed versus 256 times
single-lane with the *same* emitted bytecode:

| run | gas |
|---|---|
| packed, 256 lanes | 2,538 |
| scalar total, 256 runs | 649,728 |
| amortization | 256.0x |

The assertion bound is a conservative 64x; the measured margin is the full
256x because every packed word op replaces 256 same-priced scalar ops and
input staging is identical per-run work in both modes.

## Testing

`cargo test -p volar-evm-backend` runs golden Solidity output,
whitelist-Yul linting and interpretation, raw-bytecode interpretation,
cross-emitter agreement, the `CircuitSourceBackend` package test, and the
gas fixture. `BLESS=1` regenerates the golden `.sol` file. The `solc`
differential test is gated on a `solc` binary (`SOLC_BIN` overrides the
name) and skips cleanly when absent; it proves solc with
`--via-ir --optimize` accepts and fully compiles the emitted body (covering
Yul→EVM stack allocation, where `stack-too-deep` would surface). Executing
the compiled harness needs a full EVM and is future work (`revm`, behind an
off-by-default feature).
