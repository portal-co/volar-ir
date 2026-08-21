#![no_std]
#![warn(missing_docs)]

//! Static marker consumed by `volar-llvm-plugin`.
//!
//! A source module retains a call to [`__volar_entry`] naming the function
//! to run through the plugin's round-trip pipeline: DFA jump threading
//! (`volar-llvm-jumpthread`), structural import into VAFFLE
//! (`volar-llvm-vaffle-import`, call-preserving — no argument bindings are
//! needed, unlike `cirrus-llvm-pass`'s execution-mode importer), and replay
//! back out to LLVM IR (`volar-ssa-lir-replay` into `volar-llvm-backend`).
//! The plugin reads only the marker's own pointer argument; no descriptor
//! struct is dereferenced by generated code.

use core::ffi::c_void;

unsafe extern "C" {
    #[doc(hidden)]
    pub fn __volar_entry(target: *const c_void);
}

/// Retain a direct `__volar_entry` marker call without exposing it at
/// runtime. `volar-llvm-plugin` erases the marker (and its retaining
/// wrapper) once it has resolved and reimported `target`.
#[macro_export]
macro_rules! volar_entry_marker {
    ($name:ident, $target:path) => {
        #[used]
        static $name: unsafe extern "C" fn() = {
            unsafe extern "C" fn marker() {
                // SAFETY: this is an intentional compile-time pseudo-
                // intrinsic consumed by volar-llvm-plugin before final
                // linking.
                unsafe {
                    $crate::__volar_entry($target as *const () as *const core::ffi::c_void);
                }
            }
            marker
        };
    };
}
