//! Hand-authored WAT for the bounded RV32I interpreter fixture.

use crate::interp::{MAX_STEPS, RESULT_ADDR};

mod opcode {
    pub const ADDI: u32 = 0x13;
    pub const ADD: u32 = 0x33;
    pub const LW: u32 = 0x03;
    pub const SW: u32 = 0x23;
    pub const BEQ: u32 = 0x63;
    pub const JAL: u32 = 0x6F;
}

fn escape_bytes(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 4);
    for b in bytes {
        s.push_str(&format!("\\{b:02x}"));
    }
    s
}

fn get_reg(idx_local: &str, out_local: &str) -> String {
    let mut s = format!("(local.set {out_local} (i32.const 0))\n");
    for r in 1..=5u32 {
        s.push_str(&format!(
            "(if (i32.eq (local.get {idx_local}) (i32.const {r})) (then (local.set {out_local} (local.get $r{r}))))\n"
        ));
    }
    s
}

fn set_reg(idx_local: &str, val_local: &str) -> String {
    let mut s = String::new();
    for r in 1..=5u32 {
        s.push_str(&format!(
            "(if (i32.eq (local.get {idx_local}) (i32.const {r})) (then (local.set $r{r} (local.get {val_local}))))\n"
        ));
    }
    s
}

/// Build the full interpreter module for a program and its initial data RAM.
/// The two 32-byte fixture images fit the configured five-bit address space.
pub fn interpreter_wat(code_bytes: &[u8], data_bytes: &[u8]) -> String {
    let code_pages = code_bytes.len().div_ceil(65536).max(1);
    let data_pages = data_bytes.len().div_ceil(65536).max(1);

    format!(
        r#"(module
  (memory $code {code_pages})
  (memory $data {data_pages})
  (export "code" (memory $code))
  (export "data" (memory $data))
  (data (memory $code) (i32.const 0) "{code_data}")
  (data (memory $data) (i32.const 0) "{init_data}")

  (func (export "run") (result i32)
    (local $pc i32) (local $next_pc i32) (local $steps i32) (local $halted i32)
    (local $r1 i32) (local $r2 i32) (local $r3 i32) (local $r4 i32) (local $r5 i32)
    (local $word i32) (local $opcode i32) (local $rd i32) (local $rs1 i32) (local $rs2 i32)
    (local $rs1v i32) (local $rs2v i32)
    (local $imm_i i32) (local $imm_s i32) (local $imm_b i32) (local $imm_j i32)
    (local $addr i32) (local $result i32)

    (block $exit
      (loop $L
        (br_if $exit (i32.ge_s (local.get $steps) (i32.const {max_steps})))
        (local.set $steps (i32.add (local.get $steps) (i32.const 1)))
        (br_if $exit (local.get $halted))
        (local.set $word (i32.load $code (local.get $pc)))
        (local.set $opcode (i32.and (local.get $word) (i32.const 0x7F)))
        (local.set $rd (i32.and (i32.shr_u (local.get $word) (i32.const 7)) (i32.const 0x1F)))
        (local.set $rs1 (i32.and (i32.shr_u (local.get $word) (i32.const 15)) (i32.const 0x1F)))
        (local.set $rs2 (i32.and (i32.shr_u (local.get $word) (i32.const 20)) (i32.const 0x1F)))
        (local.set $imm_i (i32.shr_s (local.get $word) (i32.const 20)))
        (local.set $imm_s
          (i32.or
            (i32.and (local.get $imm_i) (i32.const -32))
            (i32.and (i32.shr_u (local.get $word) (i32.const 7)) (i32.const 0x1F))))
        (local.set $imm_b
          (i32.shr_s
            (i32.shl
              (i32.or
                (i32.or
                  (i32.and (i32.shr_u (local.get $word) (i32.const 19)) (i32.const 0x1000))
                  (i32.and (i32.shl (local.get $word) (i32.const 4)) (i32.const 0x800)))
                (i32.or
                  (i32.and (i32.shr_u (local.get $word) (i32.const 20)) (i32.const 0x7E0))
                  (i32.and (i32.shr_u (local.get $word) (i32.const 7)) (i32.const 0x1E))))
              (i32.const 19))
            (i32.const 19)))
        (local.set $imm_j
          (i32.shr_s
            (i32.shl
              (i32.or
                (i32.or
                  (i32.and (i32.shr_u (local.get $word) (i32.const 11)) (i32.const 0x100000))
                  (i32.and (local.get $word) (i32.const 0xFF000)))
                (i32.or
                  (i32.and (i32.shr_u (local.get $word) (i32.const 9)) (i32.const 0x800))
                  (i32.and (i32.shr_u (local.get $word) (i32.const 20)) (i32.const 0x7FE))))
              (i32.const 11))
            (i32.const 11)))
{get_rs1v}
{get_rs2v}
        (local.set $next_pc (i32.add (local.get $pc) (i32.const 4)))
        (if (i32.eq (local.get $opcode) (i32.const {op_addi}))
          (then (local.set $result (i32.add (local.get $rs1v) (local.get $imm_i)))
{set_result_to_rd}
          ))
        (if (i32.eq (local.get $opcode) (i32.const {op_add}))
          (then (local.set $result (i32.add (local.get $rs1v) (local.get $rs2v)))
{set_result_to_rd}
          ))
        (if (i32.eq (local.get $opcode) (i32.const {op_lw}))
          (then
            (local.set $addr (i32.add (local.get $rs1v) (local.get $imm_i)))
            (local.set $result (i32.load $data (local.get $addr)))
{set_result_to_rd}
          ))
        (if (i32.eq (local.get $opcode) (i32.const {op_sw}))
          (then
            (local.set $addr (i32.add (local.get $rs1v) (local.get $imm_s)))
            (i32.store $data (local.get $addr) (local.get $rs2v))
            (local.set $halted (i32.const 1))
          ))
        (if (i32.eq (local.get $opcode) (i32.const {op_beq}))
          (then
            (if (i32.eq (local.get $rs1v) (local.get $rs2v))
              (then (local.set $next_pc (i32.add (local.get $pc) (local.get $imm_b)))))
          ))
        (if (i32.eq (local.get $opcode) (i32.const {op_jal}))
          (then
            (local.set $result (local.get $next_pc))
{set_result_to_rd}
            (local.set $next_pc (i32.add (local.get $pc) (local.get $imm_j)))
          ))
        (local.set $pc (local.get $next_pc))
        (br $L)
      )
    )
    (local.get $r3)
  )
)
"#,
        code_pages = code_pages,
        data_pages = data_pages,
        code_data = escape_bytes(code_bytes),
        init_data = escape_bytes(data_bytes),
        max_steps = MAX_STEPS,
        get_rs1v = get_reg("$rs1", "$rs1v"),
        get_rs2v = get_reg("$rs2", "$rs2v"),
        set_result_to_rd = set_reg("$rd", "$result"),
        op_addi = opcode::ADDI,
        op_add = opcode::ADD,
        op_lw = opcode::LW,
        op_sw = opcode::SW,
        op_beq = opcode::BEQ,
        op_jal = opcode::JAL,
    )
}

