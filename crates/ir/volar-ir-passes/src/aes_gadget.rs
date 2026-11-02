// @reliability: normal
//! @ai: assisted
//! In-circuit AES-128 EtM AEAD (`CipherProfile::Aes128`), mirroring
//! `ascon_gadget.rs`: appends Boolar statement gadgets to a `BCircuit` for
//! the deterministic encrypt-then-MAC construction of
//! `volar_spec::aes128aead` (CTR with the full 128-bit BE counter over the
//! nonce; CMAC per SP 800-38B over `nonce ‖ ct ‖ be64(bit_len)`; role keys
//! `K_e = AES_K(0)`, `K_m = AES_K(1)`).
//!
//! ## Structure
//!
//! - The key is a build-time constant, so the whole key schedule is
//!   constant-folded: `K_e`/`K_m` and their round keys, and the CMAC
//!   subkeys `K1`/`K2`, are computed natively (the inline AES below) and
//!   embedded as constants. Only state S-boxes run in-circuit.
//! - The S-box is algebraic: inversion in GF(2^8) (mod `0x11B`) via the
//!   `x^254` addition chain (Frobenius squares are linear, six generic
//!   multiplications) followed by the FIPS affine map.
//! - Byte-granular only: `GadgetError::NotByteAligned` when the plaintext
//!   bit count is not a multiple of 8 (bit-granular blobs are the Ascon
//!   profile's domain; the padding-positions lesson of
//!   `ascon_gadget` applies — native sides reconstruct whole bytes).
//!
//! ## Wire format
//!
//! Identical to `ascon_gadget`: blob bits are LSB-first within each byte,
//! bytes in stream order; a blob is `ct ‖ tag` with 128 tag wires in byte
//! order. Nonce wires (the `_with_nonce` variants) are the same 128-bit
//! byte-order form.

use alloc::vec::Vec;

use volar_ir::boolar::BIrStmt;
use volar_ir::circuit::BCircuit;
use volar_ir::ir::IRVarId;

use crate::ascon_gadget::GadgetError;

// ---------------------------------------------------------------------------
// Native AES-128 for build-time constant folding, mirroring
// `volar_spec::faest::aes` (kept inline so this crate does not depend on
// volar-spec; the differential tests pin the agreement).
// ---------------------------------------------------------------------------

const SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab,
    0x76, 0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4,
    0x72, 0xc0, 0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71,
    0xd8, 0x31, 0x15, 0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2,
    0xeb, 0x27, 0xb2, 0x75, 0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6,
    0xb3, 0x29, 0xe3, 0x2f, 0x84, 0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb,
    0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf, 0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45,
    0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8, 0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5,
    0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2, 0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44,
    0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73, 0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a,
    0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb, 0xe0, 0x32, 0x3a, 0x0a, 0x49,
    0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79, 0xe7, 0xc8, 0x37, 0x6d,
    0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08, 0xba, 0x78, 0x25,
    0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a, 0x70, 0x3e,
    0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e, 0xe1,
    0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb,
    0x16,
];

const RCON: [u8; 11] = [
    0x00, 0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0x1b, 0x36,
];

fn native_gf_mul(mut a: u8, mut b: u8) -> u8 {
    let mut p = 0u8;
    for _ in 0..8 {
        if b & 1 == 1 {
            p ^= a;
        }
        let hi = a & 0x80;
        a <<= 1;
        if hi != 0 {
            a ^= 0x1b;
        }
        b >>= 1;
    }
    p
}

