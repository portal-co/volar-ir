// SPDX-License-Identifier: MIT
pragma solidity ^0.8.24;
import "forge-std/Test.sol";
import "../src/GenWarp.sol";

contract GenWarpTest is Test {
    GenWarp g;
    function setUp() public { g = new GenWarp(); }
    function refMul(uint256 a, uint256 b) internal pure returns (uint256) {
        uint256 MASK=(1<<128)-1; a&=MASK; b&=MASK; uint256 p=0; uint256 H=1<<127;
        for(uint256 i=0;i<128;i++){ if(b&1!=0)p^=a; uint256 h=a&H; a=(a<<1)&MASK; if(h!=0)a^=0x87; b>>=1; }
        return p;
    }
    function refKec(uint256 l, uint256 r) internal pure returns (uint256) {
        return uint256(keccak256(abi.encodePacked(bytes32(l), bytes32(r)))) & ((1<<128)-1);
    }
    function rng(uint256 x, uint256 i) internal pure returns (uint256) { unchecked { return (x*0x9e3779b97f4a7c15+i) & ((1<<128)-1); } }

    function test_all() public {
        uint256 seed = 0x0123456789abcdef;
        // dot4
        uint256[] memory a = new uint256[](4); uint256[] memory b = new uint256[](4);
        for(uint256 i=0;i<4;i++){ a[i]=rng(seed,i); b[i]=rng(seed,i+50); }
        uint256 expDot=0; for(uint256 i=0;i<4;i++){ expDot ^= refMul(a[i],b[i]); }
        uint256 g0=gasleft(); uint256 gotDot=g.mle_dot(a,b); uint256 gasDot=g0-gasleft();
        assertEq(gotDot, expDot, "dot correct");
        // mle8
        uint256[] memory values = new uint256[](8); uint256[] memory point = new uint256[](4);
        for(uint256 i=0;i<8;i++){ values[i]=rng(seed,i);} for(uint256 j=0;j<4;j++){ point[j]=rng(seed,j+7);}
        uint256[] memory eq = new uint256[](8); eq[0]=1;
        for(uint256 c=0;c<3;c++){ uint256 coord=point[c]; for(uint256 idx=0;idx<(1<<c);idx++){ uint256 pr=refMul(eq[idx],coord); eq[idx]=eq[idx]^pr; eq[idx+(1<<c)]=pr; } }
        uint256 expMle=0; for(uint256 i=0;i<8;i++){ expMle ^= refMul(values[i],eq[i]); }
        uint256 g1=gasleft(); uint256 gotMle=g.mle8(values,point); uint256 gasMle=g1-gasleft();
        assertEq(gotMle, expMle, "mle8 correct");
        // merkle8
        uint256[] memory l = new uint256[](8);
        for(uint256 i=0;i<8;i++){ l[i]=rng(seed,i); }
        uint256 ha=refKec(l[0],l[1]); uint256 hb=refKec(l[2],l[3]); uint256 hc=refKec(l[4],l[5]); uint256 hd=refKec(l[6],l[7]);
        uint256 he=refKec(ha,hb); uint256 hf=refKec(hc,hd); uint256 expRoot=refKec(he,hf);
        uint256 g2=gasleft(); uint256 gotRoot=g.merkle8(l); uint256 gasMerkle=g2-gasleft();
        assertEq(gotRoot, expRoot, "merkle8 correct");
        emit log_named_uint("gas codegen dot4", gasDot);
        emit log_named_uint("gas codegen mle8", gasMle);
        emit log_named_uint("gas codegen merkle8", gasMerkle);
    }
}
