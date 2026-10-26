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
    function mle_dot(uint256[] memory a, uint256[] memory b) public pure returns (uint256) {
        uint256 acc = 0;
        for (uint256 i = 0; i < 4; i++) {
            acc = (acc ^ Gf128.mul(a[i], b[i]));
        }
        return acc;
    }
}
