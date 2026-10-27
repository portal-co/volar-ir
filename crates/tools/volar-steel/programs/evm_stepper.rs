//! S6b: a bounded EVM stepper over a fixed opcode subset — the Steel revm-
//! subset core. Steps a tiny EVM (stack + storage + PC) over a fixed opcode
//! subset for a bounded number of steps, with the code + storage passed as
//! scalar args (matching the import convention). Compiled with `-C opt-level=0`
//! so the branches/loops stay as real control flow for the pipeline.

#![no_std]

// The opcode subset (the Steel view-call core).
const OP_STOP: u64 = 0x00;
const OP_ADD: u64 = 0x01;
const OP_MUL: u64 = 0x02;
const OP_SLOAD: u64 = 0x54;
const OP_SSTORE: u64 = 0x55;
const OP_PUSH1: u64 = 0x60;

/// One bounded EVM run. `code0..code7` is an 8-word program, `storage0..3` the
/// storage, `steps` the step bound. Returns the top of the stack (the result).
#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub fn evm_step(
    code0: u64,
    code1: u64,
    code2: u64,
    code3: u64,
    code4: u64,
    code5: u64,
    code6: u64,
    code7: u64,
    storage0: u64,
    storage1: u64,
    storage2: u64,
    storage3: u64,
    steps: u64,
) -> u64 {
    let code: [u64; 8] = [code0, code1, code2, code3, code4, code5, code6, code7];
    let mut stack: [u64; 4] = [0; 4];
    let mut sp: usize = 0;
    let mut store: [u64; 2] = [storage0, storage1];
    let mut pc: usize = 0;
    let mut i = 0u64;
    while i < steps && pc < 8 {
        let op = code[pc];
        if op == OP_STOP {
            break;
        } else if op == OP_PUSH1 {
            pc += 1;
            if sp < 4 {
                stack[sp] = code[pc];
                sp += 1;
            }
        } else if op == OP_ADD {
            if sp >= 2 {
                stack[sp - 2] = stack[sp - 1].wrapping_add(stack[sp - 2]);
                sp -= 1;
            }
        } else if op == OP_MUL {
            if sp >= 2 {
                stack[sp - 2] = stack[sp - 1].wrapping_mul(stack[sp - 2]);
                sp -= 1;
            }
        } else if op == OP_SLOAD {
            if sp >= 1 {
                stack[sp - 1] = store[(stack[sp - 1] & 1) as usize];
            }
        } else if op == OP_SSTORE {
            if sp >= 2 {
                store[(stack[sp - 1] & 1) as usize] = stack[sp - 2];
                sp -= 2;
            }
        }
        pc += 1;
        i += 1;
    }
    if sp > 0 {
        stack[sp - 1]
    } else {
        0
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
