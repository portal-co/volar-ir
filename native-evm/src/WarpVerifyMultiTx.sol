// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;

/// GF(2^128) field lib (measured). For the GF(2^256) variant, Gf256 is swapped in.
library GfM {
    uint256 internal constant REDUCE = 0x87;
    uint256 internal constant HIGH = 1 << 127;
    uint256 internal constant MASK = (1 << 128) - 1;
    function mul(uint256 a, uint256 b) internal pure returns (uint256) {
        a &= MASK; b &= MASK; uint256 p = 0;
        for (uint256 i = 0; i < 128; i++) { if (b & 1 != 0) p ^= a; uint256 h = a & HIGH; a = (a << 1) & MASK; if (h != 0) a ^= REDUCE; b >>= 1; }
        return p;
    }
}

/// The WARP succinct verifier as a RESUMABLE, MULTI-TRANSACTION contract (S5).
///
/// A single-transaction succinct verifier exceeds the block gas limit for the
/// larger proof configurations (see `evm-e2e-pipeline-plan.md` P3). This splits
/// the verification across transactions: each `processOpeningsChunk` /
/// `processSumcheckRound` call does a bounded chunk of work and persists the
/// verifier state on-chain; `finalize` runs the tail checks and exposes the
/// verdict.
///
/// **Soundness note (the Fiat-Shamir binding across transactions).** The
/// transcript is sequential: each challenge is derived from the prior
/// absorptions. The split is along the transcript order — a chunk never uses a
/// challenge before the absorption that derives it has completed. The persisted
/// `transcriptState` (a running Keccak hash of the absorbed proof elements) is
/// the Fiat-Shamir state carried between transactions. (This is the on-chain
/// analogue of the off-chain resumable accumulation, E4.)
///
/// The persisted state is small: the parameters, the running eq-weighted
/// accumulator, the sumcheck claim, the processed-openings count, and the
/// transcript state — a handful of 32-byte words.
contract WarpVerifyMultiTx {
    /// Verifier phases: the multi-tx state machine.
    enum Phase { NotStarted, Openings, Sumcheck, Finalized }

    Phase public phase;
    // Parameters (immutable for a verification session).
    uint256 public t;
    uint256 public logN;
    uint256 public logM;
    uint256 public seed;
    // The running verifier state.
    uint256 public acc;              // the eq-weighted field accumulator
    uint256 public claim;            // the sumcheck running claim
    uint256 public processed;        // Merkle openings processed so far
    uint256 public sumcheckRound;    // sumcheck rounds processed so far
    bytes32 public transcriptState;  // the Fiat-Shamir transcript state
    bool public verdict;             // the final verdict (set by finalize)

    uint256 constant MASK = (1 << 128) - 1;

    event Began(uint256 t, uint256 logN, uint256 logM);
    event OpeningsProcessed(uint256 count, uint256 total);
    event SumcheckRound(uint256 round);
    event Finalized(bool verdict);

    /// Stage 0: begin a verification session. `seed` binds the transcript to
    /// the proof's commitments (the Merkle roots + the instance) — the caller
    /// computes it off-chain as the Fiat-Shamir hash of those commitments.
    function begin(uint256 _t, uint256 _logN, uint256 _logM, uint256 _seed) external {
        require(phase == Phase.NotStarted, "already begun");
        t = _t; logN = _logN; logM = _logM; seed = _seed;
        transcriptState = keccak256(abi.encodePacked(_seed, _t, _logN, _logM));
        phase = Phase.Openings;
        emit Began(_t, _logN, _logM);
    }

    /// Process a chunk of Merkle openings (ell*t total across all chunks).
    /// Each opening: the leaf value folded up the Merkle path (logN hashes),
    /// then the eq-weighted accumulation. `chunk` is bounded so the per-tx gas
    /// stays under the target chain's block limit.
    function processOpeningsChunk(uint256[] calldata openingLeaves) external {
        require(phase == Phase.Openings, "wrong phase");
        for (uint256 k = 0; k < openingLeaves.length; k++) {
            uint256 oi = processed;
            uint256 node = openingLeaves[k];
            // Fold the Merkle path (the logN sibling hashes derived from the
            // transcript — the real verifier reads them from the proof; here
            // the path is bound into the transcript state).
            for (uint256 d = 0; d < logN; d++) {
                bytes32 sib = keccak256(abi.encodePacked(transcriptState, oi, d));
                node = uint256(keccak256(abi.encodePacked(bytes32(node), sib)));
            }
            // The eq-weighted accumulation (the codeword-batching).
            acc ^= GfM.mul(_eqWeight(oi), node & MASK);
            // Advance the transcript.
            transcriptState = keccak256(abi.encodePacked(transcriptState, node));
            processed++;
        }
        emit OpeningsProcessed(processed, 2 * t);
        if (processed >= 2 * t) {
            phase = Phase.Sumcheck;
            claim = acc; // the batched claim the sumchecks reduce
        }
    }

    /// Process one sumcheck round (the two sumchecks, log ell + log N rounds).
    function processSumcheckRound() external {
        require(phase == Phase.Sumcheck, "wrong phase");
        uint256 round = sumcheckRound;
        // The round challenge from the transcript.
        uint256 ch = uint256(keccak256(abi.encodePacked(transcriptState, round))) & MASK;
        // The quadratic univariate check (the round poly at 0, 1, ch).
        uint256 p0 = GfM.mul(claim, ch);
        uint256 p1 = GfM.mul(p0, ch) ^ claim;
        claim = p1 ^ GfM.mul(ch, ch);
        transcriptState = keccak256(abi.encodePacked(transcriptState, round));
        sumcheckRound++;
        emit SumcheckRound(round);
        if (sumcheckRound >= 1 + logN) {
            phase = Phase.Finalized;
        }
    }

    /// Finalize: the tail checks and the verdict. After this, `verdict` holds
    /// whether the proof verified.
    function finalize() external {
        require(phase == Phase.Finalized || sumcheckRound >= 1 + logN, "not done");
        // The tail check: the final claim must be consistent (nonzero-binding).
        // (The real verifier checks the MLE/eq consistency; here the verdict
        // binds the accumulated claim.)
        verdict = (claim != 0 || acc == 0); // placeholder tail check
        phase = Phase.Finalized;
        emit Finalized(verdict);
    }

    /// eq weight for opening index oi — the batching eq_eval. (The real
    /// verifier computes the eq_evals DP; here a per-index scalar for the
    /// cost model.)
    function _eqWeight(uint256 oi) internal view returns (uint256) {
        return uint256(keccak256(abi.encodePacked(seed, oi))) & MASK;
    }
}
