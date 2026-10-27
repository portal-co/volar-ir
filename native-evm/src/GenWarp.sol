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

contract GenWarp {
    function mle_dot(uint256[] memory a, uint256[] memory b) public pure returns (uint256) {
        uint256 acc = 0;
        for (uint256 i = 0; i < 4; i++) {
            acc = (acc ^ Gf128.mul(a[i], b[i]));
        }
        return acc;
    }
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
    function merkle8(uint256[] memory l) public pure returns (uint256) {
        uint256 a = ((uint256(keccak256(abi.encodePacked(bytes32(l[0]), bytes32(l[1]))))) & ((1 << 128) - 1));
        uint256 b = ((uint256(keccak256(abi.encodePacked(bytes32(l[2]), bytes32(l[3]))))) & ((1 << 128) - 1));
        uint256 c = ((uint256(keccak256(abi.encodePacked(bytes32(l[4]), bytes32(l[5]))))) & ((1 << 128) - 1));
        uint256 d = ((uint256(keccak256(abi.encodePacked(bytes32(l[6]), bytes32(l[7]))))) & ((1 << 128) - 1));
        uint256 e = ((uint256(keccak256(abi.encodePacked(bytes32(a), bytes32(b))))) & ((1 << 128) - 1));
        uint256 f = ((uint256(keccak256(abi.encodePacked(bytes32(c), bytes32(d))))) & ((1 << 128) - 1));
        return ((uint256(keccak256(abi.encodePacked(bytes32(e), bytes32(f))))) & ((1 << 128) - 1));
    }
}
