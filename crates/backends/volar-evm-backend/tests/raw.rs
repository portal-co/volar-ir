mod support;

use support::word::{self, Word};
use support::{Rng, circuits, emit_raw, emit_sol, evm, run_sol};

#[test]
fn exhaustive_three_input_equivalence() {
    let circuit = circuits::and_or_tree();
    let blocks = circuits::to_blocks(&circuit);
    let code = emit_raw(&circuit);
    // Pack all 2^3 input combinations into lanes 0..8 of three words.
    let mut inputs = [word::ZERO; 3];
    for combo in 0..8usize {
        for param in 0..3 {
            if (combo >> param) & 1 != 0 {
                word::set_bit(&mut inputs[param], combo);
            }
        }
    }
    let (outputs, _gas) = evm::run(&code, &inputs).expect("bytecode runs");
    for lane in 0..8 {
        let params: Vec<bool> = (0..3).map(|param| (lane >> param) & 1 != 0).collect();
        let expected =
            volar_fuzz::interpreter::biir::eval_biir(&blocks, &params).expect("circuit terminates");
        assert_eq!(outputs.len(), expected.len());
        for (packed, expected) in outputs.iter().zip(&expected) {
            assert_eq!(word::bit(*packed, lane), *expected, "lane {lane}");
        }
    }
}

#[test]
fn random_packed_lanes_match_biir() {
    let circuit = circuits::xor_reduce(8);
    let blocks = circuits::to_blocks(&circuit);
    let code = emit_raw(&circuit);
    let mut rng = Rng(0x1234_5678_9ABC_DEF0);
    let inputs: Vec<Word> = (0..8).map(|_| rng.word()).collect();
    let (outputs, _gas) = evm::run(&code, &inputs).expect("bytecode runs");
    assert_eq!(outputs.len(), 1);
    for lane in 0..256 {
        let params: Vec<bool> = inputs.iter().map(|w| word::bit(*w, lane)).collect();
        let expected =
            volar_fuzz::interpreter::biir::eval_biir(&blocks, &params).expect("circuit terminates");
        assert_eq!(word::bit(outputs[0], lane), expected[0], "lane {lane}");
    }
}

#[test]
fn constant_gates_evaluate() {
    let circuit = circuits::const_gates();
    let blocks = circuits::to_blocks(&circuit);
    let code = emit_raw(&circuit);
    let mut rng = Rng(0xDEAD_BEEF_CAFE_F00D);
    let inputs = [rng.word()];
    let (outputs, _gas) = evm::run(&code, &inputs).expect("bytecode runs");
    assert_eq!(outputs.len(), 4);
    for lane in 0..256 {
        let params = [word::bit(inputs[0], lane)];
        let expected =
            volar_fuzz::interpreter::biir::eval_biir(&blocks, &params).expect("circuit terminates");
        for (packed, expected) in outputs.iter().zip(&expected) {
            assert_eq!(word::bit(*packed, lane), *expected, "lane {lane}");
        }
    }
}

/// The two emitters built on the `EvmEmit` seam must agree: same circuit,
/// same packed inputs, same packed outputs.
#[test]
fn solidity_and_raw_emitters_agree() {
    let mut rng = Rng(0x0B5E_5500_1BAD_CAFE);
    for circuit in [
        circuits::full_adder(),
        circuits::and_or_tree(),
        circuits::xor_reduce(8),
    ] {
        let inputs: Vec<Word> = (0..circuit.params as usize).map(|_| rng.word()).collect();
        let sol_outputs = run_sol(&emit_sol(&circuit), &inputs);
        let (raw_outputs, _gas) = evm::run(&emit_raw(&circuit), &inputs).expect("bytecode runs");
        assert_eq!(sol_outputs, raw_outputs);
    }
}

/// The amortization fixture: one packed run of a bit-heavy circuit against
/// 256 single-lane runs of the *same* emitted bytecode. The bound is
/// conservative — 3-gas word gates against 256 same-priced scalar gates
/// gives a ~256x margin on gate work, and input staging is identical
/// per-run work in both modes.
#[test]
fn packed_gas_amortizes_over_256_scalar_runs() {
    let circuit = circuits::xor_reduce(64);
    let code = emit_raw(&circuit);
    let mut rng = Rng(0x0DDC_0FFE_E15E_600D);
    let inputs: Vec<Word> = (0..64).map(|_| rng.word()).collect();
    let (packed_out, packed_gas) = evm::run(&code, &inputs).expect("packed run");

    let mut scalar_gas = 0u64;
    for lane in 0..256 {
        let unpacked: Vec<Word> = inputs
            .iter()
            .map(|w| word::from_u64(u64::from(word::bit(*w, lane))))
            .collect();
        let (out, gas) = evm::run(&code, &unpacked).expect("scalar run");
        scalar_gas += gas;
        assert_eq!(
            word::bit(packed_out[0], lane),
            word::bit(out[0], 0),
            "lane {lane}"
        );
    }

    println!(
        "xor_reduce(64): packed = {packed_gas} gas, 256 scalar runs = {scalar_gas} gas \
         ({:.1}x)",
        scalar_gas as f64 / packed_gas as f64,
    );
    assert!(
        packed_gas.saturating_mul(64) <= scalar_gas,
        "packed run ({packed_gas}) must amortize at least 64x over scalar total ({scalar_gas})",
    );
}
