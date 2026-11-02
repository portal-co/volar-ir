//! A phase-shaped chunked pool allocator for whole-program IR pipelines.
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
//! - **Page-granular sub-chunk release** (the double-buffering core): each
//!   chunk carries per-page allocated/freed byte counters. When every
//!   allocated byte of a 4 KiB page's worth of slots has been freed, the
//!   page is marked dead; dead pages are `madvise(MADV_DONTNEED)`d in
//!   batched contiguous runs (64+ dead pages triggers a sweep), releasing
//!   their physical pages immediately. A producer/consumer pair — e.g. the
//!   movfuscator draining its input blocks while the output block grows —
//!   therefore runs at **peak-concurrent RSS**, not cumulative: the
//!   drained input's pages return to the OS mid-walk while the output
//!   re-faults fresh pages.
//! - **Dual free modes** ([`Pool::set_free_mode`]): reuse-heavy phases
//!   (the lowering's per-function SSA transients) run in *Reuse* mode —
//!   freed slots go onto per-chunk ceil-pow2 size-bucketed free lists
//!   (node = payload address, next-pointer in the payload's first 8
//!   bytes) and are served to the next same-shape request, bounding the
//!   footprint by live+one-transient. The movfuscator's drain runs in
//!   *Release* mode — freed slots are FORGOTTEN, their pages go fully
//!   dead and are madvise'd, so RSS drops mid-walk while the output
//!   grows (the double-buffering). A page is releasable only when it
//!   holds no freelist nodes (tracked per page), so reuse never fights
//!   the release.
//! - **Dead chunks are recycled, not unmapped**: a fully-freed chunk is
//!   unlinked, `madvise(MADV_DONTNEED)`d (whole region), and pushed onto
//!   a free-chunk stack. `new_chunk` pops that stack first, so the VA
//!   space is retained and the footprint tracks peak concurrent usage.
//! - **Direct routing**: allocations of [`DIRECT_THRESHOLD`] or larger go
//!   straight to the system allocator (they are mmap-backed natively and
//!   freed eagerly), so no chunk is ever pinned by one huge allocation.
//!   Routing is by allocation size class, which `GlobalAlloc`'s contract
//!   guarantees is identical between alloc and dealloc/realloc, so a
//!   pointer never needs to be probed to learn its origin.
//! - **Phase gating**: when pooling is off, a pool-eligible allocation is
//!   served by the system allocator as `size + HEADER` bytes with a
//!   sentinel-tagged header, so `dealloc` can reconstruct the system
//!   layout. This lets a driver route reuse-heavy churn phases (DCE/CSE
//!   canon maps) to malloc — where their interleaved free/realloc pattern
//!   is served by bins instead of stranding chunk capacity — while the
//!   pool keeps the phase-shaped data (lowering output, movfuscator).
//!
//! Per-allocation overhead is one 16-byte header (capacity + debug magic +
//! chunk back-pointer), replacing the system allocator's own per-allocation
//! header + bin rounding. Page metadata is 4 bytes/page (u16 allocated +
//! u16 freed), i.e. 0.4% of a chunk.
//!
//! Thread safety: a spin lock guards all pool state. Tests may run
//! concurrently; correctness never depends on single-threadedness.
//!
//! The optional next step (not required for this pipeline) is an
//! `allocator_api`-style typed integration — `Allocator`-parameterized IR
//! containers — which would drop the per-allocation header entirely for
//! known-hot types; the GlobalAlloc pool already captures the
//! phase-reclamation win without touching any IR type signatures.

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::mem::size_of;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::alloc::System;

/// Allocations at or above this size bypass the pool and go directly to
/// the system allocator (mmap-backed, freed eagerly by the OS).
const DIRECT_THRESHOLD: usize = 1024 * 1024;

/// Pool chunk size.
const CHUNK_SIZE: usize = 64 * 1024 * 1024;

/// Per-allocation header: capacity (u32) + debug magic (u32) + chunk
/// back-pointer. The data pointer returned to callers points `HEADER`
/// bytes past the header, so the header is part of the slot's span and is
/// covered by the page accounting.
const HEADER: usize = 16;

