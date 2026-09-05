// @reliability: normal
// @ai: assisted
//! Property G′ — the *typed* region/gadget authoring path preserves
//! semantics end-to-end through the boundary-moving passes.
//!
//! Pipeline under test (see `docs/typed-gadgets-and-region-threading-plan.md`):
//!
//! ```text
//! typed multi-block host (Bit-typed block params)
//!   ├─ TypedRegionTable (typed Input anchors on block-0 params)
//!   ├─ TypedGadgetLibrary (typed XOR pad: Poly { d + k } over Bit)
//!   └─ TypedGadgetBinding (Const aux)
//!         │ movfuscate_region_layout + translate_regions_movfuscate
//!         │   (typed anchors → combined-block state slots, pc offset)
//!         │ movfuscate_ir
//!         │ lower_ir_to_boolar_with_tables   (VarBitMap + lanes)
//!         │ lower_typed_region_table         (typed → landed bit anchors)
//!         │ lower_gadget_library / lower_typed_bindings
//!         │ lower_to_circuit(WithTerminationFlag)
//!         │   + translate_regions_termination_flag (+ landed output anchors)
//!         ▼
//! apply_gadgets → eval(wrapped, ciphertext) == eval(host, plaintext) ⊕ keys
//! ```
//!
//! A misplacement in *any* translation (slot mapping, bit allocation,
//! done-flag shift) makes some input combination diverge.
//!
//! **Generator restriction (documented upstream bug):** block params are
//! `Bit`-typed, i.e. one state slot each. `movfuscate_ir` currently loses
//! multi-bit (`Vec`) state-slot contents on multi-block programs (reproduced
//! by hand: a 2-block identity swap over `Vec(2, Bit)` params returns
//! `[w0, 0]` — slot 1 dropped — from the movfuscated step circuit, while the
//! pre-movfuscation program and the pre-movfuscation Boolar lowering are
//! both correct). Properties A/M do not cover this because BIr slots are
//! all 1-bit and Property M's generators use 1-slot scalar params. Widening
//! this generator to `Vec` params is blocked on that fix; the typed layers
//! themselves are word-width-tested by
//! `region_lowering::tests::typed_table_and_gadget_lower_and_splice`
//! (8-bit-wide typed gadget ports on a `VCircuit` host) and by the
//! per-plane storage fan-out test.

use std::vec::Vec;

use proptest::prelude::*;
use volar_ir::boolar::{BIrStmt, LaneId};
use volar_ir::circuit::BCircuit;
use volar_ir::gadget::{AuxSource, GadgetBinding, GadgetLibrary, GadgetSpec, Port, PortKind};
use volar_ir::ir::{
    IRBlock, IRBlockId, IRBlockTargetId, IRBranchTarget, IRBlocks, IRTerminator, IRType, IRTypeId,
    IRVarId,
};
use volar_ir::region::{RegionEntry, RegionId, RegionSelector, RegionTable, WireAnchor};
use volar_ir::typed_gadget::{
    TypedAnchor, TypedAuxSource, TypedGadgetBinding, TypedGadgetLibrary, TypedGadgetSpec,
    TypedPort, TypedRegionEntry, TypedRegionTable,
};
use volar_ir_common::{Constant, PolyCoeffs, Type, TypeTable};
use volar_ir_passes::apply_gadgets;
use volar_ir_passes::region_lowering::{
    lower_gadget_library, lower_typed_bindings, lower_typed_region_table,
    movfuscate_region_layout, movfuscate_state_regions, translate_regions_movfuscate,
    translate_regions_termination_flag,
};

use crate::interpreter::ir::{bit_flatten, eval_ir};

/// Evaluate a pure-gate `BCircuit` (no storage in this generator).
fn eval_plain(circ: &BCircuit<()>, params: &[bool]) -> Vec<bool> {
    let mut vals: Vec<Option<bool>> = vec![None; circ.var_space() as usize];
    for (i, &b) in params.iter().enumerate() {
        vals[i] = Some(b);
    }
    for (i, node) in circ.stmts.iter().enumerate() {
        let v = circ.params as usize + i;
        vals[v] = Some(match &node.kind {
            BIrStmt::Zero => false,
            BIrStmt::One => true,
            BIrStmt::And(a, b) => vals[a.0 as usize].unwrap() & vals[b.0 as usize].unwrap(),
            BIrStmt::Or(a, b) => vals[a.0 as usize].unwrap() | vals[b.0 as usize].unwrap(),
            BIrStmt::Xor(a, b) => vals[a.0 as usize].unwrap() ^ vals[b.0 as usize].unwrap(),
            BIrStmt::Not(a) => !vals[a.0 as usize].unwrap(),
            other => panic!("eval_plain: unsupported stmt {:?}", other),
        });
    }
    circ.outputs.iter().map(|o| vals[o.0 as usize].unwrap()).collect()
}

