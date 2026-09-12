//! AX3: the LLVM extern symbol `__portal_aes128_encrypt_block` lowers to an
//! IR-level `Stmt::OracleCall` against the registered `aes128_encrypt_block`
//! oracle — never a declaration-only `Value::Call` — with the key/plaintext
//! loaded and ciphertext stored through the memory-intrinsic pointer
//! machinery.

use inkwell::context::Context;
use inkwell::memory_buffer::MemoryBuffer;
use vaffle::{FuncDecl, Value};
use volar_ir_common::{Stmt, aes_extern};
use volar_llvm_vaffle_import::import_module;

const AES_EXTERN: &str = r#"
declare void @__portal_aes128_encrypt_block(ptr, ptr, ptr)

define i32 @f(i32 %x) {
entry:
  %key = alloca [2 x i64], align 8
  %pt = alloca [2 x i64], align 8
  %out = alloca [2 x i64], align 8
  %x64 = zext i32 %x to i64
  store i64 %x64, ptr %pt, align 8
  call void @__portal_aes128_encrypt_block(ptr %out, ptr %key, ptr %pt)
  %lo = load i64, ptr %out, align 8
  %r = trunc i64 %lo to i32
  ret i32 %r
}
"#;

#[test]
fn aes_extern_lowers_to_oracle_call() {
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            AES_EXTERN.as_bytes(),
            "test.ll",
        ))
        .expect("valid LLVM IR fixture");
    let out = import_module(&module, &["f"]).expect("aes extern import");

    // The extern becomes a registered oracle, not an imported callee.
    assert!(
        out.funcs
            .iter()
            .all(|f| !matches!(f, FuncDecl::Import { name, .. } if name == aes_extern::LLVM_SYMBOL)),
        "the AES extern must not become an imported callee"
    );
    let decl = out
        .oracles
        .iter()
        .find(|o| o.name == aes_extern::ORACLE_NAME)
        .expect("aes128_encrypt_block oracle registered");
    assert_eq!(decl.params.len(), 4, "(key_lo, key_hi, pt_lo, pt_hi)");
    assert_eq!(decl.results.len(), 2, "(ct_lo, ct_hi)");

    let FuncDecl::Body(body) = &out.funcs[0] else {
        panic!("expected a function body");
    };
    let calls: Vec<&volar_ir_common::Node<Value>> = body
        .values
        .iter()
        .filter(|v| matches!(&v.kind, Value::Op(Stmt::OracleCall { name, .. }) if name == aes_extern::ORACLE_NAME))
        .collect();
    assert_eq!(calls.len(), 1, "exactly one AES oracle call");
    let Value::Op(Stmt::OracleCall { args, .. }) = &calls[0].kind else {
        unreachable!()
    };
    assert_eq!(args.len(), 4);

    // Two OracleOutput projections, one per ciphertext word.
    let projections = body
        .values
        .iter()
        .filter(|v| matches!(&v.kind, Value::Op(Stmt::OracleOutput { .. })))
        .count();
    assert_eq!(projections, 2);
}
