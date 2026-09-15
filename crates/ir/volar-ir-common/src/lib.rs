#![no_std]
// @reliability: normal
// @ai: assisted

extern crate alloc;

pub mod complexity;
pub use complexity::{MeasureSpec, ReentryHint, StructRef};

mod generated;
pub use generated::{
    ActionDecl, Constant, Node, OracleDecl, PreInitSegment, RngDecl, StorageId, Type, TypeId,
};

pub mod storage_registry;
pub use storage_registry::{
    StorageBlock, StorageClaimError, StoragePurpose, StorageRegistry, VirtStorageRole,
};

use alloc::vec::Vec;

/// Canonical sparse coefficient collection for [`Stmt::Poly`].
///
/// Monomials are held in lexicographic key order with at most one entry per
/// key, matching the observable semantics of the old `BTreeMap<Vec<V>, u8>`
/// representation. A flat allocation is deliberate: owned transformations
/// can rewrite every variable in place and retain both the outer collection
/// and its monomial buffers instead of allocating a second B-tree.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct PolyCoeffs<V>(Vec<(Vec<V>, u8)>);

impl<V> Default for PolyCoeffs<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V> PolyCoeffs<V> {
    /// Create an empty coefficient collection.
    pub const fn new() -> Self {
        Self(Vec::new())
    }

    /// Number of monomial entries.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether this polynomial has no non-constant monomials.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Remove every monomial.
    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Iterate over `(monomial, coefficient)` pairs in canonical key order.
    pub fn iter(&self) -> impl Iterator<Item = (&Vec<V>, &u8)> {
        self.0.iter().map(|(monomial, coeff)| (monomial, coeff))
    }

    /// Iterate over monomial keys in canonical order.
    pub fn keys(&self) -> impl Iterator<Item = &Vec<V>> {
        self.0.iter().map(|(monomial, _)| monomial)
    }

    /// Iterate over coefficients in canonical key order.
    pub fn values(&self) -> impl Iterator<Item = &u8> {
        self.0.iter().map(|(_, coeff)| coeff)
    }

    /// Retain monomials for which `f` returns `true`.
    pub fn retain(&mut self, mut f: impl FnMut(&Vec<V>, &mut u8) -> bool) {
        self.0.retain_mut(|(monomial, coeff)| f(monomial, coeff));
    }
}

impl<V: Ord> PolyCoeffs<V> {
    fn key_index(&self, key: &[V]) -> Result<usize, usize> {
        self.0
            .binary_search_by(|(monomial, _)| monomial.as_slice().cmp(key))
    }

    /// Return the coefficient for `key`, if present.
    pub fn get(&self, key: &[V]) -> Option<&u8> {
        self.key_index(key).ok().map(|index| &self.0[index].1)
    }

    /// Insert or replace a monomial coefficient, returning the prior value.
    pub fn insert(&mut self, key: Vec<V>, coeff: u8) -> Option<u8> {
        match self.key_index(&key) {
            Ok(index) => Some(core::mem::replace(&mut self.0[index].1, coeff)),
            Err(index) => {
                self.0.insert(index, (key, coeff));
                None
            }
        }
    }

    /// Return a map-style entry for `key`.
    pub fn entry(&mut self, key: Vec<V>) -> PolyCoeffsEntry<'_, V> {
        match self.key_index(&key) {
            Ok(index) => PolyCoeffsEntry::Occupied(&mut self.0[index].1),
            Err(index) => PolyCoeffsEntry::Vacant {
                entries: &mut self.0,
                index,
                key,
            },
        }
    }

    /// Remove a monomial coefficient, returning it if present.
    pub fn remove(&mut self, key: &[V]) -> Option<u8> {
        self.key_index(key).ok().map(|index| self.0.remove(index).1)
    }

    /// Rewrite monomial vectors in place and restore canonical key order only
    /// when the rewrite actually changed it. This is the owned movfuscation
    /// fast path: monotonic substitutions retain the existing vector order and
    /// allocate no replacement coefficient collection.
    pub fn remap_monomials_in_place(&mut self, mut f: impl FnMut(&mut Vec<V>)) {
        for (monomial, _) in &mut self.0 {
            f(monomial);
        }
        if self
            .0
            .windows(2)
            .any(|pair| pair[0].0.as_slice() >= pair[1].0.as_slice())
        {
            self.0.sort_by(|a, b| a.0.cmp(&b.0));
            let mut write = 0;
            for read in 0..self.0.len() {
                if write > 0 && self.0[write - 1].0 == self.0[read].0 {
                    self.0[write - 1].1 = self.0[read].1;
                } else {
                    if write != read {
                        self.0.swap(write, read);
                    }
                    write += 1;
                }
            }
            self.0.truncate(write);
        }
    }

    /// Rewrite monomial vectors in place when `f` preserves the canonical
    /// lexicographic order of every key. This avoids the post-remap adjacent
    /// key walk in [`Self::remap_monomials_in_place`].
    ///
    /// Callers must ensure that `f` also cannot make two distinct monomial
    /// keys equal. A strictly monotonic variable substitution has both
    /// properties.
    pub fn remap_monomials_in_place_preserving_key_order(
        &mut self,
        mut f: impl FnMut(&mut Vec<V>),
    ) {
        for (monomial, _) in &mut self.0 {
            f(monomial);
        }
    }
}

/// A mutable entry returned by [`PolyCoeffs::entry`].
pub enum PolyCoeffsEntry<'a, V> {
    /// An existing coefficient.
    Occupied(&'a mut u8),
    /// A key position that has not been allocated yet.
    Vacant {
        entries: &'a mut Vec<(Vec<V>, u8)>,
        index: usize,
        key: Vec<V>,
    },
}

impl<'a, V> PolyCoeffsEntry<'a, V> {
    /// Return the existing coefficient or insert `default` at the canonical
    /// key position and return it.
    pub fn or_insert(self, default: u8) -> &'a mut u8 {
        match self {
            Self::Occupied(coeff) => coeff,
            Self::Vacant {
                entries,
                index,
                key,
            } => {
                entries.insert(index, (key, default));
                &mut entries[index].1
            }
        }
    }
}

impl<V> IntoIterator for PolyCoeffs<V> {
    type Item = (Vec<V>, u8);
    type IntoIter = alloc::vec::IntoIter<(Vec<V>, u8)>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

impl<'a, V> IntoIterator for &'a PolyCoeffs<V> {
    type Item = (&'a Vec<V>, &'a u8);
    type IntoIter = core::iter::Map<
        core::slice::Iter<'a, (Vec<V>, u8)>,
        fn(&(Vec<V>, u8)) -> (&Vec<V>, &u8),
    >;

