// @reliability: experimental
// @ai: assisted
//! Typed, typestate-based IR transformation pipeline.
//!
//! [`Pipeline<S>`] is generic over its current [`PipelineStage`] `S` — a
//! compile-time witness (mirroring `volar_discipline::Tagged<Z, T>`) of what
//! kind of data the pipeline currently holds. [`PipelinePass<From>`] is a
//! trait, not a closed enum: **any** type can implement it for any stage,
//! which is what makes passes "arbitrary" — a downstream crate can add its
//! own pass (e.g. a local-mixing transform over [`RCircuitStage`]) without
//! anything changing here. Stage mismatches that used to be a runtime
//! `Err(String)` (e.g. "FoldIr pass requires VolarIr stage") are now simply
//! methods that don't exist for the wrong `Pipeline<S>` — a compile error.
//!
//! `volar`'s `docs/pipeline.md` frames the whole system as "a graph of IRs,
//! not a pipeline" (weaving can feed Boolar/Volar IR back into a fresh
//! module). [`Pipeline::from_data`] being available at every stage — not
//! just as an initial "source" — is what keeps that representable: any
//! stage's data can seed a fresh `Pipeline<S>` at any time, so re-entry
//! after weaving is just constructing a new `Pipeline<VolarIrStage>` from
//! the woven output.
//!
//! # Behavior changes from the enum-based `Pipeline`
//!
//! - **Source constructors are eager and fallible.** The old `Source` enum
//!   was lazy — `Pipeline::from_wasm(path)` didn't parse anything until
//!   `.execute()` ran, so I/O errors only ever surfaced at the final
//!   `.to_volar_ir()`/`.to_lir()` call. A `Pipeline<S>` can't hold a
//!   not-yet-`S::Data` value and still *be* a `Pipeline<S>`, so every
//!   `from_*` constructor now does its work immediately and returns
//!   `Result<Pipeline<S>, Box<dyn std::error::Error>>`.
//! - **No implicit stage-skipping on terminals.** `to_lir()` used to accept
//!   either a `Lir` or `VolarIr` runtime stage and silently auto-lower in
//!   the latter case. `to_lir` now only exists on `Pipeline<LirStage>`; call
//!   `.lower_to_lir()` first.
//! - **Config lives on the pass, not the pipeline.** `inline_entries` and
//!   `import_config` used to be shared, implicitly-threaded `Pipeline`
//!   fields. Since an arbitrary pass can't know to look for pipeline-level
//!   fields it's unaware of, each pass is now self-contained (e.g.
//!   [`InlineVaffleEverything`] carries its own `entries`).

use std::marker::PhantomData;
#[cfg(feature = "llvm")]
use std::path::Path;
#[cfg(feature = "llvm")]
use std::path::PathBuf;

use volar_circuit_source::{
    CircuitSourceBackend, EmitOptions, SourcePackage, emit_bool_circuit, emit_volar_circuit,
};
use volar_ir::boolar::BIrBlocks;
use volar_ir::circuit::{BCircuit, VCircuit};
use volar_ir::ir::{IRBlocks, IRTypes};
use volar_ir::rcircuit::RCircuit;
use volar_lir_saved::{RecordingTarget, SavedLirModule};

type BoxError = Box<dyn std::error::Error>;

/// Some `volar-ir`/`volar-ir-passes` error enums don't (yet) implement
/// `std::error::Error`; box them via `Debug` rather than widening their
/// definitions from here.
fn box_err<E: core::fmt::Debug>(e: E) -> BoxError {
    format!("{e:?}").into()
}

// ============================================================================
// PipelineStage
// ============================================================================

/// Compile-time marker for a pipeline stage: what kind of data a
/// [`Pipeline<Self>`] currently holds.
///
/// Left open (unsealed) — a downstream crate may define its own stage if it
/// needs one, though most integrations only need to add [`PipelinePass`]
/// impls against the stages already defined here.
pub trait PipelineStage: 'static {
    type Data;
}

/// A VAFFLE module.
pub struct VaffleStage;
#[cfg(feature = "vaffle")]
impl PipelineStage for VaffleStage {
    type Data = vaffle::Module;
}

/// Volar IR, paired with its type table.
pub struct VolarIrStage;
impl PipelineStage for VolarIrStage {
    type Data = (IRBlocks, IRTypes);
}

/// Boolar IR (boolean-gate SSA, possibly multi-block). Every value is
/// exactly one bit, so — unlike [`VolarIrStage`] — no type table travels
/// alongside it.
pub struct BoolarStage;
impl PipelineStage for BoolarStage {
    type Data = BIrBlocks;
}

/// Circuit-fused (single-block) Boolar IR — the form
/// [`to_reversible`](volar_ir_passes::to_reversible) and
/// [`to_boolar_circuit`](volar_ir_passes::to_boolar_circuit) actually
/// operate on.
pub struct BoolarCircuitStage;
impl PipelineStage for BoolarCircuitStage {
    type Data = BCircuit;
}

/// A reversible gate circuit (X/CNOT/Toffoli/XorLut2/StorageSwap/...).
pub struct RCircuitStage;
impl PipelineStage for RCircuitStage {
    type Data = RCircuit;
}

/// A saved LIR module.
pub struct LirStage;
impl PipelineStage for LirStage {
    type Data = SavedLirModule;
}

