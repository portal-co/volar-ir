mod support;

use support::word::{self, Word};
use support::{Rng, circuits, emit_sol, emit_sol_with, run_sol, yul};

use volar_evm_backend::{EvmEmitError, SolidityAsm, SolidityOptions, emit_circuit};

#[test]
fn golden_full_adder_sol() {
    let src = emit_sol(&circuits::full_adder());
    if std::env::var_os("BLESS").is_some() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/full_adder.sol");
        std::fs::write(path, &src).expect("bless golden");
    }
    assert_eq!(src, include_str!("golden/full_adder.sol"));
}

#[test]
fn emitted_body_uses_only_whitelisted_yul() {
    for circuit in [
        circuits::full_adder(),
        circuits::and_or_tree(),
        circuits::xor_reduce(8),
    ] {
        let src = emit_sol(&circuit);
        let body = yul::extract_body(&src);
        // Parsing only accepts the whitelist subset.
        yul::check(body).expect("body parses in the whitelist subset");
        for kw in [
            "for ",
            "if ",
            "switch",
            "function ",
            "jump",
            "verbatim",
            "leave",
            "sstore",
            "sload",
            "calldata",
            "keccak",
            "pop",
            "dup",
            "swap",
            "revert",
            "return",
            "selfdestruct",
        ] {
            assert!(!body.contains(kw), "body must not contain `{kw}`");
        }
    }
}

#[test]
fn exhaustive_three_input_equivalence() {
    let circuit = circuits::and_or_tree();
    let blocks = circuits::to_blocks(&circuit);
    let src = emit_sol(&circuit);
    // Pack all 2^3 input combinations into lanes 0..8 of three words.
    let mut inputs = [word::ZERO; 3];
    for combo in 0..8usize {
        for param in 0..3 {
            if (combo >> param) & 1 != 0 {
                word::set_bit(&mut inputs[param], combo);
            }
        }
    }
    let outputs = run_sol(&src, &inputs);
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
    let src = emit_sol(&circuit);
    let mut rng = Rng(0x1234_5678_9ABC_DEF0);
    let inputs: Vec<Word> = (0..8).map(|_| rng.word()).collect();
    let outputs = run_sol(&src, &inputs);
    assert_eq!(outputs.len(), 1);
    for lane in 0..256 {
        let params: Vec<bool> = inputs.iter().map(|w| word::bit(*w, lane)).collect();
        let expected =
            volar_fuzz::interpreter::biir::eval_biir(&blocks, &params).expect("circuit terminates");
        assert_eq!(word::bit(outputs[0], lane), expected[0], "lane {lane}");
    }
}

#[test]
fn constant_gates_emit_and_evaluate() {
    let circuit = circuits::const_gates();
    let blocks = circuits::to_blocks(&circuit);
    let src = emit_sol(&circuit);
    let mut rng = Rng(0xDEAD_BEEF_CAFE_F00D);
    let inputs = [rng.word()];
    let outputs = run_sol(&src, &inputs);
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

#[test]
fn storage_circuits_are_rejected() {
    let circuit = circuits::storage_read();
    let err = emit_circuit(&circuit, SolidityAsm::new(SolidityOptions::default()))
        .expect_err("storage circuits are rejected");
    assert_eq!(err, EvmEmitError::NotLaneCompatible);
}

/// Optional differential against the real toolchain: when `solc` is
/// available (`SOLC_BIN` overrides the binary name), the emitted library is
/// wrapped in a harness contract and compiled with `--via-ir --optimize`.
/// This proves solc and the Yul optimizer accept the emitted body —
/// including full Yul→EVM stack allocation, which is where
/// `stack-too-deep` would surface. Executing the compiled harness needs a
/// full EVM and is future work (`revm`, behind an off-by-default feature);
/// semantic equivalence is covered by the whitelist-Yul interpreter above.
#[test]
fn solc_compiles_and_optimizes_emitted_solidity() {
    let solc = std::env::var("SOLC_BIN")
        .ok()
        .filter(|p| !p.is_empty())
        .unwrap_or_else(|| "solc".to_string());
    let Ok(version) = std::process::Command::new(&solc).arg("--version").output() else {
        eprintln!("skipping: `{solc}` not found");
        return;
    };
    if !version.status.success() {
        eprintln!("skipping: `{solc} --version` failed");
        return;
    }

    let opts = SolidityOptions::default();
    let library = emit_sol_with(&circuits::full_adder(), opts.clone());
    let harness = format!(
        "{library}\n\
         contract VolarEvmHarness {{\n\
         \x20   function run(uint256[] memory inputs) external pure returns (uint256[] memory outputs) {{\n\
         \x20       return {}.{}(inputs);\n\
         \x20   }}\n\
         }}\n",
        opts.library_name, opts.function_name,
    );
    let dir = std::env::temp_dir().join(format!("volar-evm-backend-solc-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let path = dir.join("Harness.sol");
    std::fs::write(&path, &harness).expect("write harness");
    let output = std::process::Command::new(&solc)
        .args([
            "--via-ir",
            "--optimize",
            "--bin",
            path.to_str().expect("utf-8 temp path"),
        ])
        .output()
        .expect("run solc");
    assert!(
        output.status.success(),
        "solc rejected the emitted Solidity:\n{}",
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Binary of the runtime part:"),
        "solc produced no runtime binary:\n{stdout}",
    );
}
