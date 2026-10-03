// @reliability: experimental
// @ai: assisted
//! Shared helpers for constant-folding all three IR layers.

use alloc::{collections::BTreeMap, vec, vec::Vec};
use volar_ir_common::{
    BoolRing, Constant, IrType, PolyCoeffs, PrimeSpec, Stmt, Type, TypeId, TypeTable,
    contains_prime_field, eval_prime_poly, monomial_product, mul_identity, mul_is_idempotent,
    prime_spec, repetition_residue,
};

// ============================================================================
// Alias canonicalization
// ============================================================================

/// Follow the alias chain for `v` through `alias_map`, up to 64 hops.
pub fn canon_alias<V: Copy + Ord>(alias_map: &BTreeMap<V, V>, mut v: V) -> V {
    for _ in 0..64 {
        match alias_map.get(&v) {
            Some(&w) if w != v => v = w,
            _ => break,
        }
    }
    v
}

// ============================================================================
// 256-bit constant arithmetic
// ============================================================================

pub fn constant_is_zero(c: Constant) -> bool {
    c.hi == 0 && c.lo == 0
}

/// True if the lowest `width` bits of `c` are all 1 (and width ≤ 256).
pub fn constant_is_all_ones(c: Constant, width: usize) -> bool {
    let m = mask_constant(
        Constant {
            hi: u128::MAX,
            lo: u128::MAX,
        },
        width,
    );
    c == m
}

/// Zero out bits above position `width-1`.
pub fn mask_constant(c: Constant, width: usize) -> Constant {
    if width == 0 {
        return Constant { hi: 0, lo: 0 };
    }
    if width >= 256 {
        return c;
    }
    if width >= 128 {
        let hi_bits = width - 128;
        let hi_mask = if hi_bits >= 128 {
            u128::MAX
        } else {
            (1u128 << hi_bits) - 1
        };
        Constant {
            lo: c.lo,
            hi: c.hi & hi_mask,
        }
    } else {
        let lo_mask = (1u128 << width) - 1;
        Constant {
            lo: c.lo & lo_mask,
            hi: 0,
        }
    }
}

pub fn constant_and(a: Constant, b: Constant) -> Constant {
    Constant {
        hi: a.hi & b.hi,
        lo: a.lo & b.lo,
    }
}

pub fn constant_xor(a: Constant, b: Constant) -> Constant {
    Constant {
        hi: a.hi ^ b.hi,
        lo: a.lo ^ b.lo,
    }
}

pub fn constant_or(a: Constant, b: Constant) -> Constant {
    Constant {
        hi: a.hi | b.hi,
        lo: a.lo | b.lo,
    }
}

/// Shift `c` left by `n` bit positions (into a 256-bit field).
pub fn constant_shl(c: Constant, n: usize) -> Constant {
    if n == 0 {
        return c;
    }
    if n >= 256 {
        return Constant { hi: 0, lo: 0 };
    }
    if n >= 128 {
        Constant {
            hi: c.lo << (n - 128),
            lo: 0,
        }
    } else {
        Constant {
            hi: (c.hi << n) | (c.lo >> (128 - n)),
            lo: c.lo << n,
        }
    }
}

/// Logical shift `c` right by `n` bit positions.
pub fn constant_shr(c: Constant, n: usize) -> Constant {
    if n == 0 {
        return c;
    }
    if n >= 256 {
        return Constant { hi: 0, lo: 0 };
    }
    if n >= 128 {
        Constant {
            hi: 0,
            lo: c.hi >> (n - 128),
        }
    } else {
        Constant {
            hi: c.hi >> n,
            lo: (c.lo >> n) | (c.hi << (128 - n)),
        }
    }
}

/// Rotate the lowest `width` bits of `c` left by `n`.
pub fn constant_rol(c: Constant, width: usize, n: usize) -> Constant {
    if width == 0 || n == 0 {
        return c;
    }
    let n = n % width;
    if n == 0 {
        return c;
    }
    let c = mask_constant(c, width);
    let l = constant_shl(c, n);
    let r = constant_shr(c, width - n);
    mask_constant(constant_or(l, r), width)
}

