// @reliability: normal
// @ai: assisted
//! Gadgets: named sub-circuits attached to the regions of another circuit's
//! input/output wires.
//!
//! A [`GadgetSpec`] declares a sub-circuit's *port signature* (which of its
//! boundary bits are data vs aux/key vs randomness). A [`GadgetBinding`]
//! attaches a gadget from a [`GadgetLibrary`] to a [`RegionSelector`] over
//! another circuit's [`crate::region::RegionTable`]. The actual splicing is
//! performed by `volar_ir_passes::apply_gadgets` (see
//! `docs/wire-regions-gadgets-plan.md` for the direction semantics: inputs
//! are decrypted on entry, outputs encrypted on exit, tagged storage cells
//! transparently hold ciphertext).

use alloc::string::String;
use alloc::vec::Vec;

use crate::boolar::LaneId;
use crate::region::{RegionId, RegionSelector, WireAnchor};
use volar_ir_common::StorageId;

/// The role of one gadget port in the splice.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub enum PortKind {
    /// The plaintext-side data the host circuit actually reads/writes.
    /// `Port::width` must equal the bound region's wire count (fail-closed).
    Data,
    /// Aux material supplied from outside (key, IV, tweak). Never itself
    /// gadget-wrapped by the same binding.
    Aux,
    /// Fresh randomness: lowered to `BIrStmt::Rng`/`RngBit` occurrences
    /// inside the spliced gadget body.
    Rng,
}

/// One named gadget boundary port.
#[derive(Clone, PartialEq, Eq, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct Port {
    pub name: String,
    pub kind: PortKind,
    /// Wire width of this port in bits.
    pub width: u64,
}

/// A gadget: a named sub-circuit plus its declared port signature.
///
/// Both direction bodies share the same boundary layout: input bits are, in
/// declaration order of `ports`, the `Data` port followed by each `Aux`
/// port; output bits are the `Data` port. The **encrypt** direction maps
/// plaintext data to ciphertext data (applied on outputs and storage
/// writes); the **decrypt** direction maps ciphertext back to plaintext
/// (applied on inputs and storage reads). `decrypt: None` means the
/// `encrypt` body is self-inverse and is used in both directions.
#[derive(Clone, PartialEq, Eq, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct GadgetSpec {
    pub name: String,
    /// Encrypt direction: `data(plain) ⊗ aux → data(cipher)`. Must be a
    /// pure-gate body (Zero/One/And/Or/Xor/Not); storage, oracle, action,
    /// and RNG statements are rejected fail-closed at binding-check time.
    pub encrypt: crate::circuit::BCircuit<()>,
    /// Decrypt direction: `data(cipher) ⊗ aux → data(plain)`. `None` =
    /// self-inverse (reuse `encrypt`).
    pub decrypt: Option<crate::circuit::BCircuit<()>>,
    pub ports: Vec<Port>,
}

impl GadgetSpec {
    /// Wire width of the first `Data` port, if any.
    pub fn data_width(&self) -> Option<u64> {
        self.ports
            .iter()
            .find(|p| p.kind == PortKind::Data)
            .map(|p| p.width)
    }

    /// The body to use in the given direction.
    pub fn body_for(&self, decrypt: bool) -> &crate::circuit::BCircuit<()> {
        match (decrypt, &self.decrypt) {
            (true, Some(d)) => d,
            _ => &self.encrypt,
        }
    }

    /// Offsets of each port within the gadget's flattened input bit
    /// sequence, in declaration order. Data comes first, then Aux, then Rng.
    pub fn port_offsets(&self) -> Vec<u64> {
        let mut off = 0u64;
        self.ports
            .iter()
            .map(|p| {
                let o = off;
                off += p.width;
                o
            })
            .collect()
    }

    /// Total aux (non-data, non-rng) input width, in declaration order.
    pub fn aux_widths(&self) -> Vec<(String, u64)> {
        self.ports
            .iter()
            .filter(|p| p.kind == PortKind::Aux)
            .map(|p| (p.name.clone(), p.width))
            .collect()
    }
}

/// Source for one `Aux` gadget port.
#[derive(Clone, PartialEq, Eq, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub enum AuxSource {
    /// An input-anchored bit range of the host (e.g. the `key` region of the
    /// host's params). These wires pass through the splice untransformed.
    InputRange {
        start: u32,
        len: u32,
    },
    /// A constant bit string (spliced as Zero/One statements).
    Const(Vec<bool>),
    /// A named RNG source (lowered to `Rng` statements).
    Rng(String),
}

impl AuxSource {
    pub fn width(&self) -> u64 {
        match self {
            AuxSource::InputRange { len, .. } => *len as u64,
            AuxSource::Const(bits) => bits.len() as u64,
            AuxSource::Rng(_) => 0, // width comes from the port declaration
        }
    }
}

/// One attachment: "wrap the wires selected by `selector` with gadget
/// `gadget`".
#[derive(Clone, PartialEq, Eq, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct GadgetBinding {
    /// Key into the [`GadgetLibrary`].
    pub gadget: String,
    pub selector: RegionSelector,
    /// Source for each `Aux` port, in gadget port declaration order
    /// (skipping `Data` and `Rng` ports).
    pub aux_sources: Vec<AuxSource>,
    /// Name of the RNG source used for `Rng` ports (if any).
    pub rng_source: Option<String>,
}

/// Library of available gadgets, keyed by [`GadgetSpec::name`].
#[derive(Clone, PartialEq, Eq, Default, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct GadgetLibrary {
    pub gadgets: Vec<GadgetSpec>,
}

