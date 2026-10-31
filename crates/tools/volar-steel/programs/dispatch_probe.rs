// Probe for the shared Table-on-ID dispatch: a pointer-typed parameter has
// unresolved provenance, so stores/loads through it (and through a GEP off
// it) exercise the dispatch read/write paths. The escaping alloca carves a
// second pool storage, keeping the candidate set multi-element and forcing
// the Table path (single candidates take the direct-access shortcut); the
// derived pointer reuses the selection via check-once dispatch (GEPs
// preserve ID+TAG class bits).
#[no_mangle]
pub static mut BUF: [u32; 8] = [0u32; 8];
#[no_mangle]
pub static mut GPTR: *mut u32 = core::ptr::null_mut();

#[no_mangle]
pub extern "C" fn dispatch_probe(p: *mut u32, v: u32, w: u32) -> u32 {
    unsafe {
        let mut local = 0u32;
        GPTR = &mut local; // escaping alloca: carves a fresh pool storage
        *p = v;
        let q = p.offset(1);
        *q = w;
        (*p).wrapping_add((*q) << 1)
    }
}