/// Rotate the lowest `width` bits of `c` right by `n`.
pub fn constant_ror(c: Constant, width: usize, n: usize) -> Constant {
    if width == 0 || n == 0 {
        return c;
    }
    let n = n % width;
    constant_rol(c, width, width - n)
}

// ============================================================================
// Type utilities
// ============================================================================

/// Bit-width of a `Type` (always well-defined for non-field types).
pub fn primitive_type_width(t: Type) -> usize {
    match t {
        Type::Bit => 1,
        Type::_8 => 8,
        Type::_16 => 16,
        Type::_32 => 32,
        Type::_64 => 64,
        Type::_128 => 128,
        Type::_256 => 256,
        _ => panic!("primitive_type_width: unknown Type variant"),
    }
}

/// Recursively compute the bit-width of `ty_id`.
/// Returns `None` for `Block` and `Func`.
pub fn type_bit_width(ty_id: TypeId, types: &TypeTable) -> Option<usize> {
    types.value_bit_width(ty_id)
}

/// Returns `true` when `ty_id` is an extension field or contains one.
pub fn is_field_type(ty_id: TypeId, types: &TypeTable) -> bool {
    volar_ir_common::contains_ext_field(ty_id, types)
}

/// Extract the output `TypeId` from a `Stmt`, if it produces one.
pub fn stmt_output_type<V, A>(stmt: &Stmt<V, A>) -> Option<TypeId> {
    match stmt {
        Stmt::Const(_, ty) => Some(*ty),
        Stmt::Transmute { dst_ty, .. } => Some(*dst_ty),
        Stmt::Poly { ty, .. } => Some(*ty),
        Stmt::Rol { ty, .. } => Some(*ty),
        Stmt::Ror { ty, .. } => Some(*ty),
        Stmt::Merge { ty, .. } => Some(*ty),
        Stmt::Splat { ty, .. } => Some(*ty),
        Stmt::Shuffle { ty, .. } => Some(*ty),
        Stmt::OracleCall { result_ty, .. } => Some(*result_ty),
        Stmt::OracleOutput { ty, .. } => Some(*ty),
        Stmt::ActionCall { result_ty, .. } => Some(*result_ty),
        Stmt::ActionStore { .. } => None,
        Stmt::ActionOutput { ty, .. } => Some(*ty),
        Stmt::Rng { ty, .. } => Some(*ty),
        Stmt::StorageRead { ty, .. } => Some(*ty),
        Stmt::StorageWrite { .. } => None,
        _ => None,
    }
}

// ============================================================================
// Alias application to Stmt<V, V>
// ============================================================================

