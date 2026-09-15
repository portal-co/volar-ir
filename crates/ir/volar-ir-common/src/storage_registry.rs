// @reliability: normal
// @ai: assisted
//! Runtime [`StorageId`] registry: purpose-tagged, uniqueness-enforcing
//! allocation of storage spaces, replacing the fixed numeric ranges
//! (`MEMORY_BASE`, `VIRT_REGISTERS_BASE`, `GLOBAL_STORAGE_BASE`, …) that
//! previously partitioned the flat `u32` space by convention alone.
//!
//! # What this is (and is not)
//!
//! The registry is a **producer-side coordination layer**, not a runtime
//! indirection and not an IR format change. Statements keep
//! `storage: StorageId` (flat `u32`); evaluators, store-forwarding, the
//! text/rkyv formats, region anchors, and backends are all untouched.
//! What changes is *who decides the number*: consumers ask the registry
//! instead of hard-coding a constant or scanning a module for its maximum
//! in-use ID.
//!
//! Consequences:
//!
//! - **Allocation, never validity.** The registry governs which IDs a
//!   *producer* picks; it is never consulted to decide whether a storage
//!   access in existing IR is meaningful. Hand-built modules and fuzz
//!   generators that use unregistered IDs (e.g. `StorageId(a % 4)`) remain
//!   perfectly legal. Do **not** add "storage must be registered" checks
//!   to any semantics-bearing pass.
//! - **Strictly ephemeral.** No rkyv/serde representation, no text-format
//!   section, no schema entry. Two pipelines exchanging IR exchange flat
//!   IDs; the receiver adopts them via [`StorageRegistry::claim_all_in_use`]
//!   and resolves collisions by renaming (as `volar-ir-opt`'s substitution
//!   already does).
//! - **One registry per module.** Each module being built or lowered has
//!   exactly one registry governing its storage namespace.
//! - **Dense, small IDs.** [`StorageRegistry::register`] hands out the
//!   smallest free ID. Consumers that pack storage IDs into bit-limited
//!   encodings (e.g. `volar-llvm-vaffle-import`'s pointer-value global
//!   tags) rely on this contract.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use crate::StorageId;

/// A contiguous block of `len` [`StorageId`]s allocated together.
///
/// Covers the legitimate "base + k" access patterns (per-bit lanes,
/// per-type register files): arithmetic on IDs is fine when the *whole
/// block* was reserved up front.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct StorageBlock {
    /// First [`StorageId`] of the block.
    pub base: StorageId,
    /// Number of consecutive IDs reserved, starting at `base`.
    pub len: u32,
}

impl StorageBlock {
    /// The `k`-th ID in the block: `base + k`.
    ///
    /// Debug-asserts `k < len`; out-of-range access is a logic error in
    /// the consumer, not a registry concern.
    pub fn at(&self, k: u32) -> StorageId {
        debug_assert!(k < self.len, "StorageBlock::at({k}) out of range (len {})", self.len);
        StorageId(self.base.0 + k)
    }

    /// Iterate every ID in the block.
    pub fn iter(&self) -> impl Iterator<Item = StorageId> {
        (0..self.len).map(|k| StorageId(self.base.0 + k))
    }
}

/// Why a [`StorageRegistry::claim`] (or batch claim) failed.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct StorageClaimError {
    /// The ID that was already taken.
    pub id: StorageId,
}

impl core::fmt::Display for StorageClaimError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "storage id {} is already registered", self.id.0)
    }
}

/// Generic storage-space allocator + ownership record.
///
/// `P` is the consumer's purpose vocabulary: this repo uses
/// [`StoragePurpose`]; downstream consumers define their own `P` (the
/// registry treats it as opaque metadata) rather than extending that enum.
///
/// The map *is* the uniqueness guarantee: every ID ever handed out by
/// [`register`](StorageRegistry::register) or claimed by
/// [`claim`](StorageRegistry::claim) is present as a key.
#[derive(Clone, Debug, Default)]
pub struct StorageRegistry<P> {
    /// Dense bump cursor for `register()`: the smallest candidate not yet
    /// known to be taken. Claims below the cursor are skipped on the next
    /// `register()`; claims above it leave density unaffected.
    next: u32,
    /// Every ID handed out or claimed → its purpose.
    by_id: BTreeMap<StorageId, P>,
}

