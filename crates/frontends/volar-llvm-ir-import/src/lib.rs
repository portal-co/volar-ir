//! Execution-mode LLVM IR import into Volar IR.
//!
//! Reuses `volar-llvm-import-core`'s bounded concrete/symbolic interpreter
//! unmodified: direct calls are inlined by concretely executing the callee
//! (recursion, even self-reentry, is a hard error — never bounded-unrolled),
//! and every LLVM basic block is walked through the interpreter's own Rust
//! control flow rather than translated into Volar IR blocks/jumps. The
//! result is a single flat `IRBlock` of `Poly`/`Const` statements terminated
//! by `Jmp { dest: Return }` — deliberately the "inline everything"
//! counterpart to `volar-llvm-vaffle-import`'s call-preserving structural
//! translation.
//!
//! # Scope (v1)
//!
//! - Entry function parameters must be integer-typed; each becomes a fully
//!   symbolic value (its bits become free `IRBlock` parameters).
//! - Only a scalar integer (or void) return is supported.
//! - No pointer arguments, no globals, no host calls.
//! - Control flow (branch conditions, memory addresses) must be concrete,
//!   per `volar-llvm-import-core`'s own execution model — loops must have a
//!   compile-time-known trip count; genuinely data-dependent branches are a
//!   hard error surfaced by the interpreter itself.
//! - `blockaddress`/`indirectbr` are not supported (mirrors the interpreter,
//!   which has no opcode handling for either).

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::fmt;

use inkwell::context::Context;
use inkwell::memory_buffer::MemoryBuffer;
use inkwell::module::Module as LlvmModule;
use inkwell::types::BasicTypeEnum;

use volar_ir::ir::{
    Constant, IRBlock, IRBlockTargetId, IRBlocks, IRBranchTarget, IRStmt, IRTerminator, IRTypeId,
    IRTypes, IRVarId, StorageId,
};
use volar_ir_common::{Node, PolyCoeffs};
use volar_lir::circuits::BitCircuitBuilder;

use volar_llvm_import_core::{
    ArgumentBinding, ExecutionBackend, ExecutionResult, Export, FrontendError, HostCallRegistry,
    LowerRequest, ScalarBinding, execute_module,
};
pub use volar_llvm_import_core::{LoweringLimits, ModuleInput};

/// A diagnostic from [`import`]/[`import_module`].
#[derive(Debug)]
pub enum ImportError {
    /// A shape the interpreter itself rejected (unsupported opcode, non-
    /// concrete branch, call/instruction limit exceeded, recursion, etc).
    Frontend(FrontendError),
    /// A shape this crate rejects before handing off to the interpreter
    /// (non-integer parameter/return, entry not found, parse failure).
    Unsupported(String),
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::Frontend(e) => write!(f, "{e}"),
            ImportError::Unsupported(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for ImportError {}

impl From<FrontendError> for ImportError {
    fn from(e: FrontendError) -> Self {
        ImportError::Frontend(e)
    }
}

type IResult<T> = Result<T, ImportError>;

/// Parse `input` and import `entry`'s bounded execution into a single-block
/// [`IRBlocks`], returning it alongside the [`IRTypes`] table it references.
pub fn import(
    context: &Context,
    input: ModuleInput<'_>,
    entry: &str,
    limits: LoweringLimits,
) -> IResult<(IRBlocks, IRTypes)> {
    let module = match input {
        ModuleInput::Assembly(source) => context
            .create_module_from_ir(MemoryBuffer::create_from_memory_range_copy(
                source.as_bytes(),
                "import.ll",
            ))
            .map_err(|e| ImportError::Unsupported(format!("LLVM parse error: {e}")))?,
        ModuleInput::Bitcode(bytes) => LlvmModule::parse_bitcode_from_buffer(
            &MemoryBuffer::create_from_memory_range_copy(bytes, "import.bc"),
            context,
        )
        .map_err(|e| ImportError::Unsupported(format!("LLVM parse error: {e}")))?,
    };
    import_module(&module, entry, limits)
}

/// Import an already-parsed LLVM module's `entry` function.
pub fn import_module<'ctx>(
    module: &LlvmModule<'ctx>,
    entry: &str,
    limits: LoweringLimits,
) -> IResult<(IRBlocks, IRTypes)> {
    let entry_fn = module.get_function(entry).ok_or_else(|| {
        ImportError::Unsupported(format!("entry function `{entry}` does not exist"))
    })?;

    let mut arguments = Vec::new();
    for param in entry_fn.get_params() {
        int_width(param.get_type()).ok_or_else(|| {
            ImportError::Unsupported("only integer parameters are supported".to_string())
        })?;
        arguments.push(ArgumentBinding::Scalar(ScalarBinding::Symbolic));
    }

    let exports = match entry_fn.get_type().get_return_type() {
        None => vec![],
        Some(ret) => {
            int_width(ret).ok_or_else(|| {
                ImportError::Unsupported(
                    "only an integer (or void) return is supported".to_string(),
                )
            })?;
            vec![Export::Return]
        }
    };

    let host_calls: HostCallRegistry<VolarIrBitSink> = HostCallRegistry::new();
    let request = LowerRequest {
        entry,
        arguments: &arguments,
        globals: &[],
        exports: &exports,
        limits,
        host_calls: &host_calls,
    };

    let mut types = IRTypes::new();
    let bit_tid = types.bit();
    let mut sink = VolarIrBitSink::new(bit_tid);
    let result = execute_module(module, &request, &mut sink)?;
    let block = sink.into_block(result);
    Ok((IRBlocks::new(vec![block]), types))
}

fn int_width(ty: BasicTypeEnum<'_>) -> Option<u32> {
    match ty {
        BasicTypeEnum::IntType(int_ty) => Some(int_ty.get_bit_width()),
        _ => None,
    }
}

// ============================================================================
// The bit-circuit sink
// ============================================================================

/// Emits a flat, single-block sequence of `Poly`/`Const` statements as the
/// interpreter drives it — no jumps, no calls: `execute_module` has already
/// flattened all control flow and inlined every direct call by the time any
/// `ExecutionBackend` method runs.
struct BlockEmitter {
    stmts: Vec<Node<IRStmt, ()>>,
    next_var: u32,
    bit_tid: IRTypeId,
}

impl BlockEmitter {
    fn new(bit_tid: IRTypeId) -> Self {
        BlockEmitter {
            stmts: Vec::new(),
            next_var: 0,
            bit_tid,
        }
    }

