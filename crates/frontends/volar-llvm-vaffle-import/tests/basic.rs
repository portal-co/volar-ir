//! Structural-import fixture tests: hand-written `.ll` sources, imported via
//! [`volar_llvm_vaffle_import::import_module`], asserted against the
//! resulting `vaffle::Module`'s shape — call preservation (no inlining),
//! block/phi/param structure, and terminator kinds.

use inkwell::context::Context;
use inkwell::memory_buffer::MemoryBuffer;
use vaffle::{FuncDecl, PointerWidth, Terminator, Value};
use volar_ir_common::{
    ActionExecutionPolicy, ExternalExecutor, ExternalRevealPolicy, OracleExecutionKind,
    OracleExecutionPolicy, Stmt, StorageId,
};
use volar_llvm_vaffle_import::{
    LlvmImportConfig, import_module, import_module_inlined, import_module_with_config,
};

fn parse(source: &str) -> Context {
    let context = Context::create();
    context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    context
}

#[test]
fn configured_llvm_oracle_reuses_one_declaration() {
    let source = r#"
declare i32 @pure(i32)
define i32 @entry(i32 %x) {
entry:
  %a = call i32 @pure(i32 %x)
  %b = call i32 @pure(i32 %a)
  ret i32 %b
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "extern-consistency.ll",
        ))
        .unwrap();
    let imported = import_module_with_config(
        &module,
        &["entry"],
        LlvmImportConfig::default().with_oracle_execution(
            "pure",
            OracleExecutionPolicy {
                execution: OracleExecutionKind::Assigned,
                executor: ExternalExecutor::Evaluator,
                reveal: ExternalRevealPolicy::BothRoles,
                fingerprint: [0x11; 32],
            },
        ),
    )
    .expect("repeated calls reuse one declaration");
    assert_eq!(imported.oracles.len(), 1);
}

#[test]
fn configured_llvm_external_rejects_variadic_declaration() {
    let source = r#"
declare i32 @pure(i32, ...)
define i32 @entry(i32 %x) {
entry:
  %result = call i32 (i32, ...) @pure(i32 %x)
  ret i32 %result
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "variadic-external.ll",
        ))
        .unwrap();
    let error = import_module_with_config(
        &module,
        &["entry"],
        LlvmImportConfig::default()
            .with_oracle_execution("pure", OracleExecutionPolicy::legacy_evaluator()),
    )
    .expect_err("configured externals must have a fixed ABI");
    assert!(error.to_string().contains("must not be variadic"));
}

#[test]
fn configured_llvm_external_rejects_defined_callee() {
    let source = r#"
define i32 @pure(i32 %x) {
entry:
  ret i32 %x
}
define i32 @entry(i32 %x) {
entry:
  %result = call i32 @pure(i32 %x)
  ret i32 %result
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "defined-external.ll",
        ))
        .unwrap();
    let error = import_module_with_config(
        &module,
        &["entry"],
        LlvmImportConfig::default()
            .with_oracle_execution("pure", OracleExecutionPolicy::legacy_evaluator()),
    )
    .expect_err("configured externals must not shadow defined LLVM functions");
    assert!(error.to_string().contains("must target a declaration"));
}

#[test]
fn configured_llvm_external_declaration_must_match_registered_abi() {
    let source = r#"
declare i32 @act(i1, i32, i32)
define i32 @entry(i1 %guard, i32 %arg, i32 %fallback) {
entry:
  %a = call i32 @act(i1 %guard, i32 %arg, i32 %fallback)
  ret i32 %a
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "extern-action-decl.ll",
        ))
        .unwrap();
    let error = import_module_with_config(
        &module,
        &["entry"],
        LlvmImportConfig::default().with_action_execution(
            "act",
            2,
            ActionExecutionPolicy::legacy_evaluator(),
        ),
    )
    .expect_err("the registered action ABI must match the declaration");
    assert!(error.to_string().contains("incompatible parameter count"));
}

#[test]
fn configured_llvm_action_requires_i1_guard_and_matching_fallback() {
    let source = r#"
declare i32 @act(i8, i32, i64)
define i32 @entry(i8 %guard, i32 %arg, i64 %fallback) {
entry:
  %a = call i32 @act(i8 %guard, i32 %arg, i64 %fallback)
  ret i32 %a
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "extern-action-shape.ll",
        ))
        .unwrap();
    let error = import_module_with_config(
        &module,
        &["entry"],
        LlvmImportConfig::default().with_action_execution(
            "act",
            1,
            ActionExecutionPolicy::legacy_evaluator(),
        ),
    )
    .expect_err("action ABI must be validated");
    assert!(error.to_string().contains("guard must be i1"));
}

#[test]
fn configured_llvm_oracle_and_action_preserve_execution_metadata() {
    let source = r#"
declare i32 @pure(i32)
declare i32 @act(i1, i32, i32)
define i32 @entry(i32 %x, i1 %guard) {
entry:
  %o = call i32 @pure(i32 %x)
  %a = call i32 @act(i1 %guard, i32 %o, i32 %x)
  ret i32 %a
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "extern.ll",
        ))
        .expect("valid LLVM external fixture");
    let oracle_policy = OracleExecutionPolicy {
        execution: OracleExecutionKind::Assigned,
        executor: ExternalExecutor::Garbler,
        reveal: ExternalRevealPolicy::BothRoles,
        fingerprint: [0x11; 32],
    };
    let action_policy = ActionExecutionPolicy {
        executor: ExternalExecutor::Garbler,
        reveal: ExternalRevealPolicy::BothRoles,
        fingerprint: [0x22; 32],
    };
    let config = LlvmImportConfig::default()
        .with_oracle_execution("pure", oracle_policy)
        .with_action_execution("act", 1, action_policy);
    let imported = import_module_with_config(&module, &["entry"], config).unwrap();
    assert_eq!(imported.oracles.len(), 1);
    assert_eq!(imported.actions.len(), 1);
    assert_eq!(imported.oracles[0].name, "pure");
    assert_eq!(imported.oracles[0].execution, oracle_policy);
    assert_eq!(imported.actions[0].name, "act");
    assert_eq!(imported.actions[0].execution, action_policy);
    let FuncDecl::Body(body) = &imported.funcs[imported.exports.get("entry").unwrap().0] else {
        panic!("entry must have a body");
    };
    assert!(body.values.iter().any(|node| matches!(
        node.kind,
        Value::Op(Stmt::OracleCall { ref name, .. }) if name == "pure"
    )));
    assert!(body.values.iter().any(|node| matches!(
        node.kind,
        Value::Op(Stmt::ActionCall { ref name, .. }) if name == "act"
    )));
}

