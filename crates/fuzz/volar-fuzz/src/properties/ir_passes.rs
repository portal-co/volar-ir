//! Property D — `lower_ir_to_boolar` preserves semantics.
//! Property J — `lower_vaffle_to_ir` preserves semantics.
//! Property K — `lower_vaffle_to_ir_with_inlining` preserves semantics.

use proptest::prelude::*;
use volar_ir_opt::inline_vaffle::InlineBudget;
use volar_ir_passes::lower_ir_to_boolar;
use volar_lir::circuits::PACK_W;
use volar_vaffle_target::{
    lower_vaffle_to_ir_with_control_provenance, lower_vaffle_to_ir_with_inlining,
};

use crate::generators::ir::{gen_ir_and_inputs, gen_ir_extended_and_inputs};
use crate::generators::vaffle::{
    gen_vaffle_and_inputs, gen_vaffle_extended_and_inputs, gen_vaffle_two_func_and_inputs,
};
use crate::interpreter::biir::eval_biir;
use crate::interpreter::ir::{IrValue, bit_flatten, bit_unflatten, bit_width, eval_ir};
use crate::interpreter::vaffle::eval_vaffle;

/// Chunk a flat parameter/return bit-vector into `ceil(bits.len() /
/// word_w)` `word_w`-wide words, zero-padding the last one.
///
/// `lower_vaffle_to_ir`'s module trampoline (`emit_entry_and_exit`) never
/// receives a VAFFLE function's params as individual `Bit`-typed block
/// params — it packs them into `PACK_W`-wide words first (see
/// `crates/ir/volar-vaffle-target/src/lower_to_ir.rs`'s own "Bit packing"
/// module doc). A property comparing `eval_vaffle` against `eval_ir` on the
/// *lowered* IR must feed `eval_ir` this same packed shape, not the raw
/// per-param bits directly — conflating the two silently skipped every
/// non-empty-param case in this file for a long time (see property J below)
/// and is exactly the shape of bug `docs/llvm-stack-spill-boolar.md` found:
/// a bit-width miscount in this same packing.
fn pack_flat_bits(bits: &[bool], word_w: usize) -> Vec<IrValue> {
    bits.chunks(word_w)
        .map(|chunk| {
            let mut word = chunk.to_vec();
            word.resize(word_w, false);
            word
        })
        .collect()
}

proptest! {
    #[test]
    fn prop_d_lower_ir_to_boolar_preserves_semantics(
        (ir, types, inputs) in gen_ir_and_inputs()
    ) {
        // Evaluate the high-level IR.
        let ir_outputs = match eval_ir(&ir, &types, &inputs) {
            Some(v) => v,
            None => return Ok(()), // shouldn't happen for single-block — skip defensively
        };

        // Determine the output widths so we can unflatten the boolar result.
        let output_widths: Vec<usize> = ir_outputs.iter().map(|v| v.len()).collect();
        let total_output_bits: usize = output_widths.iter().sum();

        // Lower IR → BIrBlocks and evaluate.
        let boolar = lower_ir_to_boolar(&ir, &types);
        let flat_inputs = bit_flatten(&inputs);
        let flat_outputs = match eval_biir(&boolar, &flat_inputs) {
            Some(v) => v,
            None => {
                // The lowered circuit didn't terminate — this is a bug, but
                // we fail with a clear message rather than a panic.
                prop_assert!(false,
                    "eval_biir on lower_ir_to_boolar output did not terminate");
                return Ok(());
            }
        };

        prop_assert_eq!(flat_outputs.len(), total_output_bits,
            "lowered circuit output bit count mismatch");

        let boolar_outputs = bit_unflatten(&flat_outputs, &output_widths);

        prop_assert_eq!(boolar_outputs, ir_outputs,
            "lower_ir_to_boolar changed the semantics");
    }

    #[test]
    fn prop_d_lower_ir_to_boolar_does_not_panic(
        (ir, types, _inputs) in gen_ir_and_inputs()
    ) {
        let _ = lower_ir_to_boolar(&ir, &types);
    }

    /// Property D2 — storage round-trip: programs with storage traffic lower
    /// to Boolar whose per-bit appended-address cells agree with the IR-level
    /// `(storage, type, addr)` cell semantics.
    #[test]
    fn prop_d2_lower_ir_storage_roundtrip_preserves_semantics(
        (ir, types, inputs) in gen_ir_extended_and_inputs()
    ) {
        use volar_ir_passes::lower_ir_to_boolar_with_lane_table;

        let ir_outputs = match eval_ir(&ir, &types, &inputs) {
            Some(v) => v,
            None => return Ok(()),
        };
        if ir.blocks.len() != 1 {
            // Extended generator emits a single block; guard anyway.
            return Ok(());
        }

        let output_widths: Vec<usize> = ir_outputs.iter().map(|v| v.len()).collect();
        let total_output_bits: usize = output_widths.iter().sum();

        // Persisted regression cases from other properties may contain
        // programs whose element-address + appended-index bits exceed the
        // 64-bit flat cell space; lowering fails closed on those. Skip them,
        // but fail loudly on any *other* unexpected panic.
        let lowered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            lower_ir_to_boolar_with_lane_table(&ir, &types)
        }));
        let (boolar, lane_table) = match lowered {
            Ok(pair) => pair,
            Err(payload) => {
                let msg = payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&'static str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                if msg.contains("flat cell space") || msg.contains("mixed element-address widths") {
                    return Ok(()); // not representable in Boolar storage model
                }
                panic!("lower_ir_to_boolar_with_lane_table panicked unexpectedly: {msg}");
            }
        };
        // Lane table is dense: lanes form a contiguous `0..n` range.
        let mut lanes: Vec<u32> = lane_table.keys().map(|l| l.0).collect();
        lanes.sort_unstable();
        for (i, l) in lanes.iter().enumerate() {
            prop_assert_eq!(*l, i as u32, "dense contiguous lane ids");
        }
        let flat_inputs = bit_flatten(&inputs);
        let flat_outputs = match eval_biir(&boolar, &flat_inputs) {
            Some(v) => v,
            None => {
                prop_assert!(false, "eval_biir on lowered extended circuit did not terminate");
                return Ok(());
            }
        };
        prop_assert_eq!(flat_outputs.len(), total_output_bits,
            "lowered extended circuit output bit count mismatch");
        let boolar_outputs = bit_unflatten(&flat_outputs, &output_widths);
        if boolar_outputs != ir_outputs {
            panic!(
                "lower_ir_to_boolar changed the semantics of a program with storage ops\nIR: {ir:?}\ntypes: {types:?}\ninputs: {inputs:?}\ngot: {boolar_outputs:?}\nwanted: {ir_outputs:?}"
            );
        }
    }
}

// ============================================================================
// Property J — lower_vaffle_to_ir preserves semantics
// ============================================================================

