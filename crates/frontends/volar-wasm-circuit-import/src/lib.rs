//! Direct WAFFLE/WASM import into a control-free [`VCircuit`].
//!
//! Unlike the legacy route this crate never builds VAFFLE.  It expands direct
//! calls at the source level, emits integer operations straight into Volar IR,
//! and represents symbolic acyclic branches by emitting both paths and MUXing
//! their results.  A loop is accepted only when its path through WAFFLE's CFG
//! is statically finite; no iteration limit is treated as program semantics.

#![no_std]
extern crate alloc;

use alloc::{
    collections::BTreeMap,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use core::convert::Infallible;
use core::fmt;

use portal_pc_waffle_frontend::{FrontendOptions, ModuleExt};
use portal_pc_waffle_ir::{
    Block, ExportKind, Func, FuncDecl, FunctionBody, MemoryArg, Module as WModule, Operator,
    Terminator, Type as WType, Value as WValue, ValueDef, cfg::CFGInfo, entity::EntityRef,
};
use volar_circuit_exec_core::{
    CircuitEmitter, ControlError, ControlLimits, StaticControlPlan, branch_guards, guarded_value,
};
use volar_ir::{
    circuit::VCircuit,
    ir::{Constant, IRStmt, IRTypeId, IRTypes, IRVarId, PreInitSegment, StorageId},
};
use volar_ir_common::{IrType, PolyCoeffs, StoragePurpose, StorageRegistry};
use volar_lir::{
    BitCircuitBuilder, IcmpPred,
    circuits::{
        bc_add, bc_ashr, bc_clz, bc_ctz, bc_eq, bc_lshr, bc_mul, bc_ne, bc_or_vec, bc_popcnt,
        bc_rotl, bc_rotr, bc_sdiv, bc_sle, bc_slt, bc_srem, bc_sub, bc_udiv, bc_ule, bc_ult,
        bc_urem, bc_xor_vec,
    },
};

/// Options for [`import_module`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WasmCircuitImportOptions {
    /// Restrict memory storage addresses to this many low i32 bits after
    /// normal 32-bit effective-address arithmetic. `None` keeps all 32 bits.
    pub memory_address_bits: Option<usize>,
    /// Compiler-resource limits for static control expansion. They reject an
    /// import before it becomes too large; they never define a loop bound.
    pub control_limits: ControlLimits,
}

impl Default for WasmCircuitImportOptions {
    fn default() -> Self {
        Self {
            memory_address_bits: None,
            control_limits: ControlLimits::default(),
        }
    }
}

/// Caller-owned memory metadata required to execute a [`VCircuit`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WasmMemoryLayout {
    /// WAFFLE/WASM memory index.
    pub memory: u32,
    /// Address width presented to the circuit's memory storage operations.
    pub address_bits: usize,
}

/// The direct-import result.
///
/// [`VCircuit`] intentionally has no module-level storage initializer table,
/// so active WASM data segments live alongside the circuit instead of being
/// silently dropped during fusion.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WasmCircuitArtifact {
    /// The executable, control-free Volar IR.
    pub circuit: VCircuit,
    /// Type table referenced by `circuit` and `pre_init`.
    pub types: IRTypes,
    /// Active WASM data-segment initialization.
    pub pre_init: Vec<PreInitSegment>,
    /// Storage spaces required by the circuit.
    pub memories: Vec<WasmMemoryLayout>,
}

/// A direct-import error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ImportError {
    /// The requested function did not exist.
    EntryNotFound(String),
    /// The requested function or a reachable callee is imported.
    ImportedFunction(String),
    /// An operation/type/control form is outside this importer's subset.
    Unsupported(String),
    /// Exact circuit expansion could not be proven finite.
    Control(ControlError),
    /// A direct call would recursively re-enter a function.
    RecursiveCall(Vec<String>),
    /// WAFFLE could not expand a lazy source body.
    Waffle(String),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EntryNotFound(name) => write!(f, "WASM entry `{name}` does not exist"),
            Self::ImportedFunction(name) => {
                write!(f, "WASM import `{name}` is not valid in VCircuit mode")
            }
            Self::Unsupported(message) => write!(f, "unsupported direct WASM import: {message}"),
            Self::Control(error) => write!(f, "cannot form a finite circuit: {error:?}"),
            Self::RecursiveCall(stack) => {
                write!(f, "recursive direct call: {}", stack.join(" -> "))
            }
            Self::Waffle(message) => write!(f, "failed to expand WAFFLE body: {message}"),
        }
    }
}

/// Parse WASM bytes and import one named entry directly into a circuit.
///
/// This is the convenient public entry point for embedders. Use
/// [`import_module`] when a caller already owns a WAFFLE module or needs to
/// share one parsing pass across several imports.
pub fn import_wasm_bytes(
    wasm: &[u8],
    entry: &str,
    options: WasmCircuitImportOptions,
) -> Result<WasmCircuitArtifact, ImportError> {
    let module = WModule::from_wasm_bytes(wasm, &FrontendOptions::default())
        .map_err(|error| ImportError::Waffle(error.to_string()))?;
    import_module(&module, entry, options)
}

/// Import one named function from an already-parsed WAFFLE module.
///
/// Entry parameters must be i32 or i64 and become free circuit bits in ABI
/// order.  Internal direct calls are inlined.  Oracle/action imports are
/// rejected because a bare [`VCircuit`] has no declaration table for them.
pub fn import_module<'a>(
    wasm: &'a WModule<'a>,
    entry: &str,
    options: WasmCircuitImportOptions,
) -> Result<WasmCircuitArtifact, ImportError> {
    let entry_func = wasm
        .exports
        .iter()
        .find_map(|export| {
            (export.name == entry).then(|| match &export.kind {
                ExportKind::Func(function) => Some(*function),
                _ => None,
            })
        })
        .flatten()
        .or_else(|| {
            wasm.funcs
                .entries()
                .find_map(|(func, decl)| (decl.name() == entry).then_some(func))
        })
        .ok_or_else(|| ImportError::EntryNotFound(entry.to_string()))?;

    let mut compiler = Compiler::new(wasm, options);
    let body = compiler.body(entry_func)?;
    let mut entry_args = Vec::with_capacity(body.n_params);
    for &(ty, _) in &body.blocks[body.entry].params {
        entry_args.push(compiler.input_value(ty)?);
    }
    let globals = compiler.initial_globals()?;
    let mut plan = StaticControlPlan::new(options.control_limits);
    let mut stack = Vec::new();
    let active = compiler.emitter.bc_const(true);
    let result = compiler.compile_function(
        entry_func, entry_args, globals, active, &mut plan, &mut stack,
    )?;
    compiler.emitter.circuit.outputs = result
        .outputs
        .iter()
        .flat_map(|value| value.bits.iter().copied())
        .collect();

    let pre_init = compiler.pre_init();
    let memories = compiler.memory_layouts();
    let (circuit, types) = compiler.emitter.finish();
    Ok(WasmCircuitArtifact {
        circuit,
        pre_init,
        memories,
        types,
    })
}

