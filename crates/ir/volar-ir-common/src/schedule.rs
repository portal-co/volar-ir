//! Canonical execution-schedule metadata for shared-storage runtimes.
//!
//! This module contains only protocol-neutral identities and validation.  It
//! does not acquire host locks, execute a protocol, or make a cryptographic
//! security claim.  Backends use the canonical manifest produced here to bind
//! worker execution, storage traces, correlation ranges, and communication
//! frames to one public order.

use alloc::vec::Vec;
use core::fmt;

use crate::{StorageAccess, StorageId, StorageTable, TypeId};

/// Current serialized schedule-manifest format.
pub const SCHEDULE_MANIFEST_VERSION: u16 = 1;

/// A typed storage namespace used as the conservative lock unit.
///
/// Boolar adapters map their lane identity into `type_id`; the schedule layer
/// deliberately does not flatten either component into a textual name.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct StorageKey {
    /// Storage namespace identity.
    pub storage: StorageId,
    /// Typed lane identity within that namespace.
    pub type_id: TypeId,
}

impl StorageKey {
    /// Construct a typed storage key.
    pub const fn new(storage: StorageId, type_id: TypeId) -> Self {
        Self { storage, type_id }
    }
}

/// Public address scope used to refine a storage claim.
///
/// `Whole` is the required conservative default.  `PublicRange` is reserved
/// for an adapter that has separately proved that the range is public and
/// stable; secret addresses must never select a schedule-visible partition.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum AddressScope {
    /// Claim the complete typed storage namespace.
    Whole,
    /// Claim a public contiguous range of cell addresses.
    PublicRange { start: u64, len: u64 },
}

impl AddressScope {
    /// Whether two public scopes can touch a common cell.
    pub const fn overlaps(self, other: Self) -> bool {
        match (self, other) {
            (Self::Whole, _) | (_, Self::Whole) => true,
            (
                Self::PublicRange {
                    start: left_start,
                    len: left_len,
                },
                Self::PublicRange {
                    start: right_start,
                    len: right_len,
                },
            ) => {
                let left_end = match left_start.checked_add(left_len) {
                    Some(end) => end,
                    None => return true,
                };
                let right_end = match right_start.checked_add(right_len) {
                    Some(end) => end,
                    None => return true,
                };
                left_start < right_end && right_start < left_end
            }
        }
    }

    fn validate(self) -> Result<(), ManifestError> {
        match self {
            Self::Whole => Ok(()),
            Self::PublicRange { len: 0, .. } => Err(ManifestError::EmptyAddressRange),
            Self::PublicRange { start, len } => start
                .checked_add(len)
                .map(|_| ())
                .ok_or(ManifestError::AddressRangeOverflow { start, len }),
        }
    }
}

/// Access certainty for one iteration's typed storage claim.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum StorageClaimMode {
    /// The iteration only observes the namespace.
    Read,
    /// The iteration may mutate the namespace.
    Write,
    /// Analysis could not prove the operation set; treat it as `Write`.
    Unknown,
}

impl StorageClaimMode {
    /// Return the lock mode used for conflict checks.
    pub const fn effective(self) -> Self {
        match self {
            Self::Unknown => Self::Write,
            mode => mode,
        }
    }

    const fn join(self, other: Self) -> Self {
        match (self, other) {
            (Self::Read, Self::Read) => Self::Read,
            (Self::Unknown, _) | (_, Self::Unknown) => Self::Unknown,
            _ => Self::Write,
        }
    }
}

/// One typed storage lock/access claim made by an iteration.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct StorageClaim {
    /// Typed namespace being accessed.
    pub key: StorageKey,
    /// Whole namespace unless a public partition was proven.
    pub scope: AddressScope,
    /// Conservative access mode.
    pub mode: StorageClaimMode,
}

impl StorageClaim {
    /// Construct a conservative whole-namespace claim.
    pub const fn whole(key: StorageKey, mode: StorageClaimMode) -> Self {
        Self {
            key,
            mode,
            scope: AddressScope::Whole,
        }
    }

    /// Whether this claim conflicts with another claim.
    pub fn conflicts_with(self, other: Self) -> bool {
        self.key.storage == other.key.storage
            && self.key.type_id == other.key.type_id
            && self.scope.overlaps(other.scope)
            && !(matches!(self.mode.effective(), StorageClaimMode::Read)
                && matches!(other.mode.effective(), StorageClaimMode::Read))
    }

