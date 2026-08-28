// @reliability: normal
//! @ai: assisted
// Reversible circuit-fused Boolar IR (`RCircuit`): a gate-level representation
// of *reversible* circuits — bijective boolean maps over a fixed wire vector —
// using the X / CNOT / Toffoli basis, a two-input target-XOR lookup gate,
// plus a reversible storage-exchange gate.
//
// Unlike `BCircuit` (SSA values), gates here mutate wires in place; every v1
// gate is an involution, so circuit inversion is just reversing the gate list.
// Pure data structure definitions and evaluation helpers; no cryptographic
// claims.

use crate::boolar::LaneId;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use volar_ir_common::StorageId;

mod generated;
pub use generated::{RCircuit, RExternalKind};

/// A single reversible gate. Every variant is a bijection on the joint
/// (wires, storage) state, and an involution (applying it twice is identity).
///
/// Indices refer to positions in `0..num_wires` of the enclosing [`RCircuit`].
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
#[non_exhaustive]
pub enum RGate {
    /// Pauli-X (NOT) on one wire.
    X(usize),
    /// Controlled-NOT: `target ^= ctrl`.
    Cnot { ctrl: usize, target: usize },
    /// Toffoli (CCNOT): `target ^= c1 & c2`.
    Ccnot { c1: usize, c2: usize, target: usize },
    /// Reversible storage exchange: atomically SWAP the target wire with the
    /// bit stored at `((storage, lane), addr)`.
    ///
    /// This is the reversible analogue of Boolar's `StorageRead`/`StorageWrite`:
    /// because it *exchanges* rather than copies, the joint map over (wires,
    /// storage contents) is a bijection regardless of what the cell held.
    /// Reading is destructive-but-restorable by applying the same gate again.
    ///
    /// `addr` is a list of wire indices forming the address bit-vector, LSB
    /// first (bit 0 = index 0 = least-significant), matching
    /// `BIrStmt::StorageRead`'s convention (including any appended bit-index
    /// bits produced by lowering).
    StorageSwap {
        storage: StorageId,
        lane: LaneId,
        addr: Vec<usize>,
        target: usize,
    },
    /// Two-input lookup gate: `target ^= lut(control0, control1)`.
    ///
    /// `table` is a four-bit truth table in conventional display order:
    /// bit 3 is input `00`, bit 2 is `01`, bit 1 is `10`, and bit 0 is
    /// `11`. The upper four bits must be zero. Requiring two distinct
    /// controls and a distinct target gives consumers a canonical atomic
    /// representation for arbitrary two-control reversible gates.
    ///
    /// This variant is appended after the original variants to preserve their
    /// derived archive discriminants.
    XorLut2 {
        controls: [usize; 2],
        target: usize,
        table: u8,
    },
    /// Reversibly inject one deterministic external source bit:
    /// `target ^= source(args, bit, occurrence)`.
    ///
    /// Re-applying the gate with the same replayable source registry undoes
    /// it, so this is the reversible form of direct oracle/RNG bit lowering.
    ExternalXor {
        kind: RExternalKind,
        name: String,
        args: Vec<usize>,
        target: usize,
        bit: usize,
        occurrence: u64,
    },
    /// Reversibly inject a guarded action result into storage:
    /// `storage[address] ^= guard ? action(args, bit, occurrence) : fallback`.
    ///
    /// This intentionally uses XOR rather than the ordinary direct-store
    /// action semantics. That makes the joint wire/storage transition an
    /// involution and lets the inverse replay the same occurrence token.
    ExternalStorageXor {
        name: String,
        guard: usize,
        args: Vec<usize>,
        fallback: usize,
        storage: StorageId,
        lane: LaneId,
        addr: Vec<usize>,
        bit: usize,
        occurrence: u64,
    },
}

/// Replayable external source dispatch for reversible circuits.
///
/// Implementations must return the same bit whenever they are invoked with
/// the same name, flattened inputs, bit index, and occurrence token. In
/// particular, an RNG implementation must derive its result from a replay
/// transcript or seeded PRF rather than sampling fresh entropy on inverse.
pub trait ReversibleExternalRegistry {
    fn oracle_bit(&mut self, name: &str, args: &[bool], bit: usize, occurrence: u64) -> bool;
    fn rng_bit(&mut self, name: &str, bit: usize, occurrence: u64) -> bool;
    fn action_bit(&mut self, name: &str, args: &[bool], bit: usize, occurrence: u64) -> bool;
}