/// Rewrite all var references in `stmt` through `alias_map`.
///
/// For `Poly`, monomial keys are rebuilt after substitution. Duplicate
/// factors collapse only when `factor_idempotent` says that factor's
/// product is idempotent (`v * v = v`). Extension-field factors stay, so
/// `a * a` remains the square.
///
/// Returns `true` if any reference was changed.
pub fn apply_aliases_to_stmt<V: Copy + Ord + Clone>(
    stmt: &mut Stmt<V, V>,
    alias_map: &BTreeMap<V, V>,
    mut factor_idempotent: impl FnMut(&V) -> bool,
) -> bool {
    if alias_map.is_empty() {
        return false;
    }
    let mut changed = false;

    match stmt {
        Stmt::StorageRead { addr, .. } => {
            let c = canon_alias(alias_map, *addr);
            if c != *addr {
                *addr = c;
                changed = true;
            }
        }
        Stmt::StorageWrite { src, addr, .. } => {
            let cs = canon_alias(alias_map, *src);
            if cs != *src {
                *src = cs;
                changed = true;
            }
            let ca = canon_alias(alias_map, *addr);
            if ca != *addr {
                *addr = ca;
                changed = true;
            }
        }
        Stmt::Const(_, _) | Stmt::Rng { .. } => {}
        Stmt::Transmute { src, .. } => {
            let c = canon_alias(alias_map, *src);
            if c != *src {
                *src = c;
                changed = true;
            }
        }
        Stmt::Poly { coeffs, .. } => {
            // Rebuild the canonical coefficient collection with aliased,
            // sorted, deduped keys.
            // XOR-accumulate coefficients for colliding keys.
            let old = core::mem::take(coeffs);
            for (key, coeff) in old {
                if coeff & 1 == 0 {
                    changed = true;
                    continue;
                }
                let mut tagged: Vec<(V, bool)> = key
                    .iter()
                    .map(|&v| {
                        let w = canon_alias(alias_map, v);
                        if w != v {
                            changed = true;
                        }
                        (w, factor_idempotent(&v))
                    })
                    .collect();
                tagged.sort_by(|a, b| a.0.cmp(&b.0));
                let before_len = tagged.len();
                let mut new_key: Vec<V> = Vec::with_capacity(before_len);
                for (var, idempotent) in tagged {
                    if idempotent && new_key.last() == Some(&var) {
                        continue;
                    }
                    new_key.push(var);
                }
                if new_key.len() != before_len {
                    changed = true;
                }
                *coeffs.entry(new_key).or_insert(0) ^= coeff;
            }
            coeffs.retain(|_, c| *c & 1 != 0);
        }
        Stmt::Rol { src, .. } | Stmt::Ror { src, .. } | Stmt::Splat { src, .. } => {
            let c = canon_alias(alias_map, *src);
            if c != *src {
                *src = c;
                changed = true;
            }
        }
        Stmt::Merge { parts, .. } => {
            for p in parts.iter_mut() {
                let c = canon_alias(alias_map, *p);
                if c != *p {
                    *p = c;
                    changed = true;
                }
            }
        }
        Stmt::Shuffle { result_bits, .. } => {
            for (_, v) in result_bits.iter_mut() {
                let c = canon_alias(alias_map, *v);
                if c != *v {
                    *v = c;
                    changed = true;
                }
            }
        }
        Stmt::OracleCall { args, .. } => {
            for a in args.iter_mut() {
                let c = canon_alias(alias_map, *a);
                if c != *a {
                    *a = c;
                    changed = true;
                }
            }
        }
        Stmt::OracleOutput { call, .. } => {
            let c = canon_alias(alias_map, *call);
            if c != *call {
                *call = c;
                changed = true;
            }
        }
        Stmt::ActionCall {
            guard,
            args,
            fallbacks,
            ..
        } => {
            let cg = canon_alias(alias_map, *guard);
            if cg != *guard {
                *guard = cg;
                changed = true;
            }
            for a in args.iter_mut() {
                let c = canon_alias(alias_map, *a);
                if c != *a {
                    *a = c;
                    changed = true;
                }
            }
            for f in fallbacks.iter_mut() {
                let c = canon_alias(alias_map, *f);
                if c != *f {
                    *f = c;
                    changed = true;
                }
            }
        }
        Stmt::ActionStore {
            guard,
            args,
            fallbacks,
            targets,
            ..
        } => {
            let cg = canon_alias(alias_map, *guard);
            if cg != *guard {
                *guard = cg;
                changed = true;
            }
            for a in args.iter_mut() {
                let c = canon_alias(alias_map, *a);
                if c != *a {
                    *a = c;
                    changed = true;
                }
            }
            for f in fallbacks.iter_mut() {
                let c = canon_alias(alias_map, *f);
                if c != *f {
                    *f = c;
                    changed = true;
                }
            }
            for target in targets.iter_mut() {
                let c = canon_alias(alias_map, target.addr);
                if c != target.addr {
                    target.addr = c;
                    changed = true;
                }
            }
        }
        Stmt::ActionOutput { call, .. } => {
            let c = canon_alias(alias_map, *call);
            if c != *call {
                *call = c;
                changed = true;
            }
        }
        _ => {}
    }

    changed
}

// ============================================================================
// GF(2) polynomial substitution
// ============================================================================

