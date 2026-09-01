// @reliability: experimental
// @ai: assisted
//! Budgeted inlining pass for VAFFLE (`Module`).
//!
//! Splices small, non-recursive callees directly into their `Value::Call`
//! sites so fewer calls reach `volar-vaffle-target`'s unconditional
//! on-stack call convention (see `lower_to_ir.rs`'s module doc). The
//! module's call graph is processed bottom-up (callees are fully inlined
//! before being used as a splice source for their own callers), so nested
//! inlining opportunities compound correctly.
//!
//! # Recursion
//!
//! VAFFLE supports mutually- and self-recursive functions (see
//! `volar-vaffle-target::vaffle_ssa`'s module doc). Any function that can
//! reach itself via one or more calls is never an inlining *target*
//! (callee) — this sidesteps unbounded inlining entirely rather than
//! attempting bounded recursive inlining. Such a function may still freely
//! be used as an inlining *source* (caller): its own calls to
//! non-recursive functions are inlined normally.
//!
//! # Scope
//!
//! Only `Value::Call` sites are inlining targets. `Terminator::ReturnCall`
//! (tail calls) are left untouched — `lower_to_ir.rs` already gives these
//! a cheaper direct-jump path with no spill/continuation write. A tail
//! call left inside a spliced-in callee body is *not* redirected to the
//! synthetic continuation block described below — it naturally (and
//! correctly) bypasses it: once spliced, that code executes as part of
//! the caller's own frame, so the on-stack lowering's continuation-slot
//! reuse for `ReturnCall` resolves to the caller's own continuation,
//! exactly as if the tail call had always lived there.
//!
//! Only a function's *own original* call sites are considered for
//! inlining — a call newly spliced into a caller by this function's own
//! inlining pass is not itself reconsidered in the same pass. It was
//! already resolved (inlined, or left as a real call) when its owning
//! callee was processed earlier, in bottom-up order.
//!
//! # Splicing
//!
//! For an eligible call site `Value::Call { func: callee, args }` in
//! block `B` of the caller:
//!
//! 1. The callee's body is cloned and every `ValueId`/`BlockId`/
//!    `StackAlloc::base_slot` inside it is shifted into fresh caller-local
//!    ranges (VAFFLE's ids are flat, function-global arenas — not
//!    block-local — so splicing into another function's arena requires
//!    shifting every reference, including the function-local stack-slot
//!    bump-allocator range a `StackAlloc` claims).
//! 2. `B` is split at the call site: everything before it stays in `B`;
//!    a fresh continuation block gets `B`'s old terminator plus everything
//!    after the call (minus the call's own `Value::Output` uses, which
//!    become the continuation's block params — see below). `B`'s new
//!    terminator jumps into the callee's (remapped) entry block, passing
//!    the call's original arguments as its params.
//! 3. Every remapped `Terminator::Return` inside the spliced callee is
//!    rewritten to jump into the continuation block instead, passing its
//!    return values as jump args — reconverging every callee exit.
//! 4. The call's existing `Value::Output { value, idx }` nodes (if any)
//!    are repurposed *in place* (same `ValueId`, so every existing
//!    downstream use keeps working unmodified) as the continuation
//!    block's params; any return index with no existing `Output` node
//!    (an unused result) gets a fresh, unreferenced param instead.

use alloc::{
    collections::{BTreeMap, BTreeSet},
    vec::Vec,
};
use core::convert::Infallible;
use core::fmt;

use vaffle::{
    Block, BlockId, FuncBody, FuncDecl, FuncId, Module, Target, Terminator, Value, ValueId,
};
use volar_ir_common::{Node, TypeId};

// ============================================================================
// Public API
// ============================================================================

/// Budget controlling how aggressively [`inline_vaffle_module`] inlines.
#[derive(Clone, Copy, Debug)]
pub struct InlineBudget {
    /// A callee whose body has more than this many [`Value`]s is never
    /// inlined, regardless of remaining budget.
    pub max_callee_values: usize,
    /// Total [`Value`] count this pass is allowed to add to the module
    /// (summed across every callee it splices in) before it stops.
    ///
    /// This is a simple, uniform-per-`Value` proxy for code-size growth —
    /// weighting by `Value`/`Stmt` kind (e.g. `Poly`, `Shuffle` cost more
    /// downstream than a `Const`) is a reasonable follow-up once real
    /// measurements justify it; not attempted here.
    pub total_budget: usize,
}

/// Inline eligible VAFFLE call sites in `module`, in place, until `budget`
/// is exhausted. Returns the number of call sites inlined.
pub fn inline_vaffle_module<P: Clone>(module: &mut Module<P>, budget: InlineBudget) -> usize {
    let recursive = recursive_funcs(&module.funcs);
    let order = bottom_up_order(&module.funcs);

    let mut spent = 0usize;
    let mut total_inlined = 0usize;

    for func_idx in order {
        if spent >= budget.total_budget {
            break;
        }
        inline_calls_in_function(
            module,
            func_idx,
            &recursive,
            &budget,
            &mut spent,
            &mut total_inlined,
        );
    }

    total_inlined
}

/// Why [`inline_vaffle_everything`] could not eliminate every intra-module
/// Body-to-Body call reachable from the given entries.
///
/// Aligns with the budgeted pass's recursion rule: recursive functions are
/// never inlining *targets*, so a leftover Body-to-Body call after unlimited
/// inlining is either recursion or an unexpected remainder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InlineEverythingError {
    /// `entries` contained a [`FuncId`] that is not a function in the module.
    UnknownEntry { entry: FuncId },
    /// One or more entry-reachable Body-to-Body calls remain because the
    /// callee can reach itself (self- or mutually-recursive).
    Recursive { funcs: Vec<FuncId> },
    /// A Body-to-Body call remains for a reason other than recursion.
    RemainingCall { caller: FuncId, callee: FuncId },
}

impl fmt::Display for InlineEverythingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InlineEverythingError::UnknownEntry { entry } => {
                write!(f, "unknown entry function {}", entry.0)
            }
            InlineEverythingError::Recursive { funcs } => {
                write!(f, "recursive functions cannot be fully inlined:")?;
                for id in funcs {
                    write!(f, " {}", id.0)?;
                }
                Ok(())
            }
            InlineEverythingError::RemainingCall { caller, callee } => {
                write!(
                    f,
                    "remaining Body-to-Body call from {} to {}",
                    caller.0, callee.0
                )
            }
        }
    }
}

impl core::error::Error for InlineEverythingError {}

