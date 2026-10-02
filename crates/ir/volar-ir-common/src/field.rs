//! Extension fields, their imported-symbol spelling, and the type-directed
//! `Poly` product.
//!
//! A `Poly` is still a sum of monomials. The product inside one monomial
//! follows the output type and recurses: bits multiply by AND, integer
//! primitives and `Vec`s map that product across lanes (spreading a scalar),
//! and an `ExtField` multiplies in the polynomial ring of its wrapped field.

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use crate::{Constant, IrType, PolyCoeffs, Stmt, Type, TypeId, TypeTable};

/// Why [`TypeTable::ext_field`] refused a polynomial.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExtFieldError {
    /// `wrapped` is not `Bit` or another extension field.
    WrappedNotAField,
    /// Extension degree must be at least 2.
    DegreeTooSmall,
    /// A coefficient is wider than 64 bits, so it does not fit in `u64`.
    CoefficientTooWide { width: usize },
    /// `irreducible.len()` is not `degree + 1`.
    BadLength { expected: usize, got: usize },
    /// The leading coefficient is not 1.
    NotMonic,
    /// A coefficient does not fit in the wrapped field.
    CoefficientOutOfRange { index: usize },
    /// The polynomial is reducible over the wrapped field.
    Reducible,
}

/// `x^8 + x^4 + x^3 + x + 1`, low coefficient first, including the leading 1.
pub fn aes8_irreducible() -> Vec<u64> {
    alloc::vec![1, 1, 0, 1, 1, 0, 0, 0, 1]
}

/// `x^64 + x^4 + x^3 + x + 1`, low coefficient first, including the leading 1.
pub fn galois64_irreducible() -> Vec<u64> {
    let mut coeffs = alloc::vec![0u64; 65];
    coeffs[0] = 1;
    coeffs[1] = 1;
    coeffs[3] = 1;
    coeffs[4] = 1;
    coeffs[64] = 1;
    coeffs
}

/// Build an `ExtField` value. Does not check irreducibility; interning does.
pub fn ext_field_type(wrapped: TypeId, degree: u32, irreducible: Vec<u64>) -> IrType {
    IrType::ExtField {
        wrapped,
        degree,
        irreducible,
    }
}

impl TypeTable {
    /// Intern `ExtField { wrapped, degree, irreducible }` after checking the
    /// polynomial is a monic irreducible over `wrapped`.
    pub fn ext_field(
        &mut self,
        wrapped: TypeId,
        degree: u32,
        irreducible: Vec<u64>,
    ) -> Result<TypeId, ExtFieldError> {
        validate_ext_field(self, wrapped, degree, &irreducible)?;
        Ok(self.intern(ext_field_type(wrapped, degree, irreducible)))
    }

    /// GF(2^8) under the AES polynomial.
    pub fn aes8(&mut self) -> TypeId {
        let bit = self.bit();
        self.ext_field(bit, 8, aes8_irreducible())
            .expect("the AES polynomial is irreducible")
    }

    /// GF(2^64) under `x^64 + x^4 + x^3 + x + 1`.
    pub fn galois64(&mut self) -> TypeId {
        let bit = self.bit();
        self.ext_field(bit, 64, galois64_irreducible())
            .expect("the canonical degree-64 polynomial is irreducible")
    }

    /// Bit width of an interned type. `Block` and `Func` have no value width.
    pub fn value_bit_width(&self, id: TypeId) -> Option<usize> {
        value_bit_width_of(self.0.get(id.0 as usize)?, self)
    }
}

fn value_bit_width_of(ty: &IrType, types: &TypeTable) -> Option<usize> {
    match ty {
        IrType::Primitive(t) => primitive_bit_width(*t),
        IrType::Vec(n, elem) => n.checked_mul(types.value_bit_width(*elem)?),
        IrType::Tuple(parts) => {
            let mut sum = 0usize;
            for &part in parts {
                sum = sum.checked_add(types.value_bit_width(part)?)?;
            }
            Some(sum)
        }
        IrType::ExtField {
            wrapped, degree, ..
        } => (*degree as usize).checked_mul(types.value_bit_width(*wrapped)?),
        IrType::Block { .. } | IrType::Func { .. } => None,
    }
}

/// Bit width of a primitive. `Z3` is not a GF(2) bit vector.
pub fn primitive_bit_width(ty: Type) -> Option<usize> {
    Some(match ty {
        Type::Bit => 1,
        Type::_8 => 8,
        Type::_16 => 16,
        Type::_32 => 32,
        Type::_64 => 64,
        Type::_128 => 128,
        Type::_256 => 256,
        Type::Z3 => return None,
    })
}

/// `x * x = x` under this type's `Poly` product.
///
/// True for bits, integer primitives, and vectors of those. False for an
/// extension field, including a vector whose element is one.
pub fn mul_is_idempotent(ty: TypeId, types: &TypeTable) -> bool {
    match types.0.get(ty.0 as usize) {
        Some(IrType::Primitive(Type::Z3)) | Some(IrType::Block { .. }) | Some(IrType::Func { .. }) => {
            false
        }
        Some(IrType::Primitive(_)) => true,
        Some(IrType::Vec(_, elem)) => mul_is_idempotent(*elem, types),
        Some(IrType::Tuple(parts)) => parts.iter().all(|p| mul_is_idempotent(*p, types)),
        Some(IrType::ExtField { .. }) => false,
        None => false,
    }
}

/// True when `ty` is an extension field or a vector/tuple that contains one.
pub fn contains_ext_field(ty: TypeId, types: &TypeTable) -> bool {
    match types.0.get(ty.0 as usize) {
        Some(IrType::ExtField { .. }) => true,
        Some(IrType::Vec(_, elem)) => contains_ext_field(*elem, types),
        Some(IrType::Tuple(parts)) => parts.iter().any(|p| contains_ext_field(*p, types)),
        _ => false,
    }
}