/// Registry-mode variant of [`import_module`] (claim mode): every declared
/// WASM linear memory keeps its external
/// [`StorageId::memory`](volar_ir::ir::StorageId::memory) indexing
/// convention, but the space is *claimed* in `registry` (one registry per
/// module) under [`StoragePurpose::WasmMemory`] — failing closed with a
/// named error if another consumer already owns it, instead of silently
/// sharing the space.
pub fn import_module_with_registry<'a>(
    wasm: &'a WModule<'a>,
    entry: &str,
    options: WasmCircuitImportOptions,
    registry: &mut StorageRegistry<StoragePurpose>,
) -> Result<WasmCircuitArtifact, ImportError> {
    for (memory, _) in wasm.memories.entries() {
        registry
            .claim(
                StorageId::memory(memory.index() as u32),
                StoragePurpose::WasmMemory {
                    index: memory.index() as u32,
                },
            )
            .map_err(|e| {
                ImportError::Unsupported(alloc::format!("wasm linear memory storage: {e}"))
            })?;
    }
    import_module(wasm, entry, options)
}

#[derive(Clone, Debug)]
struct Value {
    bits: Vec<IRVarId>,
    ty: WType,
    known: Option<u64>,
}

impl Value {
    fn width(&self) -> usize {
        self.bits.len()
    }

    fn with_bits(bits: Vec<IRVarId>, ty: WType, known: Option<u64>) -> Self {
        let width = bits.len();
        Self {
            bits,
            ty,
            known: known.map(|value| mask(value, width)),
        }
    }
}

#[derive(Clone)]
struct State {
    values: BTreeMap<WValue, Value>,
    globals: Vec<Value>,
    active: IRVarId,
    /// Distinguishes separate, sequential inlined invocations for static
    /// loop-state detection. Re-visiting a block in one frame is a loop;
    /// revisiting it in a later call frame is not.
    frame: u64,
}

struct ResultState {
    outputs: Vec<Value>,
    globals: Vec<Value>,
}

/// Direct, single-block Volar IR emission.
struct Emitter {
    types: IRTypes,
    circuit: VCircuit,
    bit: IRTypeId,
    address_types: BTreeMap<usize, IRTypeId>,
    byte: Option<IRTypeId>,
}

impl Emitter {
    fn new() -> Self {
        let mut types = IRTypes::new();
        let bit = types.bit();
        Self {
            types,
            circuit: VCircuit::new(Vec::new()),
            bit,
            address_types: BTreeMap::new(),
            byte: None,
        }
    }

    fn input(&mut self, ty: WType) -> Result<Value, ImportError> {
        let width = width(ty)?;
        let mut bits = Vec::with_capacity(width);
        for _ in 0..width {
            let id = IRVarId(self.circuit.params.len() as u32);
            self.circuit.params.push(self.bit);
            bits.push(id);
        }
        Ok(Value::with_bits(bits, ty, None))
    }

    fn emit(&mut self, stmt: IRStmt) -> IRVarId {
        self.circuit.push_stmt(stmt, ())
    }

    fn address_type(&mut self, bits: usize) -> IRTypeId {
        if let Some(existing) = self.address_types.get(&bits) {
            return *existing;
        }
        let ty = self.types.intern(IrType::Vec(bits, self.bit));
        self.address_types.insert(bits, ty);
        ty
    }

    fn byte_type(&mut self) -> IRTypeId {
        if let Some(existing) = self.byte {
            return existing;
        }
        let ty = self.types.intern(IrType::Vec(8, self.bit));
        self.byte = Some(ty);
        ty
    }

    fn merge(&mut self, parts: &[IRVarId], ty: IRTypeId) -> IRVarId {
        if parts.len() == 1 {
            return parts[0];
        }
        self.emit(IRStmt::Merge {
            parts: parts.to_vec(),
            ty,
        })
    }

    fn finish(self) -> (VCircuit, IRTypes) {
        (self.circuit, self.types)
    }
}

impl BitCircuitBuilder for Emitter {
    type Bit = IRVarId;

    fn bc_const(&mut self, value: bool) -> IRVarId {
        self.emit(IRStmt::Const(
            Constant {
                hi: 0,
                lo: value as u128,
            },
            self.bit,
        ))
    }

    fn bc_poly(&mut self, coeffs: PolyCoeffs<IRVarId>, constant: u128) -> IRVarId {
        self.emit(IRStmt::Poly {
            ty: self.bit,
            coeffs,
            constant: Constant {
                hi: 0,
                lo: constant,
            },
        })
    }
}

impl CircuitEmitter for Emitter {
    type Wire = IRVarId;
    type Error = Infallible;

    fn constant(&mut self, value: bool) -> Result<IRVarId, Infallible> {
        Ok(self.bc_const(value))
    }
    fn and(&mut self, left: IRVarId, right: IRVarId) -> Result<IRVarId, Infallible> {
        Ok(self.bc_and(left, right))
    }
    fn or(&mut self, left: IRVarId, right: IRVarId) -> Result<IRVarId, Infallible> {
        Ok(self.bc_or(left, right))
    }
    fn xor(&mut self, left: IRVarId, right: IRVarId) -> Result<IRVarId, Infallible> {
        Ok(self.bc_xor(left, right))
    }
    fn mux(
        &mut self,
        condition: IRVarId,
        then_value: IRVarId,
        else_value: IRVarId,
    ) -> Result<IRVarId, Infallible> {
        Ok(self.bc_select(condition, then_value, else_value))
    }
}

struct Compiler<'a> {
    wasm: &'a WModule<'a>,
    options: WasmCircuitImportOptions,
    emitter: Emitter,
    next_frame: u64,
}

impl<'a> Compiler<'a> {
    fn new(wasm: &'a WModule<'a>, options: WasmCircuitImportOptions) -> Self {
        Self {
            wasm,
            options,
            emitter: Emitter::new(),
            next_frame: 0,
        }
    }

    fn body(&self, function: Func) -> Result<FunctionBody, ImportError> {
        let declaration = &self.wasm.funcs[function];
        if matches!(declaration, FuncDecl::Import(..)) {
            return Err(ImportError::ImportedFunction(
                declaration.name().to_string(),
            ));
        }
        portal_pc_waffle_frontend::clone_and_expand_body(self.wasm, function)
            .map_err(|error| ImportError::Waffle(error.to_string()))
    }