/// Unlimited-budget inlining of every non-recursive intra-module call,
/// including [`Terminator::ReturnCall`].
///
/// After success, every Body-to-Body `Call`/`ReturnCall` reachable from
/// `entries` is gone (imports/oracles/actions may remain). Unused callee
/// bodies are left in place so [`FuncId`]s stay stable. Fails closed on
/// recursion or any leftover Body call.
pub fn inline_vaffle_everything<P: Clone>(
    module: &mut Module<P>,
    entries: &[FuncId],
) -> Result<usize, InlineEverythingError> {
    for &entry in entries {
        if entry.0 >= module.funcs.len() {
            return Err(InlineEverythingError::UnknownEntry { entry });
        }
    }

    let unlimited = InlineBudget {
        max_callee_values: usize::MAX,
        total_budget: usize::MAX,
    };
    let recursive = recursive_funcs(&module.funcs);
    let order = bottom_up_order(&module.funcs);

    let mut spent = 0usize;
    let mut total_inlined = 0usize;

    for func_idx in order {
        inline_calls_in_function(
            module,
            func_idx,
            &recursive,
            &unlimited,
            &mut spent,
            &mut total_inlined,
        );
        inline_return_calls_in_function(
            module,
            func_idx,
            &recursive,
            &mut spent,
            &mut total_inlined,
        );
    }

    if let Some(err) = remaining_body_call_error(module, entries, &recursive) {
        return Err(err);
    }

    // Unused callee bodies are left in place (FuncId-stable). A splice keeps
    // the old Value::Call arena entry; rewriting those as Import stubs
    // confuses VAFFLE-to-IR lowering, which still walks the values arena.
    Ok(total_inlined)
}

// ============================================================================
// Call graph
// ============================================================================

/// Direct call edges (by `Vec` index) for every function in `funcs`.
/// Includes both `Value::Call` and `Terminator::ReturnCall` targets — both
/// matter for recursion detection, even though only `Value::Call` sites
/// are ever inlined.
fn call_edges<P: Clone>(funcs: &[FuncDecl<P>]) -> Vec<Vec<usize>> {
    let mut edges: Vec<Vec<usize>> = alloc::vec![Vec::new(); funcs.len()];
    for (i, f) in funcs.iter().enumerate() {
        let FuncDecl::Body(body) = f else { continue };
        for v in &body.values {
            if let Value::Call { func, .. } = &v.kind {
                edges[i].push(func.0);
            }
        }
        for b in &body.blocks {
            if let Terminator::ReturnCall { func, .. } = &b.terminator {
                edges[i].push(func.0);
            }
        }
    }
    edges
}

/// Functions that can reach themselves via one or more calls (self- or
/// mutually-recursive). These are never inlining *targets* (callees) —
/// see the module doc's Recursion section.
fn recursive_funcs<P: Clone>(funcs: &[FuncDecl<P>]) -> BTreeSet<usize> {
    let edges = call_edges(funcs);
    let n = funcs.len();
    let mut recursive = BTreeSet::new();
    for start in 0..n {
        let mut visited = alloc::vec![false; n];
        let mut stack: Vec<usize> = edges[start].clone();
        let mut found = false;
        while let Some(cur) = stack.pop() {
            if cur == start {
                found = true;
                break;
            }
            if core::mem::replace(&mut visited[cur], true) {
                continue;
            }
            stack.extend(edges[cur].iter().copied());
        }
        if found {
            recursive.insert(start);
        }
    }
    recursive
}

/// Function indices in bottom-up order: if `f` calls `g`, `g` appears
/// before `f` (for the acyclic part of the call graph; order within a
/// recursive cycle is unconstrained, since no inlining ever happens
/// between cycle members anyway). Iterative post-order DFS — no recursion,
/// so no stack-depth risk on adversarial/fuzzed module shapes.
fn bottom_up_order<P: Clone>(funcs: &[FuncDecl<P>]) -> Vec<usize> {
    let edges = call_edges(funcs);
    let n = funcs.len();
    let mut visited = alloc::vec![false; n];
    let mut order = Vec::with_capacity(n);

    for start in 0..n {
        if visited[start] {
            continue;
        }
        let mut work: Vec<(usize, usize)> = alloc::vec![(start, 0)];
        visited[start] = true;
        while let Some(&mut (u, ref mut child_ix)) = work.last_mut() {
            if *child_ix < edges[u].len() {
                let v = edges[u][*child_ix];
                *child_ix += 1;
                if !visited[v] {
                    visited[v] = true;
                    work.push((v, 0));
                }
            } else {
                order.push(u);
                work.pop();
            }
        }
    }
    order
}

// ============================================================================
// Per-function inlining loop
// ============================================================================

fn inline_calls_in_function<P: Clone>(
    module: &mut Module<P>,
    func_idx: usize,
    recursive: &BTreeSet<usize>,
    budget: &InlineBudget,
    spent: &mut usize,
    total_inlined: &mut usize,
) {
    let original_value_count = match &module.funcs[func_idx] {
        FuncDecl::Body(b) => b.values.len(),
        _ => return,
    };
    let mut processed: BTreeSet<usize> = BTreeSet::new();

    loop {
        if *spent >= budget.total_budget {
            return;
        }

        let next = {
            let FuncDecl::Body(body) = &module.funcs[func_idx] else {
                return;
            };
            body.values[..original_value_count]
                .iter()
                .enumerate()
                .find_map(|(i, node)| {
                    if processed.contains(&i) {
                        return None;
                    }
                    let Value::Call { func, .. } = &node.kind else {
                        return None;
                    };
                    let callee_idx = func.0;
                    if callee_idx == func_idx || recursive.contains(&callee_idx) {
                        return None;
                    }
                    match module.funcs.get(callee_idx) {
                        Some(FuncDecl::Body(cb)) if cb.values.len() <= budget.max_callee_values => {
                            Some((ValueId(i), FuncId(callee_idx)))
                        }
                        _ => None,
                    }
                })
        };

        let Some((call_vid, callee_id)) = next else {
            return;
        };
        processed.insert(call_vid.0);

        let (callee_body, sig_id) = match &module.funcs[callee_id.0] {
            FuncDecl::Body(b) => (clone_func_body(b), b.sig),
            _ => unreachable!("eligibility check above already required FuncDecl::Body"),
        };
        let callee_results = module.sigs[sig_id.0].results.clone();

        let added = splice_call(module, func_idx, call_vid, &callee_body, &callee_results);
        *spent += added;
        *total_inlined += 1;
    }
}

fn clone_func_body<P: Clone>(b: &FuncBody<P>) -> FuncBody<P> {
    FuncBody {
        sig: b.sig,
        blocks: b.blocks.clone(),
        values: b.values.clone(),
        entry: b.entry,
    }
}

