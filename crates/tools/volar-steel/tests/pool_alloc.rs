//! A phase-shaped chunked pool allocator for the whole-program probes.
//!
//! The revm pipeline's peak RSS was dominated by allocator behavior, not
//! live data: millions of small/medium allocations (VAFFLE values, per-block
//! statement vectors, movfuscator `Poly` monomial heaps) are served by the
//! system malloc, and their frees scatter into size-bin free lists that
//! macOS only returns under pressure (`malloc_zone_pressure_relief`).
//! Stage transitions therefore leave GBs of dead pages resident, and each
//! later stage's own allocations stack on top of that high-water mark.
//!
//! This allocator fixes the shape instead of chasing the symptoms:
//!
//! - **Chunks**: allocations under [`DIRECT_THRESHOLD`] are bump-allocated
//!   inside 64 MiB chunks obtained from the system allocator (which serves
//!   them as fresh zeroed mmap regions on both macOS and Linux).
//! - **Wholesale death**: each chunk tracks its outstanding allocation
//!   count; the moment the last one is freed, the whole 64 MiB region is
//!   returned to the OS with a single `dealloc`. Phase-shaped workloads
//!   (import -> lower -> optimize -> movfuscate) thus reclaim entire
//!   chunks exactly when a phase's data dies — the import module's ~2-3GB
//!   collapses at the lowering handoff, the movfuscator's drained input
//!   blocks die chunk-by-chunk during the walk, and the ~50M `Poly`
//!   monomial heaps die in one sweep when the output drops — instead of
//!   millions of native frees scattering across memory.
//! - **Phase gating**: reuse-heavy churn phases (ir-opt's per-block canon
//!   maps) interleave with the module's persistent data and pin chunks;
//!   routing their allocations to the system allocator during those phases
//!   (`set_pooling(false)`) keeps the pool for the phase-shaped data
//!   (import/lowering, movfuscation) where wholesale reclamation wins.
//!   Pooling is always re-enabled for the drain/output phases.
//! - **Dead chunks are recycled, not unmapped**: a fully-freed chunk is
//!   unlinked, `madvise(MADV_DONTNEED)`d (physical pages released; they
//!   re-fault as zeros), and pushed onto a free-chunk stack. `new_chunk`
//!   pops that stack first, so freed capacity is REUSED and the process's
//!   footprint tracks peak concurrent usage instead of cumulative churn
//!   (measured: CSE alone grew the chunk set 80 -> 134 because freed
//!   strand slots never matched later allocation shapes; with recycling
//!   the freed regions serve whatever comes next).
//! - **Freelist reuse inside live chunks**: lowering interleaves per-function
//!   SSA transients (freed every function) with the module's persistent data
//!   in the same chunks. With a pure never-rewind bump, every freed transient
//!   permanently strands its bump space and the phase's peak balloons
//!   (measured: lowering peaked at ~10.5GB in 15s under malloc's ~4GB). So
//!   freed slots go onto per-chunk ceil-pow2 size-bucketed free lists and
//!   allocations reuse the first slot with capacity >= size (steady per-phase
//!   shapes hit exact matches immediately). Per-chunk freelists die with
//!   their chunk, so wholesale chunk death stays sound. Reused slots carry
//!   garbage, so `alloc_zeroed` goes through the trait's default (alloc +
//!   zero) — correctness over the mmap-zero shortcut.
//! - **Direct routing**: allocations of [`DIRECT_THRESHOLD`] or larger go
//!   straight to the system allocator (they are mmap-backed natively and
//!   freed eagerly), so no chunk is ever pinned by one huge allocation.
//!   Routing is by allocation size class, which `GlobalAlloc`'s contract
//!   guarantees is identical between alloc and dealloc/realloc, so a
//!   pointer never needs to be probed to learn its origin.
//!
//! Per-allocation overhead is one 16-byte header (chunk back-pointer plus
//! a debug magic), replacing the system allocator's own per-allocation
//! header + bin rounding.
//!
//! Thread safety: a spin lock guards all pool state. Tests may run
//! concurrently; correctness never depends on single-threadedness.
//!
//! The optional next step (not required for this pipeline) is an
//! `allocator_api`-style typed integration — `Allocator`-parameterized IR
//! containers (e.g. `PolyCoeffs` monomial arenas) — which would drop the
//! per-allocation header entirely for known-hot types; the GlobalAlloc
//! pool already captures the phase-reclamation win without touching any
//! IR type signatures.

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::alloc::System;