impl<P> StorageRegistry<P> {
    /// Empty registry; `register()` starts probing from ID 0, skipping
    /// claimed IDs.
    pub fn new() -> Self {
        StorageRegistry {
            next: 0,
            by_id: BTreeMap::new(),
        }
    }

    /// Allocate the smallest never-before-issued ID for `purpose`.
    pub fn register(&mut self, purpose: P) -> StorageId {
        let mut candidate = self.next;
        while self.by_id.contains_key(&StorageId(candidate)) {
            candidate += 1;
        }
        let id = StorageId(candidate);
        self.by_id.insert(id, purpose);
        self.next = candidate + 1;
        id
    }

    /// Claim a *specific* ID (external indexing convention, or adopting
    /// pre-existing IR). Fails on collision instead of stomping.
    pub fn claim(&mut self, id: StorageId, purpose: P) -> Result<(), StorageClaimError> {
        if self.by_id.contains_key(&id) {
            return Err(StorageClaimError { id });
        }
        self.by_id.insert(id, purpose);
        Ok(())
    }

    /// Claim every ID in `ids` (e.g. a statement/pre-init walk over
    /// foreign IR being adopted into this module). Atomic: if any ID
    /// collides, nothing is claimed and the first colliding ID is
    /// reported.
    pub fn claim_all_in_use(
        &mut self,
        ids: impl IntoIterator<Item = StorageId>,
        purpose: impl Fn(StorageId) -> P,
    ) -> Result<(), StorageClaimError>
    where
        P: Clone,
    {
        let mut dedup: Vec<StorageId> = Vec::new();
        for id in ids {
            if !dedup.contains(&id) {
                dedup.push(id);
            }
        }
        for &id in &dedup {
            if self.by_id.contains_key(&id) {
                return Err(StorageClaimError { id });
            }
        }
        for id in dedup {
            self.by_id.insert(id, purpose(id));
        }
        Ok(())
    }

    /// Look up the purpose recorded for `id`, if any.
    pub fn purpose_of(&self, id: StorageId) -> Option<&P> {
        self.by_id.get(&id)
    }

    /// Iterate every registered `(StorageId, purpose)` pair, in ID order.
    pub fn iter(&self) -> impl Iterator<Item = (StorageId, &P)> {
        self.by_id.iter().map(|(&id, p)| (id, p))
    }

    /// Number of registered IDs (including claimed ones).
    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    /// True when no IDs are registered.
    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    /// Advance the bump cursor past `id` if it currently sits inside the
    /// granted range, so later `register()` calls stay dense and free.
    fn advance_cursor_past(&mut self, granted_base: u32, granted_len: u32) {
        if self.next >= granted_base && self.next < granted_base + granted_len {
            self.next = granted_base + granted_len;
        }
    }
}

impl<P: Clone> StorageRegistry<P> {
    /// Allocate a contiguous block of `n` IDs sharing one purpose.
    ///
    /// The smallest base such that `[base, base + n)` is entirely free is
    /// chosen. Returns the empty block (`len == 0`, base at the cursor)
    /// for `n == 0`.
    pub fn register_block(&mut self, purpose: P, n: u32) -> StorageBlock {
        if n == 0 {
            return StorageBlock {
                base: StorageId(self.next),
                len: 0,
            };
        }
        let mut base = 0u32;
        'probe: loop {
            for k in 0..n {
                if self.by_id.contains_key(&StorageId(base + k)) {
                    base = base + k + 1;
                    continue 'probe;
                }
            }
            break;
        }
        for k in 0..n {
            self.by_id.insert(StorageId(base + k), purpose.clone());
        }
        self.advance_cursor_past(base, n);
        StorageBlock {
            base: StorageId(base),
            len: n,
        }
    }
}