/// Debug-only magic stored in every pool header; a mismatch on free means
/// a size-class routing contract violation.
const HEADER_MAGIC: u32 = 0x1B00_C0DE;

/// Freelist buckets: ceil_log2 of payload sizes 16B..1MiB -> buckets 4..=20.
const N_BUCKETS: usize = 17;
/// Smallest payload size kept in a bucket (below this, round up to 16).
const MIN_BUCKET_SIZE: usize = 16;

/// ceil_log2 bucket index for `size`, clamped to [0, N_BUCKETS-1].
fn bucket_of(size: usize) -> usize {
    let s = size.max(MIN_BUCKET_SIZE);
    let lg = usize::BITS - (s - 1).leading_zeros();
    (lg as usize).min(N_BUCKETS - 1 + 4).saturating_sub(4)
}

/// `chunk` value marking a system-routed (pooling-gated-off) allocation.
const SYSTEM_SENTINEL: usize = 1;

/// `madvise` advice: drop the pages; they re-fault as zeros. Same value
/// (4) on macOS and Linux.
const MADV_DONTNEED: i32 = 4;

unsafe extern "C" {
    fn madvise(addr: *mut core::ffi::c_void, length: usize, advice: i32) -> i32;
    fn getpagesize() -> i32;
}

/// Runtime page size, cached (4 KiB on x86_64, 16 KiB on Apple Silicon).
static PAGE_SIZE: AtomicUsize = AtomicUsize::new(0);

/// Test-side trace switch (set via [`Pool::set_trace`]).
static TRACE: AtomicBool = AtomicBool::new(false);

fn page_size() -> usize {
    let cached = PAGE_SIZE.load(Ordering::Relaxed);
    if cached != 0 {
        return cached;
    }
    // SAFETY: getpagesize is thread-safe and infallible in practice.
    let ps = unsafe { getpagesize() } as usize;
    let ps = if ps >= 4096 && ps.is_power_of_two() { ps } else { 4096 };
    PAGE_SIZE.store(ps, Ordering::Relaxed);
    ps
}

/// Begin a contiguous page-address range for `madvise`.
#[inline]
fn madvise_pages(base: usize, start_page: usize, len_pages: usize) {
    if len_pages == 0 {
        return;
    }
    // SAFETY: the range lies inside a page-aligned chunk obtained from
    // the system allocator and is currently mapped (madvise on untouched
    // pages of a mapped range is valid; it simply has no pages to drop).
    unsafe {
        let _ = madvise(
            (base + start_page * page_size()) as *mut core::ffi::c_void,
            len_pages * page_size(),
            MADV_DONTNEED,
        );
    }
}

struct Chunk {
    next: *mut Chunk,
    /// Outstanding allocations in this chunk.
    outstanding: usize,
    /// Bump offset (bytes from the chunk base) of the next free byte.
    fill: usize,
    /// The chunk's page-aligned base address (madvise ranges need it).
    fill_base: usize,
    /// Number of pages (`CHUNK_SIZE / page_size`).
    n_pages: usize,
    /// Per-page allocated/freed byte counters (u16: page spans fit).
    page_alloc: *mut u16,
    page_freed: *mut u16,
    /// Per-page freelist-node counts: a page holding free-list nodes is
    /// NOT releasable (their next-pointers live in the payloads).
    page_nodes: *mut u16,
    /// Per-chunk size-bucketed free lists (Reuse mode). null = empty.
    free_heads: [*mut u8; N_BUCKETS],
    /// Dead-page bitset (one bit per page) + count of set bits.
    dead_words: *mut u64,
    dead_count: usize,
}

impl Chunk {
    /// `#[inline]`-small helpers for the page accounting. `span` is the
    /// slot's full byte extent (header + payload).
        /// Account a freshly-allocated slot's byte span into its pages.
    unsafe fn on_alloc_pages(&mut self, base: usize, start: usize, end: usize) { unsafe {
        let ps = page_size();
        let first = (start - base) / ps;
        let last = (end - 1 - base) / ps;
        for p in first..=last {
            let lo = (start - base).max(p * ps);
            let hi = (end - base).min((p + 1) * ps);
            let old = *self.page_alloc.add(p) as usize;
            *self.page_alloc.add(p) = (old + (hi - lo)) as u16;
        }
    }}