    fn input_value(&mut self, ty: WType) -> Result<Value, ImportError> {
        self.emitter.input(ty)
    }

    fn initial_globals(&mut self) -> Result<Vec<Value>, ImportError> {
        self.wasm
            .globals
            .entries()
            .filter(|(_, global)| global.mutable)
            .map(|(_, global)| self.constant(global.ty, global.value.unwrap_or(0) as u64))
            .collect()
    }

    fn compile_function(
        &mut self,
        function: Func,
        args: Vec<Value>,
        globals: Vec<Value>,
        active: IRVarId,
        plan: &mut StaticControlPlan<(u64, usize, usize, Vec<Option<u64>>)>,
        stack: &mut Vec<Func>,
    ) -> Result<ResultState, ImportError> {
        if stack.contains(&function) {
            let mut names: Vec<String> = stack
                .iter()
                .map(|func| self.wasm.funcs[*func].name().to_string())
                .collect();
            names.push(self.wasm.funcs[function].name().to_string());
            return Err(ImportError::RecursiveCall(names));
        }
        plan.enter_call().map_err(ImportError::Control)?;
        stack.push(function);
        let body = self.body(function)?;
        self.validate_control(&body)?;
        let entry = &body.blocks[body.entry];
        if entry.params.len() != args.len() {
            return Err(ImportError::Unsupported(alloc::format!(
                "call to `{}` has {} arguments, expected {}",
                self.wasm.funcs[function].name(),
                args.len(),
                entry.params.len()
            )));
        }
        let frame = self.next_frame;
        self.next_frame = self
            .next_frame
            .checked_add(1)
            .ok_or_else(|| ImportError::Control(ControlError::ResourceLimit))?;
        let mut state = State {
            values: BTreeMap::new(),
            globals,
            active,
            frame,
        };
        for ((_, value), arg) in entry.params.iter().zip(args) {
            state.values.insert(*value, arg);
        }
        let result = self.compile_block(function, &body, body.entry, state, plan, stack);
        stack.pop();
        plan.leave_call();
        result
    }

    /// Accept only reducible CFGs whose natural-loop headers have one latch.
    /// This keeps direct control expansion aligned with structured WASM and
    /// avoids choosing arbitrary semantics for irreducible jump graphs.
    fn validate_control(&self, body: &FunctionBody) -> Result<(), ImportError> {
        body.verify_reducible().map_err(|error| {
            ImportError::Unsupported(alloc::format!("irreducible control flow: {error}"))
        })?;

        let cfg = CFGInfo::new(body);
        let mut latches: BTreeMap<Block, Vec<Block>> = BTreeMap::new();
        for (block, definition) in body.blocks.entries() {
            let Some(block_order) = cfg.rpo_pos[block] else {
                continue;
            };
            for &successor in &definition.succs {
                let Some(successor_order) = cfg.rpo_pos[successor] else {
                    continue;
                };
                if successor_order.index() <= block_order.index() && cfg.dominates(successor, block)
                {
                    let loop_latches = latches.entry(successor).or_default();
                    if !loop_latches.contains(&block) {
                        loop_latches.push(block);
                    }
                }
            }
        }
        if let Some((header, latches)) = latches.into_iter().find(|(_, latches)| latches.len() != 1)
        {
            return Err(ImportError::Unsupported(alloc::format!(
                "loop header {header:?} has {} latches; VCircuit mode requires one",
                latches.len()
            )));
        }
        Ok(())
    }

    fn compile_block(
        &mut self,
        function: Func,
        body: &FunctionBody,
        block: Block,
        mut state: State,
        plan: &mut StaticControlPlan<(u64, usize, usize, Vec<Option<u64>>)>,
        stack: &mut Vec<Func>,
    ) -> Result<ResultState, ImportError> {
        let block_def = &body.blocks[block];
        let state_key = block_def
            .params
            .iter()
            .map(|(_, value)| self.resolve(body, &state, *value).map(|value| value.known))
            .collect::<Result<Vec<_>, _>>()?;
        plan.enter((state.frame, function.index(), block.index(), state_key))
            .map_err(ImportError::Control)?;

        for record in &block_def.insts {
            let result = match &body.values[record.value] {
                ValueDef::Operator(operator, args, results) => {
                    let args = body.arg_pool[*args]
                        .iter()
                        .map(|value| self.resolve(body, &state, *value))
                        .collect::<Result<Vec<_>, _>>()?;
                    let result_tys = body.type_pool[*results].to_vec();
                    self.lower_operator(operator, &args, &result_tys, &mut state, plan, stack)?
                }
                ValueDef::PickOutput(from, index, ty) => {
                    let source = self.resolve(body, &state, *from)?;
                    let start = output_offset(body, *from, *index as usize)?;
                    let width = width(*ty)?;
                    Some(Value::with_bits(
                        source.bits[start..start + width].to_vec(),
                        *ty,
                        source.known.map(|value| value >> start),
                    ))
                }
                ValueDef::Alias(source) => Some(self.resolve(body, &state, *source)?),
                _ => None,
            };
            if let Some(value) = result {
                state.values.insert(record.value, value);
            }
        }

        match &block_def.terminator.terminator {
            Terminator::Br { target } => {
                let next = self.branch_state(body, state, target.block, &target.args)?;
                self.compile_block(function, body, target.block, next, plan, stack)
            }
            Terminator::CondBr {
                cond,
                if_true,
                if_false,
            } => {
                let condition = self.resolve(body, &state, *cond)?;
                let bit = self.truth_bit(&condition);
                if let Some(value) = condition.known {
                    let target = if value == 0 { if_false } else { if_true };
                    let next = self.branch_state(body, state, target.block, &target.args)?;
                    return self.compile_block(function, body, target.block, next, plan, stack);
                }

                let (then_guard, else_guard) = branch_guards(&mut self.emitter, state.active, bit)
                    .expect("direct VCircuit emission is infallible");
                let mut then_state =
                    self.branch_state(body, state.clone(), if_true.block, &if_true.args)?;
                then_state.active = then_guard;
                let mut else_state =
                    self.branch_state(body, state, if_false.block, &if_false.args)?;
                else_state.active = else_guard;
                let mut then_plan = plan.clone();
                let mut else_plan = plan.clone();
                let then_result = self.compile_block(
                    function,
                    body,
                    if_true.block,
                    then_state,
                    &mut then_plan,
                    stack,
                )?;
                let else_result = self.compile_block(
                    function,
                    body,
                    if_false.block,
                    else_state,
                    &mut else_plan,
                    stack,
                )?;
                self.merge_result(bit, then_result, else_result)
            }
            Terminator::Return { values } => Ok(ResultState {
                outputs: values
                    .iter()
                    .map(|value| self.resolve(body, &state, *value))
                    .collect::<Result<_, _>>()?,
                globals: state.globals,
            }),
            Terminator::ReturnCall { func, args } => {
                let args = args
                    .iter()
                    .map(|value| self.resolve(body, &state, *value))
                    .collect::<Result<Vec<_>, _>>()?;
                self.compile_function(*func, args, state.globals, state.active, plan, stack)
            }
            Terminator::Unreachable | Terminator::UB | Terminator::None => Ok(ResultState {
                outputs: body
                    .rets
                    .iter()
                    .map(|ty| self.constant(*ty, 0))
                    .collect::<Result<_, _>>()?,
                globals: state.globals,
            }),
            other => Err(ImportError::Unsupported(alloc::format!(
                "terminator {other:?}"
            ))),
        }
    }

