//! LLVM structural vs direct constructors.

use std::fs;
use std::io::Write;
use volar_ir_build::Pipeline;

fn write_temp_ll(name: &str, src: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir();
    let path = dir.join(format!(
        "volar-ir-build-llvm-{}-{}.ll",
        std::process::id(),
        name
    ));
    let mut f = fs::File::create(&path).expect("create ll");
    f.write_all(src.as_bytes()).expect("write ll");
    path
}

#[test]
fn llvm_direct_is_circuit() {
    let src = r#"
define i32 @add(i32 %a, i32 %b) {
entry:
  %sum = add i32 %a, %b
    ret i32 %sum
}
"#;
    let path = write_temp_ll("add", src);
    let (blocks, _types) = Pipeline::from_llvm_direct(&path, "add")
        .to_volar_ir()
        .expect("llvm direct");
    assert!(blocks.is_circuit());
    let _ = fs::remove_file(&path);
}

#[test]
fn llvm_inlined_has_no_live_body_calls() {
    let src = r#"
define i32 @callee(i32 %x) {
entry:
  %r = add i32 %x, 1
  ret i32 %r
}

define i32 @caller(i32 %x) {
entry:
  %r = call i32 @callee(i32 %x)
  ret i32 %r
}
"#;
    let path = write_temp_ll("caller", src);
    let module = Pipeline::from_llvm_inlined(&path, &["caller"])
        .to_vaffle()
        .expect("llvm inlined");
    let caller = *module.exports.get("caller").expect("caller export");
    let vaffle::FuncDecl::Body(body) = &module.funcs[caller.0] else {
        panic!("expected caller body");
    };
    let live_body_call = body.blocks.iter().any(|b| {
        b.stmts.iter().any(|vid| {
            matches!(
                &body.values[vid.0].kind,
                vaffle::Value::Call { func, .. }
                    if matches!(module.funcs.get(func.0), Some(vaffle::FuncDecl::Body(_)))
            )
        })
    });
    assert!(!live_body_call);
    let _ = fs::remove_file(&path);
}

#[test]
fn llvm_data_dependent_branch_unroll_fails_direct_fails() {
    let src = r#"
define i32 @max(i32 %a, i32 %b) {
entry:
  %cmp = icmp sgt i32 %a, %b
  br i1 %cmp, label %then, label %else
then:
  ret i32 %a
else:
  ret i32 %b
}
"#;
    let path = write_temp_ll("max", src);
    let unroll = Pipeline::from_llvm(&path, &["max"])
        .inline_vaffle_everything()
        .lower_to_volar_ir()
        .unroll_ir()
        .to_volar_ir();
    assert!(unroll.is_err(), "data-dependent branch must fail unroll");
    let direct = Pipeline::from_llvm_direct(&path, "max").to_volar_ir();
    assert!(
        direct.is_err(),
        "data-dependent branch must fail llvm-direct"
    );
    let mov = Pipeline::from_llvm(&path, &["max"])
        .lower_to_volar_ir()
        .movfuscate()
        .to_volar_ir();
    let (blocks, _) = mov.expect("movfuscate accepts symbolic CF");
    assert!(blocks.is_movfuscated());
    let _ = fs::remove_file(&path);
}
