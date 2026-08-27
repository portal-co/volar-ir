// @reliability: normal
//! End-to-end tests for Phase 4a's `LirTarget` additions on `LlvmBackend`:
//! sibling (intra-module) calls, the native `switch` primitive, and native
//! `blockaddress`/`indirectbr` via `block_addr`/`dyn_jump`.
//!
//! Mirrors `e2e.rs`'s harness: emit an object file, link against a small C
//! `main` stub, run it, and check stdout.

use std::{fs, process::Command};
use tempfile::TempDir;

use inkwell::OptimizationLevel;
use inkwell::context::Context;
use inkwell::targets::{
    CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine,
};

use volar_lir::{BranchTarget, IcmpPred, LirTarget, LirType};
use volar_llvm_backend::LlvmBackend;

fn init_target() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        Target::initialize_native(&InitializationConfig::default())
            .expect("failed to initialise native LLVM target");
    });
}

fn native_machine() -> TargetMachine {
    let triple = TargetMachine::get_default_triple();
    let target = Target::from_triple(&triple).expect("target from triple");
    target
        .create_target_machine(
            &triple,
            "generic",
            "",
            OptimizationLevel::None,
            RelocMode::Default,
            CodeModel::Default,
        )
        .expect("create target machine")
}

fn compile_and_run(backend: LlvmBackend<'_>, decl: &str, main_body: &str) -> (String, String) {
    init_target();

    let dir = TempDir::new().expect("tempdir");
    let obj_path = dir.path().join("test.o");
    let c_path = dir.path().join("main.c");
    let exe_path = dir.path().join("test");

    let module = backend.finish();
    module.verify().expect("LLVM module verify failed");
    let ir_text = module.print_to_string().to_string();
    let machine = native_machine();
    machine
        .write_to_file(&module, FileType::Object, &obj_path)
        .expect("write object file");

    let c_src = format!(
        "#include <stdio.h>\n#include <stdint.h>\n#include <stdbool.h>\n\
         {decl}\n\
         int main(void) {{\n{main_body}\n  return 0;\n}}\n"
    );
    fs::write(&c_path, &c_src).expect("write C main");

    let status = Command::new("cc")
        .args(["-o"])
        .arg(&exe_path)
        .arg(&obj_path)
        .arg(&c_path)
        .status()
        .expect("cc not found — install a C compiler");
    assert!(status.success(), "linking failed.\nC stub:\n{c_src}");

    let output = Command::new(&exe_path)
        .output()
        .expect("failed to run compiled program");
    (
        String::from_utf8(output.stdout).expect("non-UTF8 output"),
        ir_text,
    )
}

// ============================================================================
// Sibling calls: mutual recursion. `is_even` calls `is_odd` before `is_odd`
// has its own `begin_function`/`end_function` — a forward reference, backed
// by `resolve_or_declare_sibling`.
// ============================================================================

#[test]
fn sibling_call_mutual_recursion() {
    let ctx = Context::create();
    let mut b = LlvmBackend::new(&ctx, "sibling_recursion");

    let (entry, params) = b.begin_function("is_even", &[LirType::U32], Some(LirType::Bool));
    b.switch_to_block(entry);
    let n = params[0][0].clone();
    let zero = b.iconst(LirType::U32, 0);
    let is_zero = b.icmp(IcmpPred::Eq, n.clone(), zero);
    let then_block = b.create_block();
    let else_block = b.create_block();
    b.branch(
        is_zero,
        then_block,
        BranchTarget::args(vec![]),
        else_block,
        BranchTarget::args(vec![]),
    );
    b.switch_to_block(then_block);
    let t = b.iconst(LirType::Bool, 1);
    b.ret(&[t]);
    b.switch_to_block(else_block);
    let one = b.iconst(LirType::U32, 1);
    let n_minus_1 = b.sub(n, one);
    let result = b.call("is_odd", &[LirType::U32], &[n_minus_1], Some(LirType::Bool));
    b.ret(&result);
    b.end_function();

    let (entry2, params2) = b.begin_function("is_odd", &[LirType::U32], Some(LirType::Bool));
    b.switch_to_block(entry2);
    let n2 = params2[0][0].clone();
    let zero2 = b.iconst(LirType::U32, 0);
    let is_zero2 = b.icmp(IcmpPred::Eq, n2.clone(), zero2);
    let then_block2 = b.create_block();
    let else_block2 = b.create_block();
    b.branch(
        is_zero2,
        then_block2,
        BranchTarget::args(vec![]),
        else_block2,
        BranchTarget::args(vec![]),
    );
    b.switch_to_block(then_block2);
    let f = b.iconst(LirType::Bool, 0);
    b.ret(&[f]);
    b.switch_to_block(else_block2);
    let one2 = b.iconst(LirType::U32, 1);
    let n2_minus_1 = b.sub(n2, one2);
    let result2 = b.call(
        "is_even",
        &[LirType::U32],
        &[n2_minus_1],
        Some(LirType::Bool),
    );
    b.ret(&result2);
    b.end_function();

    let (out, _ir) = compile_and_run(
        b,
        "bool is_even(uint32_t); bool is_odd(uint32_t);",
        r#"  printf("%d %d %d\n", is_even(4), is_even(5), is_odd(7));"#,
    );
    assert_eq!(out.trim(), "1 0 1");
}

