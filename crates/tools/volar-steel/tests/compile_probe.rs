//! S2: compilation — drive the reference program (with data-dependent control
//! flow) through the full compile: LLVM → VAFFLE → Volar IR → movfuscate →
//! circuit. Gate: the reference program reaches a Volar IR step block, and the
//! step circuit's trace matches the native run (differential).
//! See `docs/steel-in-volar-plan.md` milestone S2.

use std::path::PathBuf;

use inkwell::context::Context;
use inkwell::memory_buffer::MemoryBuffer;

fn artifacts() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("programs")
}

/// Native spec of the reference program (data-dependent loop).
fn reference_native(key: u64, value: u64, n: u64, path: [u64; 4]) -> u64 {
    let mut acc = key ^ value;
    let bound = n.min(4);
    let mut i = 0;
    while i < bound {
        acc = acc.wrapping_mul(0x9e3779b97f4a7c15) ^ path[i as usize];
        i += 1;
    }
    acc
}

/// Import a program via the LLVM frontend and lower to Volar IR.
fn import_program_to_ir(
    file: &str,
    entry: &str,
) -> (volar_ir::ir::IRBlocks<()>, volar_ir::ir::IRTypes) {
    let ll = std::fs::read_to_string(artifacts().join(file)).expect(file);
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            ll.as_bytes(),
            file,
        ))
        .expect("LLVM IR parses");
    let vaffle = volar_llvm_vaffle_import::import_module(&module, &[entry]).expect("LLVM import");
    volar_vaffle_target::lower_vaffle_to_ir(&vaffle)
}

/// Import the reference program via the LLVM frontend and lower to Volar IR.
fn import_to_ir() -> (volar_ir::ir::IRBlocks<()>, volar_ir::ir::IRTypes) {
    import_program_to_ir("reference.ll", "steel_view_call")
}

/// S6a: the storage-read proof program through the full compile pipeline.
#[test]
fn s6a_storage_read_reaches_a_step_circuit() {
    let (blocks, mut types) = import_program_to_ir("steel_view_call.ll", "steel_storage_read");
    eprintln!("S6a: lowered to IR — {} blocks", blocks.blocks.len());
    let movfuscated = volar_ir_passes::movfuscate::movfuscate_ir(&blocks, &mut types);
    let bit_ty = types
        .0
        .iter()
        .position(|t| {
            matches!(
                t,
                volar_ir_common::IrType::Primitive(volar_ir_common::Type::Bit)
            )
        })
        .map(|i| volar_ir::ir::IRTypeId(i as u32))
        .expect("Bit interned");
    let circuit = volar_ir_passes::lower_to_circuit::lower_to_circuit_ir(
        &movfuscated,
        &bit_ty,
        1,
        volar_ir_passes::lower_to_circuit::LoweringMode::WithTerminationFlag,
    );
    let step = &circuit.blocks[0];
    eprintln!(
        "S6a: step block — {} params, {} stmts",
        step.params.len(),
        step.stmts.len()
    );

    // Differential: drive the step circuit from a non-zero input and check the
    // return matches the native storage-read.
    let widths: Vec<usize> = step
        .params
        .iter()
        .map(|&t| types.value_bit_width(t).expect("data type"))
        .collect();
    let pc_width = widths.iter().take_while(|&&w| w == 1).count();
    let n_params = step.params.len();
    // steel_storage_read(key, value, n, path0..3) with key=3, value=9, n=2.
    let inputs = [3u64, 9, 2, 11, 13, 17, 19];
    let mut state: Vec<Vec<bool>> = widths.iter().map(|&w| vec![false; w]).collect();
    for (k, &inp) in inputs.iter().enumerate() {
        let slot = pc_width + k;
        if slot < state.len() && state[slot].len() == 64 {
            for b in 0..64 {
                state[slot][b] = (inp >> b) & 1 == 1;
            }
        }
    }
    let mut storage = Default::default();
    let mut steps = 0usize;
    let ret_vals;
    loop {
        let outputs = volar_fuzz::interpreter::ir::eval_ir_circuit_step(
            step,
            &types,
            &circuit.oracles,
            &state,
            &mut storage,
        );
        let done = outputs[0].iter().any(|&b| b);
        if done {
            ret_vals = outputs[1 + n_params..].to_vec();
            break;
        }
        state = outputs[1..1 + n_params].to_vec();
        steps += 1;
        assert!(steps <= 1024, "step circuit did not return in 1024 steps");
    }
    // The native reference: hash_pair(key, value) then hash_pair(node, path0),
    // hash_pair(node, path1) (n=2).
    let native = {
        fn hp(l: u64, r: u64) -> u64 {
            let mut x = l.wrapping_mul(0x9e3779b97f4a7c15) ^ r;
            x = x.wrapping_mul(0xbf58476d1ce4e5b9);
            x ^ (x >> 29)
        }
        let mut node = hp(3, 9);
        node = hp(node, 11);
        node = hp(node, 13);
        node
    };
    // Reassemble the return value (the first 64 bits of ret_vals).
    let mut got = 0u64;
    let mut bitidx = 0;
    'outer: for slot in &ret_vals {
        for &bit in slot {
            if bitidx >= 64 {
                break 'outer;
            }
            if bit {
                got |= 1 << bitidx;
            }
            bitidx += 1;
        }
    }
    eprintln!(
        "S6a: step circuit returned after {steps} steps; circuit={got:#x} native={native:#x}"
    );
    assert_eq!(
        got, native,
        "S6a differential: the storage-read circuit must match native"
    );
}

/// Isolation: does a runtime-indexed local-array read work through the pipeline?
#[test]
/// IR-level twin of `array_read_dynamic_index_works`: distinguishes an
/// importer-side byte-cell bug (pre-movfuscate eval wrong) from a
/// circuit-lowering bug (pre-movfuscate right, circuit wrong).
#[test]
fn array_read_dynamic_index_pre_movfuscate_eval() {
    // arr = [10, 20, 30, 40], idx = 2 -> 30.
    eval_pre_movfuscate("array_read.ll", "read_idx", &[10, 20, 30, 40, 2], 30);
}

/// Storage-cell dump for the dynamic array read: shows where each
/// element's bytes actually landed in the pool storage after the IR eval.
#[test]
fn array_read_storage_dump() {
    let (blocks, types) = import_program_to_ir("array_read.ll", "read_idx");
    let entry_block = &blocks.blocks[0];
    let widths: Vec<usize> = entry_block
        .params
        .iter()
        .map(|&t| types.value_bit_width(t).expect("param width"))
        .collect();
    let args = [10u64, 20, 30, 40, 2];
    let inputs: Vec<Vec<bool>> = args
        .iter()
        .map(|&inp| (0..64).map(|b| (inp >> b) & 1 == 1).collect())
        .collect();
    let _ = widths;
    let (out, storage) =
        volar_fuzz::interpreter::ir::eval_ir_with_storage(&blocks, &types, &inputs);
    let mut d = String::new();
    use core::fmt::Write as _;
    let _ = writeln!(
        d,
        "ret: {:?}",
        out.map(|r| {
            let mut got = 0u64;
            let mut bitidx = 0;
            'outer: for slot in &r {
                for &bit in slot {
                    if bitidx >= 64 {
                        break 'outer;
                    }
                    if bit {
                        got |= 1 << bitidx;
                    }
                    bitidx += 1;
                }
            }
            got
        })
    );
    for ((sid, tid, addr), bits) in storage.iter() {
        let mut v = 0u64;
        for (i, &b) in bits.iter().enumerate().take(64) {
            if b {
                v |= 1 << i;
            }
        }
        let _ = writeln!(
            d,
            "cell storage={:?} ty={:?} addr={addr} bits={} val={v:#x}",
            sid,
            tid,
            bits.len()
        );
    }
    // Dump the entry block's statements (truncated) to inspect the store
    // address chains.
    for (bi, b) in blocks.blocks.iter().enumerate() {
        let _ = writeln!(d, "== block {bi}");
        for (vi, s) in b.stmts.iter().enumerate() {
            let debug = format!("{:?}", s.kind);
            let debug = if debug.len() > 140 {
                format!("{}...", &debug[..140])
            } else {
                debug
            };
            let _ = writeln!(d, "  v{vi}: {debug}");
        }
    }
    panic!("DUMP\n{d}");
}

fn array_read_dynamic_index_works() {
    let (blocks, mut types) = import_program_to_ir("array_read.ll", "read_idx");
    let movfuscated = volar_ir_passes::movfuscate::movfuscate_ir(&blocks, &mut types);
    let bit_ty = types
        .0
        .iter()
        .position(|t| {
            matches!(
                t,
                volar_ir_common::IrType::Primitive(volar_ir_common::Type::Bit)
            )
        })
        .map(|i| volar_ir::ir::IRTypeId(i as u32))
        .expect("Bit interned");
    let circuit = volar_ir_passes::lower_to_circuit::lower_to_circuit_ir(
        &movfuscated,
        &bit_ty,
        1,
        volar_ir_passes::lower_to_circuit::LoweringMode::WithTerminationFlag,
    );
    let step = &circuit.blocks[0];
    let widths: Vec<usize> = step
        .params
        .iter()
        .map(|&t| types.value_bit_width(t).expect("data type"))
        .collect();
    let pc_width = widths.iter().take_while(|&&w| w == 1).count();
    let n_params = step.params.len();
    // arr = [10, 20, 30, 40], idx = 2 -> 30.
    let inputs = [10u64, 20, 30, 40, 2];
    let mut state: Vec<Vec<bool>> = widths.iter().map(|&w| vec![false; w]).collect();
    for (k, &inp) in inputs.iter().enumerate() {
        let slot = pc_width + k;
        if slot < state.len() && state[slot].len() == 64 {
            for b in 0..64 {
                state[slot][b] = (inp >> b) & 1 == 1;
            }
        }
    }
    let mut storage = Default::default();
    let mut steps = 0usize;
    let ret_vals;
    loop {
        let outputs = volar_fuzz::interpreter::ir::eval_ir_circuit_step(
            step,
            &types,
            &circuit.oracles,
            &state,
            &mut storage,
        );
        if outputs[0].iter().any(|&b| b) {
            ret_vals = outputs[1 + n_params..].to_vec();
            break;
        }
        state = outputs[1..1 + n_params].to_vec();
        steps += 1;
        assert!(steps <= 32);
    }
    let mut got = 0u64;
    let mut bitidx = 0;
    'outer: for slot in &ret_vals {
        for &bit in slot {
            if bitidx >= 64 {
                break 'outer;
            }
            if bit {
                got |= 1 << bitidx;
            }
            bitidx += 1;
        }
    }
    eprintln!("array_read: circuit={got} (expect 30), {steps} steps");
    assert_eq!(got, 30, "the dynamic-indexed array read must work");
}

/// Isolation: does a loop-carried scalar (i, acc) work through the pipeline?
#[test]
fn simple_loop_carried_scalar_works() {
    let (blocks, mut types) = import_program_to_ir("simple_loop.ll", "count_to");
    // DEBUG: dump the loop blocks in detail (params + terminator dest + args).
    for (i, b) in blocks.blocks.iter().enumerate() {
        let widths: Vec<usize> = b
            .params
            .iter()
            .map(|&t| types.value_bit_width(t).unwrap_or(999))
            .collect();
        let term = match &b.terminator {
            volar_ir::ir::IRTerminator::Jmp { target } => format!(
                "Jmp {:?} args={:?}",
                target.dest,
                target.args.iter().map(|a| a.0).collect::<Vec<_>>()
            ),
            volar_ir::ir::IRTerminator::JumpCond {
                then_target,
                else_target,
                ..
            } => format!(
                "JumpCond then={:?}({}) else={:?}({})",
                then_target.dest,
                then_target.args.len(),
                else_target.dest,
                else_target.args.len()
            ),
            other => format!("{:?}", core::mem::discriminant(other)),
        };
        eprintln!("  block {i}: params{widths:?} term={term}");
    }
    let movfuscated = volar_ir_passes::movfuscate::movfuscate_ir(&blocks, &mut types);
    let bit_ty = types
        .0
        .iter()
        .position(|t| {
            matches!(
                t,
                volar_ir_common::IrType::Primitive(volar_ir_common::Type::Bit)
            )
        })
        .map(|i| volar_ir::ir::IRTypeId(i as u32))
        .expect("Bit interned");
    let circuit = volar_ir_passes::lower_to_circuit::lower_to_circuit_ir(
        &movfuscated,
        &bit_ty,
        1,
        volar_ir_passes::lower_to_circuit::LoweringMode::WithTerminationFlag,
    );
    let step = &circuit.blocks[0];
    let widths: Vec<usize> = step
        .params
        .iter()
        .map(|&t| types.value_bit_width(t).expect("data type"))
        .collect();
    let pc_width = widths.iter().take_while(|&&w| w == 1).count();
    let n_params = step.params.len();
    let inputs = [4u64]; // n=4 -> 0+1+2+3 = 6
    let mut state: Vec<Vec<bool>> = widths.iter().map(|&w| vec![false; w]).collect();
    for (k, &inp) in inputs.iter().enumerate() {
        let slot = pc_width + k;
        if slot < state.len() && state[slot].len() == 64 {
            for b in 0..64 {
                state[slot][b] = (inp >> b) & 1 == 1;
            }
        }
    }
    let mut storage = Default::default();
    let mut steps = 0usize;
    let ret_vals;
    loop {
        let outputs = volar_fuzz::interpreter::ir::eval_ir_circuit_step(
            step,
            &types,
            &circuit.oracles,
            &state,
            &mut storage,
        );
        if outputs[0].iter().any(|&b| b) {
            ret_vals = outputs[1 + n_params..].to_vec();
            break;
        }
        let mut pc = 0usize;
        for j in 0..pc_width {
            if outputs[1 + j][0] {
                pc |= 1 << j;
            }
        }
        let mut vals = Vec::new();
        for j in 0..6usize {
            if 1 + pc_width + j < outputs.len() {
                let slot = &outputs[1 + pc_width + j];
                if slot.len() == 64 {
                    let mut v = 0u64;
                    for (b, &bit) in slot.iter().enumerate() {
                        if bit {
                            v |= 1 << b;
                        }
                    }
                    vals.push(v);
                }
            }
        }
        eprintln!("  step {steps}: next pc = {pc} state64={vals:?}");
        state = outputs[1..1 + n_params].to_vec();
        steps += 1;
        assert!(steps <= 1024, "simple loop did not terminate");
    }
    let mut got = 0u64;
    let mut bitidx = 0;
    'outer: for slot in &ret_vals {
        for &bit in slot {
            if bitidx >= 64 {
                break 'outer;
            }
            if bit {
                got |= 1 << bitidx;
            }
            bitidx += 1;
        }
    }
    eprintln!("simple_loop: circuit={got} (expect 6), {steps} steps");
    assert_eq!(got, 6, "the loop-carried scalar must work");
}

