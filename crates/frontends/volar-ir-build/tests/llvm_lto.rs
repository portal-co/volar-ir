//! LTO static-library input and pre-build constructors.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use inkwell::context::Context;
use inkwell::memory_buffer::MemoryBuffer;
use volar_ir_build::{
    write_bitcode_archive, write_bitcode_archive_members, CommandBuild, Pipeline,
};

fn temp(name: &str, ext: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "volar-ir-build-lto-{}-{name}.{ext}",
        std::process::id()
    ))
}

fn ll_to_bitcode(name: &str, src: &str) -> Vec<u8> {
    let context = Context::create();
    let buf = MemoryBuffer::create_from_memory_range_copy(src.as_bytes(), name);
    let module = context
        .create_module_from_ir(buf)
        .expect("parse fixture IR");
    module.write_bitcode_to_memory().as_slice().to_vec()
}

fn add_ll() -> &'static str {
    r#"
define i32 @add(i32 %a, i32 %b) {
entry:
  %sum = add i32 %a, %b
  ret i32 %sum
}
"#
}

fn caller_callee_pair() -> (&'static str, &'static str) {
    (
        r#"
define i32 @callee(i32 %x) {
entry:
  %r = add i32 %x, 1
  ret i32 %r
}
"#,
        r#"
declare i32 @callee(i32)

define i32 @caller(i32 %x) {
entry:
  %r = call i32 @callee(i32 %x)
  ret i32 %r
}
"#,
    )
}

#[test]
fn archive_of_one_bitcode_direct_is_circuit() {
    let bc = ll_to_bitcode("add.ll", add_ll());
    let path = temp("add", "a");
    write_bitcode_archive(&path, "add.bc", &bc).expect("write archive");
    let (blocks, _) = Pipeline::from_llvm_direct(&path, "add")
        .to_volar_ir()
        .expect("direct import of LTO archive");
    assert!(blocks.is_circuit());
    let _ = fs::remove_file(&path);
}

#[test]
fn two_member_archive_inlined_has_no_live_body_calls() {
    let (callee_ll, caller_ll) = caller_callee_pair();
    let callee_bc = ll_to_bitcode("callee.ll", callee_ll);
    let caller_bc = ll_to_bitcode("caller.ll", caller_ll);
    let path = temp("pair", "a");
    write_bitcode_archive_members(
        &path,
        &[
            ("callee.bc", callee_bc.as_slice()),
            ("caller.bc", caller_bc.as_slice()),
        ],
    )
    .expect("write two-member archive");

    let module = Pipeline::from_llvm_inlined(&path, &["caller"])
        .to_vaffle()
        .expect("inlined import of linked archive");
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
fn native_only_member_is_rejected() {
    let path = temp("native", "a");
    write_bitcode_archive(&path, "empty.o", b"not bitcode and not an object")
        .expect("write junk archive");
    let err = Pipeline::from_llvm_direct(&path, "add")
        .to_volar_ir()
        .expect_err("native-only member must fail");
    assert!(
        err.to_string().contains("not LLVM bitcode"),
        "unexpected error: {err}"
    );
    let _ = fs::remove_file(&path);
}

#[test]
fn command_prebuild_copies_archive() {
    let bc = ll_to_bitcode("add.ll", add_ll());
    let src = temp("cmd-src", "a");
    write_bitcode_archive(&src, "add.bc", &bc).expect("write source archive");
    let dst = temp("cmd-dst", "a");
    let cmd = CommandBuild {
        program: PathBuf::from("cp"),
        args: vec![
            src.as_os_str().to_os_string(),
            dst.as_os_str().to_os_string(),
        ],
        output: dst.clone(),
        cwd: None,
    };
    let (blocks, _) = Pipeline::from_command_direct(cmd, "add")
        .to_volar_ir()
        .expect("command pre-build then direct import");
    assert!(blocks.is_circuit());
    let _ = fs::remove_file(&src);
    let _ = fs::remove_file(&dst);
}

#[cfg(feature = "cc")]
#[test]
fn cc_inlined_unroll_when_clang_available() {
    let compiler = cc::Build::new().try_get_compiler().ok();
    let Some(compiler) = compiler else {
        eprintln!("skipping cc LTO test: no C compiler");
        return;
    };
    if !compiler.is_like_clang() {
        eprintln!(
            "skipping cc LTO test: compiler {} is not clang-like",
            compiler.path().display()
        );
        return;
    }

    let c_path = temp("id", "c");
    {
        let mut f = fs::File::create(&c_path).expect("write c");
        f.write_all(
            br#"
int id(int x) { return x; }
"#,
        )
        .expect("write c body");
    }

    let mut build = cc::Build::new();
    build.file(&c_path);
    build.opt_level(0);
    let result = Pipeline::from_cc_inlined(build, "idlib", &["id"])
        .lower_to_volar_ir()
        .unroll_ir()
        .to_volar_ir();
    let _ = fs::remove_file(&c_path);
    let (blocks, _) = result.expect("cc LTO inlined+unroll");
    assert!(blocks.is_circuit());
}
