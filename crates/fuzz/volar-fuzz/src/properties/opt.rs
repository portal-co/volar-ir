//! Properties E, F, G, H, I — optimization passes preserve semantics.
//!
//! | Property | Pass                          | IR layer   |
//! |----------|-------------------------------|------------|
//! | E        | `fold_ir_blocks`              | Volar IR   |
//! | F        | `fold_biir_blocks`            | Boolar IR  |
//! | G        | `fold_vaffle_module`          | VAFFLE     |
//! | H        | `store_forward_*`             | all layers |
//! | I        | `inline_vaffle_module`        | VAFFLE     |

use proptest::prelude::*;
use volar_ir_opt::biir::fold_biir_blocks;
use volar_ir_opt::ir::{cse_ir_blocks, dce_ir_blocks, fold_ir_blocks};
use volar_ir_opt::inline_vaffle::{inline_vaffle_module, InlineBudget};
use volar_ir_opt::vaffle::fold_vaffle_module;
use volar_ir_opt::store_forward::{
    store_forward_biir_blocks,
    store_forward_ir_blocks,
    store_forward_vaffle_module,
};

use crate::generators::biir::{gen_biir_and_inputs, gen_biir_extended_and_inputs, gen_biir_multiblock_and_inputs, gen_biir_diamond_and_inputs};
use crate::generators::ir::{gen_ir_and_inputs, gen_ir_extended_and_inputs, gen_ir_multiblock_and_inputs, gen_ir_diamond_and_inputs};
use crate::generators::vaffle::{gen_vaffle_and_inputs, gen_vaffle_extended_and_inputs, gen_vaffle_multiblock_and_inputs, gen_vaffle_diamond_and_inputs, gen_vaffle_two_func_and_inputs};
use crate::interpreter::biir::eval_biir;
use crate::interpreter::ir::eval_ir;
use crate::interpreter::vaffle::eval_vaffle;

#[test]
fn named_corpus_cases_survive_generic_optimization_passes() {
    use volar_lir_test_corpus::{
        ALL_CASES, build_case, make_biir_and, make_biir_half_adder, make_biir_not,
        make_ir_and, make_ir_not, make_ir_xor,
    };
    use volar_vaffle_target::VaffleTarget;

    fn case_inputs(case: &volar_lir_test_corpus::CorpusCase, raw: &[u64]) -> Vec<Vec<bool>> {
        case.lir_param_types.iter().zip(raw).flat_map(|(ty, value)| {
            (0..ty.bit_width()).map(move |bit| vec![value & (1u64 << bit) != 0])
        }).collect()
    }

    fn eval_case(case: &volar_lir_test_corpus::CorpusCase, raw: &[u64], optimize: fn(&mut vaffle::Module) -> bool) -> Vec<Vec<bool>> {
        let mut target = VaffleTarget::new();
        assert!(build_case(case.name, &mut target));
        optimize(&mut target.module);
        crate::interpreter::vaffle::eval_vaffle(&target.module, vaffle::FuncId(0), &case_inputs(case, raw))
            .expect("corpus program should halt")
    }

    for case in ALL_CASES {
        for io in case.ios {
            let baseline = eval_case(case, io.inputs, |_| false);
            let expected: Vec<Vec<bool>> = (0..case.lir_return_type.as_ref().expect("corpus return type").bit_width())
                .map(|bit| vec![io.expected & (1u64 << bit) != 0]).collect();
            assert_eq!(baseline, expected, "{} baseline", case.name);
            assert_eq!(eval_case(case, io.inputs, fold_vaffle_module), baseline, "{} fold", case.name);
            assert_eq!(eval_case(case, io.inputs, store_forward_vaffle_module), baseline, "{} store-forward", case.name);
        }
    }

    for (blocks, inputs) in [
        (make_biir_not(), vec![true]),
        (make_biir_and(), vec![true, false]),
        (make_biir_half_adder(), vec![true, true]),
    ] {
        let before = eval_biir(&blocks, &inputs).expect("fixture should halt");
        let mut optimized = blocks;
        fold_biir_blocks(&mut optimized);
        store_forward_biir_blocks(&mut optimized);
        assert_eq!(eval_biir(&optimized, &inputs), Some(before));
    }

    for (blocks, types, inputs) in [
        { let (b, t) = make_ir_xor(); (b, t, vec![vec![true], vec![false]]) },
        { let (b, t) = make_ir_and(); (b, t, vec![vec![true], vec![true]]) },
        { let (b, t) = make_ir_not(); (b, t, vec![vec![false]]) },
    ] {
        let before = eval_ir(&blocks, &types, &inputs).expect("fixture should halt");
        let mut optimized = blocks;
        fold_ir_blocks(&mut optimized, &types);
        cse_ir_blocks(&mut optimized, &types);
        dce_ir_blocks(&mut optimized, &types);
        assert_eq!(eval_ir(&optimized, &types, &inputs), Some(before));
    }
}

