//! Bit-exact IEEE-754 floating point as GF(2) bit circuits.
//!
//! Floats are raw bit patterns (`f64` = 64 bits, `f32` = 32 bits, LSB-first
//! slices). Every function is generic over [`BitCircuitBuilder`], so the same
//! implementation emits VAFFLE (volar-vaffle-target), Volar IR, or — with the
//! test-only `BoolEval` builder in this file's tests — evaluates natively.
//!
//! Semantics: IEEE-754 round-to-nearest-even, full subnormal support, and
//! deterministic NaN handling (invalid operations produce the canonical
//! quiet NaN; NaN inputs propagate as the canonical NaN). Canonical NaNs are
//! permitted by the wasm spec and are what makes the lowering bit-exactly
//! reproducible across MPC parties.
//!
//! Conventions:
//! * exponent math happens in signed 16-bit lanes (biased exponent value
//!   fits comfortably; intermediates stay within [-1200, 3200]);
//! * significand work uses a 128-bit window with the leading significand bit
//!   aligned to bit 127 (see `round_pack_f64`).

use alloc::vec;
use alloc::vec::Vec;

use crate::circuits::{BitCircuitBuilder, bc_add, bc_clz, bc_lshr, bc_select_vec, bc_shl, bc_sub};

// ============================================================================
// Small bit-vector utilities (LSB-first)
// ============================================================================

/// Constant bit vector of width `n`, LSB-first.
pub fn sf_const<B: BitCircuitBuilder>(b: &mut B, val: u128, n: usize) -> Vec<B::Bit> {
    (0..n).map(|i| b.bc_const((val >> i) & 1 != 0)).collect()
}

/// Zero vector of width `n`.
pub fn sf_zeros<B: BitCircuitBuilder>(b: &mut B, n: usize) -> Vec<B::Bit> {
    sf_const(b, 0, n)
}

/// OR reduction.
pub fn sf_or_all<B: BitCircuitBuilder>(b: &mut B, bits: &[B::Bit]) -> B::Bit {
    let mut acc = b.bc_const(false);
    for bit in bits {
        acc = b.bc_or(acc, bit.clone());
    }
    acc
}

/// AND reduction.
pub fn sf_and_all<B: BitCircuitBuilder>(b: &mut B, bits: &[B::Bit]) -> B::Bit {
    let mut acc = b.bc_const(true);
    for bit in bits {
        acc = b.bc_and(acc, bit.clone());
    }
    acc
}

/// Bitwise equality of two equal-length vectors.
pub fn sf_eq<B: BitCircuitBuilder>(b: &mut B, a: &[B::Bit], x: &[B::Bit]) -> B::Bit {
    assert_eq!(a.len(), x.len());
    let mut acc = b.bc_const(true);
    for (ai, xi) in a.iter().zip(x) {
        let ax = b.bc_xor(ai.clone(), xi.clone());
        let nax = b.bc_not(ax);
        acc = b.bc_and(acc, nax);
    }
    acc
}

/// Unsigned `a < x` on LSB-first vectors.
pub fn sf_ult<B: BitCircuitBuilder>(b: &mut B, a: &[B::Bit], x: &[B::Bit]) -> B::Bit {
    assert_eq!(a.len(), x.len());
    let n = a.len();
    let mut lt = b.bc_const(false);
    let mut eq_above = b.bc_const(true);
    for i in (0..n).rev() {
        // lt |= eq_above && !a[i] && x[i]
        let na = b.bc_not(a[i].clone());
        let t = b.bc_and(eq_above.clone(), na);
        let t = b.bc_and(t, x[i].clone());
        lt = b.bc_or(lt, t);
        // eq_above &= a[i] == x[i]
        let ax = b.bc_xor(a[i].clone(), x[i].clone());
        let nax = b.bc_not(ax);
        eq_above = b.bc_and(eq_above, nax);
    }
    lt
}

/// Zero-extend (or truncate) to width `n`.
pub fn sf_zext<B: BitCircuitBuilder>(b: &mut B, a: &[B::Bit], n: usize) -> Vec<B::Bit> {
    let mut v = a.to_vec();
    v.truncate(n);
    while v.len() < n {
        v.push(b.bc_const(false));
    }
    v
}

/// Sign-extend (or truncate) to width `n`.
pub fn sf_sext<B: BitCircuitBuilder>(b: &mut B, a: &[B::Bit], n: usize) -> Vec<B::Bit> {
    let mut v = a.to_vec();
    v.truncate(n);
    let sign = a.last().cloned().unwrap_or_else(|| b.bc_const(false));
    while v.len() < n {
        v.push(sign.clone());
    }
    v
}

// ============================================================================
// Unpacking and classification
// ============================================================================

/// Decoded float fields. `exp`/`frac` are LSB-first slices of the raw bits.
pub struct FloatParts<B: BitCircuitBuilder> {
    pub sign: B::Bit,
    pub exp: Vec<B::Bit>,
    pub frac: Vec<B::Bit>,
}

impl<B: BitCircuitBuilder> FloatParts<B> {
    /// exponent field all ones
    pub fn exp_is_ones(&self, b: &mut B) -> B::Bit {
        sf_and_all(b, &self.exp)
    }
    /// fraction field nonzero
    pub fn frac_nonzero(&self, b: &mut B) -> B::Bit {
        sf_or_all(b, &self.frac)
    }
    /// exponent field zero
    pub fn exp_is_zero(&self, b: &mut B) -> B::Bit {
        let o = sf_or_all(b, &self.exp);
        b.bc_not(o)
    }
    pub fn is_nan(&self, b: &mut B) -> B::Bit {
        let e = self.exp_is_ones(b);
        let f = self.frac_nonzero(b);
        b.bc_and(e, f)
    }
    pub fn is_inf(&self, b: &mut B) -> B::Bit {
        let e = self.exp_is_ones(b);
        let f = self.frac_nonzero(b);
        let nf = b.bc_not(f);
        b.bc_and(e, nf)
    }
    /// +/-0.0
    pub fn is_zero(&self, b: &mut B) -> B::Bit {
        let e = self.exp_is_zero(b);
        let f = self.frac_nonzero(b);
        let nf = b.bc_not(f);
        b.bc_and(e, nf)
    }
}

/// Split raw bits into (sign, exp, frac).
pub fn unpack<B: BitCircuitBuilder>(bits: &[B::Bit], ew: usize, fw: usize) -> FloatParts<B> {
    assert_eq!(bits.len(), 1 + ew + fw);
    FloatParts {
        sign: bits[ew + fw].clone(),
        exp: bits[fw..ew + fw].to_vec(),
        frac: bits[..fw].to_vec(),
    }
}

/// Reassemble raw bits from (sign, exp, frac).
pub fn pack<B: BitCircuitBuilder>(p: &FloatParts<B>) -> Vec<B::Bit> {
    let mut v = p.frac.clone();
    v.extend(p.exp.iter().cloned());
    v.push(p.sign.clone());
    v
}

/// Canonical quiet NaN for the given format.
pub fn canonical_nan<B: BitCircuitBuilder>(b: &mut B, ew: usize, fw: usize) -> Vec<B::Bit> {
    let mut v = sf_zeros(b, 1 + ew + fw);
    for i in 0..ew {
        v[fw + i] = b.bc_const(true);
    }
    v[fw - 1] = b.bc_const(true); // quiet bit
    v
}

/// +/-Infinity for the given format.
pub fn inf_bits<B: BitCircuitBuilder>(b: &mut B, ew: usize, fw: usize, sign: B::Bit) -> Vec<B::Bit> {
    let mut v = sf_zeros(b, 1 + ew + fw);
    for i in 0..ew {
        v[fw + i] = b.bc_const(true);
    }
    v[ew + fw] = sign;
    v
}

/// +/-0.0 for the given format.
pub fn zero_bits<B: BitCircuitBuilder>(b: &mut B, ew: usize, fw: usize, sign: B::Bit) -> Vec<B::Bit> {
    let mut v = sf_zeros(b, 1 + ew + fw);
    v[ew + fw] = sign;
    v
}

/// Select between two bit vectors.
pub fn sf_mux<B: BitCircuitBuilder>(b: &mut B, cond: &B::Bit, t: &[B::Bit], f: &[B::Bit]) -> Vec<B::Bit> {
    bc_select_vec(b, cond.clone(), t, f)
}

// ============================================================================
// Comparisons
// ============================================================================

