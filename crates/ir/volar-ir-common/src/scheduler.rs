//! Deterministic, protocol-neutral execution over a schedule manifest.
//!
//! The scheduler owns a typed in-memory storage bank and transaction buffers.
//! It deliberately has no host-thread or cryptographic dependencies: host
//! adapters can run transactions on different workers, then commit fragments
//! in the manifest's canonical order.  A failed or out-of-order transaction
//! poisons the runtime rather than silently retrying under a different order.

use alloc::{collections::BTreeMap, vec::Vec};
use core::fmt;

use crate::schedule::{
    AddressScope, EpochId, EpochManifest, IterationId, IterationManifest, ManifestError,
    ScheduleLimits, StorageClaim, StorageClaimMode, StorageKey,
};

/// The kind of an observed storage operation.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum AccessKind {
    /// Read a cell without changing it.
    Read,
    /// Write a cell.
    Write,
}

/// One actual typed-cell operation in a logical trace fragment.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct StorageAccessEvent {
    /// Typed namespace accessed by the operation.
    pub key: StorageKey,
    /// Public cell address used by this storage operation.
    pub address: u64,
    /// Whether the operation read or wrote the cell.
    pub kind: AccessKind,
    /// Operation order within the iteration.
    pub sequence: u32,
}

/// The ordered trace emitted by one committed iteration.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TraceFragment {
    /// Epoch containing the iteration.
    pub epoch: EpochId,
    /// Canonical iteration identity.
    pub iteration: IterationId,
    /// Actual operations in logical operation order.
    pub events: Vec<StorageAccessEvent>,
}

/// Composite key for one typed storage cell.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct StorageCellKey {
    /// Typed storage namespace.
    pub key: StorageKey,
    /// Cell address within that namespace.
    pub address: u64,
}

impl StorageCellKey {
    /// Construct a typed cell key.
    pub const fn new(key: StorageKey, address: u64) -> Self {
        Self { key, address }
    }
}

/// Protocol-neutral storage backing a scheduler.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct SharedStorageBank<V> {
    cells: BTreeMap<StorageCellKey, V>,
}

impl<V> SharedStorageBank<V> {
    /// Construct an empty storage bank.
    pub fn new() -> Self {
        Self {
            cells: BTreeMap::new(),
        }
    }

    /// Insert or replace one typed storage cell.
    pub fn insert(&mut self, key: StorageKey, address: u64, value: V) {
        self.cells.insert(StorageCellKey::new(key, address), value);
    }

    /// Read one cell from the committed storage image.
    pub fn get(&self, key: StorageKey, address: u64) -> Option<&V> {
        self.cells.get(&StorageCellKey::new(key, address))
    }

    /// Number of materialized cells in the committed image.
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    /// Whether no cells have been materialized.
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }
}

/// A transaction-local write that has not reached the committed bank.
#[derive(Clone, Debug)]
struct BufferedWrite<V> {
    key: StorageKey,
    address: u64,
    sequence: u32,
    value: V,
}

/// A transaction opened against one manifest iteration.
#[derive(Clone, Debug)]
pub struct ScheduleTransaction<V> {
    iteration: IterationId,
    claims: Vec<StorageClaim>,
    writes: Vec<BufferedWrite<V>>,
    events: Vec<StorageAccessEvent>,
    closed: bool,
}

impl<V> ScheduleTransaction<V> {
    /// Canonical iteration represented by this transaction.
    pub const fn iteration(&self) -> IterationId {
        self.iteration
    }

    /// Claims reserved atomically when this transaction was opened.
    pub fn claims(&self) -> &[StorageClaim] {
        &self.claims
    }

    /// Actual operations recorded so far.
    pub fn events(&self) -> &[StorageAccessEvent] {
        &self.events
    }
}

