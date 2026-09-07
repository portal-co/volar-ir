// @reliability: experimental
// @ai: assisted
//! Build-time IR transformation pipeline.
//!
//! File and in-memory sources (VAFFLE, WASM, structural LLVM, LLVM-direct,
//! Volar IR, saved LIR) plus the IR passes that live in this repo. Object
//! emit and weaving stay in `volar-build`.

#[cfg(feature = "llvm")]
mod lto_archive;
mod pipeline;

pub use pipeline::{
    BoolarCircuitStage, BoolarStage, BoolarStepCircuitStage, FoldIr, FromReversible, LirStage,
    LowerStepToBoolar, LowerToBoolar, LowerToLir, Movfuscate, MovfuscatedToCircuit,
    MovfuscatedVolarStage, Pipeline, PipelinePass, PipelineStage, RCircuitStage,
    StorageToMuxBoolar, StorageToMuxIr, ToReversible, UnrollIrEverything, VaffleStage,
    VolarIrStage, VolarStepCircuitStage,
};

#[cfg(feature = "vaffle")]
pub use pipeline::{InlineVaffleEverything, LowerToVolarIr};

#[cfg(feature = "llvm")]
pub use lto_archive::{CommandBuild, write_bitcode_archive, write_bitcode_archive_members};

#[cfg(feature = "vaffle")]
pub use pipeline::serialize_vaffle_module;

#[cfg(feature = "wasm")]
pub use volar_vaffle_target::{WaffleImportConfig, WaffleImportKind};

/// Re-exported so downstream crates (e.g. `volar-build`) can name pass
/// config types like `volar_ir_build::volar_ir_passes::StorageToMuxConfig`
/// without an extra direct dependency.
pub use volar_ir_passes;

/// Circuit-source emission (Noir / POD2 / other textual backends).
pub use volar_circuit_source;
