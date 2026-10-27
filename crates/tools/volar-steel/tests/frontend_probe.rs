//! S1: frontend selection — drive BOTH frontends (WASM and LLVM) on the same
//! reference program (compiled from `docs/steel/reference.rs` to both
//! `reference.wasm` and `reference.ll` in one build) and measure completeness
//! + size. See `docs/steel-in-volar-plan.md` milestone S1.

use std::path::PathBuf;

use inkwell::context::Context;
use inkwell::memory_buffer::MemoryBuffer;

fn artifacts() -> PathBuf {
    // crates/tools/volar-steel -> volar-ir -> docs/steel
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../docs/steel")
}

/// The reference program's expected output for a fixed input, computed
/// natively (the spec of what the circuit must evaluate to).
fn reference_native(key: u64, value: u64, path: [u64; 4]) -> u64 {
    let mut acc = key ^ value;
    for p in path {
        acc = acc.wrapping_mul(0x9e3779b97f4a7c15) ^ p;
    }
    acc
}

#[test]
fn s1_wasm_frontend_imports_reference() {
    let wasm = std::fs::read(artifacts().join("reference.wasm")).expect("reference.wasm");
    let result = volar_wasm_circuit_import::import_wasm_bytes(
        &wasm,
        "steel_view_call",
        Default::default(),
    );
    match &result {
        Ok(artifact) => {
            eprintln!(
                "S1 WASM: OK — circuit stmts={}, params={}, outputs={}",
                artifact.circuit.stmts.len(),
                artifact.circuit.params.len(),
                artifact.circuit.outputs.len()
            );
        }
        Err(e) => {
            eprintln!("S1 WASM: FAILED — {e}");
        }
    }
    result.expect("WASM frontend should import the reference program");
}

#[test]
fn s1_llvm_frontend_imports_reference() {
    let ll = std::fs::read_to_string(artifacts().join("reference.ll")).expect("reference.ll");
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            ll.as_bytes(),
            "reference.ll",
        ))
        .expect("reference.ll parses as LLVM IR");
    let result = volar_llvm_vaffle_import::import_module(&module, &["steel_view_call"]);
    match &result {
        Ok(vaffle_module) => {
            let mut bodies = 0usize;
            let mut values = 0usize;
            for f in &vaffle_module.funcs {
                if let vaffle::FuncDecl::Body(body) = f {
                    bodies += 1;
                    values += body.values.len();
                }
            }
            eprintln!(
                "S1 LLVM: OK — funcs={}, bodies={}, total values={}",
                vaffle_module.funcs.len(),
                bodies,
                values
            );
        }
        Err(e) => {
            eprintln!("S1 LLVM: FAILED — {e:?}");
        }
    }
    result.expect("LLVM frontend should import the reference program");
}

/// The frontend decision input: run both, report both, assert both succeed on
/// the (small) reference program. The `reference_native` spec is asserted
/// against the circuit evaluation in S2 (once the step circuit is built).
#[test]
fn s1_both_frontends_report() {
    let expected = reference_native(0xdead, 0xbeef, [1, 2, 3, 4]);
    eprintln!("S1 reference native output: {reference_native:#x}", reference_native = reference_native(0xdead, 0xbeef, [1, 2, 3, 4]));
    s1_wasm_frontend_imports_reference();
    s1_llvm_frontend_imports_reference();
    let _ = expected;
}