/// Isolation: does a loop-carried local array (write+read across iterations)
/// work through the pipeline?
#[test]
fn loop_array_push_pop_works() {
    let (blocks, mut types) = import_program_to_ir("loop_array.ll", "push_pop");
    let movfuscated = volar_ir_passes::movfuscate::movfuscate_ir(&blocks, &mut types);
    let bit_ty = types
        .0
        .iter()
        .position(|t| {
            matches!(
                t,
                volar_ir_common::IrType::Primitive(volar_ir_common::Type::Bit)
            )
        })
        .map(|i| volar_ir::ir::IRTypeId(i as u32))
        .expect("Bit interned");
    let circuit = volar_ir_passes::lower_to_circuit::lower_to_circuit_ir(
        &movfuscated,
        &bit_ty,
        1,
        volar_ir_passes::lower_to_circuit::LoweringMode::WithTerminationFlag,
    );
    let step = &circuit.blocks[0];
    let widths: Vec<usize> = step
        .params
        .iter()
        .map(|&t| types.value_bit_width(t).expect("data type"))
        .collect();
    let pc_width = widths.iter().take_while(|&&w| w == 1).count();
    let n_params = step.params.len();
    let inputs = [7u64, 9]; // a=7, b=9 -> push 7, push 9, add -> 16
    let mut state: Vec<Vec<bool>> = widths.iter().map(|&w| vec![false; w]).collect();
    for (k, &inp) in inputs.iter().enumerate() {
        let slot = pc_width + k;
        if slot < state.len() && state[slot].len() == 64 {
            for b in 0..64 {
                state[slot][b] = (inp >> b) & 1 == 1;
            }
        }
    }
    let mut storage = Default::default();
    let mut steps = 0usize;
    let ret_vals;
    loop {
        let outputs = volar_fuzz::interpreter::ir::eval_ir_circuit_step(
            step,
            &types,
            &circuit.oracles,
            &state,
            &mut storage,
        );
        if outputs[0].iter().any(|&b| b) {
            ret_vals = outputs[1 + n_params..].to_vec();
            break;
        }
        // DEBUG: dump the PC (the leading 1-bit slots) + the first 64-bit slot (i).
        let mut pc = 0usize;
        for j in 0..pc_width {
            if outputs[1 + j][0] {
                pc |= 1 << j;
            }
        }
        // dump the first 5 64-bit state slots after the pc (a, b, then locals).
        let mut vals = Vec::new();
        for j in 0..8usize {
            let slot = &outputs[1 + pc_width + j];
            if slot.len() == 64 {
                let mut v = 0u64;
                for (b, &bit) in slot.iter().enumerate() {
                    if bit {
                        v |= 1 << b;
                    }
                }
                vals.push(v);
            }
        }
        eprintln!("  step {steps}: next pc = {pc} state64={vals:?}");
        state = outputs[1..1 + n_params].to_vec();
        steps += 1;
        assert!(steps <= 1024);
    }
    let mut got = 0u64;
    let mut bitidx = 0;
    'outer: for slot in &ret_vals {
        for &bit in slot {
            if bitidx >= 64 {
                break 'outer;
            }
            if bit {
                got |= 1 << bitidx;
            }
            bitidx += 1;
        }
    }
    eprintln!("loop_array: circuit={got} (expect 16), {steps} steps");
    assert_eq!(got, 16, "the loop-carried stack array must work");
}

/// Quick probe: dump the entry block's param→slot mapping (no circuit eval).
#[test]
fn s6b_dump_entry_slots() {
    let (blocks, mut types) = import_program_to_ir("evm_stepper.ll", "evm_step");
    let entry_slots =
        volar_ir_passes::movfuscate::movfuscate_entry_param_slots(&blocks, &mut types);
    eprintln!(
        "S6b: entry block — {} params",
        blocks.blocks[0].params.len()
    );
    eprintln!("S6b: entry_slots (param → (slot_start, slot_count)):");
    for (k, &(start, count)) in entry_slots.iter().enumerate() {
        eprintln!("  param {k}: slot {start} (count {count})");
    }
    // Also dump the entry block's terminator (how the args are threaded onward).
    let eb = &blocks.blocks[0];
    match &eb.terminator {
        volar_ir::ir::IRTerminator::Jmp { target } => eprintln!(
            "  entry term: Jmp {:?} nargs={} (block 0 has {} params)",
            target.dest,
            target.args.len(),
            eb.params.len()
        ),
        other => eprintln!("  entry term: {:?}", core::mem::discriminant(other)),
    }
    // Dump the loop-relevant blocks (a JumpCond, or a Jmp back to an earlier block).
    for (i, b) in blocks.blocks.iter().enumerate() {
        let term = match &b.terminator {
            volar_ir::ir::IRTerminator::Jmp { target } => {
                format!("Jmp {:?} nargs={}", target.dest, target.args.len())
            }
            volar_ir::ir::IRTerminator::JumpCond {
                then_target,
                else_target,
                ..
            } => format!(
                "JumpCond then={:?}({}) else={:?}({})",
                then_target.dest,
                then_target.args.len(),
                else_target.dest,
                else_target.args.len()
            ),
            other => format!("{:?}", core::mem::discriminant(other)),
        };
        let is_back_edge = matches!(&b.terminator,
            volar_ir::ir::IRTerminator::Jmp { target }
            if matches!(target.dest, volar_ir::ir::IRBlockTargetId::Block(volar_ir::ir::IRBlockId(j)) if (j as usize) <= i));
        let is_cond = matches!(&b.terminator, volar_ir::ir::IRTerminator::JumpCond { .. });
        if is_back_edge || is_cond {
            eprintln!("  block {i}: {} params, term={term}", b.params.len());
        }
    }
}

/// #5 repro: a loop with a frame-carried array + a register-carried scalar.
/// At -O1 the scalars are in registers (no frame); the array is the only
/// alloca. Does the loop-carried scalar (in registers) + the frame's array
/// work together?
// Known -O1 importer limitation (rustc -O1 unrolls into phi forms the
// importer rejects) — pre-existing rot, see portal-co/volar-ir#8.
#[test]
#[ignore = "volar-llvm-vaffle-import rejects -O1 unrolled-phi forms; see portal-co/volar-ir#8"]
fn frame_scalar_at_o1_works() {
    let (blocks, mut types) = import_program_to_ir("frame_scalar_o1.ll", "sum_with_array");
    let entry_slots =
        volar_ir_passes::movfuscate::movfuscate_entry_param_slots(&blocks, &mut types);
    let movfuscated = volar_ir_passes::movfuscate::movfuscate_ir(&blocks, &mut types);
    let bit_ty = types
        .0
        .iter()
        .position(|t| {
            matches!(
                t,
                volar_ir_common::IrType::Primitive(volar_ir_common::Type::Bit)
            )
        })
        .map(|i| volar_ir::ir::IRTypeId(i as u32))
        .expect("Bit interned");
    let circuit = volar_ir_passes::lower_to_circuit::lower_to_circuit_ir(
        &movfuscated,
        &bit_ty,
        1,
        volar_ir_passes::lower_to_circuit::LoweringMode::WithTerminationFlag,
    );
    let step = &circuit.blocks[0];
    let widths: Vec<usize> = step
        .params
        .iter()
        .map(|&t| types.value_bit_width(t).expect("data type"))
        .collect();
    let pc_width = widths.iter().take_while(|&&w| w == 1).count();
    let n_params = step.params.len();
    // sum_with_array(a0=10, a1=20, n=3): 10 + 20 + 10 = 40 (i=0,1,2 -> arr[0],arr[1],arr[0])
    let inputs = [10u64, 20, 3];
    let mut state: Vec<Vec<bool>> = widths.iter().map(|&w| vec![false; w]).collect();
    for (k, &inp) in inputs.iter().enumerate() {
        if k >= entry_slots.len() {
            break;
        }
        let (slot_start, _c) = entry_slots[k];
        let slot = pc_width + slot_start;
        if slot < state.len() && state[slot].len() == 64 {
            for b in 0..64 {
                state[slot][b] = (inp >> b) & 1 == 1;
            }
        }
    }
    let mut storage = Default::default();
    let mut steps = 0usize;
    let ret_vals;
    loop {
        let outputs = volar_fuzz::interpreter::ir::eval_ir_circuit_step(
            step,
            &types,
            &circuit.oracles,
            &state,
            &mut storage,
        );
        if outputs[0].iter().any(|&b| b) {
            ret_vals = outputs[1 + n_params..].to_vec();
            break;
        }
        state = outputs[1..1 + n_params].to_vec();
        steps += 1;
        assert!(
            steps <= 1024,
            "the frame+register scalar loop did not terminate"
        );
    }
    let mut got = 0u64;
    let mut bitidx = 0;
    'outer: for slot in &ret_vals {
        for &bit in slot {
            if bitidx >= 64 {
                break 'outer;
            }
            if bit {
                got |= 1 << bitidx;
            }
            bitidx += 1;
        }
    }
    eprintln!("frame_scalar: circuit={got} (expect 40), {steps} steps");
    assert_eq!(got, 40, "the frame+register scalar loop must work");
}

/// #5 bisection: the minimal stepper (just PUSH1 + ADD) at -O0.
#[test]
fn stepper_min_at_o0() {
    let (blocks, mut types) = import_program_to_ir("stepper_min_o0.ll", "step_min");
    // #5 instrumentation: find the blocks with a StorageWrite (the s0/s1/sp
    // writes) and dump their terminators, to pick a watch target.
    for (i, b) in blocks.blocks.iter().enumerate() {
        let n_writes = b
            .stmts
            .iter()
            .filter(|s| matches!(&s.kind, volar_ir::ir::IRStmt::StorageWrite { .. }))
            .count();
        if n_writes > 0 && i < 60 {
            eprintln!(
                "  sm blk {i}: {n_writes} storage writes, term={:?}",
                b.terminator
            );
        }
    }
    if std::env::var("SM_DUMP_ONLY").is_ok() {
        // #5 instrumentation: dump block 67's (loop header) params + first stmts.
        if let Some(b67) = blocks.blocks.get(67) {
            eprintln!(
                "  sm blk 67 params ({}): {:?}",
                b67.params.len(),
                b67.params
            );
            eprintln!("  sm blk 67 first stmts:");
            for (i, st) in b67.stmts.iter().take(20).enumerate() {
                let short = format!("{:?}", st.kind);
                eprintln!("    s{i}: {}", &short[..short.len().min(120)]);
            }
        }
        // #5 instrumentation: dump block 23's statements (the loop-back block
        // that reads s0/s1/sp from storage and passes them as args to 67).
        if let Some(b23) = blocks.blocks.get(23) {
            eprintln!("  sm blk 23 stmts ({}):", b23.stmts.len());
            for (i, st) in b23.stmts.iter().enumerate() {
                let short = format!("{:?}", st.kind);
                eprintln!("    s{i}: {}", &short[..short.len().min(140)]);
            }
        }
        return;
    }
    let entry_slots =
        volar_ir_passes::movfuscate::movfuscate_entry_param_slots(&blocks, &mut types);
    // #5 instrumentation: watch the loop-header block (67)'s first few params
    // (the loop-carried s0/s1/sp as block args), to confirm whether the
    // block-arg path carries the update.
    let watch: Vec<(usize, u32)> = (0..8u32).map(|v| (67usize, v)).collect();
    let (movfuscated, watch_results) =
        volar_ir_passes::movfuscate::movfuscate_ir_with_watch(&blocks, &mut types, &watch);
    let bit_ty = types
        .0
        .iter()
        .position(|t| {
            matches!(
                t,
                volar_ir_common::IrType::Primitive(volar_ir_common::Type::Bit)
            )
        })
        .map(|i| volar_ir::ir::IRTypeId(i as u32))
        .expect("Bit interned");
    let circuit = volar_ir_passes::lower_to_circuit::lower_to_circuit_ir(
        &movfuscated,
        &bit_ty,
        1,
        volar_ir_passes::lower_to_circuit::LoweringMode::WithTerminationFlag,
    );
    let step = &circuit.blocks[0];
    eprintln!(
        "stepper_min: step block — {} params, {} stmts, watch={:?}",
        step.params.len(),
        step.stmts.len(),
        watch_results
    );
    let widths: Vec<usize> = step
        .params
        .iter()
        .map(|&t| types.value_bit_width(t).expect("data type"))
        .collect();
    let pc_width = widths.iter().take_while(|&&w| w == 1).count();
    let n_params = step.params.len();
    // step_min(code=[PUSH1,5,PUSH1,3,ADD], steps=5): 5+3 = 8.
    let inputs = [0x60u64, 5, 0x60, 3, 0x01, 5];
    let mut state: Vec<Vec<bool>> = widths.iter().map(|&w| vec![false; w]).collect();
    for (k, &inp) in inputs.iter().enumerate() {
        if k >= entry_slots.len() {
            break;
        }
        let (slot_start, _c) = entry_slots[k];
        let slot = pc_width + slot_start;
        if slot < state.len() && state[slot].len() == 64 {
            for b in 0..64 {
                state[slot][b] = (inp >> b) & 1 == 1;
            }
        }
    }
    // #5 instrumentation: the watched combined vars (the loop-header block
    // 67's params — the loop-carried s0/s1/sp as block args).
    let watch_vars: Vec<u32> = watch_results.iter().map(|&(_, _, c)| c).collect();
    let mut storage = Default::default();
    let mut steps = 0usize;
    let ret_vals;
    loop {
        let (outputs, watched) = volar_fuzz::interpreter::ir::eval_ir_circuit_step_with_watch(
            step,
            &types,
            &circuit.oracles,
            &state,
            &mut storage,
            &watch_vars,
        );
        if outputs[0].iter().any(|&b| b) {
            ret_vals = outputs[1 + n_params..].to_vec();
            break;
        }
        if steps < 12 {
            let mut pc = 0usize;
            for j in 0..pc_width {
                if outputs[1 + j][0] {
                    pc |= 1 << j;
                }
            }
            // the watched loop-header params (the block-arg s0/s1/sp)
            let mut wv = Vec::new();
            for &(var, ref val) in watched.iter() {
                let mut v = 0u64;
                for (b, &bit) in val.iter().take(64).enumerate() {
                    if bit {
                        v |= 1 << b;
                    }
                }
                wv.push(format!("w{var}={v:#x}"));
            }
            eprintln!("  sm step {steps}: pc={pc} watch={wv:?}");
        }
        state = outputs[1..1 + n_params].to_vec();
        steps += 1;
        assert!(steps <= 1024, "the minimal stepper did not terminate");
    }
    let mut got = 0u64;
    let mut bitidx = 0;
    'outer: for slot in &ret_vals {
        for &bit in slot {
            if bitidx >= 64 {
                break 'outer;
            }
            if bit {
                got |= 1 << bitidx;
            }
            bitidx += 1;
        }
    }
    eprintln!("stepper_min: circuit={got} (expect 8), {steps} steps");
    assert_eq!(got, 8, "the minimal stepper must match native");
}

