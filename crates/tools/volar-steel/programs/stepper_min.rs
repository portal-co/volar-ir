//! #5 bisection: a minimal stepper (just PUSH1 + ADD, no jumps/storage).
#![no_std]

const OP_STOP: u64 = 0x00;
const OP_ADD: u64 = 0x01;
const OP_PUSH1: u64 = 0x60;

#[no_mangle]
#[allow(clippy::too_many_arguments)]
pub fn step_min(
    code0: u64, code1: u64, code2: u64, code3: u64, code4: u64,
    steps: u64,
) -> u64 {
    let mut s0: u64 = 0;
    let mut s1: u64 = 0;
    let mut sp: u64 = 0;
    let mut pc: u64 = 0;
    let mut i = 0u64;
    while i < steps && pc < 5 {
        let op = if pc == 0 { code0 } else if pc == 1 { code1 } else if pc == 2 { code2 } else if pc == 3 { code3 } else { code4 };
        if op == OP_STOP {
            break;
        } else if op == OP_PUSH1 {
            pc += 1;
            let imm = if pc == 0 { code0 } else if pc == 1 { code1 } else if pc == 2 { code2 } else if pc == 3 { code3 } else { code4 };
            if sp < 2 {
                if sp == 0 { s0 = imm; } else { s1 = imm; }
                sp += 1;
            }
        } else if op == OP_ADD {
            if sp >= 2 {
                let a = if sp - 1 == 0 { s0 } else { s1 };
                let b = if sp - 2 == 0 { s0 } else { s1 };
                let r = a.wrapping_add(b);
                if sp - 2 == 0 { s0 = r; } else { s1 = r; }
                sp -= 1;
            }
        }
        pc += 1;
        i += 1;
    }
    if sp > 0 {
        if sp - 1 == 0 { s0 } else { s1 }
    } else {
        0
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