// ============================================================================
// Property E — fold_ir_blocks preserves Volar IR semantics
// ============================================================================

proptest! {
    #[test]
    fn prop_e_fold_ir_blocks_preserves_semantics(
        (ir, types, inputs) in gen_ir_and_inputs()
    ) {
        let before = match eval_ir(&ir, &types, &inputs) {
            Some(v) => v,
            None => return Ok(()), // single-block shouldn't loop — skip defensively
        };

        let mut folded = ir.clone();
        fold_ir_blocks(&mut folded, &types);

        let after = match eval_ir(&folded, &types, &inputs) {
            Some(v) => v,
            None => {
                prop_assert!(false, "eval_ir on folded IR did not terminate");
                return Ok(());
            }
        };

        prop_assert_eq!(before, after, "fold_ir_blocks changed the semantics");
    }

    #[test]
    fn prop_e_fold_ir_blocks_does_not_panic(
        (ir, types, _inputs) in gen_ir_and_inputs()
    ) {
        let mut folded = ir.clone();
        let _ = fold_ir_blocks(&mut folded, &types);
    }
}

// ============================================================================
// Property F — fold_biir_blocks preserves Boolar IR semantics
// ============================================================================

proptest! {
    #[test]
    fn prop_f_fold_biir_blocks_preserves_semantics(
        (blocks, inputs) in gen_biir_and_inputs()
    ) {
        let before = match eval_biir(&blocks, &inputs) {
            Some(v) => v,
            None => return Ok(()),
        };

        let mut folded = blocks.clone();
        fold_biir_blocks(&mut folded);

        let after = match eval_biir(&folded, &inputs) {
            Some(v) => v,
            None => {
                prop_assert!(false, "eval_biir on folded BIrBlocks did not terminate");
                return Ok(());
            }
        };

        prop_assert_eq!(before, after, "fold_biir_blocks changed the semantics");
    }

    #[test]
    fn prop_f_fold_biir_blocks_does_not_panic(
        (blocks, _inputs) in gen_biir_and_inputs()
    ) {
        let mut folded = blocks.clone();
        let _ = fold_biir_blocks(&mut folded);
    }
}

// ============================================================================
// Property G — fold_vaffle_module preserves VAFFLE semantics
// ============================================================================

proptest! {
    #[test]
    fn prop_g_fold_vaffle_module_preserves_semantics(
        (module, func_id, inputs) in gen_vaffle_and_inputs()
    ) {
        // Evaluate BEFORE folding.
        let before = match eval_vaffle(&module, func_id, &inputs) {
            Some(v) => v,
            None => return Ok(()),
        };

        // Fold in place (Module doesn't implement Clone, so we evaluate
        // before and after using the same owned value).
        let mut module = module;
        fold_vaffle_module(&mut module);

        let after = match eval_vaffle(&module, func_id, &inputs) {
            Some(v) => v,
            None => {
                prop_assert!(false, "eval_vaffle on folded Module did not terminate");
                return Ok(());
            }
        };

        prop_assert_eq!(before, after, "fold_vaffle_module changed the semantics");
    }

    #[test]
    fn prop_g_fold_vaffle_module_does_not_panic(
        (module, _func_id, _inputs) in gen_vaffle_and_inputs()
    ) {
        let mut module = module;
        let _ = fold_vaffle_module(&mut module);
    }
}

