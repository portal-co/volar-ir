// @reliability: normal
// @ai: assisted
//! Region/gadget companion-metadata lowering and pass threading.
//!
//! Two jobs, both *pure functions over already-emitted metadata* (no pass
//! signature changes; see `docs/typed-gadgets-and-region-threading-plan.md`):
//!
//! 1. **Typed → bit lowering.** A [`TypedRegionTable`] authored against a
//!    typed host is lowered onto the landed bit-level
//!    [`RegionTable`](volar_ir::region::RegionTable) using the *same*
//!    allocation the host's own `lower_ir_to_boolar` run produced
//!    ([`LoweredTables`]): param anchors shift by the contiguous param bit
//!    allocation, output anchors by the typed output widths, and typed
//!    storage anchors fan out to one flat-cell anchor per bit plane
//!    (`addr + (plane << N)`). Typed gadget libraries/binding aux sources
//!    lower the same way; the splice itself stays in `apply_gadgets`.
//!
//! 2. **Translation across boundary-moving passes.**
//!    - [`translate_regions_movfuscate`]: pre-movfuscation typed anchors →
//!      combined-block state slots (`slot_of` from movfuscate's own
//!      [`compute_static_slot_classes`]).
//!    - [`movfuscate_state_regions`]: region anchors for the *internal
//!      slots movfuscation invents* — the PC bits and each state slot.
//!    - [`translate_typed_aux_movfuscate`]: same remap for typed aux
//!      `InputRange` sources.
//!    - [`translate_regions_termination_flag`]: landed-table output anchors
//!      shift by one position under `LoweringMode::WithTerminationFlag`.
//!    - [`translate_regions_to_reversible`]: landed anchors onto the
//!      reversible wire space via [`VarWireMap`].
//!
//! Everything fails closed: an anchor whose carrier vanished errors, it is
//! never silently dropped.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use volar_ir::circuit::VCircuit;
use volar_ir::gadget::{
    AuxSource, GadgetBinding, GadgetLibrary, GadgetSpec, Port, PortKind,
};
use volar_ir::ir::IRBlocks;
use volar_ir::region::{RegionEntry, RegionTable};
use volar_ir::typed_gadget::TypedRegionError;
use volar_ir::typed_gadget::{
    constant_bit, typed_bit_width, TypedAnchor, TypedAuxSource, TypedGadgetBinding,
    TypedGadgetError, TypedGadgetLibrary, TypedGadgetSpec, TypedRegionTable,
};
use volar_ir::boolar::LaneId;
use volar_ir_common::{StorageId, Type};

use crate::lower_ir_to_boolar::{LoweredTables, ExternalLoweringError};
use crate::movfuscate::{compute_return_slot_types, compute_static_slot_classes};
use crate::to_reversible::{VarWireMap, UnknownVar};

// ============================================================================
// Typed table → bit-level table
// ============================================================================

/// Errors from typed → bit metadata lowering.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LowerTypedError {
    /// The typed table failed structural validation.
    Region(TypedRegionError),
    /// The anchor's carrier has no bit allocation in the lowering run.
    UnknownAnchorCarrier(TypedAnchor),
    /// The anchor kind is not supported at this lowering level (e.g.
    /// `Output` without typed output widths).
    UnsupportedAnchor { anchor: TypedAnchor, level: &'static str },
    /// The anchor's bit range extends past the carrier's lowered width.
    RangeOutOfRange {
        anchor: TypedAnchor,
        start: u16,
        len: u16,
        width: usize,
    },
    /// A storage-anchor type has no lane in the lowering run (no traffic).
    UnknownStorageLane { storage: StorageId, ty: volar_ir::ir::IRTypeId },
    /// A wrapped `(StorageId, LaneId)` space has no recorded element-address
    /// width.
    MissingAddrWidth { storage: StorageId, lane: LaneId },
    /// A storage anchor's flat-cell fan-out overflows `u64`.
    StorageRangeOverflow(TypedAnchor),
    /// The bound gadget name is not in the typed library.
    UnknownGadget(String),
    /// The number of aux sources does not match the declared `Aux` ports.
    AuxSourceCountMismatch { gadget: String, expected: usize, got: usize },
    /// A `Const` aux source has the wrong word count for its port.
    AuxWordCountMismatch {
        gadget: String,
        port: String,
        expected_words: usize,
        got_words: usize,
    },
    /// An aux `InputRange` extends past the host param's lowered width.
    AuxRangeOutOfRange {
        param: u32,
        start: u16,
        len: u16,
        width: usize,
    },
}

impl From<TypedRegionError> for LowerTypedError {
    fn from(e: TypedRegionError) -> Self {
        LowerTypedError::Region(e)
    }
}

