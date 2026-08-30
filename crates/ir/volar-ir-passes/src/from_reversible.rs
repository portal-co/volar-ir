//! Lower a reversible [`RCircuit`] into the ordinary SSA [`BCircuit`] form,
//! and reshape a `BCircuit`'s public input/output interface.
//!
//! [`to_boolar_circuit`] exposes the complete reversible state transition:
//! wire `i` is both input parameter `i` and output `i`.  Callers that want a
//! conventional projection can then specialize known input wires with
//! [`hardcode_circuit_inputs`] and discard unwanted observable wires with
//! [`remove_circuit_outputs`].  Keeping those operations here puts the
//! mutable-wire-to-SSA bookkeeping behind one small interface.

use alloc::collections::BTreeSet;
use alloc::vec;
use alloc::vec::Vec;
use volar_ir::boolar::BIrStmt;
use volar_ir::circuit::BCircuit;
use volar_ir::ir::IRVarId;
use volar_ir::rcircuit::{RCircuit, RCircuitError, RExternalKind, RGate};
use volar_ir_common::Node;

/// Why a reversible-to-Boolar conversion or circuit-interface rewrite failed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CircuitTransformError {
    /// The source circuit failed its own gate/index validation.
    InvalidRCircuit(RCircuitError),
    /// `BCircuit` variable identifiers are `u32`, but the reversible circuit
    /// has more input wires than that representation can address.
    WireCountOutOfRange { num_wires: usize },
    /// Emitting the lowered SSA program would exceed the Boolar var-id space.
    VarSpaceExhausted,
    /// A future `RGate` variant has no established Boolar lowering yet.
    UnsupportedGate { gate: usize },
    /// `ExternalStorageXor.bit + 1` overflowed while constructing the legacy
    /// `ActionCall` result vector used to express the requested bit.
    ActionBitIndexOverflow { gate: usize },
    /// An input-position list named the same source input twice.
    DuplicateInput { input: usize },
    /// An input-position list named no parameter of the source circuit.
    InputOutOfRange { input: usize, params: u32 },
    /// An output-position list named the same source output twice.
    DuplicateOutput { output: usize },
    /// An output-position list named no output of the source circuit.
    OutputOutOfRange { output: usize, outputs: usize },
    /// A statement or result referenced a value which has not been remapped.
    /// This catches malformed and use-before-definition `BCircuit` inputs.
    UnknownValue { var: u32, known_values: usize },
}

impl core::fmt::Display for CircuitTransformError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CircuitTransformError::InvalidRCircuit(err) => {
                write!(f, "invalid reversible circuit: {err}")
            }
            CircuitTransformError::WireCountOutOfRange { num_wires } => write!(
                f,
                "reversible circuit has {num_wires} wires, exceeding Boolar's u32 var space"
            ),
            CircuitTransformError::VarSpaceExhausted => {
                write!(f, "lowered circuit exceeds Boolar's u32 var space")
            }
            CircuitTransformError::UnsupportedGate { gate } => {
                write!(f, "reversible gate {gate} has no Boolar lowering")
            }
            CircuitTransformError::ActionBitIndexOverflow { gate } => {
                write!(
                    f,
                    "reversible action-storage gate {gate} has an oversized bit index"
                )
            }
            CircuitTransformError::DuplicateInput { input } => {
                write!(f, "input {input} was hardcoded more than once")
            }
            CircuitTransformError::InputOutOfRange { input, params } => {
                write!(
                    f,
                    "input {input} out of range for circuit with {params} parameters"
                )
            }
            CircuitTransformError::DuplicateOutput { output } => {
                write!(f, "output {output} was removed more than once")
            }
            CircuitTransformError::OutputOutOfRange { output, outputs } => {
                write!(
                    f,
                    "output {output} out of range for circuit with {outputs} outputs"
                )
            }
            CircuitTransformError::UnknownValue { var, known_values } => write!(
                f,
                "value {var} is unavailable while remapping {known_values} known values"
            ),
        }
    }
}