fn native_key_expansion(key: &[u8; 16]) -> [[u8; 16]; 11] {
    let mut words = [[0u8; 4]; 44];
    for i in 0..4 {
        words[i] = [key[4 * i], key[4 * i + 1], key[4 * i + 2], key[4 * i + 3]];
    }
    for i in 4..44 {
        let mut temp = words[i - 1];
        if i % 4 == 0 {
            temp = [temp[1], temp[2], temp[3], temp[0]];
            for b in temp.iter_mut() {
                *b = SBOX[*b as usize];
            }
            temp[0] ^= RCON[i / 4];
        }
        for b in 0..4 {
            words[i][b] = words[i - 4][b] ^ temp[b];
        }
    }
    let mut round_keys = [[0u8; 16]; 11];
    for (r, rk) in round_keys.iter_mut().enumerate() {
        for c in 0..4 {
            let w = words[4 * r + c];
            rk[4 * c] = w[0];
            rk[4 * c + 1] = w[1];
            rk[4 * c + 2] = w[2];
            rk[4 * c + 3] = w[3];
        }
    }
    round_keys
}

fn native_encrypt_block(round_keys: &[[u8; 16]; 11], plain: &[u8; 16]) -> [u8; 16] {
    let mut state = *plain;
    for i in 0..16 {
        state[i] ^= round_keys[0][i];
    }
    for r in 1..10 {
        for b in state.iter_mut() {
            *b = SBOX[*b as usize];
        }
        // ShiftRows (column-major state[r + 4c]).
        let mut tmp = state;
        for r_ in 0..4 {
            for c in 0..4 {
                tmp[r_ + 4 * c] = state[r_ + 4 * ((c + r_) % 4)];
            }
        }
        state = tmp;
        // MixColumns.
        for c in 0..4 {
            let i = 4 * c;
            let (s0, s1, s2, s3) = (state[i], state[i + 1], state[i + 2], state[i + 3]);
            state[i] = native_gf_mul(s0, 2) ^ native_gf_mul(s1, 3) ^ s2 ^ s3;
            state[i + 1] = s0 ^ native_gf_mul(s1, 2) ^ native_gf_mul(s2, 3) ^ s3;
            state[i + 2] = s0 ^ s1 ^ native_gf_mul(s2, 2) ^ native_gf_mul(s3, 3);
            state[i + 3] = native_gf_mul(s0, 3) ^ s1 ^ s2 ^ native_gf_mul(s3, 2);
        }
        for i in 0..16 {
            state[i] ^= round_keys[r][i];
        }
    }
    for b in state.iter_mut() {
        *b = SBOX[*b as usize];
    }
    let mut tmp = state;
    for r_ in 0..4 {
        for c in 0..4 {
            tmp[r_ + 4 * c] = state[r_ + 4 * ((c + r_) % 4)];
        }
    }
    state = tmp;
    for i in 0..16 {
        state[i] ^= round_keys[10][i];
    }
    state
}

/// SP 800-38B doubling in GF(2^128) (big-endian, `Rb = 0x87`).
fn native_dbl(block: &[u8; 16]) -> [u8; 16] {
    let mut out = [0u8; 16];
    let mut carry = 0u8;
    for i in (0..16).rev() {
        let next = block[i] >> 7;
        out[i] = (block[i] << 1) | carry;
        carry = next;
    }
    if carry == 1 {
        out[15] ^= 0x87;
    }
    out
}

// ---------------------------------------------------------------------------
// Wire-level AES.
// ---------------------------------------------------------------------------

/// One byte, LSB-first (blob convention).
type Byte = [IRVarId; 8];
/// One 16-byte block in byte order.
type Block = [Byte; 16];

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

    fn const_byte(&self, byte: u8) -> Byte {
        core::array::from_fn(|i| self.const_bit((byte >> i) & 1 == 1))
    }
}

fn xor_byte(b: &mut WireBuilder, a: &Byte, c: &Byte) -> Byte {
    let mut out = *a;
    for i in 0..8 {
        out[i] = b.xor(a[i], c[i]);
    }
    out
}

/// `a ^= constant` as selective `Not` gates.
fn xor_const_byte(b: &mut WireBuilder, a: &mut Byte, c: u8) {
    for i in 0..8 {
        if (c >> i) & 1 == 1 {
            a[i] = b.not(a[i]);
        }
    }
}

