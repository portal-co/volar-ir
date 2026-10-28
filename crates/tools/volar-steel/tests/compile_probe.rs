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
        assert!(steps <= 64, "step circuit did not return in 64 steps");
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
        assert!(steps <= 32, "simple loop did not terminate");
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
        assert!(steps <= 64);
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
#[test]
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
            steps <= 64,
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
        .position(|t| matches!(t, volar_ir_common::IrType::Primitive(volar_ir_common::Type::Bit)))
        .map(|i| volar_ir::ir::IRTypeId(i as u32))
        .expect("Bit interned");
    let circuit = volar_ir_passes::lower_to_circuit::lower_to_circuit_ir(
        &movfuscated, &bit_ty, 1,
        volar_ir_passes::lower_to_circuit::LoweringMode::WithTerminationFlag,
    );
    let step = &circuit.blocks[0];
    eprintln!("frame_scalar_o0: step block — {} params, {} stmts", step.params.len(), step.stmts.len());
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
            step, &types, &circuit.oracles, &state, &mut storage,
        );
        if outputs[0].iter().any(|&b| b) {
            ret_vals = outputs[1 + n_params..].to_vec();
            break;
        }
        state = outputs[1..1 + n_params].to_vec();
        steps += 1;
        assert!(steps <= 64, "the frame-scalar loop did not terminate");
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
        assert!(steps <= 64, "step circuit did not return in 64 steps");
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