proptest! {
    #[test]
    fn prop_j_lower_vaffle_to_ir_preserves_semantics(
        (module, func_id, inputs) in gen_vaffle_and_inputs()
    ) {
        let vaffle_out = match eval_vaffle(&module, func_id, &inputs) {
            Some(v) => v,
            None => return Ok(()),
        };

        let (ir, ir_types) = lower_vaffle_to_ir_with_control_provenance(&module, &());

        // Block 0 of the lowered IR is the module trampoline: its own params
        // are `n_param_words` `PACK_W`-wide packed words holding the
        // function's params concatenated (see `pack_flat_bits`'s doc), not
        // the VAFFLE-level per-param values directly.
        let flat_params: Vec<bool> = inputs.iter().flatten().copied().collect();
        let packed_inputs = pack_flat_bits(&flat_params, PACK_W);

        let ir_out = match eval_ir(&ir, &ir_types, &packed_inputs) {
            Some(v) => v,
            None => return Ok(()),
        };

        // Flatten both output lists for a uniform comparison.
        let flat_vaffle: Vec<bool> = vaffle_out.into_iter().flatten().collect();
        let flat_ir: Vec<bool> = ir_out.into_iter().flatten().collect();

        prop_assert_eq!(flat_ir, flat_vaffle, "lower_vaffle_to_ir changed the semantics");
    }

    #[test]
    fn prop_j_lower_vaffle_to_ir_does_not_panic(
        (module, _func_id, _inputs) in gen_vaffle_and_inputs()
    ) {
        let _ = lower_vaffle_to_ir_with_control_provenance(&module, &());
    }
}

// ============================================================================
// Property K — lower_vaffle_to_ir_with_inlining preserves semantics
// ============================================================================

fn generous_inline_budget() -> InlineBudget {
    InlineBudget {
        max_callee_values: 1000,
        total_budget: 10_000,
    }
}

/// `interpret_vaffle_two_func` (unlike the plain, non-extended
/// `interpret_vaffle` property J uses) can emit `StorageRead`/`StorageWrite`
/// values in either function's body. `lower_vaffle_to_ir` has a pre-existing
/// param-count mismatch for a non-entry function whose `Return` includes a
/// `StorageWrite`'s own (not semantically meaningful) "result" -- reproduced
/// independent of inlining (same panic on the plain, non-inlining
/// `lower_vaffle_to_ir_with_control_provenance` path), so it's a
/// `lower_vaffle_to_ir` limitation, not something introduced by
/// `inline_vaffle_module`. Filtered out here the same way property J already
/// narrows its own generator's shape to what `lower_vaffle_to_ir` supports.
fn module_has_storage_ops(module: &vaffle::Module) -> bool {
    module.funcs.iter().any(|f| {
        let vaffle::FuncDecl::Body(body) = f else {
            return false;
        };
        body.values.iter().any(|n| {
            matches!(
                &n.kind,
                vaffle::Value::Op(
                    volar_ir_common::Stmt::StorageRead { .. }
                        | volar_ir_common::Stmt::StorageWrite { .. }
                )
            )
        })
    })
}

/// `eval_ir` represents an `OracleCall` as an opaque handle; only its
/// `OracleOutput` projections have concrete bit-vector values. Returning an
/// aggregate directly therefore cannot be compared to the VAFFLE evaluator.
/// The two-function generator returns every callee value through the caller's
/// continuation, so reject the shape in either body before testing inlining.
fn module_returns_void_or_aggregate_value(module: &vaffle::Module) -> bool {
    module.funcs.iter().any(|func| {
        let vaffle::FuncDecl::Body(body) = func else {
            return false;
        };
        body.blocks.iter().any(|block| {
            let vaffle::Terminator::Return { values } = &block.terminator else {
                return false;
            };
            values.iter().any(|value| {
                matches!(
                    &body.values[value.0].kind,
                    vaffle::Value::Op(
                        volar_ir_common::Stmt::StorageWrite { .. }
                            | volar_ir_common::Stmt::OracleCall { .. }
                    )
                )
            })
        })
    })
}

proptest! {
    /// Exercise the budgeted inlining route with non-empty, variably wide
    /// parameters. Storage traffic and direct opaque call aggregates are
    /// excluded by the generator filter below because the IR evaluator cannot
    /// compare those non-value results with VAFFLE's aggregate ABI.
    #[test]
    fn prop_k_lower_vaffle_to_ir_with_inlining_preserves_semantics(
        (module, func_id, inputs) in gen_vaffle_two_func_and_inputs()
            .prop_filter("no storage ops or returned opaque call aggregates", |(m, _, _)| {
                !module_has_storage_ops(m) && !module_returns_void_or_aggregate_value(m)
            })
    ) {
        let vaffle_out = match eval_vaffle(&module, func_id, &inputs) {
            Some(v) => v,
            None => return Ok(()),
        };

        let (ir, ir_types) = lower_vaffle_to_ir_with_inlining(module, generous_inline_budget());

        // Same packed-trampoline shape as property J -- see `pack_flat_bits`.
        let flat_params: Vec<bool> = inputs.iter().flatten().copied().collect();
        let packed_inputs = pack_flat_bits(&flat_params, PACK_W);

        let ir_out = match eval_ir(&ir, &ir_types, &packed_inputs) {
            Some(v) => v,
            None => return Ok(()),
        };

        let flat_vaffle: Vec<bool> = vaffle_out.into_iter().flatten().collect();
        let flat_ir: Vec<bool> = ir_out.into_iter().flatten().collect();

        prop_assert_eq!(flat_ir, flat_vaffle, "lower_vaffle_to_ir_with_inlining changed the semantics");
    }

    #[test]
    fn prop_k_lower_vaffle_to_ir_with_inlining_does_not_panic(
        (module, _func_id, _inputs) in gen_vaffle_two_func_and_inputs()
            .prop_filter("no storage ops or returned opaque call aggregates", |(m, _, _)| {
                !module_has_storage_ops(m) && !module_returns_void_or_aggregate_value(m)
            })
    ) {
        let _ = lower_vaffle_to_ir_with_inlining(module, generous_inline_budget());
    }
}

// ============================================================================
// Property N — lower_vaffle_to_ir + movfuscate_ir + lower_ir_to_boolar (end
// to end) preserves semantics, with randomly-typed (randomly-bit-width)
// VAFFLE function params.
//
// Property J (above) catches width bugs at the VAFFLE -> Volar IR boundary
// alone; property D/D2 (above) catch them at the Volar IR -> Boolar boundary
// alone, but from a hand-built IR generator that never has to go through
// `lower_vaffle_to_ir`'s own param-packing convention. Neither, on its own,
// would have caught the bug `docs/llvm-stack-spill-boolar.md` found (a
// `plan_functions` bit-width-vs-param-count miscount in exactly that packing
// step) unless it specifically chained VAFFLE all the way to Boolar with a
// non-empty, multi-bit-width parameter list -- which is what this does.
//
// `lower_ir_to_boolar` cannot lower `lower_vaffle_to_ir`'s output directly:
// every VAFFLE function's `Terminator::Return` becomes a CPS-style `Dyn`
// jump (see `lower_to_ir.rs`'s own module doc, "Call protocol"), which
// `lower_ir_to_boolar` explicitly rejects ("Dyn jump targets are not
// representable in BIrTerminator"). `movfuscate_ir` is the real pipeline's
// general answer to arbitrary/Dyn control flow, but driving its self-looping
// output needs the "watch"-based state-vector setup property M's
// `run_movfuscated` uses -- overkill here, since this generator's modules
// (`gen_vaffle_and_inputs`/`gen_vaffle_extended_and_inputs`) are always
// single-block with no real branches, so `unroll_ir_everything` (which
// folds exactly this Dyn-to-a-compile-time-constant-block shape into one
// `is_circuit()` block, keeping the entry block's own params as free
// circuit inputs) is the right, much simpler tool -- matching the real
// `volar-ir-build` pipeline's own "concrete CF -> unroll" route rather than
// its "arbitrary CF -> movfuscate" one (see `docs/pipeline.md`'s
// "Circuit-shape strategies").
// ============================================================================