    fn validate(self) -> Result<(), ManifestError> {
        self.scope.validate()
    }
}

/// Classify one analyzed access using the conservative storage sidecar.
///
/// A missing declaration is intentionally not interpreted as read-only.  A
/// read from an undeclared namespace therefore becomes `Unknown`, which has
/// the effective exclusive lock mode.
pub fn classify_storage_claim(
    key: StorageKey,
    writes: bool,
    storage_table: &StorageTable,
) -> Result<StorageClaim, ManifestError> {
    if writes {
        if storage_table.access_of(key.storage) == StorageAccess::ReadOnly {
            return Err(ManifestError::ReadOnlyViolation { key });
        }
        return Ok(StorageClaim::whole(key, StorageClaimMode::Write));
    }

    let mode = match storage_table.access_of(key.storage) {
        StorageAccess::ReadOnly => StorageClaimMode::Read,
        StorageAccess::ReadWrite => StorageClaimMode::Unknown,
    };
    Ok(StorageClaim::whole(key, mode))
}

/// A half-open reserved range in a protocol-specific ordinal space.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct OrdinalRange {
    /// First reserved ordinal.
    pub start: u64,
    /// Number of reserved ordinals.
    pub len: u64,
}

impl OrdinalRange {
    /// Construct an ordinal range.
    pub const fn new(start: u64, len: u64) -> Self {
        Self { start, len }
    }

    fn end(self) -> Result<u64, ManifestError> {
        self.start
            .checked_add(self.len)
            .ok_or(ManifestError::OrdinalRangeOverflow {
                start: self.start,
                len: self.len,
            })
    }

    fn overlaps(self, other: Self) -> Result<bool, ManifestError> {
        Ok(self.start < other.end()? && other.start < self.end()?)
    }
}

/// Stable worker-lane identity.  A lane is a schedule hint, not an OS thread.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct WorkerLane(pub u32);

/// Stable epoch identity.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct EpochId(pub u64);

/// Stable logical iteration identity.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct IterationId(pub u64);

/// Stable communication-group identity.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Default)]
pub struct CommunicationGroup(pub u64);

/// One canonical iteration inside an execution epoch.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct IterationManifest {
    /// Global canonical iteration ordinal.
    pub ordinal: IterationId,
    /// Source/circuit step identity used for diagnostics and replay.
    pub logical_step: u64,
    /// Deterministic virtual-core assignment.
    pub worker_lane: WorkerLane,
    /// Earlier iteration ordinals whose values must be available first.
    pub dependencies: Vec<IterationId>,
    /// Conservative typed storage claims.
    pub claims: Vec<StorageClaim>,
    /// Reserved VOLE/preprocessing range, if this iteration consumes one.
    pub correlation_range: OrdinalRange,
    /// Reserved logical communication-message range.
    pub communication_range: OrdinalRange,
}

impl IterationManifest {
    /// Create an iteration with no dependencies, claims, or reservations.
    pub fn new(ordinal: IterationId, logical_step: u64, worker_lane: WorkerLane) -> Self {
        Self {
            ordinal,
            logical_step,
            worker_lane,
            dependencies: Vec::new(),
            claims: Vec::new(),
            correlation_range: OrdinalRange::default(),
            communication_range: OrdinalRange::default(),
        }
    }

    /// Sort dependencies and canonicalize/coalesce exact duplicate claims.
    pub fn canonicalize(&mut self) {
        self.dependencies.sort_unstable();
        self.dependencies.dedup();
        self.claims.sort_unstable();
        let mut canonical: Vec<StorageClaim> = Vec::with_capacity(self.claims.len());
        for claim in self.claims.drain(..) {
            if let Some(previous) = canonical.last_mut()
                && previous.key == claim.key
                && previous.scope == claim.scope
            {
                previous.mode = previous.mode.join(claim.mode);
            } else {
                canonical.push(claim);
            }
        }
        self.claims = canonical;
    }
}

/// One bounded consistency epoch.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct EpochManifest {
    /// Canonical epoch ordinal.
    pub ordinal: EpochId,
    /// Iterations in canonical execution order.
    pub iterations: Vec<IterationManifest>,
    /// Transport grouping identity for this epoch.
    pub communication_group: CommunicationGroup,
}

