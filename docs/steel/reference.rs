//! S2 reference program: a Steel-subset view call with DATA-DEPENDENT control
//! flow. Compiled at -C opt-level=0 so the branches stay real `JumpCond`s.
//! The `n` selects how many path elements fold into the accumulator.
//! NOTE: no separate `#[panic_handler]` fn (its unwind routine linked in a
//! `Dyn`-continuation that the step-circuit lowering doesn't support); the
//! `loop {}` panic body is inlined instead.
#![no_std]

const C: u64 = 0x9e3779b97f4a7c15;

#[no_mangle]
pub fn steel_view_call(
    key: u64,
    value: u64,
    n: u64,
    path0: u64,
    path1: u64,
    path2: u64,
    path3: u64,
) -> u64 {
    let mut acc = key ^ value;
    if n > 0 {
        acc = acc.wrapping_mul(C) ^ path0;
    }
    if n > 1 {
        acc = acc.wrapping_mul(C) ^ path1;
    }
    if n > 2 {
        acc = acc.wrapping_mul(C) ^ path2;
    }
    if n > 3 {
        acc = acc.wrapping_mul(C) ^ path3;
    }
    acc
}