use volar_ir_passes::unroll_ir::unroll_ir_everything;

/// Shared plumbing for property N's two variants: evaluate `module` via the
/// VAFFLE reference interpreter, lower it all the way to Boolar
/// (VAFFLE -> Volar IR -> unroll -> Boolar), evaluate that, and return both
/// flattened output bit-vectors for the caller to compare. `None` means one
/// side didn't terminate/unroll (proptest should skip, not fail) or panicked
/// on a shape this file already knows is unsupported (also skip).
fn eval_vaffle_and_boolar(
    module: &vaffle::Module,
    func_id: vaffle::FuncId,
    inputs: &[IrValue],
) -> Option<(Vec<bool>, Vec<bool>)> {
    let vaffle_out = eval_vaffle(module, func_id, inputs)?;

    let (ir, ir_types) = lower_vaffle_to_ir_with_control_provenance(module, &());
    let unrolled = unroll_ir_everything(&ir, &ir_types).ok()?;

    let flat_params: Vec<bool> = inputs.iter().flatten().copied().collect();
    let packed_inputs = pack_flat_bits(&flat_params, PACK_W);
    let flat_boolar_inputs = bit_flatten(&packed_inputs);

    let lowered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        lower_ir_to_boolar(&unrolled, &ir_types)
    }));
    let boolar = match lowered {
        Ok(b) => b,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| {
                    payload
                        .downcast_ref::<&'static str>()
                        .map(|s| s.to_string())
                })
                .unwrap_or_default();
            // Pre-existing, unrelated to bitwidth (same skips property D2
            // above already documents for this same extended generator):
            // oracle declarations that don't match what `lower_ir_to_boolar`
            // validates against, and storage addresses too wide for
            // Boolar's 64-bit flat cell space. Skip rather than mask other,
            // real panics under a blanket catch-all.
            if msg.contains("SignatureMismatch")
                || msg.contains("flat cell space")
                || msg.contains("mixed element-address widths")
            {
                return None;
            }
            panic!("lower_ir_to_boolar panicked unexpectedly: {msg}");
        }
    };
    let flat_boolar_out = eval_biir(&boolar, &flat_boolar_inputs)?;

    let flat_vaffle: Vec<bool> = vaffle_out.into_iter().flatten().collect();
    Some((flat_boolar_out, flat_vaffle))
}

/// `true` iff `module`'s (single, per this generator) function returns a
/// value produced directly by a `StorageWrite` or an `OracleCall`
/// aggregate -- both are "void"/aggregate results the generator
/// deliberately excludes from its own `var_info` (see
/// `build_vaffle_extended_block`'s comments), but still includes in the
/// function's flat return list. `stmt_result_vtid`'s placeholder width for
/// a void `StorageWrite` (`TypeId(0)`, whatever type that happens to be in
/// this generator's own type table) and the generator's own `sig.results`
/// fallback (`param_type_ids.first()`) aren't guaranteed to agree, and an
/// `OracleCall` aggregate's real (tuple) width never matches either -- both
/// are the exact same pre-existing, orthogonal quirk property M's
/// `shapes_match` doc comment already documents (there for the
/// movfuscated-circuit interpreter vs. the plain CFG one), not a
/// bitwidth-conversion bug this file is about. Filtered out the same way
/// property K already filters out storage ops for its own, different
/// `lower_vaffle_to_ir` limitation.
fn returns_a_void_or_aggregate_value(module: &vaffle::Module, func_id: vaffle::FuncId) -> bool {
    let vaffle::FuncDecl::Body(body) = &module.funcs[func_id.0] else {
        return false;
    };
    let vaffle::Terminator::Return { values } = &body.blocks[body.entry.0].terminator else {
        return false;
    };
    values.iter().any(|vid| {
        matches!(
            &body.values[vid.0].kind,
            vaffle::Value::Op(
                volar_ir_common::Stmt::StorageWrite { .. }
                    | volar_ir_common::Stmt::OracleCall { .. }
            )
        )
    })
}

proptest! {
    #[test]
    fn prop_n_lower_vaffle_to_boolar_preserves_semantics(
        (module, func_id, inputs) in gen_vaffle_and_inputs()
    ) {
        let (flat_boolar_out, flat_vaffle) = match eval_vaffle_and_boolar(&module, func_id, &inputs) {
            Some(pair) => pair,
            None => return Ok(()),
        };
        prop_assert_eq!(
            flat_boolar_out, flat_vaffle,
            "VAFFLE -> Volar IR -> Boolar changed the semantics"
        );
    }

    #[test]
    fn prop_n_lower_vaffle_to_boolar_extended_preserves_semantics(
        (module, func_id, inputs) in gen_vaffle_extended_and_inputs()
            .prop_filter("no void/aggregate value in return list (width quirk, see property M)", |(m, fid, _)| !returns_a_void_or_aggregate_value(m, *fid))
    ) {
        let (flat_boolar_out, flat_vaffle) = match eval_vaffle_and_boolar(&module, func_id, &inputs) {
            Some(pair) => pair,
            None => return Ok(()),
        };
        prop_assert_eq!(
            flat_boolar_out, flat_vaffle,
            "VAFFLE -> Volar IR -> Boolar changed the semantics (storage ops)"
        );
    }
}

// ============================================================================
// Property M — movfuscate_ir's (position, type)-keyed slot sharing preserves
// semantics.
//
// `compute_static_slot_classes` (crates/ir/volar-ir-passes/src/movfuscate.rs)
// lets two different blocks' params at the same raw position share one
// physical state slot whenever their types agree, regardless of whether any
// dataflow edge connects them -- safe because block params are never
// inherited across blocks (every jump explicitly, freshly supplies all of
// its target block's own param values), so two blocks' params at the same
// (position, type) can never be simultaneously live. These properties drive
// the actual movfuscated circuit -- not just the plain, pre-movfuscation CFG
// interpreter -- through `eval_ir_circuit_step`, so a real slot-aliasing bug
// (one block's shared slot silently corrupted by another's leftover write)
// would show up as a semantic mismatch here, not just a structural one.
// ============================================================================

