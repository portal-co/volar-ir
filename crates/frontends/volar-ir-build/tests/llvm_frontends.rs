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

#[test]
fn llvm_memset_stack_spill_unrolls_and_computes_x_xor_x_plus_1() {
    let src = r#"
declare void @llvm.memset.p0.i64(ptr, i8, i64, i1 immarg)

define i32 @stack_spill(i32 %x) {
entry:
  %buf = alloca [16 x i8], align 4
  call void @llvm.memset.p0.i64(ptr %buf, i8 0, i64 16, i1 false)
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
    let path = write_temp_ll("stack_spill_memset", src);
    let (blocks, types) = Pipeline::from_llvm(&path, &["stack_spill"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.unroll_ir())
        .expect("constant memset must lower before unroll")
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
    assert_eq!(r, x ^ (x + 1), "memset stack_spill(5) must compute 5 ^ 6");
    let _ = fs::remove_file(&path);
}

#[test]
fn llvm_memcpy_between_allocas_unrolls_and_preserves_value() {
    let src = r#"
declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1 immarg)

define i32 @copy(i32 %x) {
entry:
  %src = alloca [4 x i8], align 4
  %dst = alloca [4 x i8], align 4
  store i32 %x, ptr %src
  call void @llvm.memcpy.p0.p0.i64(ptr %dst, ptr %src, i64 4, i1 false)
  %out = load i32, ptr %dst
  ret i32 %out
}
"#;
    let path = write_temp_ll("memcpy_allocas", src);
    let (blocks, types) = Pipeline::from_llvm(&path, &["copy"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.unroll_ir())
        .expect("constant memcpy must lower before unroll")
        .to_volar_ir();
    assert!(blocks.is_circuit());

    let x: u64 = 0x4433_2211;
    let input_word: Vec<bool> = (0..64).map(|i| (x >> i) & 1 != 0).collect();
    let out = volar_fuzz::interpreter::ir::eval_ir(&blocks, &types, &[input_word])
        .expect("eval terminates");
    let r = out
        .iter()
        .enumerate()
        .fold(0u64, |acc, (i, bit)| acc | ((bit[0] as u64) << i));
    assert_eq!(r, x, "memcpy must preserve the source bytes");
    let _ = fs::remove_file(&path);
}

#[test]
fn llvm_memmove_same_alloca_overlap_preserves_source_bytes() {
    let src = r#"
declare void @llvm.memmove.p0.p0.i64(ptr, ptr, i64, i1 immarg)

define i32 @move_overlap(i32 %x) {
entry:
  %buf = alloca [4 x i8], align 4
  store i32 %x, ptr %buf
  %dst = getelementptr i8, ptr %buf, i64 1
  call void @llvm.memmove.p0.p0.i64(ptr %dst, ptr %buf, i64 3, i1 false)
  %out = load i32, ptr %buf
  ret i32 %out
}
"#;
    let path = write_temp_ll("memmove_overlap", src);
    let (blocks, types) = Pipeline::from_llvm(&path, &["move_overlap"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.unroll_ir())
        .expect("overlapping constant memmove must lower before unroll")
        .to_volar_ir();
    assert!(blocks.is_circuit());

    let x: u64 = 0x4433_2211;
    let input_word: Vec<bool> = (0..64).map(|i| (x >> i) & 1 != 0).collect();
    let out = volar_fuzz::interpreter::ir::eval_ir(&blocks, &types, &[input_word])
        .expect("eval terminates");
    let r = out
        .iter()
        .enumerate()
        .fold(0u64, |acc, (i, bit)| acc | ((bit[0] as u64) << i));
    assert_eq!(
        r, 0x3322_1111,
        "memmove must read its source before writing overlap"
    );
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
/// Also exercises (now fixed, see `llvm_register_xor_call_computes_correct_value`
/// for the isolated regression test) the calling convention's own numeric
/// path for a non-inlined, cross-function call: `n_params` sourced from the
/// callee's actual entry-block params (not its declared `sig`, which
/// `vaffle_ssa`'s SP-threading silently widens for any non-entry function)
/// and a multi-bit call result reaching its user via `Value::Output`
/// (previously unhandled, silently defaulting to a zero wire).
#[test]
fn llvm_alloca_survives_nested_call() {
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
    let (blocks, types) = Pipeline::from_llvm(&path, &["caller"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.unroll_ir())
        .expect("alloca + nested call via LLVM→VAFFLE")
        .to_volar_ir();
    assert!(blocks.is_circuit());

    let x: u64 = 11;
    let input_word: Vec<bool> = (0..64).map(|i| (x >> i) & 1 != 0).collect();
    let out = volar_fuzz::interpreter::ir::eval_ir(&blocks, &types, &[input_word])
        .expect("eval terminates");
    let r = out
        .iter()
        .enumerate()
        .fold(0u64, |acc, (i, bit)| acc | ((bit[0] as u64) << i));
    assert_eq!(r, x + x, "caller(11) must compute buf(11) + helper(11) = 22");
    let _ = fs::remove_file(&path);
}

/// Isolated regression test for the two bugs `llvm_alloca_survives_nested_call`
/// found in the calling convention's own numeric path (unrelated to alloca):
/// a plain two-function call chain with a real multi-bit argument and
/// return value, previously computing the wrong result (or failing to
/// unroll at all -- see `docs/llvm-array-alloca.md`'s "Cross-function call
/// numeric correctness" section for the full root-cause writeup).
#[test]
fn llvm_register_xor_call_computes_correct_value() {
    let src = r#"
define i32 @helper(i32 %x) {
  ret i32 %x
}
define i32 @caller(i32 %x) {
entry:
  %y = call i32 @helper(i32 %x)
  %r = add i32 %y, 1
  ret i32 %r
}
"#;
    let path = write_temp_ll("plain_call", src);
    let (blocks, types) = Pipeline::from_llvm(&path, &["caller"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.unroll_ir())
        .expect("plain cross-function call via LLVM→VAFFLE")
        .to_volar_ir();
    assert!(blocks.is_circuit());

    let x: u64 = 123;
    let input_word: Vec<bool> = (0..64).map(|i| (x >> i) & 1 != 0).collect();
    let out = volar_fuzz::interpreter::ir::eval_ir(&blocks, &types, &[input_word])
        .expect("eval terminates");
    let r = out
        .iter()
        .enumerate()
        .fold(0u64, |acc, (i, bit)| acc | ((bit[0] as u64) << i));
    assert_eq!(r, x + 1, "caller(123) must compute helper(123) + 1 = 124");
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
fn llvm_dead_landingpad_movfuscates_lowers_to_boolar_and_fuses() {
    let src = r#"
declare i32 @rust_eh_personality(...)
declare void @cant_unwind()

define i32 @poll_fsm(i8 %state, i32 %acc) personality ptr @rust_eh_personality {
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
terminate:
  %lp = landingpad { ptr, i32 }
          filter [0 x ptr] zeroinitializer
  call void @cant_unwind()
  unreachable
}
"#;
    let path = write_temp_ll("poll_fsm_dead_landingpad", src);
    let (original, original_types) = Pipeline::from_llvm(&path, &["poll_fsm"])
        .and_then(|p| p.lower_to_volar_ir())
        .expect("dead landingpad must not block structural import")
        .to_volar_ir();

    let (movfuscated, movfuscated_types) = Pipeline::from_llvm(&path, &["poll_fsm"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.movfuscate())
        .expect("movfuscate accepts poll_fsm with dead landingpad")
        .to_volar_ir();
    assert!(movfuscated.is_movfuscated());

    let pc_inputs = volar_ir_passes::pc_bits_needed(original.blocks.len());
    for (state, acc, expected_value) in [(0u8, 7u32, 8u32), (1, 7, 7 ^ 40503), (2, 7, 7)] {
        let packed = state as u64 | ((acc as u64) << 8);
        let original_input: Vec<bool> = (0..64).map(|i| (packed >> i) & 1 != 0).collect();
        let expected = volar_fuzz::interpreter::ir::eval_ir(
            &original,
            &original_types,
            &[original_input.clone()],
        )
        .expect("original poll_fsm terminates");
        let expected_bits = volar_fuzz::interpreter::ir::bit_flatten(&expected);
        let expected_word = expected_bits
            .iter()
            .enumerate()
            .fold(0u32, |word, (i, bit)| word | ((*bit as u32) << i));
        assert_eq!(expected_word, expected_value);

        let mut mov_inputs: Vec<Vec<bool>> = movfuscated.blocks[0]
            .params
            .iter()
            .map(|ty| vec![false; volar_fuzz::interpreter::ir::bit_width(*ty, &movfuscated_types)])
            .collect();
        mov_inputs[pc_inputs] = original_input;
        let actual =
            volar_fuzz::interpreter::ir::eval_ir(&movfuscated, &movfuscated_types, &mov_inputs)
                .expect("movfuscated poll_fsm terminates");
        assert_eq!(actual, expected, "movfuscation changed state {state}");
    }

    let boolar = Pipeline::from_llvm(&path, &["poll_fsm"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.movfuscate())
        .and_then(|p| p.lower_to_boolar())
        .expect("dead landingpad poll_fsm must lower through Boolar");
    let boolar = boolar.to_boolar();
    assert_eq!(boolar.blocks.len(), 1);
    for (state, acc) in [(0u8, 7u32), (1, 7), (2, 7)] {
        let packed = state as u64 | ((acc as u64) << 8);
        let original_input: Vec<bool> = (0..64).map(|i| (packed >> i) & 1 != 0).collect();
        let mut mov_inputs: Vec<Vec<bool>> = movfuscated.blocks[0]
            .params
            .iter()
            .map(|ty| vec![false; volar_fuzz::interpreter::ir::bit_width(*ty, &movfuscated_types)])
            .collect();
        mov_inputs[pc_inputs] = original_input.clone();
        let boolar_output = volar_fuzz::interpreter::biir::eval_biir(
            &boolar,
            &volar_fuzz::interpreter::ir::bit_flatten(&mov_inputs),
        )
        .expect("Boolar poll_fsm terminates");
        let expected =
            volar_fuzz::interpreter::ir::eval_ir(&original, &original_types, &[original_input])
                .expect("original poll_fsm terminates");
        assert_eq!(
            boolar_output,
            volar_fuzz::interpreter::ir::bit_flatten(&expected),
            "Boolar lowering changed state {state}"
        );
    }

    let path2 = write_temp_ll("poll_fsm_dead_landingpad_fuse", src);
    let fused = Pipeline::from_llvm(&path2, &["poll_fsm"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.movfuscate())
        .and_then(|p| p.lower_to_boolar())
        .and_then(|p| p.fuse(64, volar_ir_passes::LoweringMode::Unconditional))
        .expect("fused dead-landingpad poll_fsm must round-trip through fuse without panicking");
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
