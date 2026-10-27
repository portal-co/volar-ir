// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// GF(2^128) field lib (measured). For the GF(2^256) variant, Gf256 is swapped in.
library Gf {
    uint256 internal constant REDUCE = 0x87;
    uint256 internal constant HIGH = 1 << 127;
    uint256 internal constant MASK = (1 << 128) - 1;
    function mul(uint256 a, uint256 b) internal pure returns (uint256) {
        a &= MASK; b &= MASK; uint256 p = 0;
        for (uint256 i = 0; i < 128; i++) { if (b & 1 != 0) p ^= a; uint256 h = a & HIGH; a = (a << 1) & MASK; if (h != 0) a ^= REDUCE; b >>= 1; }
        return p;
    }
}

/// The WARP succinct verifier's full cost core: eq_evals (twice), the Merkle
/// openings, the eq-weighted batching, and the two sumchecks.
library WarpVerifyFull {
    function eqEvals(uint256[] memory point) internal pure returns (uint256[] memory out) {
        uint256 n = 1 << point.length;
        out = new uint256[](n);
        out[0] = 1;
        for (uint256 c = 0; c < point.length; c++) {
            uint256 coord = point[c]; uint256 half = 1 << c;
            for (uint256 i = 0; i < half; i++) { uint256 pr = Gf.mul(out[i], coord); out[i] ^= pr; out[i+half] = pr; }
        }
    }
    /// eq(point, b) for a single hypercube index b — O(log) muls, O(1) memory.
    function eqScalar(uint256[] memory point, uint256 b) internal pure returns (uint256 acc) {
        acc = 1;
        for (uint256 i = 0; i < point.length; i++) {
            uint256 coord = point[i];
            // b_i ? coord : (1 - coord); in char 2, 1 - coord = 1 ^ coord.
            uint256 factor = ((b >> i) & 1 == 1) ? coord : (1 ^ coord);
            acc = Gf.mul(acc, factor);
        }
    }
    /// Full verifier cost core at the measured structure.
    function verify(
        uint256 t, uint256 logN, uint256 logM,
        uint256[] memory openings,   // ell*t field elements
        uint256 seed
    ) internal pure returns (uint256 acc) {
        // 1. The batching challenge point (log(claims) coords) — derived from the transcript.
        uint256 logClaims = 0; while ((1 << logClaims) < 2*t) { logClaims++; }
        uint256[] memory ptB = new uint256[](logClaims);
        for (uint256 i=0;i<logClaims;i++){ ptB[i] = uint256(keccak256(abi.encodePacked(seed,i))) & ((1<<128)-1); }
        // batching eq_evals over the claim domain (2^logClaims values) — feasible DP.
        uint256[] memory eqB = eqEvals(ptB);
        // the codeword MLE point (log n coords) — per-point eq_scalar, O(log n) muls.
        uint256[] memory ptN = new uint256[](logN);
        for (uint256 i=0;i<logN;i++){ ptN[i] = uint256(keccak256(abi.encodePacked(seed,1000+i))) & ((1<<128)-1); }
        uint256 mleScalar = eqScalar(ptN, 0);
        // 2. Merkle openings (ell=2 accumulators x t queries x logN hashes) + batching.
        acc = 0;
        uint256 oi = 0;
        for (uint256 accIdx = 0; accIdx < 2; accIdx++) {
            for (uint256 q = 0; q < t; q++) {
                uint256 node = openings[oi];
                for (uint256 d = 0; d < logN; d++) {
                    uint256 sib = uint256(keccak256(abi.encodePacked(seed, accIdx, q, d)));
                    node = uint256(keccak256(abi.encodePacked(bytes32(node), bytes32(sib))));
                }
                acc ^= Gf.mul(eqB[oi % eqB.length], node & ((1<<128)-1));
                oi++;
            }
        }
        // 3. Two sumchecks (log ell + log n rounds), each a quadratic univariate check.
        acc ^= mleScalar;
        uint256 claim = acc;
        for (uint256 round = 0; round < 1 + logN; round++) {
            uint256 ch = uint256(keccak256(abi.encodePacked(seed, uint256(99), round))) & ((1<<128)-1);
            // evaluate the round poly at 0, 1, ch and check consistency (a few muls)
            uint256 p0 = Gf.mul(claim, ch);
            uint256 p1 = Gf.mul(p0, ch) ^ claim;
            claim = p1 ^ Gf.mul(ch, ch);
        }
    }
}