    fn into_iter(self) -> Self::IntoIter {
        fn as_pair<V>(entry: &(Vec<V>, u8)) -> (&Vec<V>, &u8) {
            (&entry.0, &entry.1)
        }
        self.0.iter().map(as_pair)
    }
}

impl<V: Ord> core::iter::FromIterator<(Vec<V>, u8)> for PolyCoeffs<V> {
    fn from_iter<T: IntoIterator<Item = (Vec<V>, u8)>>(iter: T) -> Self {
        let mut entries: Vec<(Vec<V>, u8)> = iter.into_iter().collect();
        entries.sort_by(|a, b| a.0.cmp(&b.0));
        let mut write = 0;
        for read in 0..entries.len() {
            if write > 0 && entries[write - 1].0 == entries[read].0 {
                entries[write - 1].1 = entries[read].1;
            } else {
                if write != read {
                    entries.swap(write, read);
                }
                write += 1;
            }
        }
        entries.truncate(write);
        Self(entries)
    }
}

#[cfg(test)]
mod poly_coeffs_tests {
    use alloc::{vec, vec::Vec};

    use super::PolyCoeffs;

    #[test]
    fn from_iter_canonicalizes_and_keeps_the_last_coefficient() {
        let coeffs = PolyCoeffs::from_iter([
            (vec![3], 3u8),
            (vec![1], 1u8),
            (vec![3], 7u8),
        ]);

        assert_eq!(
            coeffs.into_iter().collect::<Vec<_>>(),
            vec![(vec![1], 1), (vec![3], 7)]
        );
    }

    #[test]
    fn remap_reuses_monomials_and_normalizes_collisions() {
        let mut coeffs = PolyCoeffs::from_iter([(vec![0], 3u8), (vec![1], 7u8)]);
        let outer_ptr = coeffs.0.as_ptr();

        coeffs.remap_monomials_in_place(|monomial| {
            monomial[0] += 2;
        });
        assert_eq!(outer_ptr, coeffs.0.as_ptr());
        assert_eq!(
            coeffs.iter().map(|(key, value)| (key.clone(), *value)).collect::<Vec<_>>(),
            vec![(vec![2], 3), (vec![3], 7)]
        );

        coeffs.remap_monomials_in_place(|monomial| monomial[0] = 9);
        assert_eq!(
            coeffs.into_iter().collect::<Vec<_>>(),
            vec![(vec![9], 7)]
        );
    }

    #[test]
    fn ordered_remap_skips_normalization_for_strictly_monotonic_keys() {
        let mut coeffs = PolyCoeffs::from_iter([(vec![0], 3u8), (vec![1], 7u8)]);
        let outer_ptr = coeffs.0.as_ptr();

        coeffs.remap_monomials_in_place_preserving_key_order(|monomial| {
            monomial[0] += 2;
        });

        assert_eq!(outer_ptr, coeffs.0.as_ptr());
        assert_eq!(
            coeffs.into_iter().collect::<Vec<_>>(),
            vec![(vec![2], 3), (vec![3], 7)]
        );
    }
}

// ============================================================================
// Unified type system
// ============================================================================

/// The full IR type language, shared between Volar IR and VAFFLE.
///
/// Primitive scalars are wrapped in [`Primitive`](IrType::Primitive) so that
/// the [`Type`] enum remains the single source of truth for leaf types.
/// Compound types are recursive via [`TypeId`] references into a [`TypeTable`].
///
/// # Variants unique to VAFFLE
/// [`Func`](IrType::Func) represents a first-class function type (used for
/// imports, exports, and higher-order values in VAFFLE modules).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
#[non_exhaustive]
pub enum IrType {
    /// A primitive scalar type (bit, integer, or Galois-field element).
    Primitive(Type),
    /// A fixed-length homogeneous vector: `[element; len]`.
    Vec(usize, TypeId),
    /// A heterogeneous product type (tuple).
    Tuple(alloc::vec::Vec<TypeId>),
    /// A block / continuation type: a control-flow label that accepts the
    /// listed parameter types.  Used by Volar IR's `Block`-typed SSA params
    /// and dynamic jump targets.
    Block { params: alloc::vec::Vec<TypeId> },
    /// A function type: a callable with the given parameter and result types.
    /// Present in VAFFLE for import/export declarations and first-class
    /// function values.  Not used by Volar IR (which represents functions via
    /// `IRBlocks` rather than typed values).
    Func {
        params: alloc::vec::Vec<TypeId>,
        results: alloc::vec::Vec<TypeId>,
    },
}

/// An interning table for [`IrType`] values.
///
/// Both Volar IR and VAFFLE carry one of these (Volar IR as `IRTypes`, VAFFLE
/// as `Module::types`).  [`TypeId`]s are valid only within the table that
/// produced them.
///
/// # Deduplication
/// [`intern`](TypeTable::intern) does a linear scan for an existing entry
/// before pushing.  Type tables are typically small (tens of entries), so
/// this is acceptable; for large tables consider a separate index.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct TypeTable(pub alloc::vec::Vec<IrType>);

impl TypeTable {
    /// Create an empty table.
    pub fn new() -> Self {
        TypeTable(alloc::vec::Vec::new())
    }

    /// Push `ty` without deduplication and return its new [`TypeId`].
    pub fn push(&mut self, ty: IrType) -> TypeId {
        let id = TypeId(self.0.len() as u32);
        self.0.push(ty);
        id
    }

    /// Look for an existing entry equal to `ty`; if not found, push it.
    /// Returns the [`TypeId`] of the (found or newly inserted) entry.
    pub fn intern(&mut self, ty: IrType) -> TypeId {
        if let Some(pos) = self.0.iter().position(|t| t == &ty) {
            TypeId(pos as u32)
        } else {
            self.push(ty)
        }
    }

    /// Convenience: intern `IrType::Primitive(ty)`.
    pub fn primitive(&mut self, ty: Type) -> TypeId {
        self.intern(IrType::Primitive(ty))
    }

    /// Convenience: intern the `Bit` primitive type.
    pub fn bit(&mut self) -> TypeId {
        self.primitive(Type::Bit)
    }

    /// Return `true` if `id` resolves to `IrType::Primitive(Type::Bit)`.
    pub fn is_bit(&self, id: TypeId) -> bool {
        matches!(
            self.0.get(id.0 as usize),
            Some(IrType::Primitive(Type::Bit))
        )
    }

