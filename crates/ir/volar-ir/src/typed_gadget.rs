// @reliability: normal
// @ai: assisted
//! Typed region anchors and typed gadget specifications.
//!
//! This is the **authoring layer** above the landed bit-level
//! [`crate::region::RegionTable`] / [`crate::gadget::GadgetSpec`]: anchors
//! are expressed in typed terms (bit ranges *within* a param/output var's
//! bit layout, typed storage ranges covering every bit plane) and gadget
//! bodies are typed [`VCircuit`]s instead of hand-written Boolean gates.
//!
//! Nothing here splices. Typed tables and libraries are *lowered* onto the
//! bit level by `volar_ir_passes::region_lowering` and applied by the
//! existing `apply_gadgets` pass — one splicer, one correctness story
//! (see `docs/typed-gadgets-and-region-threading-plan.md`).
//!
//! Anchor kinds by host level:
//!
//! | Anchor | `VCircuit` | `IRBlocks` | VAFFLE module |
//! |---|---|---|---|
//! | [`TypedAnchor::Input`] | entry params | entry-block params | — |
//! | [`TypedAnchor::BlockInput`] | — | any block's params | — |
//! | [`TypedAnchor::FuncInput`] / [`TypedAnchor::FuncOutput`] | — | — | per-function |
//! | [`TypedAnchor::Output`] | outputs | — | — |
//! | [`TypedAnchor::Storage`] | ✓ | ✓ | ✓ (validated by `volar-vaffle-target`) |

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec::Vec;

use volar_ir_common::{Constant, StorageId};

use crate::circuit::VCircuit;
use crate::gadget::PortKind;
use crate::ir::{IRBlockId, IRBlocks, IRStmt, IRType, IRTypeId, IRTypes, IRVarId};
use crate::region::{RegionId, RegionNames, RegionSelector};

// ============================================================================
// Bit widths
// ============================================================================

/// Total GF(2) bit width of a type, or `None` if the type has no Boolean
/// lowering (`Z3`). Unlike the pass-level `ir_type_bits`, this never panics:
/// callers turn `None` into a fail-closed error.
///
/// `Block`/`Func` types have width 0 (control-flow labels carry no bits).
pub fn typed_bit_width(ty_id: IRTypeId, types: &IRTypes) -> Option<usize> {
    match &types.0[ty_id.0 as usize] {
        IRType::Primitive(t) => match t {
            volar_ir_common::Type::Bit => Some(1),
            volar_ir_common::Type::_8 | volar_ir_common::Type::AES8 => Some(8),
            volar_ir_common::Type::_16 => Some(16),
            volar_ir_common::Type::_32 => Some(32),
            volar_ir_common::Type::_64 | volar_ir_common::Type::Galois64 => Some(64),
            volar_ir_common::Type::_128 => Some(128),
            volar_ir_common::Type::_256 => Some(256),
            volar_ir_common::Type::Z3 => None,
            _ => None,
        },
        IRType::Vec(n, elem) => typed_bit_width(*elem, types).map(|w| n.saturating_mul(w)),
        IRType::Tuple(ids) => ids
            .iter()
            .try_fold(0usize, |acc, &id| {
                typed_bit_width(id, types).map(|w| acc + w)
            }),
        IRType::Block { .. } | IRType::Func { .. } => Some(0),
        _ => None,
    }
}

/// Extract bit `bit` (LSB-first) of a [`Constant`].
pub fn constant_bit(c: &Constant, bit: usize) -> bool {
    if bit < 128 {
        (c.lo >> bit) & 1 == 1
    } else {
        (c.hi >> (bit - 128)) & 1 == 1
    }
}

// ============================================================================
// Typed anchors
// ============================================================================

/// A typed boundary anchor: a contiguous range *within the bit layout of a
/// typed carrier* (param, output var, function result, or storage value).
/// Bit ranges are LSB-first and never span two carriers.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
#[cfg_attr(feature = "rkyv", rkyv(derive(PartialEq, Eq, PartialOrd, Ord)))]
pub enum TypedAnchor {
    /// Bit range `[start, start + len)` of entry input param `param`
    /// (`VCircuit::params[param]`; at the `IRBlocks` level, block 0's
    /// param `param`).
    Input { param: u32, start: u16, len: u16 },
    /// Bit range of one block param of a multi-block `IRBlocks` host.
    BlockInput {
        block: IRBlockId,
        param: u32,
        start: u16,
        len: u16,
    },
    /// Bit range of one function param of a VAFFLE module host
    /// (`func` is [`vaffle::FuncId`]`.0`).
    FuncInput {
        func: u32,
        param: u32,
        start: u16,
        len: u16,
    },
    /// Bit range of output position `out` (`VCircuit::outputs[out]`, the
    /// var's bit layout in output order).
    Output { out: u32, start: u16, len: u16 },
    /// Bit range of one function result of a VAFFLE module host.
    FuncOutput {
        func: u32,
        result: u32,
        start: u16,
        len: u16,
    },
    /// Typed storage value range: element addresses
    /// `[addr_start, addr_start + addr_len)` of `(storage, ty)`. One anchor
    /// covers **all bit planes** — at the Boolar level it fans out to one
    /// flat-cell anchor per plane `i`, covering cells
    /// `[addr + (i << N), …)` where `N` is the lane's element-address width.
    Storage {
        storage: StorageId,
        ty: IRTypeId,
        addr_start: u64,
        addr_len: u64,
    },
}

impl TypedAnchor {
    /// Carrier-space key used for per-space disjointness checking:
    /// `(kind, carrier_a, carrier_b)`. Two anchors may overlap only if all
    /// three components are equal.
    fn space_key(&self) -> (u8, u64, u64) {
        match self {
            TypedAnchor::Input { param, .. } => (0, *param as u64, 0),
            TypedAnchor::BlockInput { block, param, .. } => (1, block.0 as u64, *param as u64),
            TypedAnchor::FuncInput { func, param, .. } => (2, *func as u64, *param as u64),
            TypedAnchor::Output { out, .. } => (3, *out as u64, 0),
            TypedAnchor::FuncOutput { func, result, .. } => (4, *func as u64, *result as u64),
            TypedAnchor::Storage { storage, ty, .. } => (5, storage.0 as u64, ty.0 as u64),
        }
    }