// ============================================================================
// PipelinePass
// ============================================================================

/// A pass from stage `From` to `Self::Output`.
///
/// Implement this for any type to add a custom pass — this is the
/// extensibility point that replaces the old closed `PipelinePass` enum.
pub trait PipelinePass<From: PipelineStage> {
    type Output: PipelineStage;

    fn apply(self, data: From::Data) -> Result<<Self::Output as PipelineStage>::Data, BoxError>;
}

// ============================================================================
// Pipeline
// ============================================================================

/// An IR-transform pipeline currently holding stage `S`'s data.
pub struct Pipeline<S: PipelineStage> {
    data: S::Data,
    _stage: PhantomData<S>,
}

impl<S: PipelineStage> Pipeline<S> {
    /// Wrap already-in-hand stage data. The entry point for re-entering the
    /// pipeline at an arbitrary stage (e.g. after weaving feeds Boolar IR
    /// back into a fresh Volar IR module) as well as for ordinary sources.
    pub fn from_data(data: S::Data) -> Self {
        Pipeline {
            data,
            _stage: PhantomData,
        }
    }

    /// Unwrap the pipeline's current data.
    pub fn into_data(self) -> S::Data {
        self.data
    }

    /// Borrow the pipeline's current data.
    pub fn data(&self) -> &S::Data {
        &self.data
    }

    /// Apply any pass whose input stage is `S`.
    pub fn apply<P: PipelinePass<S>>(self, pass: P) -> Result<Pipeline<P::Output>, BoxError> {
        Ok(Pipeline::from_data(pass.apply(self.data)?))
    }
}

// Manual impl (rather than `#[derive(Debug)]`) because a derive would add an
// incorrect `S: Debug` bound — only `S::Data` needs to be printable.
impl<S: PipelineStage> core::fmt::Debug for Pipeline<S>
where
    S::Data: core::fmt::Debug,
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Pipeline")
            .field("data", &self.data)
            .finish()
    }
}

// ============================================================================
// Built-in passes
// ============================================================================

/// Inline every non-recursive intra-module VAFFLE call, including tail
/// calls. `entries` names the roots; empty defaults to every export (or,
/// failing that, every function body).
#[cfg(feature = "vaffle")]
#[derive(Debug, Clone, Default)]
pub struct InlineVaffleEverything {
    pub entries: Vec<String>,
}

#[cfg(feature = "vaffle")]
impl PipelinePass<VaffleStage> for InlineVaffleEverything {
    type Output = VaffleStage;

    fn apply(self, mut module: vaffle::Module) -> Result<vaffle::Module, BoxError> {
        let ids = resolve_inline_entries(&module, &self.entries)?;
        volar_ir_opt::inline_vaffle::inline_vaffle_everything(&mut module, &ids)?;
        Ok(module)
    }
}

/// Lower a VAFFLE module to Volar IR.
#[cfg(feature = "vaffle")]
#[derive(Debug, Clone, Copy, Default)]
pub struct LowerToVolarIr;

#[cfg(feature = "vaffle")]
impl PipelinePass<VaffleStage> for LowerToVolarIr {
    type Output = VolarIrStage;

    fn apply(self, module: vaffle::Module) -> Result<(IRBlocks, IRTypes), BoxError> {
        Ok(volar_vaffle_target::lower_vaffle_to_ir_owned(module))
    }
}

/// Constant-fold and DCE Volar IR until stable.
#[derive(Debug, Clone, Copy, Default)]
pub struct FoldIr;

impl PipelinePass<VolarIrStage> for FoldIr {
    type Output = VolarIrStage;

    fn apply(
        self,
        (mut blocks, types): (IRBlocks, IRTypes),
    ) -> Result<(IRBlocks, IRTypes), BoxError> {
        loop {
            let folded = volar_ir_opt::ir::fold_ir_blocks(&mut blocks, &types);
            let deadcode = volar_ir_opt::ir::dce_ir_blocks(&mut blocks, &types);
            if !folded && !deadcode {
                break;
            }
        }
        Ok((blocks, types))
    }
}

/// Movfuscate Volar IR into a single self-looping block. Mutually
/// alternative to [`UnrollIrEverything`] for circuit shape.
#[derive(Debug, Clone, Copy, Default)]
pub struct Movfuscate;

impl PipelinePass<VolarIrStage> for Movfuscate {
    type Output = VolarIrStage;

    fn apply(
        self,
        (blocks, mut types): (IRBlocks, IRTypes),
    ) -> Result<(IRBlocks, IRTypes), BoxError> {
        // The pipeline owns this stage, so transfer its large Poly payloads
        // into the step circuit instead of routing through the legacy
        // borrowed compatibility API.
        let blocks = volar_ir_passes::movfuscate_ir_owned(blocks, &mut types);
        Ok((blocks, types))
    }
}

/// Unroll Volar IR into a combinational circuit (`is_circuit()`). Requires
/// concrete control flow; fails closed otherwise.
#[derive(Debug, Clone, Copy)]
pub struct UnrollIrEverything {
    /// Explicit compiler-resource caps for this finite concrete-control walk.
    pub limits: volar_ir_passes::UnrollLimits,
}