/// One randomly generated typed case: a 2-block host over `n_params` Bit
/// block params (1 state slot each).
#[derive(Debug)]
struct TypedCase {
    n_params: usize,
    /// `(op, a, b)` gate specs per block — operands resolved modulo the
    /// defined-var count. Ops: 0=Zero, 1=One, 2=And, 3=Or, 4=Xor, 5=Not.
    ops0: Vec<(u8, u32, u32)>,
    ops1: Vec<(u8, u32, u32)>,
    /// Wrap input param 1 (param 0 is always wrapped).
    wrap_param1: bool,
    keys: Vec<bool>,
}

fn build_typed_host(tc: &TypedCase, types: &mut TypeTable) -> IRBlocks {
    use volar_ir_common::Node;
    let bit = types.intern(IRType::Primitive(Type::Bit));
    let _ = bit;
    let run = |params: usize, ops: &[(u8, u32, u32)]| -> (Vec<Node<volar_ir::ir::IRStmt, ()>>, Vec<IRVarId>) {
        let mut stmts = Vec::new();
        let mut defined: Vec<IRVarId> = (0..params as u32).map(IRVarId).collect();
        for &(op, ra, rb) in ops {
            let pick = |r: u32| defined[(r as usize) % defined.len()];
            let kind = match op % 6 {
                0 => volar_ir::ir::IRStmt::Const(Constant { hi: 0, lo: 0 }, bit),
                1 => volar_ir::ir::IRStmt::Const(Constant { hi: 0, lo: 1 }, bit),
                2 => volar_ir::ir::IRStmt::Poly {
                    ty: bit,
                    coeffs: PolyCoeffs::from_iter([(vec![pick(ra), pick(rb)], 1u8)]),
                    constant: Constant { hi: 0, lo: 0 },
                },
                3 => volar_ir::ir::IRStmt::Poly {
                    ty: bit,
                    coeffs: PolyCoeffs::from_iter([
                        (vec![pick(ra)], 1u8),
                        (vec![pick(rb)], 1u8),
                        (vec![pick(ra), pick(rb)], 1u8),
                    ]),
                    constant: Constant { hi: 0, lo: 0 },
                },
                4 => volar_ir::ir::IRStmt::Poly {
                    ty: bit,
                    coeffs: PolyCoeffs::from_iter([
                        (vec![pick(ra)], 1u8),
                        (vec![pick(rb)], 1u8),
                    ]),
                    constant: Constant { hi: 0, lo: 0 },
                },
                _ => volar_ir::ir::IRStmt::Poly {
                    ty: bit,
                    coeffs: PolyCoeffs::from_iter([
                        (vec![pick(ra)], 1u8),
                    ]),
                    constant: Constant { hi: 0, lo: 1 },
                },
            };
            stmts.push(Node {
                kind,
                prov: (),
                side: None,
            });
            defined.push(IRVarId(params as u32 + stmts.len() as u32 - 1));
        }
        (stmts, defined)
    };
    let (stmts0, defined0) = run(tc.n_params, &tc.ops0);
    let (stmts1, defined1) = run(tc.n_params, &tc.ops1);
    // Jump args / return values must match the target signature width (np).
    let take = |mut v: Vec<IRVarId>| {
        v.truncate(tc.n_params);
        v
    };
    let defined0 = take(defined0);
    let defined1 = take(defined1);
    let b0 = IRBlock {
        params: vec![bit; tc.n_params],
        stmts: stmts0,
        terminator: IRTerminator::Jmp {
            target: IRBranchTarget {
                dest: IRBlockTargetId::Block(IRBlockId(1)),
                args: defined0,
                reentry: None,
            },
        },
    };
    let b1 = IRBlock {
        params: vec![bit; tc.n_params],
        stmts: stmts1,
        terminator: IRTerminator::Jmp {
            target: IRBranchTarget {
                dest: IRBlockTargetId::Return,
                args: defined1,
                reentry: None,
            },
        },
    };
    IRBlocks {
        oracles: vec![],
        actions: vec![],
        rngs: vec![],
        blocks: vec![b0, b1],
        pre_init: vec![],
    }
}