/// #5 repro at -O0: the frame-carried scalars (spilled to the frame at -O0).
#[test]
fn frame_scalar_at_o0_works() {
    let (blocks, mut types) = import_program_to_ir("frame_scalar_o0.ll", "sum_with_array");
    let entry_slots =
        volar_ir_passes::movfuscate::movfuscate_entry_param_slots(&blocks, &mut types);
    let movfuscated = volar_ir_passes::movfuscate::movfuscate_ir(&blocks, &mut types);
    let bit_ty = types
        .0
        .iter()
        .position(|t| {
            matches!(
                t,
                volar_ir_common::IrType::Primitive(volar_ir_common::Type::Bit)
            )
        })
        .map(|i| volar_ir::ir::IRTypeId(i as u32))
        .expect("Bit interned");
    let circuit = volar_ir_passes::lower_to_circuit::lower_to_circuit_ir(
        &movfuscated,
        &bit_ty,
        1,
        volar_ir_passes::lower_to_circuit::LoweringMode::WithTerminationFlag,
    );
    let step = &circuit.blocks[0];
    eprintln!(
        "frame_scalar_o0: step block — {} params, {} stmts",
        step.params.len(),
        step.stmts.len()
    );
    let widths: Vec<usize> = step
        .params
        .iter()
        .map(|&t| types.value_bit_width(t).expect("data type"))
        .collect();
    let pc_width = widths.iter().take_while(|&&w| w == 1).count();
    let n_params = step.params.len();
    // sum_with_array(a0=10, a1=20, n=3): 10 + 20 + 10 = 40.
    let inputs = [10u64, 20, 3];
    let mut state: Vec<Vec<bool>> = widths.iter().map(|&w| vec![false; w]).collect();
    for (k, &inp) in inputs.iter().enumerate() {
        if k >= entry_slots.len() {
            break;
        }
        let (slot_start, _c) = entry_slots[k];
        let slot = pc_width + slot_start;
        if slot < state.len() && state[slot].len() == 64 {
            for b in 0..64 {
                state[slot][b] = (inp >> b) & 1 == 1;
            }
        }
    }
    let mut storage = Default::default();
    let mut steps = 0usize;
    let ret_vals;
    loop {
        let outputs = volar_fuzz::interpreter::ir::eval_ir_circuit_step(
            step,
            &types,
            &circuit.oracles,
            &state,
            &mut storage,
        );
        if outputs[0].iter().any(|&b| b) {
            ret_vals = outputs[1 + n_params..].to_vec();
            break;
        }
        state = outputs[1..1 + n_params].to_vec();
        steps += 1;
        assert!(steps <= 1024, "the frame-scalar loop did not terminate");
    }
    let mut got = 0u64;
    let mut bitidx = 0;
    'outer: for slot in &ret_vals {
        for &bit in slot {
            if bitidx >= 64 {
                break 'outer;
            }
            if bit {
                got |= 1 << bitidx;
            }
            bitidx += 1;
        }
    }
    eprintln!("frame_scalar_o0: circuit={got} (expect 40), {steps} steps");
    assert_eq!(got, 40, "the frame-scalar loop must work");
}

/// S6b: the bounded EVM stepper (a fixed opcode subset) through the pipeline.
///
/// **Landed** (via the all-scalars sidestep): the EVM stepper compiles
/// through the full pipeline (LLVM → VAFFLE → IR → movfuscate → step circuit)
/// and matches native bit-exactly. The stepper keeps all state as bare scalars
/// (no arrays/structs → no frame) at `-C opt-level=1` (registers, not allocas),
/// sidestepping the frame-carried-scalar propagation bug
/// (https://github.com/portal-co/volar-ir/issues/5) — the general (frame-based)
/// stepper is the open follow-up.
#[test]
fn s6b_evm_stepper_reaches_a_step_circuit() {
    run_evm_stepper("evm_stepper.ll", "(-O1, the all-scalars sidestep)");
}

/// #5: the EVM stepper at -O0 (the frame — the scalars spilled to allocas).
#[test]
fn s6b_evm_stepper_at_o0() {
    run_evm_stepper("evm_stepper_o0.ll", "(-O0, the frame)");
}

/// Drive the EVM stepper (the given LLVM IR) through the pipeline and check
/// the step circuit matches native.
fn run_evm_stepper(file: &str, label: &str) {
    let (blocks, mut types) = import_program_to_ir(file, "evm_step");
    eprintln!("S6b: lowered to IR — {} blocks", blocks.blocks.len());
    let movfuscated = volar_ir_passes::movfuscate::movfuscate_ir(&blocks, &mut types);
    let bit_ty = types
        .0
        .iter()
        .position(|t| {
            matches!(
                t,
                volar_ir_common::IrType::Primitive(volar_ir_common::Type::Bit)
            )
        })
        .map(|i| volar_ir::ir::IRTypeId(i as u32))
        .expect("Bit interned");
    let circuit = volar_ir_passes::lower_to_circuit::lower_to_circuit_ir(
        &movfuscated,
        &bit_ty,
        1,
        volar_ir_passes::lower_to_circuit::LoweringMode::WithTerminationFlag,
    );
    let step = &circuit.blocks[0];
    eprintln!(
        "S6b: step block — {} params, {} stmts",
        step.params.len(),
        step.stmts.len()
    );
    // Differential: run PUSH1 5, PUSH1 3, ADD, PUSH1 2, MUL = (5+3)*2 = 16.
    let widths: Vec<usize> = step
        .params
        .iter()
        .map(|&t| types.value_bit_width(t).expect("data type"))
        .collect();
    let pc_width = widths.iter().take_while(|&&w| w == 1).count();
    let n_params = step.params.len();
    // The entry block's param→slot mapping (movfuscation exposes it so the
    // caller can initialize the state; the state is then carried generically).
    let entry_slots =
        volar_ir_passes::movfuscate::movfuscate_entry_param_slots(&blocks, &mut types);
    eprintln!(
        "S6b: entry block — {} params, entry_slots={entry_slots:?}",
        blocks.blocks[0].params.len()
    );
    // evm_step(code0..7, storage0..1, steps): [PUSH1 5, PUSH1 3, ADD, PUSH1 2, MUL], store=[0,0], steps=8.
    let inputs = [0x60u64, 5, 0x60, 3, 0x01, 0x60, 2, 0x02, 0, 0, 8];
    let mut state: Vec<Vec<bool>> = widths.iter().map(|&w| vec![false; w]).collect();
    for (k, &inp) in inputs.iter().enumerate() {
        if k >= entry_slots.len() {
            break;
        }
        let (slot_start, _slot_count) = entry_slots[k];
        let slot = pc_width + slot_start;
        if slot < state.len() && state[slot].len() == 64 {
            for b in 0..64 {
                state[slot][b] = (inp >> b) & 1 == 1;
            }
        }
    }
    let mut storage = Default::default();
    let mut steps = 0usize;
    let ret_vals;
    loop {
        let outputs = volar_fuzz::interpreter::ir::eval_ir_circuit_step(
            step,
            &types,
            &circuit.oracles,
            &state,
            &mut storage,
        );
        let done = outputs[0].iter().any(|&b| b);
        if done {
            ret_vals = outputs[1 + n_params..].to_vec();
            break;
        }
        state = outputs[1..1 + n_params].to_vec();
        steps += 1;
        assert!(steps <= 1024, "step circuit did not return in 1024 steps");
    }
    let mut got = 0u64;
    let mut bitidx = 0;
    'outer: for slot in &ret_vals {
        for &bit in slot {
            if bitidx >= 64 {
                break 'outer;
            }
            if bit {
                got |= 1 << bitidx;
            }
            bitidx += 1;
        }
    }
    let native = 16u64;
    eprintln!("S6b: step circuit returned after {steps} steps; circuit={got} native={native}");
    assert_eq!(
        got, native,
        "S6b differential: the EVM stepper circuit must match native"
    );
}

#[test]
fn s2_reference_reaches_a_step_circuit() {
    let (blocks, mut types) = import_to_ir();
    eprintln!("S2: lowered to IR — {} blocks", blocks.blocks.len());
    for (i, b) in blocks.blocks.iter().enumerate() {
        eprintln!(
            "S2 IR block {i}: {} params, term={:?}",
            b.params.len(),
            match &b.terminator {
                volar_ir::ir::IRTerminator::Jmp { target } => format!("Jmp {:?}", target.dest),
                volar_ir::ir::IRTerminator::JumpCond {
                    then_target,
                    else_target,
                    ..
                } => format!(
                    "JumpCond then={:?} else={:?}",
                    then_target.dest, else_target.dest
                ),
                volar_ir::ir::IRTerminator::JumpTable { .. } => "JumpTable".to_string(),
                _ => "other".to_string(),
            }
        );
    }

    // Virtualize is optional for a single function; the Steel pipeline's
    // step-granularity PESAT needs the movfuscated step circuit.
    // Reproduce the movfuscation's DFS reorder to map new-index -> original.
    let mut dfs_order: Vec<usize> = Vec::new();
    {
        let mut seen = vec![false; blocks.blocks.len()];
        let mut stack = vec![0usize];
        while let Some(b) = stack.pop() {
            if seen[b] {
                continue;
            }
            seen[b] = true;
            dfs_order.push(b);
            let mut push = |d: &volar_ir::ir::IRBlockTargetId| {
                if let volar_ir::ir::IRBlockTargetId::Block(id) = d {
                    stack.push(id.0 as usize);
                }
            };
            match &blocks.blocks[b].terminator {
                volar_ir::ir::IRTerminator::Jmp { target } => push(&target.dest),
                volar_ir::ir::IRTerminator::JumpCond {
                    then_target,
                    else_target,
                    ..
                } => {
                    push(&then_target.dest);
                    push(&else_target.dest);
                }
                volar_ir::ir::IRTerminator::JumpTable { cases, .. } => {
                    for t in cases.values() {
                        push(&t.dest);
                    }
                }
                _ => {}
            }
        }
    }
    eprintln!("S2: DFS order (new->old): {:?}", dfs_order);
    for (new, &old) in dfs_order.iter().enumerate() {
        let is_ret = matches!(
            &blocks.blocks[old].terminator,
            volar_ir::ir::IRTerminator::Jmp { target }
                if target.dest == volar_ir::ir::IRBlockTargetId::Return
        );
        eprintln!("S2 new-block {new} (orig {old}): is_return={is_ret}");
    }
    let (movfuscated, _boundaries, _accum) =
        volar_ir_passes::movfuscate::movfuscate_ir_with_boundary(&blocks, &mut types);
    eprintln!("S2: movfuscated — {} blocks", movfuscated.blocks.len());

    // Find/intern the Bit type for the termination flag.
    let bit_ty = types
        .0
        .iter()
        .position(|t| {
            matches!(
                t,
                volar_ir_common::IrType::Primitive(volar_ir_common::Type::Bit)
            )
        })
        .map(|i| volar_ir::ir::IRTypeId(i as u32))
        .expect("Bit type interned by movfuscate_ir");
    let circuit = volar_ir_passes::lower_to_circuit::lower_to_circuit_ir(
        &movfuscated,
        &bit_ty,
        1,
        volar_ir_passes::lower_to_circuit::LoweringMode::WithTerminationFlag,
    );
    eprintln!("S2: step circuit — {} blocks", circuit.blocks.len());
    assert_eq!(
        circuit.blocks.len(),
        1,
        "movfuscation yields one step block"
    );
    let step = &circuit.blocks[0];
    eprintln!(
        "S2: step block — {} params, {} stmts",
        step.params.len(),
        step.stmts.len()
    );
    let widths: Vec<usize> = step
        .params
        .iter()
        .map(|&t| types.value_bit_width(t).expect("data type"))
        .collect();
    eprintln!(
        "S2: param widths (first 12 of {}): {:?}",
        widths.len(),
        &widths[..widths.len().min(12)]
    );
    if let volar_ir::ir::IRTerminator::Jmp { target } = &step.terminator {
        eprintln!(
            "S2: terminator dest={:?} return args={}",
            target.dest,
            target.args.len()
        );
    }

    // S2 gate: the per-step trace evaluates correctly (differential vs the
    // reference interpreter). Drive the movfuscated step circuit from the
    // all-zero entry state (pc=0 -> block 0; all program inputs zero) until
    // it returns, and check the trace is consistent and terminates.
    let n_params = step.params.len();
    let mut state: Vec<Vec<bool>> = widths.iter().map(|&w| vec![false; w]).collect();
    let mut storage = Default::default();
    let mut steps = 0usize;
    let ret_vals;
    loop {
        let outputs = volar_fuzz::interpreter::ir::eval_ir_circuit_step(
            step,
            &types,
            &circuit.oracles,
            &state,
            &mut storage,
        );
        // outputs = [done, ...next_state (n_params), ...ret_vals].
        if steps == 0 {
            eprintln!(
                "S2 outputs[0..14] bit0s: {:?}",
                outputs[..14.min(outputs.len())]
                    .iter()
                    .map(|v| v.first().copied().unwrap_or(false) as u8)
                    .collect::<Vec<_>>()
            );
        }
        let done = outputs[0].iter().any(|&b| b);
        // pc_width = number of leading 1-bit (Bit) params.
        let pc_width = widths.iter().take_while(|&&w| w == 1).count();
        let pc: u32 = outputs[1..1 + pc_width]
            .iter()
            .enumerate()
            .map(|(i, b)| (b.first().copied().unwrap_or(false) as u32) << i)
            .sum();
        if steps < 6 {
            eprintln!("S2 step {steps}: done={done} next_pc={pc} (pc_width={pc_width})");
        }
        if done {
            ret_vals = outputs[1 + n_params..].to_vec();
            break;
        }
        state = outputs[1..1 + n_params].to_vec();
        steps += 1;
        assert!(steps <= 32, "step circuit did not return in 32 steps");
    }
    eprintln!("S2: step circuit returned after {steps} steps");

    // S2 differential (non-zero input): drive the circuit with key=5, value=7,
    // n=2, path=[11,13,17,19] and check the return value matches the native
    // reference. The state slots after the 4 PC bits are the program inputs
    // (the trampoline's 7 params), one u64 slot each.
    let run = |inputs: [u64; 7]| -> u64 {
        let mut state: Vec<Vec<bool>> = widths.iter().map(|&w| vec![false; w]).collect();
        for (k, &inp) in inputs.iter().enumerate() {
            let slot = 4 + k; // skip the 4 PC bits
            if slot < state.len() && state[slot].len() == 64 {
                for b in 0..64 {
                    state[slot][b] = (inp >> b) & 1 == 1;
                }
            }
        }
        let mut storage = Default::default();
        let mut steps = 0usize;
        loop {
            let outputs = volar_fuzz::interpreter::ir::eval_ir_circuit_step(
                step,
                &types,
                &circuit.oracles,
                &state,
                &mut storage,
            );
            let done = outputs[0].iter().any(|&b| b);
            if done {
                // The return value (a u64) is the ret_vals segment: reassemble.
                let rv = &outputs[1 + n_params..];
                let mut acc = 0u64;
                // ret_vals may be one u64 slot or bit-decomposed; take the
                // first 64 bits across the segment.
                let mut bitidx = 0;
                'outer: for slot in rv {
                    for &bit in slot {
                        if bitidx >= 64 {
                            break 'outer;
                        }
                        if bit {
                            acc |= 1 << bitidx;
                        }
                        bitidx += 1;
                    }
                }
                return acc;
            }
            state = outputs[1..1 + n_params].to_vec();
            steps += 1;
            assert!(steps <= 32, "step circuit did not return in 32 steps");
        }
    };
    let got = run([5, 7, 2, 11, 13, 17, 19]);
    let want = reference_native(5, 7, 2, [11, 13, 17, 19]);
    eprintln!("S2: non-zero input -> circuit={got:#x} native={want:#x}");
    assert_eq!(got, want, "differential: circuit must match native");
}

