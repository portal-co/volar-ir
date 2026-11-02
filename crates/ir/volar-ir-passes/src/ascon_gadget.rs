// @reliability: normal
//! @ai: assisted
//! In-circuit Ascon-128a AEAD gadgets as Boolar building blocks, for the
//! single-party segment-state encryption (obfustopia plan
//! `2026-11-02-single-party-oram-external-obfuscation.md`, Phase B).
//!
//! The gadgets mirror `volar_spec::ascon::ascon128a_encrypt` **byte-exactly**
//! (empty associated data), so the native slice form is the reference for
//! differential tests. Keys and nonces are circuit constants; XOR with a
//! constant word is emitted as selective `Not` gates, so key whitening is
//! nearly free.
//!
//! Wire conventions:
//!
//! - A 64-bit word is 64 wires; wire `i` carries native u64 bit `i`.
//! - Blob plaintext bits pack into bytes LSB-first (blob bit `j` is bit
//!   `j % 8` of byte `j / 8`), and bytes feed `be_u64` words exactly like
//!   the native AEAD.
//! - Ciphertext and tag wires are exposed in *native bit order*: ciphertext
//!   block word bits `0..64` per word, then the tag as
//!   `state[3] (64 bits) ++ state[4] (64 bits)`. This is the blob format;
//!   byte-exact agreement with the native AEAD happens at the byte-packing
//!   helpers below, which tests verify against `volar-spec`.

use alloc::vec;
use alloc::vec::Vec;

use volar_ir::boolar::BIrStmt;
use volar_ir::circuit::BCircuit;
use volar_ir::ir::IRVarId;

// Ascon round constants and the Ascon-128a IV, mirroring
// `volar_spec::ascon::{ASCON_RC, ASCON_128A_IV}` (kept inline so this crate
// does not depend on volar-spec; the differential tests pin the agreement).
const ASCON_RC: [u64; 12] = [
    0xf0, 0xe1, 0xd2, 0xc3, 0xb4, 0xa5, 0x96, 0x87, 0x78, 0x69, 0x5a, 0x4b,
];
const ASCON_128A_IV: u64 = 0x80800c0800000000;

/// Cipher profile for blob/tree encryption. AES-128 is reserved for
/// conformance with the `oram_2pc` baseline and intentionally fails closed
/// until ported.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CipherProfile {
    /// Ascon-128a (default): cheap in-circuit sponge AEAD.
    Ascon128a,
    /// AES-128 per-node pads (reserved; not yet implemented).
    Aes128,
}

/// Gadget construction errors.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GadgetError {
    /// The AES-128 profile is reserved but not yet implemented.
    AesProfileUnimplemented,
}

impl core::fmt::Display for GadgetError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::AesProfileUnimplemented => write!(
                f,
                "AES-128 cipher profile is reserved but not yet implemented (use Ascon128a)"
            ),
        }
    }
}

/// Builder over a `BCircuit` with shared constant wires.
struct WireBuilder<'a> {
    circ: &'a mut BCircuit,
    zero: IRVarId,
    one: IRVarId,
}

impl<'a> WireBuilder<'a> {
    fn new(circ: &'a mut BCircuit) -> Self {
        let zero = circ.push_stmt(BIrStmt::Zero, ());
        let one = circ.push_stmt(BIrStmt::One, ());
        WireBuilder { circ, zero, one }
    }

    fn const_bit(&self, bit: bool) -> IRVarId {
        if bit { self.one } else { self.zero }
    }

    fn xor(&mut self, a: IRVarId, b: IRVarId) -> IRVarId {
        self.circ.push_stmt(BIrStmt::Xor(a, b), ())
    }

    fn and(&mut self, a: IRVarId, b: IRVarId) -> IRVarId {
        self.circ.push_stmt(BIrStmt::And(a, b), ())
    }

    fn not(&mut self, a: IRVarId) -> IRVarId {
        self.circ.push_stmt(BIrStmt::Not(a), ())
    }
}

type Word = [IRVarId; 64];
type State = [Word; 5];

fn const_word(b: &WireBuilder, w: u64) -> Word {
    core::array::from_fn(|i| b.const_bit((w >> i) & 1 == 1))
}

/// `rotr64(w, k)` as a wire relabeling: no gates.
fn rotr(w: &Word, k: usize) -> Word {
    core::array::from_fn(|i| w[(i + k) % 64])
}

fn xor_word(b: &mut WireBuilder, a: &Word, c: &Word) -> Word {
    let mut out = *a;
    for i in 0..64 {
        out[i] = b.xor(a[i], c[i]);
    }
    out
}