/// Why an [`RCircuit`] could not be constructed (gate validation failed).
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RCircuitError {
    /// A gate referenced a wire index outside `0..num_wires`.
    IndexOutOfRange {
        /// Zero-based position of the offending gate in the gate list.
        gate: usize,
        wire: usize,
    },
    /// A control/address wire was aliased with the gate's target wire.
    TargetAliased {
        /// Zero-based position of the offending gate in the gate list.
        gate: usize,
        wire: usize,
    },
    /// The two controls of an [`RGate::XorLut2`] gate were the same wire.
    ControlAliased {
        /// Zero-based position of the offending gate in the gate list.
        gate: usize,
        wire: usize,
    },
    /// An [`RGate::XorLut2`] table used bits outside its four-bit encoding.
    InvalidTruthTable {
        /// Zero-based position of the offending gate in the gate list.
        gate: usize,
        table: u8,
    },
    /// A `StorageSwap` address exceeded 64 address bits (the storage-state
    /// model indexes cells by a `u64` cell index).
    AddressTooWide {
        /// Zero-based position of the offending gate in the gate list.
        gate: usize,
        width: usize,
    },
    /// Wire-count mismatch when composing two circuits.
    WireCountMismatch { lhs: usize, rhs: usize },
    /// [`RCircuit::apply_pure`] was called on a circuit that references
    /// storage; use [`RCircuit::apply`] with a [`StorageState`] instead.
    StorageOpsUnsupported,
    /// [`RCircuit::apply`] or [`RCircuit::apply_pure`] was called on a
    /// circuit with external gates. Use [`RCircuit::apply_with_externals`].
    ExternalOpsUnsupported,
}

impl core::fmt::Display for RCircuitError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            RCircuitError::IndexOutOfRange { gate, wire } => {
                write!(f, "gate {gate} references wire {wire} out of range")
            }
            RCircuitError::TargetAliased { gate, wire } => {
                write!(
                    f,
                    "gate {gate} aliases control/address wire {wire} with its target"
                )
            }
            RCircuitError::ControlAliased { gate, wire } => {
                write!(f, "gate {gate} uses wire {wire} for both lookup controls")
            }
            RCircuitError::InvalidTruthTable { gate, table } => {
                write!(
                    f,
                    "gate {gate} has invalid two-input truth table {table:#04x}"
                )
            }
            RCircuitError::AddressTooWide { gate, width } => {
                write!(f, "gate {gate} has a {width}-bit storage address (max 64)")
            }
            RCircuitError::WireCountMismatch { lhs, rhs } => {
                write!(f, "cannot compose circuits with {lhs} and {rhs} wires")
            }
            RCircuitError::StorageOpsUnsupported => {
                write!(
                    f,
                    "circuit references storage; use `apply` with a StorageState"
                )
            }
            RCircuitError::ExternalOpsUnsupported => {
                write!(
                    f,
                    "circuit references external sources; use `apply_with_externals`"
                )
            }
        }
    }
}

/// Simple storage model for evaluating [`RCircuit`]s containing
/// [`RGate::StorageSwap`]: maps `((storage space, lane), cell index)` to one
/// bit, matching the fuzz interpreter's keyed Boolar storage model.
pub type StorageState = BTreeMap<((StorageId, LaneId), u64), bool>;

/// Compute the flat cell index for a storage address given its address-bit
/// wires. LSB-first per the Boolar convention.
fn addr_cell_index(addr_wires: &[usize], wires: &[bool]) -> u64 {
    let mut idx: u64 = 0;
    for (bit, w) in addr_wires.iter().enumerate() {
        if wires[*w] {
            idx |= 1u64 << bit;
        }
    }
    idx
}