fn validate_ext_field(
    types: &TypeTable,
    wrapped: TypeId,
    degree: u32,
    irreducible: &[u64],
) -> Result<(), ExtFieldError> {
    if degree < 2 {
        return Err(ExtFieldError::DegreeTooSmall);
    }
    let wrapped_desc = match types.0.get(wrapped.0 as usize) {
        Some(IrType::Primitive(Type::Bit)) => WrappedDesc::Bit,
        Some(IrType::ExtField { .. }) => {
            WrappedDesc::Field(alloc::boxed::Box::new(field_desc(wrapped, types).map_err(
                |_| ExtFieldError::WrappedNotAField,
            )?))
        }
        _ => return Err(ExtFieldError::WrappedNotAField),
    };
    let coeff_width = wrapped_desc.width();
    if coeff_width == 0 || coeff_width > 64 {
        return Err(ExtFieldError::CoefficientTooWide {
            width: coeff_width,
        });
    }
    let expected = degree as usize + 1;
    if irreducible.len() != expected {
        return Err(ExtFieldError::BadLength {
            expected,
            got: irreducible.len(),
        });
    }
    if irreducible[degree as usize] != 1 {
        return Err(ExtFieldError::NotMonic);
    }
    let limit = if coeff_width == 64 {
        u64::MAX
    } else {
        (1u64 << coeff_width) - 1
    };
    for (index, &coeff) in irreducible.iter().enumerate().take(degree as usize) {
        if coeff > limit {
            return Err(ExtFieldError::CoefficientOutOfRange { index });
        }
    }
    let desc = FieldDesc {
        degree,
        irreducible: irreducible.to_vec(),
        wrapped: wrapped_desc,
        width: (degree as usize)
            .checked_mul(coeff_width)
            .expect("extension width fits in usize"),
    };
    if !is_irreducible(&desc) {
        return Err(ExtFieldError::Reducible);
    }
    Ok(())
}

#[derive(Clone)]
struct FieldDesc {
    degree: u32,
    irreducible: Vec<u64>,
    wrapped: WrappedDesc,
    width: usize,
}

#[derive(Clone)]
enum WrappedDesc {
    Bit,
    Field(alloc::boxed::Box<FieldDesc>),
}

impl WrappedDesc {
    fn width(&self) -> usize {
        match self {
            WrappedDesc::Bit => 1,
            WrappedDesc::Field(field) => field.width,
        }
    }
}

fn field_desc(id: TypeId, types: &TypeTable) -> Result<FieldDesc, ()> {
    match types.0.get(id.0 as usize) {
        Some(IrType::ExtField {
            wrapped,
            degree,
            irreducible,
        }) => {
            let wrapped_desc = match types.0.get(wrapped.0 as usize) {
                Some(IrType::Primitive(Type::Bit)) => WrappedDesc::Bit,
                Some(IrType::ExtField { .. }) => {
                    WrappedDesc::Field(alloc::boxed::Box::new(field_desc(*wrapped, types)?))
                }
                _ => return Err(()),
            };
            let coeff_width = wrapped_desc.width();
            Ok(FieldDesc {
                degree: *degree,
                irreducible: irreducible.clone(),
                wrapped: wrapped_desc,
                width: (*degree as usize) * coeff_width,
            })
        }
        _ => Err(()),
    }
}

/// Carrier of AND/XOR used by the type-directed product.
pub trait BitRing {
    type Bit: Clone;
    fn bit_and(&mut self, lhs: Self::Bit, rhs: Self::Bit) -> Self::Bit;
    fn bit_xor(&mut self, lhs: Self::Bit, rhs: Self::Bit) -> Self::Bit;
    fn bit_zero(&mut self) -> Self::Bit;
    fn bit_one(&mut self) -> Self::Bit;
}

/// Boolean ring. Evaluation and constant folding use this directly.
pub struct BoolRing;

impl BitRing for BoolRing {
    type Bit = bool;

    fn bit_and(&mut self, lhs: bool, rhs: bool) -> bool {
        lhs & rhs
    }

    fn bit_xor(&mut self, lhs: bool, rhs: bool) -> bool {
        lhs ^ rhs
    }

    fn bit_zero(&mut self) -> bool {
        false
    }

    fn bit_one(&mut self) -> bool {
        true
    }
}

fn zeros<R: BitRing>(n: usize, ops: &mut R) -> Vec<R::Bit> {
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        out.push(ops.bit_zero());
    }
    out
}

fn elem_is_zero(elem: &[bool]) -> bool {
    elem.iter().all(|bit| !*bit)
}

/// Multiplicative identity of `ty` as an LSB-first bit vector.
pub fn mul_identity(ty: TypeId, types: &TypeTable) -> Option<Vec<bool>> {
    let mut ops = BoolRing;
    Some(identity_bits(ty, types, &mut ops)?)
}

