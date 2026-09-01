// @reliability: experimental
// @ai: assisted
//! Build-time IR transformation pipeline.
//!
//! File and in-memory sources (VAFFLE, WASM, structural LLVM, LLVM-direct,
//! Volar IR, saved LIR) plus the IR passes that live in this repo. Object
//! emit and weaving stay in `volar-build`.

mod pipeline;

pub use pipeline::{Pipeline, PipelinePass};

#[cfg(feature = "vaffle")]
pub use pipeline::serialize_vaffle_module;

#[cfg(feature = "wasm")]
pub use volar_vaffle_target::{WaffleImportConfig, WaffleImportKind};
