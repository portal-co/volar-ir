// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

library Gf128 {
    uint256 internal constant REDUCE = 0x87;
    uint256 internal constant HIGH = 1 << 127;
    uint256 internal constant MASK128 = (1 << 128) - 1;
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
}

contract Generated {
    function mle8(uint256[] memory values, uint256[] memory point) public pure returns (uint256) {
        uint256[] memory eq = new uint256[](8);
        eq[0] = 1;
        for (uint256 c = 0; c < 3; c++) {
            uint256 coordinate = point[c];
            for (uint256 index = 0; index < (1 << c); index++) {
                uint256 product = Gf128.mul(eq[index], coordinate);
                eq[index] = (eq[index] ^ product);
                eq[(index + (1 << c))] = product;
            }
        }
        uint256 out = 0;
        for (uint256 i = 0; i < 8; i++) {
            out = (out ^ Gf128.mul(values[i], eq[i]));
        }
        return out;
    }
}