/// Outcome of attempting concrete CFG unrolling before movfuscation.
///
/// A failed attempt intentionally preserves the original CFG, allowing the
/// caller to movfuscate the residual loop instead of treating a finite-prefix
/// optimization failure as a compilation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PreMovfuscationUnroll {
    /// Concrete control reached `Return`; the result is a circuit.
    Complete,
    /// An executable residual CFG was retained after the concrete prefix.
    RetainedLoop(volar_ir_passes::UnrollError),
}

impl Default for UnrollIrEverything {
    fn default() -> Self {
        Self {
            limits: volar_ir_passes::UnrollLimits::default(),
        }
    }
}

impl PipelinePass<VolarIrStage> for UnrollIrEverything {
    type Output = VolarIrStage;

    fn apply(self, (blocks, types): (IRBlocks, IRTypes)) -> Result<(IRBlocks, IRTypes), BoxError> {
        let blocks =
            volar_ir_passes::unroll_ir_everything_with_limits(&blocks, &types, self.limits)?;
        Ok((blocks, types))
    }
}

/// Lower Volar IR to a saved LIR module.
#[derive(Debug, Clone, Copy, Default)]
pub struct LowerToLir;

impl PipelinePass<VolarIrStage> for LowerToLir {
    type Output = LirStage;

    fn apply(self, (blocks, types): (IRBlocks, IRTypes)) -> Result<SavedLirModule, BoxError> {
        Ok(lower_volar_ir_to_lir(&blocks, &types))
    }
}

/// Lower Volar IR to Boolar IR ("booleanize"). Fails closed on an
/// undeclared external primitive rather than panicking.
#[derive(Debug, Clone, Copy, Default)]
pub struct LowerToBoolar;

impl PipelinePass<VolarIrStage> for LowerToBoolar {
    type Output = BoolarStage;

    fn apply(self, (blocks, types): (IRBlocks, IRTypes)) -> Result<BIrBlocks, BoxError> {
        volar_ir_passes::lower_ir_to_boolar::try_lower_ir_to_boolar(&blocks, &types)
            .map_err(box_err)
    }
}

/// Eliminate `StorageRead`/`StorageWrite` for one storage id by promoting it
/// to an explicit MUX/demux register file (Volar IR level). See
/// [`volar_ir_passes::storage_to_mux_ir`] for the algorithm and its
/// correctness contract.
#[derive(Debug, Clone)]
pub struct StorageToMuxIr(pub volar_ir_passes::StorageToMuxConfig);

impl PipelinePass<VolarIrStage> for StorageToMuxIr {
    type Output = VolarIrStage;

    fn apply(
        self,
        (blocks, mut types): (IRBlocks, IRTypes),
    ) -> Result<(IRBlocks, IRTypes), BoxError> {
        let blocks = volar_ir_passes::storage_to_mux_ir(&blocks, &mut types, &self.0)?;
        Ok((blocks, types))
    }
}

/// Eliminate `StorageRead`/`StorageWrite` for one `(StorageId, LaneId)` by
/// promoting it to an explicit MUX/demux bit register file (Boolar IR
/// level). See [`volar_ir_passes::storage_to_mux_boolar`] for the algorithm
/// and its correctness contract.
#[derive(Debug, Clone)]
pub struct StorageToMuxBoolar(pub volar_ir_passes::StorageToMuxBoolarConfig);

impl PipelinePass<BoolarStage> for StorageToMuxBoolar {
    type Output = BoolarStage;

    fn apply(self, blocks: BIrBlocks) -> Result<BIrBlocks, BoxError> {
        Ok(volar_ir_passes::storage_to_mux_boolar(&blocks, &self.0)?)
    }
}

/// Fuse (already movfuscated, or unroll-then-fuse) Boolar IR into its
/// single-block circuit form. `limit` bounds the MUX-unroll budget; see
/// [`volar_ir_passes::LoweringMode`].
#[derive(Debug, Clone, Copy)]
pub struct FuseBoolar {
    pub limit: u32,
    pub mode: volar_ir_passes::LoweringMode,
}

impl PipelinePass<BoolarStage> for FuseBoolar {
    type Output = BoolarCircuitStage;

    fn apply(self, blocks: BIrBlocks) -> Result<BCircuit, BoxError> {
        volar_ir_passes::fuse_to_circuit::lower_to_circuit_fused(&blocks, self.limit, self.mode)
            .map_err(box_err)
    }
}

/// Convert circuit-fused Boolar IR to a reversible gate circuit. The
/// returned `RCircuit` discards the `VarWireMap` side table
/// [`volar_ir_passes::to_reversible_with_mode`] also returns; call that
/// directly if the wire map is needed downstream.
#[derive(Debug, Clone, Copy, Default)]
pub struct ToReversible {
    pub mode: volar_ir_passes::ReversibleMode,
}

impl PipelinePass<BoolarCircuitStage> for ToReversible {
    type Output = RCircuitStage;

    fn apply(self, circ: BCircuit) -> Result<RCircuit, BoxError> {
        let (rcircuit, _map) =
            volar_ir_passes::to_reversible_with_mode(&circ, self.mode).map_err(box_err)?;
        Ok(rcircuit)
    }
}

/// Lower a reversible gate circuit back to circuit-fused Boolar IR.
#[derive(Debug, Clone, Copy, Default)]
pub struct FromReversible;

impl PipelinePass<RCircuitStage> for FromReversible {
    type Output = BoolarCircuitStage;