impl GadgetLibrary {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, spec: GadgetSpec) -> Self {
        self.gadgets.push(spec);
        self
    }

    pub fn get(&self, name: &str) -> Option<&GadgetSpec> {
        self.gadgets.iter().find(|g| g.name == name)
    }
}

/// Errors from binding validation and gadget application.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum GadgetError {
    /// The region table failed validation against the host circuit.
    Region(crate::region::RegionError),
    /// The bound gadget name is not in the library.
    UnknownGadget(String),
    /// A selector matched no boundary wires.
    EmptySelection,
    /// The gadget's data-port width does not match the selected wire count.
    DataWidthMismatch {
        gadget: String,
        port: u64,
        wires: u64,
        boundary: &'static str,
    },
    /// An aux source's total width does not match the declared aux ports.
    AuxWidthMismatch {
        gadget: String,
        port: String,
        expected: u64,
        got: u64,
    },
    /// `Rng` ports / `AuxSource::Rng` are declared in the type surface but
    /// not supported by the v1 splicer (wrap/unwrap keystream consistency
    /// needs an out-of-band channel — see the pass module docs).
    UnsupportedRng,
    /// An `InputRange` aux source extends past the host's param count.
    AuxInputOutOfRange { start: u32, len: u32, params: u32 },
    /// An aux `InputRange` overlaps a claimed (selected) input wire — e.g. a
    /// key region that its own binding would encrypt.
    AuxRangeOverlapsSelection { wire: crate::region::BoundaryWire },
    /// Two bindings claim the same boundary wire.
    OverlappingBindings { wire: crate::region::BoundaryWire },
    /// The gadget body is not a pure-gate circuit (or its shape does not
    /// match its declared ports).
    InvalidGadgetBody { gadget: String, reason: &'static str },
    /// A storage statement in a wrapped `(StorageId, LaneId)` space has a
    /// non-constant address; v1 storage wrapping is static-address only.
    DynamicStorageAddress,
    /// A static storage address cannot be represented by the numeric region
    /// metadata used by the v1 gadget planner.
    StorageAddressTooWide { bits: usize },
    /// A `pre_init` segment inside a selected storage range could not be
    /// re-encrypted: the owning binding's aux sources are not all `Const`.
    PreInitNeedsConstantAux { gadget: String },
    /// The host circuit contains a statement kind the v1 splicer does not
    /// support (oracle/action/RNG statements).
    UnsupportedHostStmt(&'static str),
    /// Internal invariant violation (unmapped variable, empty data output).
    Internal(String),
}

/// Convenience constructor for a range anchor over input bits.
pub fn input_range(start: u32, len: u32) -> WireAnchor {
    WireAnchor::Input { start, len }
}

/// Convenience constructor for a range anchor over output bits.
pub fn output_range(start: u32, len: u32) -> WireAnchor {
    WireAnchor::Output { start, len }
}

/// Convenience constructor for a storage-cell-range anchor.
pub fn storage_range(storage: StorageId, lane: LaneId, start: u64, len: u64) -> WireAnchor {
    WireAnchor::Storage {
        storage,
        lane,
        start,
        len,
    }
}

/// Build a selector requiring all of `all` and none of `none`.
pub fn selector(all: &[RegionId], none: &[RegionId]) -> RegionSelector {
    RegionSelector {
        all_of: all.iter().copied().collect(),
        none_of: none.iter().copied().collect(),
    }
}

#[cfg(all(test, feature = "rkyv"))]
mod rkyv_tests {
    use super::*;
    use alloc::string::ToString;
    use crate::ir::IRVarId;

    #[test]
    fn gadget_types_rkyv_roundtrip() {
        let mut body = crate::circuit::BCircuit::<()>::new(2);
        let x = body.push_stmt(crate::boolar::BIrStmt::Xor(IRVarId(0), IRVarId(1)), ());
        body.outputs = alloc::vec![x];
        let lib = GadgetLibrary::new().with(GadgetSpec {
            name: "pad".to_string(),
            encrypt: body.clone(),
            decrypt: Some(body),
            ports: alloc::vec![
                Port { name: "data".to_string(), kind: PortKind::Data, width: 1 },
                Port { name: "key".to_string(), kind: PortKind::Aux, width: 1 },
            ],
        });
        let binding = GadgetBinding {
            gadget: "pad".to_string(),
            selector: crate::region::RegionSelector {
                all_of: alloc::collections::BTreeSet::from([crate::region::RegionId(0)]),
                none_of: alloc::collections::BTreeSet::from([crate::region::RegionId(1)]),
            },
            aux_sources: alloc::vec![
                AuxSource::Const(alloc::vec![true]),
                AuxSource::InputRange { start: 4, len: 1 },
                AuxSource::Rng("n".to_string()),
            ],
            rng_source: None,
        };

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&lib).expect("serializes");
        let archived = rkyv::access::<ArchivedGadgetLibrary, rkyv::rancor::Error>(&bytes)
            .expect("validates");
        let round: GadgetLibrary =
            rkyv::deserialize::<GadgetLibrary, rkyv::rancor::Error>(archived).expect("deserializes");
        assert_eq!(round, lib);

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&binding).expect("serializes");
        let archived = rkyv::access::<ArchivedGadgetBinding, rkyv::rancor::Error>(&bytes)
            .expect("validates");
        let round: GadgetBinding =
            rkyv::deserialize::<GadgetBinding, rkyv::rancor::Error>(archived).expect("deserializes");
        assert_eq!(round, binding);
    }
}