pub fn test_program_wat() -> String {
    interpreter_wat(&crate::interp::program_bytes(), &crate::interp::initial_data_bytes())
}

#[allow(dead_code)]
const _: i32 = RESULT_ADDR;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpreter_wat_assembles() {
        wat::parse_str(test_program_wat()).expect("WAT should assemble");
    }

    #[test]
    fn interpreter_wat_matches_the_rust_reference() {
        let wasm = wat::parse_str(test_program_wat()).expect("WAT should assemble");
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, &wasm).expect("WASM should validate");
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).expect("instantiate WAT");
        let run = instance
            .get_typed_func::<(), i32>(&mut store, "run")
            .expect("run export must be callable");

        let actual_sum = run.call(&mut store, ()).expect("bounded RISC program should halt");
        let memory = instance.get_memory(&mut store, "data").expect("data memory export");
        let mut actual_bytes = [0; 4];
        memory
            .read(&store, crate::interp::RESULT_ADDR as usize, &mut actual_bytes)
            .expect("stored sum should be in bounds");

        let program = crate::interp::assemble_program();
        let mut expected_memory = crate::interp::initial_data_bytes();
        let expected_sum = crate::interp::native_reference(&program, &mut expected_memory);
        let expected_stored = i32::from_le_bytes(
            expected_memory[crate::interp::RESULT_ADDR as usize..crate::interp::RESULT_ADDR as usize + 4]
                .try_into()
                .expect("result word"),
        );
        assert_eq!(actual_sum, expected_sum);
        assert_eq!(i32::from_le_bytes(actual_bytes), expected_stored);
    }
}
