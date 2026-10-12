// @pinnedness: unpinned
// @stability: very-unstable
// @ai: assisted

//! The generator-agnostic EVM emission seam.

use core::fmt;

use volar_ir::circuit::BCircuit;
use volar_ir_simd::{LaneOp, LanePlan};

/// A streaming sink for lane-scheduled 256-bit word operations.
///
/// The vocabulary is deliberately neutral — no Yul syntax, no opcode names,
/// no byte types — so any EVM code generator (Solidity inline assembly, raw
/// bytecode, Huff, a standalone Yul pipeline) can implement it. `slot`
/// values are abstract scratch-word indices owned by the caller's
/// [`LanePlan`]; each emitter maps them to stack variables, memory, or its
/// own scheme. Slots are SSA: every slot is defined exactly once, by
/// [`EvmEmit::input`], [`EvmEmit::constant`], or one gate method.
///
/// Emitters that want a whole-program view (e.g. stack scheduling) can
/// ignore this trait and consume the public [`LanePlan`] directly.
pub trait EvmEmit {
    /// The finished artifact (source text, bytecode, ...).
    type Output;
    /// Bind packed input word `index` to `slot`.
    fn input(&mut self, slot: u32, index: u32);
    /// Define `slot` as all-zero or all-one lanes.
    fn constant(&mut self, slot: u32, value: bool);
    /// Define `dst` as the lane-wise AND of `a` and `b`.
    fn and(&mut self, dst: u32, a: u32, b: u32);
    /// Define `dst` as the lane-wise OR of `a` and `b`.
    fn or(&mut self, dst: u32, a: u32, b: u32);
    /// Define `dst` as the lane-wise XOR of `a` and `b`.
    fn xor(&mut self, dst: u32, a: u32, b: u32);
    /// Define `dst` as the lane-wise inversion of `a`.
    fn not(&mut self, dst: u32, a: u32);
    /// Emit `slot` as packed output word `index`.
    fn output(&mut self, index: u32, slot: u32);
    /// Finish and return the artifact.
    fn finish(self) -> Self::Output;
}

/// Why a circuit could not be emitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EvmEmitError {
    /// The circuit is not lane-compatible: it uses storage, oracle, action,
    /// or RNG statements. There is no scalar EVM fallback; callers must
    /// keep such circuits on another path.
    NotLaneCompatible,
}

impl fmt::Display for EvmEmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EvmEmitError::NotLaneCompatible => write!(
                f,
                "circuit is not lane-compatible (storage, oracle, action, or RNG statements); \
                 there is no scalar EVM fallback"
            ),
        }
    }
}

impl std::error::Error for EvmEmitError {}

/// Drive `emit` with every operation in `plan`, in schedule order.
pub fn emit_plan<E: EvmEmit>(plan: &LanePlan, mut emit: E) -> E::Output {
    // Inputs occupy slots `0..num_params`, so slot and index coincide.
    for index in plan.input_slots() {
        emit.input(index, index);
    }
    for stmt in &plan.stmts {
        match stmt.op {
            LaneOp::Const(value) => emit.constant(stmt.dst, value),
            LaneOp::And(a, b) => emit.and(stmt.dst, a, b),
            LaneOp::Or(a, b) => emit.or(stmt.dst, a, b),
            LaneOp::Xor(a, b) => emit.xor(stmt.dst, a, b),
            LaneOp::Not(a) => emit.not(stmt.dst, a),
        }
    }
    for (index, &slot) in plan.outputs.iter().enumerate() {
        emit.output(index as u32, slot);
    }
    emit.finish()
}

/// Plan `circuit` with `volar-ir-simd` and emit it through `emit`.
pub fn emit_circuit<E: EvmEmit, P: Clone>(
    circuit: &BCircuit<P>,
    emit: E,
) -> Result<E::Output, EvmEmitError> {
    let plan = volar_ir_simd::plan_lanes(circuit).ok_or(EvmEmitError::NotLaneCompatible)?;
    Ok(emit_plan(&plan, emit))
}
