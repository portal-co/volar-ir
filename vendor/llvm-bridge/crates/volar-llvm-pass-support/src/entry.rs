//! Panic-safe entry-point wrapping for `extern "C" fn(LLVMModuleRef, *mut
//! *mut c_char) -> i32` — the calling convention every pass plugin's C++
//! shim (see `volar-llvm-pass-build::shim`) invokes: negative means `*error`
//! holds an allocated diagnostic (release with [`free_error`]), `0`/`1`
//! mean unchanged/changed.
//!
//! Extracted from `cirrus-llvm-pass`'s `cirrus_llvm_pass_run`/
//! `run_borrowed_module`/`store_error`/`cirrus_llvm_pass_free_error`, which
//! duplicated this exact wrapping once per registered pass.

use std::ffi::{CString, c_char};
use std::mem::ManuallyDrop;
use std::panic::{AssertUnwindSafe, UnwindSafe, catch_unwind};
use std::ptr;

use inkwell::context::Context;
use inkwell::llvm_sys::core::LLVMGetModuleContext;
use inkwell::llvm_sys::prelude::LLVMModuleRef;
use inkwell::module::Module;

/// Run `body` against a raw, pass-manager-owned module, converting its
/// `Result<bool, E>` into this crate family's C-ABI convention. Catches
/// panics so a bug in `body` reports as a diagnostic instead of aborting
/// the host process — LLVM's C API has no panic-safety of its own, and a
/// panic unwinding across the `extern "C"` boundary back into C++ is
/// undefined behavior.
///
/// # Safety
/// `raw_module` must be a valid module reference borrowed from a live LLVM
/// pass-manager invocation (exactly what a C++ New-PM callback receives) —
/// the same requirement as [`inkwell::module::Module::new`]. `error`, if
/// non-null, must be a valid `*mut *mut c_char` output slot.
pub unsafe fn run_pass_body<E: core::fmt::Display>(
    raw_module: LLVMModuleRef,
    error: *mut *mut c_char,
    body: impl for<'ctx> FnOnce(&'ctx Context, &Module<'ctx>) -> Result<bool, E> + UnwindSafe,
) -> i32 {
    if !error.is_null() {
        // SAFETY: caller supplies a valid output slot.
        unsafe { *error = ptr::null_mut() };
    }
    if raw_module.is_null() {
        return store_error(error, "received null LLVM module".to_string());
    }

    let result = catch_unwind(AssertUnwindSafe(|| {
        // SAFETY: raw_module is borrowed from the pass manager for the
        // duration of this call. ManuallyDrop prevents inkwell from
        // disposing either the context or the module — neither is owned
        // by this call.
        let context = ManuallyDrop::new(unsafe { Context::new(LLVMGetModuleContext(raw_module)) });
        let module = ManuallyDrop::new(unsafe { Module::new(raw_module) });
        body(&context, &module)
    }));

    match result {
        Ok(Ok(changed)) => i32::from(changed),
        Ok(Err(reason)) => store_error(error, reason.to_string()),
        Err(_) => store_error(
            error,
            "pass panicked; no LLVM state was retained".to_string(),
        ),
    }
}

/// Allocate `message` as the diagnostic at `*out` (if `out` is non-null)
/// and return `-1`, the "failed" sentinel every entry point in this family
/// returns alongside an allocated error string.
pub fn store_error(out: *mut *mut c_char, message: String) -> i32 {
    if !out.is_null() {
        let message = CString::new(message)
            .unwrap_or_else(|_| CString::new("pass diagnostic contained NUL").unwrap());
        // SAFETY: caller supplied a valid output slot (checked by every
        // caller in this crate before invoking this function).
        unsafe { *out = message.into_raw() };
    }
    -1
}

/// Release a diagnostic allocated by [`store_error`] (transitively, by
/// [`run_pass_body`]). Every plugin exposes its own `extern "C"` wrapper
/// around this with its own symbol name, matching whatever name its
/// generated C++ shim calls (see `volar_llvm_pass_build::ShimConfig::free_error_symbol`).
///
/// # Safety
/// `error` must be null, or a pointer this crate previously produced via
/// `CString::into_raw` inside [`store_error`].
pub unsafe fn free_error(error: *mut c_char) {
    if !error.is_null() {
        // SAFETY: see function doc.
        unsafe { drop(CString::from_raw(error)) };
    }
}