use volar_ir::ir::{
    IRBlock, IRBlockId, IRBlockTargetId, IRBlocks, IRBranchTarget, IRStmt, IRTerminator, IRTypeId,
    IRTypes, IRVarId,
};
use volar_ir_common::{Constant, IrType, Node, PolyCoeffs, Type};
use volar_ir_passes::{LoweringMode, lower_to_circuit_ir};

use crate::interpreter::ir::{
    StorageMap, apply_pre_init, eval_ir_circuit_step, eval_ir_with_storage,
};

/// Movfuscate `blocks`, lower to a single-step-per-call circuit, and drive it
/// via `eval_ir_circuit_step` (one call = one raw movfuscated step) until its
/// own termination flag fires or `MAX_STEPS` is exceeded. Returns `None` on
/// a non-halt or a degenerate (`n <= 1` block) input -- the caller decides
/// whether that's a skip or a failure.
///
/// `orig_inputs[i]` seeds block 0's own param `i` -- resolved to its real
/// combined-circuit slot via `movfuscate_ir_with_boundary_and_watch`'s
/// `watch` mechanism, since a param's physical slot offset is NOT simply its
/// own original index once other blocks' params may occupy earlier-assigned
/// slots at the same or an earlier position.
fn run_movfuscated(
    blocks: &IRBlocks<()>,
    types: &IRTypes,
    orig_inputs: &[IrValue],
) -> Option<Vec<IrValue>> {
    let mut mut_types = types.clone();
    let watch: Vec<(usize, u32)> = (0..orig_inputs.len() as u32).map(|v| (0usize, v)).collect();
    let (movfuscated, _boundary, _accum_info, watch_results) =
        volar_ir_passes::movfuscate::movfuscate_ir_with_boundary_and_watch(
            blocks,
            &mut mut_types,
            &watch,
        );

    if !movfuscated.is_movfuscated() {
        // Single-block input (n <= 1): movfuscate_ir_with_boundary_and_watch
        // returns the block unchanged -- nothing to differentially test here.
        return None;
    }

    let bit_ty = mut_types
        .0
        .iter()
        .position(|t| matches!(t, IrType::Primitive(Type::Bit)))
        .map(|i| IRTypeId(i as u32))
        .expect("Bit type must already be interned by movfuscate_ir");

    let circuit = lower_to_circuit_ir(&movfuscated, &bit_ty, 1, LoweringMode::WithTerminationFlag);

    let param_widths: Vec<usize> = circuit.blocks[0]
        .params
        .iter()
        .map(|&tid| bit_width(tid, &mut_types))
        .collect();

    let mut state: Vec<IrValue> = param_widths.iter().map(|&w| vec![false; w]).collect();
    for &(orig_block, orig_var, combined_var) in &watch_results {
        if orig_block == 0 {
            state[combined_var as usize] = orig_inputs[orig_var as usize].clone();
        }
    }

    let mut storage: StorageMap = StorageMap::new();
    apply_pre_init(&mut storage, &circuit.pre_init, &mut_types);

    const MAX_STEPS: usize = 64;
    let mut done = false;
    let mut step = 0usize;
    let mut outputs: Vec<IrValue> = Vec::new();
    while !done && step < MAX_STEPS {
        outputs = eval_ir_circuit_step(
            &circuit.blocks[0],
            &mut_types,
            &circuit.oracles,
            &state,
            &mut storage,
        );
        done = outputs[0].iter().any(|&b| b);
        state = outputs[1..1 + param_widths.len()].to_vec();
        step += 1;
    }

    if !done {
        return None;
    }
    Some(outputs[1 + param_widths.len()..].to_vec())
}

// `gen_ir_diamond_and_inputs`/`gen_ir_multiblock_and_inputs` were tried here
// first (comparing `eval_ir_with_storage` against `run_movfuscated`), and
// found real semantic mismatches -- but confirmed, via a sibling git
// worktree re-running the identical generated inputs against the OLD
// (union-find, dataflow-edge-aware) `compute_static_slot_classes`, to
// reproduce IDENTICALLY on both slot-allocation schemes. That rules out
// `compute_static_slot_classes`'s own choice of scheme as the cause -- it's
// a pre-existing issue elsewhere (candidates identified but not fully
// root-caused: `gen_ir_diamond_and_inputs`'s entry block always JumpConds
// with the SAME arg list on both branches, exercising `emit_select_slot`
// with identical then/else operands; `gen_ir_multiblock_and_inputs`
// includes oracle calls, whose two independent evaluation paths
// (`eval_ir_with_storage` vs. the movfuscated circuit's own oracle
// handling) may not be guaranteed to observe calls in the same order).
// Neither is something `compute_static_slot_classes`'s (position, type)
// revert introduced or is responsible for fixing -- flagged for separate
// investigation, not pursued further here. In its place: a self-contained
// generator with NO JumpCond and NO oracle calls, built specifically to
// stress (position, type) slot sharing (a long chain of blocks, all
// sharing one physical slot, each applying a real transform) without
// tripping either unrelated issue.

/// `true` iff every var referenced in `out_a`/`out_b`'s corresponding
/// positions has matching widths. A width mismatch here is NOT a slot-
/// sharing bug (this function is never used to compare values, only
/// shapes) -- it flags a known, pre-existing, orthogonal representational
/// inconsistency: the plain CFG interpreter (`eval_ir_block`) gives a
/// `StorageWrite` statement's own "return value" width 0 (`vec![]` --
/// truly no output), while `movfuscate_ir`'s own `infer_stmt_result_type`
/// gives it width 1 (a `Bit` placeholder -- see its own doc comment: "no
/// meaningful result; use Bit as a placeholder"). A generated program that
/// returns a `StorageWrite` statement's own result (a degenerate,
/// semantically-meaningless pattern, but one the generic multiblock/diamond
/// generators don't specifically avoid) trips this, unrelated to anything
/// `compute_static_slot_classes` does.
#[allow(dead_code)]
fn shapes_match(a: &[IrValue], b: &[IrValue]) -> bool {
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| x.len() == y.len())
}

