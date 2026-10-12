pub mod circuits;
pub mod evm;
pub mod word;
pub mod yul;

use volar_evm_backend::{RawEvm, RawEvmConfig, SolidityAsm, SolidityOptions, emit_circuit};
use volar_ir::circuit::BCircuit;

use word::Word;

/// Memory placement used by [`run_sol`]: the `inputs` array pointer and the
/// initial free-memory pointer.
pub const INPUTS_PTR: u64 = 0x80;
pub const FREE_PTR: u64 = 0x400;

/// Emit a circuit as a Solidity library with default options.
pub fn emit_sol(circuit: &BCircuit<()>) -> String {
    emit_sol_with(circuit, SolidityOptions::default())
}

/// Emit a circuit as a Solidity library with the given options.
pub fn emit_sol_with(circuit: &BCircuit<()>, opts: SolidityOptions) -> String {
    emit_circuit(circuit, SolidityAsm::new(opts)).expect("circuit is lane-compatible")
}

/// Emit a circuit as raw EVM runtime bytecode with default scratch layout.
pub fn emit_raw(circuit: &BCircuit<()>) -> Vec<u8> {
    emit_circuit(circuit, RawEvm::new(RawEvmConfig::default())).expect("circuit is lane-compatible")
}

/// Run an emitted Solidity library body over packed input words and read
/// back the packed output words.
pub fn run_sol(src: &str, inputs: &[Word]) -> Vec<Word> {
    let body = yul::extract_body(src);
    let env = yul::run(body, INPUTS_PTR, inputs, FREE_PTR).expect("whitelist Yul runs");
    let out_ptr = word::as_u64(env.vars["outputs"]).expect("outputs pointer fits u64");
    let len = word::as_u64(env.mem[&out_ptr]).expect("outputs length fits u64");
    (0..len)
        .map(|i| env.mem[&(out_ptr + 0x20 * (i + 1))])
        .collect()
}

/// xorshift64* for deterministic pseudorandom packed words.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    pub fn word(&mut self) -> Word {
        [self.next(), self.next(), self.next(), self.next()]
    }
}