    /// The selected range as `(start, len)` within the carrier.
    fn range(&self) -> (u64, u64) {
        match self {
            TypedAnchor::Input { start, len, .. }
            | TypedAnchor::BlockInput { start, len, .. }
            | TypedAnchor::FuncInput { start, len, .. }
            | TypedAnchor::Output { start, len, .. }
            | TypedAnchor::FuncOutput { start, len, .. } => (*start as u64, *len as u64),
            TypedAnchor::Storage {
                addr_start,
                addr_len,
                ..
            } => (*addr_start, *addr_len),
        }
    }

}

/// One typed entry: a boundary anchor together with the regions its bits
/// belong to.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct TypedRegionEntry {
    pub anchor: TypedAnchor,
    pub regions: BTreeSet<RegionId>,
}

/// One typed boundary position produced by [`TypedRegionTable::select`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
#[cfg_attr(feature = "rkyv", rkyv(derive(PartialEq, Eq, PartialOrd, Ord)))]
pub enum TypedBoundaryWire {
    Input { param: u32, bit: u16 },
    BlockInput {
        block: IRBlockId,
        param: u32,
        bit: u16,
    },
    FuncInput { func: u32, param: u32, bit: u16 },
    Output { out: u32, bit: u16 },
    FuncOutput { func: u32, result: u32, bit: u16 },
    /// One bit plane of one typed storage cell.
    Cell {
        storage: StorageId,
        ty: IRTypeId,
        addr: u64,
        plane: u16,
    },
}

/// Errors from typed region-table validation.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TypedRegionError {
    /// Structural: an anchor selects zero positions.
    EmptyAnchor(TypedAnchor),
    /// Structural: an entry carries an empty region set.
    EmptyRegions(TypedAnchor),
    /// Structural: entries are not in ascending anchor order.
    Unsorted,
    /// Structural: two entries claim positions in the same carrier space
    /// with overlapping ranges.
    OverlappingEntries {
        first: TypedAnchor,
        second: TypedAnchor,
    },
    /// The anchor's carrier (param/block/output/function) does not exist on
    /// the host, or its type has no Boolean width.
    UnknownCarrier { anchor: TypedAnchor },
    /// A bit range extends past the carrier's width.
    RangeOutOfRange {
        anchor: TypedAnchor,
        start: u16,
        len: u16,
        width: usize,
    },
    /// A storage range extends past `u64`.
    StorageRangeOverflow { anchor: TypedAnchor },
    /// The anchor's `(StorageId, TypeId)` space is never touched by the
    /// host's storage traffic (or `pre_init`, where the host has one).
    UnknownStorageSpace { storage: StorageId, ty: IRTypeId },
    /// The anchor kind is not meaningful for this host level (e.g.
    /// `Output` on a multi-block `IRBlocks`, which has no module output
    /// list).
    UnsupportedAnchor { anchor: TypedAnchor, host: &'static str },
}

/// The typed input/output/storage region assignment for one host program.
/// Structural invariants mirror the landed bit-level [`crate::region::RegionTable`].
#[derive(Clone, PartialEq, Eq, Default, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct TypedRegionTable {
    pub entries: Vec<TypedRegionEntry>,
    pub names: RegionNames,
}

impl TypedRegionTable {
    pub fn new() -> Self {
        Self::default()
    }

    /// Level-independent structural validation: ascending anchor order,
    /// non-empty selections and region sets, per-carrier-space disjointness.
    pub fn validate_structure(&self) -> Result<(), TypedRegionError> {
        // (space key) -> (last range end, anchor), for disjointness.
        let mut space_ends: BTreeMap<(u8, u64, u64), (u64, TypedAnchor)> = BTreeMap::new();
        let mut prev: Option<&TypedAnchor> = None;
        for entry in &self.entries {
            if entry.regions.is_empty() {
                return Err(TypedRegionError::EmptyRegions(entry.anchor.clone()));
            }
            if let Some(p) = prev {
                if p == &entry.anchor {
                    return Err(TypedRegionError::OverlappingEntries {
                        first: p.clone(),
                        second: entry.anchor.clone(),
                    });
                }
                if p > &entry.anchor {
                    return Err(TypedRegionError::Unsorted);
                }
            }
            prev = Some(&entry.anchor);

            let (start, len) = entry.anchor.range();
            if len == 0 {
                return Err(TypedRegionError::EmptyAnchor(entry.anchor.clone()));
            }
            let end = match start.checked_add(len) {
                Some(e) => e,
                None => return Err(TypedRegionError::StorageRangeOverflow {
                    anchor: entry.anchor.clone(),
                }),
            };
            let key = entry.anchor.space_key();
            if let Some((last_end, first)) = space_ends.get(&key)
                && start < *last_end
            {
                return Err(TypedRegionError::OverlappingEntries {
                    first: first.clone(),
                    second: entry.anchor.clone(),
                });
            }
            space_ends.insert(key, (end, entry.anchor.clone()));
        }
        Ok(())
    }

