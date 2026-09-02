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
        .expect("llvm direct")
        .to_volar_ir();
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
        .expect("llvm inlined")
        .to_vaffle();
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

    let (blocks, _) = Pipeline::from_llvm_inlined(&path, &["caller"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.unroll_ir())
        .expect("llvm inlined+unroll")
        .to_volar_ir();
    assert!(blocks.is_circuit());
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
        .and_then(|p| p.inline_vaffle_everything())
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.unroll_ir());
    assert!(unroll.is_err(), "data-dependent branch must fail unroll");
    let direct = Pipeline::from_llvm_direct(&path, "max");
    assert!(
        direct.is_err(),
        "data-dependent branch must fail llvm-direct"
    );
    let mov = Pipeline::from_llvm(&path, &["max"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.movfuscate());
    let (blocks, _) = mov.expect("movfuscate accepts symbolic CF").to_volar_ir();
    assert!(blocks.is_movfuscated());
    let _ = fs::remove_file(&path);
}

#[test]
fn llvm_alloca_spill_round_trips() {
    let src = r#"
define i32 @spill(i32 %x) {
entry:
  %p = alloca i32, align 4
  store i32 %x, ptr %p
  %y = load i32, ptr %p
  ret i32 %y
}
"#;
    let path = write_temp_ll("spill", src);
    let (blocks, _) = Pipeline::from_llvm(&path, &["spill"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.unroll_ir())
        .expect("alloca spill via LLVM→VAFFLE")
        .to_volar_ir();
    assert!(blocks.is_circuit());
    let _ = fs::remove_file(&path);
}

#[test]
fn llvm_alloca_symbolic_count_is_named_unsupported() {
    let src = r#"
define i32 @spill_n(i32 %x, i32 %n) {
entry:
  %p = alloca i32, i32 %n
  store i32 %x, ptr %p
  %y = load i32, ptr %p
  ret i32 %y
}
"#;
    let path = write_temp_ll("spill_n", src);
    let err = Pipeline::from_llvm(&path, &["spill_n"]).expect_err("VLA alloca must fail closed");
    let msg = err.to_string();
    assert!(
        msg.contains("alloca") && msg.contains("symbolic"),
        "expected a named symbolic-alloca error, got {msg}"
    );
    let _ = fs::remove_file(&path);
}

#[test]
fn llvm_register_xor_unrolls() {
    let src = r#"
define i32 @xor_one(i32 %x) {
entry:
  %y = xor i32 %x, 1
  ret i32 %y
}
"#;
    let path = write_temp_ll("xor_one", src);
    let (blocks, _) = Pipeline::from_llvm(&path, &["xor_one"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.unroll_ir())
        .expect("register xor via LLVM→VAFFLE")
        .to_volar_ir();
    assert!(blocks.is_circuit());
    let _ = fs::remove_file(&path);
}
