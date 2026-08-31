// @reliability: normal
// @ai: assisted
//! Input/output wire regions: boundary-metadata for circuit wires.
//!
//! A [`RegionTable`] records which named region(s) each boundary wire group
//! of a circuit belongs to — function input bits, function output bits, and
//! storage cells (storage values, stack/temp slots, state slots — all
//! uniform `(StorageId, LaneId)` bit cells at the Boolar level).
//!
//! The table is *companion metadata*, carried alongside the circuit and not
//! inside it: the boundary of a [`crate::circuit::BCircuit`] has no
//! `Node`-wrapped statements to hang metadata on (`params` is a bare bit
//! count; storage cells are `(StorageId, LaneId, addr)` triples), matching
//! how circuit-input sides are handled by explicit assignment tables rather
//! than IR-node metadata. Region ids are opaque numeric identifiers that are
//! never synthesized — they are supplied explicitly by the frontend or
//! consumer, exactly like [`volar_ir_common::StorageId`] and
//! [`volar_side::SideId`].

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::vec::Vec;

use volar_ir_common::StorageId;

use crate::boolar::LaneId;

/// Opaque region identifier. Never invented locally; always supplied
/// explicitly at a true introduction point (frontend or consumer config).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
#[cfg_attr(feature = "rkyv", rkyv(derive(PartialEq, Eq, PartialOrd, Ord)))]
pub struct RegionId(pub u32);

/// Where a region-annotated group of wires is anchored on the circuit
/// boundary. Each anchor selects a contiguous range of wires; a wire may
/// belong to several regions via different (or shared) region-id sets.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
#[cfg_attr(feature = "rkyv", rkyv(derive(PartialEq, Eq, PartialOrd, Ord)))]
pub enum WireAnchor {
    /// Function input: bit range `[start, start + len)` within the entry
    /// params bit sequence (`BCircuit::params`, LSB = param bit 0).
    Input { start: u32, len: u32 },
    /// Function output: bit range `[start, start + len)` within the output
    /// wire sequence (`BCircuit::outputs`, in output order).
    Output { start: u32, len: u32 },
    /// Storage cells: flat cell-address range `[addr, addr + len)` within
    /// one `(StorageId, LaneId)` space. Every Boolar storage cell holds
    /// exactly one bit and is addressed by its flat cell address, so one
    /// variant covers storage values, `StorageId::STACK` temp slots, and
    /// arbitrary state slots alike.
    Storage {
        storage: StorageId,
        lane: LaneId,
        start: u64,
        len: u64,
    },
}

impl WireAnchor {
    /// Number of wires selected by this anchor.
    pub fn len(&self) -> u64 {
        match self {
            WireAnchor::Input { len, .. } => *len as u64,
            WireAnchor::Output { len, .. } => *len as u64,
            WireAnchor::Storage { len, .. } => *len,
        }
    }

    /// True if this anchor selects no wires.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// One entry: a boundary anchor together with the set of regions its wires
/// belong to.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct RegionEntry {
    pub anchor: WireAnchor,
    pub regions: BTreeSet<RegionId>,
}

/// A boundary wire position produced by [`RegionTable::select`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
#[cfg_attr(feature = "rkyv", rkyv(derive(PartialEq, Eq, PartialOrd, Ord)))]
pub enum BoundaryWire {
    Input(u32),
    Output(u32),
    Cell {
        storage: StorageId,
        lane: LaneId,
        addr: u64,
    },
}

/// Positive + negative region membership, enough to express e.g.
/// "all wires tagged `public` except those also tagged `plaintext`".
#[derive(Clone, PartialEq, Eq, Default, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct RegionSelector {
    /// Wire must carry *every* region in this set (empty = no constraint).
    pub all_of: BTreeSet<RegionId>,
    /// Wire must carry *none* of the regions in this set.
    pub none_of: BTreeSet<RegionId>,
}

impl RegionSelector {
    pub fn all_of(iter: impl IntoIterator<Item = RegionId>) -> Self {
        RegionSelector {
            all_of: iter.into_iter().collect(),
            none_of: BTreeSet::new(),
        }
    }