    unsafe fn on_free_pages(&mut self, base: usize, start: usize, end: usize) -> usize {
        unsafe {

        let ps = page_size();
        let first = (start - base) / ps;
        let last = (end - 1 - base) / ps;
        let mut newly_dead = 0usize;
        for p in first..=last {
            let lo = (start - base).max(p * ps);
            let hi = (end - base).min((p + 1) * ps);
            let alloced = *self.page_alloc.add(p) as usize;
            let mut freed = *self.page_freed.add(p) as usize + (hi - lo);
            // Clamp: over-accounting (if any) must not block the dead
            // transition — the counters are a release heuristic, not a
            // correctness invariant, and a panic here would deadlock the
            // allocator (see the module doc).
            if freed > alloced {
                freed = alloced;
            }
            *self.page_freed.add(p) = freed as u16;
            let nodes = *self.page_nodes.add(p) as usize;
            if freed == alloced && alloced > 0 && nodes == 0 {
                let word = self.dead_words.add(p / 64);
                let bit = 1u64 << (p % 64);
                if (*word & bit) == 0 {
                    *word |= bit;
                    self.dead_count += 1;
                    newly_dead += 1;
                }
            }
        }
        newly_dead
        }
    }

    /// Release the currently-dead pages in batched contiguous runs.
    /// Returns the number of pages whose physical memory was dropped.
    unsafe fn sweep_dead_pages(&mut self, base: usize) -> usize { unsafe {
        if self.dead_count == 0 {
            return 0;
        }
        let mut released = 0usize;
        let mut run_start: Option<usize> = None;
        for p in 0..self.n_pages {
            let dead = ((*self.dead_words.add(p / 64) >> (p % 64)) & 1) == 1;
            if dead {
                if run_start.is_none() {
                    run_start = Some(p);
                }
            } else if let Some(s) = run_start.take() {
                madvise_pages(base, s, p - s);
                released += p - s;
            }
        }
        if let Some(s) = run_start.take() {
            madvise_pages(base, s, self.n_pages - s);
            released += self.n_pages - s;
        }
        // All dead bits consumed.
        ptr::write_bytes(self.dead_words, 0, self.n_pages.div_ceil(64) * 8 / 8);
        self.dead_count = 0;
        released
    }}
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
    /// was off carry a sentinel-tagged header — see the module doc.
    pooling: AtomicBool,
    /// true = frees push slots to the freelist for reuse (churn phases);
    /// false = frees forget slots so their pages can be released
    /// (drain phases). See [`Pool::set_free_mode`].
    free_mode_reuse: AtomicBool,
    /// Live diagnostics for the probes.
    pub chunks_live: AtomicUsize,
    pub bytes_live: AtomicUsize,
    /// Total bytes ever allocated through the pool (diagnostic).
    pub bytes_allocated: AtomicUsize,
    /// Chunks reused from the recycle stack (diagnostic).
    pub chunks_recycled: AtomicUsize,
    /// Pages whose physical memory was released by sweeps (diagnostic).
    pub pages_released: AtomicUsize,
}

impl Pool {
    pub const fn new() -> Self {
        Self {
            locked: AtomicBool::new(false),
            head: UnsafeCell::new(ptr::null_mut()),
            free_chunks: UnsafeCell::new(ptr::null_mut()),
            pooling: AtomicBool::new(true),
            free_mode_reuse: AtomicBool::new(true),
            chunks_live: AtomicUsize::new(0),
            bytes_live: AtomicUsize::new(0),
            bytes_allocated: AtomicUsize::new(0),
            chunks_recycled: AtomicUsize::new(0),
            pages_released: AtomicUsize::new(0),
        }
    }

    /// Gate pool service for pool-eligible allocations. Must not be
    /// toggled while phase data is live unless the caller accepts that
    /// phase allocations bypass the pool (they are freed eagerly by the
    /// system allocator's own routing — see `dealloc`'s sentinel branch).
    pub fn set_pooling(&self, on: bool) {
        self.pooling.store(on, Ordering::Relaxed);
    }