/// Runtime failures are explicit so adapters can fail closed.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum SchedulerError {
    /// The manifest failed canonical validation.
    Manifest(ManifestError),
    /// No current epoch contains the requested iteration.
    UnknownIteration { iteration: IterationId },
    /// An iteration was requested after it had already completed.
    DuplicateCompletion { iteration: IterationId },
    /// A transaction was opened for a later epoch.
    EpochNotCurrent {
        iteration: IterationId,
        epoch: EpochId,
    },
    /// A dependency was not committed before this iteration began.
    DependencyNotCommitted {
        iteration: IterationId,
        dependency: IterationId,
    },
    /// A live transaction conflicts with the requested transaction.
    ConflictingClaims {
        active: IterationId,
        requested: IterationId,
    },
    /// An actual operation was not covered by the public claim set.
    UndeclaredAccess {
        iteration: IterationId,
        key: StorageKey,
        address: u64,
        kind: AccessKind,
    },
    /// A write was attempted through a read-only claim.
    AccessModeViolation {
        iteration: IterationId,
        key: StorageKey,
    },
    /// A transaction no longer belongs to the runtime's active set.
    TransactionClosed { iteration: IterationId },
    /// The transaction's trace sequence cannot be represented.
    TraceOverflow { iteration: IterationId },
    /// An epoch was finished while an iteration was still missing.
    IncompleteEpoch {
        epoch: EpochId,
        iteration: IterationId,
    },
    /// No epoch remains to finish.
    NoRemainingEpochs,
    /// A physical completion order contained an item more than once.
    InvalidCompletionOrder { iteration: IterationId },
    /// The operation callback failed; all buffered writes were discarded.
    OperationFailed,
    /// A previous access or scheduling error permanently aborted this runtime.
    RuntimeAborted,
}

impl fmt::Display for SchedulerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Manifest(error) => write!(f, "invalid schedule manifest: {error}"),
            Self::UnknownIteration { iteration } => write!(f, "unknown iteration {iteration:?}"),
            Self::DuplicateCompletion { iteration } => {
                write!(f, "iteration {iteration:?} completed more than once")
            }
            Self::EpochNotCurrent { iteration, epoch } => {
                write!(
                    f,
                    "iteration {iteration:?} is not in current epoch {epoch:?}"
                )
            }
            Self::DependencyNotCommitted {
                iteration,
                dependency,
            } => write!(
                f,
                "iteration {iteration:?} started before dependency {dependency:?} committed"
            ),
            Self::ConflictingClaims { active, requested } => write!(
                f,
                "active iteration {active:?} conflicts with requested {requested:?}"
            ),
            Self::UndeclaredAccess {
                iteration,
                key,
                address,
                kind,
            } => write!(
                f,
                "iteration {iteration:?} made undeclared {kind:?} access to {key:?}@{address}"
            ),
            Self::AccessModeViolation { iteration, key } => write!(
                f,
                "iteration {iteration:?} wrote through a read-only claim for {key:?}"
            ),
            Self::TransactionClosed { iteration } => {
                write!(f, "transaction {iteration:?} is closed")
            }
            Self::TraceOverflow { iteration } => {
                write!(f, "transaction {iteration:?} has too many accesses")
            }
            Self::IncompleteEpoch { epoch, iteration } => write!(
                f,
                "epoch {epoch:?} finished before iteration {iteration:?} committed"
            ),
            Self::NoRemainingEpochs => f.write_str("no schedule epoch remains"),
            Self::InvalidCompletionOrder { iteration } => {
                write!(f, "physical completion order repeats {iteration:?}")
            }
            Self::OperationFailed => f.write_str("transaction operation failed"),
            Self::RuntimeAborted => f.write_str("schedule runtime was aborted"),
        }
    }
}

impl From<ManifestError> for SchedulerError {
    fn from(error: ManifestError) -> Self {
        Self::Manifest(error)
    }
}

/// Deterministic execution state for a validated schedule.
#[derive(Clone, Debug)]
pub struct ScheduleRuntime<V> {
    manifest: crate::schedule::ScheduleManifest,
    bank: SharedStorageBank<V>,
    current_epoch: usize,
    active: Vec<IterationId>,
    completed: Vec<IterationId>,
    trace: Vec<TraceFragment>,
    aborted: bool,
}

impl<V> ScheduleRuntime<V> {
    /// Validate a manifest and create a runtime over the supplied storage.
    pub fn new(
        manifest: crate::schedule::ScheduleManifest,
        bank: SharedStorageBank<V>,
    ) -> Result<Self, SchedulerError> {
        manifest.validate(ScheduleLimits::default())?;
        Ok(Self {
            manifest,
            bank,
            current_epoch: 0,
            active: Vec::new(),
            completed: Vec::new(),
            trace: Vec::new(),
            aborted: false,
        })
    }

    /// The manifest driving this runtime.
    pub fn manifest(&self) -> &crate::schedule::ScheduleManifest {
        &self.manifest
    }

    /// The committed storage image.
    pub fn bank(&self) -> &SharedStorageBank<V> {
        &self.bank
    }

    /// Whether all epochs have been committed.
    pub fn is_complete(&self) -> bool {
        self.current_epoch == self.manifest.epochs.len() && !self.aborted
    }