/// `a ^= constant w` as selective `Not` gates.
fn xor_const_word(b: &mut WireBuilder, a: &mut Word, w: u64) {
    for i in 0..64 {
        if (w >> i) & 1 == 1 {
            a[i] = b.not(a[i]);
        }
    }
}

fn not_word(b: &mut WireBuilder, a: &Word) -> Word {
    let mut out = *a;
    for i in 0..64 {
        out[i] = b.not(a[i]);
    }
    out
}

fn and_word_with_not(b: &mut WireBuilder, x: &Word, y: &Word) -> Word {
    // (!x) & y, computing the negation once.
    let nx = not_word(b, x);
    let mut out = *x;
    for i in 0..64 {
        out[i] = b.and(nx[i], y[i]);
    }
    out
}

/// One Ascon round on the wire state, mirroring
/// `volar_spec::ascon::ascon_round` (the algebraic form) bit for bit.
fn ascon_round_wires(b: &mut WireBuilder, s: &mut State, c: u64) {
    // x0 = s0 ^ s4 ; x2 = s2 ^ s1 ^ c ; x4 = s4 ^ s3
    let x0 = xor_word(b, &s[0], &s[4]);
    let mut x2 = xor_word(b, &s[2], &s[1]);
    xor_const_word(b, &mut x2, c);
    let x4 = xor_word(b, &s[4], &s[3]);
    // tx0 = x0 ^ (!s1 & x2) ; tx1 = s1 ^ (!x2 & s3) ; tx2 = x2 ^ (!s3 & x4)
    // tx3 = s3 ^ (!x4 & x0) ; tx4 = x4 ^ (!x0 & s1)
    let t0 = and_word_with_not(b, &s[1], &x2);
    let t1 = and_word_with_not(b, &x2, &s[3]);
    let t2 = and_word_with_not(b, &s[3], &x4);
    let t3 = and_word_with_not(b, &x4, &x0);
    let t4 = and_word_with_not(b, &x0, &s[1]);
    let tx0 = xor_word(b, &x0, &t0);
    let tx1 = xor_word(b, &s[1], &t1);
    let tx2 = xor_word(b, &x2, &t2);
    let tx3 = xor_word(b, &s[3], &t3);
    let tx4 = xor_word(b, &x4, &t4);
    // tx1 ^= tx0 ; tx3 ^= tx2 ; tx0 ^= tx4
    let tx1 = xor_word(b, &tx1, &tx0);
    let tx3 = xor_word(b, &tx3, &tx2);
    let tx0 = xor_word(b, &tx0, &tx4);
    // Linear layer: y = tx ^ rotr_a(tx); out = tx ^ rotr_b(y) (x2 negated).
    let finish = |b: &mut WireBuilder, tx: &Word, ra: usize, rb: usize| -> Word {
        let rot = rotr(tx, ra);
        let y = xor_word(b, tx, &rot);
        let rot_y = rotr(&y, rb);
        xor_word(b, tx, &rot_y)
    };
    s[0] = finish(b, &tx0, 9, 19);
    s[1] = finish(b, &tx1, 22, 39);
    let y2 = {
        let rot = rotr(&tx2, 5);
        xor_word(b, &tx2, &rot)
    };
    let y2r = rotr(&y2, 1);
    let out2 = xor_word(b, &tx2, &y2r);
    s[2] = not_word(b, &out2);
    s[3] = finish(b, &tx3, 7, 10);
    s[4] = finish(b, &tx4, 34, 7);
}

fn ascon_p12_wires(b: &mut WireBuilder, s: &mut State) {
    for round in 0..12 {
        ascon_round_wires(b, s, ASCON_RC[round]);
    }
}

fn ascon_p8_wires(b: &mut WireBuilder, s: &mut State) {
    for round in 4..12 {
        ascon_round_wires(b, s, ASCON_RC[round]);
    }
}

/// Load blob bits `8*byte .. 8*byte + 64` as one `be_u64` word: word bit
/// `i` = bit `(i % 8)` of byte `7 - i/8` = blob bit
/// `8*byte + 8*(7 - i/8) + (i % 8)`. Saturating: blob bits beyond
/// `data.len()` (the final partial byte's padding) load as `zero`.
fn load_word(data: &[IRVarId], byte: usize, zero: IRVarId) -> Word {
    core::array::from_fn(|i| {
        let bit = 8 * byte + 8 * (7 - i / 8) + (i % 8);
        if bit < data.len() { data[bit] } else { zero }
    })
}

