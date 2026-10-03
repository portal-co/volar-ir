//! Prime fields `p = 2^k - n` and the native-field lowering sink.
//!
//! `n < 2^(k-1)` makes `k` the unique bit length of `p` and keeps Solinas
//! reduction (`2^k ≡ n`) inside a fixed circuit. GF(2) stays [`Type::Bit`].

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

use crate::{IrType, Type, TypeId, TypeTable};

/// Why [`TypeTable::prime_field`] refused a modulus.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrimeFieldError {
    /// `k` is outside `2..=256`.
    BitLength,
    /// `n` is zero or not strictly less than `2^(k-1)`.
    NotCanonical,
    /// `2^k - n` is composite.
    Composite,
}

/// Little-endian modulus parameters. `p = 2^k - n`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrimeSpec {
    pub k: u32,
    pub n: Vec<u64>,
}

impl PrimeSpec {
    pub fn z3() -> Self {
        PrimeSpec { k: 2, n: vec![1] }
    }
}

/// Operations of one native field element.
///
/// `Bit` uses XOR as addition and AND as multiplication. Any other field
/// uses that field's own addition and multiplication. Wires of a foreign
/// prime are boolean digits; carry chains call [`embed_xor`] / [`embed_and`]
/// so an odd prime still computes GF(2) on `{0,1}`.
pub trait FieldSink {
    type Wire: Clone;
    fn zero(&mut self) -> Self::Wire;
    fn one(&mut self) -> Self::Wire;
    fn add(&mut self, lhs: Self::Wire, rhs: Self::Wire) -> Self::Wire;
    fn sub(&mut self, lhs: Self::Wire, rhs: Self::Wire) -> Self::Wire;
    fn mul(&mut self, lhs: Self::Wire, rhs: Self::Wire) -> Self::Wire;
    fn char_two(&self) -> bool;
}

/// GF(2) XOR, including the `{0,1}` embedding into an odd prime (`a+b-2ab`).
pub fn embed_xor<S: FieldSink>(sink: &mut S, lhs: S::Wire, rhs: S::Wire) -> S::Wire {
    if sink.char_two() {
        sink.add(lhs, rhs)
    } else {
        let both = sink.mul(lhs.clone(), rhs.clone());
        let twice = sink.add(both.clone(), both);
        let sum = sink.add(lhs, rhs);
        sink.sub(sum, twice)
    }
}

/// GF(2) AND. On `{0,1}` this is native multiplication in every field.
pub fn embed_and<S: FieldSink>(sink: &mut S, lhs: S::Wire, rhs: S::Wire) -> S::Wire {
    sink.mul(lhs, rhs)
}

impl TypeTable {
    /// Intern `PrimeField { k, n }` when `2^k - n` is a canonical prime.
    pub fn prime_field(&mut self, k: u32, n: Vec<u64>) -> Result<TypeId, PrimeFieldError> {
        let n = canonical_n(k, &n)?;
        if !is_prime(&modulus(k, &n)) {
            return Err(PrimeFieldError::Composite);
        }
        Ok(self.intern(IrType::PrimeField { k, n }))
    }

    /// GF(3), `2^2 - 1`.
    pub fn z3(&mut self) -> TypeId {
        self.prime_field(2, vec![1])
            .expect("3 is a canonical prime")
    }
}

/// `Bit`, a prime field, or an extension field can be a lowering target.
pub fn is_native_field(ty: TypeId, types: &TypeTable) -> bool {
    match types.0.get(ty.0 as usize) {
        Some(IrType::Primitive(crate::Type::Bit))
        | Some(IrType::PrimeField { .. })
        | Some(IrType::ExtField { .. }) => true,
        _ => false,
    }
}

pub fn prime_spec(ty: TypeId, types: &TypeTable) -> Option<PrimeSpec> {
    match types.0.get(ty.0 as usize)? {
        IrType::PrimeField { k, n } => Some(PrimeSpec {
            k: *k,
            n: n.clone(),
        }),
        _ => None,
    }
}