/// #5 diagnostic: dump the VAFFLE entry function's CFG before and after
/// `ssa_ify_module` for `stepper_min_o0.ll`, focusing on block params,
/// terminator args, and VAFFLE_SSA_SPILL traffic. Gated on `SM_VAFFLE_DUMP`;
/// panics with the dump so it shows up in test output without building or
/// evaluating the movfuscated circuit.
#[test]
fn stepper_min_vaffle_dump() {
    if std::env::var("SM_VAFFLE_DUMP").is_err() {
        return;
    }
    let ll = std::fs::read_to_string(artifacts().join("stepper_min_o0.ll")).expect("ll");
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            ll.as_bytes(),
            "stepper_min_o0.ll",
        ))
        .expect("LLVM IR parses");
    let vaffle = volar_llvm_vaffle_import::import_module(&module, &["step_min"]).expect("import");

    let mut d = String::new();
    d.push_str("================ PRE ssa_ify ================\n");
    dump_vaffle_func(&vaffle, &mut d);
    let ssa = volar_vaffle_target::vaffle_ssa::ssa_ify_module(&vaffle);
    d.push_str("================ POST ssa_ify ================\n");
    dump_vaffle_func(&ssa, &mut d);
    panic!("SM_VAFFLE_DUMP\n{}", d);
}

fn dump_vaffle_func<P: Clone + Default>(m: &vaffle::Module<P>, d: &mut String) {
    use vaffle::{FuncDecl, Terminator, Value};
    const SPILL: volar_ir_common::StorageId = volar_ir_common::StorageId::VAFFLE_SSA_SPILL;
    let Some(fi) = m.funcs.iter().position(|f| matches!(f, FuncDecl::Body(_))) else {
        d.push_str("no body functions\n");
        return;
    };
    let FuncDecl::Body(body) = &m.funcs[fi] else {
        return;
    };
    let _ = &body.sig;
    let entry = body.entry;
    // Reverse map: ValueId -> owning block (params + stmts), for labeling.
    let mut owner = vec![usize::MAX; body.values.len()];
    for (bi, b) in body.blocks.iter().enumerate() {
        for &(vid, _) in &b.params {
            owner[vid.0] = bi;
        }
        for &vid in &b.stmts {
            owner[vid.0] = bi;
        }
    }
    for (bi, b) in body.blocks.iter().enumerate() {
        use core::fmt::Write;
        let _ = writeln!(
            d,
            "--- block {}{} ---",
            bi,
            if bi == entry.0 { " (entry)" } else { "" }
        );
        let _ = writeln!(
            d,
            "  params: {}",
            b.params
                .iter()
                .map(|(vid, ty)| format!("v{}:t{}", vid.0, ty.0))
                .collect::<Vec<_>>()
                .join(", ")
        );
        for &svid in &b.stmts {
            let kind = match &body.values[svid.0].kind {
                Value::Op(volar_ir_common::Stmt::StorageRead { storage, addr, .. }) => {
                    format!(
                        "StorageRead st{} addr=v{}{}",
                        storage.0,
                        addr.0,
                        if *storage == SPILL { " <SPILL>" } else { "" }
                    )
                }
                Value::Op(volar_ir_common::Stmt::StorageWrite {
                    storage, addr, src, ..
                }) => {
                    format!(
                        "StorageWrite st{} addr=v{} src=v{}{}",
                        storage.0,
                        addr.0,
                        src.0,
                        if *storage == SPILL { " <SPILL>" } else { "" }
                    )
                }
                Value::Op(volar_ir_common::Stmt::Const(c, ty)) => {
                    format!("Const lo={} t{}", c.lo, ty.0)
                }
                Value::Op(volar_ir_common::Stmt::Poly { ty, .. }) => format!("Poly t{}", ty.0),
                Value::PtrLoad { ptr, .. } => format!("PtrLoad ptr=v{}", ptr.0),
                Value::PtrStore { ptr, val } => {
                    format!("PtrStore ptr=v{} val=v{}", ptr.0, val.0)
                }
                Value::Param { block, idx, .. } => format!("ParamRef b{}#{}", block.0, idx),
                Value::Call { func, .. } => format!("Call f{}", func.0),
                Value::Output { value, idx } => format!("Output v{}#{}", value.0, idx),
                Value::StackAlloc { storage, .. } => format!("StackAlloc st{}", storage.0),
                other => format!("{other:?}"),
            };
            let own = owner[svid.0];
            let _ = writeln!(
                d,
                "  v{} [b{}] = {}",
                svid.0,
                if own == usize::MAX {
                    "?".to_string()
                } else {
                    own.to_string()
                },
                kind
            );
        }
        match &b.terminator {
            Terminator::Jump(t) => {
                let _ = writeln!(
                    d,
                    "  term: Jump -> b{} args=[{}]",
                    t.block.0,
                    t.args
                        .iter()
                        .map(|v| format!("v{}", v.0))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            Terminator::IfNonzero {
                cond,
                then_target,
                else_target,
            } => {
                let fmt_t = |t: &vaffle::Target| {
                    format!(
                        "b{} args=[{}]",
                        t.block.0,
                        t.args
                            .iter()
                            .map(|v| format!("v{}", v.0))
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                };
                let _ = writeln!(
                    d,
                    "  term: IfNonzero v{} then {} else {}",
                    cond.0,
                    fmt_t(then_target),
                    fmt_t(else_target)
                );
            }
            Terminator::Return { values } => {
                let _ = writeln!(
                    d,
                    "  term: Return args=[{}]",
                    values
                        .iter()
                        .map(|v| format!("v{}", v.0))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            other => {
                let _ = writeln!(d, "  term: {other:?}");
            }
        }
    }
}

/// #5 decisive bisection, generalized: eval any imported program's RAW
/// (pre-movfuscate) IRBlocks with the reference CFG interpreter.
fn eval_pre_movfuscate(file: &str, entry: &str, args: &[u64], expect: u64) {
    let (blocks, types) = import_program_to_ir(file, entry);
    let entry_block = &blocks.blocks[0];
    let widths: Vec<usize> = entry_block
        .params
        .iter()
        .map(|&t| types.value_bit_width(t).expect("param width"))
        .collect();
    eprintln!(
        "{file} pre-movf: {} entry params, widths {:?}",
        entry_block.params.len(),
        &widths[..widths.len().min(8)]
    );
    let inputs: Vec<Vec<bool>> = if widths.len() == args.len() && widths.iter().all(|&w| w == 64) {
        args.iter()
            .map(|&inp| (0..64).map(|b| (inp >> b) & 1 == 1).collect())
            .collect()
    } else {
        panic!("unexpected entry param shape: {widths:?} for {args:?}");
    };
    let out = volar_fuzz::interpreter::ir::eval_ir(&blocks, &types, &inputs);
    match out {
        Some(ret) => {
            let mut got = 0u64;
            let mut bitidx = 0;
            'outer: for slot in &ret {
                for &bit in slot {
                    if bitidx >= 64 {
                        break 'outer;
                    }
                    if bit {
                        got |= 1 << bitidx;
                    }
                    bitidx += 1;
                }
            }
            eprintln!("{file} pre-movf eval: got {got} (expect {expect})");
            assert_eq!(
                got, expect,
                "{file}: pre-movfuscate CFG eval must match native"
            );
        }
        None => panic!("{file}: pre-movfuscate CFG eval did not terminate (frontend bug)"),
    }
}

/// count_to(3) = 0+0+1+2 = 3 through the raw pre-movfuscate CFG.
#[test]
fn simple_loop_pre_movfuscate_eval() {
    eval_pre_movfuscate("simple_loop.ll", "count_to", &[3], 3);
}

/// #5 decisive bisection: evaluate the RAW (pre-movfuscate) IRBlocks for
/// stepper_min with the reference CFG interpreter. If this already diverges
/// from native, the bug is in the frontend (importer / vaffle_ssa /
/// lower_to_ir) — not movfuscate. Native: PUSH1 5, PUSH1 3, ADD => 8.
#[test]
fn stepper_min_pre_movfuscate_eval() {
    eval_pre_movfuscate(
        "stepper_min_o0.ll",
        "step_min",
        &[0x60, 5, 0x60, 3, 0x01, 5],
        8,
    );
}

/// #5 bisect v1: PUSH1-only MUX writes (no ADD, no panic paths).
/// Native: PUSH1 5, PUSH1 3 (no ADD) => sp=2, top = s1 = 3.
#[test]
fn v1_o0_pre_movfuscate_eval() {
    eval_pre_movfuscate("v1_o0.ll", "step_v1", &[0x60, 5, 0x60, 3, 0x01, 5], 3);
}

/// v2 = v1 + the ADD branch. Native: 5+3 => s0=8, sp=1 => 8.
#[test]
fn v2_o0_pre_movfuscate_eval() {
    eval_pre_movfuscate("v2_o0.ll", "step_v2", &[0x60, 5, 0x60, 3, 0x01, 5], 8);
}

/// v3 = v1 + STOP/break. Native: PUSH1 5, PUSH1 3 => 3.
#[test]
fn v3_o0_pre_movfuscate_eval() {
    eval_pre_movfuscate("v3_o0.ll", "step_v3", &[0x60, 5, 0x60, 3, 0x01, 5], 3);
}

/// v4 = exact stepper_min copy (sanity: must FAIL with 0).
#[test]
fn v4_o0_pre_movfuscate_eval() {
    eval_pre_movfuscate("v4_o0.ll", "step_v4", &[0x60, 5, 0x60, 3, 0x01, 5], 8);
}

/// v5 = v1 + the 3-branch chain (STOP, PUSH1, ADD). Native: 8.
#[test]
fn v5_o0_pre_movfuscate_eval() {
    eval_pre_movfuscate("v5_o0.ll", "step_v5", &[0x60, 5, 0x60, 3, 0x01, 5], 8);
}

/// #5: dump the importer's storage assignment for the two semantically
/// identical modules (they differ only in panic-metadata string length).
#[test]
fn sm_storage_layout_compare() {
    for (file, entry) in [("stepper_min_o0.ll", "step_min"), ("v4_o0.ll", "step_v4")] {
        let ll = std::fs::read_to_string(artifacts().join(file)).expect(file);
        let context = Context::create();
        let module = context
            .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
                ll.as_bytes(),
                file,
            ))
            .expect("LLVM IR parses");
        let vaffle = volar_llvm_vaffle_import::import_module(&module, &[entry]).expect("import");
        eprintln!("=== {file} ===");
        eprintln!("  pre_init segments: {}", vaffle.pre_init.len());
        for seg in vaffle.pre_init.iter().take(40) {
            eprintln!("    seg: {:?}", seg);
        }
        let mut fi = None;
        for (i, f) in vaffle.funcs.iter().enumerate() {
            if let vaffle::FuncDecl::Body(b) = f {
                let name = match &vaffle.funcs[i] {
                    vaffle::FuncDecl::Body(_) => "?",
                    _ => "?",
                };
                let n_blocks = b.blocks.len();
                let mut storages = std::collections::BTreeSet::new();
                for blk in &b.blocks {
                    for &vid in &blk.stmts {
                        match &b.values[vid.0].kind {
                            vaffle::Value::StackAlloc { storage, sp, .. } => {
                                storages.insert(("alloc", storage.0, sp.0));
                            }
                            vaffle::Value::Op(volar_ir_common::Stmt::StorageRead {
                                storage,
                                ..
                            })
                            | vaffle::Value::Op(volar_ir_common::Stmt::StorageWrite {
                                storage,
                                ..
                            }) => {
                                storages.insert(("acc", storage.0, 0));
                            }
                            _ => {}
                        }
                    }
                }
                eprintln!("  func[{i}] {name}: {n_blocks} blocks, storages={storages:?}");
                fi = Some(i);
            }
        }
        let _ = fi;
    }
}

/// #5: dump per-block summaries of the lowered IRBlocks for both modules
/// (gated on SM_IRDIFF, writes /tmp/sm_ir_{tag}.txt).
#[test]
fn sm_ir_diff_dump() {
    if std::env::var("SM_IRDIFF").is_err() {
        return;
    }
    for (file, entry, tag) in [
        ("stepper_min_o0.ll", "step_min", "bad"),
        ("v4_o0.ll", "step_v4", "good"),
    ] {
        let (blocks, _types) = import_program_to_ir(file, entry);
        let mut d = String::new();
        for (i, b) in blocks.blocks.iter().enumerate() {
            use core::fmt::Write;
            let _ = writeln!(
                d,
                "blk {} params={} stmts={} term_kind={}",
                i,
                b.params.len(),
                b.stmts.len(),
                match &b.terminator {
                    volar_ir::ir::IRTerminator::Jmp { .. } => "Jmp",
                    volar_ir::ir::IRTerminator::JumpCond { .. } => "JumpCond",
                    volar_ir::ir::IRTerminator::JumpTable { .. } => "JumpTable",
                    _ => "other",
                }
            );
        }
        std::fs::write(format!("/tmp/sm_ir_{tag}.txt"), d).unwrap();
        // Also dump a full Debug of every block's stmts, hashed per block so
        // diffs localize: first 400 chars of each stmt's Debug.
        let mut full = String::new();
        for (i, b) in blocks.blocks.iter().enumerate() {
            use core::fmt::Write;
            let _ = writeln!(full, "=== blk {} ({} stmts)", i, b.stmts.len());
            for (j, st) in b.stmts.iter().enumerate() {
                let s = format!("{:?}", st.kind);
                let _ = writeln!(full, "  {} {}", j, &s[..s.len().min(200)]);
            }
            let t = format!("{:?}", b.terminator);
            let _ = writeln!(full, " TERM {}", &t[..t.len().min(400)]);
        }
        std::fs::write(format!("/tmp/sm_ir_full_{tag}.txt"), full).unwrap();
    }
}

/// #5: the lowered IRs are byte-identical yet eval differs. Diff the rest of
/// the VirtOutput-irrelevant state: types table, oracles, pre_init. Also swap
/// module names at the inkwell layer to see if the failure follows the name
/// or the bytes.
#[test]
fn sm_types_preinit_diff() {
    for (file, entry, tag) in [
        ("stepper_min_o0.ll", "step_min", "bad"),
        ("v4_o0.ll", "step_v4", "good"),
    ] {
        let ll = std::fs::read_to_string(artifacts().join(file)).expect(file);
        let context = Context::create();
        let module = context
            .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
                ll.as_bytes(),
                file,
            ))
            .expect("LLVM IR parses");
        let vaffle = volar_llvm_vaffle_import::import_module(&module, &[entry]).expect("import");
        let (blocks, types) = volar_vaffle_target::lower_vaffle_to_ir(&vaffle);
        eprintln!("=== {tag} ({file}) ===");
        eprintln!("  types: {}", types.0.len());
        for (i, t) in types.0.iter().enumerate() {
            eprintln!("    tid{}: {:?}", i, t);
        }
        eprintln!("  oracles: {}", blocks.oracles.len());
        eprintln!("  pre_init segs: {}", blocks.pre_init.len());
        for seg in blocks.pre_init.iter() {
            eprintln!("    {:?}", seg);
        }
    }
}

