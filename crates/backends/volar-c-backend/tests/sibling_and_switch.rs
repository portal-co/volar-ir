// @reliability: normal
//! Tests for Phase 4a's `LirTarget` additions on `CBackend`: sibling
//! (intra-module) calls, the general `switch` primitive, and the default
//! `block_addr`/`dyn_jump` implementations built on top of it.

use volar_c_backend::CBackend;
use volar_lir::{BranchTarget, IcmpPred, LirTarget, LirType};
use volar_lir_test_corpus::compile_and_run;

// ============================================================================
// Sibling calls: mutual recursion between two module-local functions.
// `is_even` calls `is_odd` before `is_odd` has been defined (forward
// reference) — this must compile via CBackend's forward-declaration list.
// ============================================================================

#[test]
fn sibling_call_mutual_recursion() {
    let mut b = CBackend::new();

    let (entry, params) = b.begin_function("is_even", &[LirType::U32], Some(LirType::Bool));
    b.switch_to_block(entry);
    let n = params[0][0];
    let zero = b.iconst(LirType::U32, 0);
    let is_zero = b.icmp(IcmpPred::Eq, n, zero);
    let then_block = b.create_block();
    let else_block = b.create_block();
    b.branch(
        is_zero,
        then_block,
        BranchTarget::args([]),
        else_block,
        BranchTarget::args([]),
    );
    b.switch_to_block(then_block);
    let t = b.iconst(LirType::Bool, 1);
    b.ret(&[t]);
    b.switch_to_block(else_block);
    let one = b.iconst(LirType::U32, 1);
    let n_minus_1 = b.sub(n, one);
    let result = b.call("is_odd", &[LirType::U32], &[n_minus_1], Some(LirType::Bool));
    b.ret(&result);
    b.end_function();

    let (entry2, params2) = b.begin_function("is_odd", &[LirType::U32], Some(LirType::Bool));
    b.switch_to_block(entry2);
    let n2 = params2[0][0];
    let zero2 = b.iconst(LirType::U32, 0);
    let is_zero2 = b.icmp(IcmpPred::Eq, n2, zero2);
    let then_block2 = b.create_block();
    let else_block2 = b.create_block();
    b.branch(
        is_zero2,
        then_block2,
        BranchTarget::args([]),
        else_block2,
        BranchTarget::args([]),
    );
    b.switch_to_block(then_block2);
    let f = b.iconst(LirType::Bool, 0);
    b.ret(&[f]);
    b.switch_to_block(else_block2);
    let one2 = b.iconst(LirType::U32, 1);
    let n2_minus_1 = b.sub(n2, one2);
    let result2 = b.call(
        "is_even",
        &[LirType::U32],
        &[n2_minus_1],
        Some(LirType::Bool),
    );
    b.ret(&result2);
    b.end_function();

    let c_src = b.finish();
    let output = compile_and_run(
        &c_src,
        r#"  printf("%d %d %d\n", is_even(4), is_even(5), is_odd(7));"#,
    );
    assert_eq!(output.trim(), "1 0 1");
}

// ============================================================================
// `switch`: heterogeneous per-case args (unlike `dyn_jump`, which requires
// uniform args across all destinations).
// ============================================================================

#[test]
fn switch_heterogeneous_args() {
    let mut b = CBackend::new();

    let (entry, params) = b.begin_function("classify", &[LirType::U32], Some(LirType::U32));
    b.switch_to_block(entry);
    let n = params[0][0];

    let one_block = b.create_block();
    let one_param = b.add_block_param(one_block, LirType::U32);
    let two_block = b.create_block();
    let two_param = b.add_block_param(two_block, LirType::U32);
    let default_block = b.create_block();
    let default_param = b.add_block_param(default_block, LirType::U32);

    let hundred = b.iconst(LirType::U32, 100);
    let twohundred = b.iconst(LirType::U32, 200);
    let neg1 = b.iconst(LirType::U32, u32::MAX as i64);

    b.switch(
        n,
        &[
            (1, one_block, BranchTarget::args([hundred])),
            (2, two_block, BranchTarget::args([twohundred])),
        ],
        default_block,
        BranchTarget::args([neg1]),
    );

    b.switch_to_block(one_block);
    b.ret(&[one_param]);
    b.switch_to_block(two_block);
    b.ret(&[two_param]);
    b.switch_to_block(default_block);
    b.ret(&[default_param]);
    b.end_function();

    let c_src = b.finish();
    let output = compile_and_run(
        &c_src,
        r#"  printf("%u %u %u\n", classify(1), classify(2), classify(9));"#,
    );
    assert_eq!(output.trim(), "100 200 4294967295");
}

// ============================================================================
// `block_addr` + `dyn_jump` (default implementations, built on `switch`):
// select one of two block addresses at runtime, then jump to it with
// uniform args.
// ============================================================================

#[test]
fn block_addr_dyn_jump() {
    let mut b = CBackend::new();

    let (entry, params) = b.begin_function(
        "dispatch",
        &[LirType::Bool, LirType::U32],
        Some(LirType::U32),
    );
    b.switch_to_block(entry);
    let cond = params[0][0];
    let n = params[1][0];

    let block_a = b.create_block();
    let a_param = b.add_block_param(block_a, LirType::U32);
    let block_b = b.create_block();
    let b_param = b.add_block_param(block_b, LirType::U32);

    let addr_a = b.block_addr(block_a);
    let addr_b = b.block_addr(block_b);
    let chosen = b.select(cond, addr_a, addr_b);
    b.dyn_jump(chosen, &[block_a, block_b], BranchTarget::args([n]));

    b.switch_to_block(block_a);
    let ten = b.iconst(LirType::U32, 10);
    let a_result = b.add(a_param, ten);
    b.ret(&[a_result]);

    b.switch_to_block(block_b);
    let twenty = b.iconst(LirType::U32, 20);
    let b_result = b.add(b_param, twenty);
    b.ret(&[b_result]);

    b.end_function();

    let c_src = b.finish();
    let output = compile_and_run(
        &c_src,
        r#"  printf("%u %u\n", dispatch(true, 5), dispatch(false, 5));"#,
    );
    assert_eq!(output.trim(), "15 25");
}