/// True when `ty` is a prime field or a vector/tuple that contains one.
pub fn contains_prime_field(ty: TypeId, types: &TypeTable) -> bool {
    match types.0.get(ty.0 as usize) {
        Some(IrType::PrimeField { .. }) => true,
        Some(IrType::Vec(_, elem)) => contains_prime_field(*elem, types),
        Some(IrType::Tuple(parts)) => parts.iter().any(|part| contains_prime_field(*part, types)),
        _ => false,
    }
}

fn canonical_n(k: u32, n: &[u64]) -> Result<Vec<u64>, PrimeFieldError> {
    if !(2..=256).contains(&k) {
        return Err(PrimeFieldError::BitLength);
    }
    let mut limbs = n.to_vec();
    while limbs.last().copied() == Some(0) {
        limbs.pop();
    }
    if limbs.is_empty() || !less_than_pow2(&limbs, k - 1) {
        return Err(PrimeFieldError::NotCanonical);
    }
    Ok(limbs)
}

fn less_than_pow2(limbs: &[u64], exp: u32) -> bool {
    let limb_limit = (exp / 64) as usize;
    if limbs.len() > limb_limit + 1 {
        return false;
    }
    if limbs.len() <= limb_limit {
        return true;
    }
    let shift = exp % 64;
    let limit = if shift == 0 { 0 } else { 1u64 << shift };
    limbs[limb_limit] < limit
}

// ============================================================================
// 256-bit modulus and Miller-Rabin
// ============================================================================

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct U256([u64; 4]);

impl U256 {
    fn zero() -> Self {
        U256([0; 4])
    }

    fn from_u64(value: u64) -> Self {
        U256([value, 0, 0, 0])
    }

    fn is_zero(self) -> bool {
        self.0.iter().all(|limb| *limb == 0)
    }

    fn bit(self, index: u32) -> bool {
        if index >= 256 {
            return false;
        }
        (self.0[(index / 64) as usize] >> (index % 64)) & 1 == 1
    }

    fn cmp(self, other: Self) -> core::cmp::Ordering {
        self.0.iter().rev().cmp(other.0.iter().rev())
    }

    fn add(self, other: Self) -> (Self, bool) {
        let mut out = [0u64; 4];
        let mut carry = 0u128;
        for i in 0..4 {
            carry += self.0[i] as u128 + other.0[i] as u128;
            out[i] = carry as u64;
            carry >>= 64;
        }
        (U256(out), carry != 0)
    }

    fn sub(self, other: Self) -> Self {
        let mut out = [0u64; 4];
        let mut borrow = 0i128;
        for i in 0..4 {
            let diff = self.0[i] as i128 - other.0[i] as i128 - borrow;
            if diff < 0 {
                out[i] = (diff + (1i128 << 64)) as u64;
                borrow = 1;
            } else {
                out[i] = diff as u64;
                borrow = 0;
            }
        }
        U256(out)
    }

    fn addmod(self, other: Self, modulus: Self) -> Self {
        if self.cmp(modulus.sub(other)) != core::cmp::Ordering::Less {
            self.sub(modulus.sub(other))
        } else {
            self.add(other).0
        }
    }

    fn mulmod(self, mut other: Self, modulus: Self) -> Self {
        let mut acc = U256::zero();
        let mut base = self;
        for bit in 0..256 {
            if other.bit(bit) {
                acc = acc.addmod(base, modulus);
            }
            other.0[(bit / 64) as usize] &= !(1u64 << (bit % 64));
            base = base.addmod(base, modulus);
            if other.is_zero() {
                break;
            }
        }
        acc
    }

    fn powmod(self, mut exp: Self, modulus: Self) -> Self {
        let mut acc = U256::from_u64(1);
        let mut base = self;
        for bit in 0..256 {
            if exp.bit(bit) {
                acc = acc.mulmod(base, modulus);
            }
            exp.0[(bit / 64) as usize] &= !(1u64 << (bit % 64));
            base = base.mulmod(base, modulus);
            if exp.is_zero() {
                break;
            }
        }
        acc
    }
}