    pub fn none_of(iter: impl IntoIterator<Item = RegionId>) -> Self {
        RegionSelector {
            all_of: BTreeSet::new(),
            none_of: iter.into_iter().collect(),
        }
    }

    /// Does the given region set satisfy this selector?
    pub fn matches(&self, regions: &BTreeSet<RegionId>) -> bool {
        self.all_of.iter().all(|r| regions.contains(r))
            && !self.none_of.iter().any(|r| regions.contains(r))
    }
}

/// Optional human-readable names for region ids (printer-only metadata;
/// semantics are by id, never by name).
pub type RegionNames = BTreeMap<RegionId, alloc::string::String>;

/// Errors produced when validating a [`RegionTable`] against a host circuit.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RegionError {
    /// An anchor selects zero wires.
    EmptyAnchor(WireAnchor),
    /// An `Input` anchor extends past the host entry param count.
    InputOutOfRange { start: u32, len: u32, params: u32 },
    /// An `Output` anchor extends past the host output count.
    OutputOutOfRange { start: u32, len: u32, outputs: u32 },
    /// The anchor's `(StorageId, LaneId)` space is never touched by the host
    /// circuit's storage traffic (read/write/pre-init).
    UnknownStorageSpace {
        storage: StorageId,
        lane: LaneId,
    },
    /// The anchor's flat-cell range overflows `u64`.
    StorageRangeOverflow { start: u64, len: u64 },
    /// Two entries claim the same wire position; the regions of a wire must
    /// be a well-defined unique lookup. (Multiple regions on one wire are
    /// expressed as multiple ids in a single entry's set.)
    OverlappingEntries {
        first: WireAnchor,
        second: WireAnchor,
    },
    /// Entries are not in sorted order.
    Unsorted,
    /// An entry carries an empty region set.
    EmptyRegions(WireAnchor),
}

/// The full input/output wire-region assignment for one circuit.
///
/// Entries are kept sorted by anchor; a single wire position must be claimed
/// by at most one entry (multiple regions on one wire = one entry with a
/// multi-element `regions` set). Overlapping *region sets* across entries are
/// allowed — a wire can be both `public` and `key` — as long as the claimed
/// wire ranges themselves do not overlap.
#[derive(Clone, PartialEq, Eq, Default, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct RegionTable {
    pub entries: alloc::vec::Vec<RegionEntry>,
    pub names: RegionNames,
}

impl RegionTable {
    /// An empty table.
    pub fn new() -> Self {
        Self::default()
    }

    /// Validate this table against a host circuit (see [`RegionError`] for
    /// the individual invariants).
    pub fn validate<P: Clone>(
        &self,
        host: &crate::circuit::BCircuit<P>,
    ) -> Result<(), RegionError> {
        let mut claimed_inputs: alloc::vec::Vec<(u64, u64, WireAnchor)> = Vec::new();
        let mut claimed_outputs: alloc::vec::Vec<(u64, u64, WireAnchor)> = Vec::new();
        // Per (storage, lane): sorted claimed ranges.
        let mut claimed_cells: BTreeMap<
            (StorageId, LaneId),
            alloc::vec::Vec<(u64, u64, WireAnchor)>,
        > = BTreeMap::new();

        let mut prev: Option<&WireAnchor> = None;
        for entry in &self.entries {
            if entry.regions.is_empty() {
                return Err(RegionError::EmptyRegions(entry.anchor.clone()));
            }
            if let Some(p) = prev {
                if p >= &entry.anchor {
                    if p == &entry.anchor {
                        return Err(RegionError::OverlappingEntries {
                            first: p.clone(),
                            second: entry.anchor.clone(),
                        });
                    }
                    return Err(RegionError::Unsorted);
                }
            }
            prev = Some(&entry.anchor);

            // Collect observed storage traffic first? No — anchors are
            // validated against `host` below.
            match &entry.anchor {
                WireAnchor::Input { start, len } => {
                    if *len == 0 {
                        return Err(RegionError::EmptyAnchor(entry.anchor.clone()));
                    }
                    if start.saturating_add(*len) > host.params {
                        return Err(RegionError::InputOutOfRange {
                            start: *start,
                            len: *len,
                            params: host.params,
                        });
                    }
                    Self::check_disjoint(
                        claimed_inputs.iter(),
                        *start as u64,
                        *len as u64,
                        &entry.anchor,
                    )?;
                    claimed_inputs.push((*start as u64, *len as u64, entry.anchor.clone()));
                }
                WireAnchor::Output { start, len } => {
                    if *len == 0 {
                        return Err(RegionError::EmptyAnchor(entry.anchor.clone()));
                    }
                    if start.saturating_add(*len) > host.outputs.len() as u32 {
                        return Err(RegionError::OutputOutOfRange {
                            start: *start,
                            len: *len,
                            outputs: host.outputs.len() as u32,
                        });
                    }
                    Self::check_disjoint(
                        claimed_outputs.iter(),
                        *start as u64,
                        *len as u64,
                        &entry.anchor,
                    )?;
                    claimed_outputs.push((*start as u64, *len as u64, entry.anchor.clone()));
                }
                WireAnchor::Storage {
                    storage,
                    lane,
                    start,
                    len,
                } => {
                    if *len == 0 {
                        return Err(RegionError::EmptyAnchor(entry.anchor.clone()));
                    }
                    start.checked_add(*len).ok_or(RegionError::StorageRangeOverflow {
                        start: *start,
                        len: *len,
                    })?;
                    if !host_uses_storage(host, *storage, *lane) {
                        return Err(RegionError::UnknownStorageSpace {
                            storage: *storage,
                            lane: *lane,
                        });
                    }
                    let key = (*storage, *lane);
                    let slots = claimed_cells.entry(key).or_default();
                    Self::check_disjoint(slots.iter(), *start, *len, &entry.anchor)?;
                    slots.push((*start, *len, entry.anchor.clone()));
                }
            }
        }
        Ok(())
    }

