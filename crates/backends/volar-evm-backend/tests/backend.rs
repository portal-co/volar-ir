//! The `CircuitSourceBackend` adapter: standard-pipeline emission of the
//! Solidity library as a `SourcePackage`.

mod support;

use support::word::{self, Word};
use support::{Rng, circuits, run_sol};

use volar_circuit_source::{EmitOptions, emit_bool_circuit};
use volar_evm_backend::EvmSolidityBackend;

#[test]
fn emits_a_sol_library_package() {
    let pkg = emit_bool_circuit(
        &circuits::full_adder(),
        &EvmSolidityBackend::default(),
        &EmitOptions::default(),
    )
    .expect("emit");
    // The circuit name option drives the Solidity function name.
    let src = pkg
        .file_text("VolarEvmSimd.sol")
        .expect("a .sol library file");
    assert!(src.contains("library VolarEvmSimd {"), "{src}");
    assert!(src.contains("function eval_circuit("), "{src}");
}

#[test]
fn package_body_is_whitelist_yul_and_correct() {
    let circuit = circuits::and_or_tree();
    let blocks = circuits::to_blocks(&circuit);
    let pkg = emit_bool_circuit(
        &circuit,
        &EvmSolidityBackend::default(),
        &EmitOptions::default(),
    )
    .expect("emit");
    let src = pkg.file_text("VolarEvmSimd.sol").expect("a .sol file");

    let mut rng = Rng(0x5EED_5EED_5EED_5EED);
    let inputs: Vec<Word> = (0..3).map(|_| rng.word()).collect();
    let outputs = run_sol(src, &inputs);
    assert_eq!(outputs.len(), 1);
    for lane in 0..256 {
        let params: Vec<bool> = inputs.iter().map(|w| word::bit(*w, lane)).collect();
        let expected =
            volar_fuzz::interpreter::biir::eval_biir(&blocks, &params).expect("circuit terminates");
        assert_eq!(word::bit(outputs[0], lane), expected[0], "lane {lane}");
    }
}

#[test]
fn storage_circuits_are_rejected() {
    let err = emit_bool_circuit(
        &circuits::storage_read(),
        &EvmSolidityBackend::default(),
        &EmitOptions::default(),
    )
    .expect_err("storage circuits are rejected");
    assert!(format!("{err}").contains("storage"), "{err}");
}
