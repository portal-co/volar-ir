// @reliability: experimental
// @ai: assisted
//! Opt-in [vc-spec](https://sinui0.github.io/vc-spec/docs/spec) lowering:
//! tagged call-configuration arguments, embedder `mem_write`/`mem_reveal`,
//! and VCI `vc.reveal_*` host functions, expressed with the existing
//! [`volar_side::SideId`] / [`TypedRegionTable`] machinery.
//!
//! `volar-side` stays policy-free: [`VcProtection`] and [`VcSideHandler`]
//! live here, next to the WAFFLE→VAFFLE frontend that consumes them.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use portal_pc_waffle_ir::{
    ExportKind, Func, ImportKind, Module as WModule, Type as WType,
};
use vaffle::{FuncDecl as VaffleFuncDecl, Module as VaffleModule, ValueId};
use volar_ir::ir::IRTypeId;
use volar_ir::region::RegionId;
use volar_ir::typed_gadget::{TypedAnchor, TypedRegionEntry, TypedRegionTable};
use volar_ir_common::StorageId;
use volar_lir::LirType;
use volar_side::{SideHandler, SideId, SideTable};

use crate::vaffle_regions::validate_vaffle_regions;

/// Visibility at the vc-spec invocation / memory boundary, from the local
/// party's perspective.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VcVisibility {
    /// Known to both parties; enters execution as concrete.
    Public,
    /// Known only to the local party; enters as symbolic.
    Private,
    /// Known only to the remote party; enters as symbolic.
    Blind,
}

/// One tagged argument in a call configuration.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VcArg {
    Public,
    Private,
    Blind,
}

impl VcArg {
    pub(crate) fn visibility(self) -> VcVisibility {
        match self {
            VcArg::Public => VcVisibility::Public,
            VcArg::Private => VcVisibility::Private,
            VcArg::Blind => VcVisibility::Blind,
        }
    }
}

/// Embedder `mem_write`: place `len` bytes at `offset` in WASM memory
/// `memory` with the given visibility.
///
/// Public writes with [`bytes`](Self::bytes) present are baked into
/// `pre_init`. Private and blind writes are region-tagged only — those
/// cells are party inputs at weave/eval time, not constants.
#[derive(Clone, Debug)]
pub struct VcMemWrite {
    pub memory: u32,
    pub offset: u32,
    pub visibility: VcVisibility,
    /// Byte count of the written range.
    pub len: u32,
    /// Required to bake a public write into `pre_init`. Ignored for Blind.
    /// For Private, accepted for local eval helpers but never written to
    /// `pre_init`.
    pub bytes: Option<Vec<u8>>,
}

impl VcMemWrite {
    pub fn public(memory: u32, offset: u32, bytes: Vec<u8>) -> Self {
        Self {
            memory,
            offset,
            visibility: VcVisibility::Public,
            len: bytes.len() as u32,
            bytes: Some(bytes),
        }
    }

    pub fn private(memory: u32, offset: u32, len: u32) -> Self {
        Self {
            memory,
            offset,
            visibility: VcVisibility::Private,
            len,
            bytes: None,
        }
    }

    pub fn blind(memory: u32, offset: u32, len: u32) -> Self {
        Self {
            memory,
            offset,
            visibility: VcVisibility::Blind,
            len,
            bytes: None,
        }
    }
}

/// Embedder `mem_reveal`: both parties agree to treat `[offset, offset+len)`
/// as public. Overlay is expressed by unioning region ids on the split
/// interval, not by overlapping [`TypedAnchor`]s.
#[derive(Clone, Copy, Debug)]
pub struct VcMemReveal {
    pub memory: u32,
    pub offset: u32,
    pub len: u32,
}

/// Opt-in vc-spec configuration for [`crate::lower_waffle_module_with_vc`].
#[derive(Clone, Debug, Default)]
pub struct VcConfig {
    /// Export name (or, as a fallback, function name) → one tag per WASM
    /// parameter. Threaded mutable globals are not included.
    pub calls: BTreeMap<String, Vec<VcArg>>,
    pub mem_writes: Vec<VcMemWrite>,
    pub mem_reveals: Vec<VcMemReveal>,
}

impl VcConfig {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_call(mut self, export: impl Into<String>, args: Vec<VcArg>) -> Self {
        self.calls.insert(export.into(), args);
        self
    }

    pub fn with_mem_write(mut self, write: VcMemWrite) -> Self {
        self.mem_writes.push(write);
        self
    }

    pub fn with_mem_reveal(mut self, reveal: VcMemReveal) -> Self {
        self.mem_reveals.push(reveal);
        self
    }
}