/// A reversible circuit: a bijection on `{0,1}^num_wires` (jointly with any
/// referenced storage) given as a sequence of reversible gates over a fixed
/// wire vector.
///
/// Invariants, validated on construction:
/// - every wire index `< num_wires`;
/// - control/target disjointness (`Cnot`: `target ≠ ctrl`; `Ccnot`: target
///   distinct from both controls; `XorLut2`: three distinct wires;
///   `StorageSwap`: `target` not among the address wires);
/// - `XorLut2` truth tables fit in four bits;
/// - `StorageSwap` addresses are at most 64 bits wide.
///
/// There are no SSA values here and no provenance annotation (see the plan
/// document: wire-mutation form does not fit `Node<Stmt, P>`; revisit on
/// consumer demand). Cross-referencing back to Boolar value space goes through
/// the watchlist machinery produced alongside circuits by `to_reversible`
/// (in `volar-ir-passes`), not through embedded names.
impl RCircuit {
    /// Construct and validate a reversible circuit.
    pub fn new(num_wires: usize, gates: Vec<RGate>) -> Result<Self, RCircuitError> {
        let circuit = RCircuit { num_wires, gates };
        circuit.validate()?;
        Ok(circuit)
    }

    /// An empty circuit over `num_wires` wires.
    pub fn empty(num_wires: usize) -> Self {
        RCircuit {
            num_wires,
            gates: Vec::new(),
        }
    }

    /// The gate list.
    pub fn gates(&self) -> &[RGate] {
        &self.gates
    }

    /// Validate every gate against this circuit's wire count.
    pub fn validate(&self) -> Result<(), RCircuitError> {
        for (i, gate) in self.gates.iter().enumerate() {
            self.validate_gate_at(i, gate)?;
        }
        Ok(())
    }

    fn validate_gate_at(&self, i: usize, gate: &RGate) -> Result<(), RCircuitError> {
        let n = self.num_wires;
        let check = |w: usize| -> Result<(), RCircuitError> {
            if w < n {
                Ok(())
            } else {
                Err(RCircuitError::IndexOutOfRange { gate: i, wire: w })
            }
        };
        match gate {
            RGate::X(w) => check(*w),
            RGate::Cnot { ctrl, target } => {
                check(*ctrl)?;
                check(*target)?;
                if ctrl == target {
                    return Err(RCircuitError::TargetAliased {
                        gate: i,
                        wire: *ctrl,
                    });
                }
                Ok(())
            }
            RGate::Ccnot { c1, c2, target } => {
                check(*c1)?;
                check(*c2)?;
                check(*target)?;
                if target == c1 || target == c2 {
                    return Err(RCircuitError::TargetAliased {
                        gate: i,
                        wire: *target,
                    });
                }
                Ok(())
            }
            RGate::XorLut2 {
                controls,
                target,
                table,
            } => {
                check(controls[0])?;
                check(controls[1])?;
                check(*target)?;
                if *target == controls[0] || *target == controls[1] {
                    return Err(RCircuitError::TargetAliased {
                        gate: i,
                        wire: *target,
                    });
                }
                if controls[0] == controls[1] {
                    return Err(RCircuitError::ControlAliased {
                        gate: i,
                        wire: controls[0],
                    });
                }
                if table & !0x0f != 0 {
                    return Err(RCircuitError::InvalidTruthTable {
                        gate: i,
                        table: *table,
                    });
                }
                Ok(())
            }
            RGate::StorageSwap { addr, target, .. } => {
                check(*target)?;
                for w in addr.iter() {
                    check(*w)?;
                    if w == target {
                        return Err(RCircuitError::TargetAliased { gate: i, wire: *w });
                    }
                }
                if addr.len() > 64 {
                    return Err(RCircuitError::AddressTooWide {
                        gate: i,
                        width: addr.len(),
                    });
                }
                Ok(())
            }
            RGate::ExternalXor { args, target, .. } => {
                check(*target)?;
                for wire in args {
                    check(*wire)?;
                    if wire == target {
                        return Err(RCircuitError::TargetAliased {
                            gate: i,
                            wire: *wire,
                        });
                    }
                }
                Ok(())
            }
            RGate::ExternalStorageXor {
                guard,
                args,
                fallback,
                addr,
                ..
            } => {
                check(*guard)?;
                check(*fallback)?;
                for wire in args.iter().chain(addr) {
                    check(*wire)?;
                }
                if addr.len() > 64 {
                    return Err(RCircuitError::AddressTooWide {
                        gate: i,
                        width: addr.len(),
                    });
                }
                Ok(())
            }
        }
    }

