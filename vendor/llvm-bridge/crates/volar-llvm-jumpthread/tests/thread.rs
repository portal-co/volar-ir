//! End-to-end tests: parse hand-authored `.ll`, run `thread_jumps`, then
//! JIT-execute the (possibly rewritten) function and check its result.
//!
//! Verifies both that threading actually fires when expected (comparing
//! against an untouched-loop reference result) and that it *cleanly bails*
//! — leaving the function correct and unchanged — on shapes outside its
//! documented scope (a genuinely data-dependent branch, or an oversized
//! state space).

use inkwell::OptimizationLevel;
use inkwell::context::Context;
use inkwell::execution_engine::ExecutionEngine;
use inkwell::memory_buffer::MemoryBuffer;
use inkwell::module::Module;

use volar_llvm_jumpthread::{JumpThreadLimits, ThreadResult, thread_jumps};

fn parse<'ctx>(context: &'ctx Context, src: &str) -> Module<'ctx> {
    context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            src.as_bytes(),
            "test.ll",
        ))
        .expect("valid test IR")
}

/// Verify and JIT-compile `module` once; the returned engine can be reused
/// for multiple calls (a module may only be claimed by one engine).
fn jit<'ctx>(module: &Module<'ctx>) -> ExecutionEngine<'ctx> {
    module.verify().expect("module verifies");
    module
        .create_jit_execution_engine(OptimizationLevel::None)
        .expect("jit engine")
}

unsafe fn run_i32(ee: &ExecutionEngine<'_>, name: &str, args: &[i32]) -> i32 {
    unsafe {
        match args.len() {
            0 => {
                let f = ee
                    .get_function::<unsafe extern "C" fn() -> i32>(name)
                    .expect("function exists");
                f.call()
            }
            1 => {
                let f = ee
                    .get_function::<unsafe extern "C" fn(i32) -> i32>(name)
                    .expect("function exists");
                f.call(args[0])
            }
            _ => panic!("run_i32: unsupported arg count {}", args.len()),
        }
    }
}

// ============================================================================
// Trivial case: a plain concretely-bounded counting loop with a carried
// (symbolic) accumulator. No internal branching beyond the latch guard —
// the "trivial single-transition" shape jump threading should fully unroll.
// ============================================================================

const TRIVIAL_COUNT: &str = "\
    define i32 @trivial(i32 %seed) {\n\
    entry:\n\
      br label %header\n\
    header:\n\
      %i = phi i32 [ 0, %entry ], [ %i.next, %latch ]\n\
      %acc = phi i32 [ %seed, %entry ], [ %acc.next, %latch ]\n\
      br label %body\n\
    body:\n\
      %acc.next = add i32 %acc, %i\n\
      br label %latch\n\
    latch:\n\
      %i.next = add i32 %i, 1\n\
      %cont = icmp ult i32 %i.next, 4\n\
      br i1 %cont, label %header, label %exit\n\
    exit:\n\
      %result = phi i32 [ %acc.next, %latch ]\n\
      ret i32 %result\n\
    }\n\
    ";

#[test]
fn trivial_counting_loop_threads_and_matches_reference() {
    let context = Context::create();
    let module = parse(&context, TRIVIAL_COUNT);
    let function = module.get_function("trivial").expect("trivial exists");

    let result = thread_jumps(function, JumpThreadLimits::default());
    assert_eq!(result, ThreadResult::Threaded { loops_rewritten: 1 });

    // Reference: seed + 0 + 1 + 2 + 3 = seed + 6.
    let ee = jit(&module);
    for seed in [0i32, 5, -3, 100] {
        let actual = unsafe { run_i32(&ee, "trivial", &[seed]) };
        assert_eq!(actual, seed + 6, "seed={seed}");
    }
}

#[test]
fn trivial_counting_loop_bails_under_a_tight_state_ceiling() {
    let context = Context::create();
    let module = parse(&context, TRIVIAL_COUNT);
    let function = module.get_function("trivial").expect("trivial exists");

    // Only 2 states allowed; the loop needs 4 (i=0,1,2,3) plus headroom for
    // discovery -- must bail cleanly, and the function must still work.
    let result = thread_jumps(function, JumpThreadLimits { max_states: 2 });
    assert_eq!(result, ThreadResult::NoChange);

    let ee = jit(&module);
    let actual = unsafe { run_i32(&ee, "trivial", &[10]) };
    assert_eq!(actual, 16);
}