fn identity_bits<R: BitRing>(ty: TypeId, types: &TypeTable, ops: &mut R) -> Option<Vec<R::Bit>> {
    match types.0.get(ty.0 as usize)? {
        IrType::Primitive(Type::Bit) => Some(alloc::vec![ops.bit_one()]),
        IrType::Primitive(prim) => {
            let width = primitive_bit_width(*prim)?;
            let one = ops.bit_one();
            Some(alloc::vec![one; width])
        }
        IrType::Vec(n, elem) => {
            let lane = identity_bits(*elem, types, ops)?;
            let mut out = Vec::with_capacity(n * lane.len());
            for _ in 0..*n {
                out.extend(lane.iter().cloned());
            }
            Some(out)
        }
        IrType::ExtField { .. } => {
            let width = types.value_bit_width(ty)?;
            let mut bits = zeros(width, ops);
            if width > 0 {
                bits[0] = ops.bit_one();
            }
            Some(bits)
        }
        IrType::Tuple(parts) => {
            let mut out = Vec::new();
            for &part in parts {
                out.extend(identity_bits(part, types, ops)?);
            }
            Some(out)
        }
        IrType::Block { .. } | IrType::Func { .. } => None,
    }
}

/// Product of `factors` in the ring of `out_ty`. An empty factor list is the
/// multiplicative identity.
pub fn monomial_product<R: BitRing>(
    out_ty: TypeId,
    factors: &[(TypeId, &[R::Bit])],
    types: &TypeTable,
    ops: &mut R,
) -> Vec<R::Bit> {
    match types.0.get(out_ty.0 as usize) {
        Some(IrType::Primitive(Type::Bit)) => bit_product(factors, ops),
        Some(IrType::Primitive(prim)) => {
            let width = primitive_bit_width(*prim).unwrap_or_else(|| {
                panic!("monomial_product: {prim:?} has no GF(2) bit width")
            });
            lane_product(width, factors, ops)
        }
        Some(IrType::Vec(n, elem)) => {
            let lane_width = types.value_bit_width(*elem).unwrap_or_else(|| {
                panic!("monomial_product: vector element has no bit width")
            });
            let mut out = Vec::with_capacity(*n * lane_width);
            for lane in 0..*n {
                let mut lane_factors: Vec<(TypeId, Vec<R::Bit>)> = Vec::with_capacity(factors.len());
                let mut lane_refs: Vec<(TypeId, &[R::Bit])> = Vec::with_capacity(factors.len());
                for &(factor_ty, bits) in factors {
                    if same_type(factor_ty, out_ty, types) {
                        let start = lane * lane_width;
                        let slice = slice_or_pad(bits, start, lane_width, ops);
                        lane_factors.push(((*elem), slice));
                    } else if same_type(factor_ty, *elem, types)
                        || is_bit(factor_ty, types)
                    {
                        lane_refs.push((factor_ty, bits));
                    } else {
                        panic!("monomial_product: vector factor type does not match the lane");
                    }
                }
                // The owned lane slices must outlive the reference list.
                let owned_refs: Vec<(TypeId, &[R::Bit])> = lane_factors
                    .iter()
                    .map(|(ty, bits)| (*ty, bits.as_slice()))
                    .chain(lane_refs)
                    .collect();
                out.extend(monomial_product(*elem, &owned_refs, types, ops));
            }
            out
        }
        Some(IrType::ExtField { .. }) => {
            let desc = field_desc(out_ty, types)
                .unwrap_or_else(|_| panic!("monomial_product: bad extension field"));
            let mut acc = identity_bits(out_ty, types, ops)
                .unwrap_or_else(|| panic!("monomial_product: extension field has no identity"));
            if factors.is_empty() {
                return acc;
            }
            for &(factor_ty, bits) in factors {
                if is_bit(factor_ty, types) {
                    let sel = bits.first().cloned().unwrap_or_else(|| ops.bit_zero());
                    acc = acc
                        .into_iter()
                        .map(|bit| ops.bit_and(bit, sel.clone()))
                        .collect();
                } else if same_type(factor_ty, out_ty, types) {
                    acc = field_mul_desc(&acc, bits, &desc, ops);
                } else if wrapped_type_matches(factor_ty, out_ty, types) {
                    let mut embedded = zeros(desc.width, ops);
                    let coeff_width = desc.wrapped.width();
                    for (index, bit) in bits.iter().take(coeff_width).cloned().enumerate() {
                        embedded[index] = bit;
                    }
                    acc = field_mul_desc(&acc, &embedded, &desc, ops);
                } else {
                    panic!("monomial_product: factor type is not in this extension field");
                }
            }
            acc
        }
        Some(IrType::Tuple(_)) | Some(IrType::Block { .. }) | Some(IrType::Func { .. }) | None => {
            panic!("monomial_product: output type cannot carry a polynomial")
        }
    }
}

fn bit_product<R: BitRing>(factors: &[(TypeId, &[R::Bit])], ops: &mut R) -> Vec<R::Bit> {
    let mut acc = ops.bit_one();
    for (_, bits) in factors {
        let bit = bits.first().cloned().unwrap_or_else(|| ops.bit_zero());
        acc = ops.bit_and(acc, bit);
    }
    alloc::vec![acc]
}

fn lane_product<R: BitRing>(
    width: usize,
    factors: &[(TypeId, &[R::Bit])],
    ops: &mut R,
) -> Vec<R::Bit> {
    let mut out = Vec::with_capacity(width);
    for lane in 0..width {
        let mut acc = ops.bit_one();
        for (_, bits) in factors {
            let bit = if bits.len() == 1 {
                bits[0].clone()
            } else {
                bits.get(lane).cloned().unwrap_or_else(|| ops.bit_zero())
            };
            acc = ops.bit_and(acc, bit);
        }
        out.push(acc);
    }
    out
}

fn slice_or_pad<R: BitRing>(bits: &[R::Bit], start: usize, width: usize, ops: &mut R) -> Vec<R::Bit> {
    let mut out = Vec::with_capacity(width);
    for offset in 0..width {
        out.push(
            bits.get(start + offset)
                .cloned()
                .unwrap_or_else(|| ops.bit_zero()),
        );
    }
    out
}

