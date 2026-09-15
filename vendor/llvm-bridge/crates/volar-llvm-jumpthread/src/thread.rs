//! Discovery (Phase A, pure Rust — no LLVM mutation) and materialization
//! (Phase B — builds real LLVM IR) for one candidate loop.
//!
//! See `lib.rs` for the algorithm overview. Every bail point here returns
//! `None`/`false` and leaves the module completely untouched; only a
//! successful, fully-discovered plan ever reaches `materialize`.

use std::collections::{HashMap, HashSet};

use inkwell::basic_block::BasicBlock;
use inkwell::llvm_sys::core::{
    LLVMGetPoison, LLVMGetSwitchCaseValue, LLVMReplaceAllUsesWith, LLVMTypeOf,
};
use inkwell::values::{
    AnyValueEnum, AsValueRef, BasicValue, BasicValueEnum, FunctionValue, InstructionOpcode,
    InstructionValue, Operand, PhiValue,
};

use crate::JumpThreadLimits;
use crate::cfg;
use crate::eval;

/// One discriminator-tuple value: the concrete value of every discriminator
/// phi, in `disc_phis` order. Doubles as the DFA state and the memoization
/// key for `(header, state)` convergence.
pub type State = Vec<u64>;

/// What a value resolves to along one trace.
#[derive(Clone)]
enum Resolved<'ctx> {
    /// Provably this compile-time constant, for this trace.
    Concrete(u64),
    /// Resolves to whatever `InstructionValue` becomes once cloned — always
    /// an instruction already recorded in the *same* node's `to_clone` list.
    Cloned(InstructionValue<'ctx>),
    /// A value from outside the loop (function arg, global, or an
    /// instruction defined outside the loop body), or a plain LLVM
    /// constant — used as-is, never rewritten.
    External(BasicValueEnum<'ctx>),
    /// Resolves to node `.0`'s own real header phi for `carried_phis[.1]`,
    /// built up front in `materialize` before any node body is filled in.
    HeaderCarried(usize, usize),
}

enum TraceExit<'ctx> {
    /// Back edge: transitions to `next_state` (a new or already-discovered
    /// node), carrying `carried` values (one per `carried_phis` entry).
    Loop {
        next_state: State,
        carried: Vec<Resolved<'ctx>>,
    },
    /// Leaves the loop to `target` from in-loop block `from`.
    Exit {
        target: BasicBlock<'ctx>,
        from: BasicBlock<'ctx>,
    },
    /// `ret`/`unreachable` inside the loop body: the trace simply ends here.
    Terminal { terminator: InstructionValue<'ctx> },
}

struct NodeBody<'ctx> {
    to_clone: Vec<InstructionValue<'ctx>>,
    env: HashMap<InstructionValue<'ctx>, Resolved<'ctx>>,
    exit: TraceExit<'ctx>,
}

/// A contribution to some node's (or exit block's) header/exit phis.
enum ContribSource<'ctx> {
    Outside(BasicBlock<'ctx>),
    Node(usize),
}

struct Contribution<'ctx> {
    source: ContribSource<'ctx>,
    /// Resolved in the *source*'s own environment (empty env + `External`
    /// bindings for an `Outside` source, since it has no trace of its own).
    carried: Vec<Resolved<'ctx>>,
}

struct ExitContribution<'ctx> {
    node_id: usize,
    from: BasicBlock<'ctx>,
}

pub struct LoopPlan<'ctx> {
    header: BasicBlock<'ctx>,
    carried_phis: Vec<PhiValue<'ctx>>,
    body: HashSet<BasicBlock<'ctx>>,
    /// Indexed by `node_id` (see `node_index`) — *not* walk-completion
    /// order, which the worklist's LIFO processing can reorder relative to
    /// discovery order. `None` only transiently, between a node being
    /// discovered (`get_or_create_node`) and its walk completing.
    nodes: Vec<Option<NodeBody<'ctx>>>,
    node_index: HashMap<State, usize>,
    /// Contributions to each node's header (its own `carried_phis`), by node id.
    node_contributions: HashMap<usize, Vec<Contribution<'ctx>>>,
    /// Original outside-of-loop predecessors of `header`, each redirected to
    /// its entry node's specialized block.
    entry_edges: Vec<(BasicBlock<'ctx>, usize)>,
    /// Exit blocks touched, each with the specialized nodes that branch to it.
    exits: HashMap<BasicBlock<'ctx>, Vec<ExitContribution<'ctx>>>,
}

