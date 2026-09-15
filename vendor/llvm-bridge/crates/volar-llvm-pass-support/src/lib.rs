//! Runtime helpers shared by LLVM New-PM pass-plugin crates: panic-safe
//! entry-point wrapping ([`entry`]) and marker-call scanning ([`marker`]).
//!
//! See `volar-llvm-pass-build` for this family's build-time half
//! (`llvm-config` probing and C++ shim generation) — kept as a separate
//! crate since it has no runtime LLVM binding and is only ever a
//! `build-dependencies` entry.

pub mod entry;
pub mod marker;

pub use entry::{free_error, run_pass_body, store_error};
pub use marker::{
    MarkerCall, find_calls_to, is_marker_wrapper, retained_wrapper_globals,
    retained_wrapper_globals_in, validate_marker_declaration,
};