    fn emit(&mut self, stmt: IRStmt) -> IRVarId {
        let id = IRVarId(self.next_var);
        self.next_var += 1;
        self.stmts.push(Node::new(stmt, (), None));
        id
    }
}

impl BitCircuitBuilder for BlockEmitter {
    type Bit = IRVarId;

    fn bc_const(&mut self, val: bool) -> IRVarId {
        self.emit(IRStmt::Const(
            Constant {
                hi: 0,
                lo: val as u128,
            },
            self.bit_tid,
        ))
    }

    fn bc_poly(&mut self, coeffs: PolyCoeffs<IRVarId>, constant: u128) -> IRVarId {
        self.emit(IRStmt::Poly {
            ty: self.bit_tid,
            coeffs,
            constant: Constant {
                hi: 0,
                lo: constant,
            },
        })
    }
}

/// [`ExecutionBackend`] adapter: each Boolean operation the interpreter asks
/// for maps directly onto [`BitCircuitBuilder`]'s derived gate ops.
struct VolarIrBitSink {
    emitter: BlockEmitter,
}

impl VolarIrBitSink {
    fn new(bit_tid: IRTypeId) -> Self {
        VolarIrBitSink {
            emitter: BlockEmitter::new(bit_tid),
        }
    }

    /// Convert the flat gate trace plus the interpreter's designated
    /// input/output wires into a real `IRBlock`.
    ///
    /// Every wire in `result.inputs` was emitted as an ordinary `Const`
    /// statement — the interpreter can't distinguish "materialize a literal
    /// LLVM constant" from "materialize a free symbolic input bit" at the
    /// `ExecutionBackend::create` call site, both go through the same
    /// method. This rewrites exactly those designated statements into real
    /// block parameters, drops them from the statement list, and renumbers
    /// every remaining statement and reference around the gap.
    fn into_block(self, result: ExecutionResult<IRVarId>) -> IRBlock {
        let bit_tid = self.emitter.bit_tid;
        let old_stmts = self.emitter.stmts;
        let n_inputs = result.inputs.len();

        let input_set: HashSet<IRVarId> = result.inputs.iter().copied().collect();
        let mut old_to_new: HashMap<IRVarId, IRVarId> = HashMap::with_capacity(old_stmts.len());
        for (i, old) in result.inputs.iter().enumerate() {
            old_to_new.insert(*old, IRVarId(i as u32));
        }
        let mut next_new = n_inputs as u32;
        for old_idx in 0..old_stmts.len() as u32 {
            let old_id = IRVarId(old_idx);
            if input_set.contains(&old_id) {
                continue;
            }
            old_to_new.insert(old_id, IRVarId(next_new));
            next_new += 1;
        }

        let mut new_stmts = Vec::with_capacity(old_stmts.len().saturating_sub(n_inputs));
        for (old_idx, node) in old_stmts.into_iter().enumerate() {
            let old_id = IRVarId(old_idx as u32);
            if input_set.contains(&old_id) {
                continue;
            }
            let remapped = node
                .kind
                .map_var(
                    &mut (),
                    &mut |_ctx, v: IRVarId| -> Result<IRVarId, Infallible> { Ok(old_to_new[&v]) },
                    &mut |_ctx, ty: IRTypeId| -> Result<IRTypeId, Infallible> { Ok(ty) },
                    &mut |_ctx, s: StorageId| -> Result<StorageId, Infallible> { Ok(s) },
                )
                .unwrap();
            new_stmts.push(Node::new(remapped, node.prov, node.side));
        }

        let params = vec![bit_tid; n_inputs];
        let output_vars: Vec<IRVarId> = result.outputs.iter().map(|v| old_to_new[v]).collect();
        let terminator = IRTerminator::Jmp {
            target: IRBranchTarget::new(IRBlockTargetId::Return, output_vars),
        };
        IRBlock {
            params,
            stmts: new_stmts,
            terminator,
        }
    }
}

impl ExecutionBackend for VolarIrBitSink {
    type Wire = IRVarId;
    type Error = Infallible;

    fn describe_error(error: Self::Error) -> String {
        match error {}
    }

    fn create(&mut self, val: bool) -> Result<IRVarId, Infallible> {
        Ok(self.emitter.bc_const(val))
    }

    fn bitand(&mut self, left: IRVarId, right: IRVarId) -> Result<IRVarId, Infallible> {
        Ok(self.emitter.bc_and(left, right))
    }

    fn bitor(&mut self, left: IRVarId, right: IRVarId) -> Result<IRVarId, Infallible> {
        Ok(self.emitter.bc_or(left, right))
    }

    fn bitxor(&mut self, left: IRVarId, right: IRVarId) -> Result<IRVarId, Infallible> {
        Ok(self.emitter.bc_xor(left, right))
    }

    fn mux(
        &mut self,
        cond: IRVarId,
        then: IRVarId,
        r#else: IRVarId,
    ) -> Result<IRVarId, Infallible> {
        Ok(self.emitter.bc_select(cond, then, r#else))
    }
}