/// Lower a typed region table onto the bit level.
///
/// `tables` must come from the `lower_ir_to_boolar` run of the *same host*
/// the anchors are expressed against (post-translation if a pass moved the
/// boundary). `output_widths` is required for `Output` anchors: the Boolean
/// bit width of each typed output var, in output order (see
/// [`vcircuit_output_widths`]).
pub fn lower_typed_region_table(
    table: &TypedRegionTable,
    tables: &LoweredTables,
    types: &volar_ir::ir::IRTypes,
    output_widths: Option<&[usize]>,
) -> Result<RegionTable, LowerTypedError> {
    table.validate_structure()?;
    let mut entries: Vec<RegionEntry> = Vec::new();
    for entry in &table.entries {
        match &entry.anchor {
            TypedAnchor::Input { param, start, len } => {
                let bit_start = param_bit_start(tables, 0, *param, entry, *start, *len)?;
                entries.push(RegionEntry {
                    anchor: volar_ir::region::WireAnchor::Input {
                        start: bit_start,
                        len: *len as u32,
                    },
                    regions: entry.regions.clone(),
                });
            }
            TypedAnchor::BlockInput {
                block,
                param,
                start,
                len,
            } => {
                let bit_start =
                    param_bit_start(tables, block.0 as usize, *param, entry, *start, *len)?;
                entries.push(RegionEntry {
                    anchor: volar_ir::region::WireAnchor::Input {
                        start: bit_start,
                        len: *len as u32,
                    },
                    regions: entry.regions.clone(),
                });
            }
            TypedAnchor::Output { out, start, len } => {
                let Some(widths) = output_widths else {
                    return Err(LowerTypedError::UnsupportedAnchor {
                        anchor: entry.anchor.clone(),
                        level: "bit (no typed output widths supplied)",
                    });
                };
                let base: usize = widths
                    .get(..*out as usize)
                    .ok_or_else(|| carrier_error(entry))?
                    .iter()
                    .sum();
                let width = *widths
                    .get(*out as usize)
                    .ok_or_else(|| carrier_error(entry))?;
                check_range(entry, *start, *len, width)?;
                entries.push(RegionEntry {
                    anchor: volar_ir::region::WireAnchor::Output {
                        start: (base + *start as usize) as u32,
                        len: *len as u32,
                    },
                    regions: entry.regions.clone(),
                });
            }
            TypedAnchor::Storage {
                storage,
                ty,
                addr_start,
                addr_len,
            } => {
                let lane = lane_of(tables, *ty)
                    .ok_or(LowerTypedError::UnknownStorageLane {
                        storage: *storage,
                        ty: *ty,
                    })?;
                let n = *tables
                    .addr_widths
                    .get(&(*storage, lane))
                    .ok_or(LowerTypedError::MissingAddrWidth {
                        storage: *storage,
                        lane,
                    })? as u64;
                let planes = typed_bit_width(*ty, types)
                    .ok_or_else(|| carrier_error(entry))? as u64;
                for plane in 0..planes {
                    let start = addr_start
                        .checked_add(plane << n)
                        .ok_or_else(|| LowerTypedError::StorageRangeOverflow(entry.anchor.clone()))?;
                    start
                        .checked_add(*addr_len)
                        .ok_or_else(|| LowerTypedError::StorageRangeOverflow(entry.anchor.clone()))?;
                    entries.push(RegionEntry {
                        anchor: volar_ir::region::WireAnchor::Storage {
                            storage: *storage,
                            lane,
                            start,
                            len: *addr_len,
                        },
                        regions: entry.regions.clone(),
                    });
                }
            }
            anchor @ (TypedAnchor::FuncInput { .. } | TypedAnchor::FuncOutput { .. }) => {
                return Err(LowerTypedError::UnsupportedAnchor {
                    anchor: anchor.clone(),
                    level: "bit (VAFFLE anchors lower via volar-vaffle-target first)",
                });
            }
        }
    }

    // Per-plane fan-out can interleave across typed entries; the landed
    // table requires ascending anchor order. Overlap is impossible after
    // structural validation (per-space disjointness + distinct lanes per
    // type), but a duplicate/overlap check keeps this fail-closed.
    entries.sort_by(|a, b| a.anchor.cmp(&b.anchor));
    for w in entries.windows(2) {
        if w[0].anchor == w[1].anchor {
            return Err(LowerTypedError::Region(
                TypedRegionError::OverlappingEntries {
                    first: typed_view(&w[0].anchor),
                    second: typed_view(&w[1].anchor),
                },
            ));
        }
    }

    Ok(RegionTable {
        entries,
        names: table.names.clone(),
    })
}

/// Boolean bit widths of a `VCircuit` host's outputs, in output order.
pub fn vcircuit_output_widths(host: &VCircuit, types: &volar_ir::ir::IRTypes) -> Vec<usize> {
    let widths = volar_ir::typed_gadget::vcircuit_var_widths(host, types);
    host.outputs
        .iter()
        .map(|v| widths.get(&v.0).copied().unwrap_or(0))
        .collect()
}

fn carrier_error(entry: &volar_ir::typed_gadget::TypedRegionEntry) -> LowerTypedError {
    LowerTypedError::UnknownAnchorCarrier(entry.anchor.clone())
}

fn check_range(
    entry: &volar_ir::typed_gadget::TypedRegionEntry,
    start: u16,
    len: u16,
    width: usize,
) -> Result<(), LowerTypedError> {
    if (start as usize).saturating_add(len as usize) > width {
        return Err(LowerTypedError::RangeOutOfRange {
            anchor: entry.anchor.clone(),
            start,
            len,
            width,
        });
    }
    Ok(())
}

fn param_bit_start(
    tables: &LoweredTables,
    block: usize,
    param: u32,
    entry: &volar_ir::typed_gadget::TypedRegionEntry,
    start: u16,
    len: u16,
) -> Result<u32, LowerTypedError> {
    let bits = tables
        .var_bits
        .bits(block, param)
        .ok_or_else(|| carrier_error(entry))?;
    check_range(entry, start, len, bits.len())?;
    Ok(bits[start as usize].0)
}

fn lane_of(tables: &LoweredTables, ty: volar_ir::ir::IRTypeId) -> Option<LaneId> {
    tables.lanes.iter().find(|(_, t)| **t == ty).map(|(l, _)| *l)
}

/// Best-effort typed view of a landed anchor for error reporting.
fn typed_view(anchor: &volar_ir::region::WireAnchor) -> TypedAnchor {
    match anchor {
        volar_ir::region::WireAnchor::Input { start, len } => TypedAnchor::Input {
            param: 0,
            start: *start as u16,
            len: *len as u16,
        },
        volar_ir::region::WireAnchor::Output { start, len } => TypedAnchor::Output {
            out: 0,
            start: *start as u16,
            len: *len as u16,
        },
        volar_ir::region::WireAnchor::Storage { storage, lane, start, len } => TypedAnchor::Storage {
            storage: *storage,
            ty: volar_ir::ir::IRTypeId(lane.0),
            addr_start: *start,
            addr_len: *len,
        },
    }
}

// ============================================================================
// Typed bindings & library → bit level
// ============================================================================

/// Lower typed gadget bindings against the bit-level table's host: aux
/// `InputRange` sources resolve through the same var-bit allocation, and
/// `Const` word vectors flatten to bit strings (LSB-first). Run *after* any
/// pass translation (see [`translate_typed_aux_movfuscate`]) so aux bits
/// refer to the final host.
pub fn lower_typed_bindings(
    bindings: &[TypedGadgetBinding],
    lib: &TypedGadgetLibrary,
    tables: &LoweredTables,
    types: &volar_ir::ir::IRTypes,
) -> Result<Vec<GadgetBinding>, LowerTypedError> {
    let mut out = Vec::with_capacity(bindings.len());
    for binding in bindings {
        let spec = lib
            .get(&binding.gadget)
            .ok_or_else(|| LowerTypedError::UnknownGadget(binding.gadget.clone()))?;
        let aux_ports: Vec<&volar_ir::typed_gadget::TypedPort> = spec
            .ports
            .iter()
            .filter(|p| p.kind == PortKind::Aux)
            .collect();
        if aux_ports.len() != binding.aux_sources.len() {
            return Err(LowerTypedError::AuxSourceCountMismatch {
                gadget: binding.gadget.clone(),
                expected: aux_ports.len(),
                got: binding.aux_sources.len(),
            });
        }
        let mut aux_sources = Vec::with_capacity(aux_ports.len());
        for (port, src) in aux_ports.iter().zip(&binding.aux_sources) {
            let width = typed_bit_width(port.ty, types)
                .ok_or(LowerTypedError::UnknownGadget(spec.name.clone()))?
                .saturating_mul(port.count);
            aux_sources.push(match src {
                TypedAuxSource::InputRange { param, start } => {
                    let bits = tables.var_bits.bits(0, *param).ok_or(
                        LowerTypedError::UnknownAnchorCarrier(entry_anchor_aux(*param)),
                    )?;
                    if (*start as usize).saturating_add(width) > bits.len() {
                        return Err(LowerTypedError::AuxRangeOutOfRange {
                            param: *param,
                            start: *start,
                            len: width as u16,
                            width: bits.len(),
                        });
                    }
                    AuxSource::InputRange {
                        start: bits[*start as usize].0,
                        len: width as u32,
                    }
                }
                TypedAuxSource::Const(words) => {
                    if words.len() != port.count {
                        return Err(LowerTypedError::AuxWordCountMismatch {
                            gadget: spec.name.clone(),
                            port: port.name.clone(),
                            expected_words: port.count,
                            got_words: words.len(),
                        });
                    }
                    let mut bits = Vec::with_capacity(width);
                    for word in words {
                        for b in 0..typed_bit_width(port.ty, types).unwrap_or(0) {
                            bits.push(constant_bit(word, b));
                        }
                    }
                    AuxSource::Const(bits)
                }
                TypedAuxSource::Rng(name) => AuxSource::Rng(name.clone()),
            });
        }
        out.push(GadgetBinding {
            gadget: binding.gadget.clone(),
            selector: binding.selector.clone(),
            aux_sources,
            rng_source: binding.rng_source.clone(),
        });
    }
    Ok(out)
}

