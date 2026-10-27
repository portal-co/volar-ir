//! Isolation: does a loop-carried local array (write + read across loop
//! iterations) work through the pipeline?
#![no_std]

#[no_mangle]
pub fn push_pop(a: u64, b: u64) -> u64 {
    let mut stack: [u64; 4] = [0; 4];
    let mut sp: usize = 0;
    let mut i = 0u64;
    // Push a, push b, then add them (the EVM stepper's pattern).
    while i < 2 {
        if sp < 4 {
            stack[sp] = if i == 0 { a } else { b };
            sp += 1;
        }
        i += 1;
    }
    if sp >= 2 {
        stack[sp - 2] = stack[sp - 1].wrapping_add(stack[sp - 2]);
        sp -= 1;
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