/// Inline `src_poly` into `dst_poly` wherever `src_var` appears as a
/// **singleton-key** monomial (i.e. the key is exactly `[src_var]`).
///
/// `src_poly` evaluates to `src_constant XOR (XOR of src_coeffs monomials)`.
/// When `dst_coeffs` contains the entry `[src_var] → c` with `c & 1 != 0`:
/// - Remove the singleton entry.
/// - XOR `src_constant` into `*dst_constant`.
/// - For each `(src_key, src_coeff)` in `src_coeffs` with odd coeff: XOR a new
///   monomial `src_key` into `dst_coeffs`.
///
/// Zero-coefficient entries are removed afterwards.
/// Returns `true` if any substitution was made.
pub fn merge_poly_into<V: Clone + Ord>(
    dst_coeffs: &mut PolyCoeffs<V>,
    dst_constant: &mut Constant,
    src_var: &V,
    src_coeffs: &PolyCoeffs<V>,
    src_constant: Constant,
) -> bool {
    let singleton_key = alloc::vec![src_var.clone()];
    let coeff = match dst_coeffs.get(&singleton_key).copied() {
        Some(c) if c & 1 != 0 => c,
        _ => return false,
    };

    // Remove the singleton entry.
    dst_coeffs.remove(&singleton_key);

    // XOR src_constant into dst_constant (coefficient is odd → multiply by 1 in GF(2)).
    if !constant_is_zero(src_constant) {
        *dst_constant = constant_xor(*dst_constant, src_constant);
    }

    // XOR in each src monomial.
    for (src_key, &src_coeff) in src_coeffs {
        if src_coeff & 1 == 0 {
            continue;
        }
        *dst_coeffs.entry(src_key.clone()).or_insert(0) ^= coeff;
    }

    // Remove zero-coefficient entries (XOR cancellations).
    dst_coeffs.retain(|_, c| *c & 1 != 0);

    true
}

// ============================================================================
// GF(2) polynomial folding
// ============================================================================

/// Simplify a `Poly` in-place using known constants and type information.
///
/// Characteristic-2 cancellation applies to bit, integer, and extension-field
/// outputs. A prime-field sum adds repetition counts modulo `p` and does not
/// cancel even coefficients. Folds that assume `a * a = a` apply only when
/// the factor's type is idempotent. An all-constant monomial evaluates with
/// the real field product, and an empty monomial contributes that type's
/// multiplicative identity.
///
/// Returns `true` if any change was made.
pub fn fold_poly_in_place<V: Clone + Ord>(
    ty: TypeId,
    coeffs: &mut PolyCoeffs<V>,
    constant: &mut Constant,
    const_map: &BTreeMap<V, Constant>,
    type_map: &BTreeMap<V, TypeId>,
    types: &TypeTable,
) -> bool {
    if contains_prime_field(ty, types) {
        return fold_prime_poly(ty, coeffs, constant, const_map, type_map, types);
    }
    if is_field_type(ty, types) {
        return fold_extension_poly(ty, coeffs, constant, const_map, type_map, types);
    }

    let mut changed = false;
    let old_coeffs = core::mem::take(coeffs);
    // Keep a clone to compare at the end (old_coeffs is consumed by the loop).
    let old_coeffs_for_cmp = old_coeffs.clone();
    let mut new_coeffs = PolyCoeffs::new();

    for (key, coeff) in old_coeffs {
        if coeff & 1 == 0 {
            changed = true;
            continue;
        }

        // A non-idempotent factor is not an AND. Leave that monomial alone.
        let has_field = key.iter().any(|v| {
            type_map
                .get(v)
                .map_or(false, |&tid| !mul_is_idempotent(tid, types))
        });
        if has_field {
            *new_coeffs.entry(key).or_insert(0) ^= coeff;
            continue;
        }

        // Simplify key using const_map.
        let mut monomial_zero = false;
        let mut new_key: Vec<V> = Vec::with_capacity(key.len());

        for v in &key {
            if let Some(&c) = const_map.get(v) {
                let w = type_map
                    .get(v)
                    .and_then(|&tid| type_bit_width(tid, types))
                    .unwrap_or(1);
                let c_masked = mask_constant(c, w);
                if constant_is_zero(c_masked) {
                    // AND with 0 → whole monomial is 0.
                    monomial_zero = true;
                    changed = true;
                    break;
                } else if constant_is_all_ones(c_masked, w) {
                    // AND with all-ones → multiplicative identity, drop var.
                    changed = true;
                } else {
                    // Non-trivial constant — keep conservatively.
                    new_key.push(v.clone());
                }
            } else {
                new_key.push(v.clone());
            }
        }

        if monomial_zero {
            continue;
        }

        new_key.sort();
        let before_len = new_key.len();
        new_key.dedup(); // idempotent AND in GF(2)
        if new_key.len() != before_len || new_key.len() != key.len() {
            changed = true;
        }

        if new_key.is_empty() {
            // Empty monomial (odd coeff) → contribute all-ones to constant.
            let w = type_bit_width(ty, types).unwrap_or(1);
            let all_ones = mask_constant(
                Constant {
                    hi: u128::MAX,
                    lo: u128::MAX,
                },
                w,
            );
            *constant = constant_xor(*constant, all_ones);
            changed = true;
        } else {
            *new_coeffs.entry(new_key).or_insert(0) ^= coeff;
        }
    }

    // Remove zero-coeff entries.
    new_coeffs.retain(|_, c| *c & 1 != 0);

    // Compare against the original (now-moved) coeffs to detect XOR cancellations.
    if new_coeffs != old_coeffs_for_cmp {
        changed = true;
    }
    *coeffs = new_coeffs;

    // Mask constant to output width.
    if let Some(w) = type_bit_width(ty, types) {
        let masked = mask_constant(*constant, w);
        if masked != *constant {
            *constant = masked;
            changed = true;
        }
    }

    changed
}

