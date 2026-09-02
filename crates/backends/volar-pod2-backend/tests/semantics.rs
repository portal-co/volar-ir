//! Compare Product/Sum verification to the Boolar interpreter.

use volar_circuit_source::{
    emit_bool_circuit, eval_named_bool, name_bool_circuit, BoolStorageMap, EmitOptions,
};
use volar_fuzz::interpreter::biir::eval_biir;
use volar_ir::boolar::BIrStmt;
use volar_ir::circuit::BCircuit;
use volar_ir::ir::IRVarId;
use volar_ir_build::{BoolarCircuitStage, Pipeline};
use volar_pod2_backend::{sanitize_ident, verify_named_bool, Pod2Backend};

fn mix_circuit() -> BCircuit<()> {
    let mut c = BCircuit::new(2);
    c.push_stmt(BIrStmt::And(IRVarId(0), IRVarId(1)), ());
    c.push_stmt(BIrStmt::Or(IRVarId(0), IRVarId(1)), ());
    c.push_stmt(BIrStmt::Xor(IRVarId(2), IRVarId(3)), ());
    c.push_stmt(BIrStmt::Not(IRVarId(4)), ());
    c.outputs = vec![IRVarId(5)];
    c
}

#[test]
fn product_sum_matches_biir() {
    let circuit = mix_circuit();
    let named = name_bool_circuit(&circuit, &EmitOptions::default(), sanitize_ident)
        .unwrap();
    let blocks = circuit.clone().to_bir_blocks();
    for a in [false, true] {
        for b in [false, true] {
            let mut st = BoolStorageMap::new();
            let named_out = eval_named_bool(&named, &[a, b], &mut st).unwrap();
            let biir_out = eval_biir(&blocks, &[a, b]).unwrap();
            assert_eq!(named_out, biir_out, "inputs {a},{b}");
            assert!(
                verify_named_bool(&named, &[a, b]).unwrap(),
                "Product/Sum witness check failed for {a},{b}"
            );
        }
    }
}

#[test]
fn pipeline_emit_is_a_module() {
    let pkg = Pipeline::<BoolarCircuitStage>::from_data(mix_circuit())
        .emit_source(&Pod2Backend, &EmitOptions::default())
        .unwrap();
    let src = pkg.file_text("volar_circuit.podlang").unwrap();
    assert!(src.contains("bool_xor"));
    assert!(src.contains("eval_circuit"));
    assert!(!src.contains("REQUEST("));
    assert!(emit_bool_circuit(&mix_circuit(), &Pod2Backend, &EmitOptions::default())
        .unwrap()
        .file("examples/embed.podlang")
        .is_some());
}