impl EpochManifest {
    /// Construct an empty epoch.
    pub fn new(ordinal: EpochId, communication_group: CommunicationGroup) -> Self {
        Self {
            ordinal,
            iterations: Vec::new(),
            communication_group,
        }
    }
}

/// Public schedule manifest shared by compiler, runtime, and verifier.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct ScheduleManifest {
    /// Canonical format version.
    pub format_version: u16,
    /// Digest of the compiled program/circuit selected by the adapter.
    pub program_digest: [u8; 32],
    /// Digest of storage mutability/partition policy.
    pub storage_policy_digest: [u8; 32],
    /// Adapter-selected protocol profile identifier.
    pub protocol_profile: u32,
    /// Maximum intended iterations per epoch.
    pub epoch_size: u32,
    /// Number of deterministic virtual-core lanes.
    pub worker_lane_count: u32,
    /// Canonical epochs.
    pub epochs: Vec<EpochManifest>,
}

impl ScheduleManifest {
    /// Construct an empty manifest for the current format.
    pub fn new(
        program_digest: [u8; 32],
        storage_policy_digest: [u8; 32],
        protocol_profile: u32,
        epoch_size: u32,
        worker_lane_count: u32,
    ) -> Self {
        Self {
            format_version: SCHEDULE_MANIFEST_VERSION,
            program_digest,
            storage_policy_digest,
            protocol_profile,
            epoch_size,
            worker_lane_count,
            epochs: Vec::new(),
        }
    }

    /// Canonicalize nested vectors without changing iteration order.
    pub fn canonicalize(&mut self) {
        for epoch in &mut self.epochs {
            for iteration in &mut epoch.iterations {
                iteration.canonicalize();
            }
        }
    }

    /// Validate canonical order, bounds, dependencies, and reservation ranges.
    pub fn validate(&self, limits: ScheduleLimits) -> Result<(), ManifestError> {
        if self.format_version != SCHEDULE_MANIFEST_VERSION {
            return Err(ManifestError::UnsupportedVersion {
                found: self.format_version,
            });
        }
        if self.epoch_size == 0 {
            return Err(ManifestError::ZeroEpochSize);
        }
        if self.worker_lane_count == 0 {
            return Err(ManifestError::ZeroWorkerLanes);
        }
        if self.epochs.len() > limits.max_epochs {
            return Err(ManifestError::TooManyEpochs {
                found: self.epochs.len(),
                max: limits.max_epochs,
            });
        }

        let mut expected_epoch = 0u64;
        let mut expected_iteration = 0u64;
        let mut total_iterations = 0usize;
        let mut correlation_ranges = Vec::new();
        let mut communication_ranges = Vec::new();

        for epoch in &self.epochs {
            if epoch.ordinal.0 != expected_epoch {
                return Err(ManifestError::NonCanonicalEpoch {
                    expected: expected_epoch,
                    found: epoch.ordinal.0,
                });
            }
            expected_epoch = expected_epoch
                .checked_add(1)
                .ok_or(ManifestError::OrdinalOverflow)?;
            if epoch.iterations.len() > self.epoch_size as usize {
                return Err(ManifestError::EpochTooLarge {
                    epoch: epoch.ordinal,
                    found: epoch.iterations.len(),
                    max: self.epoch_size as usize,
                });
            }

            for iteration in &epoch.iterations {
                if iteration.ordinal.0 != expected_iteration {
                    return Err(ManifestError::NonCanonicalIteration {
                        expected: expected_iteration,
                        found: iteration.ordinal.0,
                    });
                }
                expected_iteration = expected_iteration
                    .checked_add(1)
                    .ok_or(ManifestError::OrdinalOverflow)?;
                total_iterations += 1;
                if total_iterations > limits.max_iterations {
                    return Err(ManifestError::TooManyIterations {
                        found: total_iterations,
                        max: limits.max_iterations,
                    });
                }
                if iteration.worker_lane.0 >= self.worker_lane_count {
                    return Err(ManifestError::WorkerLaneOutOfRange {
                        iteration: iteration.ordinal,
                        lane: iteration.worker_lane,
                    });
                }
                if iteration.dependencies.len() > limits.max_dependencies {
                    return Err(ManifestError::TooManyDependencies {
                        iteration: iteration.ordinal,
                        found: iteration.dependencies.len(),
                        max: limits.max_dependencies,
                    });
                }
                if iteration.claims.len() > limits.max_claims {
                    return Err(ManifestError::TooManyClaims {
                        iteration: iteration.ordinal,
                        found: iteration.claims.len(),
                        max: limits.max_claims,
                    });
                }
                if !is_sorted_unique(&iteration.dependencies) {
                    return Err(ManifestError::NonCanonicalDependencies {
                        iteration: iteration.ordinal,
                    });
                }
                if !is_sorted_unique(&iteration.claims) {
                    return Err(ManifestError::NonCanonicalClaims {
                        iteration: iteration.ordinal,
                    });
                }
                for dependency in &iteration.dependencies {
                    if dependency.0 >= iteration.ordinal.0 {
                        return Err(ManifestError::ForwardDependency {
                            iteration: iteration.ordinal,
                            dependency: *dependency,
                        });
                    }
                }
                for claim in &iteration.claims {
                    claim.validate()?;
                }
                add_range(
                    &mut correlation_ranges,
                    iteration.correlation_range,
                    RangeKind::Correlation,
                    iteration.ordinal,
                )?;
                add_range(
                    &mut communication_ranges,
                    iteration.communication_range,
                    RangeKind::Communication,
                    iteration.ordinal,
                )?;
            }
        }
        Ok(())
    }