// ============================================================================
// Phase A: discovery
// ============================================================================

pub fn plan_loop<'ctx>(
    header: BasicBlock<'ctx>,
    latch: BasicBlock<'ctx>,
    body: &HashSet<BasicBlock<'ctx>>,
    successors: &HashMap<BasicBlock<'ctx>, Vec<BasicBlock<'ctx>>>,
    predecessors: &HashMap<BasicBlock<'ctx>, Vec<BasicBlock<'ctx>>>,
    limits: &JumpThreadLimits,
) -> Option<LoopPlan<'ctx>> {
    let _ = latch; // identified structurally via back-edge detection during the walk.

    let outside_preds: Vec<BasicBlock<'ctx>> = predecessors
        .get(&header)
        .into_iter()
        .flatten()
        .copied()
        .filter(|p| !body.contains(p))
        .collect();
    if outside_preds.is_empty() {
        return None;
    }
    // Every outside predecessor must be a clean, unconditional preheader-style
    // edge, so entry redirection never has to partially patch a terminator.
    for &pred in &outside_preds {
        let terminator = pred.get_terminator()?;
        if terminator.get_num_operands() != 1 {
            return None;
        }
    }

    let (disc_phis, carried_phis) = classify_header_phis(header, &outside_preds)?;
    if disc_phis.is_empty() {
        return None;
    }

    if !external_uses_are_legal(body, successors) {
        return None;
    }

    let mut plan = LoopPlan {
        header,
        carried_phis: carried_phis.clone(),
        body: body.clone(),
        nodes: Vec::new(),
        node_index: HashMap::new(),
        node_contributions: HashMap::new(),
        entry_edges: Vec::new(),
        exits: HashMap::new(),
    };

    let mut worklist: Vec<State> = Vec::new();

    for &pred in &outside_preds {
        let mut state = Vec::with_capacity(disc_phis.len());
        for p in &disc_phis {
            let incoming = find_incoming(*p, pred)?;
            state.push(int_const_value(incoming)?);
        }
        let node_id = get_or_create_node(&mut plan, &state, &mut worklist, limits)?;
        plan.entry_edges.push((pred, node_id));
        let carried: Vec<Resolved<'ctx>> = carried_phis
            .iter()
            .map(|p| find_incoming(*p, pred).map(Resolved::External))
            .collect::<Option<_>>()?;
        plan.node_contributions
            .entry(node_id)
            .or_default()
            .push(Contribution {
                source: ContribSource::Outside(pred),
                carried,
            });
    }

    while let Some(state) = worklist.pop() {
        let node_id = *plan.node_index.get(&state)?;
        if plan.nodes[node_id].is_some() {
            continue; // already walked (queued twice — harmless).
        }
        let body_result = walk_node(header, body, &disc_phis, &carried_phis, &state, node_id)?;
        match &body_result.exit {
            TraceExit::Loop {
                next_state,
                carried,
            } => {
                let target_id = get_or_create_node(&mut plan, next_state, &mut worklist, limits)?;
                plan.node_contributions
                    .entry(target_id)
                    .or_default()
                    .push(Contribution {
                        source: ContribSource::Node(node_id),
                        carried: carried.clone(),
                    });
            }
            TraceExit::Exit { target, from } => {
                plan.exits
                    .entry(*target)
                    .or_default()
                    .push(ExitContribution {
                        node_id,
                        from: *from,
                    });
            }
            TraceExit::Terminal { .. } => {}
        }
        plan.nodes[node_id] = Some(body_result);
    }

    Some(plan)
}