    /// Ordered trace fragments accumulated so far.
    pub fn trace(&self) -> &[TraceFragment] {
        &self.trace
    }

    /// Open a transaction and atomically reserve every claim in canonical
    /// claim order.  There is no partial lock acquisition, so multi-key
    /// transactions cannot deadlock.
    pub fn begin(
        &mut self,
        iteration: IterationId,
    ) -> Result<ScheduleTransaction<V>, SchedulerError> {
        self.ensure_live()?;
        let (epoch, descriptor) = match self.find_iteration(iteration) {
            Ok(found) => found,
            Err(error) => return self.abort_with(error),
        };
        let epoch_id = epoch.ordinal;
        let descriptor = descriptor.clone();
        if epoch_id != self.current_epoch_id()? {
            return self.abort_with(SchedulerError::EpochNotCurrent {
                iteration,
                epoch: epoch_id,
            });
        }
        if self.completed.contains(&iteration) {
            return self.abort_with(SchedulerError::DuplicateCompletion { iteration });
        }
        if self.active.contains(&iteration) {
            return self.abort_with(SchedulerError::DuplicateCompletion { iteration });
        }
        for dependency in &descriptor.dependencies {
            if !self.completed.contains(dependency) {
                return self.abort_with(SchedulerError::DependencyNotCommitted {
                    iteration,
                    dependency: *dependency,
                });
            }
        }
        for active in &self.active {
            let (_, active_descriptor) = self.find_iteration(*active)?;
            if active_descriptor.claims.iter().any(|left| {
                descriptor
                    .claims
                    .iter()
                    .any(|right| left.conflicts_with(*right))
            }) {
                return self.abort_with(SchedulerError::ConflictingClaims {
                    active: *active,
                    requested: iteration,
                });
            }
        }
        self.active.push(iteration);
        Ok(ScheduleTransaction {
            iteration,
            claims: descriptor.claims,
            writes: Vec::new(),
            events: Vec::new(),
            closed: false,
        })
    }

    /// Read a cell, observing this transaction's buffered writes first.
    pub fn read(
        &self,
        transaction: &mut ScheduleTransaction<V>,
        key: StorageKey,
        address: u64,
    ) -> Result<Option<V>, SchedulerError>
    where
        V: Clone,
    {
        self.validate_transaction_access(transaction, key, address, AccessKind::Read)?;
        self.record_event(transaction, key, address, AccessKind::Read)?;
        if let Some(write) = transaction
            .writes
            .iter()
            .rev()
            .find(|write| write.key == key && write.address == address)
        {
            return Ok(Some(write.value.clone()));
        }
        Ok(self.bank.get(key, address).cloned())
    }

    /// Buffer a cell write.  No committed state changes until `commit`.
    pub fn write(
        &self,
        transaction: &mut ScheduleTransaction<V>,
        key: StorageKey,
        address: u64,
        value: V,
    ) -> Result<(), SchedulerError> {
        self.validate_transaction_access(transaction, key, address, AccessKind::Write)?;
        let sequence = self.record_event(transaction, key, address, AccessKind::Write)?;
        transaction.writes.push(BufferedWrite {
            key,
            address,
            sequence,
            value,
        });
        Ok(())
    }

    /// Commit all buffered writes and append a logical trace fragment.
    pub fn commit(
        &mut self,
        mut transaction: ScheduleTransaction<V>,
    ) -> Result<TraceFragment, SchedulerError> {
        self.ensure_live()?;
        self.ensure_transaction_active(&transaction)?;
        let epoch = self.current_epoch_id()?;
        transaction
            .writes
            .sort_unstable_by_key(|write| (write.key, write.address, write.sequence));
        for write in transaction.writes {
            self.bank.insert(write.key, write.address, write.value);
        }
        let fragment = TraceFragment {
            epoch,
            iteration: transaction.iteration,
            events: transaction.events,
        };
        self.remove_active(transaction.iteration);
        self.completed.push(transaction.iteration);
        self.trace.push(fragment.clone());
        self.trace
            .sort_unstable_by_key(|candidate| (candidate.epoch, candidate.iteration));
        Ok(fragment)
    }

    /// Abort a transaction and poison the runtime without applying writes.
    pub fn abort(&mut self, mut transaction: ScheduleTransaction<V>) {
        if self.active.contains(&transaction.iteration) {
            self.remove_active(transaction.iteration);
        }
        transaction.closed = true;
        self.aborted = true;
    }

