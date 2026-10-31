//! Bulk-memory differential program: constant-length memset (zero and
//! nonzero fills), constant-length memcpy (intrinsic), and a spot-cell
//! checksum. Loop-free on purpose: the program's own loops trip a
//! pre-existing lowering/eval param-threading off-by-one (documented in
//! the probe notes), unrelated to bulk memory. Imported in both
//! `BulkMemoryMode`s — see `bulk_memory_loop_matches_unroll`.

#[no_mangle]
pub extern "C" fn bulk_sum(sel: u32) -> u32 {
    let mut a: [u8; 300] = [0u8; 300];
    let mut b: [u8; 300] = [7u8; 300];
    if sel & 1 != 0 {
        a[0] = 3;
        a[299] = 5;
        // SAFETY: same allocation context, disjoint 300-byte regions,
        // sizes checked by construction in this test program. An
        // intrinsic copy (no bounds-assert panic path).
        unsafe {
            b.as_mut_ptr().copy_from_nonoverlapping(a.as_ptr(), 300);
        }
    }
    // Shift-only spot checksum: non-power-of-two multiplies would dwarf
    // the bulk traffic being measured.
    ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | ((b[299] as u32) << 4) | (a[1] as u32)
}