// ============================================================================
// Property H — store_forward passes preserve semantics
// ============================================================================

proptest! {
    #[test]
    fn prop_h_store_forward_ir_preserves_semantics(
        (ir, types, inputs) in gen_ir_extended_and_inputs()
    ) {
        let before = match eval_ir(&ir, &types, &inputs) {
            Some(v) => v,
            None => return Ok(()),
        };

        let mut forwarded = ir.clone();
        store_forward_ir_blocks(&mut forwarded, &types);

        let after = match eval_ir(&forwarded, &types, &inputs) {
            Some(v) => v,
            None => {
                prop_assert!(false, "eval_ir on store-forwarded IR did not terminate");
                return Ok(());
            }
        };

        prop_assert_eq!(before, after, "store_forward_ir_blocks changed the semantics");
    }

    #[test]
    fn prop_h_store_forward_ir_does_not_panic(
        (ir, types, _inputs) in gen_ir_extended_and_inputs()
    ) {
        let mut forwarded = ir.clone();
        let _ = store_forward_ir_blocks(&mut forwarded, &types);
    }

    #[test]
    fn prop_h_store_forward_vaffle_preserves_semantics(
        (module, func_id, inputs) in gen_vaffle_extended_and_inputs()
    ) {
        let before = match eval_vaffle(&module, func_id, &inputs) {
            Some(v) => v,
            None => return Ok(()),
        };

        let mut module = module;
        store_forward_vaffle_module(&mut module);

        let after = match eval_vaffle(&module, func_id, &inputs) {
            Some(v) => v,
            None => {
                prop_assert!(false, "eval_vaffle on store-forwarded module did not terminate");
                return Ok(());
            }
        };

        prop_assert_eq!(before, after, "store_forward_vaffle_module changed the semantics");
    }

    #[test]
    fn prop_h_store_forward_vaffle_does_not_panic(
        (module, _func_id, _inputs) in gen_vaffle_extended_and_inputs()
    ) {
        let mut module = module;
        let _ = store_forward_vaffle_module(&mut module);
    }
}

// ============================================================================
// Property H (multi-block) — cross-block store forwarding preserves semantics
// ============================================================================

proptest! {
    #[test]
    fn prop_h_store_forward_vaffle_multiblock_preserves_semantics(
        (module, func_id, inputs) in gen_vaffle_multiblock_and_inputs()
    ) {
        let before = match eval_vaffle(&module, func_id, &inputs) {
            Some(v) => v,
            None => return Ok(()),
        };

        let mut module = module;
        store_forward_vaffle_module(&mut module);

        let after = match eval_vaffle(&module, func_id, &inputs) {
            Some(v) => v,
            None => {
                prop_assert!(false, "eval_vaffle on multi-block store-forwarded module did not terminate");
                return Ok(());
            }
        };

        prop_assert_eq!(before, after, "store_forward_vaffle_module changed multi-block semantics");
    }

    #[test]
    fn prop_h_store_forward_vaffle_multiblock_does_not_panic(
        (module, _func_id, _inputs) in gen_vaffle_multiblock_and_inputs()
    ) {
        let mut module = module;
        let _ = store_forward_vaffle_module(&mut module);
    }
}

// ============================================================================
// Property H (multi-block IR) — cross-block store forwarding preserves semantics
// ============================================================================

