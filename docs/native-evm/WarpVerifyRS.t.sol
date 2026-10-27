// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;
import "forge-std/Test.sol";
import "../src/WarpVerifyFull.sol";
import "../src/WarpVerifyFull256.sol";
contract WarpVerifyRSTest is Test {
    function measure(uint256 t, uint256 logN, uint256 logM) external returns (uint256) {
        uint256 n = 2*t;
        uint256[] memory openings = new uint256[](n);
        for (uint256 i=0;i<n;i++){ openings[i]=i+1; }
        uint256 g0=gasleft(); WarpVerifyFull.verify(t, logN, logM, openings, 0xabc); return g0-gasleft();
    }
    function measure256(uint256 t, uint256 logN, uint256 logM) external returns (uint256) {
        uint256 n = 2*t;
        uint256[] memory openings = new uint256[](n);
        for (uint256 i=0;i<n;i++){ openings[i]=i+1; }
        uint256 g0=gasleft(); WarpVerifyFull256.verify(t, logN, logM, openings, 0xabc); return g0-gasleft();
    }
    function test_rs() public {
        // RS rate-1/4: log_n=11 (codeword 4k). IdentityCode baseline was log_n=9.
        emit log_named_uint("RS14 gf128 t=255 logN=11 (lam=173)", this.measure(255,11,4));
        emit log_named_uint("RS14 gf256 t=255 logN=11 (lam=173)", this.measure256(255,11,4));
        // Reduced-t at lambda=100 (delta=3/8): t=148 -> claim 256 (s=107 ood pad). openings = 2*148.
        emit log_named_uint("RS14 gf128 t=148 logN=11 (lam=100)", this.measure(148,11,4));
        emit log_named_uint("RS14 gf256 t=148 logN=11 (lam=100)", this.measure256(148,11,4));
        // Rate-1/8: delta=7/16=0.4375, lambda=128 -> t=189, log_n=12 (codeword 8k). claim=256 (s=66).
        emit log_named_uint("RS18 gf128 t=189 logN=12 (lam=128)", this.measure(189,12,4));
    }
}