    /// Return the canonical byte encoding used as input to a protocol digest.
    pub fn canonical_bytes(&self, limits: ScheduleLimits) -> Result<Vec<u8>, ManifestError> {
        self.validate(limits)?;
        let mut out = Vec::new();
        out.extend_from_slice(b"volar.schedule\0");
        push_u16(&mut out, self.format_version);
        out.extend_from_slice(&self.program_digest);
        out.extend_from_slice(&self.storage_policy_digest);
        push_u32(&mut out, self.protocol_profile);
        push_u32(&mut out, self.epoch_size);
        push_u32(&mut out, self.worker_lane_count);
        push_len(&mut out, self.epochs.len())?;
        for epoch in &self.epochs {
            push_u64(&mut out, epoch.ordinal.0);
            push_u64(&mut out, epoch.communication_group.0);
            push_len(&mut out, epoch.iterations.len())?;
            for iteration in &epoch.iterations {
                push_u64(&mut out, iteration.ordinal.0);
                push_u64(&mut out, iteration.logical_step);
                push_u32(&mut out, iteration.worker_lane.0);
                push_len(&mut out, iteration.dependencies.len())?;
                for dependency in &iteration.dependencies {
                    push_u64(&mut out, dependency.0);
                }
                push_len(&mut out, iteration.claims.len())?;
                for claim in &iteration.claims {
                    push_u32(&mut out, claim.key.storage.0);
                    push_u32(&mut out, claim.key.type_id.0);
                    out.push(match claim.mode {
                        StorageClaimMode::Read => 0,
                        StorageClaimMode::Write => 1,
                        StorageClaimMode::Unknown => 2,
                    });
                    match claim.scope {
                        AddressScope::Whole => out.push(0),
                        AddressScope::PublicRange { start, len } => {
                            out.push(1);
                            push_u64(&mut out, start);
                            push_u64(&mut out, len);
                        }
                    }
                }
                push_range(&mut out, iteration.correlation_range);
                push_range(&mut out, iteration.communication_range);
            }
        }
        Ok(out)
    }

    /// Hash the canonical encoding with an adapter-selected digest.
    ///
    /// The trait intentionally has no default algorithm.  This layer only
    /// defines the bytes and binding domain; protocol crates select and review
    /// their hash profile.
    pub fn digest_with<H: ScheduleHasher>(
        &self,
        limits: ScheduleLimits,
        hasher: &H,
    ) -> Result<[u8; 32], ManifestError> {
        Ok(hasher.digest(&self.canonical_bytes(limits)?))
    }
}

/// Adapter-provided digest for a canonical schedule encoding.
pub trait ScheduleHasher {
    /// Return the adapter's 32-byte digest of the supplied domain-separated
    /// canonical bytes.
    fn digest(&self, bytes: &[u8]) -> [u8; 32];
}

/// Resource limits used while validating and encoding untrusted manifests.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct ScheduleLimits {
    /// Maximum number of epochs.
    pub max_epochs: usize,
    /// Maximum total iteration count.
    pub max_iterations: usize,
    /// Maximum dependency count per iteration.
    pub max_dependencies: usize,
    /// Maximum claim count per iteration.
    pub max_claims: usize,
}