fn is_bit(ty: TypeId, types: &TypeTable) -> bool {
    matches!(
        types.0.get(ty.0 as usize),
        Some(IrType::Primitive(Type::Bit))
    )
}

fn same_type(lhs: TypeId, rhs: TypeId, types: &TypeTable) -> bool {
    lhs == rhs
        || types
            .0
            .get(lhs.0 as usize)
            .zip(types.0.get(rhs.0 as usize))
            .is_some_and(|(a, b)| a == b)
}

fn wrapped_type_matches(factor: TypeId, field: TypeId, types: &TypeTable) -> bool {
    match types.0.get(field.0 as usize) {
        Some(IrType::ExtField { wrapped, .. }) => same_type(factor, *wrapped, types),
        _ => false,
    }
}

fn field_mul_desc<R: BitRing>(
    lhs: &[R::Bit],
    rhs: &[R::Bit],
    field: &FieldDesc,
    ops: &mut R,
) -> Vec<R::Bit> {
    let degree = field.degree as usize;
    let coeff_width = field.wrapped.width();
    let mut acc = vec![zeros(coeff_width, ops); degree];
    let mut shifted = split_coeffs(lhs, degree, coeff_width, ops);
    let right = split_coeffs(rhs, degree, coeff_width, ops);
    for right_coeff in &right {
        for (index, shifted_coeff) in shifted.iter().enumerate() {
            let product = coeff_mul(shifted_coeff, right_coeff, &field.wrapped, ops);
            acc[index] = coeff_add(&acc[index], &product, ops);
        }
        shifted = xtime(&shifted, field, ops);
    }
    acc.into_iter().flatten().collect()
}

fn split_coeffs<R: BitRing>(
    bits: &[R::Bit],
    degree: usize,
    width: usize,
    ops: &mut R,
) -> Vec<Vec<R::Bit>> {
    let mut out = Vec::with_capacity(degree);
    for index in 0..degree {
        out.push(slice_or_pad(bits, index * width, width, ops));
    }
    out
}

fn coeff_add<R: BitRing>(lhs: &[R::Bit], rhs: &[R::Bit], ops: &mut R) -> Vec<R::Bit> {
    let width = lhs.len().max(rhs.len());
    let mut out = Vec::with_capacity(width);
    for index in 0..width {
        let left = lhs.get(index).cloned().unwrap_or_else(|| ops.bit_zero());
        let right = rhs.get(index).cloned().unwrap_or_else(|| ops.bit_zero());
        out.push(ops.bit_xor(left, right));
    }
    out
}

fn coeff_mul<R: BitRing>(
    lhs: &[R::Bit],
    rhs: &[R::Bit],
    wrapped: &WrappedDesc,
    ops: &mut R,
) -> Vec<R::Bit> {
    match wrapped {
        WrappedDesc::Bit => {
            let left = lhs.first().cloned().unwrap_or_else(|| ops.bit_zero());
            let right = rhs.first().cloned().unwrap_or_else(|| ops.bit_zero());
            alloc::vec![ops.bit_and(left, right)]
        }
        WrappedDesc::Field(field) => field_mul_desc(lhs, rhs, field, ops),
    }
}

fn coeff_const<R: BitRing>(value: u64, width: usize, ops: &mut R) -> Vec<R::Bit> {
    let mut out = Vec::with_capacity(width);
    for index in 0..width {
        let bit = ((value >> index) & 1) == 1;
        out.push(if bit { ops.bit_one() } else { ops.bit_zero() });
    }
    out
}

fn xtime<R: BitRing>(coeffs: &[Vec<R::Bit>], field: &FieldDesc, ops: &mut R) -> Vec<Vec<R::Bit>> {
    let degree = coeffs.len();
    let coeff_width = field.wrapped.width();
    let overflow = coeffs[degree - 1].clone();
    let mut next = vec![zeros(coeff_width, ops); degree];
    for index in 1..degree {
        next[index] = coeffs[index - 1].clone();
    }
    for index in 0..degree {
        let reduction = coeff_const(field.irreducible[index], coeff_width, ops);
        let term = coeff_mul(&overflow, &reduction, &field.wrapped, ops);
        next[index] = coeff_add(&next[index], &term, ops);
    }
    next
}

fn is_irreducible(field: &FieldDesc) -> bool {
    // Rabin: x^{q^n} ≡ x (mod p), and gcd(x^{q^{n/r}} + x, p) = 1 for each
    // prime r dividing n. q = 2^{width(wrapped)}.
    let mut ops = BoolRing;
    let modulus = modulus_poly(field, &mut ops);
    let x = monomial_x(field, &mut ops);
    let q_bits = field.wrapped.width();
    let xn = frobenius_pow(&x, field.degree, q_bits, &modulus, field, &mut ops);
    if poly_coeffs(&xn) != poly_coeffs(&x) {
        return false;
    }
    for prime in distinct_primes(field.degree) {
        let exponent = field.degree / prime;
        let xq = frobenius_pow(&x, exponent, q_bits, &modulus, field, &mut ops);
        let mut sum = xq;
        poly_add_assign(&mut sum, &x);
        let gcd = poly_gcd(&modulus, &sum, field, &mut ops);
        if !poly_is_one(&gcd) {
            return false;
        }
    }
    true
}

fn modulus_poly(field: &FieldDesc, ops: &mut BoolRing) -> Vec<Vec<bool>> {
    let mut coeffs = Vec::with_capacity(field.degree as usize + 1);
    for index in 0..=field.degree as usize {
        coeffs.push(u64_elem(field.irreducible[index], field.wrapped.width(), ops));
    }
    coeffs
}