    fn branch_state(
        &self,
        body: &FunctionBody,
        mut state: State,
        target: Block,
        args: &[WValue],
    ) -> Result<State, ImportError> {
        let params = &body.blocks[target].params;
        if params.len() != args.len() {
            return Err(ImportError::Unsupported(
                "branch argument arity mismatch".to_string(),
            ));
        }
        let values = args
            .iter()
            .map(|value| self.resolve(body, &state, *value))
            .collect::<Result<Vec<_>, _>>()?;
        for ((_, parameter), value) in params.iter().zip(values) {
            state.values.insert(*parameter, value);
        }
        Ok(state)
    }

    fn merge_result(
        &mut self,
        condition: IRVarId,
        then_result: ResultState,
        else_result: ResultState,
    ) -> Result<ResultState, ImportError> {
        if then_result.outputs.len() != else_result.outputs.len()
            || then_result.globals.len() != else_result.globals.len()
        {
            return Err(ImportError::Unsupported(
                "branch result arity mismatch".to_string(),
            ));
        }
        let outputs = then_result
            .outputs
            .into_iter()
            .zip(else_result.outputs)
            .map(|(then_value, else_value)| self.select(condition, then_value, else_value))
            .collect();
        let globals = then_result
            .globals
            .into_iter()
            .zip(else_result.globals)
            .map(|(then_value, else_value)| self.select(condition, then_value, else_value))
            .collect();
        Ok(ResultState { outputs, globals })
    }

    fn resolve(
        &self,
        body: &FunctionBody,
        state: &State,
        mut value: WValue,
    ) -> Result<Value, ImportError> {
        for _ in 0..10_000 {
            if let Some(found) = state.values.get(&value) {
                return Ok(found.clone());
            }
            match &body.values[value] {
                ValueDef::Alias(next) => value = *next,
                _ => break,
            }
        }
        Err(ImportError::Unsupported(alloc::format!(
            "undefined WAFFLE value {value:?}"
        )))
    }

    fn constant(&mut self, ty: WType, value: u64) -> Result<Value, ImportError> {
        let width = width(ty)?;
        let bits = (0..width)
            .map(|bit| self.emitter.bc_const((value >> bit) & 1 != 0))
            .collect();
        Ok(Value::with_bits(bits, ty, Some(value)))
    }

    fn binary(&mut self, op: &Operator, left: Value, right: Value) -> Result<Value, ImportError> {
        let width = left.width();
        if width != right.width() {
            return Err(ImportError::Unsupported(
                "integer width mismatch".to_string(),
            ));
        }
        let known = match (left.known, right.known) {
            (Some(known_left), Some(known_right)) => match op {
                Operator::I32Add | Operator::I64Add => Some(known_left.wrapping_add(known_right)),
                Operator::I32Sub | Operator::I64Sub => Some(known_left.wrapping_sub(known_right)),
                Operator::I32Mul | Operator::I64Mul => Some(known_left.wrapping_mul(known_right)),
                Operator::I32And | Operator::I64And => Some(known_left & known_right),
                Operator::I32Or | Operator::I64Or => Some(known_left | known_right),
                Operator::I32Xor | Operator::I64Xor => Some(known_left ^ known_right),
                Operator::I32Shl | Operator::I64Shl => {
                    Some(known_left.wrapping_shl(known_right as u32))
                }
                Operator::I32ShrU | Operator::I64ShrU => {
                    Some(known_left.wrapping_shr(known_right as u32))
                }
                Operator::I32ShrS | Operator::I64ShrS => {
                    Some(signed_shr(known_left, known_right, width))
                }
                _ => None,
            },
            _ => None,
        };
        Ok(Value::with_bits(
            self.binary_bits(op, &left.bits, &right.bits)?,
            left.ty,
            known,
        ))
    }

    fn binary_bits(
        &mut self,
        op: &Operator,
        left: &[IRVarId],
        right: &[IRVarId],
    ) -> Result<Vec<IRVarId>, ImportError> {
        Ok(match op {
            Operator::I32Add | Operator::I64Add => bc_add(&mut self.emitter, left, right, false),
            Operator::I32Sub | Operator::I64Sub => bc_sub(&mut self.emitter, left, right),
            Operator::I32Mul | Operator::I64Mul => bc_mul(&mut self.emitter, left, right),
            Operator::I32DivU | Operator::I64DivU => bc_udiv(&mut self.emitter, left, right),
            Operator::I32DivS | Operator::I64DivS => bc_sdiv(&mut self.emitter, left, right),
            Operator::I32RemU | Operator::I64RemU => bc_urem(&mut self.emitter, left, right),
            Operator::I32RemS | Operator::I64RemS => bc_srem(&mut self.emitter, left, right),
            Operator::I32And | Operator::I64And => left
                .iter()
                .zip(right)
                .map(|(a, b)| self.emitter.bc_and(*a, *b))
                .collect(),
            Operator::I32Or | Operator::I64Or => bc_or_vec(&mut self.emitter, left, right),
            Operator::I32Xor | Operator::I64Xor => bc_xor_vec(&mut self.emitter, left, right),
            Operator::I32Shl | Operator::I64Shl => {
                volar_lir::circuits::bc_shl(&mut self.emitter, left, right)
            }
            Operator::I32ShrU | Operator::I64ShrU => bc_lshr(&mut self.emitter, left, right),
            Operator::I32ShrS | Operator::I64ShrS => bc_ashr(&mut self.emitter, left, right),
            other => {
                return Err(ImportError::Unsupported(alloc::format!(
                    "operator {other:?}"
                )));
            }
        })
    }