    /// `claimed`: `(start, len)` pairs already validated to not overflow.
    fn check_disjoint<'a, I: Iterator<Item = &'a (u64, u64, WireAnchor)>>(
        claimed: I,
        next_start: u64,
        next_len: u64,
        anchor: &WireAnchor,
    ) -> Result<(), RegionError> {
        let next_end = next_start + next_len; // overflow already checked upstream
        for (start, len, first) in claimed {
            let end = start + len;
            if next_start < end && *start < next_end {
                return Err(RegionError::OverlappingEntries {
                    first: first.clone(),
                    second: anchor.clone(),
                });
            }
        }
        Ok(())
    }

    /// The region set claimed by one input bit, or `None` if untagged.
    pub fn input_regions(&self, bit: u32) -> Option<&BTreeSet<RegionId>> {
        self.find(
            |a| matches!(a, WireAnchor::Input { .. }),
            |a| match a {
                WireAnchor::Input { start, len } => (*start as u64, (*start + *len) as u64),
                _ => (0, 0),
            },
            bit as u64,
        )
    }

    /// The region set claimed by one output bit, or `None` if untagged.
    pub fn output_regions(&self, bit: u32) -> Option<&BTreeSet<RegionId>> {
        self.find(
            |a| matches!(a, WireAnchor::Output { .. }),
            |a| match a {
                WireAnchor::Output { start, len } => (*start as u64, (*start + *len) as u64),
                _ => (0, 0),
            },
            bit as u64,
        )
    }

    /// The region set claimed by one storage cell, or `None` if untagged.
    pub fn cell_regions(
        &self,
        storage: StorageId,
        lane: LaneId,
        addr: u64,
    ) -> Option<&BTreeSet<RegionId>> {
        self.find(
            |a| match a {
                WireAnchor::Storage { storage: s, lane: l, .. } => {
                    *s == storage && *l == lane
                }
                _ => false,
            },
            |a| match a {
                WireAnchor::Storage { start, len, .. } => (*start, *start + *len),
                _ => (0, 0),
            },
            addr,
        )
    }

    fn find(
        &self,
        kind: impl Fn(&WireAnchor) -> bool,
        range: impl Fn(&WireAnchor) -> (u64, u64),
        pos: u64,
    ) -> Option<&BTreeSet<RegionId>> {
        // Entries are sorted by anchor (Ord derives variant order
        // Input < Output < Storage, then by fields), so a binary search
        // would need kind-aware keys; a linear scan keeps this simple and
        // region tables are small boundary metadata.
        for entry in &self.entries {
            if !kind(&entry.anchor) {
                continue;
            }
            let (r_start, r_end) = range(&entry.anchor);
            if r_start <= pos && pos < r_end {
                return Some(&entry.regions);
            }
        }
        None
    }