fn fold_extension_poly<V: Clone + Ord>(
    ty: TypeId,
    coeffs: &mut PolyCoeffs<V>,
    constant: &mut Constant,
    const_map: &BTreeMap<V, Constant>,
    type_map: &BTreeMap<V, TypeId>,
    types: &TypeTable,
) -> bool {
    let Some(width) = type_bit_width(ty, types) else {
        return false;
    };
    let mut changed = false;
    let old_coeffs = core::mem::take(coeffs);
    let old_coeffs_for_cmp = old_coeffs.clone();
    let mut new_coeffs = PolyCoeffs::new();

    for (key, coeff) in old_coeffs {
        if coeff & 1 == 0 {
            changed = true;
            continue;
        }

        let mut monomial_zero = false;
        let mut all_constant = true;
        let mut const_factors: Vec<(TypeId, Vec<bool>)> = Vec::new();
        let mut new_key: Vec<V> = Vec::with_capacity(key.len());

        for v in &key {
            let Some(&factor_ty) = type_map.get(v) else {
                all_constant = false;
                new_key.push(v.clone());
                continue;
            };
            let Some(&c) = const_map.get(v) else {
                all_constant = false;
                new_key.push(v.clone());
                continue;
            };
            let factor_width = type_bit_width(factor_ty, types).unwrap_or(1);
            let masked = mask_constant(c, factor_width);
            if constant_is_zero(masked) {
                monomial_zero = true;
                changed = true;
                break;
            }
            if constant_is_mul_identity(masked, factor_ty, types) {
                changed = true;
                continue;
            }
            const_factors.push((factor_ty, constant_to_bits(masked, factor_width)));
            new_key.push(v.clone());
        }

        if monomial_zero {
            continue;
        }

        if all_constant && width <= 256 {
            let product = eval_constant_product(ty, &const_factors, types);
            *constant = constant_xor(*constant, bits_to_constant(&product));
            changed = true;
            continue;
        }

        new_key.sort();
        let before_len = new_key.len();
        let mut deduped = Vec::with_capacity(new_key.len());
        for v in new_key {
            let idempotent = type_map
                .get(&v)
                .map_or(false, |&tid| mul_is_idempotent(tid, types));
            if idempotent && deduped.last() == Some(&v) {
                continue;
            }
            deduped.push(v);
        }
        if deduped.len() != before_len || deduped.len() != key.len() {
            changed = true;
        }

        if deduped.is_empty() {
            if let Some(identity) = mul_identity(ty, types) {
                *constant = constant_xor(*constant, bits_to_constant(&identity));
            }
            changed = true;
        } else {
            *new_coeffs.entry(deduped).or_insert(0) ^= coeff;
        }
    }

    new_coeffs.retain(|_, c| *c & 1 != 0);
    if new_coeffs != old_coeffs_for_cmp {
        changed = true;
    }
    *coeffs = new_coeffs;

    if let Some(w) = type_bit_width(ty, types) {
        let masked = mask_constant(*constant, w);
        if masked != *constant {
            *constant = masked;
            changed = true;
        }
    }
    changed
}