/// Compute (eq, lt, unordered) for two floats of the same format.
pub fn float_cmp<B: BitCircuitBuilder>(
    b: &mut B,
    a: &[B::Bit],
    x: &[B::Bit],
    ew: usize,
    fw: usize,
) -> (B::Bit, B::Bit, B::Bit) {
    let pa = unpack::<B>(a, ew, fw);
    let px = unpack::<B>(x, ew, fw);
    let nan_a = pa.is_nan(b);
    let nan_x = px.is_nan(b);
    let unordered = b.bc_or(nan_a, nan_x);

    let zero_a = pa.is_zero(b);
    let zero_x = px.is_zero(b);
    let both_zero = b.bc_and(zero_a.clone(), zero_x.clone());

    // Magnitudes: exp+frac (drop sign).
    let mag_a = &a[..ew + fw];
    let mag_x = &x[..ew + fw];
    let mag_eq = sf_eq(b, mag_a, mag_x);
    let eq = {
        // +0 == -0 (both magnitudes zero); otherwise signs must also match.
        let signs_eq = {
            let x_ = b.bc_xor(pa.sign.clone(), px.sign.clone());
            b.bc_not(x_)
        };
        let full = b.bc_and(mag_eq, signs_eq);
        let t = b.bc_or(both_zero.clone(), full);
        let n_un = b.bc_not(unordered.clone());
        b.bc_and(n_un, t)
    };

    let mag_lt = sf_ult(b, mag_a, mag_x); // a < x as positives
    let mag_gt = sf_ult(b, mag_x, mag_a);
    let signs_differ = b.bc_xor(pa.sign.clone(), px.sign.clone());
    // Same sign: negatives reverse magnitude order.
    let lt_same_sign = sf_mux(b, &pa.sign, &[mag_gt], &[mag_lt])[0].clone();
    // Different signs: a < b iff a is negative and not both zero.
    let nz = b.bc_not(both_zero);
    let lt_diff_sign = b.bc_and(pa.sign.clone(), nz);
    let lt_sel = sf_mux(b, &signs_differ, &[lt_diff_sign], &[lt_same_sign])[0].clone();
    let n_un = b.bc_not(unordered.clone());
    let lt = b.bc_and(n_un, lt_sel);
    (eq, lt, unordered)
}

/// Derive the six wasm comparisons from (eq, lt, unordered).
pub fn cmp_results<B: BitCircuitBuilder>(
    b: &mut B,
    eq: &B::Bit,
    lt: &B::Bit,
    unordered: &B::Bit,
) -> [B::Bit; 6] {
    // order: eq, ne, lt, le, gt, ge
    let n_un = b.bc_not(unordered.clone());
    let n_eq = b.bc_not(eq.clone());
    let n_lt = b.bc_not(lt.clone());
    let le = b.bc_or(eq.clone(), lt.clone());
    let gt = {
        let t = b.bc_or(eq.clone(), lt.clone());
        let nt = b.bc_not(t);
        b.bc_and(n_un.clone(), nt)
    };
    let ge = {
        let t = b.bc_and(n_un.clone(), n_lt);
        t
    };
    let ne = b.bc_or(unordered.clone(), n_eq.clone());
    [
        eq.clone(),
        ne,
        lt.clone(),
        le,
        gt,
        ge,
    ]
}

/// f64 comparison; `op`: 0=eq 1=ne 2=lt 3=le 4=gt 5=ge. Returns a single bit.
pub fn f64_cmp<B: BitCircuitBuilder>(b: &mut B, a: &[B::Bit], x: &[B::Bit], op: usize) -> B::Bit {
    let (eq, lt, un) = float_cmp(b, a, x, 11, 52);
    cmp_results(b, &eq, &lt, &un)[op].clone()
}

/// f32 comparison; `op` as for [`f64_cmp`].
pub fn f32_cmp<B: BitCircuitBuilder>(b: &mut B, a: &[B::Bit], x: &[B::Bit], op: usize) -> B::Bit {
    let (eq, lt, un) = float_cmp(b, a, x, 8, 23);
    cmp_results(b, &eq, &lt, &un)[op].clone()
}

// ============================================================================
// Round-and-pack engine (shared by arithmetic and conversions)
// ============================================================================
//
// `window` holds a significand scaled so its leading bit (for normals) is at
// position `top` (= 127). `exp16` is the *biased* target exponent as signed
// 16-bit. `sticky` accumulates any bits lost below the window.

/// Round `window` (leading significand bit at `top_bit`, guard below) into
/// an (ew, fw) float with sign `sign`, RNE, subnormal flush, inf overflow.
///
/// `top_bit` is the window position of the implicit leading bit; the kept
/// significand is `window[top_bit-fw ..= top_bit]`, guard `window[top_bit-fw-1]`,
/// sticky = OR(window[..top_bit-fw-1]) | sticky_in.
pub fn round_pack<B: BitCircuitBuilder>(
    b: &mut B,
    sign: B::Bit,
    exp16: &[B::Bit],
    window: &[B::Bit],
    top_bit: usize,
    sticky_in: B::Bit,
    ew: usize,
    fw: usize,
) -> Vec<B::Bit> {
    let n = window.len();
    assert!(top_bit + 1 <= n && top_bit >= fw + 1);

    // --- Subnormal / underflow pre-shift -----------------------------------
    // If exp <= 0 the result cannot be normal: shift the window right by
    // (1 - exp), folding dropped bits into sticky, and pin exp to 0.
    let exp_is_nonpos = window_signed_nonpositive(b, exp16);
    let one16 = sf_const(b, 1, 16);
    let k16_full = bc_sub(b, &one16, exp16); // 1 - exp (valid when exp <= 0)
    // Clamp k at the window length in 16 bits (k can far exceed 255).
    let n16 = sf_const(b, n as u128, 16);
    let k_over16 = sf_ult(b, &n16, &k16_full);
    let k_capped16 = sf_mux(b, &k_over16, &n16, &k16_full);
    let k_capped: Vec<B::Bit> = k_capped16[..8].to_vec();
    let n_const: Vec<B::Bit> = n16[..8].to_vec();

    let shift7: Vec<B::Bit> = k_capped[..7].to_vec();
    let k_ge_n = {
        // k >= n means the entire window is shifted out
        let k_lt_n = sf_ult(b, &k_capped, &n_const);
        let ge = b.bc_not(k_lt_n);
        b.bc_and(exp_is_nonpos.clone(), ge)
    };
    let win_any = sf_or_all(b, window);
    let sticky_all = b.bc_and(exp_is_nonpos.clone(), k_ge_n.clone());
    let sticky_all = b.bc_and(sticky_all, win_any.clone());

    // dropped = window & ((1 << k) - 1), for k < n
    let one_n = sf_const(b, 1, n);
    let one_shl = bc_shl(b, &one_n, &shift7);
    let mask = bc_sub(b, &one_shl, &one_n); // (1<<k)-1
    let dropped: Vec<B::Bit> = window
        .iter()
        .zip(mask.iter())
        .map(|(w, m)| b.bc_and(w.clone(), m.clone()))
        .collect();
    let dropped_any = sf_or_all(b, &dropped);
    let sticky_drop = b.bc_and(exp_is_nonpos.clone(), dropped_any);

    let win_shifted = bc_lshr(b, window, &shift7);
    let zn = sf_zeros(b, n);
    let win_shifted = sf_mux(b, &k_ge_n, &zn, &win_shifted);
    let win_pre = sf_mux(b, &exp_is_nonpos, &win_shifted, &window.to_vec());
    let sticky1 = b.bc_or(sticky_in, sticky_all);
    let sticky2 = b.bc_or(sticky1, sticky_drop);
    let zero16 = sf_zeros(b, 16);
    let exp_pre = sf_mux(b, &exp_is_nonpos, &zero16, &exp16.to_vec());

    // --- RNE rounding at the (fw+1)-bit boundary ----------------------------
    let g = win_pre[top_bit - fw - 1].clone();
    let r_and_below = sf_or_all(b, &win_pre[..top_bit - fw - 1]);
    let sticky_all2 = b.bc_or(sticky2, r_and_below);
    let kept: Vec<B::Bit> = win_pre[top_bit - fw..=top_bit].to_vec(); // fw+1 bits
    let inc = {
        let t = b.bc_or(sticky_all2, kept[0].clone());
        b.bc_and(g, t)
    };
    let kept_wide = sf_zext(b, &kept, fw + 2);
    let inc_wide = sf_zext(b, &[inc], fw + 2);
    let sum = bc_add(b, &kept_wide, &inc_wide, false); // fw+2 bits
    let carry = sum[fw + 1].clone();
    let sum_shr: Vec<B::Bit> = sum[1..=fw + 1].to_vec();
    let sig_final = sf_mux(b, &carry, &sum_shr, &sum[..fw + 1].to_vec());
    let carry16 = sf_zext(b, &[carry], 16);
    let exp_final = bc_add(b, &exp_pre, &carry16, false);

    // --- Overflow to infinity ------------------------------------------------
    let max_exp = sf_const(b, ((1u128 << ew) - 1) as u128, 16);
    let ovf = {
        // exp_final >= max_exp  <=>  !(exp_final < max_exp), exp_final >= 0
        let lt = sf_ult(b, &exp_final, &max_exp);
        let n_lt = b.bc_not(lt);
        let neg = exp_final[15].clone();
        let n_neg = b.bc_not(neg);
        b.bc_and(n_lt, n_neg)
    };

    // --- Assemble ------------------------------------------------------------
    let frac: Vec<B::Bit> = sig_final[..fw].to_vec();
    let exp_lo: Vec<B::Bit> = exp_final[..ew].to_vec();
    let normal_out = {
        let mut v = frac;
        v.extend(exp_lo);
        v.push(sign.clone());
        v
    };
    let inf = inf_bits(b, ew, fw, sign);
    sf_mux(b, &ovf, &inf, &normal_out)
}