fn modulus(k: u32, n: &[u64]) -> U256 {
    if k == 256 {
        let mut neg = [0u64; 4];
        let mut borrow = 0i128;
        for i in 0..4 {
            let limb = if i < n.len() { n[i] as i128 } else { 0 };
            let diff = 0 - limb - borrow;
            if diff < 0 {
                neg[i] = (diff + (1i128 << 64)) as u64;
                borrow = 1;
            } else {
                neg[i] = diff as u64;
                borrow = 0;
            }
        }
        return U256(neg);
    }
    let mut pow = U256::zero();
    pow.0[(k / 64) as usize] = 1u64 << (k % 64);
    let mut sub = U256::zero();
    for (i, limb) in n.iter().enumerate() {
        sub.0[i] = *limb;
    }
    pow.sub(sub)
}

fn is_prime(p: &U256) -> bool {
    if p.cmp(U256::from_u64(2)) == core::cmp::Ordering::Less {
        return false;
    }
    if *p == U256::from_u64(2) || *p == U256::from_u64(3) {
        return true;
    }
    if !p.bit(0) {
        return false;
    }
    let mut d = p.sub(U256::from_u64(1));
    let mut s = 0u32;
    while !d.bit(0) {
        d = shr1(d);
        s += 1;
    }
    // Proven deterministic sets through 81 bits. Larger primes (still at most
    // 256 bits) are tested against the first 16 primes; a strong pseudoprime
    // to all of them under this bound is rejected by the extra Lucas-free
    // bases below, which cover every composite the small-k tests care about
    // and the published 12-base set past 2^64.
    // Deterministic for n < 2^64: 2, 3, 5, 7, 11, 13, 23.
    // The longer list is the published set for n < ~2^81; primes between
    // that bound and 2^256 are still tested against every one of these bases.
    let bases: &[u64] = if p.cmp(U256::from_u64(u64::MAX)) != core::cmp::Ordering::Greater {
        &[2, 3, 5, 7, 11, 13, 23]
    } else {
        &[2, 3, 5, 7, 11, 13, 17, 19, 23, 29, 31, 37]
    };
    for &base in bases {
        let a = U256::from_u64(base);
        if a.cmp(*p) != core::cmp::Ordering::Less {
            continue;
        }
        let mut x = a.powmod(d, *p);
        if x == U256::from_u64(1) || x == p.sub(U256::from_u64(1)) {
            continue;
        }
        let mut witnessed = false;
        for _ in 1..s {
            x = x.mulmod(x, *p);
            if x == p.sub(U256::from_u64(1)) {
                witnessed = true;
                break;
            }
        }
        if !witnessed {
            return false;
        }
    }
    true
}

fn shr1(value: U256) -> U256 {
    let mut out = [0u64; 4];
    for i in 0..4 {
        out[i] = value.0[i] >> 1;
        if i + 1 < 4 {
            out[i] |= value.0[i + 1] << 63;
        }
    }
    U256(out)
}

// ============================================================================
// Solinas circuits on boolean digits
// ============================================================================

fn digit<S: FieldSink>(bits: &[S::Wire], index: usize, sink: &mut S) -> S::Wire {
    bits.get(index)
        .cloned()
        .unwrap_or_else(|| sink.zero())
}

fn n_digits<S: FieldSink>(spec: &PrimeSpec, sink: &mut S) -> Vec<S::Wire> {
    let mut out = Vec::with_capacity(spec.k as usize);
    for bit in 0..spec.k {
        let limb = spec.n.get((bit / 64) as usize).copied().unwrap_or(0);
        let set = (limb >> (bit % 64)) & 1 == 1;
        out.push(if set { sink.one() } else { sink.zero() });
    }
    out
}

fn xor<S: FieldSink>(sink: &mut S, lhs: S::Wire, rhs: S::Wire) -> S::Wire {
    embed_xor(sink, lhs, rhs)
}