    /// Return `true` if `id` resolves to `IrType::Block { .. }`.
    pub fn is_block(&self, id: TypeId) -> bool {
        matches!(self.0.get(id.0 as usize), Some(IrType::Block { .. }))
    }
}

impl Default for TypeTable {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// External-primitive declarations (shared by Volar IR and VAFFLE)
// ============================================================================

/// The storage destination for one declared result of an [`ActionDecl`].
///
/// Actions are effects, not values: their declared results are written to
/// these destinations in declaration order.  A Boolar lowering expands each
/// typed destination into its lane and one bit-address per result bit.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct ActionTarget<Addr, Stor = StorageId> {
    /// Storage namespace receiving this result.
    pub storage: Stor,
    /// Element address within that namespace.
    pub addr: Addr,
}

// ============================================================================
// Shared statement type
// ============================================================================

impl StorageId {
    /// The default / "main" storage space.
    pub const DEFAULT: StorageId = StorageId(0);
    /// A dedicated call-stack storage space.
    pub const STACK: StorageId = StorageId(1);
    /// Storage space for the virtualisation pass bytecode table (see
    /// `volar-ir-virt`).  Chosen to be outside the WASM memory range so
    /// virtualised modules can still reference any `memory(i)`.
    pub const VIRT_BYTECODE: StorageId = StorageId(2);
    /// Base of the reserved range for the virtualisation pass
    /// *register file*.  Each IR type used as a block-param or
    /// terminator-arg type is assigned its own `StorageId` in the range
    /// `[VIRT_REGISTERS_BASE, VIRT_REGISTERS_BASE + n_types)`.  Register
    /// indices live at the address level within that storage.
    #[deprecated(
        note = "fixed-range storage allocation is superseded by StorageRegistry; use volar-ir-virt's *_with_registry entry points, which allocate the register file from a registry"
    )]
    pub const VIRT_REGISTERS_BASE: u32 = 3;
    /// Base ID for WASM linear memories.  Memory `i` uses `StorageId(MEMORY_BASE + i)`.
    pub const MEMORY_BASE: u32 = 16;
    /// Convenience: StorageId for WASM memory index `i`.
    pub const fn memory(i: u32) -> StorageId {
        StorageId(Self::MEMORY_BASE + i)
    }
    /// Dedicated scratch space for `vaffle_ssa`'s own cross-block value
    /// spilling (`crates/ir/volar-vaffle-target/src/vaffle_ssa.rs`).
    /// Addressed directly by the spilled VAFFLE `ValueId` itself (not
    /// SP-relative, no frame-layout coordination needed) — chosen well
    /// outside the WASM memory range (like [`VIRT_BYTECODE`]) so a module
    /// with any realistic number of declared memories can't collide with
    /// it.
    pub const VAFFLE_SSA_SPILL: StorageId = StorageId(1_000_000);
    /// Dedicated marker space for a frontend's `alloca` (e.g.
    /// `volar-llvm-vaffle-import`'s `Value::StackAlloc`/`PtrLoad`/
    /// `PtrStore`/`PtrOffset`).
    ///
    /// This is the numeric value of
    /// [`vaffle::StackFrameConvention::LEGACY`]'s `alloca_marker` — new
    /// code should thread the typed convention handle (or register fresh
    /// spaces in a `StorageRegistry`) rather than matching this constant
    /// numerically.
    ///
    /// Deliberately *not* [`STACK`]: `volar-vaffle-target/src/lower_to_ir.rs`
    /// rebases every `StorageRead`/`StorageWrite` tagged `ALLOCA` onto the
    /// enclosing function's real runtime frame (`sp_bits + local_offset`,
    /// re-tagged `STACK` in the lowered output — that's genuinely where the
    /// data ends up living) before emitting it. [`STACK`] itself carries no
    /// such contract — it is (and must stay) a plain, unrebased storage
    /// space free for the calling convention's own internal frame *and* for
    /// arbitrary hand-built or fuzzer-generated VAFFLE code that has no
    /// notion of "this address is relative to some frame" (confirmed by
    /// `volar-fuzz`'s own extended-block generator, which picks a random
    /// `StorageId` including `STACK`'s numeric value as just another id).
    /// Rebasing based on the numeric value of [`STACK`] instead of this
    /// dedicated id would silently corrupt any such unrelated access.
    pub const ALLOCA: StorageId = StorageId(1_000_001);
}

impl PreInitSegment {
    /// Value of element `i` as a `u8`.
    pub fn as_u8(&self, i: usize) -> u8 {
        self.data[i].lo as u8
    }
    /// Value of element `i` as a `u16`.
    pub fn as_u16(&self, i: usize) -> u16 {
        self.data[i].lo as u16
    }
    /// Value of element `i` as a `u32`.
    pub fn as_u32(&self, i: usize) -> u32 {
        self.data[i].lo as u32
    }
    /// Value of element `i` as a `u64`.
    pub fn as_u64(&self, i: usize) -> u64 {
        self.data[i].lo as u64
    }
    /// Value of element `i` as a `u128`.
    pub fn as_u128(&self, i: usize) -> u128 {
        self.data[i].lo
    }
    /// Full 256-bit `Constant` for element `i`.
    pub fn as_constant(&self, i: usize) -> Constant {
        self.data[i]
    }
    /// Absolute cell index for element `i`: `self.offset + i`.
    pub fn cell_index(&self, i: usize) -> usize {
        self.offset + i
    }
}