    fn apply(self, circuit: RCircuit) -> Result<BCircuit, BoxError> {
        volar_ir_passes::to_boolar_circuit(&circuit).map_err(box_err)
    }
}

// ============================================================================
// Per-stage constructors and convenience methods
// ============================================================================

impl Pipeline<LirStage> {
    /// Load a pre-recorded `.lir` file.
    pub fn from_saved_lir(path: impl AsRef<Path2>) -> Result<Self, BoxError> {
        let bytes = std::fs::read(path.as_ref())?;
        let saved = rkyv::from_bytes::<SavedLirModule, rkyv::rancor::Error>(&bytes)?;
        Ok(Self::from_data(saved))
    }

    /// Wrap an in-memory saved LIR module.
    pub fn from_saved_lir_module(saved: SavedLirModule) -> Self {
        Self::from_data(saved)
    }

    /// Terminal: the saved LIR module.
    pub fn to_lir(self) -> SavedLirModule {
        self.into_data()
    }
}

// `std::path::Path` under a local alias so `AsRef<Path2>` reads naturally
// even in builds without the `llvm` feature (which otherwise owns the
// `Path` import above).
#[cfg(feature = "llvm")]
type Path2 = Path;
#[cfg(not(feature = "llvm"))]
type Path2 = std::path::Path;

impl Pipeline<VolarIrStage> {
    /// Load a rkyv-serialized `(IRBlocks, IRTypes)` file.
    pub fn from_volar_ir_file(path: impl AsRef<Path2>) -> Result<Self, BoxError> {
        let bytes = std::fs::read(path.as_ref())?;
        let (blocks, types) = rkyv::from_bytes::<(IRBlocks, IRTypes), rkyv::rancor::Error>(&bytes)
            .map_err(|e| format!("failed to deserialize Volar IR file: {e}"))?;
        Ok(Self::from_data((blocks, types)))
    }

    /// Wrap in-memory Volar IR.
    pub fn from_volar_ir_blocks(blocks: IRBlocks, types: IRTypes) -> Self {
        Self::from_data((blocks, types))
    }

    /// Constant-fold Volar IR until stable.
    pub fn fold_ir(self) -> Result<Self, BoxError> {
        self.apply(FoldIr)
    }

    /// Movfuscate Volar IR into a single self-looping block.
    pub fn movfuscate(self) -> Result<Self, BoxError> {
        self.apply(Movfuscate)
    }

    /// Unroll Volar IR into a combinational circuit under conservative default
    /// resource limits (concrete control flow required).
    pub fn unroll_ir(self) -> Result<Self, BoxError> {
        self.apply(UnrollIrEverything::default())
    }

    /// Unroll Volar IR under caller-supplied concrete-control resource caps.
    /// Exceeding a cap fails closed; it never truncates the program.
    pub fn unroll_ir_with_limits(
        self,
        limits: volar_ir_passes::UnrollLimits,
    ) -> Result<Self, BoxError> {
        self.apply(UnrollIrEverything { limits })
    }

    /// Splice concrete prefixes throughout the CFG before movfuscation.
    ///
    /// Every replacement preserves its original block id and parameter
    /// interface, so predecessor targets remain valid. Symbolic or bounded
    /// segments stay as residual loops; no path is truncated.
    pub fn unroll_cfg_segments_before_movfuscation(
        mut self,
        limits: volar_ir_passes::UnrollLimits,
    ) -> (Self, volar_ir_passes::CfgSegmentUnroll) {
        let (mut blocks, types) = self.into_data();
        let report = volar_ir_passes::unroll_cfg_segments_with_limits(&mut blocks, &types, limits);
        self = Self::from_data((blocks, types));
        (self, report)
    }

    /// Attempt concrete unrolling before movfuscation.
    ///
    /// When the whole CFG reaches `Return` within `limits`, the returned
    /// pipeline is a circuit. Symbolic control, a non-finite loop, or a
    /// resource cap retains an executable residual CFG after the concrete
    /// prefix; callers can then call [`Self::movfuscate`] on that loop.
    /// It never emits a truncated path.
    pub fn try_unroll_before_movfuscation(
        self,
        limits: volar_ir_passes::UnrollLimits,
    ) -> (Self, PreMovfuscationUnroll) {
        let (blocks, types) = self.into_data();
        let result = volar_ir_passes::unroll_ir_prefix_with_limits(&blocks, &types, limits);
        let outcome = match result.outcome {
            volar_ir_passes::PrefixUnrollOutcome::Complete => PreMovfuscationUnroll::Complete,
            volar_ir_passes::PrefixUnrollOutcome::Residual(err) => {
                PreMovfuscationUnroll::RetainedLoop(err)
            }
        };
        (Self::from_data((result.blocks, types)), outcome)
    }

    /// Lower Volar IR → saved LIR.
    pub fn lower_to_lir(self) -> Result<Pipeline<LirStage>, BoxError> {
        self.apply(LowerToLir)
    }

    /// Lower Volar IR → Boolar IR.
    pub fn lower_to_boolar(self) -> Result<Pipeline<BoolarStage>, BoxError> {
        self.apply(LowerToBoolar)
    }

