//! Fixed-shape TLS 1.3 MPC circuit-extern contracts.
//!
//! These imports identify secret-mixing operations before Boolar lowering.
//! They are not evaluator-hosted actions: a strict action reveals decoded
//! arguments to its host, which is unsuitable for TLS key material. Volar VC
//! instead realizes these pure imports as boolar circuits while their inputs
//! and outputs remain held labels in the strict-chain registry.
//!
//! @pinnedness: unpinned
//! @stability: very-unstable
//! @ai: assisted

/// WASM import module for the fixed-shape TLS operations.
pub const WAFFLE_MODULE: &str = "portal_tls13";

/// SHA-256 over an exactly 64-byte raw message. The realization appends the
/// standard SHA-256 padding. Input and output bytes are LSB-first within each
/// byte.
pub mod sha256_64 {
    pub const WAFFLE_FIELD: &str = "sha256_64";
    pub const WAFFLE_IMPORT: &str = "portal_tls13.sha256_64";
    pub const ORACLE_NAME: &str = "tls13_sha256_64";
    pub const MSG_BITS: usize = 64 * 8;
    pub const RESULT_BITS: usize = 32 * 8;
}

/// HMAC-SHA-256 with a 32-byte key and exactly 32-byte message. This is the
/// fixed geometry used for TLS 1.3 extract/Finished-style single-block calls.
pub mod hmac_sha256_32_32 {
    pub const WAFFLE_FIELD: &str = "hmac_sha256_32_32";
    pub const WAFFLE_IMPORT: &str = "portal_tls13.hmac_sha256_32_32";
    pub const ORACLE_NAME: &str = "tls13_hmac_sha256_32_32";
    pub const KEY_BITS: usize = 32 * 8;
    pub const MSG_BITS: usize = 32 * 8;
    pub const ARG_BITS: usize = KEY_BITS + MSG_BITS;
    pub const RESULT_BITS: usize = 32 * 8;
}

/// One RFC 7748 X25519 Montgomery-ladder step. The wire order is the exact
/// `volar_vc::x25519_gadget::build_x25519_step` ABI:
/// `x2 || z2 || x3 || z3 || swap || scalar_bit`, all field elements LSB-first.
pub mod x25519_step {
    pub const WAFFLE_FIELD: &str = "x25519_step";
    pub const WAFFLE_IMPORT: &str = "portal_tls13.x25519_step";
    pub const ORACLE_NAME: &str = "tls13_x25519_step";
    pub const FIELD_BITS: usize = 255;
    pub const ARG_BITS: usize = 5 * FIELD_BITS + 2;
    pub const RESULT_BITS: usize = 4 * FIELD_BITS + 1;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contracts_have_the_realizer_geometries() {
        assert_eq!(sha256_64::MSG_BITS, 512);
        assert_eq!(sha256_64::RESULT_BITS, 256);
        assert_eq!(hmac_sha256_32_32::ARG_BITS, 512);
        assert_eq!(hmac_sha256_32_32::RESULT_BITS, 256);
        assert_eq!(x25519_step::ARG_BITS, 1277);
        assert_eq!(x25519_step::RESULT_BITS, 1021);
        assert!(sha256_64::WAFFLE_IMPORT.starts_with(WAFFLE_MODULE));
        assert!(hmac_sha256_32_32::WAFFLE_IMPORT.starts_with(WAFFLE_MODULE));
        assert!(x25519_step::WAFFLE_IMPORT.starts_with(WAFFLE_MODULE));
    }
}
