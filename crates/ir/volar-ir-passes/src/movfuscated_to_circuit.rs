//! Typed lowering of one movfuscated Volar-IR step into a non-looping circuit.
//!
//! The output is a state transition, not an unrolled program: callers drive
//! `boundary.next_state` until `boundary.terminated` becomes true.

use alloc::collections::BTreeSet;
use alloc::string::String;
use alloc::{vec, vec::Vec};

use volar_ir::circuit::{BStepCircuit, StepCircuitBoundary, VStepCircuit};
use volar_ir::ir::{
    IRBlockId, IRBlockTargetId, IRBlocks, IRBranchTarget, IRStmt, IRTerminator, IRType, IRTypeId,
    IRTypes, IRVarId,
};
use volar_ir_common::{Constant, Node, PolyCoeffs, Type};

use crate::dispatch_accumulator::{
    DispatchBitPrimitives, DispatchSlotPrimitives, emit_select_slot,
};
use crate::lower_ir_to_boolar::{
    ExternalLoweringError, LoweredTables, try_lower_ir_to_boolar_with_tables,
};
use crate::movfuscate::{MovfuscAccumInfo, MovfuscBlockBoundary};
use crate::to_reversible::ValueWatchlist;

/// One source value selected before movfuscation.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct MovfuscationWatch {
    pub block: usize,
    pub var: IRVarId,
}

/// A set of source values to resolve as the movfuscator combines blocks.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct MovfuscationWatchlist {
    pub values: BTreeSet<MovfuscationWatch>,
}

/// A source-value to combined-value correspondence produced by movfuscation.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct MovfuscationWatchMap {
    pub entries: Vec<(MovfuscationWatch, IRVarId)>,
}

impl MovfuscationWatchMap {
    /// Turn successfully resolved combined values into a step-circuit watch
    /// list. Source values that cannot be emitted are never silently added.
    pub fn step_watchlist(&self) -> StepValueWatchlist {
        StepValueWatchlist::from_vars(self.entries.iter().map(|(_, combined)| combined.0))
    }
}

/// Movfuscation output plus metadata that must survive later lowering.
#[derive(Clone, Debug)]
pub struct MovfuscatedProgram<P: Clone = ()> {
    pub blocks: IRBlocks<P>,
    pub boundaries: Vec<MovfuscBlockBoundary>,
    pub accumulation: MovfuscAccumInfo,
    pub watches: MovfuscationWatchMap,
}

/// A watchlist over the value space of a [`VStepCircuit`].
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct StepValueWatchlist {
    pub vars: BTreeSet<u32>,
}

impl StepValueWatchlist {
    pub fn from_vars(vars: impl IntoIterator<Item = u32>) -> Self {
        Self {
            vars: vars.into_iter().collect(),
        }
    }
}

/// One typed step value and its LSB-first Boolar bits.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct StepBitWatchEntry {
    pub source: IRVarId,
    pub bits: Vec<IRVarId>,
}

/// The Boolean side of a [`StepValueWatchlist`].
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct StepBitWatchlist {
    pub entries: Vec<StepBitWatchEntry>,
}

impl StepBitWatchlist {
    /// Flatten into the existing Boolar-value watchlist consumed by
    /// `translate_watchlist` after reversible lowering.
    pub fn as_value_watchlist(&self) -> ValueWatchlist {
        ValueWatchlist::from_vars(
            self.entries
                .iter()
                .flat_map(|entry| entry.bits.iter().map(|bit| bit.0)),
        )
    }
}

/// A typed Volar step together with movfuscation metadata and resolved watches.
#[derive(Clone, Debug)]
pub struct VStepCircuitLowering<P: Clone = ()> {
    pub circuit: VStepCircuit<P>,
    pub boundaries: Vec<MovfuscBlockBoundary>,
    pub accumulation: MovfuscAccumInfo,
    pub watches: MovfuscationWatchMap,
}

