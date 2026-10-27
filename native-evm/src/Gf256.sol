// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// GF(2^256), polynomial x^256 + x^10 + x^5 + x^2 + 1 (0x425), matching
/// volar-primitives' Galois256 (gf_mul_256 shift-and-reduce). A uint256 holds
/// the whole element, so no masking is needed (shifts wrap at 256 bits).
library Gf256 {
    uint256 internal constant REDUCE = 0x425;
    uint256 internal constant HIGH = 1 << 255;

    function mul(uint256 a, uint256 b) internal pure returns (uint256) {
        uint256 p = 0;
        for (uint256 i = 0; i < 256; i++) {
            if (b & 1 != 0) p ^= a;
            uint256 high = a & HIGH;
            a <<= 1; // uint256 wraps at 256 bits — the shift drops the top bit
            if (high != 0) a ^= REDUCE;
            b >>= 1;
        }
        return p;
    }

    function add(uint256 a, uint256 b) internal pure returns (uint256) {
        return a ^ b;
    }
}
