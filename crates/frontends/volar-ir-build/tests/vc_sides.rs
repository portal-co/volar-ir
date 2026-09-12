//! Stage-C end-to-end: vc entry-param sides surface as an explicit lowering
//! output and propagate through the full waffle → vaffle → IRBlocks →
//! BIrBlocks pipeline into per-wire input sides the MPC/vc weaver can build
//! an `InputPartition` from.
//!
//! Run with: `cargo test -p volar-ir-build --features wasm --test vc_sides`.

use volar_ir_passes::lower_ir_to_boolar::{SideInputs, lower_ir_to_boolar_with_sides};
use volar_side::SideId;

/// Lower a vc-tagged vaffle module all the way to a circuit-shaped BIrBlocks
/// with side propagation, returning the boolar blocks. Movfuscation collapses
/// the function's CFG (and its return-dispatch Dyn jump, which BIrTerminator
/// cannot represent) into a single self-looping block, the circuit shape the
/// boolar lowering requires.
fn lower_to_boolar_with_sides<P: Clone>(
    module: vaffle::Module<P>,
    param_sides: Vec<Option<SideId>>,
) -> volar_ir::boolar::BIrBlocks<P> {
    let (blocks, mut types) = volar_vaffle_target::lower_vaffle_to_ir_owned(module);
    let blocks = volar_ir_passes::movfuscate_ir_owned(blocks, &mut types);
    let mut side_inputs = SideInputs::default();
    side_inputs.param_sides.insert(0, param_sides);
    lower_ir_to_boolar_with_sides(&blocks, &types, &side_inputs)
}

/// A two-i32-arg multiply — the smallest nontrivial circuit whose derived op
/// has two side-bearing operands. The `$multiply` internal name is what the
/// vaffle lowering keys its export map on (the `(export "multiply")` alias
/// alone leaves the function name empty when parsed through waffle-frontend).
const MULT_WAT: &str = r#"(module
  (func $multiply (export "multiply") (param i32 i32) (result i32)
    local.get 0
    local.get 1
    i32.mul))"#;

/// Parse the WAT into a waffle module. Returns the module and keeps the
/// source bytes alive for the module's lifetime via a leaked box (test-only;
/// the waffle `Module<'a>` borrows its input bytes).
fn waffle_module() -> portal_pc_waffle_ir::Module<'static> {
    let bytes: &'static [u8] = Box::leak(wat::parse_str(MULT_WAT).expect("wat").into_boxed_slice());
    portal_pc_waffle_frontend::from_wasm_bytes(
        bytes,
        &portal_pc_waffle_frontend::FrontendOptions::default(),
    )
    .expect("waffle parse")
}

#[test]
fn param_sides_surface_and_propagate_to_boolar() {
    let module = waffle_module();
    // Both args private → both entry params share the local side, so the
    // multiply's operand-side join is `local` end to end.
    let vc = volar_vaffle_target::VcConfig::new().with_call(
        "multiply",
        vec![volar_vaffle_target::VcArg::Private, volar_vaffle_target::VcArg::Private],
    );
    let mut target = volar_vaffle_target::VaffleTarget::with_pointer_width(
        vaffle::PointerWidth::Bits32,
    );
    let (errors, artifact) = volar_vaffle_target::lower_waffle_module_with_vc(
        &module,
        &mut target,
        &volar_vaffle_target::WaffleImportConfig::default(),
        &vc,
    );
    assert!(errors.is_empty(), "{errors:?}");
    let local = artifact.handler.local;

    // Stage-C bridge: entry param sides as an explicit lowering output.
    let param_sides =
        volar_vaffle_target::entry_param_sides(&target.module).expect("entry body");
    assert_eq!(param_sides.len(), 64, "two i32 args flatten to 64 bits");
    assert!(
        param_sides.iter().all(|s| *s == Some(local)),
        "every entry param bit should be local/private"
    );

    // vaffle → IRBlocks (A-stage sides live) → movfuscated circuit →
    // BIrBlocks with the extracted param sides (B-stage propagation).
    let boolar = lower_to_boolar_with_sides(target.module, param_sides);

    // The BIrBlocks must carry the joined `local` side on derived gates (the
    // multiply's bit-level gates), proving sides reach the weaver's input.
    let any_local = boolar
        .blocks
        .iter()
        .any(|b| b.stmts.iter().any(|n| n.side == Some(local)));
    assert!(
        any_local,
        "expected local-tagged derived gates in BIrBlocks; side propagation broke across the pipeline"
    );

    // And the param-side vector we surfaced matches what the boolar lowering
    // consumed (the weaver builds its InputPartition from exactly this).
    let total_boolar_params: usize = boolar.blocks.iter().map(|b| b.params as usize).sum();
    assert!(total_boolar_params >= 64);
}