/// Allocations at or above this size bypass the pool and go directly to
/// the system allocator (mmap-backed, freed eagerly by the OS).
const DIRECT_THRESHOLD: usize = 1024 * 1024;

/// Pool chunk size.
const CHUNK_SIZE: usize = 64 * 1024 * 1024;

/// Per-allocation header layout (16 bytes): `cap: u32` (payload capacity,
/// the bump-rounded slot size), `magic: u32` (debug builds), `chunk: *mut
/// Chunk`. The data pointer returned to callers points `HEADER` bytes past
/// the header. Freelist scans read `cap` to enforce capacity >= request.
///
/// Phase gating: when pooling is off, a pool-eligible allocation is served
/// by the system allocator as `cap + HEADER` bytes (align max(16, align)),
/// with `chunk` set to [`SYSTEM_SENTINEL`]; `dealloc` reconstructs the
/// system layout from the stored `cap`. The header is what makes the two
/// routings distinguishable at free time.
const HEADER: usize = 16;

/// Debug-only magic stored in every pool header; a mismatch on free means
/// a size-class routing contract violation.

/// `chunk` value marking a system-routed (pooling-gated-off) allocation.
const SYSTEM_SENTINEL: usize = 1;
const HEADER_MAGIC: u32 = 0x1B00_C0DE; // sentinel: "b00-code" with an l-shaped 1

const N_BUCKETS: usize = 17; // ceil_log2 of sizes 16B..1MiB -> buckets 4..=20

/// Smallest payload size kept in a bucket (below this, round up to 16).
const MIN_BUCKET_SIZE: usize = 16;

/// ceil_log2 for bucket selection, clamped to [4, 20].
fn bucket_of(size: usize) -> usize {
    let s = size.max(MIN_BUCKET_SIZE);
    let lg = usize::BITS - (s - 1).leading_zeros(); // ceil(log2(s))
    (lg as usize).min(N_BUCKETS - 1 + 4).saturating_sub(4)
}

struct Chunk {
    next: *mut Chunk,
    /// Outstanding allocations in this chunk.
    outstanding: usize,
    /// Bump offset (bytes from the chunk base) of the next free byte.
    fill: usize,
    /// Per-bucket free lists (null = empty). Each node is a freed slot;
    /// the slot's first 8 bytes hold the next-node pointer (payload area,
    /// so no extra space). Nodes live only in their own chunk's lists, so
    /// a chunk's whole-region free is dangling-free by construction.
    free_heads: [*mut u8; N_BUCKETS],
}

pub struct Pool {
    /// Spin lock. `true` = locked.
    locked: AtomicBool,
    /// Singly-linked list of live chunks.
    head: UnsafeCell<*mut Chunk>,
    /// Stack of fully-freed chunks awaiting reuse (VA retained; pages
    /// madvise'd away). Recycled by `new_chunk` before hitting System.
    free_chunks: UnsafeCell<*mut Chunk>,
    /// When false, pool-eligible allocations route to the system
    /// allocator (phase gating for reuse-heavy churn phases). Dealloc
    /// stays pool-routed by size class; pointers allocated while pooling
    /// was off have NO pool header — see the routing note in `dealloc`.
    pooling: AtomicBool,
    pub chunks_recycled: AtomicUsize,
    /// Live diagnostics for the probes.
    pub chunks_live: AtomicUsize,
    pub bytes_live: AtomicUsize,
    /// Total bytes ever allocated through the pool (diagnostic).
    pub bytes_allocated: AtomicUsize,
}