    /// All typed boundary bits whose region set satisfies `sel`.
    ///
    /// Storage anchors need their host's bit-plane count; `planes_of` maps a
    /// typed storage anchor's type to its Boolean width (`None` = unknown).
    pub fn select(
        &self,
        sel: &RegionSelector,
        planes_of: impl Fn(IRTypeId) -> Option<usize>,
    ) -> Vec<TypedBoundaryWire> {
        let mut wires = Vec::new();
        for entry in &self.entries {
            if !sel.matches(&entry.regions) {
                continue;
            }
            match &entry.anchor {
                TypedAnchor::Input { param, start, len } => {
                    for b in *start..*start + *len {
                        wires.push(TypedBoundaryWire::Input { param: *param, bit: b });
                    }
                }
                TypedAnchor::BlockInput {
                    block,
                    param,
                    start,
                    len,
                } => {
                    for b in *start..*start + *len {
                        wires.push(TypedBoundaryWire::BlockInput {
                            block: *block,
                            param: *param,
                            bit: b,
                        });
                    }
                }
                TypedAnchor::FuncInput {
                    func,
                    param,
                    start,
                    len,
                } => {
                    for b in *start..*start + *len {
                        wires.push(TypedBoundaryWire::FuncInput {
                            func: *func,
                            param: *param,
                            bit: b,
                        });
                    }
                }
                TypedAnchor::Output { out, start, len } => {
                    for b in *start..*start + *len {
                        wires.push(TypedBoundaryWire::Output { out: *out, bit: b });
                    }
                }
                TypedAnchor::FuncOutput {
                    func,
                    result,
                    start,
                    len,
                } => {
                    for b in *start..*start + *len {
                        wires.push(TypedBoundaryWire::FuncOutput {
                            func: *func,
                            result: *result,
                            bit: b,
                        });
                    }
                }
                TypedAnchor::Storage {
                    storage,
                    ty,
                    addr_start,
                    addr_len,
                } => {
                    if let Some(planes) = planes_of(*ty) {
                        for plane in 0..planes {
                            for a in *addr_start..*addr_start + *addr_len {
                                wires.push(TypedBoundaryWire::Cell {
                                    storage: *storage,
                                    ty: *ty,
                                    addr: a,
                                    plane: plane as u16,
                                });
                            }
                        }
                    }
                }
            }
        }
        wires
    }
}

/// Per-var Boolean bit width for a straight-line typed host (`VCircuit`).
///
/// Params map to `0..params.len()`; statement `i` defines var
/// `params.len() + i`. `StorageWrite` defines no usable value and is
/// omitted (a width lookup on its var slot returns `None`).
pub fn vcircuit_var_widths(host: &VCircuit, types: &IRTypes) -> BTreeMap<u32, usize> {
    let mut widths = BTreeMap::new();
    for (i, ty) in host.params.iter().enumerate() {
        if let Some(w) = typed_bit_width(*ty, types) {
            widths.insert(i as u32, w);
        }
    }
    let base = host.params.len() as u32;
    for (i, node) in host.stmts.iter().enumerate() {
        let ty = match &node.kind {
            IRStmt::StorageRead { ty, .. } => *ty,
            IRStmt::Const(_, ty) => *ty,
            IRStmt::Transmute { dst_ty, .. } => *dst_ty,
            IRStmt::Poly { ty, .. }
            | IRStmt::Rol { ty, .. }
            | IRStmt::Ror { ty, .. }
            | IRStmt::Merge { ty, .. }
            | IRStmt::Splat { ty, .. }
            | IRStmt::Shuffle { ty, .. } => *ty,
            IRStmt::OracleCall { result_ty, .. } | IRStmt::ActionCall { result_ty, .. } => {
                *result_ty
            }
            IRStmt::OracleOutput { ty, .. }
            | IRStmt::ActionOutput { ty, .. }
            | IRStmt::Rng { ty, .. } => *ty,
            IRStmt::StorageWrite { .. } | IRStmt::ActionStore { .. } => continue,
            _ => continue,
        };
        if let Some(w) = typed_bit_width(ty, types) {
            widths.insert(base + i as u32, w);
        }
    }
    widths
}

impl TypedRegionTable {
    /// Validate against a typed fused-circuit host.
    pub fn validate_vcircuit(
        &self,
        host: &VCircuit,
        types: &IRTypes,
    ) -> Result<(), TypedRegionError> {
        self.validate_structure()?;
        let var_widths = vcircuit_var_widths(host, types);
        for entry in &self.entries {
            match &entry.anchor {
                TypedAnchor::Input { param, start, len } => {
                    check_range(
                        entry,
                        *param,
                        &host.params,
                        |&ty| typed_bit_width(ty, types),
                        *start,
                        *len,
                    )?;
                }
                TypedAnchor::Output { out, start, len } => {
                    let Some(&var) = host.outputs.get(*out as usize) else {
                        return Err(TypedRegionError::UnknownCarrier {
                            anchor: entry.anchor.clone(),
                        });
                    };
                    let Some(width) = var_widths.get(&var.0).copied() else {
                        return Err(TypedRegionError::UnknownCarrier {
                            anchor: entry.anchor.clone(),
                        });
                    };
                    check_range_width(entry, *start, *len, width)?;
                }
                TypedAnchor::Storage { storage, ty, .. } => {
                    if !vcircuit_uses_storage(host, *storage, *ty) {
                        return Err(TypedRegionError::UnknownStorageSpace {
                            storage: *storage,
                            ty: *ty,
                        });
                    }
                }
                TypedAnchor::BlockInput { .. } | TypedAnchor::FuncInput { .. }
                | TypedAnchor::FuncOutput { .. } => {
                    return Err(TypedRegionError::UnsupportedAnchor {
                        anchor: entry.anchor.clone(),
                        host: "VCircuit",
                    });
                }
            }
        }
        Ok(())
    }

