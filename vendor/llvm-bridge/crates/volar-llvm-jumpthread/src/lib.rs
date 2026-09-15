//! DFA jump threading: unfolds a dispatch/state-machine loop into its
//! actual concrete control flow when a discriminator's reachable states are
//! statically enumerable.
//!
//! Runs as an LLVM-to-LLVM transform on raw IR, before selective import —
//! this gives it the full CFG with all edges, so its output passes through
//! any downstream selective importer unchanged, and lets it compose with
//! other pre-import cleanup passes (like `cirrus-llvm-pass`'s
//! `deloopify_early_exits`) in the same pipeline slot.
//!
//! # Algorithm
//!
//! For each natural loop with a single latch (multi-latch/irreducible loops
//! and loops containing a nested loop are left untouched):
//!
//! 1. **Classify** every phi at the loop header: if *every* edge into the
//!    header from outside the loop carries a literal integer constant for
//!    that phi, it joins the **discriminator tuple** (the DFA state, and the
//!    memoization key for `(header, state)` convergence); otherwise it's
//!    **carried** state (e.g. an accumulator fed by memory loads) — always
//!    treated as symbolic, regardless of whether a specific unrolled trace's
//!    value happens to be constant. This is a static, non-iterative
//!    classification, which keeps the whole algorithm free of any fixed-
//!    point-over-classifications complexity.
//! 2. **Discover** (pure Rust, no LLVM mutation): starting from each
//!    concrete entry state, walk the loop body one instruction at a time.
//!    Pure integer ops fold when every operand resolves to a discriminator-
//!    derived constant; everything else (calls, loads, stores, GEPs,
//!    non-integer ops) is left to be cloned symbolically. Every branch/
//!    switch inside the loop body must fold concretely — if one doesn't,
//!    the *whole loop* is abandoned untouched (never a partial/approximate
//!    rewrite). Because every intra-loop branch folds, each state's trace is
//!    provably a simple path with no internal merges; the state space is
//!    exactly the `(header, state)` pairs visited, memoized so repeats
//!    reconnect instead of re-unrolling — the classic control-location ×
//!    discriminator-value product, materialized once it's finite. Bails
//!    cleanly if it would exceed `JumpThreadLimits::max_states`.
//! 3. **Materialize** (only once discovery fully succeeds): one new basic
//!    block per discovered state, entry edges redirected to their state's
//!    block, loop-exit blocks' phis extended with one incoming edge per
//!    state that reaches them, and the original loop body deleted.
//!
//! See `thread::plan_loop`/`thread::materialize` for the implementation, and
//! `cfg` for the shared natural-loop utilities (adapted from
//! `cirrus-llvm-pass`'s `deloopify.rs`, which proved this exact CFG-analysis
//! approach safe for a real LLVM-IR-mutating pass).

mod cfg;
mod eval;
mod thread;

use std::collections::HashMap;

use inkwell::values::FunctionValue;

/// Safety backstops on the discovered state space.
#[derive(Clone, Debug)]
pub struct JumpThreadLimits {
    /// Ceiling on the total number of distinct `(header, state)` nodes
    /// discovered across the whole transform (per function, cumulative
    /// across all loops rewritten in the same `thread_jumps` call). Bails
    /// the loop currently being discovered — leaving it, and everything not
    /// yet visited, untouched — the instant this would be exceeded.
    pub max_states: usize,
}

impl Default for JumpThreadLimits {
    fn default() -> Self {
        JumpThreadLimits { max_states: 4096 }
    }
}

/// Outcome of a [`thread_jumps`] call.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThreadResult {
    /// At least one loop was rewritten.
    Threaded { loops_rewritten: usize },
    /// No loop in the function matched the idiom (or the function has no
    /// loops at all); the module is byte-for-byte unchanged.
    NoChange,
}

/// Attempt DFA jump threading on every eligible loop in `function`.
///
/// Never partially rewrites a loop: every loop either fully threads (all its
/// reachable discriminator states enumerated and materialized) or is left
/// completely untouched. Re-scans the CFG after each successful rewrite
/// (rewriting one loop can only ever remove blocks, never invalidate a
/// different loop's own eligibility) until no further loop matches.
pub fn thread_jumps<'ctx>(function: FunctionValue<'ctx>, limits: JumpThreadLimits) -> ThreadResult {
    let Some(entry) = function.get_first_basic_block() else {
        return ThreadResult::NoChange;
    };

    let mut loops_rewritten = 0;
    loop {
        let (successors, predecessors) = cfg::build_cfg_maps(function);
        let back_edges = cfg::find_back_edges(entry, &successors);

        let mut by_header: HashMap<_, Vec<_>> = HashMap::new();
        for (latch, header) in &back_edges {
            by_header.entry(*header).or_default().push(*latch);
        }

        let mut rewrote_one = false;
        for (&header, latches) in &by_header {
            if latches.len() != 1 {
                continue; // multi-latch/irreducible: not handled.
            }
            let latch = latches[0];
            let body = cfg::natural_loop_body(header, latch, &predecessors);
            let nested = back_edges.iter().any(|(other_latch, other_header)| {
                *other_header != header && *other_latch != latch && body.contains(other_header)
            });
            if nested {
                continue;
            }
            if let Some(plan) =
                thread::plan_loop(header, latch, &body, &successors, &predecessors, &limits)
            {
                thread::materialize(function, &plan);
                loops_rewritten += 1;
                rewrote_one = true;
                break; // CFG changed; recompute before looking further.
            }
        }
        if !rewrote_one {
            break;
        }
    }

    if loops_rewritten > 0 {
        ThreadResult::Threaded { loops_rewritten }
    } else {
        ThreadResult::NoChange
    }
}