/// True when the signed 16-bit value is <= 0.
fn window_signed_nonpositive<B: BitCircuitBuilder>(b: &mut B, v: &[B::Bit]) -> B::Bit {
    debug_assert_eq!(v.len(), 16);
    let neg = v[15].clone();
    let zero = {
        let o = sf_or_all(b, v);
        b.bc_not(o)
    };
    b.bc_or(neg, zero)
}

// ============================================================================
// Integer -> float conversions
// ============================================================================

/// `f64` from 64-bit integer (`signed` selects interpretation). RNE.
pub fn f64_from_i64<B: BitCircuitBuilder>(b: &mut B, x: &[B::Bit], signed: bool) -> Vec<B::Bit> {
    assert_eq!(x.len(), 64);
    let (sign, mag) = if signed {
        let sign = x[63].clone();
        let zero = sf_zeros(b, 64);
        let neg = bc_sub(b, &zero, &x.to_vec());
        (sign.clone(), sf_mux(b, &sign, &neg, &x.to_vec()))
    } else {
        (b.bc_const(false), x.to_vec())
    };
    let is_zero = {
        let o = sf_or_all(b, &mag);
        b.bc_not(o)
    };
    let lz = bc_clz(b, &mag); // 64-bit count, value 0..=64
    let lz7: Vec<B::Bit> = lz[..7].to_vec();
    // normalize: leading bit to position 63
    let norm = bc_shl(b, &mag, &lz7[..6]);
    // significand: top 53 bits (bits 63..11), guard bit 10, sticky 0..10
    let kept: Vec<B::Bit> = norm[11..64].to_vec(); // 53 bits
    let g = norm[10].clone();
    let sticky = sf_or_all(b, &norm[..10]);
    let inc = {
        let t = b.bc_or(sticky, kept[0].clone());
        b.bc_and(g, t)
    };
    let kept_w = sf_zext(b, &kept, 54);
    let inc_w = sf_zext(b, &[inc], 54);
    let sum = bc_add(b, &kept_w, &inc_w, false);
    let carry = sum[53].clone();
    let sum_shr: Vec<B::Bit> = sum[1..54].to_vec();
    let sig53 = sf_mux(b, &carry, &sum_shr, &sum[..53].to_vec());
    // exponent: unbiased = 63 - lz; biased = 1086 - lz (+carry)
    let e_const = sf_const(b, 1086, 16);
    let lz16 = sf_zext(b, &lz, 16);
    let mut exp = bc_sub(b, &e_const, &lz16);
    let carry16 = sf_zext(b, &[carry], 16);
    exp = bc_add(b, &exp, &carry16, false);
    let frac: Vec<B::Bit> = sig53[..52].to_vec();
    let exp11: Vec<B::Bit> = exp[..11].to_vec();
    let mut out = frac;
    out.extend(exp11);
    out.push(sign.clone());
    // zero shortcut
    let zero = zero_bits(b, 11, 52, sign);
    sf_mux(b, &is_zero, &zero, &out)
}

/// `f64` from 32-bit integer.
pub fn f64_from_i32<B: BitCircuitBuilder>(b: &mut B, x: &[B::Bit], signed: bool) -> Vec<B::Bit> {
    assert_eq!(x.len(), 32);
    let wide = if signed { sf_sext(b, x, 64) } else { sf_zext(b, x, 64) };
    f64_from_i64(b, &wide, signed)
}

// ============================================================================
// Float -> integer conversions (wasm trunc_sat semantics)
// ============================================================================

/// `f64` -> signed 32-bit with wasm `i32.trunc_sat_f64_s` semantics:
/// NaN -> 0, below/above range -> INT32_MIN/MAX, else truncate toward zero.
pub fn i32_trunc_sat_f64<B: BitCircuitBuilder>(b: &mut B, a: &[B::Bit], signed: bool) -> Vec<B::Bit> {
    assert_eq!(a.len(), 64);
    let p = unpack::<B>(a, 11, 52);
    let is_nan = p.is_nan(b);
    let is_zero = p.is_zero(b);
    let is_inf = p.is_inf(b);

    // e = exp - 1023 (signed 16)
    let bias = sf_const(b, 1023, 16);
    let exp16 = sf_zext(b, &p.exp, 16);
    let e = bc_sub(b, &exp16, &bias);

    // value < 1 in magnitude when e < 0 (or zero) -> result 0
    let e_neg = e[15].clone();

    // significand 53 bits, implicit 1
    let mut sig = p.frac.clone();
    sig.push(b.bc_const(true)); // bit 52 = implicit (harmless for subnormals: e<0 handled)

    // truncate toward zero: val = sig >> (52 - e), valid for 0 <= e <= 52
    let c52 = sf_const(b, 52, 16);
    let sh = bc_sub(b, &c52, &e); // 52 - e
    let sh7: Vec<B::Bit> = sh[..7].to_vec();
    let shifted = bc_lshr(b, &sig, &sh7[..6]);

    // apply sign
    let zero32 = sf_zeros(b, 64);
    let mag64 = sf_zext(b, &shifted, 64);
    let neg = bc_sub(b, &zero32, &mag64);
    let signed_val = sf_mux(b, &p.sign, &neg, &mag64);

    // range check for 32-bit signed/unsigned
    let (min_v, max_v) = if signed {
        (-(1i64 << 31) as i128 as u128, (1u128 << 31) - 1)
    } else {
        (0u128, (1u128 << 32) - 1)
    };
    // |x| >= 2^31 (signed) / 2^32 (unsigned) overflows positive side; sign adds one more for min.
    let lim_exp = if signed { 31 } else { 32 };
    let lim = sf_const(b, lim_exp as u128, 16);
    let ge_lim = {
        let lt = sf_ult(b, &e, &lim);
        let nn = b.bc_not(e[15].clone());
        let nlt = b.bc_not(lt);
        b.bc_and(nn, nlt) // e >= lim_exp
    };
    let over = b.bc_or(ge_lim, is_inf.clone());

    let max_bits = sf_const(b, max_v & 0xFFFF_FFFF_FFFF_FFFF, 64)[..32].to_vec();
    let min_bits = sf_const(b, min_v & 0xFFFF_FFFF_FFFF_FFFF, 64)[..32].to_vec();
    let sat_val = sf_mux(b, &p.sign, &min_bits, &max_bits);
    let mut out = sf_mux(b, &over, &sat_val, &signed_val[..32].to_vec());
    // NaN -> 0; |x| < 1 -> 0 (covers zero)
    let z32 = sf_zeros(b, 32);
    let to_zero = b.bc_or(is_nan.clone(), e_neg.clone());
    let to_zero = b.bc_or(to_zero, is_zero.clone());
    out = sf_mux(b, &to_zero, &z32, &out);
    out
}

// ============================================================================
// f64 -> f64 rounding to integer
// ============================================================================

