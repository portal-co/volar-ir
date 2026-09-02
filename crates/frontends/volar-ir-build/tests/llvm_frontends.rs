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

/// docs/llvm-array-alloca.md item 1/2: a `[16 x i8]` byte-blob alloca,
/// indexed as an array of i32 via a single-index, differently-typed
/// constant GEP (the rustc `-O0` `stack_spill` shape) -- must unroll to
/// `is_circuit()` and actually compute `x ^ (x+1)`, not just import.
#[test]
fn llvm_array_alloca_stack_spill_computes_x_xor_x_plus_1() {
    let src = r#"
define i32 @stack_spill(i32 %x) {
entry:
  %buf = alloca [16 x i8], align 4
  %p0 = getelementptr i32, ptr %buf, i64 0
  %x1 = add i32 %x, 1
  store i32 %x, ptr %p0
  %p1 = getelementptr i32, ptr %buf, i64 1
  store i32 %x1, ptr %p1
  %a = load i32, ptr %p0
  %b = load i32, ptr %p1
  %r = xor i32 %a, %b
  ret i32 %r
}
"#;
    let path = write_temp_ll("stack_spill", src);
    let (blocks, types) = Pipeline::from_llvm(&path, &["stack_spill"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.unroll_ir())
        .expect("array alloca + typed-view GEP via LLVM→VAFFLE")
        .to_volar_ir();
    assert!(blocks.is_circuit());

    let x: u64 = 5;
    let input_word: Vec<bool> = (0..64).map(|i| (x >> i) & 1 != 0).collect();
    let out = volar_fuzz::interpreter::ir::eval_ir(&blocks, &types, &[input_word])
        .expect("eval terminates");
    let r = out
        .iter()
        .enumerate()
        .fold(0u64, |acc, (i, bit)| acc | ((bit[0] as u64) << i));
    assert_eq!(r, x ^ (x + 1), "stack_spill(5) must compute 5 ^ 6");
    let _ = fs::remove_file(&path);
}

/// docs/llvm-array-alloca.md item 3: a struct alloca either flattens or
/// names a clear error -- this importer chooses the latter.
#[test]
fn llvm_struct_alloca_is_named_unsupported() {
    let src = r#"
define i32 @two_field(i32 %a, i32 %b) {
entry:
  %s = alloca { i32, i32 }, align 4
  %p0 = getelementptr { i32, i32 }, ptr %s, i32 0, i32 0
  store i32 %a, ptr %p0
  %y = load i32, ptr %p0
  ret i32 %y
}
"#;
    let path = write_temp_ll("two_field", src);
    let err = Pipeline::from_llvm(&path, &["two_field"]).expect_err("struct alloca must fail closed");
    let msg = err.to_string();
    assert!(
        msg.contains("alloca"),
        "expected a named struct-alloca error, got {msg}"
    );
    let _ = fs::remove_file(&path);
}

/// A caller's `alloca` must survive a nested call uncorrupted: the callee's
/// own frame (params/ret/spill/cont) must not be placed on top of the
/// caller's still-live alloca storage. Regression test for
/// `lower_to_ir.rs`'s `FuncInfo::alloca_budget` -- call-site SP advancement
/// must skip past the caller's own alloca budget, not just the callee's
/// own `own_layout.size` (see docs/llvm-array-alloca.md's rebasing note).
///
/// This can only check that lowering succeeds, not the computed value:
/// `unroll_ir`/`movfuscate` both reject *any* call-preserving cross-function
/// call as "not statically finite" (confirmed reproducible with zero
/// allocas involved -- a pre-existing gap in the calling convention's own
/// numeric-evaluation support, not something this task introduces or fixes).
/// `volar-vaffle-target::lower_to_ir`'s own unit test
/// `test_alloca_budget_reserved_across_nested_call` checks the actual
/// computed budget directly against a hand-built two-function module.
#[test]
fn llvm_alloca_survives_nested_call_lowers_without_panicking() {
    let src = r#"
define i32 @helper(i32 %x) {
  ret i32 %x
}

define i32 @caller(i32 %x) {
entry:
  %buf = alloca i32, align 4
  store i32 %x, ptr %buf
  %y = call i32 @helper(i32 %x)
  %v = load i32, ptr %buf
  %r = add i32 %v, %y
  ret i32 %r
}
"#;
    let path = write_temp_ll("alloca_survives_call", src);
    let (blocks, _types) = Pipeline::from_llvm(&path, &["caller"])
        .and_then(|p| p.lower_to_volar_ir())
        .expect("alloca + nested call via LLVM→VAFFLE must lower without panicking")
        .to_volar_ir();
    assert!(
        blocks.blocks.len() >= 5,
        "expected >=5 blocks (module entry + exit + caller entry + call continuation \
         + helper entry), got {}",
        blocks.blocks.len()
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

#[test]
fn llvm_switch_lowers_to_jump_table_and_movfuscates() {
    let src = r#"
define i32 @poll_fsm(i8 %state, i32 %acc) {
entry:
  switch i8 %state, label %bb4 [
    i8 0, label %bb3
    i8 1, label %bb2
  ]
bb3:
  %add = add i32 %acc, 1
  br label %bb4
bb2:
  %x = xor i32 %acc, 40503
  br label %bb4
bb4:
  %r = phi i32 [ %x, %bb2 ], [ %acc, %entry ], [ %add, %bb3 ]
  ret i32 %r
}
"#;
    let path = write_temp_ll("poll_fsm_jt", src);
    let (ir, _) = Pipeline::from_llvm(&path, &["poll_fsm"])
        .and_then(|p| p.lower_to_volar_ir())
        .expect("switch via LLVM→VAFFLE→IR")
        .to_volar_ir();
    let has_jt = ir.blocks.iter().any(|b| {
        matches!(b.terminator, volar_ir::ir::IRTerminator::JumpTable { .. })
    });
    assert!(
        has_jt,
        "VAFFLE Table must lower to IR JumpTable, not the Return catch-all"
    );
    let unroll = Pipeline::from_llvm(&path, &["poll_fsm"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.unroll_ir());
    assert!(unroll.is_err(), "symbolic switch must fail unroll");
    let (blocks, _) = Pipeline::from_llvm(&path, &["poll_fsm"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.movfuscate())
        .expect("movfuscate accepts switch")
        .to_volar_ir();
    assert!(blocks.is_movfuscated());
    let _ = fs::remove_file(&path);
}

#[test]
fn llvm_switch_movfuscated_lowers_to_boolar_and_fuses() {
    let src = r#"
define i32 @poll_fsm(i8 %state, i32 %acc) {
entry:
  switch i8 %state, label %bb4 [
    i8 0, label %bb3
    i8 1, label %bb2
  ]
bb3:
  %add = add i32 %acc, 1
  br label %bb4
bb2:
  %x = xor i32 %acc, 40503
  br label %bb4
bb4:
  %r = phi i32 [ %x, %bb2 ], [ %acc, %entry ], [ %add, %bb3 ]
  ret i32 %r
}
"#;
    let path = write_temp_ll("poll_fsm_boolar", src);
    let boolar = Pipeline::from_llvm(&path, &["poll_fsm"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.movfuscate())
        .and_then(|p| p.lower_to_boolar())
        .expect("cross-block STACK spill: see docs/llvm-stack-spill-boolar.md");
    assert!(boolar.to_boolar().blocks.len() == 1);

    let path2 = write_temp_ll("poll_fsm_boolar_fuse", src);
    let fused = Pipeline::from_llvm(&path2, &["poll_fsm"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.movfuscate())
        .and_then(|p| p.lower_to_boolar())
        .and_then(|p| p.fuse(64, volar_ir_passes::LoweringMode::Unconditional))
        .expect("fused poll_fsm must round-trip through fuse without panicking");
    let _ = fused.to_boolar_circuit();

    let _ = fs::remove_file(&path);
    let _ = fs::remove_file(&path2);
}

/// Regression test for a bug found while triaging
/// `docs/llvm-stack-spill-boolar.md`: `lower_to_ir.rs`'s `plan_functions`
/// sized a function's entry-block param unpacking off `sig.params.len()`
/// (the *count* of logical parameters, e.g. 2 for `(i8, i32)`) instead of
/// their total *bit width* (40) — silently leaving most parameter bits
/// unmapped, which `translate_stmt`'s `s(vid)` fallback then substituted
/// with a wrong-but-plausible sentinel value instead of failing loudly.
/// This affected every function with more than one bit's worth of
/// parameters and was never caught because prior tests only checked
/// circuit *shape* (`is_circuit()`), never the computed *value*.
#[test]
fn llvm_multi_bit_params_compute_correct_value() {
    let src = r#"
define i32 @xor_one(i32 %x) {
entry:
  %y = xor i32 %x, 1
  ret i32 %y
}
"#;
    let path = write_temp_ll("xor_one_value", src);
    let (blocks, types) = Pipeline::from_llvm(&path, &["xor_one"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.unroll_ir())
        .expect("register xor via LLVM→VAFFLE")
        .to_volar_ir();
    assert!(blocks.is_circuit());

    let x: u64 = 5;
    let input_word: Vec<bool> = (0..64).map(|i| (x >> i) & 1 != 0).collect();
    let out = volar_fuzz::interpreter::ir::eval_ir(&blocks, &types, &[input_word])
        .expect("eval terminates");
    let y = out
        .iter()
        .enumerate()
        .fold(0u64, |acc, (i, bit)| acc | ((bit[0] as u64) << i));
    assert_eq!(y, x ^ 1, "xor_one(5) must compute 4, not silently use garbage upper bits");
    let _ = fs::remove_file(&path);
}