impl Default for ScheduleLimits {
    fn default() -> Self {
        Self {
            max_epochs: 1 << 20,
            max_iterations: 1 << 24,
            max_dependencies: 1 << 16,
            max_claims: 1 << 16,
        }
    }
}

/// Validation/encoding failure for a schedule manifest.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ManifestError {
    /// The manifest format is not supported.
    UnsupportedVersion { found: u16 },
    /// Epochs must have a positive size.
    ZeroEpochSize,
    /// At least one virtual lane is required.
    ZeroWorkerLanes,
    /// The manifest exceeds its configured epoch limit.
    TooManyEpochs { found: usize, max: usize },
    /// An epoch contains more iterations than the public cap.
    EpochTooLarge {
        epoch: EpochId,
        found: usize,
        max: usize,
    },
    /// The manifest exceeds its configured iteration limit.
    TooManyIterations { found: usize, max: usize },
    /// The manifest exceeds a per-iteration dependency limit.
    TooManyDependencies {
        iteration: IterationId,
        found: usize,
        max: usize,
    },
    /// The manifest exceeds a per-iteration claim limit.
    TooManyClaims {
        iteration: IterationId,
        found: usize,
        max: usize,
    },
    /// Epoch ordinals must be contiguous and zero-based.
    NonCanonicalEpoch { expected: u64, found: u64 },
    /// Iteration ordinals must be contiguous and zero-based.
    NonCanonicalIteration { expected: u64, found: u64 },
    /// Dependencies must be sorted and unique.
    NonCanonicalDependencies { iteration: IterationId },
    /// Claims must be sorted and unique after canonicalization.
    NonCanonicalClaims { iteration: IterationId },
    /// A dependency cannot refer to this or a later iteration.
    ForwardDependency {
        iteration: IterationId,
        dependency: IterationId,
    },
    /// A worker lane is outside the manifest's public lane count.
    WorkerLaneOutOfRange {
        iteration: IterationId,
        lane: WorkerLane,
    },
    /// A public address range has length zero.
    EmptyAddressRange,
    /// A public address range overflows `u64`.
    AddressRangeOverflow { start: u64, len: u64 },
    /// An ordinal reservation overflows `u64`.
    OrdinalRangeOverflow { start: u64, len: u64 },
    /// Two reservations in one ordinal domain overlap.
    OverlappingOrdinalRanges {
        first: OrdinalRange,
        second: OrdinalRange,
    },
    /// A claim attempts to write a declared read-only storage namespace.
    ReadOnlyViolation { key: StorageKey },
    /// An ordinal counter overflowed while validating the manifest.
    OrdinalOverflow,
    /// A length could not be represented by the canonical `u64` encoding.
    LengthOverflow,
}

impl fmt::Display for ManifestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedVersion { found } => {
                write!(f, "unsupported schedule format version {found}")
            }
            Self::ZeroEpochSize => f.write_str("schedule epoch size must be non-zero"),
            Self::ZeroWorkerLanes => f.write_str("schedule worker-lane count must be non-zero"),
            Self::TooManyEpochs { found, max } => {
                write!(f, "schedule has {found} epochs, maximum is {max}")
            }
            Self::EpochTooLarge { epoch, found, max } => {
                write!(
                    f,
                    "epoch {epoch:?} has {found} iterations, maximum is {max}"
                )
            }
            Self::TooManyIterations { found, max } => {
                write!(f, "schedule has {found} iterations, maximum is {max}")
            }
            Self::TooManyDependencies {
                iteration,
                found,
                max,
            } => write!(
                f,
                "iteration {iteration:?} has {found} dependencies, maximum is {max}"
            ),
            Self::TooManyClaims {
                iteration,
                found,
                max,
            } => write!(
                f,
                "iteration {iteration:?} has {found} storage claims, maximum is {max}"
            ),
            Self::NonCanonicalEpoch { expected, found } => {
                write!(f, "expected epoch {expected}, found {found}")
            }
            Self::NonCanonicalIteration { expected, found } => {
                write!(f, "expected iteration {expected}, found {found}")
            }
            Self::NonCanonicalDependencies { iteration } => {
                write!(f, "iteration {iteration:?} dependencies are not canonical")
            }
            Self::NonCanonicalClaims { iteration } => {
                write!(f, "iteration {iteration:?} claims are not canonical")
            }
            Self::ForwardDependency {
                iteration,
                dependency,
            } => write!(
                f,
                "iteration {iteration:?} depends forward on {dependency:?}"
            ),
            Self::WorkerLaneOutOfRange { iteration, lane } => {
                write!(
                    f,
                    "iteration {iteration:?} uses out-of-range worker lane {lane:?}"
                )
            }
            Self::EmptyAddressRange => f.write_str("public address range must be non-empty"),
            Self::AddressRangeOverflow { start, len } => {
                write!(f, "public address range {start}+{len} overflows")
            }
            Self::OrdinalRangeOverflow { start, len } => {
                write!(f, "ordinal range {start}+{len} overflows")
            }
            Self::OverlappingOrdinalRanges { first, second } => {
                write!(f, "ordinal ranges {first:?} and {second:?} overlap")
            }
            Self::ReadOnlyViolation { key } => {
                write!(f, "write claim targets read-only storage {key:?}")
            }
            Self::OrdinalOverflow => f.write_str("schedule ordinal overflow"),
            Self::LengthOverflow => f.write_str("schedule length exceeds u64"),
        }
    }
}