/// Shared computational statement type for Volar IR and VAFFLE.
///
/// Generic over four parameters:
///
/// | Parameter | Volar IR        | VAFFLE      | Default     |
/// |-----------|-----------------|-------------|-------------|
/// | `Var`     | `IRVarId`       | `ValueId`   | (required)  |
/// | `Addr`    | `IRVarId`       | `ValueId`   | `Var`       |
/// | `Ty`      | `TypeId`        | `TypeId`    | `TypeId`    |
/// | `Stor`    | `StorageId`     | `StorageId` | `StorageId` |
///
/// The `Ty` and `Stor` parameters allow transformations (e.g. type-table
/// remapping, storage relabelling) to be expressed as a single [`Stmt::map`]
/// call.  Existing usages `Stmt<IRVarId>` continue to compile unchanged.
///
/// # Invariants
/// * `Poly.coeffs` — monomial keys must be sorted (no duplicates within a key).
/// * `Shuffle.result_bits` — each `(bit_idx, var)` selects bit `bit_idx`
///   from `var`; together they define every bit of the output, LSB first.
///   Length equals the output bit-width.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
#[non_exhaustive]
pub enum Stmt<Var, Addr = Var, Ty = TypeId, Stor = StorageId> {
    /// Load a value from a storage location addressed by `addr`.
    StorageRead { storage: Stor, ty: Ty, addr: Addr },
    /// Write `src` to the storage location addressed by `addr`.
    StorageWrite {
        storage: Stor,
        src: Var,
        ty: Ty,
        addr: Addr,
    },
    /// A compile-time constant value of type `ty`.
    Const(Constant, Ty),
    /// Reinterpret `src` (of type `src_ty`) as `dst_ty` without changing bits.
    Transmute { src: Var, src_ty: Ty, dst_ty: Ty },
    /// Multivariate polynomial over variables.
    ///
    /// Each `(monomial, coeff)` contributes `coeff * product(monomial)` to
    /// the sum; `constant` is the degree-0 term.  Monomial keys must be
    /// sorted.
    ///
    /// # Type semantics
    /// * If `ty` resolves to `Bit`: all variables must be `Bit`; arithmetic
    ///   is GF(2) (mod 2 on every coefficient bit).
    /// * If `ty` resolves to a bitvector or field element `T`: at most one
    ///   variable across all monomials may have type `T` (the "non-Bit slot");
    ///   all other variables in that monomial must be `Bit` and act as GF(2)
    ///   selectors.  The constant term occupies the lowest `bits(T)` bits of
    ///   `constant`.  Mixing two distinct non-GF(2) field types is prohibited.
    Poly {
        /// Output (and dominant operand) type.
        ty: Ty,
        coeffs: PolyCoeffs<Var>,
        constant: Constant,
    },
    /// Rotate-left `src` (of type `ty`) by `n` bit positions.
    Rol { src: Var, ty: Ty, n: usize },
    /// Rotate-right `src` (of type `ty`) by `n` bit positions.
    Ror { src: Var, ty: Ty, n: usize },
    /// Concatenate `parts` in order (LSB-first) into a wider value of type `ty`.
    Merge { parts: Vec<Var>, ty: Ty },
    /// Broadcast a single-bit value across every bit position of type `ty`.
    Splat { src: Var, ty: Ty },
    /// Arbitrary bit shuffle: assemble an output from individually selected bits.
    ///
    /// `result_bits[i] = (bit_idx, var)` — bit `i` of the output is taken
    /// from bit `bit_idx` of `var`.  Length equals the output bit-width.
    Shuffle { result_bits: Vec<(u8, Var)>, ty: Ty },

    // ---- External access primitives ----------------------------------------
    /// Invoke a named pure oracle, producing a multi-output aggregate result.
    ///
    /// The result type is `IrType::Tuple(output_tys)`, pre-interned as
    /// `result_ty`.  Project individual outputs with [`OracleOutput`].
    ///
    /// **Ordering**: an `OracleCall` is pure and may be reordered or CSE'd
    /// freely.  It may only be DCE'd when every corresponding `OracleOutput`
    /// is also DCE'd.
    OracleCall {
        name: alloc::string::String,
        args: Vec<Var>,
        /// Return type of each output, in declaration order.  Non-empty.
        output_tys: Vec<Ty>,
        /// Pre-interned `TypeId` of `IrType::Tuple(output_tys)`.
        /// Stored at construction time so type inference never mutates the table.
        result_ty: Ty,
    },

    /// Project output `idx` from an [`OracleCall`] result var.
    ///
    /// `call` must be the SSA var produced by an `OracleCall` in the same block.
    /// `ty` must equal `oracle_call.output_tys[idx]`.
    OracleOutput { call: Var, idx: usize, ty: Ty },

    /// Conditionally invoke a named impure action, producing a multi-output aggregate result.
    ///
    /// The result type is `IrType::Tuple(output_tys)`, pre-interned as `result_ty`.
    /// Output `i` is `action(args)[i]` when `guard != 0`, `fallbacks[i]` otherwise.
    /// Project individual outputs with [`ActionOutput`].
    ///
    /// **Ordering**: an `ActionCall` has side effects and must not be
    /// reordered, CSE'd, or DCE'd.  All `ActionOutput` projections from it
    /// are kept alive as long as the call itself is live.
    ActionCall {
        name: alloc::string::String,
        guard: Var,
        args: Vec<Var>,
        /// Fallback vars — one per output (typed as `output_tys[i]`).  Used
        /// when `guard = 0` and the action is not invoked.
        fallbacks: Vec<Var>,
        /// Return type of each output, in declaration order.  Non-empty.
        output_tys: Vec<Ty>,
        /// Pre-interned `TypeId` of `IrType::Tuple(output_tys)`.
        result_ty: Ty,
    },

    /// Conditionally invoke an action and store every declared result directly.
    ///
    /// `targets` has exactly one entry per `output_tys` item.  If `guard` is
    /// false, the corresponding fallback is written instead.  This is the
    /// effect-only replacement for the legacy `ActionCall`/`ActionOutput`
    /// aggregate projection pair; it deliberately has no useful SSA result.
    ActionStore {
        name: alloc::string::String,
        guard: Var,
        args: Vec<Var>,
        fallbacks: Vec<Var>,
        output_tys: Vec<Ty>,
        targets: Vec<ActionTarget<Addr, Stor>>,
    },

    /// Project output `idx` from an [`ActionCall`] result var.
    ///
    /// `call` must be the SSA var produced by an `ActionCall` in the same block.
    /// `ty` must equal `action_call.output_tys[idx]`.
    ActionOutput { call: Var, idx: usize, ty: Ty },

    /// Produce a fresh random value drawn uniformly from the type’s domain.
    ///
    /// `name` identifies the [`RngDecl`] in the enclosing [`IRBlocks::rngs`]
    /// that provides this source of randomness.  Each occurrence is an
    /// independent sample.  Optimisers must **not** deduplicate, CSE, or
    /// reorder `Rng` stmts.  An `Rng` may be DCE’d only when its output is
    /// demonstrably unused.
    Rng {
        /// Name of the declared RNG source (matches an [`RngDecl::name`]).
        name: alloc::string::String,
        ty: Ty,
    },
}

