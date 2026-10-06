// @reliability: normal
//! End-to-end field semantics for the public CBackend `LirTarget` interface.

use volar_c_backend::CBackend;
use volar_ir_common::Type as NativeType;
use volar_lir::{IcmpPred, LirTarget, LirType};
use volar_lir_test_corpus::compile_and_run;

fn aes8() -> LirType {
    LirType::ExtField {
        wrapped: Box::new(LirType::Native(NativeType::Bit)),
        degree: 8,
        irreducible: vec![1, 1, 0, 1, 1, 0, 0, 0, 1],
    }
}

#[test]
fn prime_field_operations_reduce_in_gf3() {
    let field = LirType::PrimeField { k: 2, n: vec![1] };
    let mut backend = CBackend::new();
    for (name, operation) in [("z3_add", "add"), ("z3_sub", "sub"), ("z3_mul", "mul")] {
        let (entry, params) =
            backend.begin_function(name, &[field.clone(), field.clone()], Some(field.clone()));
        backend.switch_to_block(entry);
        let result = match operation {
            "add" => backend.add(params[0][0], params[1][0]),
            "sub" => backend.sub(params[0][0], params[1][0]),
            "mul" => backend.mul(params[0][0], params[1][0]),
            _ => unreachable!(),
        };
        backend.ret(&[result]);
        backend.end_function();
    }

    let source = backend.finish();
    let output = compile_and_run(
        &source,
        r#"  printf("%u %u %u", z3_add(2, 2), z3_sub(0, 1), z3_mul(2, 2));"#,
    );
    assert_eq!(output.trim(), "1 2 1");
}

#[test]
fn prime_field_normalizes_constants_and_arithmetic_operands() {
    let field = LirType::PrimeField { k: 3, n: vec![3] }; // GF(5)
    let mut backend = CBackend::new();
    for (name, operation) in [("gf5_add", "add"), ("gf5_sub", "sub"), ("gf5_mul", "mul")] {
        let (entry, params) =
            backend.begin_function(name, &[field.clone(), field.clone()], Some(field.clone()));
        backend.switch_to_block(entry);
        let result = match operation {
            "add" => backend.add(params[0][0], params[1][0]),
            "sub" => backend.sub(params[0][0], params[1][0]),
            "mul" => backend.mul(params[0][0], params[1][0]),
            _ => unreachable!(),
        };
        backend.ret(&[result]);
        backend.end_function();
    }
    let (entry, _) = backend.begin_function("gf5_negative_one", &[], Some(field.clone()));
    backend.switch_to_block(entry);
    let negative_one = backend.iconst(field.clone(), -1);
    backend.ret(&[negative_one]);
    backend.end_function();
    let (entry, _) = backend.begin_function("gf5_const_seven", &[], Some(field.clone()));
    backend.switch_to_block(entry);
    let seven = backend.iconst(field, 7);
    backend.ret(&[seven]);
    backend.end_function();

    let source = backend.finish();
    let output = compile_and_run(
        &source,
        r#"  printf("%u %u %u %u %u", gf5_add(7, 7), gf5_sub(0, 7), gf5_mul(7, 7), gf5_negative_one(), gf5_const_seven());"#,
    );
    assert_eq!(output.trim(), "4 3 4 4 2");
}

#[test]
fn aes8_multiplication_matches_fips_197_product() {
    let mut backend = CBackend::new();
    let (entry, _) = backend.begin_function("aes8_product_matches", &[], Some(LirType::Bool));
    backend.switch_to_block(entry);

    let lhs = backend.iconst(aes8(), 0x57);
    let rhs = backend.iconst(aes8(), 0x13);
    let product = backend.mul(lhs, rhs);
    let expected = backend.iconst(aes8(), 0xfe);
    let matches = backend.icmp(IcmpPred::Eq, product, expected);
    backend.ret(&[matches]);
    backend.end_function();

    let source = backend.finish();
    let output = compile_and_run(&source, r#"  printf("%d", aes8_product_matches());"#);
    assert_eq!(output.trim(), "1");
}