/// Protection vocabulary for vc-spec visibilities. Lives here, not in
/// `volar-side`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum VcProtection {
    Public,
    Private,
    Blind,
}

/// [`SideHandler`] that maps interned `"public"` / `"local"` / `"remote"`
/// sides onto [`VcProtection`]. An absent side is public (concrete by
/// default, matching constants and untagged values).
#[derive(Clone, Debug)]
pub struct VcSideHandler {
    pub public: SideId,
    pub local: SideId,
    pub remote: SideId,
}

impl SideHandler for VcSideHandler {
    type Protection = VcProtection;

    fn protection(&self, side: Option<SideId>) -> VcProtection {
        match side {
            Some(s) if s == self.local => VcProtection::Private,
            Some(s) if s == self.remote => VcProtection::Blind,
            _ => VcProtection::Public,
        }
    }
}

/// Companion metadata produced by VC-mode lowering.
#[derive(Clone, Debug)]
pub struct VcArtifact {
    pub regions: TypedRegionTable,
    pub sides: SideTable,
    pub handler: VcSideHandler,
}

impl VcArtifact {
    /// Region id interned as `name`, if present.
    pub fn region(&self, name: &str) -> Option<RegionId> {
        self.regions
            .names
            .iter()
            .find_map(|(id, n)| (n.as_str() == name).then_some(*id))
    }
}

/// Interned side and region ids for one VC lowering session.
#[derive(Clone, Debug)]
pub(crate) struct VcIds {
    pub sides: SideTable,
    pub public: SideId,
    pub local: SideId,
    pub remote: SideId,
    pub region_public: RegionId,
    pub region_private: RegionId,
    pub region_blind: RegionId,
    pub region_names: BTreeMap<RegionId, String>,
}

impl VcIds {
    pub(crate) fn intern() -> Self {
        let mut sides = SideTable::new();
        let public = sides.intern("public");
        let local = sides.intern("local");
        let remote = sides.intern("remote");
        let region_public = RegionId(0);
        let region_private = RegionId(1);
        let region_blind = RegionId(2);
        let mut region_names = BTreeMap::new();
        region_names.insert(region_public, String::from("public"));
        region_names.insert(region_private, String::from("private"));
        region_names.insert(region_blind, String::from("blind"));
        Self {
            sides,
            public,
            local,
            remote,
            region_public,
            region_private,
            region_blind,
            region_names,
        }
    }

    pub(crate) fn side_of(&self, vis: VcVisibility) -> SideId {
        match vis {
            VcVisibility::Public => self.public,
            VcVisibility::Private => self.local,
            VcVisibility::Blind => self.remote,
        }
    }

    pub(crate) fn region_of(&self, vis: VcVisibility) -> RegionId {
        match vis {
            VcVisibility::Public => self.region_public,
            VcVisibility::Private => self.region_private,
            VcVisibility::Blind => self.region_blind,
        }
    }

    pub(crate) fn handler(&self) -> VcSideHandler {
        VcSideHandler {
            public: self.public,
            local: self.local,
            remote: self.remote,
        }
    }
}

/// Per-lowering-session state stored on [`crate::VaffleTarget`].
pub(crate) struct VcLoweringState {
    pub ids: VcIds,
    /// Function *declaration* name → tagged WASM params.
    pub calls: BTreeMap<String, Vec<VcArg>>,
    /// Spec handle counter *N*; first allocated handle is 1.
    pub next_handle: u32,
    /// Unconsumed reveals in the function currently being lowered.
    pub reveals: BTreeMap<u32, RevealedBits>,
}

/// Bits of a value passed to `vc.reveal_*`, keyed by handle.
#[derive(Clone, Debug)]
pub(crate) struct RevealedBits {
    pub bits: Vec<ValueId>,
    pub ty: LirType,
}

impl VcLoweringState {
    pub(crate) fn new(ids: VcIds, calls: BTreeMap<String, Vec<VcArg>>) -> Self {
        Self {
            ids,
            calls,
            next_handle: 0,
            reveals: BTreeMap::new(),
        }
    }
}

/// VCI reveal import classified by field name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VciOp {
    RevealI32,
    RevealI64,
    WaitI32,
    WaitI64,
    UnsupportedFloat,
}