/// Round to integer; mode: 0=trunc, 1=floor, 2=ceil, 3=nearest-even.
pub fn f64_round_int<B: BitCircuitBuilder>(b: &mut B, a: &[B::Bit], mode: u8) -> Vec<B::Bit> {
    assert_eq!(a.len(), 64);
    let p = unpack::<B>(a, 11, 52);
    let is_nan = p.is_nan(b);
    let is_inf = p.is_inf(b);
    let is_zero = p.is_zero(b);
    let passthrough = {
        let t = b.bc_or(is_nan.clone(), is_inf.clone());
        b.bc_or(t, is_zero.clone())
    };

    let bias = sf_const(b, 1023, 16);
    let exp16 = sf_zext(b, &p.exp, 16);
    let e = bc_sub(b, &exp16, &bias); // unbiased exponent, signed 16
    let e_neg = e[15].clone();

    // m = number of low mantissa bits to clear = 52 - e (for 0 <= e <= 52)
    let c52 = sf_const(b, 52, 16);
    let m16 = bc_sub(b, &c52, &e);

    // |x| >= 2^52 -> already integral
    let already = {
        let ge = {
            let lt = sf_ult(b, &e, &c52);
            let nn = b.bc_not(e[15].clone());
            let nlt = b.bc_not(lt);
            b.bc_and(nn, nlt)
        };
        b.bc_or(ge, passthrough.clone())
    };

    // Cleared-bit analysis (for e in 0..52): bits [0..m) of frac drop.
    let m7: Vec<B::Bit> = m16[..7].to_vec();
    let m6: Vec<B::Bit> = m7[..6].to_vec();
    let one52 = sf_const(b, 1, 52);
    let one_shl = bc_shl(b, &one52, &m6);
    let mask = bc_sub(b, &one_shl, &one52); // low m bits set (m <= 52)
    let dropped: Vec<B::Bit> = p
        .frac
        .iter()
        .zip(mask.iter())
        .map(|(f, mm)| b.bc_and(f.clone(), mm.clone()))
        .collect();
    let dropped_any = sf_or_all(b, &dropped);
    // guard = top dropped bit (bit m-1), rest = below it
    let one6 = sf_const(b, 1, 6);
    let mask_shr1 = bc_lshr(b, &mask, &one6);
    let rest_bits: Vec<B::Bit> = dropped
        .iter()
        .zip(mask_shr1.iter())
        .map(|(d, mm)| b.bc_and(d.clone(), mm.clone()))
        .collect();
    let rest_any = sf_or_all(b, &rest_bits);
    let guard = {
        // guard bit = dropped bit at position m-1 = frac & (mask ^ (mask>>1)) msb
        let g_mask: Vec<B::Bit> = mask
            .iter()
            .zip(mask_shr1.iter())
            .map(|(h, l)| b.bc_xor(h.clone(), l.clone()))
            .collect();
        let g: Vec<B::Bit> = p
            .frac
            .iter()
            .zip(g_mask.iter())
            .map(|(f, mm)| b.bc_and(f.clone(), mm.clone()))
            .collect();
        sf_or_all(b, &g)
    };

    // increment decision per mode
    // LSB of the kept part = bit m of the combined (exp,frac) magnitude
    // (m == 52 lands in the exponent field, so probe the 63-bit vector).
    let mut mag_full = p.frac.clone();
    mag_full.extend(p.exp.iter().cloned()); // 63 bits
    let one63 = sf_const(b, 1, 63);
    let pos63 = bc_shl(b, &one63, &m6); // 1 << m
    let kept_lsb = {
        let cand: Vec<B::Bit> = mag_full
            .iter()
            .zip(pos63.iter())
            .map(|(f, mm)| b.bc_and(f.clone(), mm.clone()))
            .collect();
        sf_or_all(b, &cand)
    };
    let inc = match mode {
        0 => b.bc_const(false),                                  // trunc
        1 => b.bc_and(p.sign.clone(), dropped_any.clone()),      // floor
        2 => {
            let ns = b.bc_not(p.sign.clone());
            b.bc_and(ns, dropped_any.clone())
        }                                                        // ceil
        _ => {
            let t = b.bc_or(rest_any, kept_lsb);
            b.bc_and(guard.clone(), t)
        }                                                        // nearest-even
    };

    // Apply: clear low m bits of frac, optionally +1 ulp (carries into exp).
    let cleared: Vec<B::Bit> = p
        .frac
        .iter()
        .zip(mask.iter())
        .map(|(f, mm)| {
            let nm = b.bc_not(mm.clone());
            b.bc_and(f.clone(), nm)
        })
        .collect();
    // (exp,frac) as one 63-bit magnitude for the ulp increment
    let mut mag = cleared.clone();
    mag.extend(p.exp.iter().cloned()); // 63 bits
    // the increment is one ULP of the *kept* precision: bit m of the
    // (exp,frac) magnitude, which may carry into the exponent.
    let inc63 = sf_zext(b, &[inc], 63);
    let inc64 = bc_shl(b, &inc63, &m6);
    let mag_inc = bc_add(b, &mag, &inc64, false);
    let mut rounded = mag_inc;
    rounded.push(p.sign.clone());

    // |x| < 1 cases (e < 0)
    let one_of_sign = |bb: &mut B, sign: B::Bit| -> Vec<B::Bit> {
        // 1.0 with given sign
        let mut v = sf_const(bb, 1023, 11);
        let mut f = sf_zeros(bb, 52);
        f.append(&mut v);
        f.push(sign);
        f
    };
    let small = match mode {
        0 => zero_bits(b, 11, 52, p.sign.clone()),                          // trunc -> +-0
        1 => {
            // floor: x<0 & x!=0 -> -1.0; else +0
            let t = b.bc_const(true);
            let neg_one = one_of_sign(b, t);
            let f = b.bc_const(false);
            let pos_zero = zero_bits(b, 11, 52, f);
            let neg_nz = {
                let nz = b.bc_not(is_zero.clone());
                b.bc_and(p.sign.clone(), nz)
            };
            sf_mux(b, &neg_nz, &neg_one, &pos_zero)
        }
        2 => {
            // ceil: x>0 & x!=0 -> +1.0; else -0 (preserve sign for zero)
            let f = b.bc_const(false);
            let pos_one = one_of_sign(b, f);
            let sign_zero = zero_bits(b, 11, 52, p.sign.clone());
            let pos_nz = {
                let ns = b.bc_not(p.sign.clone());
                let nz = b.bc_not(is_zero.clone());
                b.bc_and(ns, nz)
            };
            sf_mux(b, &pos_nz, &pos_one, &sign_zero)
        }
        _ => {
            // nearest: in this branch |x| < 1 (e <= -1).
            //   |x| > 0.5 <=> e == -1 && frac != 0  -> +-1.0
            //   |x| == 0.5 (e == -1 && frac == 0)   -> tie -> +-0 (even)
            //   |x| < 0.5                           -> +-0
            let e_is_m1 = sf_and_all(b, &e); // -1 is all-ones in two's complement
            let frac_any = sf_or_all(b, &p.frac);
            let gt_half = b.bc_and(e_is_m1, frac_any);
            let one_s = one_of_sign(b, p.sign.clone());
            let zero_s = zero_bits(b, 11, 52, p.sign.clone());
            sf_mux(b, &gt_half, &one_s, &zero_s)
        }
    };

    // compose
    let out1 = sf_mux(b, &e_neg, &small, &rounded);
    let out2 = sf_mux(b, &already, &a.to_vec(), &out1);
    out2
}

// ============================================================================
// f32 <-> f64
// ============================================================================

/// f64 <- f32 (exact).
pub fn f64_promote_f32<B: BitCircuitBuilder>(b: &mut B, x: &[B::Bit]) -> Vec<B::Bit> {
    assert_eq!(x.len(), 32);
    let p = unpack::<B>(x, 8, 23);
    let is_nan = p.is_nan(b);
    let is_inf = p.is_inf(b);
    let is_zero = p.is_zero(b);
    let is_sub = {
        let ez = p.exp_is_zero(b);
        let fnz = p.frac_nonzero(b);
        b.bc_and(ez, fnz)
    };

    // subnormal: normalize frac (leading bit at p = 22 - clz23)
    let lz23 = bc_clz(b, &p.frac); // 23-bit count
    // shift frac left by lz+1 to drop the implicit leading 1 into the void:
    let one = sf_const(b, 1, 23)[..5].to_vec();
    let lz5: Vec<B::Bit> = lz23[..5].to_vec();
    let lz5w = sf_zext(b, &lz5, 23);
    let one23 = sf_const(b, 1, 23);
    let lz1 = bc_add(b, &lz5w, &one23, false);
    let frac_shifted = bc_shl(b, &p.frac, &lz1[..5]);
    let mut sub_frac52 = sf_zeros(b, 52);
    sub_frac52[29..52].clone_from_slice(&frac_shifted[..23]);
    // value = 1.f x 2^(22-lz-149) = 2^(-127 - lz); biased64 = 1023 - 127 - lz
    let base = sf_const(b, 1023 - 127, 16);
    let lz16 = sf_zext(b, &lz23, 16);
    let sub_exp = bc_sub(b, &base, &lz16);

    // normal: rebias
    let bias = sf_const(b, 1023 - 127, 16);
    let exp16 = sf_zext(b, &p.exp, 16);
    let norm_exp = bc_add(b, &exp16, &bias, false);
    let norm_frac52 = {
        let mut f = sf_zeros(b, 52);
        f[29..52].clone_from_slice(&p.frac);
        f
    };

    let exp = sf_mux(b, &is_sub, &sub_exp, &norm_exp);
    let frac = sf_mux(b, &is_sub, &sub_frac52, &norm_frac52);
    let mut out = frac;
    out.extend(exp[..11].iter().cloned());
    out.push(p.sign.clone());

    let nan = canonical_nan(b, 11, 52);
    let inf = inf_bits(b, 11, 52, p.sign.clone());
    let zero = zero_bits(b, 11, 52, p.sign.clone());
    let o1 = sf_mux(b, &is_zero, &zero, &out);
    let o2 = sf_mux(b, &is_inf, &inf, &o1);
    sf_mux(b, &is_nan, &nan, &o2)
}

