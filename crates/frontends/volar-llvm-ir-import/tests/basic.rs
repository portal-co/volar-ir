//! Execution-mode import fixture tests: hand-written `.ll` sources, imported
//! via [`volar_llvm_ir_import::import`], asserted against the resulting
//! [`volar_ir::ir::IRBlocks`]' shape — flat single-block structure, free
//! parameters only for genuinely symbolic inputs (never for an inlined
//! callee's own parameters), and hard failure on unbounded recursion.

use inkwell::context::Context;
use volar_ir::ir::{IRBlockTargetId, IRTerminator};
use volar_llvm_ir_import::{import, LoweringLimits, ModuleInput};

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
    let (blocks, _types) = import(
        &context,
        ModuleInput::Assembly(source),
        "add",
        LoweringLimits::default(),
    )
    .expect("import succeeds");

    assert_eq!(blocks.blocks.len(), 1);
    let block = &blocks.blocks[0];
    // Two i32 params, both symbolic: 32 free bits each.
    assert_eq!(block.params.len(), 64);
    match &block.terminator {
        IRTerminator::Jmp { target } => {
            assert_eq!(target.dest, IRBlockTargetId::Return);
            assert_eq!(target.args.len(), 32, "i32 result should be 32 bits");
        }
        other => panic!("expected a Jmp{{dest: Return}} terminator, got {other:?}"),
    }
}

#[test]
fn call_is_inlined() {
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
    let (blocks, _types) = import(
        &context,
        ModuleInput::Assembly(source),
        "caller",
        LoweringLimits::default(),
    )
    .expect("import succeeds");

    assert_eq!(blocks.blocks.len(), 1);
    let block = &blocks.blocks[0];
    // Only caller's own %x is a free input; callee's parameter is bound to
    // caller's argument during inlining, not a second set of free bits.
    assert_eq!(block.params.len(), 32);
    assert!(!block.stmts.is_empty(), "callee's body should have been executed inline");
}

#[test]
fn concretely_bounded_loop() {
    // The loop counter/condition are derived only from constants (0, 1, 8),
    // so they stay concrete throughout and the interpreter unrolls the loop
    // via real Rust-level recursion through basic blocks; %a (and the %acc
    // chain derived from it) remains symbolic without affecting any branch.
    let source = r#"
define i32 @sum8(i32 %a) {
entry:
  br label %loop
loop:
  %i = phi i32 [ 0, %entry ], [ %i.next, %loop ]
  %acc = phi i32 [ %a, %entry ], [ %acc.next, %loop ]
  %acc.next = add i32 %acc, %i
  %i.next = add i32 %i, 1
  %cond = icmp slt i32 %i.next, 8
  br i1 %cond, label %loop, label %exit
exit:
  ret i32 %acc.next
}
"#;
    let context = Context::create();
    let (blocks, _types) = import(
        &context,
        ModuleInput::Assembly(source),
        "sum8",
        LoweringLimits::default(),
    )
    .expect("import succeeds");

    assert_eq!(blocks.blocks.len(), 1);
    let block = &blocks.blocks[0];
    assert_eq!(block.params.len(), 32, "only %a is symbolic");
    match &block.terminator {
        IRTerminator::Jmp { target } => {
            assert_eq!(target.dest, IRBlockTargetId::Return);
            assert_eq!(target.args.len(), 32);
        }
        other => panic!("expected a Jmp{{dest: Return}} terminator, got {other:?}"),
    }
}

#[test]
fn void_return() {
    let source = r#"
define void @noop(i32 %a) {
entry:
  ret void
}
"#;
    let context = Context::create();
    let (blocks, _types) = import(
        &context,
        ModuleInput::Assembly(source),
        "noop",
        LoweringLimits::default(),
    )
    .expect("import succeeds");

    let block = &blocks.blocks[0];
    match &block.terminator {
        IRTerminator::Jmp { target } => {
            assert_eq!(target.dest, IRBlockTargetId::Return);
            assert!(target.args.is_empty(), "void return should have no output bits");
        }
        other => panic!("expected a Jmp{{dest: Return}} terminator, got {other:?}"),
    }
}

#[test]
fn unbounded_recursion_is_a_hard_error() {
    let source = r#"
define i32 @rec(i32 %n) {
entry:
  %r = call i32 @rec(i32 %n)
  ret i32 %r
}
"#;
    let context = Context::create();
    let result = import(
        &context,
        ModuleInput::Assembly(source),
        "rec",
        LoweringLimits::default(),
    );
    let err = result.expect_err("recursive call must be rejected");
    let message = err.to_string();
    assert!(
        message.contains("recursive"),
        "expected a recursion diagnostic, got: {message}"
    );
}