/// Match a WAFFLE import field or `vc.field` concatenated name.
pub(crate) fn match_vci_name(name: &str) -> Option<VciOp> {
    let field = name.strip_prefix("vc.").unwrap_or(name);
    match field {
        "reveal_i32" => Some(VciOp::RevealI32),
        "reveal_i64" => Some(VciOp::RevealI64),
        "reveal_i32_wait" => Some(VciOp::WaitI32),
        "reveal_i64_wait" => Some(VciOp::WaitI64),
        "reveal_f32" | "reveal_f64" | "reveal_f32_wait" | "reveal_f64_wait" => {
            Some(VciOp::UnsupportedFloat)
        }
        _ => None,
    }
}

/// Resolve VCI op for a WAFFLE function index: declaration name and
/// `module.imports` `(module, field)` / `module.field` aliases.
pub(crate) fn vci_op_for_func(wasm: &WModule, fid: Func) -> Option<VciOp> {
    for name in import_aliases(wasm, fid) {
        if let Some(op) = match_vci_name(&name) {
            return Some(op);
        }
    }
    None
}

fn import_aliases(wasm: &WModule, fid: Func) -> Vec<String> {
    let mut names = Vec::new();
    let decl_name = wasm.funcs[fid].name();
    if !decl_name.is_empty() {
        names.push(decl_name.to_string());
    }
    for imp in &wasm.imports {
        if let ImportKind::Func(f) = imp.kind {
            if f == fid {
                names.push(imp.name.clone());
                names.push(alloc::format!("{}.{}", imp.module, imp.name));
            }
        }
    }
    names
}

/// Map a call-configuration key (export name, or function name fallback)
/// onto the WAFFLE function's declaration name used by `begin_function`.
pub(crate) fn resolve_call_func_name(wasm: &WModule, key: &str) -> String {
    for exp in &wasm.exports {
        if exp.name == key {
            if let ExportKind::Func(f) = exp.kind {
                let name = wasm.funcs[f].name();
                if !name.is_empty() {
                    return name.to_string();
                }
            }
        }
    }
    key.to_string()
}

pub(crate) fn wasm_param_bit_width(ty: WType) -> Option<u16> {
    match ty {
        WType::I32 | WType::F32 => Some(32),
        WType::I64 | WType::F64 => Some(64),
        _ => None,
    }
}

/// Build the VAFFLE-level typed region table for a completed VC lowering.
///
/// `FuncInput`/`FuncOutput` anchors are one Bit-typed signature slot per
/// WASM bit (VaffleTarget flattens i32/i64 into `Bit` params/results).
/// Storage intervals are split so overlapping public/private/blind/reveal
/// ranges become disjoint anchors with unioned region sets.
pub(crate) fn build_vc_regions(
    wasm: &WModule,
    module: &VaffleModule,
    vc: &VcConfig,
    ids: &VcIds,
    calls: &BTreeMap<String, Vec<VcArg>>,
    byte_tid: IRTypeId,
) -> TypedRegionTable {
    let mut entries: Vec<TypedRegionEntry> = Vec::new();

    for (func_name, args) in calls {
        let Some(&func_id) = module.exports.get(func_name) else {
            continue;
        };
        let Some((param_tys, ret_tys)) = wasm_func_types(wasm, func_name) else {
            continue;
        };
        let (sig_param_slots, sig_result_slots) = match module.funcs.get(func_id.0) {
            Some(VaffleFuncDecl::Body(body)) => module
                .sigs
                .get(body.sig.0)
                .map(|s| (s.params.len() as u32, s.results.len() as u32))
                .unwrap_or((0, 0)),
            _ => continue,
        };
        let mut bit = 0u32;
        for (i, arg) in args.iter().enumerate() {
            let Some(&ty) = param_tys.get(i) else {
                break;
            };
            let Some(width) = wasm_param_bit_width(ty) else {
                continue;
            };
            let region = ids.region_of(arg.visibility());
            for j in 0..width as u32 {
                let param = bit + j;
                if param >= sig_param_slots {
                    break;
                }
                entries.push(TypedRegionEntry {
                    anchor: TypedAnchor::FuncInput {
                        func: func_id.0 as u32,
                        param,
                        start: 0,
                        len: 1,
                    },
                    regions: BTreeSet::from([region]),
                });
            }
            bit += width as u32;
        }
        let mut out_bit = 0u32;
        for ty in &ret_tys {
            let Some(width) = wasm_param_bit_width(*ty) else {
                continue;
            };
            for j in 0..width as u32 {
                let result = out_bit + j;
                if result >= sig_result_slots {
                    break;
                }
                entries.push(TypedRegionEntry {
                    anchor: TypedAnchor::FuncOutput {
                        func: func_id.0 as u32,
                        result,
                        start: 0,
                        len: 1,
                    },
                    regions: BTreeSet::from([ids.region_public]),
                });
            }
            out_bit += width as u32;
        }
    }

    let storage = merge_storage_intervals(module, vc, ids, byte_tid);
    entries.extend(storage);

    entries.sort_by(|a, b| a.anchor.cmp(&b.anchor));
    TypedRegionTable {
        entries,
        names: ids.region_names.clone(),
    }
}