/// The typed XOR pad over `Bit`: `data + key` (two linear Poly monomials).
fn typed_pad(types: &mut TypeTable) -> TypedGadgetSpec {
    let bit = types.intern(IRType::Primitive(Type::Bit));
    let mut body = volar_ir::circuit::VCircuit::new(vec![bit, bit]);
    let out = body.push_stmt(
        volar_ir::ir::IRStmt::Poly {
            ty: bit,
            coeffs: PolyCoeffs::from_iter([
                (vec![IRVarId(0)], 1u8),
                (vec![IRVarId(1)], 1u8),
            ]),
            constant: Constant { hi: 0, lo: 0 },
        },
        (),
    );
    body.outputs = vec![out];
    TypedGadgetSpec {
        name: String::from("tpad"),
        ports: vec![
            TypedPort {
                name: String::from("data"),
                kind: PortKind::Data,
                ty: bit,
                count: 1,
            },
            TypedPort {
                name: String::from("key"),
                kind: PortKind::Aux,
                ty: bit,
                count: 1,
            },
        ],
        encrypt: body,
        decrypt: None, // XOR is self-inverse
    }
}

fn bit_const(b: bool) -> TypedAuxSource {
    TypedAuxSource::Const(vec![Constant { hi: 0, lo: b as u128 }])
}

/// The landed bit-level XOR pad (used for the landed output wrap).
fn bit_pad_lib() -> GadgetLibrary {
    let mut body = BCircuit::<()>::new(2);
    let x = body.push_stmt(BIrStmt::Xor(IRVarId(0), IRVarId(1)), ());
    body.outputs = vec![x];
    GadgetLibrary::new().with(GadgetSpec {
        name: String::from("pad"),
        encrypt: body,
        decrypt: None,
        ports: vec![
            Port { name: String::from("data"), kind: PortKind::Data, width: 1 },
            Port { name: String::from("key"), kind: PortKind::Aux, width: 1 },
        ],
    })
}