/// Generic GF(2^8) multiplication mod `x^8+x^4+x^3+x+1` (`0x11B`):
/// carry-less product, then linear reduction.
fn gf_mul(b: &mut WireBuilder, x: &Byte, y: &Byte) -> Byte {
    let mut prod: [Option<IRVarId>; 15] = [None; 15];
    for i in 0..8 {
        for j in 0..8 {
            let t = b.and(x[i], y[j]);
            let k = i + j;
            prod[k] = Some(match prod[k] {
                None => t,
                Some(acc) => b.xor(acc, t),
            });
        }
    }
    reduce(b, &mut prod);
    core::array::from_fn(|i| prod[i].unwrap_or(b.zero))
}

/// Square in GF(2^8): `x^2` spreads bits to even positions (no ANDs — in
/// characteristic 2 the cross terms cancel), then linear reduction.
fn gf_square(b: &mut WireBuilder, x: &Byte) -> Byte {
    let mut prod: [Option<IRVarId>; 15] = [None; 15];
    for i in 0..8 {
        prod[2 * i] = Some(x[i]);
    }
    reduce(b, &mut prod);
    core::array::from_fn(|i| prod[i].unwrap_or(b.zero))
}

/// Reduce a 15-bit carry-less product mod `0x11B`: each high bit `k` XORs
/// into positions `k-8 + {0, 1, 3, 4}` (the low byte `0x1B` of the modulus).
fn reduce(b: &mut WireBuilder, prod: &mut [Option<IRVarId>; 15]) {
    for k in (8..15).rev() {
        if let Some(bit) = prod[k] {
            for off in [0usize, 1, 3, 4] {
                let pos = k - 8 + off;
                prod[pos] = Some(match prod[pos] {
                    None => bit,
                    Some(acc) => b.xor(acc, bit),
                });
            }
            prod[k] = None;
        }
    }
}

/// The AES S-box: inversion in GF(2^8) (`x^254` addition chain), then the
/// FIPS affine map. `0^254 = 0`, so the zero byte needs no special case.
fn sbox_wires(b: &mut WireBuilder, x: &Byte) -> Byte {
    let p2 = gf_square(b, x);
    let p4 = gf_square(b, &p2);
    let p8 = gf_square(b, &p4);
    let p16 = gf_square(b, &p8);
    let p32 = gf_square(b, &p16);
    let p64 = gf_square(b, &p32);
    let p128 = gf_square(b, &p64);
    let m = gf_mul(b, &p2, &p4); // x^6
    let m = gf_mul(b, &m, &p8); // x^14
    let m = gf_mul(b, &m, &p16); // x^30
    let m = gf_mul(b, &m, &p32); // x^62
    let m = gf_mul(b, &m, &p64); // x^126
    let inv = gf_mul(b, &m, &p128); // x^254
    let mut out = inv;
    for j in 0..8 {
        let mut s = inv[j];
        for off in [4usize, 5, 6, 7] {
            s = b.xor(s, inv[(j + off) % 8]);
        }
        out[j] = s;
    }
    xor_const_byte(b, &mut out, 0x63);
    out
}

/// `xtime` (`·2` in GF(2^8)): shift and conditional reduction by `0x1B`.
fn xtime(b: &mut WireBuilder, a: &Byte) -> Byte {
    [
        a[7],
        b.xor(a[0], a[7]),
        a[1],
        b.xor(a[2], a[7]),
        b.xor(a[3], a[7]),
        a[4],
        a[5],
        a[6],
    ]
}

fn mix_columns(b: &mut WireBuilder, s: &mut Block) {
    for c in 0..4 {
        let i = 4 * c;
        let col: Vec<Byte> = (0..4).map(|r| xor_fold(b, &col_bytes(s, i), r)).collect();
        for (r, byte) in col.iter().enumerate() {
            s[i + r] = *byte;
        }
    }
}