/// The reverse mapping: word bit `i` yields blob bit
/// `8*byte + 8*(7 - i/8) + (i % 8)`. Saturating: blob bits beyond the output
/// length are dropped (they are padding positions).
fn store_word(word: &Word, byte: usize, out: &mut [Option<IRVarId>]) {
    for i in 0..64 {
        let bit = 8 * byte + 8 * (7 - i / 8) + (i % 8);
        if bit < out.len() {
            out[bit] = Some(word[i]);
        }
    }
}

/// Blob length in bytes (LSB-first packing).
fn blob_bytes(plaintext_bits: usize) -> usize {
    plaintext_bits.div_ceil(8)
}

/// The 0x80 pad right after `n` high-end bytes of a word: bit `63 - 8n`.
fn aead_pad_bit(n: usize) -> usize {
    63 - 8 * n
}

/// Append Ascon-128a encryption (empty AD) to `circ`.
///
/// `plaintext` are the blob plaintext wires; returns the ciphertext wires
/// (same count as `plaintext`, zero-padded to whole bytes) and the 128 tag
/// wires, in the blob wire format documented at the top of this module.
/// Nonce bytes (byte order, as in the AEAD) as state words.
fn nonce_words(b: &WireBuilder, nonce: &[u8; 16]) -> [Word; 2] {
    [
        const_word(b, u64::from_be_bytes(nonce[..8].try_into().expect("nonce half"))),
        const_word(b, u64::from_be_bytes(nonce[8..].try_into().expect("nonce half"))),
    ]
}

/// Wire-carried nonce (byte order, like the tag format) as state words:
/// word bit `i` = nonce byte `7 - i/8`, bit `i % 8`.
fn nonce_wires_to_words(nonce: &[IRVarId; 128]) -> [Word; 2] {
    core::array::from_fn(|w| {
        core::array::from_fn(|i| nonce[64 * w + 8 * (7 - i / 8) + (i % 8)])
    })
}

pub fn ascon128a_encrypt_wires(
    circ: &mut BCircuit,
    key: &[u8; 16],
    nonce: &[u8; 16],
    plaintext: &[IRVarId],
) -> (Vec<IRVarId>, [IRVarId; 128]) {
    let b = &mut WireBuilder::new(circ);
    let nw = nonce_words(b, nonce);
    ascon128a_encrypt_inner(circ, key, nw, plaintext)
}

/// [`ascon128a_encrypt_wires`] with the nonce carried on wires (public but
/// runtime-dynamic values, e.g. ORAM slot nonces read from the tree).
pub fn ascon128a_encrypt_wires_with_nonce(
    circ: &mut BCircuit,
    key: &[u8; 16],
    nonce: &[IRVarId; 128],
    plaintext: &[IRVarId],
) -> (Vec<IRVarId>, [IRVarId; 128]) {
    ascon128a_encrypt_inner(circ, key, nonce_wires_to_words(nonce), plaintext)
}