/// #5: hash each stmt's FULL Debug (no truncation) and diff those — a
/// difference hidden beyond the 200-char truncation would explain the
/// identical-dump-yet-different-eval contradiction.
#[test]
fn sm_ir_hash_diff() {
    if std::env::var("SM_IRDIFF").is_err() {
        return;
    }
    for (file, entry, tag) in [
        ("stepper_min_o0.ll", "step_min", "bad"),
        ("v4_o0.ll", "step_v4", "good"),
    ] {
        let (blocks, _types) = import_program_to_ir(file, entry);
        let mut d = String::new();
        for (i, b) in blocks.blocks.iter().enumerate() {
            use core::fmt::Write;
            let _ = writeln!(
                d,
                "=== blk {} params={} stmts={}",
                i,
                b.params.len(),
                b.stmts.len()
            );
            for (j, st) in b.stmts.iter().enumerate() {
                let full = format!("{:?}", st.kind);
                let mut h: u64 = 0xcbf29ce484222325;
                for byte in full.as_bytes() {
                    h ^= *byte as u64;
                    h = h.wrapping_mul(0x100000001b3);
                }
                let _ = writeln!(d, "  {} {:016x} len={}", j, h, full.len());
            }
            let t = format!("{:?}", b.terminator);
            let mut th: u64 = 0xcbf29ce484222325;
            for byte in t.as_bytes() {
                th ^= *byte as u64;
                th = th.wrapping_mul(0x100000001b3);
            }
            let _ = writeln!(d, " TERM {:016x} len={}", th, t.len());
        }
        std::fs::write(format!("/tmp/sm_irh_{tag}.txt"), d).unwrap();
    }
}

/// #5 movfuscation trace (per user direction): drive the movfuscated step
/// circuit step-by-step and, after each step, diff the circuit's storage
/// against the reference CFG interpreter's storage after the same hop
/// (through movfuscate's `2s+1` non-block lane remap). The first divergent
/// (step, cell) is the crime scene.
// ~8-minute diagnostic (74+ circuit steps at ~6s/step). Run with --ignored
// when tracing movfuscation; stepper_min_at_o0 covers the regression itself.
#[test]
#[ignore = "long-running diagnostic; stepper_min_at_o0 covers the regression"]
fn sm_movf_storage_trace() {
    let (blocks, mut types) = import_program_to_ir("stepper_min_o0.ll", "step_min");
    let inputs = [0x60u64, 5, 0x60, 3, 0x01, 5];

    // ---- Reference: per-hop snapshots ----
    let ref_inputs: Vec<Vec<bool>> = inputs
        .iter()
        .map(|&inp| (0..64).map(|b| (inp >> b) & 1 == 1).collect())
        .collect();
    let (ref_ret, ref_snaps) =
        volar_fuzz::interpreter::ir::eval_ir_with_snapshots(&blocks, &types, &ref_inputs);
    eprintln!(
        "reference: {} hops, ret={:?}",
        ref_snaps.len(),
        ref_ret.as_ref().map(|v| v.len())
    );

    // ---- Circuit ----
    let entry_slots =
        volar_ir_passes::movfuscate::movfuscate_entry_param_slots(&blocks, &mut types);
    let movfuscated = volar_ir_passes::movfuscate::movfuscate_ir(&blocks, &mut types);
    let bit_ty = types
        .0
        .iter()
        .position(|t| {
            matches!(
                t,
                volar_ir_common::IrType::Primitive(volar_ir_common::Type::Bit)
            )
        })
        .map(|i| volar_ir::ir::IRTypeId(i as u32))
        .expect("Bit interned");
    let circuit = volar_ir_passes::lower_to_circuit::lower_to_circuit_ir(
        &movfuscated,
        &bit_ty,
        1,
        volar_ir_passes::lower_to_circuit::LoweringMode::WithTerminationFlag,
    );
    let step = &circuit.blocks[0];
    let widths: Vec<usize> = step
        .params
        .iter()
        .map(|&t| types.value_bit_width(t).expect("width"))
        .collect();
    let pc_width = widths.iter().take_while(|&&w| w == 1).count();
    let n_params = step.params.len();
    let mut state: Vec<Vec<bool>> = widths.iter().map(|&w| vec![false; w]).collect();
    for (k, &inp) in inputs.iter().enumerate() {
        if k >= entry_slots.len() {
            break;
        }
        let (slot_start, _c) = entry_slots[k];
        let slot = pc_width + slot_start;
        if slot < state.len() && state[slot].len() == 64 {
            for b in 0..64 {
                state[slot][b] = (inp >> b) & 1 == 1;
            }
        }
    }

    let lane = |s: u32| 2 * s + 1; // non-block lane remap
    let mut storage = Default::default();
    let mut steps = 0usize;
    loop {
        let outputs = volar_fuzz::interpreter::ir::eval_ir_circuit_step(
            step,
            &types,
            &circuit.oracles,
            &state,
            &mut storage,
        );
        let mut pc = 0usize;
        for j in 0..pc_width {
            if outputs[1 + j][0] {
                pc |= 1 << j;
            }
        }
        // Reference storage after the hop INTO this pc (snapshot index = steps+1
        // is the state AFTER block ref_snaps[steps].0 executed).
        let empty = Default::default();
        let (ref_block, ref_store) = if steps < ref_snaps.len() {
            let (b, m) = &ref_snaps[steps];
            (*b, m.clone())
        } else {
            (usize::MAX, empty)
        };
        eprintln!(
            "step {steps}: pc={pc} (reference executed block {ref_block}, ref storage cells={})",
            ref_store.len()
        );
        if ref_block != usize::MAX && ref_block != pc {
            eprintln!("  !! pc mismatch: circuit pc {pc} vs reference block {ref_block}");
        }
        // Diff storage cells for the reference cells written so far.
        let mut mismatches = 0usize;
        for ((s, ty, addr), val) in ref_store.iter() {
            let key = (volar_ir_common::StorageId(lane(s.0)), *ty, *addr);
            match storage.get(&key) {
                Some(cval) if cval == val => {}
                Some(cval) => {
                    mismatches += 1;
                    if mismatches <= 6 {
                        let rv = val.iter().fold(0u64, |a, &b| (a << 1) | b as u64);
                        let cv = cval.iter().fold(0u64, |a, &b| (a << 1) | b as u64);
                        eprintln!(
                            "  !! cell st{:#x} t{:?} addr {:#x}: ref={rv:#x} circuit={cv:#x}",
                            s.0, ty, addr
                        );
                    }
                }
                None => {
                    let all_zero = val.iter().all(|&b| !b);
                    if !all_zero {
                        mismatches += 1;
                        if mismatches <= 6 {
                            let rv = val.iter().fold(0u64, |a, &b| (a << 1) | b as u64);
                            eprintln!(
                                "  !! cell st{:#x} t{:?} addr {:#x}: ref={rv:#x} circuit=<missing>",
                                s.0, ty, addr
                            );
                        }
                    }
                }
            }
        }
        if mismatches > 0 {
            eprintln!("  step {steps}: {mismatches} storage mismatches");
        }
        if outputs[0].iter().any(|&b| b) {
            eprintln!("circuit done after {steps} steps");
            break;
        }
        state = outputs[1..1 + n_params].to_vec();
        steps += 1;
        assert!(steps <= 1024, "circuit did not terminate in 1024 steps");
    }
}

/// S6c: import the whole-program revm driver (unmodified revm-interpreter at
/// -O0). Diagnostic first pass: report how far the import gets and the first
/// (or counted) Unsupported gap. Gated on REVM_IMPORT to keep it out of the
/// default suite until the pipeline accepts it.
#[test]
fn revm_import_probe() {
    if std::env::var("REVM_IMPORT").is_err() {
        return;
    }
    let ll = std::fs::read_to_string(artifacts().join("revm_driver_o0.ll")).expect("ll");
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            ll.as_bytes(),
            "revm_driver_o0.ll",
        ))
        .expect("LLVM IR parses");
    let n_defines = ll.matches("\ndefine ").count();
    eprintln!("revm .ll: {n_defines} defines, {} bytes", ll.len());
    let started = std::time::Instant::now();
    let config = volar_llvm_vaffle_import::LlvmImportConfig::default()
        .with_max_dispatch_candidates(1024)
        // VLVI_BULK_LOOP=1 measures the loop-based bulk mode on revm.
        .with_bulk_if_env()
        .with_reachable_unreachable_zeros()
        .with_circuit_helpers(volar_llvm_vaffle_import::CircuitHelperMode::Extract);
    match volar_llvm_vaffle_import::import_module_with_config(&module, &["revm_run"], config) {
        Ok(vaffle) => {
            let mut n_blocks = 0usize;
            let mut n_values = 0usize;
            for f in &vaffle.funcs {
                if let vaffle::FuncDecl::Body(b) = f {
                    n_blocks += b.blocks.len();
                    n_values += b.values.len();
                }
            }
            eprintln!(
                "revm import OK in {:?}: {} funcs, {n_blocks} blocks, {n_values} values",
                started.elapsed(),
                vaffle.funcs.len()
            );
        }
        Err(e) => {
            panic!("revm import failed after {:?}: {e:?}", started.elapsed());
        }
    }
}