/// One mixed column byte `r` of column bytes `s[i..i+4]` via the
/// xtime-difference form with shared `t = s0^s1^s2^s3`.
fn xor_fold(b: &mut WireBuilder, col: &[Byte; 4], r: usize) -> Byte {
    let t01 = xor_byte(b, &col[0], &col[1]);
    let t23 = xor_byte(b, &col[2], &col[3]);
    let t = xor_byte(b, &t01, &t23);
    let d = xor_byte(b, &col[r], &col[(r + 1) % 4]);
    let xd = xtime(b, &d);
    let col_r_t = xor_byte(b, &col[r], &t);
    xor_byte(b, &col_r_t, &xd)
}

fn col_bytes(s: &Block, i: usize) -> [Byte; 4] {
    [s[i], s[i + 1], s[i + 2], s[i + 3]]
}

/// ShiftRows as a wire relabeling: `new[r + 4c] = old[r + 4((c + r) % 4)]`.
fn shift_rows(s: &mut Block) {
    let old = *s;
    for r in 0..4 {
        for c in 0..4 {
            s[r + 4 * c] = old[r + 4 * ((c + r) % 4)];
        }
    }
}

fn add_round_key_const(b: &mut WireBuilder, s: &mut Block, rk: &[u8; 16]) {
    for i in 0..16 {
        xor_const_byte(b, &mut s[i], rk[i]);
    }
}

/// One AES-128 block on wires with constant (build-time) round keys.
fn aes_block_wires(b: &mut WireBuilder, input: &Block, rks: &[[u8; 16]; 11]) -> Block {
    let mut s = *input;
    add_round_key_const(b, &mut s, &rks[0]);
    for r in 1..10 {
        for byte in s.iter_mut() {
            *byte = sbox_wires(b, byte);
        }
        shift_rows(&mut s);
        mix_columns(b, &mut s);
        add_round_key_const(b, &mut s, &rks[r]);
    }
    for byte in s.iter_mut() {
        *byte = sbox_wires(b, byte);
    }
    shift_rows(&mut s);
    add_round_key_const(b, &mut s, &rks[10]);
    s
}

/// `nonce + i` as a 128-bit big-endian ripple adder over nonce wires
/// (wrapping, like the native `wrapping_add`). Wire for value bit `j`
/// (LSB-first) is byte `15 - j/8`, bit `j % 8`.
fn add_const_be(b: &mut WireBuilder, nonce: &Block, i: u128) -> Block {
    let mut out = *nonce;
    let mut carry: Option<IRVarId> = None;
    for j in 0..128 {
        let byte = 15 - j / 8;
        let bit = j % 8;
        let w = nonce[byte][bit];
        let c = (i >> j) & 1 == 1;
        let (sum, next) = match (c, carry) {
            (false, None) => (w, None),
            (false, Some(cy)) => {
                // Half-adder: sum = w ^ carry, carry' = w & carry.
                let sum = b.xor(w, cy);
                let next = b.and(w, cy);
                (sum, Some(next))
            }
            (true, None) => (b.not(w), Some(w)),
            (true, Some(cy)) => {
                let x = b.xor(w, cy);
                let sum = b.not(x);
                // carry' = w | cy = !(!w & !cy)
                let nw = b.not(w);
                let ncy = b.not(cy);
                let anded = b.and(nw, ncy);
                (sum, Some(b.not(anded)))
            }
        };
        out[byte][bit] = sum;
        carry = next;
    }
    out
}

/// Flatten a block to 128 wires in byte order.
fn block_wires(block: &Block) -> [IRVarId; 128] {
    let mut out = [block[0][0]; 128];
    for (i, byte) in block.iter().enumerate() {
        for (j, w) in byte.iter().enumerate() {
            out[8 * i + j] = *w;
        }
    }
    out
}

/// Load 128 wires (byte order, LSB-first bytes) as a block.
fn wires_block(wires: &[IRVarId; 128]) -> Block {
    core::array::from_fn(|i| core::array::from_fn(|j| wires[8 * i + j]))
}

