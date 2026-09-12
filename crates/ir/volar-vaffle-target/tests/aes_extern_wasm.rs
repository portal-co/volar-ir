//! AX2: the `portal_crypto.aes128_enc` WASM import maps to the
//! `aes128_encrypt_block` oracle (the `volar_ir_common::aes_extern` contract)
//! and survives the full waffle → vaffle → IRBlocks lowering as a real
//! `IRStmt::OracleCall`, validated against the registered `OracleDecl`.
//!
//! Regression coverage for two gaps this closes:
//! - Real-WASM imports carry their names in the module's import table
//!   (`"<module>.<field>"`), not in `FuncDecl::Import`'s empty name field;
//!   the config lookup previously could never match them.
//! - Oracle calls used to be emitted as `Value::Call` to an `env/` import,
//!   which `lower_to_ir` routed to an abort sink (and `vaffle_ssa` panicked
//!   on when the import landed at `FuncId(0)`); they are now emitted as
//!   `Value::Op(Stmt::OracleCall)` and bypass the call protocol entirely.

use volar_ir::ir::IRStmt;
use volar_ir_common::aes_extern;
use volar_vaffle_target::*;

/// `(module
///   (import "portal_crypto" "aes128_enc" (func (param i64 i64 i64 i64) (result i64 i64)))
///   (func $f (export "f") (param $x i64) (result i64) ...))`
///
/// One AES call; the two 64-bit ciphertext halves are XORed together so the
/// entry function has a single i64 result.
const AES_WAT: &str = r#"(module
  (import "portal_crypto" "aes128_enc" (func $aes (param i64 i64 i64 i64) (result i64 i64)))
  (func $f (export "f") (param $x i64) (result i64)
    (local $lo i64) (local $hi i64)
    (call $aes (local.get $x) (i64.const 0) (i64.const 1) (i64.const 2))
    (local.set $hi)
    (local.set $lo)
    (i64.xor (local.get $lo) (local.get $hi))))"#;

fn lower_aes_wat() -> (vaffle::Module, WaffleImportConfig) {
    let bytes = wat::parse_str(AES_WAT).expect("wat assembles");
    let mut wasm = portal_pc_waffle_frontend::from_wasm_bytes(
        &bytes,
        &portal_pc_waffle_frontend::FrontendOptions::default(),
    )
    .expect("wasm parses");
    portal_pc_waffle_frontend::expand_all_funcs(&mut wasm).expect("expand");

    let mut target = VaffleTarget::new();
    let config = WaffleImportConfig::new().with_portal_crypto_aes();
    let errors = lower_waffle_module(&wasm, &mut target, &config);
    assert!(errors.is_empty(), "lowering errors: {errors:?}");
    (target.module, config)
}

#[test]
fn aes_import_registers_oracle_and_no_env_import() {
    let (module, _config) = lower_aes_wat();

    // The oracle is registered under the contract name with the contract
    // signature: 4 x i64 params (key_lo, key_hi, pt_lo, pt_hi), 2 x i64
    // results (ct_lo, ct_hi).
    let decl = module
        .oracles
        .iter()
        .find(|o| o.name == aes_extern::ORACLE_NAME)
        .expect("aes128_encrypt_block oracle registered");
    assert_eq!(decl.params.len(), 4, "key/pt halves as four i64 params");
    assert_eq!(decl.results.len(), 2, "ciphertext as two i64 results");

    // The import never materializes as a vaffle env-import; the module
    // contains only the entry body.
    assert!(
        module
            .funcs
            .iter()
            .all(|f| !matches!(f, vaffle::FuncDecl::Import { .. })),
        "oracle import must not become a vaffle env import"
    );
}

#[test]
fn aes_import_survives_to_ir_as_oracle_call() {
    let (module, _config) = lower_aes_wat();
    let (ir, _types) = lower_vaffle_to_ir(&module);

    // The IRBlocks carry the oracle declaration for downstream validation.
    assert!(
        ir.oracles.iter().any(|o| o.name == aes_extern::ORACLE_NAME),
        "IRBlocks.oracles carries the contract name"
    );

    let calls: Vec<&IRStmt> = ir
        .blocks
        .iter()
        .flat_map(|b| b.stmts.iter().map(|s| &s.kind))
        .filter(|s| matches!(s, IRStmt::OracleCall { .. }))
        .collect();
    assert_eq!(calls.len(), 1, "exactly one oracle call in the IR");
    match calls[0] {
        IRStmt::OracleCall {
            name,
            args,
            output_tys,
            ..
        } => {
            assert_eq!(name, aes_extern::ORACLE_NAME);
            assert_eq!(args.len(), 4);
            assert_eq!(output_tys.len(), 2);
        }
        _ => unreachable!(),
    }
}
