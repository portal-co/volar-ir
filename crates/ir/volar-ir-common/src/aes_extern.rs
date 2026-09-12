//! The `aes128_encrypt_block` circuit-extern contract (AX1 of the web-proofs
//! MPC plan).
//!
//! AES-128 single-block encryption is surfaced to guest code as an **extern**
//! rather than lowered through the generic pipeline: WASM guests import it,
//! LLVM guests call the extern symbol, and both map to the same IR-level
//! [`OracleDecl`](crate::OracleDecl) / `OracleCall` / `BIrStmt::OracleBit`
//! chain. A schedule-time realization pass (volar-vc's `aes_extern`) inlines
//! the fixed-length AES boolar gadget per call site, so the interface is the
//! stable part and the realization (composite-field S-box today, a cheaper
//! S-box later) is a drop-in upgrade.
//!
//! # Contract
//!
//! ```text
//! aes128_encrypt_block(key: [u8; 16], pt: [u8; 16]) -> [u8; 16]
//! ```
//!
//! Bit-level ABI (what the oracle sees at the boolar level): `256` argument
//! wires — `key[0..128]` then `pt[0..128]`, each **byte-major, LSB-first
//! within each byte** (bit `8*j + i` is bit `i` of byte `j`) — and `128`
//! result wires, the ciphertext in the same layout. This matches volar-vc's
//! `aes_gadget::build_aes128` parameter/output layout exactly.
//!
//! # Bindings
//!
//! - **WASM** import `(import "portal_crypto" "aes128_enc" (func (param i64
//!   i64 i64 i64) (result i64 i64)))`: params are `(key_lo, key_hi, pt_lo,
//!   pt_hi)`, each `i64` carrying 8 bytes little-endian (its bit `8*j + i` is
//!   byte `j` bit `i`); results are `(ct_lo, ct_hi)` likewise. The waffle
//!   frontend resolves the import through the module's import table as
//!   [`WAFFLE_IMPORT`].
//! - **LLVM** extern symbol [`LLVM_SYMBOL`]: `void
//!   __portal_aes128_encrypt_block(u64 out[2], const u64 key[2], const u64
//!   pt[2])` — out/key/pt in the same little-endian packing.
//! - **Oracle name** [`ORACLE_NAME`] in `module.oracles` / `IRStmt::
//!   OracleCall` / `BIrStmt::OracleBit`: params `[u64; 4]`, results `[u64;
//!   2]`.

/// The canonical WAFFLE import lookup key: `"portal_crypto.aes128_enc"`.
pub const WAFFLE_IMPORT: &str = "portal_crypto.aes128_enc";

/// The WASM import module half of [`WAFFLE_IMPORT`].
pub const WAFFLE_MODULE: &str = "portal_crypto";

/// The WASM import field half of [`WAFFLE_IMPORT`].
pub const WAFFLE_FIELD: &str = "aes128_enc";

/// The oracle name carried by `OracleDecl`, `IRStmt::OracleCall`, and
/// `BIrStmt::OracleBit` once either frontend has mapped the import.
pub const ORACLE_NAME: &str = "aes128_encrypt_block";

/// The LLVM extern symbol mapped to the same oracle.
pub const LLVM_SYMBOL: &str = "__portal_aes128_encrypt_block";

/// AES-128 key width in bits.
pub const KEY_BITS: usize = 128;
/// AES block (plaintext) width in bits.
pub const PT_BITS: usize = 128;
/// AES block (ciphertext) width in bits.
pub const CT_BITS: usize = 128;
/// Total oracle argument width in bits (`key` then `pt`).
pub const ARG_BITS: usize = KEY_BITS + PT_BITS;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_widths_are_aes128() {
        assert_eq!(KEY_BITS, 128);
        assert_eq!(PT_BITS, 128);
        assert_eq!(CT_BITS, 128);
        assert_eq!(ARG_BITS, 256);
        assert_eq!(WAFFLE_IMPORT, "portal_crypto.aes128_enc");
        assert!(WAFFLE_IMPORT.starts_with(WAFFLE_MODULE));
        assert!(WAFFLE_IMPORT.ends_with(WAFFLE_FIELD));
    }
}
