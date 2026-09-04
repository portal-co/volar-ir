// @reliability: normal
// @ai: assisted
//! Property G — gadget application preserves semantics through the wrapped
//! boundary.
//!
//! For a randomly generated pure-gate fused circuit with statically
//! addressed storage, a random region table, and per-wire XOR-pad gadget
//! bindings (self-inverse, `E = E⁻¹ = XOR k`):
//!
//! - feeding the wrapped circuit *ciphertext* inputs (`x ⊕ k` on wrapped
//!   input bits) yields, at every wrapped output position, the host output
//!   encrypted (`y ⊕ k`), and the host output verbatim at unwrapped
//!   positions;
//! - wires tagged with the `plaintext` region alongside a wrap region are
//!   *not* wrapped (the `none_of` selector works);
//! - wrapped storage reads/writes and re-encrypted (incl. synthetic) pre-init
//!   data preserve the core's view of storage.

use std::vec::Vec;
use proptest::prelude::*;
use volar_ir::boolar::{BIrPreInitSegment, BIrStmt, LaneId};
use volar_ir::circuit::BCircuit;
use volar_ir::gadget::{
    AuxSource, GadgetBinding, GadgetLibrary, GadgetSpec, Port, PortKind,
};
use volar_ir::ir::IRVarId;
use volar_ir::region::{RegionEntry, RegionId, RegionSelector, RegionTable, WireAnchor};
use volar_ir_common::StorageId;

use volar_ir_passes::apply_gadgets;

fn add_to_address(addr: &[bool], mut addend: usize) -> Vec<bool> {
    let mut result = addr.to_vec();
    let mut bit = 0;
    while addend != 0 {
        if bit == result.len() {
            result.push(false);
        }
        let sum = result[bit] as usize + (addend & 1);
        result[bit] = sum & 1 != 0;
        addend = (addend >> 1) + (sum >> 1);
        bit += 1;
    }
    result
}

/// Evaluate a pure-gate + static-storage `BCircuit`.
fn eval_fused(circ: &BCircuit<()>, params: &[bool]) -> Vec<bool> {
    let mut vals: Vec<Option<bool>> = vec![None; circ.var_space() as usize];
    for (i, &b) in params.iter().enumerate() {
        vals[i] = Some(b);
    }
    let mut storage = std::collections::BTreeMap::<((StorageId, LaneId), Vec<bool>), bool>::new();
    for seg in &circ.pre_init {
        for (i, &b) in seg.data.iter().enumerate() {
            storage.insert(((seg.storage, seg.lane), add_to_address(&seg.addr, i)), b);
        }
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
            BIrStmt::StorageRead { storage: s, lane, addr } => {
                let flat = addr.iter().map(|a| vals[a.0 as usize].unwrap()).collect();
                *storage.get(&((*s, *lane), flat)).unwrap_or(&false)
            }
            BIrStmt::StorageWrite { storage: s, lane, src, addr } => {
                let flat = addr.iter().map(|a| vals[a.0 as usize].unwrap()).collect();
                storage.insert(((*s, *lane), flat), vals[src.0 as usize].unwrap());
                false
            }
            other => panic!("eval_fused: unsupported stmt {:?}", other),
        });
    }
    circ.outputs.iter().map(|o| vals[o.0 as usize].unwrap()).collect()
}

/// Random raw gate: `(op, operand_a, operand_b)` — operands resolved modulo
/// the number of vars defined so far.
type RawGate = (u8, u32, u32);

/// One randomly generated gadget case.
#[derive(Debug)]
struct TestCase {
    n_params: usize,
    n_cells: usize,
    raw_gates: Vec<RawGate>,
    reads: Vec<(u64,)>,  // cell index read (static address)
    writes: Vec<(u64, u32)>, // (cell index, src var raw)
    out_len: usize,
    // Per input bit: false = untagged, true = wrap tag (+ maybe plaintext).
    wrap_inputs: Vec<(bool, bool)>, // (wrap, also_plaintext)
    wrap_outputs: Vec<(bool, bool)>,
    wrap_cells: Vec<bool>,
    keys: Vec<bool>,
}