fn ascon128a_encrypt_inner(
    circ: &mut BCircuit,
    key: &[u8; 16],
    nonce_words: [Word; 2],
    plaintext: &[IRVarId],
) -> (Vec<IRVarId>, [IRVarId; 128]) {
    let b = &mut WireBuilder::new(circ);
    let k1 = u64::from_be_bytes(key[..8].try_into().expect("key half"));
    let k2 = u64::from_be_bytes(key[8..].try_into().expect("key half"));
    let mut state: State = [
        const_word(b, ASCON_128A_IV),
        const_word(b, k1),
        const_word(b, k2),
        nonce_words[0],
        nonce_words[1],
    ];
    ascon_p12_wires(b, &mut state);
    xor_const_word(b, &mut state[3], k1);
    xor_const_word(b, &mut state[4], k2);
    // Empty AD: no absorb blocks. Domain separation.
    xor_const_word(b, &mut state[4], 1);

    let n_bytes = blob_bytes(plaintext.len());
    let mut ciphertext: Vec<Option<IRVarId>> = vec![None; plaintext.len()];
    let mut byte = 0usize;
    // Full 16-byte blocks. (Loads/stores saturate at the final partial
    // byte's bit length, so no separate bit-level handling is needed.)
    while n_bytes - byte >= 16 {
        let m0 = load_word(plaintext, byte, b.zero);
        state[0] = xor_word(b, &state[0], &m0);
        store_word(&state[0], byte, &mut ciphertext);
        let m1 = load_word(plaintext, byte + 8, b.zero);
        state[1] = xor_word(b, &state[1], &m1);
        store_word(&state[1], byte + 8, &mut ciphertext);
        ascon_p8_wires(b, &mut state);
        byte += 16;
    }
    // Tail block. Ascon-128a always pads: even an empty or block-aligned
    // plaintext absorbs `aead_pad(0)` into state[0].
    let rem = n_bytes - byte;
    {
        if rem == 0 {
            let pad = aead_pad_bit(0);
            state[0][pad] = b.xor(state[0][pad], b.one);
        } else if rem >= 8 {
            // Full first tail word.
            let m0 = load_word(plaintext, byte, b.zero);
            state[0] = xor_word(b, &state[0], &m0);
            store_word(&state[0], byte, &mut ciphertext);
            // The second word takes the pad even when empty (rest == 0);
            // ciphertext bytes are the post-absorb keystream, and the pad
            // bit sits at byte `rest` — beyond the emitted bytes — so
            // storing before the pad XOR is exact.
            let rest = rem - 8;
            if rest > 0 {
                let m1 = load_word(plaintext, byte + 8, b.zero);
                state[1] = xor_word(b, &state[1], &m1);
                store_word(&state[1], byte + 8, &mut ciphertext);
            }
            let pad = aead_pad_bit(rest);
            state[1][pad] = b.xor(state[1][pad], b.one);
        } else {
            // Partial first word only.
            let m0 = load_word(plaintext, byte, b.zero);
            state[0] = xor_word(b, &state[0], &m0);
            let pad = aead_pad_bit(rem);
            state[0][pad] = b.xor(state[0][pad], b.one);
            store_word(&state[0], byte, &mut ciphertext);
        }
    }
    // Finalize.
    xor_const_word(b, &mut state[2], k1);
    xor_const_word(b, &mut state[3], k2);
    ascon_p12_wires(b, &mut state);
    xor_const_word(b, &mut state[3], k1);
    xor_const_word(b, &mut state[4], k2);
    // Tag wires in byte order: tag byte j bit k = word bit 56-8j+k, so
    // LSB-first byte packing of the tag wires yields the native BE tag.
    let tag: [IRVarId; 128] = core::array::from_fn(|i| {
        let word_bit = 8 * (7 - (i % 64) / 8) + (i % 8);
        if i < 64 {
            state[3][word_bit]
        } else {
            state[4][word_bit]
        }
    });
    let ciphertext: Vec<IRVarId> = ciphertext
        .into_iter()
        .map(|c| c.expect("every plaintext bit produces a ciphertext bit"))
        .collect();
    (ciphertext, tag)
}

/// Append Ascon-128a decryption (empty AD) to `circ`.
///
/// `ciphertext` and `tag` are wires in the blob wire format. Returns the
/// plaintext wires and a **poison** wire that is 1 iff the tag check fails
/// — wire it into the hardened poison accumulator so tampered state makes
/// the segment the identity.
pub fn ascon128a_decrypt_wires(
    circ: &mut BCircuit,
    key: &[u8; 16],
    nonce: &[u8; 16],
    plaintext_bits: usize,
    ciphertext: &[IRVarId],
    tag: &[IRVarId; 128],
) -> (Vec<IRVarId>, IRVarId) {
    assert_eq!(
        ciphertext.len(),
        plaintext_bits,
        "ciphertext and plaintext bit counts agree (blob format)"
    );
    let b = &mut WireBuilder::new(circ);
    let nw = nonce_words(b, nonce);
    ascon128a_decrypt_inner(circ, key, nw, plaintext_bits, ciphertext, tag)
}

/// [`ascon128a_decrypt_wires`] with the nonce carried on wires.
pub fn ascon128a_decrypt_wires_with_nonce(
    circ: &mut BCircuit,
    key: &[u8; 16],
    nonce: &[IRVarId; 128],
    plaintext_bits: usize,
    ciphertext: &[IRVarId],
    tag: &[IRVarId; 128],
) -> (Vec<IRVarId>, IRVarId) {
    ascon128a_decrypt_inner(circ, key, nonce_wires_to_words(nonce), plaintext_bits, ciphertext, tag)
}