// ============================================================================
// `switch`: heterogeneous per-case args, native LLVM `switch` instruction.
// ============================================================================

#[test]
fn switch_heterogeneous_args() {
    let ctx = Context::create();
    let mut b = LlvmBackend::new(&ctx, "switch_test");

    let (entry, params) = b.begin_function("classify", &[LirType::U32], Some(LirType::U32));
    b.switch_to_block(entry);
    let n = params[0][0].clone();

    let one_block = b.create_block();
    let one_param = b.add_block_param(one_block, LirType::U32);
    let two_block = b.create_block();
    let two_param = b.add_block_param(two_block, LirType::U32);
    let default_block = b.create_block();
    let default_param = b.add_block_param(default_block, LirType::U32);

    b.switch_to_block(entry);
    let hundred = b.iconst(LirType::U32, 100);
    let twohundred = b.iconst(LirType::U32, 200);
    let neg1 = b.iconst(LirType::U32, -1);
    b.switch(
        n,
        &[
            (1, one_block, BranchTarget::args(vec![hundred])),
            (2, two_block, BranchTarget::args(vec![twohundred])),
        ],
        default_block,
        BranchTarget::args(vec![neg1]),
    );

    b.switch_to_block(one_block);
    b.ret(&[one_param]);
    b.switch_to_block(two_block);
    b.ret(&[two_param]);
    b.switch_to_block(default_block);
    b.ret(&[default_param]);
    b.end_function();

    let (out, ir) = compile_and_run(
        b,
        "uint32_t classify(uint32_t);",
        r#"  printf("%u %u %u\n", classify(1), classify(2), classify(9));"#,
    );
    assert_eq!(out.trim(), "100 200 4294967295");
    assert!(
        ir.contains("switch i32"),
        "expected a native LLVM `switch`, got:\n{ir}"
    );
}

// ============================================================================
// `block_addr` + `dyn_jump`: native LLVM `blockaddress`/`indirectbr`.
// ============================================================================

#[test]
fn block_addr_dyn_jump() {
    let ctx = Context::create();
    let mut b = LlvmBackend::new(&ctx, "dyn_jump_test");

    let (entry, params) = b.begin_function(
        "dispatch",
        &[LirType::Bool, LirType::U32],
        Some(LirType::U32),
    );
    let cond = params[0][0].clone();
    let n = params[1][0].clone();

    let block_a = b.create_block();
    let a_param = b.add_block_param(block_a, LirType::U32);
    let block_b = b.create_block();
    let b_param = b.add_block_param(block_b, LirType::U32);

    b.switch_to_block(entry);
    let addr_a = b.block_addr(block_a);
    let addr_b = b.block_addr(block_b);
    let chosen = b.select(cond, addr_a, addr_b);
    b.dyn_jump(chosen, &[block_a, block_b], BranchTarget::args(vec![n]));

    b.switch_to_block(block_a);
    let ten = b.iconst(LirType::U32, 10);
    let a_result = b.add(a_param, ten);
    b.ret(&[a_result]);

    b.switch_to_block(block_b);
    let twenty = b.iconst(LirType::U32, 20);
    let b_result = b.add(b_param, twenty);
    b.ret(&[b_result]);

    b.end_function();

    let (out, ir) = compile_and_run(
        b,
        "uint32_t dispatch(bool, uint32_t);",
        r#"  printf("%u %u\n", dispatch(true, 5), dispatch(false, 5));"#,
    );
    assert_eq!(out.trim(), "15 25");
    assert!(
        ir.contains("blockaddress("),
        "expected a native `blockaddress` constant, got:\n{ir}"
    );
    assert!(
        ir.contains("indirectbr "),
        "expected a native `indirectbr`, got:\n{ir}"
    );
}

// ============================================================================
// `block_addr` on the entry block is a hard error, matching LLVM's own
// restriction (`BasicBlock::get_address` returns `None` there).
// ============================================================================

#[test]
#[should_panic(expected = "cannot take the address of a function's entry block")]
fn block_addr_on_entry_block_panics() {
    let ctx = Context::create();
    let mut b = LlvmBackend::new(&ctx, "entry_block_addr");
    let (entry, _) = b.begin_function("bad", &[], Some(LirType::U32));
    b.switch_to_block(entry);
    let _ = b.block_addr(entry);
}
