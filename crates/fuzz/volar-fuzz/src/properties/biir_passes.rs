//! Property A — Boolar movfuscation preserves semantics.
//!
//! Circuit production deliberately is not tested from Boolar IR: the typed
//! Volar step path is the sole circuit-lowering route. See Property P for
//! the Volar-step/Boolar-step adapter equivalence check.

use proptest::prelude::*;
use volar_ir_passes::movfuscate_biir_with_control_provenance;

use crate::generators::biir::gen_biir_and_inputs;
use crate::interpreter::biir::eval_biir;

proptest! {
    #[test]
    fn prop_a_movfuscate_preserves_semantics(
        (cfg, inputs) in gen_biir_and_inputs()
    ) {
        let expected = match eval_biir(&cfg, &inputs) {
            Some(v) => v,
            None => return Ok(()),
        };
        let movfuscated = movfuscate_biir_with_control_provenance(&cfg, &());
        let pc_width = volar_ir_passes::pc_bits_needed(cfg.blocks.len());
        let state_width = cfg
            .blocks
            .iter()
            .map(|block| block.params as usize)
            .max()
            .unwrap_or(0);
        let mut state = vec![false; pc_width];
        state.extend_from_slice(&inputs);
        state.resize(pc_width + state_width, false);
        let actual = match eval_biir(&movfuscated, &state) {
            Some(v) => v,
            None => return Ok(()),
        };
        prop_assert_eq!(actual, expected, "movfuscate_biir changed the output");
    }
}