/// f32 <- f64, RNE with subnormal flush and inf overflow.
pub fn f32_demote_f64<B: BitCircuitBuilder>(b: &mut B, a: &[B::Bit]) -> Vec<B::Bit> {
    assert_eq!(a.len(), 64);
    let p = unpack::<B>(a, 11, 52);
    let is_nan = p.is_nan(b);
    let is_inf = p.is_inf(b);
    let is_zero = p.is_zero(b);

    // Window: 53-bit significand top-aligned in 64 bits at position 63.
    let mut win = sf_zeros(b, 64);
    win[11..63].clone_from_slice(&p.frac); // frac at 11..=62, implicit at 63
    let implicit = {
        let nz = p.exp_is_zero(b);
        b.bc_not(nz)
    };
    win[63] = implicit;
    // biased-f32 exponent as signed 16: e32 = exp64 - 1023 + 127; for
    // subnormal f64 inputs use -126 (they are all far below f32 range and
    // will round to zero, but keep the math consistent).
    let sub_adj = sf_const(b, 1023 - 127, 16);
    let exp16 = sf_zext(b, &p.exp, 16);
    let e32 = bc_sub(b, &exp16, &sub_adj);
    let ez = p.exp_is_zero(b);
    let min126 = sf_const(b, (-126i16 as u16) as u128, 16);
    let exp16 = sf_mux(b, &ez, &min126, &e32);

    let f0 = b.bc_const(false);
    let rounded = round_pack(b, p.sign.clone(), &exp16, &win, 63, f0, 8, 23);

    let nan = canonical_nan(b, 8, 23);
    let inf = inf_bits(b, 8, 23, p.sign.clone());
    let zero = zero_bits(b, 8, 23, p.sign.clone());
    let o1 = sf_mux(b, &is_zero, &zero, &rounded);
    let o2 = sf_mux(b, &is_inf, &inf, &o1);
    sf_mux(b, &is_nan, &nan, &o2)
}

// ============================================================================
// Add / subtract
// ============================================================================

/// f64 add; `negate_b` implements subtraction.
pub fn f64_add<B: BitCircuitBuilder>(
    b: &mut B,
    a: &[B::Bit],
    x: &[B::Bit],
    negate_b: bool,
) -> Vec<B::Bit> {
    assert_eq!(a.len(), 64);
    assert_eq!(x.len(), 64);
    let pa = unpack::<B>(a, 11, 52);
    let px_raw = unpack::<B>(x, 11, 52);
    let sign_b = if negate_b {
        b.bc_not(px_raw.sign.clone())
    } else {
        px_raw.sign.clone()
    };
    let px = FloatParts {
        sign: sign_b,
        exp: px_raw.exp.clone(),
        frac: px_raw.frac.clone(),
    };

    let nan_a = pa.is_nan(b);
    let nan_b = px.is_nan(b);
    let inf_a = pa.is_inf(b);
    let inf_b = px.is_inf(b);
    let zero_a = pa.is_zero(b);
    let zero_b = px.is_zero(b);
    let any_nan = b.bc_or(nan_a.clone(), nan_b.clone());
    // inf + -inf = NaN
    let inf_signs_differ = {
        let sd = b.bc_xor(pa.sign.clone(), px.sign.clone());
        let bi = b.bc_and(inf_a.clone(), inf_b.clone());
        b.bc_and(bi, sd)
    };
    let result_nan = b.bc_or(any_nan.clone(), inf_signs_differ);
    let any_inf = b.bc_or(inf_a.clone(), inf_b.clone());
    let inf_sign = sf_mux(b, &inf_a, &[pa.sign.clone()], &[px.sign.clone()])[0].clone();

    // Effective subtraction when signs differ.
    let eff_sub = b.bc_xor(pa.sign.clone(), px.sign.clone());

    // Order operands by magnitude: (exp, frac) of a must be >= that of b.
    let mag_a: Vec<B::Bit> = a[..63].to_vec();
    let mag_b: Vec<B::Bit> = x[..63].to_vec();
    let swap = sf_ult(b, &mag_a, &mag_b);
    let big = sf_mux(b, &swap, &x.to_vec(), &a.to_vec());
    let small = sf_mux(b, &swap, &a.to_vec(), &x.to_vec());
    let pb = unpack::<B>(&big, 11, 52);
    let ps = unpack::<B>(&small, 11, 52);
    // result sign: sign of the larger-magnitude operand (after b-negation)
    let sign_big = sf_mux(b, &swap, &[px.sign.clone()], &[pa.sign.clone()])[0].clone();

    // Significands with implicit bit (53 bits); subnormal trick: exp->1.
    let imp_big = {
        let z = pb.exp_is_zero(b);
        b.bc_not(z)
    };
    let imp_small = {
        let z = ps.exp_is_zero(b);
        b.bc_not(z)
    };
    let mut sig_big = pb.frac.clone();
    sig_big.push(imp_big);
    let mut sig_small = ps.frac.clone();
    sig_small.push(imp_small);
    let one11 = sf_const(b, 1, 11);
    let pb_ez = pb.exp_is_zero(b);
    let e_big = sf_mux(b, &pb_ez, &one11, &pb.exp);
    let ps_ez = ps.exp_is_zero(b);
    let e_small = sf_mux(b, &ps_ez, &one11, &ps.exp);
    let e_big16 = sf_zext(b, &e_big, 16);
    let e_small16 = sf_zext(b, &e_small, 16);
    let diff = bc_sub(b, &e_big16, &e_small16); // >= 0

    // 128-bit windows, leading significand bit at 127.
    let mut win_big = sf_zeros(b, 128);
    win_big[75..128].clone_from_slice(&sig_big);
    let mut small_pre = sf_zeros(b, 128);
    small_pre[75..128].clone_from_slice(&sig_small);

    // Align small: shift right by diff (clamp at 128; sticky = dropped bits).
    let d128 = sf_const(b, 128, 16);
    let diff_ge_128 = {
        let lt = sf_ult(b, &diff, &d128);
        b.bc_not(lt) // diff >= 128
    };
    let diff8: Vec<B::Bit> = diff[..8].to_vec();
    let c127 = sf_const(b, 127, 8);
    let shift7 = sf_mux(b, &diff_ge_128, &c127, &diff8)[..7].to_vec();
    let one128 = sf_const(b, 1, 128);
    let mask = {
        let shl = bc_shl(b, &one128, &shift7);
        bc_sub(b, &shl, &one128)
    };
    let dropped: Vec<B::Bit> = small_pre
        .iter()
        .zip(mask.iter())
        .map(|(w, m)| b.bc_and(w.clone(), m.clone()))
        .collect();
    let dropped_any = sf_or_all(b, &dropped);
    let small_any = sf_or_all(b, &small_pre);
    let sticky = {
        let t = b.bc_and(diff_ge_128.clone(), small_any);
        b.bc_or(t, dropped_any)
    };
    let mut win_small = bc_lshr(b, &small_pre, &shift7);
    let z128 = sf_zeros(b, 128);
    win_small = sf_mux(b, &diff_ge_128, &z128, &win_small);

    // Add or subtract at 129 bits (carry room).
    let big129 = sf_zext(b, &win_big, 129);
    let small129 = sf_zext(b, &win_small, 129);
    let sum129 = bc_add(b, &big129, &small129, false);
    let diff129 = bc_sub(b, &big129, &small129);
    let raw = sf_mux(b, &eff_sub, &diff129, &sum129);
    let carry = raw[128].clone();
    // On carry (add only): shift right one, sticky |= dropped bit.
    let raw_shr: Vec<B::Bit> = raw[1..129].to_vec();
    let sticky2 = b.bc_and(carry.clone(), raw[0].clone());
    let sticky2 = b.bc_or(sticky.clone(), sticky2);
    let mut win = sf_mux(b, &carry, &raw_shr, &raw[..128].to_vec());
    let one16 = sf_const(b, 1, 16);
    let exp_c = bc_add(b, &e_big16, &one16, false);
    let mut exp16 = sf_mux(b, &carry, &exp_c, &e_big16);

    // Exact zero (subtraction of equal magnitudes).
    let win_zero = {
        let o = sf_or_all(b, &win);
        b.bc_not(o)
    };

    // Normalize: left shift so leading bit is at 127.
    let lz = bc_clz(b, &win);
    let lz7: Vec<B::Bit> = lz[..7].to_vec();
    win = bc_shl(b, &win, &lz7);
    let lz16 = sf_zext(b, &lz, 16);
    exp16 = bc_sub(b, &exp16, &lz16);

    let normal = round_pack(b, sign_big.clone(), &exp16, &win, 127, sticky2, 11, 52);

    // Special cases.
    let f0 = b.bc_const(false);
    // Zero results: x + (-x) cancels to +0 (RNE), but (-0)+(-0) = -0 and
    // (+0)+(-0) = +0 — i.e. both-inputs-zero keeps sa & sb.
    let both_zero = b.bc_and(zero_a.clone(), zero_b.clone());
    let zsign = b.bc_and(pa.sign.clone(), px.sign.clone());
    let zero_both = zero_bits(b, 11, 52, zsign);
    let zero_cancel = zero_bits(b, 11, 52, f0);
    let zero_res = sf_mux(b, &both_zero, &zero_both, &zero_cancel);
    let nan = canonical_nan(b, 11, 52);
    let inf = inf_bits(b, 11, 52, inf_sign);
    let o1 = sf_mux(b, &win_zero, &zero_res, &normal);
    let o2 = sf_mux(b, &any_inf, &inf, &o1);
    sf_mux(b, &result_nan, &nan, &o2)
}

/// f64 subtraction.
pub fn f64_sub<B: BitCircuitBuilder>(b: &mut B, a: &[B::Bit], x: &[B::Bit]) -> Vec<B::Bit> {
    f64_add(b, a, x, true)
}

