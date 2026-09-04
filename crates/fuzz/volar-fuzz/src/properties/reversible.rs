//! Property E — `to_reversible` implements the Bennett-style embedding.
//!
//! ## Property E — reversible embedding preserves semantics
//! For a circuit-fused Boolar program computing `f(x)`, the reversible circuit
//! produced by `to_reversible` maps `(x, y, 0…0) ↦ (x, y ⊕ f(x), …)`:
//! - the `x` register is bit-for-bit untouched,
//! - the `y` register becomes `y ⊕ f(x)`,
//! - for every initial `y ∈ {0,1}^n_out`.
//!
//! Inputs that hit unsupported features (external primitives, storage traffic)
//! are skipped — those have no naive reversible lowering by design.

use proptest::prelude::*;
use volar_ir::boolar::{BIrStmt, LaneId};
use volar_ir::circuit::BCircuit;
use volar_ir::ir::IRVarId;
use volar_ir::rcircuit::StorageState;
use volar_ir_common::StorageId;
use volar_ir_passes::to_reversible::{ValueWatchlist, to_reversible, translate_watchlist};
use volar_ir_passes::{
    LoweringMode, lower_to_circuit_fused, movfuscate_biir_with_control_provenance,
    to_boolar_circuit,
};

use crate::generators::biir::gen_biir_and_inputs;
use crate::interpreter::biir::eval_biir_with_limit;

use super::biir_passes::{LOWER_LIMIT, movfuscated_inputs};

fn u64_address(value: u64) -> Vec<bool> {
    (0..u64::BITS as usize).map(|bit| (value >> bit) & 1 != 0).collect()
}

