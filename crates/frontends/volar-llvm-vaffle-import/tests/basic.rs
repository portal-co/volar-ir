//! Structural-import fixture tests: hand-written `.ll` sources, imported via
//! [`volar_llvm_vaffle_import::import_module`], asserted against the
//! resulting `vaffle::Module`'s shape — call preservation (no inlining),
//! block/phi/param structure, and terminator kinds.

use inkwell::context::Context;
use inkwell::memory_buffer::MemoryBuffer;
use vaffle::{FuncDecl, Terminator, Value};
use volar_ir_common::Stmt;
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
    assert!(has_read, "expected an ALLOCA StorageRead for the spill load");
    assert!(has_write, "expected an ALLOCA StorageWrite for the spill store");
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
    let out =
        import_module(&module, &["dynamic_gep"]).expect("symbolic-index GEP into alloca must import");
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
fn memory_intrinsic_symbolic_length_is_named_unsupported() {
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
    let err = import_module(&module, &["symbolic"]).expect_err("symbolic length must fail closed");
    assert!(
        err.to_string().contains("length"),
        "expected named intrinsic-length error, got {err}"
    );
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
fn global_gep_memory_intrinsic_is_named_unsupported() {
    let source = r#"
@bytes = global [4 x i8] zeroinitializer
declare void @llvm.memset.p0.i64(ptr, i8, i64, i1 immarg)

define void @offset_global() {
entry:
  call void @llvm.memset.p0.i64(ptr getelementptr inbounds ([4 x i8], ptr @bytes, i64 0, i64 1), i8 0, i64 1, i1 false)
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
    let err = import_module(&module, &["offset_global"])
        .expect_err("global GEP intrinsic pointer must fail closed");
    assert!(
        err.to_string().contains("global GEP"),
        "expected named global-GEP error, got {err}"
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
        !body.blocks[1].stmts.iter().any(|value| matches!(
            body.values[value.0].kind,
            Value::Call { .. }
        )),
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
