//! Minimal isolation test: does a runtime-indexed local-array read work
//! through the LLVM import + the full pipeline?
#![no_std]

#[no_mangle]
pub fn read_idx(a0: u64, a1: u64, a2: u64, a3: u64, idx: u64) -> u64 {
    let arr: [u64; 4] = [a0, a1, a2, a3];
    arr[(idx & 3) as usize]
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
