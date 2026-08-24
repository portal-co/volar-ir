//! Retro CPU interpreters generated from the retrop decoder/semantics
//! tables.
//!
//! The data under [`gen`] is produced by `retrop-emit-volar` (the generator
//! lives in the retrop repository; only its output is committed here).
//! Everything else in this crate is hand-written and independent of retrop:
//!
//! - [`table`] — the generated-data schema;
//! - [`cpu`] — a concrete software interpreter over those tables;
//! - [`bir`] — a compiler from the same tables into Boolar IR, one
//!   oblivious single-block circuit per CPU step.
//!
//! Both engines are usable independently of any test harness.

pub mod bir;
pub mod cpu;
pub mod generated;
pub mod table;
