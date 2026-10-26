// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;
import "forge-std/Test.sol";
import "../src/Merkle.sol";

contract MerkleTest is Test {
    function measureVerify(uint256 depth) external returns (uint256 gasUsed) {
        uint256[] memory path = new uint256[](depth);
        for (uint256 i=0;i<depth;i++){ path[i] = uint256(keccak256(abi.encodePacked(i))); }
        uint256 g0 = gasleft();
        Merkle.verify(0xabc, path, 0, bytes32(0)); // root won't match; we only measure gas
        gasUsed = g0 - gasleft();
    }
    function test_merkle_gas() public {
        emit log_named_uint("gas merkle verify depth=20 (N=2^20)", this.measureVerify(20));
        emit log_named_uint("gas merkle verify depth=3 (N=8)", this.measureVerify(3));
    }
}