fn monomial_x(field: &FieldDesc, ops: &mut BoolRing) -> Vec<Vec<bool>> {
    let width = field.wrapped.width();
    let mut coeffs = vec![u64_elem(0, width, ops); field.degree as usize];
    if field.degree as usize > 1 {
        coeffs[1] = u64_elem(1, width, ops);
    }
    coeffs
}

fn u64_elem(value: u64, width: usize, ops: &mut BoolRing) -> Vec<bool> {
    coeff_const(value, width, ops)
}

fn frobenius_pow(
    base: &[Vec<bool>],
    times: u32,
    q_bits: usize,
    modulus: &[Vec<bool>],
    field: &FieldDesc,
    ops: &mut BoolRing,
) -> Vec<Vec<bool>> {
    let mut acc = base.to_vec();
    for _ in 0..times {
        for _ in 0..q_bits {
            acc = poly_mul_mod(&acc, &acc, modulus, field, ops);
        }
    }
    acc
}

fn poly_coeffs(poly: &[Vec<bool>]) -> Vec<Vec<bool>> {
    poly.to_vec()
}

fn poly_add_assign(lhs: &mut Vec<Vec<bool>>, rhs: &[Vec<bool>]) {
    let mut ops = BoolRing;
    let width = lhs.len().max(rhs.len());
    lhs.resize(width, Vec::new());
    for index in 0..rhs.len() {
        if lhs[index].is_empty() {
            lhs[index] = rhs[index].clone();
        } else {
            lhs[index] = coeff_add(&lhs[index], &rhs[index], &mut ops);
        }
    }
}

fn poly_mul_mod(
    lhs: &[Vec<bool>],
    rhs: &[Vec<bool>],
    modulus: &[Vec<bool>],
    field: &FieldDesc,
    ops: &mut BoolRing,
) -> Vec<Vec<bool>> {
    let mut product = vec![u64_elem(0, field.wrapped.width(), ops); lhs.len() + rhs.len()];
    for (i, left) in lhs.iter().enumerate() {
        if elem_is_zero(left) {
            continue;
        }
        for (j, right) in rhs.iter().enumerate() {
            if elem_is_zero(right) {
                continue;
            }
            let term = coeff_mul(left, right, &field.wrapped, ops);
            product[i + j] = coeff_add(&product[i + j], &term, ops);
        }
    }
    poly_mod(&product, modulus, field, ops)
}

fn poly_mod(
    value: &[Vec<bool>],
    modulus: &[Vec<bool>],
    field: &FieldDesc,
    ops: &mut BoolRing,
) -> Vec<Vec<bool>> {
    let mut rest = value.to_vec();
    let degree = modulus.len() - 1;
    while poly_degree(&rest) >= degree as i32 {
        let shift = poly_degree(&rest) as usize - degree;
        let lead = rest[poly_degree(&rest) as usize].clone();
        for (index, coeff) in modulus.iter().enumerate() {
            let term = coeff_mul(&lead, coeff, &field.wrapped, ops);
            let at = index + shift;
            if at >= rest.len() {
                rest.resize(at + 1, u64_elem(0, field.wrapped.width(), ops));
            }
            rest[at] = coeff_add(&rest[at], &term, ops);
        }
    }
    rest.truncate(degree);
    while rest.len() < degree {
        rest.push(u64_elem(0, field.wrapped.width(), ops));
    }
    rest
}

fn poly_degree(poly: &[Vec<bool>]) -> i32 {
    for index in (0..poly.len()).rev() {
        if !elem_is_zero(&poly[index]) {
            return index as i32;
        }
    }
    -1
}

fn poly_gcd(
    lhs: &[Vec<bool>],
    rhs: &[Vec<bool>],
    field: &FieldDesc,
    ops: &mut BoolRing,
) -> Vec<Vec<bool>> {
    let mut a = lhs.to_vec();
    let mut b = rhs.to_vec();
    while poly_degree(&b) >= 0 {
        let remainder = poly_rem(&a, &b, field, ops);
        a = b;
        b = remainder;
    }
    a
}

fn poly_rem(
    lhs: &[Vec<bool>],
    rhs: &[Vec<bool>],
    field: &FieldDesc,
    ops: &mut BoolRing,
) -> Vec<Vec<bool>> {
    let mut rest = lhs.to_vec();
    let divisor_degree = poly_degree(rhs);
    if divisor_degree < 0 {
        return rest;
    }
    while poly_degree(&rest) >= divisor_degree {
        let shift = (poly_degree(&rest) - divisor_degree) as usize;
        let lead = coeff_div(
            &rest[poly_degree(&rest) as usize],
            &rhs[divisor_degree as usize],
            &field.wrapped,
            ops,
        );
        for (index, coeff) in rhs.iter().enumerate() {
            if elem_is_zero(coeff) {
                continue;
            }
            let term = coeff_mul(&lead, coeff, &field.wrapped, ops);
            let at = index + shift;
            rest[at] = coeff_add(&rest[at], &term, ops);
        }
    }
    rest
}

/// Divide coefficients. Over GF(2) this is identity when the divisor bit is 1.
/// Over a tower, multiply by the inverse via Fermat: a^{q-2}.
fn coeff_div<R: BitRing>(
    lhs: &[R::Bit],
    rhs: &[R::Bit],
    wrapped: &WrappedDesc,
    ops: &mut R,
) -> Vec<R::Bit> {
    match wrapped {
        WrappedDesc::Bit => lhs.to_vec(),
        WrappedDesc::Field(field) => {
            let inverse = field_inv(rhs, field, ops);
            field_mul_desc(lhs, &inverse, field, ops)
        }
    }
}