/// S6c-wasm: the same revm driver compiled for wasm32-unknown-unknown,
/// stopping at LLVM IR — the pattern: WASM inputs carry no raw inline
/// assembly for the importer to fail on, and the 32-bit pointer width
/// halves every address circuit (the word-cell layer uses 4-byte words).
/// Gated on REVM_WASM32.
#[test]
fn revm_wasm32_import_probe() {
    if std::env::var("REVM_WASM32").is_err() {
        return;
    }
    let ll = std::fs::read_to_string(artifacts().join("revm_driver_wasm32_o0.ll")).expect("ll");
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            ll.as_bytes(),
            "revm_driver_wasm32_o0.ll",
        ))
        .expect("LLVM IR parses");
    let n_defines = ll.matches("\ndefine ").count();
    eprintln!("revm wasm32 .ll: {n_defines} defines, {} bytes", ll.len());
    let started = std::time::Instant::now();
    let config = volar_llvm_vaffle_import::LlvmImportConfig::default()
        .with_max_dispatch_candidates(1024)
        // VLVI_BULK_LOOP=1 measures the loop-based bulk mode on revm.
        .with_bulk_if_env()
        .with_reachable_unreachable_zeros()
        .with_circuit_helpers(volar_llvm_vaffle_import::CircuitHelperMode::Extract);
    match volar_llvm_vaffle_import::import_module_with_config(&module, &["revm_run"], config) {
        Ok(vaffle) => {
            let mut n_blocks = 0usize;
            let mut n_values = 0usize;
            for f in &vaffle.funcs {
                if let vaffle::FuncDecl::Body(b) = f {
                    n_blocks += b.blocks.len();
                    n_values += b.values.len();
                }
            }
            eprintln!(
                "revm wasm32 import OK in {:?}: {} funcs, {n_blocks} blocks, {n_values} values",
                started.elapsed(),
                vaffle.funcs.len()
            );
            if std::env::var_os("VLVI_DOM_TRACE").is_some() {
                // Empty-Return probe: Return{[]} in a sig with results.
                for (fi, f) in vaffle.funcs.iter().enumerate() {
                    if let vaffle::FuncDecl::Body(b) = f {
                        let sig_results = vaffle.sigs[b.sig.0].results.len();
                        if sig_results == 0 {
                            continue;
                        }
                        for (bi, blk) in b.blocks.iter().enumerate() {
                            if let vaffle::Terminator::Return { values } =
                                &blk.terminator
                            {
                                if values.is_empty() {
                                    eprintln!(
                                        "EMPTY RET PROBE f{fi} b{bi}: sig_results={sig_results} stmts={}",
                                        blk.stmts.len()
                                    );
                                    break;
                                }
                            }
                        }
                    }
                }
                for (fi, f) in vaffle.funcs.iter().enumerate() {
                    if let vaffle::FuncDecl::Body(b) = f {
                        let mut def_block = vec![usize::MAX; b.values.len()];
                        for (bi, blk) in b.blocks.iter().enumerate() {
                            for &sid in &blk.stmts {
                                def_block[sid.0] = bi;
                            }
                            for &(vid, _) in &blk.params {
                                if vid.0 < def_block.len() {
                                    def_block[vid.0] = bi;
                                }
                            }
                        }
                        let mut shown = 0usize;
                        'outer: for (bi, blk) in b.blocks.iter().enumerate() {
                            for &sid in &blk.stmts {
                                if shown >= 3 {
                                    break 'outer;
                                }
                                if let vaffle::Value::Op(
                                    volar_ir_common::Stmt::Merge { parts, .. },
                                ) = &b.values[sid.0].kind
                                {
                                    let foreign: Vec<usize> = parts
                                        .iter()
                                        .filter_map(|&p| def_block.get(p.0).copied())
                                        .filter(|&d| d != bi && d != usize::MAX && d != 0)
                                        .collect();
                                    if !foreign.is_empty() {
                                        eprintln!(
                                            "DOM PROBE f{fi} b{bi} merge v{}: {} foreign, defs={foreign:?}",
                                            sid.0,
                                            foreign.len()
                                        );
                                        shown += 1;
                                    }
                                }
                            }
                        }
                    }
                }
            }
            // SSA bisect: which functions trip vaffle_ssa's
            // postcondition? Release mode leaves the debug_assert off, so
            // each function's SSA output is inspected directly with the
            // crate's own cross-block computation.
            if std::env::var_os("REVM_SSA_BISECT").is_some() {
                for (fi, f) in vaffle.funcs.iter().enumerate() {
                    let body = match f {
                        vaffle::FuncDecl::Body(b) => vaffle::FuncBody {
                            sig: b.sig,
                            blocks: b.blocks.clone(),
                            values: b.values.clone(),
                            entry: b.entry,
                        },
                        _ => continue,
                    };
                    let sub = vaffle::Module {
                        pointer_width: vaffle.pointer_width,
                        types: vaffle.types.clone(),
                        oracles: vaffle.oracles.clone(),
                        actions: vaffle.actions.clone(),
                        funcs: vec![vaffle::FuncDecl::Body(body)],
                        sigs: vaffle.sigs.clone(),
                        exports: Default::default(),
                        pre_init: vaffle.pre_init.clone(),
                    };
                    let ssa_mod = volar_vaffle_target::vaffle_ssa::ssa_ify_module(&sub);
                    for f2 in &ssa_mod.funcs {
                        if let vaffle::FuncDecl::Body(b) = f2 {
                            let cross =
                                volar_vaffle_target::lower_to_ir::compute_cross_block_values(b);
                            if cross.is_empty() {
                                continue;
                            }
                            eprintln!("SSA SHAPE: func {fi} has {} cross-block values", cross.len());
                            for &v in cross.iter().take(4) {
                                let def = format!("{:?}", b.values[v].kind);
                                let owner = b
                                    .blocks
                                    .iter()
                                    .position(|blk| {
                                        blk.params.iter().any(|&(vid, _)| vid.0 == v)
                                            || blk.stmts.iter().any(|&sid| sid.0 == v)
                                    })
                                    .unwrap_or(usize::MAX);
                                eprintln!(
                                    "SSA SHAPE: v{v} owner=b{owner} def={}",
                                    def.chars().take(110).collect::<String>()
                                );
                                for (bi, blk) in b.blocks.iter().enumerate() {
                                    if bi == owner {
                                        continue;
                                    }
                                    for &sid in &blk.stmts {
                                        let d = format!("{:?}", b.values[sid.0].kind);
                                        if d.contains(&format!("IRVarId({v})")) {
                                            eprintln!(
                                                "SSA SHAPE: v{v} used in b{bi} stmt {sid:?}: {}",
                                                d.chars().take(140).collect::<String>()
                                            );
                                        }
                                    }
                                    let t = format!("{:?}", blk.terminator);
                                    if t.contains(&format!("IRVarId({v})")) {
                                        eprintln!(
                                            "SSA SHAPE: v{v} used in b{bi} TERM: {}",
                                            t.chars().take(140).collect::<String>()
                                        );
                                    }
                                }
                                if owner == usize::MAX {
                                    eprintln!("SSA SHAPE: v{v} owner NOT FOUND (undefined use)");
                                }
                            }
                        }
                    }
                }
                eprintln!("SSA BISECT done");
            }

            // Next pipeline stage: VAFFLE -> volar IR. Gated so the
            // import-only measurement stays isolable.
            if std::env::var_os("REVM_UNMOD_LOWER").is_some() {
                // Pre/post-SP dominance comparison on the failing function.
                if std::env::var_os("VLVI_DOM_TRACE").is_some() {
                    use volar_vaffle_target::vaffle_ssa;
                    let post = vaffle_ssa::ssa_ify_module(&vaffle);
                    let mut reported = 0usize;
                    for (fi, (pre_f, post_f)) in
                        vaffle.funcs.iter().zip(post.funcs.iter()).enumerate()
                    {
                        if reported >= 2 {
                            break;
                        }
                        if let (
                            vaffle::FuncDecl::Body(pre_b),
                            vaffle::FuncDecl::Body(post_b),
                        ) = (pre_f, post_f)
                        {
                            let pre_v = dom_violations(pre_b);
                            if pre_v.is_empty() {
                                continue;
                            }
                            let post_v = dom_violations(post_b);
                            reported += 1;
                            eprintln!(
                                "DOM CMP f{fi}: pre={} post={} violations",
                                pre_v.len(),
                                post_v.len()
                            );
                            for (vid, owner, use_b, pre_preds) in pre_v.iter().take(2) {
                                let post_entry = post_v
                                    .iter()
                                    .find(|(v2, _, _, _)| v2 == vid);
                                let post_preds: Option<&Vec<usize>> =
                                    post_entry.map(|(_, _, _, ps)| ps);
                                eprintln!(
                                    "  v{vid} owner=b{owner} use=b{use_b} pre_preds={pre_preds:?} post_preds={post_preds:?}"
                                );
                            }
                        }
                    }
                }
                // FACT (vaffle level): import names + the entry's first
                // call target kind. Imports lower as terminal sinks, so an
                // early import call orphans everything downstream of it.
                {
                    let mut import_names: std::collections::BTreeMap<String, usize> =
                        Default::default();
                    let mut body_count = 0usize;
                    for f in vaffle.funcs.iter() {
                        match f {
                            vaffle::FuncDecl::Body(_) => body_count += 1,
                            vaffle::FuncDecl::Import { module: m, name, .. } => {
                                *import_names
                                    .entry(format!("{m}::{name}"))
                                    .or_insert(0usize) += 1;
                            }
                            _ => {}
                        }
                    }
                    eprintln!(
                        "FACT vaffle: bodies={body_count} imports={} names={:?}",
                        import_names.len(),
                        import_names.iter().take(14).collect::<Vec<_>>()
                    );
                    if let Some(vaffle::FuncDecl::Body(entry_body)) = vaffle.funcs.first() {
                        'outer: for blk in &entry_body.blocks {
                            for svid in &blk.stmts {
                                if let vaffle::Value::Call { func, .. } =
                                    &entry_body.values[svid.0].kind
                                {
                                    let kind = match &vaffle.funcs[func.0] {
                                        vaffle::FuncDecl::Body(_) => "Body",
                                        vaffle::FuncDecl::Import { .. } => "Import",
                                        _ => "?",
                                    };
                                    eprintln!(
                                        "FACT entry first call: callee=FuncId({}) kind={kind}",
                                        func.0
                                    );
                                    break 'outer;
                                }
                            }
                        }
                    }
                }
                let lowered_at = std::time::Instant::now();
                let (blocks, types) =
                    volar_vaffle_target::lower_vaffle_to_ir_incremental(vaffle);
                let n_stmts: usize =
                    blocks.blocks.iter().map(|b| b.stmts.len()).sum();
                eprintln!(
                    "revm wasm32 lower OK in {:?}: {} blocks, {n_stmts} stmts, {} types",
                    lowered_at.elapsed(),
                    blocks.blocks.len(),
                    types.0.len()
                );
                // Movfuscation stage: convert the lowered control flow into
                // data-controlled iterations (mov-only execution). The
                // dispatch values become loop iterations, which compress
                // better in cryptographic protocols.
                if std::env::var_os("REVM_MOVFUSCATE").is_some() {
                    let m_at = std::time::Instant::now();
                    let pre_b = blocks.blocks.len();
                    let pre_s = n_stmts;
                    // Equivalence evidence: op-kind histograms pre/post. A
                    // faithful movfuscation preserves every operation as a
                    // guarded iteration; an elision loses them.
                    let hist =
                        |b: &volar_ir::ir::IRBlocks<()>| -> std::collections::BTreeMap<String, usize> {
                            let mut h: std::collections::BTreeMap<String, usize> = Default::default();
                            for blk in b.blocks.iter() {
                                for st in blk.stmts.iter() {
                                    let key = match &st.kind {
                                        volar_ir::ir::IRStmt::Const(..) => "Const",
                                        volar_ir::ir::IRStmt::Poly { .. } => "Poly",
                                        volar_ir::ir::IRStmt::StorageRead { .. } => "StorageRead",
                                        volar_ir::ir::IRStmt::StorageWrite { .. } => "StorageWrite",
                                        _ => "other",
                                    };
                                    *h.entry(key.to_string()).or_insert(0) += 1;
                                }
                            }
                            h
                        };
                    let pre_hist = hist(&blocks);
                    eprintln!("movfuscate pre-hist: {pre_hist:?}");
                    // Replica of cfg_layout's reachability walk: static
                    // edges only (Block targets); reports what the layout's
                    // DFS sees before any Dyn visit-all kicks in. FACT lines
                    // expose the connectedness evidence: block 2's real
                    // terminator, sink-shaped block count, Dyn-terminated
                    // block count, and the first unreachable blocks.
                    {
                        let n = pre_b;
                        let mut seen = vec![false; n];
                        let mut dyn_seen = false;
                        let mut stack = vec![0usize];
                        seen[0] = true;
                        while let Some(bi) = stack.pop() {
                            let term = &blocks.blocks[bi].terminator;
                            let mut succ: Vec<usize> = Vec::new();
                            let mut push_target = |t: &volar_ir::ir::IRBranchTarget| match &t.dest {
                                volar_ir::ir::IRBlockTargetId::Block(id) => {
                                    succ.push(id.0 as usize);
                                }
                                volar_ir::ir::IRBlockTargetId::Dyn(_) => dyn_seen = true,
                                _ => {}
                            };
                            match term {
                                volar_ir::ir::IRTerminator::Jmp { target } => push_target(target),
                                volar_ir::ir::IRTerminator::JumpCond { then_target, else_target, .. } => {
                                    push_target(then_target);
                                    push_target(else_target);
                                }
                                volar_ir::ir::IRTerminator::JumpTable { cases, .. } => {
                                    for t in cases.values() {
                                        push_target(t);
                                    }
                                }
                                _ => {}
                            }
                            for succ_b in succ {
                                if !seen[succ_b] {
                                    seen[succ_b] = true;
                                    stack.push(succ_b);
                                }
                            }
                        }
                        let reachable = seen.iter().filter(|&&x| x).count();
                        eprintln!(
                            "layout replica: reachable={reachable}/{n} static_dyn_seen={dyn_seen}"
                        );
                        let sinks = blocks
                            .blocks
                            .iter()
                            .filter(|b| {
                                b.stmts.is_empty()
                                    && matches!(
                                        &b.terminator,
                                        volar_ir::ir::IRTerminator::Jmp { target }
                                            if matches!(target.dest, volar_ir::ir::IRBlockTargetId::Return)
                                    )
                            })
                            .count();
                        let dyn_terms = blocks
                            .blocks
                            .iter()
                            .filter(|b| {
                                matches!(
                                    &b.terminator,
                                    volar_ir::ir::IRTerminator::Jmp { target }
                                        if matches!(target.dest, volar_ir::ir::IRBlockTargetId::Dyn(_))
                                )
                            })
                            .count();
                        eprintln!("FACT sinks={sinks} dyn_terms={dyn_terms}");

                        let _ = &seen;
                    }
                    let mut types = types;
                    let mov_src = blocks.clone();
                    let mov_b = volar_ir_passes::movfuscate::movfuscate_ir_owned(
                        blocks,
                        &mut types,
                    );
                    let post_s: usize =
                        mov_b.blocks.iter().map(|b| b.stmts.len()).sum();
                    let post_hist = hist(&mov_b);
                    eprintln!("movfuscate post-hist: {post_hist:?}");
                    let stats = volar_ir_passes::movfuscate::LAST_LAYOUT_STATS
                        .load(std::sync::atomic::Ordering::Relaxed);
                    let total = volar_ir_passes::movfuscate::MOV_TOTAL_STMTS
                        .load(std::sync::atomic::Ordering::Relaxed);
                    eprintln!(
                        "movfuscate internals: post-layout n={} min_block_stmts={} total_stmts={total}",
                        (stats >> 32) as usize,
                        (stats & 0xffff_ffff) as usize
                    );
                    eprintln!(
                        "revm wasm32 movfuscate OK in {:?}: {} blocks/{pre_s} stmts -> {} blocks/{post_s} stmts, {} types",
                        m_at.elapsed(),
                        pre_b,
                        mov_b.blocks.len(),
                        types.0.len()
                    );
                }
            }
        }
        Err(e) => {
            panic!(
                "revm wasm32 import failed after {:?}: {e:?}",
                started.elapsed()
            );
        }
    }
}

/// S6c-unmodified: the driver's `revm_run_unmodified` entry — revm's own
/// `run_plain` loop over its real `InstructionTable` function-pointer
/// dispatch (741 defines: every monomorphized instruction), the shape the
/// static-dispatch driver was built to avoid. Gated on REVM_UNMODIFIED;
/// run behind the RSS kill-switch.
#[test]
fn revm_unmodified_import_probe() {
    if std::env::var("REVM_UNMODIFIED").is_err() {
        return;
    }
    let ll = std::fs::read_to_string(artifacts().join("revm_driver_unmodified_wasm32_o0.ll"))
        .expect("ll");
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            ll.as_bytes(),
            "revm_driver_unmodified_wasm32_o0.ll",
        ))
        .expect("LLVM IR parses");
    let n_defines = ll.matches("\ndefine ").count();
    eprintln!(
        "revm unmodified .ll: {n_defines} defines, {} bytes",
        ll.len()
    );
    let started = std::time::Instant::now();
    let config = volar_llvm_vaffle_import::LlvmImportConfig::default()
        .with_max_dispatch_candidates(1024)
        // VLVI_BULK_LOOP=1 measures the loop-based bulk mode on revm.
        .with_bulk_if_env()
        .with_reachable_unreachable_zeros()
        .with_circuit_helpers(volar_llvm_vaffle_import::CircuitHelperMode::Extract);
    match volar_llvm_vaffle_import::import_module_with_config(
        &module,
        &["revm_run_unmodified"],
        config,
    ) {
        Ok(vaffle) => {
            let mut n_blocks = 0usize;
            let mut n_values = 0usize;
            for f in &vaffle.funcs {
                if let vaffle::FuncDecl::Body(b) = f {
                    n_blocks += b.blocks.len();
                    n_values += b.values.len();
                }
            }
            eprintln!(
                "revm unmodified import OK in {:?}: {} funcs, {n_blocks} blocks, {n_values} values",
                started.elapsed(),
                vaffle.funcs.len()
            );
            // Next pipeline stage: VAFFLE -> volar IR, consuming the
            // module (transferring ~1B values by clone would double RSS).
            if std::env::var_os("REVM_UNMOD_LOWER").is_some() {
                let lowered_at = std::time::Instant::now();
                let (blocks, types) =
                    volar_vaffle_target::lower_vaffle_to_ir_incremental(vaffle);
                let n_stmts: usize =
                    blocks.blocks.iter().map(|b| b.stmts.len()).sum();
                eprintln!(
                    "revm unmodified lower OK in {:?}: {} blocks, {n_stmts} stmts, {} types",
                    lowered_at.elapsed(),
                    blocks.blocks.len(),
                    types.0.len()
                );
            }
        }
        Err(e) => {
            panic!(
                "revm unmodified import failed after {:?}: {e:?}",
                started.elapsed()
            );
        }
    }
}

