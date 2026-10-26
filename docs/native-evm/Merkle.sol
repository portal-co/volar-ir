// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// Native-Keccak Merkle path verification — the WARP verifier's hashing
/// component. Each node is one KECCAK256 over the 64-byte (left,right) pair.
library Merkle {
    function verify(uint256 leaf, uint256[] memory path, uint256 index, bytes32 root)
        internal pure returns (bool)
    {
        uint256 acc = leaf;
        for (uint256 i = 0; i < path.length; i++) {
            uint256 sib = path[i];
            (uint256 lo, uint256 hi) = ((index >> i) & 1 == 0) ? (acc, sib) : (sib, acc);
            acc = uint256(keccak256(abi.encodePacked(lo, hi)));
        }
        return bytes32(acc) == root;
    }
}