    /// Finish the current epoch and return its fragments in logical order.
    pub fn finish_epoch(&mut self) -> Result<Vec<TraceFragment>, SchedulerError> {
        self.ensure_live()?;
        let epoch = self
            .manifest
            .epochs
            .get(self.current_epoch)
            .ok_or(SchedulerError::NoRemainingEpochs)?;
        if !self.active.is_empty() {
            return self.abort_with(SchedulerError::IncompleteEpoch {
                epoch: epoch.ordinal,
                iteration: self.active[0],
            });
        }
        for iteration in &epoch.iterations {
            if !self.completed.contains(&iteration.ordinal) {
                return self.abort_with(SchedulerError::IncompleteEpoch {
                    epoch: epoch.ordinal,
                    iteration: iteration.ordinal,
                });
            }
        }
        let mut fragments: Vec<TraceFragment> = self
            .trace
            .iter()
            .filter(|fragment| fragment.epoch == epoch.ordinal)
            .cloned()
            .collect();
        fragments.sort_unstable_by_key(|fragment| fragment.iteration);
        self.current_epoch += 1;
        Ok(fragments)
    }

    /// Execute one epoch in an arbitrary physical completion order while
    /// retaining canonical trace order.  Independent operations may therefore
    /// be completed by different host workers without changing the result.
    pub fn execute_epoch<F>(
        &mut self,
        physical_order: &[IterationId],
        mut operation: F,
    ) -> Result<Vec<TraceFragment>, SchedulerError>
    where
        F: FnMut(
            &Self,
            &IterationManifest,
            &mut ScheduleTransaction<V>,
        ) -> Result<(), SchedulerError>,
    {
        self.ensure_live()?;
        let expected = self
            .manifest
            .epochs
            .get(self.current_epoch)
            .ok_or(SchedulerError::NoRemainingEpochs)?
            .iterations
            .len();
        if physical_order.len() != expected {
            return self.abort_with(SchedulerError::IncompleteEpoch {
                epoch: self.current_epoch_id()?,
                iteration: self
                    .manifest
                    .epochs
                    .get(self.current_epoch)
                    .and_then(|epoch| epoch.iterations.first())
                    .map(|iteration| iteration.ordinal)
                    .unwrap_or_default(),
            });
        }
        for iteration_id in physical_order {
            let (_, descriptor) = match self.find_iteration(*iteration_id) {
                Ok(found) => found,
                Err(error) => return self.abort_with(error),
            };
            let descriptor = descriptor.clone();
            let mut transaction = self.begin(*iteration_id)?;
            if let Err(error) = operation(self, &descriptor, &mut transaction) {
                self.abort(transaction);
                return Err(error);
            }
            self.commit(transaction)?;
        }
        self.finish_epoch()
    }

    fn ensure_live(&self) -> Result<(), SchedulerError> {
        if self.aborted {
            Err(SchedulerError::RuntimeAborted)
        } else {
            Ok(())
        }
    }

    fn abort_with<T>(&mut self, error: SchedulerError) -> Result<T, SchedulerError> {
        self.aborted = true;
        Err(error)
    }

    fn current_epoch_id(&self) -> Result<EpochId, SchedulerError> {
        self.manifest
            .epochs
            .get(self.current_epoch)
            .map(|epoch| epoch.ordinal)
            .ok_or(SchedulerError::NoRemainingEpochs)
    }

    fn find_iteration(
        &self,
        iteration: IterationId,
    ) -> Result<(&EpochManifest, &IterationManifest), SchedulerError> {
        self.manifest
            .epochs
            .iter()
            .find_map(|epoch| {
                epoch
                    .iterations
                    .iter()
                    .find(|candidate| candidate.ordinal == iteration)
                    .map(|candidate| (epoch, candidate))
            })
            .ok_or(SchedulerError::UnknownIteration { iteration })
    }

    fn ensure_transaction_active(
        &self,
        transaction: &ScheduleTransaction<V>,
    ) -> Result<(), SchedulerError> {
        if transaction.closed || !self.active.contains(&transaction.iteration) {
            Err(SchedulerError::TransactionClosed {
                iteration: transaction.iteration,
            })
        } else {
            Ok(())
        }
    }