fn entry_anchor_aux(param: u32) -> TypedAnchor {
    TypedAnchor::Input {
        param,
        start: 0,
        len: 0,
    }
}

/// Errors from typed gadget-library lowering.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum LowerGadgetError {
    /// Typed spec validation failed.
    Typed(TypedGadgetError),
    /// The typed body could not be booleanized.
    External(ExternalLoweringError),
    /// The booleanized body could not be fused.
    Fusion(volar_ir::circuit::CircuitFusionError),
    /// The lowered body's boundary shape does not match the declared ports.
    ShapeMismatch { gadget: String, reason: &'static str },
}

impl From<TypedGadgetError> for LowerGadgetError {
    fn from(e: TypedGadgetError) -> Self {
        LowerGadgetError::Typed(e)
    }
}

impl From<ExternalLoweringError> for LowerGadgetError {
    fn from(e: ExternalLoweringError) -> Self {
        LowerGadgetError::External(e)
    }
}

impl From<volar_ir::circuit::CircuitFusionError> for LowerGadgetError {
    fn from(e: volar_ir::circuit::CircuitFusionError) -> Self {
        LowerGadgetError::Fusion(e)
    }
}

/// Lower a typed gadget library onto the bit level: each typed body is
/// booleanized through the standard pipeline (`VCircuit → IRBlocks → Boolar
/// → fused BCircuit`) and shape-checked against the flattened port bits.
pub fn lower_gadget_library(
    lib: &TypedGadgetLibrary,
    types: &volar_ir::ir::IRTypes,
) -> Result<GadgetLibrary, LowerGadgetError> {
    let mut out = GadgetLibrary::new();
    for spec in &lib.gadgets {
        out.gadgets.push(lower_typed_gadget(spec, types)?);
    }
    Ok(out)
}

/// Lower one typed gadget spec to a bit-level [`GadgetSpec`].
pub fn lower_typed_gadget(
    spec: &TypedGadgetSpec,
    types: &volar_ir::ir::IRTypes,
) -> Result<GadgetSpec, LowerGadgetError> {
    volar_ir::typed_gadget::validate_typed_gadget(spec, types)?;
    let total_bits: usize = spec
        .ports
        .iter()
        .map(|p| typed_bit_width(p.ty, types).unwrap_or(0).saturating_mul(p.count))
        .sum();
    let data_bits = spec
        .data_bit_width(types)
        .ok_or(LowerGadgetError::ShapeMismatch {
            gadget: spec.name.clone(),
            reason: "no Boolean-width data port",
        })?;

    let lower_body = |body: &VCircuit| -> Result<volar_ir::circuit::BCircuit, LowerGadgetError> {
        let blocks = body.clone().to_ir_blocks();
        let bir = crate::lower_ir_to_boolar::lower_ir_to_boolar(&blocks, types);
        let fused = crate::fuse_to_circuit::to_circuit_fused_boolar(&bir)?;
        if fused.params as usize != total_bits {
            return Err(LowerGadgetError::ShapeMismatch {
                gadget: spec.name.clone(),
                reason: "booleanized body param count does not match flattened port bits",
            });
        }
        if fused.outputs.len() != data_bits {
            return Err(LowerGadgetError::ShapeMismatch {
                gadget: spec.name.clone(),
                reason: "booleanized body output count does not match data-port bits",
            });
        }
        Ok(fused)
    };

    Ok(GadgetSpec {
        name: spec.name.clone(),
        encrypt: lower_body(&spec.encrypt)?,
        decrypt: spec.decrypt.as_ref().map(lower_body).transpose()?,
        ports: spec
            .ports
            .iter()
            .map(|p| Port {
                name: p.name.clone(),
                kind: p.kind,
                width: (typed_bit_width(p.ty, types).unwrap_or(0) * p.count) as u64,
            })
            .collect(),
    })
}

// ============================================================================
// Region threading across boundary-moving passes
// ============================================================================

