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

contract GenDecider {
    function mle_eval(uint256[] memory values, uint256[] memory point) public pure returns (uint256) {
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
    function bundled_eval(uint256[] memory coeffs, uint256[] memory nvars, uint256[] memory tvars, uint256[] memory nterms, uint256[] memory beta, uint256[] memory witness) public pure returns (uint256) {
        uint256[] memory eq = new uint256[](16);
        eq[0] = 1;
        for (uint256 j = 0; j < 4; j++) {
            uint256 coordinate = beta[(1 + j)];
            for (uint256 index = 0; index < (1 << j); index++) {
                uint256 product = Gf128.mul(eq[index], coordinate);
                eq[index] = (eq[index] ^ product);
                eq[(index + (1 << j))] = product;
            }
        }
        uint256 out = 0;
        for (uint256 c = 0; c < 16; c++) {
            uint256 value = 0;
            for (uint256 t = 0; t < nterms[c]; t++) {
                uint256 product = coeffs[((c * 3) + t)];
                for (uint256 v = 0; v < nvars[((c * 3) + t)]; v++) {
                    uint256 _var = tvars[((((c * 3) + t) * 2) + v)];
                    uint256 x = ((_var < 1) ? beta[_var] : witness[(_var - 1)]);
                    product = Gf128.mul(product, x);
                }
                value = (value ^ product);
            }
            out = (out ^ Gf128.mul(eq[c], value));
        }
        return out;
    }
    function decide_toy(uint256[] memory coeffs, uint256[] memory nvars, uint256[] memory tvars, uint256[] memory nterms, uint256 root, uint256[] memory alpha, uint256 mu, uint256[] memory beta, uint256 eta, uint256[] memory codeword, uint256[] memory witness) public pure returns (bool) {
        uint256 a = ((uint256(keccak256(abi.encodePacked(bytes32(codeword[0]), bytes32(codeword[1]))))) & ((1 << 128) - 1));
        uint256 b = ((uint256(keccak256(abi.encodePacked(bytes32(codeword[2]), bytes32(codeword[3]))))) & ((1 << 128) - 1));
        uint256 c = ((uint256(keccak256(abi.encodePacked(bytes32(codeword[4]), bytes32(codeword[5]))))) & ((1 << 128) - 1));
        uint256 d = ((uint256(keccak256(abi.encodePacked(bytes32(codeword[6]), bytes32(codeword[7]))))) & ((1 << 128) - 1));
        uint256 e = ((uint256(keccak256(abi.encodePacked(bytes32(a), bytes32(b))))) & ((1 << 128) - 1));
        uint256 f = ((uint256(keccak256(abi.encodePacked(bytes32(c), bytes32(d))))) & ((1 << 128) - 1));
        uint256 computed_root = ((uint256(keccak256(abi.encodePacked(bytes32(e), bytes32(f))))) & ((1 << 128) - 1));
        if ((computed_root != root)) {
            return false;
        }
        for (uint256 i = 0; i < 8; i++) {
            if ((witness[i] != codeword[i])) {
                return false;
            }
        }
        uint256 mle = mle_eval(codeword, alpha);
        if ((mle != mu)) {
            return false;
        }
        uint256 bundled = bundled_eval(coeffs, nvars, tvars, nterms, beta, witness);
        if ((bundled != eta)) {
            return false;
        }
        return true;
    }
}