impl Pool {
    pub const fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
            head: UnsafeCell::new(ptr::null_mut()),
            free_chunks: UnsafeCell::new(ptr::null_mut()),
            pooling: AtomicBool::new(true),
            chunks_recycled: AtomicUsize::new(0),
            chunks_live: AtomicUsize::new(0),
            bytes_live: AtomicUsize::new(0),
            bytes_allocated: AtomicUsize::new(0),
        }
    }

    /// Gate pool service for pool-eligible allocations. Must not be
    /// toggled while phase data is live unless the caller accepts that
    /// phase allocations bypass the pool (they are freed eagerly by the
    /// system allocator's own routing — see `dealloc`'s note).
    pub fn set_pooling(&self, on: bool) {
        self.pooling.store(on, Ordering::Relaxed);
    }

    fn lock(&self) {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
    }

    fn unlock(&self) {
        self.locked.store(false, Ordering::Release);
    }

    unsafe fn new_chunk(&self) -> *mut Chunk {
        // Recycle a fully-freed chunk first: the VA is retained, the
        // physical pages were madvise'd away at death and re-fault as
        // zeros. No new system allocation, no RSS growth.
        let recycled = *self.free_chunks.get();
        if !recycled.is_null() {
            *self.free_chunks.get() = (*recycled).next;
            ptr::write(
                recycled,
                Chunk {
                    next: *self.head.get(),
                    outstanding: 0,
                    fill: core::mem::size_of::<Chunk>(),
                    free_heads: [ptr::null_mut(); N_BUCKETS],
                },
            );
            *self.head.get() = recycled;
            self.chunks_recycled.fetch_add(1, Ordering::Relaxed);
            self.chunks_live.fetch_add(1, Ordering::Relaxed);
            return recycled;
        }
        // Page-aligned so the whole-region madvise on death is exact.
        let layout = Layout::from_size_align(CHUNK_SIZE, 16384).expect("chunk layout");
        let base = System.alloc(layout);
        if base.is_null() {
            return ptr::null_mut();
        }
        let chunk = base as *mut Chunk;
        ptr::write(
            chunk,
            Chunk {
                next: *self.head.get(),
                outstanding: 0,
                fill: core::mem::size_of::<Chunk>(),
                free_heads: [ptr::null_mut(); N_BUCKETS],
            },
        );
        *self.head.get() = chunk;
        self.chunks_live.fetch_add(1, Ordering::Relaxed);
        chunk
    }

    /// Walk the chunk list and assert structural sanity: no duplicate
    /// chunk addresses, every chunk non-degenerate. Test aid.
    pub fn check_invariants(&self) {
        self.lock();
        unsafe {
            let mut link = self.head.get();
            // Fixed-capacity: NO allocation may happen under the lock (any
            // nested Pool::alloc would deadlock on this very spin lock).
            let mut seen = [0usize; 4096];
            let mut n = 0usize;
            while !(*link).is_null() {
                let c = *link as usize;
                assert!(n < seen.len(), "chunk list longer than the checker bound");
                assert!(
                    !seen[..n].contains(&c),
                    "duplicate chunk {c:#x} in live list"
                );
                seen[n] = c;
                n += 1;
                assert!(
                    (*(*link)).outstanding > 0,
                    "live chunk {c:#x} has outstanding=0"
                );
                link = &mut (*(*link)).next;
            }
        }
        self.unlock();
    }

    unsafe fn drop_chunk(&self, chunk: *mut Chunk) {
        // Unlink (the list is short: tens of chunks at whole-program scale).
        let mut link = self.head.get();
        while !(*link).is_null() {
            if *link == chunk {
                *link = (*chunk).next;
                break;
            }
            link = &mut (*(*link)).next;
        }
        // Release the physical pages (re-fault as zeros) and keep the VA
        // on the recycle stack. Region is page-aligned by construction.
        const MADV_DONTNEED: i32 = 4;
        unsafe extern "C" {
            fn madvise(addr: *mut core::ffi::c_void, length: usize, advice: i32) -> i32;
        }
        let _ = madvise(chunk as *mut core::ffi::c_void, CHUNK_SIZE, MADV_DONTNEED);
        (*chunk).next = *self.free_chunks.get();
        *self.free_chunks.get() = chunk;
        self.chunks_live.fetch_sub(1, Ordering::Relaxed);
    }
}