fn field_inv<R: BitRing>(elem: &[R::Bit], field: &FieldDesc, ops: &mut R) -> Vec<R::Bit> {
    // a^{2^w - 2} = product of a^{2^k} for k = 1..w-1.
    if field.width <= 1 {
        return elem.to_vec();
    }
    let mut square = field_mul_desc(elem, elem, field, ops);
    let mut result = square.clone();
    for _ in 2..field.width {
        square = field_mul_desc(&square, &square, field, ops);
        result = field_mul_desc(&result, &square, field, ops);
    }
    result
}

fn poly_is_one(poly: &[Vec<bool>]) -> bool {
    poly_degree(poly) == 0 && poly.first().is_some_and(|coeff| {
        coeff.first().copied().unwrap_or(false) && coeff.iter().skip(1).all(|bit| !*bit)
    })
}

fn distinct_primes(mut n: u32) -> Vec<u32> {
    let mut primes = Vec::new();
    let mut divisor = 2u32;
    while divisor.saturating_mul(divisor) <= n {
        if n % divisor == 0 {
            primes.push(divisor);
            while n % divisor == 0 {
                n /= divisor;
            }
        }
        divisor += if divisor == 2 { 1 } else { 2 };
    }
    if n > 1 {
        primes.push(n);
    }
    primes
}

/// An imported `volar.field.<op>.…` symbol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldSymbol {
    pub op: FieldOp,
    pub field: FieldSpec,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldOp {
    Add,
    Mul,
    Pack,
    Unpack,
}

/// A field encoded in an import name. `irreducible` includes the leading 1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldSpec {
    pub degree: u32,
    pub wrapped: WrappedSpec,
    pub irreducible: Vec<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WrappedSpec {
    Bit,
    Field(alloc::boxed::Box<FieldSpec>),
}

impl FieldSpec {
    pub fn intern(&self, types: &mut TypeTable) -> Result<TypeId, ExtFieldError> {
        let wrapped = match &self.wrapped {
            WrappedSpec::Bit => types.bit(),
            WrappedSpec::Field(inner) => inner.intern(types)?,
        };
        types.ext_field(wrapped, self.degree, self.irreducible.clone())
    }
}

/// Parse `volar.field.<op>.d<degree>.<wrapped>.p<hex>`.
pub fn parse_field_symbol(name: &str) -> Option<FieldSymbol> {
    let rest = name.strip_prefix("volar.field.")?;
    let (op, rest) = if let Some(rest) = rest.strip_prefix("add.") {
        (FieldOp::Add, rest)
    } else if let Some(rest) = rest.strip_prefix("mul.") {
        (FieldOp::Mul, rest)
    } else if let Some(rest) = rest.strip_prefix("pack.") {
        (FieldOp::Pack, rest)
    } else if let Some(rest) = rest.strip_prefix("unpack.") {
        (FieldOp::Unpack, rest)
    } else {
        return None;
    };
    let (field, rest) = parse_field_body(rest)?;
    if !rest.is_empty() {
        return None;
    }
    Some(FieldSymbol { op, field })
}

fn parse_field_body(input: &str) -> Option<(FieldSpec, &str)> {
    let rest = input.strip_prefix('d')?;
    let (degree_text, rest) = split_dot(rest)?;
    let degree: u32 = degree_text.parse().ok()?;
    if degree < 2 {
        return None;
    }
    let (wrapped, rest) = if let Some(rest) = rest.strip_prefix("bit") {
        (WrappedSpec::Bit, rest)
    } else {
        let rest = rest.strip_prefix('e')?;
        let (inner, rest) = parse_field_body(rest)?;
        (WrappedSpec::Field(alloc::boxed::Box::new(inner)), rest)
    };
    let rest = rest.strip_prefix(".p")?;
    let (hex, rest) = take_hex(rest);
    if hex.is_empty() {
        return None;
    }
    let coeff_width = match &wrapped {
        WrappedSpec::Bit => 1,
        WrappedSpec::Field(inner) => field_spec_width(inner),
    };
    if coeff_width == 0 || coeff_width > 64 {
        return None;
    }
    let irreducible = irreducible_from_hex(&hex, degree, coeff_width)?;
    Some((
        FieldSpec {
            degree,
            wrapped,
            irreducible,
        },
        rest,
    ))
}

fn field_spec_width(spec: &FieldSpec) -> usize {
    let wrapped = match &spec.wrapped {
        WrappedSpec::Bit => 1,
        WrappedSpec::Field(inner) => field_spec_width(inner),
    };
    spec.degree as usize * wrapped
}

fn split_dot(input: &str) -> Option<(&str, &str)> {
    let (head, tail) = input.split_once('.')?;
    if head.is_empty() {
        return None;
    }
    Some((head, tail))
}

fn take_hex(input: &str) -> (String, &str) {
    let end = input
        .find(|ch: char| !ch.is_ascii_hexdigit())
        .unwrap_or(input.len());
    (input[..end].to_string(), &input[end..])
}

fn irreducible_from_hex(hex: &str, degree: u32, coeff_width: usize) -> Option<Vec<u64>> {
    let mut bits = Vec::new();
    for ch in hex.chars().rev() {
        let nibble = ch.to_digit(16)? as u8;
        for shift in 0..4 {
            bits.push(((nibble >> shift) & 1) == 1);
        }
    }
    while bits.last().copied() == Some(false) {
        bits.pop();
    }
    let needed = degree as usize * coeff_width;
    if bits.len() > needed {
        return None;
    }
    bits.resize(needed, false);
    let mut coeffs = Vec::with_capacity(degree as usize + 1);
    for index in 0..degree as usize {
        let mut coeff = 0u64;
        for bit in 0..coeff_width {
            if bits[index * coeff_width + bit] {
                coeff |= 1u64 << bit;
            }
        }
        coeffs.push(coeff);
    }
    coeffs.push(1);
    Some(coeffs)
}

