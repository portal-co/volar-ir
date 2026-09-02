//! Compare named-circuit evaluation to the Boolar interpreter.

use volar_circuit_source::{
    emit_bool_circuit, eval_named_bool, name_bool_circuit, BoolStorageMap, EmitOptions,
};
use volar_fuzz::interpreter::biir::eval_biir;
use volar_ir::boolar::BIrStmt;
use volar_ir::circuit::BCircuit;
use volar_ir::ir::IRVarId;
use volar_ir_build::{BoolarCircuitStage, Pipeline};
use volar_noir_backend::{sanitize_ident, NoirBackend};

fn xor_not_circuit() -> BCircuit<()> {
    let mut c = BCircuit::new(2);
    c.push_stmt(BIrStmt::Xor(IRVarId(0), IRVarId(1)), ());
    c.push_stmt(BIrStmt::Not(IRVarId(2)), ());
    c.outputs = vec![IRVarId(3)];
    c
}

#[test]
fn named_eval_matches_biir() {
    let circuit = xor_not_circuit();
    let named = name_bool_circuit(&circuit, &EmitOptions::default(), sanitize_ident)
        .unwrap();
    let blocks = circuit.clone().to_bir_blocks();
    for a in [false, true] {
        for b in [false, true] {
            let mut st = BoolStorageMap::new();
            let named_out = eval_named_bool(&named, &[a, b], &mut st).unwrap();
            let biir_out = eval_biir(&blocks, &[a, b]).unwrap();
            assert_eq!(named_out, biir_out, "inputs {a},{b}");
        }
    }
}

#[test]
fn pipeline_emit_is_a_lib() {
    let pkg = Pipeline::<BoolarCircuitStage>::from_data(xor_not_circuit())
        .emit_source(&NoirBackend, &EmitOptions::default())
        .unwrap();
    let lib = pkg.file_text("src/lib.nr").unwrap();
    assert!(lib.contains("pub fn eval_circuit"));
    assert!(lib.contains("!="));
    assert!(emit_bool_circuit(&xor_not_circuit(), &NoirBackend, &EmitOptions::default())
        .unwrap()
        .file_text("Nargo.toml")
        .unwrap()
        .contains("type = \"lib\""));
}