fn inline_return_calls_in_function<P: Clone>(
    module: &mut Module<P>,
    func_idx: usize,
    recursive: &BTreeSet<usize>,
    spent: &mut usize,
    total_inlined: &mut usize,
) {
    if !matches!(&module.funcs[func_idx], FuncDecl::Body(_)) {
        return;
    }

    loop {
        let next = {
            let FuncDecl::Body(body) = &module.funcs[func_idx] else {
                return;
            };
            body.blocks.iter().enumerate().find_map(|(bi, b)| {
                let Terminator::ReturnCall { func, .. } = &b.terminator else {
                    return None;
                };
                let callee_idx = func.0;
                if callee_idx == func_idx || recursive.contains(&callee_idx) {
                    return None;
                }
                match module.funcs.get(callee_idx) {
                    Some(FuncDecl::Body(_)) => Some((BlockId(bi), FuncId(callee_idx))),
                    _ => None,
                }
            })
        };

        let Some((block_id, callee_id)) = next else {
            return;
        };

        let callee_body = match &module.funcs[callee_id.0] {
            FuncDecl::Body(b) => clone_func_body(b),
            _ => unreachable!("eligibility check above already required FuncDecl::Body"),
        };

        let added = splice_return_call(module, func_idx, block_id, &callee_body);
        *spent += added;
        *total_inlined += 1;
    }
}

fn live_callee_ids<P: Clone>(body: &FuncBody<P>) -> impl Iterator<Item = FuncId> + '_ {
    let from_stmts = body.blocks.iter().flat_map(|b| {
        b.stmts.iter().filter_map(|&vid| match &body.values[vid.0].kind {
            Value::Call { func, .. } => Some(*func),
            _ => None,
        })
    });
    let from_terms = body.blocks.iter().filter_map(|b| match &b.terminator {
        Terminator::ReturnCall { func, .. } => Some(*func),
        _ => None,
    });
    from_stmts.chain(from_terms)
}

fn remaining_body_call_error<P: Clone>(
    module: &Module<P>,
    entries: &[FuncId],
    recursive: &BTreeSet<usize>,
) -> Option<InlineEverythingError> {
    let mut seen = BTreeSet::new();
    let mut stack: Vec<usize> = entries.iter().map(|e| e.0).collect();
    let mut leftover: Vec<(FuncId, FuncId)> = Vec::new();

    while let Some(idx) = stack.pop() {
        if !seen.insert(idx) {
            continue;
        }
        let FuncDecl::Body(body) = &module.funcs[idx] else {
            continue;
        };
        for callee in live_callee_ids(body) {
            stack.push(callee.0);
            if matches!(module.funcs.get(callee.0), Some(FuncDecl::Body(_))) {
                leftover.push((FuncId(idx), callee));
            }
        }
    }

    if leftover.is_empty() {
        return None;
    }

    let recursive_leftover: Vec<FuncId> = leftover
        .iter()
        .filter(|(_, callee)| recursive.contains(&callee.0))
        .map(|(_, callee)| *callee)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    if !recursive_leftover.is_empty() {
        return Some(InlineEverythingError::Recursive {
            funcs: recursive_leftover,
        });
    }

    let (caller, callee) = leftover[0];
    Some(InlineEverythingError::RemainingCall { caller, callee })
}

// ============================================================================
// Splicing
// ============================================================================

/// Splice `callee_body` into `module.funcs[func_idx]` at `call_vid`,
/// returning the number of `Value`s added (for budget accounting). See the
/// module doc's Splicing section for the algorithm.
fn splice_call<P: Clone>(
    module: &mut Module<P>,
    func_idx: usize,
    call_vid: ValueId,
    callee_body: &FuncBody<P>,
    callee_results: &[TypeId],
) -> usize {
    let (value_base, block_base, slot_base) = match &module.funcs[func_idx] {
        FuncDecl::Body(b) => (b.values.len(), b.blocks.len(), stack_slot_high_water(b)),
        _ => unreachable!(),
    };

    let remapped_values: Vec<Node<Value, P>> = callee_body
        .values
        .iter()
        .map(|n| remap_callee_node(n, value_base, block_base, slot_base))
        .collect();
    let remapped_blocks: Vec<Block> = callee_body
        .blocks
        .iter()
        .map(|b| remap_callee_block(b, value_base, block_base))
        .collect();
    let remapped_entry = BlockId(callee_body.entry.0 + block_base);
    let added = remapped_values.len();

    let (call_args, call_block_id, call_stmt_pos) = {
        let FuncDecl::Body(body) = &module.funcs[func_idx] else {
            unreachable!()
        };
        let Value::Call { args, .. } = &body.values[call_vid.0].kind else {
            panic!("inline_vaffle::splice_call: call_vid does not point at a Value::Call");
        };
        let (bid, pos) = find_call_site(body, call_vid)
            .expect("inline_vaffle::splice_call: call site not found in any block's stmts");
        (args.clone(), bid, pos)
    };

    // Existing `Value::Output { value: call_vid, idx }` nodes, keyed by
    // idx — these get repurposed in place as the continuation's params.
    let existing_outputs: BTreeMap<usize, ValueId> = {
        let FuncDecl::Body(body) = &module.funcs[func_idx] else {
            unreachable!()
        };
        body.values
            .iter()
            .enumerate()
            .filter_map(|(i, n)| match &n.kind {
                Value::Output { value, idx } if *value == call_vid => Some((*idx, ValueId(i))),
                _ => None,
            })
            .collect()
    };
    let repurposed: BTreeSet<usize> = existing_outputs.values().map(|v| v.0).collect();

    let FuncDecl::Body(body) = &mut module.funcs[func_idx] else {
        unreachable!()
    };

    body.values.extend(remapped_values);
    let callee_block_start = body.blocks.len();
    body.blocks.extend(remapped_blocks);
    let cont_block_id = BlockId(body.blocks.len());

    let (call_prov, call_side) = {
        let call_node = &body.values[call_vid.0];
        (call_node.prov.clone(), call_node.side)
    };
    let mut cont_params: Vec<(ValueId, TypeId)> = Vec::with_capacity(callee_results.len());
    for (k, ty) in callee_results.iter().enumerate() {
        let param_vid = if let Some(&vid) = existing_outputs.get(&k) {
            body.values[vid.0].kind = Value::Param {
                block: cont_block_id,
                ty: *ty,
                idx: k,
            };
            vid
        } else {
            let vid = ValueId(body.values.len());
            body.values.push(Node::new(
                Value::Param {
                    block: cont_block_id,
                    ty: *ty,
                    idx: k,
                },
                call_prov.clone(),
                call_side,
            ));
            vid
        };
        cont_params.push((param_vid, *ty));
    }

    let (cont_stmts, old_terminator) = {
        let b = &mut body.blocks[call_block_id.0];
        let tail: Vec<ValueId> = b
            .stmts
            .split_off(call_stmt_pos + 1)
            .into_iter()
            .filter(|v| !repurposed.contains(&v.0))
            .collect();
        let removed_call = b.stmts.pop();
        debug_assert_eq!(
            removed_call.map(|v| v.0),
            Some(call_vid.0),
            "inline_vaffle::splice_call: call site position mismatch"
        );
        let old_term = core::mem::replace(
            &mut b.terminator,
            Terminator::Jump(Target {
                block: remapped_entry,
                args: call_args,
                reentry: None,
            }),
        );
        (tail, old_term)
    };
    body.blocks.push(Block {
        params: cont_params,
        stmts: cont_stmts,
        terminator: old_terminator,
    });

    for b in &mut body.blocks[callee_block_start..callee_block_start + callee_body.blocks.len()] {
        if let Terminator::Return { values } = &b.terminator {
            let values = values.clone();
            b.terminator = Terminator::Jump(Target {
                block: cont_block_id,
                args: values,
                reentry: None,
            });
        }
    }

    added
}