// ============================================================================
// Dispatch/state-machine case: a discriminator (`%pc`) reached from *two*
// distinct entry constants that both transition to the *same* next state —
// the DFA-convergence case the memoization exists for. Exercises multiple
// entry edges, a `switch` inside the loop, and node reuse (state `2` is
// discovered once but has two contributing predecessors).
// ============================================================================

const DISPATCH: &str = "\
    define i32 @dispatch(i32 %which) {\n\
    entry:\n\
      %cond = icmp ne i32 %which, 0\n\
      br i1 %cond, label %pre0, label %pre1\n\
    pre0:\n\
      br label %header\n\
    pre1:\n\
      br label %header\n\
    header:\n\
      %pc = phi i32 [ 0, %pre0 ], [ 1, %pre1 ], [ 2, %join ]\n\
      switch i32 %pc, label %halt [ i32 0, label %s0\n\
                                     i32 1, label %s1 ]\n\
    s0:\n\
      br label %join\n\
    s1:\n\
      br label %join\n\
    join:\n\
      br label %header\n\
    halt:\n\
      %result = phi i32 [ 10, %header ]\n\
      ret i32 %result\n\
    }\n\
    ";

#[test]
fn dispatch_loop_threads_with_converging_states() {
    let context = Context::create();
    let module = parse(&context, DISPATCH);
    let function = module.get_function("dispatch").expect("dispatch exists");

    let result = thread_jumps(function, JumpThreadLimits::default());
    assert_eq!(result, ThreadResult::Threaded { loops_rewritten: 1 });

    let ee = jit(&module);
    for which in [0i32, 1] {
        let actual = unsafe { run_i32(&ee, "dispatch", &[which]) };
        assert_eq!(actual, 10, "which={which}");
    }
}

// ============================================================================
// Regression / safety net: a data-dependent early-exit branch inside the
// loop (the idiom `cirrus-llvm-pass`'s `deloopify_early_exits` targets via a
// different, narrower mechanism). Since the branch condition depends on
// loaded memory, not the discriminator, thread_jumps cannot fold it and
// must bail the whole loop untouched -- never a partial/approximate rewrite.
// ============================================================================

const MEMCMP_KERNEL: &str = "\
    define i1 @kernel(ptr %a, ptr %b, i32 %n) {\n\
    entry:\n\
      br label %header\n\
    header:\n\
      %i = phi i32 [ 0, %entry ], [ %i.next, %latch ]\n\
      br label %body\n\
    body:\n\
      %pa = getelementptr i8, ptr %a, i32 %i\n\
      %pb = getelementptr i8, ptr %b, i32 %i\n\
      %va = load i8, ptr %pa\n\
      %vb = load i8, ptr %pb\n\
      %eq = icmp eq i8 %va, %vb\n\
      br i1 %eq, label %continue, label %exit.trampoline\n\
    exit.trampoline:\n\
      br label %merge\n\
    continue:\n\
      %i.next = add i32 %i, 1\n\
      br label %latch\n\
    latch:\n\
      %cont = icmp ult i32 %i.next, %n\n\
      br i1 %cont, label %header, label %merge\n\
    merge:\n\
      %result = phi i1 [ false, %exit.trampoline ], [ true, %latch ]\n\
      ret i1 %result\n\
    }\n\
    ";

#[test]
fn data_dependent_branch_inside_the_loop_bails_cleanly() {
    let context = Context::create();
    let module = parse(&context, MEMCMP_KERNEL);
    let function = module.get_function("kernel").expect("kernel exists");

    let result = thread_jumps(function, JumpThreadLimits::default());
    assert_eq!(result, ThreadResult::NoChange);

    // The function must still verify and behave correctly, untouched.
    module
        .verify()
        .expect("module still verifies after a clean bail");
}