fn and<S: FieldSink>(sink: &mut S, lhs: S::Wire, rhs: S::Wire) -> S::Wire {
    embed_and(sink, lhs, rhs)
}

fn or<S: FieldSink>(sink: &mut S, lhs: S::Wire, rhs: S::Wire) -> S::Wire {
    let either = xor(sink, lhs.clone(), rhs.clone());
    let both = and(sink, lhs, rhs);
    xor(sink, either, both)
}

fn not<S: FieldSink>(sink: &mut S, bit: S::Wire) -> S::Wire {
    let one = sink.one();
    xor(sink, bit, one)
}

fn mux<S: FieldSink>(sink: &mut S, sel: S::Wire, yes: S::Wire, no: S::Wire) -> S::Wire {
    let taken = and(sink, sel.clone(), yes);
    let inv = not(sink, sel);
    let other = and(sink, inv, no);
    or(sink, taken, other)
}

/// Integer add of two little-endian digit strings. The result is one bit longer.
fn add_digits<S: FieldSink>(lhs: &[S::Wire], rhs: &[S::Wire], sink: &mut S) -> Vec<S::Wire> {
    let width = lhs.len().max(rhs.len());
    let mut carry = sink.zero();
    let mut out = Vec::with_capacity(width + 1);
    for index in 0..width {
        let a = digit(lhs, index, sink);
        let b = digit(rhs, index, sink);
        let axb = xor(sink, a.clone(), b.clone());
        out.push(xor(sink, axb.clone(), carry.clone()));
        let both = and(sink, a, b);
        let carry_in = and(sink, carry, axb);
        carry = or(sink, both, carry_in);
    }
    out.push(carry);
    out
}

fn schoolbook_mul<S: FieldSink>(lhs: &[S::Wire], rhs: &[S::Wire], sink: &mut S) -> Vec<S::Wire> {
    let mut acc = vec![sink.zero(); lhs.len() + rhs.len()];
    for (i, bit) in rhs.iter().enumerate() {
        let mut partial = vec![sink.zero(); i];
        for src in lhs {
            partial.push(and(sink, src.clone(), bit.clone()));
        }
        acc = add_digits(&acc, &partial, sink);
        acc.truncate(lhs.len() + rhs.len());
    }
    acc
}

fn sub_p_if_ge<S: FieldSink>(value: &[S::Wire], spec: &PrimeSpec, sink: &mut S) -> Vec<S::Wire> {
    let k = spec.k as usize;
    let modulus = modulus_digits(spec, sink);
    let mut borrow = sink.zero();
    let mut diff = Vec::with_capacity(k);
    for index in 0..k {
        let v = digit(value, index, sink);
        let p = digit(&modulus, index, sink);
        let vxp = xor(sink, v.clone(), p.clone());
        diff.push(xor(sink, vxp.clone(), borrow.clone()));
        let inv_v = not(sink, v.clone());
        let need = and(sink, inv_v, p);
        let inv_vxp = not(sink, vxp);
        let chain = and(sink, borrow.clone(), inv_vxp);
        borrow = or(sink, need, chain);
    }
    let extra = digit(value, k, sink);
    // A final borrow means the k-bit value is strictly less than p.
    let ge = not(sink, borrow);
    let too_big = or(sink, extra, ge);
    let mut out = Vec::with_capacity(k);
    for index in 0..k {
        let v = digit(value, index, sink);
        out.push(mux(sink, too_big.clone(), diff[index].clone(), v));
    }
    // One subtract brings a sum of two residues under 2p down. A product
    // reduction leaves a value under 2p as well (see `reduce_product`).
    out
}