/// Splice `callee_body` in place of a [`Terminator::ReturnCall`]. The
/// terminator becomes a jump into the remapped callee entry; callee
/// `Return`s stay `Return` (they are already the caller's returns).
fn splice_return_call<P: Clone>(
    module: &mut Module<P>,
    func_idx: usize,
    block_id: BlockId,
    callee_body: &FuncBody<P>,
) -> usize {
    let (value_base, block_base, slot_base) = match &module.funcs[func_idx] {
        FuncDecl::Body(b) => (b.values.len(), b.blocks.len(), stack_slot_high_water(b)),
        _ => unreachable!(),
    };

    let remapped_values: Vec<Node<Value, P>> = callee_body
        .values
        .iter()
        .map(|n| remap_callee_node(n, value_base, block_base, slot_base))
        .collect();
    let remapped_blocks: Vec<Block> = callee_body
        .blocks
        .iter()
        .map(|b| remap_callee_block(b, value_base, block_base))
        .collect();
    let remapped_entry = BlockId(callee_body.entry.0 + block_base);
    let added = remapped_values.len();

    let call_args = {
        let FuncDecl::Body(body) = &module.funcs[func_idx] else {
            unreachable!()
        };
        match &body.blocks[block_id.0].terminator {
            Terminator::ReturnCall { args, .. } => args.clone(),
            _ => panic!(
                "inline_vaffle::splice_return_call: block terminator is not ReturnCall"
            ),
        }
    };

    let FuncDecl::Body(body) = &mut module.funcs[func_idx] else {
        unreachable!()
    };

    body.values.extend(remapped_values);
    body.blocks.extend(remapped_blocks);
    body.blocks[block_id.0].terminator = Terminator::Jump(Target {
        block: remapped_entry,
        args: call_args,
        reentry: None,
    });

    added
}

fn find_call_site<P: Clone>(body: &FuncBody<P>, call_vid: ValueId) -> Option<(BlockId, usize)> {
    for (bi, b) in body.blocks.iter().enumerate() {
        if let Some(pos) = b.stmts.iter().position(|&v| v == call_vid) {
            return Some((BlockId(bi), pos));
        }
    }
    None
}

fn stack_slot_high_water<P: Clone>(body: &FuncBody<P>) -> u64 {
    body.values.iter().fold(0u64, |acc, n| match &n.kind {
        Value::StackAlloc {
            count, base_slot, ..
        } => acc.max(base_slot + *count as u64),
        _ => acc,
    })
}

// ============================================================================
// Callee remapping (ValueId / BlockId / stack-slot shift)
// ============================================================================

fn remap_callee_node<P: Clone>(
    node: &Node<Value, P>,
    value_base: usize,
    block_base: usize,
    slot_base: u64,
) -> Node<Value, P> {
    let shifted = node
        .kind
        .clone()
        .map(&mut (), |_, v: ValueId| {
            Ok::<_, Infallible>(ValueId(v.0 + value_base))
        })
        .unwrap();
    let kind = match shifted {
        Value::Param { block, ty, idx } => Value::Param {
            block: BlockId(block.0 + block_base),
            ty,
            idx,
        },
        Value::BlockAddr { block } => Value::BlockAddr {
            block: BlockId(block.0 + block_base),
        },
        Value::StackAlloc {
            elem_ty,
            count,
            base_slot,
        } => Value::StackAlloc {
            elem_ty,
            count,
            base_slot: base_slot + slot_base,
        },
        other => other,
    };
    Node::new(kind, node.prov.clone(), node.side)
}

fn remap_callee_block(block: &Block, value_base: usize, block_base: usize) -> Block {
    Block {
        params: block
            .params
            .iter()
            .map(|(vid, ty)| (ValueId(vid.0 + value_base), *ty))
            .collect(),
        stmts: block
            .stmts
            .iter()
            .map(|vid| ValueId(vid.0 + value_base))
            .collect(),
        terminator: remap_callee_terminator(block.terminator.clone(), value_base, block_base),
    }
}