/// Look up `state`'s node id, creating it (and growing `plan.nodes` in
/// lock-step, keeping indices aligned) if this is the first time it's been
/// reached. Newly-created ids are queued for walking; ids already present
/// (whether walked yet or not) are returned without re-queueing.
fn get_or_create_node<'ctx>(
    plan: &mut LoopPlan<'ctx>,
    state: &State,
    worklist: &mut Vec<State>,
    limits: &JumpThreadLimits,
) -> Option<usize> {
    if let Some(&id) = plan.node_index.get(state) {
        return Some(id);
    }
    if plan.node_index.len() >= limits.max_states {
        return None; // ceiling exceeded: bail the whole loop, not just this state.
    }
    let id = plan.node_index.len();
    plan.node_index.insert(state.clone(), id);
    plan.nodes.push(None);
    worklist.push(state.clone());
    Some(id)
}

/// Classify `header`'s phis into discriminator (all outside-incoming edges
/// are literal integer constants) vs. carried (everything else — including
/// non-integer types, which can never fold).
fn classify_header_phis<'ctx>(
    header: BasicBlock<'ctx>,
    outside_preds: &[BasicBlock<'ctx>],
) -> Option<(Vec<PhiValue<'ctx>>, Vec<PhiValue<'ctx>>)> {
    let mut all_phis = Vec::new();
    for instr in header.get_instructions() {
        if instr.get_opcode() != InstructionOpcode::Phi {
            break;
        }
        all_phis.push(PhiValue::try_from(instr).ok()?);
    }
    if all_phis.is_empty() {
        return None;
    }

    let mut disc = Vec::new();
    let mut carried = Vec::new();
    for phi in all_phis {
        let mut is_disc = true;
        for &pred in outside_preds {
            let Some(incoming) = find_incoming(phi, pred) else {
                return None; // malformed SSA: every predecessor must have an entry.
            };
            if int_const_value(incoming).is_none() {
                is_disc = false;
                break;
            }
        }
        if is_disc {
            disc.push(phi);
        } else {
            carried.push(phi);
        }
    }
    Some((disc, carried))
}

/// Every use of a value defined inside `body` must be either another
/// in-body instruction, or a phi at a direct (static) successor of some
/// in-body block — an LCSSA-style exit phi. Anything else makes deleting
/// the original loop body unsafe.
fn external_uses_are_legal<'ctx>(
    body: &HashSet<BasicBlock<'ctx>>,
    successors: &HashMap<BasicBlock<'ctx>, Vec<BasicBlock<'ctx>>>,
) -> bool {
    let static_exits: HashSet<BasicBlock<'ctx>> = body
        .iter()
        .flat_map(|b| successors.get(b).into_iter().flatten())
        .copied()
        .filter(|s| !body.contains(s))
        .collect();

    for &block in body {
        for instr in block.get_instructions() {
            let mut use_ = instr.get_first_use();
            while let Some(u) = use_ {
                let Some(user) = user_instruction(u.get_user()) else {
                    return false;
                };
                let Some(parent) = user.get_parent() else {
                    return false;
                };
                let ok = if body.contains(&parent) {
                    true
                } else if static_exits.contains(&parent) {
                    user.get_opcode() == InstructionOpcode::Phi
                } else {
                    false
                };
                if !ok {
                    return false;
                }
                use_ = u.get_next_use();
            }
        }
    }
    true
}