impl<Var: Ord, Ty, Stor> Stmt<Var, Var, Ty, Stor> {
    /// Convenience for the common `Addr = Var` case: map `Var` and `Addr`
    /// with a **single shared callback**, leaving `Ty` and `Stor` to their
    /// own callbacks.
    ///
    /// Avoids the borrow-checker conflict that arises when two closures both
    /// capture the same `&mut FnMut` to pass to [`Stmt::map`].
    pub fn map_var<Ctx, NV: Ord, NT, NS, E>(
        self,
        ctx: &mut Ctx,
        go: &mut impl FnMut(&mut Ctx, Var) -> Result<NV, E>,
        ty_fn: &mut impl FnMut(&mut Ctx, Ty) -> Result<NT, E>,
        stor_fn: &mut impl FnMut(&mut Ctx, Stor) -> Result<NS, E>,
    ) -> Result<Stmt<NV, NV, NT, NS>, E> {
        Ok(match self {
            Stmt::StorageRead { storage, ty, addr } => Stmt::StorageRead {
                storage: stor_fn(ctx, storage)?,
                ty: ty_fn(ctx, ty)?,
                addr: go(ctx, addr)?,
            },
            Stmt::StorageWrite {
                storage,
                src,
                ty,
                addr,
            } => Stmt::StorageWrite {
                storage: stor_fn(ctx, storage)?,
                src: go(ctx, src)?,
                ty: ty_fn(ctx, ty)?,
                addr: go(ctx, addr)?,
            },
            Stmt::Const(c, ty) => Stmt::Const(c, ty_fn(ctx, ty)?),
            Stmt::Transmute {
                src,
                src_ty,
                dst_ty,
            } => Stmt::Transmute {
                src: go(ctx, src)?,
                src_ty: ty_fn(ctx, src_ty)?,
                dst_ty: ty_fn(ctx, dst_ty)?,
            },
            Stmt::Poly {
                ty,
                coeffs,
                constant,
            } => {
                let ty = ty_fn(ctx, ty)?;
                let coeffs = coeffs
                    .into_iter()
                    .map(|(mono, coeff)| {
                        let mono = mono
                            .into_iter()
                            .map(|v| go(ctx, v))
                            .collect::<Result<Vec<NV>, E>>()?;
                        Ok((mono, coeff))
                    })
                    .collect::<Result<PolyCoeffs<NV>, E>>()?;
                Stmt::Poly {
                    ty,
                    coeffs,
                    constant,
                }
            }
            Stmt::Rol { src, ty, n } => Stmt::Rol {
                src: go(ctx, src)?,
                ty: ty_fn(ctx, ty)?,
                n,
            },
            Stmt::Ror { src, ty, n } => Stmt::Ror {
                src: go(ctx, src)?,
                ty: ty_fn(ctx, ty)?,
                n,
            },
            Stmt::Merge { parts, ty } => Stmt::Merge {
                parts: parts
                    .into_iter()
                    .map(|v| go(ctx, v))
                    .collect::<Result<Vec<NV>, E>>()?,
                ty: ty_fn(ctx, ty)?,
            },
            Stmt::Splat { src, ty } => Stmt::Splat {
                src: go(ctx, src)?,
                ty: ty_fn(ctx, ty)?,
            },
            Stmt::Shuffle { result_bits, ty } => Stmt::Shuffle {
                result_bits: result_bits
                    .into_iter()
                    .map(|(bit_idx, v)| Ok((bit_idx, go(ctx, v)?)))
                    .collect::<Result<Vec<(u8, NV)>, E>>()?,
                ty: ty_fn(ctx, ty)?,
            },
            Stmt::OracleCall {
                name,
                args,
                output_tys,
                result_ty,
            } => Stmt::OracleCall {
                name,
                args: args
                    .into_iter()
                    .map(|v| go(ctx, v))
                    .collect::<Result<Vec<NV>, E>>()?,
                output_tys: output_tys
                    .into_iter()
                    .map(|t| ty_fn(ctx, t))
                    .collect::<Result<Vec<NT>, E>>()?,
                result_ty: ty_fn(ctx, result_ty)?,
            },
            Stmt::OracleOutput { call, idx, ty } => Stmt::OracleOutput {
                call: go(ctx, call)?,
                idx,
                ty: ty_fn(ctx, ty)?,
            },
            Stmt::ActionCall {
                name,
                guard,
                args,
                fallbacks,
                output_tys,
                result_ty,
            } => Stmt::ActionCall {
                name,
                guard: go(ctx, guard)?,
                args: args
                    .into_iter()
                    .map(|v| go(ctx, v))
                    .collect::<Result<Vec<NV>, E>>()?,
                fallbacks: fallbacks
                    .into_iter()
                    .map(|v| go(ctx, v))
                    .collect::<Result<Vec<NV>, E>>()?,
                output_tys: output_tys
                    .into_iter()
                    .map(|t| ty_fn(ctx, t))
                    .collect::<Result<Vec<NT>, E>>()?,
                result_ty: ty_fn(ctx, result_ty)?,
            },
            Stmt::ActionStore {
                name,
                guard,
                args,
                fallbacks,
                output_tys,
                targets,
            } => Stmt::ActionStore {
                name,
                guard: go(ctx, guard)?,
                args: args
                    .into_iter()
                    .map(|v| go(ctx, v))
                    .collect::<Result<Vec<NV>, E>>()?,
                fallbacks: fallbacks
                    .into_iter()
                    .map(|v| go(ctx, v))
                    .collect::<Result<Vec<NV>, E>>()?,
                output_tys: output_tys
                    .into_iter()
                    .map(|t| ty_fn(ctx, t))
                    .collect::<Result<Vec<NT>, E>>()?,
                targets: targets
                    .into_iter()
                    .map(|target| {
                        Ok(ActionTarget {
                            storage: stor_fn(ctx, target.storage)?,
                            addr: go(ctx, target.addr)?,
                        })
                    })
                    .collect::<Result<Vec<_>, E>>()?,
            },
            Stmt::ActionOutput { call, idx, ty } => Stmt::ActionOutput {
                call: go(ctx, call)?,
                idx,
                ty: ty_fn(ctx, ty)?,
            },
            Stmt::Rng { name, ty } => Stmt::Rng {
                name,
                ty: ty_fn(ctx, ty)?,
            },
        })
    }
}

