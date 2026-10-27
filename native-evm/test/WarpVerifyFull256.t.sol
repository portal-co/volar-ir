// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;
import "forge-std/Test.sol";
import "../src/WarpVerifyFull256.sol";
contract WarpVerifyFull256Test is Test {
    function measure(uint256 t, uint256 logN, uint256 logM) external returns (uint256) {
        uint256 n = 2*t;
        uint256[] memory openings = new uint256[](n);
        for (uint256 i=0;i<n;i++){ openings[i]=i+1; }
        uint256 g0=gasleft();
        WarpVerifyFull256.verify(t, logN, logM, openings, 0xabc);
        return g0-gasleft();
    }
    function test_full() public {
        emit log_named_uint("gas full verifier t=255 logN=9 (lam100, small)", this.measure(255,9,4));
        emit log_named_uint("gas full verifier t=511 logN=9 (lam=128)", this.measure(511,9,4));
        emit log_named_uint("gas full verifier t=255 logN=20 (lam=100 big stmt)", this.measure(255,20,10));
        emit log_named_uint("gas full verifier t=511 logN=20 (lam=128 big)", this.measure(511,20,10));
    }
}