proptest! {
    #[test]
    fn prop_h_store_forward_ir_multiblock_preserves_semantics(
        (ir, types, inputs) in gen_ir_multiblock_and_inputs()
    ) {
        let before = match eval_ir(&ir, &types, &inputs) {
            Some(v) => v,
            None => return Ok(()),
        };

        let mut forwarded = ir.clone();
        store_forward_ir_blocks(&mut forwarded, &types);

        let after = match eval_ir(&forwarded, &types, &inputs) {
            Some(v) => v,
            None => {
                prop_assert!(false, "eval_ir on multi-block store-forwarded IR did not terminate");
                return Ok(());
            }
        };

        prop_assert_eq!(before, after, "store_forward_ir_blocks changed multi-block semantics");
    }

    #[test]
    fn prop_h_store_forward_ir_multiblock_does_not_panic(
        (ir, types, _inputs) in gen_ir_multiblock_and_inputs()
    ) {
        let mut forwarded = ir.clone();
        let _ = store_forward_ir_blocks(&mut forwarded, &types);
    }
}

// ============================================================================
// Property H (BIR) — store forwarding preserves BIR semantics
// ============================================================================

proptest! {
    #[test]
    fn prop_h_store_forward_biir_preserves_semantics(
        (blocks, inputs) in gen_biir_extended_and_inputs()
    ) {
        let before = match eval_biir(&blocks, &inputs) {
            Some(v) => v,
            None => return Ok(()),
        };

        let mut forwarded = blocks.clone();
        store_forward_biir_blocks(&mut forwarded);

        let after = match eval_biir(&forwarded, &inputs) {
            Some(v) => v,
            None => {
                prop_assert!(false, "eval_biir on store-forwarded BIR did not terminate");
                return Ok(());
            }
        };

        prop_assert_eq!(before, after, "store_forward_biir_blocks changed the semantics");
    }

    #[test]
    fn prop_h_store_forward_biir_does_not_panic(
        (blocks, _inputs) in gen_biir_extended_and_inputs()
    ) {
        let mut forwarded = blocks.clone();
        let _ = store_forward_biir_blocks(&mut forwarded);
    }

    #[test]
    fn prop_h_store_forward_biir_multiblock_preserves_semantics(
        (blocks, inputs) in gen_biir_multiblock_and_inputs()
    ) {
        let before = match eval_biir(&blocks, &inputs) {
            Some(v) => v,
            None => return Ok(()),
        };

        let mut forwarded = blocks.clone();
        store_forward_biir_blocks(&mut forwarded);

        let after = match eval_biir(&forwarded, &inputs) {
            Some(v) => v,
            None => {
                prop_assert!(false, "eval_biir on multi-block store-forwarded BIR did not terminate");
                return Ok(());
            }
        };

        prop_assert_eq!(before, after, "store_forward_biir_blocks changed multi-block semantics");
    }

    #[test]
    fn prop_h_store_forward_biir_multiblock_does_not_panic(
        (blocks, _inputs) in gen_biir_multiblock_and_inputs()
    ) {
        let mut forwarded = blocks.clone();
        let _ = store_forward_biir_blocks(&mut forwarded);
    }
}

// ============================================================================
// Property H (diamond) — diamond-CFG store forwarding preserves semantics
// ============================================================================