/// Constant block from bytes.
fn const_block(b: &WireBuilder, bytes: &[u8; 16]) -> Block {
    core::array::from_fn(|i| b.const_byte(bytes[i]))
}

/// Build-time constants: role round keys and CMAC subkeys.
struct RoleConstants {
    enc_rks: [[u8; 16]; 11],
    mac_rks: [[u8; 16]; 11],
    k1: [u8; 16],
    k2: [u8; 16],
}

fn role_constants(key: &[u8; 16]) -> RoleConstants {
    let key_rks = native_key_expansion(key);
    let ke = native_encrypt_block(&key_rks, &[0u8; 16]);
    let mut one = [0u8; 16];
    one[15] = 1;
    let km = native_encrypt_block(&key_rks, &one);
    let mac_rks = native_key_expansion(&km);
    let l = native_encrypt_block(&mac_rks, &[0u8; 16]);
    let k1 = native_dbl(&l);
    let k2 = native_dbl(&k1);
    RoleConstants {
        enc_rks: native_key_expansion(&ke),
        mac_rks,
        k1,
        k2,
    }
}

/// CTR keystream application: `ct = pt ^ stream`, returning ciphertext bits.
fn ctr_apply(
    b: &mut WireBuilder,
    rc: &RoleConstants,
    nonce: &Block,
    data: &[IRVarId],
) -> Vec<IRVarId> {
    let mut out = Vec::with_capacity(data.len());
    for (i, chunk) in data.chunks(128).enumerate() {
        let ctr = add_const_be(b, nonce, i as u128);
        let ks = block_wires(&aes_block_wires(b, &ctr, &rc.enc_rks));
        for (j, &d) in chunk.iter().enumerate() {
            out.push(b.xor(d, ks[j]));
        }
    }
    out
}

/// CMAC over `nonce ‖ data ‖ be64(bit_len)` (byte blocks of mixed const and
/// wire bytes), mirroring `volar_spec::aes128aead`'s message layout.
fn cmac_wires(
    b: &mut WireBuilder,
    rc: &RoleConstants,
    nonce: &Block,
    data: &[IRVarId],
) -> Block {
    debug_assert_eq!(data.len() % 8, 0, "byte-granular only");
    // Message bytes: nonce (16) || data bytes || be64(bitlen).
    let mut msg: Vec<Byte> = Vec::new();
    msg.extend_from_slice(nonce);
    for chunk in data.chunks(8) {
        let mut byte = [b.zero; 8];
        byte.copy_from_slice(chunk);
        msg.push(byte);
    }
    let bit_len = (data.len() as u64).to_be_bytes();
    for byte in bit_len {
        msg.push(b.const_byte(byte));
    }
    // Padding and subkey selection, mirroring native cmac().
    let complete = msg.len() % 16 == 0;
    if !complete {
        let mut pad = [b.zero; 8];
        pad[7] = b.one; // 0x80
        msg.push(pad);
        while msg.len() % 16 != 0 {
            msg.push([b.zero; 8]);
        }
    }
    let subkey = if complete { rc.k1 } else { rc.k2 };
    let n_blocks = msg.len() / 16;
    let mut state_block: Block = [[b.zero; 8]; 16];
    for (block_idx, chunk) in msg.chunks(16).enumerate() {
        let mut block: Block = chunk.try_into().expect("16-byte block");
        if block_idx + 1 == n_blocks {
            for i in 0..16 {
                xor_const_byte(b, &mut block[i], subkey[i]);
            }
        }
        for i in 0..16 {
            state_block[i] = xor_byte(b, &state_block[i], &block[i]);
        }
        state_block = aes_block_wires(b, &state_block, &rc.mac_rks);
    }
    state_block
}

fn aes128_etm_encrypt_inner(
    circ: &mut BCircuit,
    key: &[u8; 16],
    nonce: Block,
    plaintext: &[IRVarId],
) -> Result<(Vec<IRVarId>, [IRVarId; 128]), GadgetError> {
    if plaintext.len() % 8 != 0 {
        return Err(GadgetError::NotByteAligned);
    }
    let b = &mut WireBuilder::new(circ);
    let rc = role_constants(key);
    let ct = ctr_apply(b, &rc, &nonce, plaintext);
    let tag = cmac_wires(b, &rc, &nonce, &ct);
    Ok((ct, block_wires(&tag)))
}