/// A Boolar step plus its allocation side tables and translated watches.
#[derive(Clone, Debug)]
pub struct BStepCircuitLowering<P: Clone = ()> {
    pub circuit: BStepCircuit<P>,
    pub tables: LoweredTables,
    pub watches: StepBitWatchlist,
}

/// Why a movfuscated block cannot become a typed step circuit.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum MovfuscatedToCircuitError {
    NotSingleBlock { found: usize },
    MissingControlProvenance,
    UnsupportedTerminator,
    UnsupportedTarget,
    InvalidVar { var: u32 },
    TerminationNotBit { var: u32 },
    StateArity { expected: usize, found: usize },
    StateType { slot: usize },
    ReturnArity { expected: usize, found: usize },
    ReturnType { output: usize },
    UnknownSourceWatch { block: usize, var: u32 },
    UnknownWatchedValue { var: u32 },
    ZeroWidthWatchedValue { var: u32 },
    External(ExternalLoweringError),
}

impl From<ExternalLoweringError> for MovfuscatedToCircuitError {
    fn from(value: ExternalLoweringError) -> Self {
        Self::External(value)
    }
}

/// Convert a movfuscated program to its typed one-step Volar circuit.
///
/// Infrastructure statements derive provenance from the first source
/// statement. Statement-free programs should call
/// [`movfuscated_to_vstep_circuit_with_control_provenance`] instead.
pub fn movfuscated_to_vstep_circuit<P: Clone>(
    program: &MovfuscatedProgram<P>,
    types: &IRTypes,
) -> Result<VStepCircuitLowering<P>, MovfuscatedToCircuitError> {
    let control = program
        .blocks
        .blocks
        .first()
        .and_then(|block| block.stmts.first())
        .map(|node| &node.prov)
        .ok_or(MovfuscatedToCircuitError::MissingControlProvenance)?;
    movfuscated_to_vstep_circuit_with_control_provenance(program, types, control)
}

/// As [`movfuscated_to_vstep_circuit`], with explicit provenance for the
/// synthetic transition gates of a statement-free program.
pub fn movfuscated_to_vstep_circuit_with_control_provenance<P: Clone>(
    program: &MovfuscatedProgram<P>,
    types: &IRTypes,
    control_prov: &P,
) -> Result<VStepCircuitLowering<P>, MovfuscatedToCircuitError> {
    let [block] = &program.blocks.blocks[..] else {
        return Err(MovfuscatedToCircuitError::NotSingleBlock {
            found: program.blocks.blocks.len(),
        });
    };
    let bit_type = types
        .0
        .iter()
        .position(|ty| matches!(ty, IRType::Primitive(Type::Bit)))
        .map(|index| IRTypeId(index as u32))
        .ok_or(MovfuscatedToCircuitError::TerminationNotBit { var: 0 })?;

    let mut var_types = block.params.clone();
    for stmt in &block.stmts {
        var_types.push(stmt_result_type(&stmt.kind, bit_type)?);
    }
    let mut emitter = StepEmitter::new(var_types.len() as u32, bit_type, control_prov.clone());
    let params: Vec<IRVarId> = (0..block.params.len() as u32).map(IRVarId).collect();

    let boundary = lower_terminator(&block.terminator, &params, &var_types, types, &mut emitter)?;
    let mut stmts = block.stmts.clone();
    stmts.extend(emitter.stmts);
    Ok(VStepCircuitLowering {
        circuit: VStepCircuit {
            oracles: program.blocks.oracles.clone(),
            actions: program.blocks.actions.clone(),
            rngs: program.blocks.rngs.clone(),
            params: block.params.clone(),
            stmts,
            pre_init: program.blocks.pre_init.clone(),
            boundary,
        },
        boundaries: program.boundaries.clone(),
        accumulation: program.accumulation.clone(),
        watches: program.watches.clone(),
    })
}

