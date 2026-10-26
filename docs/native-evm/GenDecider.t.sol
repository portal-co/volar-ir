// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;
import "forge-std/Test.sol";
import "../src/GenDecider.sol";

contract GenDeciderTest is Test {
    GenDecider g;
    function setUp() public { g = new GenDecider(); }
    function refMul(uint256 a, uint256 b) internal pure returns (uint256) {
        uint256 MASK=(1<<128)-1; a&=MASK; b&=MASK; uint256 p=0; uint256 H=1<<127;
        for(uint256 i=0;i<128;i++){ if(b&1!=0)p^=a; uint256 h=a&H; a=(a<<1)&MASK; if(h!=0)a^=0x87; b>>=1; }
        return p;
    }
    function refKec(uint256 l, uint256 r) internal pure returns (uint256) {
        return uint256(keccak256(abi.encodePacked(bytes32(l), bytes32(r)))) & ((1<<128)-1);
    }
    function rng(uint256 x, uint256 i) internal pure returns (uint256) { unchecked { return (x*0x9e3779b97f4a7c15+i) & ((1<<128)-1); } }

    function test_decider() public {
        uint256 seed = 0x0123456789abcdef;
        // codeword (N=8), witness == codeword (identity code)
        uint256[] memory codeword = new uint256[](8);
        for (uint256 i=0;i<8;i++){ codeword[i]=rng(seed,i); }
        uint256[] memory witness = new uint256[](8);
        for (uint256 i=0;i<8;i++){ witness[i]=codeword[i]; }
        uint256[] memory alpha = new uint256[](4);
        for (uint256 j=0;j<4;j++){ alpha[j]=rng(seed,j+100); }
        uint256[] memory beta = new uint256[](8);
        for (uint256 j=0;j<8;j++){ beta[j]=rng(seed,j+200); }
        // index: M=16 constraints, TERMS=3, DEG=2. Make each constraint 1 term, 1 var.
        uint256[] memory coeffs = new uint256[](48);
        uint256[] memory nvars = new uint256[](48);
        uint256[] memory tvars = new uint256[](96);
        uint256[] memory nterms = new uint256[](16);
        for (uint256 c=0;c<16;c++){
            nterms[c]=1;
            nvars[c*3]=1;
            coeffs[c*3]=rng(seed,c+300);
            tvars[(c*3)*2]= (c % 8) + 1;  // a witness index (var>=1 -> witness[var-1])
        }
        // reference values
        uint256 root = refKec(refKec(refKec(codeword[0],codeword[1]), refKec(codeword[2],codeword[3])),
                              refKec(refKec(codeword[4],codeword[5]), refKec(codeword[6],codeword[7])));
        uint256 mu = g.mle_eval(codeword, alpha);
        uint256 eta = g.bundled_eval(coeffs, nvars, tvars, nterms, beta, witness);

        uint256 g0=gasleft();
        bool ok = g.decide_toy(coeffs, nvars, tvars, nterms, root, alpha, mu, beta, eta, codeword, witness);
        uint256 gasAccept = g0-gasleft();
        assertTrue(ok, "decider accepts a valid toy acc");
        emit log_named_uint("gas decide_toy ACCEPT", gasAccept);

        // tamper eta -> reject
        bool bad = g.decide_toy(coeffs, nvars, tvars, nterms, root, alpha, mu, beta, eta ^ 1, codeword, witness);
        assertTrue(!bad, "decider rejects a tampered eta");
        // tamper the codeword -> reject (root + mle + bundled all break)
        uint256[] memory badcw = new uint256[](8);
        for (uint256 i=0;i<8;i++){ badcw[i]=codeword[i]; } badcw[3] ^= 5;
        bool bad2 = g.decide_toy(coeffs, nvars, tvars, nterms, root, alpha, mu, beta, eta, badcw, witness);
        assertTrue(!bad2, "decider rejects a tampered codeword");
    }
}
