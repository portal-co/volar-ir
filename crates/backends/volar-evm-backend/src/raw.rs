// @pinnedness: unpinned
// @stability: very-unstable
// @ai: assisted

//! Raw EVM bytecode emitter.
//!
//! Produces straight-line runtime bytecode with no jumps, proving the
//! [`EvmEmit`] seam is not Solidity-shaped. The ABI is deliberately minimal:
//! packed input words are read with `CALLDATALOAD` from calldata offset
//! `32 * index` (no function selector), and packed output words are staged
//! to memory and returned with `RETURN`.
//!
//! Every lane slot lives in one scratch memory word at `scratch_base +
//! 32 * slot`; gates load operands with `MLOAD`, apply one word-wide
//! `AND`/`OR`/`XOR`/`NOT`, and store with `MSTORE` — the same
//! spill-everything discipline as the retro renderers. Constants use
//! `PUSH0` (all-zero, 2 gas) and `PUSH32` of 32 `0xff` bytes (all-ones):
//! `PUSH0; NOT` would cost 5 gas against `PUSH32`'s 3 but 33 fewer bytes of
//! code — deployment size versus runtime cost, with runtime cost winning
//! here.
//!
//! Gas sources: <https://ethereum.org/developers/docs/evm/opcodes/> and the
//! Yellow Paper, appendix G (`AND`/`OR`/`XOR`/`NOT`/`PUSH1..32`/
//! `MLOAD`/`MSTORE`/`CALLDATALOAD` = 3 gas, `PUSH0` = 2, `RETURN` = 0, plus
//! memory expansion `C(a) = 3a + a²/512` on the touched word count).

use crate::emit::EvmEmit;

const OP_AND: u8 = 0x16;
const OP_OR: u8 = 0x17;
const OP_XOR: u8 = 0x18;
const OP_NOT: u8 = 0x19;
const OP_CALLDATALOAD: u8 = 0x35;
const OP_MLOAD: u8 = 0x51;
const OP_MSTORE: u8 = 0x52;
const OP_PUSH0: u8 = 0x5f;
const OP_RETURN: u8 = 0xf3;

/// Configuration for [`RawEvm`].
#[derive(Debug, Clone, Copy)]
pub struct RawEvmConfig {
    /// Byte address of the first scratch word. Slot `s` lives at
    /// `scratch_base + 32 * s`; the returned output words are staged
    /// immediately after the highest used slot.
    pub scratch_base: u32,
}

impl Default for RawEvmConfig {
    fn default() -> Self {
        RawEvmConfig { scratch_base: 0x80 }
    }
}

/// An [`EvmEmit`] sink rendering straight-line EVM runtime bytecode.
pub struct RawEvm {
    cfg: RawEvmConfig,
    code: Vec<u8>,
    /// Buffered output stores; the output region base is only known once
    /// every slot has been seen, so these emit in [`EvmEmit::finish`].
    out_ops: Vec<(u32, u32)>,
    max_slot: Option<u32>,
}

impl RawEvm {
    /// A fresh emitter with the given configuration.
    pub fn new(cfg: RawEvmConfig) -> Self {
        RawEvm {
            cfg,
            code: Vec::new(),
            out_ops: Vec::new(),
            max_slot: None,
        }
    }

    fn touch(&mut self, slot: u32) {
        self.max_slot = Some(self.max_slot.map_or(slot, |max| max.max(slot)));
    }

    fn slot_addr(&self, slot: u32) -> u64 {
        u64::from(self.cfg.scratch_base) + 32 * u64::from(slot)
    }

    /// Minimal-width `PUSHn` of a small value (`PUSH0` for zero).
    fn push_uint(code: &mut Vec<u8>, value: u64) {
        if value == 0 {
            code.push(OP_PUSH0);
            return;
        }
        let bytes = value.to_be_bytes();
        let start = bytes.iter().position(|&b| b != 0).expect("nonzero value");
        let width = 8 - start;
        code.push(0x60 + (width as u8 - 1));
        code.extend_from_slice(&bytes[start..]);
    }

    /// `PUSH32` of the all-ones word.
    fn push_all_ones(code: &mut Vec<u8>) {
        code.push(0x60 + 31);
        code.extend_from_slice(&[0xff; 32]);
    }

    fn mload_from(&mut self, addr: u64) {
        Self::push_uint(&mut self.code, addr);
        self.code.push(OP_MLOAD);
    }

    /// Store the stack-top-below value at `addr` (`MSTORE` takes the offset
    /// on top of the value).
    fn mstore_to(&mut self, addr: u64) {
        Self::push_uint(&mut self.code, addr);
        self.code.push(OP_MSTORE);
    }
}

impl EvmEmit for RawEvm {
    type Output = Vec<u8>;

    fn input(&mut self, slot: u32, index: u32) {
        self.touch(slot);
        Self::push_uint(&mut self.code, 32 * u64::from(index));
        self.code.push(OP_CALLDATALOAD);
        self.mstore_to(self.slot_addr(slot));
    }

    fn constant(&mut self, slot: u32, value: bool) {
        self.touch(slot);
        if value {
            Self::push_all_ones(&mut self.code);
        } else {
            self.code.push(OP_PUSH0);
        }
        self.mstore_to(self.slot_addr(slot));
    }

    fn and(&mut self, dst: u32, a: u32, b: u32) {
        self.gate2(dst, a, b, OP_AND);
    }

    fn or(&mut self, dst: u32, a: u32, b: u32) {
        self.gate2(dst, a, b, OP_OR);
    }

    fn xor(&mut self, dst: u32, a: u32, b: u32) {
        self.gate2(dst, a, b, OP_XOR);
    }

    fn not(&mut self, dst: u32, a: u32) {
        self.touch(dst);
        self.mload_from(self.slot_addr(a));
        self.code.push(OP_NOT);
        self.mstore_to(self.slot_addr(dst));
    }

    fn output(&mut self, index: u32, slot: u32) {
        self.out_ops.push((index, slot));
    }

    fn finish(mut self) -> Vec<u8> {
        let num_slots = self.max_slot.map_or(0, |max| u64::from(max) + 1);
        let out_base = u64::from(self.cfg.scratch_base) + 32 * num_slots;
        let out_ops = std::mem::take(&mut self.out_ops);
        for (index, slot) in &out_ops {
            self.mload_from(self.slot_addr(*slot));
            self.mstore_to(out_base + 32 * u64::from(*index));
        }
        Self::push_uint(&mut self.code, 32 * out_ops.len() as u64);
        Self::push_uint(&mut self.code, out_base);
        self.code.push(OP_RETURN);
        self.code
    }
}

impl RawEvm {
    fn gate2(&mut self, dst: u32, a: u32, b: u32, op: u8) {
        self.touch(dst);
        self.mload_from(self.slot_addr(a));
        self.mload_from(self.slot_addr(b));
        self.code.push(op);
        self.mstore_to(self.slot_addr(dst));
    }
}