    /// Eliminate `StorageRead`/`StorageWrite` for `cfg.storage` via an
    /// explicit MUX/demux register file.
    pub fn storage_to_mux(
        self,
        cfg: volar_ir_passes::StorageToMuxConfig,
    ) -> Result<Self, BoxError> {
        self.apply(StorageToMuxIr(cfg))
    }

    /// Terminal: the Volar IR.
    pub fn to_volar_ir(self) -> (IRBlocks, IRTypes) {
        self.into_data()
    }

    /// Fuse to a [`VCircuit`] and emit a reusable source package.
    ///
    /// Requires a single `Jmp(Return)`-terminated block (run `unroll_ir` or
    /// `movfuscate` first).
    pub fn emit_source<B: CircuitSourceBackend>(
        self,
        backend: &B,
        opt: &EmitOptions,
    ) -> Result<SourcePackage, BoxError> {
        let (blocks, types) = self.into_data();
        let circuit = VCircuit::try_from_ir(&blocks).map_err(box_err)?;
        emit_volar_circuit(&circuit, &types, backend, opt).map_err(box_err)
    }
}

impl Pipeline<BoolarStage> {
    /// Eliminate `StorageRead`/`StorageWrite` for `cfg.storage`/`cfg.lane`
    /// via an explicit MUX/demux bit register file.
    pub fn storage_to_mux(
        self,
        cfg: volar_ir_passes::StorageToMuxBoolarConfig,
    ) -> Result<Self, BoxError> {
        self.apply(StorageToMuxBoolar(cfg))
    }

    /// Fuse to the single-block circuit form (unrolling with `limit`/`mode`
    /// if not already movfuscated).
    pub fn fuse(
        self,
        limit: u32,
        mode: volar_ir_passes::LoweringMode,
    ) -> Result<Pipeline<BoolarCircuitStage>, BoxError> {
        self.apply(FuseBoolar { limit, mode })
    }

    /// Terminal: the Boolar IR.
    pub fn to_boolar(self) -> BIrBlocks {
        self.into_data()
    }
}

impl Pipeline<BoolarCircuitStage> {
    /// Convert to a reversible gate circuit.
    pub fn to_reversible(self) -> Result<Pipeline<RCircuitStage>, BoxError> {
        self.apply(ToReversible::default())
    }

    /// Convert to a reversible gate circuit with an explicit embedding mode.
    pub fn to_reversible_with_mode(
        self,
        mode: volar_ir_passes::ReversibleMode,
    ) -> Result<Pipeline<RCircuitStage>, BoxError> {
        self.apply(ToReversible { mode })
    }

    /// Terminal: the circuit-fused Boolar IR.
    pub fn to_boolar_circuit(self) -> BCircuit {
        self.into_data()
    }

    /// Emit a reusable source package from the fused Boolar circuit.
    pub fn emit_source<B: CircuitSourceBackend>(
        self,
        backend: &B,
        opt: &EmitOptions,
    ) -> Result<SourcePackage, BoxError> {
        let circuit = self.into_data();
        emit_bool_circuit(&circuit, backend, opt).map_err(box_err)
    }
}

impl Pipeline<RCircuitStage> {
    /// Wrap an in-memory reversible circuit.
    pub fn from_rcircuit(circuit: RCircuit) -> Self {
        Self::from_data(circuit)
    }

    /// Lower back to circuit-fused Boolar IR.
    pub fn from_reversible(self) -> Result<Pipeline<BoolarCircuitStage>, BoxError> {
        self.apply(FromReversible)
    }

    /// Terminal: the reversible gate circuit.
    pub fn to_rcircuit(self) -> RCircuit {
        self.into_data()
    }
}

#[cfg(feature = "vaffle")]
impl Pipeline<VaffleStage> {
    /// Load a `.vaffle` file.
    pub fn from_vaffle(path: impl AsRef<Path2>) -> Result<Self, BoxError> {
        let bytes = std::fs::read(path.as_ref())?;
        let module = rkyv::from_bytes::<vaffle::Module, rkyv::rancor::Error>(&bytes)
            .map_err(|e| format!("failed to deserialize .vaffle file: {e}"))?;
        Ok(Self::from_data(module))
    }

    /// Wrap an in-memory VAFFLE module.
    pub fn from_vaffle_module(module: vaffle::Module) -> Self {
        Self::from_data(module)
    }

    /// Parse a `.wasm` file.
    #[cfg(feature = "wasm")]
    pub fn from_wasm(path: impl AsRef<Path2>) -> Result<Self, BoxError> {
        Self::from_wasm_with_config(path, volar_vaffle_target::WaffleImportConfig::new())
    }

    /// Parse a `.wasm` file with an explicit oracle/action import config.
    #[cfg(feature = "wasm")]
    pub fn from_wasm_with_config(
        path: impl AsRef<Path2>,
        import_config: volar_vaffle_target::WaffleImportConfig,
    ) -> Result<Self, BoxError> {
        let bytes = std::fs::read(path.as_ref())?;
        let waffle_module = portal_pc_waffle_frontend::from_wasm_bytes(
            &bytes,
            &portal_pc_waffle_frontend::FrontendOptions::default(),
        )
        .map_err(|e| format!("WAFFLE parse failed: {e}"))?;
        // WASM linear-memory addresses remain a 32-bit ABI even though the
        // generic VAFFLE target defaults to the host-friendly 64-bit ABI.
        let mut target =
            volar_vaffle_target::VaffleTarget::with_pointer_width(vaffle::PointerWidth::Bits32);
        volar_vaffle_target::lower_waffle_module(&waffle_module, &mut target, &import_config);
        Ok(Self::from_data(target.module))
    }