    fn compare(
        &mut self,
        predicate: IcmpPred,
        left: Value,
        right: Value,
        result_ty: WType,
    ) -> Value {
        let input_width = left.width();
        let bit = match predicate {
            IcmpPred::Eq => bc_eq(&mut self.emitter, &left.bits, &right.bits),
            IcmpPred::Ne => bc_ne(&mut self.emitter, &left.bits, &right.bits),
            IcmpPred::Ult => bc_ult(&mut self.emitter, &left.bits, &right.bits),
            IcmpPred::Ule => bc_ule(&mut self.emitter, &left.bits, &right.bits),
            IcmpPred::Ugt => bc_ult(&mut self.emitter, &right.bits, &left.bits),
            IcmpPred::Uge => bc_ule(&mut self.emitter, &right.bits, &left.bits),
            IcmpPred::Slt => bc_slt(&mut self.emitter, &left.bits, &right.bits),
            IcmpPred::Sle => bc_sle(&mut self.emitter, &left.bits, &right.bits),
            IcmpPred::Sgt => bc_slt(&mut self.emitter, &right.bits, &left.bits),
            IcmpPred::Sge => bc_sle(&mut self.emitter, &right.bits, &left.bits),
        };
        let known = match (left.known, right.known) {
            (Some(known_left), Some(known_right)) => Some(match predicate {
                IcmpPred::Eq => known_left == known_right,
                IcmpPred::Ne => known_left != known_right,
                IcmpPred::Ult => known_left < known_right,
                IcmpPred::Ule => known_left <= known_right,
                IcmpPred::Ugt => known_left > known_right,
                IcmpPred::Uge => known_left >= known_right,
                IcmpPred::Slt => {
                    as_signed(known_left, input_width) < as_signed(known_right, input_width)
                }
                IcmpPred::Sle => {
                    as_signed(known_left, input_width) <= as_signed(known_right, input_width)
                }
                IcmpPred::Sgt => {
                    as_signed(known_left, input_width) > as_signed(known_right, input_width)
                }
                IcmpPred::Sge => {
                    as_signed(known_left, input_width) >= as_signed(known_right, input_width)
                }
            } as u64),
            _ => None,
        };
        let mut bits = vec![bit];
        while bits.len() < width(result_ty).expect("WASM compare result is integer") {
            bits.push(self.emitter.bc_const(false));
        }
        Value::with_bits(bits, result_ty, known)
    }

    fn select(&mut self, condition: IRVarId, then_value: Value, else_value: Value) -> Value {
        let known = match (then_value.known, else_value.known) {
            (Some(then_value), Some(else_value)) if then_value == else_value => Some(then_value),
            _ => None,
        };
        let bits = then_value
            .bits
            .iter()
            .zip(&else_value.bits)
            .map(|(then_bit, else_bit)| self.emitter.bc_select(condition, *then_bit, *else_bit))
            .collect();
        Value::with_bits(bits, then_value.ty, known)
    }

    fn truth_bit(&mut self, value: &Value) -> IRVarId {
        value
            .bits
            .iter()
            .copied()
            .reduce(|left, right| self.emitter.bc_or(left, right))
            .unwrap_or_else(|| self.emitter.bc_const(false))
    }