/// Convert a reversible circuit into a full-state ordinary Boolar circuit.
///
/// The result has `source.num_wires` inputs and the same number of outputs.
/// Both are ordered by reversible wire index.  Each output is the final value
/// of its corresponding input wire after applying every reversible gate.
///
/// `ExternalStorageXor` is represented through Boolar's legacy
/// `ActionCall`/`ActionBit` pair.  That form retains the action name, guard,
/// arguments, fallback, and result bit, but has no explicit `occurrence`
/// field; its replay identity therefore follows normal Boolar action order.
pub fn to_boolar_circuit(source: &RCircuit) -> Result<BCircuit, CircuitTransformError> {
    source
        .validate()
        .map_err(CircuitTransformError::InvalidRCircuit)?;
    let params = u32::try_from(source.num_wires).map_err(|_| {
        CircuitTransformError::WireCountOutOfRange {
            num_wires: source.num_wires,
        }
    })?;
    let mut builder = BoolarBuilder::new(params);

    for (gate_idx, gate) in source.gates().iter().enumerate() {
        match gate {
            RGate::X(target) => {
                let next = builder.emit(BIrStmt::Not(builder.wire(*target)))?;
                builder.set_wire(*target, next);
            }
            RGate::Cnot { ctrl, target } => {
                let next =
                    builder.emit(BIrStmt::Xor(builder.wire(*ctrl), builder.wire(*target)))?;
                builder.set_wire(*target, next);
            }
            RGate::Ccnot { c1, c2, target } => {
                let product = builder.emit(BIrStmt::And(builder.wire(*c1), builder.wire(*c2)))?;
                let next = builder.emit(BIrStmt::Xor(builder.wire(*target), product))?;
                builder.set_wire(*target, next);
            }
            RGate::XorLut2 {
                controls,
                target,
                table,
            } => lower_lut2_xor(&mut builder, controls, *target, *table)?,
            RGate::StorageSwap {
                storage,
                lane,
                addr,
                target,
            } => {
                let addr = builder.wires(addr);
                let target_before = builder.wire(*target);
                let stored = builder.emit(BIrStmt::StorageRead {
                    storage: *storage,
                    lane: *lane,
                    addr: addr.clone(),
                })?;
                builder.emit(BIrStmt::StorageWrite {
                    storage: *storage,
                    lane: *lane,
                    src: target_before,
                    addr,
                })?;
                builder.set_wire(*target, stored);
            }
            RGate::ExternalXor {
                kind,
                name,
                args,
                target,
                bit,
                occurrence,
            } => {
                let source = match kind {
                    RExternalKind::Oracle => builder.emit(BIrStmt::OracleBit {
                        name: name.clone(),
                        args: builder.wires(args),
                        bit: *bit,
                        occurrence: *occurrence,
                    })?,
                    RExternalKind::Rng => builder.emit(BIrStmt::RngBit {
                        name: name.clone(),
                        bit: *bit,
                        occurrence: *occurrence,
                    })?,
                };
                let next = builder.emit(BIrStmt::Xor(builder.wire(*target), source))?;
                builder.set_wire(*target, next);
            }
            RGate::ExternalStorageXor {
                name,
                guard,
                args,
                fallback,
                storage,
                lane,
                addr,
                bit,
                ..
            } => {
                let result_bits = bit
                    .checked_add(1)
                    .ok_or(CircuitTransformError::ActionBitIndexOverflow { gate: gate_idx })?;
                let addr = builder.wires(addr);
                let stored = builder.emit(BIrStmt::StorageRead {
                    storage: *storage,
                    lane: *lane,
                    addr: addr.clone(),
                })?;
                let action = builder.emit(BIrStmt::ActionCall {
                    name: name.clone(),
                    guard: builder.wire(*guard),
                    args: builder.wires(args),
                    fallback: vec![builder.wire(*fallback); result_bits],
                    num_bits: result_bits,
                })?;
                let action_bit = builder.emit(BIrStmt::ActionBit {
                    call: action,
                    bit: *bit,
                })?;
                let next = builder.emit(BIrStmt::Xor(stored, action_bit))?;
                builder.emit(BIrStmt::StorageWrite {
                    storage: *storage,
                    lane: *lane,
                    src: next,
                    addr,
                })?;
            }
            _ => return Err(CircuitTransformError::UnsupportedGate { gate: gate_idx }),
        }
    }

    Ok(builder.finish())
}