#[allow(clippy::too_many_arguments)]
fn ascon128a_decrypt_inner(
    circ: &mut BCircuit,
    key: &[u8; 16],
    nonce_words: [Word; 2],
    plaintext_bits: usize,
    ciphertext: &[IRVarId],
    tag: &[IRVarId; 128],
) -> (Vec<IRVarId>, IRVarId) {
    assert_eq!(
        ciphertext.len(),
        plaintext_bits,
        "ciphertext and plaintext bit counts agree (blob format)"
    );
    let b = &mut WireBuilder::new(circ);
    let k1 = u64::from_be_bytes(key[..8].try_into().expect("key half"));
    let k2 = u64::from_be_bytes(key[8..].try_into().expect("key half"));
    let mut state: State = [
        const_word(b, ASCON_128A_IV),
        const_word(b, k1),
        const_word(b, k2),
        nonce_words[0],
        nonce_words[1],
    ];
    ascon_p12_wires(b, &mut state);
    xor_const_word(b, &mut state[3], k1);
    xor_const_word(b, &mut state[4], k2);
    xor_const_word(b, &mut state[4], 1);

    let n_bytes = blob_bytes(plaintext_bits);
    let mut plaintext: Vec<Option<IRVarId>> = vec![None; plaintext_bits];
    let mut byte = 0usize;
    // Full 16-byte blocks: pt = ct ^ state; then state = ct. (Padding-bit
    // positions carry no ciphertext wire; the state keeps the keystream
    // there, which equals ct ^ 0 — byte-exact with the padded native form.)
    while n_bytes - byte >= 16 {
        let c0 = load_word(ciphertext, byte, b.zero);
        let p0 = xor_word(b, &c0, &state[0]);
        store_word(&p0, byte, &mut plaintext);
        state[0] = c0;
        let c1 = load_word(ciphertext, byte + 8, b.zero);
        let p1 = xor_word(b, &c1, &state[1]);
        store_word(&p1, byte + 8, &mut plaintext);
        state[1] = c1;
        ascon_p8_wires(b, &mut state);
        byte += 16;
    }
    // Partial tail, mirroring the encrypt side: pt = ct ^ keystream; the
    // covered bytes of the state word are replaced by ct; the pad goes to
    // state[0] when rem < 8 and to state[1] otherwise (even when rest == 0).
    // Tail block: the pad is absorbed even for empty/aligned plaintext.
    let rem = n_bytes - byte;
    {
        if rem == 0 {
            let pad = aead_pad_bit(0);
            state[0][pad] = b.xor(state[0][pad], b.one);
        } else if rem >= 8 {
            let c0 = load_word(ciphertext, byte, b.zero);
            let p0 = xor_word(b, &c0, &state[0]);
            store_word(&p0, byte, &mut plaintext);
            state[0] = c0;
            let rest = rem - 8;
            if rest > 0 {
                let c1 = load_word(ciphertext, byte + 8, b.zero);
                let p1 = xor_word(b, &c1, &state[1]);
                store_word(&p1, byte + 8, &mut plaintext);
                // Covered bits are replaced by ct; padding positions keep
                // the keystream (equal to ct ^ 0 there).
                for i in 0..64 {
                    let bit = 8 * (byte + 8) + 8 * (7 - i / 8) + (i % 8);
                    if bit < ciphertext.len() {
                        state[1][i] = c1[i];
                    }
                }
            }
            let pad = aead_pad_bit(rest);
            state[1][pad] = b.xor(state[1][pad], b.one);
        } else {
            let c0 = load_word(ciphertext, byte, b.zero);
            let p0 = xor_word(b, &c0, &state[0]);
            store_word(&p0, byte, &mut plaintext);
            for i in 0..64 {
                let bit = 8 * byte + 8 * (7 - i / 8) + (i % 8);
                if bit < ciphertext.len() {
                    state[0][i] = c0[i];
                }
            }
            let pad = aead_pad_bit(rem);
            state[0][pad] = b.xor(state[0][pad], b.one);
        }
    }
    // Finalize and check the tag.
    xor_const_word(b, &mut state[2], k1);
    xor_const_word(b, &mut state[3], k2);
    ascon_p12_wires(b, &mut state);
    xor_const_word(b, &mut state[3], k1);
    xor_const_word(b, &mut state[4], k2);
    // poison = OR over bits of (computed_tag ^ given_tag)
    let mut poison: Option<IRVarId> = None;
    for i in 0..128 {
        // Same byte-order mapping as the encrypt side's tag emission.
        let word_bit = 8 * (7 - (i % 64) / 8) + (i % 8);
        let computed = if i < 64 {
            state[3][word_bit]
        } else {
            state[4][word_bit]
        };
        let diff = b.xor(computed, tag[i]);
        poison = Some(match poison {
            None => diff,
            Some(acc) => b.circ.push_stmt(BIrStmt::Or(acc, diff), ()),
        });
    }
    let plaintext: Vec<IRVarId> = plaintext
        .into_iter()
        .map(|p| p.expect("every ciphertext bit yields a plaintext bit"))
        .collect();
    (plaintext, poison.expect("128 tag bits always yield a poison wire"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Evaluate a pure-boolean BCircuit on the given param bits; returns the
    /// full var space.
    fn eval(circ: &BCircuit, params: &[bool]) -> Vec<bool> {
        let mut vals = alloc::vec![false; circ.var_space() as usize];
        vals[..params.len()].copy_from_slice(params);
        for (i, node) in circ.stmts.iter().enumerate() {
            let v = circ.params as usize + i;
            vals[v] = match &node.kind {
                BIrStmt::Zero => false,
                BIrStmt::One => true,
                BIrStmt::And(a, b) => vals[a.0 as usize] & vals[b.0 as usize],
                BIrStmt::Or(a, b) => vals[a.0 as usize] | vals[b.0 as usize],
                BIrStmt::Xor(a, b) => vals[a.0 as usize] ^ vals[b.0 as usize],
                BIrStmt::Not(a) => !vals[a.0 as usize],
                other => panic!("gadget circuits are pure: {other:?}"),
            };
        }
        vals
    }

    /// Pack bits LSB-first into bytes (blob byte format).
    fn pack_bytes(bits: &[bool]) -> Vec<u8> {
        let mut out = alloc::vec![0u8; bits.len().div_ceil(8)];
        for (j, bit) in bits.iter().enumerate() {
            if *bit {
                out[j / 8] |= 1 << (j % 8);
            }
        }
        out
    }

    fn unpack_bytes(bytes: &[u8], bits: usize) -> Vec<bool> {
        (0..bits).map(|j| (bytes[j / 8] >> (j % 8)) & 1 == 1).collect()
    }

    /// Build an encrypt circuit for `bits` plaintext bits and evaluate it.
    fn encrypt_in_circuit(key: &[u8; 16], nonce: &[u8; 16], pt: &[bool]) -> (Vec<bool>, Vec<bool>) {
        let mut circ = BCircuit::new(pt.len() as u32);
        let wires: Vec<IRVarId> = (0..pt.len() as u32).map(IRVarId).collect();
        let (ct, tag) = ascon128a_encrypt_wires(&mut circ, key, nonce, &wires);
        let vals = eval(&circ, pt);
        let ct_bits = ct.iter().map(|v| vals[v.0 as usize]).collect();
        let tag_bits = tag.iter().map(|v| vals[v.0 as usize]).collect();
        (ct_bits, tag_bits)
    }

    #[test]
    fn encrypt_gadget_matches_native_ascon128a() {
        let key = [0x42u8; 16];
        let nonce = [0x24u8; 16];
        for bits in [1usize, 7, 8, 9, 63, 64, 65, 127, 128, 129, 200] {
            let pt: Vec<bool> = (0..bits).map(|j| (j * 37 + 11) % 5 < 2).collect();
            let (ct_bits, tag_bits) = encrypt_in_circuit(&key, &nonce, &pt);
            let (ct_native, tag_native) =
                volar_spec::ascon::ascon128a_encrypt(&key, &nonce, &[], &pack_bytes(&pt));
            // The blob format carries exactly `bits` ciphertext bits; the
            // native form's padding bits are keystream ^ 0 and are not
            // emitted. Compare bit-level over the emitted prefix.
            assert_eq!(
                unpack_bytes(&ct_native, bits),
                ct_bits,
                "ciphertext mismatch at {bits} bits"
            );
            assert_eq!(
                pack_bytes(&tag_bits),
                tag_native.as_slice(),
                "tag mismatch at {bits} bits"
            );
        }
    }

    #[test]
    fn decrypt_gadget_round_trips_and_flags_tampering() {
        let key = [0x77u8; 16];
        let nonce = [0x11u8; 16];
        for bits in [3usize, 64, 130] {
            let pt: Vec<bool> = (0..bits).map(|j| (j * 13 + 5) % 3 == 0).collect();
            let (ct_bits, tag_bits) = encrypt_in_circuit(&key, &nonce, &pt);

            let mut circ = BCircuit::new((bits + 128) as u32);
            let ct_wires: Vec<IRVarId> = (0..bits as u32).map(IRVarId).collect();
            let tag_wires: [IRVarId; 128] =
                core::array::from_fn(|i| IRVarId(bits as u32 + i as u32));
            let (out, poison) =
                ascon128a_decrypt_wires(&mut circ, &key, &nonce, bits, &ct_wires, &tag_wires);

            let mut input = ct_bits.clone();
            input.extend_from_slice(&tag_bits);
            let vals = eval(&circ, &input);
            let got: Vec<bool> = out.iter().map(|v| vals[v.0 as usize]).collect();
            assert_eq!(got, pt, "round trip at {bits} bits");
            assert!(!vals[poison.0 as usize], "clean tag must not poison");

            // Flip one ciphertext bit: decryption must poison.
            let mut tampered = input.clone();
            tampered[0] = !tampered[0];
            let vals = eval(&circ, &tampered);
            assert!(vals[poison.0 as usize], "tampered ciphertext must poison");
        }
    }

    #[test]
    fn round_gadget_matches_native_ascon_round() {
        // One round over fully-parametric state; compare against the native
        // algebraic round on deterministic inputs.
        let mut circ = BCircuit::new(320);
        let mut state: State = core::array::from_fn(|w| {
            core::array::from_fn(|i| IRVarId((64 * w + i) as u32))
        });
        {
            let b = &mut WireBuilder::new(&mut circ);
            ascon_round_wires(b, &mut state, ASCON_RC[0]);
        }
        for trial in 0u64..4 {
            let input: Vec<bool> = (0..320)
                .map(|i| {
                    (trial
                        .wrapping_mul(0x9e3779b97f4a7c15)
                        .wrapping_add(i as u64)
                        .wrapping_mul(0x2545f4914f6cdd1d)
                        >> 63)
                        == 1
                })
                .collect();
            let vals = eval(&circ, &input);
            let native_in: [u64; 5] = core::array::from_fn(|w| {
                (0..64).fold(0u64, |acc, i| {
                    acc | ((input[64 * w + i] as u64) << i)
                })
            });
            let native_out = volar_spec::ascon::ascon_round(native_in, ASCON_RC[0]);
            for w in 0..5 {
                let got = (0..64).fold(0u64, |acc, i| {
                    acc | ((vals[state[w][i].0 as usize] as u64) << i)
                });
                assert_eq!(got, native_out[w], "trial {trial} word {w}");
            }
        }
    }

// probe: compare init state handling
#[test]
fn probe_init_state_matches() {
    use super::*;
    let key = [0x42u8; 16];
    let nonce = [0x24u8; 16];
    // Gadget: encrypt 0 bits (empty plaintext) — state right before
    // finalization absorbs nothing; probe via the tag only.
    let mut circ = BCircuit::new(0);
    let (_ct, tag) = ascon128a_encrypt_wires(&mut circ, &key, &nonce, &[]);
    let vals = eval(&circ, &[]);
    let tag_bits: Vec<bool> = tag.iter().map(|v| vals[v.0 as usize]).collect();
    let (_ct_n, tag_n) = volar_spec::ascon::ascon128a_encrypt(&key, &nonce, &[], &[]);
    assert_eq!(pack_bytes(&tag_bits), tag_n.as_slice(), "empty-pt tag");
}


    #[test]
    fn probe_p12_matches_native() {
        let mut circ = BCircuit::new(320);
        let mut state: State =
            core::array::from_fn(|w| core::array::from_fn(|i| IRVarId((64 * w + i) as u32)));
        {
            let b = &mut WireBuilder::new(&mut circ);
            ascon_p12_wires(b, &mut state);
        }
        let key = [0x42u8; 16];
        let nonce = [0x24u8; 16];
        let native_in = [
            ASCON_128A_IV,
            u64::from_be_bytes(key[..8].try_into().unwrap()),
            u64::from_be_bytes(key[8..].try_into().unwrap()),
            u64::from_be_bytes(nonce[..8].try_into().unwrap()),
            u64::from_be_bytes(nonce[8..].try_into().unwrap()),
        ];
        let input: Vec<bool> = native_in
            .iter()
            .flat_map(|w| (0..64).map(move |i| (w >> i) & 1 == 1))
            .collect();
        let vals = eval(&circ, &input);
        let mut native = native_in;
        volar_spec::ascon::ascon_p12(&mut native);
        for w in 0..5 {
            let got =
                (0..64).fold(0u64, |acc, i| acc | ((vals[state[w][i].0 as usize] as u64) << i));
            assert_eq!(got, native[w], "p12 word {w}");
        }
    }


    #[test]
    fn probe_finalize_state_matches() {
        // Replicate the encrypt flow for empty plaintext, outputting the
        // raw state words before tag mapping.
        let key = [0x42u8; 16];
        let nonce = [0x24u8; 16];
        let k1 = u64::from_be_bytes(key[..8].try_into().unwrap());
        let k2 = u64::from_be_bytes(key[8..].try_into().unwrap());
        let mut circ = BCircuit::new(0);
        let mut state: State = [
            const_word(&WireBuilder::new(&mut circ), 0), // placeholder, replaced below
            const_word(&WireBuilder::new(&mut circ), 0),
            const_word(&WireBuilder::new(&mut circ), 0),
            const_word(&WireBuilder::new(&mut circ), 0),
            const_word(&WireBuilder::new(&mut circ), 0),
        ];
        {
            let b = &mut WireBuilder::new(&mut circ);
            state = [
                const_word(b, ASCON_128A_IV),
                const_word(b, k1),
                const_word(b, k2),
                const_word(b, u64::from_be_bytes(nonce[..8].try_into().unwrap())),
                const_word(b, u64::from_be_bytes(nonce[8..].try_into().unwrap())),
            ];
            ascon_p12_wires(b, &mut state);
            xor_const_word(b, &mut state[3], k1);
            xor_const_word(b, &mut state[4], k2);
            xor_const_word(b, &mut state[4], 1);
            xor_const_word(b, &mut state[2], k1);
            xor_const_word(b, &mut state[3], k2);
            ascon_p12_wires(b, &mut state);
            xor_const_word(b, &mut state[3], k1);
            xor_const_word(b, &mut state[4], k2);
        }
        let vals = eval(&circ, &[]);
        let mut native = [
            ASCON_128A_IV,
            k1,
            k2,
            u64::from_be_bytes(nonce[..8].try_into().unwrap()),
            u64::from_be_bytes(nonce[8..].try_into().unwrap()),
        ];
        volar_spec::ascon::ascon_p12(&mut native);
        native[3] ^= k1;
        native[4] ^= k2;
        native[4] ^= 1;
        native[2] ^= k1;
        native[3] ^= k2;
        volar_spec::ascon::ascon_p12(&mut native);
        native[3] ^= k1;
        native[4] ^= k2;
        for w in 0..5 {
            let got =
                (0..64).fold(0u64, |acc, i| acc | ((vals[state[w][i].0 as usize] as u64) << i));
            assert_eq!(got, native[w], "finalize word {w}");
        }
    }


    #[test]
    fn probe_one_bit_tail() {
        let key = [0x42u8; 16];
        let nonce = [0x24u8; 16];
        let pt = [true];
        let (ct_bits, tag_bits) = encrypt_in_circuit(&key, &nonce, &pt);
        let (ct_n, tag_n) = volar_spec::ascon::ascon128a_encrypt(&key, &nonce, &[], &[0x01]);
        // Bit-level comparison: the blob format carries exactly L ciphertext
        // bits; the native form carries ceil(L/8) bytes (padding bits are
        // keystream ^ 0). The first L bits must agree.
        assert_eq!(unpack_bytes(&ct_n, 1), ct_bits, "ct");
        assert_eq!(pack_bytes(&tag_bits), tag_n.as_slice(), "tag");
    }

    #[test]
    fn wire_nonce_matches_const_nonce() {
        let key = [0x42u8; 16];
        let nonce = [0x24u8; 16];
        let pt: Vec<bool> = (0..37).map(|j| j % 3 == 1).collect();
        let (ct_c, tag_c) = encrypt_in_circuit(&key, &nonce, &pt);

        // Same, but with the nonce as 128 input wires (byte order).
        let mut circ = BCircuit::new((pt.len() + 128) as u32);
        let pt_wires: Vec<IRVarId> = (0..pt.len() as u32).map(IRVarId).collect();
        let nonce_wires: [IRVarId; 128] =
            core::array::from_fn(|i| IRVarId(pt.len() as u32 + i as u32));
        let (ct_w, tag_w) =
            ascon128a_encrypt_wires_with_nonce(&mut circ, &key, &nonce_wires, &pt_wires);
        let mut input = pt.clone();
        input.extend(unpack_bytes(&nonce, 128));
        let vals = eval(&circ, &input);
        let ct_bits: Vec<bool> = ct_w.iter().map(|v| vals[v.0 as usize]).collect();
        let tag_bits: Vec<bool> = tag_w.iter().map(|v| vals[v.0 as usize]).collect();
        assert_eq!(ct_bits, ct_c);
        assert_eq!(pack_bytes(&tag_bits), pack_bytes(&tag_c));
    }

    #[test]
    fn unpack_pack_round_trip() {
        let bits: Vec<bool> = (0..100).map(|j| j % 3 == 1).collect();
        assert_eq!(unpack_bytes(&pack_bytes(&bits), 100), bits);
    }
}