    #[allow(clippy::too_many_arguments)]
    fn lower_operator(
        &mut self,
        operator: &Operator,
        args: &[Value],
        result_tys: &[WType],
        state: &mut State,
        plan: &mut StaticControlPlan<(u64, usize, usize, Vec<Option<u64>>)>,
        stack: &mut Vec<Func>,
    ) -> Result<Option<Value>, ImportError> {
        let arg = |index: usize| {
            args.get(index)
                .cloned()
                .ok_or_else(|| ImportError::Unsupported("missing operator argument".to_string()))
        };
        let simple_binary = matches!(
            operator,
            Operator::I32Add
                | Operator::I64Add
                | Operator::I32Sub
                | Operator::I64Sub
                | Operator::I32Mul
                | Operator::I64Mul
                | Operator::I32DivU
                | Operator::I64DivU
                | Operator::I32DivS
                | Operator::I64DivS
                | Operator::I32RemU
                | Operator::I64RemU
                | Operator::I32RemS
                | Operator::I64RemS
                | Operator::I32And
                | Operator::I64And
                | Operator::I32Or
                | Operator::I64Or
                | Operator::I32Xor
                | Operator::I64Xor
                | Operator::I32Shl
                | Operator::I64Shl
                | Operator::I32ShrU
                | Operator::I64ShrU
                | Operator::I32ShrS
                | Operator::I64ShrS
        );
        if simple_binary {
            return self.binary(operator, arg(0)?, arg(1)?).map(Some);
        }

        let value = match operator {
            Operator::I32Const { value } => self.constant(WType::I32, *value as u32 as u64)?,
            Operator::I64Const { value } => self.constant(WType::I64, *value as u64)?,
            Operator::I32Clz | Operator::I64Clz => {
                let input = arg(0)?;
                let input_width = input.width() as u32;
                Value::with_bits(
                    bc_clz(&mut self.emitter, &input.bits),
                    input.ty,
                    input
                        .known
                        .map(|value| u64::from(value.leading_zeros() - (u64::BITS - input_width))),
                )
            }
            Operator::I32Ctz | Operator::I64Ctz => {
                let input = arg(0)?;
                let input_width = input.width() as u32;
                Value::with_bits(
                    bc_ctz(&mut self.emitter, &input.bits),
                    input.ty,
                    input
                        .known
                        .map(|value| u64::from(value.trailing_zeros().min(input_width))),
                )
            }
            Operator::I32Popcnt | Operator::I64Popcnt => {
                let input = arg(0)?;
                Value::with_bits(
                    bc_popcnt(&mut self.emitter, &input.bits),
                    input.ty,
                    input.known.map(|v| v.count_ones() as u64),
                )
            }
            Operator::I32Rotl | Operator::I64Rotl => {
                let input = arg(0)?;
                let shift = arg(1)?;
                Value::with_bits(
                    bc_rotl(&mut self.emitter, &input.bits, &shift.bits),
                    input.ty,
                    None,
                )
            }
            Operator::I32Rotr | Operator::I64Rotr => {
                let input = arg(0)?;
                let shift = arg(1)?;
                Value::with_bits(
                    bc_rotr(&mut self.emitter, &input.bits, &shift.bits),
                    input.ty,
                    None,
                )
            }
            Operator::I32Eqz => {
                let zero = self.constant(WType::I32, 0)?;
                self.compare(IcmpPred::Eq, arg(0)?, zero, WType::I32)
            }
            Operator::I64Eqz => {
                let zero = self.constant(WType::I64, 0)?;
                self.compare(IcmpPred::Eq, arg(0)?, zero, WType::I64)
            }
            Operator::I32Eq | Operator::I64Eq => {
                self.compare(IcmpPred::Eq, arg(0)?, arg(1)?, result_tys[0])
            }
            Operator::I32Ne | Operator::I64Ne => {
                self.compare(IcmpPred::Ne, arg(0)?, arg(1)?, result_tys[0])
            }
            Operator::I32LtU | Operator::I64LtU => {
                self.compare(IcmpPred::Ult, arg(0)?, arg(1)?, result_tys[0])
            }
            Operator::I32LeU | Operator::I64LeU => {
                self.compare(IcmpPred::Ule, arg(0)?, arg(1)?, result_tys[0])
            }
            Operator::I32GtU | Operator::I64GtU => {
                self.compare(IcmpPred::Ugt, arg(0)?, arg(1)?, result_tys[0])
            }
            Operator::I32GeU | Operator::I64GeU => {
                self.compare(IcmpPred::Uge, arg(0)?, arg(1)?, result_tys[0])
            }
            Operator::I32LtS | Operator::I64LtS => {
                self.compare(IcmpPred::Slt, arg(0)?, arg(1)?, result_tys[0])
            }
            Operator::I32LeS | Operator::I64LeS => {
                self.compare(IcmpPred::Sle, arg(0)?, arg(1)?, result_tys[0])
            }
            Operator::I32GtS | Operator::I64GtS => {
                self.compare(IcmpPred::Sgt, arg(0)?, arg(1)?, result_tys[0])
            }
            Operator::I32GeS | Operator::I64GeS => {
                self.compare(IcmpPred::Sge, arg(0)?, arg(1)?, result_tys[0])
            }
            Operator::I32WrapI64 => self.truncate(arg(0)?, WType::I32),
            Operator::I64ExtendI32U => self.extend(arg(0)?, WType::I64, false),
            Operator::I64ExtendI32S => self.extend(arg(0)?, WType::I64, true),
            Operator::I32Extend8S => self.extend(
                self.truncate_to_bits(arg(0)?, 8, WType::I32),
                WType::I32,
                true,
            ),
            Operator::I32Extend16S => self.extend(
                self.truncate_to_bits(arg(0)?, 16, WType::I32),
                WType::I32,
                true,
            ),
            Operator::I64Extend8S => self.extend(
                self.truncate_to_bits(arg(0)?, 8, WType::I64),
                WType::I64,
                true,
            ),
            Operator::I64Extend16S => self.extend(
                self.truncate_to_bits(arg(0)?, 16, WType::I64),
                WType::I64,
                true,
            ),
            Operator::I64Extend32S => self.extend(
                self.truncate_to_bits(arg(0)?, 32, WType::I64),
                WType::I64,
                true,
            ),
            Operator::Select | Operator::TypedSelect { .. } => {
                let then_value = arg(0)?;
                let else_value = arg(1)?;
                let condition = self.truth_bit(&arg(2)?);
                self.select(condition, then_value, else_value)
            }
            Operator::GlobalGet { global_index } => {
                let global = &self.wasm.globals[*global_index];
                if global.mutable {
                    let index = self.mutable_global_index(*global_index)?;
                    return Ok(Some(state.globals[index].clone()));
                }
                self.constant(global.ty, global.value.unwrap_or(0) as u64)?
            }
            Operator::GlobalSet { global_index } => {
                let global = &self.wasm.globals[*global_index];
                if global.mutable {
                    let index = self.mutable_global_index(*global_index)?;
                    state.globals[index] = arg(0)?;
                }
                return Ok(None);
            }
            Operator::Call { function_index } => {
                if matches!(&self.wasm.funcs[*function_index], FuncDecl::Import(..)) {
                    return Err(ImportError::ImportedFunction(
                        self.wasm.funcs[*function_index].name().to_string(),
                    ));
                }
                let result = self.compile_function(
                    *function_index,
                    args.to_vec(),
                    state.globals.clone(),
                    state.active,
                    plan,
                    stack,
                )?;
                state.globals = result.globals;
                if result.outputs.is_empty() {
                    return Ok(None);
                }
                let ty = result.outputs[0].ty;
                let known = (result.outputs.len() == 1)
                    .then(|| result.outputs[0].known)
                    .flatten();
                Value::with_bits(
                    result
                        .outputs
                        .into_iter()
                        .flat_map(|value| value.bits)
                        .collect(),
                    ty,
                    known,
                )
            }
            Operator::I32Load { memory } => self.load(memory, arg(0)?, 4, WType::I32, false)?,
            Operator::I64Load { memory } => self.load(memory, arg(0)?, 8, WType::I64, false)?,
            Operator::I32Load8U { memory } => self.load(memory, arg(0)?, 1, WType::I32, false)?,
            Operator::I32Load8S { memory } => self.load(memory, arg(0)?, 1, WType::I32, true)?,
            Operator::I32Load16U { memory } => self.load(memory, arg(0)?, 2, WType::I32, false)?,
            Operator::I32Load16S { memory } => self.load(memory, arg(0)?, 2, WType::I32, true)?,
            Operator::I64Load8U { memory } => self.load(memory, arg(0)?, 1, WType::I64, false)?,
            Operator::I64Load8S { memory } => self.load(memory, arg(0)?, 1, WType::I64, true)?,
            Operator::I64Load16U { memory } => self.load(memory, arg(0)?, 2, WType::I64, false)?,
            Operator::I64Load16S { memory } => self.load(memory, arg(0)?, 2, WType::I64, true)?,
            Operator::I64Load32U { memory } => self.load(memory, arg(0)?, 4, WType::I64, false)?,
            Operator::I64Load32S { memory } => self.load(memory, arg(0)?, 4, WType::I64, true)?,
            Operator::I32Store { memory } => {
                self.store(memory, &arg(0)?, &arg(1)?, 4, state.active)?;
                return Ok(None);
            }
            Operator::I64Store { memory } => {
                self.store(memory, &arg(0)?, &arg(1)?, 8, state.active)?;
                return Ok(None);
            }
            Operator::I32Store8 { memory } | Operator::I64Store8 { memory } => {
                self.store(memory, &arg(0)?, &arg(1)?, 1, state.active)?;
                return Ok(None);
            }
            Operator::I32Store16 { memory } | Operator::I64Store16 { memory } => {
                self.store(memory, &arg(0)?, &arg(1)?, 2, state.active)?;
                return Ok(None);
            }
            Operator::I64Store32 { memory } => {
                self.store(memory, &arg(0)?, &arg(1)?, 4, state.active)?;
                return Ok(None);
            }
            Operator::MemorySize { mem } => {
                let memory = &self.wasm.memories[*mem];
                if memory.memory64 {
                    return Err(ImportError::Unsupported("memory64".to_string()));
                }
                self.constant(WType::I32, memory.initial_pages as u64)?
            }
            Operator::MemoryGrow { .. } => {
                return Err(ImportError::Unsupported(
                    "memory.grow changes circuit storage shape".to_string(),
                ));
            }
            Operator::Nop => return Ok(None),
            other => {
                return Err(ImportError::Unsupported(alloc::format!(
                    "operator {other:?}"
                )));
            }
        };
        Ok(Some(value))
    }

