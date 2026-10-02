//! WASM fully-inlined frontend via [`volar_ir_build::Pipeline`].

use std::fs;
use vaffle::{FuncDecl, PointerWidth, Value};
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
    // `caller` is the module's only export, so the default (every export)
    // inline-root set is equivalent to the old explicit `&["caller"]` list.
    let module = Pipeline::from_wasm_inlined(&path)
        .expect("wasm inlined import")
        .to_vaffle();
    assert_eq!(module.pointer_width, PointerWidth::Bits32);
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
fn wasm_field_mul_matches_fips_197() {
    let wat = r#"
    (module
      (func $mul (import "env" "volar.field.mul.d8.bit.p1b") (param i32 i32) (result i32))
      (func (export "main") (param i32 i32) (result i32)
        local.get 0
        local.get 1
        call $mul))
    "#;
    let path = write_temp_wasm("field-mul", wat);
    let (blocks, types) = Pipeline::from_wasm_inlined(&path)
        .expect("wasm field import")
        .lower_to_volar_ir()
        .and_then(|pipeline| pipeline.unroll_ir())
        .expect("field mul unrolls")
        .to_volar_ir();
    assert!(blocks.is_circuit());
    // Entry params are one `Vec(64, Bit)` word: the two i32s, LSB-first.
    let mut input = vec![false; 64];
    for bit in 0..32 {
        input[bit] = (0x57u32 >> bit) & 1 != 0;
        input[32 + bit] = (0x13u32 >> bit) & 1 != 0;
    }
    let out = volar_fuzz::interpreter::ir::eval_ir(&blocks, &types, &[input])
        .expect("field mul evaluates");
    let word = out.iter().enumerate().fold(0u32, |acc, (bit, value)| {
        acc | ((value[0] as u32) << bit)
    });
    assert_eq!(word, 0xfe);
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
        .expect("wasm parse+inline")
        .lower_to_volar_ir()
        .and_then(|p| p.unroll_ir())
        .expect("straight-line wasm unrolls")
        .to_volar_ir();
    assert!(blocks.is_circuit());
    let _ = fs::remove_file(&path);
}
