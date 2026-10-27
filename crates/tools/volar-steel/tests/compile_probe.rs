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

/// Import the reference program via the LLVM frontend and lower to Volar IR.
fn import_to_ir() -> (volar_ir::ir::IRBlocks<()>, volar_ir::ir::IRTypes) {
    let ll = std::fs::read_to_string(artifacts().join("reference.ll")).expect("reference.ll");
    let context = Context::create();
    let module = context
        .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
            ll.as_bytes(),
            "reference.ll",
        ))
        .expect("reference.ll parses");
    let vaffle =
        volar_llvm_vaffle_import::import_module(&module, &["steel_view_call"]).expect("LLVM import");
    volar_vaffle_target::lower_vaffle_to_ir(&vaffle)
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
                volar_ir::ir::IRTerminator::JumpCond { then_target, else_target, .. } => format!(
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
        .position(|t| matches!(t, volar_ir_common::IrType::Primitive(volar_ir_common::Type::Bit)))
        .map(|i| volar_ir::ir::IRTypeId(i as u32))
        .expect("Bit type interned by movfuscate_ir");
    let circuit = volar_ir_passes::lower_to_circuit::lower_to_circuit_ir(
        &movfuscated,
        &bit_ty,
        1,
        volar_ir_passes::lower_to_circuit::LoweringMode::WithTerminationFlag,
    );
    eprintln!("S2: step circuit — {} blocks", circuit.blocks.len());
    assert_eq!(circuit.blocks.len(), 1, "movfuscation yields one step block");
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
