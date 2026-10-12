//! Minimal interpreter for exactly the bytecode subset `RawEvm` emits:
//! `PUSH0`..`PUSH32`, `CALLDATALOAD`, `MLOAD`, `MSTORE`, `AND`, `OR`,
//! `XOR`, `NOT`, `RETURN`. Word-aligned memory only. Carries a static gas
//! table (sources: <https://ethereum.org/developers/docs/evm/opcodes/> and
//! the Yellow Paper, appendix G) plus exact memory-expansion accounting,
//! `C(a) = 3a + a²/512` for `a` 32-byte words, charged incrementally.

use std::collections::BTreeMap;

use super::word::{self, Word};

/// Static gas for the "verylow" tier: `AND`/`OR`/`XOR`/`NOT`/`MLOAD`/
/// `MSTORE`/`CALLDATALOAD`/`PUSH1..32`.
const GAS_VERYLOW: u64 = 3;
/// Static gas for `PUSH0` ("base" tier).
const GAS_BASE: u64 = 2;

/// Memory expansion cost for `a` allocated 32-byte words (YP, appendix G).
fn expansion_cost(words: u64) -> u64 {
    3 * words + words * words / 512
}

struct Vm {
    mem: BTreeMap<u64, Word>,
    gas: u64,
    mem_words: u64,
}

impl Vm {
    fn touch(&mut self, highest_byte: u64) {
        let words = highest_byte.div_ceil(32);
        if words > self.mem_words {
            self.gas += expansion_cost(words) - expansion_cost(self.mem_words);
            self.mem_words = words;
        }
    }
}

fn as_addr(word: Word) -> Result<u64, String> {
    let addr = word::as_u64(word).ok_or("address exceeds 64 bits")?;
    if addr % 32 != 0 {
        return Err("unaligned memory access".into());
    }
    Ok(addr)
}

/// Run `code` against packed calldata words (calldata offset `32 * i` reads
/// `calldata[i]`, zero-padded). Returns the returned words and total gas.
pub fn run(code: &[u8], calldata: &[Word]) -> Result<(Vec<Word>, u64), String> {
    let mut vm = Vm {
        mem: BTreeMap::new(),
        gas: 0,
        mem_words: 0,
    };
    let mut stack: Vec<Word> = Vec::new();
    let mut pc = 0usize;
    macro_rules! pop {
        () => {
            stack.pop().ok_or("stack underflow")?
        };
    }
    loop {
        let op = *code.get(pc).ok_or("ran past the end without RETURN")?;
        pc += 1;
        match op {
            0x5f => {
                vm.gas += GAS_BASE;
                stack.push(word::ZERO);
            }
            0x60..=0x7f => {
                let width = usize::from(op - 0x5f);
                let data = code.get(pc..pc + width).ok_or("truncated PUSH immediate")?;
                pc += width;
                let mut bytes = [0u8; 32];
                bytes[32 - width..].copy_from_slice(data);
                stack.push([
                    u64::from_be_bytes(bytes[24..32].try_into().expect("8 bytes")),
                    u64::from_be_bytes(bytes[16..24].try_into().expect("8 bytes")),
                    u64::from_be_bytes(bytes[8..16].try_into().expect("8 bytes")),
                    u64::from_be_bytes(bytes[0..8].try_into().expect("8 bytes")),
                ]);
                vm.gas += GAS_VERYLOW;
            }
            0x51 => {
                let addr = as_addr(pop!())?;
                vm.gas += GAS_VERYLOW;
                vm.touch(addr + 32);
                stack.push(vm.mem.get(&addr).copied().unwrap_or(word::ZERO));
            }
            0x52 => {
                let addr = as_addr(pop!())?;
                let value = pop!();
                vm.gas += GAS_VERYLOW;
                vm.touch(addr + 32);
                vm.mem.insert(addr, value);
            }
            0x35 => {
                let offset = word::as_u64(pop!()).ok_or("calldata offset exceeds 64 bits")?;
                if offset % 32 != 0 {
                    return Err("unaligned CALLDATALOAD".into());
                }
                vm.gas += GAS_VERYLOW;
                stack.push(
                    calldata
                        .get((offset / 32) as usize)
                        .copied()
                        .unwrap_or(word::ZERO),
                );
            }
            0x16 | 0x17 | 0x18 => {
                let b = pop!();
                let a = pop!();
                vm.gas += GAS_VERYLOW;
                stack.push(match op {
                    0x16 => word::and(a, b),
                    0x17 => word::or(a, b),
                    _ => word::xor(a, b),
                });
            }
            0x19 => {
                let a = pop!();
                vm.gas += GAS_VERYLOW;
                stack.push(word::not(a));
            }
            0xf3 => {
                let offset = as_addr(pop!())?;
                let len = word::as_u64(pop!()).ok_or("RETURN length exceeds 64 bits")?;
                if len % 32 != 0 {
                    return Err("RETURN length is not word-aligned".into());
                }
                // RETURN itself is 0 gas; only memory expansion applies.
                if len > 0 {
                    vm.touch(offset + len);
                }
                let words = (0..len / 32)
                    .map(|i| {
                        vm.mem
                            .get(&(offset + 32 * i))
                            .copied()
                            .unwrap_or(word::ZERO)
                    })
                    .collect();
                return Ok((words, vm.gas));
            }
            _ => return Err(format!("unsupported opcode {op:#04x} at {}", pc - 1)),
        }
        if stack.len() > 1024 {
            return Err("stack overflow".into());
        }
    }
}