    fn truncate(&self, value: Value, ty: WType) -> Value {
        self.truncate_to_bits(value, width(ty).expect("integer type"), ty)
    }

    fn truncate_to_bits(&self, value: Value, bits: usize, ty: WType) -> Value {
        Value::with_bits(value.bits[..bits].to_vec(), ty, value.known)
    }

    fn extend(&mut self, value: Value, ty: WType, signed: bool) -> Value {
        let target = width(ty).expect("integer type");
        let extension = if signed {
            *value.bits.last().expect("non-empty integer")
        } else {
            self.emitter.bc_const(false)
        };
        let mut bits = value.bits;
        bits.resize(target, extension);
        Value::with_bits(bits, ty, value.known)
    }

    fn load(
        &mut self,
        memory: &MemoryArg,
        base: Value,
        bytes: usize,
        result_ty: WType,
        signed: bool,
    ) -> Result<Value, ImportError> {
        let address = self.effective_address(base, memory.offset)?;
        let byte_ty = self.emitter.byte_type();
        let mut bits = Vec::with_capacity(bytes * 8);
        for offset in 0..bytes {
            let address = if offset == 0 {
                address.clone()
            } else {
                self.effective_address(address.clone(), offset as u64)?
            };
            let address = self.address_var(&address)?;
            let read = self.emitter.emit(IRStmt::StorageRead {
                storage: StorageId::memory(memory.memory.index() as u32),
                ty: byte_ty,
                addr: address,
            });
            for bit in 0..8u8 {
                bits.push(self.emitter.emit(IRStmt::Shuffle {
                    result_bits: vec![(bit, read)],
                    ty: self.emitter.bit,
                }));
            }
        }
        let loaded = Value::with_bits(bits, result_ty, None);
        Ok(if loaded.width() == width(result_ty)? {
            loaded
        } else {
            self.extend_to(loaded, result_ty, signed)
        })
    }

    fn extend_to(&mut self, value: Value, ty: WType, signed: bool) -> Value {
        let target = width(ty).expect("integer type");
        let extension = if signed {
            *value.bits.last().expect("non-empty integer")
        } else {
            self.emitter.bc_const(false)
        };
        let mut bits = value.bits;
        bits.resize(target, extension);
        Value::with_bits(bits, ty, value.known)
    }

    fn store(
        &mut self,
        memory: &MemoryArg,
        base: &Value,
        value: &Value,
        bytes: usize,
        active: IRVarId,
    ) -> Result<(), ImportError> {
        let address = self.effective_address(base.clone(), memory.offset)?;
        let byte_ty = self.emitter.byte_type();
        for offset in 0..bytes {
            let address = if offset == 0 {
                address.clone()
            } else {
                self.effective_address(address.clone(), offset as u64)?
            };
            let address = self.address_var(&address)?;
            let source = self
                .emitter
                .merge(&value.bits[offset * 8..offset * 8 + 8], byte_ty);
            let storage = StorageId::memory(memory.memory.index() as u32);
            let previous = self.emitter.emit(IRStmt::StorageRead {
                storage,
                ty: byte_ty,
                addr: address,
            });
            let selected = guarded_value(&mut self.emitter, active, source, previous)
                .expect("direct VCircuit emission is infallible");
            self.emitter.emit(IRStmt::StorageWrite {
                storage,
                src: selected,
                ty: byte_ty,
                addr: address,
            });
        }
        Ok(())
    }

    fn effective_address(&mut self, base: Value, offset: u64) -> Result<Value, ImportError> {
        if offset == 0 {
            return Ok(base);
        }
        let added = self.constant(WType::I32, offset)?;
        self.binary(&Operator::I32Add, base, added)
    }

    fn address_var(&mut self, value: &Value) -> Result<IRVarId, ImportError> {
        let bits = self.options.memory_address_bits.unwrap_or(32);
        if bits > value.bits.len() {
            return Err(ImportError::Unsupported(
                "memory address width exceeds i32".to_string(),
            ));
        }
        let address_ty = self.emitter.address_type(bits);
        Ok(self.emitter.merge(&value.bits[..bits], address_ty))
    }

    fn mutable_global_index(
        &self,
        wanted: portal_pc_waffle_ir::Global,
    ) -> Result<usize, ImportError> {
        self.wasm
            .globals
            .entries()
            .filter(|(_, global)| global.mutable)
            .position(|(global, _)| global == wanted)
            .ok_or_else(|| ImportError::Unsupported("unknown mutable global".to_string()))
    }

    fn pre_init(&mut self) -> Vec<PreInitSegment> {
        let byte = self.emitter.byte_type();
        self.wasm
            .memories
            .entries()
            .flat_map(|(memory, data)| {
                data.segments.iter().map(move |segment| PreInitSegment {
                    storage: StorageId::memory(memory.index() as u32),
                    ty: byte,
                    offset: segment.offset,
                    data: segment
                        .data
                        .iter()
                        .map(|value| Constant {
                            hi: 0,
                            lo: *value as u128,
                        })
                        .collect(),
                })
            })
            .collect()
    }

    fn memory_layouts(&self) -> Vec<WasmMemoryLayout> {
        let address_bits = self.options.memory_address_bits.unwrap_or(32);
        self.wasm
            .memories
            .entries()
            .map(|(memory, _)| WasmMemoryLayout {
                memory: memory.index() as u32,
                address_bits,
            })
            .collect()
    }
}

fn width(ty: WType) -> Result<usize, ImportError> {
    match ty {
        WType::I32 => Ok(32),
        WType::I64 => Ok(64),
        other => Err(ImportError::Unsupported(alloc::format!("type {other:?}"))),
    }
}

fn output_offset(body: &FunctionBody, value: WValue, index: usize) -> Result<usize, ImportError> {
    match &body.values[value] {
        ValueDef::Operator(_, _, results) => body.type_pool[*results]
            .iter()
            .take(index)
            .try_fold(0usize, |sum, ty| Ok(sum + width(*ty)?)),
        _ => Err(ImportError::Unsupported(
            "PickOutput source is not an operator".to_string(),
        )),
    }
}

fn mask(value: u64, width: usize) -> u64 {
    if width >= u64::BITS as usize {
        value
    } else {
        value & ((1u64 << width) - 1)
    }
}