// ============================================================================
// Multiply
// ============================================================================

/// f64 multiply, RNE.
pub fn f64_mul<B: BitCircuitBuilder>(b: &mut B, a: &[B::Bit], x: &[B::Bit]) -> Vec<B::Bit> {
    assert_eq!(a.len(), 64);
    assert_eq!(x.len(), 64);
    let pa = unpack::<B>(a, 11, 52);
    let px = unpack::<B>(x, 11, 52);
    let sign = b.bc_xor(pa.sign.clone(), px.sign.clone());

    let nan_a = pa.is_nan(b);
    let nan_b = px.is_nan(b);
    let inf_a = pa.is_inf(b);
    let inf_b = px.is_inf(b);
    let zero_a = pa.is_zero(b);
    let zero_b = px.is_zero(b);
    let any_nan = b.bc_or(nan_a.clone(), nan_b.clone());
    // inf * 0 = NaN
    let inf_zero = {
        let t1 = b.bc_and(inf_a.clone(), zero_b.clone());
        let t2 = b.bc_and(inf_b.clone(), zero_a.clone());
        b.bc_or(t1, t2)
    };
    let result_nan = b.bc_or(any_nan.clone(), inf_zero);
    let any_inf = b.bc_or(inf_a.clone(), inf_b.clone());
    let any_zero = b.bc_or(zero_a.clone(), zero_b.clone());

    // 53-bit significands with implicit bit; subnormal trick exp->1.
    let imp_a = {
        let z = pa.exp_is_zero(b);
        b.bc_not(z)
    };
    let imp_b = {
        let z = px.exp_is_zero(b);
        b.bc_not(z)
    };
    let mut sig_a = pa.frac.clone();
    sig_a.push(imp_a);
    let mut sig_b = px.frac.clone();
    sig_b.push(imp_b);
    let one11 = sf_const(b, 1, 11);
    let pa_ez = pa.exp_is_zero(b);
    let ea = sf_mux(b, &pa_ez, &one11, &pa.exp);
    let px_ez = px.exp_is_zero(b);
    let eb = sf_mux(b, &px_ez, &one11, &px.exp);
    let ea16 = sf_zext(b, &ea, 16);
    let eb16 = sf_zext(b, &eb, 16);
    // exp_r = ea + eb - 1023 (biased-sum), then normalization subtracts lz-105
    let sum_e = bc_add(b, &ea16, &eb16, false);
    let bias = sf_const(b, 1023, 16);
    let exp0 = bc_sub(b, &sum_e, &bias);

    // 53x53 -> 106 product via 32/21 limb split, accumulated in 128 bits.
    let a_lo = sf_zext(b, &sig_a[..32], 64);
    let a_hi = sf_zext(b, &sig_a[32..], 64);
    let b_lo = sf_zext(b, &sig_b[..32], 64);
    let b_hi = sf_zext(b, &sig_b[32..], 64);
    let m = |b_: &mut B, u: &[B::Bit], v: &[B::Bit]| -> Vec<B::Bit> { crate::circuits::bc_mul(b_, u, v) };
    let p0 = m(b, &a_lo, &b_lo); // <= 64 bits
    let p1 = m(b, &a_lo, &b_hi); // <= 53 bits
    let p2 = m(b, &a_hi, &b_lo); // <= 53 bits
    let p3 = m(b, &a_hi, &b_hi); // <= 42 bits
    let sh32 = sf_const(b, 32, 7);
    let sh64 = sf_const(b, 64, 7);
    let p1w = sf_zext(b, &p1, 128);
    let p2w = sf_zext(b, &p2, 128);
    let p3w = sf_zext(b, &p3, 128);
    let t1 = bc_shl(b, &p1w, &sh32);
    let t2 = bc_shl(b, &p2w, &sh32);
    let t3 = bc_shl(b, &p3w, &sh64);
    let mut prod = sf_zext(b, &p0, 128);
    prod = bc_add(b, &prod, &t1, false);
    prod = bc_add(b, &prod, &t2, false);
    prod = bc_add(b, &prod, &t3, false); // 106-bit product

    let prod_zero = {
        let o = sf_or_all(b, &prod);
        b.bc_not(o)
    };

    // Normalize: leading bit of prod (104 or 105 for normals) to 127.
    let lz = bc_clz(b, &prod); // 128-wide count
    let lz7: Vec<B::Bit> = lz[..7].to_vec();
    let win = bc_shl(b, &prod, &lz7);
    // exp_r = ea + eb - 1023 + (22 - lz): leading bit of prod is at
    // 127 - lz; it must land at 127, and each shift position is one exponent
    // unit: exp_r = exp0 - lz + 22.
    let lz16 = sf_zext(b, &lz, 16);
    let c23 = sf_const(b, 23, 16);
    let adj = bc_sub(b, &c23, &lz16);
    let exp16 = bc_add(b, &exp0, &adj, false);

    let f0 = b.bc_const(false);
    let normal = round_pack(b, sign.clone(), &exp16, &win, 127, f0, 11, 52);

    let nan = canonical_nan(b, 11, 52);
    let inf = inf_bits(b, 11, 52, sign.clone());
    let zero = zero_bits(b, 11, 52, sign.clone());
    let o1 = sf_mux(b, &any_zero, &zero, &normal);
    let o2 = sf_mux(b, &any_inf, &inf, &o1);
    let o3 = sf_mux(b, &result_nan, &nan, &o2);
    // prod_zero without input zero is impossible for finite nonzero inputs
    // (min subnormal * min normal underflows through round_pack), but guard.
    let _ = prod_zero;
    o3
}

// ============================================================================
// Divide
// ============================================================================

/// f64 divide, RNE.
pub fn f64_div<B: BitCircuitBuilder>(b: &mut B, a: &[B::Bit], x: &[B::Bit]) -> Vec<B::Bit> {
    assert_eq!(a.len(), 64);
    assert_eq!(x.len(), 64);
    let pa = unpack::<B>(a, 11, 52);
    let px = unpack::<B>(x, 11, 52);
    let sign = b.bc_xor(pa.sign.clone(), px.sign.clone());

    let nan_a = pa.is_nan(b);
    let nan_b = px.is_nan(b);
    let inf_a = pa.is_inf(b);
    let inf_b = px.is_inf(b);
    let zero_a = pa.is_zero(b);
    let zero_b = px.is_zero(b);
    let any_nan = {
        let t = b.bc_or(nan_a.clone(), nan_b.clone());
        let ii = b.bc_and(inf_a.clone(), inf_b.clone());
        let t = b.bc_or(t, ii);
        let zz = b.bc_and(zero_a.clone(), zero_b.clone());
        b.bc_or(t, zz)
    };
    // x / 0 = inf (x finite nonzero); inf / y = inf; 0 / y = 0; x / inf = 0
    let result_inf = {
        let b_nan_or_inf = b.bc_or(nan_b.clone(), inf_b.clone());
        let b_ok = b.bc_not(b_nan_or_inf);
        let t1 = b.bc_and(inf_a.clone(), b_ok);
        let nz_a = b.bc_not(zero_a.clone());
        let a_nan_or_inf = b.bc_or(nan_a.clone(), inf_a.clone());
        let fin_a = b.bc_not(a_nan_or_inf);
        let t = b.bc_and(nz_a, fin_a);
        let t2 = b.bc_and(t, zero_b.clone());
        b.bc_or(t1, t2)
    };
    let result_zero = {
        let b_nan_or_inf = b.bc_or(nan_b.clone(), inf_b.clone());
        let fin_b = b.bc_not(b_nan_or_inf);
        let nz_b = b.bc_not(zero_b.clone());
        let t = b.bc_and(fin_b, nz_b);
        let t1 = b.bc_and(t, zero_a.clone());
        let a_nan_or_inf = b.bc_or(nan_a.clone(), inf_a.clone());
        let fin_a = b.bc_not(a_nan_or_inf);
        let t = b.bc_and(fin_a, inf_b.clone());
        let n_nan_a = b.bc_not(nan_a.clone());
        let t2 = b.bc_and(t, n_nan_a);
        b.bc_or(t1, t2)
    };

    let imp_a = {
        let z = pa.exp_is_zero(b);
        b.bc_not(z)
    };
    let imp_b = {
        let z = px.exp_is_zero(b);
        b.bc_not(z)
    };
    let mut sig_a = pa.frac.clone();
    sig_a.push(imp_a);
    let mut sig_b = px.frac.clone();
    sig_b.push(imp_b);
    let one11 = sf_const(b, 1, 11);
    let pa_ez = pa.exp_is_zero(b);
    let ea = sf_mux(b, &pa_ez, &one11, &pa.exp);
    let px_ez = px.exp_is_zero(b);
    let eb = sf_mux(b, &px_ez, &one11, &px.exp);
    // Normalize both significands (leading bit at 52) so the quotient lands
    // in [0.5, 2) — subnormal inputs otherwise overflow the fixed window.
    let lz_a = bc_clz(b, &sig_a)[..6].to_vec(); // 53-bit clz -> 0..=53
    let lz_b = bc_clz(b, &sig_b)[..6].to_vec();
    let sig_a = bc_shl(b, &sig_a, &lz_a);
    let sig_b = bc_shl(b, &sig_b, &lz_b);
    let ea16 = {
        let e = sf_zext(b, &ea, 16);
        let lz = sf_zext(b, &lz_a, 16);
        bc_sub(b, &e, &lz)
    };
    let eb16 = {
        let e = sf_zext(b, &eb, 16);
        let lz = sf_zext(b, &lz_b, 16);
        bc_sub(b, &e, &lz)
    };
    let exp0 = bc_sub(b, &ea16, &eb16); // unbiased difference (signed)
    let bias = sf_const(b, 1023, 16);
    let exp0 = bc_add(b, &exp0, &bias, false); // biased target exponent

    // Restoring division: q = floor(sig_a * 2^75 / sig_b) with the sig ratio
    // in [0.5, 2), so q's leading bit is at 74..=76.
    let sig_a128 = sf_zext(b, &sig_a, 128);
    let sh75 = sf_const(b, 75, 7);
    let dividend = bc_shl(b, &sig_a128, &sh75);
    let divisor = sf_zext(b, &sig_b, 128);
    // r entering bit 76 = dividend >> 77 (top 51 bits)
    let sh77 = sf_const(b, 77, 7);
    let mut r = bc_lshr(b, &dividend, &sh77);
    let mut q = sf_zeros(b, 128);
    // Run to bit 0 (not just 22): the final remainder must be exact since
    // it feeds the rounding sticky bit.
    for i in (0..=76usize).rev() {
        // r = (r << 1) | dividend[i]
        let mut new_r = Vec::with_capacity(128);
        new_r.push(dividend[i].clone());
        new_r.extend(r[..127].iter().cloned());
        // if r >= divisor: r -= divisor, q[i] = 1
        let ge = {
            let lt = sf_ult(b, &new_r, &divisor);
            b.bc_not(lt)
        };
        let subbed = bc_sub(b, &new_r, &divisor);
        r = sf_mux(b, &ge, &subbed, &new_r);
        q[i] = ge;
    }
    let sticky = sf_or_all(b, &r);

    // Normalize quotient (leading at 75 or 76) to window leading-127.
    let lz = bc_clz(b, &q);
    let lz7: Vec<B::Bit> = lz[..7].to_vec();
    let win = bc_shl(b, &q, &lz7);
    // q ~= sig_a/sig_b * 2^75 ; V = q << lz normalizes: exp adjustment
    // (127 - lz) - 75 = 52 - lz.
    let lz16 = sf_zext(b, &lz, 16);
    let c52 = sf_const(b, 52, 16);
    let adj = bc_sub(b, &c52, &lz16);
    let exp16 = bc_add(b, &exp0, &adj, false);

    let normal = round_pack(b, sign.clone(), &exp16, &win, 127, sticky, 11, 52);

    let nan = canonical_nan(b, 11, 52);
    let inf = inf_bits(b, 11, 52, sign.clone());
    let zero = zero_bits(b, 11, 52, sign.clone());
    let o1 = sf_mux(b, &result_zero, &zero, &normal);
    let o2 = sf_mux(b, &result_inf, &inf, &o1);
    sf_mux(b, &any_nan, &nan, &o2)
}