/// `AnyValueEnum::new`'s dispatch classifies by the value's *result type*
/// (integer, float, pointer, ...), not by "is this an instruction" — a
/// `phi i32` user comes back as `AnyValueEnum::IntValue`, never
/// `AnyValueEnum::PhiValue` (that variant is effectively never constructed
/// by the generic path). So every non-`InstructionValue`/`PhiValue` variant
/// must still be checked via its own `BasicValue::as_instruction_value()`.
fn user_instruction<'ctx>(user: AnyValueEnum<'ctx>) -> Option<InstructionValue<'ctx>> {
    match user {
        AnyValueEnum::InstructionValue(i) => Some(i),
        AnyValueEnum::PhiValue(p) => Some(p.as_instruction()),
        AnyValueEnum::ArrayValue(v) => v.as_instruction_value(),
        AnyValueEnum::IntValue(v) => v.as_instruction_value(),
        AnyValueEnum::FloatValue(v) => v.as_instruction_value(),
        AnyValueEnum::PointerValue(v) => v.as_instruction_value(),
        AnyValueEnum::StructValue(v) => v.as_instruction_value(),
        AnyValueEnum::VectorValue(v) => v.as_instruction_value(),
        AnyValueEnum::ScalableVectorValue(v) => v.as_instruction_value(),
        _ => None,
    }
}

fn find_incoming<'ctx>(
    phi: PhiValue<'ctx>,
    pred: BasicBlock<'ctx>,
) -> Option<BasicValueEnum<'ctx>> {
    for i in 0..phi.count_incoming() {
        let (value, block) = phi.get_incoming(i)?;
        if block == pred {
            return Some(value);
        }
    }
    None
}

fn int_const_value(v: BasicValueEnum<'_>) -> Option<u64> {
    match v {
        BasicValueEnum::IntValue(iv) if iv.is_const() => iv.get_zero_extended_constant(),
        _ => None,
    }
}

fn resolve_operand<'ctx>(
    v: BasicValueEnum<'ctx>,
    env: &HashMap<InstructionValue<'ctx>, Resolved<'ctx>>,
) -> Resolved<'ctx> {
    if let Some(instr) = v.as_instruction_value() {
        if let Some(r) = env.get(&instr) {
            return r.clone();
        }
    }
    Resolved::External(v)
}

fn resolved_to_u64(r: &Resolved<'_>) -> Option<u64> {
    match r {
        Resolved::Concrete(v) => Some(*v),
        Resolved::External(v) => int_const_value(*v),
        _ => None,
    }
}

/// `switch`'s case *values* aren't tracked LLVM operands (unlike its
/// condition and successor blocks, which are) — `LLVMGetOperand`/
/// `LLVMGetNumOperands` on a switch only ever see `[cond, successor_0,
/// successor_1, ...]` where `successor_0` is the default destination.
/// `LLVMGetSwitchCaseValue(sw, i)` is the dedicated accessor for the value
/// belonging to successor `i` (`i` ranges over the same successor indices —
/// `i == 0` is the default and has no case value, so this starts at 1).
fn switch_target<'ctx>(
    terminator: InstructionValue<'ctx>,
    cond_val: u64,
) -> Option<BasicBlock<'ctx>> {
    let n = terminator.get_num_operands();
    let default = terminator.get_operand(1).and_then(Operand::block)?;
    for i in 2..n {
        let case_block = terminator.get_operand(i).and_then(Operand::block)?;
        let successor_index = i - 1;
        let case_value = unsafe {
            inkwell::values::IntValue::new(LLVMGetSwitchCaseValue(
                terminator.as_value_ref(),
                successor_index,
            ))
        };
        if case_value.is_const() && case_value.get_zero_extended_constant() == Some(cond_val) {
            return Some(case_block);
        }
    }
    Some(default)
}

enum StepResult<'ctx> {
    Continue(BasicBlock<'ctx>),
    Done(TraceExit<'ctx>),
}

fn classify_next<'ctx>(
    header: BasicBlock<'ctx>,
    body: &HashSet<BasicBlock<'ctx>>,
    from: BasicBlock<'ctx>,
    next: BasicBlock<'ctx>,
) -> ClassifiedNext<'ctx> {
    if next == header {
        ClassifiedNext::BackEdge
    } else if body.contains(&next) {
        ClassifiedNext::Continue(next)
    } else {
        ClassifiedNext::Exit(from, next)
    }
}

enum ClassifiedNext<'ctx> {
    Continue(BasicBlock<'ctx>),
    BackEdge,
    Exit(BasicBlock<'ctx>, BasicBlock<'ctx>),
}