    /// Validate against a (possibly multi-block) Volar IR host. `Input`
    /// anchors address block 0's params; `Output` anchors are rejected
    /// (`IRBlocks` has no module-level output list).
    pub fn validate_irblocks(
        &self,
        host: &IRBlocks,
        types: &IRTypes,
    ) -> Result<(), TypedRegionError> {
        self.validate_structure()?;
        for entry in &self.entries {
            match &entry.anchor {
                TypedAnchor::Input { param, start, len } => match host.blocks.first() {
                    Some(b0) => check_range(
                        entry,
                        *param,
                        &b0.params,
                        |&ty| typed_bit_width(ty, types),
                        *start,
                        *len,
                    )?,
                    None => {
                        return Err(TypedRegionError::UnknownCarrier {
                            anchor: entry.anchor.clone(),
                        });
                    }
                },
                TypedAnchor::BlockInput {
                    block,
                    param,
                    start,
                    len,
                } => match host.blocks.get(block.0 as usize) {
                    Some(b) => check_range(
                        entry,
                        *param,
                        &b.params,
                        |&ty| typed_bit_width(ty, types),
                        *start,
                        *len,
                    )?,
                    None => {
                        return Err(TypedRegionError::UnknownCarrier {
                            anchor: entry.anchor.clone(),
                        });
                    }
                },
                TypedAnchor::Storage { storage, ty, .. } => {
                    if !irblocks_uses_storage(host, *storage, *ty) {
                        return Err(TypedRegionError::UnknownStorageSpace {
                            storage: *storage,
                            ty: *ty,
                        });
                    }
                }
                TypedAnchor::Output { .. } | TypedAnchor::FuncInput { .. }
                | TypedAnchor::FuncOutput { .. } => {
                    return Err(TypedRegionError::UnsupportedAnchor {
                        anchor: entry.anchor.clone(),
                        host: "IRBlocks",
                    });
                }
            }
        }
        Ok(())
    }
}

fn check_range(
    entry: &TypedRegionEntry,
    param: u32,
    params: &[IRTypeId],
    width_of: impl Fn(&IRTypeId) -> Option<usize>,
    start: u16,
    len: u16,
) -> Result<(), TypedRegionError> {
    let Some(ty) = params.get(param as usize) else {
        return Err(TypedRegionError::UnknownCarrier {
            anchor: entry.anchor.clone(),
        });
    };
    let Some(width) = width_of(ty) else {
        return Err(TypedRegionError::UnknownCarrier {
            anchor: entry.anchor.clone(),
        });
    };
    check_range_width(entry, start, len, width)
}

fn check_range_width(
    entry: &TypedRegionEntry,
    start: u16,
    len: u16,
    width: usize,
) -> Result<(), TypedRegionError> {
    if (start as usize).saturating_add(len as usize) > width {
        return Err(TypedRegionError::RangeOutOfRange {
            anchor: entry.anchor.clone(),
            start,
            len,
            width,
        });
    }
    Ok(())
}

fn vcircuit_uses_storage(host: &VCircuit, storage: StorageId, ty: IRTypeId) -> bool {
    host.stmts.iter().any(|s| {
        matches!(
            &s.kind,
            IRStmt::StorageRead {
                storage: s,
                ty: t,
                ..
            }
            | IRStmt::StorageWrite {
                storage: s,
                ty: t,
                ..
            } if *s == storage && *t == ty
        )
    })
}

fn irblocks_uses_storage(host: &IRBlocks, storage: StorageId, ty: IRTypeId) -> bool {
    host.blocks.iter().any(|b| {
        b.stmts.iter().any(|s| {
            matches!(
                &s.kind,
                IRStmt::StorageRead {
                    storage: s,
                    ty: t,
                    ..
                }
                | IRStmt::StorageWrite {
                    storage: s,
                    ty: t,
                    ..
                } if *s == storage && *t == ty
            )
        })
    }) || host
        .pre_init
        .iter()
        .any(|seg| seg.storage == storage && seg.ty == ty)
}

// ============================================================================
// Typed gadgets
// ============================================================================

/// One named typed gadget port: `count` values of type `ty`
/// (total bit width `count × typed_bit_width(ty)`).
#[derive(Clone, PartialEq, Eq, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct TypedPort {
    pub name: String,
    pub kind: PortKind,
    pub ty: IRTypeId,
    pub count: usize,
}

/// A typed gadget: named sub-computation plus its typed port signature.
///
/// Body conventions mirror the landed bit-level [`crate::gadget::GadgetSpec`]:
/// the body's params are the port words flattened in port declaration order
/// (conventionally `Data` first, then `Aux`, then `Rng`), and the body's
/// outputs are the `Data` port words in declaration order. Bodies must be
/// pure typed computations (`Const`, `Transmute`, `Poly`, `Rol`, `Ror`,
/// `Merge`, `Splat`, `Shuffle`); storage, oracle, action, and RNG statements
/// are rejected fail-closed by [`validate_typed_gadget`].
#[derive(Clone, PartialEq, Eq, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct TypedGadgetSpec {
    pub name: String,
    pub ports: Vec<TypedPort>,
    /// Encrypt direction: `data(plain) ⊗ aux → data(cipher)`.
    pub encrypt: VCircuit,
    /// Decrypt direction. `None` = self-inverse (reuse `encrypt`).
    pub decrypt: Option<VCircuit>,
}

impl TypedGadgetSpec {
    /// The first `Data` port, if any.
    pub fn data_port(&self) -> Option<&TypedPort> {
        self.ports.iter().find(|p| p.kind == PortKind::Data)
    }

    /// Total bit width of the `Data` port.
    pub fn data_bit_width(&self, types: &IRTypes) -> Option<usize> {
        let p = self.data_port()?;
        typed_bit_width(p.ty, types).map(|w| w.saturating_mul(p.count))
    }

    /// The body to use in the given direction.
    pub fn body_for(&self, decrypt: bool) -> &VCircuit {
        match (decrypt, &self.decrypt) {
            (true, Some(d)) => d,
            _ => &self.encrypt,
        }
    }

    /// Flattened body param types: port words in declaration order.
    pub fn port_word_types(&self) -> Vec<IRTypeId> {
        let mut out = Vec::new();
        for p in &self.ports {
            for _ in 0..p.count {
                out.push(p.ty);
            }
        }
        out
    }

