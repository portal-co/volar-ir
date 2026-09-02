//! Structural-import fixture tests: hand-written `.ll` sources, imported via
//! [`volar_llvm_vaffle_import::import_module`], asserted against the
//! resulting `vaffle::Module`'s shape — call preservation (no inlining),
//! block/phi/param structure, and terminator kinds.

use inkwell::context::Context;
use inkwell::memory_buffer::MemoryBuffer;
use vaffle::{FuncDecl, Terminator, Value};
use volar_llvm_vaffle_import::{import_module, import_module_inlined};

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
                if *storage == volar_ir_common::StorageId::STACK
        )
    });
    let has_write = body.values.iter().any(|v| {
        matches!(
            &v.kind,
            Value::Op(volar_ir_common::Stmt::StorageWrite { storage, .. })
                if *storage == volar_ir_common::StorageId::STACK
        )
    });
    assert!(has_alloc, "expected a Value::StackAlloc marker");
    assert!(has_read, "expected a STACK StorageRead for the spill load");
    assert!(
        has_write,
        "expected a STACK StorageWrite for the spill store"
    );
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
                    if *storage == volar_ir_common::StorageId::STACK
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
                    if *storage == volar_ir_common::StorageId::STACK
            )
        })
        .count();
    // 32 bits per i32 load/store.
    assert_eq!(stack_reads, 64, "two 32-bit loads");
    assert_eq!(stack_writes, 64, "two 32-bit stores");
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
fn stack_pointer_param_is_not_mistaken_for_alloca() {
    // A pointer *parameter* is also bit-decomposed and cached like an
    // alloca'd stack pointer would be, but it is not one — it must still
    // fail closed (regression test for the `stack_slot_of`-vs-`cache`
    // provenance distinction).
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
    let err =
        import_module(&module, &["through_param"]).expect_err("param pointer must fail closed");
    let _ = err.to_string();
}

#[test]
fn switch_is_named_unsupported() {
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
    let err = import_module(&module, &["poll"]).expect_err("switch must fail closed");
    let msg = err.to_string();
    assert!(
        msg.contains("switch"),
        "expected named switch error, got {msg}"
    );
    let _ = context;
}
