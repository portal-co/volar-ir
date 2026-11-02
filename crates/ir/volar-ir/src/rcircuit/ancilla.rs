// @reliability: normal
//! Borrow/restore ancilla-wire allocator for reversible-circuit generation.
//!
//! Generation passes often need statement-local temporary wires. Allocating
//! each one fresh inflates the wire space (and, in hardened embeddings, the
//! borrowed-wire region that scales with it). An [`AncillaPool`] recycles
//! temporaries instead, under a strict contract:
//!
//! - **Borrow/restore.** A wire handed out by [`AncillaPool::acquire`] must,
//!   before [`AncillaPool::release`], be restored by reversing the gates that
//!   modified it to the exact value it held at acquisition.
//! - **Totality.** Restoration must hold for *every* initial wire assignment,
//!   not just zero-initialized runs. Pooled wires then behave identically on
//!   legitimate and illegitimate initial states, which is what keeps
//!   hardened-mode embeddings (nonzero ancilla input implies the identity
//!   function) intact under sharing.
//!
//! Reuse rule (the sharing invariant): a released wire may be handed out
//! again immediately. Because every borrower restores its wire, successive
//! borrows on one physical wire have disjoint live intervals separated by
//! restore points — the interval-coloring precondition of the ancilla-sharing
//! proof sketch (obfustopia plan 2026-11-02, §5; the sketch's "clean at
//! interval boundary" is the zero-initialized special case of the
//! borrow/restore contract above).
//!
//! The pool is a generation-time data structure: it never inspects runtime
//! wire values, so `release` cannot check the restore itself. Contract
//! violations are therefore logic errors in the generator; the pool does
//! fail closed on structural misuse (double release, releasing a wire it
//! never issued).

use alloc::collections::BTreeSet;
use alloc::vec::Vec;

/// Free-list allocator over a contiguous wire region `[base, ∞)`, handing out
/// fresh wires on demand and recycling restored ones.
#[derive(Clone, Debug)]
pub struct AncillaPool {
    /// First wire index this pool may allocate.
    base: usize,
    /// Next never-issued wire index.
    next_fresh: usize,
    /// Issued-and-restored wires available for reuse (a stack: most recently
    /// released first, keeping reuse deterministic and local).
    free: Vec<usize>,
    /// Currently borrowed wires, for structural misuse detection.
    live: BTreeSet<usize>,
}

impl AncillaPool {
    /// A pool managing wires from `base` upward.
    pub fn new(base: usize) -> Self {
        Self {
            base,
            next_fresh: base,
            free: Vec::new(),
            live: BTreeSet::new(),
        }
    }

    /// Borrow a wire: a recycled restored wire when one is available, else a
    /// fresh index. The borrower must restore and release it (see module
    /// docs); wires intentionally left as garbage are simply never released.
    pub fn acquire(&mut self) -> usize {
        if let Some(wire) = self.free.pop() {
            let newly = self.live.insert(wire);
            debug_assert!(newly, "free-listed wire was still live");
            return wire;
        }
        let wire = self.next_fresh;
        self.next_fresh += 1;
        let newly = self.live.insert(wire);
        debug_assert!(newly, "fresh wire collided with a live one");
        wire
    }

    /// Return a borrowed wire after its restore. Panics on structural misuse:
    /// the wire must have been issued by this pool, be currently live, and be
    /// released at most once per borrow.
    pub fn release(&mut self, wire: usize) {
        assert!(
            wire >= self.base && wire < self.next_fresh,
            "ancilla pool: release of foreign wire {wire} (region starts at {})",
            self.base
        );
        assert!(
            self.live.remove(&wire),
            "ancilla pool: double release of wire {wire}"
        );
        self.free.push(wire);
    }

    /// Peak wires ever issued (fresh allocations), i.e. the size of the
    /// region above `base` the circuit must reserve for this pool.
    pub fn peak_allocated(&self) -> usize {
        self.next_fresh - self.base
    }

    /// Wires currently borrowed (not yet restored).
    pub fn live_count(&self) -> usize {
        self.live.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_wires_are_dense_and_sequential() {
        let mut pool = AncillaPool::new(10);
        assert_eq!(pool.acquire(), 10);
        assert_eq!(pool.acquire(), 11);
        assert_eq!(pool.peak_allocated(), 2);
        assert_eq!(pool.live_count(), 2);
    }

    #[test]
    fn released_wires_are_recycled_before_fresh_growth() {
        let mut pool = AncillaPool::new(0);
        let a = pool.acquire();
        let b = pool.acquire();
        pool.release(a);
        pool.release(b);
        // LIFO: most recently released comes back first.
        assert_eq!(pool.acquire(), b);
        assert_eq!(pool.acquire(), a);
        assert_eq!(pool.peak_allocated(), 2);
    }

    #[test]
    #[should_panic(expected = "double release")]
    fn double_release_fails_closed() {
        let mut pool = AncillaPool::new(0);
        let a = pool.acquire();
        pool.release(a);
        pool.release(a);
    }

    #[test]
    #[should_panic(expected = "foreign wire")]
    fn releasing_a_foreign_wire_fails_closed() {
        let mut pool = AncillaPool::new(5);
        pool.release(2);
    }
}