    /// Bit widths of the `Aux` ports, in declaration order.
    pub fn aux_bit_widths(&self, types: &IRTypes) -> Vec<(String, usize)> {
        self.ports
            .iter()
            .filter(|p| p.kind == PortKind::Aux)
            .map(|p| {
                let w = typed_bit_width(p.ty, types).unwrap_or(0) * p.count;
                (p.name.clone(), w)
            })
            .collect()
    }
}

/// Source for one typed `Aux` port. The needed width is implied by the port
/// declaration (fail-closed mismatch at lowering time).
#[derive(Clone, PartialEq, Eq, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub enum TypedAuxSource {
    /// Bits `[start, start + port_bits)` of host input param `param`. These
    /// bits pass through the splice untransformed. Must not span past the
    /// param's width (fail-closed).
    InputRange { param: u32, start: u16 },
    /// One [`Constant`] per port word (the port's `count`).
    Const(Vec<Constant>),
    /// A named RNG source (not supported by the v1 splicer; carried for
    /// forward compatibility, mirroring the bit-level type surface).
    Rng(String),
}

/// One typed attachment: "wrap the wires selected by `selector` with typed
/// gadget `gadget`".
#[derive(Clone, PartialEq, Eq, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct TypedGadgetBinding {
    /// Key into the [`TypedGadgetLibrary`].
    pub gadget: String,
    pub selector: RegionSelector,
    /// Source for each `Aux` port, in gadget port declaration order
    /// (skipping `Data` and `Rng` ports).
    pub aux_sources: Vec<TypedAuxSource>,
    /// Name of the RNG source used for `Rng` ports (if any).
    pub rng_source: Option<String>,
}

/// Library of typed gadgets, keyed by [`TypedGadgetSpec::name`].
#[derive(Clone, PartialEq, Eq, Default, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct TypedGadgetLibrary {
    pub gadgets: Vec<TypedGadgetSpec>,
}

impl TypedGadgetLibrary {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, spec: TypedGadgetSpec) -> Self {
        self.gadgets.push(spec);
        self
    }

    pub fn get(&self, name: &str) -> Option<&TypedGadgetSpec> {
        self.gadgets.iter().find(|g| g.name == name)
    }
}