// The spin lock guards every mutable field (chunk list, per-chunk
// counters), so `&Pool` shared across threads is sound.
unsafe impl Sync for Pool {}

unsafe impl GlobalAlloc for Pool {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.size() >= DIRECT_THRESHOLD {
            return System.alloc(layout);
        }
        let size = layout.size();
        if size == 0 {
            return System.alloc(layout);
        }
        let align = layout.align().max(16);
        if !self.pooling.load(Ordering::Relaxed) {
            // System-routed with a tagged header (see the header doc).
            let sys = Layout::from_size_align(size + HEADER, align).expect("gated layout");
            let base = System.alloc(sys);
            if base.is_null() {
                return ptr::null_mut();
            }
            let b = base as usize;
            ptr::write(b as *mut u32, size as u32);
            #[cfg(debug_assertions)]
            ptr::write((b + 4) as *mut u32, HEADER_MAGIC);
            ptr::write((b + 8) as *mut usize, SYSTEM_SENTINEL);
            self.bytes_allocated.fetch_add(size, Ordering::Relaxed);
            return (b + HEADER) as *mut u8;
        }
        self.lock();
        let mut chunk = *self.head.get();
        loop {
            if chunk.is_null() {
                chunk = self.new_chunk();
                if chunk.is_null() {
                    self.unlock();
                    return ptr::null_mut();
                }
            }
            // Freelist first: a bucket slot with capacity >= size (the
            // payload fits because bucket capacity is a strict pow2 ceiling
            // and align <= 16 <= MIN_BUCKET_SIZE).
            let b = bucket_of(size);
            if align <= MIN_BUCKET_SIZE && !(*chunk).free_heads[b].is_null() {
                // Bounded scan: take the first node whose stored capacity
                // >= the request (bucket membership alone does not imply
                // it — a slot with cap just over 2^(b-1) shares bucket b
                // with a request of size 2^b). Nodes that don't fit stay
                // on the list; falling through to the bump is safe.
                // `node` is a PAYLOAD address (== the returned data
                // pointer); its header lives at node-HEADER: cap at +0,
                // magic at +4 (debug), chunk at +8. The freelist next
                // pointer lives in the payload's first 8 bytes.
                let mut prev: *mut *mut u8 = &mut (*chunk).free_heads[b];
                let mut node = (*chunk).free_heads[b];
                let mut hops = 0usize;
                while !node.is_null() && hops < 32 {
                    let cap =
                        ptr::read((node as usize - HEADER) as *const u32) as usize;
                    if cap >= size {
                        // Unlink: prev's next (head field or node payload).
                        let next = ptr::read(node as *const *mut u8);
                        ptr::write(prev, next);
                        let hdr = node as usize - HEADER;
                        ptr::write((hdr + 8) as *mut *mut Chunk, chunk);
                        (*chunk).outstanding += 1;
                        self.bytes_live.fetch_add(size, Ordering::Relaxed);
                        self.bytes_allocated.fetch_add(size, Ordering::Relaxed);
                        self.unlock();
                        return node;
                    }
                    prev = node as *mut *mut u8;
                    node = ptr::read(node as *const *mut u8);
                    hops += 1;
                }
            }
            // Data goes at `data`, header 16 bytes before it, so that both
            // the header (needs 8 for the pointer) and the data (needs
            // `align`) are satisfied.
            let base = chunk as usize;
            let data = (base + (*chunk).fill + HEADER + (align - 1)) & !(align - 1);
            let hdr = data - HEADER;
            if hdr + HEADER + size <= base + CHUNK_SIZE {
                ptr::write(hdr as *mut u32, size as u32);
                #[cfg(debug_assertions)]
                ptr::write((hdr + 4) as *mut u32, HEADER_MAGIC);
                ptr::write((hdr + 8) as *mut *mut Chunk, chunk);
                (*chunk).fill = hdr + HEADER + size - base;
                (*chunk).outstanding += 1;
                self.bytes_live.fetch_add(size, Ordering::Relaxed);
                self.bytes_allocated.fetch_add(size, Ordering::Relaxed);
                self.unlock();
                return data as *mut u8;
            }
            // Full: try the next chunk, else a fresh one. If even a fresh
            // chunk cannot hold it (size close to CHUNK_SIZE), fall back to
            // the system allocator for this allocation.
            let next = (*chunk).next;
            if next.is_null() {
                if HEADER + size + align > CHUNK_SIZE - core::mem::size_of::<Chunk>() {
                    self.unlock();
                    return System.alloc(layout);
                }
                let fresh = self.new_chunk();
                if fresh.is_null() {
                    self.unlock();
                    return ptr::null_mut();
                }
                chunk = fresh;
            } else {
                chunk = next;
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if layout.size() >= DIRECT_THRESHOLD || layout.size() == 0 {
            System.dealloc(ptr, layout);
            return;
        }
        self.lock();
        let hdr = ptr as usize - HEADER;
        #[cfg(debug_assertions)]
        {
            // NEVER panic under the lock: the panic machinery allocates,
            // and the re-entrant alloc would self-deadlock on this very
            // spin lock (that hang once masked the real bug here).
            let magic = ptr::read((hdr + 4) as *const u32);
            if magic != HEADER_MAGIC {
                self.unlock();
                panic!(
                    "pool free of a non-pool/corrupt pointer {ptr:?} (magic {magic:#x}); pool state: chunks={} live={}MB",
                    self.chunks_live.load(Ordering::Relaxed),
                    self.bytes_live.load(Ordering::Relaxed) / (1024 * 1024),
                );
            }
        }
        let chunk_tag = ptr::read((hdr + 8) as *const usize);
        if chunk_tag == SYSTEM_SENTINEL {
            // System-routed (pooling was off at alloc): free via System
            // with the reconstructed layout.
            let cap = ptr::read(hdr as *const u32) as usize;
            let sys = Layout::from_size_align(cap + HEADER, layout.align().max(16))
                .expect("gated free");
            self.unlock();
            System.dealloc(hdr as *mut u8, sys);
            return;
        }
        let chunk = chunk_tag as *mut Chunk;
        (*chunk).outstanding -= 1;
        self.bytes_live.fetch_sub(layout.size(), Ordering::Relaxed);
        if (*chunk).outstanding == 0 {
            self.drop_chunk(chunk);
        } else {
            // Return the slot to its chunk's bucket list (the next-pointer
            // lives in the payload area, which the caller just gave up).
            // Bucket by the STORED capacity: realloc'd-down slots keep
            // their bigger bucket, so capacity >= request always holds for
            // exact-shape reuse and the scan only skips genuine misfits.
            let cap = ptr::read(hdr as *const u32) as usize;
            let b = bucket_of(cap);
            // The freelist node is the PAYLOAD address; the next-pointer
            // goes into the payload's first 8 bytes (the caller just gave
            // them up), never into the header.
            let slot = (hdr + HEADER) as *mut u8;
            ptr::write(slot as *mut *mut u8, (*chunk).free_heads[b]);
            (*chunk).free_heads[b] = slot;
        }
        self.unlock();
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // Always allocate-copy-free: pool memory never extends in place,
        // and a size-class change (pool -> direct system) is handled by the
        // same path. The copy preserves contents across the move.
        let new_layout = Layout::from_size_align(new_size, layout.align()).expect("realloc layout");
        let new_ptr = self.alloc(new_layout);
        if new_ptr.is_null() {
            return ptr::null_mut();
        }
        let copy = layout.size().min(new_size);
        if copy > 0 {
            ptr::copy_nonoverlapping(ptr, new_ptr, copy);
        }
        self.dealloc(ptr, layout);
        new_ptr
    }
}

// ============================================================================
// Stress / correctness tests (run as part of the compile_probe test set).
// ============================================================================

fn drops<T>(v: &mut Vec<T>) {
    let mut taken = std::mem::take(v);
    drop(taken);
}

#[test]
fn pool_vec_grow_and_content() {
    let mut v: Vec<u64> = Vec::new();
    for i in 0..10_000u64 {
        v.push(i.wrapping_mul(0x9E37_79B9_7F4A_7C15));
    }
    for (i, x) in v.iter().enumerate() {
        assert_eq!(*x, (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15));
    }
    v.shrink_to_fit();
    assert_eq!(v[1234], 1234u64.wrapping_mul(0x9E37_79B9_7F4A_7C15));
}

#[test]
fn pool_random_alloc_free_stress() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    // Seeded LCG so failures reproduce.
    let mut state: u64 = 0x1234_5678_9ABC_DEF0;
    let mut next = move || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        state >> 33
    };
    struct Slot {
        ptr: *mut u8,
        layout: Layout,
        fill: u8,
    }
    let mut slots: Vec<Slot> = Vec::new();
    for round in 0..200_000 {
        let pick = next() as usize;
        if slots.len() < 256 && pick % 3 != 0 {
            // Allocate: sizes span tiny -> near-chunk, alignments up to 256.
            let size = 1 + (next() as usize) % (64 * 1024);
            let align = 1usize << (next() % 9); // 1..=256
            let layout = Layout::from_size_align(size, align).expect("layout");
            let ptr = unsafe { std::alloc::alloc(layout) };
            assert!(!ptr.is_null(), "alloc failed size={size} align={align}");
            unsafe { ptr::write_bytes(ptr, (round % 251) as u8 + 1, size) };
            slots.push(Slot {
                ptr,
                layout,
                fill: (round % 251) as u8 + 1,
            });
        } else if !slots.is_empty() {
            let idx = pick % slots.len();
            let slot = slots.swap_remove(idx);
            // Verify the fill byte survived (no aliasing corruption).
            let ok = (0..slot.layout.size())
                .step_by(97)
                .all(|i| unsafe { *slot.ptr.add(i) == slot.fill });
            assert!(ok, "corruption at round {round}");
            unsafe { std::alloc::dealloc(slot.ptr, slot.layout) };
        }
        // Occasional realloc.
        if pick % 7 == 0 && !slots.is_empty() {
            let idx = pick % slots.len();
            let new_size = 1 + (next() as usize) % (128 * 1024);
            let slot = &mut slots[idx];
            let ptr = unsafe { std::alloc::realloc(slot.ptr, slot.layout, new_size) };
            assert!(!ptr.is_null(), "realloc failed");
            let keep = slot.layout.size().min(new_size);
            let ok = (0..keep)
                .step_by(101)
                .all(|i| unsafe { *ptr.add(i) == slot.fill });
            assert!(ok, "realloc corruption at round {round}");
            // The grown tail [keep..new_size) is uninitialized by realloc's
            // contract -- define it so later whole-buffer checks are sound.
            unsafe { ptr::write_bytes(ptr.add(keep), slot.fill, new_size - keep) };
            slot.ptr = ptr;
            slot.layout = Layout::from_size_align(new_size, slot.layout.align()).expect("layout");
        }
        let _ = DefaultHasher::new(); // keep the import used without noise
    }
    // Reclaim check: a scoped burst of chunk-sized traffic must leave the
    // pool exactly as it found it (same live bytes, same chunk count) --
    // every burst allocation is freed, so every burst chunk must die.
    // (A process-global chunks==0 assert would be wrong here: the test
    // harness's own long-lived allocations pin chunks.) The strict delta
    // asserts need an exclusive harness (--test-threads=1 with
    // POOL_STRICT_STRESS=1): under the default parallel harness other
    // tests' allocations interleave with the burst, so the deltas drift
    // by whatever those tests hold live. The content checks above are
    // concurrency-safe and always run.
    let strict = std::env::var_os("POOL_STRICT_STRESS").is_some();
    let live_before = crate::POOL
        .bytes_live
        .load(std::sync::atomic::Ordering::Relaxed);
    let chunks_before = crate::POOL
        .chunks_live
        .load(std::sync::atomic::Ordering::Relaxed);
    let mut bursts: Vec<Vec<u8>> = Vec::new();
    for i in 0..16 {
        let mut v = vec![7u8; 8 * 1024 * 1024 + i * 4096];
        v[i * 977] = (i as u8).wrapping_add(3);
        bursts.push(v);
    }
    for (i, v) in bursts.iter_mut().enumerate() {
        assert_eq!(v[i * 977], (i as u8).wrapping_add(3));
    }
    drops(&mut bursts);
    if strict {
        assert_eq!(
            crate::POOL
                .bytes_live
                .load(std::sync::atomic::Ordering::Relaxed),
            live_before,
            "burst bytes must reclaim exactly"
        );
        assert_eq!(
            crate::POOL
                .chunks_live
                .load(std::sync::atomic::Ordering::Relaxed),
            chunks_before,
            "burst chunks must die wholesale"
        );
    }
    for slot in slots {
        unsafe { std::alloc::dealloc(slot.ptr, slot.layout) };
    }
}