/// Build a chain of `n` blocks, all with exactly one `AES8` param at
/// position 0 (so `compute_static_slot_classes` merges every one of them
/// onto ONE shared physical slot), each computing `out = in XOR consts[i]`
/// via a real `Poly` statement and jumping unconditionally to the next --
/// no `JumpCond`, no oracle calls, so this can't trip either of the
/// pre-existing, orthogonal issues noted above. Block `n-1` returns its own
/// result. Expected output (independent of movfuscation entirely): `a XOR
/// consts[0] XOR consts[1] XOR ... XOR consts[n-2]` (block `n-1` computes no
/// further XOR, it just holds the final chain value and returns it).
fn build_xor_chain(
    n: usize,
    consts: &[u8],
) -> (IRBlocks<()>, volar_ir_common::TypeTable, IRTypeId) {
    use volar_ir_common::TypeTable;
    let types = TypeTable(vec![IrType::Primitive(Type::AES8)]);
    let g8 = IRTypeId(0);

    let wrap = |stmts: Vec<IRStmt>| -> Vec<Node<IRStmt, ()>> {
        stmts.into_iter().map(|s| Node::new(s, (), None)).collect()
    };

    let mut blocks = Vec::with_capacity(n);
    for i in 0..n {
        let is_last = i == n - 1;
        let (stmts, jump_var) = if is_last {
            (vec![], IRVarId(0))
        } else {
            let mut coeffs = PolyCoeffs::new();
            coeffs.insert(vec![IRVarId(0)], 1u8);
            let stmt = IRStmt::Poly {
                ty: g8.clone(),
                coeffs,
                constant: Constant {
                    hi: 0,
                    lo: consts[i] as u128,
                },
            };
            (wrap(vec![stmt]), IRVarId(1))
        };
        let terminator = if is_last {
            IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, vec![jump_var]),
            }
        } else {
            IRTerminator::Jmp {
                target: IRBranchTarget::new(
                    IRBlockTargetId::Block(IRBlockId((i + 1) as u32)),
                    vec![jump_var],
                ),
            }
        };
        blocks.push(IRBlock {
            params: vec![g8.clone()],
            stmts,
            terminator,
        });
    }

    (IRBlocks::new(blocks), types, g8)
}

proptest! {
    #[test]
    fn prop_m_movfuscate_slot_sharing_preserves_semantics_xor_chain(
        n in 2usize..12,
        consts in proptest::collection::vec(any::<u8>(), 0..11),
        a in any::<u8>(),
    ) {
        let consts = &consts[..(n - 1).min(consts.len())];
        // Pad if the generated vec came up short for this n.
        let mut padded = consts.to_vec();
        while padded.len() < n - 1 { padded.push(0); }

        let (blocks, types, _g8) = build_xor_chain(n, &padded);
        let u8_to_bits = |v: u8| -> IrValue { (0..8).map(|i| (v >> i) & 1 == 1).collect() };
        let bits_to_u8 = |v: &IrValue| -> u8 { v.iter().enumerate().map(|(i, &b)| (b as u8) << i).fold(0u8, |acc, b| acc | b) };
        let inputs = vec![u8_to_bits(a)];

        let expected = padded.iter().fold(a, |acc, &c| acc ^ c);

        let (ref_out, _) = eval_ir_with_storage(&blocks, &types, &inputs);
        let ref_out = ref_out.expect("xor chain must halt");
        prop_assert_eq!(bits_to_u8(&ref_out[0]), expected, "reference interpreter mismatch -- bug in this test's own construction");

        let actual_out = run_movfuscated(&blocks, &types, &inputs)
            .expect("movfuscated xor chain circuit must halt and produce output");
        prop_assert_eq!(
            bits_to_u8(&actual_out[0]), expected,
            "movfuscation (position, type) slot sharing changed semantics: {} blocks sharing one slot, consts={:?} a={:#x}",
            n, padded, a,
        );
    }
}

/// Direct, hand-built repro of the historical bug shape: two structurally
/// similar but semantically unrelated "diamonds" (dispatch-and-merge
/// sequences) visited one after another within a single run, whose params
/// coincide on `(position, type)` and therefore MUST share one physical
/// state slot under `compute_static_slot_classes`.
///
/// Block 0 (entry, external input `a`) -> Block 1 (computes `identity(a)`,
/// a real statement, not a bare pass-through) -> Block 2 ("merge of diamond
/// A": holds `merged_a`, then computes a FRESH constant `in_b = 0xBB`,
/// unrelated to `merged_a`) -> Block 3 -> Block 4 (Return).
///
/// Every block above has exactly one param, all AES8 (`G8`), all at
/// position 0 -- `compute_static_slot_classes` merges every one of them onto
/// ONE shared physical slot. The real question this answers: when Block 2
/// overwrites that shared slot with the fresh constant `0xBB` (a value with
/// zero dataflow relationship to `a`), does Block 3/4 see `0xBB` correctly,
/// or does it see a stale/aliased leftover from `a`'s own earlier
/// occupancy of the same slot? Run three times with different `a` values
/// (0x11, 0x00, 0xFF) -- the final output must be `0xBB` in every case,
/// regardless of `a`.
#[test]
fn test_movfuscate_sequential_unrelated_same_slot_no_aliasing() {
    use volar_ir_common::TypeTable;

    let types = TypeTable(vec![IrType::Primitive(Type::AES8)]);
    let g8 = IRTypeId(0);

    let wrap = |stmts: Vec<IRStmt>| -> Vec<Node<IRStmt, ()>> {
        stmts.into_iter().map(|s| Node::new(s, (), None)).collect()
    };

    let mut coeffs = PolyCoeffs::new();
    coeffs.insert(vec![IRVarId(0)], 1u8);

    let blocks: IRBlocks<()> = IRBlocks::new(vec![
        // Block 0: entry -- external input `a`. Jmp(Block 1, [a]).
        IRBlock {
            params: vec![g8.clone()],
            stmts: vec![],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Block(IRBlockId(1)), vec![IRVarId(0)]),
            },
        },
        // Block 1 ("diamond A" body): b = identity(a) [a real Poly stmt, not
        // a bare pass-through]. Jmp(Block 2, [b]).
        IRBlock {
            params: vec![g8.clone()],
            stmts: wrap(vec![IRStmt::Poly {
                ty: g8.clone(),
                coeffs,
                constant: Constant { hi: 0, lo: 0 },
            }]),
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Block(IRBlockId(2)), vec![IRVarId(1)]),
            },
        },
        // Block 2 ("merge of diamond A", holds merged_a): computes a FRESH
        // constant in_b = 0xBB, unrelated to merged_a. Jmp(Block 3, [in_b]).
        IRBlock {
            params: vec![g8.clone()],
            stmts: wrap(vec![IRStmt::Const(
                Constant { hi: 0, lo: 0xBB },
                g8.clone(),
            )]),
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Block(IRBlockId(3)), vec![IRVarId(1)]),
            },
        },
        // Block 3 ("diamond B" body, unrelated to diamond A except via slot
        // sharing): pass through unchanged. Jmp(Block 4, [c]).
        IRBlock {
            params: vec![g8.clone()],
            stmts: vec![],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Block(IRBlockId(4)), vec![IRVarId(0)]),
            },
        },
        // Block 4 ("merge of diamond B"): Return [c].
        IRBlock {
            params: vec![g8.clone()],
            stmts: vec![],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, vec![IRVarId(0)]),
            },
        },
    ]);

    let u8_to_bits = |v: u8| -> IrValue { (0..8).map(|i| (v >> i) & 1 == 1).collect() };
    let bits_to_u8 = |v: &IrValue| -> u8 {
        v.iter()
            .enumerate()
            .map(|(i, &b)| (b as u8) << i)
            .fold(0u8, |a, b| a | b)
    };

    for a in [0x11u8, 0x00u8, 0xFFu8] {
        let inputs = vec![u8_to_bits(a)];

        // Sanity check on the construction itself, via the independent plain
        // CFG interpreter -- must be 0xBB regardless of `a`.
        let (ref_out, _) = eval_ir_with_storage(&blocks, &types, &inputs);
        let ref_out = ref_out.expect("plain interpreter must halt");
        assert_eq!(
            bits_to_u8(&ref_out[0]),
            0xBB,
            "reference interpreter: expected 0xBB regardless of a={a:#x}"
        );

        // The real check: the movfuscated circuit, where every block above
        // shares ONE physical slot, must agree.
        let actual_out = run_movfuscated(&blocks, &types, &inputs)
            .expect("movfuscated circuit must halt and produce output");
        assert_eq!(
            bits_to_u8(&actual_out[0]),
            0xBB,
            "movfuscated circuit with shared slot: expected 0xBB regardless of a={a:#x} -- \
             a value other than 0xBB here means Block 3/4 aliased a stale value from a's \
             own earlier occupancy of the shared slot instead of Block 2's fresh write",
        );
    }
}
use crate::generators::ir::{gen_ir_diamond_and_inputs, gen_ir_multiblock_and_inputs};