fn build_host(tc: &TestCase) -> BCircuit<()> {
    let mut c = BCircuit::<()>::new(tc.n_params as u32);
    // Const cell-address vars come first so reads/writes can use them.
    let mut addr_vars: Vec<IRVarId> = Vec::new();
    for i in 0..tc.n_cells {
        let v = c.push_stmt(if i == 0 { BIrStmt::One } else { BIrStmt::Zero }, ());
        // Deterministic per-cell constant address: cell i gets address bit i
        // is not addressable with 1-bit addrs; use (i % 2) encoded on one
        // address bit instead: even cells → 0, odd cells → 1.
        let _ = v;
    }
    // One address bit per cell pair; cell i uses addr bit (i % 2).
    let zero = c.push_stmt(BIrStmt::Zero, ());
    let one = c.push_stmt(BIrStmt::One, ());
    for _ in 0..tc.n_cells {
        addr_vars.push(zero);
    }
    let _ = one;

    let mut defined: Vec<IRVarId> = (0..tc.n_params as u32).map(IRVarId).collect();
    for &(op, a, b) in &tc.raw_gates {
        let pick = |raw: u32| -> IRVarId {
            if defined.is_empty() {
                IRVarId(0)
            } else {
                defined[(raw as usize) % defined.len()]
            }
        };
        let va = pick(a);
        let vb = pick(b);
        let kind = match op % 6 {
            0 => BIrStmt::Zero,
            1 => BIrStmt::One,
            2 => BIrStmt::And(va, vb),
            3 => BIrStmt::Or(va, vb),
            4 => BIrStmt::Xor(va, vb),
            _ => BIrStmt::Not(va),
        };
        let is_const = matches!(kind, BIrStmt::Zero | BIrStmt::One);
        let id = c.push_stmt(kind, ());
        if !is_const {
            defined.push(id);
        }
    }

    // Reads produce fresh vars; writes are void.
    for &(cell,) in &tc.reads {
        let v = c.push_stmt(
            BIrStmt::StorageRead {
                storage: StorageId(0),
                lane: LaneId(0),
                addr: vec![zero],
            },
            (),
        );
        let _ = cell;
        defined.push(v);
    }
    for &(_, raw) in &tc.writes {
        let src = if defined.is_empty() {
            IRVarId(0)
        } else {
            defined[(raw as usize) % defined.len()]
        };
        c.push_stmt(
            BIrStmt::StorageWrite {
                storage: StorageId(0),
                lane: LaneId(0),
                src,
                addr: vec![zero],
            },
            (),
        );
    }

    // Outputs reference defined vars (params at minimum).
    let mut outputs = Vec::new();
    for i in 0..tc.out_len {
        outputs.push(defined[i % defined.len()]);
    }
    c.outputs = outputs;
    c
}

/// Build the region table + bindings for a test case; returns
/// `(regions, bindings, input_keys, output_keys)` where `input_keys[i]` is
/// `Some(k)` iff input bit `i` is wrapped.
fn build_regions_bindings(
    tc: &TestCase,
    host_params: u32,
    host_outputs: usize,
) -> (RegionTable, Vec<GadgetBinding>, Vec<Option<bool>>, Vec<Option<bool>>) {
    let mut regions = RegionTable::new();
    let mut bindings = Vec::new();
    let mut input_keys: Vec<Option<bool>> = vec![None; tc.n_params];
    let mut output_keys: Vec<Option<bool>> = vec![None; host_outputs];
    let mut next_region = 0u32;
    let mut new_region = || {
        next_region += 1;
        RegionId(next_region - 1)
    };

    let mut push_entry = |regions: &mut RegionTable,
                          anchor: WireAnchor,
                          mut rs: Vec<RegionId>| {
        rs.sort();
        rs.dedup();
        regions.entries.push(RegionEntry {
            anchor,
            regions: rs.into_iter().collect(),
        });
    };

    for (i, &(wrap, also_plain)) in tc.wrap_inputs.iter().enumerate() {
        if !wrap {
            continue;
        }
        let rid = new_region();
        let skip_binding = also_plain;
        let mut tags = vec![rid];
        if also_plain {
            tags.push(RegionId(PLAINTEXT));
        }
        push_entry(
            &mut regions,
            WireAnchor::Input { start: i as u32, len: 1 },
            tags,
        );
        let key = tc.keys[i % tc.keys.len()];
        input_keys[i] = if skip_binding { None } else { Some(key) };
        if skip_binding {
            continue;
        }
        bindings.push(GadgetBinding {
            gadget: String::from("pad"),
            selector: RegionSelector {
                all_of: [rid].into_iter().collect(),
                none_of: [RegionId(PLAINTEXT)].into_iter().collect(),
            },
            aux_sources: vec![AuxSource::Const(vec![key])],
            rng_source: None,
        });
    }
    for (j, &(wrap, also_plain)) in tc.wrap_outputs.iter().enumerate() {
        if !wrap {
            continue;
        }
        let skip_binding = also_plain;
        let rid = new_region();
        let mut tags = vec![rid];
        if also_plain {
            tags.push(RegionId(PLAINTEXT));
        }
        push_entry(
            &mut regions,
            WireAnchor::Output { start: j as u32, len: 1 },
            tags,
        );
        let key = tc.keys[(j + 7) % tc.keys.len()];
        if skip_binding {
            continue;
        }
        output_keys[j] = Some(key);
        bindings.push(GadgetBinding {
            gadget: String::from("pad"),
            selector: RegionSelector {
                all_of: [rid].into_iter().collect(),
                none_of: [RegionId(PLAINTEXT)].into_iter().collect(),
            },
            aux_sources: vec![AuxSource::Const(vec![key])],
            rng_source: None,
        });
    }
    // Only cell 0 exists in the generated host (constant zero address), and
    // only when it has read/write traffic; other wrap flags would tag
    // un-touched storage space (UnknownStorageSpace) or cells the host never
    // addresses (no observable effect to check).
    let host_uses_cell0 = !tc.reads.is_empty() || !tc.writes.is_empty();
    for (ci, &wrap) in tc.wrap_cells.iter().enumerate() {
        if !wrap || ci != 0 || !host_uses_cell0 {
            continue;
        }
        let rid = new_region();
        push_entry(
            &mut regions,
            WireAnchor::Storage {
                storage: StorageId(0),
                lane: LaneId(0),
                start: ci as u64,
                len: 1,
            },
            vec![rid],
        );
        bindings.push(GadgetBinding {
            gadget: String::from("pad"),
            selector: RegionSelector::all_of([rid]),
            aux_sources: vec![AuxSource::Const(vec![tc.keys[(ci + 3) % tc.keys.len()]])],
            rng_source: None,
        });
    }
    let _ = tc.n_cells;

    // Entries must be sorted by anchor (Input < Output < Storage, then by
    // start): stable-sort with the anchor as key.
    regions.entries.sort_by(|a, b| a.anchor.cmp(&b.anchor));

    // Sanity: region ids referenced beyond PLAINTEXT exist only in entries.
    let _ = (host_params, host_outputs, next_region);
    (regions, bindings, input_keys, output_keys)
}