/// Format a canonical import name for `spec`.
pub fn format_field_symbol(op: FieldOp, spec: &FieldSpec) -> String {
    let op = match op {
        FieldOp::Add => "add",
        FieldOp::Mul => "mul",
        FieldOp::Pack => "pack",
        FieldOp::Unpack => "unpack",
    };
    let mut name = String::from("volar.field.");
    name.push_str(op);
    name.push('.');
    push_field_body(&mut name, spec);
    name
}

fn push_field_body(out: &mut String, spec: &FieldSpec) {
    use alloc::format;
    out.push_str(&format!("d{}.", spec.degree));
    match &spec.wrapped {
        WrappedSpec::Bit => out.push_str("bit"),
        WrappedSpec::Field(inner) => {
            out.push('e');
            push_field_body(out, inner);
        }
    }
    out.push_str(".p");
    let coeff_width = match &spec.wrapped {
        WrappedSpec::Bit => 1,
        WrappedSpec::Field(inner) => field_spec_width(inner),
    };
    let mut bits = Vec::new();
    for coeff in spec.irreducible.iter().take(spec.degree as usize) {
        for shift in 0..coeff_width {
            bits.push(((coeff >> shift) & 1) == 1);
        }
    }
    while bits.last().copied() == Some(false) {
        bits.pop();
    }
    if bits.is_empty() {
        out.push('0');
        return;
    }
    while bits.len() % 4 != 0 {
        bits.push(false);
    }
    for nibble in bits.chunks(4).rev() {
        let mut value = 0u8;
        for (shift, bit) in nibble.iter().enumerate() {
            if *bit {
                value |= 1 << shift;
            }
        }
        out.push(char::from_digit(value as u32, 16).unwrap());
    }
}

/// One imported operand: its carrier bits, LSB first, and a constant reading
/// when every bit is public.
pub struct FieldArg<V> {
    pub bits: Vec<V>,
    pub constant: Option<u128>,
}

/// Why an imported field call could not be lowered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FieldCallError {
    /// The call did not have the operand count this operation requires.
    Arity,
    /// A carrier was narrower than the field element it should hold.
    NarrowCarrier,
    /// `unpack`'s index operand was not a constant.
    IndexNotConstant,
    /// `unpack`'s index was outside `0..degree`.
    IndexOutOfRange,
    /// A coefficient bit offset does not fit in a `Shuffle` index.
    BitIndexTooWide,
    /// The symbol's polynomial was rejected by [`TypeTable::ext_field`].
    Field(ExtFieldError),
}

/// Lower one `volar.field` call to `Merge` / `Poly` / `Shuffle` statements.
///
/// `emit` appends a statement and returns its SSA result. The returned bits
/// are the result projected into a carrier of `ret_bits`, zero-extended when
/// the carrier is wider than the field.
pub fn lower_field_call<V: Clone + Ord>(
    symbol: &FieldSymbol,
    args: &[FieldArg<V>],
    ret_bits: usize,
    types: &mut TypeTable,
    mut emit: impl FnMut(Stmt<V>) -> V,
) -> Result<Vec<V>, FieldCallError> {
    let field_ty = symbol
        .field
        .intern(types)
        .map_err(FieldCallError::Field)?;
    let width = types
        .value_bit_width(field_ty)
        .ok_or(FieldCallError::NarrowCarrier)?;
    let bit = types.bit();
    let degree = symbol.field.degree as usize;
    let coeff_width = width / degree;
    let wrapped_ty = match &symbol.field.wrapped {
        WrappedSpec::Bit => bit,
        WrappedSpec::Field(inner) => inner.intern(types).map_err(FieldCallError::Field)?,
    };

    let packed = match symbol.op {
        FieldOp::Add | FieldOp::Mul => {
            if args.len() != 2 {
                return Err(FieldCallError::Arity);
            }
            let left = pack_from_bits(
                &args[0].bits,
                degree,
                coeff_width,
                wrapped_ty,
                field_ty,
                &mut emit,
            )?;
            let right = pack_from_bits(
                &args[1].bits,
                degree,
                coeff_width,
                wrapped_ty,
                field_ty,
                &mut emit,
            )?;
            let mut coeffs = PolyCoeffs::new();
            if symbol.op == FieldOp::Add {
                *coeffs.entry(vec![left]).or_insert(0) ^= 1;
                *coeffs.entry(vec![right]).or_insert(0) ^= 1;
                coeffs.retain(|_, coeff| *coeff & 1 != 0);
            } else {
                let mut key = vec![left, right];
                key.sort();
                coeffs.insert(key, 1);
            }
            emit(Stmt::Poly {
                ty: field_ty,
                coeffs,
                constant: Constant { hi: 0, lo: 0 },
            })
        }
        FieldOp::Pack => {
            if args.len() != degree {
                return Err(FieldCallError::Arity);
            }
            let mut parts = Vec::with_capacity(degree);
            for arg in args {
                parts.push(merge_bits(&arg.bits, coeff_width, wrapped_ty, &mut emit)?);
            }
            emit(Stmt::Merge {
                parts,
                ty: field_ty,
            })
        }
        FieldOp::Unpack => {
            if args.len() != 2 {
                return Err(FieldCallError::Arity);
            }
            let index = args[1]
                .constant
                .ok_or(FieldCallError::IndexNotConstant)?;
            if index >= degree as u128 {
                return Err(FieldCallError::IndexOutOfRange);
            }
            let packed = pack_from_bits(
                &args[0].bits,
                degree,
                coeff_width,
                wrapped_ty,
                field_ty,
                &mut emit,
            )?;
            let start = index as usize * coeff_width;
            let mut bits = Vec::with_capacity(coeff_width);
            for offset in 0..coeff_width {
                let bit_index = start + offset;
                if bit_index > u8::MAX as usize {
                    return Err(FieldCallError::BitIndexTooWide);
                }
                bits.push(emit(Stmt::Shuffle {
                    result_bits: vec![(bit_index as u8, packed.clone())],
                    ty: bit,
                }));
            }
            return Ok(pad_carrier(bits, ret_bits, bit, &mut emit));
        }
    };
    project_bits(packed, width, ret_bits, bit, &mut emit)
}

