// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;
import "forge-std/Test.sol";
import "../src/Gf256.sol";

contract Gf256Test is Test {
    function measureMul(uint256 a, uint256 b) external returns (uint256 gasUsed, uint256 product) {
        uint256 g0 = gasleft();
        product = Gf256.mul(a, b);
        gasUsed = g0 - gasleft();
    }
    function test_gas() public {
        (uint256 gMul, uint256 prod) = this.measureMul(0x0123456789abcdef0123456789abcdef, 0xfedcba9876543210fedcba9876543210);
        emit log_named_uint("gas gf256 mul", gMul);
        emit log_named_uint("gf256 mul product", prod);
    }
}

contract Gf256LoopTest is Test {
    function measureMul256(uint256 seed) external returns (uint256 gasUsed, uint256 acc) {
        uint256 g0 = gasleft();
        acc = 0;
        for (uint256 i = 0; i < 256; i++) {
            acc ^= Gf256.mul(seed + i, seed ^ (i * 0x9e3779b97f4a7c15));
        }
        gasUsed = g0 - gasleft();
    }
    function test_loop() public {
        (uint256 g,) = this.measureMul256(0x0123456789abcdef);
        emit log_named_uint("gas 256 gf256 muls", g);
        emit log_named_uint("per gf256 mul (loop)", g / 256);
    }
}