/// Hardcode selected input parameters and remove them from a Boolar circuit's
/// public input interface.
///
/// Input positions are interpreted in the original circuit.  Remaining
/// parameters retain their relative order.  New constant statements receive
/// clones of `constant_provenance`; callers must provide an existing
/// provenance value rather than this pass inventing one.
pub fn hardcode_circuit_inputs<P: Clone>(
    circuit: BCircuit<P>,
    hardcoded: &[(usize, bool)],
    constant_provenance: P,
) -> Result<BCircuit<P>, CircuitTransformError> {
    let mut fixed = vec![None; circuit.params as usize];
    for &(input, value) in hardcoded {
        let Some(slot) = fixed.get_mut(input) else {
            return Err(CircuitTransformError::InputOutOfRange {
                input,
                params: circuit.params,
            });
        };
        if slot.replace(value).is_some() {
            return Err(CircuitTransformError::DuplicateInput { input });
        }
    }

    let BCircuit {
        params: _,
        stmts,
        pre_init,
        outputs,
    } = circuit;
    let remaining_params = fixed.iter().filter(|value| value.is_none()).count();
    let params =
        u32::try_from(remaining_params).map_err(|_| CircuitTransformError::VarSpaceExhausted)?;
    let mut result = BCircuit {
        params,
        stmts: Vec::with_capacity(stmts.len() + hardcoded.len()),
        pre_init,
        outputs: Vec::with_capacity(outputs.len()),
    };
    let mut remap = Vec::with_capacity(fixed.len() + stmts.len());
    let mut next_param = 0u32;

    for value in fixed {
        let mapped = match value {
            Some(true) => result.push_stmt(BIrStmt::One, constant_provenance.clone()),
            Some(false) => result.push_stmt(BIrStmt::Zero, constant_provenance.clone()),
            None => {
                let param = IRVarId(next_param);
                next_param += 1;
                param
            }
        };
        remap.push(mapped);
    }

    for node in stmts {
        let kind = node.kind.map(
            &mut (),
            |_, value| lookup_remap(&remap, value),
            |_, storage| Ok::<_, CircuitTransformError>(storage),
        )?;
        if result.var_space() == u32::MAX {
            return Err(CircuitTransformError::VarSpaceExhausted);
        }
        let id = IRVarId(result.var_space());
        result.stmts.push(Node {
            kind,
            prov: node.prov,
            side: node.side,
        });
        remap.push(id);
    }
    result.outputs = outputs
        .into_iter()
        .map(|value| lookup_remap(&remap, value))
        .collect::<Result<_, _>>()?;
    Ok(result)
}

/// Remove selected output positions from a Boolar circuit.
///
/// Positions are interpreted in the original output list.  The remaining
/// outputs preserve their original order; statements, storage effects, and
/// pre-initialized storage are intentionally left untouched.
pub fn remove_circuit_outputs<P: Clone>(
    mut circuit: BCircuit<P>,
    removed: &[usize],
) -> Result<BCircuit<P>, CircuitTransformError> {
    let mut positions = BTreeSet::new();
    for &output in removed {
        if output >= circuit.outputs.len() {
            return Err(CircuitTransformError::OutputOutOfRange {
                output,
                outputs: circuit.outputs.len(),
            });
        }
        if !positions.insert(output) {
            return Err(CircuitTransformError::DuplicateOutput { output });
        }
    }
    circuit.outputs = circuit
        .outputs
        .into_iter()
        .enumerate()
        .filter_map(|(index, value)| (!positions.contains(&index)).then_some(value))
        .collect();
    Ok(circuit)
}