fn pack_from_bits<V: Clone>(
    bits: &[V],
    degree: usize,
    coeff_width: usize,
    wrapped_ty: TypeId,
    field_ty: TypeId,
    emit: &mut impl FnMut(Stmt<V>) -> V,
) -> Result<V, FieldCallError> {
    let mut parts = Vec::with_capacity(degree);
    for index in 0..degree {
        let start = index * coeff_width;
        let end = start + coeff_width;
        if bits.len() < end {
            return Err(FieldCallError::NarrowCarrier);
        }
        parts.push(merge_bits(&bits[start..end], coeff_width, wrapped_ty, emit)?);
    }
    Ok(emit(Stmt::Merge {
        parts,
        ty: field_ty,
    }))
}

fn merge_bits<V: Clone>(
    bits: &[V],
    width: usize,
    ty: TypeId,
    emit: &mut impl FnMut(Stmt<V>) -> V,
) -> Result<V, FieldCallError> {
    if bits.len() < width {
        return Err(FieldCallError::NarrowCarrier);
    }
    if width == 1 {
        return Ok(bits[0].clone());
    }
    Ok(emit(Stmt::Merge {
        parts: bits[..width].to_vec(),
        ty,
    }))
}

fn project_bits<V: Clone>(
    value: V,
    width: usize,
    ret_bits: usize,
    bit: TypeId,
    emit: &mut impl FnMut(Stmt<V>) -> V,
) -> Result<Vec<V>, FieldCallError> {
    let mut bits = Vec::with_capacity(ret_bits);
    let keep = width.min(ret_bits);
    for index in 0..keep {
        if index > u8::MAX as usize {
            return Err(FieldCallError::BitIndexTooWide);
        }
        bits.push(emit(Stmt::Shuffle {
            result_bits: vec![(index as u8, value.clone())],
            ty: bit,
        }));
    }
    Ok(pad_carrier(bits, ret_bits, bit, emit))
}

fn pad_carrier<V>(
    mut bits: Vec<V>,
    ret_bits: usize,
    bit: TypeId,
    emit: &mut impl FnMut(Stmt<V>) -> V,
) -> Vec<V> {
    while bits.len() < ret_bits {
        bits.push(emit(Stmt::Const(
            Constant { hi: 0, lo: 0 },
            bit,
        )));
    }
    bits.truncate(ret_bits);
    bits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bits_of(value: u64, width: usize) -> Vec<bool> {
        (0..width).map(|index| ((value >> index) & 1) == 1).collect()
    }

    fn u64_of(bits: &[bool]) -> u64 {
        bits.iter().enumerate().fold(0u64, |acc, (index, bit)| {
            if *bit { acc | (1u64 << index) } else { acc }
        })
    }

    #[test]
    fn aes_product_matches_fips_197() {
        let mut types = TypeTable::new();
        let field = types.aes8();
        let mut ops = BoolRing;
        let product = monomial_product(
            field,
            &[
                (field, &bits_of(0x57, 8)),
                (field, &bits_of(0x13, 8)),
            ],
            &types,
            &mut ops,
        );
        assert_eq!(u64_of(&product), 0xfe);
    }

    #[test]
    fn field_square_is_not_the_element() {
        let mut types = TypeTable::new();
        let field = types.aes8();
        let value = bits_of(0x57, 8);
        let mut ops = BoolRing;
        let square = monomial_product(field, &[(field, &value), (field, &value)], &types, &mut ops);
        assert_ne!(u64_of(&square), 0x57);
    }

    #[test]
    fn aes_and_galois64_polynomials_intern() {
        let mut types = TypeTable::new();
        let aes = types.aes8();
        let wide = types.galois64();
        assert!(matches!(
            types.0[aes.0 as usize],
            IrType::ExtField { degree: 8, .. }
        ));
        assert!(matches!(
            types.0[wide.0 as usize],
            IrType::ExtField { degree: 64, .. }
        ));
        assert!(!mul_is_idempotent(aes, &types));
        assert!(mul_is_idempotent(types.bit(), &types));
    }

    #[test]
    fn reducible_polynomial_is_rejected() {
        let mut types = TypeTable::new();
        let bit = types.bit();
        // (x + 1)^2 = x^2 + 1 over GF(2), written with the leading 1.
        let error = types.ext_field(bit, 2, alloc::vec![1, 0, 1]);
        assert_eq!(error, Err(ExtFieldError::Reducible));
    }

    #[test]
    fn symbol_round_trips_the_aes_field() {
        let name = "volar.field.mul.d8.bit.p1b";
        let parsed = parse_field_symbol(name).expect("symbol");
        assert_eq!(parsed.op, FieldOp::Mul);
        assert_eq!(parsed.field.irreducible, aes8_irreducible());
        assert_eq!(format_field_symbol(FieldOp::Mul, &parsed.field), name);
        let mut types = TypeTable::new();
        let id = parsed.field.intern(&mut types).expect("intern");
        assert_eq!(id, types.aes8());
    }
}
