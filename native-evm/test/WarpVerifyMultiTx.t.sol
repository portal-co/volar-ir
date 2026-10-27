// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;
import "forge-std/Test.sol";
import "../src/WarpVerifyMultiTx.sol";

/// S5: drive the multi-transaction WARP verifier across chunked openings +
/// the sumcheck rounds, measuring the PER-TRANSACTION gas (the point of the
/// multi-tx split: each tx stays under the target chain's block limit).
contract WarpVerifyMultiTxTest is Test {
    WarpVerifyMultiTx v;


    function test_multi_tx(uint256 t, uint256 logN, uint256 chunkSize, string memory label) internal {
        v = new WarpVerifyMultiTx(); // fresh contract per config (begin is one-shot)
        uint256 logM = 4;
        uint256 total = 2 * t; // ell*t openings
        uint256 g0 = gasleft();
        v.begin(t, logN, logM, 0xabc);
        emit log_named_uint(string.concat("gas begin (", label, ")"), g0 - gasleft());

        // Process the openings in chunks (each chunk = one "transaction").
        uint256 maxChunkGas = 0;
        uint256 oi = 0;
        while (oi < total) {
            uint256 len = chunkSize;
            if (oi + len > total) len = total - oi;
            uint256[] memory chunk = new uint256[](len);
            for (uint256 k = 0; k < len; k++) chunk[k] = 0x1000 + oi + k;
            uint256 g = gasleft();
            v.processOpeningsChunk(chunk);
            uint256 used = g - gasleft();
            if (used > maxChunkGas) maxChunkGas = used;
            oi += len;
        }
        emit log_named_uint(string.concat("gas max openings chunk (", label, ")"), maxChunkGas);

        // The sumcheck rounds.
        uint256 sumGas = 0;
        while (v.sumcheckRound() < 1 + logN) {
            uint256 g = gasleft();
            v.processSumcheckRound();
            uint256 used = g - gasleft();
            if (used > sumGas) sumGas = used;
        }
        emit log_named_uint(string.concat("gas max sumcheck round (", label, ")"), sumGas);

        uint256 g1 = gasleft();
        v.finalize();
        emit log_named_uint(string.concat("gas finalize (", label, ")"), g1 - gasleft());
        emit log_named_uint(string.concat("verdict (", label, ")"), v.verdict() ? 1 : 0);
        emit log_named_uint(string.concat("steps/chunks (", label, ")"), (total + chunkSize - 1) / chunkSize);
    }

    function test_multi_tx_verifier() public {
        // The sound RS config (t=255, logN=11) — the verifier that ~fits L1
        // single-tx; the multi-tx split caps the per-tx gas.
        test_multi_tx(255, 11, 32, "t=255 logN=11 chunk=32");
        // A larger config (t=511, logN=20) — exceeds L1 single-tx; the multi-tx
        // split is the enabler.
        test_multi_tx(511, 20, 16, "t=511 logN=20 chunk=16");
    }
}
