//! Conservative schedule extraction from circuit-shaped Volar and Boolar IR.
//!
//! This is a compiler-side analysis only.  It emits a canonical manifest for
//! fixed circuit-shaped programs; arbitrary control flow must use a later
//! bounded/runtime extractor or the serial fallback.  The analysis never uses
//! a secret address to select a public lock partition.

use alloc::vec::Vec;

use volar_ir::{
    boolar::{BIrBlock, BIrBlocks, BIrStmt, LaneId},
    ir::{IRBlock, IRBlocks, IRStmt},
};
use volar_ir_common::{
    ManifestError, StorageClaim, StorageClaimMode, StorageKey, StorageTable, StorageTableError,
    TypeId,
};
use volar_ir_common::{
    CommunicationGroup, EpochId, EpochManifest, IterationId, IterationManifest, ScheduleManifest,
    WorkerLane,
};

/// Inputs that identify a schedule independently of the storage operations
/// being extracted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ScheduleExtractionConfig {
    /// Digest of the compiled program/circuit selected by the backend.
    pub program_digest: [u8; 32],
    /// Digest of the storage policy and any public partition proof.
    pub storage_policy_digest: [u8; 32],
    /// Adapter-selected protocol profile identifier.
    pub protocol_profile: u32,
    /// Maximum iterations in an epoch.
    pub epoch_size: u32,
    /// Number of virtual worker lanes.
    pub worker_lane_count: u32,
}

impl ScheduleExtractionConfig {
    /// Construct a configuration for one fixed circuit iteration.
    pub const fn new(
        program_digest: [u8; 32],
        storage_policy_digest: [u8; 32],
        protocol_profile: u32,
        epoch_size: u32,
        worker_lane_count: u32,
    ) -> Self {
        Self {
            program_digest,
            storage_policy_digest,
            protocol_profile,
            epoch_size,
            worker_lane_count,
        }
    }
}

/// Conservative typed storage claims discovered in one IR block.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct StorageAccessSummary {
    /// Canonical, coalesced claims.
    pub claims: Vec<StorageClaim>,
}

impl StorageAccessSummary {
    /// Extract claims from a typed Volar IR block.
    pub fn from_ir_block<P: Clone>(
        block: &IRBlock<P>,
        storage_table: &StorageTable,
    ) -> Result<Self, ScheduleExtractionError> {
        let mut claims = Vec::new();
        for node in &block.stmts {
            match &node.kind {
                IRStmt::StorageRead { storage, ty, .. } => claims.push(classify(
                    StorageKey::new(*storage, *ty),
                    false,
                    storage_table,
                )?),
                IRStmt::StorageWrite { storage, ty, .. } => claims.push(classify(
                    StorageKey::new(*storage, *ty),
                    true,
                    storage_table,
                )?),
                _ => {}
            }
        }
        canonicalize_claims(&mut claims);
        Ok(Self { claims })
    }

    /// Extract claims from one Boolar block.  Boolar lanes occupy the shared
    /// `TypeId` slot in the protocol-neutral schedule identity.
    pub fn from_biir_block<P: Clone>(
        block: &BIrBlock<P>,
        storage_table: &StorageTable,
    ) -> Result<Self, ScheduleExtractionError> {
        let mut claims = Vec::new();
        for node in &block.stmts {
            match &node.kind {
                BIrStmt::StorageRead { storage, lane, .. } => claims.push(classify(
                    StorageKey::new(*storage, lane_type(*lane)),
                    false,
                    storage_table,
                )?),
                BIrStmt::StorageWrite { storage, lane, .. } => claims.push(classify(
                    StorageKey::new(*storage, lane_type(*lane)),
                    true,
                    storage_table,
                )?),
                BIrStmt::ActionStoreBit { storage, lane, .. } => claims.push(classify(
                    StorageKey::new(*storage, lane_type(*lane)),
                    true,
                    storage_table,
                )?),
                _ => {}
            }
        }
        canonicalize_claims(&mut claims);
        Ok(Self { claims })
    }
}