    fn validate_transaction_access(
        &self,
        transaction: &ScheduleTransaction<V>,
        key: StorageKey,
        address: u64,
        kind: AccessKind,
    ) -> Result<(), SchedulerError> {
        self.ensure_live()?;
        self.ensure_transaction_active(transaction)?;
        let matching_claims = transaction
            .claims
            .iter()
            .filter(|claim| claim.key == key && scope_contains(claim.scope, address));
        let mut has_claim = false;
        let mut permits_write = false;
        for claim in matching_claims {
            has_claim = true;
            permits_write |= !matches!(claim.mode.effective(), StorageClaimMode::Read);
        }
        if !has_claim {
            return Err(SchedulerError::UndeclaredAccess {
                iteration: transaction.iteration,
                key,
                address,
                kind,
            });
        }
        if matches!(kind, AccessKind::Write) && !permits_write {
            return Err(SchedulerError::AccessModeViolation {
                iteration: transaction.iteration,
                key,
            });
        }
        Ok(())
    }

    fn record_event(
        &self,
        transaction: &mut ScheduleTransaction<V>,
        key: StorageKey,
        address: u64,
        kind: AccessKind,
    ) -> Result<u32, SchedulerError> {
        self.ensure_transaction_active(transaction)?;
        let sequence =
            u32::try_from(transaction.events.len()).map_err(|_| SchedulerError::TraceOverflow {
                iteration: transaction.iteration,
            })?;
        transaction.events.push(StorageAccessEvent {
            key,
            address,
            kind,
            sequence,
        });
        Ok(sequence)
    }

    fn remove_active(&mut self, iteration: IterationId) {
        if let Some(index) = self
            .active
            .iter()
            .position(|candidate| *candidate == iteration)
        {
            self.active.remove(index);
        }
    }
}

