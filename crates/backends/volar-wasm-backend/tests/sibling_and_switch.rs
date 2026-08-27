// @reliability: experimental
//! Direct `LirTarget` API tests for Phase 4a's additions on [`WasmBackend`]:
//! sibling (intra-module) calls, the general `switch` primitive, and the
//! default `block_addr`/`dyn_jump` implementations built on top of it.

use volar_lir::{BranchTarget, IcmpPred, LirTarget, LirType};
use volar_wasm_backend::WasmBackend;
use wasmtime::{Engine, Instance, Module, Store};

fn instantiate(bytes: &[u8]) -> (Store<()>, Instance) {
    let engine = Engine::default();
    let module = Module::new(&engine, bytes).expect("invalid wasm module");
    let mut store = Store::new(&engine, ());
    let instance = Instance::new(&mut store, &module, &[]).expect("instantiate");
    (store, instance)
}

// ============================================================================
// Sibling calls: mutual recursion. `is_even` calls `is_odd` before `is_odd`
// has been defined (forward reference) — resolved only at `finish()`.
// ============================================================================

#[test]
fn sibling_call_mutual_recursion() {
    let mut b = WasmBackend::new();

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
        BranchTarget::args(vec![]),
        else_block,
        BranchTarget::args(vec![]),
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
        BranchTarget::args(vec![]),
        else_block2,
        BranchTarget::args(vec![]),
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

    let bytes = b.finish();
    let (mut store, instance) = instantiate(&bytes);
    let is_even = instance
        .get_typed_func::<i32, i32>(&mut store, "is_even")
        .unwrap();
    let is_odd = instance
        .get_typed_func::<i32, i32>(&mut store, "is_odd")
        .unwrap();
    assert_eq!(is_even.call(&mut store, 4).unwrap(), 1);
    assert_eq!(is_even.call(&mut store, 5).unwrap(), 0);
    assert_eq!(is_odd.call(&mut store, 7).unwrap(), 1);
}

// ============================================================================
// `switch`: heterogeneous per-case args.
// ============================================================================

#[test]
fn switch_heterogeneous_args() {
    let mut b = WasmBackend::new();

    let (entry, params) = b.begin_function("classify", &[LirType::U32], Some(LirType::U32));
    let n = params[0][0];

    let one_block = b.create_block();
    let one_param = b.add_block_param(one_block, LirType::U32);
    let two_block = b.create_block();
    let two_param = b.add_block_param(two_block, LirType::U32);
    let default_block = b.create_block();
    let default_param = b.add_block_param(default_block, LirType::U32);

    b.switch_to_block(entry);
    let hundred = b.iconst(LirType::U32, 100);
    let twohundred = b.iconst(LirType::U32, 200);
    let neg1 = b.iconst(LirType::U32, -1);

    b.switch(
        n,
        &[
            (1, one_block, BranchTarget::args(vec![hundred])),
            (2, two_block, BranchTarget::args(vec![twohundred])),
        ],
        default_block,
        BranchTarget::args(vec![neg1]),
    );

    b.switch_to_block(one_block);
    b.ret(&[one_param]);
    b.switch_to_block(two_block);
    b.ret(&[two_param]);
    b.switch_to_block(default_block);
    b.ret(&[default_param]);
    b.end_function();

    let bytes = b.finish();
    let (mut store, instance) = instantiate(&bytes);
    let classify = instance
        .get_typed_func::<i32, i32>(&mut store, "classify")
        .unwrap();
    assert_eq!(classify.call(&mut store, 1).unwrap(), 100);
    assert_eq!(classify.call(&mut store, 2).unwrap(), 200);
    assert_eq!(classify.call(&mut store, 9).unwrap(), -1);
}

// ============================================================================
// `block_addr` + `dyn_jump` (default implementations, built on `switch`).
// ============================================================================

#[test]
fn block_addr_dyn_jump() {
    let mut b = WasmBackend::new();

    let (entry, params) = b.begin_function(
        "dispatch",
        &[LirType::Bool, LirType::U32],
        Some(LirType::U32),
    );
    let cond = params[0][0];
    let n = params[1][0];

    let block_a = b.create_block();
    let a_param = b.add_block_param(block_a, LirType::U32);
    let block_b = b.create_block();
    let b_param = b.add_block_param(block_b, LirType::U32);

    b.switch_to_block(entry);
    let addr_a = b.block_addr(block_a);
    let addr_b = b.block_addr(block_b);
    let chosen = b.select(cond, addr_a, addr_b);
    b.dyn_jump(chosen, &[block_a, block_b], BranchTarget::args(vec![n]));

    b.switch_to_block(block_a);
    let ten = b.iconst(LirType::U32, 10);
    let a_result = b.add(a_param, ten);
    b.ret(&[a_result]);

    b.switch_to_block(block_b);
    let twenty = b.iconst(LirType::U32, 20);
    let b_result = b.add(b_param, twenty);
    b.ret(&[b_result]);

    b.end_function();

    let bytes = b.finish();
    let (mut store, instance) = instantiate(&bytes);
    let dispatch = instance
        .get_typed_func::<(i32, i32), i32>(&mut store, "dispatch")
        .unwrap();
    assert_eq!(dispatch.call(&mut store, (1, 5)).unwrap(), 15);
    assert_eq!(dispatch.call(&mut store, (0, 5)).unwrap(), 25);
}