/// Extract the initial fixed schedule for one circuit-shaped typed IR.
///
/// A single circuit-shaped block is represented as one iteration.  This is
/// intentionally conservative and is suitable as the first manifest seam for
/// VOLE.  Multi-block control flow is rejected until a pass supplies a public
/// iteration order or an explicit serial fallback.
pub fn extract_ir_circuit_schedule<P: Clone>(
    circuit: &IRBlocks<P>,
    storage_table: &StorageTable,
    config: ScheduleExtractionConfig,
) -> Result<ScheduleManifest, ScheduleExtractionError> {
    if circuit.blocks.is_empty() {
        return Err(ScheduleExtractionError::EmptyProgram);
    }
    if !circuit.is_circuit() {
        return Err(ScheduleExtractionError::UnsupportedShape {
            blocks: circuit.blocks.len(),
        });
    }
    storage_table.validate()?;
    let summary = StorageAccessSummary::from_ir_block(&circuit.blocks[0], storage_table)?;
    one_iteration_manifest(summary, config)
}

/// Extract the initial fixed schedule for one circuit-shaped Boolar IR.
///
/// Boolar's `LaneId` is preserved structurally by mapping it into the shared
/// typed-lane field; it is never converted into a string or merged with a
/// different storage namespace.
pub fn extract_biir_circuit_schedule<P: Clone>(
    circuit: &BIrBlocks<P>,
    storage_table: &StorageTable,
    config: ScheduleExtractionConfig,
) -> Result<ScheduleManifest, ScheduleExtractionError> {
    if circuit.blocks.is_empty() {
        return Err(ScheduleExtractionError::EmptyProgram);
    }
    if !circuit.is_circuit() {
        return Err(ScheduleExtractionError::UnsupportedShape {
            blocks: circuit.blocks.len(),
        });
    }
    storage_table.validate()?;
    let summary = StorageAccessSummary::from_biir_block(&circuit.blocks[0], storage_table)?;
    one_iteration_manifest(summary, config)
}

fn one_iteration_manifest(
    summary: StorageAccessSummary,
    config: ScheduleExtractionConfig,
) -> Result<ScheduleManifest, ScheduleExtractionError> {
    let mut manifest = ScheduleManifest::new(
        config.program_digest,
        config.storage_policy_digest,
        config.protocol_profile,
        config.epoch_size,
        config.worker_lane_count,
    );
    let mut epoch = EpochManifest::new(EpochId(0), CommunicationGroup(0));
    let mut iteration = IterationManifest::new(IterationId(0), 0, WorkerLane(0));
    iteration.claims = summary.claims;
    epoch.iterations.push(iteration);
    manifest.epochs.push(epoch);
    manifest.canonicalize();
    manifest.validate(Default::default())?;
    Ok(manifest)
}

fn classify(
    key: StorageKey,
    writes: bool,
    storage_table: &StorageTable,
) -> Result<StorageClaim, ScheduleExtractionError> {
    volar_ir_common::classify_storage_claim(key, writes, storage_table)
        .map_err(ScheduleExtractionError::Manifest)
}

fn lane_type(lane: LaneId) -> TypeId {
    TypeId(lane.0)
}

fn canonicalize_claims(claims: &mut Vec<StorageClaim>) {
    claims.sort_unstable();
    let mut canonical: Vec<StorageClaim> = Vec::with_capacity(claims.len());
    for claim in claims.drain(..) {
        if let Some(previous) = canonical.last_mut()
            && previous.key == claim.key
            && previous.scope == claim.scope
        {
            previous.mode = join_mode(previous.mode, claim.mode);
        } else {
            canonical.push(claim);
        }
    }
    *claims = canonical;
}

fn join_mode(left: StorageClaimMode, right: StorageClaimMode) -> StorageClaimMode {
    match (left, right) {
        (StorageClaimMode::Read, StorageClaimMode::Read) => StorageClaimMode::Read,
        (StorageClaimMode::Unknown, _) | (_, StorageClaimMode::Unknown) => {
            StorageClaimMode::Unknown
        }
        _ => StorageClaimMode::Write,
    }
}

/// Extraction failure.  All failures are explicit so callers can choose the
/// serial fallback rather than emitting an incomplete public schedule.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ScheduleExtractionError {
    /// No executable block was supplied.
    EmptyProgram,
    /// The input needs a public iteration extractor before scheduling.
    UnsupportedShape { blocks: usize },
    /// The storage sidecar was malformed.
    StorageTable(StorageTableError),
    /// The claim or resulting manifest is invalid.
    Manifest(ManifestError),
}