fn walk_node<'ctx>(
    header: BasicBlock<'ctx>,
    body: &HashSet<BasicBlock<'ctx>>,
    disc_phis: &[PhiValue<'ctx>],
    carried_phis: &[PhiValue<'ctx>],
    state: &State,
    node_id: usize,
) -> Option<NodeBody<'ctx>> {
    let mut env: HashMap<InstructionValue<'ctx>, Resolved<'ctx>> = HashMap::new();
    for (i, p) in disc_phis.iter().enumerate() {
        env.insert(p.as_instruction(), Resolved::Concrete(state[i]));
    }
    for (i, p) in carried_phis.iter().enumerate() {
        env.insert(p.as_instruction(), Resolved::HeaderCarried(node_id, i));
    }

    let mut to_clone = Vec::new();
    let mut current = header;
    let mut prev: Option<BasicBlock<'ctx>> = None;

    loop {
        if let Some(from) = prev {
            for instr in current.get_instructions() {
                if instr.get_opcode() != InstructionOpcode::Phi {
                    break;
                }
                let phi = PhiValue::try_from(instr).ok()?;
                let incoming = find_incoming(phi, from)?;
                let resolved = resolve_operand(incoming, &env);
                env.insert(instr, resolved);
            }
        }

        let terminator = current.get_terminator()?;
        for instr in current.get_instructions() {
            if instr == terminator {
                break;
            }
            if instr.get_opcode() == InstructionOpcode::Phi {
                continue;
            }
            let folded =
                eval::fold_instruction(instr, &|v| resolved_to_u64(&resolve_operand(v, &env)));
            match folded {
                Some(n) => {
                    env.insert(instr, Resolved::Concrete(n));
                }
                None => {
                    env.insert(instr, Resolved::Cloned(instr));
                    to_clone.push(instr);
                }
            }
        }

        let step: StepResult<'ctx> = match terminator.get_opcode() {
            InstructionOpcode::Br if terminator.get_num_operands() == 1 => {
                let next = terminator.get_operand(0).and_then(Operand::block)?;
                match classify_next(header, body, current, next) {
                    ClassifiedNext::Continue(b) => StepResult::Continue(b),
                    ClassifiedNext::BackEdge => {
                        StepResult::Done(back_edge_exit(disc_phis, carried_phis, current, &env)?)
                    }
                    ClassifiedNext::Exit(from, target) => {
                        StepResult::Done(TraceExit::Exit { target, from })
                    }
                }
            }
            InstructionOpcode::Br => {
                let (cond, false_block, true_block) = cfg::conditional_branch_parts(terminator)?;
                let cond_val = resolved_to_u64(&resolve_operand(cond, &env))?;
                let next = if cond_val != 0 {
                    true_block
                } else {
                    false_block
                };
                match classify_next(header, body, current, next) {
                    ClassifiedNext::Continue(b) => StepResult::Continue(b),
                    ClassifiedNext::BackEdge => {
                        StepResult::Done(back_edge_exit(disc_phis, carried_phis, current, &env)?)
                    }
                    ClassifiedNext::Exit(from, target) => {
                        StepResult::Done(TraceExit::Exit { target, from })
                    }
                }
            }
            InstructionOpcode::Switch => {
                let cond = terminator.get_operand(0).and_then(Operand::value)?;
                let cond_val = resolved_to_u64(&resolve_operand(cond, &env))?;
                let next = switch_target(terminator, cond_val)?;
                match classify_next(header, body, current, next) {
                    ClassifiedNext::Continue(b) => StepResult::Continue(b),
                    ClassifiedNext::BackEdge => {
                        StepResult::Done(back_edge_exit(disc_phis, carried_phis, current, &env)?)
                    }
                    ClassifiedNext::Exit(from, target) => {
                        StepResult::Done(TraceExit::Exit { target, from })
                    }
                }
            }
            InstructionOpcode::Return | InstructionOpcode::Unreachable => {
                StepResult::Done(TraceExit::Terminal { terminator })
            }
            _ => return None, // invoke/callbr/indirectbr/etc: out of scope.
        };

        match step {
            StepResult::Continue(next) => {
                prev = Some(current);
                current = next;
            }
            StepResult::Done(exit) => {
                return Some(NodeBody {
                    to_clone,
                    env,
                    exit,
                });
            }
        }
    }
}