/// Evaluate a pure-gate fused circuit (Zero/One/And/Or/Xor/Not only) on
/// parameter bits; returns `None` if it contains any other statement.
fn eval_fused_pure(circ: &BCircuit, params: &[bool]) -> Option<Vec<bool>> {
    let mut vals = vec![false; circ.var_space() as usize];
    vals[..params.len()].copy_from_slice(params);
    for (i, node) in circ.stmts.iter().enumerate() {
        let v = params.len() + i;
        vals[v] = match &node.kind {
            BIrStmt::Zero => false,
            BIrStmt::One => true,
            BIrStmt::And(a, b) => vals[a.0 as usize] & vals[b.0 as usize],
            BIrStmt::Or(a, b) => vals[a.0 as usize] | vals[b.0 as usize],
            BIrStmt::Xor(a, b) => vals[a.0 as usize] ^ vals[b.0 as usize],
            BIrStmt::Not(a) => !vals[a.0 as usize],
            _ => return None,
        };
    }
    Some(circ.outputs.iter().map(|o| vals[o.0 as usize]).collect())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    #[test]
    fn prop_e_to_reversible_implements_xor_embedding(
        (cfg, inputs) in gen_biir_and_inputs(),
        ymask in proptest::num::u32::ANY,
    ) {
        let movfuscated = movfuscate_biir_with_control_provenance(&cfg, &());
        let m_inputs = movfuscated_inputs(&cfg, &inputs);
        let expected =
            match eval_biir_with_limit(&movfuscated, &m_inputs, LOWER_LIMIT as usize) {
                Some(v) => v,
                None => return Ok(()),
            };

        // Fuse the lowered circuit; skip shapes fusion rejects.
        let circuit =
            lower_to_circuit_fused(&movfuscated, LOWER_LIMIT, LoweringMode::Unconditional);
        let circ = match circuit {
            Ok(c) => c,
            Err(_) => return Ok(()),
        };
        let f_out = match eval_fused_pure(&circ, &m_inputs[..circ.params as usize]) {
            Some(v) => v,
            None => return Ok(()),
        };
        prop_assert_eq!(&f_out, &expected, "fused circuit disagrees with eval");

        // Convert to reversible; skip unsupported external primitives.
        let (rc, map) = match to_reversible(&circ) {
            Ok(x) => x,
            Err(_) => return Ok(()),
        };

        // The reverse transform exposes every reversible wire as both a
        // Boolar input and output, so it must agree for arbitrary dirty
        // workspace/y assignments too, not only the initialized embedding
        // path checked below.
        let normal = to_boolar_circuit(&rc).expect("validated RCircuit lowers");
        prop_assert_eq!(normal.params as usize, rc.num_wires);
        let mut arbitrary_wires: Vec<bool> = (0..rc.num_wires)
            .map(|i| (ymask >> (i % u32::BITS as usize)) & 1 == 1)
            .collect();
        let normal_outputs = eval_fused_pure(&normal, &arbitrary_wires)
            .expect("pure RCircuit lowers to pure Boolar");
        rc.apply_pure(&mut arbitrary_wires)
            .expect("pure RCircuit applies without storage or externals");
        prop_assert_eq!(normal_outputs, arbitrary_wires,
            "reverse Boolar transform disagrees on complete wire state");

        // Watchlist translation must resolve a sample var fail-closed.
        if circ.var_space() > 0 {
            let wl = ValueWatchlist::from_vars([0u32]);
            let translated = translate_watchlist(&wl, &map).expect("var 0 always mapped");
            prop_assert_eq!(translated.entries[0].var, 0);
        }

        let n_params = circ.params as usize;
        let n_out = circ.outputs.len();
        // Locate the y register through the map (wire reuse may compact it).
        let y_base = map.y_base();
        let py: Vec<bool> = (0..n_out).map(|i| (ymask >> i) & 1 == 1).collect();

        let mut wires = vec![false; rc.num_wires];
        for (i, b) in m_inputs.iter().take(n_params).enumerate() {
            wires[map.wire(IRVarId(i as u32)).unwrap()] = *b;
        }
        for (k, b) in py.iter().enumerate() {
            wires[y_base + k] = *b;
        }

        // Deterministically seeded initial storage so read/write traffic runs
        // against non-trivial cell contents.
        let mut storage = StorageState::new();
        for cell in 0..16u64 {
            storage.insert(
                ((StorageId(0), LaneId(0)), u64_address(cell)),
                (ymask >> (cell % 31)) & 1 == 1,
            );
            storage.insert(
                ((StorageId(3), LaneId(0)), u64_address(cell)),
                (ymask >> (cell % 17)) & 1 == 1,
            );
        }
        let storage_before = storage.clone();

        rc.apply(&mut wires, &mut storage);

        // Reversibility over the joint (wires, storage) state: running the
        // inverse must restore exactly what we started with.
        let mut wires_after = wires.clone();
        let mut storage_after = storage.clone();
        rc.inverse().apply(&mut wires_after, &mut storage_after);
        prop_assert_eq!(wires_after.len(), rc.num_wires);
        // Recompute pristine inputs for comparison.
        let mut pristine = vec![false; rc.num_wires];
        for (i, b) in m_inputs.iter().take(n_params).enumerate() {
            pristine[map.wire(IRVarId(i as u32)).unwrap()] = *b;
        }
        for (k, b) in py.iter().enumerate() {
            pristine[y_base + k] = *b;
        }
        prop_assert_eq!(wires_after, pristine,
            "inverse did not restore wires");
        prop_assert_eq!(storage_after, storage_before,
            "inverse did not restore storage");

        // x register untouched:
        for (i, b) in m_inputs.iter().take(n_params).enumerate() {
            prop_assert_eq!(wires[map.wire(IRVarId(i as u32)).unwrap()], *b,
                "x wire {} disturbed", i);
        }
        // y ^= f(x):
        for (k, b) in f_out.iter().enumerate() {
            prop_assert_eq!(wires[y_base + k], py[k] ^ b,
                "y wire {} wrong under x={:?}", k, m_inputs);
        }
    }
}