    /// [`Pipeline::from_wasm`] plus [`InlineVaffleEverything`] over every
    /// export.
    #[cfg(feature = "wasm")]
    pub fn from_wasm_inlined(path: impl AsRef<Path2>) -> Result<Self, BoxError> {
        Self::from_wasm(path)?.inline_vaffle_everything()
    }

    /// Parse LLVM bitcode (`.bc`), assembly (`.ll`), or an LTO static
    /// library (`.a` / `.lib`) via the call-preserving structural importer.
    #[cfg(feature = "llvm")]
    pub fn from_llvm(path: impl AsRef<Path2>, entries: &[&str]) -> Result<Self, BoxError> {
        Self::from_llvm_with_config(
            path,
            entries,
            volar_llvm_vaffle_import::LlvmImportConfig::default(),
        )
    }

    /// Configured structural LLVM import. The configuration may assert the
    /// expected pointer ABI but cannot override LLVM's data layout.
    #[cfg(feature = "llvm")]
    pub fn from_llvm_with_config(
        path: impl AsRef<Path2>,
        entries: &[&str],
        config: volar_llvm_vaffle_import::LlvmImportConfig,
    ) -> Result<Self, BoxError> {
        Self::from_llvm_origin_with_config(
            LlvmLibOrigin::Path(as_path_buf(path.as_ref())),
            entries,
            config,
        )
    }

    /// Structural LLVM import plus [`InlineVaffleEverything`] over `entries`.
    #[cfg(feature = "llvm")]
    pub fn from_llvm_inlined(path: impl AsRef<Path2>, entries: &[&str]) -> Result<Self, BoxError> {
        Self::from_llvm(path, entries)?.inline_vaffle_everything_over(entries)
    }

    /// Compile `build` to a static library of clang full-LTO objects, then
    /// import structurally. Requires a clang-like compiler; GCC LTO is
    /// rejected.
    #[cfg(feature = "cc")]
    pub fn from_cc(build: cc::Build, lib_name: &str, entries: &[&str]) -> Result<Self, BoxError> {
        Self::from_llvm_origin(cc_origin(build, lib_name), entries)
    }

    /// [`Pipeline::from_cc`] plus [`InlineVaffleEverything`] over `entries`.
    #[cfg(feature = "cc")]
    pub fn from_cc_inlined(
        build: cc::Build,
        lib_name: &str,
        entries: &[&str],
    ) -> Result<Self, BoxError> {
        Self::from_cc(build, lib_name, entries)?.inline_vaffle_everything_over(entries)
    }

    /// Run `cmd` (no shell) to produce a static library, then import
    /// structurally.
    #[cfg(feature = "llvm")]
    pub fn from_command(cmd: crate::CommandBuild, entries: &[&str]) -> Result<Self, BoxError> {
        Self::from_llvm_origin(LlvmLibOrigin::Command(cmd), entries)
    }

    /// [`Pipeline::from_command`] plus [`InlineVaffleEverything`] over
    /// `entries`.
    #[cfg(feature = "llvm")]
    pub fn from_command_inlined(
        cmd: crate::CommandBuild,
        entries: &[&str],
    ) -> Result<Self, BoxError> {
        Self::from_command(cmd, entries)?.inline_vaffle_everything_over(entries)
    }

    #[cfg(feature = "llvm")]
    fn from_llvm_origin(origin: LlvmLibOrigin, entries: &[&str]) -> Result<Self, BoxError> {
        Self::from_llvm_origin_with_config(
            origin,
            entries,
            volar_llvm_vaffle_import::LlvmImportConfig::default(),
        )
    }

    #[cfg(feature = "llvm")]
    fn from_llvm_origin_with_config(
        origin: LlvmLibOrigin,
        entries: &[&str],
        config: volar_llvm_vaffle_import::LlvmImportConfig,
    ) -> Result<Self, BoxError> {
        let path = materialize_llvm_lib(origin)?;
        let context = inkwell::context::Context::create();
        let llvm_module = load_llvm_module(&context, &path)?;
        let entry_refs: Vec<&str> = entries.iter().copied().collect();
        let module =
            volar_llvm_vaffle_import::import_module_with_config(&llvm_module, &entry_refs, config)?;
        Ok(Self::from_data(module))
    }

    /// Inline every non-recursive intra-module call, using every export (or
    /// every function body, if there are no exports) as the root set.
    #[cfg(feature = "vaffle")]
    pub fn inline_vaffle_everything(self) -> Result<Self, BoxError> {
        self.apply(InlineVaffleEverything::default())
    }

    /// Inline every non-recursive intra-module call reachable from
    /// `entries`.
    #[cfg(feature = "vaffle")]
    pub fn inline_vaffle_everything_over(self, entries: &[&str]) -> Result<Self, BoxError> {
        self.apply(InlineVaffleEverything {
            entries: entries.iter().map(|s| s.to_string()).collect(),
        })
    }

    /// Lower VAFFLE → Volar IR.
    #[cfg(feature = "vaffle")]
    pub fn lower_to_volar_ir(self) -> Result<Pipeline<VolarIrStage>, BoxError> {
        self.apply(LowerToVolarIr)
    }

