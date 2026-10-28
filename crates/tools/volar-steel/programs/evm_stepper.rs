//! S6b: a bounded EVM stepper over a fixed opcode subset — the Steel revm-
//! subset core, with ALL state as bare scalars (no arrays, no structs) so the
//! state stays as block params, not the calling convention's frame (see
//! https://github.com/portal-co/volar-ir/issues/5). The MUXes (the indexed
//! accesses) are inline if-chains. Compiled with `-C opt-level=0`.

#![no_std]

const OP_STOP: u64 = 0x00;
const OP_ADD: u64 = 0x01;
const OP_MUL: u64 = 0x02;
const OP_SLOAD: u64 = 0x54;
const OP_SSTORE: u64 = 0x55;
const OP_PUSH1: u64 = 0x60;

/// One bounded EVM run. All state (the 4-deep stack s0..3 + sp, the 2-slot
/// storage store0/1, the pc, the loop counter i) is bare scalars; the indexed
/// accesses (code[pc], stack[sp], store[key]) are inline MUXes.
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
    steps: u64,
) -> u64 {
    let mut s0: u64 = 0;
    let mut s1: u64 = 0;
    let mut s2: u64 = 0;
    let mut s3: u64 = 0;
    let mut sp: u64 = 0;
    let mut store0 = storage0;
    let mut store1 = storage1;
    let mut pc: u64 = 0;
    let mut i = 0u64;
    while i < steps && pc < 8 {
        // code[pc]
        let op = if pc == 0 {
            code0
        } else if pc == 1 {
            code1
        } else if pc == 2 {
            code2
        } else if pc == 3 {
            code3
        } else if pc == 4 {
            code4
        } else if pc == 5 {
            code5
        } else if pc == 6 {
            code6
        } else {
            code7
        };
        if op == OP_STOP {
            break;
        } else if op == OP_PUSH1 {
            pc += 1;
            let imm = if pc == 0 {
                code0
            } else if pc == 1 {
                code1
            } else if pc == 2 {
                code2
            } else if pc == 3 {
                code3
            } else if pc == 4 {
                code4
            } else if pc == 5 {
                code5
            } else if pc == 6 {
                code6
            } else {
                code7
            };
            // push(imm)
            if sp < 4 {
                if sp == 0 {
                    s0 = imm;
                } else if sp == 1 {
                    s1 = imm;
                } else if sp == 2 {
                    s2 = imm;
                } else {
                    s3 = imm;
                }
                sp += 1;
            }
        } else if op == OP_ADD || op == OP_MUL {
            if sp >= 2 {
                // a = stack[sp-1], b = stack[sp-2]
                let a = if sp - 1 == 0 {
                    s0
                } else if sp - 1 == 1 {
                    s1
                } else if sp - 1 == 2 {
                    s2
                } else {
                    s3
                };
                let b = if sp - 2 == 0 {
                    s0
                } else if sp - 2 == 1 {
                    s1
                } else if sp - 2 == 2 {
                    s2
                } else {
                    s3
                };
                let r = if op == OP_ADD {
                    a.wrapping_add(b)
                } else {
                    a.wrapping_mul(b)
                };
                // stack[sp-2] = r
                if sp - 2 == 0 {
                    s0 = r;
                } else if sp - 2 == 1 {
                    s1 = r;
                } else if sp - 2 == 2 {
                    s2 = r;
                } else {
                    s3 = r;
                }
                sp -= 1;
            }
        } else if op == OP_SLOAD {
            if sp >= 1 {
                let key = (if sp - 1 == 0 {
                    s0
                } else if sp - 1 == 1 {
                    s1
                } else if sp - 1 == 2 {
                    s2
                } else {
                    s3
                }) & 1;
                let val = if key == 0 { store0 } else { store1 };
                if sp - 1 == 0 {
                    s0 = val;
                } else if sp - 1 == 1 {
                    s1 = val;
                } else if sp - 1 == 2 {
                    s2 = val;
                } else {
                    s3 = val;
                }
            }
        } else if op == OP_SSTORE {
            if sp >= 2 {
                let key = (if sp - 1 == 0 {
                    s0
                } else if sp - 1 == 1 {
                    s1
                } else if sp - 1 == 2 {
                    s2
                } else {
                    s3
                }) & 1;
                let val = if sp - 2 == 0 {
                    s0
                } else if sp - 2 == 1 {
                    s1
                } else if sp - 2 == 2 {
                    s2
                } else {
                    s3
                };
                if key == 0 {
                    store0 = val;
                } else {
                    store1 = val;
                }
                sp -= 2;
            }
        }
        pc += 1;
        i += 1;
    }
    // top of stack
    if sp > 0 {
        if sp - 1 == 0 {
            s0
        } else if sp - 1 == 1 {
            s1
        } else if sp - 1 == 2 {
            s2
        } else {
            s3
        }
    } else {
        0
    }
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