fn aes128_etm_decrypt_inner(
    circ: &mut BCircuit,
    key: &[u8; 16],
    nonce: Block,
    ciphertext: &[IRVarId],
    tag: &[IRVarId; 128],
) -> Result<(Vec<IRVarId>, IRVarId), GadgetError> {
    if ciphertext.len() % 8 != 0 {
        return Err(GadgetError::NotByteAligned);
    }
    let b = &mut WireBuilder::new(circ);
    let rc = role_constants(key);
    let computed = block_wires(&cmac_wires(b, &rc, &nonce, ciphertext));
    // Poison = OR of tag mismatches.
    let mut poison = b.zero;
    for i in 0..128 {
        let d = b.xor(tag[i], computed[i]);
        poison = b.circ.push_stmt(BIrStmt::Or(poison, d), ());
    }
    let pt = ctr_apply(b, &rc, &nonce, ciphertext);
    Ok((pt, poison))
}

/// Append AES-128 EtM encryption to `circ`, mirroring
/// [`crate::ascon_gadget::ascon128a_encrypt_wires`]: returns the ciphertext
/// wires (same count as `plaintext`) and the 128 tag wires.
pub fn aes128_etm_encrypt_wires(
    circ: &mut BCircuit,
    key: &[u8; 16],
    nonce: &[u8; 16],
    plaintext: &[IRVarId],
) -> Result<(Vec<IRVarId>, [IRVarId; 128]), GadgetError> {
    let nonce_block = {
        let wb = WireBuilder::new(circ);
        const_block(&wb, nonce)
    };
    aes128_etm_encrypt_inner(circ, key, nonce_block, plaintext)
}

/// [`aes128_etm_encrypt_wires`] with the nonce carried on wires (public
/// but runtime-dynamic values, e.g. ORAM slot nonces read from the tree).
pub fn aes128_etm_encrypt_wires_with_nonce(
    circ: &mut BCircuit,
    key: &[u8; 16],
    nonce: &[IRVarId; 128],
    plaintext: &[IRVarId],
) -> Result<(Vec<IRVarId>, [IRVarId; 128]), GadgetError> {
    aes128_etm_encrypt_inner(circ, key, wires_block(nonce), plaintext)
}

/// Append AES-128 EtM decryption to `circ`: returns the plaintext wires
/// and a **poison** wire that is 1 iff the tag check fails — wire it into
/// the hardened poison accumulator (or the segment poison output) so
/// tampered state fails closed.
pub fn aes128_etm_decrypt_wires(
    circ: &mut BCircuit,
    key: &[u8; 16],
    nonce: &[u8; 16],
    ciphertext: &[IRVarId],
    tag: &[IRVarId; 128],
) -> Result<(Vec<IRVarId>, IRVarId), GadgetError> {
    let wb = WireBuilder::new(circ);
    let nonce_block = const_block(&wb, nonce);
    aes128_etm_decrypt_inner(circ, key, nonce_block, ciphertext, tag)
}