    /// Terminal: the VAFFLE module.
    pub fn to_vaffle(self) -> vaffle::Module {
        self.into_data()
    }
}

/// Execution-mode (direct) LLVM import already produces `is_circuit()` Volar
/// IR — it lands on [`VolarIrStage`] directly rather than [`VaffleStage`].
#[cfg(feature = "llvm")]
impl Pipeline<VolarIrStage> {
    /// Import via the execution-mode (direct) importer. Accepts `.ll`,
    /// `.bc`, or an LTO static library.
    pub fn from_llvm_direct(path: impl AsRef<Path2>, entry: &str) -> Result<Self, BoxError> {
        Self::from_llvm_direct_origin(LlvmLibOrigin::Path(as_path_buf(path.as_ref())), entry)
    }

    /// [`Pipeline::from_cc`] then the execution-mode importer.
    #[cfg(feature = "cc")]
    pub fn from_cc_direct(build: cc::Build, lib_name: &str, entry: &str) -> Result<Self, BoxError> {
        Self::from_llvm_direct_origin(cc_origin(build, lib_name), entry)
    }

    /// [`Pipeline::from_command`] then the execution-mode importer.
    pub fn from_command_direct(cmd: crate::CommandBuild, entry: &str) -> Result<Self, BoxError> {
        Self::from_llvm_direct_origin(LlvmLibOrigin::Command(cmd), entry)
    }

    fn from_llvm_direct_origin(origin: LlvmLibOrigin, entry: &str) -> Result<Self, BoxError> {
        let path = materialize_llvm_lib(origin)?;
        let context = inkwell::context::Context::create();
        let llvm_module = load_llvm_module(&context, &path)?;
        let (blocks, types) = volar_llvm_ir_import::import_module(
            &llvm_module,
            entry,
            volar_llvm_ir_import::LoweringLimits::default(),
        )?;
        Ok(Self::from_data((blocks, types)))
    }
}

#[cfg(feature = "llvm")]
fn as_path_buf(p: &Path2) -> PathBuf {
    p.to_path_buf()
}

// ============================================================================
// LLVM plumbing (unchanged in spirit from the enum-based pipeline)
// ============================================================================

/// How an LLVM pipeline source is materialized at construction time.
#[cfg(feature = "llvm")]
enum LlvmLibOrigin {
    /// Existing `.ll`, `.bc`, or LTO static library (`.a` / `.lib`).
    Path(PathBuf),
    /// `cc::Build` that compiles a static library of full-LTO objects.
    #[cfg(feature = "cc")]
    Cc {
        build: Box<cc::Build>,
        lib_name: String,
        out_dir: PathBuf,
    },
    /// User command that must write [`crate::CommandBuild::output`].
    Command(crate::CommandBuild),
}

#[cfg(feature = "llvm")]
fn materialize_llvm_lib(origin: LlvmLibOrigin) -> Result<PathBuf, BoxError> {
    match origin {
        LlvmLibOrigin::Path(path) => Ok(path),
        #[cfg(feature = "cc")]
        LlvmLibOrigin::Cc {
            mut build,
            lib_name,
            out_dir,
        } => compile_cc_lto(&mut build, &lib_name, &out_dir),
        LlvmLibOrigin::Command(cmd) => cmd.run(),
    }
}

#[cfg(feature = "cc")]
fn cc_origin(mut build: cc::Build, lib_name: &str) -> LlvmLibOrigin {
    let out_dir = std::env::var_os("OUT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let dir = std::env::temp_dir().join(format!(
                "volar-ir-build-lto-{}-{}",
                std::process::id(),
                lib_name
            ));
            let _ = std::fs::create_dir_all(&dir);
            dir
        });
    build.out_dir(&out_dir);
    LlvmLibOrigin::Cc {
        build: Box::new(build),
        lib_name: lib_name.to_string(),
        out_dir,
    }
}

#[cfg(feature = "cc")]
fn compile_cc_lto(
    build: &mut cc::Build,
    lib_name: &str,
    out_dir: &Path,
) -> Result<PathBuf, BoxError> {
    build.out_dir(out_dir);
    build.cargo_metadata(false);
    build.static_flag(true);
    build.flag("-flto=full");
    let compiler = build
        .try_get_compiler()
        .map_err(|e| format!("cc LTO pre-build: failed to detect C compiler: {e}"))?;
    if !compiler.is_like_clang() {
        return Err(format!(
            "cc LTO pre-build requires a clang-like compiler (got {}); GCC LTO is not LLVM bitcode",
            compiler.path().display()
        )
        .into());
    }
    build
        .try_compile(lib_name)
        .map_err(|e| format!("cc LTO pre-build compile failed: {e}"))?;
    let archive = out_dir.join(format!("lib{lib_name}.a"));
    if !archive.exists() {
        return Err(format!("cc LTO pre-build did not write {}", archive.display()).into());
    }
    Ok(archive)
}