impl<Var, Addr, Ty, Stor> Stmt<Var, Addr, Ty, Stor> {
    /// Map all four generic parameters simultaneously, potentially fallibly.
    ///
    /// `ctx` is passed by `&mut` to every callback so that all four callbacks
    /// can share mutable state (e.g., a `TypeTable` under construction or a
    /// `StorageAllocator`) without borrow conflicts.
    ///
    /// # Bounds
    /// `NV: Ord` is required because `Poly.coeffs` is a `BTreeMap<Vec<Var>, _>`
    /// and the new keys must remain ordered.
    ///
    /// # Non-generic fields
    /// `name` fields in `OracleCall`, `ActionCall`, and `Rng` are owned
    /// `String`s that are not parameterised by `Var`/`Addr`/`Ty`/`Stor`.
    /// They are moved into the result unchanged.
    pub fn map<Ctx, NV, NA, NT, NS, E>(
        self,
        ctx: &mut Ctx,
        mut var_fn: impl FnMut(&mut Ctx, Var) -> Result<NV, E>,
        mut addr_fn: impl FnMut(&mut Ctx, Addr) -> Result<NA, E>,
        mut ty_fn: impl FnMut(&mut Ctx, Ty) -> Result<NT, E>,
        mut stor_fn: impl FnMut(&mut Ctx, Stor) -> Result<NS, E>,
    ) -> Result<Stmt<NV, NA, NT, NS>, E>
    where
        NV: Ord,
    {
        Ok(match self {
            Stmt::StorageRead { storage, ty, addr } => Stmt::StorageRead {
                storage: stor_fn(ctx, storage)?,
                ty: ty_fn(ctx, ty)?,
                addr: addr_fn(ctx, addr)?,
            },
            Stmt::StorageWrite {
                storage,
                src,
                ty,
                addr,
            } => Stmt::StorageWrite {
                storage: stor_fn(ctx, storage)?,
                src: var_fn(ctx, src)?,
                ty: ty_fn(ctx, ty)?,
                addr: addr_fn(ctx, addr)?,
            },
            Stmt::Const(c, ty) => Stmt::Const(c, ty_fn(ctx, ty)?),
            Stmt::Transmute {
                src,
                src_ty,
                dst_ty,
            } => Stmt::Transmute {
                src: var_fn(ctx, src)?,
                src_ty: ty_fn(ctx, src_ty)?,
                dst_ty: ty_fn(ctx, dst_ty)?,
            },
            Stmt::Poly {
                ty,
                coeffs,
                constant,
            } => {
                let ty = ty_fn(ctx, ty)?;
                let coeffs = coeffs
                    .into_iter()
                    .map(|(mono, coeff)| {
                        let mono = mono
                            .into_iter()
                            .map(|v| var_fn(ctx, v))
                            .collect::<Result<Vec<NV>, E>>()?;
                        Ok((mono, coeff))
                    })
                    .collect::<Result<PolyCoeffs<NV>, E>>()?;
                Stmt::Poly {
                    ty,
                    coeffs,
                    constant,
                }
            }
            Stmt::Rol { src, ty, n } => Stmt::Rol {
                src: var_fn(ctx, src)?,
                ty: ty_fn(ctx, ty)?,
                n,
            },
            Stmt::Ror { src, ty, n } => Stmt::Ror {
                src: var_fn(ctx, src)?,
                ty: ty_fn(ctx, ty)?,
                n,
            },
            Stmt::Merge { parts, ty } => Stmt::Merge {
                parts: parts
                    .into_iter()
                    .map(|v| var_fn(ctx, v))
                    .collect::<Result<Vec<NV>, E>>()?,
                ty: ty_fn(ctx, ty)?,
            },
            Stmt::Splat { src, ty } => Stmt::Splat {
                src: var_fn(ctx, src)?,
                ty: ty_fn(ctx, ty)?,
            },
            Stmt::Shuffle { result_bits, ty } => Stmt::Shuffle {
                result_bits: result_bits
                    .into_iter()
                    .map(|(bit_idx, v)| Ok((bit_idx, var_fn(ctx, v)?)))
                    .collect::<Result<Vec<(u8, NV)>, E>>()?,
                ty: ty_fn(ctx, ty)?,
            },
            Stmt::OracleCall {
                name,
                args,
                output_tys,
                result_ty,
            } => Stmt::OracleCall {
                name,
                args: args
                    .into_iter()
                    .map(|v| var_fn(ctx, v))
                    .collect::<Result<Vec<NV>, E>>()?,
                output_tys: output_tys
                    .into_iter()
                    .map(|t| ty_fn(ctx, t))
                    .collect::<Result<Vec<NT>, E>>()?,
                result_ty: ty_fn(ctx, result_ty)?,
            },
            Stmt::OracleOutput { call, idx, ty } => Stmt::OracleOutput {
                call: var_fn(ctx, call)?,
                idx,
                ty: ty_fn(ctx, ty)?,
            },
            Stmt::ActionCall {
                name,
                guard,
                args,
                fallbacks,
                output_tys,
                result_ty,
            } => Stmt::ActionCall {
                name,
                guard: var_fn(ctx, guard)?,
                args: args
                    .into_iter()
                    .map(|v| var_fn(ctx, v))
                    .collect::<Result<Vec<NV>, E>>()?,
                fallbacks: fallbacks
                    .into_iter()
                    .map(|v| var_fn(ctx, v))
                    .collect::<Result<Vec<NV>, E>>()?,
                output_tys: output_tys
                    .into_iter()
                    .map(|t| ty_fn(ctx, t))
                    .collect::<Result<Vec<NT>, E>>()?,
                result_ty: ty_fn(ctx, result_ty)?,
            },
            Stmt::ActionStore {
                name,
                guard,
                args,
                fallbacks,
                output_tys,
                targets,
            } => Stmt::ActionStore {
                name,
                guard: var_fn(ctx, guard)?,
                args: args
                    .into_iter()
                    .map(|v| var_fn(ctx, v))
                    .collect::<Result<Vec<NV>, E>>()?,
                fallbacks: fallbacks
                    .into_iter()
                    .map(|v| var_fn(ctx, v))
                    .collect::<Result<Vec<NV>, E>>()?,
                output_tys: output_tys
                    .into_iter()
                    .map(|t| ty_fn(ctx, t))
                    .collect::<Result<Vec<NT>, E>>()?,
                targets: targets
                    .into_iter()
                    .map(|target| {
                        Ok(ActionTarget {
                            storage: stor_fn(ctx, target.storage)?,
                            addr: addr_fn(ctx, target.addr)?,
                        })
                    })
                    .collect::<Result<Vec<_>, E>>()?,
            },
            Stmt::ActionOutput { call, idx, ty } => Stmt::ActionOutput {
                call: var_fn(ctx, call)?,
                idx,
                ty: ty_fn(ctx, ty)?,
            },
            Stmt::Rng { name, ty } => Stmt::Rng {
                name,
                ty: ty_fn(ctx, ty)?,
            },
        })
    }

