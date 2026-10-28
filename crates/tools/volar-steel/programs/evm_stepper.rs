//! S6c: a fuller EVM stepper — the Steel revm-subset core, extended with the
//! arithmetic (SUB/DIV/MOD), the comparisons (LT/GT/EQ/ISZERO), and the jumps
//! (JUMP/JUMPI). All state is bare scalars (no arrays/structs → no frame) at
//! `-C opt-level=1`, sidestepping https://github.com/portal-co/volar-ir/issues/5.
//! The EVM's *memory* (MLOAD/MSTORE) is the one piece that needs a real
//! (frame-based) byte array — deferred to the #5 fix.

#![no_std]

const OP_STOP: u64 = 0x00;
const OP_ADD: u64 = 0x01;
const OP_MUL: u64 = 0x02;
const OP_SUB: u64 = 0x03;
const OP_DIV: u64 = 0x04;
const OP_LT: u64 = 0x10;
const OP_GT: u64 = 0x11;
const OP_EQ: u64 = 0x14;
const OP_ISZERO: u64 = 0x15;
const OP_SLOAD: u64 = 0x54;
const OP_SSTORE: u64 = 0x55;
const OP_JUMP: u64 = 0x56;
const OP_JUMPI: u64 = 0x57;
const OP_JUMPDEST: u64 = 0x5b;
const OP_PUSH1: u64 = 0x60;

/// One bounded EVM run over a 4-deep stack, 2-slot storage, 8-word code.
/// All state is bare scalars; the indexed accesses are inline MUXes.
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
        } else if op == OP_JUMP || op == OP_JUMPI {
            // JUMP: pc = stack[sp-1]. JUMPI: if stack[sp-2] != 0 { pc = stack[sp-1] }.
            if sp >= 1 {
                let dest = if sp - 1 == 0 {
                    s0
                } else if sp - 1 == 1 {
                    s1
                } else if sp - 1 == 2 {
                    s2
                } else {
                    s3
                };
                let do_jump = if op == OP_JUMP {
                    sp -= 1;
                    true
                } else {
                    // JUMPI
                    if sp >= 2 {
                        let cond = if sp - 2 == 0 {
                            s0
                        } else if sp - 2 == 1 {
                            s1
                        } else if sp - 2 == 2 {
                            s2
                        } else {
                            s3
                        };
                        sp -= 2;
                        cond != 0
                    } else {
                        false
                    }
                };
                if do_jump {
                    pc = dest;
                    i += 1;
                    continue; // skip the pc += 1 below
                }
            }
        } else if op == OP_JUMPDEST {
            // no-op
        } else if op == OP_ISZERO {
            if sp >= 1 {
                let a = if sp - 1 == 0 {
                    s0
                } else if sp - 1 == 1 {
                    s1
                } else if sp - 1 == 2 {
                    s2
                } else {
                    s3
                };
                let r = if a == 0 { 1 } else { 0 };
                if sp - 1 == 0 {
                    s0 = r;
                } else if sp - 1 == 1 {
                    s1 = r;
                } else if sp - 1 == 2 {
                    s2 = r;
                } else {
                    s3 = r;
                }
            }
        } else if op == OP_ADD
            || op == OP_MUL
            || op == OP_SUB
            || op == OP_DIV
            || op == OP_LT
            || op == OP_GT
            || op == OP_EQ
        {
            if sp >= 2 {
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
                // EVM order: the top of the stack is the first operand.
                let r = if op == OP_ADD {
                    a.wrapping_add(b)
                } else if op == OP_MUL {
                    a.wrapping_mul(b)
                } else if op == OP_SUB {
                    a.wrapping_sub(b)
                } else if op == OP_DIV {
                    if b == 0 {
                        0
                    } else {
                        a / b
                    }
                } else if op == OP_LT {
                    if b < a {
                        1
                    } else {
                        0
                    }
                } else if op == OP_GT {
                    if b > a {
                        1
                    } else {
                        0
                    }
                } else {
                    // EQ
                    if a == b {
                        1
                    } else {
                        0
                    }
                };
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