/// Import helper honoring a full config (bulk-memory differential).
/// Import and return the VAFFLE module, the entry's FuncId, and the entry
/// body's value count (the importer's own size metric).
fn import_vaffle_with_config(
    file: &str,
    entry: &str,
    config: volar_llvm_vaffle_import::LlvmImportConfig,
) -> (
    vaffle::Module,
    vaffle::FuncId,
    usize,
    volar_ir::ir::IRBlocks<()>,
    volar_ir::ir::IRTypes,
) {
    let ll = std::fs::read_to_string(artifacts().join(file)).expect(file);
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            ll.as_bytes(),
            file,
        ))
        .expect("LLVM IR parses");
    let vaffle = volar_llvm_vaffle_import::import_module_with_config(&module, &[entry], config)
        .expect("LLVM import");
    let entry_fid = vaffle.exports.get(entry).copied().expect("entry export");
    // The unmodified-revm work also imports plain `Import` decls for
    // externals; bulk_sum is a real body (and the import step inlines
    // specializations as separate decls).
    let entry_values: usize = match &vaffle.funcs[entry_fid.0] {
        vaffle::FuncDecl::Body(body) => body.values.len(),
        other => panic!("entry is not a body: {other:?}"),
    };
    let (lowered, lowered_types) = volar_vaffle_target::lower_vaffle_to_ir(&vaffle);
    (vaffle, entry_fid, entry_values, lowered, lowered_types)
}

fn decode_bits(r: &[std::vec::Vec<bool>]) -> u64 {
    let mut got = 0u64;
    let mut bitidx = 0;
    'outer: for slot in r {
        for &bit in slot {
            if bitidx >= 64 {
                break 'outer;
            }
            if bit {
                got |= 1 << bitidx;
            }
            bitidx += 1;
        }
    }
    got
}

/// Expected value of bulk_sum, mirroring programs/bulk_mem.rs natively.
fn native_bulk_sum(sel: u32) -> u32 {
    let mut a = [0u8; 300];
    let mut b = [7u8; 300];
    if sel & 1 != 0 {
        a[0] = 3;
        a[299] = 5;
        b.copy_from_slice(&a);
    }
    ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | ((b[299] as u32) << 4) | (a[1] as u32)
}

/// S6d: constant-length bulk memset/memcpy lower as runtime loops in
/// `BulkMemoryMode::Loop`, evaluate identically to the unrolled lowering
/// (VAFFLE level, lowered-CFG level, and the INCREMENTAL lowering), and
/// cost several times fewer entry-function import values.
/// Dominance-violation scanner for one VAFFLE body: cross-block uses
/// whose def block does not dominate the using block. Returns
/// (value, owner, use_block, use_block_preds) triples.
fn dom_violations(
    b: &vaffle::FuncBody,
) -> Vec<(usize, usize, usize, Vec<usize>)> {
    let n = b.blocks.len();
    let mut def_block = vec![usize::MAX; b.values.len()];
    let mut preds: Vec<std::collections::BTreeSet<usize>> =
        vec![std::collections::BTreeSet::new(); n];
    for (bi, blk) in b.blocks.iter().enumerate() {
        for &sid in &blk.stmts {
            def_block[sid.0] = bi;
        }
        for &(vid, _) in &blk.params {
            if vid.0 < def_block.len() {
                def_block[vid.0] = bi;
            }
        }
        let succs: Vec<usize> = match &blk.terminator {
            vaffle::Terminator::Jump(t) => vec![t.block.0],
            vaffle::Terminator::IfNonzero {
                then_target,
                else_target,
                ..
            } => vec![then_target.block.0, else_target.block.0],
            vaffle::Terminator::Table {
                targets,
                default_target,
                ..
            } => {
                let mut v: Vec<usize> =
                    targets.iter().map(|t| t.block.0).collect();
                v.push(default_target.block.0);
                v
            }
            _ => Vec::new(),
        };
        for s in succs {
            preds[s].insert(bi);
        }
    }
    // RPO from entry
    let entry = b.entry.0;
    let mut succs_adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (bi, ps) in preds.iter().enumerate() {
        for &p in ps {
            succs_adj[p].push(bi);
        }
    }
    let mut seen = vec![false; n];
    let mut post = Vec::new();
    let mut stack: Vec<(usize, usize)> = vec![(entry, 0)];
    seen[entry] = true;
    while let Some(top) = stack.last().copied() {
        let (node, mut si) = top;
        let mut advanced = false;
        while si < succs_adj[node].len() {
            let c = succs_adj[node][si];
            si += 1;
            if !seen[c] {
                seen[c] = true;
                stack.last_mut().expect("nonempty").1 = si;
                stack.push((c, 0));
                advanced = true;
                break;
            }
        }
        if advanced {
            stack.last_mut().expect("nonempty").1 = si;
        } else {
            post.push(node);
            stack.pop();
        }
    }
    let mut rpo = post;
    rpo.reverse();
    let rpo_pos: std::collections::HashMap<usize, usize> =
        rpo.iter().enumerate().map(|(p, &b)| (b, p)).collect();
    // CHK idom
    let mut idom = vec![usize::MAX; n];
    idom[entry] = entry;
    let mut changed = true;
    while changed {
        changed = false;
        for &b in rpo.iter().skip(1) {
            let mut new_idom = usize::MAX;
            for &p in &preds[b] {
                if idom[p] != usize::MAX {
                    new_idom = if new_idom == usize::MAX {
                        p
                    } else {
                        let (mut a, mut m) = (new_idom, p);
                        while a != m {
                            while rpo_pos[&a] > rpo_pos[&m] {
                                a = idom[a];
                            }
                            while rpo_pos[&m] > rpo_pos[&a] {
                                m = idom[m];
                            }
                        }
                        a
                    };
                }
            }
            if new_idom != usize::MAX && idom[b] != new_idom {
                idom[b] = new_idom;
                changed = true;
            }
        }
    }
    let dominates = |a: usize, mut x: usize| -> bool {
        loop {
            if a == x {
                return true;
            }
            if x == usize::MAX || idom[x] == usize::MAX || idom[x] == x {
                return false;
            }
            x = idom[x];
        }
    };
    // Cross-block uses violating dominance
    let mut out = Vec::new();
    for (bi, blk) in b.blocks.iter().enumerate() {
        for &sid in &blk.stmts {
            let k = format!("{:?}", b.values[sid.0].kind);
            let mut scan = &k[..];
            while let Some(pos) = scan.find("ValueId(") {
                let rest = &scan[pos + "ValueId(".len()..];
                let end = match rest.find(')') {
                    Some(e) => e,
                    None => break,
                };
                let vid: usize = rest[..end].parse().unwrap_or(usize::MAX);
                scan = &rest[end..];
                if vid == usize::MAX || vid >= def_block.len() {
                    continue;
                }
                let d = def_block[vid];
                if d != bi && d != usize::MAX && !dominates(d, bi) {
                    out.push((vid, d, bi, preds[bi].iter().copied().collect()));
                }
            }
        }
    }
    out
}

#[test]
fn dispatch_table_read_write_reuse() {
    // A pointer-typed parameter has unresolved provenance, so the stores
    // and loads (direct and through a GEP off the param) route through the
    // shared Table-on-ID dispatch: write path (incl. the value-param
    // threading), read path, and check-once reuse for the derived pointer.
    let ll = std::fs::read_to_string(artifacts().join("dispatch_probe.ll"))
        .expect("dispatch_probe.ll");
    let context = Context::create();
    let llvm_mod = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            ll.as_bytes(),
            "dispatch_probe.ll",
        ))
        .expect("LLVM IR parses");
    let config = if std::env::var_os("VLVI_DOM_CASCADE").is_some() {
        volar_llvm_vaffle_import::LlvmImportConfig::default()
    } else {
        volar_llvm_vaffle_import::LlvmImportConfig::default().with_table_dispatch()
    };
    let module = volar_llvm_vaffle_import::import_module_with_config(
        &llvm_mod,
        &["dispatch_probe"],
        config,
    )
    .expect("LLVM import");
    let fid = module
        .exports
        .get("dispatch_probe")
        .copied()
        .expect("entry export");
    let _ = fid;
    for (v, w) in [
        (5u32, 9u32),
        (0, 1),
        (0xdead_beef, 7),
        (1, 0x7fff_ffff),
        (0xffff_ffff, 3),
    ] {
        let want = v.wrapping_add(w.wrapping_shl(1)) as u64;
        // This module is x86_64: pointers are 64 bits, encoded
        // [ADDR: 49][ID: 14][TAG: 1] with tag=1 for the global class;
        // brute-force BUF's import-time StorageId (the eval seeds its own
        // storage, so stores/loads round-trip within one eval).
        let mut got: Option<(u64, u32)> = None;
        for sid in 1u32..=64 {
            let p = 0x8000_0000_0000_0000u64 | ((sid as u64) << 49);
            let inputs: Vec<Vec<bool>> = (0..64)
                .map(|b| vec![(p >> b) & 1 == 1])
                .chain((0..32).map(|b| vec![(v >> b) & 1 == 1]))
                .chain((0..32).map(|b| vec![(w >> b) & 1 == 1]))
                .collect();
            match volar_fuzz::interpreter::vaffle::eval_vaffle(&module, fid, &inputs) {
                Some(out) => {
                    let ret = decode_bits(&out);
                    if ret == want {
                        eprintln!("MATCH at sid {sid}");
                        got = Some((ret, sid));
                        break;
                    }
                    if std::env::var_os("VLVI_DISPATCH_TRACE").is_some() {
                        eprintln!("sid {sid}: ret={ret:#x} want={want:#x}");
                    }
                }
                None => {
                    if std::env::var_os("VLVI_DISPATCH_TRACE").is_some() {
                        eprintln!("sid {sid}: eval returned None");
                    }
                }
            }
        }
        assert_eq!(
            got.map(|(r, _)| r),
            Some(want),
            "dispatch probe v={v:#x} w={w:#x}: no storage id produced the expected value"
        );
    }
}

