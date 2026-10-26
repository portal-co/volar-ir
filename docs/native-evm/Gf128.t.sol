// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

import "forge-std/Test.sol";
import "../src/Gf128.sol";

contract Gf128Test is Test {
    function measureMul(uint256 a, uint256 b) external returns (uint256 gasUsed, uint256 product) {
        uint256 g0 = gasleft();
        product = Gf128.mul(a, b);
        gasUsed = g0 - gasleft();
    }
    function measureAdd(uint256 a, uint256 b) external returns (uint256 gasUsed, uint256 sum) {
        uint256 g0 = gasleft();
        sum = Gf128.add(a, b);
        gasUsed = g0 - gasleft();
    }
    function measureKeccak(uint256 l, uint256 r) external returns (uint256 gasUsed, bytes32 h) {
        uint256 g0 = gasleft();
        h = Gf128.hashNode(l, r);
        gasUsed = g0 - gasleft();
    }
    // Batch: 256 independent muls packed (loop, NOT simd — the scalar baseline for batching).
    function measureMul256(uint256 seed) external returns (uint256 gasUsed, uint256 acc) {
        uint256 g0 = gasleft();
        acc = 0;
        for (uint256 i = 0; i < 256; i++) {
            acc ^= Gf128.mul(seed + i, seed ^ (i * 0x9e3779b97f4a7c15));
        }
        gasUsed = g0 - gasleft();
    }

    function test_gas() public {
        (uint256 gMul, uint256 prod) = this.measureMul(0x0123456789abcdef0123456789abcdef, 0xfedcba9876543210fedcba9876543210);
        emit log_named_uint("gas gf128 mul", gMul);
        emit log_named_uint("gf128 mul product", prod);
        (uint256 gAdd,) = this.measureAdd(0x0123456789abcdef0123456789abcdef, 0xfedcba9876543210fedcba9876543210);
        emit log_named_uint("gas gf128 add", gAdd);
        (uint256 gK,) = this.measureKeccak(0x0123456789abcdef0123456789abcdef, 0xfedcba9876543210fedcba9876543210);
        emit log_named_uint("gas keccak256 64B node", gK);
        (uint256 g256,) = this.measureMul256(0x0123456789abcdef0123456789abcdef);
        emit log_named_uint("gas 256 scalar muls", g256mul(g256));
    }
    function g256mul(uint256 g) internal pure returns (uint256) { return g; }

    // MLE eval over a size-N codeword (LOGN = log2(N)).
    function measureMle(uint256 logn, uint256 seed) external returns (uint256 gasUsed, uint256 result) {
        uint256 n = 1 << logn;
        uint256[] memory values = new uint256[](n);
        uint256[] memory point = new uint256[](logn);
        uint256 x = seed;
        unchecked {
            for (uint256 i = 0; i < n; i++) { x = x * 0x9e3779b97f4a7c15 + i; values[i] = x & ((1<<128)-1); }
            for (uint256 j = 0; j < logn; j++) { x = x * 0x9e3779b97f4a7c15 + j; point[j] = x & ((1<<128)-1); }
        }
        uint256 g0 = gasleft();
        result = Gf128.mleEval(values, point);
        gasUsed = g0 - gasleft();
    }

    function test_mle_gas() public {
        (uint256 g8,) = this.measureMle(3, 0x0123456789abcdef);   // N=8 (toy decider)
        emit log_named_uint("gas mleEval N=8", g8);
        (uint256 g256,) = this.measureMle(8, 0x0123456789abcdef); // N=256
        emit log_named_uint("gas mleEval N=256", g256);
    }
}
