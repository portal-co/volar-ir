//! Minimal 256-bit word helpers for the test interpreters. Little-endian
//! limbs; only the operations the whitelisted subsets need.

/// A 256-bit word as four little-endian 64-bit limbs.
pub type Word = [u64; 4];

/// All-zero word.
pub const ZERO: Word = [0; 4];

/// Lane-wise AND.
pub fn and(a: Word, b: Word) -> Word {
    [a[0] & b[0], a[1] & b[1], a[2] & b[2], a[3] & b[3]]
}

/// Lane-wise OR.
pub fn or(a: Word, b: Word) -> Word {
    [a[0] | b[0], a[1] | b[1], a[2] | b[2], a[3] | b[3]]
}

/// Lane-wise XOR.
pub fn xor(a: Word, b: Word) -> Word {
    [a[0] ^ b[0], a[1] ^ b[1], a[2] ^ b[2], a[3] ^ b[3]]
}

/// Lane-wise inversion.
pub fn not(a: Word) -> Word {
    [!a[0], !a[1], !a[2], !a[3]]
}

/// Wrapping 256-bit addition (pointer arithmetic in the whitelist).
pub fn add(a: Word, b: Word) -> Word {
    let mut out = [0u64; 4];
    let mut carry = false;
    for (i, limb) in out.iter_mut().enumerate() {
        let (s, c1) = a[i].overflowing_add(b[i]);
        let (s, c2) = s.overflowing_add(u64::from(carry));
        *limb = s;
        carry = c1 || c2;
    }
    out
}

/// A word holding one small value.
pub fn from_u64(value: u64) -> Word {
    [value, 0, 0, 0]
}

/// The value as `u64`, or `None` when any upper limb is nonzero.
pub fn as_u64(word: Word) -> Option<u64> {
    (word[1] == 0 && word[2] == 0 && word[3] == 0).then_some(word[0])
}

/// Lane `lane` of the packed word (lane zero = least-significant bit).
pub fn bit(word: Word, lane: usize) -> bool {
    word[lane / 64] >> (lane % 64) & 1 != 0
}

/// Set lane `lane` of the packed word.
pub fn set_bit(word: &mut Word, lane: usize) {
    word[lane / 64] |= 1 << (lane % 64);
}
