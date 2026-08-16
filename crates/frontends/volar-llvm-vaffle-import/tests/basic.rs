//! Structural-import fixture tests: hand-written `.ll` sources, imported via
//! [`volar_llvm_vaffle_import::import_module`], asserted against the
//! resulting `vaffle::Module`'s shape — call preservation (no inlining),
//! block/phi/param structure, and terminator kinds.

use inkwell::context::Context;
use inkwell::memory_buffer::MemoryBuffer;
use vaffle::{FuncDecl, Terminator, Value};
use volar_llvm_vaffle_import::import_module;

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
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(source.as_bytes(), "test.ll"))
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
        Terminator::Return { values } => assert_eq!(values.len(), 32, "i32 result should be 32 bits"),
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
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(source.as_bytes(), "test.ll"))
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
    assert!(has_call, "caller's body should retain a Value::Call, not be inlined");

    let callee_id = *out.exports.get("callee").expect("callee exported");
    assert!(
        matches!(&out.funcs[callee_id.0], FuncDecl::Body(_)),
        "callee should also be imported as its own function body"
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
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(source.as_bytes(), "test.ll"))
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
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(source.as_bytes(), "test.ll"))
        .expect("valid LLVM IR fixture");

    let out = import_module(&module, &["bump"]).expect("import succeeds");
    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };
    let has_read = body
        .values
        .iter()
        .any(|v| matches!(&v.kind, Value::Op(volar_ir_common::Stmt::StorageRead { .. })));
    let has_write = body
        .values
        .iter()
        .any(|v| matches!(&v.kind, Value::Op(volar_ir_common::Stmt::StorageWrite { .. })));
    assert!(has_read, "expected a StorageRead for the global load");
    assert!(has_write, "expected a StorageWrite for the global store");
    let _ = context;
}