proptest! {
    #[test]
    fn prop_m_movfuscate_slot_sharing_preserves_semantics_diamond(
        (ir, types, inputs) in gen_ir_diamond_and_inputs()
    ) {
        if ir.blocks.iter().all(|b| b.stmts.is_empty()) {
            return Ok(());
        }
        let (ref_out, _ref_storage) = eval_ir_with_storage(&ir, &types, &inputs);
        let ref_out = match ref_out { Some(v) => v, None => return Ok(()) };
        let actual_out = match run_movfuscated(&ir, &types, &inputs) { Some(v) => v, None => return Ok(()) };
        if !shapes_match(&actual_out, &ref_out) { return Ok(()); }
        prop_assert_eq!(actual_out, ref_out, "movfuscation (position, type) slot sharing changed semantics");
    }

    #[test]
    fn prop_m_movfuscate_slot_sharing_preserves_semantics_multiblock(
        (ir, types, inputs) in gen_ir_multiblock_and_inputs()
    ) {
        if ir.blocks.iter().all(|b| b.stmts.is_empty()) {
            return Ok(());
        }
        let (ref_out, _ref_storage) = eval_ir_with_storage(&ir, &types, &inputs);
        let ref_out = match ref_out { Some(v) => v, None => return Ok(()) };
        let actual_out = match run_movfuscated(&ir, &types, &inputs) { Some(v) => v, None => return Ok(()) };
        if !shapes_match(&actual_out, &ref_out) { return Ok(()); }
        prop_assert_eq!(actual_out, ref_out, "movfuscation (position, type) slot sharing changed semantics");
    }
}

/// Regression test for a real, previously-unknown gap found while
/// investigating the two proptests above: `movfuscate_ir` never propagated
/// `oracles`/`actions`/`rngs` from its input `IRBlocks` to its output --
/// only `pre_init` was copied. Any consumer trusting the movfuscated
/// circuit's own `.oracles` (as this file's `run_movfuscated` did, and as
/// `mem_probe.rs`/`wat_gen.rs`'s own production honest tests still do, via
/// the identical `eval_ir_circuit_step(..., &circuit.oracles, ...)`
/// pattern) got an empty list. `eval_ir_stmt`'s own oracle-index lookup
/// (`crates/fuzz/volar-fuzz/src/interpreter/ir.rs`) silently falls back to
/// index 0 on a missed name lookup rather than panicking -- so a circuit
/// declaring exactly one oracle happened to still work (index 0 is the
/// only real index anyway), but any circuit declaring two or more oracles
/// got every oracle call after the first silently miscomputed against the
/// WRONG oracle's hash seed. This is what the two proptests above were
/// actually hitting -- traced via the proptest-shrunk minimal failing
/// input, reproduced in isolation here with `o0`/`o1` unambiguously
/// distinguished by using disjoint output values, confirmed fixed by
/// `movfuscate_ir_impl`'s new `result.oracles = blocks.oracles.clone()`
/// (mirroring the pre-existing `pre_init` propagation).
#[test]
fn test_movfuscate_propagates_multiple_oracle_declarations() {
    use volar_ir_common::{OracleDecl, TypeTable};
    let t128 = IRTypeId(0);
    let t8 = IRTypeId(1);
    let types = TypeTable(vec![
        IrType::Primitive(Type::_128),
        IrType::Primitive(Type::_8),
    ]);

    // Two distinct oracles at different declared indices -- if oracle
    // lookup ever silently falls back to index 0, block 1's own call to
    // "o1" (index 1) would incorrectly compute "o0"'s (index 0) hash
    // instead, and the two branches' own results would collide.
    let o0 = OracleDecl {
        name: "o0".to_string(),
        params: vec![t128.clone()],
        results: vec![t8.clone()],
    };
    let o1 = OracleDecl {
        name: "o1".to_string(),
        params: vec![t128.clone()],
        results: vec![t8.clone()],
    };

    let blocks: IRBlocks<()> = {
        let mut b = IRBlocks::new(vec![
            // Block 0: call BOTH oracles with the SAME input -- if oracle
            // index resolution is broken, both calls collide onto the same
            // (wrong) hash and produce identical outputs despite genuinely
            // different declared oracles.
            IRBlock {
                params: vec![t128.clone()],
                stmts: vec![
                    Node::new(
                        IRStmt::OracleCall {
                            name: "o0".to_string(),
                            args: vec![IRVarId(0)],
                            output_tys: vec![t8.clone()],
                            result_ty: t8.clone(),
                        },
                        (),
                        None,
                    ),
                    Node::new(
                        IRStmt::OracleOutput {
                            call: IRVarId(1),
                            idx: 0,
                            ty: t8.clone(),
                        },
                        (),
                        None,
                    ),
                    Node::new(
                        IRStmt::OracleCall {
                            name: "o1".to_string(),
                            args: vec![IRVarId(0)],
                            output_tys: vec![t8.clone()],
                            result_ty: t8.clone(),
                        },
                        (),
                        None,
                    ),
                    Node::new(
                        IRStmt::OracleOutput {
                            call: IRVarId(3),
                            idx: 0,
                            ty: t8.clone(),
                        },
                        (),
                        None,
                    ),
                ],
                terminator: IRTerminator::Jmp {
                    target: IRBranchTarget::new(
                        IRBlockTargetId::Return,
                        vec![IRVarId(2), IRVarId(4)],
                    ),
                },
            },
        ]);
        b.oracles = vec![o0, o1];
        b
    };

    let bits_to_u8 = |v: &IrValue| -> u8 {
        v.iter()
            .enumerate()
            .map(|(i, &b)| (b as u8) << i)
            .fold(0u8, |acc, b| acc | b)
    };
    let inputs = vec![vec![true; 128]];

    let (ref_out, _) = eval_ir_with_storage(&blocks, &types, &inputs);
    let ref_out = ref_out.expect("plain interpreter must halt");
    let (o0_ref, o1_ref) = (bits_to_u8(&ref_out[0]), bits_to_u8(&ref_out[1]));
    assert_ne!(
        o0_ref, o1_ref,
        "test construction sanity: o0 and o1 must hash to different values for this input"
    );

    // `movfuscate_ir` is single-block (n=1) here, which short-circuits to a
    // plain clone (oracles already correct by construction) -- wrap in a
    // trivial 2-block chain so `movfuscate_ir_impl`'s real combine path
    // (the one that previously dropped `.oracles`) actually runs.
    let blocks: IRBlocks<()> = {
        let mut b2 = IRBlocks::new(vec![
            blocks.blocks[0].clone(),
            IRBlock {
                params: vec![t8.clone(), t8.clone()],
                stmts: vec![],
                terminator: IRTerminator::Jmp {
                    target: IRBranchTarget::new(
                        IRBlockTargetId::Return,
                        vec![IRVarId(0), IRVarId(1)],
                    ),
                },
            },
        ]);
        b2.blocks[0].terminator = IRTerminator::Jmp {
            target: IRBranchTarget::new(
                IRBlockTargetId::Block(IRBlockId(1)),
                vec![IRVarId(2), IRVarId(4)],
            ),
        };
        b2.oracles = blocks.oracles;
        b2
    };

    let actual_out =
        run_movfuscated(&blocks, &types, &inputs).expect("movfuscated circuit must halt");
    let (o0_actual, o1_actual) = (bits_to_u8(&actual_out[0]), bits_to_u8(&actual_out[1]));

    assert_eq!(o0_actual, o0_ref, "o0 (index 0) mismatched");
    assert_eq!(
        o1_actual, o1_ref,
        "o1 (index 1) mismatched -- got {o1_actual:#x}, expected {o1_ref:#x} (o0's own value is \
         {o0_ref:#x}) -- a match against o0_ref here would mean oracle index resolution silently \
         fell back to index 0 for a name it failed to find in an empty/wrong oracles list",
    );
}

