// @pinnedness: unpinned
// @stability: very-unstable
// @ai: assisted

//! EVM 256-lane SIMD backends for Boolar circuits.
//!
//! Each 256-bit EVM word carries 256 independent Boolean lanes (lane zero
//! is the least-significant bit), so one lane-wise `AND`/`OR`/`XOR`/`NOT`
//! evaluates 256 circuit instances in a single 3-gas operation — the
//! amortization that makes EVM a worthwhile SIMD target.
//!
//! Emission is generator-agnostic over [`EvmEmit`]: the sink vocabulary
//! mentions neither Yul syntax nor bytes, so Solidity inline assembly
//! ([`solidity`]), raw bytecode ([`raw`]), or any other EVM generator
//! (Huff, a standalone Yul pipeline, ...) can implement it. Lane scheduling
//! comes from `volar-ir-simd`; emitters that want a whole-program view can
//! consume its `LanePlan` directly instead of the streaming trait.
//!
//! For the standard `volar-circuit-source` pipeline, [`EvmSolidityBackend`]
//! implements `CircuitSourceBackend` and packages the Solidity library as a
//! `SourcePackage`.
//!
//! Storage-bearing or otherwise non-lane-compatible circuits are rejected
//! with [`EvmEmitError::NotLaneCompatible`]; there is no scalar EVM
//! fallback. This crate is the destination of the EVM work staged in
//! retrop's `retrop-emit-evm`; see retrop's `docs/PLAN-EVM-SIMD.md`.

pub mod emit;
pub mod raw;
pub mod solidity;

pub use emit::{EvmEmit, EvmEmitError, emit_circuit, emit_plan};
pub use raw::{RawEvm, RawEvmConfig};
pub use solidity::{EvmSolidityBackend, SolidityAsm, SolidityOptions};