fn as_signed(value: u64, width: usize) -> i64 {
    if width == 64 {
        value as i64
    } else {
        ((value << (64 - width)) as i64) >> (64 - width)
    }
}

fn signed_shr(value: u64, shift: u64, width: usize) -> u64 {
    mask(
        (as_signed(value, width) >> shift.min((width.saturating_sub(1)) as u64)) as u64,
        width,
    )
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;

    #[test]
    fn unknown_entry_bits_become_vcircuit_params() {
        let bytes = wat::parse_str("(module (func (export \"add\") (param i32 i32) (result i32) local.get 0 local.get 1 i32.add))").unwrap();
        let artifact =
            import_wasm_bytes(&bytes, "add", WasmCircuitImportOptions::default()).unwrap();
        assert_eq!(artifact.circuit.params.len(), 64);
        assert_eq!(artifact.circuit.outputs.len(), 32);
        assert!(!artifact.circuit.stmts.is_empty());
    }

    #[test]
    fn symbolic_if_is_lowered_without_a_terminator() {
        let bytes = wat::parse_str("(module (func (export \"choose\") (param i32 i32 i32) (result i32) local.get 0 if (result i32) local.get 1 else local.get 2 end))").unwrap();
        let module = WModule::from_wasm_bytes(&bytes, &FrontendOptions::default()).unwrap();
        let artifact =
            import_module(&module, "choose", WasmCircuitImportOptions::default()).unwrap();
        assert_eq!(artifact.circuit.outputs.len(), 32);
        assert!(!artifact.circuit.stmts.is_empty());
    }

    #[test]
    fn direct_calls_are_inlined_into_one_circuit() {
        let bytes = wat::parse_str(
            "(module
                (func $increment (param i32) (result i32)
                    local.get 0 i32.const 1 i32.add)
                (func (export \"twice\") (param i32) (result i32)
                    local.get 0 call $increment call $increment))",
        )
        .unwrap();
        let module = WModule::from_wasm_bytes(&bytes, &FrontendOptions::default()).unwrap();
        let artifact =
            import_module(&module, "twice", WasmCircuitImportOptions::default()).unwrap();
        assert_eq!(artifact.circuit.params.len(), 32);
        assert_eq!(artifact.circuit.outputs.len(), 32);
    }

    #[test]
    fn statically_finite_loop_is_unrolled() {
        let bytes = wat::parse_str(
            "(module
                (func (export \"count_to_four\") (result i32)
                    (local i32)
                    i32.const 0
                    local.set 0
                    block
                        loop
                            local.get 0
                            i32.const 1
                            i32.add
                            local.tee 0
                            i32.const 4
                            i32.lt_u
                            br_if 0
                        end
                    end
                    local.get 0))",
        )
        .unwrap();
        let module = WModule::from_wasm_bytes(&bytes, &FrontendOptions::default()).unwrap();
        let artifact = import_module(
            &module,
            "count_to_four",
            WasmCircuitImportOptions::default(),
        )
        .unwrap();
        assert_eq!(artifact.circuit.outputs.len(), 32);
        assert!(artifact.circuit.stmts.len() > 100);
    }

    #[test]
    fn symbolic_loop_exit_is_rejected_instead_of_bounded() {
        let bytes = wat::parse_str(
            "(module
                (func (export \"loop\") (param i32) (result i32)
                    block
                        loop
                            local.get 0
                            br_if 0
                        end
                    end
                    i32.const 0))",
        )
        .unwrap();
        let module = WModule::from_wasm_bytes(&bytes, &FrontendOptions::default()).unwrap();
        assert_eq!(
            import_module(&module, "loop", WasmCircuitImportOptions::default()),
            Err(ImportError::Control(ControlError::NonFiniteControl)),
        );
    }

    #[test]
    fn data_segments_are_preserved_as_artifact_metadata() {
        let bytes = wat::parse_str(
            "(module
                (memory 1)
                (data (i32.const 3) \"\\01\\02\")
                (func (export \"load\") (result i32)
                    i32.const 3
                    i32.load8_u))",
        )
        .unwrap();
        let module = WModule::from_wasm_bytes(&bytes, &FrontendOptions::default()).unwrap();
        let artifact = import_module(&module, "load", WasmCircuitImportOptions::default()).unwrap();
        assert_eq!(artifact.memories.len(), 1);
        assert_eq!(artifact.pre_init.len(), 1);
        assert_eq!(artifact.pre_init[0].offset, 3);
        assert_eq!(artifact.pre_init[0].data.len(), 2);
    }

    #[test]
    fn registry_mode_claims_memory_spaces() {
        let bytes = wat::parse_str(
            "(module
                (memory 1)
                (data (i32.const 3) \"\\01\\02\")
                (func (export \"load\") (result i32)
                    i32.const 3
                    i32.load8_u))",
        )
        .unwrap();
        let module = WModule::from_wasm_bytes(&bytes, &FrontendOptions::default()).unwrap();
        let mut registry = StorageRegistry::<StoragePurpose>::new();
        let artifact = import_module_with_registry(
            &module,
            "load",
            WasmCircuitImportOptions::default(),
            &mut registry,
        )
        .unwrap();
        // The legacy `StorageId::memory(0)` id is preserved, now with a
        // purpose recorded in the module's registry.
        assert_eq!(artifact.pre_init[0].storage, StorageId::memory(0));
        assert_eq!(
            registry.purpose_of(StorageId::memory(0)),
            Some(&StoragePurpose::WasmMemory { index: 0 })
        );
    }

    #[test]
    fn registry_mode_fails_closed_on_memory_collision() {
        let bytes = wat::parse_str(
            "(module
                (memory 1)
                (func (export \"load\") (result i32)
                    i32.const 0
                    i32.load8_u))",
        )
        .unwrap();
        let module = WModule::from_wasm_bytes(&bytes, &FrontendOptions::default()).unwrap();
        let mut registry = StorageRegistry::<StoragePurpose>::new();
        // Another consumer already owns the space `memory(0)` maps to.
        registry
            .claim(
                StorageId::memory(0),
                StoragePurpose::Other("foreign".to_string()),
            )
            .unwrap();
        let err = import_module_with_registry(
            &module,
            "load",
            WasmCircuitImportOptions::default(),
            &mut registry,
        )
        .unwrap_err();
        match err {
            ImportError::Unsupported(msg) => {
                assert!(msg.contains("wasm linear memory storage"), "{msg}")
            }
            other => panic!("expected Unsupported collision error, got {other:?}"),
        }
        // The foreign claim is untouched.
        assert_eq!(
            registry.purpose_of(StorageId::memory(0)),
            Some(&StoragePurpose::Other("foreign".to_string()))
        );
    }
}