#[test]
fn simple_add() {
    let source = r#"
define i32 @add(i32 %a, i32 %b) {
entry:
  %sum = add i32 %a, %b
  ret i32 %sum
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");

    let out = import_module(&module, &["add"]).expect("import succeeds");
    assert_eq!(
        out.pointer_width,
        PointerWidth::Bits64,
        "an absent LLVM data layout uses LLVM's 64-bit default"
    );
    assert_eq!(out.funcs.len(), 1);
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body, got an import declaration");
    };
    assert_eq!(body.blocks.len(), 1);
    // Two i32 params, bit-decomposed: 32 Bit-typed params each.
    assert_eq!(body.blocks[0].params.len(), 64);
    match &body.blocks[0].terminator {
        Terminator::Return { values } => {
            assert_eq!(values.len(), 32, "i32 result should be 32 bits")
        }
        other => panic!("expected Return terminator, got {other:?}"),
    }
    let _ = context;
}

#[test]
fn pointer_load_uses_the_64_bit_layout_width() {
    let source = r#"
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
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");

    let out = import_module(&module, &["pointer_spill"]).expect("64-bit pointer spill imports");
    assert_eq!(out.pointer_width, PointerWidth::Bits64);
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };
    let Terminator::Return { values } = &body.blocks[0].terminator else {
        panic!("expected return");
    };
    assert_eq!(values.len(), 64, "i64 return remains 64 bits");
    let stack_alloc_widths: Vec<_> = body
        .values
        .iter()
        .filter_map(|value| match &value.kind {
            Value::StackAlloc { elem_ty, .. } => Some(*elem_ty),
            _ => None,
        })
        .collect();
    assert_eq!(stack_alloc_widths.len(), 2);
    assert!(stack_alloc_widths.iter().all(|&ty| {
        out.types.0[ty.0 as usize] == volar_ir_common::IrType::Primitive(volar_ir_common::Type::_64)
    }));
    let stack_reads = body
        .values
        .iter()
        .filter(|value| {
            matches!(
                &value.kind,
                Value::Op(Stmt::StorageRead { storage, .. }) if *storage == StorageId::ALLOCA
            )
        })
        .count();
    // `load ptr` + `load i64`: both are 64-bit accesses.
    assert_eq!(stack_reads, 128);
}

#[test]
fn pointer_layout_configuration_is_checked_and_32_bit_remains_supported() {
    let source = r#"
target datalayout = "e-p:32:32"

define i32 @pointer_array(i32 %x) {
entry:
  %value = alloca i32, align 4
  %slots = alloca [2 x ptr], align 4
  %slot = getelementptr ptr, ptr %slots, i32 1
  store i32 %x, ptr %value, align 4
  store ptr %value, ptr %slot, align 4
  %loaded_ptr = load ptr, ptr %slot, align 4
  %result = load i32, ptr %loaded_ptr, align 4
  ret i32 %result
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let out = import_module_with_config(
        &module,
        &["pointer_array"],
        LlvmImportConfig {
            pointer_width: Some(PointerWidth::Bits32),
            ..Default::default()
        },
    )
    .expect("matching 32-bit layout imports");
    assert_eq!(out.pointer_width, PointerWidth::Bits32);
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };
    assert!(
        body.values
            .iter()
            .any(|value| { matches!(value.kind, Value::StackAlloc { count: 2, .. }) }),
        "pointer arrays retain their two pointer-sized stack elements"
    );

    let err = import_module_with_config(
        &module,
        &["pointer_array"],
        LlvmImportConfig {
            pointer_width: Some(PointerWidth::Bits64),
            ..Default::default()
        },
    )
    .expect_err("an incompatible pointer ABI must not be silently overridden");
    assert!(err.to_string().contains("does not match"));
}