// ============================================================================
// min / max (wasm semantics)
// ============================================================================

/// f64 min: NaN -> canonical NaN; min(-0,+0) = -0; else the smaller.
pub fn f64_min<B: BitCircuitBuilder>(b: &mut B, a: &[B::Bit], x: &[B::Bit]) -> Vec<B::Bit> {
    let (eq, lt, un) = float_cmp(b, a, x, 11, 52);
    let _ = eq;
    let pa = unpack::<B>(a, 11, 52);
    let px = unpack::<B>(x, 11, 52);
    let za = pa.is_zero(b);
    let zx = px.is_zero(b);
    let both_zero = b.bc_and(za, zx);
    // both zero: result sign = a.sign | b.sign (i.e. -0 if either is -0)
    let zsign = b.bc_or(pa.sign.clone(), px.sign.clone());
    let zero = zero_bits(b, 11, 52, zsign);
    let sel = sf_mux(b, &lt, &a.to_vec(), &x.to_vec());
    let o1 = sf_mux(b, &both_zero, &zero, &sel);
    let nan = canonical_nan(b, 11, 52);
    sf_mux(b, &un, &nan, &o1)
}

/// f64 max: NaN -> canonical NaN; max(-0,+0) = +0; else the larger.
pub fn f64_max<B: BitCircuitBuilder>(b: &mut B, a: &[B::Bit], x: &[B::Bit]) -> Vec<B::Bit> {
    let (eq, lt, un) = float_cmp(b, a, x, 11, 52);
    let _ = eq;
    let pa = unpack::<B>(a, 11, 52);
    let px = unpack::<B>(x, 11, 52);
    let za = pa.is_zero(b);
    let zx = px.is_zero(b);
    let both_zero = b.bc_and(za, zx);
    let zsign = b.bc_and(pa.sign.clone(), px.sign.clone());
    let zero = zero_bits(b, 11, 52, zsign);
    let sel = sf_mux(b, &lt, &x.to_vec(), &a.to_vec());
    let o1 = sf_mux(b, &both_zero, &zero, &sel);
    let nan = canonical_nan(b, 11, 52);
    sf_mux(b, &un, &nan, &o1)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use volar_ir_common::PolyCoeffs;

    /// Test-only evaluator: Bit = wire index into a value trace, so wire
    /// identity (not boolean value) drives `PolyCoeffs` keys — faithful to
    /// the real emitters' SSA semantics.
    #[derive(Default)]
    struct TraceEval {
        values: Vec<bool>,
    }
    impl TraceEval {
        fn get(&self, w: u32) -> bool {
            self.values[w as usize]
        }
    }
    impl BitCircuitBuilder for TraceEval {
        type Bit = u32;
        fn bc_const(&mut self, val: bool) -> u32 {
            self.values.push(val);
            (self.values.len() - 1) as u32
        }
        fn bc_poly(&mut self, coeffs: PolyCoeffs<u32>, constant: u128) -> u32 {
            let mut acc = (constant & 1) != 0;
            for (mono, coeff) in coeffs.iter() {
                if coeff & 1 == 1 {
                    acc ^= mono.iter().all(|w| self.values[*w as usize]);
                }
            }
            self.values.push(acc);
            (self.values.len() - 1) as u32
        }
    }
    type BoolEval = TraceEval;

    fn bits_of_f64(v: f64) -> Vec<bool> {
        (0..64).map(|i| (v.to_bits() >> i) & 1 != 0).collect()
    }
    fn f64_of_bits(bits: &[bool]) -> f64 {
        let mut v = 0u64;
        for (i, b) in bits.iter().enumerate() {
            if *b {
                v |= 1 << i;
            }
        }
        f64::from_bits(v)
    }
    fn bits_of_f32(v: f32) -> Vec<bool> {
        (0..32).map(|i| (v.to_bits() >> i) & 1 != 0).collect()
    }
    fn f32_of_bits(bits: &[bool]) -> f32 {
        let mut v = 0u32;
        for (i, b) in bits.iter().enumerate() {
            if *b {
                v |= 1 << i;
            }
        }
        f32::from_bits(v)
    }

    /// Run a circuit under TraceEval: feed `inputs` as fresh wires, return
    /// the output wires' values.
    fn run(
        inputs: &[&[bool]],
        f: impl Fn(&mut TraceEval, &[Vec<u32>]) -> Vec<u32>,
    ) -> Vec<bool> {
        let mut ev = TraceEval::default();
        let mut wires: Vec<Vec<u32>> = Vec::new();
        for input in inputs {
            wires.push(input.iter().map(|&b| ev.bc_const(b)).collect());
        }
        let out = f(&mut ev, &wires);
        out.iter().map(|&w| ev.get(w)).collect()
    }

    /// Deterministic xorshift for test vectors.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
    }

    fn edge_f64s() -> Vec<f64> {
        vec![
            0.0,
            -0.0,
            1.0,
            -1.0,
            0.5,
            -0.5,
            2.0,
            1.5,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NAN,
            f64::MIN_POSITIVE,
            -f64::MIN_POSITIVE,
            f64::from_bits(1),                 // min subnormal
            f64::from_bits(0x000F_FFFF_FFFF_FFFF), // max subnormal
            f64::MAX,
            f64::MIN,
            1e-300,
            1e300,
            9007199254740992.0, // 2^53
            0.1,
            0.3,
            1.0 / 3.0,
            65536.0,
            4294967296.0,
            2147483647.0,
            2147483648.0,
            -2147483648.0,
        ]
    }

    /// Evaluate a two-input f64 circuit against native semantics.
    fn check_binop(
        name: &str,
        f: impl Fn(&mut TraceEval, &[u32], &[u32]) -> Vec<u32>,
        op: impl Fn(f64, f64) -> f64,
    ) {
        let mut rng = Lcg(0x1234_5678_9abc_def1);
        let mut cases: Vec<(f64, f64)> = Vec::new();
        let edges = edge_f64s();
        for &a in &edges {
            for &b in &edges {
                cases.push((a, b));
            }
        }
        for _ in 0..4000 {
            cases.push((f64::from_bits(rng.next()), f64::from_bits(rng.next())));
        }
        // finite-focused randoms
        for _ in 0..4000 {
            let a = f64::from_bits(rng.next() & 0x7FEF_FFFF_FFFF_FFFF);
            let b = f64::from_bits((rng.next() & 0x000F_FFFF_FFFF_FFFF) | 0x3FF0_0000_0000_0000);
            cases.push((a, b));
        }
        for (a, b) in cases {
            let av = bits_of_f64(a);
            let bv = bits_of_f64(b);
            let out = run(&[&av, &bv], |ev, ws| f(ev, &ws[0], &ws[1]));
            let got = f64_of_bits(&out);
            let want = op(a, b);
            if want.is_nan() {
                assert!(got.is_nan(), "{name}({a:e}, {b:e}): want NaN, got {got:e}");
            } else {
                assert_eq!(
                    got.to_bits(),
                    want.to_bits(),
                    "{name}({a:e}, {b:e}): got {got:e}, want {want:e}"
                );
            }
        }
    }

    #[test]
    fn f64_add_matches_native() {
        check_binop("add", |ev, a, b| f64_add(ev, a, b, false), |a, b| a + b);
    }

    #[test]
    fn f64_sub_matches_native() {
        check_binop("sub", |ev, a, b| f64_sub(ev, a, b), |a, b| a - b);
    }

    #[test]
    fn f64_mul_matches_native() {
        check_binop("mul", |ev, a, b| f64_mul(ev, a, b), |a, b| a * b);
    }

    #[test]
    fn f64_div_matches_native() {
        check_binop("div", |ev, a, b| f64_div(ev, a, b), |a, b| a / b);
    }

    #[test]
    fn f64_cmp_matches_native() {
        let mut rng = Lcg(0xdead_beef_cafe_f00d);
        let mut cases: Vec<(f64, f64)> = Vec::new();
        for &a in &edge_f64s() {
            for &b in &edge_f64s() {
                cases.push((a, b));
            }
        }
        for _ in 0..8000 {
            cases.push((f64::from_bits(rng.next()), f64::from_bits(rng.next())));
        }
        for (a, b) in cases {
            let av = bits_of_f64(a);
            let bv = bits_of_f64(b);
            let outs = run(&[&av, &bv], |ev, ws| {
                (0..6).map(|op| f64_cmp(ev, &ws[0], &ws[1], op)).collect()
            });
            let wants = [a == b, a != b, a < b, a <= b, a > b, a >= b];
            for op in 0..6 {
                assert_eq!(outs[op], wants[op], "cmp{op}({a:e}, {b:e})");
            }
        }
    }

    #[test]
    fn f64_from_i64_matches_native() {
        let mut rng = Lcg(0x0bad_c0de_1357_9bdf);
        let mut cases: Vec<i64> =
            vec![0, 1, -1, 2, i64::MAX, i64::MIN, (1 << 53) - 1, 1 << 53, (1 << 53) + 1];
        for _ in 0..8000 {
            cases.push(rng.next() as i64);
        }
        for v in cases.iter().copied() {
            let x: Vec<bool> = (0..64).map(|i| (v as u64 >> i) & 1 != 0).collect();
            let outs = run(&[&x], |ev, ws| f64_from_i64(ev, &ws[0], true));
            let got = f64_of_bits(&outs);
            assert_eq!(got.to_bits(), (v as f64).to_bits(), "from_i64({v})");
        }
        // unsigned check
        for v in cases.iter().copied().map(|v| v as u64).chain(
            [0u64, 1, u64::MAX, 1 << 53, (1 << 53) + 1].into_iter(),
        ) {
            let x: Vec<bool> = (0..64).map(|i| (v >> i) & 1 != 0).collect();
            let out = run(&[&x], |ev, ws| f64_from_i64(ev, &ws[0], false));
            let got = f64_of_bits(&out);
            assert_eq!(got.to_bits(), (v as f64).to_bits(), "from_u64({v})");
        }
    }

    #[test]
    fn i32_trunc_sat_f64_matches_native() {
        let mut rng = Lcg(0x1111_2222_3333_4444);
        let mut cases = edge_f64s();
        for _ in 0..8000 {
            cases.push(f64::from_bits(rng.next()));
        }
        for v in cases {
            let av = bits_of_f64(v);
            let out = run(&[&av], |ev, ws| i32_trunc_sat_f64(ev, &ws[0], true));
            let mut got = 0u32;
            for (i, b) in out.iter().enumerate() {
                if *b {
                    got |= 1 << i;
                }
            }
            let want = v as i32; // Rust `as` == wasm trunc_sat semantics
            assert_eq!(got, want as u32, "trunc_sat_s({v:e})");
        }
    }

    #[test]
    fn f64_round_int_matches_native() {
        let mut rng = Lcg(0x5555_aaaa_6666_bbbb);
        let mut cases = edge_f64s();
        for _ in 0..8000 {
            cases.push(f64::from_bits(rng.next()));
        }
        for v in cases {
            let av = bits_of_f64(v);
            let outs = run(&[&av], |ev, ws| {
                let mut all = Vec::new();
                for m in 0..4 {
                    all.extend(f64_round_int(ev, &ws[0], m));
                }
                all
            });
            let wants = [v.trunc(), v.floor(), v.ceil(), v.round_ties_even()];
            for mode in 0..4 {
                let got = f64_of_bits(&outs[mode * 64..(mode + 1) * 64]);
                let want = wants[mode];
                if want.is_nan() {
                    assert!(got.is_nan(), "round{mode}({v:e}): want NaN got {got:e}");
                } else {
                    assert_eq!(got.to_bits(), want.to_bits(), "round{mode}({v:e})");
                }
            }
        }
    }

    #[test]
    fn promote_demote_match_native() {
        let mut rng = Lcg(0x9999_0000_eeee_1111);
        let mut cases: Vec<f32> = vec![
            0.0, -0.0, 1.0, -1.0, 0.5, f32::INFINITY, f32::NEG_INFINITY, f32::NAN,
            f32::MIN_POSITIVE, f32::from_bits(1), f32::MAX, f32::MIN,
        ];
        for _ in 0..8000 {
            cases.push(f32::from_bits(rng.next() as u32));
        }
        for v in cases {
            let xv = bits_of_f32(v);
            let out = run(&[&xv], |ev, ws| f64_promote_f32(ev, &ws[0]));
            let got = f64_of_bits(&out);
            let want = v as f64;
            if want.is_nan() {
                assert!(got.is_nan(), "promote({v:e}): want NaN got {got:e}");
            } else {
                assert_eq!(got.to_bits(), want.to_bits(), "promote({v:e})");
            }
        }
        let mut cases64 = edge_f64s();
        for _ in 0..8000 {
            cases64.push(f64::from_bits(rng.next()));
        }
        for v in cases64 {
            let av = bits_of_f64(v);
            let out = run(&[&av], |ev, ws| f32_demote_f64(ev, &ws[0]));
            let got = f32_of_bits(&out);
            let want = v as f32;
            if want.is_nan() {
                assert!(got.is_nan(), "demote({v:e}): want NaN got {got:e}");
            } else {
                assert_eq!(got.to_bits(), want.to_bits(), "demote({v:e})");
            }
        }
    }

    #[test]
    fn f64_min_max_match_native() {
        let mut rng = Lcg(0x2468_ace0_1357_bdf9);
        let mut cases: Vec<(f64, f64)> = Vec::new();
        for &a in &edge_f64s() {
            for &b in &edge_f64s() {
                cases.push((a, b));
            }
        }
        for _ in 0..4000 {
            cases.push((f64::from_bits(rng.next()), f64::from_bits(rng.next())));
        }
        for (a, b) in cases {
            let av = bits_of_f64(a);
            let bv = bits_of_f64(b);
            let outs = run(&[&av, &bv], |ev, ws| {
                let mut all = f64_min(ev, &ws[0], &ws[1]);
                all.extend(f64_max(ev, &ws[0], &ws[1]));
                all
            });
            let got_min = f64_of_bits(&outs[..64]);
            let got_max = f64_of_bits(&outs[64..]);
            let want_min = if a.is_nan() || b.is_nan() { f64::NAN } else { a.min(b) };
            let want_max = if a.is_nan() || b.is_nan() { f64::NAN } else { a.max(b) };
            if want_min.is_nan() {
                assert!(got_min.is_nan());
            } else {
                assert_eq!(got_min.to_bits(), want_min.to_bits(), "min({a:e},{b:e})");
            }
            if want_max.is_nan() {
                assert!(got_max.is_nan());
            } else {
                assert_eq!(got_max.to_bits(), want_max.to_bits(), "max({a:e},{b:e})");
            }
        }
    }
}
