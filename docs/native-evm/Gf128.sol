// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// GF(2^128) with the GCM polynomial x^128 + x^7 + x^2 + x + 1 (0x87),
/// matching volar-primitives' Galois128 (gf_mul_u128: interleaved
/// shift-and-reduce). Field add is XOR. This is the WARP verifier's field.
library Gf128 {
    uint256 internal constant REDUCE = 0x87;
    uint256 internal constant HIGH = 1 << 127;
    uint256 internal constant MASK128 = (1 << 128) - 1;

    /// Carry-less multiply + reduce, exactly gf_mul_u128's 128-iteration loop.
    /// `a` is masked to 128 bits after the shift (Solidity uint256 does not wrap
    /// at 128 bits like Rust's u128; the reduction handles bit 127 -> the poly).
    function mul(uint256 a, uint256 b) internal pure returns (uint256) {
        a &= MASK128;
        b &= MASK128;
        uint256 p = 0;
        for (uint256 i = 0; i < 128; i++) {
            if (b & 1 != 0) p ^= a;
            uint256 high = a & HIGH;
            a = (a << 1) & MASK128;
            if (high != 0) a ^= REDUCE;
            b >>= 1;
        }
        return p;
    }

    function add(uint256 a, uint256 b) internal pure returns (uint256) {
        return a ^ b;
    }

    /// Native Keccak-256 over a 64-byte Merkle node (two digests).
    function hashNode(uint256 left, uint256 right) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked(left, right));
    }

    /// eq_evals: the multilinear eq polynomial's evaluations over the hypercube,
    /// matching volar-spec's warp_eq_evals (sub = XOR in characteristic 2).
    function eqEvals(uint256[] memory point) internal pure returns (uint256[] memory out) {
        uint256 n = 1 << point.length;
        out = new uint256[](n);
        out[0] = 1;
        for (uint256 c = 0; c < point.length; c++) {
            uint256 coord = point[c];
            uint256 half = 1 << c;
            for (uint256 index = 0; index < half; index++) {
                uint256 product = mul(out[index], coord);
                out[index] = out[index] ^ product;
                out[index + half] = product;
            }
        }
    }

    /// MLE eval: sum_i values[i] * eq[i], matching volar-spec's warp_mle_eval.
    function mleEval(uint256[] memory values, uint256[] memory point) internal pure returns (uint256 out) {
        uint256[] memory eq = eqEvals(point);
        out = 0;
        for (uint256 i = 0; i < values.length; i++) {
            out ^= mul(values[i], eq[i]);
        }
    }
}
