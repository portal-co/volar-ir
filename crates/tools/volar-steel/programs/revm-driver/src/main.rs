//! S6c driver: run the UNMODIFIED revm interpreter over a small EVM program,
//! compiled to whole-program LLVM IR for the Steel pipeline.
//!
//! `cargo rustc --offline -- --emit=llvm-ir` on this bin emits a single .ll
//! containing the driver plus all monomorphized revm code at -O0 (the
//! frontend-supported level; see #5's resolution — the -O0 frame path is
//! proven end-to-end).
//!
//! The EVM program under test is `PUSH32 a, PUSH32 b, ADD, MSTORE, RETURN`:
//! revm's actual instruction loop over its real U256 stack, memory, and
//! output paths. The entry returns the low 64 bits of the sum so the
//! volar-steel test can drive the circuit differentially against native.

#![no_std]
#![no_main]
#![allow(internal_features)]
#![allow(unused)]

extern crate alloc;

use core::alloc::{GlobalAlloc, Layout};

/// Static bump pool backing `alloc` — revm's `Vec`/`Bytes`/`Rc` allocations
/// never free and never grow past a few KiB for the programs under test.
const POOL_BYTES: usize = 16 * 1024; // 16 KiB — ample for the driver's tiny heap

static mut POOL: [u8; POOL_BYTES] = [0u8; POOL_BYTES];
static mut BUMP: usize = 0;

struct BumpAlloc;

unsafe impl GlobalAlloc for BumpAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // Plain non-atomic bump: the Steel driver is single-threaded, and
        // atomics import as unsupported instructions.
        let align = layout.align().max(8);
        let size = layout.size().max(align);
        let pos = unsafe {
            let p = core::ptr::addr_of_mut!(BUMP);
            let old = *p;
            *p = old + size + align;
            old
        };
        if pos + size > POOL_BYTES {
            // Out of pool: hand back a null so callers see OOM instead of
            // silently aliasing the start of the pool.
            return core::ptr::null_mut();
        }
        let base = unsafe { core::ptr::addr_of_mut!(POOL) as *mut u8 };
        unsafe { base.add((pos + align) & !(align - 1)) }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {
        unsafe {
            // Bump allocation: frees are no-ops by design.
        }
    }
}

#[global_allocator]
static ALLOC: BumpAlloc = BumpAlloc;

#[panic_handler]
fn steel_panic(_: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}

#[no_mangle]
pub extern "C" fn revm_run(a: u64, b: u64) -> u64 {
    use revm_context_interface::host::DummyHost;
    use revm_interpreter::instruction_context::InstructionContext;
    use revm_interpreter::instructions::{arithmetic, control, memory, stack};
    use revm_interpreter::interpreter_types::{Jumps as _, LoopControl as _};
    use revm_interpreter::{
        InputsImpl, Interpreter, SharedMemory,
        interpreter::{ext_bytecode::ExtBytecode, EthInterpreter},
        interpreter_action::InterpreterAction,
    };
    use revm_interpreter::bytecode::Bytecode;
    use revm_interpreter::primitives::{hardfork::SpecId, Bytes};

    // PUSH32 a; PUSH32 b; ADD; PUSH1 0; MSTORE; PUSH1 32; PUSH1 0; RETURN
    let mut code: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    code.push(0x7f);
    code.extend_from_slice(&a.to_be_bytes());
    code.push(0x7f);
    code.extend_from_slice(&b.to_be_bytes());
    code.push(0x01); // ADD
    code.push(0x60); // PUSH1
    code.push(0x00); //   0 (memory offset)
    code.push(0x52); // MSTORE
    code.push(0x60); // PUSH1
    code.push(0x20); //   32 (length)
    code.push(0x60); // PUSH1
    code.push(0x00); //   0 (offset)
    code.push(0xf3); // RETURN

    let memory = SharedMemory::new();
    let bytecode = ExtBytecode::new(Bytecode::new_raw(Bytes::from(code)));
    let input = InputsImpl::default();
    let mut interpreter =
        Interpreter::<EthInterpreter>::new(memory, bytecode, input, false, SpecId::default(), u64::MAX);

    let host = &mut DummyHost::default();

    // Static-dispatch driver loop: mirrors revm's `Interpreter::step`
    // (opcode fetch, `relative_jump(1)`, static-gas record, instruction
    // execute) with a compile-time `match` instead of revm's fn-pointer
    // `InstructionTable` — an indirect call is unrepresentable in the
    // circuit model. revm itself is unmodified; this is driver-side
    // plumbing over revm's own public instruction functions and gas
    // accounting. Static gas values match revm's table
    // (instructions.rs: STOP=0, ADD=3, PUSHn=3, MSTORE=3, RETURN=0 static).
    loop {
        if !interpreter.bytecode.is_not_end() {
            break;
        }
        let opcode = interpreter.bytecode.opcode();
        interpreter.bytecode.relative_jump(1);
        let static_gas: u64 = match opcode {
            0x01 => 3,                   // ADD
            0x60 | 0x7f => 3,            // PUSH1 / PUSH32
            0x52 => 3,                   // MSTORE
            _ => 0,                      // STOP / RETURN / unknown
        };
        if !interpreter.gas.record_cost_unsafe(static_gas) {
            interpreter.halt_oog();
            break;
        }
        match opcode {
            0x00 => control::stop(InstructionContext {
                interpreter: &mut interpreter,
                host,
            }),
            0x01 => arithmetic::add(InstructionContext {
                interpreter: &mut interpreter,
                host,
            }),
            0x60 => stack::push::<1, _, _>(InstructionContext {
                interpreter: &mut interpreter,
                host,
            }),
            0x7f => stack::push::<32, _, _>(InstructionContext {
                interpreter: &mut interpreter,
                host,
            }),
            0x52 => memory::mstore(InstructionContext {
                interpreter: &mut interpreter,
                host,
            }),
            0xf3 => control::ret(InstructionContext {
                interpreter: &mut interpreter,
                host,
            }),
            _ => control::unknown(InstructionContext {
                interpreter: &mut interpreter,
                host,
            }),
        }
    }
    let action = interpreter.take_next_action();

    match action {
        InterpreterAction::Return(result) => {
            // result.output = the 32 stored bytes, big-endian U256. Return the
            // low 64 bits (last 8 bytes) so small sums compare exactly.
            let out = &result.output;
            let mut low: u64 = 0;
            let base = out.len().saturating_sub(8);
            for &byte in &out[base..] {
                low = (low << 8) | byte as u64;
            }
            low
        }
        _ => 0,
    }
}

