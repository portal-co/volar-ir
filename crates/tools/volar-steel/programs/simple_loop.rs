//! Minimal isolation: does a loop-carried scalar work through the pipeline?
#![no_std]

#[no_mangle]
pub fn count_to(n: u64) -> u64 {
    let mut i = 0u64;
    let mut acc = 0u64;
    while i < n {
        acc = acc.wrapping_add(i);
        i += 1;
    }
    acc
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
