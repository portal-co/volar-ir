// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// Windowed GF(2^128) mul: process b 4 bits at a time using a precomputed
/// table T[v] = a*v (v in 0..16), cutting the 128 shift-and-reduce iterations
/// to 32 window steps + a 16-entry table build (amortized when a is reused,
/// as in the eq_evals DP where the challenge coordinate is fixed per level).
library Gf128Windowed {
    uint256 internal constant REDUCE = 0x87;
    uint256 internal constant HIGH = 1 << 127;
    uint256 internal constant MASK = (1 << 128) - 1;

    function baseMul(uint256 a, uint256 b) internal pure returns (uint256) {
        a &= MASK; b &= MASK; uint256 p = 0;
        for (uint256 i = 0; i < 128; i++) { if (b & 1 != 0) p ^= a; uint256 h = a & HIGH; a = (a << 1) & MASK; if (h != 0) a ^= REDUCE; b >>= 1; }
        return p;
    }

    /// Windowed mul with the table built fresh (single-shot cost).
    function mulWindowed(uint256 a, uint256 b) internal pure returns (uint256) {
        a &= MASK; b &= MASK;
        // T[v] = a * v for v in 0..16 (v's bits select shifts of a).
        uint256[16] memory T;
        T[0] = 0;
        for (uint256 v = 1; v < 16; v++) {
            // T[v] = xor of (a << j reduced) for set bits j of v
            uint256 acc = 0; uint256 aa = a;
            for (uint256 j = 0; j < 4; j++) {
                if ((v >> j) & 1 != 0) acc ^= aa;
                uint256 h = aa & HIGH; aa = (aa << 1) & MASK; if (h != 0) aa ^= REDUCE;
            }
            T[v] = acc;
        }
        // Process b 4 bits at a time, from the high nibble down.
        uint256 result = 0;
        for (uint256 w = 0; w < 32; w++) {
            // shift result left 4 with reduction
            for (uint256 j = 0; j < 4; j++) {
                uint256 h = result & HIGH; result = (result << 1) & MASK; if (h != 0) result ^= REDUCE;
            }
            uint256 nib = (b >> (4 * (31 - w))) & 0xF;
            result ^= T[nib];
        }
        return result;
    }
}
