//! Chunk assignment for multi-translation-unit LIR compilation.
//!
//! Splitting a large compiled program into several smaller C files / LLVM
//! modules (instead of one giant one) only requires deciding, up front,
//! which named unit (function) goes into which output file — both the C and
//! LLVM backends already forward-declare a [`crate::LirTarget::call`] to a
//! name they haven't locally defined as a plain external-linkage
//! declaration, so a cross-chunk call needs no special handling once the
//! chunk assignment is made. See `docs/agent-context` in the workspace root
//! for the full design rationale.

use alloc::vec;
use alloc::vec::Vec;

/// Assigns each item (identified by its index into `weights`) to one of
/// `n_chunks` buckets, using a greedy longest-processing-time-first (LPT)
/// heuristic that minimizes the maximum bucket weight.
///
/// Items are considered largest-first and each is placed into the
/// currently-lightest bucket. This ignores call-graph locality — two
/// functions that call each other frequently may land in different
/// chunks, which is correct (not just fast) but not necessarily optimal
/// for minimizing cross-chunk declaration surface. Call-graph-aware
/// packing is a possible future refinement, not implemented here.
///
/// Returns a `Vec` the same length as `weights`, where `result[i]` is the
/// chunk index (`0..n_chunks`) item `i` was assigned to.
///
/// # Panics
///
/// Panics if `n_chunks == 0` and `weights` is non-empty.
pub fn assign_chunks(weights: &[u64], n_chunks: usize) -> Vec<usize> {
    if weights.is_empty() {
        return Vec::new();
    }
    assert!(n_chunks > 0, "assign_chunks: n_chunks must be > 0");

    let n_chunks = n_chunks.min(weights.len()).max(1);

    // Sort item indices by descending weight (stable, so equal-weight items
    // keep their original relative order).
    let mut order: Vec<usize> = (0..weights.len()).collect();
    order.sort_by(|&a, &b| weights[b].cmp(&weights[a]));

    let mut bucket_load: Vec<u64> = vec![0; n_chunks];
    let mut assignment: Vec<usize> = vec![0; weights.len()];

    for idx in order {
        // Pick the lightest bucket (first one wins ties, for determinism).
        let mut best = 0;
        for b in 1..n_chunks {
            if bucket_load[b] < bucket_load[best] {
                best = b;
            }
        }
        assignment[idx] = best;
        bucket_load[best] += weights[idx];
    }

    assignment
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_weights() {
        assert_eq!(assign_chunks(&[], 4), Vec::<usize>::new());
    }

    #[test]
    fn single_chunk_gets_everything() {
        let assignment = assign_chunks(&[1, 2, 3, 4], 1);
        assert_eq!(assignment, vec![0, 0, 0, 0]);
    }

    #[test]
    fn more_chunks_than_items_clamps() {
        // 3 items, 10 requested chunks -> at most 3 chunks actually used.
        let assignment = assign_chunks(&[1, 1, 1], 10);
        assert_eq!(assignment.len(), 3);
        let max_chunk = assignment.iter().copied().max().unwrap();
        assert!(max_chunk < 3);
    }

    #[test]
    fn balances_equal_weights() {
        let assignment = assign_chunks(&[1, 1, 1, 1], 2);
        let mut load = [0u64; 2];
        for (i, &c) in assignment.iter().enumerate() {
            load[c] += 1u64;
            let _ = i;
        }
        assert_eq!(load, [2, 2]);
    }

    #[test]
    fn lpt_balances_uneven_weights() {
        // Classic LPT case: one big item + several small ones should still
        // balance reasonably well across chunks.
        let weights = [10u64, 3, 3, 3, 3];
        let assignment = assign_chunks(&weights, 2);
        let mut load = [0u64; 2];
        for (i, &c) in assignment.iter().enumerate() {
            load[c] += weights[i];
        }
        // Optimal split is 10 vs 12; LPT should never do worse than putting
        // everything in one bucket, and should actually split here.
        assert_ne!(load[0], 0);
        assert_ne!(load[1], 0);
        let max = load[0].max(load[1]);
        assert!(max <= 12, "max bucket load {max} too unbalanced");
    }

    #[test]
    fn every_item_assigned_exactly_once() {
        let weights = [5u64, 1, 9, 2, 7, 3];
        let assignment = assign_chunks(&weights, 3);
        assert_eq!(assignment.len(), weights.len());
        for c in &assignment {
            assert!(*c < 3);
        }
    }
}
