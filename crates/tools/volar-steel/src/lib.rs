//! Steel-in-Volar: hosting a fork of the Steel EVM view-call coprocessor on
//! the Volar pipeline. See `docs/steel-in-volar-plan.md` (repo root).
//!
//! S1 (frontend selection): drive BOTH frontends — WASM
//! ([`volar_wasm_circuit_import`]) and LLVM ([`volar_llvm_vaffle_import`]) — on
//! the same reference program and measure completeness + size. The program is
//! compiled from `docs/steel/reference.rs` to both `reference.wasm` and
//! `reference.ll` in one build (see `docs/steel/README.md`).

#![forbid(unsafe_code)]

/// Path to the checked-in Steel reference-program artifacts
/// (`docs/steel/`), relative to the workspace root.
pub const STEEL_ARTIFACTS_DIR: &str = "docs/steel";