/// Lower a typed step circuit to its Boolar counterpart, returning allocation
/// tables and watch mappings produced by the same Booleanization run.
pub fn lower_vstep_to_bstep<P: Clone>(
    circuit: &VStepCircuit<P>,
    types: &IRTypes,
    watchlist: &StepValueWatchlist,
) -> Result<BStepCircuitLowering<P>, MovfuscatedToCircuitError> {
    let (blocks, tables) =
        try_lower_ir_to_boolar_with_tables(&circuit.clone().to_ir_blocks(), types)?;
    let bits = |value: IRVarId| -> Result<Vec<IRVarId>, MovfuscatedToCircuitError> {
        let found = tables
            .var_bits
            .bits(0, value.0)
            .ok_or(MovfuscatedToCircuitError::InvalidVar { var: value.0 })?
            .to_vec();
        if found.is_empty() {
            return Err(MovfuscatedToCircuitError::ZeroWidthWatchedValue { var: value.0 });
        }
        Ok(found)
    };
    let terminated = bits(circuit.boundary.terminated)?;
    if terminated.len() != 1 {
        return Err(MovfuscatedToCircuitError::TerminationNotBit {
            var: circuit.boundary.terminated.0,
        });
    }
    let mut next_state = Vec::new();
    for value in &circuit.boundary.next_state {
        next_state.extend(bits(*value)?);
    }
    let mut return_values = Vec::new();
    for value in &circuit.boundary.return_values {
        return_values.extend(bits(*value)?);
    }
    let watches = StepBitWatchlist {
        entries: watchlist
            .vars
            .iter()
            .map(|var| {
                let source = IRVarId(*var);
                let found = tables
                    .var_bits
                    .bits(0, *var)
                    .ok_or(MovfuscatedToCircuitError::UnknownWatchedValue { var: *var })?
                    .to_vec();
                if found.is_empty() {
                    return Err(MovfuscatedToCircuitError::ZeroWidthWatchedValue { var: *var });
                }
                Ok(StepBitWatchEntry {
                    source,
                    bits: found,
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
    };
    let volar_ir::boolar::BIrBlocks {
        blocks: mut lowered_blocks,
        pre_init,
    } = blocks;
    let block = lowered_blocks
        .pop()
        .ok_or(MovfuscatedToCircuitError::NotSingleBlock { found: 0 })?;
    if !lowered_blocks.is_empty() {
        return Err(MovfuscatedToCircuitError::NotSingleBlock {
            found: lowered_blocks.len() + 1,
        });
    }
    Ok(BStepCircuitLowering {
        circuit: BStepCircuit {
            params: block.params,
            stmts: block.stmts,
            pre_init,
            boundary: StepCircuitBoundary {
                terminated: terminated[0],
                next_state,
                return_values,
            },
        },
        tables,
        watches,
    })
}

/// Compact diagnostic view of the state-transition ABI.
pub fn debug_dump_step<P: Clone>(circuit: &VStepCircuit<P>) -> String {
    alloc::format!(
        "params={} stmts={} terminated={} next_state={:?} return_values={:?}",
        circuit.params.len(),
        circuit.stmts.len(),
        circuit.boundary.terminated.0,
        circuit.boundary.next_state,
        circuit.boundary.return_values,
    )
}

/// Boundary-oriented movfuscation diagnostic, including values that remain
/// observable after the original blocks have been combined.
pub fn debug_dump_step_lowering<P: Clone>(lowering: &VStepCircuitLowering<P>) -> String {
    let mut dump = debug_dump_step(&lowering.circuit);
    dump.push_str(&alloc::format!(
        " boundaries={} accumulation_steps={} watches={:?}",
        lowering.boundaries.len(),
        lowering.accumulation.steps.len(),
        lowering.watches.entries,
    ));
    dump
}

struct StepEmitter<P: Clone> {
    stmts: Vec<Node<IRStmt, P>>,
    next_id: u32,
    bit_type: IRTypeId,
    prov: P,
}

impl<P: Clone> StepEmitter<P> {
    fn new(next_id: u32, bit_type: IRTypeId, prov: P) -> Self {
        Self {
            stmts: Vec::new(),
            next_id,
            bit_type,
            prov,
        }
    }

    fn push(&mut self, stmt: IRStmt) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        self.stmts.push(Node::new(stmt, self.prov.clone(), None));
        id
    }

    fn emit_poly(&mut self, coeffs: PolyCoeffs<IRVarId>, constant: u128, ty: IRTypeId) -> u32 {
        self.push(IRStmt::Poly {
            ty,
            coeffs,
            constant: Constant {
                hi: 0,
                lo: constant,
            },
        })
    }
}

impl<P: Clone> DispatchBitPrimitives for StepEmitter<P> {
    fn emit_zero_bit(&mut self) -> u32 {
        self.push(IRStmt::Const(Constant { hi: 0, lo: 0 }, self.bit_type))
    }
    fn emit_one_bit(&mut self) -> u32 {
        self.push(IRStmt::Const(Constant { hi: 0, lo: 1 }, self.bit_type))
    }
    fn emit_and_bit(&mut self, a: u32, b: u32) -> u32 {
        let mut key = vec![IRVarId(a), IRVarId(b)];
        key.sort();
        let mut coeffs = PolyCoeffs::new();
        coeffs.insert(key, 1);
        self.emit_poly(coeffs, 0, self.bit_type)
    }
    fn emit_xor_bit(&mut self, a: u32, b: u32) -> u32 {
        let mut coeffs = PolyCoeffs::new();
        coeffs.insert(vec![IRVarId(a)], 1);
        coeffs.insert(vec![IRVarId(b)], 1);
        self.emit_poly(coeffs, 0, self.bit_type)
    }
    fn emit_not(&mut self, a: u32) -> u32 {
        let mut coeffs = PolyCoeffs::new();
        coeffs.insert(vec![IRVarId(a)], 1);
        self.emit_poly(coeffs, 1, self.bit_type)
    }
}

impl<P: Clone> DispatchSlotPrimitives for StepEmitter<P> {
    type SlotTy = IRTypeId;
    fn emit_zero_slot(&mut self, ty: &IRTypeId) -> u32 {
        self.push(IRStmt::Const(Constant { hi: 0, lo: 0 }, *ty))
    }
    fn emit_gate(&mut self, active: u32, value: u32, ty: &IRTypeId) -> u32 {
        let mut key = vec![IRVarId(active), IRVarId(value)];
        key.sort();
        let mut coeffs = PolyCoeffs::new();
        coeffs.insert(key, 1);
        self.emit_poly(coeffs, 0, *ty)
    }
    fn emit_field_add(&mut self, a: u32, b: u32, ty: &IRTypeId) -> u32 {
        let mut coeffs = PolyCoeffs::new();
        coeffs.insert(vec![IRVarId(a)], 1);
        coeffs.insert(vec![IRVarId(b)], 1);
        self.emit_poly(coeffs, 0, *ty)
    }
}

fn lower_terminator<P: Clone>(
    terminator: &IRTerminator,
    params: &[IRVarId],
    var_types: &[IRTypeId],
    types: &IRTypes,
    emitter: &mut StepEmitter<P>,
) -> Result<StepCircuitBoundary, MovfuscatedToCircuitError> {
    let loop_args = |target: &IRBranchTarget| validate_loop_target(target, params, var_types);
    let returns = |target: &IRBranchTarget| validate_return_target(target, var_types);
    let zero_returns = |return_types: &[IRTypeId], emitter: &mut StepEmitter<P>| {
        return_types
            .iter()
            .map(|ty| IRVarId(emitter.emit_zero_slot(ty)))
            .collect::<Vec<_>>()
    };
    match terminator {
        IRTerminator::Jmp { target } if matches!(target.dest, IRBlockTargetId::Return) => {
            let (values, _) = returns(target)?;
            Ok(StepCircuitBoundary {
                terminated: IRVarId(emitter.emit_one_bit()),
                next_state: params.to_vec(),
                return_values: values,
            })
        }
        IRTerminator::Jmp { target } => Ok(StepCircuitBoundary {
            terminated: IRVarId(emitter.emit_zero_bit()),
            next_state: loop_args(target)?,
            return_values: Vec::new(),
        }),
        IRTerminator::JumpCond {
            condition,
            then_target,
            else_target,
        } => {
            let condition_ty = get_type(var_types, *condition)?;
            if !types.is_bit(condition_ty) {
                return Err(MovfuscatedToCircuitError::TerminationNotBit { var: condition.0 });
            }
            let then_return = matches!(then_target.dest, IRBlockTargetId::Return);
            let else_return = matches!(else_target.dest, IRBlockTargetId::Return);
            match (then_return, else_return) {
                (true, true) => {
                    let (then_values, then_types) = returns(then_target)?;
                    let (else_values, else_types) = returns(else_target)?;
                    validate_return_types(&then_types, &else_types)?;
                    let values = then_values
                        .iter()
                        .zip(else_values)
                        .zip(then_types.iter())
                        .map(|((&then_value, else_value), ty)| {
                            IRVarId(emit_select_slot(
                                emitter,
                                condition.0,
                                then_value.0,
                                else_value.0,
                                ty,
                            ))
                        })
                        .collect();
                    Ok(StepCircuitBoundary {
                        terminated: IRVarId(emitter.emit_one_bit()),
                        next_state: params.to_vec(),
                        return_values: values,
                    })
                }
                (true, false) | (false, true) => {
                    let (return_target, loop_target, done) = if then_return {
                        (then_target, else_target, condition.0)
                    } else {
                        (else_target, then_target, emitter.emit_not(condition.0))
                    };
                    let (return_values, return_types) = returns(return_target)?;
                    let loop_values = loop_args(loop_target)?;
                    let next_state = params
                        .iter()
                        .zip(loop_values)
                        .zip(var_types.iter())
                        .map(|((&current, continuation), ty)| {
                            IRVarId(emit_select_slot(
                                emitter,
                                done,
                                current.0,
                                continuation.0,
                                ty,
                            ))
                        })
                        .collect();
                    let zeros = zero_returns(&return_types, emitter);
                    let return_values = return_values
                        .iter()
                        .zip(zeros)
                        .zip(return_types.iter())
                        .map(|((&value, zero), ty)| {
                            IRVarId(emit_select_slot(emitter, done, value.0, zero.0, ty))
                        })
                        .collect();
                    Ok(StepCircuitBoundary {
                        terminated: IRVarId(done),
                        next_state,
                        return_values,
                    })
                }
                (false, false) => {
                    let then_values = loop_args(then_target)?;
                    let else_values = loop_args(else_target)?;
                    let next_state = then_values
                        .iter()
                        .zip(else_values)
                        .zip(var_types.iter())
                        .map(|((&then_value, else_value), ty)| {
                            IRVarId(emit_select_slot(
                                emitter,
                                condition.0,
                                then_value.0,
                                else_value.0,
                                ty,
                            ))
                        })
                        .collect();
                    Ok(StepCircuitBoundary {
                        terminated: IRVarId(emitter.emit_zero_bit()),
                        next_state,
                        return_values: Vec::new(),
                    })
                }
            }
        }
        _ => Err(MovfuscatedToCircuitError::UnsupportedTerminator),
    }
}

fn validate_loop_target(
    target: &IRBranchTarget,
    params: &[IRVarId],
    var_types: &[IRTypeId],
) -> Result<Vec<IRVarId>, MovfuscatedToCircuitError> {
    if !matches!(target.dest, IRBlockTargetId::Block(IRBlockId(0))) {
        return Err(MovfuscatedToCircuitError::UnsupportedTarget);
    }
    if target.args.len() != params.len() {
        return Err(MovfuscatedToCircuitError::StateArity {
            expected: params.len(),
            found: target.args.len(),
        });
    }
    for (slot, (&arg, &param)) in target.args.iter().zip(params).enumerate() {
        if get_type(var_types, arg)? != get_type(var_types, param)? {
            return Err(MovfuscatedToCircuitError::StateType { slot });
        }
    }
    Ok(target.args.clone())
}

fn validate_return_target(
    target: &IRBranchTarget,
    var_types: &[IRTypeId],
) -> Result<(Vec<IRVarId>, Vec<IRTypeId>), MovfuscatedToCircuitError> {
    if !matches!(target.dest, IRBlockTargetId::Return) {
        return Err(MovfuscatedToCircuitError::UnsupportedTarget);
    }
    let result_types = target
        .args
        .iter()
        .map(|&value| get_type(var_types, value))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((target.args.clone(), result_types))
}

fn validate_return_types(
    expected: &[IRTypeId],
    found: &[IRTypeId],
) -> Result<(), MovfuscatedToCircuitError> {
    if expected.len() != found.len() {
        return Err(MovfuscatedToCircuitError::ReturnArity {
            expected: expected.len(),
            found: found.len(),
        });
    }
    for (output, (&expected, &found)) in expected.iter().zip(found).enumerate() {
        if expected != found {
            return Err(MovfuscatedToCircuitError::ReturnType { output });
        }
    }
    Ok(())
}

fn get_type(var_types: &[IRTypeId], value: IRVarId) -> Result<IRTypeId, MovfuscatedToCircuitError> {
    var_types
        .get(value.0 as usize)
        .copied()
        .ok_or(MovfuscatedToCircuitError::InvalidVar { var: value.0 })
}

fn stmt_result_type(
    stmt: &IRStmt,
    bit_type: IRTypeId,
) -> Result<IRTypeId, MovfuscatedToCircuitError> {
    match stmt {
        IRStmt::StorageRead { ty, .. }
        | IRStmt::Const(_, ty)
        | IRStmt::Rol { ty, .. }
        | IRStmt::Ror { ty, .. }
        | IRStmt::Merge { ty, .. }
        | IRStmt::Splat { ty, .. }
        | IRStmt::Shuffle { ty, .. }
        | IRStmt::OracleOutput { ty, .. }
        | IRStmt::ActionOutput { ty, .. }
        | IRStmt::Rng { ty, .. }
        | IRStmt::Poly { ty, .. } => Ok(*ty),
        IRStmt::Transmute { dst_ty, .. }
        | IRStmt::OracleCall {
            result_ty: dst_ty, ..
        }
        | IRStmt::ActionCall {
            result_ty: dst_ty, ..
        } => Ok(*dst_ty),
        IRStmt::StorageWrite { .. } | IRStmt::ActionStore { .. } => Ok(bit_type),
        _ => Err(MovfuscatedToCircuitError::UnsupportedTerminator),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use volar_ir::ir::{IRBlock, IRBranchTarget};
    use volar_ir_common::{ActionDecl, OracleDecl, PreInitSegment, RngDecl, StorageId, TypeTable};

    fn types() -> (TypeTable, IRTypeId) {
        let mut types = TypeTable::new();
        let bit = types.bit();
        (types, bit)
    }

    fn empty_accumulation() -> MovfuscAccumInfo {
        crate::movfuscate::MovfuscAccumInfo {
            init: crate::movfuscate::MovfuscAccumInit {
                start: 0,
                end: 0,
                done_acc: 0,
                next_pc: Vec::new(),
                next_state: Vec::new(),
                ret_vals: Vec::new(),
                synthetic_out: Vec::new(),
                synthetic_in: Vec::new(),
            },
            steps: Vec::new(),
        }
    }

    fn program(params: Vec<IRTypeId>, terminator: IRTerminator) -> MovfuscatedProgram<()> {
        MovfuscatedProgram {
            blocks: IRBlocks {
                oracles: Vec::new(),
                actions: Vec::new(),
                rngs: Vec::new(),
                blocks: vec![IRBlock {
                    params,
                    stmts: Vec::new(),
                    terminator,
                }],
                pre_init: Vec::new(),
            },
            boundaries: Vec::new(),
            accumulation: empty_accumulation(),
            watches: MovfuscationWatchMap::default(),
        }
    }

    fn branch(dest: IRBlockTargetId, args: Vec<IRVarId>) -> IRBranchTarget {
        IRBranchTarget::new(dest, args)
    }

    #[test]
    fn return_has_named_termination_and_frozen_state() {
        let (types, bit) = types();
        let program = program(
            vec![bit],
            IRTerminator::Jmp {
                target: branch(IRBlockTargetId::Return, vec![IRVarId(0)]),
            },
        );
        let step = movfuscated_to_vstep_circuit_with_control_provenance(&program, &types, &())
            .expect("return is a valid step terminator");
        assert_eq!(step.circuit.boundary.next_state, vec![IRVarId(0)]);
        assert_eq!(step.circuit.boundary.return_values, vec![IRVarId(0)]);
        assert_eq!(step.circuit.outputs().len(), 3);
        assert_eq!(
            step.circuit.stmts.len(),
            1,
            "only the done constant is added"
        );
        let flat = step.circuit.to_ir_blocks();
        assert!(flat.is_circuit());
        let IRTerminator::Jmp { target } = &flat.blocks[0].terminator else {
            panic!("step conversion must return");
        };
        assert_eq!(target.args.len(), 3);
    }

    #[test]
    fn preserves_declarations_and_preinit_without_copying_the_body() {
        let (types, bit) = types();
        let mut program = program(
            vec![bit],
            IRTerminator::Jmp {
                target: branch(IRBlockTargetId::Return, vec![IRVarId(1)]),
            },
        );
        program.blocks.blocks[0].stmts.push(Node::new(
            IRStmt::Const(Constant { hi: 0, lo: 1 }, bit),
            (),
            None,
        ));
        program.blocks.oracles.push(OracleDecl {
            name: "oracle".into(),
            params: vec![bit],
            results: vec![bit],
        });
        program.blocks.actions.push(ActionDecl {
            name: "action".into(),
            params: vec![bit],
            results: vec![bit],
        });
        program.blocks.rngs.push(RngDecl {
            name: "rng".into(),
            ty: bit,
        });
        program.blocks.pre_init.push(PreInitSegment {
            storage: StorageId(7),
            ty: bit,
            offset: 0,
            data: vec![Constant { hi: 0, lo: 1 }],
        });

        let step = movfuscated_to_vstep_circuit(&program, &types).expect("step lowers");
        assert_eq!(step.circuit.stmts.len(), 2, "body plus one done constant");
        assert_eq!(step.circuit.oracles, program.blocks.oracles);
        assert_eq!(step.circuit.actions, program.blocks.actions);
        assert_eq!(step.circuit.rngs, program.blocks.rngs);
        assert_eq!(step.circuit.pre_init, program.blocks.pre_init);

        let boolar = lower_vstep_to_bstep(&step.circuit, &types, &StepValueWatchlist::default())
            .expect("Boolar adapter lowers pre-init");
        assert_eq!(boolar.circuit.pre_init.len(), 1);
    }

    #[test]
    fn self_loop_has_false_termination_and_no_payload() {
        let (types, bit) = types();
        let program = program(
            vec![bit],
            IRTerminator::Jmp {
                target: branch(IRBlockTargetId::Block(IRBlockId(0)), vec![IRVarId(0)]),
            },
        );
        let step = movfuscated_to_vstep_circuit_with_control_provenance(&program, &types, &())
            .expect("self loop is a valid step terminator");
        assert_eq!(step.circuit.boundary.next_state, vec![IRVarId(0)]);
        assert!(step.circuit.boundary.return_values.is_empty());
        assert_eq!(
            step.circuit.stmts.len(),
            1,
            "only the false constant is added"
        );
    }

    #[test]
    fn conditional_return_freezes_state_and_zeroes_payload_while_running() {
        let (types, bit) = types();
        let program = program(
            vec![bit, bit],
            IRTerminator::JumpCond {
                condition: IRVarId(0),
                then_target: branch(IRBlockTargetId::Return, vec![IRVarId(1)]),
                else_target: branch(
                    IRBlockTargetId::Block(IRBlockId(0)),
                    vec![IRVarId(0), IRVarId(1)],
                ),
            },
        );
        let step = movfuscated_to_vstep_circuit_with_control_provenance(&program, &types, &())
            .expect("conditional return/self-loop is a valid step");
        assert_eq!(step.circuit.boundary.terminated, IRVarId(0));
        assert_eq!(step.circuit.boundary.next_state.len(), 2);
        assert_eq!(step.circuit.boundary.return_values.len(), 1);
        // Two three-gate selects for state, then zero + three-gate select for
        // the invalid return payload. No copied iteration body appears.
        assert_eq!(step.circuit.stmts.len(), 10);
    }

    #[test]
    fn rejects_malformed_targets_and_signatures() {
        let (types, bit) = types();
        let bad_target = program(
            vec![bit],
            IRTerminator::Jmp {
                target: branch(IRBlockTargetId::Block(IRBlockId(1)), vec![IRVarId(0)]),
            },
        );
        assert!(matches!(
            movfuscated_to_vstep_circuit_with_control_provenance(&bad_target, &types, &()),
            Err(MovfuscatedToCircuitError::UnsupportedTarget)
        ));

        let bad_arity = program(
            vec![bit],
            IRTerminator::Jmp {
                target: branch(IRBlockTargetId::Block(IRBlockId(0)), Vec::new()),
            },
        );
        assert!(matches!(
            movfuscated_to_vstep_circuit_with_control_provenance(&bad_arity, &types, &()),
            Err(MovfuscatedToCircuitError::StateArity {
                expected: 1,
                found: 0,
            })
        ));
    }

    #[test]
    fn missing_provenance_fails_closed() {
        let (types, bit) = types();
        let program = program(
            vec![bit],
            IRTerminator::Jmp {
                target: branch(IRBlockTargetId::Return, vec![IRVarId(0)]),
            },
        );
        assert!(matches!(
            movfuscated_to_vstep_circuit(&program, &types),
            Err(MovfuscatedToCircuitError::MissingControlProvenance)
        ));
    }

    #[test]
    fn boolar_adapter_rejects_unavailable_watches() {
        let (types, bit) = types();
        let program = program(
            vec![bit],
            IRTerminator::Jmp {
                target: branch(IRBlockTargetId::Return, vec![IRVarId(0)]),
            },
        );
        let step = movfuscated_to_vstep_circuit_with_control_provenance(&program, &types, &())
            .expect("step lowers");
        assert!(matches!(
            lower_vstep_to_bstep(&step.circuit, &types, &StepValueWatchlist::from_vars([99])),
            Err(MovfuscatedToCircuitError::UnknownWatchedValue { var: 99 })
        ));
    }

    #[test]
    fn watch_bits_share_the_boolean_allocation_and_translate_to_wires() {
        let (types, bit) = types();
        let program = program(
            vec![bit],
            IRTerminator::Jmp {
                target: branch(IRBlockTargetId::Return, vec![IRVarId(0)]),
            },
        );
        let step = movfuscated_to_vstep_circuit_with_control_provenance(&program, &types, &())
            .expect("step lowers");
        let lowered =
            lower_vstep_to_bstep(&step.circuit, &types, &StepValueWatchlist::from_vars([0]))
                .expect("watch lowers from the same allocation run");
        assert_eq!(lowered.watches.entries[0].bits, vec![IRVarId(0)]);
        let circuit = lowered.circuit.to_b_circuit();
        let (_reversible, map) =
            crate::to_reversible::to_reversible(&circuit).expect("pure step circuit reverses");
        let translated =
            crate::to_reversible::translate_watchlist(&lowered.watches.as_value_watchlist(), &map)
                .expect("bit watch translates to reversible wires");
        assert_eq!(translated.entries[0].var, 0);
    }
}