proptest! {
    #[test]
    fn prop_h_store_forward_ir_diamond_preserves_semantics(
        (ir, types, inputs) in gen_ir_diamond_and_inputs()
    ) {
        let before = match eval_ir(&ir, &types, &inputs) {
            Some(v) => v,
            None => return Ok(()),
        };

        let mut forwarded = ir.clone();
        store_forward_ir_blocks(&mut forwarded, &types);

        let after = match eval_ir(&forwarded, &types, &inputs) {
            Some(v) => v,
            None => {
                prop_assert!(false, "eval_ir on diamond store-forwarded IR did not terminate");
                return Ok(());
            }
        };

        prop_assert_eq!(before, after, "store_forward_ir_blocks changed diamond semantics");
    }

    #[test]
    fn prop_h_store_forward_ir_diamond_does_not_panic(
        (ir, types, _inputs) in gen_ir_diamond_and_inputs()
    ) {
        let mut forwarded = ir.clone();
        let _ = store_forward_ir_blocks(&mut forwarded, &types);
    }

    #[test]
    fn prop_h_store_forward_biir_diamond_preserves_semantics(
        (blocks, inputs) in gen_biir_diamond_and_inputs()
    ) {
        let before = match eval_biir(&blocks, &inputs) {
            Some(v) => v,
            None => return Ok(()),
        };

        let mut forwarded = blocks.clone();
        store_forward_biir_blocks(&mut forwarded);

        let after = match eval_biir(&forwarded, &inputs) {
            Some(v) => v,
            None => {
                prop_assert!(false, "eval_biir on diamond store-forwarded BIR did not terminate");
                return Ok(());
            }
        };

        prop_assert_eq!(before, after, "store_forward_biir_blocks changed diamond semantics");
    }

    #[test]
    fn prop_h_store_forward_biir_diamond_does_not_panic(
        (blocks, _inputs) in gen_biir_diamond_and_inputs()
    ) {
        let mut forwarded = blocks.clone();
        let _ = store_forward_biir_blocks(&mut forwarded);
    }

    #[test]
    fn prop_h_store_forward_vaffle_diamond_preserves_semantics(
        (module, func_id, inputs) in gen_vaffle_diamond_and_inputs()
    ) {
        let before = match eval_vaffle(&module, func_id, &inputs) {
            Some(v) => v,
            None => return Ok(()),
        };

        let mut module = module;
        store_forward_vaffle_module(&mut module);

        let after = match eval_vaffle(&module, func_id, &inputs) {
            Some(v) => v,
            None => {
                prop_assert!(false, "eval_vaffle on diamond store-forwarded module did not terminate");
                return Ok(());
            }
        };

        prop_assert_eq!(before, after, "store_forward_vaffle_module changed diamond semantics");
    }

    #[test]
    fn prop_h_store_forward_vaffle_diamond_does_not_panic(
        (module, _func_id, _inputs) in gen_vaffle_diamond_and_inputs()
    ) {
        let mut module = module;
        let _ = store_forward_vaffle_module(&mut module);
    }
}

// ============================================================================
// Property I — inline_vaffle_module preserves VAFFLE semantics
// ============================================================================

/// Generous enough to inline the (always non-recursive, at most a handful of
/// `Value`s) callee `gen_vaffle_two_func_and_inputs` generates on every run,
/// so this property actually exercises splicing rather than being a no-op.
fn generous_inline_budget() -> InlineBudget {
    InlineBudget { max_callee_values: 1000, total_budget: 10_000 }
}

proptest! {
    #[test]
    fn prop_i_inline_vaffle_module_preserves_semantics(
        (module, func_id, inputs) in gen_vaffle_two_func_and_inputs()
    ) {
        let before = match eval_vaffle(&module, func_id, &inputs) {
            Some(v) => v,
            None => return Ok(()),
        };

        let mut module = module;
        inline_vaffle_module(&mut module, generous_inline_budget());

        let after = match eval_vaffle(&module, func_id, &inputs) {
            Some(v) => v,
            None => {
                prop_assert!(false, "eval_vaffle on inlined Module did not terminate");
                return Ok(());
            }
        };

        prop_assert_eq!(before, after, "inline_vaffle_module changed the semantics");
    }

    #[test]
    fn prop_i_inline_vaffle_module_does_not_panic(
        (module, _func_id, _inputs) in gen_vaffle_two_func_and_inputs()
    ) {
        let mut module = module;
        let _ = inline_vaffle_module(&mut module, generous_inline_budget());
    }
}