/// Errors from typed gadget-spec validation.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum TypedGadgetError {
    /// v1 requires exactly one `Data` port.
    NotExactlyOneDataPort { gadget: String, found: usize },
    /// The body uses a statement kind with no pure Boolean lowering.
    UnsupportedBodyStmt { gadget: String, stmt: &'static str },
    /// The body's params do not match the flattened port words.
    BodyParamMismatch { gadget: String, reason: &'static str },
    /// The body's outputs do not match the `Data` port words.
    BodyOutputMismatch { gadget: String, reason: &'static str },
    /// A port type has no Boolean bit width (e.g. `Z3`).
    UnsupportedWidth { gadget: String, ty: IRTypeId },
}

/// Statement kinds allowed in a typed gadget body: everything with a pure
/// Boolean lowering. Storage, oracle, action, and RNG statements are
/// rejected.
fn is_pure_typed_stmt(stmt: &IRStmt) -> Option<&'static str> {
    match stmt {
        IRStmt::Const(..)
        | IRStmt::Transmute { .. }
        | IRStmt::Poly { .. }
        | IRStmt::Rol { .. }
        | IRStmt::Ror { .. }
        | IRStmt::Merge { .. }
        | IRStmt::Splat { .. }
        | IRStmt::Shuffle { .. } => None,
        IRStmt::StorageRead { .. } => Some("StorageRead"),
        IRStmt::StorageWrite { .. } => Some("StorageWrite"),
        IRStmt::OracleCall { .. } | IRStmt::OracleOutput { .. } => Some("Oracle"),
        IRStmt::ActionCall { .. }
        | IRStmt::ActionOutput { .. }
        | IRStmt::ActionStore { .. } => Some("Action"),
        IRStmt::Rng { .. } => Some("Rng"),
        _ => Some("unknown-stmt"),
    }
}

/// Validate a typed gadget's port signature and body shapes against
/// `types`. Both direction bodies (encrypt, and decrypt when present) are
/// checked.
pub fn validate_typed_gadget(
    spec: &TypedGadgetSpec,
    types: &IRTypes,
) -> Result<(), TypedGadgetError> {
    let data_count = spec.ports.iter().filter(|p| p.kind == PortKind::Data).count();
    if data_count != 1 {
        return Err(TypedGadgetError::NotExactlyOneDataPort {
            gadget: spec.name.clone(),
            found: data_count,
        });
    }
    for port in &spec.ports {
        if typed_bit_width(port.ty, types).is_none() {
            return Err(TypedGadgetError::UnsupportedWidth {
                gadget: spec.name.clone(),
                ty: port.ty,
            });
        }
    }

    let word_types = spec.port_word_types();
    let data_outputs: Vec<IRTypeId> = spec
        .data_port()
        .map(|p| alloc::vec![p.ty; p.count])
        .unwrap_or_default();

    for direction in ["encrypt", "decrypt"] {
        let body = match (direction, &spec.decrypt) {
            ("decrypt", Some(d)) => d,
            ("decrypt", None) => continue, // self-inverse: checked via encrypt
            _ => &spec.encrypt,
        };
        for node in &body.stmts {
            if let Some(stmt) = is_pure_typed_stmt(&node.kind) {
                return Err(TypedGadgetError::UnsupportedBodyStmt {
                    gadget: spec.name.clone(),
                    stmt,
                });
            }
        }
        if body.params != word_types {
            return Err(TypedGadgetError::BodyParamMismatch {
                gadget: spec.name.clone(),
                reason: "body params must equal the flattened port words in declaration order",
            });
        }
        // Outputs: one var per data word, with the data word's type.
        if body.outputs.len() != data_outputs.len() {
            return Err(TypedGadgetError::BodyOutputMismatch {
                gadget: spec.name.clone(),
                reason: "body must output exactly one var per Data port word",
            });
        }
        let var_ty = |var: IRVarId| -> Option<IRTypeId> {
            let idx = var.0 as usize;
            if idx < body.params.len() {
                Some(body.params[idx])
            } else {
                body.stmts
                    .get(idx - body.params.len())
                    .and_then(|node| stmt_output_type(&node.kind))
            }
        };
        for (i, &want) in data_outputs.iter().enumerate() {
            match var_ty(body.outputs[i]) {
                Some(ty) if ty == want => {}
                _ => {
                    return Err(TypedGadgetError::BodyOutputMismatch {
                        gadget: spec.name.clone(),
                        reason: "output var missing or of the wrong type",
                    });
                }
            }
        }
    }
    Ok(())
}

/// Output type of a pure typed statement (the only kinds legal in a
/// validated body). `None` for value-less statements.
fn stmt_output_type(stmt: &IRStmt) -> Option<IRTypeId> {
    match stmt {
        IRStmt::Const(_, ty) => Some(*ty),
        IRStmt::Transmute { dst_ty, .. } => Some(*dst_ty),
        IRStmt::Poly { ty, .. }
        | IRStmt::Rol { ty, .. }
        | IRStmt::Ror { ty, .. }
        | IRStmt::Merge { ty, .. }
        | IRStmt::Splat { ty, .. }
        | IRStmt::Shuffle { ty, .. } => Some(*ty),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{IRBlock, IRBlockTargetId, IRBranchTarget, IRBlocks, IRTerminator, IRType};
    use alloc::string::ToString;
    use alloc::vec;
    use volar_ir_common::{Node, TypeTable};

    fn types() -> IRTypes {
        let mut t = TypeTable::new();
        let bit = t.intern(IRType::Primitive(volar_ir_common::Type::Bit));
        let w8 = t.intern(IRType::Primitive(volar_ir_common::Type::_8));
        let w32 = t.intern(IRType::Primitive(volar_ir_common::Type::_32));
        let _ = (bit, w8, w32);
        t
    }

    fn ty_of(types: &mut IRTypes, ty: IRType) -> IRTypeId {
        types.intern(ty)
    }

    // ---- structural validation -------------------------------------------

    fn input_entry(param: u32, start: u16, len: u16, regions: &[u32]) -> TypedRegionEntry {
        TypedRegionEntry {
            anchor: TypedAnchor::Input { param, start, len },
            regions: regions.iter().map(|&r| RegionId(r)).collect(),
        }
    }

    #[test]
    fn structure_rejects_overlap_and_unsorted() {
        let table = TypedRegionTable {
            entries: vec![
                input_entry(0, 0, 4, &[1]),
                input_entry(0, 2, 2, &[2]), // overlaps the first
            ],
            names: BTreeMap::new(),
        };
        assert!(matches!(
            table.validate_structure(),
            Err(TypedRegionError::OverlappingEntries { .. })
        ));

        let table = TypedRegionTable {
            entries: vec![
                input_entry(1, 0, 4, &[1]),
                input_entry(0, 0, 4, &[2]), // out of order
            ],
            names: BTreeMap::new(),
        };
        assert!(matches!(
            table.validate_structure(),
            Err(TypedRegionError::Unsorted)
        ));
    }

    #[test]
    fn structure_allows_same_bits_in_different_carriers() {
        let table = TypedRegionTable {
            entries: vec![
                input_entry(0, 0, 4, &[1]),
                TypedRegionEntry {
                    anchor: TypedAnchor::BlockInput {
                        block: IRBlockId(0),
                        param: 0,
                        start: 0,
                        len: 4,
                    },
                    regions: BTreeSet::from([RegionId(2)]),
                },
            ],
            names: BTreeMap::new(),
        };
        assert!(table.validate_structure().is_ok());
    }

    // ---- VCircuit validation ---------------------------------------------

    fn const_circuit(types: &IRTypes, w8: IRTypeId) -> VCircuit {
        let mut vc = VCircuit::new(vec![w8, w8]);
        // out = const 0x5a (8 bits)
        let c = Constant { hi: 0, lo: 0x5a };
        let _ = vc.push_stmt(IRStmt::Const(c, w8), ());
        vc.outputs = vec![IRVarId(0), IRVarId(2)];
        let _ = types;
        vc
    }

    #[test]
    fn vcircuit_validation_bounds() {
        let mut types = types();
        let w8 = ty_of(&mut types, IRType::Primitive(volar_ir_common::Type::_8));
        let host = const_circuit(&types, w8);

        let ok = TypedRegionTable {
            entries: vec![
                input_entry(0, 2, 3, &[1]),
                TypedRegionEntry {
                    anchor: TypedAnchor::Output {
                        out: 1,
                        start: 0,
                        len: 8,
                    },
                    regions: BTreeSet::from([RegionId(3)]),
                },
            ],
            names: BTreeMap::new(),
        };
        assert_eq!(ok.validate_vcircuit(&host, &types), Ok(()));

        let oob = TypedRegionTable {
            entries: vec![input_entry(0, 6, 4, &[1])],
            names: BTreeMap::new(),
        };
        assert!(matches!(
            oob.validate_vcircuit(&host, &types),
            Err(TypedRegionError::RangeOutOfRange { width: 8, .. })
        ));

        let bad_out = TypedRegionTable {
            entries: vec![TypedRegionEntry {
                anchor: TypedAnchor::Output {
                    out: 5,
                    start: 0,
                    len: 1,
                },
                regions: BTreeSet::from([RegionId(1)]),
            }],
            names: BTreeMap::new(),
        };
        assert!(matches!(
            bad_out.validate_vcircuit(&host, &types),
            Err(TypedRegionError::UnknownCarrier { .. })
        ));
    }

    #[test]
    fn vcircuit_rejects_block_anchors() {
        let mut types = types();
        let w8 = ty_of(&mut types, IRType::Primitive(volar_ir_common::Type::_8));
        let host = const_circuit(&types, w8);
        let table = TypedRegionTable {
            entries: vec![TypedRegionEntry {
                anchor: TypedAnchor::BlockInput {
                    block: IRBlockId(0),
                    param: 0,
                    start: 0,
                    len: 1,
                },
                regions: BTreeSet::from([RegionId(1)]),
            }],
            names: BTreeMap::new(),
        };
        assert!(matches!(
            table.validate_vcircuit(&host, &types),
            Err(TypedRegionError::UnsupportedAnchor { host: "VCircuit", .. })
        ));
    }

    // ---- IRBlocks validation ---------------------------------------------

    fn irblocks_with_storage(_types: &IRTypes, w8: IRTypeId) -> IRBlocks {
        use volar_ir_common::StorageId;
        let s = StorageId(7);
        let mut b0 = IRBlock {
            params: vec![w8],
            stmts: vec![],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget {
                    dest: IRBlockTargetId::Return,
                    args: vec![],
                    reentry: None,
                },
            },
        };
        b0.stmts.push(Node {
            kind: IRStmt::StorageWrite {
                storage: s,
                src: IRVarId(0),
                ty: w8,
                addr: IRVarId(0),
            },
            prov: (),
            side: None,
        });
        IRBlocks {
            oracles: vec![],
            actions: vec![],
            rngs: vec![],
            blocks: vec![b0],
            pre_init: vec![],
        }
    }

    #[test]
    fn irblocks_storage_and_block_anchors() {
        use volar_ir_common::StorageId;
        let mut types = types();
        let w8 = ty_of(&mut types, IRType::Primitive(volar_ir_common::Type::_8));
        let host = irblocks_with_storage(&types, w8);

        let ok = TypedRegionTable {
            entries: vec![
                input_entry(0, 0, 8, &[1]),
                TypedRegionEntry {
                    anchor: TypedAnchor::BlockInput {
                        block: IRBlockId(0),
                        param: 0,
                        start: 0,
                        len: 8,
                    },
                    regions: BTreeSet::from([RegionId(2)]),
                },
                TypedRegionEntry {
                    anchor: TypedAnchor::Storage {
                        storage: StorageId(7),
                        ty: w8,
                        addr_start: 0,
                        addr_len: 4,
                    },
                    regions: BTreeSet::from([RegionId(3)]),
                },
            ],
            names: BTreeMap::new(),
        };
        assert_eq!(ok.validate_irblocks(&host, &types), Ok(()));

        let unknown = TypedRegionTable {
            entries: vec![TypedRegionEntry {
                anchor: TypedAnchor::Storage {
                    storage: StorageId(9),
                    ty: w8,
                    addr_start: 0,
                    addr_len: 1,
                },
                regions: BTreeSet::from([RegionId(3)]),
            }],
            names: BTreeMap::new(),
        };
        assert!(matches!(
            unknown.validate_irblocks(&host, &types),
            Err(TypedRegionError::UnknownStorageSpace { .. })
        ));
    }

    #[test]
    fn irblocks_rejects_output_anchors() {
        let mut types = types();
        let w8 = ty_of(&mut types, IRType::Primitive(volar_ir_common::Type::_8));
        let host = irblocks_with_storage(&types, w8);
        let table = TypedRegionTable {
            entries: vec![TypedRegionEntry {
                anchor: TypedAnchor::Output {
                    out: 0,
                    start: 0,
                    len: 1,
                },
                regions: BTreeSet::from([RegionId(1)]),
            }],
            names: BTreeMap::new(),
        };
        assert!(matches!(
            table.validate_irblocks(&host, &types),
            Err(TypedRegionError::UnsupportedAnchor { host: "IRBlocks", .. })
        ));
    }

    // ---- typed gadget validation -----------------------------------------

    fn xor_body(types: &IRTypes, w8: IRTypeId) -> VCircuit {
        let _ = types;
        let mut vc = VCircuit::new(vec![w8, w8]);
        let coeffs = BTreeMap::from([(vec![IRVarId(0), IRVarId(1)], 1u8)]);
        let out = vc.push_stmt(
            IRStmt::Poly {
                ty: w8,
                coeffs,
                constant: Constant { hi: 0, lo: 0 },
            },
            (),
        );
        vc.outputs = vec![out];
        vc
    }

    #[test]
    fn typed_gadget_validates() {
        let mut types = types();
        let w8 = ty_of(&mut types, IRType::Primitive(volar_ir_common::Type::_8));
        let spec = TypedGadgetSpec {
            name: "pad8".to_string(),
            ports: vec![
                TypedPort {
                    name: "data".to_string(),
                    kind: PortKind::Data,
                    ty: w8,
                    count: 1,
                },
                TypedPort {
                    name: "key".to_string(),
                    kind: PortKind::Aux,
                    ty: w8,
                    count: 1,
                },
            ],
            encrypt: xor_body(&types, w8),
            decrypt: None,
        };
        assert_eq!(validate_typed_gadget(&spec, &types), Ok(()));
        assert_eq!(spec.data_bit_width(&types), Some(8));
        assert_eq!(spec.aux_bit_widths(&types), vec![("key".to_string(), 8)]);
    }

    #[test]
    fn typed_gadget_rejects_impure_body_and_bad_shape() {
        use volar_ir_common::StorageId;
        let mut types = types();
        let w8 = ty_of(&mut types, IRType::Primitive(volar_ir_common::Type::_8));

        // Impure: storage read in the body.
        let mut bad = VCircuit::new(vec![w8, w8]);
        let r = bad.push_stmt(
            IRStmt::StorageRead {
                storage: StorageId(0),
                ty: w8,
                addr: IRVarId(0),
            },
            (),
        );
        bad.outputs = vec![r];
        let spec = TypedGadgetSpec {
            name: "bad".to_string(),
            ports: vec![TypedPort {
                name: "data".to_string(),
                kind: PortKind::Data,
                ty: w8,
                count: 1,
            }],
            encrypt: bad,
            decrypt: None,
        };
        assert!(matches!(
            validate_typed_gadget(&spec, &types),
            Err(TypedGadgetError::UnsupportedBodyStmt { stmt: "StorageRead", .. })
        ));

        // Wrong param shape: body takes one word too many.
        let body = xor_body(&types, w8); // 2 params, but only 1 port word
        let spec = TypedGadgetSpec {
            name: "shape".to_string(),
            ports: vec![TypedPort {
                name: "data".to_string(),
                kind: PortKind::Data,
                ty: w8,
                count: 1,
            }],
            encrypt: body,
            decrypt: None,
        };
        assert!(matches!(
            validate_typed_gadget(&spec, &types),
            Err(TypedGadgetError::BodyParamMismatch { .. })
        ));
    }

    // ---- select ------------------------------------------------------------

    #[test]
    fn select_expands_typed_bits_and_planes() {
        use volar_ir_common::StorageId;
        let mut types = types();
        let w8 = ty_of(&mut types, IRType::Primitive(volar_ir_common::Type::_8));
        let table = TypedRegionTable {
            entries: vec![
                input_entry(2, 1, 2, &[5]),
                TypedRegionEntry {
                    anchor: TypedAnchor::Storage {
                        storage: StorageId(1),
                        ty: w8,
                        addr_start: 3,
                        addr_len: 2,
                    },
                    regions: BTreeSet::from([RegionId(6)]),
                },
            ],
            names: BTreeMap::new(),
        };
        let sel = RegionSelector::all_of([RegionId(6)]);
        let wires = table.select(&sel, |ty| typed_bit_width(ty, &types));
        assert_eq!(wires.len(), 2 * 8); // 2 addresses × 8 planes
        assert!(matches!(
            wires[0],
            TypedBoundaryWire::Cell { plane: 0, addr: 3, .. }
        ));
        assert!(matches!(
            wires[1],
            TypedBoundaryWire::Cell { plane: 0, addr: 4, .. }
        ));
        assert!(matches!(
            wires[2],
            TypedBoundaryWire::Cell { plane: 1, addr: 3, .. }
        ));
    }
}

#[cfg(all(test, feature = "rkyv"))]
mod rkyv_tests {
    use super::*;
    use crate::region::RegionId;
    use alloc::string::ToString;
    use alloc::vec;

    #[test]
    fn typed_region_table_rkyv_roundtrip() {
        let table = TypedRegionTable {
            entries: vec![
                TypedRegionEntry {
                    anchor: TypedAnchor::Input {
                        param: 0,
                        start: 1,
                        len: 3,
                    },
                    regions: BTreeSet::from([RegionId(0), RegionId(1)]),
                },
                TypedRegionEntry {
                    anchor: TypedAnchor::Storage {
                        storage: StorageId(2),
                        ty: IRTypeId(1),
                        addr_start: 4,
                        addr_len: 6,
                    },
                    regions: BTreeSet::from([RegionId(2)]),
                },
            ],
            names: BTreeMap::from([(RegionId(0), "public".to_string())]),
        };
        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&table).expect("serializes");
        let archived =
            rkyv::access::<ArchivedTypedRegionTable, rkyv::rancor::Error>(&bytes).expect("validates");
        let round: TypedRegionTable =
            rkyv::deserialize::<TypedRegionTable, rkyv::rancor::Error>(archived)
                .expect("deserializes");
        assert_eq!(round, table);
    }

    #[test]
    fn typed_gadget_types_rkyv_roundtrip() {
        let mut body = VCircuit::new(vec![IRTypeId(0), IRTypeId(0)]);
        let coeffs = BTreeMap::from([(vec![IRVarId(0), IRVarId(1)], 1u8)]);
        let out = body.push_stmt(
            IRStmt::Poly {
                ty: IRTypeId(0),
                coeffs,
                constant: Constant { hi: 0, lo: 0 },
            },
            (),
        );
        body.outputs = vec![out];
        let lib = TypedGadgetLibrary::new().with(TypedGadgetSpec {
            name: "pad".to_string(),
            ports: vec![
                TypedPort {
                    name: "data".to_string(),
                    kind: PortKind::Data,
                    ty: IRTypeId(0),
                    count: 1,
                },
                TypedPort {
                    name: "key".to_string(),
                    kind: PortKind::Aux,
                    ty: IRTypeId(0),
                    count: 1,
                },
            ],
            encrypt: body.clone(),
            decrypt: Some(body),
        });
        let binding = TypedGadgetBinding {
            gadget: "pad".to_string(),
            selector: crate::region::RegionSelector {
                all_of: BTreeSet::from([RegionId(0)]),
                none_of: BTreeSet::from([RegionId(1)]),
            },
            aux_sources: vec![
                TypedAuxSource::Const(vec![Constant { hi: 0, lo: 7 }]),
                TypedAuxSource::InputRange { param: 1, start: 0 },
            ],
            rng_source: None,
        };

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&lib).expect("serializes");
        let archived =
            rkyv::access::<ArchivedTypedGadgetLibrary, rkyv::rancor::Error>(&bytes).expect("validates");
        let round: TypedGadgetLibrary =
            rkyv::deserialize::<TypedGadgetLibrary, rkyv::rancor::Error>(archived)
                .expect("deserializes");
        assert_eq!(round, lib);

        let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&binding).expect("serializes");
        let archived =
            rkyv::access::<ArchivedTypedGadgetBinding, rkyv::rancor::Error>(&bytes).expect("validates");
        let round: TypedGadgetBinding =
            rkyv::deserialize::<TypedGadgetBinding, rkyv::rancor::Error>(archived)
                .expect("deserializes");
        assert_eq!(round, binding);
    }
}