fn scope_contains(scope: AddressScope, address: u64) -> bool {
    match scope {
        AddressScope::Whole => true,
        AddressScope::PublicRange { start, len } => start
            .checked_add(len)
            .is_some_and(|end| address >= start && address < end),
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::schedule::{CommunicationGroup, EpochManifest, IterationManifest, ScheduleManifest};

    fn key(storage: u32, type_id: u32) -> StorageKey {
        StorageKey::new(crate::StorageId(storage), crate::TypeId(type_id))
    }

    fn manifest(two_lanes: bool, conflicting: bool) -> ScheduleManifest {
        let first_key = key(1, 0);
        let second_key = if conflicting { first_key } else { key(2, 0) };
        let mut first = IterationManifest::new(IterationId(0), 0, crate::schedule::WorkerLane(0));
        first
            .claims
            .push(StorageClaim::whole(first_key, StorageClaimMode::Write));
        let mut second = IterationManifest::new(
            IterationId(1),
            1,
            crate::schedule::WorkerLane(if two_lanes { 1 } else { 0 }),
        );
        second
            .claims
            .push(StorageClaim::whole(second_key, StorageClaimMode::Write));
        let mut epoch = EpochManifest::new(EpochId(0), CommunicationGroup(0));
        epoch.iterations = vec![first, second];
        let mut manifest =
            ScheduleManifest::new([1; 32], [2; 32], 0, 2, if two_lanes { 2 } else { 1 });
        manifest.epochs.push(epoch);
        manifest
    }

    #[test]
    fn completion_order_does_not_change_committed_image_or_trace_order() {
        let run = |physical_order: &[IterationId], two_lanes: bool| {
            let mut runtime =
                ScheduleRuntime::new(manifest(two_lanes, false), SharedStorageBank::<u64>::new())
                    .unwrap();
            runtime
                .execute_epoch(physical_order, |runtime, iteration, transaction| {
                    let key = key(iteration.ordinal.0 as u32 + 1, 0);
                    runtime.write(transaction, key, 0, iteration.ordinal.0 + 10)
                })
                .unwrap();
            (
                runtime.bank().get(key(1, 0), 0).copied(),
                runtime.bank().get(key(2, 0), 0).copied(),
                runtime.trace().to_vec(),
            )
        };
        let canonical = run(&[IterationId(0), IterationId(1)], false);
        assert_eq!(canonical, run(&[IterationId(1), IterationId(0)], true));
        assert_eq!(canonical.2[0].iteration, IterationId(0));
        assert_eq!(canonical.2[1].iteration, IterationId(1));
    }

    #[test]
    fn conflicting_reservation_aborts_instead_of_retrying() {
        let mut runtime =
            ScheduleRuntime::new(manifest(true, true), SharedStorageBank::<u64>::new()).unwrap();
        let transaction = runtime.begin(IterationId(0)).unwrap();
        assert!(matches!(
            runtime.begin(IterationId(1)),
            Err(SchedulerError::ConflictingClaims { .. })
        ));
        runtime.abort(transaction);
        assert!(matches!(
            runtime.begin(IterationId(0)),
            Err(SchedulerError::RuntimeAborted)
        ));
    }

    #[test]
    fn failed_transaction_has_no_partial_writes() {
        let mut runtime =
            ScheduleRuntime::new(manifest(false, false), SharedStorageBank::<u64>::new()).unwrap();
        let result = runtime.execute_epoch(
            &[IterationId(0), IterationId(1)],
            |runtime, iteration, transaction| {
                let key = key(iteration.ordinal.0 as u32 + 1, 0);
                runtime.write(transaction, key, 0, 99)?;
                if iteration.ordinal == IterationId(0) {
                    return Err(SchedulerError::OperationFailed);
                }
                Ok(())
            },
        );
        assert!(matches!(result, Err(SchedulerError::OperationFailed)));
        assert!(runtime.bank().is_empty());
        assert!(matches!(
            runtime.begin(IterationId(0)),
            Err(SchedulerError::RuntimeAborted)
        ));
    }

    #[test]
    fn actual_access_must_match_claim_and_mode() {
        let mut runtime =
            ScheduleRuntime::new(manifest(false, false), SharedStorageBank::<u64>::new()).unwrap();
        let mut transaction = runtime.begin(IterationId(0)).unwrap();
        assert!(matches!(
            runtime.read(&mut transaction, key(9, 0), 0),
            Err(SchedulerError::UndeclaredAccess { .. })
        ));
        runtime.abort(transaction);

        let mut read_manifest = manifest(false, false);
        read_manifest.epochs[0].iterations[0].claims[0].mode = StorageClaimMode::Read;
        let mut runtime =
            ScheduleRuntime::new(read_manifest, SharedStorageBank::<u64>::new()).unwrap();
        let mut transaction = runtime.begin(IterationId(0)).unwrap();
        assert!(matches!(
            runtime.write(&mut transaction, key(1, 0), 0, 1),
            Err(SchedulerError::AccessModeViolation { .. })
        ));
        runtime.abort(transaction);
    }

    #[test]
    fn buffered_writes_merge_in_logical_order_before_commit() {
        let mut iteration =
            IterationManifest::new(IterationId(0), 0, crate::schedule::WorkerLane(0));
        iteration
            .claims
            .push(StorageClaim::whole(key(1, 0), StorageClaimMode::Write));
        let mut epoch = EpochManifest::new(EpochId(0), CommunicationGroup(0));
        epoch.iterations.push(iteration);
        let mut manifest = ScheduleManifest::new([0; 32], [0; 32], 0, 1, 1);
        manifest.epochs.push(epoch);
        let mut runtime = ScheduleRuntime::new(manifest, SharedStorageBank::<u64>::new()).unwrap();
        let fragments = runtime
            .execute_epoch(&[IterationId(0)], |runtime, _, transaction| {
                runtime.write(transaction, key(1, 0), 0, 1)?;
                runtime.write(transaction, key(1, 0), 0, 2)
            })
            .unwrap();
        assert_eq!(runtime.bank().get(key(1, 0), 0), Some(&2));
        assert_eq!(
            fragments[0]
                .events
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![0, 1]
        );
    }

    #[test]
    fn multi_key_claims_are_reserved_as_one_atomic_set() {
        let mut first = IterationManifest::new(IterationId(0), 0, crate::schedule::WorkerLane(0));
        first
            .claims
            .push(StorageClaim::whole(key(2, 0), StorageClaimMode::Write));
        first
            .claims
            .push(StorageClaim::whole(key(1, 0), StorageClaimMode::Write));
        first.canonicalize();
        assert!(first.claims.windows(2).all(|pair| pair[0] < pair[1]));
        let mut epoch = EpochManifest::new(EpochId(0), CommunicationGroup(0));
        epoch.iterations.push(first);
        let mut manifest = ScheduleManifest::new([0; 32], [0; 32], 0, 1, 1);
        manifest.epochs.push(epoch);
        let mut runtime = ScheduleRuntime::new(manifest, SharedStorageBank::<u64>::new()).unwrap();
        let transaction = runtime.begin(IterationId(0)).unwrap();
        assert_eq!(
            transaction.claims(),
            &[
                StorageClaim::whole(key(1, 0), StorageClaimMode::Write),
                StorageClaim::whole(key(2, 0), StorageClaimMode::Write)
            ]
        );
    }
}