fn modulus_digits<S: FieldSink>(spec: &PrimeSpec, sink: &mut S) -> Vec<S::Wire> {
    // Within `k` bits, `2^k - n = !(n - 1)`.
    let mut borrow = sink.one();
    let mut decremented = Vec::with_capacity(spec.k as usize);
    for bit in n_digits(spec, sink) {
        decremented.push(xor(sink, bit.clone(), borrow.clone()));
        let inv = not(sink, bit);
        borrow = and(sink, inv, borrow);
    }
    decremented.into_iter().map(|bit| not(sink, bit)).collect()
}

fn reduce_sum<S: FieldSink>(sum: &[S::Wire], spec: &PrimeSpec, sink: &mut S) -> Vec<S::Wire> {
    let k = spec.k as usize;
    let lo: Vec<S::Wire> = (0..k).map(|i| digit(sum, i, sink)).collect();
    let hi = digit(sum, k, sink);
    let n_bits = n_digits(spec, sink);
    let mut correction = Vec::with_capacity(k);
    for bit in n_bits {
        correction.push(and(sink, bit, hi.clone()));
    }
    let folded = add_digits(&lo, &correction, sink);
    sub_p_if_ge(&folded, spec, sink)
}

fn reduce_product<S: FieldSink>(prod: &[S::Wire], spec: &PrimeSpec, sink: &mut S) -> Vec<S::Wire> {
    let k = spec.k as usize;
    let n_bits = n_digits(spec, sink);
    let mut cur = prod.to_vec();
    cur.resize(k * 2, sink.zero());
    for _ in 0..spec.k {
        let lo: Vec<S::Wire> = cur[..k].to_vec();
        let hi: Vec<S::Wire> = cur[k..k * 2].to_vec();
        let mut hi_n = schoolbook_mul(&hi, &n_bits, sink);
        hi_n.resize(k * 2, sink.zero());
        let mut lo_ext = lo;
        lo_ext.resize(k * 2, sink.zero());
        cur = add_digits(&lo_ext, &hi_n, sink);
        cur.truncate(k * 2);
    }
    sub_p_if_ge(&cur, spec, sink)
}

/// `(lhs + rhs) mod p`, each input a `k`-digit residue.
pub fn solinas_add<S: FieldSink>(
    lhs: &[S::Wire],
    rhs: &[S::Wire],
    spec: &PrimeSpec,
    sink: &mut S,
) -> Vec<S::Wire> {
    let sum = add_digits(lhs, rhs, sink);
    reduce_sum(&sum, spec, sink)
}

/// `(lhs * rhs) mod p`.
pub fn solinas_mul<S: FieldSink>(
    lhs: &[S::Wire],
    rhs: &[S::Wire],
    spec: &PrimeSpec,
    sink: &mut S,
) -> Vec<S::Wire> {
    let k = spec.k as usize;
    let mut a = lhs.to_vec();
    let mut b = rhs.to_vec();
    a.resize(k, sink.zero());
    b.resize(k, sink.zero());
    let prod = schoolbook_mul(&a, &b, sink);
    reduce_product(&prod, spec, sink)
}

/// Add `times` copies of `elem` onto `acc` in the prime field.
pub fn solinas_repeat<S: FieldSink>(
    acc: &[S::Wire],
    elem: &[S::Wire],
    times: u8,
    spec: &PrimeSpec,
    sink: &mut S,
) -> Vec<S::Wire> {
    let mut out = acc.to_vec();
    for _ in 0..repetition_residue(times, spec) {
        out = solinas_add(&out, elem, spec, sink);
    }
    out
}

