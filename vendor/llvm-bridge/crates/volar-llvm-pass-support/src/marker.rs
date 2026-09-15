//! Scans an LLVM module for calls to a named "marker" pseudo-intrinsic —
//! the pattern `cirrus-llvm-pass-api::cirrus_entry_marker!` uses: a source
//! language retains a call to an externally-declared, never-defined marker
//! function so a later pass can find it, validate its shape, decode
//! whatever constant arguments it carries, and erase it before final
//! linking.
//!
//! Extracted from `cirrus-llvm-pass`'s `find_marker_calls`/
//! `validate_marker_declaration`/`marker_helper`/`marker_retention_globals`,
//! generalized to not assume any particular descriptor-argument shape —
//! callers decode the call's own arguments themselves (see
//! `volar-llvm-constchain` for that).

use std::collections::BTreeMap;

use inkwell::llvm_sys::core::{LLVMGetInitializer, LLVMGetNumOperands, LLVMGetOperand};
use inkwell::module::{Linkage, Module};
use inkwell::types::BasicMetadataTypeEnum;
use inkwell::values::{
    AsValueRef, CallSiteValue, FunctionValue, GlobalValue, InstructionOpcode, InstructionValue,
};
use volar_llvm_constchain::{global_from_pointer, strip_pointer};

/// One call site invoking a marker function.
pub struct MarkerCall<'ctx> {
    /// The `call` instruction itself (erase this to remove the marker
    /// after decoding it).
    pub instruction: InstructionValue<'ctx>,
    /// The function the call appears in.
    pub caller: FunctionValue<'ctx>,
    /// The call, typed for argument access (`get_called_fn_value` already
    /// confirmed to name `marker_name` by the time this is constructed).
    pub call: CallSiteValue<'ctx>,
}

/// Find every `call` to a function literally named `marker_name`, anywhere
/// in `module`. Does not validate the marker's declaration shape or decode
/// its arguments — see [`validate_marker_declaration`] and
/// `volar-llvm-constchain` for those.
pub fn find_calls_to<'ctx>(module: &Module<'ctx>, marker_name: &str) -> Vec<MarkerCall<'ctx>> {
    let mut calls = Vec::new();
    for function in module.get_functions() {
        for block in function.get_basic_blocks() {
            let mut instruction = block.get_first_instruction();
            while let Some(current) = instruction {
                instruction = current.get_next_instruction();
                if current.get_opcode() != InstructionOpcode::Call {
                    continue;
                }
                let Ok(call) = CallSiteValue::try_from(current) else {
                    continue;
                };
                let Some(callee) = call.get_called_fn_value() else {
                    continue;
                };
                if callee.get_name().to_str() != Ok(marker_name) {
                    continue;
                }
                calls.push(MarkerCall {
                    instruction: current,
                    caller: function,
                    call,
                });
            }
        }
    }
    calls
}

/// Verify `marker` is a plausible pseudo-intrinsic declaration: an external
/// (never-defined) function returning `void`, taking exactly
/// `expected_pointer_params` pointer-typed parameters and no varargs.
pub fn validate_marker_declaration(
    marker: FunctionValue<'_>,
    marker_name: &str,
    expected_pointer_params: usize,
) -> Result<(), String> {
    if marker.count_basic_blocks() != 0 || marker.get_linkage() != Linkage::External {
        return Err(format!(
            "{marker_name} must be an external pseudo-intrinsic declaration"
        ));
    }
    let type_ = marker.get_type();
    let params = type_.get_param_types();
    if type_.is_var_arg()
        || type_.get_return_type().is_some()
        || params.len() != expected_pointer_params
        || !params
            .iter()
            .all(|t| matches!(t, BasicMetadataTypeEnum::PointerType(_)))
    {
        return Err(format!(
            "{marker_name} must have type void with {expected_pointer_params} pointer parameter(s)"
        ));
    }
    Ok(())
}

/// Whether `function` is exactly a single-block "call the marker, then
/// return" wrapper — the shape a `#[used] static ... = { unsafe extern "C"
/// fn marker() { ... } marker }` retention idiom compiles down to (see
/// `cirrus_entry_marker!`'s expansion). Callers use this to identify which
/// `llvm.used` entries exist purely to keep a marker call alive through
/// optimization, so they can be dropped once the marker itself is decoded
/// and erased.
pub fn is_marker_wrapper(function: FunctionValue<'_>, marker_name: &str) -> bool {
    let blocks = function.get_basic_blocks();
    if blocks.len() != 1 {
        return false;
    }
    let mut saw_marker = false;
    let mut instruction = blocks[0].get_first_instruction();
    while let Some(current) = instruction {
        instruction = current.get_next_instruction();
        match current.get_opcode() {
            InstructionOpcode::Return => return saw_marker && instruction.is_none(),
            InstructionOpcode::Call => {
                let Ok(call) = CallSiteValue::try_from(current) else {
                    return false;
                };
                let Some(callee) = call.get_called_fn_value() else {
                    return false;
                };
                if callee.get_name().to_str() != Ok(marker_name) {
                    return false;
                }
                saw_marker = true;
            }
            _ => return false,
        }
    }
    false
}

/// Every `llvm.used` entry whose initializer points directly at one of
/// `wrappers` (keyed by raw `LLVMValueRef` address, e.g. from
/// [`is_marker_wrapper`]-identified functions) — the global retention
/// records safe to also drop once those wrappers are no longer needed.
pub fn retained_wrapper_globals<'ctx>(
    module: &Module<'ctx>,
    wrappers: &BTreeMap<usize, FunctionValue<'ctx>>,
) -> BTreeMap<usize, GlobalValue<'ctx>> {
    retained_wrapper_globals_in(module, "llvm.used", wrappers)
}

/// As [`retained_wrapper_globals`], but inspect the named LLVM retention
/// list.  Clang may use `llvm.compiler.used` while Rust commonly uses
/// `llvm.used`; a pass deleting marker wrappers must account for both.
pub fn retained_wrapper_globals_in<'ctx>(
    module: &Module<'ctx>,
    used_name: &str,
    wrappers: &BTreeMap<usize, FunctionValue<'ctx>>,
) -> BTreeMap<usize, GlobalValue<'ctx>> {
    let Some(used) = module.get_global(used_name) else {
        return BTreeMap::new();
    };
    let initializer = unsafe { LLVMGetInitializer(used.as_value_ref()) };
    if initializer.is_null() {
        return BTreeMap::new();
    }
    let mut retained = BTreeMap::new();
    for index in 0..unsafe { LLVMGetNumOperands(initializer) } as u32 {
        let entry = unsafe { LLVMGetOperand(initializer, index) };
        let Ok(global) = global_from_pointer(entry, "llvm.used marker entry") else {
            continue;
        };
        let initializer = unsafe { LLVMGetInitializer(global.as_value_ref()) };
        if initializer.is_null() {
            continue;
        }
        let points_to_wrapper = strip_pointer(initializer, "llvm.used marker initializer")
            .ok()
            .is_some_and(|value| wrappers.contains_key(&(value as usize)));
        if points_to_wrapper {
            retained.insert(global.as_value_ref() as usize, global);
        }
    }
    retained
}