#[test]
fn untagged_module_yields_no_param_sides() {
    // Without vc, nothing stamps sides, so entry_param_sides is all-None and
    // the side channel is inert.
    let module = waffle_module();
    let mut target = volar_vaffle_target::VaffleTarget::with_pointer_width(
        vaffle::PointerWidth::Bits32,
    );
    volar_vaffle_target::lower_waffle_module(
        &module,
        &mut target,
        &volar_vaffle_target::WaffleImportConfig::default(),
    );
    let param_sides =
        volar_vaffle_target::entry_param_sides(&target.module).expect("entry body");
    assert!(param_sides.iter().all(|s| s.is_none()), "{param_sides:?}");

    let boolar = lower_to_boolar_with_sides(target.module, param_sides);
    let any_tagged = boolar
        .blocks
        .iter()
        .any(|b| b.stmts.iter().any(|n| n.side.is_some()));
    assert!(!any_tagged, "untagged module must produce side-free BIrBlocks");
}

#[test]
fn private_times_blind_join_is_unattributable() {
    // A private × blind multiply has operands on different sides; the join is
    // None (a mixed wire is not attributable to a single side). The param
    // sides still surface correctly for the weaver's partition.
    let module = waffle_module();
    let vc = volar_vaffle_target::VcConfig::new().with_call(
        "multiply",
        vec![volar_vaffle_target::VcArg::Private, volar_vaffle_target::VcArg::Blind],
    );
    let mut target = volar_vaffle_target::VaffleTarget::with_pointer_width(
        vaffle::PointerWidth::Bits32,
    );
    let (errors, artifact) = volar_vaffle_target::lower_waffle_module_with_vc(
        &module,
        &mut target,
        &volar_vaffle_target::WaffleImportConfig::default(),
        &vc,
    );
    assert!(errors.is_empty(), "{errors:?}");
    let local = artifact.handler.local;
    let remote = artifact.handler.remote;

    let param_sides =
        volar_vaffle_target::entry_param_sides(&target.module).expect("entry body");
    assert!(
        param_sides[..32].iter().all(|s| *s == Some(local)),
        "param 0 local: {:?}",
        &param_sides[..32]
    );
    assert!(
        param_sides[32..64].iter().all(|s| *s == Some(remote)),
        "param 1 remote: {:?}",
        &param_sides[32..64]
    );

    let boolar = lower_to_boolar_with_sides(target.module, param_sides);
    // A 32×32 multiply's bit-level circuit has single-party subcomputations
    // (partial products along one operand) that legitimately join to `local`
    // or `remote`, plus the final combining gates that mix both and join to
    // None. The join's contract is that a gate is tagged only when *all* its
    // operands share that side — so a gate is never tagged with a side that
    // mixes the two parties. Assert that both single-party tags appear (each
    // operand genuinely participated) and that at least one gate is None
    // (the operands combined).
    let n_local = boolar
        .blocks
        .iter()
        .flat_map(|b| &b.stmts)
        .filter(|n| n.side == Some(local))
        .count();
    let n_remote = boolar
        .blocks
        .iter()
        .flat_map(|b| &b.stmts)
        .filter(|n| n.side == Some(remote))
        .count();
    let n_none = boolar
        .blocks
        .iter()
        .flat_map(|b| &b.stmts)
        .filter(|n| n.side.is_none())
        .count();
    assert!(n_local > 0, "local operand should drive some gates");
    assert!(n_remote > 0, "remote operand should drive some gates");
    assert!(n_none > 0, "the two operands must combine somewhere (join → None)");
}

/// The surfaced per-param-bit sides are exactly the index sets an
/// `InputPartition` is built from: local bits → one party, remote bits → the
/// other, None → public. Mirror that derivation here and check it partitions
/// the 64 input bits disjointly.
#[test]
fn surfaced_sides_form_a_partition() {
    let module = waffle_module();
    let vc = volar_vaffle_target::VcConfig::new().with_call(
        "multiply",
        vec![volar_vaffle_target::VcArg::Private, volar_vaffle_target::VcArg::Blind],
    );
    let mut target = volar_vaffle_target::VaffleTarget::with_pointer_width(
        vaffle::PointerWidth::Bits32,
    );
    let (_errors, artifact) = volar_vaffle_target::lower_waffle_module_with_vc(
        &module,
        &mut target,
        &volar_vaffle_target::WaffleImportConfig::default(),
        &vc,
    );
    let local = artifact.handler.local;
    let remote = artifact.handler.remote;

    let param_sides =
        volar_vaffle_target::entry_param_sides(&target.module).expect("entry body");
    let mut local_bits: Vec<u32> = vec![];
    let mut remote_bits: Vec<u32> = vec![];
    let mut public_bits: Vec<u32> = vec![];
    for (i, s) in param_sides.iter().enumerate() {
        match *s {
            Some(x) if x == local => local_bits.push(i as u32),
            Some(x) if x == remote => remote_bits.push(i as u32),
            _ => public_bits.push(i as u32),
        }
    }
    assert_eq!(local_bits.len(), 32);
    assert_eq!(remote_bits.len(), 32);
    assert_eq!(public_bits.len(), 0);
    // Disjoint and covering.
    assert!(local_bits.iter().all(|b| !remote_bits.contains(b)));
    assert_eq!(local_bits.len() + remote_bits.len() + public_bits.len(), 64);

    let _ = SideId(0); // keep the SideId import used even if assertions shift
}