    /// Set the free policy for pool-served deallocations. `true` (Reuse):
    /// freed slots join their chunk's bucket lists for same-shape reuse —
    /// for interleaved churn phases (the lowering's per-function SSA
    /// transients) this bounds the footprint by live + one transient.
    /// `false` (Release): freed slots are forgotten so their pages can go
    /// fully dead and be madvise'd — for producer/consumer phases (the
    /// movfuscator draining its input while the output grows) this makes
    /// RSS track peak-concurrent usage.
    pub fn set_free_mode(&self, reuse: bool) {
        self.free_mode_reuse.store(reuse, Ordering::Relaxed);
    }

    /// Enable allocator-internal op tracing (test aid; stderr).
    pub fn set_trace(&self, on: bool) {
        TRACE.store(on, Ordering::Relaxed);
    }

    #[inline]
    fn tracing() -> bool {
        TRACE.load(Ordering::Relaxed)
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

    unsafe fn new_chunk(&self) -> *mut Chunk { unsafe {
        let ps = page_size();
        let n_pages = CHUNK_SIZE / ps;
        // Recycle a fully-freed chunk first: the VA is retained, the
        // physical pages were madvise'd away at death and re-fault as
        // zeros. No new system allocation, no RSS growth.
        let recycled = *self.free_chunks.get();
        if !recycled.is_null() {
            *self.free_chunks.get() = (*recycled).next;
            let c = &mut *recycled;
            c.next = *self.head.get();
            c.outstanding = 0;
            c.fill = 0;
            // fill_base is already correct: the struct carries its own
            // data region through the recycle stack. (Overwriting it with
            // the struct's address here is what once madvise'd a 64MiB
            // window across unrelated heap mappings.)
            debug_assert_ne!(c.fill_base, 0);
            ptr::write_bytes(c.page_alloc, 0, n_pages);
            ptr::write_bytes(c.page_freed, 0, n_pages);
            ptr::write_bytes(c.page_nodes, 0, n_pages);
            ptr::write_bytes(c.dead_words, 0, n_pages.div_ceil(64));
            c.dead_count = 0;
            for h in &mut c.free_heads {
                *h = ptr::null_mut();
            }
            *self.head.get() = recycled;
            self.chunks_recycled.fetch_add(1, Ordering::Relaxed);
            self.chunks_live.fetch_add(1, Ordering::Relaxed);
            return recycled;
        }
        // Page-aligned so whole-region madvise on death is exact.
        let layout = Layout::from_size_align(CHUNK_SIZE, ps).expect("chunk layout");
        let base = System.alloc(layout);
        if base.is_null() {
            return ptr::null_mut();
        }
        // 8-byte aligned: the dead-words table is dereferenced as *mut u64
        // (a misaligned deref panics in debug builds — and its formatting
        // would re-enter this allocator under the lock).
        let meta_layout = Layout::from_size_align(
            n_pages * 3 * size_of::<u16>() + n_pages.div_ceil(64) * size_of::<u64>(),
            8,
        )
        .expect("page metadata layout");
        let meta = System.alloc(meta_layout);
        if meta.is_null() {
            System.dealloc(base, layout);
            return ptr::null_mut();
        }
        // Chunk metadata must NOT allocate through the global allocator
        // (that is this Pool — allocating here would self-deadlock on the
        // spin lock; the sample-verified hang). System.alloc directly.
        let chunk_layout = Layout::from_size_align(size_of::<Chunk>(), 16).expect("chunk meta");
        let chunk_mem = System.alloc(chunk_layout);
        if chunk_mem.is_null() {
            System.dealloc(base, layout);
            System.dealloc(meta, meta_layout);
            return ptr::null_mut();
        }
        let chunk = chunk_mem as *mut Chunk;
        ptr::write(
            chunk,
            Chunk {
                next: *self.head.get(),
                outstanding: 0,
                fill: 0,
                fill_base: base as usize,
                n_pages,
                page_alloc: meta as *mut u16,
                page_freed: meta.add(n_pages) as *mut u16,
                page_nodes: meta.add(n_pages * 2) as *mut u16,
                dead_words: meta.add(n_pages * 3) as *mut u64,
                dead_count: 0,
                free_heads: [ptr::null_mut(); N_BUCKETS],
            },
        );
        ptr::write_bytes((*chunk).page_alloc, 0, n_pages);
        ptr::write_bytes((*chunk).page_freed, 0, n_pages);
        ptr::write_bytes((*chunk).page_nodes, 0, n_pages);
        ptr::write_bytes((*chunk).dead_words, 0, n_pages.div_ceil(64));
        *self.head.get() = chunk;
        self.chunks_live.fetch_add(1, Ordering::Relaxed);
        chunk
    }}

    unsafe fn drop_chunk(&self, chunk: *mut Chunk) { unsafe {
        // Unlink (the list is short: tens of chunks at whole-program scale).
        let mut link = self.head.get();
        while !(*link).is_null() {
            if *link == chunk {
                *link = (*chunk).next;
                break;
            }
            link = &mut (*(*link)).next;
        }
        // Release the DATA region's physical pages (they re-fault as
        // zeros) and keep the struct + VA on the recycle stack. The base
        // is the recorded fill_base — madvise from the struct's own
        // address would sweep a 64MiB window across unrelated mappings.
        madvise_pages((*chunk).fill_base, 0, CHUNK_SIZE / page_size());
        (*chunk).next = *self.free_chunks.get();
        *self.free_chunks.get() = chunk;
        self.chunks_live.fetch_sub(1, Ordering::Relaxed);
    }}

    /// Sweep a chunk's dead pages if enough have accumulated (batched
    /// contiguous runs keep the syscall count low). Returns pages dropped.
    unsafe fn maybe_sweep(&self, chunk: *mut Chunk) -> usize { unsafe {
        if (*chunk).dead_count < 64 {
            return 0;
        }
        if Self::tracing() {
            eprintln!("    sweep chunk {chunk:?} dead_count={}", (*chunk).dead_count);
        }
        let released = (*chunk).sweep_dead_pages((*chunk).fill_base);
        if released > 0 {
            self.pages_released.fetch_add(released, Ordering::Relaxed);
        }
        released
    }}

}

impl Default for Pool {
    fn default() -> Self {
        Self::new()
    }
}

unsafe impl Sync for Pool {}

// SAFETY: all mutable pool state (chunk list, recycle stack, per-chunk
// counters and page tables) is guarded by the spin lock; `Sync` marks the
// shared `&Pool` sound across threads.
unsafe impl GlobalAlloc for Pool {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 { unsafe {
        if layout.size() >= DIRECT_THRESHOLD {
            return System.alloc(layout);
        }
        let size = layout.size();
        if size == 0 {
            return System.alloc(layout);
        }
        let align = layout.align().max(16);
        if !self.pooling.load(Ordering::Relaxed) {
            // System-routed with a tagged header (see the module doc).
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
        let _ps = page_size();
        if Self::tracing() {
            eprintln!(
                "  alloc size={size} align={align} chunks={}",
                self.chunks_live.load(Ordering::Relaxed)
            );
        }
        loop {
            if chunk.is_null() {
                chunk = self.new_chunk();
                if chunk.is_null() {
                    self.unlock();
                    return ptr::null_mut();
                }
            }
            // Freelist reuse (Reuse mode): the bucket list holds NODE
            // addresses (the slot headers); the payload is node + HEADER.
            // Bucket membership does NOT imply capacity — the node's cap
            // (stored in its header) is checked. Nodes that don't fit stay
            // on the list; falling through to the bump is safe.
            let b = bucket_of(size);
            if align <= MIN_BUCKET_SIZE && !(*chunk).free_heads[b].is_null() {
                let mut prev: *mut *mut u8 = &mut (*chunk).free_heads[b];
                let mut node = (*chunk).free_heads[b];
                let mut hops = 0usize;
                while !node.is_null() && hops < 32 {
                    let cap = ptr::read(
                        (node as usize - HEADER) as *const u32,
                    ) as usize;
                    if cap >= size {
                        let next = ptr::read(node as *const *mut u8);
                        ptr::write(prev, next);
                        let hdr = node as usize - HEADER;
                        ptr::write((hdr + 8) as *mut *mut Chunk, chunk);
                        (*chunk).outstanding += 1;
                        self.bytes_live.fetch_add(size, Ordering::Relaxed);
                        self.bytes_allocated.fetch_add(size, Ordering::Relaxed);
                        let pn = (node as usize - (*chunk).fill_base) / page_size();
                        // Saturating: a lost count is a SAFE degradation —
                        // the page just never becomes releasable (its
                        // freelist nodes' next-pointers live in payload
                        // bytes that madvise zeroes, truncating the list
                        // without ever aliasing live data).
                        let cnt = *(*chunk).page_nodes.add(pn);
                        *(*chunk).page_nodes.add(pn) = cnt.saturating_sub(1);
                        self.unlock();
                        return node as *mut u8;
                    }
                    prev = node as *mut *mut u8;
                    node = ptr::read(node as *const *mut u8);
                    hops += 1;
                }
            }
            // Data goes at `data`, header 16 bytes before it, so that both
            // the header (needs 8 for the pointer) and the data (needs
            // `align`) are satisfied.
            let base = (*chunk).fill_base;
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
                (*chunk).on_alloc_pages(base, hdr, hdr + HEADER + size);
                self.unlock();
                return data as *mut u8;
            }
            // Full: try the next chunk, else a fresh one. If even a fresh
            // chunk cannot hold it (size close to CHUNK_SIZE), fall back to
            // the system allocator for this allocation.
            let next = (*chunk).next;
            if Self::tracing() {
                eprintln!("    walk chunk {chunk:?} full; next={next:?}");
            }
            if next.is_null() {
                if HEADER + size + align > CHUNK_SIZE {
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
    }}

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) { unsafe {
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
            // spin lock.
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
        if Self::tracing() {
            eprintln!(
                "  free ptr={ptr:?} cap={} chunk={chunk:?} outstanding={}",
                ptr::read(hdr as *const u32),
                (*chunk).outstanding
            );
        }
        (*chunk).outstanding -= 1;
        self.bytes_live.fetch_sub(layout.size(), Ordering::Relaxed);
        let cap = ptr::read(hdr as *const u32) as usize;
        let span_end = hdr + HEADER + cap;
        if self.free_mode_reuse.load(Ordering::Relaxed)
            && cap >= MIN_BUCKET_SIZE
            && layout.align().max(16) <= MIN_BUCKET_SIZE
            && (*chunk).outstanding > 0
        {
            // Reuse mode: return the slot to its chunk's bucket list. The
            // NODE address is the PAYLOAD address; the next-pointer lives
            // in the payload's first 8 bytes (dead data — the caller just
            // gave them up), NEVER in the header (the header's cap/magic
            // must survive for the slot's eventual re-free). The slot's
            // pages are NOT releasable while a node exists (page_nodes).
            let b = bucket_of(cap);
            let node = (hdr + HEADER) as *mut u8;
            ptr::write(node as *mut *mut u8, (*chunk).free_heads[b]);
            (*chunk).free_heads[b] = node;
            *(*chunk)
                .page_nodes
                .add((node as usize - (*chunk).fill_base) / page_size()) += 1;
        } else {
            // Release mode (or oversized/gated slot): forget the slot —
            // its pages can now go fully dead.
            (*chunk).on_free_pages((*chunk).fill_base, hdr, span_end);
        }
        // Dead pages are swept lazily; a chunk whose last allocation died
        // recycles wholesale (its whole region madvise'd anyway).
        if (*chunk).outstanding == 0 {
            self.drop_chunk(chunk);
        } else {
            self.maybe_sweep(chunk);
        }
        self.unlock();
    }}

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 { unsafe {
        // Always allocate-copy-free: pool memory never extends in place,
        // and a size-class change (pool -> direct system) is handled by
        // the same path. The copy preserves contents across the move.
        let new_layout = Layout::from_size_align(new_size, layout.align())
            .expect("realloc layout");
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
    }}
}