    /// Append a gate after validating it against this circuit's wire count.
    pub fn push_gate(&mut self, gate: RGate) -> Result<(), RCircuitError> {
        self.validate_gate_at(self.gates.len(), &gate)?;
        self.gates.push(gate);
        Ok(())
    }

    /// Does this circuit reference storage at all?
    pub fn uses_storage(&self) -> bool {
        self.gates.iter().any(|g| {
            matches!(
                g,
                RGate::StorageSwap { .. } | RGate::ExternalStorageXor { .. }
            )
        })
    }

    /// Does this circuit require a replayable external source registry?
    pub fn uses_externals(&self) -> bool {
        self.gates.iter().any(|g| {
            matches!(
                g,
                RGate::ExternalXor { .. } | RGate::ExternalStorageXor { .. }
            )
        })
    }

    /// Apply the circuit to a wire vector, exchanging with `storage` for any
    /// [`RGate::StorageSwap`] gates.
    ///
    /// Panics if `wires.len() != num_wires` — callers own the wire vector.
    pub fn apply(&self, wires: &mut [bool], storage: &mut StorageState) {
        assert_eq!(wires.len(), self.num_wires, "wire vector length mismatch");
        assert!(
            !self.uses_externals(),
            "circuit references external sources; use apply_with_externals"
        );
        for gate in &self.gates {
            match gate {
                RGate::X(w) => wires[*w] = !wires[*w],
                RGate::Cnot { ctrl, target } => wires[*target] ^= wires[*ctrl],
                RGate::Ccnot { c1, c2, target } => wires[*target] ^= wires[*c1] & wires[*c2],
                RGate::XorLut2 {
                    controls,
                    target,
                    table,
                } => {
                    let input = ((wires[controls[0]] as u8) << 1) | wires[controls[1]] as u8;
                    wires[*target] ^= (table >> (3 - input)) & 1 == 1;
                }
                RGate::StorageSwap {
                    storage: sid,
                    lane,
                    addr,
                    target,
                } => {
                    let cell = ((*sid, *lane), addr_cell_index(addr, wires));
                    let stored = storage.remove(&cell).unwrap_or(false);
                    storage.insert(cell, wires[*target]);
                    wires[*target] = stored;
                }
                RGate::ExternalXor { .. } | RGate::ExternalStorageXor { .. } => {
                    unreachable!("external gates were checked before evaluation")
                }
            }
        }
    }

    /// Apply a circuit with deterministic/replayable external primitives.
    pub fn apply_with_externals<R: ReversibleExternalRegistry>(
        &self,
        wires: &mut [bool],
        storage: &mut StorageState,
        externals: &mut R,
    ) {
        assert_eq!(wires.len(), self.num_wires, "wire vector length mismatch");
        for gate in &self.gates {
            match gate {
                RGate::X(w) => wires[*w] = !wires[*w],
                RGate::Cnot { ctrl, target } => wires[*target] ^= wires[*ctrl],
                RGate::Ccnot { c1, c2, target } => wires[*target] ^= wires[*c1] & wires[*c2],
                RGate::XorLut2 {
                    controls,
                    target,
                    table,
                } => {
                    let input = ((wires[controls[0]] as u8) << 1) | wires[controls[1]] as u8;
                    wires[*target] ^= (table >> (3 - input)) & 1 == 1;
                }
                RGate::StorageSwap {
                    storage: sid,
                    lane,
                    addr,
                    target,
                } => {
                    let cell = ((*sid, *lane), addr_cell_index(addr, wires));
                    let stored = storage.remove(&cell).unwrap_or(false);
                    storage.insert(cell, wires[*target]);
                    wires[*target] = stored;
                }
                RGate::ExternalXor {
                    kind,
                    name,
                    args,
                    target,
                    bit,
                    occurrence,
                } => {
                    let args: Vec<_> = args.iter().map(|wire| wires[*wire]).collect();
                    let value = match kind {
                        RExternalKind::Oracle => {
                            externals.oracle_bit(name, &args, *bit, *occurrence)
                        }
                        RExternalKind::Rng => externals.rng_bit(name, *bit, *occurrence),
                    };
                    wires[*target] ^= value;
                }
                RGate::ExternalStorageXor {
                    name,
                    guard,
                    args,
                    fallback,
                    storage: sid,
                    lane,
                    addr,
                    bit,
                    occurrence,
                } => {
                    let value = if wires[*guard] {
                        let args: Vec<_> = args.iter().map(|wire| wires[*wire]).collect();
                        externals.action_bit(name, &args, *bit, *occurrence)
                    } else {
                        wires[*fallback]
                    };
                    let cell = ((*sid, *lane), addr_cell_index(addr, wires));
                    let next = storage.get(&cell).copied().unwrap_or(false) ^ value;
                    if next {
                        storage.insert(cell, true);
                    } else {
                        // The storage model's absence is canonically false;
                        // remove a cleared cell so inverse replay restores
                        // the exact sparse state as well as its value.
                        storage.remove(&cell);
                    }
                }
            }
        }
    }