#[test]
fn bulk_memory_loop_matches_unroll() {
    let file = "bulk_mem.ll";
    let entry = "bulk_sum";
    let (unroll_mod, unroll_fid, unroll_values, unroll_ir, unroll_types) =
        import_vaffle_with_config(
            file,
            entry,
            volar_llvm_vaffle_import::LlvmImportConfig::default(),
        );
    let (loop_mod, loop_fid, loop_values, loop_ir, loop_types) =
        import_vaffle_with_config(
            file,
            entry,
            volar_llvm_vaffle_import::LlvmImportConfig::default().with_bulk_memory_loop(),
        );

    // The importer's own metric: the entry body's values shrink several
    // times (the 300-byte fills/copy collapse to loop scaffolding).
    assert!(
        loop_values * 3 < unroll_values,
        "loop mode should shrink entry import values: unroll={unroll_values} loop={loop_values}"
    );

    for sel in [0u32, 1] {
        // VAFFLE flattens the u32 param to 32 one-bit entry params; the
        // lowered IR keeps one 32-bit value.
        let vaffle_inputs: Vec<Vec<bool>> =
            (0..32).map(|b| vec![(sel >> b) & 1 == 1]).collect();
        let lowered_inputs: Vec<Vec<bool>> =
            vec![(0..32).map(|b| (sel >> b) & 1 == 1).collect()];
        if std::env::var_os("VLVI_THREAD_DEBUG").is_some() {
            for f2 in &unroll_mod.funcs {
                if let vaffle::FuncDecl::Body(b) = f2 {
                    if b.values.len() > 303 {
                        let def = format!("{:?}", b.values[303].kind);
                        eprintln!(
                            "THREAD DBG: v303 def={} owner-find",
                            def.chars().take(100).collect::<String>()
                        );
                        for (bi, blk) in b.blocks.iter().enumerate() {
                            if let Some(pos303) =
                                blk.stmts.iter().position(|&sid| sid.0 == 303)
                            {
                                let pos313 =
                                    blk.stmts.iter().position(|&sid| sid.0 == 313);
                                eprintln!(
                                    "THREAD DBG: v303 in b{bi} stmts at pos {pos303}; v313 at {pos313:?}; block stmts {}",
                                    blk.stmts.len()
                                );
                                for w in blk.stmts.iter().take(6) {
                                    let d = format!("{:?}", b.values[w.0].kind);
                                    eprintln!(
                                        "  b{bi} v{}: {}",
                                        w.0,
                                        d.chars().take(70).collect::<String>()
                                    );
                                }
                            }
                            if blk.params.iter().any(|&(vid, _)| vid.0 == 303) {
                                eprintln!("THREAD DBG: v303 is a PARAM of b{bi} ({} params)", blk.params.len());
                            }
                            let t = format!("{:?}", blk.terminator);
                            if t.contains("ValueId(303)") {
                                eprintln!(
                                    "THREAD DBG: v303 TERM-USED b{bi}: {}",
                                    t.chars().take(160).collect::<String>()
                                );
                            }
                            for &sid in &blk.stmts {
                                let d = format!("{:?}", b.values[sid.0].kind);
                                if d.contains("ValueId(303)") {
                                    eprintln!(
                                        "THREAD DBG: v303 used in b{bi} stmt {sid:?}: {}",
                                        d.chars().take(140).collect::<String>()
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
        let unroll_out =
            volar_fuzz::interpreter::vaffle::eval_vaffle(&unroll_mod, unroll_fid, &vaffle_inputs)
                .expect("unroll eval");
        let loop_out =
            volar_fuzz::interpreter::vaffle::eval_vaffle(&loop_mod, loop_fid, &vaffle_inputs)
                .expect("loop eval");
        let want = native_bulk_sum(sel) as u64;
        assert_eq!(decode_bits(&unroll_out), want, "unroll mode sel={sel}");
        assert_eq!(decode_bits(&loop_out), want, "loop mode sel={sel}");

        // The lowered-CFG path agrees too (this exercises
        // lower_vaffle_to_ir on loop CFGs; the off-by-one seen earlier was
        // debug-assertion panic machinery, not a lowering bug).
        let lowered_unroll = volar_fuzz::interpreter::ir::eval_ir_with_storage(
            &unroll_ir,
            &unroll_types,
            &lowered_inputs,
        )
        .0
        .expect("lowered unroll eval");
        let lowered_loop = volar_fuzz::interpreter::ir::eval_ir_with_storage(
            &loop_ir,
            &loop_types,
            &lowered_inputs,
        )
        .0
        .expect("lowered loop eval");
        assert_eq!(decode_bits(&lowered_unroll), want, "lowered unroll sel={sel}");
        assert_eq!(decode_bits(&lowered_loop), want, "lowered loop sel={sel}");
    }

    // The INCREMENTAL lowering agrees as well (block ids are rebased per
    // function; vars are block-scoped so no var rebase is needed). It
    // consumes the module, so it runs after the borrow-heavy gates.
    let (inc_blocks, inc_types) =
        volar_vaffle_target::lower_vaffle_to_ir_incremental(unroll_mod);
    for sel in [0u32, 1] {
        let lowered_inputs: Vec<Vec<bool>> =
            vec![(0..32).map(|b| (sel >> b) & 1 == 1).collect()];
        let inc_out = volar_fuzz::interpreter::ir::eval_ir_with_storage(
            &inc_blocks,
            &inc_types,
            &lowered_inputs,
        )
        .0
        .expect("incremental eval");
        let want = native_bulk_sum(sel) as u64;
        assert_eq!(decode_bits(&inc_out), want, "incremental sel={sel}");
    }
}

// ============================================================================
// Incremental-lowering call-edge differential: a multi-function module
// (forward callee reference, chained calls, a call inside a conditional
// branch) must lower to a statically CONNECTED IR module through BOTH
// `lower_vaffle_to_ir` (whole-module) and `lower_vaffle_to_ir_incremental`
// (persistent single-module driver), and all three evaluations (VAFFLE
// reference, whole-module IR, incremental IR) must agree.
//
// This is the regression guard for the call-terminator bug: incremental
// lowering used to emit call-site `Jmp`s to ghost abort ids that were never
// emitted, orphaning every callee entry (reachable=3/N) — invisible to
// block/stmt-count differentials, only visible to evaluation.
// ============================================================================

fn bits_to_u64(v: &[std::vec::Vec<bool>]) -> u64 {
    let mut x = 0u64;
    for (i, b) in v.iter().flatten().enumerate() {
        if *b {
            x |= 1 << i;
        }
    }
    x
}

#[test]
fn incremental_multifunction_call_eval_matches_whole() {
    use vaffle::{Block, BlockId, FuncBody, FuncDecl, FuncId, Module, SigDecl, SigId, Target as VTarget, Terminator, Value, ValueId};
    use volar_ir_common::{IrType, Node, Stmt, TypeTable};

    let mut types = TypeTable::new();
    let bit = types.bit();
    let u8_ty = types.intern(IrType::Vec(8, bit));
    let sigs = vec![
        SigDecl { params: vec![u8_ty, u8_ty], results: vec![u8_ty] }, // 0: main
        SigDecl { params: vec![u8_ty], results: vec![u8_ty] },        // 1: helper
    ];
    let n = |kind: Value| Node::new(kind, (), None);
    let tgt = |block: BlockId, args: Vec<ValueId>| VTarget { block, args, reentry: None };

    // helper(x) = rol8(x, 3)
    let helper = FuncBody {
        sig: SigId(1),
        entry: BlockId(0),
        blocks: vec![Block {
            params: vec![(ValueId(0), u8_ty)],
            stmts: vec![ValueId(1)],
            terminator: Terminator::Return { values: vec![ValueId(1)] },
        }],
        values: vec![
            n(Value::Param { block: BlockId(0), ty: u8_ty, idx: 0 }),
            n(Value::Op(Stmt::Rol { src: ValueId(0), ty: u8_ty, n: 3 })),
        ],
    };

    // main(a, b):
    //   t0 = helper(a); t1 = helper(b); t2 = helper(t0)   [chained call]
    //   t3 = if t1 != 0 { helper(t2) } else { t1 }        [call in a branch]
    //   return shuffle(even bits <- t0, odd bits <- t3)
    //
    // Call results are consumed via the producer convention: one
    // `Value::Output` projection per result bit, merged into the wide value
    // (`val_map` records only bit 0 under the call's own ValueId).
    let u8v = |base: ValueId| -> Value {
        // The eight `Output` projections sit at base-8 .. base-1.
        Value::Op(Stmt::Merge {
            parts: (0..8).map(|j| ValueId(base.0 - 8 + j)).collect(),
            ty: u8_ty,
        })
    };
    // Value id plan (main):
    //   0..2 params; 2 = c0; 3..10 = Output{c0,j}; 11 = t0;
    //   12 = c1; 13..20 = Output{c1,j}; 21 = t1;
    //   22 = c2; 23..30 = Output{c2,j}; 31 = t2;
    //   b1: 32..34 params; 35 = c3; 36..43 = Output{c3,j}; 44 = t3;
    //   b2: 45..47 params; b3: 48..50 params; 51 = shuffle.
    let (c0, t0) = (ValueId(2), ValueId(11));
    let (c1, t1) = (ValueId(12), ValueId(21));
    let (c2, t2) = (ValueId(22), ValueId(31));
    let (c3, t3) = (ValueId(35), ValueId(44));
    let m = ValueId(51);
    let mut main_values: Vec<Node<Value, ()>> = vec![
        n(Value::Param { block: BlockId(0), ty: u8_ty, idx: 0 }),
        n(Value::Param { block: BlockId(0), ty: u8_ty, idx: 1 }),
        n(Value::Call { func: FuncId(1), args: vec![ValueId(0)] }),
    ];
    for j in 0..8u8 {
        main_values.push(n(Value::Output { value: c0, idx: j as usize }));
    }
    main_values.push(n(u8v(t0)));
    main_values.push(n(Value::Call { func: FuncId(1), args: vec![t0] }));
    for j in 0..8u8 {
        main_values.push(n(Value::Output { value: c1, idx: j as usize }));
    }
    main_values.push(n(u8v(t1)));
    main_values.push(n(Value::Call { func: FuncId(1), args: vec![t0] }));
    for j in 0..8u8 {
        main_values.push(n(Value::Output { value: c2, idx: j as usize }));
    }
    main_values.push(n(u8v(t2)));
    // b1's param values (32..34), then its call/outputs/merge (35..44),
    // then b2's params (45..47), b3's params (48..50).
    for vid in 32..35u32 {
        main_values.push(n(Value::Param {
            block: BlockId(0),
            ty: u8_ty,
            idx: (vid - 32) as usize,
        }));
    }
    main_values.push(n(Value::Call { func: FuncId(1), args: vec![ValueId(34)] }));
    for j in 0..8u8 {
        main_values.push(n(Value::Output { value: c3, idx: j as usize }));
    }
    main_values.push(n(u8v(t3)));
    for vid in 45..51u32 {
        main_values.push(n(Value::Param {
            block: BlockId(0),
            ty: u8_ty,
            idx: (vid - 45) as usize,
        }));
    }
    let mut rb = Vec::new();
    for i in 0..8u8 {
        rb.push((i, ValueId(48))); // even result bits <- r0
        rb.push((i, ValueId(50))); // odd result bits <- r2
    }
    main_values.push(n(Value::Op(Stmt::Shuffle { result_bits: rb, ty: u8_ty })));

    let mut b0_stmts = vec![c0];
    for j in 0..8u8 {
        b0_stmts.push(ValueId(3 + j as usize));
    }
    b0_stmts.push(t0);
    b0_stmts.push(c1);
    for j in 0..8u8 {
        b0_stmts.push(ValueId(13 + j as usize));
    }
    b0_stmts.push(t1);
    b0_stmts.push(c2);
    for j in 0..8u8 {
        b0_stmts.push(ValueId(23 + j as usize));
    }
    b0_stmts.push(t2);
    let mut b1_stmts = vec![c3];
    for j in 0..8u8 {
        b1_stmts.push(ValueId(36 + j as usize));
    }
    b1_stmts.push(t3);

    let main = FuncBody {
        sig: SigId(0),
        entry: BlockId(0),
        blocks: vec![
            Block {
                params: vec![(ValueId(0), u8_ty), (ValueId(1), u8_ty)],
                stmts: b0_stmts,
                terminator: Terminator::IfNonzero {
                    cond: t1,
                    then_target: tgt(BlockId(1), vec![t0, t1, t2]),
                    else_target: tgt(BlockId(2), vec![t0, t1, t2]),
                },
            },
            Block {
                params: vec![(ValueId(32), u8_ty), (ValueId(33), u8_ty), (ValueId(34), u8_ty)],
                stmts: b1_stmts,
                terminator: Terminator::Jump(tgt(
                    BlockId(3),
                    vec![ValueId(32), ValueId(33), t3],
                )),
            },
            Block {
                params: vec![(ValueId(45), u8_ty), (ValueId(46), u8_ty), (ValueId(47), u8_ty)],
                stmts: vec![],
                terminator: Terminator::Jump(tgt(
                    BlockId(3),
                    vec![ValueId(45), ValueId(46), ValueId(47)],
                )),
            },
            Block {
                params: vec![(ValueId(48), u8_ty), (ValueId(49), u8_ty), (ValueId(50), u8_ty)],
                stmts: vec![m],
                terminator: Terminator::Return { values: vec![m] },
            },
        ],
        values: main_values,
    };

    fn build(
        types: TypeTable,
        main: FuncBody<()>,
        helper: FuncBody<()>,
        sigs: Vec<SigDecl>,
    ) -> Module<()> {
        Module {
            pointer_width: vaffle::PointerWidth::Bits64,
            types,
            oracles: vec![],
            actions: vec![],
            funcs: vec![FuncDecl::Body(main), FuncDecl::Body(helper)],
            sigs,
            exports: Default::default(),
            pre_init: vec![],
        }
    }
    fn build_types() -> TypeTable {
        let mut types = volar_ir_common::TypeTable::new();
        let bit = types.bit();
        let _ = types.intern(volar_ir_common::IrType::Vec(8, bit));
        types
    }
    fn build_sigs() -> Vec<SigDecl> {
        let mut types = build_types();
        let bit = types.bit();
        let u8_ty = types.intern(volar_ir_common::IrType::Vec(8, bit));
        vec![
            SigDecl { params: vec![u8_ty, u8_ty], results: vec![u8_ty] },
            SigDecl { params: vec![u8_ty], results: vec![u8_ty] },
        ]
    }

    // VAFFLE reference.
    for a in [0x01u64, 0x5A, 0xFF] {
        for b in [0x00u64, 0x81, 0xFF] {
            let to_bits = |v: u64| (0..8).map(|i| (v >> i) & 1 == 1).collect::<Vec<bool>>();
            let vaffle_out = volar_fuzz::interpreter::vaffle::eval_vaffle(
                &build(build_types(), main.clone(), helper.clone(), build_sigs()),
                FuncId(0),
                &[to_bits(a), to_bits(b)],
            )
            .expect("vaffle eval");
            // The VAFFLE interpreter is the ground truth; the two lowerings
            // must agree with it (and each other).
            let want = bits_to_u64(&vaffle_out);

            // Whole-module lowering: connected + correct.
            let (whole, whole_types) =
                volar_vaffle_target::lower_vaffle_to_ir(&build(
                    build_types(),
                    main.clone(),
                    helper.clone(),
                    build_sigs(),
                ));
            if a == 0x01 && b == 0x00 {
                for (bi, blk) in whole.blocks.iter().enumerate() {
                    eprintln!(
                        "IR b{bi}: params={:?} stmts={} term={:?}",
                        blk.params,
                        blk.stmts.len(),
                        blk.terminator
                    );
                }
            }
            let reachable = count_reachable_from_entry(&whole);
            assert_eq!(reachable, whole.blocks.len(), "whole module not fully reachable");
            let whole_out = volar_fuzz::interpreter::ir::eval_ir_with_storage(
                &whole,
                &whole_types,
                &[{
                    let mut w = to_bits(a);
                    w.extend(to_bits(b));
                    w
                }],
            )
            .0
            .expect("whole IR eval");

            // Incremental lowering: connected + correct + same block count.
            let (inc, inc_types) = volar_vaffle_target::lower_vaffle_to_ir_incremental(build(
                build_types(),
                main.clone(),
                helper.clone(),
                build_sigs(),
            ));
            assert_eq!(inc.blocks.len(), whole.blocks.len(), "block count divergence");
            let reachable = count_reachable_from_entry(&inc);
            assert_eq!(reachable, inc.blocks.len(), "incremental module not fully reachable");
            let inc_out = volar_fuzz::interpreter::ir::eval_ir_with_storage(
                &inc,
                &inc_types,
                &[{
                    let mut w = to_bits(a);
                    w.extend(to_bits(b));
                    w
                }],
            )
            .0
            .expect("incremental IR eval");
            assert_eq!(bits_to_u64(&inc_out), want, "incremental IR a={a:#x} b={b:#x}");
        }
    }
}

/// DFS from the module entry over static edges; `Dyn` targets visit ALL
/// blocks (the movfuscation layout's own convention). Returns the count of
/// distinct blocks reached — a lowered module with proper call terminators
/// must reach every block.
fn count_reachable_from_entry(blocks: &volar_ir::ir::IRBlocks) -> usize {
    use volar_ir::ir::{IRBlockTargetId, IRTerminator};
    let n = blocks.blocks.len();
    let mut seen = std::collections::BTreeSet::new();
    let mut stack = vec![0usize];
    while let Some(b) = stack.pop() {
        if b >= n || !seen.insert(b) {
            continue;
        }
        let mut push_all = |stack: &mut Vec<usize>| {
            for i in 0..n {
                stack.push(i);
            }
        };
        match &blocks.blocks[b].terminator {
            IRTerminator::Jmp { target } => match target.dest {
                IRBlockTargetId::Block(id) => stack.push(id.0 as usize),
                IRBlockTargetId::Dyn(_) => push_all(&mut stack),
                _ => {}
            },
            IRTerminator::JumpCond { then_target, else_target, .. } => {
                for t in [then_target, else_target] {
                    match t.dest {
                        IRBlockTargetId::Block(id) => stack.push(id.0 as usize),
                        IRBlockTargetId::Dyn(_) => push_all(&mut stack),
                        _ => {}
                    }
                }
            }
            IRTerminator::JumpTable { cases, .. } => {
                for t in cases.values() {
                    match t.dest {
                        IRBlockTargetId::Block(id) => stack.push(id.0 as usize),
                        IRBlockTargetId::Dyn(_) => push_all(&mut stack),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    seen.len()
}
