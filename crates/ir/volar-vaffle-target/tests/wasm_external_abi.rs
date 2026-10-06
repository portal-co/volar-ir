//! Fail-closed admission coverage for configured WASM action/oracle imports.

use volar_ir_common::{ActionExecutionPolicy, OracleExecutionPolicy};
use volar_vaffle_target::{VaffleTarget, WaffleImportConfig, lower_waffle_module};

fn lower(source: &str, config: WaffleImportConfig) -> Vec<(String, String)> {
    let bytes = wat::parse_str(source).expect("WAT assembles");
    let mut wasm = portal_pc_waffle_frontend::from_wasm_bytes(
        &bytes,
        &portal_pc_waffle_frontend::FrontendOptions::default(),
    )
    .expect("WASM parses");
    portal_pc_waffle_frontend::expand_all_funcs(&mut wasm).expect("WASM functions expand");
    let mut target = VaffleTarget::new();
    lower_waffle_module(&wasm, &mut target, &config)
        .into_iter()
        .map(|(name, error)| (name, error.to_string()))
        .collect()
}

#[test]
fn configured_wasm_external_reuses_matching_declaration() {
    let source = r#"(module
      (import "portal" "left" (func $left (param i32) (result i32)))
      (import "portal" "right" (func $right (param i32) (result i32)))
      (func (export "entry") (param i32) (result i32)
        (call $right (call $left (local.get 0)))))"#;
    let bytes = wat::parse_str(source).expect("WAT assembles");
    let mut wasm = portal_pc_waffle_frontend::from_wasm_bytes(
        &bytes,
        &portal_pc_waffle_frontend::FrontendOptions::default(),
    )
    .expect("WASM parses");
    portal_pc_waffle_frontend::expand_all_funcs(&mut wasm).expect("WASM functions expand");
    let mut target = VaffleTarget::new();
    let policy = OracleExecutionPolicy::legacy_evaluator();
    let errors = lower_waffle_module(
        &wasm,
        &mut target,
        &WaffleImportConfig::new()
            .with_oracle_execution("portal.left", "shared", policy)
            .with_oracle_execution("portal.right", "shared", policy),
    );
    assert!(errors.is_empty(), "unexpected errors: {errors:?}");
    assert_eq!(
        target
            .module
            .oracles
            .iter()
            .filter(|decl| decl.name == "shared")
            .count(),
        1
    );
}

#[test]
fn configured_wasm_external_rejects_inconsistent_duplicate_declaration() {
    let source = r#"(module
      (import "portal" "left" (func $left (param i32) (result i32)))
      (import "portal" "right" (func $right (param i64) (result i32)))
      (func (export "entry") (param i32) (result i32)
        (call $left (local.get 0))))"#;
    let bytes = wat::parse_str(source).expect("WAT assembles");
    let mut wasm = portal_pc_waffle_frontend::from_wasm_bytes(
        &bytes,
        &portal_pc_waffle_frontend::FrontendOptions::default(),
    )
    .expect("WASM parses");
    portal_pc_waffle_frontend::expand_all_funcs(&mut wasm).expect("WASM functions expand");
    let mut target = VaffleTarget::new();
    let policy = OracleExecutionPolicy::legacy_evaluator();
    let errors = lower_waffle_module(
        &wasm,
        &mut target,
        &WaffleImportConfig::new()
            .with_oracle_execution("portal.left", "shared", policy)
            .with_oracle_execution("portal.right", "shared", policy),
    );
    assert_eq!(errors.len(), 1);
    assert!(errors[0].1.0.contains("inconsistent declarations"));
    assert!(target.module.oracles.is_empty());
}

#[test]
fn configured_wasm_external_rejection_leaves_no_partial_declarations() {
    let source = r#"(module
      (import "portal" "pure" (func $pure (param i32) (result i32)))
      (func (export "entry") (param i32) (result i32)
        (call $pure (local.get 0))))"#;
    let bytes = wat::parse_str(source).expect("WAT assembles");
    let mut wasm = portal_pc_waffle_frontend::from_wasm_bytes(
        &bytes,
        &portal_pc_waffle_frontend::FrontendOptions::default(),
    )
    .expect("WASM parses");
    portal_pc_waffle_frontend::expand_all_funcs(&mut wasm).expect("WASM functions expand");
    let mut target = VaffleTarget::new();
    let errors = lower_waffle_module(
        &wasm,
        &mut target,
        &WaffleImportConfig::new()
            .with_oracle_execution(
                "portal.pure",
                "pure",
                OracleExecutionPolicy::legacy_evaluator(),
            )
            .with_action_execution(
                "portal.missing",
                "missing",
                0,
                ActionExecutionPolicy::legacy_evaluator(),
            ),
    );
    assert_eq!(errors.len(), 1);
    assert!(target.module.oracles.is_empty());
    assert!(target.module.actions.is_empty());
}

#[test]
fn configured_wasm_external_rejects_unresolved_import() {
    let errors = lower(
        r#"(module (func (export "entry") (result i32) (i32.const 0)))"#,
        WaffleImportConfig::new().with_oracle_execution(
            "portal.pure",
            "pure",
            OracleExecutionPolicy::legacy_evaluator(),
        ),
    );
    assert_eq!(errors.len(), 1);
    assert!(errors[0].1.contains("is not imported by the WASM module"));
}

#[test]
fn configured_wasm_action_requires_guard_and_fallback_abi() {
    let errors = lower(
        r#"(module
          (import "portal" "act" (func $act (param i64 i32 i32) (result i32)))
          (func (export "entry") (param i32) (result i32)
            (call $act (i64.const 1) (local.get 0) (local.get 0))))"#,
        WaffleImportConfig::new().with_action_execution(
            "portal.act",
            "act",
            1,
            ActionExecutionPolicy::legacy_evaluator(),
        ),
    );
    assert_eq!(errors.len(), 1);
    assert!(errors[0].1.contains("guard must be i32"));
}

#[test]
fn configured_wasm_external_rejects_float_abi() {
    let errors = lower(
        r#"(module
          (import "portal" "pure" (func $pure (param f32) (result i32)))
          (func (export "entry") (param f32) (result i32)
            (call $pure (local.get 0))))"#,
        WaffleImportConfig::new().with_oracle_execution(
            "portal.pure",
            "pure",
            OracleExecutionPolicy::legacy_evaluator(),
        ),
    );
    assert_eq!(errors.len(), 1);
    assert!(
        errors[0]
            .1
            .contains("requires scalar integer parameters and results")
    );
}