fn wasm_func_types(wasm: &WModule, name: &str) -> Option<(Vec<WType>, Vec<WType>)> {
    for (_, decl) in wasm.funcs.entries() {
        if decl.name() != name {
            continue;
        }
        let data = wasm.signatures.get(decl.sig())?;
        return match data {
            portal_pc_waffle_ir::SignatureData::Func {
                params, returns, ..
            } => Some((params.clone(), returns.clone())),
            _ => None,
        };
    }
    None
}

struct StorageCover {
    storage: StorageId,
    ty: IRTypeId,
    start: u64,
    end: u64,
    regions: BTreeSet<RegionId>,
}

fn merge_storage_intervals(
    module: &VaffleModule,
    vc: &VcConfig,
    ids: &VcIds,
    byte_tid: IRTypeId,
) -> Vec<TypedRegionEntry> {
    let mut covers: Vec<StorageCover> = Vec::new();
    for seg in &module.pre_init {
        if seg.data.is_empty() {
            continue;
        }
        covers.push(StorageCover {
            storage: seg.storage,
            ty: seg.ty,
            start: seg.offset as u64,
            end: (seg.offset + seg.data.len()) as u64,
            regions: BTreeSet::from([ids.region_public]),
        });
    }
    for w in &vc.mem_writes {
        if w.len == 0 {
            continue;
        }
        let region = ids.region_of(w.visibility);
        covers.push(StorageCover {
            storage: StorageId::memory(w.memory),
            ty: byte_tid,
            start: w.offset as u64,
            end: w.offset as u64 + w.len as u64,
            regions: BTreeSet::from([region]),
        });
    }
    for r in &vc.mem_reveals {
        if r.len == 0 {
            continue;
        }
        covers.push(StorageCover {
            storage: StorageId::memory(r.memory),
            ty: byte_tid,
            start: r.offset as u64,
            end: r.offset as u64 + r.len as u64,
            regions: BTreeSet::from([ids.region_public]),
        });
    }

    // Group by (storage, ty), split on every boundary, union region sets.
    let mut grouped: BTreeMap<(u32, u32), Vec<StorageCover>> = BTreeMap::new();
    for c in covers {
        grouped
            .entry((c.storage.0, c.ty.0))
            .or_default()
            .push(c);
    }
    let mut entries = Vec::new();
    for ((storage, ty), spans) in grouped {
        let mut bounds: BTreeSet<u64> = BTreeSet::new();
        for s in &spans {
            bounds.insert(s.start);
            bounds.insert(s.end);
        }
        let bounds: Vec<u64> = bounds.into_iter().collect();
        for w in bounds.windows(2) {
            let (lo, hi) = (w[0], w[1]);
            let mut regions = BTreeSet::new();
            for s in &spans {
                if s.start < hi && s.end > lo {
                    regions.extend(s.regions.iter().copied());
                }
            }
            if regions.is_empty() {
                continue;
            }
            entries.push(TypedRegionEntry {
                anchor: TypedAnchor::Storage {
                    storage: StorageId(storage),
                    ty: IRTypeId(ty),
                    addr_start: lo,
                    addr_len: hi - lo,
                },
                regions,
            });
        }
    }
    entries
}

/// Validate a VC region table against the lowered module. Storage anchors
/// for private/blind writes may predate any load/store traffic; those are
/// still valid introduction points, so unknown-storage is tolerated for
/// `StorageId::memory(*)` ranges the config named.
pub(crate) fn validate_vc_regions(
    table: &TypedRegionTable,
    module: &VaffleModule,
    vc: &VcConfig,
) -> Result<(), crate::vaffle_regions::VaffleRegionError> {
    match validate_vaffle_regions(table, module) {
        Ok(()) => Ok(()),
        Err(crate::vaffle_regions::VaffleRegionError::UnknownStorageSpace { storage, ty }) => {
            let named = vc.mem_writes.iter().any(|w| {
                StorageId::memory(w.memory) == storage && w.len > 0
            }) || vc.mem_reveals.iter().any(|r| {
                StorageId::memory(r.memory) == storage && r.len > 0
            });
            if named {
                Ok(())
            } else {
                Err(crate::vaffle_regions::VaffleRegionError::UnknownStorageSpace { storage, ty })
            }
        }
        Err(e) => Err(e),
    }
}