/// Role a `volar-ir-virt`-allocated storage plays, for
/// [`StoragePurpose::Virt`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum VirtStorageRole {
    /// Bytecode table (one storage per PC bit or per bytecode word).
    BytecodeTable,
    /// Per-IR-type register file for block params / return values.
    RegisterFile,
    /// Per-handler slot storage (dispatch schema values).
    HandlerSlot,
    /// Per-PC bytecode commitment (committed virtualization).
    Commitment,
}

/// In-repo purpose vocabulary for [`StorageRegistry`].
///
/// Extensible within the repo; the registry itself treats `P` as opaque
/// metadata, so downstream consumers use `StorageRegistry<TheirPurpose>`
/// instead of extending this enum.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StoragePurpose {
    /// The default / "main" space (legacy `StorageId::DEFAULT`).
    Default,
    /// Call-stack frame space (legacy `StorageId::STACK`); see
    /// `vaffle::StackFrameConvention`.
    Stack,
    /// Frontend alloca marker space (legacy `StorageId::ALLOCA`); see
    /// `vaffle::StackFrameConvention`.
    AllocaMarker,
    /// WASM linear memory `index` (external indexing convention).
    WasmMemory {
        /// WASM memory index this space corresponds to.
        index: u32,
    },
    /// One LLVM-imported global.
    LlvmGlobal {
        /// Global's name (or synthesized label) for diagnostics/naming.
        name: String,
    },
    /// `volar-ir-virt` bytecode/register-file/slot/commitment storage.
    Virt {
        /// Which role this space plays in the virtualized layout.
        role: VirtStorageRole,
        /// Role-specific detail (type index, handler index, …).
        detail: u32,
    },
    /// `vaffle_ssa` cross-block value spill scratch space.
    VaffleSsaSpill,
    /// Substitution-remapped guest space; records the guest's original ID.
    Remapped {
        /// The guest module's original `StorageId` before remapping.
        from: StorageId,
    },
    /// Anything else; free-form label for diagnostics/naming.
    Other(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;
    use alloc::vec::Vec;

    /// Downstream-style custom purpose vocabulary: the registry is generic
    /// over `P` and must work with a type it knows nothing about.
    #[derive(Clone, Debug, PartialEq)]
    enum DownstreamPurpose {
        Witness,
        Challenge(u8),
    }

    #[test]
    fn register_hands_out_unique_dense_ids() {
        let mut reg = StorageRegistry::<StoragePurpose>::new();
        let a = reg.register(StoragePurpose::Default);
        let b = reg.register(StoragePurpose::Stack);
        let c = reg.register(StoragePurpose::Other("scratch".to_string()));
        assert_eq!((a, b, c), (StorageId(0), StorageId(1), StorageId(2)));
        assert!(reg.purpose_of(a).is_some());
        assert!(reg.purpose_of(StorageId(3)).is_none());
        assert_eq!(reg.len(), 3);
    }

    #[test]
    fn register_stays_dense_and_unique_with_custom_purpose_type() {
        let mut reg = StorageRegistry::<DownstreamPurpose>::new();
        let a = reg.register(DownstreamPurpose::Witness);
        let b = reg.register(DownstreamPurpose::Challenge(7));
        assert_eq!(a, StorageId(0));
        assert_eq!(b, StorageId(1));
        assert_eq!(reg.purpose_of(b), Some(&DownstreamPurpose::Challenge(7)));
    }

    #[test]
    fn claim_fails_on_collision_without_stomping() {
        let mut reg = StorageRegistry::<StoragePurpose>::new();
        let a = reg.register(StoragePurpose::Default);
        let err = reg.claim(a, StoragePurpose::Stack).unwrap_err();
        assert_eq!(err, StorageClaimError { id: a });
        // Original purpose intact after the failed claim.
        assert_eq!(reg.purpose_of(a), Some(&StoragePurpose::Default));
        // Claiming a *free* specific ID succeeds and is skipped by register.
        reg.claim(StorageId(9), StoragePurpose::VaffleSsaSpill)
            .unwrap();
        let b = reg.register(StoragePurpose::Other("x".to_string()));
        assert_eq!(b, StorageId(1)); // dense: 0 taken, 1 free, 9 claimed high
    }

    #[test]
    fn register_skips_low_claims_for_density() {
        let mut reg = StorageRegistry::<StoragePurpose>::new();
        reg.claim(StorageId(0), StoragePurpose::Stack).unwrap();
        reg.claim(StorageId(2), StoragePurpose::AllocaMarker)
            .unwrap();
        let a = reg.register(StoragePurpose::Default);
        assert_eq!(a, StorageId(1));
        let b = reg.register(StoragePurpose::WasmMemory { index: 0 });
        assert_eq!(b, StorageId(3));
    }

    #[test]
    fn claim_all_in_use_is_atomic() {
        let mut reg = StorageRegistry::<StoragePurpose>::new();
        let existing = reg.register(StoragePurpose::Default);
        let err = reg
            .claim_all_in_use(
                [StorageId(5), existing, StorageId(6)],
                StoragePurpose::default_for_test,
            )
            .unwrap_err();
        assert_eq!(err, StorageClaimError { id: existing });
        // Nothing from the failed batch was claimed.
        assert!(reg.purpose_of(StorageId(5)).is_none());
        assert!(reg.purpose_of(StorageId(6)).is_none());
        // A clean batch (with duplicates) claims each ID once.
        reg.claim_all_in_use(
            [StorageId(5), StorageId(5), StorageId(6)],
            StoragePurpose::default_for_test,
        )
        .unwrap();
        assert_eq!(reg.len(), 3);
    }

    #[test]
    fn register_block_reserves_a_contiguous_free_range() {
        let mut reg = StorageRegistry::<StoragePurpose>::new();
        reg.claim(StorageId(0), StoragePurpose::Stack).unwrap();
        let blk = reg.register_block(StoragePurpose::Other("lanes".to_string()), 3);
        assert_eq!(blk.base, StorageId(1));
        assert_eq!(blk.len, 3);
        assert_eq!(blk.at(0), StorageId(1));
        assert_eq!(blk.at(2), StorageId(3));
        assert_eq!(blk.iter().collect::<Vec<_>>(), alloc::vec![
            StorageId(1),
            StorageId(2),
            StorageId(3)
        ]);
        // A second block starts right after the first; the cursor skipped it.
        let blk2 = reg.register_block(StoragePurpose::Other("more".to_string()), 2);
        assert_eq!(blk2.base, StorageId(4));
        // Singles stay dense after block allocations.
        let single = reg.register(StoragePurpose::Default);
        assert_eq!(single, StorageId(6));
        // Zero-length block reserves nothing.
        let empty = reg.register_block(StoragePurpose::Default, 0);
        assert_eq!(empty.len, 0);
        assert_eq!(reg.len(), 1 + 3 + 2 + 1);
    }

    #[test]
    fn register_block_hops_over_fragmented_claims() {
        let mut reg = StorageRegistry::<StoragePurpose>::new();
        reg.claim(StorageId(1), StoragePurpose::Stack).unwrap();
        let blk = reg.register_block(StoragePurpose::Default, 3);
        // [0..3) is blocked by the claim at 1; [2..5) is free... except 1
        // is already taken, so the probe lands at 2.
        assert_eq!(blk.base, StorageId(2));
    }

    #[test]
    fn clone_is_independent() {
        let mut reg = StorageRegistry::<StoragePurpose>::new();
        let a = reg.register(StoragePurpose::Default);
        let mut fork = reg.clone();
        let b_fork = fork.register(StoragePurpose::Stack);
        let b_orig = reg.register(StoragePurpose::AllocaMarker);
        // Both forks got the same next ID, with their own purposes.
        assert_eq!(b_fork, b_orig);
        assert_eq!(reg.purpose_of(b_orig), Some(&StoragePurpose::AllocaMarker));
        assert_eq!(fork.purpose_of(b_fork), Some(&StoragePurpose::Stack));
        assert!(a == StorageId(0));
    }
}

#[cfg(test)]
impl StoragePurpose {
    /// Shared claim-purpose factory for `claim_all_in_use` tests.
    fn default_for_test(_id: StorageId) -> Self {
        StoragePurpose::Other(alloc::string::String::new())
    }
}