struct BoolarBuilder {
    circuit: BCircuit,
    wires: Vec<IRVarId>,
}

impl BoolarBuilder {
    fn new(params: u32) -> Self {
        BoolarBuilder {
            circuit: BCircuit::new(params),
            wires: (0..params).map(IRVarId).collect(),
        }
    }

    fn emit(&mut self, kind: BIrStmt) -> Result<IRVarId, CircuitTransformError> {
        checked_push_stmt(&mut self.circuit, kind, ())
    }

    fn wire(&self, wire: usize) -> IRVarId {
        self.wires[wire]
    }

    fn wires(&self, wires: &[usize]) -> Vec<IRVarId> {
        wires.iter().map(|&wire| self.wire(wire)).collect()
    }

    fn set_wire(&mut self, wire: usize, value: IRVarId) {
        self.wires[wire] = value;
    }

    fn finish(mut self) -> BCircuit {
        self.circuit.outputs = self.wires;
        self.circuit
    }
}

fn checked_push_stmt<P: Clone>(
    circuit: &mut BCircuit<P>,
    kind: BIrStmt,
    prov: P,
) -> Result<IRVarId, CircuitTransformError> {
    if circuit.var_space() == u32::MAX {
        return Err(CircuitTransformError::VarSpaceExhausted);
    }
    Ok(circuit.push_stmt(kind, prov))
}

fn lookup_remap(remap: &[IRVarId], value: IRVarId) -> Result<IRVarId, CircuitTransformError> {
    remap
        .get(value.0 as usize)
        .copied()
        .ok_or(CircuitTransformError::UnknownValue {
            var: value.0,
            known_values: remap.len(),
        })
}

/// Lower `target ^= lut(control0, control1)` using the truth table's
/// algebraic normal form.  The table bit order is the RCircuit display order:
/// `00, 01, 10, 11` at bits `3, 2, 1, 0` respectively.
fn lower_lut2_xor(
    builder: &mut BoolarBuilder,
    controls: &[usize; 2],
    target: usize,
    table: u8,
) -> Result<(), CircuitTransformError> {
    let t00 = (table >> 3) & 1 != 0;
    let t01 = (table >> 2) & 1 != 0;
    let t10 = (table >> 1) & 1 != 0;
    let t11 = table & 1 != 0;
    let control0 = builder.wire(controls[0]);
    let control1 = builder.wire(controls[1]);
    let mut value = None;
    if t00 {
        let one = builder.emit(BIrStmt::One)?;
        append_xor_term(builder, &mut value, one)?;
    }
    if t00 ^ t10 {
        append_xor_term(builder, &mut value, control0)?;
    }
    if t00 ^ t01 {
        append_xor_term(builder, &mut value, control1)?;
    }
    if t00 ^ t01 ^ t10 ^ t11 {
        let product = builder.emit(BIrStmt::And(control0, control1))?;
        append_xor_term(builder, &mut value, product)?;
    }
    if let Some(value) = value {
        let next = builder.emit(BIrStmt::Xor(builder.wire(target), value))?;
        builder.set_wire(target, next);
    }
    Ok(())
}