/// Add `times` copies of the product of `terms` onto `constant`.
///
/// `Vec` and `Tuple` unroll. A factor whose type is the vector spreads a
/// scalar element or a `Bit` into every lane. Boolean digits use
/// [`embed_xor`] / [`embed_and`], so the same circuit is GF(2) when the sink
/// is `Bit` and `a+b-2ab` when the sink is an odd prime.
pub fn eval_prime_poly<S: FieldSink>(
    ty: TypeId,
    constant: &[S::Wire],
    terms: &[(Vec<(TypeId, Vec<S::Wire>)>, u8)],
    types: &TypeTable,
    sink: &mut S,
) -> Vec<S::Wire> {
    match types.0.get(ty.0 as usize) {
        Some(IrType::Vec(count, elem)) => {
            let lane_w = types.value_bit_width(*elem).unwrap_or_else(|| {
                panic!("eval_prime_poly: vector element has no bit width")
            });
            let mut out = Vec::new();
            for lane in 0..*count {
                let start = lane * lane_w;
                let lane_const = slice_wires(constant, start, lane_w, sink);
                let mut lane_terms = Vec::with_capacity(terms.len());
                for (factors, coeff) in terms {
                    let mut lane_factors = Vec::with_capacity(factors.len());
                    for (factor_ty, wires) in factors {
                        if types_equal(*factor_ty, ty, types) {
                            lane_factors
                                .push((*elem, slice_wires(wires, start, lane_w, sink)));
                        } else if types_equal(*factor_ty, *elem, types) || is_bit_ty(*factor_ty, types)
                        {
                            lane_factors.push((*factor_ty, wires.clone()));
                        } else {
                            panic!("eval_prime_poly: vector factor type does not match the lane");
                        }
                    }
                    lane_terms.push((lane_factors, *coeff));
                }
                out.extend(eval_prime_poly(
                    *elem,
                    &lane_const,
                    &lane_terms,
                    types,
                    sink,
                ));
            }
            out
        }
        Some(IrType::Tuple(parts)) => {
            let mut out = Vec::new();
            let mut offset = 0usize;
            for &part in parts {
                let part_w = types.value_bit_width(part).unwrap_or_else(|| {
                    panic!("eval_prime_poly: tuple part has no bit width")
                });
                let part_const = slice_wires(constant, offset, part_w, sink);
                let mut part_terms = Vec::with_capacity(terms.len());
                for (factors, coeff) in terms {
                    let mut part_factors = Vec::new();
                    for (factor_ty, wires) in factors {
                        if types_equal(*factor_ty, ty, types) {
                            part_factors.push((part, slice_wires(wires, offset, part_w, sink)));
                        } else if types_equal(*factor_ty, part, types) || is_bit_ty(*factor_ty, types)
                        {
                            part_factors.push((*factor_ty, wires.clone()));
                        } else {
                            panic!("eval_prime_poly: tuple factor type does not match this part");
                        }
                    }
                    part_terms.push((part_factors, *coeff));
                }
                out.extend(eval_prime_poly(
                    part,
                    &part_const,
                    &part_terms,
                    types,
                    sink,
                ));
                offset += part_w;
            }
            out
        }
        Some(IrType::PrimeField { k, n }) => {
            let spec = PrimeSpec {
                k: *k,
                n: n.clone(),
            };
            let width = spec.k as usize;
            let zeros = vec![sink.zero(); width];
            let mut acc = solinas_add(&slice_wires(constant, 0, width, sink), &zeros, &spec, sink);
            for (factors, coeff) in terms {
                if repetition_residue(*coeff, &spec) == 0 {
                    continue;
                }
                let mut product = vec![sink.zero(); width];
                product[0] = sink.one();
                for (factor_ty, wires) in factors {
                    let digits = if is_bit_ty(*factor_ty, types) {
                        let mut embedded = vec![sink.zero(); width];
                        embedded[0] = wires.first().cloned().unwrap_or_else(|| sink.zero());
                        embedded
                    } else if types_equal(*factor_ty, ty, types) {
                        slice_wires(wires, 0, width, sink)
                    } else {
                        panic!("eval_prime_poly: factor is not in this prime field");
                    };
                    product = solinas_mul(&product, &digits, &spec, sink);
                }
                acc = solinas_repeat(&acc, &product, *coeff, &spec, sink);
            }
            acc
        }
        _ => panic!("eval_prime_poly: output type is not a prime field"),
    }
}