fn gen_typed_case() -> impl Strategy<Value = TypedCase> {
    (
        2usize..=3usize,
        proptest::collection::vec((any::<u8>(), any::<u32>(), any::<u32>()), 0usize..=3usize),
        proptest::collection::vec((any::<u8>(), any::<u32>(), any::<u32>()), 0usize..=3usize),
        any::<bool>(),
        any::<bool>(),
        any::<bool>(),
    )
        .prop_map(|(n_params, ops0, ops1, wrap_param1, k0, k1)| TypedCase {
            n_params,
            ops0,
            ops1,
            wrap_param1,
            keys: vec![k0, k1],
        })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    /// Property G′: typed authoring survives lowering + movfuscation
    /// translation + unroll done-shift, and the spliced circuit preserves
    /// boundary semantics.
    #[test]
    fn prop_g_typed_translation_preserves_boundary_semantics(tc in gen_typed_case()) {
        let mut types = TypeTable::new();
        let host_pre = build_typed_host(&tc, &mut types);
        let np = tc.n_params;

        // ---- typed authoring ------------------------------------------
        let mut typed_entries = vec![TypedRegionEntry {
            anchor: TypedAnchor::Input { param: 0, start: 0, len: 1 },
            regions: std::collections::BTreeSet::from([RegionId(0)]),
        }];
        if tc.wrap_param1 {
            typed_entries.push(TypedRegionEntry {
                anchor: TypedAnchor::Input { param: 1, start: 0, len: 1 },
                regions: std::collections::BTreeSet::from([RegionId(1)]),
            });
        }
        let typed_table = TypedRegionTable {
            entries: typed_entries,
            names: std::collections::BTreeMap::new(),
        };
        let typed_lib = TypedGadgetLibrary::new().with(typed_pad(&mut types));
        let mut typed_bindings = vec![TypedGadgetBinding {
            gadget: String::from("tpad"),
            selector: RegionSelector::all_of([RegionId(0)]),
            aux_sources: vec![bit_const(tc.keys[0])],
            rng_source: None,
        }];
        if tc.wrap_param1 {
            typed_bindings.push(TypedGadgetBinding {
                gadget: String::from("tpad"),
                selector: RegionSelector::all_of([RegionId(1)]),
                aux_sources: vec![bit_const(tc.keys[1])],
                rng_source: None,
            });
        }

        // ---- movfuscation translation ----------------------------------
        let layout = movfuscate_region_layout(&host_pre, &types).expect("layout");
        prop_assert_eq!(layout.pc_width, 1);
        let typed_post = translate_regions_movfuscate(&typed_table, &layout).expect("translates");
        // Translated anchors: param k → combined param 1 + k (PC bit first).
        prop_assert!(
            match &typed_post.entries[0].anchor {
                TypedAnchor::Input { param: 1, start: 0, len: 1 } => true,
                _ => false,
            },
            "translated input anchor"
        );

        // Also exercise the invented-internal-slot regions: tag the PC bit.
        let state_regions = movfuscate_state_regions(
            &layout,
            &types,
            &std::collections::BTreeSet::from([RegionId(50)]),
            &[],
        )
        .expect("state regions");
        prop_assert_eq!(state_regions.entries.len(), 1);
        prop_assert!(
            match &state_regions.entries[0].anchor {
                TypedAnchor::Input { param: 0, start: 0, len: 1 } => true,
                _ => false,
            },
            "state-region anchor"
        );

        // ---- lower the (movfuscated) host + typed metadata --------------
        // The generated host may be statement-free; supply control provenance.
        let movfuscated = volar_ir_passes::movfuscate::movfuscate_ir_with_control_provenance(
            &host_pre,
            &mut types,
            &(),
        );
        let (bir, tables) =
            volar_ir_passes::lower_ir_to_boolar::lower_ir_to_boolar_with_tables(&movfuscated, &types);
        let bit_table_typed = lower_typed_region_table(&typed_post, &tables, &types, None)
            .expect("typed table lowers");
        let lowered_lib = lower_gadget_library(&typed_lib, &types).expect("lib lowers");
        let lowered_bindings = lower_typed_bindings(&typed_bindings, &typed_lib, &tables, &types)
            .expect("bindings lower");

        // ---- unroll with the done flag, and shift landed outputs --------
        let bir_unrolled = volar_ir_passes::lower_to_circuit::lower_to_circuit(
            &bir,
            8,
            volar_ir_passes::lower_to_circuit::LoweringMode::WithTerminationFlag,
        );
        let host_bc =
            volar_ir_passes::fuse_to_circuit::to_circuit_fused_boolar(&bir_unrolled)
                .expect("fuses");
        // done + ret; ret width = movfuscate's return-slot width (>= np for
        // this generator: every block returns np values).
        let ret_width = host_bc.outputs.len() - 1;
        prop_assert!(ret_width >= np, "ret width {ret_width} < np {np}");

        // Landed output anchors authored at pre-shift ret positions, then
        // moved past the done flag by translate_regions_termination_flag.
        let mut bit_entries = bit_table_typed.entries.clone();
        bit_entries.push(RegionEntry {
            anchor: WireAnchor::Output { start: 0, len: ret_width as u32 },
            regions: std::collections::BTreeSet::from([RegionId(2)]),
        });
        let pre_shift = RegionTable { entries: bit_entries, names: std::collections::BTreeMap::new() };
        let final_table = translate_regions_termination_flag(&pre_shift).expect("shifts");
        final_table.validate(&host_bc).expect("landed table valid");

        let mut full_lib = lowered_lib;
        full_lib.gadgets.extend(bit_pad_lib().gadgets);
        let mut full_bindings = lowered_bindings;
        full_bindings.push(GadgetBinding {
            gadget: String::from("pad"),
            selector: RegionSelector::all_of([RegionId(2)]),
            aux_sources: vec![AuxSource::Const(vec![tc.keys[0]])],
            rng_source: None,
        });

        let applied = apply_gadgets(&host_bc, &final_table, &full_bindings, &full_lib)
            .expect("applies");
        let wrapped = applied.circuit;
        prop_assert_eq!(wrapped.outputs.len(), host_bc.outputs.len());

        // ---- semantic check over all plaintext inputs -------------------
        let n_state = host_bc.params as usize; // pc bit + np state bits
        for mask in 0..(1u32 << np) {
            let pt: Vec<bool> = (0..np).map(|i| (mask >> i) & 1 == 1).collect();

            // Host reference: run the typed multi-block program.
            let host_vals = eval_ir(
                &host_pre,
                &types,
                &pt.iter().map(|&b| vec![b]).collect::<Vec<_>>(),
            )
            .expect("host terminates");
            let host_ret = bit_flatten(&host_vals);
            prop_assert_eq!(host_ret.len(), np);

            // Ciphertext boundary: wrapped input bits flipped by their keys.
            let mut cipher = vec![false; n_state];
            cipher[0] = false; // pc = 0
            for i in 0..np {
                let wrapped_bit = i == 0 || (i == 1 && tc.wrap_param1);
                let k = if i < tc.keys.len() { tc.keys[i] } else { false };
                cipher[1 + i] = pt[i] ^ (if wrapped_bit { k } else { false });
            }
            let wrapped_out = eval_plain(&wrapped, &cipher);

            // done flag must be set (straight-line program).
            prop_assert!(wrapped_out[0], "done flag for mask {mask}");
            // ret slot m in the movfuscated layout = (Return arg position m);
            // the first np ret positions mirror the host's np return values.
            for j in 0..np {
                prop_assert!(
                    wrapped_out[1 + j] == (host_ret[j] ^ tc.keys[0]),
                    "ret bit {j} mask {mask}: got {} want {}",
                    wrapped_out[1 + j] as u8,
                    (host_ret[j] ^ tc.keys[0]) as u8
                );
            }
        }
        let _ = LaneId(0);
    }
}