fn remap_callee_terminator(term: Terminator, value_base: usize, block_base: usize) -> Terminator {
    let mut term = term
        .map(&mut (), |_, v: ValueId| {
            Ok::<_, Infallible>(ValueId(v.0 + value_base))
        })
        .unwrap();

    fn shift_target(t: &mut Target, block_base: usize) {
        t.block = BlockId(t.block.0 + block_base);
    }

    match &mut term {
        Terminator::Jump(t) => shift_target(t, block_base),
        Terminator::IfNonzero {
            then_target,
            else_target,
            ..
        } => {
            shift_target(then_target, block_base);
            shift_target(else_target, block_base);
        }
        Terminator::Table {
            targets,
            default_target,
            ..
        } => {
            for t in targets.iter_mut() {
                shift_target(t, block_base);
            }
            shift_target(default_target, block_base);
        }
        Terminator::Return { .. } | Terminator::ReturnCall { .. } => {}
        _ => panic!(
            "inline_vaffle::remap_callee_terminator: unhandled Terminator variant — add block-id remapping for this variant"
        ),
    }
    term
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    extern crate std;
    use alloc::vec;
    use vaffle::{
        Block, BlockId, FuncBody, FuncDecl, FuncId, Module, SigDecl, Target, Terminator, Value,
        ValueId,
    };
    use volar_ir_common::{Constant, Node, Stmt, Type, TypeId, TypeTable};

    use super::{InlineBudget, InlineEverythingError, inline_vaffle_everything, inline_vaffle_module};

    fn empty_module() -> Module {
        Module {
            types: TypeTable::new(),
            oracles: vec![],
            actions: vec![],
            funcs: vec![],
            sigs: vec![],
            exports: std::collections::BTreeMap::new(),
            pre_init: vec![],
        }
    }

    fn op_node(stmt: Stmt<ValueId>) -> Node<Value, ()> {
        Node::new(Value::Op(stmt), (), None)
    }

    fn const_node(v: u128, ty: TypeId) -> Node<Value, ()> {
        op_node(Stmt::Const(Constant { hi: 0, lo: v }, ty))
    }

    fn generous_budget() -> InlineBudget {
        InlineBudget {
            max_callee_values: 64,
            total_budget: 1024,
        }
    }

    /// Whether `callee` is still called from a *live* (listed in some
    /// block's `stmts`) `Value::Call` site. A splice leaves the old call
    /// node's arena entry in place but unreferenced -- this checks
    /// liveness, not mere presence in the values arena.
    fn has_live_call_to(body: &FuncBody, callee: FuncId) -> bool {
        body.blocks.iter().any(|b| {
            b.stmts.iter().any(|vid| {
                matches!(&body.values[vid.0].kind, Value::Call { func, .. } if *func == callee)
            })
        })
    }

    // ------------------------------------------------------------------
    // 1. Straight-line callee
    // ------------------------------------------------------------------

    #[test]
    fn straight_line_callee_is_inlined() {
        let mut m = empty_module();
        let u64_ty = m.types.primitive(Type::_64);

        // sig 0: caller () -> u64. sig 1: callee (u64) -> u64.
        m.sigs.push(SigDecl {
            params: vec![],
            results: vec![u64_ty],
        });
        m.sigs.push(SigDecl {
            params: vec![u64_ty],
            results: vec![u64_ty],
        });

        // Callee (FuncId 1): identity function, returns its own param.
        let callee = FuncBody {
            sig: vaffle::SigId(1),
            blocks: vec![Block {
                params: vec![(ValueId(0), u64_ty)],
                stmts: vec![],
                terminator: Terminator::Return {
                    values: vec![ValueId(0)],
                },
            }],
            values: vec![Node::new(
                Value::Param {
                    block: BlockId(0),
                    ty: u64_ty,
                    idx: 0,
                },
                (),
                None,
            )],
            entry: BlockId(0),
        };

        // Caller (FuncId 0): v0 = const 5; v1 = call callee(v0); v2 = output(v1, 0); return v2.
        let v0 = ValueId(0);
        let v1 = ValueId(1);
        let v2 = ValueId(2);
        let caller_values = vec![
            const_node(5, u64_ty),
            Node::new(
                Value::Call {
                    func: FuncId(1),
                    args: vec![v0],
                },
                (),
                None,
            ),
            Node::new(Value::Output { value: v1, idx: 0 }, (), None),
        ];
        let caller = FuncBody {
            sig: vaffle::SigId(0),
            blocks: vec![Block {
                params: vec![],
                stmts: vec![v0, v1, v2],
                terminator: Terminator::Return { values: vec![v2] },
            }],
            values: caller_values,
            entry: BlockId(0),
        };

        m.funcs.push(FuncDecl::Body(caller));
        m.funcs.push(FuncDecl::Body(callee));

        let n = inline_vaffle_module(&mut m, generous_budget());
        assert_eq!(n, 1, "expected exactly one call site inlined");

        let FuncDecl::Body(caller) = &m.funcs[0] else {
            panic!("expected FuncDecl::Body")
        };
        assert!(
            !has_live_call_to(caller, FuncId(1)),
            "Value::Call to the inlined callee should be gone"
        );

        // v2 (the old Output node) was repurposed in place as the
        // continuation block's param.
        match &caller.values[v2.0].kind {
            Value::Param { idx: 0, .. } => {}
            other => panic!("expected v2 to become a Value::Param, got {other:?}"),
        }

        // Block 0 was split: one new callee-entry block plus one
        // continuation block were appended (1 -> 3 total).
        assert_eq!(caller.blocks.len(), 3, "expected the block to split into 3");

        match &caller.blocks[0].terminator {
            Terminator::Jump(Target { block, args, .. }) => {
                assert_eq!(
                    *block,
                    BlockId(1),
                    "should jump into the spliced callee entry"
                );
                assert_eq!(args, &vec![v0], "should pass the original call args");
            }
            other => panic!("expected Terminator::Jump, got {other:?}"),
        }
    }

    // ------------------------------------------------------------------
    // 2. Multi-block / branching callee
    // ------------------------------------------------------------------

    #[test]
    fn branching_callee_reconverges_at_continuation() {
        let mut m = empty_module();
        let u64_ty = m.types.primitive(Type::_64);

        m.sigs.push(SigDecl {
            params: vec![],
            results: vec![u64_ty],
        });
        m.sigs.push(SigDecl {
            params: vec![u64_ty],
            results: vec![u64_ty],
        });

        // Callee (FuncId 1): if (param) { return 1 } else { return 2 }
        let callee = FuncBody {
            sig: vaffle::SigId(1),
            blocks: vec![
                Block {
                    params: vec![(ValueId(0), u64_ty)],
                    stmts: vec![],
                    terminator: Terminator::IfNonzero {
                        cond: ValueId(0),
                        then_target: Target {
                            block: BlockId(1),
                            args: vec![],
                            reentry: None,
                        },
                        else_target: Target {
                            block: BlockId(2),
                            args: vec![],
                            reentry: None,
                        },
                    },
                },
                Block {
                    params: vec![],
                    stmts: vec![ValueId(1)],
                    terminator: Terminator::Return {
                        values: vec![ValueId(1)],
                    },
                },
                Block {
                    params: vec![],
                    stmts: vec![ValueId(2)],
                    terminator: Terminator::Return {
                        values: vec![ValueId(2)],
                    },
                },
            ],
            values: vec![
                Node::new(
                    Value::Param {
                        block: BlockId(0),
                        ty: u64_ty,
                        idx: 0,
                    },
                    (),
                    None,
                ),
                const_node(1, u64_ty),
                const_node(2, u64_ty),
            ],
            entry: BlockId(0),
        };

        let v0 = ValueId(0);
        let v1 = ValueId(1);
        let v2 = ValueId(2);
        let caller = FuncBody {
            sig: vaffle::SigId(0),
            blocks: vec![Block {
                params: vec![],
                stmts: vec![v0, v1, v2],
                terminator: Terminator::Return { values: vec![v2] },
            }],
            values: vec![
                const_node(1, u64_ty),
                Node::new(
                    Value::Call {
                        func: FuncId(1),
                        args: vec![v0],
                    },
                    (),
                    None,
                ),
                Node::new(Value::Output { value: v1, idx: 0 }, (), None),
            ],
            entry: BlockId(0),
        };

        m.funcs.push(FuncDecl::Body(caller));
        m.funcs.push(FuncDecl::Body(callee));

        let n = inline_vaffle_module(&mut m, generous_budget());
        assert_eq!(n, 1);

        let FuncDecl::Body(caller) = &m.funcs[0] else {
            panic!("expected FuncDecl::Body")
        };
        // 1 original block + 3 spliced callee blocks + 1 continuation = 5.
        assert_eq!(caller.blocks.len(), 5);

        // The continuation block has exactly one param.
        let cont_id = caller.blocks.len() - 1;
        assert_eq!(caller.blocks[cont_id].params.len(), 1);

        // Both remapped callee exit blocks (indices 2 and 3: entry=1,
        // then=2, else=3) now jump into the continuation block.
        let mut jumps_to_cont = 0;
        for b in &caller.blocks[1..cont_id] {
            if let Terminator::Jump(Target { block, .. }) = &b.terminator {
                if block.0 == cont_id {
                    jumps_to_cont += 1;
                }
            }
        }
        assert_eq!(
            jumps_to_cont, 2,
            "both callee exits should reconverge at the continuation"
        );
    }

    // ------------------------------------------------------------------
    // 3. Self-recursive callee is never inlined
    // ------------------------------------------------------------------

    #[test]
    fn self_recursive_callee_is_never_inlined() {
        let mut m = empty_module();
        let u64_ty = m.types.primitive(Type::_64);

        m.sigs.push(SigDecl {
            params: vec![],
            results: vec![u64_ty],
        });
        m.sigs.push(SigDecl {
            params: vec![u64_ty],
            results: vec![u64_ty],
        });

        // Callee (FuncId 1): calls itself, then returns a constant (never
        // actually reached at eval time -- structure only matters here).
        let callee = FuncBody {
            sig: vaffle::SigId(1),
            blocks: vec![Block {
                params: vec![(ValueId(0), u64_ty)],
                stmts: vec![ValueId(1)],
                terminator: Terminator::Return {
                    values: vec![ValueId(0)],
                },
            }],
            values: vec![
                Node::new(
                    Value::Param {
                        block: BlockId(0),
                        ty: u64_ty,
                        idx: 0,
                    },
                    (),
                    None,
                ),
                Node::new(
                    Value::Call {
                        func: FuncId(1),
                        args: vec![ValueId(0)],
                    },
                    (),
                    None,
                ),
            ],
            entry: BlockId(0),
        };

        let v0 = ValueId(0);
        let v1 = ValueId(1);
        let v2 = ValueId(2);
        let caller = FuncBody {
            sig: vaffle::SigId(0),
            blocks: vec![Block {
                params: vec![],
                stmts: vec![v0, v1, v2],
                terminator: Terminator::Return { values: vec![v2] },
            }],
            values: vec![
                const_node(1, u64_ty),
                Node::new(
                    Value::Call {
                        func: FuncId(1),
                        args: vec![v0],
                    },
                    (),
                    None,
                ),
                Node::new(Value::Output { value: v1, idx: 0 }, (), None),
            ],
            entry: BlockId(0),
        };

        m.funcs.push(FuncDecl::Body(caller));
        m.funcs.push(FuncDecl::Body(callee));

        let n = inline_vaffle_module(&mut m, generous_budget());
        assert_eq!(n, 0, "a recursive callee must never be inlined");

        let FuncDecl::Body(caller) = &m.funcs[0] else {
            panic!("expected FuncDecl::Body")
        };
        assert!(
            has_live_call_to(caller, FuncId(1)),
            "the call site should be left untouched"
        );
        let FuncDecl::Body(callee) = &m.funcs[1] else {
            panic!("expected FuncDecl::Body")
        };
        assert!(
            has_live_call_to(callee, FuncId(1)),
            "the callee's own self-call should be left untouched"
        );
    }

    // ------------------------------------------------------------------
    // 4. Mutually-recursive pair is never inlined
    // ------------------------------------------------------------------

    #[test]
    fn mutually_recursive_pair_is_never_inlined() {
        let mut m = empty_module();
        let u64_ty = m.types.primitive(Type::_64);

        m.sigs.push(SigDecl {
            params: vec![u64_ty],
            results: vec![u64_ty],
        });

        // FuncId 0 calls FuncId 1; FuncId 1 calls FuncId 0.
        let func0 = FuncBody {
            sig: vaffle::SigId(0),
            blocks: vec![Block {
                params: vec![(ValueId(0), u64_ty)],
                stmts: vec![ValueId(1), ValueId(2)],
                terminator: Terminator::Return {
                    values: vec![ValueId(2)],
                },
            }],
            values: vec![
                Node::new(
                    Value::Param {
                        block: BlockId(0),
                        ty: u64_ty,
                        idx: 0,
                    },
                    (),
                    None,
                ),
                Node::new(
                    Value::Call {
                        func: FuncId(1),
                        args: vec![ValueId(0)],
                    },
                    (),
                    None,
                ),
                Node::new(
                    Value::Output {
                        value: ValueId(1),
                        idx: 0,
                    },
                    (),
                    None,
                ),
            ],
            entry: BlockId(0),
        };
        let func1 = FuncBody {
            sig: vaffle::SigId(0),
            blocks: vec![Block {
                params: vec![(ValueId(0), u64_ty)],
                stmts: vec![ValueId(1), ValueId(2)],
                terminator: Terminator::Return {
                    values: vec![ValueId(2)],
                },
            }],
            values: vec![
                Node::new(
                    Value::Param {
                        block: BlockId(0),
                        ty: u64_ty,
                        idx: 0,
                    },
                    (),
                    None,
                ),
                Node::new(
                    Value::Call {
                        func: FuncId(0),
                        args: vec![ValueId(0)],
                    },
                    (),
                    None,
                ),
                Node::new(
                    Value::Output {
                        value: ValueId(1),
                        idx: 0,
                    },
                    (),
                    None,
                ),
            ],
            entry: BlockId(0),
        };

        m.funcs.push(FuncDecl::Body(func0));
        m.funcs.push(FuncDecl::Body(func1));

        let n = inline_vaffle_module(&mut m, generous_budget());
        assert_eq!(
            n, 0,
            "mutually-recursive functions must never be inlined into each other"
        );

        let FuncDecl::Body(func0) = &m.funcs[0] else {
            panic!()
        };
        let FuncDecl::Body(func1) = &m.funcs[1] else {
            panic!()
        };
        assert!(has_live_call_to(func0, FuncId(1)));
        assert!(has_live_call_to(func1, FuncId(0)));
    }

    // ------------------------------------------------------------------
    // 5. Budget exhaustion
    // ------------------------------------------------------------------

    #[test]
    fn budget_exhaustion_stops_after_first_call_site() {
        let mut m = empty_module();
        let u64_ty = m.types.primitive(Type::_64);

        m.sigs.push(SigDecl {
            params: vec![],
            results: vec![u64_ty],
        });
        m.sigs.push(SigDecl {
            params: vec![u64_ty],
            results: vec![u64_ty],
        });

        // Callee (FuncId 1): identity, 1 Value in its body.
        let callee = FuncBody {
            sig: vaffle::SigId(1),
            blocks: vec![Block {
                params: vec![(ValueId(0), u64_ty)],
                stmts: vec![],
                terminator: Terminator::Return {
                    values: vec![ValueId(0)],
                },
            }],
            values: vec![Node::new(
                Value::Param {
                    block: BlockId(0),
                    ty: u64_ty,
                    idx: 0,
                },
                (),
                None,
            )],
            entry: BlockId(0),
        };

        // Caller calls the callee twice in sequence, chaining the result.
        let v0 = ValueId(0); // const
        let v1 = ValueId(1); // call 1
        let v2 = ValueId(2); // output of call 1
        let v3 = ValueId(3); // call 2, using v2 as arg
        let v4 = ValueId(4); // output of call 2
        let caller = FuncBody {
            sig: vaffle::SigId(0),
            blocks: vec![Block {
                params: vec![],
                stmts: vec![v0, v1, v2, v3, v4],
                terminator: Terminator::Return { values: vec![v4] },
            }],
            values: vec![
                const_node(1, u64_ty),
                Node::new(
                    Value::Call {
                        func: FuncId(1),
                        args: vec![v0],
                    },
                    (),
                    None,
                ),
                Node::new(Value::Output { value: v1, idx: 0 }, (), None),
                Node::new(
                    Value::Call {
                        func: FuncId(1),
                        args: vec![v2],
                    },
                    (),
                    None,
                ),
                Node::new(Value::Output { value: v3, idx: 0 }, (), None),
            ],
            entry: BlockId(0),
        };

        m.funcs.push(FuncDecl::Body(caller));
        m.funcs.push(FuncDecl::Body(callee));

        // Exactly enough budget for one inlining (callee has 1 Value), not two.
        let budget = InlineBudget {
            max_callee_values: 64,
            total_budget: 1,
        };
        let n = inline_vaffle_module(&mut m, budget);
        assert_eq!(n, 1, "only the first call site should fit the budget");

        let FuncDecl::Body(caller) = &m.funcs[0] else {
            panic!()
        };
        // The second call (originally v3) must still be a real Value::Call.
        match &caller.values[v3.0].kind {
            Value::Call { func, .. } => assert_eq!(*func, FuncId(1)),
            other => panic!("expected the second call site to remain a Value::Call, got {other:?}"),
        }
        // The first call site (v1) is no longer *live*: it's not listed in
        // any block's stmts anymore (its old arena entry is left in place,
        // unreferenced -- see the module doc). v2, its Output, was
        // repurposed in place as the continuation block's param.
        assert!(
            caller.blocks.iter().all(|b| !b.stmts.contains(&v1)),
            "the first call's own ValueId should no longer be listed as a live statement"
        );
        match &caller.values[v2.0].kind {
            Value::Param { idx: 0, .. } => {}
            other => panic!("expected v2 to become a Value::Param, got {other:?}"),
        }
    }

    // ------------------------------------------------------------------
    // 6. Multi-result callee
    // ------------------------------------------------------------------

    #[test]
    fn multi_result_callee_rewires_both_outputs() {
        let mut m = empty_module();
        let u64_ty = m.types.primitive(Type::_64);

        m.sigs.push(SigDecl {
            params: vec![],
            results: vec![u64_ty],
        });
        m.sigs.push(SigDecl {
            params: vec![],
            results: vec![u64_ty, u64_ty],
        });

        // Callee (FuncId 1): () -> (1, 2)
        let callee = FuncBody {
            sig: vaffle::SigId(1),
            blocks: vec![Block {
                params: vec![],
                stmts: vec![ValueId(0), ValueId(1)],
                terminator: Terminator::Return {
                    values: vec![ValueId(0), ValueId(1)],
                },
            }],
            values: vec![const_node(1, u64_ty), const_node(2, u64_ty)],
            entry: BlockId(0),
        };

        let v0 = ValueId(0); // call
        let v1 = ValueId(1); // output idx 0
        let v2 = ValueId(2); // output idx 1
        let caller = FuncBody {
            sig: vaffle::SigId(0),
            blocks: vec![Block {
                params: vec![],
                stmts: vec![v0, v1, v2],
                terminator: Terminator::Return { values: vec![v1] },
            }],
            values: vec![
                Node::new(
                    Value::Call {
                        func: FuncId(1),
                        args: vec![],
                    },
                    (),
                    None,
                ),
                Node::new(Value::Output { value: v0, idx: 0 }, (), None),
                Node::new(Value::Output { value: v0, idx: 1 }, (), None),
            ],
            entry: BlockId(0),
        };

        m.funcs.push(FuncDecl::Body(caller));
        m.funcs.push(FuncDecl::Body(callee));

        let n = inline_vaffle_module(&mut m, generous_budget());
        assert_eq!(n, 1);

        let FuncDecl::Body(caller) = &m.funcs[0] else {
            panic!()
        };
        match &caller.values[v1.0].kind {
            Value::Param { idx: 0, .. } => {}
            other => panic!("expected v1 -> Value::Param{{idx:0}}, got {other:?}"),
        }
        match &caller.values[v2.0].kind {
            Value::Param { idx: 1, .. } => {}
            other => panic!("expected v2 -> Value::Param{{idx:1}}, got {other:?}"),
        }
        // Both params should belong to the same (continuation) block.
        let (Value::Param { block: b1, .. }, Value::Param { block: b2, .. }) =
            (&caller.values[v1.0].kind, &caller.values[v2.0].kind)
        else {
            unreachable!()
        };
        assert_eq!(b1, b2);
    }

    fn has_live_return_call_to(body: &FuncBody, callee: FuncId) -> bool {
        body.blocks.iter().any(|b| {
            matches!(&b.terminator, Terminator::ReturnCall { func, .. } if *func == callee)
        })
    }

    #[test]
    fn everything_inlines_call_and_dces_callee() {
        let mut m = empty_module();
        let u64_ty = m.types.primitive(Type::_64);

        m.sigs.push(SigDecl {
            params: vec![],
            results: vec![u64_ty],
        });
        m.sigs.push(SigDecl {
            params: vec![u64_ty],
            results: vec![u64_ty],
        });

        let callee = FuncBody {
            sig: vaffle::SigId(1),
            blocks: vec![Block {
                params: vec![(ValueId(0), u64_ty)],
                stmts: vec![],
                terminator: Terminator::Return {
                    values: vec![ValueId(0)],
                },
            }],
            values: vec![Node::new(
                Value::Param {
                    block: BlockId(0),
                    ty: u64_ty,
                    idx: 0,
                },
                (),
                None,
            )],
            entry: BlockId(0),
        };

        let v0 = ValueId(0);
        let v1 = ValueId(1);
        let v2 = ValueId(2);
        let caller = FuncBody {
            sig: vaffle::SigId(0),
            blocks: vec![Block {
                params: vec![],
                stmts: vec![v0, v1, v2],
                terminator: Terminator::Return { values: vec![v2] },
            }],
            values: vec![
                const_node(5, u64_ty),
                Node::new(
                    Value::Call {
                        func: FuncId(1),
                        args: vec![v0],
                    },
                    (),
                    None,
                ),
                Node::new(Value::Output { value: v1, idx: 0 }, (), None),
            ],
            entry: BlockId(0),
        };

        m.funcs.push(FuncDecl::Body(caller));
        m.funcs.push(FuncDecl::Body(callee));
        m.exports
            .insert(alloc::string::String::from("caller"), FuncId(0));

        let n = inline_vaffle_everything(&mut m, &[FuncId(0)]).expect("should fully inline");
        assert_eq!(n, 1);
        let FuncDecl::Body(caller) = &m.funcs[0] else {
            panic!("expected FuncDecl::Body")
        };
        assert!(
            !has_live_call_to(caller, FuncId(1)),
            "no live Body-to-Body call should remain"
        );
        assert!(!caller.blocks.iter().any(|b| {
            matches!(&b.terminator, Terminator::ReturnCall { .. })
        }));
    }

    #[test]
    fn everything_splices_return_call() {
        let mut m = empty_module();
        let u64_ty = m.types.primitive(Type::_64);

        m.sigs.push(SigDecl {
            params: vec![],
            results: vec![u64_ty],
        });
        m.sigs.push(SigDecl {
            params: vec![u64_ty],
            results: vec![u64_ty],
        });

        let callee = FuncBody {
            sig: vaffle::SigId(1),
            blocks: vec![Block {
                params: vec![(ValueId(0), u64_ty)],
                stmts: vec![],
                terminator: Terminator::Return {
                    values: vec![ValueId(0)],
                },
            }],
            values: vec![Node::new(
                Value::Param {
                    block: BlockId(0),
                    ty: u64_ty,
                    idx: 0,
                },
                (),
                None,
            )],
            entry: BlockId(0),
        };

        let v0 = ValueId(0);
        let caller = FuncBody {
            sig: vaffle::SigId(0),
            blocks: vec![Block {
                params: vec![],
                stmts: vec![v0],
                terminator: Terminator::ReturnCall {
                    func: FuncId(1),
                    args: vec![v0],
                },
            }],
            values: vec![const_node(7, u64_ty)],
            entry: BlockId(0),
        };

        m.funcs.push(FuncDecl::Body(caller));
        m.funcs.push(FuncDecl::Body(callee));

        let n = inline_vaffle_everything(&mut m, &[FuncId(0)]).expect("should inline ReturnCall");
        assert_eq!(n, 1);
        let FuncDecl::Body(caller) = &m.funcs[0] else {
            panic!("expected FuncDecl::Body")
        };
        assert!(!has_live_return_call_to(caller, FuncId(1)));
        assert!(
            caller
                .blocks
                .iter()
                .any(|b| matches!(&b.terminator, Terminator::Jump(_))),
            "ReturnCall should become a jump into the spliced callee"
        );
    }

    #[test]
    fn everything_leaves_import_calls() {
        let mut m = empty_module();
        let u64_ty = m.types.primitive(Type::_64);

        m.sigs.push(SigDecl {
            params: vec![],
            results: vec![u64_ty],
        });
        m.sigs.push(SigDecl {
            params: vec![u64_ty],
            results: vec![u64_ty],
        });

        let v0 = ValueId(0);
        let v1 = ValueId(1);
        let v2 = ValueId(2);
        let caller = FuncBody {
            sig: vaffle::SigId(0),
            blocks: vec![Block {
                params: vec![],
                stmts: vec![v0, v1, v2],
                terminator: Terminator::Return { values: vec![v2] },
            }],
            values: vec![
                const_node(1, u64_ty),
                Node::new(
                    Value::Call {
                        func: FuncId(1),
                        args: vec![v0],
                    },
                    (),
                    None,
                ),
                Node::new(Value::Output { value: v1, idx: 0 }, (), None),
            ],
            entry: BlockId(0),
        };

        m.funcs.push(FuncDecl::Body(caller));
        m.funcs.push(FuncDecl::Import {
            module: alloc::string::String::from("env"),
            name: alloc::string::String::from("oracle"),
            sig: vaffle::SigId(1),
        });

        let n = inline_vaffle_everything(&mut m, &[FuncId(0)]).expect("imports are allowed");
        assert_eq!(n, 0);
        assert_eq!(m.funcs.len(), 2);
        let FuncDecl::Body(caller) = &m.funcs[0] else {
            panic!()
        };
        assert!(has_live_call_to(caller, FuncId(1)));
    }

    #[test]
    fn everything_fails_closed_on_recursion() {
        let mut m = empty_module();
        let u64_ty = m.types.primitive(Type::_64);

        m.sigs.push(SigDecl {
            params: vec![],
            results: vec![u64_ty],
        });
        m.sigs.push(SigDecl {
            params: vec![u64_ty],
            results: vec![u64_ty],
        });

        let callee = FuncBody {
            sig: vaffle::SigId(1),
            blocks: vec![Block {
                params: vec![(ValueId(0), u64_ty)],
                stmts: vec![ValueId(1)],
                terminator: Terminator::Return {
                    values: vec![ValueId(0)],
                },
            }],
            values: vec![
                Node::new(
                    Value::Param {
                        block: BlockId(0),
                        ty: u64_ty,
                        idx: 0,
                    },
                    (),
                    None,
                ),
                Node::new(
                    Value::Call {
                        func: FuncId(1),
                        args: vec![ValueId(0)],
                    },
                    (),
                    None,
                ),
            ],
            entry: BlockId(0),
        };

        let v0 = ValueId(0);
        let v1 = ValueId(1);
        let v2 = ValueId(2);
        let caller = FuncBody {
            sig: vaffle::SigId(0),
            blocks: vec![Block {
                params: vec![],
                stmts: vec![v0, v1, v2],
                terminator: Terminator::Return { values: vec![v2] },
            }],
            values: vec![
                const_node(1, u64_ty),
                Node::new(
                    Value::Call {
                        func: FuncId(1),
                        args: vec![v0],
                    },
                    (),
                    None,
                ),
                Node::new(Value::Output { value: v1, idx: 0 }, (), None),
            ],
            entry: BlockId(0),
        };

        m.funcs.push(FuncDecl::Body(caller));
        m.funcs.push(FuncDecl::Body(callee));

        let err = inline_vaffle_everything(&mut m, &[FuncId(0)]).expect_err("recursion");
        match err {
            InlineEverythingError::Recursive { funcs } => {
                assert!(funcs.contains(&FuncId(1)));
            }
            other => panic!("expected Recursive, got {other:?}"),
        }
    }

    #[test]
    fn everything_unknown_entry() {
        let mut m = empty_module();
        let err = inline_vaffle_everything(&mut m, &[FuncId(0)]).expect_err("missing entry");
        assert_eq!(
            err,
            InlineEverythingError::UnknownEntry { entry: FuncId(0) }
        );
    }
}