#[test]
fn unsupported_pointer_layouts_and_address_spaces_fail_closed() {
    let source = r#"
target datalayout = "e-p:16:16"
define i16 @f(i16 %x) { ret i16 %x }
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    assert!(
        import_module(&module, &["f"])
            .expect_err("16-bit pointers are intentionally unsupported")
            .to_string()
            .contains("16")
    );

    let source = r#"
define i8 @f(ptr addrspace(1) %p) {
entry:
  %v = load i8, ptr addrspace(1) %p
  ret i8 %v
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    assert!(
        import_module(&module, &["f"])
            .expect_err("non-default address space must fail closed")
            .to_string()
            .contains("address space")
    );
}

#[test]
fn call_is_preserved_not_inlined() {
    let source = r#"
define i32 @callee(i32 %x) {
entry:
  %r = add i32 %x, 1
  ret i32 %r
}

define i32 @caller(i32 %x) {
entry:
  %r = call i32 @callee(i32 %x)
  %out = add i32 %r, 0
  ret i32 %out
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");

    let out = import_module(&module, &["caller"]).expect("import succeeds");
    // Both `caller` and (transitively reached) `callee` get their own func.
    assert_eq!(out.funcs.len(), 2);

    let caller_id = *out.exports.get("caller").expect("caller exported");
    let FuncDecl::Body(caller_body) = &out.funcs[caller_id.0] else {
        panic!("expected caller to have a body");
    };
    let has_call = caller_body
        .values
        .iter()
        .any(|v| matches!(&v.kind, Value::Call { .. }));
    assert!(
        has_call,
        "caller's body should retain a Value::Call, not be inlined"
    );

    let callee_id = *out.exports.get("callee").expect("callee exported");
    assert!(
        matches!(&out.funcs[callee_id.0], FuncDecl::Body(_)),
        "callee should also be imported as its own function body"
    );
    let _ = context;
}

#[test]
fn direct_call_return_becomes_return_call() {
    let source = r#"
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
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");

    let out = import_module(&module, &["caller"]).expect("tail call must import");
    let caller_id = *out.exports.get("caller").expect("caller exported");
    let callee_id = *out.exports.get("callee").expect("callee exported");
    let FuncDecl::Body(caller_body) = &out.funcs[caller_id.0] else {
        panic!("expected caller body");
    };
    assert!(
        !caller_body
            .values
            .iter()
            .any(|value| matches!(value.kind, Value::Call { .. })),
        "a call returned directly must not materialize Value::Call"
    );
    assert!(
        matches!(
            caller_body.blocks[0].terminator,
            Terminator::ReturnCall { func, .. } if func == callee_id
        ),
        "call followed by return must become ReturnCall"
    );
}

#[test]
fn import_module_inlined_eliminates_body_calls() {
    let source = r#"
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
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");

    let out = import_module_inlined(&module, &["caller"]).expect("inlined import succeeds");
    let caller_id = *out.exports.get("caller").expect("caller exported");
    let FuncDecl::Body(caller_body) = &out.funcs[caller_id.0] else {
        panic!("expected caller to have a body");
    };
    let live_body_call = caller_body.blocks.iter().any(|b| {
        b.stmts.iter().any(|vid| {
            matches!(
                &caller_body.values[vid.0].kind,
                Value::Call { func, .. }
                    if matches!(out.funcs.get(func.0), Some(FuncDecl::Body(_)))
            )
        }) || matches!(
            &b.terminator,
            Terminator::ReturnCall { func, .. }
                if matches!(out.funcs.get(func.0), Some(FuncDecl::Body(_)))
        )
    });
    assert!(
        !live_body_call,
        "inline-everything should leave no live Body-to-Body calls"
    );
    let _ = context;
}

#[test]
fn branch_and_phi() {
    let source = r#"
define i32 @max(i32 %a, i32 %b) {
entry:
  %cmp = icmp sgt i32 %a, %b
  br i1 %cmp, label %then, label %else
then:
  br label %merge
else:
  br label %merge
merge:
  %result = phi i32 [ %a, %then ], [ %b, %else ]
  ret i32 %result
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");

    let out = import_module(&module, &["max"]).expect("import succeeds");
    assert_eq!(out.funcs.len(), 1);
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };
    // entry, then, else, merge
    assert_eq!(body.blocks.len(), 4);

    match &body.blocks[0].terminator {
        Terminator::IfNonzero { .. } => {}
        other => panic!("expected entry to end in IfNonzero, got {other:?}"),
    }
    for i in [1usize, 2] {
        match &body.blocks[i].terminator {
            Terminator::Jump(_) => {}
            other => panic!("expected block {i} to end in Jump, got {other:?}"),
        }
    }
    // merge's i32 phi becomes 32 bit-decomposed block params.
    assert_eq!(body.blocks[3].params.len(), 32);
    match &body.blocks[3].terminator {
        Terminator::Return { values } => assert_eq!(values.len(), 32),
        other => panic!("expected Return, got {other:?}"),
    }
    let _ = context;
}

#[test]
fn const_literal_in_sibling_blocks_is_not_shared() {
    let source = r#"
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
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");

    let out = import_module(&module, &["opt_join"]).expect("import succeeds");
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };
    let one_in = |block: usize| {
        body.blocks[block]
            .stmts
            .iter()
            .copied()
            .find(|id| {
                matches!(
                    &body.values[id.0].kind,
                    Value::Op(Stmt::Const(constant, _)) if constant.lo == 1 && constant.hi == 0
                )
            })
            .expect("each sibling must materialize its own literal one")
    };
    assert_ne!(
        one_in(1),
        one_in(2),
        "a literal emitted in one sibling cannot be used by the other"
    );
}

#[test]
fn global_load_store() {
    let source = r#"
@counter = global i32 0

define i32 @bump() {
entry:
  %old = load i32, ptr @counter
  %new = add i32 %old, 1
  store i32 %new, ptr @counter
  ret i32 %new
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");

    let out = import_module(&module, &["bump"]).expect("import succeeds");
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };
    let has_read = body.values.iter().any(|v| {
        matches!(
            &v.kind,
            Value::Op(volar_ir_common::Stmt::StorageRead { .. })
        )
    });
    let has_write = body.values.iter().any(|v| {
        matches!(
            &v.kind,
            Value::Op(volar_ir_common::Stmt::StorageWrite { .. })
        )
    });
    assert!(has_read, "expected a StorageRead for the global load");
    assert!(has_write, "expected a StorageWrite for the global store");
    let _ = context;
}

#[test]
fn global_gep_instruction_offset_folds_into_storage_addr() {
    // A constant-index `getelementptr` *instruction* (not embedded as a
    // constant expression) off a global. Previously this failed closed at
    // the subsequent `load`: the GEP's own result wasn't tracked, so
    // `storage_for` saw a non-global pointer and hit ConstChain.
    let source = r#"
@arr = global [4 x i8] zeroinitializer

define i8 @get_byte() {
entry:
  %p = getelementptr inbounds [4 x i8], ptr @arr, i64 0, i64 2
  %v = load i8, ptr %p
  ret i8 %v
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let out = import_module(&module, &["get_byte"])
        .expect("constant-index GEP instruction off a global must import");
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };
    let has_offset_const = body
        .values
        .iter()
        .any(|v| matches!(&v.kind, Value::Op(Stmt::Const(c, _)) if c.lo == 2 && c.hi == 0));
    assert!(
        has_offset_const,
        "expected the GEP's byte offset (2) to be folded into a StorageRead address constant"
    );
    let _ = context;
}

#[test]
fn global_gep_constant_expr_offset_folds_into_storage_addr() {
    // The same offset, but expressed as a `getelementptr` constant
    // expression embedded directly in the load's pointer operand (LLVM
    // constant-folds this shape instead of emitting a separate
    // instruction). Previously `storage_for`'s `strip_pointer` walk resolved
    // straight through to `@arr` at offset 0, silently discarding the index
    // -- not an error, just the wrong byte.
    let source = r#"
@arr = global [4 x i8] zeroinitializer

define i8 @get_byte() {
entry:
  %v = load i8, ptr getelementptr inbounds ([4 x i8], ptr @arr, i64 0, i64 2)
  ret i8 %v
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let out = import_module(&module, &["get_byte"])
        .expect("constant-index GEP constant expression off a global must import");
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };
    let has_offset_const = body
        .values
        .iter()
        .any(|v| matches!(&v.kind, Value::Op(Stmt::Const(c, _)) if c.lo == 2 && c.hi == 0));
    assert!(
        has_offset_const,
        "expected the GEP constant expression's byte offset (2) to be folded into a StorageRead \
         address constant, not silently dropped"
    );
    let _ = context;
}

#[test]
fn global_gep_multi_index_symbolic_still_deferred() {
    // A *multi*-index GEP instruction with a symbolic index off a global --
    // a single-index dynamic offset is supported (see
    // `global_gep_dynamic_index_imports`), but the multi-index (array/
    // nested-aggregate-descending) case remains deferred. Must still fail
    // closed, not silently misresolve to offset 0.
    let source = r#"
@arr = global [4 x i8] zeroinitializer

define i8 @get_byte(i64 %i) {
entry:
  %p = getelementptr inbounds [4 x i8], ptr @arr, i64 0, i64 %i
  %v = load i8, ptr %p
  ret i8 %v
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let err = import_module(&module, &["get_byte"]).expect_err(
        "multi-index symbolic GEP into a global must still fail closed (deferred, not yet supported)",
    );
    let _ = err.to_string();
    let _ = context;
}

#[test]
fn global_gep_dynamic_index_imports() {
    // A single-index `getelementptr` with a *symbolic* (non-constant) index
    // off a global -- previously deferred (left untracked, so the
    // subsequent load/store failed closed). Now emits real bit-circuit
    // address arithmetic, the same shape
    // `dynamic_gep_index_into_alloca_imports` exercises for the stack side.
    // This is the exact `xs[i]` shape the ConstChain-fallback plan targets.
    let source = r#"
@arr = global [4 x i32] zeroinitializer

define i32 @dynamic_global_gep(i32 %x, i32 %i) {
entry:
  %p = getelementptr i32, ptr @arr, i32 %i
  store i32 %x, ptr %p
  %y = load i32, ptr %p
  ret i32 %y
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let out = import_module(&module, &["dynamic_global_gep"])
        .expect("symbolic-index GEP into a global must import");
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };

    // Every non-ALLOCA StorageRead/StorageWrite address must be a
    // *computed* value (a `Stmt::Merge` of bit-circuit-adder output), not a
    // `Stmt::Const` -- confirming the address is genuinely runtime.
    let addr_ids: Vec<usize> = body
        .values
        .iter()
        .filter_map(|v| match &v.kind {
            Value::Op(Stmt::StorageRead {
                storage: volar_ir_common::StorageId(id),
                addr,
                ..
            })
            | Value::Op(Stmt::StorageWrite {
                storage: volar_ir_common::StorageId(id),
                addr,
                ..
            }) if *id != volar_ir_common::StorageId::ALLOCA.0 => Some(addr.0),
            _ => None,
        })
        .collect();
    assert!(
        !addr_ids.is_empty(),
        "expected global StorageRead/StorageWrite operations"
    );
    for id in addr_ids {
        assert!(
            !matches!(&body.values[id].kind, Value::Op(Stmt::Const(..))),
            "expected a computed (non-constant) address for the dynamic GEP, got {:?}",
            body.values[id].kind
        );
    }
    let _ = context;
}

#[test]
fn alloca_spill_imports() {
    let source = r#"
define i32 @spill(i32 %x) {
entry:
  %p = alloca i32, align 4
  store i32 %x, ptr %p
  %y = load i32, ptr %p
  ret i32 %y
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let out = import_module(&module, &["spill"]).expect("alloca must import");
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };
    let has_alloc = body
        .values
        .iter()
        .any(|v| matches!(&v.kind, Value::StackAlloc { .. }));
    let has_read = body.values.iter().any(|v| {
        matches!(
            &v.kind,
            Value::Op(volar_ir_common::Stmt::StorageRead { storage, .. })
                if *storage == volar_ir_common::StorageId::ALLOCA
        )
    });
    let has_write = body.values.iter().any(|v| {
        matches!(
            &v.kind,
            Value::Op(volar_ir_common::Stmt::StorageWrite { storage, .. })
                if *storage == volar_ir_common::StorageId::ALLOCA
        )
    });
    assert!(has_alloc, "expected a Value::StackAlloc marker");
    assert!(
        has_read,
        "expected an ALLOCA StorageRead for the spill load"
    );
    assert!(
        has_write,
        "expected an ALLOCA StorageWrite for the spill store"
    );
}

#[test]
fn dynamic_gep_index_into_alloca_imports() {
    // A `getelementptr` with a *symbolic* (non-constant) index into a stack
    // pointer -- previously a named `Unsupported` error
    // ("symbolic index into stack pointer not supported"). ALLOCA addresses
    // already tolerate a runtime `addr` operand (see `rebase_stack_addr`),
    // so this just needed the importer's own restriction lifted: real
    // bit-circuit multiply-and-add instead of a compile-time-constant
    // offset.
    let source = r#"
define i32 @dynamic_gep(i32 %x, i32 %i) {
entry:
  %buf = alloca [4 x i32], align 4
  %p = getelementptr i32, ptr %buf, i32 %i
  store i32 %x, ptr %p
  %y = load i32, ptr %p
  ret i32 %y
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let out = import_module(&module, &["dynamic_gep"])
        .expect("symbolic-index GEP into alloca must import");
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };

    // Every ALLOCA StorageRead/StorageWrite address must be a *computed*
    // value (a `Stmt::Merge` of bit-circuit-adder output), not a
    // `Stmt::Const` -- confirming the address is genuinely runtime, not
    // silently folded back down to a fixed offset.
    let addr_ids: Vec<usize> = body
        .values
        .iter()
        .filter_map(|v| match &v.kind {
            Value::Op(Stmt::StorageRead {
                storage: volar_ir_common::StorageId::ALLOCA,
                addr,
                ..
            })
            | Value::Op(Stmt::StorageWrite {
                storage: volar_ir_common::StorageId::ALLOCA,
                addr,
                ..
            }) => Some(addr.0),
            _ => None,
        })
        .collect();
    assert!(
        !addr_ids.is_empty(),
        "expected ALLOCA StorageRead/StorageWrite operations"
    );
    for id in addr_ids {
        assert!(
            !matches!(&body.values[id].kind, Value::Op(Stmt::Const(..))),
            "expected a computed (non-constant) address for the dynamic GEP, got {:?}",
            body.values[id].kind
        );
    }
    let _ = context;
}

#[test]
fn alloca_constant_gep_two_elements() {
    // Two-element i32 stack array, written through a constant-index GEP off
    // the alloca'd base pointer, read back through the base pointer itself.
    let source = r#"
define i32 @two_slots(i32 %a, i32 %b) {
entry:
  %p = alloca i32, i32 2, align 4
  %q = getelementptr i32, ptr %p, i32 1
  store i32 %a, ptr %p
  store i32 %b, ptr %q
  %x = load i32, ptr %p
  %y = load i32, ptr %q
  %sum = add i32 %x, %y
  ret i32 %sum
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let out = import_module(&module, &["two_slots"]).expect("constant-index GEP must import");
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };
    let stack_reads = body
        .values
        .iter()
        .filter(|v| {
            matches!(
                &v.kind,
                Value::Op(volar_ir_common::Stmt::StorageRead { storage, .. })
                    if *storage == volar_ir_common::StorageId::ALLOCA
            )
        })
        .count();
    let stack_writes = body
        .values
        .iter()
        .filter(|v| {
            matches!(
                &v.kind,
                Value::Op(volar_ir_common::Stmt::StorageWrite { storage, .. })
                    if *storage == volar_ir_common::StorageId::ALLOCA
            )
        })
        .count();
    // 32 bits per i32 load/store.
    assert_eq!(stack_reads, 64, "two 32-bit loads");
    assert_eq!(stack_writes, 64, "two 32-bit stores");
}

#[test]
fn array_alloca_imports() {
    // rustc `-O0` stack-spill shape: alloca a byte blob, index into it with a
    // constant `getelementptr` at a *different* (wider) element type than
    // the alloca's own declared element type -- a "typed view" of the blob.
    let source = r#"
define i8 @byte_blob(i8 %x) {
entry:
  %buf = alloca [4 x i8], align 1
  store i8 %x, ptr %buf
  %y = load i8, ptr %buf
  ret i8 %y
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let out = import_module(&module, &["byte_blob"]).expect("array alloca must import");
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };
    let has_alloc = body
        .values
        .iter()
        .any(|v| matches!(&v.kind, Value::StackAlloc { count: 4, .. }));
    assert!(has_alloc, "expected a 4-element Value::StackAlloc marker");
}

#[test]
fn array_alloca_gep_typed_view_two_i32_slots() {
    // The exact docs/llvm-array-alloca.md shape: a `[16 x i8]` blob indexed
    // as an array of i32 via a single-index, differently-typed constant GEP.
    let source = r#"
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
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let out =
        import_module(&module, &["stack_spill"]).expect("typed-view GEP into array must import");
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };
    let stack_reads = body
        .values
        .iter()
        .filter(|v| {
            matches!(
                &v.kind,
                Value::Op(volar_ir_common::Stmt::StorageRead { storage, .. })
                    if *storage == volar_ir_common::StorageId::ALLOCA
            )
        })
        .count();
    let stack_writes = body
        .values
        .iter()
        .filter(|v| {
            matches!(
                &v.kind,
                Value::Op(volar_ir_common::Stmt::StorageWrite { storage, .. })
                    if *storage == volar_ir_common::StorageId::ALLOCA
            )
        })
        .count();
    assert_eq!(stack_reads, 64, "two 32-bit loads");
    assert_eq!(stack_writes, 64, "two 32-bit stores");
}

#[test]
fn struct_alloca_is_named_unsupported() {
    let source = r#"
define i32 @two_field(i32 %a, i32 %b) {
entry:
  %s = alloca { i32, i32 }, align 4
  %p0 = getelementptr { i32, i32 }, ptr %s, i32 0, i32 0
  store i32 %a, ptr %p0
  %y = load i32, ptr %p0
  ret i32 %y
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let err = import_module(&module, &["two_field"]).expect_err("struct alloca must fail closed");
    let msg = err.to_string();
    assert!(
        msg.contains("alloca"),
        "expected a named struct-alloca error, got {msg}"
    );
}

#[test]
fn alloca_symbolic_count_is_named_unsupported() {
    let source = r#"
define i32 @spill_n(i32 %x, i32 %n) {
entry:
  %p = alloca i32, i32 %n
  store i32 %x, ptr %p
  %y = load i32, ptr %p
  ret i32 %y
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let err = import_module(&module, &["spill_n"]).expect_err("VLA alloca must fail closed");
    let msg = err.to_string();
    assert!(
        msg.contains("alloca") && msg.contains("symbolic"),
        "expected a named symbolic-alloca error, got {msg}"
    );
}

#[test]
fn stack_pointer_param_dispatches_at_runtime() {
    // A pointer *parameter* is bit-decomposed and cached like an alloca'd
    // stack pointer would be, but it is not one -- it must never be
    // mistaken for a tracked alloca with a known, fixed address (regression
    // test for the `stack_slot_of`-vs-`cache` provenance distinction).
    // Before runtime storage-identity dispatch (stage 4 of the
    // ConstChain-fallback plan), this meant the subsequent load simply
    // failed closed. Now it succeeds via genuine runtime dispatch
    // (`dispatch_read`) instead -- every candidate's address is computed
    // from the parameter's own raw bits, never a compile-time constant.
    let source = r#"
define i32 @through_param(ptr %p) {
entry:
  %y = load i32, ptr %p
  ret i32 %y
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let out = import_module(&module, &["through_param"])
        .expect("param pointer now dispatches at runtime instead of failing closed");
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };
    let alloca_reads: Vec<usize> = body
        .values
        .iter()
        .filter_map(|v| match &v.kind {
            Value::Op(Stmt::StorageRead {
                storage: volar_ir_common::StorageId::ALLOCA,
                addr,
                ..
            }) => Some(addr.0),
            _ => None,
        })
        .collect();
    assert!(
        !alloca_reads.is_empty(),
        "expected the dispatch cascade's stack-candidate StorageReads"
    );
    for id in alloca_reads {
        assert!(
            !matches!(&body.values[id].kind, Value::Op(Stmt::Const(..))),
            "a pointer parameter must never resolve to a compile-time-constant stack address, \
             got {:?}",
            body.values[id].kind
        );
    }
    let _ = context;
}

#[test]
fn slice_get_dispatches_through_pointer_parameter() {
    // The exact motivating shape of the ConstChain-fallback plan: a pointer
    // *parameter* GEP'd with a *symbolic* index, then loaded through --
    // `fn slice_get(xs: &[i32], i: usize) -> i32 { xs[i] }`. Previously
    // named ConstChain (`docs/llvm-const-cache-dominance.md`'s own
    // "Measured" table: "rustc `-O0` `xs[i]` pointer-param GEP"). Now
    // succeeds end-to-end: the GEP computes an offset pointer via ordinary
    // bit-circuit arithmetic on `%xs`'s own raw bits (ptr_value_bits's
    // encoding), and the load dispatches on the result at runtime.
    let source = r#"
define i32 @slice_get(ptr %xs, i64 %i) {
entry:
  %p = getelementptr i32, ptr %xs, i64 %i
  %v = load i32, ptr %p
  ret i32 %v
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let out = import_module(&module, &["slice_get"])
        .expect("xs[i] through a pointer parameter must import via runtime dispatch");
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };
    let has_alloca_read = body.values.iter().any(|v| {
        matches!(
            &v.kind,
            Value::Op(Stmt::StorageRead {
                storage: volar_ir_common::StorageId::ALLOCA,
                ..
            })
        )
    });
    assert!(
        has_alloca_read,
        "expected the dispatch cascade's stack-candidate StorageRead"
    );
    let _ = context;
}

#[test]
fn dispatch_write_through_pointer_parameter_reaches_every_candidate() {
    // `store` through an unresolved pointer parameter, in a module that
    // also has a global -- confirms the write side (`dispatch_write`)
    // reaches *every* candidate, not just the stack: a read-modify-write
    // per candidate (`storage_to_mux_ir::mux_write`'s technique,
    // generalized from "N addresses in one storage" to "N storages, one
    // address"), so both the stack candidate and `@g`'s candidate get a
    // paired StorageRead (the "old" value) and StorageWrite.
    let source = r#"
@g = global i32 0

define void @store_through_param(ptr %p, i32 %x) {
entry:
  store i32 %x, ptr %p
  ret void
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let out = import_module(&module, &["store_through_param"])
        .expect("store through an unresolved pointer must dispatch at runtime");
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };
    let (mut alloca_writes, mut global_writes) = (0usize, 0usize);
    for v in &body.values {
        if let Value::Op(Stmt::StorageWrite { storage, .. }) = &v.kind {
            if *storage == volar_ir_common::StorageId::ALLOCA {
                alloca_writes += 1;
            } else {
                global_writes += 1;
            }
        }
    }
    assert!(
        alloca_writes >= 32,
        "expected a full 32-bit read-modify-write against the stack candidate, got {alloca_writes}"
    );
    assert!(
        global_writes >= 4,
        "expected a full 4-byte read-modify-write against @g's candidate, got {global_writes}"
    );
    let _ = context;
}

#[test]
fn select_between_stack_and_global_pointer_imports() {
    // `select` merging two *individually* statically-resolved but
    // differently-provenanced pointers (one stack, one global) -- until
    // now, a bare global reference used directly as a generic value (not
    // immediately loaded/stored through) had no `Bits` representation at
    // all and hard-errored ("unsupported value kind"). The stack arm
    // already had one via `Alloca`'s own cached `addr_bits`; only the
    // global arm was the gap. Not loading through the merged result here --
    // that still requires runtime storage-identity dispatch, a later stage.
    let source = r#"
@g = global i32 42

define ptr @select_ptr(i1 %cond, i32 %x) {
entry:
  %p = alloca i32, align 4
  store i32 %x, ptr %p
  %sel = select i1 %cond, ptr %p, ptr @g
  ret ptr %sel
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    import_module(&module, &["select_ptr"])
        .expect("select between a stack pointer and a global pointer must import");
    let _ = context;
}

#[test]
fn phi_between_stack_and_global_pointer_imports() {
    // Same shape as `select_between_stack_and_global_pointer_imports`, but
    // via a control-flow-join `phi` instead of `select` -- exercises the
    // block-param merge path instead of `bc_select_vec`.
    let source = r#"
@g = global i32 42

define ptr @phi_ptr(i1 %cond, i32 %x) {
entry:
  %p = alloca i32, align 4
  store i32 %x, ptr %p
  br i1 %cond, label %use_stack, label %use_global
use_stack:
  br label %join
use_global:
  br label %join
join:
  %res = phi ptr [ %p, %use_stack ], [ @g, %use_global ]
  ret ptr %res
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    import_module(&module, &["phi_ptr"])
        .expect("phi between a stack pointer and a global pointer must import");
    let _ = context;
}

#[test]
fn memory_intrinsics_lower_without_residual_call() {
    let source = r#"
declare void @llvm.memset.p0.i64(ptr, i8, i64, i1 immarg)
declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1 immarg)
declare void @llvm.memmove.p0.p0.i64(ptr, ptr, i64, i1 immarg)
@global = global [4 x i8] zeroinitializer

define i32 @copy_and_fill(i32 %x) {
entry:
  %src = alloca [4 x i8], align 4
  %dst = alloca [4 x i8], align 4
  store i32 %x, ptr %src
  call void @llvm.memcpy.p0.p0.i64(ptr %dst, ptr %src, i64 4, i1 false)
  call void @llvm.memmove.p0.p0.i64(ptr %dst, ptr %dst, i64 4, i1 false)
  call void @llvm.memset.p0.i64(ptr %src, i8 0, i64 4, i1 false)
  call void @llvm.memset.p0.i64(ptr @global, i8 0, i64 4, i1 false)
  %out = load i32, ptr %dst
  ret i32 %out
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let out = import_module(&module, &["copy_and_fill"]).expect("memory intrinsics must import");
    assert_eq!(
        out.funcs.len(),
        1,
        "intrinsics must not become imported callees"
    );
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };
    assert!(
        !body
            .values
            .iter()
            .any(|value| matches!(value.kind, Value::Call { .. })),
        "supported memory intrinsics must lower to storage operations"
    );
}

#[test]
fn symbolic_memset_lowers_to_cfg_loop_without_residual_call() {
    let source = r#"
declare void @llvm.memset.p0.i64(ptr, i8, i64, i1 immarg)

define void @symbolic(i64 %n) {
entry:
  %buf = alloca [4 x i8], align 4
  call void @llvm.memset.p0.i64(ptr %buf, i8 0, i64 %n, i1 false)
  ret void
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let out = import_module(&module, &["symbolic"])
        .expect("an unbounded symbolic memset length must import");
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };
    assert!(
        !body
            .values
            .iter()
            .any(|value| matches!(value.kind, Value::Call { .. })),
        "a supported symbolic memset must lower to CFG storage operations"
    );
    assert!(
        body.blocks.len() >= 4,
        "a symbolic memset must append header, body, and continuation blocks"
    );
    assert!(
        body.blocks
            .iter()
            .any(|block| matches!(block.terminator, Terminator::IfNonzero { .. }))
    );
}

#[test]
fn unbounded_symbolic_memcpy_lowers_to_cfg_loop_without_residual_call() {
    let source = r#"
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
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let out = import_module(&module, &["copy_prefix"])
        .expect("an unbounded symbolic memcpy length must import");
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };
    assert!(
        !body
            .values
            .iter()
            .any(|value| matches!(value.kind, Value::Call { .. })),
        "a supported symbolic memcpy must lower to CFG storage operations"
    );
    assert!(
        body.blocks.len() >= 4,
        "a symbolic memcpy must append header, body, and continuation blocks"
    );
    assert!(
        body.blocks
            .iter()
            .any(|block| matches!(block.terminator, Terminator::IfNonzero { .. }))
    );
}

#[test]
fn constant_memory_intrinsics_through_symbolic_stack_gep_lower_without_calls() {
    let source = r#"
declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1 immarg)
declare void @llvm.memset.p0.i64(ptr, i8, i64, i1 immarg)

define void @copy_at(i64 %i, i32 %src) {
entry:
  %src_buf = alloca [4 x i8], align 4
  %buf = alloca [64 x i8], align 1
  store i32 %src, ptr %src_buf, align 4
  %p = getelementptr inbounds i8, ptr %buf, i64 %i
  call void @llvm.memcpy.p0.p0.i64(ptr %p, ptr %src_buf, i64 4, i1 false)
  ret void
}

define void @fill_at(i64 %i) {
entry:
  %buf = alloca [64 x i8], align 1
  %p = getelementptr inbounds i8, ptr %buf, i64 %i
  call void @llvm.memset.p0.i64(ptr %p, i8 -86, i64 4, i1 false)
  ret void
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let out = import_module(&module, &["copy_at", "fill_at"])
        .expect("constant memory intrinsics through symbolic stack GEPs must import");
    assert_eq!(out.funcs.len(), 2);

    let dynamic_stack_writes = |body: &vaffle::FuncBody| {
        body.values
            .iter()
            .filter(|value| {
                let Value::Op(Stmt::StorageWrite { storage, addr, .. }) = &value.kind else {
                    return false;
                };
                *storage == StorageId::ALLOCA
                    && matches!(body.values[addr.0].kind, Value::Op(Stmt::Merge { .. }))
            })
            .count()
    };
    for func in &out.funcs {
        let FuncDecl::Body(body) = func else {
            panic!("expected a function body");
        };
        assert!(
            !body
                .values
                .iter()
                .any(|value| matches!(value.kind, Value::Call { .. })),
            "a supported intrinsic must not remain a call"
        );
        assert_eq!(
            dynamic_stack_writes(body),
            32,
            "four byte intrinsic must perform 32 dynamically addressed stack writes"
        );
    }
}

#[test]
fn null_pointer_values_flow_through_compare_and_phi() {
    let source = r#"
define i1 @is_null(ptr %p) {
entry:
  %z = icmp eq ptr %p, null
  ret i1 %z
}

define i1 @phi_null(i1 %choose_null, ptr %p) {
entry:
  br i1 %choose_null, label %null, label %param
null:
  br label %join
param:
  br label %join
join:
  %q = phi ptr [ null, %null ], [ %p, %param ]
  %z = icmp eq ptr %q, null
  ret i1 %z
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let out = import_module(&module, &["is_null", "phi_null"])
        .expect("null pointer comparisons and phis must import");
    assert_eq!(out.funcs.len(), 2, "both null-pointer fixtures must import");
}

#[test]
fn memory_intrinsic_volatile_is_named_unsupported() {
    let source = r#"
declare void @llvm.memset.p0.i64(ptr, i8, i64, i1 immarg)

define void @volatile_memset() {
entry:
  %buf = alloca [4 x i8], align 4
  call void @llvm.memset.p0.i64(ptr %buf, i8 0, i64 4, i1 true)
  ret void
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let err = import_module(&module, &["volatile_memset"]).expect_err("volatile must fail closed");
    assert!(
        err.to_string().contains("volatile"),
        "expected named volatile error, got {err}"
    );
}

#[test]
fn overlapping_memcpy_is_named_unsupported() {
    let source = r#"
declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1 immarg)

define void @overlap() {
entry:
  %buf = alloca [4 x i8], align 4
  %dst = getelementptr i8, ptr %buf, i64 1
  call void @llvm.memcpy.p0.p0.i64(ptr %dst, ptr %buf, i64 3, i1 false)
  ret void
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let err =
        import_module(&module, &["overlap"]).expect_err("overlapping memcpy must fail closed");
    assert!(
        err.to_string().contains("overlap"),
        "expected named overlap error, got {err}"
    );
}

#[test]
fn escaping_memmove_is_named_unsupported() {
    let source = r#"
declare void @llvm.memmove.p0.p0.i64(ptr, ptr, i64, i1 immarg)

define void @escape() {
entry:
  %a = alloca [4 x i8], align 4
  %b = alloca [4 x i8], align 4
  %outside_a = getelementptr i8, ptr %a, i64 4
  call void @llvm.memmove.p0.p0.i64(ptr %outside_a, ptr %b, i64 4, i1 false)
  ret void
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let err = import_module(&module, &["escape"]).expect_err("escaping memmove must fail closed");
    assert!(
        err.to_string().contains("provenance"),
        "expected named provenance error, got {err}"
    );
}

#[test]
fn memcpy_from_global_and_offset_gep_lower_without_residual_call() {
    let source = r#"
@iv = private unnamed_addr constant [32 x i8] zeroinitializer
declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1 immarg)

define void @copy_iv() {
entry:
  %buf = alloca [64 x i8], align 8
  call void @llvm.memcpy.p0.p0.i64(ptr %buf, ptr @iv, i64 32, i1 false)
  %dst = getelementptr i8, ptr %buf, i64 32
  %src = getelementptr [32 x i8], ptr @iv, i64 0, i64 7
  call void @llvm.memcpy.p0.p0.i64(ptr %dst, ptr %src, i64 8, i1 false)
  ret void
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let out = import_module(&module, &["copy_iv"])
        .expect("constant global memcpy and constant-offset GEP must import");
    assert_eq!(out.funcs.len(), 1, "intrinsics must not become callees");
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };
    assert!(
        !body
            .values
            .iter()
            .any(|value| matches!(value.kind, Value::Call { .. })),
        "supported global memcpy intrinsics must lower to storage operations"
    );
    // The two copies read `@iv` at `[0, 32)` and `[7, 15)`, respectively.
    // Inspecting the address constants makes an offset-zero regression
    // observable even though both copies' data happen to be zero here.
    let global_read_offsets: Vec<u64> = body
        .values
        .iter()
        .filter_map(|node| match &node.kind {
            Value::Op(Stmt::StorageRead { storage, addr, .. })
                if *storage != volar_ir_common::StorageId::ALLOCA =>
            {
                match &body.values[addr.0].kind {
                    Value::Op(Stmt::Const(constant, _)) => u64::try_from(constant.lo).ok(),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        global_read_offsets.len(),
        40,
        "expected 32-byte and 8-byte global reads"
    );
    assert!(
        global_read_offsets.contains(&0) && global_read_offsets.contains(&31),
        "base-global memcpy must read the full [0, 32) range: {global_read_offsets:?}"
    );
    assert!(
        global_read_offsets.contains(&7) && global_read_offsets.contains(&14),
        "GEP memcpy must preserve its nonzero [7, 15) base offset: {global_read_offsets:?}"
    );
}

#[test]
fn constant_size_memcpy_through_pointer_params_dispatches() {
    // A constant size is enough to lower a memcpy through untracked slice
    // pointers. The source materializes through `dispatch_read` before the
    // destination `dispatch_write`, exactly as for the direct paths.
    let source = r#"
@g = global [4 x i8] zeroinitializer
declare void @llvm.memcpy.p0.p0.i64(ptr, ptr, i64, i1 immarg)

define void @copy_param(ptr %out, ptr %in) {
entry:
  call void @llvm.memcpy.p0.p0.i64(ptr %out, ptr %in, i64 4, i1 false)
  ret void
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let out = import_module(&module, &["copy_param"])
        .expect("constant-size pointer-param memcpy must dispatch instead of becoming a call");
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };
    assert!(
        !body
            .values
            .iter()
            .any(|value| matches!(value.kind, Value::Call { .. })),
        "dispatched memcpy must not leave a Value::Call"
    );
    assert!(
        body.values.iter().any(|value| matches!(
            value.kind,
            Value::Op(Stmt::StorageRead { storage, .. })
                if storage == volar_ir_common::StorageId::ALLOCA
        )),
        "the read side must include the stack candidate in the runtime dispatch"
    );
}

#[test]
fn noalias_and_lifetime_intrinsics_are_skipped() {
    let source = r#"
declare void @llvm.experimental.noalias.scope.decl(metadata)
declare void @llvm.lifetime.start.p0(i64 immarg, ptr nocapture)
declare void @llvm.lifetime.end.p0(i64 immarg, ptr nocapture)

define i32 @xor_one(i32 %x) {
entry:
  %slot = alloca i32, align 4
  call void @llvm.experimental.noalias.scope.decl(metadata !0)
  call void @llvm.lifetime.start.p0(i64 4, ptr %slot)
  %y = xor i32 %x, 1
  call void @llvm.lifetime.end.p0(i64 4, ptr %slot)
  ret i32 %y
}

!0 = !{!0}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let out = import_module(&module, &["xor_one"])
        .expect("metadata, noalias, and lifetime intrinsics must be skipped");
    assert_eq!(
        out.funcs.len(),
        1,
        "skipped intrinsics must not become callees"
    );
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };
    assert!(
        !body
            .values
            .iter()
            .any(|value| matches!(value.kind, Value::Call { .. })),
        "ignored LLVM intrinsics must not survive as Value::Call"
    );
}

#[test]
fn unsupported_metadata_call_is_a_named_error_not_a_panic() {
    let source = r#"
declare void @takes_metadata(metadata)

define void @caller() {
entry:
  call void @takes_metadata(metadata !0)
  ret void
}

!0 = !{!0}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let imported = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        import_module(&module, &["caller"])
    }));
    let err = imported
        .expect("metadata on a non-skipped call must not panic")
        .expect_err("metadata on a non-skipped call must fail closed");
    assert!(
        err.to_string().contains("metadata"),
        "expected a named metadata-call error, got {err}"
    );
}

#[test]
fn overflow_intrinsics_lower_without_residual_call() {
    let source = r#"
declare { i8, i1 } @llvm.sadd.with.overflow.i8(i8, i8)
declare { i8, i1 } @llvm.uadd.with.overflow.i8(i8, i8)
declare { i8, i1 } @llvm.ssub.with.overflow.i8(i8, i8)
declare { i8, i1 } @llvm.usub.with.overflow.i8(i8, i8)
declare { i8, i1 } @llvm.smul.with.overflow.i8(i8, i8)
declare { i8, i1 } @llvm.umul.with.overflow.i8(i8, i8)

define i8 @all_overflow(i8 %x) {
entry:
  %sadd = call { i8, i1 } @llvm.sadd.with.overflow.i8(i8 %x, i8 1)
  %sadd_value = extractvalue { i8, i1 } %sadd, 0
  %sadd_overflow = extractvalue { i8, i1 } %sadd, 1
  %uadd = call { i8, i1 } @llvm.uadd.with.overflow.i8(i8 %x, i8 1)
  %uadd_value = extractvalue { i8, i1 } %uadd, 0
  %uadd_overflow = extractvalue { i8, i1 } %uadd, 1
  %ssub = call { i8, i1 } @llvm.ssub.with.overflow.i8(i8 %x, i8 1)
  %ssub_value = extractvalue { i8, i1 } %ssub, 0
  %ssub_overflow = extractvalue { i8, i1 } %ssub, 1
  %usub = call { i8, i1 } @llvm.usub.with.overflow.i8(i8 %x, i8 1)
  %usub_value = extractvalue { i8, i1 } %usub, 0
  %usub_overflow = extractvalue { i8, i1 } %usub, 1
  %smul = call { i8, i1 } @llvm.smul.with.overflow.i8(i8 %x, i8 2)
  %smul_value = extractvalue { i8, i1 } %smul, 0
  %smul_overflow = extractvalue { i8, i1 } %smul, 1
  %umul = call { i8, i1 } @llvm.umul.with.overflow.i8(i8 %x, i8 2)
  %umul_value = extractvalue { i8, i1 } %umul, 0
  %umul_overflow = extractvalue { i8, i1 } %umul, 1
  ret i8 %sadd_value
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let out = import_module(&module, &["all_overflow"]).expect("overflow intrinsics must import");
    assert_eq!(
        out.funcs.len(),
        1,
        "overflow intrinsics must not become imported callees"
    );
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };
    assert!(
        !body
            .values
            .iter()
            .any(|value| matches!(value.kind, Value::Call { .. })),
        "supported overflow intrinsics must lower to bit operations"
    );
}

#[test]
fn extractvalue_of_untracked_aggregate_is_named_unsupported() {
    let source = r#"
declare { i8, i1 } @ordinary_pair(i8)

define i8 @untracked(i8 %x) {
entry:
  %pair = call { i8, i1 } @ordinary_pair(i8 %x)
  %value = extractvalue { i8, i1 } %pair, 0
  ret i8 %value
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let err = import_module(&module, &["untracked"])
        .expect_err("arbitrary aggregate extractvalue must fail closed");
    assert!(
        err.to_string().contains("untracked aggregate"),
        "expected named untracked-aggregate error, got {err}"
    );
}

#[test]
fn reachable_call_unreachable_becomes_return_call() {
    let source = r#"
declare void @panic_abort()

define i32 @abort_on_flag(i1 %flag, i32 %x) {
entry:
  br i1 %flag, label %panic, label %ok
panic:
  call void @panic_abort()
  unreachable
ok:
  ret i32 %x
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");

    let out = import_module(&module, &["abort_on_flag"])
        .expect("reachable call; unreachable must import as ReturnCall");
    let caller_id = *out.exports.get("abort_on_flag").expect("caller exported");
    let FuncDecl::Body(body) = &out.funcs[caller_id.0] else {
        panic!("expected caller body");
    };
    let Terminator::ReturnCall { func, .. } = &body.blocks[1].terminator else {
        panic!("panic block must end with ReturnCall");
    };
    assert!(
        matches!(&out.funcs[func.0], FuncDecl::Import { name, .. } if name == "panic_abort"),
        "noreturn declaration must remain an import"
    );
    assert!(
        !body.blocks[1]
            .stmts
            .iter()
            .any(|value| matches!(body.values[value.0].kind, Value::Call { .. })),
        "noreturn call must not leave a Value::Call behind"
    );
}

#[test]
fn reachable_unreachable_without_call_is_named_unsupported() {
    let source = r#"
define void @abort() {
entry:
  unreachable
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let err = import_module(&module, &["abort"])
        .expect_err("reachable standalone unreachable must fail closed");
    assert!(
        err.to_string().contains("reachable unreachable"),
        "expected named reachable-unreachable error, got {err}"
    );
}

#[test]
fn dead_landingpad_is_ignored() {
    let source = r#"
declare i32 @rust_eh_personality(...)
declare void @cant_unwind()

define i32 @dead_lpad(i32 %x) personality ptr @rust_eh_personality {
entry:
  ret i32 %x
terminate:
  %lp = landingpad { ptr, i32 }
          filter [0 x ptr] zeroinitializer
  call void @cant_unwind()
  unreachable
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let out = import_module(&module, &["dead_lpad"]).expect("dead landingpad must be ignored");
    assert_eq!(
        out.funcs.len(),
        1,
        "dead cant_unwind call must not be imported"
    );
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };
    assert_eq!(body.blocks.len(), 2, "unreachable block IDs remain stable");
}

#[test]
fn reachable_invoke_is_named_unsupported() {
    let source = r#"
declare i32 @rust_eh_personality(...)
declare void @may_unwind()

define i32 @live_eh(i32 %x) personality ptr @rust_eh_personality {
entry:
  invoke void @may_unwind() to label %ok unwind label %terminate
ok:
  ret i32 %x
terminate:
  %lp = landingpad { ptr, i32 }
          cleanup
  ret i32 0
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let err = import_module(&module, &["live_eh"]).expect_err("reachable invoke must fail closed");
    assert!(
        err.to_string().contains("Invoke"),
        "expected named invoke error, got {err}"
    );
}

#[test]
fn switch_imports_to_table() {
    let source = r#"
define i32 @poll(i8 %s, i32 %acc) {
entry:
  switch i8 %s, label %other [
    i8 0, label %a
    i8 1, label %b
  ]
a:
  %x = add i32 %acc, 1
  ret i32 %x
b:
  %y = xor i32 %acc, 9
  ret i32 %y
other:
  ret i32 %acc
}
"#;
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let out = import_module(&module, &["poll"]).expect("switch must import");
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };
    let has_table = body
        .blocks
        .iter()
        .any(|b| matches!(b.terminator, Terminator::Table { .. }));
    assert!(has_table, "expected a Terminator::Table for LLVM switch");
    let _ = context;
}

#[test]
fn switch_with_phi_imports() {
    // rustc `-C opt-level=1` `match` on a byte: switch + join phi, no alloca.
    let source = r#"
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
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            source.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let out = import_module(&module, &["poll_fsm"]).expect("switch+phi must import");
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };
    let has_table = body
        .blocks
        .iter()
        .any(|b| matches!(b.terminator, Terminator::Table { .. }));
    assert!(has_table, "expected a Terminator::Table for rustc match");
    let _ = context;
}