/// [`aes128_etm_decrypt_wires`] with the nonce carried on wires.
pub fn aes128_etm_decrypt_wires_with_nonce(
    circ: &mut BCircuit,
    key: &[u8; 16],
    nonce: &[IRVarId; 128],
    ciphertext: &[IRVarId],
    tag: &[IRVarId; 128],
) -> Result<(Vec<IRVarId>, IRVarId), GadgetError> {
    aes128_etm_decrypt_inner(circ, key, wires_block(nonce), ciphertext, tag)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    fn eval(circ: &BCircuit, params: &[bool]) -> Vec<bool> {
        let mut values = vec![false; circ.params as usize];
        values[..params.len()].copy_from_slice(params);
        let mut out = alloc::collections::BTreeMap::new();
        for (i, node) in circ.stmts.iter().enumerate() {
            let v = IRVarId(circ.params + i as u32);
            let val = match &node.kind {
                BIrStmt::Zero => false,
                BIrStmt::One => true,
                BIrStmt::Not(a) => !get(&values, &out, *a),
                BIrStmt::Xor(a, c) => get(&values, &out, *a) ^ get(&values, &out, *c),
                BIrStmt::And(a, c) => get(&values, &out, *a) & get(&values, &out, *c),
                BIrStmt::Or(a, c) => get(&values, &out, *a) | get(&values, &out, *c),
                other => panic!("gadget emitted non-pure stmt {other:?}"),
            };
            out.insert(v, val);
        }
        circ.outputs
            .iter()
            .map(|v| get(&values, &out, *v))
            .collect()
    }

    fn get(values: &[bool], out: &alloc::collections::BTreeMap<IRVarId, bool>, v: IRVarId) -> bool {
        if (v.0 as usize) < values.len() {
            values[v.0 as usize]
        } else {
            out[&v]
        }
    }

    fn unpack_bytes(bytes: &[u8], bits: usize) -> Vec<bool> {
        (0..bits).map(|i| (bytes[i / 8] >> (i % 8)) & 1 == 1).collect()
    }

    /// Build the encrypt gadget for `pt_bits` plaintext bits with `pt_bits`
    /// params, evaluate, and split (ct, tag).
    fn encrypt_circuit(
        key: &[u8; 16],
        nonce: &[u8; 16],
        pt: &[bool],
    ) -> (Vec<bool>, Vec<bool>) {
        let mut circ = BCircuit::new(pt.len() as u32);
        let pt_wires: Vec<IRVarId> = (0..pt.len()).map(|i| IRVarId(i as u32)).collect();
        let (ct, tag) = aes128_etm_encrypt_wires(&mut circ, key, nonce, &pt_wires).unwrap();
        circ.outputs = ct.iter().chain(tag.iter()).copied().collect();
        let result = eval(&circ, pt);
        (result[..ct.len()].to_vec(), result[ct.len()..].to_vec())
    }

    #[test]
    fn native_aes_matches_self_consistency() {
        // The inline native AES doubles as the const-folding engine; the
        // differential test below pins it against volar-spec.
        let key = [0xabu8; 16];
        let rks = native_key_expansion(&key);
        let block = [7u8; 16];
        let a = native_encrypt_block(&rks, &block);
        let b2 = native_encrypt_block(&rks, &block);
        assert_eq!(a, b2);
    }

    #[test]
    fn encrypt_gadget_matches_native_aes128aead() {
        let key = [0xabu8; 16];
        for pt_bytes in [8usize, 16, 24, 32] {
            let pt: Vec<u8> = (0..pt_bytes).map(|i| (i * 5 + 1) as u8).collect();
            // Odd nonce: exercises the counter ripple-carry path.
            let nonce = [(pt_bytes as u8) | 0x99; 16];
            let pt_bits = unpack_bytes(&pt, pt_bytes * 8);
            let (ct_bits, tag_bits) = encrypt_circuit(&key, &nonce, &pt_bits);
            let (ct_native, tag_native) =
                volar_spec::aes128aead::aes128_etm_seal(&key, &nonce, &pt);
            assert_eq!(
                ct_bits,
                unpack_bytes(&ct_native, pt_bytes * 8),
                "ciphertext at {pt_bytes} bytes"
            );
            assert_eq!(
                tag_bits,
                unpack_bytes(&tag_native, 128),
                "tag at {pt_bytes} bytes"
            );
            // Native open accepts the in-circuit seal.
            let ct_bytes: Vec<u8> = (0..pt_bytes)
                .map(|i| {
                    (0..8).fold(0u8, |a, j| a | ((ct_bits[8 * i + j] as u8) << j))
                })
                .collect();
            let tag_bytes: [u8; 16] = core::array::from_fn(|i| {
                (0..8).fold(0u8, |a, j| a | ((tag_bits[8 * i + j] as u8) << j))
            });
            assert_eq!(
                volar_spec::aes128aead::aes128_etm_open(&key, &nonce, &ct_bytes, &tag_bytes),
                Some(pt.clone())
            );
        }
    }

    #[test]
    fn decrypt_gadget_round_trips_and_poisons_tampering() {
        let key = [0x24u8; 16];
        let pt_bytes = 24usize;
        let pt: Vec<u8> = (0..pt_bytes).map(|i| (i * 3 + 2) as u8).collect();
        let nonce = [0x99u8; 16];
        let (ct_native, tag_native) = volar_spec::aes128aead::aes128_etm_seal(&key, &nonce, &pt);
        let ct_bits = unpack_bytes(&ct_native, pt_bytes * 8);
        let tag_bits = unpack_bytes(&tag_native, 128);

        let mut circ = BCircuit::new((pt_bytes * 8 + 128) as u32);
        let ct_wires: Vec<IRVarId> = (0..pt_bytes * 8).map(|i| IRVarId(i as u32)).collect();
        let tag_wires: [IRVarId; 128] =
            core::array::from_fn(|i| IRVarId((pt_bytes * 8 + i) as u32));
        let (pt_out, poison) =
            aes128_etm_decrypt_wires(&mut circ, &key, &nonce, &ct_wires, &tag_wires).unwrap();
        let mut outs = pt_out.clone();
        outs.push(poison);
        circ.outputs = outs;

        let mut params = ct_bits.clone();
        params.extend_from_slice(&tag_bits);
        let result = eval(&circ, &params);
        assert_eq!(&result[..pt_bytes * 8], &unpack_bytes(&pt, pt_bytes * 8)[..]);
        assert!(!result[pt_bytes * 8], "honest blob: no poison");

        let mut bad = params.clone();
        bad[0] = !bad[0];
        let result = eval(&circ, &bad);
        assert!(result[pt_bytes * 8], "tampered ct: poison");
    }

    #[test]
    fn wire_nonce_matches_const_nonce() {
        let key = [0x5au8; 16];
        let nonce = [0x11u8; 16];
        let pt_bytes = 16usize;
        let pt: Vec<u8> = (0..pt_bytes).map(|i| (i * 9 + 4) as u8).collect();
        let pt_bits = unpack_bytes(&pt, pt_bytes * 8);

        let (ct_c, tag_c) = encrypt_circuit(&key, &nonce, &pt_bits);

        let mut circ = BCircuit::new((128 + pt_bytes * 8) as u32);
        let nonce_wires: [IRVarId; 128] = core::array::from_fn(|i| IRVarId(i as u32));
        let pt_wires: Vec<IRVarId> = (0..pt_bytes * 8).map(|i| IRVarId((128 + i) as u32)).collect();
        let (ct_w, tag_w) =
            aes128_etm_encrypt_wires_with_nonce(&mut circ, &key, &nonce_wires, &pt_wires).unwrap();
        circ.outputs = ct_w.iter().chain(tag_w.iter()).copied().collect();
        let mut params = unpack_bytes(&nonce, 128);
        params.extend_from_slice(&pt_bits);
        let result = eval(&circ, &params);
        assert_eq!(&result[..pt_bytes * 8], &ct_c[..]);
        assert_eq!(&result[pt_bytes * 8..], &tag_c[..]);
    }

    #[test]
    fn non_byte_aligned_fails_closed() {
        let mut circ = BCircuit::new(3);
        let pt = [IRVarId(0), IRVarId(1), IRVarId(2)];
        let key = [0u8; 16];
        let nonce = [0u8; 16];
        assert_eq!(
            aes128_etm_encrypt_wires(&mut circ, &key, &nonce, &pt),
            Err(GadgetError::NotByteAligned)
        );
    }
}