#[cfg(feature = "llvm")]
fn load_llvm_module<'ctx>(
    context: &'ctx inkwell::context::Context,
    path: &Path,
) -> Result<inkwell::module::Module<'ctx>, BoxError> {
    use inkwell::memory_buffer::MemoryBuffer;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let bytes = std::fs::read(path)?;
    // Unix `ar` magic (`!<arch>\n`) so a misnamed archive is still loaded
    // as members rather than as a single bitcode blob.
    if ext == "a" || ext == "lib" || bytes.starts_with(b"!<arch>\n") {
        return crate::lto_archive::load_lto_archive(context, path);
    }
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("module");
    if ext == "ll" {
        Ok(context
            .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(&bytes, name))?)
    } else {
        Ok(inkwell::module::Module::parse_bitcode_from_buffer(
            &MemoryBuffer::create_from_memory_range_copy(&bytes, name),
            context,
        )?)
    }
}

#[cfg(feature = "vaffle")]
fn resolve_inline_entries(
    module: &vaffle::Module,
    inline_entries: &[String],
) -> Result<Vec<vaffle::FuncId>, BoxError> {
    if inline_entries.is_empty() {
        if module.exports.is_empty() {
            let ids: Vec<vaffle::FuncId> = module
                .funcs
                .iter()
                .enumerate()
                .filter_map(|(i, d)| match d {
                    vaffle::FuncDecl::Body(_) => Some(vaffle::FuncId(i)),
                    _ => None,
                })
                .collect();
            if ids.is_empty() {
                return Err("InlineVaffleEverything: no function bodies to use as entries".into());
            }
            return Ok(ids);
        }
        return Ok(module.exports.values().copied().collect());
    }
    let mut ids = Vec::with_capacity(inline_entries.len());
    for name in inline_entries {
        let id = module
            .exports
            .get(name)
            .copied()
            .ok_or_else(|| format!("inline entry `{name}` is not exported"))?;
        ids.push(id);
    }
    Ok(ids)
}

pub(crate) fn lower_volar_ir_to_lir(blocks: &IRBlocks, types: &IRTypes) -> SavedLirModule {
    let mut rec = RecordingTarget::new();
    volar_ir_passes::lower_lir::lower_ir(blocks, types, "volar_module", &mut rec);
    rec.finish()
}

/// Serialize a VAFFLE [`Module`](vaffle::Module) to bytes for use as a `.vaffle`
/// file with [`Pipeline::<VaffleStage>::from_vaffle`].
#[cfg(feature = "vaffle")]
pub fn serialize_vaffle_module(module: &vaffle::Module) -> Result<Vec<u8>, BoxError> {
    Ok(rkyv::to_bytes::<rkyv::rancor::Error>(module)?.into_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use volar_ir::ir::{
        IRBlock, IRBlockId, IRBlockTargetId, IRBranchTarget, IRStmt, IRTerminator, IRType,
        IRTypeId, IRVarId,
    };
    use volar_ir_common::{Constant, Node, Type};

    fn bit_types() -> IRTypes {
        IRTypes(vec![IRType::Primitive(Type::Bit)])
    }

    #[test]
    fn unroll_const_branch_via_pipeline() {
        let types = bit_types();
        let blocks: IRBlocks<()> = IRBlocks::new(vec![
            IRBlock {
                params: vec![],
                stmts: vec![Node::new(
                    IRStmt::Const(Constant { hi: 0, lo: 1 }, IRTypeId(0)),
                    (),
                    None,
                )],
                terminator: IRTerminator::JumpCond {
                    condition: IRVarId(0),
                    then_target: IRBranchTarget::new(IRBlockTargetId::Block(IRBlockId(1)), vec![]),
                    else_target: IRBranchTarget::new(IRBlockTargetId::Block(IRBlockId(2)), vec![]),
                },
            },
            IRBlock {
                params: vec![],
                stmts: vec![Node::new(
                    IRStmt::Const(Constant { hi: 0, lo: 1 }, IRTypeId(0)),
                    (),
                    None,
                )],
                terminator: IRTerminator::Jmp {
                    target: IRBranchTarget::new(IRBlockTargetId::Return, vec![IRVarId(0)]),
                },
            },
            IRBlock {
                params: vec![],
                stmts: vec![Node::new(
                    IRStmt::Const(Constant { hi: 0, lo: 0 }, IRTypeId(0)),
                    (),
                    None,
                )],
                terminator: IRTerminator::Jmp {
                    target: IRBranchTarget::new(IRBlockTargetId::Return, vec![IRVarId(0)]),
                },
            },
        ]);
        let (out, _) = Pipeline::from_volar_ir_blocks(blocks, types)
            .unroll_ir()
            .expect("unroll via pipeline")
            .to_volar_ir();
        assert!(out.is_circuit());
    }

    #[test]
    fn unroll_rejects_symbolic_branch_via_pipeline() {
        let types = bit_types();
        let blocks: IRBlocks<()> = IRBlocks::new(vec![IRBlock {
            params: vec![IRTypeId(0)],
            stmts: vec![],
            terminator: IRTerminator::JumpCond {
                condition: IRVarId(0),
                then_target: IRBranchTarget::new(IRBlockTargetId::Return, vec![IRVarId(0)]),
                else_target: IRBranchTarget::new(IRBlockTargetId::Return, vec![IRVarId(0)]),
            },
        }]);
        match Pipeline::from_volar_ir_blocks(blocks, types).unroll_ir() {
            Ok(_) => panic!("expected a symbolic-branch error"),
            Err(e) => assert!(e.to_string().contains("symbolic branch")),
        }
    }
}