/// Errors from region-table pass threading.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum RegionThreadError {
    Region(TypedRegionError),
    /// The host's block params do not have the uniform per-position layout
    /// movfuscation's slot allocation requires for anchor translation.
    Layout(&'static str),
    /// The anchor kind cannot cross this boundary.
    UnsupportedAnchor { anchor: TypedAnchor, stage: &'static str },
    /// The anchor's carrier does not exist at this stage.
    UnknownCarrier(TypedAnchor),
    /// A bit/position arithmetic overflow.
    Overflow,
    /// A reversible wire translation found a non-contiguous range.
    NonContiguousWires { var: u32 },
}

impl From<TypedRegionError> for RegionThreadError {
    fn from(e: TypedRegionError) -> Self {
        RegionThreadError::Region(e)
    }
}

impl From<UnknownVar> for RegionThreadError {
    fn from(_: UnknownVar) -> Self {
        // The specific var is not carried by the error type; callers get
        // contiguity/unknown detail from the anchor itself.
        RegionThreadError::NonContiguousWires { var: u32::MAX }
    }
}

/// The combined-block slot layout movfuscation produced, as needed for
/// region-anchor translation: `movfuscate_ir`'s own analysis, re-exposed.
#[derive(Clone, Debug)]
pub struct MovfuscRegionLayout {
    /// Number of PC bits prepended to the combined block's params.
    pub pc_width: u32,
    /// Types of the state slots, in slot order (slot `k` = combined-block
    /// param `pc_width + k`).
    pub state_slot_types: Vec<volar_ir::ir::IRTypeId>,
    /// `slot_of[block][param_pos] = (slot_offset, slot_count)` — the param's
    /// state-slot range *after* the PC bits.
    pub slot_of: Vec<Vec<(usize, usize)>>,
    /// Expanded return-slot types (the ret accumulator, later the unrolled
    /// outputs after the optional `done` flag).
    pub return_slot_types: Vec<volar_ir::ir::IRTypeId>,
}

/// Compute the movfuscation slot layout for `blocks` — the same analysis
/// [`crate::movfuscate::movfuscate_ir`] runs internally, without running it.
pub fn movfuscate_region_layout(
    blocks: &IRBlocks,
    types: &volar_ir::ir::IRTypes,
) -> Result<MovfuscRegionLayout, RegionThreadError> {
    let mut types_mut = types.clone();
    let bit_type_id = types_mut.intern(volar_ir::ir::IRType::Primitive(Type::Bit));
    let n = blocks.blocks.len();
    let pc_width = crate::movfuscate::pc_bits_needed(n);
    let (state_slot_types, slot_of, _sig_fallback) =
        compute_static_slot_classes(blocks, &types_mut.0, &bit_type_id, pc_width);
    let return_slot_types =
        compute_return_slot_types(blocks, &types_mut.0, &bit_type_id, pc_width);
    Ok(MovfuscRegionLayout {
        pc_width: pc_width as u32,
        state_slot_types,
        slot_of,
        return_slot_types,
    })
}

/// Translate a pre-movfuscation typed region table onto the movfuscated
/// combined block's boundary.
///
/// - `Input { param }` (block 0) and `BlockInput { block, param }` move to
///   the state slot hosting that param (`Input { param: pc_width + offset }`).
/// - `Storage` anchors pass through (storage survives movfuscation).
/// - `Output`/function anchors are rejected (the movfuscated module has no
///   module-level output list).
pub fn translate_regions_movfuscate(
    pre: &TypedRegionTable,
    layout: &MovfuscRegionLayout,
) -> Result<TypedRegionTable, RegionThreadError> {
    pre.validate_structure()?;
    let mut entries = Vec::with_capacity(pre.entries.len());
    for entry in &pre.entries {
        match &entry.anchor {
            TypedAnchor::Input { param, start, len } => {
                let slot = slot_param(layout, 0, *param, entry)?;
                entries.push(renamed(entry, TypedAnchor::Input {
                    param: slot,
                    start: *start,
                    len: *len,
                }));
            }
            TypedAnchor::BlockInput {
                block,
                param,
                start,
                len,
            } => {
                let slot = slot_param(layout, block.0 as usize, *param, entry)?;
                entries.push(renamed(entry, TypedAnchor::Input {
                    param: slot,
                    start: *start,
                    len: *len,
                }));
            }
            TypedAnchor::Storage { .. } => entries.push(entry.clone()),
            anchor @ (TypedAnchor::Output { .. }
            | TypedAnchor::FuncInput { .. }
            | TypedAnchor::FuncOutput { .. }) => {
                return Err(RegionThreadError::UnsupportedAnchor {
                    anchor: anchor.clone(),
                    stage: "movfuscate",
                });
            }
        }
    }
    // State-slot remapping is order-preserving on `Input` anchors (params in
    // ascending order map to ascending slots), so entries stay sorted.
    Ok(TypedRegionTable {
        entries,
        names: pre.names.clone(),
    })
}

fn slot_param(
    layout: &MovfuscRegionLayout,
    block: usize,
    param: u32,
    entry: &volar_ir::typed_gadget::TypedRegionEntry,
) -> Result<u32, RegionThreadError> {
    let Some(per_block) = layout.slot_of.get(block) else {
        return Err(RegionThreadError::UnknownCarrier(entry.anchor.clone()));
    };
    let Some(&(offset, count)) = per_block.get(param as usize) else {
        return Err(RegionThreadError::UnknownCarrier(entry.anchor.clone()));
    };
    if count != 1 {
        // Block-typed (label) params expand to pc_width Bit slots and carry
        // no region-addressable typed bits.
        return Err(RegionThreadError::Layout(
            "anchor on a non-scalar (Block-typed) block param",
        ));
    }
    layout
        .pc_width
        .checked_add(offset as u32)
        .ok_or(RegionThreadError::Overflow)
}

/// Translate a typed aux `InputRange` source the same way
/// [`translate_regions_movfuscate`] moves input anchors: the source's param
/// (an original entry param) moves to its state slot.
pub fn translate_typed_aux_movfuscate(
    aux: &TypedAuxSource,
    layout: &MovfuscRegionLayout,
) -> Result<TypedAuxSource, RegionThreadError> {
    match aux {
        TypedAuxSource::InputRange { param, start } => {
            let Some(&(offset, count)) =
                layout.slot_of.first().and_then(|b| b.get(*param as usize))
            else {
                return Err(RegionThreadError::Layout(
                    "aux param outside block 0 layout",
                ));
            };
            if count != 1 {
                return Err(RegionThreadError::Layout(
                    "aux range on a non-scalar (Block-typed) block param",
                ));
            }
            let slot = layout
                .pc_width
                .checked_add(offset as u32)
                .ok_or(RegionThreadError::Overflow)?;
            Ok(TypedAuxSource::InputRange {
                param: slot,
                start: *start,
            })
        }
        other => Ok(other.clone()),
    }
}

/// Region anchors for the **internal slots movfuscation invents**: one entry
/// per PC bit param and one per state slot of the combined block. Slots
/// whose region set is empty (or absent from `slot_regions`) are left
/// untagged. Merge with a translated pre-table via
/// [`TypedRegionTable`]-level callers (entries are produced sorted).
pub fn movfuscate_state_regions(
    layout: &MovfuscRegionLayout,
    types: &volar_ir::ir::IRTypes,
    pc_regions: &alloc::collections::BTreeSet<volar_ir::region::RegionId>,
    slot_regions: &[alloc::collections::BTreeSet<volar_ir::region::RegionId>],
) -> Result<TypedRegionTable, RegionThreadError> {
    use volar_ir::typed_gadget::{TypedAnchor as TA, TypedRegionEntry};
    let mut entries = Vec::new();
    if !pc_regions.is_empty() {
        for p in 0..layout.pc_width {
            entries.push(TypedRegionEntry {
                anchor: TA::Input {
                    param: p,
                    start: 0,
                    len: 1,
                },
                regions: pc_regions.clone(),
            });
        }
    }
    for (k, ty) in layout.state_slot_types.iter().enumerate() {
        let Some(regions) = slot_regions.get(k) else { break };
        if regions.is_empty() {
            continue;
        }
        let width = typed_bit_width(*ty, types)
            .ok_or(RegionThreadError::Layout("state slot type has no Boolean width"))?
            as u16;
        entries.push(TypedRegionEntry {
            anchor: TA::Input {
                param: layout.pc_width + k as u32,
                start: 0,
                len: width,
            },
            regions: regions.clone(),
        });
    }
    Ok(TypedRegionTable {
        entries,
        names: BTreeMap::new(),
    })
}

/// Shift landed output anchors by one position: the unroller
/// (`lower_to_circuit` with `LoweringMode::WithTerminationFlag`) prepends a
/// single `done` bit to the return values. Input and storage anchors are
/// unchanged.
pub fn translate_regions_termination_flag(
    table: &RegionTable,
) -> Result<RegionTable, RegionThreadError> {
    let mut entries = Vec::with_capacity(table.entries.len());
    for entry in &table.entries {
        match &entry.anchor {
            volar_ir::region::WireAnchor::Output { start, len } => {
                let start = start.checked_add(1).ok_or(RegionThreadError::Overflow)?;
                entries.push(RegionEntry {
                    anchor: volar_ir::region::WireAnchor::Output {
                        start,
                        len: *len,
                    },
                    regions: entry.regions.clone(),
                });
            }
            _other => entries.push(entry.clone()),
        }
    }
    Ok(RegionTable {
        entries,
        names: table.names.clone(),
    })
}

/// Translate landed anchors onto the reversible wire space: input bits via
/// the var→wire map (contiguity checked), output bits via the y register
/// (`y_base + position`, one wire per output in order).
pub fn translate_regions_to_reversible(
    table: &RegionTable,
    map: &VarWireMap,
) -> Result<RegionTable, RegionThreadError> {
    let mut entries = Vec::with_capacity(table.entries.len());
    for entry in &table.entries {
        match &entry.anchor {
            volar_ir::region::WireAnchor::Input { start, len } => {
                let first = map
                    .wire(volar_ir::ir::IRVarId(*start))
                    .ok_or(RegionThreadError::NonContiguousWires { var: *start })?;
                for i in 0..*len {
                    let w = map
                        .wire(volar_ir::ir::IRVarId(start + i))
                        .ok_or(RegionThreadError::NonContiguousWires {
                            var: start + i,
                        })?;
                    if w != first + i as usize {
                        return Err(RegionThreadError::NonContiguousWires {
                            var: start + i,
                        });
                    }
                }
                entries.push(RegionEntry {
                    anchor: volar_ir::region::WireAnchor::Input {
                        start: first as u32,
                        len: *len,
                    },
                    regions: entry.regions.clone(),
                });
            }
            volar_ir::region::WireAnchor::Output { start, len } => {
                entries.push(RegionEntry {
                    anchor: volar_ir::region::WireAnchor::Output {
                        start: map.y_base() as u32 + *start,
                        len: *len,
                    },
                    regions: entry.regions.clone(),
                });
            }
            volar_ir::region::WireAnchor::Storage { .. } => {
                // Reversible circuits in this repo carry no storage; hardened
                // mode even rejects storage outright. Anchors cannot follow.
                return Err(RegionThreadError::UnsupportedAnchor {
                    anchor: TypedAnchor::Storage {
                        storage: StorageId(0),
                        ty: volar_ir::ir::IRTypeId(0),
                        addr_start: 0,
                        addr_len: 0,
                    },
                    stage: "to_reversible",
                });
            }
        }
    }
    Ok(RegionTable {
        entries,
        names: table.names.clone(),
    })
}

fn renamed(
    entry: &volar_ir::typed_gadget::TypedRegionEntry,
    anchor: TypedAnchor,
) -> volar_ir::typed_gadget::TypedRegionEntry {
    volar_ir::typed_gadget::TypedRegionEntry {
        anchor,
        regions: entry.regions.clone(),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeSet;
    use alloc::vec;
    use volar_ir::circuit::{BCircuit, VCircuit};
    use volar_ir::gadget::PortKind as PK;
    use volar_ir::boolar::BIrTerminator;
    use volar_ir::ir::{
        IRBlock, IRBlockTargetId, IRBranchTarget, IRTerminator, IRType, IRTypeId, IRVarId,
    };
    use volar_ir::region::{RegionId, RegionSelector, WireAnchor};
    use volar_ir::typed_gadget::{TypedAnchor as TA, TypedPort, TypedRegionEntry};
    use volar_ir_common::{Constant, Node, StorageId, Type, TypeTable};

    fn types() -> (TypeTable, IRTypeId, IRTypeId) {
        let mut t = TypeTable::new();
        let bit = t.intern(IRType::Primitive(Type::Bit));
        let w8 = t.intern(IRType::Primitive(Type::_8));
        (t, bit, w8)
    }

    fn ret_target(args: Vec<IRVarId>) -> IRBranchTarget {
        IRBranchTarget {
            dest: IRBlockTargetId::Return,
            args,
            reentry: None,
        }
    }

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

    /// Evaluate a pure-gate + storage `BCircuit` (static addresses only).
    fn eval_circuit(circ: &BCircuit<()>, params: &[bool]) -> Vec<bool> {
        use volar_ir::boolar::{BIrPreInitSegment, BIrStmt};
        use volar_ir::boolar::LaneId;
        let mut vals: Vec<Option<bool>> = vec![None; circ.var_space() as usize];
        for (i, &b) in params.iter().enumerate() {
            vals[i] = Some(b);
        }
        let mut storage: BTreeMap<((StorageId, LaneId), Vec<bool>), bool> = BTreeMap::new();
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
                other => panic!("eval_circuit: unsupported stmt {:?}", other),
            });
        }
        circ.outputs.iter().map(|o| vals[o.0 as usize].unwrap()).collect()
    }

    // ---- fixtures ----------------------------------------------------------

    /// Identity host: `out = param0` over two `_8` params.
    fn identity_host(w8: IRTypeId) -> VCircuit {
        let mut vc = VCircuit::new(vec![w8, w8]);
        vc.outputs = vec![IRVarId(0)];
        vc
    }

    /// Typed XOR-pad gadget: `data[i] = data[i] ^ key[i]` over `Bit` ports of
    /// count 8 (a word of bits expressed as 8 single-bit words).
    fn typed_pad(w8: IRTypeId, bit: IRTypeId) -> TypedGadgetSpec {
        let _ = w8;
        let mut body = VCircuit::new(vec![bit; 16]);
        let mut outs = Vec::with_capacity(8);
        for i in 0..8 {
            // data_i + key_i (two linear monomials) = XOR.
            let coeffs = BTreeMap::from([
                (vec![IRVarId(i)], 1u8),
                (vec![IRVarId(8 + i)], 1u8),
            ]);
            let o = body.push_stmt(
                volar_ir::ir::IRStmt::Poly {
                    ty: bit,
                    coeffs,
                    constant: Constant { hi: 0, lo: 0 },
                },
                (),
            );
            outs.push(o);
        }
        body.outputs = outs;
        TypedGadgetSpec {
            name: "pad8".into(),
            ports: vec![
                TypedPort {
                    name: "data".into(),
                    kind: PK::Data,
                    ty: bit,
                    count: 8,
                },
                TypedPort {
                    name: "key".into(),
                    kind: PK::Aux,
                    ty: bit,
                    count: 8,
                },
            ],
            encrypt: body,
            decrypt: None, // XOR is self-inverse
        }
    }

    fn key_words(key: &[bool]) -> TypedAuxSource {
        TypedAuxSource::Const(
            key.iter()
                .map(|&b| Constant {
                    hi: 0,
                    lo: b as u128,
                })
                .collect(),
        )
    }

    // ---- lowering + splice round-trip ---------------------------------------

    #[test]
    fn typed_table_and_gadget_lower_and_splice() {
        let (types, bit, w8) = types();
        let host = identity_host(w8);

        // Typed table: param0 = public data (region A), param1 = key region,
        // output = encrypted face of A.
        let ta = TypedRegionTable {
            entries: vec![
                TypedRegionEntry {
                    anchor: TA::Input { param: 0, start: 0, len: 8 },
                    regions: BTreeSet::from([RegionId(0)]),
                },
                TypedRegionEntry {
                    anchor: TA::Input { param: 1, start: 0, len: 8 },
                    regions: BTreeSet::from([RegionId(1)]),
                },
                TypedRegionEntry {
                    anchor: TA::Output { out: 0, start: 0, len: 8 },
                    regions: BTreeSet::from([RegionId(0)]),
                },
            ],
            names: BTreeMap::new(),
        };

        // Lower the host; allocation must be param0 → bits [0,8), param1 → [8,16).
        let irblocks = host.clone().to_ir_blocks();
        let (bir, tables) =
            crate::lower_ir_to_boolar::lower_ir_to_boolar_with_tables(&irblocks, &types);
        assert_eq!(
            tables.var_bits.bits(0, 0).unwrap(),
            &(0..8).map(IRVarId).collect::<Vec<_>>()
        );
        assert_eq!(
            tables.var_bits.bits(0, 1).unwrap(),
            &(8..16).map(IRVarId).collect::<Vec<_>>()
        );

        let widths = vcircuit_output_widths(&host, &types);
        assert_eq!(widths, vec![8]);
        let bit_table = lower_typed_region_table(&ta, &tables, &types, Some(&widths))
            .expect("table lowers");
        // Landed anchors: inputs at the allocated bit positions, output at 0.
        assert_eq!(
            bit_table.entries[0].anchor,
            WireAnchor::Input { start: 0, len: 8 }
        );
        assert_eq!(
            bit_table.entries[1].anchor,
            WireAnchor::Input { start: 8, len: 8 }
        );
        assert_eq!(
            bit_table.entries[2].anchor,
            WireAnchor::Output { start: 0, len: 8 }
        );
        let outputs: Vec<IRVarId> = match &bir.blocks[0].terminator {
            BIrTerminator::Jmp(t) => t.args.clone(),
            _ => panic!("expected Jmp(Return)"),
        };
        assert_eq!(bit_table.validate(&BCircuit {
            params: bir.blocks[0].params,
            stmts: bir.blocks[0].stmts.clone(),
            pre_init: bir.pre_init.clone(),
            outputs,
        }), Ok(()));

        // Typed gadget library + binding.
        let lib = TypedGadgetLibrary::new().with(typed_pad(w8, bit));
        let key = [true, false, true, true, false, false, true, false];
        let typed_lib_lowered = lower_gadget_library(&lib, &types).expect("lib lowers");
        let pad = typed_lib_lowered.get("pad8").expect("present");
        assert_eq!(pad.data_width(), Some(8));
        assert_eq!(pad.encrypt.params, 16);
        assert_eq!(pad.encrypt.outputs.len(), 8);

        let bindings = lower_typed_bindings(
            &[TypedGadgetBinding {
                gadget: "pad8".into(),
                selector: RegionSelector::all_of([RegionId(0)]),
                aux_sources: vec![key_words(&key)],
                rng_source: None,
            }],
            &lib,
            &tables,
            &types,
        )
        .expect("bindings lower");
        assert_eq!(
            bindings[0].aux_sources[0],
            AuxSource::Const(key.to_vec())
        );

        // Apply and check boundary semantics: wrapped(host, pt ⊕ k) = host(pt) ⊕ k.
        let host_bc = crate::fuse_to_circuit::to_circuit_fused_boolar(&bir).expect("fuses");
        let applied =
            crate::apply_gadgets::apply_gadgets(&host_bc, &bit_table, &bindings, &typed_lib_lowered)
                .expect("applies");
        let wrapped = applied.circuit;
        let k: Vec<bool> = key.to_vec();
        for seed in 0u8..=255 {
            let pt: Vec<bool> = (0..8).map(|i| (seed >> i) & 1 == 1).collect();
            let mut host_in = pt.clone();
            host_in.extend_from_slice(&[false; 8]); // param1 unused by identity host
            let host_out = eval_circuit(&host_bc, &host_in);
            let mut ct_in = pt.clone();
            for i in 0..8 {
                ct_in[i] = pt[i] ^ k[i];
            }
            ct_in.extend_from_slice(&[false; 8]);
            let wrapped_out = eval_circuit(&wrapped, &ct_in);
            for i in 0..8 {
                assert_eq!(
                    wrapped_out[i],
                    host_out[i] ^ k[i],
                    "bit {i}, seed {seed}"
                );
            }
        }
    }

    #[test]
    fn typed_storage_anchor_fans_out_per_plane() {
        use volar_ir_common::StorageId;
        let (types, _bit, w8) = types();
        let s = StorageId(3);
        // Host: writes param0 to (s, w8) at address param1.
        let mut vc = VCircuit::new(vec![w8, w8]);
        let _w = vc.push_stmt(
            volar_ir::ir::IRStmt::StorageWrite {
                storage: s,
                src: IRVarId(0),
                ty: w8,
                addr: IRVarId(1),
            },
            (),
        );
        vc.outputs = vec![];
        let irblocks = vc.to_ir_blocks();
        let (bir, tables) =
            crate::lower_ir_to_boolar::lower_ir_to_boolar_with_tables(&irblocks, &types);
        // addr = param1, width 8 → n = 8; planes land at a + (i << 8).
        let ta = TypedRegionTable {
            entries: vec![TypedRegionEntry {
                anchor: TA::Storage {
                    storage: s,
                    ty: w8,
                    addr_start: 0,
                    addr_len: 2,
                },
                regions: BTreeSet::from([RegionId(5)]),
            }],
            names: BTreeMap::new(),
        };
        let bit_table =
            lower_typed_region_table(&ta, &tables, &types, None).expect("lowers");
        assert_eq!(bit_table.entries.len(), 8);
        for (plane, e) in bit_table.entries.iter().enumerate() {
            match &e.anchor {
                WireAnchor::Storage { storage, lane, start, len } => {
                    assert_eq!(*storage, s);
                    assert_eq!(*len, 2);
                    assert_eq!(*start, (plane as u64) << 8);
                    let lane_ty = tables.lanes.get(lane).unwrap();
                    assert_eq!(*lane_ty, w8);
                }
                other => panic!("unexpected anchor {other:?}"),
            }
        }
        let _ = bir;
    }

    // ---- movfuscation translation -------------------------------------------

    fn two_block_program(w8: IRTypeId) -> volar_ir::ir::IRBlocks {
        volar_ir::ir::IRBlocks {
            oracles: vec![],
            actions: vec![],
            rngs: vec![],
            blocks: vec![
                IRBlock {
                    params: vec![w8],
                    stmts: vec![],
                    terminator: IRTerminator::Jmp {
                        target: IRBranchTarget {
                            dest: IRBlockTargetId::Block(volar_ir::ir::IRBlockId(1)),
                            args: vec![IRVarId(0)],
                            reentry: None,
                        },
                    },
                },
                IRBlock {
                    params: vec![w8],
                    stmts: vec![],
                    terminator: IRTerminator::Jmp {
                        target: ret_target(vec![IRVarId(0)]),
                    },
                },
            ],
            pre_init: vec![],
        }
    }

    #[test]
    fn movfuscate_layout_and_translation() {
        let (types, _bit, w8) = types();
        let blocks = two_block_program(w8);
        let layout = movfuscate_region_layout(&blocks, &types).expect("layout");
        assert_eq!(layout.pc_width, 1); // pc_bits_needed(2)
        assert_eq!(layout.state_slot_types, vec![w8]);
        assert_eq!(layout.return_slot_types, vec![w8]);
        // Block 1's param 0 → slot 0.
        assert_eq!(layout.slot_of[1][0], (0, 1));

        let pre = TypedRegionTable {
            entries: vec![
                // `Input` sorts before `BlockInput` (variant order).
                TypedRegionEntry {
                    anchor: TA::Input { param: 0, start: 0, len: 1 },
                    regions: BTreeSet::from([RegionId(8)]),
                },
                TypedRegionEntry {
                    anchor: TA::BlockInput {
                        block: volar_ir::ir::IRBlockId(1),
                        param: 0,
                        start: 2,
                        len: 3,
                    },
                    regions: BTreeSet::from([RegionId(7)]),
                },
            ],
            names: BTreeMap::new(),
        };
        let post = translate_regions_movfuscate(&pre, &layout).expect("translates");
        let anchors: Vec<_> = post.entries.iter().map(|e| e.anchor.clone()).collect();
        // Entry block param 0 → combined param 1 (after the PC bit); block 1's
        // param 0 → the same slot 0 → combined param 1 as well. Sorted order:
        // Input{param 1, [0,1)} then Input{param 1, [2,5)}.
        assert_eq!(
            anchors,
            vec![
                TA::Input { param: 1, start: 0, len: 1 },
                TA::Input { param: 1, start: 2, len: 3 },
            ]
        );
        assert!(post.validate_structure().is_ok());
    }

    #[test]
    fn movfuscate_translation_rejects_output_anchors() {
        let (types, _bit, w8) = types();
        let blocks = two_block_program(w8);
        let layout = movfuscate_region_layout(&blocks, &types).unwrap();
        let pre = TypedRegionTable {
            entries: vec![TypedRegionEntry {
                anchor: TA::Output { out: 0, start: 0, len: 1 },
                regions: BTreeSet::from([RegionId(0)]),
            }],
            names: BTreeMap::new(),
        };
        assert!(matches!(
            translate_regions_movfuscate(&pre, &layout),
            Err(RegionThreadError::UnsupportedAnchor { stage: "movfuscate", .. })
        ));
    }

    #[test]
    fn movfuscate_state_regions_shape() {
        let (types, _bit, w8) = types();
        let blocks = two_block_program(w8);
        let layout = movfuscate_region_layout(&blocks, &types).unwrap();
        let table = movfuscate_state_regions(
            &layout,
            &types,
            &BTreeSet::from([RegionId(9)]),
            &[BTreeSet::from([RegionId(10)])],
        )
        .expect("regions");
        let anchors: Vec<_> = table.entries.iter().map(|e| e.anchor.clone()).collect();
        assert_eq!(
            anchors,
            vec![
                TA::Input { param: 0, start: 0, len: 1 }, // PC bit
                TA::Input { param: 1, start: 0, len: 8 }, // state slot 0
            ]
        );
        assert!(table.validate_structure().is_ok());
        // Merging translated + invented tables works and stays sorted.
        let mut merged_entries = movfuscate_state_regions(
            &layout,
            &types,
            &BTreeSet::new(),
            &[BTreeSet::from([RegionId(10)])],
        )
        .unwrap()
        .entries;
        merged_entries.extend(
            translate_regions_movfuscate(
                &TypedRegionTable {
                    entries: vec![TypedRegionEntry {
                        anchor: TA::Input { param: 0, start: 0, len: 8 },
                        regions: BTreeSet::from([RegionId(8)]),
                    }],
                    names: BTreeMap::new(),
                },
                &layout,
            )
            .unwrap()
            .entries,
        );
        // Both claim Input param 1 → genuine overlap must be detected.
        let merged = TypedRegionTable {
            entries: merged_entries,
            names: BTreeMap::new(),
        };
        assert!(matches!(
            merged.validate_structure(),
            Err(TypedRegionError::OverlappingEntries { .. })
        ));
    }

    #[test]
    fn aux_translation_and_lowering() {
        let (types, bit, w8) = types();
        let blocks = two_block_program(w8);
        let layout = movfuscate_region_layout(&blocks, &types).unwrap();
        let aux = translate_typed_aux_movfuscate(
            &TypedAuxSource::InputRange { param: 0, start: 3 },
            &layout,
        )
        .expect("translates");
        assert_eq!(aux, TypedAuxSource::InputRange { param: 1, start: 3 });

        // Const flattening: one word per port word.
        let host = identity_host(w8);
        let irblocks = host.to_ir_blocks();
        let (_bir, tables) =
            crate::lower_ir_to_boolar::lower_ir_to_boolar_with_tables(&irblocks, &types);
        let lib = TypedGadgetLibrary::new().with(typed_pad(w8, bit));
        let bindings = lower_typed_bindings(
            &[TypedGadgetBinding {
                gadget: "pad8".into(),
                selector: RegionSelector::all_of([RegionId(0)]),
                aux_sources: vec![key_words(&[true, false, false, false, false, false, false, false])],
                rng_source: None,
            }],
            &lib,
            &tables,
            &types,
        )
        .expect("lowers");
        assert_eq!(
            bindings[0].aux_sources[0],
            AuxSource::Const(vec![true, false, false, false, false, false, false, false])
        );
    }

    // ---- unroll + reversible translations -----------------------------------

    #[test]
    fn termination_flag_shifts_output_anchors() {
        let table = RegionTable {
            entries: vec![
                RegionEntry {
                    anchor: WireAnchor::Input { start: 0, len: 4 },
                    regions: BTreeSet::from([RegionId(0)]),
                },
                RegionEntry {
                    anchor: WireAnchor::Output { start: 2, len: 3 },
                    regions: BTreeSet::from([RegionId(1)]),
                },
                RegionEntry {
                    anchor: WireAnchor::Output { start: 6, len: 1 },
                    regions: BTreeSet::from([RegionId(2)]),
                },
            ],
            names: BTreeMap::new(),
        };
        let shifted = translate_regions_termination_flag(&table).unwrap();
        let outs: Vec<u32> = shifted
            .entries
            .iter()
            .filter_map(|e| match &e.anchor {
                WireAnchor::Output { start, .. } => Some(*start),
                _ => None,
            })
            .collect();
        assert_eq!(outs, vec![3, 7]);
        // Inputs untouched.
        assert_eq!(
            shifted.entries[0].anchor,
            WireAnchor::Input { start: 0, len: 4 }
        );
    }

    #[test]
    fn reversible_translation() {
        let circ = BCircuit {
            params: 2,
            stmts: vec![],
            pre_init: vec![],
            outputs: vec![IRVarId(0), IRVarId(1)],
        };
        let (_rc, map) = crate::to_reversible::to_reversible(&circ).expect("reverses");
        let table = RegionTable {
            entries: vec![
                RegionEntry {
                    anchor: WireAnchor::Input { start: 0, len: 2 },
                    regions: BTreeSet::from([RegionId(0)]),
                },
                RegionEntry {
                    anchor: WireAnchor::Output { start: 1, len: 1 },
                    regions: BTreeSet::from([RegionId(1)]),
                },
            ],
            names: BTreeMap::new(),
        };
        let rev = translate_regions_to_reversible(&table, &map).expect("translates");
        match &rev.entries[0].anchor {
            WireAnchor::Input { start, len } => {
                assert_eq!((*start, *len), (0, 2));
            }
            other => panic!("unexpected {other:?}"),
        }
        match &rev.entries[1].anchor {
            WireAnchor::Output { start, len } => {
                assert_eq!((*start, *len), (map.y_base() as u32 + 1, 1));
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    // ---- fail-closed paths ---------------------------------------------------

    #[test]
    fn fail_closed_paths() {
        let (types, _bit, w8) = types();
        let host = identity_host(w8);
        let irblocks = host.to_ir_blocks();
        let (_bir, tables) =
            crate::lower_ir_to_boolar::lower_ir_to_boolar_with_tables(&irblocks, &types);

        // Unknown carrier: param 5 doesn't exist.
        let ta = TypedRegionTable {
            entries: vec![TypedRegionEntry {
                anchor: TA::Input { param: 5, start: 0, len: 1 },
                regions: BTreeSet::from([RegionId(0)]),
            }],
            names: BTreeMap::new(),
        };
        assert!(matches!(
            lower_typed_region_table(&ta, &tables, &types, None),
            Err(LowerTypedError::UnknownAnchorCarrier(_))
        ));

        // Out of range: param 0 is 8 bits.
        let ta = TypedRegionTable {
            entries: vec![TypedRegionEntry {
                anchor: TA::Input { param: 0, start: 6, len: 4 },
                regions: BTreeSet::from([RegionId(0)]),
            }],
            names: BTreeMap::new(),
        };
        assert!(matches!(
            lower_typed_region_table(&ta, &tables, &types, None),
            Err(LowerTypedError::RangeOutOfRange { width: 8, .. })
        ));

        // Output anchors require widths.
        let ta = TypedRegionTable {
            entries: vec![TypedRegionEntry {
                anchor: TA::Output { out: 0, start: 0, len: 1 },
                regions: BTreeSet::from([RegionId(0)]),
            }],
            names: BTreeMap::new(),
        };
        assert!(matches!(
            lower_typed_region_table(&ta, &tables, &types, None),
            Err(LowerTypedError::UnsupportedAnchor { .. })
        ));

        // Unknown gadget in bindings.
        let lib = TypedGadgetLibrary::new();
        let err = lower_typed_bindings(
            &[TypedGadgetBinding {
                gadget: "nope".into(),
                selector: RegionSelector::all_of([RegionId(0)]),
                aux_sources: vec![],
                rng_source: None,
            }],
            &lib,
            &tables,
            &types,
        )
        .unwrap_err();
        assert_eq!(err, LowerTypedError::UnknownGadget("nope".into()));

        // Aux word-count mismatch: port wants 8 words, got 1.
        let lib = TypedGadgetLibrary::new().with(typed_pad(w8, volar_ir::ir::IRTypeId(0)));
        let err = lower_typed_bindings(
            &[TypedGadgetBinding {
                gadget: "pad8".into(),
                selector: RegionSelector::all_of([RegionId(0)]),
                aux_sources: vec![key_words(&[true])],
                rng_source: None,
            }],
            &lib,
            &tables,
            &types,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            LowerTypedError::AuxWordCountMismatch { expected_words: 8, got_words: 1, .. }
        ));
    }

    #[test]
    fn typed_gadget_body_shape_checked_after_booleanization() {
        let (types, bit, w8) = types();
        let mut body = VCircuit::new(vec![bit; 9]); // wrong: 9 params, ports want 16
        let o = body.push_stmt(
            volar_ir::ir::IRStmt::Poly {
                ty: bit,
                coeffs: BTreeMap::new(),
                constant: Constant { hi: 0, lo: 1 },
            },
            (),
        );
        body.outputs = vec![o];
        let lib = TypedGadgetLibrary::new().with(TypedGadgetSpec {
            name: "badshape".into(),
            ports: vec![
                TypedPort { name: "data".into(), kind: PK::Data, ty: bit, count: 8 },
                TypedPort { name: "key".into(), kind: PK::Aux, ty: bit, count: 8 },
            ],
            encrypt: body,
            decrypt: None,
        });
        // validate_typed_gadget rejects the param mismatch before lowering.
        let err = lower_gadget_library(&lib, &types).unwrap_err();
        assert!(matches!(err, LowerGadgetError::Typed(TypedGadgetError::BodyParamMismatch { .. })));
        let _ = w8;
    }
}
