//! Minimal #5 repro: a loop with a frame-carried scalar. The local array
//! forces the frame; the scalar (the loop counter) goes to the frame too, and
//! its update doesn't propagate across the movfuscated steps.
#![no_std]

#[no_mangle]
pub fn sum_with_array(a0: u64, a1: u64, n: u64) -> u64 {
    // A local array (forces the frame).
    let arr: [u64; 2] = [a0, a1];
    let mut i = 0u64;
    let mut acc = 0u64;
    while i < n {
        // Read the array (so the array is live) and accumulate.
        acc = acc.wrapping_add(arr[(i & 1) as usize]);
        i += 1;
    }
    acc
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