fn back_edge_exit<'ctx>(
    disc_phis: &[PhiValue<'ctx>],
    carried_phis: &[PhiValue<'ctx>],
    from: BasicBlock<'ctx>,
    env: &HashMap<InstructionValue<'ctx>, Resolved<'ctx>>,
) -> Option<TraceExit<'ctx>> {
    let mut next_state = Vec::with_capacity(disc_phis.len());
    for p in disc_phis {
        let incoming = find_incoming(*p, from)?;
        next_state.push(resolved_to_u64(&resolve_operand(incoming, env))?);
    }
    let mut carried = Vec::with_capacity(carried_phis.len());
    for p in carried_phis {
        let incoming = find_incoming(*p, from)?;
        carried.push(resolve_operand(incoming, env));
    }
    Some(TraceExit::Loop {
        next_state,
        carried,
    })
}

// ============================================================================
// Phase B: materialization
// ============================================================================

pub fn materialize<'ctx>(function: FunctionValue<'ctx>, plan: &LoopPlan<'ctx>) {
    let context = plan.header.get_context();
    let builder = context.create_builder();

    // 1. Pre-create one block per node.
    let blocks: Vec<BasicBlock<'ctx>> = (0..plan.nodes.len())
        .map(|i| context.append_basic_block(function, &format!("jt.n{i}")))
        .collect();

    // 2. Pre-create carried-phi placeholders per node (no incoming edges yet).
    let mut header_phis: HashMap<(usize, usize), PhiValue<'ctx>> = HashMap::new();
    for nid in 0..plan.nodes.len() {
        builder.position_at_end(blocks[nid]);
        for (pi, phi) in plan.carried_phis.iter().enumerate() {
            let ty = phi.as_basic_value().get_type();
            if let Ok(new_phi) = builder.build_phi(ty, &format!("jt.n{nid}.c{pi}")) {
                header_phis.insert((nid, pi), new_phi);
            }
        }
    }

    // 3. Fill in each node's body. `all_built[nid]` is kept around after its
    // node's body is constructed — steps 4 and 6 need it to resolve
    // cross-node `Resolved::Cloned` references (a back-edge/exit
    // contribution's value always lives in the *contributing* node's own
    // `built` map).
    let mut all_built: Vec<HashMap<InstructionValue<'ctx>, BasicValueEnum<'ctx>>> =
        (0..plan.nodes.len()).map(|_| HashMap::new()).collect();
    for nid in 0..plan.nodes.len() {
        let node = plan.nodes[nid]
            .as_ref()
            .expect("every discovered node was walked");
        builder.position_at_end(blocks[nid]);
        let mut built: HashMap<InstructionValue<'ctx>, BasicValueEnum<'ctx>> = HashMap::new();
        for &orig in &node.to_clone {
            let cloned = orig.explicit_clone();
            for i in 0..orig.get_num_operands() {
                let Some(operand_value) = orig.get_operand(i).and_then(Operand::value) else {
                    continue; // block-typed operand on a non-terminator: shouldn't occur.
                };
                let materialized =
                    materialize_operand(operand_value, &node.env, &built, &header_phis);
                cloned.set_operand(i, materialized);
            }
            builder.insert_instruction(&cloned, None);
            let cloned_value = unsafe { BasicValueEnum::new(cloned.as_value_ref()) };
            built.insert(orig, cloned_value);
        }

        match &node.exit {
            TraceExit::Loop { next_state, .. } => {
                let target_id = plan.node_index[next_state];
                let _ = builder.build_unconditional_branch(blocks[target_id]);
            }
            TraceExit::Exit { target, .. } => {
                let _ = builder.build_unconditional_branch(*target);
            }
            TraceExit::Terminal { terminator } => {
                let cloned = terminator.explicit_clone();
                for i in 0..terminator.get_num_operands() {
                    if let Some(operand_value) = terminator.get_operand(i).and_then(Operand::value)
                    {
                        let materialized =
                            materialize_operand(operand_value, &node.env, &built, &header_phis);
                        cloned.set_operand(i, materialized);
                    }
                }
                builder.insert_instruction(&cloned, None);
            }
        }

        all_built[nid] = built;
    }

    // 4. Wire up each node's carried-phi incoming edges from its contributions.
    for nid in 0..plan.nodes.len() {
        let Some(contribs) = plan.node_contributions.get(&nid) else {
            continue;
        };
        for (pi, _phi) in plan.carried_phis.iter().enumerate() {
            let Some(&target_phi) = header_phis.get(&(nid, pi)) else {
                continue;
            };
            for contrib in contribs {
                let (pred_block, value) = match &contrib.source {
                    ContribSource::Outside(pred) => {
                        let v = match &contrib.carried[pi] {
                            Resolved::External(v) => *v,
                            _ => continue,
                        };
                        (*pred, v)
                    }
                    ContribSource::Node(source_nid) => {
                        let v = materialize_resolved(
                            &contrib.carried[pi],
                            &all_built[*source_nid],
                            &header_phis,
                        );
                        (blocks[*source_nid], v)
                    }
                };
                target_phi.add_incoming(&[(&value, pred_block)]);
            }
        }
    }

    // 5. Redirect original entry edges.
    for &(pred, node_id) in &plan.entry_edges {
        let terminator = pred.get_terminator().expect("checked in plan_loop");
        builder.position_before(&terminator);
        let _ = builder.build_unconditional_branch(blocks[node_id]);
        terminator.erase_from_basic_block();
    }

    // 6. Rebuild exit-block phis.
    for (&exit_block, contribs) in &plan.exits {
        rebuild_exit_phis(
            &builder, &plan.body, exit_block, contribs, &blocks, &all_built,
        );
    }

    // 7. Reduce the original (now-dead) loop body to bare `unreachable`
    // blocks. Every remaining instruction (phis included) is first
    // `poison`-and-erased — redirecting every use (including cross-
    // references *within* this doomed set, e.g. a phi another block's
    // instruction reads) before erasing is what makes deletion safe
    // regardless of order; erasing an instruction with remaining uses is
    // undefined behavior in LLVM. The blocks stay in the function as
    // ordinary unreachable code (nothing — inside or outside this set —
    // reaches them anymore); downstream `opt`/simplifycfg trivially removes
    // them.
    for &b in &plan.body {
        if let Some(terminator) = b.get_terminator() {
            builder.position_before(&terminator);
            let _ = builder.build_unreachable();
            poison_and_erase(terminator);
        }
        let remaining: Vec<InstructionValue<'ctx>> = b
            .get_instructions()
            .filter(|i| i.get_opcode() != InstructionOpcode::Unreachable)
            .collect();
        for instr in remaining {
            poison_and_erase(instr);
        }
    }
}