fn fold_prime_poly<V: Clone + Ord>(
    ty: TypeId,
    coeffs: &mut PolyCoeffs<V>,
    constant: &mut Constant,
    const_map: &BTreeMap<V, Constant>,
    type_map: &BTreeMap<V, TypeId>,
    types: &TypeTable,
) -> bool {
    let Some(width) = type_bit_width(ty, types) else {
        return false;
    };
    let spec = uniform_prime_spec(ty, types);
    let mut changed = false;
    let mut overflow = false;
    let old_constant = *constant;
    let old_coeffs = core::mem::take(coeffs);
    let old_coeffs_for_cmp = old_coeffs.clone();
    let mut new_coeffs = PolyCoeffs::new();

    for (key, coeff) in old_coeffs {
        let stored = match &spec {
            Some(spec) => repetition_residue(coeff, spec),
            None => coeff,
        };
        if stored == 0 {
            changed = true;
            continue;
        }

        let mut monomial_zero = false;
        let mut all_constant = true;
        let mut const_factors: Vec<(TypeId, Vec<bool>)> = Vec::new();
        let mut new_key: Vec<V> = Vec::with_capacity(key.len());

        for v in &key {
            let Some(&factor_ty) = type_map.get(v) else {
                all_constant = false;
                new_key.push(v.clone());
                continue;
            };
            let Some(&c) = const_map.get(v) else {
                all_constant = false;
                new_key.push(v.clone());
                continue;
            };
            let factor_width = type_bit_width(factor_ty, types).unwrap_or(1);
            let masked = mask_constant(c, factor_width);
            if constant_is_zero(masked) {
                monomial_zero = true;
                changed = true;
                break;
            }
            if constant_is_mul_identity(masked, factor_ty, types) {
                changed = true;
                continue;
            }
            const_factors.push((factor_ty, constant_to_bits(masked, factor_width)));
            new_key.push(v.clone());
        }

        if monomial_zero {
            continue;
        }

        if all_constant && width <= 256 {
            let acc = constant_to_bits(*constant, width);
            let terms = vec![(const_factors, stored)];
            let mut sink = BoolRing;
            let reduced = eval_prime_poly(ty, &acc, &terms, types, &mut sink);
            *constant = bits_to_constant(&reduced);
            changed = true;
            continue;
        }

        new_key.sort();
        let before_len = new_key.len();
        let mut deduped = Vec::with_capacity(new_key.len());
        for v in new_key {
            let idempotent = type_map
                .get(&v)
                .map_or(false, |&tid| mul_is_idempotent(tid, types));
            if idempotent && deduped.last() == Some(&v) {
                continue;
            }
            deduped.push(v);
        }
        if deduped.len() != before_len || deduped.len() != key.len() {
            changed = true;
        }

        if deduped.is_empty() && width <= 256 {
            let acc = constant_to_bits(*constant, width);
            let terms = vec![(Vec::new(), stored)];
            let mut sink = BoolRing;
            let reduced = eval_prime_poly(ty, &acc, &terms, types, &mut sink);
            *constant = bits_to_constant(&reduced);
            changed = true;
            continue;
        }

        match new_coeffs.get(&deduped).copied() {
            Some(prev) => match merge_repetition(prev, stored, spec.as_ref()) {
                Some(sum) => {
                    if sum == 0 {
                        new_coeffs.remove(&deduped);
                    } else {
                        new_coeffs.insert(deduped, sum);
                    }
                    changed = true;
                }
                None => overflow = true,
            },
            None => {
                new_coeffs.insert(deduped, stored);
            }
        }
    }

    if overflow {
        *coeffs = old_coeffs_for_cmp;
        *constant = old_constant;
        return false;
    }

    new_coeffs.retain(|_, c| *c != 0);
    if new_coeffs != old_coeffs_for_cmp {
        changed = true;
    }
    *coeffs = new_coeffs;

    if width <= 256 {
        let acc = constant_to_bits(*constant, width);
        let mut sink = BoolRing;
        let reduced = eval_prime_poly(ty, &acc, &[], types, &mut sink);
        let masked = bits_to_constant(&reduced);
        if masked != *constant {
            *constant = masked;
            changed = true;
        }
    }
    changed
}