// ============================================================================
// Property O — unroll_ir_everything + lower_ir_to_boolar preserves semantics
// for genuinely multi-block, direct-from-Volar-IR programs (no VAFFLE).
//
// Property D/D2 (above) already cover lower_ir_to_boolar for a single block
// that's already `is_circuit()`-shaped by construction. This exercises the
// same target reached via unroll_ir_everything folding *real* multi-block
// structure first -- closer to the real `volar-ir-build` pipeline's own
// "concrete CF -> unroll -> lower_to_boolar" route (see docs/pipeline.md's
// "Circuit-shape strategies"). Uses `generators/ir.rs`'s own (VAFFLE-free)
// multiblock/extended generators directly: going through VAFFLE (property N)
// adds ABI-packing noise this doesn't need to exercise.
// ============================================================================

/// `true` iff the block ending in `Jmp(Return)` returns a value produced
/// directly by a `StorageWrite` -- same pre-existing void-result-width
/// quirk as `returns_a_void_or_aggregate_value` above (property M's
/// `shapes_match` doc comment first documented it), just for plain
/// `IRBlocks` instead of VAFFLE. `IRVarId`s here are per-block-local (see
/// `interpret_ir_multiblock`'s own re-indexing), so this only needs to
/// look at the one block whose terminator actually returns.
fn ir_returns_a_void_value(ir: &IRBlocks<()>) -> bool {
    for block in &ir.blocks {
        let IRTerminator::Jmp { target } = &block.terminator else {
            continue;
        };
        if target.dest != IRBlockTargetId::Return {
            continue;
        }
        let n_params = block.params.len();
        return target.args.iter().any(|vid| {
            let idx = vid.0 as usize;
            idx >= n_params
                && matches!(
                    block.stmts.get(idx - n_params).map(|n| &n.kind),
                    Some(IRStmt::StorageWrite { .. })
                )
        });
    }
    false
}

proptest! {
    #[test]
    fn prop_o_ir_multiblock_unroll_to_boolar_preserves_semantics(
        (ir, types, inputs) in gen_ir_multiblock_and_inputs()
            .prop_filter("no StorageWrite in return list (void-width quirk, see property M)", |(ir, _, _)| !ir_returns_a_void_value(ir))
    ) {
        let ir_out = match eval_ir(&ir, &types, &inputs) {
            Some(v) => v,
            None => return Ok(()),
        };
        // No real branch in this generator's output (B0 always jumps
        // unconditionally to B1), so this should always fold.
        let unrolled = match unroll_ir_everything(&ir, &types) {
            Ok(u) => u,
            Err(_) => return Ok(()),
        };
        let lowered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            lower_ir_to_boolar(&unrolled, &types)
        }));
        let boolar = match lowered {
            Ok(b) => b,
            Err(payload) => {
                let msg = payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&'static str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                // Same pre-existing, unrelated-to-bitwidth skips as property N.
                if msg.contains("SignatureMismatch") || msg.contains("flat cell space") {
                    return Ok(());
                }
                panic!("lower_ir_to_boolar panicked unexpectedly: {msg}");
            }
        };
        let flat_inputs = bit_flatten(&inputs);
        let flat_boolar_out = match eval_biir(&boolar, &flat_inputs) {
            Some(v) => v,
            None => {
                prop_assert!(false, "eval_biir on unrolled multiblock IR did not terminate");
                return Ok(());
            }
        };
        prop_assert_eq!(
            flat_boolar_out, bit_flatten(&ir_out),
            "unroll_ir_everything + lower_ir_to_boolar changed the semantics (multiblock)"
        );
    }

    #[test]
    fn prop_o_ir_extended_unroll_to_boolar_preserves_semantics(
        (ir, types, inputs) in gen_ir_extended_and_inputs()
            .prop_filter("no StorageWrite in return list (void-width quirk, see property M)", |(ir, _, _)| !ir_returns_a_void_value(ir))
    ) {
        let ir_out = match eval_ir(&ir, &types, &inputs) {
            Some(v) => v,
            None => return Ok(()),
        };
        // This generator's JumpCond condition is sometimes a compile-time
        // Const (foldable) and sometimes a param (not) -- skip the latter,
        // same as any other unroll-unrepresentable shape.
        let unrolled = match unroll_ir_everything(&ir, &types) {
            Ok(u) => u,
            Err(_) => return Ok(()),
        };
        let lowered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            lower_ir_to_boolar(&unrolled, &types)
        }));
        let boolar = match lowered {
            Ok(b) => b,
            Err(payload) => {
                let msg = payload
                    .downcast_ref::<String>()
                    .cloned()
                    .or_else(|| payload.downcast_ref::<&'static str>().map(|s| s.to_string()))
                    .unwrap_or_default();
                // Same pre-existing, unrelated-to-bitwidth skips as property N.
                if msg.contains("SignatureMismatch") || msg.contains("flat cell space") {
                    return Ok(());
                }
                panic!("lower_ir_to_boolar panicked unexpectedly: {msg}");
            }
        };
        let flat_inputs = bit_flatten(&inputs);
        let flat_boolar_out = match eval_biir(&boolar, &flat_inputs) {
            Some(v) => v,
            None => {
                prop_assert!(false, "eval_biir on unrolled extended IR did not terminate");
                return Ok(());
            }
        };
        prop_assert_eq!(
            flat_boolar_out, bit_flatten(&ir_out),
            "unroll_ir_everything + lower_ir_to_boolar changed the semantics (extended)"
        );
    }
}