/// Redirect every use of `instr` to a fresh `poison` value of its own type,
/// then erase it. Safe regardless of erasure order across a whole doomed
/// block set, since each instruction's uses are cleared before it (or
/// anything it referenced) is actually removed.
fn poison_and_erase(instr: InstructionValue<'_>) {
    unsafe {
        let ty = LLVMTypeOf(instr.as_value_ref());
        let poison = LLVMGetPoison(ty);
        LLVMReplaceAllUsesWith(instr.as_value_ref(), poison);
    }
    instr.erase_from_basic_block();
}

fn materialize_operand<'ctx>(
    v: BasicValueEnum<'ctx>,
    env: &HashMap<InstructionValue<'ctx>, Resolved<'ctx>>,
    built: &HashMap<InstructionValue<'ctx>, BasicValueEnum<'ctx>>,
    header_phis: &HashMap<(usize, usize), PhiValue<'ctx>>,
) -> BasicValueEnum<'ctx> {
    let Some(instr) = v.as_instruction_value() else {
        return v; // literal constant or a value from outside the loop: unchanged.
    };
    let Some(resolved) = env.get(&instr) else {
        return v; // value defined outside this trace's env: unchanged.
    };
    materialize_resolved_with_type(resolved, v.get_type(), built, header_phis)
}