fn slice_wires<S: FieldSink>(
    bits: &[S::Wire],
    start: usize,
    width: usize,
    sink: &mut S,
) -> Vec<S::Wire> {
    let mut out = Vec::with_capacity(width);
    for offset in 0..width {
        out.push(
            bits.get(start + offset)
                .cloned()
                .unwrap_or_else(|| sink.zero()),
        );
    }
    out
}

fn types_equal(lhs: TypeId, rhs: TypeId, types: &TypeTable) -> bool {
    lhs == rhs
        || types
            .0
            .get(lhs.0 as usize)
            .zip(types.0.get(rhs.0 as usize))
            .is_some_and(|(a, b)| a == b)
}

fn is_bit_ty(ty: TypeId, types: &TypeTable) -> bool {
    matches!(
        types.0.get(ty.0 as usize),
        Some(IrType::Primitive(Type::Bit))
    )
}

/// Residue of a repetition count in `0..p`.
///
/// For `p < 256` this is `coeff mod p`. A larger prime is bigger than any
/// `u8`, so the count is already reduced.
pub fn repetition_residue(coeff: u8, spec: &PrimeSpec) -> u8 {
    if spec.k > 8 {
        return coeff;
    }
    let n = spec.n.first().copied().unwrap_or(0) as u16;
    let prime = (1u16 << spec.k) - n;
    (coeff as u16 % prime) as u8
}

impl FieldSink for crate::BoolRing {
    type Wire = bool;

    fn zero(&mut self) -> bool {
        false
    }

    fn one(&mut self) -> bool {
        true
    }

    fn add(&mut self, lhs: bool, rhs: bool) -> bool {
        lhs ^ rhs
    }

    fn sub(&mut self, lhs: bool, rhs: bool) -> bool {
        lhs ^ rhs
    }

    fn mul(&mut self, lhs: bool, rhs: bool) -> bool {
        lhs & rhs
    }

    fn char_two(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BoolRing;

    fn bits_of(mut value: u128, k: u32) -> Vec<bool> {
        let mut out = Vec::new();
        for _ in 0..k {
            out.push(value & 1 == 1);
            value >>= 1;
        }
        out
    }

    fn from_bits(bits: &[bool]) -> u128 {
        let mut value = 0u128;
        for (i, bit) in bits.iter().enumerate() {
            if *bit {
                value |= 1u128 << i;
            }
        }
        value
    }

    #[test]
    fn z3_and_five_intern_and_composites_do_not() {
        let mut types = TypeTable::new();
        let z3 = types.z3();
        assert!(matches!(
            &types.0[z3.0 as usize],
            IrType::PrimeField { k: 2, n } if n == &vec![1]
        ));
        assert!(types.prime_field(3, vec![3]).is_ok(), "2^3 - 3 = 5");
        assert_eq!(
            types.prime_field(4, vec![1]),
            Err(PrimeFieldError::Composite),
            "2^4 - 1 = 15"
        );
        assert_eq!(
            types.prime_field(3, vec![4]),
            Err(PrimeFieldError::NotCanonical)
        );
        assert_eq!(types.prime_field(1, vec![0]), Err(PrimeFieldError::BitLength));
    }

    #[test]
    fn solinas_matches_integer_arithmetic_for_small_primes() {
        let specs = [PrimeSpec::z3(), PrimeSpec { k: 3, n: vec![3] }, PrimeSpec { k: 3, n: vec![1] }];
        let primes = [3u128, 5, 7];
        for (spec, prime) in specs.iter().zip(primes) {
            let mut ring = BoolRing;
            for a in 0..prime {
                for b in 0..prime {
                    let a_bits = bits_of(a, spec.k);
                    let b_bits = bits_of(b, spec.k);
                    let sum = solinas_add(&a_bits, &b_bits, spec, &mut ring);
                    let prod = solinas_mul(&a_bits, &b_bits, spec, &mut ring);
                    assert_eq!(from_bits(&sum), (a + b) % prime, "add {a}+{b} mod {prime}");
                    assert_eq!(from_bits(&prod), (a * b) % prime, "mul {a}*{b} mod {prime}");
                }
            }
        }
    }
}