impl core::fmt::Display for ScheduleExtractionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::EmptyProgram => f.write_str("cannot extract a schedule from an empty program"),
            Self::UnsupportedShape { blocks } => write!(
                f,
                "schedule extraction requires one circuit-shaped block, found {blocks} blocks"
            ),
            Self::StorageTable(error) => write!(f, "invalid storage table: {error}"),
            Self::Manifest(error) => write!(f, "invalid schedule manifest: {error}"),
        }
    }
}

impl From<StorageTableError> for ScheduleExtractionError {
    fn from(error: StorageTableError) -> Self {
        Self::StorageTable(error)
    }
}

impl From<ManifestError> for ScheduleExtractionError {
    fn from(error: ManifestError) -> Self {
        Self::Manifest(error)
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use volar_ir::ir::{IRBlock, IRBlockTargetId, IRBlocks, IRBranchTarget, IRTerminator};
    use volar_ir_common::Node;
    use volar_ir_common::{StorageAccess, StorageId, TypeId};

    use super::*;

    fn config() -> ScheduleExtractionConfig {
        ScheduleExtractionConfig::new([1; 32], [2; 32], 9, 4, 2)
    }

    fn ir_block(stmts: Vec<IRStmt>) -> IRBlock {
        IRBlock {
            params: Vec::new(),
            stmts: stmts
                .into_iter()
                .map(|kind| Node::new(kind, (), None))
                .collect(),
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, Vec::new()),
            },
        }
    }

    #[test]
    fn typed_circuit_extraction_is_conservative_for_missing_policy() {
        let circuit = IRBlocks::new(vec![ir_block(vec![
            IRStmt::StorageRead {
                storage: StorageId(3),
                ty: TypeId(8),
                addr: volar_ir::ir::IRVarId(0),
            },
            IRStmt::StorageWrite {
                storage: StorageId(3),
                src: volar_ir::ir::IRVarId(1),
                ty: TypeId(8),
                addr: volar_ir::ir::IRVarId(2),
            },
        ])]);
        let manifest = extract_ir_circuit_schedule(&circuit, &StorageTable::new(), config()).unwrap();
        let claims = &manifest.epochs[0].iterations[0].claims;
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0].mode, StorageClaimMode::Unknown);
        assert_eq!(claims[0].key, StorageKey::new(StorageId(3), TypeId(8)));
    }

    #[test]
    fn read_only_policy_allows_reads_and_rejects_writes() {
        let mut table = StorageTable::new();
        table.set(StorageId(3), StorageAccess::ReadOnly);
        let circuit = IRBlocks::new(vec![ir_block(vec![IRStmt::StorageRead {
            storage: StorageId(3),
            ty: TypeId(8),
            addr: volar_ir::ir::IRVarId(0),
        }])]);
        let manifest = extract_ir_circuit_schedule(&circuit, &table, config()).unwrap();
        assert_eq!(
            manifest.epochs[0].iterations[0].claims[0].mode,
            StorageClaimMode::Read
        );

        let circuit = IRBlocks::new(vec![ir_block(vec![IRStmt::StorageWrite {
            storage: StorageId(3),
            src: volar_ir::ir::IRVarId(1),
            ty: TypeId(8),
            addr: volar_ir::ir::IRVarId(0),
        }])]);
        assert!(matches!(
            extract_ir_circuit_schedule(&circuit, &table, config()),
            Err(ScheduleExtractionError::Manifest(
                ManifestError::ReadOnlyViolation { .. }
            ))
        ));
    }

    #[test]
    fn non_circuit_control_flow_fails_closed() {
        let circuit = IRBlocks {
            oracles: Vec::new(),
            actions: Vec::new(),
            rngs: Vec::new(),
            blocks: vec![ir_block(Vec::new()), ir_block(Vec::new())],
            pre_init: Vec::new(),
        };
        assert!(matches!(
            extract_ir_circuit_schedule(&circuit, &StorageTable::new(), config()),
            Err(ScheduleExtractionError::UnsupportedShape { blocks: 2 })
        ));
    }

    #[test]
    fn claims_from_empty_circuit_are_empty_but_manifest_is_valid() {
        let circuit = IRBlocks::new(vec![ir_block(Vec::new())]);
        let manifest = extract_ir_circuit_schedule(&circuit, &StorageTable::new(), config()).unwrap();
        assert!(manifest.epochs[0].iterations[0].claims.is_empty());
        assert!(manifest.validate(Default::default()).is_ok());
    }
}