    /// Borrow all generic parameters in place.
    ///
    /// Returns a `Stmt<&Var, &Addr, &Ty, &Stor>` whose fields are references
    /// into `self`.
    ///
    /// # Cost
    /// * Most variants: O(1) field borrows.
    /// * `Poly`: O(n log n) — the BTreeMap is rebuilt with `Vec<&Var>` keys.
    /// * `OracleCall`, `ActionCall`, `Rng`: the non-generic `name: String` is
    ///   **cloned** because it is not a generic parameter.
    ///
    /// # Bounds
    /// `Var: Ord` is required to rebuild the `Poly.coeffs` BTreeMap.
    pub fn as_ref(&self) -> Stmt<&Var, &Addr, &Ty, &Stor>
    where
        Var: Ord,
    {
        match self {
            Stmt::StorageRead { storage, ty, addr } => Stmt::StorageRead { storage, ty, addr },
            Stmt::StorageWrite {
                storage,
                src,
                ty,
                addr,
            } => Stmt::StorageWrite {
                storage,
                src,
                ty,
                addr,
            },
            Stmt::Const(c, ty) => Stmt::Const(*c, ty),
            Stmt::Transmute {
                src,
                src_ty,
                dst_ty,
            } => Stmt::Transmute {
                src,
                src_ty,
                dst_ty,
            },
            Stmt::Poly {
                ty,
                coeffs,
                constant,
            } => {
                let coeffs = coeffs
                    .iter()
                    .map(|(mono, coeff)| (mono.iter().collect::<Vec<&Var>>(), *coeff))
                    .collect::<PolyCoeffs<&Var>>();
                Stmt::Poly {
                    ty,
                    coeffs,
                    constant: *constant,
                }
            }
            Stmt::Rol { src, ty, n } => Stmt::Rol { src, ty, n: *n },
            Stmt::Ror { src, ty, n } => Stmt::Ror { src, ty, n: *n },
            Stmt::Merge { parts, ty } => Stmt::Merge {
                parts: parts.iter().collect(),
                ty,
            },
            Stmt::Splat { src, ty } => Stmt::Splat { src, ty },
            Stmt::Shuffle { result_bits, ty } => Stmt::Shuffle {
                result_bits: result_bits.iter().map(|(b, v)| (*b, v)).collect(),
                ty,
            },
            Stmt::OracleCall {
                name,
                args,
                output_tys,
                result_ty,
            } => Stmt::OracleCall {
                name: name.clone(),
                args: args.iter().collect(),
                output_tys: output_tys.iter().collect(),
                result_ty,
            },
            Stmt::OracleOutput { call, idx, ty } => Stmt::OracleOutput {
                call,
                idx: *idx,
                ty,
            },
            Stmt::ActionCall {
                name,
                guard,
                args,
                fallbacks,
                output_tys,
                result_ty,
            } => Stmt::ActionCall {
                name: name.clone(),
                guard,
                args: args.iter().collect(),
                fallbacks: fallbacks.iter().collect(),
                output_tys: output_tys.iter().collect(),
                result_ty,
            },
            Stmt::ActionStore {
                name,
                guard,
                args,
                fallbacks,
                output_tys,
                targets,
            } => Stmt::ActionStore {
                name: name.clone(),
                guard,
                args: args.iter().collect(),
                fallbacks: fallbacks.iter().collect(),
                output_tys: output_tys.iter().collect(),
                targets: targets
                    .iter()
                    .map(|target| ActionTarget {
                        storage: &target.storage,
                        addr: &target.addr,
                    })
                    .collect(),
            },
            Stmt::ActionOutput { call, idx, ty } => Stmt::ActionOutput {
                call,
                idx: *idx,
                ty,
            },
            Stmt::Rng { name, ty } => Stmt::Rng {
                name: name.clone(),
                ty,
            },
        }
    }
}

// ============================================================================
// Generic substitution utilities
// ============================================================================

/// Merges a guest [`TypeTable`] into a host [`TypeTable`], producing a mapping
/// from guest [`TypeId`]s to their equivalent host [`TypeId`]s.
///
/// Types that are structurally identical to an existing host type share the
/// same [`TypeId`] (via [`TypeTable::intern`]).  New types are appended.
///
/// Reusable in any pass that combines programs from different modules.
pub struct TypeRemapper {
    /// `map[guest_id.0]` = the corresponding [`TypeId`] in the host table.
    pub map: alloc::vec::Vec<TypeId>,
}

impl TypeRemapper {
    /// Merge `guest` into `host` and return the resulting remapper.
    pub fn merge(host: &mut TypeTable, guest: &TypeTable) -> TypeRemapper {
        let n = guest.0.len();
        let mut map = alloc::vec![TypeId(0); n];
        let mut done = alloc::vec![false; n];
        for i in 0..n {
            Self::remap_one(i, guest, host, &mut map, &mut done);
        }
        TypeRemapper { map }
    }

    fn remap_one(
        idx: usize,
        guest: &TypeTable,
        host: &mut TypeTable,
        map: &mut alloc::vec::Vec<TypeId>,
        done: &mut alloc::vec::Vec<bool>,
    ) -> TypeId {
        if done[idx] {
            return map[idx];
        }
        done[idx] = true; // set before recursing (cycle guard)
        let remapped = match &guest.0[idx] {
            IrType::Primitive(p) => IrType::Primitive(*p),
            IrType::Vec(n, inner) => {
                let inner_host = Self::remap_one(inner.0 as usize, guest, host, map, done);
                IrType::Vec(*n, inner_host)
            }
            IrType::Tuple(parts) => {
                let parts_host: alloc::vec::Vec<TypeId> = parts
                    .iter()
                    .map(|p| Self::remap_one(p.0 as usize, guest, host, map, done))
                    .collect();
                IrType::Tuple(parts_host)
            }
            IrType::Block { params } => {
                let params_host: alloc::vec::Vec<TypeId> = params
                    .iter()
                    .map(|p| Self::remap_one(p.0 as usize, guest, host, map, done))
                    .collect();
                IrType::Block {
                    params: params_host,
                }
            }
            IrType::Func { params, results } => {
                let params_host: alloc::vec::Vec<TypeId> = params
                    .iter()
                    .map(|p| Self::remap_one(p.0 as usize, guest, host, map, done))
                    .collect();
                let results_host: alloc::vec::Vec<TypeId> = results
                    .iter()
                    .map(|r| Self::remap_one(r.0 as usize, guest, host, map, done))
                    .collect();
                IrType::Func {
                    params: params_host,
                    results: results_host,
                }
            }
        };
        let host_id = host.intern(remapped);
        map[idx] = host_id;
        host_id
    }

    /// Remap a single guest [`TypeId`] to its host equivalent.
    #[inline]
    pub fn remap(&self, id: TypeId) -> TypeId {
        self.map[id.0 as usize]
    }

    /// Remap every [`TypeId`] field in a [`Stmt<V, A>`] in-place.
    ///
    /// Variable references (`V`, `A`) are left unchanged.
    pub fn remap_stmt_types<V: Clone, A: Clone>(&self, stmt: &mut Stmt<V, A>) {
        match stmt {
            Stmt::StorageRead { ty, .. } => {
                *ty = self.remap(*ty);
            }
            Stmt::StorageWrite { ty, .. } => {
                *ty = self.remap(*ty);
            }
            Stmt::Const(_, ty) => {
                *ty = self.remap(*ty);
            }
            Stmt::Transmute { src_ty, dst_ty, .. } => {
                *src_ty = self.remap(*src_ty);
                *dst_ty = self.remap(*dst_ty);
            }
            Stmt::Poly {
                ty,
                coeffs: _,
                constant: _,
            } => {
                *ty = self.remap(*ty);
            }
            Stmt::Rol { ty, .. } | Stmt::Ror { ty, .. } => {
                *ty = self.remap(*ty);
            }
            Stmt::Merge { ty, .. } => {
                *ty = self.remap(*ty);
            }
            Stmt::Splat { ty, .. } => {
                *ty = self.remap(*ty);
            }
            Stmt::Shuffle { ty, .. } => {
                *ty = self.remap(*ty);
            }
            Stmt::OracleCall {
                output_tys,
                result_ty,
                ..
            } => {
                for t in output_tys.iter_mut() {
                    *t = self.remap(*t);
                }
                *result_ty = self.remap(*result_ty);
            }
            Stmt::OracleOutput { ty, .. } => {
                *ty = self.remap(*ty);
            }
            Stmt::ActionCall {
                output_tys,
                result_ty,
                ..
            } => {
                for t in output_tys.iter_mut() {
                    *t = self.remap(*t);
                }
                *result_ty = self.remap(*result_ty);
            }
            Stmt::ActionStore { output_tys, .. } => {
                for t in output_tys.iter_mut() {
                    *t = self.remap(*t);
                }
            }
            Stmt::ActionOutput { ty, .. } => {
                *ty = self.remap(*ty);
            }
            Stmt::Rng { ty, .. } => {
                *ty = self.remap(*ty);
            }
        }
    }