    /// Apply a storage-free circuit to a wire vector.
    ///
    /// Fails closed on circuits containing [`RGate::StorageSwap`] so that
    /// consumers unaware of the storage model cannot silently mis-evaluate.
    pub fn apply_pure(&self, wires: &mut [bool]) -> Result<(), RCircuitError> {
        if self.uses_externals() {
            return Err(RCircuitError::ExternalOpsUnsupported);
        }
        if self.uses_storage() {
            return Err(RCircuitError::StorageOpsUnsupported);
        }
        let mut no_storage = StorageState::new();
        self.apply(wires, &mut no_storage);
        Ok(())
    }

    /// The inverse circuit: same wires, gates reversed. Every v1 gate is an
    /// involution, so inversion is exactly gate-list reversal.
    pub fn inverse(&self) -> RCircuit {
        RCircuit {
            num_wires: self.num_wires,
            gates: self.gates.iter().rev().cloned().collect(),
        }
    }

    /// Compose: run `self`, then `next`, over the shared wire vector.
    pub fn then(&self, next: &RCircuit) -> Result<RCircuit, RCircuitError> {
        if self.num_wires != next.num_wires {
            return Err(RCircuitError::WireCountMismatch {
                lhs: self.num_wires,
                rhs: next.num_wires,
            });
        }
        let mut gates = self.gates.clone();
        gates.extend(next.gates.iter().cloned());
        RCircuit::new(self.num_wires, gates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn validates_gate_indices() {
        assert_eq!(
            RCircuit::new(2, vec![RGate::X(2)]).unwrap_err(),
            RCircuitError::IndexOutOfRange { gate: 0, wire: 2 }
        );
        assert_eq!(
            RCircuit::new(2, vec![RGate::Cnot { ctrl: 0, target: 0 }]).unwrap_err(),
            RCircuitError::TargetAliased { gate: 0, wire: 0 }
        );
        assert_eq!(
            RCircuit::new(
                2,
                vec![RGate::Ccnot {
                    c1: 0,
                    c2: 1,
                    target: 1
                }]
            )
            .unwrap_err(),
            RCircuitError::TargetAliased { gate: 0, wire: 1 }
        );
        assert_eq!(
            RCircuit::new(
                2,
                vec![RGate::StorageSwap {
                    storage: StorageId::DEFAULT,
                    lane: LaneId(0),
                    addr: vec![0],
                    target: 0
                }]
            )
            .unwrap_err(),
            RCircuitError::TargetAliased { gate: 0, wire: 0 }
        );
        assert!(
            RCircuit::new(
                3,
                vec![RGate::Ccnot {
                    c1: 0,
                    c2: 1,
                    target: 2
                }]
            )
            .is_ok()
        );
        assert_eq!(
            RCircuit::new(
                3,
                vec![RGate::XorLut2 {
                    controls: [0, 1],
                    target: 1,
                    table: 0x1
                }],
            )
            .unwrap_err(),
            RCircuitError::TargetAliased { gate: 0, wire: 1 }
        );
        assert_eq!(
            RCircuit::new(
                3,
                vec![RGate::XorLut2 {
                    controls: [0, 0],
                    target: 2,
                    table: 0x1
                }],
            )
            .unwrap_err(),
            RCircuitError::ControlAliased { gate: 0, wire: 0 }
        );
        assert_eq!(
            RCircuit::new(
                3,
                vec![RGate::XorLut2 {
                    controls: [0, 1],
                    target: 2,
                    table: 0x10
                }],
            )
            .unwrap_err(),
            RCircuitError::InvalidTruthTable {
                gate: 0,
                table: 0x10
            }
        );
    }

    #[test]
    fn x_cnot_toffoli_semantics() {
        let c = RCircuit::new(
            3,
            vec![
                RGate::X(0),
                RGate::Cnot { ctrl: 0, target: 2 },
                RGate::Ccnot {
                    c1: 0,
                    c2: 1,
                    target: 2,
                },
            ],
        )
        .unwrap();
        // [1,1,0] -X(0)-> [0,1,0] -cnot(0->2)-> [0,1,0] -ccnot(0,1->2)-> [0,1,0]
        let mut wires = vec![true, true, false];
        c.apply_pure(&mut wires).unwrap();
        assert_eq!(wires, vec![false, true, false]);
        // inverse undoes it exactly:
        let inv = c.inverse();
        inv.apply_pure(&mut wires).unwrap();
        assert_eq!(wires, vec![true, true, false]);
    }

    #[test]
    fn xor_lut2_exhaustive_semantics_and_inverse() {
        for table in 0u8..16 {
            let c = RCircuit::new(
                3,
                vec![RGate::XorLut2 {
                    controls: [0, 1],
                    target: 2,
                    table,
                }],
            )
            .unwrap();
            for input in 0u8..8 {
                let a = input & 0b100 != 0;
                let b = input & 0b010 != 0;
                let target = input & 0b001 != 0;
                let lut_input = ((a as u8) << 1) | b as u8;
                let expected = target ^ ((table >> (3 - lut_input)) & 1 == 1);
                let mut wires = vec![a, b, target];

                c.apply_pure(&mut wires).unwrap();
                assert_eq!(
                    wires,
                    vec![a, b, expected],
                    "table={table:#x}, input={input:#x}"
                );
                c.inverse().apply_pure(&mut wires).unwrap();
                assert_eq!(wires, vec![a, b, target]);
            }
        }
    }

    #[test]
    fn storage_swap_is_involution_and_exchanges() {
        let sid = StorageId::DEFAULT;
        let c = RCircuit::new(
            3,
            vec![RGate::StorageSwap {
                storage: sid,
                lane: LaneId(0),
                addr: vec![0, 1],
                target: 2,
            }],
        )
        .unwrap();
        // LSB-first address over wires [0, 1]: wires[1]=1 selects cell 2.
        let mut wires = vec![false, true, false]; // target wire starts 0
        let mut storage = StorageState::new();
        storage.insert(((sid, LaneId(0)), 2), true); // cell 2 holds 1
        c.apply(&mut wires, &mut storage);
        // Wire picked up the stored bit; the cell now holds the old wire bit.
        assert_eq!(wires[2], true);
        assert_eq!(storage.get(&((sid, LaneId(0)), 2)), Some(&false));
        // Applying again restores both (involution).
        c.apply(&mut wires, &mut storage);
        assert_eq!(wires[2], false);
        assert_eq!(storage.get(&((sid, LaneId(0)), 2)), Some(&true));
        // apply_pure rejects storage-using circuits.
        assert_eq!(
            c.apply_pure(&mut vec![false; 3]).unwrap_err(),
            RCircuitError::StorageOpsUnsupported
        );
    }

    #[test]
    fn then_requires_matching_wire_counts() {
        let a = RCircuit::empty(2);
        let b = RCircuit::empty(3);
        assert_eq!(
            a.then(&b).unwrap_err(),
            RCircuitError::WireCountMismatch { lhs: 2, rhs: 3 }
        );
    }
}