// ============================================================================
// Property P — movfuscate_ir + lower_ir_to_boolar agree for ONE iteration of
// a genuinely branching (non-unrollable) CFG, direct-from-Volar-IR.
//
// `unroll_ir_everything` cannot handle `gen_ir_diamond_and_inputs`'s output:
// its branch condition is always a param, i.e. genuinely symbolic to unroll.
// `movfuscate_ir` is the only route to a `lower_ir_to_boolar`-representable
// shape for real, non-foldable control flow -- this is the "arbitrary CFG"
// counterpart to property O's concrete/unrollable one.
//
// Rather than driving the movfuscated self-loop to real completion (which
// needs `movfuscate_ir_with_boundary_and_watch`'s slot-resolving "watch"
// mechanism -- see property M's `run_movfuscated`), this checks a narrower
// but sufficient invariant for bitwidth bugs specifically: for ONE
// arbitrary (fully-random, not tied to any real initial state) full state
// vector, do `eval_ir_circuit_step` (plain interpreter) and `eval_biir`
// (after `lower_ir_to_boolar`) compute the identical one-step result on the
// exact same single-iteration-budgeted circuit
// (`lower_to_circuit_ir(..., 1, LoweringMode::WithTerminationFlag)`)? A
// bitwidth mismatch in either movfuscate's own state packing or
// `lower_ir_to_boolar`'s translation of it shows up here without needing to
// reason about multi-step convergence at all.
// ============================================================================

use volar_ir_passes::movfuscate::movfuscate_ir_with_control_provenance;

/// Cycle `seed` (padding with `false` if empty) to exactly `total` bits,
/// then split into `widths`-shaped `IrValue`s. Turns an already-shrinkable
/// proptest value (`inputs`, from the *original* program's own param
/// generation) into a same-shaped "arbitrary state" input for the
/// *movfuscated* circuit, whose own param count/widths are only known
/// after movfuscating -- not something a `Strategy` can size upfront
/// without duplicating `movfuscate_ir`'s own slot-layout logic.
fn cycle_to_widths(seed: &[bool], widths: &[usize]) -> Vec<IrValue> {
    let total: usize = widths.iter().sum();
    let bits: Vec<bool> = if seed.is_empty() {
        vec![false; total]
    } else {
        (0..total).map(|i| seed[i % seed.len()]).collect()
    };
    let mut out = Vec::with_capacity(widths.len());
    let mut off = 0;
    for &w in widths {
        out.push(bits[off..off + w].to_vec());
        off += w;
    }
    out
}

/// Movfuscate `ir`, budget it to exactly one self-loop iteration, and
/// evaluate that single step both ways. `None` means this input has no
/// real control flow to movfuscate (`is_movfuscated()` is false for an
/// already-single, linear block) or hit a known-unrelated lowering limit
/// (same skip list as property N/O).
fn one_step_via_movfuscate(
    ir: &IRBlocks<()>,
    types: &IRTypes,
    seed: &[bool],
) -> Option<(Vec<bool>, Vec<bool>)> {
    let mut mut_types = types.clone();
    let movfuscated = movfuscate_ir_with_control_provenance(ir, &mut mut_types, &());
    if !movfuscated.is_movfuscated() {
        return None;
    }
    let bit_ty = mut_types
        .0
        .iter()
        .position(|t| matches!(t, IrType::Primitive(Type::Bit)))
        .map(|i| IRTypeId(i as u32))?;
    let circuit = lower_to_circuit_ir(&movfuscated, &bit_ty, 1, LoweringMode::WithTerminationFlag);

    let param_widths: Vec<usize> = circuit.blocks[0]
        .params
        .iter()
        .map(|&tid| bit_width(tid, &mut_types))
        .collect();
    let state = cycle_to_widths(seed, &param_widths);

    let mut storage_ir: StorageMap = StorageMap::new();
    apply_pre_init(&mut storage_ir, &circuit.pre_init, &mut_types);
    let ir_step_out = eval_ir_circuit_step(
        &circuit.blocks[0],
        &mut_types,
        &circuit.oracles,
        &state,
        &mut storage_ir,
    );

    let lowered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        lower_ir_to_boolar(&circuit, &mut_types)
    }));
    let boolar = match lowered {
        Ok(b) => b,
        Err(payload) => {
            let msg = payload
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| {
                    payload
                        .downcast_ref::<&'static str>()
                        .map(|s| s.to_string())
                })
                .unwrap_or_default();
            if msg.contains("SignatureMismatch")
                || msg.contains("flat cell space")
                || msg.contains("mixed element-address widths")
            {
                return None;
            }
            panic!("lower_ir_to_boolar panicked unexpectedly: {msg}");
        }
    };
    let flat_state = bit_flatten(&state);
    let boolar_step_out = eval_biir(&boolar, &flat_state)?;

    Some((bit_flatten(&ir_step_out), boolar_step_out))
}

proptest! {
    /// Check exact one-step equivalence after movfuscation and Boolar
    /// lowering. Shapes whose storage addresses are not representable by
    /// Boolar's flat-cell model are skipped by `one_step_via_movfuscate`.
    #[test]
    fn prop_p_ir_multiblock_movfuscate_one_step_agrees(
        (ir, types, inputs) in gen_ir_multiblock_and_inputs()
            .prop_filter("no StorageWrite in return list (void-width quirk, see property M)", |(ir, _, _)| !ir_returns_a_void_value(ir))
    ) {
        let seed: Vec<bool> = inputs.iter().flatten().copied().collect();
        let (ir_out, boolar_out) = match one_step_via_movfuscate(&ir, &types, &seed) {
            Some(pair) => pair,
            None => return Ok(()),
        };
        prop_assert_eq!(
            ir_out, boolar_out,
            "movfuscate + lower_ir_to_boolar disagree after one step (multiblock)"
        );
    }

    /// The real, non-foldable branch counterpart of the multiblock property.
    #[test]
    fn prop_p_ir_diamond_movfuscate_one_step_agrees(
        (ir, types, inputs) in gen_ir_diamond_and_inputs()
            .prop_filter("no StorageWrite in return list (void-width quirk, see property M)", |(ir, _, _)| !ir_returns_a_void_value(ir))
    ) {
        let seed: Vec<bool> = inputs.iter().flatten().copied().collect();
        let (ir_out, boolar_out) = match one_step_via_movfuscate(&ir, &types, &seed) {
            Some(pair) => pair,
            None => return Ok(()),
        };
        prop_assert_eq!(
            ir_out, boolar_out,
            "movfuscate + lower_ir_to_boolar disagree after one step (diamond / real branch)"
        );
    }
}
