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
fn llvm_slice_get_pointer_param_gep_movfuscates() {
    // The exact motivating shape of the ConstChain-fallback plan: a pointer
    // *parameter* GEP'd with a *symbolic* index, then loaded through --
    // `fn slice_get(xs: &[i32], i: usize) -> i32 { xs[i] }`. Previously
    // named ConstChain (`docs/llvm-const-cache-dominance.md`'s own
    // "Measured" table: "rustc `-O0` `xs[i]` pointer-param GEP"). Confirms
    // the full import -> lower_to_volar_ir -> movfuscate pipeline accepts
    // the runtime-dispatch shape this now produces, end to end.
    let src = r#"
define i32 @slice_get(ptr %xs, i64 %i) {
entry:
  %p = getelementptr i32, ptr %xs, i64 %i
  %v = load i32, ptr %p
  ret i32 %v
}
"#;
    let path = write_temp_ll("slice_get", src);
    let mov = Pipeline::from_llvm(&path, &["slice_get"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.movfuscate());
    let (blocks, _) = mov
        .expect("xs[i] through a pointer parameter must movfuscate")
        .to_volar_ir();
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
fn llvm_64_bit_pointer_spill_loads_through_boolar_and_reversible() {
    // `load ptr` is not an integer cast: this fixture stores an address in a
    // stack slot, reloads the 64-bit pointer, and dereferences it. It covers
    // the VAFFLE call-frame address ABI as well as Boolar/reversible storage.
    let src = r#"
target datalayout = "e-p:64:64"

define i64 @pointer_spill(i64 %x) {
entry:
  %value = alloca i64, align 8
  %slot = alloca ptr, align 8
  store i64 %x, ptr %value, align 8
  store ptr %value, ptr %slot, align 8
  %loaded_ptr = load ptr, ptr %slot, align 8
  %result = load i64, ptr %loaded_ptr, align 8
  ret i64 %result
}
"#;
    let path = write_temp_ll("pointer_spill_64", src);
    let (blocks, types) = Pipeline::from_llvm(&path, &["pointer_spill"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.unroll_ir())
        .expect("64-bit pointer spill must lower to a circuit")
        .to_volar_ir();
    assert!(blocks.is_circuit());

    let x = 0x8877_6655_4433_2211u64;
    let input = (0..64).map(|bit| (x >> bit) & 1 != 0).collect();
    let out = volar_fuzz::interpreter::ir::eval_ir(&blocks, &types, &[input])
        .expect("64-bit pointer spill evaluation terminates");
    let value = out
        .iter()
        .enumerate()
        .fold(0u64, |word, (bit, value)| word | ((value[0] as u64) << bit));
    assert_eq!(value, x, "dereference through reloaded pointer preserves x");

    let reversible = Pipeline::from_llvm(&path, &["pointer_spill"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.unroll_ir())
        .and_then(|p| p.lower_to_boolar())
        .and_then(|p| p.fuse(1, volar_ir_passes::LoweringMode::Unconditional))
        .and_then(|p| p.to_reversible())
        .expect("64-bit pointer spill reaches reversible lowering")
        .to_rcircuit();
    assert!(
        reversible.gates().iter().any(|gate| {
            matches!(
                gate,
                volar_ir::rcircuit::RGate::StorageSwap { addr, .. } if addr.len() == 70
            )
        }),
        "a 64-bit storage value must retain its 64-bit address plus 6-bit cell suffix"
    );
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
fn llvm_symbolic_memcpy_builds_cfg_and_step_circuit() {
    // The length is an ordinary runtime value. The importer emits a CFG loop,
    // which interprets directly and becomes the pipeline's step circuit when
    // movfuscated; it is deliberately not combinationally unrolled.
    let src = r#"
declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1 immarg)

define i32 @copy_prefix(i64 %n, i32 %src) {
entry:
  %src_buf = alloca [4 x i8], align 4
  %dst_buf = alloca [4 x i8], align 4
  store i32 %src, ptr %src_buf, align 4
  store i32 0, ptr %dst_buf, align 4
  call void @llvm.memcpy.p0.p0.i64(ptr %dst_buf, ptr %src_buf, i64 %n, i1 false)
  %out = load i32, ptr %dst_buf, align 4
  ret i32 %out
}
"#;
    let path = write_temp_ll("symbolic_memcpy", src);
    let (blocks, types) = Pipeline::from_llvm(&path, &["copy_prefix"])
        .and_then(|p| p.lower_to_volar_ir())
        .expect("symbolic memcpy must lower to a CFG")
        .to_volar_ir();
    assert!(
        !blocks.is_circuit(),
        "runtime copy must retain control flow"
    );

    let bits = |value: u64, width: usize| {
        (0..width)
            .map(|bit| (value >> bit) & 1 != 0)
            .collect::<Vec<bool>>()
    };

    let (movfuscated, movfuscated_types) = Pipeline::from_llvm(&path, &["copy_prefix"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.movfuscate())
        .expect("symbolic memcpy CFG must movfuscate into a step circuit")
        .to_volar_ir();
    assert!(movfuscated.is_movfuscated());
    let pc_inputs = volar_ir_passes::pc_bits_needed(blocks.blocks.len());

    for (n, expected) in [
        (0, 0x0000_0000u32),
        (1, 0x0000_0011),
        (2, 0x0000_2211),
        (3, 0x0033_2211),
    ] {
        let original_input = vec![bits(n, 64), bits(0x4433_2211, 32)];
        let original_result =
            volar_fuzz::interpreter::ir::eval_ir(&blocks, &types, &original_input)
                .expect("symbolic memcpy evaluation terminates for an in-bounds length");
        let value = original_result
            .iter()
            .enumerate()
            .fold(0u32, |word, (bit, value)| word | ((value[0] as u32) << bit));
        assert_eq!(value, expected, "copy_prefix({n})");

        let mut mov_inputs: Vec<Vec<bool>> = movfuscated.blocks[0]
            .params
            .iter()
            .map(|ty| vec![false; volar_fuzz::interpreter::ir::bit_width(*ty, &movfuscated_types)])
            .collect();
        for (i, input_word) in original_input.into_iter().enumerate() {
            mov_inputs[pc_inputs + i] = input_word;
        }
        let movfuscated_result =
            volar_fuzz::interpreter::ir::eval_ir(&movfuscated, &movfuscated_types, &mov_inputs)
                .expect("movfuscated symbolic memcpy evaluation terminates");
        assert_eq!(
            movfuscated_result, original_result,
            "movfuscation changed copy_prefix({n})"
        );
    }
    let _ = fs::remove_file(&path);
}

#[test]
fn llvm_symbolic_memset_builds_cfg_and_step_circuit() {
    // Like memcpy, a runtime memset length remains an actual CFG loop until
    // movfuscation turns it into a reversible step circuit.
    let src = r#"
declare void @llvm.memset.p0.i64(ptr, i8, i64, i1 immarg)

define i32 @zero_prefix(i64 %n, i32 %src) {
entry:
  %buf = alloca [4 x i8], align 4
  store i32 %src, ptr %buf, align 4
  call void @llvm.memset.p0.i64(ptr %buf, i8 0, i64 %n, i1 false)
  %out = load i32, ptr %buf, align 4
  ret i32 %out
}
"#;
    let path = write_temp_ll("symbolic_memset", src);
    let (blocks, types) = Pipeline::from_llvm(&path, &["zero_prefix"])
        .and_then(|p| p.lower_to_volar_ir())
        .expect("symbolic memset must lower to a CFG")
        .to_volar_ir();
    assert!(
        !blocks.is_circuit(),
        "runtime memset must retain control flow"
    );

    let bits = |value: u64, width: usize| {
        (0..width)
            .map(|bit| (value >> bit) & 1 != 0)
            .collect::<Vec<bool>>()
    };

    let (movfuscated, movfuscated_types) = Pipeline::from_llvm(&path, &["zero_prefix"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.movfuscate())
        .expect("symbolic memset CFG must movfuscate into a step circuit")
        .to_volar_ir();
    assert!(movfuscated.is_movfuscated());
    let pc_inputs = volar_ir_passes::pc_bits_needed(blocks.blocks.len());

    for (n, expected) in [
        (0, 0x4433_2211u32),
        (1, 0x4433_2200),
        (2, 0x4433_0000),
        (3, 0x4400_0000),
    ] {
        let original_input = vec![bits(n, 64), bits(0x4433_2211, 32)];
        let original_result =
            volar_fuzz::interpreter::ir::eval_ir(&blocks, &types, &original_input)
                .expect("symbolic memset evaluation terminates for an in-bounds length");
        let value = original_result
            .iter()
            .enumerate()
            .fold(0u32, |word, (bit, value)| word | ((value[0] as u32) << bit));
        assert_eq!(value, expected, "zero_prefix({n})");

        let mut mov_inputs: Vec<Vec<bool>> = movfuscated.blocks[0]
            .params
            .iter()
            .map(|ty| vec![false; volar_fuzz::interpreter::ir::bit_width(*ty, &movfuscated_types)])
            .collect();
        for (i, input_word) in original_input.into_iter().enumerate() {
            mov_inputs[pc_inputs + i] = input_word;
        }
        let movfuscated_result =
            volar_fuzz::interpreter::ir::eval_ir(&movfuscated, &movfuscated_types, &mov_inputs)
                .expect("movfuscated symbolic memset evaluation terminates");
        assert_eq!(
            movfuscated_result, original_result,
            "movfuscation changed zero_prefix({n})"
        );
    }
    let _ = fs::remove_file(&path);
}

#[test]
fn llvm_constant_memcpy_through_symbolic_stack_gep_preserves_value() {
    let src = r#"
declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1 immarg)

define i32 @copy_at(i64 %i, i32 %src) {
entry:
  %src_buf = alloca [4 x i8], align 4
  %buf = alloca [64 x i8], align 1
  store i32 %src, ptr %src_buf, align 4
  %p = getelementptr inbounds i8, ptr %buf, i64 %i
  call void @llvm.memcpy.p0.p0.i64(ptr %p, ptr %src_buf, i64 4, i1 false)
  %out = load i32, ptr %p, align 1
  ret i32 %out
}
"#;
    let path = write_temp_ll("symbolic_stack_memcpy", src);
    let (original, original_types) = Pipeline::from_llvm(&path, &["copy_at"])
        .and_then(|p| p.lower_to_volar_ir())
        .expect("constant memcpy through symbolic stack GEP must lower")
        .to_volar_ir();
    assert!(
        !original.is_circuit(),
        "symbolic address must retain runtime work"
    );

    let (movfuscated, movfuscated_types) = Pipeline::from_llvm(&path, &["copy_at"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.movfuscate())
        .expect("symbolic stack memcpy must movfuscate")
        .to_volar_ir();
    assert!(movfuscated.is_movfuscated());

    let bits = |value: u64, width: usize| {
        (0..width)
            .map(|bit| (value >> bit) & 1 != 0)
            .collect::<Vec<bool>>()
    };
    let pc_inputs = volar_ir_passes::pc_bits_needed(original.blocks.len());
    for (index, src) in [(0u64, 0x4433_2211u32), (17, 0xBBAA_9988), (60, 0xDEAD_BEEF)] {
        let original_input = vec![bits(index, 64), bits(src as u64, 32)];
        let original_result =
            volar_fuzz::interpreter::ir::eval_ir(&original, &original_types, &original_input)
                .expect("in-bounds symbolic stack memcpy must terminate");
        let value = original_result
            .iter()
            .enumerate()
            .fold(0u32, |word, (bit, value)| word | ((value[0] as u32) << bit));
        assert_eq!(value, src, "copy_at({index})");

        let mut mov_inputs: Vec<Vec<bool>> = movfuscated.blocks[0]
            .params
            .iter()
            .map(|ty| vec![false; volar_fuzz::interpreter::ir::bit_width(*ty, &movfuscated_types)])
            .collect();
        for (i, input_word) in original_input.into_iter().enumerate() {
            mov_inputs[pc_inputs + i] = input_word;
        }
        let movfuscated_result =
            volar_fuzz::interpreter::ir::eval_ir(&movfuscated, &movfuscated_types, &mov_inputs)
                .expect("movfuscated symbolic stack memcpy must terminate");
        assert_eq!(
            movfuscated_result, original_result,
            "movfuscation changed copy_at({index})"
        );
    }
    let _ = fs::remove_file(&path);
}

#[test]
fn llvm_null_pointer_compares_without_aliasing_stack_zero() {
    let src = r#"
define i1 @is_null(ptr %p) {
entry:
  %z = icmp eq ptr %p, null
  ret i1 %z
}
"#;
    let path = write_temp_ll("const_null", src);
    let (blocks, types) = Pipeline::from_llvm(&path, &["is_null"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.unroll_ir())
        .expect("null comparison must lower before unroll")
        .to_volar_ir();
    assert!(blocks.is_circuit());

    // An LLVM module without a data layout uses the importer's 64-bit
    // default address-space ABI.  Null is the unmatched tagged-global
    // pattern, so its provenance bit is the top ABI bit.
    let null = (0..64).map(|bit| bit == 63).collect::<Vec<bool>>();
    let stack_zero = vec![false; 64];
    for (pointer, expected) in [(null, true), (stack_zero, false)] {
        let out = volar_fuzz::interpreter::ir::eval_ir(&blocks, &types, &[pointer])
            .expect("null comparison evaluation terminates");
        assert_eq!(out, vec![vec![expected]]);
    }
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
    let err =
        Pipeline::from_llvm(&path, &["two_field"]).expect_err("struct alloca must fail closed");
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
    assert_eq!(
        r,
        x + x,
        "caller(11) must compute buf(11) + helper(11) = 22"
    );
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
fn llvm_direct_tail_call_return_computes_correct_value() {
    let src = r#"
define i32 @helper(i32 %x) {
entry:
  %r = add i32 %x, 1
  ret i32 %r
}

define i32 @caller(i32 %x) {
entry:
  %r = call i32 @helper(i32 %x)
  ret i32 %r
}
"#;
    let path = write_temp_ll("tail_call", src);
    let (blocks, types) = Pipeline::from_llvm(&path, &["caller"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.unroll_ir())
        .expect("call returned directly must lower as a tail call")
        .to_volar_ir();
    assert!(blocks.is_circuit());

    let input_word: Vec<bool> = (0..64).map(|i| (5u64 >> i) & 1 != 0).collect();
    let out = volar_fuzz::interpreter::ir::eval_ir(&blocks, &types, &[input_word])
        .expect("tail-call evaluation terminates");
    let value = out
        .iter()
        .enumerate()
        .fold(0u64, |word, (i, bit)| word | ((bit[0] as u64) << i));
    assert_eq!(value, 6, "caller(5) must tail-call helper(5) and return 6");
    let _ = fs::remove_file(&path);
}

#[test]
fn llvm_overflow_panic_unreachable_imports_but_unroll_fails_closed() {
    let src = r#"
declare { i32, i1 } @llvm.sadd.with.overflow.i32(i32, i32)
declare void @panic_const_add_overflow()

define i32 @add_one(i32 %x) {
entry:
  %pair = call { i32, i1 } @llvm.sadd.with.overflow.i32(i32 %x, i32 1)
  %value = extractvalue { i32, i1 } %pair, 0
  %overflow = extractvalue { i32, i1 } %pair, 1
  br i1 %overflow, label %panic, label %ok
panic:
  call void @panic_const_add_overflow()
  unreachable
ok:
  ret i32 %value
}
"#;
    let path = write_temp_ll("overflow_panic_unreachable", src);
    let lowered = Pipeline::from_llvm(&path, &["add_one"])
        .and_then(|p| p.lower_to_volar_ir())
        .expect("reachable call; unreachable must lower through ReturnCall");
    let unroll = lowered.unroll_ir();
    assert!(
        unroll.is_err(),
        "symbolic overflow branch must remain non-finite for unroll"
    );
    let _ = fs::remove_file(&path);
}

#[test]
fn llvm_import_return_call_movfuscates_via_abort_sink() {
    let src = r#"
declare { i32, i1 } @llvm.sadd.with.overflow.i32(i32, i32)
declare void @panic_const_add_overflow()

define i32 @add_one(i32 %x) {
entry:
  %pair = call { i32, i1 } @llvm.sadd.with.overflow.i32(i32 %x, i32 1)
  %value = extractvalue { i32, i1 } %pair, 0
  %overflow = extractvalue { i32, i1 } %pair, 1
  br i1 %overflow, label %panic, label %ok
panic:
  call void @panic_const_add_overflow()
  unreachable
ok:
  ret i32 %value
}
"#;
    let path = write_temp_ll("import_return_call", src);
    let (original, _) = Pipeline::from_llvm(&path, &["add_one"])
        .and_then(|p| p.lower_to_volar_ir())
        .expect("call; unreachable must lower through the import abort sink")
        .to_volar_ir();
    let (movfuscated, types) = Pipeline::from_llvm(&path, &["add_one"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.movfuscate())
        .expect("movfuscate must not jump past the import abort sink")
        .to_volar_ir();
    assert!(movfuscated.is_movfuscated());

    let pc_inputs = volar_ir_passes::pc_bits_needed(original.blocks.len());
    let evaluate = |x: u64| {
        let mut inputs: Vec<Vec<bool>> = movfuscated.blocks[0]
            .params
            .iter()
            .map(|ty| vec![false; volar_fuzz::interpreter::ir::bit_width(*ty, &types)])
            .collect();
        inputs[pc_inputs] = (0..64).map(|i| (x >> i) & 1 != 0).collect();
        let out = volar_fuzz::interpreter::ir::eval_ir(&movfuscated, &types, &inputs)
            .expect("movfuscated import-return-call fixture terminates");
        out.iter()
            .enumerate()
            .fold(0u64, |value, (i, bit)| value | ((bit[0] as u64) << i))
    };
    assert_eq!(
        evaluate(5),
        6,
        "the non-abort path must still compute add_one"
    );
    assert_eq!(
        evaluate(i32::MAX as u64),
        0,
        "the modeled abort path returns zero-valued entry results"
    );
    let _ = fs::remove_file(&path);
}

#[test]
fn llvm_import_call_movfuscates_via_abort_sink() {
    let src = r#"
declare i32 @opaque(i32)

define i32 @caller(i32 %x) {
entry:
  %result = call i32 @opaque(i32 %x)
  %after = add i32 %result, 1
  ret i32 %after
}
"#;
    let path = write_temp_ll("import_call", src);
    let (original, _) = Pipeline::from_llvm(&path, &["caller"])
        .and_then(|p| p.lower_to_volar_ir())
        .expect("ordinary call to an import must lower to its abort sink")
        .to_volar_ir();
    let (movfuscated, types) = Pipeline::from_llvm(&path, &["caller"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.movfuscate())
        .expect("ordinary call to an import must not jump past the abort sink")
        .to_volar_ir();
    assert!(movfuscated.is_movfuscated());

    let pc_inputs = volar_ir_passes::pc_bits_needed(original.blocks.len());
    let mut inputs: Vec<Vec<bool>> = movfuscated.blocks[0]
        .params
        .iter()
        .map(|ty| vec![false; volar_fuzz::interpreter::ir::bit_width(*ty, &types)])
        .collect();
    inputs[pc_inputs] = (0..64).map(|i| (5u64 >> i) & 1 != 0).collect();
    let out = volar_fuzz::interpreter::ir::eval_ir(&movfuscated, &types, &inputs)
        .expect("movfuscated import-call fixture terminates");
    assert!(
        out.iter().all(|bit| !bit[0]),
        "an import sink must terminate with zero-valued entry results"
    );
    let _ = fs::remove_file(&path);
}

#[test]
fn llvm_const_literal_in_sibling_blocks_lowers_movfuscates_and_fuses() {
    let src = r#"
define i32 @opt_join(i1 %flag, i32 %a, i32 %b) {
entry:
  %slot = alloca i32, align 4
  br i1 %flag, label %some_a, label %some_b
some_a:
  store i32 1, ptr %slot
  %a_minus_one = sub i32 %a, 1
  br label %join
some_b:
  store i32 1, ptr %slot
  %b_minus_one = sub i32 %b, 1
  br label %join
join:
  %result = phi i32 [ %a_minus_one, %some_a ], [ %b_minus_one, %some_b ]
  ret i32 %result
}
"#;
    let path = write_temp_ll("const_literal_siblings", src);
    let (original, original_types) = Pipeline::from_llvm(&path, &["opt_join"])
        .and_then(|p| p.lower_to_volar_ir())
        .expect("sibling literals must lower without a VAFFLE dominance panic")
        .to_volar_ir();
    let (movfuscated, movfuscated_types) = Pipeline::from_llvm(&path, &["opt_join"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.movfuscate())
        .expect("sibling literals must movfuscate")
        .to_volar_ir();
    assert!(movfuscated.is_movfuscated());

    let input = |flag: bool, a: u32, b: u32| {
        let mut packed = vec![vec![false; 64]; 2];
        packed[0][0] = flag;
        for (value, offset) in [(a as u64, 1usize), (b as u64, 33)] {
            for bit in 0..32 {
                let position = offset + bit;
                packed[position / 64][position % 64] = (value >> bit) & 1 != 0;
            }
        }
        packed
    };
    let pc_inputs = volar_ir_passes::pc_bits_needed(original.blocks.len());
    for (flag, expected) in [(true, 4u64), (false, 8u64)] {
        let original_input = input(flag, 5, 9);
        let original_result =
            volar_fuzz::interpreter::ir::eval_ir(&original, &original_types, &original_input)
                .expect("original opt_join terminates");
        let original_value = original_result
            .iter()
            .enumerate()
            .fold(0u64, |value, (i, bit)| value | ((bit[0] as u64) << i));
        assert_eq!(original_value, expected, "opt_join({flag}, 5, 9)");

        let mut mov_inputs: Vec<Vec<bool>> = movfuscated.blocks[0]
            .params
            .iter()
            .map(|ty| vec![false; volar_fuzz::interpreter::ir::bit_width(*ty, &movfuscated_types)])
            .collect();
        for (i, input_word) in original_input.into_iter().enumerate() {
            mov_inputs[pc_inputs + i] = input_word;
        }
        let movfuscated_result =
            volar_fuzz::interpreter::ir::eval_ir(&movfuscated, &movfuscated_types, &mov_inputs)
                .expect("movfuscated opt_join terminates");
        assert_eq!(
            movfuscated_result, original_result,
            "movfuscation changed opt_join({flag}, 5, 9)"
        );
    }

    let fused = Pipeline::from_llvm(&path, &["opt_join"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.movfuscate())
        .and_then(|p| p.lower_to_boolar())
        .and_then(|p| p.fuse(64, volar_ir_passes::LoweringMode::Unconditional))
        .expect("sibling literal fixture must lower through fuse");
    let _ = fused.to_boolar_circuit();
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
    let has_jt = ir
        .blocks
        .iter()
        .any(|b| matches!(b.terminator, volar_ir::ir::IRTerminator::JumpTable { .. }));
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
    assert_eq!(
        y,
        x ^ 1,
        "xor_one(5) must compute 4, not silently use garbage upper bits"
    );
    let _ = fs::remove_file(&path);
}

#[test]
fn llvm_overflow_extractvalue_result_unrolls_and_computes() {
    let src = r#"
declare { i32, i1 } @llvm.sadd.with.overflow.i32(i32, i32)

define i32 @add_one(i32 %x) {
entry:
  %pair = call { i32, i1 } @llvm.sadd.with.overflow.i32(i32 %x, i32 1)
  %value = extractvalue { i32, i1 } %pair, 0
  ret i32 %value
}
"#;
    let path = write_temp_ll("overflow_result", src);
    let (blocks, types) = Pipeline::from_llvm(&path, &["add_one"])
        .and_then(|p| p.lower_to_volar_ir())
        .and_then(|p| p.unroll_ir())
        .expect("overflow result extraction must lower before unroll")
        .to_volar_ir();
    assert!(blocks.is_circuit());

    let input_word: Vec<bool> = (0..64).map(|i| (5u64 >> i) & 1 != 0).collect();
    let out = volar_fuzz::interpreter::ir::eval_ir(&blocks, &types, &[input_word])
        .expect("overflow result evaluation terminates");
    let value = out
        .iter()
        .enumerate()
        .fold(0u64, |word, (i, bit)| word | ((bit[0] as u64) << i));
    assert_eq!(
        value, 6,
        "sadd.with.overflow result field must be wrapping x + 1"
    );
    let _ = fs::remove_file(&path);
}

#[test]
fn llvm_overflow_extractvalue_flags_are_correct() {
    let src = r#"
declare { i8, i1 } @llvm.sadd.with.overflow.i8(i8, i8)
declare { i8, i1 } @llvm.uadd.with.overflow.i8(i8, i8)
declare { i8, i1 } @llvm.ssub.with.overflow.i8(i8, i8)
declare { i8, i1 } @llvm.usub.with.overflow.i8(i8, i8)
declare { i8, i1 } @llvm.smul.with.overflow.i8(i8, i8)
declare { i8, i1 } @llvm.umul.with.overflow.i8(i8, i8)

define i1 @sadd_flag(i8 %x) {
entry:
  %pair = call { i8, i1 } @llvm.sadd.with.overflow.i8(i8 %x, i8 1)
  %overflow = extractvalue { i8, i1 } %pair, 1
  ret i1 %overflow
}

define i1 @uadd_flag(i8 %x) {
entry:
  %pair = call { i8, i1 } @llvm.uadd.with.overflow.i8(i8 %x, i8 1)
  %overflow = extractvalue { i8, i1 } %pair, 1
  ret i1 %overflow
}

define i1 @ssub_flag(i8 %x) {
entry:
  %pair = call { i8, i1 } @llvm.ssub.with.overflow.i8(i8 %x, i8 1)
  %overflow = extractvalue { i8, i1 } %pair, 1
  ret i1 %overflow
}

define i1 @usub_flag(i8 %x) {
entry:
  %pair = call { i8, i1 } @llvm.usub.with.overflow.i8(i8 %x, i8 1)
  %overflow = extractvalue { i8, i1 } %pair, 1
  ret i1 %overflow
}

define i1 @smul_flag(i8 %x) {
entry:
  %pair = call { i8, i1 } @llvm.smul.with.overflow.i8(i8 %x, i8 2)
  %overflow = extractvalue { i8, i1 } %pair, 1
  ret i1 %overflow
}

define i1 @umul_flag(i8 %x) {
entry:
  %pair = call { i8, i1 } @llvm.umul.with.overflow.i8(i8 %x, i8 16)
  %overflow = extractvalue { i8, i1 } %pair, 1
  ret i1 %overflow
}
"#;
    let path = write_temp_ll("overflow_flags", src);
    for (entry, input, expected) in [
        ("sadd_flag", 5u64, false),
        ("sadd_flag", 127, true),
        ("uadd_flag", 255, true),
        ("ssub_flag", 128, true),
        ("usub_flag", 0, true),
        ("smul_flag", 64, true),
        ("umul_flag", 16, true),
    ] {
        let (blocks, types) = Pipeline::from_llvm(&path, &[entry])
            .and_then(|p| p.lower_to_volar_ir())
            .and_then(|p| p.unroll_ir())
            .unwrap_or_else(|err| panic!("{entry} must lower: {err}"))
            .to_volar_ir();
        assert!(blocks.is_circuit());
        let input_word: Vec<bool> = (0..64).map(|i| (input >> i) & 1 != 0).collect();
        let out = volar_fuzz::interpreter::ir::eval_ir(&blocks, &types, &[input_word])
            .expect("overflow flag evaluation must terminate");
        assert_eq!(out, vec![vec![expected]], "{entry}({input})");
    }
    let _ = fs::remove_file(&path);
}