fn uniform_prime_spec(ty: TypeId, types: &TypeTable) -> Option<PrimeSpec> {
    match types.0.get(ty.0 as usize)? {
        IrType::PrimeField { .. } => prime_spec(ty, types),
        IrType::Vec(_, elem) => uniform_prime_spec(*elem, types),
        IrType::Tuple(parts) => {
            let mut found = None;
            for &part in parts {
                let spec = uniform_prime_spec(part, types)?;
                if let Some(prev) = &found {
                    if prev != &spec {
                        return None;
                    }
                } else {
                    found = Some(spec);
                }
            }
            found
        }
        _ => None,
    }
}

fn merge_repetition(prev: u8, add: u8, spec: Option<&PrimeSpec>) -> Option<u8> {
    match spec {
        Some(spec) if spec.k <= 8 => {
            let n = spec.n.first().copied().unwrap_or(0) as u16;
            let prime = (1u16 << spec.k) - n;
            Some(((prev as u16 + add as u16) % prime) as u8)
        }
        _ => {
            let sum = prev as u16 + add as u16;
            if sum > 255 {
                None
            } else {
                Some(sum as u8)
            }
        }
    }
}

fn constant_is_mul_identity(c: Constant, ty: TypeId, types: &TypeTable) -> bool {
    let Some(identity) = mul_identity(ty, types) else {
        return false;
    };
    constant_to_bits(c, identity.len()) == identity
}

fn constant_to_bits(c: Constant, width: usize) -> Vec<bool> {
    let mut bits = Vec::with_capacity(width);
    for bit in 0..width {
        let set = if bit < 128 {
            (c.lo >> bit) & 1 == 1
        } else if bit < 256 {
            (c.hi >> (bit - 128)) & 1 == 1
        } else {
            false
        };
        bits.push(set);
    }
    bits
}

fn bits_to_constant(bits: &[bool]) -> Constant {
    let mut lo = 0u128;
    let mut hi = 0u128;
    for (bit, set) in bits.iter().enumerate() {
        if !*set {
            continue;
        }
        if bit < 128 {
            lo |= 1u128 << bit;
        } else if bit < 256 {
            hi |= 1u128 << (bit - 128);
        }
    }
    Constant { hi, lo }
}

fn eval_constant_product(
    ty: TypeId,
    factors: &[(TypeId, Vec<bool>)],
    types: &TypeTable,
) -> Vec<bool> {
    let refs: Vec<(TypeId, &[bool])> = factors
        .iter()
        .map(|(factor_ty, bits)| (*factor_ty, bits.as_slice()))
        .collect();
    monomial_product(ty, &refs, types, &mut BoolRing)
}

#[cfg(test)]
mod fold_tests {
    use super::*;
    use alloc::vec;