    /// All boundary wires whose region set satisfies `sel`.
    pub fn select(&self, sel: &RegionSelector) -> Vec<BoundaryWire> {
        let mut out = Vec::new();
        for entry in &self.entries {
            if !sel.matches(&entry.regions) {
                continue;
            }
            match &entry.anchor {
                WireAnchor::Input { start, len } => {
                    for b in *start..*start + *len {
                        out.push(BoundaryWire::Input(b));
                    }
                }
                WireAnchor::Output { start, len } => {
                    for b in *start..*start + *len {
                        out.push(BoundaryWire::Output(b));
                    }
                }
                WireAnchor::Storage {
                    storage,
                    lane,
                    start,
                    len,
                } => {
                    for a in *start..*start + *len {
                        out.push(BoundaryWire::Cell {
                            storage: *storage,
                            lane: *lane,
                            addr: a,
                        });
                    }
                }
            }
        }
        out
    }
}

/// Does the host circuit touch any cell of `(storage, lane)`?
///
/// Storage addresses in Boolar are data-dependent (`Vec<IRVarId>` bit
/// vectors), so a static cell-count upper bound is not generally derivable;
/// the fail-closed check is that the `(StorageId, LaneId)` space appears in
/// the circuit's storage traffic or `pre_init` at all.
fn host_uses_storage<P: Clone>(host: &crate::circuit::BCircuit<P>, storage: StorageId, lane: crate::boolar::LaneId) -> bool {
    use crate::boolar::BIrStmt;
    host.pre_init
        .iter()
        .any(|seg| seg.storage == storage && seg.lane == lane)
        || host.stmts.iter().any(|s| match &s.kind {
            BIrStmt::StorageRead {
                storage: s, lane: l, ..
            }
            | BIrStmt::StorageWrite {
                storage: s, lane: l, ..
            } => *s == storage && *l == lane,
            _ => false,
        })
}
#[cfg(all(test, feature = "rkyv"))]
mod rkyv_tests {
    use super::*;
    use alloc::string::ToString;

    fn sample_table() -> RegionTable {
        RegionTable {
            entries: alloc::vec![
                RegionEntry {
                    anchor: WireAnchor::Input { start: 0, len: 4 },
                    regions: BTreeSet::from([RegionId(0), RegionId(1)]),
                },
                RegionEntry {
                    anchor: WireAnchor::Output { start: 2, len: 1 },
                    regions: BTreeSet::from([RegionId(1)]),
                },
                RegionEntry {
                    anchor: WireAnchor::Storage {
                        storage: StorageId(0),
                        lane: crate::boolar::LaneId(0),
                        start: 3,
                        len: 5,
                    },
                    regions: BTreeSet::from([RegionId(2)]),
                },
            ],
            names: BTreeMap::from([(RegionId(0), "public".to_string())]),
        }
    }

    #[test]
    fn region_table_rkyv_roundtrip() {
        let table = sample_table();
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&table).expect("serializes");
        let archived = rkyv::access::<ArchivedRegionTable, rkyv::rancor::Error>(&bytes)
            .expect("validates");
        let round: RegionTable = rkyv::deserialize::<RegionTable, rkyv::rancor::Error>(archived)
            .expect("deserializes");
        assert_eq!(round, table);
    }

    #[test]
    fn selector_rkyv_roundtrip() {
        let sel = RegionSelector {
            all_of: BTreeSet::from([RegionId(0)]),
            none_of: BTreeSet::from([RegionId(1), RegionId(2)]),
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&sel).expect("serializes");
        let archived = rkyv::access::<ArchivedRegionSelector, rkyv::rancor::Error>(&bytes)
            .expect("validates");
        let round: RegionSelector =
            rkyv::deserialize::<RegionSelector, rkyv::rancor::Error>(archived).expect("deserializes");
        assert_eq!(round, sel);
    }
}