fn append_xor_term(
    builder: &mut BoolarBuilder,
    accumulator: &mut Option<IRVarId>,
    term: IRVarId,
) -> Result<(), CircuitTransformError> {
    *accumulator = Some(match *accumulator {
        Some(value) => builder.emit(BIrStmt::Xor(value, term))?,
        None => term,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;
    use volar_ir::boolar::LaneId;
    use volar_ir::rcircuit::{ReversibleExternalRegistry, StorageState};
    use volar_ir_common::StorageId;

    fn eval_pure<P: Clone>(circuit: &BCircuit<P>, inputs: &[bool]) -> Vec<bool> {
        let mut values = vec![false; circuit.var_space() as usize];
        values[..inputs.len()].copy_from_slice(inputs);
        for (index, node) in circuit.stmts.iter().enumerate() {
            let value = match &node.kind {
                BIrStmt::Zero => false,
                BIrStmt::One => true,
                BIrStmt::And(a, b) => values[a.0 as usize] & values[b.0 as usize],
                BIrStmt::Or(a, b) => values[a.0 as usize] | values[b.0 as usize],
                BIrStmt::Xor(a, b) => values[a.0 as usize] ^ values[b.0 as usize],
                BIrStmt::Not(a) => !values[a.0 as usize],
                _ => panic!("test evaluator: unexpected non-pure Boolar statement"),
            };
            values[circuit.params as usize + index] = value;
        }
        circuit
            .outputs
            .iter()
            .map(|value| values[value.0 as usize])
            .collect()
    }

    fn eval_with_effects<R: ReversibleExternalRegistry>(
        circuit: &BCircuit,
        inputs: &[bool],
        storage: &mut StorageState,
        externals: &mut R,
    ) -> Vec<bool> {
        let mut values = vec![false; circuit.var_space() as usize];
        let mut action_results = BTreeMap::new();
        values[..inputs.len()].copy_from_slice(inputs);
        for (index, node) in circuit.stmts.iter().enumerate() {
            let value = match &node.kind {
                BIrStmt::Zero => false,
                BIrStmt::One => true,
                BIrStmt::And(a, b) => values[a.0 as usize] & values[b.0 as usize],
                BIrStmt::Or(a, b) => values[a.0 as usize] | values[b.0 as usize],
                BIrStmt::Xor(a, b) => values[a.0 as usize] ^ values[b.0 as usize],
                BIrStmt::Not(a) => !values[a.0 as usize],
                BIrStmt::OracleBit {
                    name,
                    args,
                    bit,
                    occurrence,
                } => externals.oracle_bit(
                    name,
                    &args
                        .iter()
                        .map(|v| values[v.0 as usize])
                        .collect::<Vec<_>>(),
                    *bit,
                    *occurrence,
                ),
                BIrStmt::RngBit {
                    name,
                    bit,
                    occurrence,
                } => externals.rng_bit(name, *bit, *occurrence),
                BIrStmt::ActionCall {
                    name,
                    guard,
                    args,
                    fallback,
                    num_bits,
                } => {
                    let bits = if values[guard.0 as usize] {
                        let args = args
                            .iter()
                            .map(|v| values[v.0 as usize])
                            .collect::<Vec<_>>();
                        (0..*num_bits)
                            .map(|bit| externals.action_bit(name, &args, bit, 0))
                            .collect()
                    } else {
                        fallback.iter().map(|v| values[v.0 as usize]).collect()
                    };
                    action_results.insert(circuit.params as usize + index, bits);
                    false
                }
                BIrStmt::ActionBit { call, bit } => action_results
                    .get(&(call.0 as usize))
                    .and_then(|bits: &Vec<bool>| bits.get(*bit))
                    .copied()
                    .expect("test evaluator: missing action result bit"),
                BIrStmt::StorageRead {
                    storage: store,
                    lane,
                    addr,
                } => storage
                    .get(&((*store, *lane), address(addr, &values)))
                    .copied()
                    .unwrap_or(false),
                BIrStmt::StorageWrite {
                    storage: store,
                    lane,
                    src,
                    addr,
                } => {
                    storage.insert(
                        ((*store, *lane), address(addr, &values)),
                        values[src.0 as usize],
                    );
                    false
                }
                _ => panic!("test evaluator: unexpected Boolar statement"),
            };
            values[circuit.params as usize + index] = value;
        }
        circuit
            .outputs
            .iter()
            .map(|value| values[value.0 as usize])
            .collect()
    }

    fn address(addr: &[IRVarId], values: &[bool]) -> u64 {
        addr.iter().enumerate().fold(0, |result, (bit, value)| {
            result | ((values[value.0 as usize] as u64) << bit)
        })
    }

    fn assert_pure_equivalent(source: RCircuit) {
        let lowered = to_boolar_circuit(&source).expect("lowers");
        for mask in 0..(1usize << source.num_wires) {
            let input = (0..source.num_wires)
                .map(|bit| (mask >> bit) & 1 == 1)
                .collect::<Vec<_>>();
            let mut expected = input.clone();
            source.apply_pure(&mut expected).expect("pure source");
            assert_eq!(eval_pure(&lowered, &input), expected, "input={input:?}");
        }
    }

    #[test]
    fn lowers_pure_gates_and_all_lut_tables() {
        assert_pure_equivalent(
            RCircuit::new(
                3,
                vec![
                    RGate::X(0),
                    RGate::Cnot { ctrl: 0, target: 1 },
                    RGate::Ccnot {
                        c1: 0,
                        c2: 1,
                        target: 2,
                    },
                ],
            )
            .unwrap(),
        );
        for table in 0..16 {
            assert_pure_equivalent(
                RCircuit::new(
                    3,
                    vec![RGate::XorLut2 {
                        controls: [0, 1],
                        target: 2,
                        table,
                    }],
                )
                .unwrap(),
            );
        }
    }

    #[derive(Default)]
    struct TestSources;

    impl ReversibleExternalRegistry for TestSources {
        fn oracle_bit(&mut self, _: &str, args: &[bool], bit: usize, occurrence: u64) -> bool {
            args.iter().copied().fold(false, |acc, value| acc ^ value)
                ^ ((bit as u64 ^ occurrence) & 1 != 0)
        }

        fn rng_bit(&mut self, _: &str, bit: usize, occurrence: u64) -> bool {
            (bit as u64 ^ occurrence) & 1 != 0
        }

        fn action_bit(&mut self, _: &str, args: &[bool], bit: usize, _: u64) -> bool {
            args[bit % args.len()]
        }
    }

    #[test]
    fn lowers_storage_and_replayable_external_gates() {
        let storage = StorageId::DEFAULT;
        let lane = LaneId(0);
        let source = RCircuit::new(
            4,
            vec![
                RGate::StorageSwap {
                    storage,
                    lane,
                    addr: vec![0],
                    target: 1,
                },
                RGate::ExternalXor {
                    kind: RExternalKind::Oracle,
                    name: "oracle".into(),
                    args: vec![0, 1],
                    target: 2,
                    bit: 3,
                    occurrence: 7,
                },
                RGate::ExternalXor {
                    kind: RExternalKind::Rng,
                    name: "rng".into(),
                    args: vec![],
                    target: 3,
                    bit: 2,
                    occurrence: 6,
                },
                RGate::ExternalStorageXor {
                    name: "action".into(),
                    guard: 0,
                    args: vec![1, 2],
                    fallback: 3,
                    storage,
                    lane,
                    addr: vec![1],
                    bit: 1,
                    occurrence: 99,
                },
            ],
        )
        .unwrap();
        let lowered = to_boolar_circuit(&source).expect("lowers");
        for mask in 0..16 {
            let input = (0..4).map(|bit| (mask >> bit) & 1 == 1).collect::<Vec<_>>();
            let mut source_storage = StorageState::new();
            source_storage.insert(((storage, lane), 0), mask & 1 != 0);
            source_storage.insert(((storage, lane), 1), mask & 2 != 0);
            let mut lowered_storage = source_storage.clone();
            let mut source_wires = input.clone();
            source.apply_with_externals(&mut source_wires, &mut source_storage, &mut TestSources);
            let lowered_outputs =
                eval_with_effects(&lowered, &input, &mut lowered_storage, &mut TestSources);
            assert_eq!(lowered_outputs, source_wires, "input={input:?}");
            assert_eq!(
                logical_storage(&lowered_storage),
                logical_storage(&source_storage),
                "input={input:?}"
            );
        }
    }

    /// Storage maps treat absence as false. `ExternalStorageXor` canonicalizes
    /// cleared cells by removing them, while ordinary Boolar stores retain an
    /// explicit false entry, so compare their logical states here.
    fn logical_storage(storage: &StorageState) -> StorageState {
        storage
            .iter()
            .filter_map(|(key, value)| value.then_some((*key, true)))
            .collect()
    }

    #[test]
    fn hardcodes_inputs_and_removes_outputs_without_reordering_survivors() {
        let mut source = BCircuit::new(2);
        let xor = source.push_stmt(BIrStmt::Xor(IRVarId(0), IRVarId(1)), 7u8);
        source.outputs = vec![IRVarId(0), xor, IRVarId(1)];
        let specialized = hardcode_circuit_inputs(source, &[(1, true)], 99).unwrap();
        assert_eq!(specialized.params, 1);
        assert_eq!(specialized.stmts[0].kind, BIrStmt::One);
        assert_eq!(specialized.stmts[0].prov, 99);
        assert_eq!(specialized.stmts[1].prov, 7);
        assert_eq!(eval_pure(&specialized, &[false]), vec![false, true, true]);
        assert_eq!(eval_pure(&specialized, &[true]), vec![true, false, true]);

        let projected = remove_circuit_outputs(specialized, &[1]).unwrap();
        assert_eq!(eval_pure(&projected, &[false]), vec![false, true]);
        assert_eq!(eval_pure(&projected, &[true]), vec![true, true]);
    }

    #[test]
    fn interface_utilities_fail_closed() {
        let source = BCircuit::<()>::new(1);
        assert_eq!(
            hardcode_circuit_inputs(source.clone(), &[(1, false)], ()),
            Err(CircuitTransformError::InputOutOfRange {
                input: 1,
                params: 1
            })
        );
        assert_eq!(
            hardcode_circuit_inputs(source.clone(), &[(0, false), (0, true)], ()),
            Err(CircuitTransformError::DuplicateInput { input: 0 })
        );
        let mut one_output = BCircuit::<()>::new(1);
        one_output.outputs = vec![IRVarId(0)];
        assert_eq!(
            remove_circuit_outputs(one_output, &[0, 0]),
            Err(CircuitTransformError::DuplicateOutput { output: 0 })
        );
        assert_eq!(
            remove_circuit_outputs(source, &[0]),
            Err(CircuitTransformError::OutputOutOfRange {
                output: 0,
                outputs: 0,
            })
        );
    }

    #[test]
    fn hardcoding_all_inputs_produces_a_parameter_free_circuit() {
        let mut source = BCircuit::new(2);
        source.outputs = vec![IRVarId(0), IRVarId(1)];
        let specialized = hardcode_circuit_inputs(source, &[(0, false), (1, true)], ()).unwrap();
        assert_eq!(specialized.params, 0);
        assert_eq!(eval_pure(&specialized, &[]), vec![false, true]);
    }

    #[test]
    fn reversible_round_trip_can_specialize_and_project_the_output_register() {
        let mut source = BCircuit::new(2);
        let output = source.push_stmt(BIrStmt::And(IRVarId(0), IRVarId(1)), ());
        source.outputs = vec![output];
        let (reversible, map) = crate::to_reversible::to_reversible(&source).unwrap();
        let normal = to_boolar_circuit(&reversible).unwrap();
        let fixed = ((source.params as usize)..reversible.num_wires)
            .map(|wire| (wire, false))
            .collect::<Vec<_>>();
        let specialized = hardcode_circuit_inputs(normal, &fixed, ()).unwrap();
        let removed = (0..reversible.num_wires)
            .filter(|wire| *wire != map.y_base())
            .collect::<Vec<_>>();
        let projected = remove_circuit_outputs(specialized, &removed).unwrap();
        assert_eq!(eval_pure(&projected, &[false, false]), vec![false]);
        assert_eq!(eval_pure(&projected, &[false, true]), vec![false]);
        assert_eq!(eval_pure(&projected, &[true, false]), vec![false]);
        assert_eq!(eval_pure(&projected, &[true, true]), vec![true]);
    }
}