/// The UNMODIFIED revm execution path: revm's own `run_plain` loop over
/// its real `InstructionTable` function-pointer dispatch — no driver-side
/// mirror. This is the import-stress shape the static-dispatch driver was
/// built to avoid (indirect calls through a table); the S6c levers (pool
/// packing, specialization, word cells, adder scaling) are what make it
/// tractable to attempt at all. Gated entry for the REVM_UNMODIFIED probe.
#[no_mangle]
pub extern "C" fn revm_run_unmodified(a: u64, b: u64) -> u64 {
    use revm_context_interface::host::DummyHost;
    use revm_interpreter::instructions::instruction_table;
    use revm_interpreter::{
        InputsImpl, Interpreter, SharedMemory,
        interpreter::{ext_bytecode::ExtBytecode, EthInterpreter},
        interpreter_action::InterpreterAction,
        bytecode::Bytecode,
        primitives::{hardfork::SpecId, Bytes},
    };

    // PUSH32 a; PUSH32 b; ADD; PUSH1 0; MSTORE; PUSH1 32; PUSH1 0; RETURN
    let mut code: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    code.push(0x7f);
    code.extend_from_slice(&a.to_be_bytes());
    code.push(0x7f);
    code.extend_from_slice(&b.to_be_bytes());
    code.push(0x01); // ADD
    code.push(0x60); // PUSH1
    code.push(0x00); //   0 (memory offset)
    code.push(0x52); // MSTORE
    code.push(0x60); // PUSH1
    code.push(0x20); //   32 (length)
    code.push(0x60); // PUSH1
    code.push(0x00); //   0 (offset)
    code.push(0xf3); // RETURN

    let memory = SharedMemory::new();
    let bytecode = ExtBytecode::new(Bytecode::new_raw(Bytes::from(code)));
    let input = InputsImpl::default();
    let mut interpreter = Interpreter::<EthInterpreter>::new(
        memory,
        bytecode,
        input,
        false,
        SpecId::default(),
        u64::MAX,
    );
    let mut host = DummyHost::default();
    let table = instruction_table::<EthInterpreter, DummyHost>();
    let action = interpreter.run_plain(&table, &mut host);

    match action {
        InterpreterAction::Return(result) => {
            let out = &result.output;
            let mut low: u64 = 0;
            let base = out.len().saturating_sub(8);
            for &byte in &out[base..] {
                low = (low << 8) | byte as u64;
            }
            low
        }
        _ => 0,
    }
}

#[no_mangle]
pub extern "C" fn main() -> u64 {
    0
}