    /// Remap [`TypeId`]s inside an [`OracleDecl`].
    pub fn remap_oracle_decl(&self, decl: &mut OracleDecl) {
        for t in decl.params.iter_mut() {
            *t = self.remap(*t);
        }
        for t in decl.results.iter_mut() {
            *t = self.remap(*t);
        }
    }

    /// Remap [`TypeId`]s inside an [`ActionDecl`].
    pub fn remap_action_decl(&self, decl: &mut ActionDecl) {
        for t in decl.params.iter_mut() {
            *t = self.remap(*t);
        }
        for t in decl.results.iter_mut() {
            *t = self.remap(*t);
        }
    }

    /// Remap the [`TypeId`] inside an [`RngDecl`].
    pub fn remap_rng_decl(&self, decl: &mut RngDecl) {
        decl.ty = self.remap(decl.ty);
    }
}

/// Allocates fresh [`StorageId`]s above the range already in use.
///
/// Call [`StorageAllocator::new`] with `first_free` set to one above the
/// maximum [`StorageId`] observed in the program (clamped to ≥ 64 to avoid
/// the reserved protocol range).
pub struct StorageAllocator {
    pub next: u32,
}

impl StorageAllocator {
    /// Create an allocator starting at `first_free`.
    ///
    /// The caller is responsible for scanning the program to find the current
    /// maximum StorageId and passing `max + 1` here (clamped to ≥ 64).
    pub fn new(first_free: u32) -> Self {
        StorageAllocator { next: first_free }
    }

    /// Allocate the next fresh [`StorageId`].
    pub fn alloc(&mut self) -> StorageId {
        let id = StorageId(self.next);
        self.next += 1;
        id
    }
}

// ============================================================================
// Node: shared per-value provenance + side wrapper
// ============================================================================

impl<T, P: Clone> Node<T, P> {
    /// Construct a node from its parts.
    pub fn new(kind: T, prov: P, side: Option<volar_side::SideId>) -> Self {
        Node { kind, prov, side }
    }

    /// Map this node's provenance via `f`. `kind` and `side` pass through
    /// unchanged — the right operation whenever the payload `T` does not
    /// itself mention `P` (the common case for SSA/arena statement types).
    pub fn map_prov<Q: Clone>(self, f: impl FnOnce(P) -> Q) -> Node<T, Q> {
        Node {
            kind: self.kind,
            prov: f(self.prov),
            side: self.side,
        }
    }
}

#[cfg(all(test, feature = "rkyv"))]
mod generated_binary_compat_tests {
    use super::*;

    #[test]
    fn scalar_artifacts_match_the_pinned_derive_layout() {
        // These are fixtures produced by the former rkyv_derive definitions
        // under the workspace's pinned rkyv 0.8 configuration.  Keep them
        // byte-for-byte: persisted blobs deliberately have no migration here.
        assert_eq!(
            rkyv::to_bytes::<rkyv::rancor::Error>(&TypeId(0x1020_3040))
                .unwrap()
                .as_slice(),
            &[0x40, 0x30, 0x20, 0x10]
        );
        assert_eq!(
            rkyv::to_bytes::<rkyv::rancor::Error>(&StorageId(0xa0b0_c0d0))
                .unwrap()
                .as_slice(),
            &[0xd0, 0xc0, 0xb0, 0xa0]
        );
        assert_eq!(
            rkyv::to_bytes::<rkyv::rancor::Error>(&Type::Galois64)
                .unwrap()
                .as_slice(),
            &[8]
        );
        assert_eq!(
            rkyv::to_bytes::<rkyv::rancor::Error>(&Constant {
                hi: 0x0001_0203_0405_0607_0809_0a0b_0c0d_0e0f,
                lo: 0xf0f1_f2f3_f4f5_f6f7_f8f9_fafb_fcfd_feff,
            })
            .unwrap()
            .as_slice(),
            &[
                0x0f, 0x0e, 0x0d, 0x0c, 0x0b, 0x0a, 0x09, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02,
                0x01, 0x00, 0xff, 0xfe, 0xfd, 0xfc, 0xfb, 0xfa, 0xf9, 0xf8, 0xf7, 0xf6, 0xf5, 0xf4,
                0xf3, 0xf2, 0xf1, 0xf0,
            ]
        );
    }
}

/// Implemented by a tree-shaped `Kind` payload that itself embeds `P` in
/// nested [`Node`]s (e.g. a compiler's expression/statement IR, where a
/// `Binary` variant holds boxed `Node<ExprKind<P>, P>` operands).
///
/// `Node<T, P>::map_kind_prov` delegates to this trait to recurse into `T`
/// and remap every nested `P`, then maps its own top-level `prov`. Payload
/// types that do not embed `P` (SSA/arena statement kinds) have no need for
/// this trait — use [`Node::map_prov`] directly instead.
pub trait MapKind<P: Clone, Q: Clone> {
    /// The same `Kind` shape with every nested `P` replaced by `Q`.
    type Output;

    /// Recurse into `self`, replacing every nested `P` via `f`.
    fn map_kind(self, f: &impl Fn(P) -> Q) -> Self::Output;
}

impl<T, P: Clone> Node<T, P> {
    /// Map provenance through a tree-shaped payload that itself embeds `P`,
    /// recursing via `T::map_kind` and then mapping this node's own `prov`.
    pub fn map_kind_prov<Q: Clone>(self, f: &impl Fn(P) -> Q) -> Node<T::Output, Q>
    where
        T: MapKind<P, Q>,
    {
        Node {
            kind: self.kind.map_kind(f),
            prov: f(self.prov),
            side: self.side,
        }
    }
}
