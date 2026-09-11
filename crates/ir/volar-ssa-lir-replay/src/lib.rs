// @reliability: experimental
// @ai: assisted
//! Generic `vaffle::Module<P> → LirTarget<H::Output>` replay walker.
//!
//! Unlike `volar-ir-passes::lower_lir::lower_ir` (which lowers a single,
//! fully-inlined `IRBlocks` function), VAFFLE modules preserve call
//! structure and may contain several functions. This crate walks every
//! `FuncDecl::Body` in a module, forward-declares block handles up front
//! (VAFFLE's `Terminator::Jump`/`IfNonzero`/`Table` can all target blocks
//! not yet reached in iteration order), and translates each `Value`/
//! `Terminator` into `LirTarget` primitive calls — reusing the sibling-call,
//! `switch`, and `block_addr`/`dyn_jump` primitives added for exactly this
//! purpose.
//!
//! See [`lower_vaffle_module`] for the entry point.

#![no_std]
extern crate alloc;

pub mod lower_vaffle;

pub use lower_vaffle::{
    build_func_names, lower_vaffle_func, lower_vaffle_module, lower_vaffle_module_to_lir_chunks,
    lower_vaffle_module_to_lir_chunks_with_handler, lower_vaffle_module_with_handler,
};
