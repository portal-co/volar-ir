use inkwell::context::Context;
use inkwell::memory_buffer::MemoryBuffer;
use volar_ir::ir::IRStmt;
use volar_ir_common::{
    ActionExecutionPolicy, ExternalExecutor, ExternalRevealPolicy, OracleExecutionKind,
    OracleExecutionPolicy,
};
use volar_llvm_vaffle_import::{LlvmImportConfig, import_module_with_config};
use volar_vaffle_target::lower_vaffle_to_ir;

#[test]
fn configured_llvm_externals_survive_to_ir() {
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
        .unwrap();
    let oracle = OracleExecutionPolicy {
        execution: OracleExecutionKind::Assigned,
        executor: ExternalExecutor::Garbler,
        reveal: ExternalRevealPolicy::BothRoles,
        fingerprint: [0x31; 32],
    };
    let action = ActionExecutionPolicy {
        executor: ExternalExecutor::Garbler,
        reveal: ExternalRevealPolicy::BothRoles,
        fingerprint: [0x32; 32],
    };
    let config = LlvmImportConfig::default()
        .with_oracle_execution("pure", oracle)
        .with_action_execution("act", 1, action);
    let module = import_module_with_config(&module, &["entry"], config).unwrap();
    let (ir, _) = lower_vaffle_to_ir(&module);

    assert_eq!(ir.oracles.iter().filter(|decl| decl.name == "pure").count(), 1);
    assert_eq!(ir.actions.iter().filter(|decl| decl.name == "act").count(), 1);
    assert_eq!(ir.oracles[0].execution, oracle);
    assert_eq!(ir.actions[0].execution, action);
    assert!(ir.blocks.iter().flat_map(|block| block.stmts.iter()).any(|stmt| {
        matches!(stmt.kind, IRStmt::OracleCall { ref name, .. } if name == "pure")
    }));
    assert!(ir.blocks.iter().flat_map(|block| block.stmts.iter()).any(|stmt| {
        matches!(stmt.kind, IRStmt::ActionCall { ref name, .. } if name == "act")
    }));
}