fn materialize_resolved<'ctx>(
    r: &Resolved<'ctx>,
    built: &HashMap<InstructionValue<'ctx>, BasicValueEnum<'ctx>>,
    header_phis: &HashMap<(usize, usize), PhiValue<'ctx>>,
) -> BasicValueEnum<'ctx> {
    match r {
        Resolved::External(v) => *v,
        Resolved::Cloned(instr) => built[instr],
        Resolved::HeaderCarried(nid, idx) => header_phis[&(*nid, *idx)].as_basic_value(),
        Resolved::Concrete(_) => {
            unreachable!("Concrete requires a target type; use materialize_resolved_with_type")
        }
    }
}

fn materialize_resolved_with_type<'ctx>(
    r: &Resolved<'ctx>,
    ty: inkwell::types::BasicTypeEnum<'ctx>,
    built: &HashMap<InstructionValue<'ctx>, BasicValueEnum<'ctx>>,
    header_phis: &HashMap<(usize, usize), PhiValue<'ctx>>,
) -> BasicValueEnum<'ctx> {
    match r {
        Resolved::Concrete(n) => match ty {
            inkwell::types::BasicTypeEnum::IntType(it) => it.const_int(*n, false).into(),
            other => other.const_zero(),
        },
        Resolved::External(v) => *v,
        Resolved::Cloned(instr) => built[instr],
        Resolved::HeaderCarried(nid, idx) => header_phis[&(*nid, *idx)].as_basic_value(),
    }
}

fn rebuild_exit_phis<'ctx>(
    builder: &inkwell::builder::Builder<'ctx>,
    body: &HashSet<BasicBlock<'ctx>>,
    exit_block: BasicBlock<'ctx>,
    contribs: &[ExitContribution<'ctx>],
    node_blocks: &[BasicBlock<'ctx>],
    all_built: &[HashMap<InstructionValue<'ctx>, BasicValueEnum<'ctx>>],
) {
    let mut phis = Vec::new();
    for instr in exit_block.get_instructions() {
        if instr.get_opcode() != InstructionOpcode::Phi {
            break;
        }
        if let Ok(p) = PhiValue::try_from(instr) {
            phis.push(p);
        }
    }
    for old_phi in phis {
        let ty = old_phi.as_basic_value().get_type();
        builder.position_before(&old_phi.as_instruction());
        let Ok(new_phi) = builder.build_phi(ty, "jt.exit") else {
            continue;
        };
        for i in 0..old_phi.count_incoming() {
            let Some((value, block)) = old_phi.get_incoming(i) else {
                continue;
            };
            if !body.contains(&block) {
                new_phi.add_incoming(&[(&value, block)]);
            }
        }
        for c in contribs {
            let built = &all_built[c.node_id];
            // The value this exit sees is whatever the trace's final env had
            // for the phi's original incoming operand from `c.from`.
            if let Some((incoming_value, _)) = (0..old_phi.count_incoming())
                .filter_map(|i| old_phi.get_incoming(i))
                .find(|(_, b)| *b == c.from)
            {
                let resolved = incoming_value
                    .as_instruction_value()
                    .and_then(|i| built.get(&i).copied())
                    .unwrap_or(incoming_value);
                new_phi.add_incoming(&[(&resolved, node_blocks[c.node_id])]);
            }
        }
        old_phi.replace_all_uses_with(&new_phi);
        old_phi.as_instruction().erase_from_basic_block();
    }
}