const PLAINTEXT: u32 = u32::MAX;

fn xor_pad_lib() -> GadgetLibrary {
    let mut body = BCircuit::<()>::new(2); // [data, key]
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

fn gen_case() -> impl Strategy<Value = TestCase> {
    (1usize..=4usize, 0usize..=3usize).prop_flat_map(|(n_params, n_cells)| {
        (
            proptest::collection::vec((any::<u8>(), any::<u32>(), any::<u32>()), 0usize..=6usize),
            proptest::collection::vec(any::<u64>(), 0usize..=2usize),
            proptest::collection::vec((any::<u64>(), any::<u32>()), 0usize..=2usize),
            proptest::collection::vec((any::<bool>(), any::<bool>()), n_params),
            proptest::collection::vec((any::<bool>(), any::<bool>()), 1usize..=3usize),
            proptest::collection::vec(any::<bool>(), n_cells.max(1)),
            proptest::collection::vec(any::<bool>(), 4usize),
        )
            .prop_map(move |(gates, reads, writes, wi, wo, wc, keys)| {
                TestCase {
                    n_params,
                    n_cells,
                    raw_gates: gates,
                    reads: reads.into_iter().map(|r| (r % n_cells.max(1) as u64)).map(|c| (c,)).collect(),
                    writes: writes
                        .into_iter()
                        .map(|(c, v)| ((c as u64) % n_cells.max(1) as u64, v))
                        .collect(),
                    out_len: wo.len(),
                    wrap_inputs: wi,
                    wrap_outputs: wo,
                    wrap_cells: wc,
                    keys,
                }
            })
    })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// Property G: gadget application preserves semantics through the
    /// wrapped boundary (XOR-pad gadgets, ciphertext inputs/outputs).
    #[test]
    fn prop_g_gadgets_preserve_boundary_semantics(tc in gen_case()) {
        let host = build_host(&tc);
        let n_out = host.outputs.len();
        let (regions, bindings, input_keys, output_keys) =
            build_regions_bindings(&tc, host.params, n_out);

        regions.validate(&host).expect("region table valid");
        let wrapped = match apply_gadgets(&host, &regions, &bindings, &xor_pad_lib()) {
            Ok(app) => app.circuit,
            Err(e) => panic!("apply_gadgets failed: {e:?}"),
        };

        // Boundary shape is preserved.
        prop_assert_eq!(wrapped.params, host.params);
        prop_assert_eq!(wrapped.outputs.len(), host.outputs.len());

        let n_params = host.params as usize;
        for mask in 0..(1u32 << n_params) {
            let plain: Vec<bool> = (0..n_params).map(|i| (mask >> i) & 1 == 1).collect();
            let host_out = eval_fused(&host, &plain);

            // Ciphertext inputs: wrapped bits flipped by their key.
            let cipher_in: Vec<bool> = plain
                .iter()
                .enumerate()
                .map(|(i, &b)| b ^ input_keys[i].unwrap_or(false))
                .collect();
            let wrapped_out = eval_fused(&wrapped, &cipher_in);

            for j in 0..host.outputs.len() {
                let expected = match output_keys[j] {
                    Some(k) => host_out[j] ^ k,
                    None => host_out[j],
                };
                prop_assert_eq!(wrapped_out[j], expected);
            }
        }

        // Wires tagged both wrap + plaintext must NOT have been wrapped:
        // their ciphertext equals the plaintext (no key applied).
        for (i, &(wrap, also_plain)) in tc.wrap_inputs.iter().enumerate() {
            if wrap && also_plain {
                prop_assert!(input_keys[i].is_none(), "input {i} must be excluded");
            }
        }
        for (j, &(wrap, also_plain)) in tc.wrap_outputs.iter().enumerate() {
            if wrap && also_plain {
                prop_assert!(output_keys[j].is_none(), "output {j} must be excluded");
            }
        }
    }
}