fn is_sorted_unique<T: Ord>(values: &[T]) -> bool {
    values.windows(2).all(|pair| pair[0] < pair[1])
}

#[derive(Clone, Copy)]
enum RangeKind {
    Correlation,
    Communication,
}

fn add_range(
    ranges: &mut Vec<OrdinalRange>,
    range: OrdinalRange,
    _kind: RangeKind,
    _iteration: IterationId,
) -> Result<(), ManifestError> {
    range.end()?;
    for previous in ranges.iter().copied() {
        if range.overlaps(previous)? {
            return Err(ManifestError::OverlappingOrdinalRanges {
                first: previous,
                second: range,
            });
        }
    }
    if range.len != 0 {
        ranges.push(range);
    }
    Ok(())
}

fn push_u16(out: &mut Vec<u8>, value: u16) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn push_len(out: &mut Vec<u8>, value: usize) -> Result<(), ManifestError> {
    let value = u64::try_from(value).map_err(|_| ManifestError::LengthOverflow)?;
    push_u64(out, value);
    Ok(())
}

fn push_range(out: &mut Vec<u8>, range: OrdinalRange) {
    push_u64(out, range.start);
    push_u64(out, range.len);
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;

    const LIMITS: ScheduleLimits = ScheduleLimits {
        max_epochs: 8,
        max_iterations: 32,
        max_dependencies: 8,
        max_claims: 8,
    };

    fn key(storage: u32, type_id: u32) -> StorageKey {
        StorageKey::new(StorageId(storage), TypeId(type_id))
    }

    fn manifest() -> ScheduleManifest {
        let mut manifest = ScheduleManifest::new([1; 32], [2; 32], 7, 4, 2);
        let mut epoch = EpochManifest::new(EpochId(0), CommunicationGroup(3));
        let mut first = IterationManifest::new(IterationId(0), 100, WorkerLane(0));
        first
            .claims
            .push(StorageClaim::whole(key(4, 1), StorageClaimMode::Read));
        let mut second = IterationManifest::new(IterationId(1), 101, WorkerLane(1));
        second.dependencies.push(IterationId(0));
        second
            .claims
            .push(StorageClaim::whole(key(8, 1), StorageClaimMode::Write));
        epoch.iterations = vec![first, second];
        manifest.epochs.push(epoch);
        manifest
    }

    #[test]
    fn absent_storage_is_exclusive_and_read_only_is_shared() {
        let table = StorageTable::new();
        assert_eq!(
            classify_storage_claim(key(1, 2), false, &table)
                .unwrap()
                .mode,
            StorageClaimMode::Unknown
        );

        let mut table = StorageTable::new();
        table.set(StorageId(1), StorageAccess::ReadOnly);
        assert_eq!(
            classify_storage_claim(key(1, 2), false, &table)
                .unwrap()
                .mode,
            StorageClaimMode::Read
        );
        assert!(matches!(
            classify_storage_claim(key(1, 2), true, &table),
            Err(ManifestError::ReadOnlyViolation { .. })
        ));
    }

    #[test]
    fn read_claims_share_but_unknown_claims_conflict() {
        let read_a = StorageClaim::whole(key(1, 2), StorageClaimMode::Read);
        let read_b = StorageClaim::whole(key(1, 2), StorageClaimMode::Read);
        let unknown = StorageClaim::whole(key(1, 2), StorageClaimMode::Unknown);
        assert!(!read_a.conflicts_with(read_b));
        assert!(read_a.conflicts_with(unknown));
        assert!(unknown.conflicts_with(unknown));
        assert!(!read_a.conflicts_with(StorageClaim::whole(key(1, 3), StorageClaimMode::Write,)));
    }

    #[test]
    fn public_ranges_only_conflict_when_they_overlap() {
        let left = StorageClaim {
            key: key(1, 2),
            mode: StorageClaimMode::Write,
            scope: AddressScope::PublicRange { start: 0, len: 4 },
        };
        let right = StorageClaim {
            key: key(1, 2),
            mode: StorageClaimMode::Write,
            scope: AddressScope::PublicRange { start: 4, len: 4 },
        };
        assert!(!left.conflicts_with(right));
        assert!(left.conflicts_with(StorageClaim {
            scope: AddressScope::PublicRange { start: 3, len: 2 },
            ..right
        }));
    }

    #[test]
    fn canonicalization_sorts_and_joins_duplicate_claims() {
        let mut iteration = IterationManifest::new(IterationId(0), 0, WorkerLane(0));
        iteration
            .dependencies
            .extend([IterationId(2), IterationId(1), IterationId(1)]);
        iteration.claims.extend([
            StorageClaim::whole(key(5, 0), StorageClaimMode::Read),
            StorageClaim::whole(key(2, 0), StorageClaimMode::Read),
            StorageClaim::whole(key(5, 0), StorageClaimMode::Write),
        ]);
        iteration.canonicalize();
        assert_eq!(iteration.dependencies, vec![IterationId(1), IterationId(2)]);
        assert_eq!(iteration.claims.len(), 2);
        assert_eq!(iteration.claims[0].key, key(2, 0));
        assert_eq!(iteration.claims[1].mode, StorageClaimMode::Write);
    }

    #[test]
    fn manifest_canonical_bytes_are_stable_after_canonicalization() {
        let mut first = manifest();
        first.canonicalize();
        let first_bytes = first.canonical_bytes(LIMITS).unwrap();

        let mut second = manifest();
        second.epochs[0].iterations[0].claims.reverse();
        second.canonicalize();
        assert_eq!(first_bytes, second.canonical_bytes(LIMITS).unwrap());
    }

    #[test]
    fn validation_rejects_forward_dependencies_and_overlapping_ranges() {
        let mut forward = manifest();
        forward.epochs[0].iterations[0]
            .dependencies
            .push(IterationId(1));
        forward.canonicalize();
        assert!(matches!(
            forward.validate(LIMITS),
            Err(ManifestError::ForwardDependency { .. })
        ));

        let mut overlap = manifest();
        overlap.epochs[0].iterations[0].correlation_range = OrdinalRange::new(0, 2);
        overlap.epochs[0].iterations[1].correlation_range = OrdinalRange::new(1, 2);
        assert!(matches!(
            overlap.validate(LIMITS),
            Err(ManifestError::OverlappingOrdinalRanges { .. })
        ));
    }

    #[test]
    fn validation_requires_contiguous_epoch_and_iteration_ordinals() {
        let mut bad_epoch = manifest();
        bad_epoch.epochs[0].ordinal = EpochId(1);
        assert!(matches!(
            bad_epoch.validate(LIMITS),
            Err(ManifestError::NonCanonicalEpoch { .. })
        ));

        let mut bad_iteration = manifest();
        bad_iteration.epochs[0].iterations[1].ordinal = IterationId(3);
        assert!(matches!(
            bad_iteration.validate(LIMITS),
            Err(ManifestError::NonCanonicalIteration { .. })
        ));
    }

    #[test]
    fn digest_adapter_receives_domain_separated_canonical_bytes() {
        struct TestHasher;
        impl ScheduleHasher for TestHasher {
            fn digest(&self, bytes: &[u8]) -> [u8; 32] {
                let mut result = [0; 32];
                for (index, byte) in bytes.iter().copied().enumerate() {
                    result[index % result.len()] ^= byte;
                }
                result
            }
        }

        let manifest = manifest();
        let digest = manifest.digest_with(LIMITS, &TestHasher).unwrap();
        assert_ne!(digest, [0; 32]);
        assert!(
            manifest
                .canonical_bytes(LIMITS)
                .unwrap()
                .starts_with(b"volar.schedule\0")
        );
    }
}