    fn zero() -> Constant {
        Constant { hi: 0, lo: 0 }
    }

    #[test]
    fn bit_square_collapses_and_field_square_stays() {
        let mut types = TypeTable::new();
        let bit = types.bit();
        let field = types.aes8();
        let mut bit_coeffs = PolyCoeffs::new();
        bit_coeffs.insert(vec![0u32, 0], 1);
        let mut constant = zero();
        let const_map = BTreeMap::new();
        let mut type_map = BTreeMap::new();
        type_map.insert(0u32, bit);
        fold_poly_in_place(
            bit,
            &mut bit_coeffs,
            &mut constant,
            &const_map,
            &type_map,
            &types,
        );
        assert_eq!(bit_coeffs.get(&[0u32]).copied(), Some(1));

        let mut field_coeffs = PolyCoeffs::new();
        field_coeffs.insert(vec![1u32, 1], 1);
        type_map.insert(1u32, field);
        fold_poly_in_place(
            field,
            &mut field_coeffs,
            &mut constant,
            &const_map,
            &type_map,
            &types,
        );
        assert_eq!(field_coeffs.get(&[1u32, 1]).copied(), Some(1));
    }

    #[test]
    fn prime_repetition_adds_and_does_not_cancel_evens() {
        let mut types = TypeTable::new();
        let z3 = types.z3();
        let mut type_map = BTreeMap::new();
        type_map.insert(0u32, z3);
        type_map.insert(1u32, z3);
        let mut const_map = BTreeMap::new();
        const_map.insert(0u32, Constant { hi: 0, lo: 1 });
        let mut coeffs = PolyCoeffs::new();
        // Dropping the constant one makes this the same monomial as `[1]`,
        // so the repetitions add: 1 + 1 = 2 (mod 3), which stays.
        coeffs.insert(vec![0u32, 1], 1);
        coeffs.insert(vec![1u32], 1);
        let mut constant = zero();
        fold_poly_in_place(
            z3,
            &mut coeffs,
            &mut constant,
            &const_map,
            &type_map,
            &types,
        );
        assert_eq!(coeffs.get(&[1u32]).copied(), Some(2));

        let mut doubled = PolyCoeffs::new();
        doubled.insert(vec![1u32], 2);
        fold_poly_in_place(
            z3,
            &mut doubled,
            &mut constant,
            &const_map,
            &type_map,
            &types,
        );
        assert_eq!(doubled.get(&[1u32]).copied(), Some(2));

        let mut cancelled = PolyCoeffs::new();
        cancelled.insert(vec![1u32], 3);
        fold_poly_in_place(
            z3,
            &mut cancelled,
            &mut constant,
            &const_map,
            &type_map,
            &types,
        );
        assert!(cancelled.is_empty());
    }

    #[test]
    fn field_one_drops_and_all_ones_stays() {
        let mut types = TypeTable::new();
        let field = types.aes8();
        let mut type_map = BTreeMap::new();
        type_map.insert(0u32, field);
        type_map.insert(1u32, field);
        let mut const_map = BTreeMap::new();
        const_map.insert(0u32, Constant { hi: 0, lo: 1 });
        let mut coeffs = PolyCoeffs::new();
        coeffs.insert(vec![0u32, 1], 1);
        let mut constant = zero();
        fold_poly_in_place(
            field,
            &mut coeffs,
            &mut constant,
            &const_map,
            &type_map,
            &types,
        );
        assert_eq!(coeffs.get(&[1u32]).copied(), Some(1));
        assert!(coeffs.get(&[0u32, 1]).is_none());

        const_map.insert(0u32, Constant { hi: 0, lo: 0xff });
        let mut coeffs = PolyCoeffs::new();
        coeffs.insert(vec![0u32, 1], 1);
        fold_poly_in_place(
            field,
            &mut coeffs,
            &mut constant,
            &const_map,
            &type_map,
            &types,
        );
        assert_eq!(coeffs.get(&[0u32, 1]).copied(), Some(1));
    }
}
