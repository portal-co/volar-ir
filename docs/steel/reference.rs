//! S1 reference program: a minimal Steel-subset view call — the state-access
//! Merkle proof for a single storage read. `key`/`value` form the leaf; the
//! 4-deep `path` folds up to a root. A stand-in hash (multiply-xor) keeps S1
//! about the *frontend*, not the in-circuit Keccak (deferred to S6).
#![no_std]
#![no_main]

#[no_mangle]
pub fn steel_view_call(key: u64, value: u64, path0: u64, path1: u64, path2: u64, path3: u64) -> u64 {
    let mut acc = key ^ value;
    let path = [path0, path1, path2, path3];
    let mut i = 0usize;
    while i < 4 {
        acc = acc.wrapping_mul(0x9e3779b97f4a7c15) ^ path[i];
        i += 1;
    }
    acc
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}