#[test]
fn pool_alignment_is_honored() {
    for align in [1usize, 2, 4, 8, 16, 32, 64, 128, 256] {
        for size in [1usize, 3, 7, 16, 100, 1000, 4096] {
            let layout = Layout::from_size_align(size, align).expect("layout");
            let ptr = unsafe { std::alloc::alloc(layout) };
            assert!(!ptr.is_null());
            assert_eq!(ptr as usize % align, 0, "align {align} size {size}");
            unsafe { std::alloc::dealloc(ptr, layout) };
        }
    }
}

#[test]
fn pool_repro_trace() {
    let mut state: u64 = 0x1234_5678_9ABC_DEF0;
    let mut next = move || {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        state >> 33
    };
    let mut slots: Vec<(usize, Layout, u8)> = Vec::new(); // (raw ptr, layout, fill)
    for round in 0..12usize {
        let pick = next() as usize;
        let op;
        if slots.len() < 256 && pick % 3 != 0 {
            let size = 1 + (next() as usize) % (64 * 1024);
            let align = 1usize << (next() % 9);
            let layout = Layout::from_size_align(size, align).expect("layout");
            let ptr = unsafe { std::alloc::alloc(layout) } as usize;
            assert!(ptr != 0);
            unsafe { std::ptr::write_bytes(ptr as *mut u8, (round % 251) as u8 + 1, size) };
            op = format!("alloc size={size} align={align} ptr={ptr:#x}");
            slots.push((ptr, layout, (round % 251) as u8 + 1));
        } else if !slots.is_empty() {
            let idx = pick % slots.len();
            let (ptr, layout, fill) = slots.swap_remove(idx);
            let ok = (0..layout.size())
                .step_by(97)
                .all(|i| unsafe { *((ptr + i) as *const u8) == fill });
            op = format!(
                "free idx={idx} size={} align={} ptr={ptr:#x} ok={ok}",
                layout.size(),
                layout.align()
            );
            assert!(ok, "corruption: {op}");
            unsafe { std::alloc::dealloc(ptr as *mut u8, layout) };
        } else {
            op = "skip".into();
        }
        // Realloc branch (same condition as the stress test).
        if pick % 7 == 0 && !slots.is_empty() {
            let idx = pick % slots.len();
            let new_size = 1 + (next() as usize) % (128 * 1024);
            let (old_ptr, old_layout, fill) = slots[idx];
            let new_ptr =
                unsafe { std::alloc::realloc(old_ptr as *mut u8, old_layout, new_size) } as usize;
            let keep = old_layout.size().min(new_size);
            let ok = (0..keep)
                .step_by(101)
                .all(|i| unsafe { *((new_ptr + i) as *const u8) == fill });
            assert!(ok, "realloc corruption round {round}: old={old_ptr:#x} new={new_ptr:#x} old_size={} new={new_size}", old_layout.size());
            // Define the grown tail so whole-buffer checks stay sound.
            unsafe { std::ptr::write_bytes((new_ptr + keep) as *mut u8, fill, new_size - keep) };
            slots[idx] = (
                new_ptr,
                Layout::from_size_align(new_size, old_layout.align()).expect("l"),
                fill,
            );
            eprintln!("round {round}: realloc idx={idx} old={old_ptr:#x} new={new_ptr:#x} oldsz={} newsz={new_size}", old_layout.size());
        }
        // Per-op verification of every live buffer's fill bytes: pinpoints
        // the exact op that corrupts.
        for (k, (p, l, f)) in slots.iter().enumerate() {
            let ok = (0..l.size())
                .step_by(97)
                .all(|i| unsafe { *((p + i) as *const u8) == *f });
            if !ok {
                // Read the pool header: cap at ptr-16, magic at ptr-12,
                // chunk ptr at ptr-8.
                let cap = unsafe { std::ptr::read((p - 16) as *const u32) };
                let magic = unsafe { std::ptr::read((p - 12) as *const u32) };
                let chunk = unsafe { std::ptr::read((p - 8) as *const usize) };
                let first_bad = (0..l.size()).find(|&i| unsafe { *((p + i) as *const u8) != *f });
                panic!(
                    "fill corrupt after round {round} slot {k} ptr={p:#x} size={} fill={f} cap={cap} magic={magic:#x} chunk={chunk:#x} first_bad={first_bad:?}",
                    l.size()
                );
            }
        }
        // Overlap invariant: no two live buffers may share a byte.
        for i in 0..slots.len() {
            for j in (i + 1)..slots.len() {
                let (pi, li, _) = slots[i];
                let (pj, lj, _) = slots[j];
                let (a, asz) = (pi.min(pj), if pi <= pj { li.size() } else { lj.size() });
                let (b, _) = if pi <= pj {
                    (pj, lj.size())
                } else {
                    (pi, li.size())
                };
                assert!(
                    a + asz <= b,
                    "OVERLAP round {round}: [{a:#x}..{:#x}] vs [{b:#x}..{:#x}]\n  i={i}: {pi:#x} sz={}\n  j={j}: {pj:#x} sz={}",
                    a + asz,
                    b + lj.size(),
                    li.size(),
                    lj.size(),
                );
            }
        }
        eprintln!("round {round}: pick={pick:#x} {op}");
        crate::POOL.check_invariants();
    }
}

#[test]
fn pool_same_size_reuse_hits_freelist() {
    // A freed slot must serve a same-size request (same payload address
    // if it is the head of its bucket list).
    let a = unsafe { std::alloc::alloc(Layout::from_size_align(16000, 16).unwrap()) };
    let b = unsafe { std::alloc::alloc(Layout::from_size_align(16000, 16).unwrap()) };
    unsafe { std::alloc::dealloc(a, Layout::from_size_align(16000, 16).unwrap()) };
    let c = unsafe { std::alloc::alloc(Layout::from_size_align(16000, 16).unwrap()) };
    eprintln!("a={a:?} b={b:?} c={c:?}");
    assert_eq!(c, a, "same-size realloc must reuse the freed slot");
    unsafe { std::alloc::dealloc(b, Layout::from_size_align(16000, 16).unwrap()) };
    unsafe { std::alloc::dealloc(c, Layout::from_size_align(16000, 16).unwrap()) };
}
