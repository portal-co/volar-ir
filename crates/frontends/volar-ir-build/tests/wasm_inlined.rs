//! WASM fully-inlined frontend via [`volar_ir_build::Pipeline`].

use std::fs;
use vaffle::{FuncDecl, Value};
use volar_ir_build::Pipeline;

fn write_temp_wasm(name: &str, wat: &str) -> std::path::PathBuf {
    let bytes = wat::parse_str(wat).expect("valid wat");
    let dir = std::env::temp_dir();
    let path = dir.join(format!(
        "volar-ir-build-wasm-{}-{}.wasm",
        std::process::id(),
        name
    ));
    fs::write(&path, bytes).expect("write wasm");
    path
}

#[test]
fn wasm_inlined_eliminates_body_calls() {
    let wat = r#"
    (module
      (func $add1 (param i32) (result i32)
        local.get 0
        i32.const 1
        i32.add)
      (func $caller (export "caller") (param i32) (result i32)
        local.get 0
        call $add1))
    "#;
    let path = write_temp_wasm("caller", wat);
    let module = Pipeline::from_wasm_inlined(&path)
        .with_inline_entries(&["caller"])
        .to_vaffle()
        .expect("wasm inlined import");
    let caller = *module.exports.get("caller").expect("caller export");
    let FuncDecl::Body(body) = &module.funcs[caller.0] else {
        panic!("expected caller body");
    };
    let live_body_call = body.blocks.iter().any(|b| {
        b.stmts.iter().any(|vid| {
            matches!(
                &body.values[vid.0].kind,
                Value::Call { func, .. }
                    if matches!(module.funcs.get(func.0), Some(FuncDecl::Body(_)))
            )
        })
    });
    assert!(
        !live_body_call,
        "inlined WASM should have no Body-to-Body calls"
    );
    let _ = fs::remove_file(&path);
}

#[test]
fn wasm_inlined_then_unroll_is_circuit_for_straight_line() {
    let wat = r#"
    (module
      (func $id (export "id") (param i32) (result i32)
        local.get 0))
    "#;
    let path = write_temp_wasm("id", wat);
    let (blocks, _types) = Pipeline::from_wasm_inlined(&path)
        .with_inline_entries(&["id"])
        .lower_to_volar_ir()
        .unroll_ir()
        .to_volar_ir()
        .expect("straight-line wasm unrolls");
    assert!(blocks.is_circuit());
    let _ = fs::remove_file(&path);
}
