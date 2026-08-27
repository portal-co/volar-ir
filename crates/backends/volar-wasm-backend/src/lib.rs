// @reliability: experimental
// @ai: assisted
//! WASM backend for [`LirTarget`].
//!
//! Emits a complete WASM module (`Vec<u8>`) via [`wasm_encoder`] and
//! [`wax_core::build::InstructionSink`].  SSA values and block parameters are
//! mapped to WASM locals; multi-block control flow uses a `br_table`
//! dispatcher (`locals + br`).
//!
//! # ABI
//!
//! Returns [`LirAbi::DEFAULT`]: aggregates are flattened to scalar locals /
//! parameters.  [`define_struct`](LirTarget::define_struct) records field
//! layouts for flattening.  [`StackAllocExt`] is not supported yet
//! (`stack_alloc_ext` returns `None`).
//!
//! # Arrays
//!
//! Small fixed arrays are represented as flat scalar locals.  Helper methods
//! [`arr_new`](WasmBackend::arr_new) / [`arr_get`](WasmBackend::arr_get) /
//! [`arr_set`](WasmBackend::arr_set) operate on those flat lists.

#![cfg_attr(not(test), no_std)]
extern crate alloc;

use alloc::{borrow::Cow, collections::BTreeMap, string::String, vec, vec::Vec};
use volar_ir_common::Type as NativeType;
use volar_lir::{
    BranchTarget, IcmpPred, LirAbi, LirTarget, LirType, NameConfig, StructDef, StructId,
};
use wasm_encoder::{
    CodeSection, ExportKind, ExportSection, Function, FunctionSection, ImportSection, Instruction,
    Module, TypeSection, ValType,
};
use wax_core::build::InstructionSink;

// ============================================================================
// Handles
// ============================================================================

/// An SSA value: index into the current function's local/value table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WasmValue(pub u32);

/// A basic-block handle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct WasmBlock(pub u32);

// ============================================================================
// Buffered ops (lowered at `end_function`)
// ============================================================================

#[derive(Clone, Debug)]
enum BinOpKind {
    Add,
    Sub,
    Mul,
    Udiv,
    Sdiv,
    And,
    Or,
    Xor,
    Shl,
    Lshr,
    Ashr,
}

#[derive(Clone, Debug)]
enum Op {
    Iconst {
        dests: Vec<u32>,
        ty: LirType,
        val: i64,
    },
    Binop {
        dest: u32,
        kind: BinOpKind,
        lhs: u32,
        rhs: u32,
        is_i64: bool,
    },
    /// Carry/borrow-propagating add/subtract over a packed multi-word value.
    WideAddSub {
        dests: Vec<u32>,
        lhs: Vec<u32>,
        rhs: Vec<u32>,
        subtract: bool,
        is_i64: bool,
        carry: u32,
        aux: u32,
    },
    Not {
        dest: u32,
        val: u32,
        is_i64: bool,
        is_bool: bool,
    },
    Icmp {
        dest: u32,
        pred: IcmpPred,
        lhs: u32,
        rhs: u32,
    },
    Zext {
        dest: u32,
        val: u32,
        dst_ty: LirType,
    },
    Sext {
        dest: u32,
        val: u32,
        dst_ty: LirType,
    },
    Trunc {
        dest: u32,
        val: u32,
        dst_ty: LirType,
    },
    Select {
        dests: Vec<u32>,
        cond: u32,
        then_vals: Vec<u32>,
        else_vals: Vec<u32>,
    },
    /// Copy `src` into local `dest` (used by arr helpers / parallel assign).
    Copy {
        dest: u32,
        src: u32,
    },
    Call {
        /// Destination locals for results (may be empty).
        dests: Vec<u32>,
        func_idx: u32,
        args: Vec<u32>,
    },
    /// A sibling (intra-module) call, resolved by name to a final function
    /// index only in [`WasmBackend::finish`] — unlike [`Op::Call`], the
    /// callee's index isn't known at emission time since it depends on the
    /// final import count and every defined function's position, including
    /// ones not yet `begin_function`'d (forward references / mutual
    /// recursion).
    CallSibling {
        dests: Vec<u32>,
        name: String,
        args: Vec<u32>,
    },
    Jump {
        target: u32,
        args: Vec<u32>,
    },
    Branch {
        cond: u32,
        then_block: u32,
        then_args: Vec<u32>,
        else_block: u32,
        else_args: Vec<u32>,
    },
    /// A multi-way branch: `index` compared against each case key in turn,
    /// falling back to `default_block` — the `LirTarget::switch` terminator.
    /// Lowered as a chain of `if (index == key) {...} else {...}`, since
    /// `br_table` requires a dense, statically-known `0..n` target list
    /// (already used internally for the per-function block dispatcher) and
    /// can't be reused directly for an arbitrary sparse `i64`-keyed switch.
    Table {
        index: u32,
        cases: Vec<(i64, u32, Vec<u32>)>,
        default_block: u32,
        default_args: Vec<u32>,
    },
    Ret {
        vals: Vec<u32>,
    },
}

struct BlockState {
    /// Local indices for this block's parameters, in order.
    param_locals: Vec<u32>,
    ops: Vec<Op>,
    /// True once a terminator has been recorded.
    terminated: bool,
}

struct ValueInfo {
    /// WASM locals holding this value in canonical little-endian ABI order.
    /// A scalar has exactly one local; `_128`/`_256` and lane vectors are
    /// represented by multiple MVP numeric locals rather than SIMD.
    locals: Vec<u32>,
    ty: LirType,
}

struct FunctionState {
    name: String,
    /// Flattened WASM parameter types (scalars only).
    wasm_param_tys: Vec<ValType>,
    /// Flattened WASM result types (scalars only).
    wasm_result_tys: Vec<ValType>,
    values: Vec<ValueInfo>,
    /// Next WASM local index to allocate (params occupy `0..wasm_param_tys.len()`).
    next_local: u32,
    blocks: Vec<BlockState>,
    current_block: Option<u32>,
    /// Local index reserved for the PC dispatcher (`None` until first multi-block use).
    pc_local: Option<u32>,
    /// Scratch locals used by multi-word operations.
    temp_locals: Vec<(u32, ValType)>,
}

impl FunctionState {
    fn alloc_value(&mut self, ty: LirType) -> WasmValue {
        let local_tys = WasmBackend::wasm_value_tys(&ty);
        let locals = (0..local_tys.len())
            .map(|_| {
                let local = self.next_local;
                self.next_local += 1;
                local
            })
            .collect();
        let id = self.values.len() as u32;
        self.values.push(ValueInfo { locals, ty });
        WasmValue(id)
    }

    fn local_of(&self, v: WasmValue) -> u32 {
        let locals = &self.values[v.0 as usize].locals;
        assert_eq!(
            locals.len(),
            1,
            "WasmBackend: scalar operation received a multi-word value"
        );
        locals[0]
    }

    fn locals_of(&self, v: WasmValue) -> &[u32] {
        &self.values[v.0 as usize].locals
    }

    fn ty_of(&self, v: WasmValue) -> &LirType {
        &self.values[v.0 as usize].ty
    }

    fn push_op(&mut self, op: Op) {
        let bid = self.current_block.expect("WasmBackend: no current block");
        let block = &mut self.blocks[bid as usize];
        assert!(
            !block.terminated,
            "WasmBackend: emitting after terminator in block {bid}"
        );
        let is_term = matches!(
            op,
            Op::Jump { .. } | Op::Branch { .. } | Op::Ret { .. } | Op::Table { .. }
        );
        block.ops.push(op);
        if is_term {
            block.terminated = true;
        }
    }

    fn alloc_temp(&mut self, ty: ValType) -> u32 {
        let local = self.next_local;
        self.next_local += 1;
        self.temp_locals.push((local, ty));
        local
    }

    fn ensure_pc_local(&mut self) -> u32 {
        if let Some(pc) = self.pc_local {
            return pc;
        }
        let pc = self.next_local;
        self.next_local += 1;
        self.pc_local = Some(pc);
        pc
    }
}

// ============================================================================
// Completed function record
// ============================================================================

struct CompletedFunction {
    name: String,
    type_idx: u32,
    /// Raw per-function state, not yet lowered to a [`Function`] body.
    ///
    /// Lowering is deferred to [`WasmBackend::finish`] (rather than done
    /// eagerly in `end_function`) because [`Op::CallSibling`] can reference
    /// a function whose own `begin_function`/`end_function` hasn't run yet
    /// (forward references / mutual recursion) — the name→index map for
    /// sibling calls is only fully known once every function is complete.
    state: FunctionState,
}

// ============================================================================
// The backend
// ============================================================================

/// WASM backend implementing [`LirTarget`].
///
/// Drive via `begin_function` / emit / `end_function`, then call [`finish`](Self::finish)
/// to obtain module bytes.
pub struct WasmBackend {
    current: Option<FunctionState>,
    completed: Vec<CompletedFunction>,

    struct_defs: Vec<StructDef>,
    next_struct_id: StructId,

    /// Deduplicated function types: key → type index.
    type_map: BTreeMap<(Vec<ValType>, Vec<ValType>), u32>,
    types: TypeSection,

    /// Import name → function index (imports occupy the low indices).
    import_map: BTreeMap<String, u32>,
    /// Parallel to import order for emission.
    imports: Vec<(String, u32)>, // (name, type_idx)
    next_func_idx: u32,

    pub name_config: NameConfig,
    /// Import module name for `call_extern` (default `"env"`).
    pub import_module: String,
    /// Import name used for `rng` (default `"volar_rng"`).
    pub rng_fn: String,
}

impl Default for WasmBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl WasmBackend {
    pub fn new() -> Self {
        WasmBackend {
            current: None,
            completed: Vec::new(),
            struct_defs: Vec::new(),
            next_struct_id: 0,
            type_map: BTreeMap::new(),
            types: TypeSection::new(),
            import_map: BTreeMap::new(),
            imports: Vec::new(),
            next_func_idx: 0,
            name_config: NameConfig::default(),
            import_module: String::from("env"),
            rng_fn: String::from("volar_rng"),
        }
    }

    pub fn with_name_config(mut self, config: NameConfig) -> Self {
        self.name_config = config;
        self
    }

    pub fn with_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.name_config.prefix = prefix.into();
        self
    }

    /// Set the module from which external primitive symbols are imported.
    /// Together with [`Self::with_name_config`], this selects both sides of
    /// the WASM external ABI without changing the source circuit.
    pub fn with_import_module(mut self, module: impl Into<String>) -> Self {
        self.import_module = module.into();
        self
    }

    /// Consume the backend and return the finished WASM module bytes.
    pub fn finish(self) -> Vec<u8> {
        let mut module = Module::new();

        // Type section.
        module.section(&self.types);

        // Import section (functions from `import_module`).
        if !self.imports.is_empty() {
            let mut imports = ImportSection::new();
            for (name, type_idx) in &self.imports {
                imports.import(
                    &self.import_module,
                    name,
                    wasm_encoder::EntityType::Function(*type_idx),
                );
            }
            module.section(&imports);
        }

        // Function section (type indices for defined functions).
        if !self.completed.is_empty() {
            let mut functions = FunctionSection::new();
            for f in &self.completed {
                functions.function(f.type_idx);
            }
            module.section(&functions);
        }

        // Export section — export every defined function by name.
        if !self.completed.is_empty() {
            let mut exports = ExportSection::new();
            let import_count = self.imports.len() as u32;
            for (i, f) in self.completed.iter().enumerate() {
                exports.export(&f.name, ExportKind::Func, import_count + i as u32);
            }
            module.section(&exports);
        }

        // Code section. Lowered here (not in `end_function`) so that every
        // sibling-call name resolves to a final function index, including
        // forward references to functions completed after the call site.
        if !self.completed.is_empty() {
            let import_count = self.imports.len() as u32;
            let sibling_idx: BTreeMap<String, u32> = self
                .completed
                .iter()
                .enumerate()
                .map(|(i, f)| (f.name.clone(), import_count + i as u32))
                .collect();
            let mut codes = CodeSection::new();
            for f in &self.completed {
                let body = lower_function(&f.state, &sibling_idx);
                codes.function(&body);
            }
            module.section(&codes);
        }

        module.finish()
    }

    // ---- Array helpers (flat scalar representation) -------------------------

    /// Copy `elems` into fresh locals — the "array" is the returned flat list.
    pub fn arr_new(&mut self, elems: &[WasmValue]) -> Vec<WasmValue> {
        elems
            .iter()
            .map(|&e| {
                let ty = self.state().ty_of(e).clone();
                let dest = self.state().alloc_value(ty);
                let src_local = self.state().local_of(e);
                let dest_local = self.state().local_of(dest);
                self.state().push_op(Op::Copy {
                    dest: dest_local,
                    src: src_local,
                });
                dest
            })
            .collect()
    }

    /// Read one element of width `elem_width` at constant `idx`.
    pub fn arr_get(&mut self, arr: &[WasmValue], elem_width: usize, idx: usize) -> Vec<WasmValue> {
        let n = arr.len() / elem_width;
        assert!(idx < n, "arr_get: index {idx} out of bounds (len {n})");
        let start = idx * elem_width;
        self.arr_new(&arr[start..start + elem_width])
    }

    /// Functional update: replace element `idx` (width `elem_width`) with `vals`.
    pub fn arr_set(
        &mut self,
        arr: &[WasmValue],
        elem_width: usize,
        idx: usize,
        vals: &[WasmValue],
    ) -> Vec<WasmValue> {
        assert_eq!(vals.len(), elem_width, "arr_set: vals width mismatch");
        let n = arr.len() / elem_width;
        assert!(idx < n, "arr_set: index {idx} out of bounds (len {n})");
        let mut out = self.arr_new(arr);
        let start = idx * elem_width;
        for (i, &v) in vals.iter().enumerate() {
            let ty = self.state().ty_of(v).clone();
            let dest = self.state().alloc_value(ty);
            let src_local = self.state().local_of(v);
            let dest_local = self.state().local_of(dest);
            self.state().push_op(Op::Copy {
                dest: dest_local,
                src: src_local,
            });
            out[start + i] = dest;
        }
        out
    }

    // ---- Internal helpers ---------------------------------------------------

    fn state(&mut self) -> &mut FunctionState {
        self.current
            .as_mut()
            .expect("WasmBackend: not inside a function")
    }

    fn flatten_value_locals(&self, values: &[WasmValue]) -> Vec<u32> {
        let state = self
            .current
            .as_ref()
            .expect("WasmBackend: not inside a function");
        values
            .iter()
            .flat_map(|value| state.locals_of(*value).iter().copied())
            .collect()
    }

    fn intern_type(&mut self, params: &[ValType], results: &[ValType]) -> u32 {
        let key = (params.to_vec(), results.to_vec());
        if let Some(&idx) = self.type_map.get(&key) {
            return idx;
        }
        let idx = self.type_map.len() as u32;
        self.types
            .ty()
            .function(params.iter().copied(), results.iter().copied());
        self.type_map.insert(key, idx);
        idx
    }

    fn ensure_import(&mut self, name: &str, params: &[ValType], results: &[ValType]) -> u32 {
        if let Some(&idx) = self.import_map.get(name) {
            return idx;
        }
        let type_idx = self.intern_type(params, results);
        let func_idx = self.next_func_idx;
        self.next_func_idx += 1;
        self.import_map.insert(String::from(name), func_idx);
        self.imports.push((String::from(name), type_idx));
        func_idx
    }

    fn flatten_scalar_tys(&self, ty: &LirType) -> Vec<LirType> {
        match ty {
            LirType::Arr(elem, n) => {
                let mut out = Vec::new();
                for _ in 0..*n {
                    out.extend(self.flatten_scalar_tys(elem));
                }
                out
            }
            LirType::Struct(id) => {
                let mut out = Vec::new();
                for field in &self.struct_defs[*id as usize].fields {
                    out.extend(self.flatten_scalar_tys(&field.ty));
                }
                out
            }
            _ => vec![ty.clone()],
        }
    }

    fn lir_to_val_ty(ty: &LirType) -> ValType {
        match ty {
            LirType::Bool
            | LirType::I8
            | LirType::U8
            | LirType::I16
            | LirType::U16
            | LirType::I32
            | LirType::U32 => ValType::I32,
            LirType::I64 | LirType::U64 => ValType::I64,
            LirType::I128
            | LirType::U128
            | LirType::I256
            | LirType::U256
            | LirType::Vector(_, _) => {
                panic!("WasmBackend: multi-word type passed where one Wasm value was required")
            }
            LirType::Native(t) => match t {
                NativeType::Bit
                | NativeType::_8
                | NativeType::AES8
                | NativeType::_16
                | NativeType::_32 => ValType::I32,
                NativeType::_64 | NativeType::Galois64 => ValType::I64,
                NativeType::_128 | NativeType::_256 => {
                    panic!(
                        "WasmBackend: multi-word native type passed where one Wasm value was required"
                    )
                }
                _ => ValType::I64,
            },
            LirType::Arr(_, _) | LirType::Struct(_) => {
                panic!("WasmBackend: aggregate passed to lir_to_val_ty; flatten first")
            }
            LirType::Ptr(_) => {
                panic!("WasmBackend: Ptr not supported (no StackAllocExt)")
            }
            _ => panic!("WasmBackend: unsupported LirType {ty:?}"),
        }
    }

    fn is_i64(ty: &LirType) -> bool {
        matches!(Self::lir_to_val_ty(ty), ValType::I64)
    }

    /// Canonical portable Wasm ABI expansion for one logical LIR value.
    ///
    /// Wasm MVP has neither arbitrary-width integers nor portable SIMD, so
    /// wide packed integers use little-endian `i64` words and vectors expand
    /// lane-by-lane using the element's native `i32`/`i64` representation.
    fn wasm_value_tys(ty: &LirType) -> Vec<ValType> {
        match ty {
            LirType::I128 | LirType::U128 => vec![ValType::I64, ValType::I64],
            LirType::I256 | LirType::U256 => {
                vec![ValType::I64, ValType::I64, ValType::I64, ValType::I64]
            }
            LirType::Vector(element, lanes) => {
                let mut values = Vec::new();
                for _ in 0..*lanes {
                    values.extend(Self::wasm_value_tys(element));
                }
                values
            }
            LirType::Native(NativeType::_128) => vec![ValType::I64, ValType::I64],
            LirType::Native(NativeType::_256) => {
                vec![ValType::I64, ValType::I64, ValType::I64, ValType::I64]
            }
            _ => vec![Self::lir_to_val_ty(ty)],
        }
    }

    fn binop(&mut self, lhs: WasmValue, rhs: WasmValue, kind: BinOpKind) -> WasmValue {
        let ty = self.state().ty_of(lhs).clone();
        let dest = self.state().alloc_value(ty.clone());
        let dests = self.state().locals_of(dest).to_vec();
        let lhs = self.state().locals_of(lhs).to_vec();
        let rhs = self.state().locals_of(rhs).to_vec();
        self.push_binop_components(&ty, &dests, &lhs, &rhs, &kind);
        dest
    }

    /// Whether a logical value is one integer split into little-endian i64
    /// limbs, rather than a vector of independently-operable values.
    fn is_packed_wide(ty: &LirType) -> bool {
        matches!(
            ty,
            LirType::I128
                | LirType::U128
                | LirType::I256
                | LirType::U256
                | LirType::Native(NativeType::_128 | NativeType::_256)
        )
    }

    fn push_binop_components(
        &mut self,
        ty: &LirType,
        dests: &[u32],
        lhs: &[u32],
        rhs: &[u32],
        kind: &BinOpKind,
    ) {
        assert_eq!(
            dests.len(),
            lhs.len(),
            "binary destination ABI width mismatch"
        );
        assert_eq!(lhs.len(), rhs.len(), "binary operand ABI width mismatch");

        if let LirType::Vector(element, lanes) = ty {
            let lane_width = Self::wasm_value_tys(element).len();
            for lane in 0..*lanes {
                let start = lane * lane_width;
                self.push_binop_components(
                    element,
                    &dests[start..start + lane_width],
                    &lhs[start..start + lane_width],
                    &rhs[start..start + lane_width],
                    kind,
                );
            }
            return;
        }

        if Self::is_packed_wide(ty) {
            match kind {
                BinOpKind::Add | BinOpKind::Sub => {
                    let carry = self.state().alloc_temp(ValType::I32);
                    let aux = self.state().alloc_temp(ValType::I32);
                    self.state().push_op(Op::WideAddSub {
                        dests: dests.to_vec(),
                        lhs: lhs.to_vec(),
                        rhs: rhs.to_vec(),
                        subtract: matches!(kind, BinOpKind::Sub),
                        is_i64: true,
                        carry,
                        aux,
                    });
                }
                BinOpKind::And | BinOpKind::Or | BinOpKind::Xor => {
                    for ((dest, lhs), rhs) in dests.iter().zip(lhs).zip(rhs) {
                        self.state().push_op(Op::Binop {
                            dest: *dest,
                            kind: kind.clone(),
                            lhs: *lhs,
                            rhs: *rhs,
                            is_i64: true,
                        });
                    }
                }
                _ => panic!(
                    "WasmBackend: {kind:?} is not yet supported for packed {}-bit integers",
                    ty.bit_width()
                ),
            }
            return;
        }

        assert_eq!(dests.len(), 1, "scalar operation received grouped values");
        self.state().push_op(Op::Binop {
            dest: dests[0],
            kind: kind.clone(),
            lhs: lhs[0],
            rhs: rhs[0],
            is_i64: Self::is_i64(ty),
        });
    }

    fn push_not_components(&mut self, ty: &LirType, dests: &[u32], vals: &[u32]) {
        assert_eq!(dests.len(), vals.len(), "not operand ABI width mismatch");
        if let LirType::Vector(element, lanes) = ty {
            let lane_width = Self::wasm_value_tys(element).len();
            for lane in 0..*lanes {
                let start = lane * lane_width;
                self.push_not_components(
                    element,
                    &dests[start..start + lane_width],
                    &vals[start..start + lane_width],
                );
            }
            return;
        }

        if Self::is_packed_wide(ty) {
            for (dest, val) in dests.iter().zip(vals) {
                self.state().push_op(Op::Not {
                    dest: *dest,
                    val: *val,
                    is_i64: true,
                    is_bool: false,
                });
            }
            return;
        }

        assert_eq!(dests.len(), 1, "scalar not received grouped values");
        self.state().push_op(Op::Not {
            dest: dests[0],
            val: vals[0],
            is_i64: Self::is_i64(ty),
            is_bool: *ty == LirType::Bool,
        });
    }

    fn call_extern_results(
        &mut self,
        name: &str,
        arg_tys: &[LirType],
        args: &[WasmValue],
        ret_tys: &[LirType],
    ) -> Vec<WasmValue> {
        let flat_arg_tys: Vec<LirType> = arg_tys
            .iter()
            .flat_map(|ty| self.flatten_scalar_tys(ty))
            .collect();
        assert_eq!(
            flat_arg_tys.len(),
            args.len(),
            "call_extern_results: flat arg count mismatch"
        );
        let params = flat_arg_tys
            .iter()
            .flat_map(Self::wasm_value_tys)
            .collect::<Vec<_>>();
        let flat_rets = ret_tys
            .iter()
            .flat_map(|ty| self.flatten_scalar_tys(ty))
            .collect::<Vec<_>>();
        let results = flat_rets
            .iter()
            .flat_map(Self::wasm_value_tys)
            .collect::<Vec<_>>();
        let resolved = self.name_config.apply(name);
        let func_idx = self.ensure_import(&resolved, &params, &results);

        let mut values = Vec::with_capacity(flat_rets.len());
        let mut dests = Vec::new();
        for ty in &flat_rets {
            let value = self.state().alloc_value(ty.clone());
            dests.extend_from_slice(self.state().locals_of(value));
            values.push(value);
        }
        let args = self.flatten_value_locals(args);
        self.state().push_op(Op::Call {
            dests,
            func_idx,
            args,
        });
        values
    }
}

// ============================================================================
// Lowering buffered ops → wasm_encoder::Function
// ============================================================================

/// Declare each extra local individually (groups of size 1) in allocation
/// order so WASM local indices match our monotonic allocator.
fn local_decls_in_alloc_order(state: &FunctionState) -> Vec<(u32, ValType)> {
    let param_count = state.wasm_param_tys.len() as u32;
    let mut decls = Vec::new();
    // Build a map local_idx → ValType for every non-param local.
    let mut local_ty: BTreeMap<u32, ValType> = BTreeMap::new();
    for v in &state.values {
        for (local, ty) in v
            .locals
            .iter()
            .copied()
            .zip(WasmBackend::wasm_value_tys(&v.ty))
        {
            if local >= param_count {
                local_ty.insert(local, ty);
            }
        }
    }
    if let Some(pc) = state.pc_local {
        if pc >= param_count {
            local_ty.insert(pc, ValType::I32);
        }
    }
    for (local, ty) in &state.temp_locals {
        if *local >= param_count {
            local_ty.insert(*local, *ty);
        }
    }
    for (_idx, vt) in local_ty {
        decls.push((1, vt));
    }
    decls
}

/// Emit via [`InstructionSink`] so we go through wax-core (not the inherent
/// `Function::instruction` method, which has a different signature).
fn emit(func: &mut Function, instr: Instruction<'_>) {
    InstructionSink::<(), ()>::instruction(func, &mut (), &instr).unwrap();
}

fn emit_val_const(func: &mut Function, ty: ValType, val: i64) {
    match ty {
        ValType::I32 => emit(func, Instruction::I32Const(val as i32)),
        ValType::I64 => emit(func, Instruction::I64Const(val)),
        _ => panic!("WasmBackend: unsupported const type"),
    }
}

fn emit_binop_instr(func: &mut Function, kind: &BinOpKind, is_i64: bool) {
    let instr = match (kind, is_i64) {
        (BinOpKind::Add, false) => Instruction::I32Add,
        (BinOpKind::Add, true) => Instruction::I64Add,
        (BinOpKind::Sub, false) => Instruction::I32Sub,
        (BinOpKind::Sub, true) => Instruction::I64Sub,
        (BinOpKind::Mul, false) => Instruction::I32Mul,
        (BinOpKind::Mul, true) => Instruction::I64Mul,
        (BinOpKind::Udiv, false) => Instruction::I32DivU,
        (BinOpKind::Udiv, true) => Instruction::I64DivU,
        (BinOpKind::Sdiv, false) => Instruction::I32DivS,
        (BinOpKind::Sdiv, true) => Instruction::I64DivS,
        (BinOpKind::And, false) => Instruction::I32And,
        (BinOpKind::And, true) => Instruction::I64And,
        (BinOpKind::Or, false) => Instruction::I32Or,
        (BinOpKind::Or, true) => Instruction::I64Or,
        (BinOpKind::Xor, false) => Instruction::I32Xor,
        (BinOpKind::Xor, true) => Instruction::I64Xor,
        (BinOpKind::Shl, false) => Instruction::I32Shl,
        (BinOpKind::Shl, true) => Instruction::I64Shl,
        (BinOpKind::Lshr, false) => Instruction::I32ShrU,
        (BinOpKind::Lshr, true) => Instruction::I64ShrU,
        (BinOpKind::Ashr, false) => Instruction::I32ShrS,
        (BinOpKind::Ashr, true) => Instruction::I64ShrS,
    };
    emit(func, instr);
}

fn emit_icmp_instr(func: &mut Function, pred: IcmpPred, is_i64: bool) {
    let instr = match (pred, is_i64) {
        (IcmpPred::Eq, false) => Instruction::I32Eq,
        (IcmpPred::Eq, true) => Instruction::I64Eq,
        (IcmpPred::Ne, false) => Instruction::I32Ne,
        (IcmpPred::Ne, true) => Instruction::I64Ne,
        (IcmpPred::Ult, false) => Instruction::I32LtU,
        (IcmpPred::Ult, true) => Instruction::I64LtU,
        (IcmpPred::Ule, false) => Instruction::I32LeU,
        (IcmpPred::Ule, true) => Instruction::I64LeU,
        (IcmpPred::Ugt, false) => Instruction::I32GtU,
        (IcmpPred::Ugt, true) => Instruction::I64GtU,
        (IcmpPred::Uge, false) => Instruction::I32GeU,
        (IcmpPred::Uge, true) => Instruction::I64GeU,
        (IcmpPred::Slt, false) => Instruction::I32LtS,
        (IcmpPred::Slt, true) => Instruction::I64LtS,
        (IcmpPred::Sle, false) => Instruction::I32LeS,
        (IcmpPred::Sle, true) => Instruction::I64LeS,
        (IcmpPred::Sgt, false) => Instruction::I32GtS,
        (IcmpPred::Sgt, true) => Instruction::I64GtS,
        (IcmpPred::Sge, false) => Instruction::I32GeS,
        (IcmpPred::Sge, true) => Instruction::I64GeS,
    };
    emit(func, instr);
}

fn local_ty_lookup(state: &FunctionState, local: u32) -> LirType {
    for v in &state.values {
        if v.locals.contains(&local) {
            return v.ty.clone();
        }
    }
    if state.pc_local == Some(local) {
        return LirType::I32;
    }
    panic!("WasmBackend: unknown local {local}");
}

fn emit_op(
    func: &mut Function,
    state: &FunctionState,
    op: &Op,
    sibling_idx: &BTreeMap<String, u32>,
) {
    match op {
        Op::Iconst { dests, ty, val } => {
            let word_tys = WasmBackend::wasm_value_tys(ty);
            assert_eq!(dests.len(), word_tys.len(), "constant ABI width mismatch");
            for (index, (dest, word_ty)) in dests.iter().zip(word_tys).enumerate() {
                emit_val_const(func, word_ty, if index == 0 { *val } else { 0 });
                emit(func, Instruction::LocalSet(*dest));
            }
        }
        Op::Binop {
            dest,
            kind,
            lhs,
            rhs,
            is_i64,
        } => {
            emit(func, Instruction::LocalGet(*lhs));
            emit(func, Instruction::LocalGet(*rhs));
            emit_binop_instr(func, kind, *is_i64);
            emit(func, Instruction::LocalSet(*dest));
        }
        Op::WideAddSub {
            dests,
            lhs,
            rhs,
            subtract,
            is_i64,
            carry,
            aux,
        } => {
            assert_eq!(
                dests.len(),
                lhs.len(),
                "wide add/sub destination ABI width mismatch"
            );
            assert_eq!(
                lhs.len(),
                rhs.len(),
                "wide add/sub operand ABI width mismatch"
            );
            // `carry` is an i32 Boolean carried between little-endian limbs.
            emit(func, Instruction::I32Const(0));
            emit(func, Instruction::LocalSet(*carry));
            for ((dest, lhs), rhs) in dests.iter().zip(lhs).zip(rhs) {
                // First calculate a +/- b and remember its carry/borrow.
                emit(func, Instruction::LocalGet(*lhs));
                emit(func, Instruction::LocalGet(*rhs));
                if *subtract {
                    emit_binop_instr(func, &BinOpKind::Sub, *is_i64);
                } else {
                    emit_binop_instr(func, &BinOpKind::Add, *is_i64);
                }
                emit(func, Instruction::LocalSet(*dest));

                if *subtract {
                    emit(func, Instruction::LocalGet(*lhs));
                    emit(func, Instruction::LocalGet(*rhs));
                    emit_icmp_instr(func, IcmpPred::Ult, *is_i64);
                } else {
                    emit(func, Instruction::LocalGet(*dest));
                    emit(func, Instruction::LocalGet(*lhs));
                    emit_icmp_instr(func, IcmpPred::Ult, *is_i64);
                }
                emit(func, Instruction::LocalSet(*aux));

                // Fold in the carry/borrow from the less-significant limb.
                emit(func, Instruction::LocalGet(*dest));
                emit(func, Instruction::LocalGet(*carry));
                if *is_i64 {
                    emit(func, Instruction::I64ExtendI32U);
                }
                if *subtract {
                    emit(func, Instruction::I64Sub);
                } else {
                    emit(func, Instruction::I64Add);
                }
                emit(func, Instruction::LocalSet(*dest));

                // A carry-in creates another carry exactly when `d == 0`;
                // a borrow-in creates another borrow exactly when `d == MAX`.
                emit(func, Instruction::LocalGet(*carry));
                emit(func, Instruction::LocalGet(*dest));
                if *subtract {
                    if *is_i64 {
                        emit(func, Instruction::I64Const(-1));
                    } else {
                        emit(func, Instruction::I32Const(-1));
                    }
                } else if *is_i64 {
                    emit(func, Instruction::I64Const(0));
                } else {
                    emit(func, Instruction::I32Const(0));
                }
                if *is_i64 {
                    emit(func, Instruction::I64Eq);
                } else {
                    emit(func, Instruction::I32Eq);
                }
                emit(func, Instruction::I32And);
                emit(func, Instruction::LocalSet(*carry));
                emit(func, Instruction::LocalGet(*aux));
                emit(func, Instruction::LocalGet(*carry));
                emit(func, Instruction::I32Or);
                emit(func, Instruction::LocalSet(*carry));
            }
        }
        Op::Not {
            dest,
            val,
            is_i64,
            is_bool,
        } => {
            emit(func, Instruction::LocalGet(*val));
            if *is_bool {
                emit(func, Instruction::I32Const(1));
                emit(func, Instruction::I32Xor);
            } else if *is_i64 {
                emit(func, Instruction::I64Const(-1));
                emit(func, Instruction::I64Xor);
            } else {
                emit(func, Instruction::I32Const(-1));
                emit(func, Instruction::I32Xor);
            }
            emit(func, Instruction::LocalSet(*dest));
        }
        Op::Icmp {
            dest,
            pred,
            lhs,
            rhs,
        } => {
            let ty = local_ty_lookup(state, *lhs);
            let i64 = WasmBackend::is_i64(&ty);
            emit(func, Instruction::LocalGet(*lhs));
            emit(func, Instruction::LocalGet(*rhs));
            emit_icmp_instr(func, *pred, i64);
            emit(func, Instruction::LocalSet(*dest));
        }
        Op::Zext { dest, val, dst_ty } => {
            let src_ty = local_ty_lookup(state, *val);
            emit(func, Instruction::LocalGet(*val));
            match (
                WasmBackend::lir_to_val_ty(&src_ty),
                WasmBackend::lir_to_val_ty(dst_ty),
            ) {
                (ValType::I32, ValType::I64) => {
                    emit(func, Instruction::I64ExtendI32U);
                }
                (ValType::I32, ValType::I32) => {
                    let bits = src_ty.bit_width().min(32);
                    if bits < 32 && src_ty != LirType::Bool {
                        let mask = (1i32 << bits) - 1;
                        emit(func, Instruction::I32Const(mask));
                        emit(func, Instruction::I32And);
                    }
                }
                (ValType::I64, ValType::I64) => {}
                _ => {}
            }
            emit(func, Instruction::LocalSet(*dest));
        }
        Op::Sext { dest, val, dst_ty } => {
            let src_ty = local_ty_lookup(state, *val);
            emit(func, Instruction::LocalGet(*val));
            match (
                WasmBackend::lir_to_val_ty(&src_ty),
                WasmBackend::lir_to_val_ty(dst_ty),
            ) {
                (ValType::I32, ValType::I64) => {
                    emit(func, Instruction::I64ExtendI32S);
                }
                _ => {
                    let bits = src_ty.bit_width().min(32);
                    if bits < 32 {
                        let sh = 32 - bits;
                        emit(func, Instruction::I32Const(sh as i32));
                        emit(func, Instruction::I32Shl);
                        emit(func, Instruction::I32Const(sh as i32));
                        emit(func, Instruction::I32ShrS);
                    }
                }
            }
            emit(func, Instruction::LocalSet(*dest));
        }
        Op::Trunc { dest, val, dst_ty } => {
            let src_ty = local_ty_lookup(state, *val);
            emit(func, Instruction::LocalGet(*val));
            match (
                WasmBackend::lir_to_val_ty(&src_ty),
                WasmBackend::lir_to_val_ty(dst_ty),
            ) {
                (ValType::I64, ValType::I32) => {
                    emit(func, Instruction::I32WrapI64);
                }
                (ValType::I32, ValType::I32) => {
                    let bits = dst_ty.bit_width().min(32);
                    if bits < 32 {
                        let mask = (1i32 << bits) - 1;
                        emit(func, Instruction::I32Const(mask));
                        emit(func, Instruction::I32And);
                    }
                }
                _ => {}
            }
            emit(func, Instruction::LocalSet(*dest));
        }
        Op::Select {
            dests,
            cond,
            then_vals,
            else_vals,
        } => {
            for ((dest, then_val), else_val) in dests.iter().zip(then_vals).zip(else_vals) {
                emit(func, Instruction::LocalGet(*then_val));
                emit(func, Instruction::LocalGet(*else_val));
                emit(func, Instruction::LocalGet(*cond));
                emit(func, Instruction::Select);
                emit(func, Instruction::LocalSet(*dest));
            }
        }
        Op::Copy { dest, src } => {
            emit(func, Instruction::LocalGet(*src));
            emit(func, Instruction::LocalSet(*dest));
        }
        Op::Call {
            dests,
            func_idx,
            args,
        } => {
            for &a in args {
                emit(func, Instruction::LocalGet(a));
            }
            emit(func, Instruction::Call(*func_idx));
            for &d in dests.iter().rev() {
                emit(func, Instruction::LocalSet(d));
            }
        }
        Op::CallSibling { dests, name, args } => {
            let func_idx = *sibling_idx.get(name).unwrap_or_else(|| {
                panic!("WasmBackend: sibling call to undefined function `{name}`")
            });
            for &a in args {
                emit(func, Instruction::LocalGet(a));
            }
            emit(func, Instruction::Call(func_idx));
            for &d in dests.iter().rev() {
                emit(func, Instruction::LocalSet(d));
            }
        }
        Op::Jump { .. } | Op::Branch { .. } | Op::Ret { .. } | Op::Table { .. } => {
            panic!("emit_op: terminators must be lowered by emit_terminator");
        }
    }
}

fn emit_assign_block_params(func: &mut Function, state: &FunctionState, target: u32, args: &[u32]) {
    let params = &state.blocks[target as usize].param_locals;
    assert_eq!(
        params.len(),
        args.len(),
        "jump arg count mismatch for block {target}"
    );
    for &a in args {
        emit(func, Instruction::LocalGet(a));
    }
    for &p in params.iter().rev() {
        emit(func, Instruction::LocalSet(p));
    }
}

fn emit_terminator(func: &mut Function, state: &FunctionState, op: &Op, dispatch_depth: u32) {
    match op {
        Op::Ret { vals } => {
            for &v in vals {
                emit(func, Instruction::LocalGet(v));
            }
            emit(func, Instruction::Return);
        }
        Op::Jump { target, args } => {
            emit_assign_block_params(func, state, *target, args);
            let pc = state.pc_local.expect("pc local");
            emit(func, Instruction::I32Const(*target as i32));
            emit(func, Instruction::LocalSet(pc));
            emit(func, Instruction::Br(dispatch_depth));
        }
        Op::Branch {
            cond,
            then_block,
            then_args,
            else_block,
            else_args,
        } => {
            let pc = state.pc_local.expect("pc local");
            emit(func, Instruction::LocalGet(*cond));
            emit(func, Instruction::If(wasm_encoder::BlockType::Empty));
            emit_assign_block_params(func, state, *then_block, then_args);
            emit(func, Instruction::I32Const(*then_block as i32));
            emit(func, Instruction::LocalSet(pc));
            emit(func, Instruction::Else);
            emit_assign_block_params(func, state, *else_block, else_args);
            emit(func, Instruction::I32Const(*else_block as i32));
            emit(func, Instruction::LocalSet(pc));
            emit(func, Instruction::End);
            emit(func, Instruction::Br(dispatch_depth));
        }
        Op::Table {
            index,
            cases,
            default_block,
            default_args,
        } => {
            let pc = state.pc_local.expect("pc local");
            let idx_ty = local_ty_lookup(state, *index);
            let is_i64 = WasmBackend::is_i64(&idx_ty);
            // Nested `if (index == key) {...} else { <next case, or default> }`,
            // each branch only assigning block params and setting `pc` —
            // mirroring `Branch`'s trick of a single shared `Br` after every
            // `if`/`else` has closed, rather than one `Br` per case (which
            // would need a depth that grows with nesting).
            for (key, target, args) in cases {
                emit(func, Instruction::LocalGet(*index));
                if is_i64 {
                    emit(func, Instruction::I64Const(*key));
                    emit(func, Instruction::I64Eq);
                } else {
                    emit(func, Instruction::I32Const(*key as i32));
                    emit(func, Instruction::I32Eq);
                }
                emit(func, Instruction::If(wasm_encoder::BlockType::Empty));
                emit_assign_block_params(func, state, *target, args);
                emit(func, Instruction::I32Const(*target as i32));
                emit(func, Instruction::LocalSet(pc));
                emit(func, Instruction::Else);
            }
            emit_assign_block_params(func, state, *default_block, default_args);
            emit(func, Instruction::I32Const(*default_block as i32));
            emit(func, Instruction::LocalSet(pc));
            for _ in cases {
                emit(func, Instruction::End);
            }
            emit(func, Instruction::Br(dispatch_depth));
        }
        _ => panic!("not a terminator"),
    }
}

fn lower_function(state: &FunctionState, sibling_idx: &BTreeMap<String, u32>) -> Function {
    let decls = local_decls_in_alloc_order(state);
    let mut func = Function::new(decls);
    let nblocks = state.blocks.len() as u32;
    let multi = nblocks > 1 || state.pc_local.is_some();

    if !multi {
        let block = &state.blocks[0];
        for op in &block.ops {
            if matches!(
                op,
                Op::Jump { .. } | Op::Branch { .. } | Op::Ret { .. } | Op::Table { .. }
            ) {
                match op {
                    Op::Ret { vals } => {
                        for &v in vals {
                            emit(&mut func, Instruction::LocalGet(v));
                        }
                        emit(&mut func, Instruction::Return);
                    }
                    _ => panic!("WasmBackend: multi-block terminator in single-block function"),
                }
            } else {
                emit_op(&mut func, state, op, sibling_idx);
            }
        }
        InstructionSink::<(), ()>::finish(&mut func).unwrap();
        return func;
    }

    let pc = state.ensure_pc_local_immut();
    emit(&mut func, Instruction::I32Const(0));
    emit(&mut func, Instruction::LocalSet(pc));

    // loop $dispatch
    //   block $default
    //     block $b_{n-1} ... block $b_0
    //       br_table ...
    //     end ;; fall into b0 body
    //     ...
    //   end ;; default → unreachable
    // end ;; dispatch
    emit(&mut func, Instruction::Loop(wasm_encoder::BlockType::Empty)); // $dispatch
    emit(
        &mut func,
        Instruction::Block(wasm_encoder::BlockType::Empty),
    ); // $default
    for _ in 0..nblocks {
        emit(
            &mut func,
            Instruction::Block(wasm_encoder::BlockType::Empty),
        );
    }

    let targets: Vec<u32> = (0..nblocks).collect();
    let default_target = nblocks;
    emit(&mut func, Instruction::LocalGet(pc));
    emit(
        &mut func,
        Instruction::BrTable(Cow::Owned(targets), default_target),
    );

    for bid in 0..nblocks {
        emit(&mut func, Instruction::End);
        let block = &state.blocks[bid as usize];
        for op in &block.ops {
            if matches!(
                op,
                Op::Jump { .. } | Op::Branch { .. } | Op::Ret { .. } | Op::Table { .. }
            ) {
                // Depth to $dispatch: remaining b_* wrappers + $default.
                let dispatch_depth = nblocks - bid;
                emit_terminator(&mut func, state, op, dispatch_depth);
            } else {
                emit_op(&mut func, state, op, sibling_idx);
            }
        }
        if !block.terminated {
            emit(&mut func, Instruction::Unreachable);
        }
    }

    emit(&mut func, Instruction::End); // $default
    emit(&mut func, Instruction::Unreachable);
    emit(&mut func, Instruction::End); // $dispatch
    // All real paths `return`; mark fallthrough unreachable so the function
    // result type validates.
    emit(&mut func, Instruction::Unreachable);

    InstructionSink::<(), ()>::finish(&mut func).unwrap();
    func
}

impl FunctionState {
    /// Read pc local; panics if missing (multi-block path must have allocated it).
    fn ensure_pc_local_immut(&self) -> u32 {
        self.pc_local
            .expect("WasmBackend: pc local missing for multi-block function")
    }
}

// ============================================================================
// LirTarget impl
// ============================================================================

impl LirTarget for WasmBackend {
    type Value = WasmValue;
    type Block = WasmBlock;

    fn abi(&self) -> LirAbi {
        LirAbi::DEFAULT
    }

    fn define_struct(&mut self, def: StructDef) -> StructId {
        let id = self.next_struct_id;
        self.next_struct_id += 1;
        self.struct_defs.push(def);
        id
    }

    fn begin_function(
        &mut self,
        name: &str,
        params: &[LirType],
        ret: Option<LirType>,
    ) -> (WasmBlock, Vec<Vec<WasmValue>>) {
        assert!(
            self.current.is_none(),
            "begin_function called while already inside a function"
        );

        let flat_param_groups: Vec<Vec<LirType>> = params
            .iter()
            .map(|ty| self.flatten_scalar_tys(ty))
            .collect();
        let flat_params: Vec<LirType> = flat_param_groups.iter().flatten().cloned().collect();
        let wasm_param_tys: Vec<ValType> =
            flat_params.iter().flat_map(Self::wasm_value_tys).collect();

        let flat_rets = ret
            .as_ref()
            .map(|ty| self.flatten_scalar_tys(ty))
            .unwrap_or_default();
        let wasm_result_tys: Vec<ValType> =
            flat_rets.iter().flat_map(Self::wasm_value_tys).collect();

        let mut state = FunctionState {
            name: self.name_config.apply(name),
            wasm_param_tys: wasm_param_tys.clone(),
            wasm_result_tys,
            values: Vec::new(),
            next_local: 0,
            blocks: vec![BlockState {
                param_locals: Vec::new(),
                ops: Vec::new(),
                terminated: false,
            }],
            current_block: None,
            pc_local: None,
            temp_locals: Vec::new(),
        };

        // Allocate parameter values as locals 0..n-1.
        let mut param_vals: Vec<Vec<WasmValue>> = Vec::new();
        for group in &flat_param_groups {
            let mut g = Vec::new();
            for ty in group {
                g.push(state.alloc_value(ty.clone()));
            }
            param_vals.push(g);
        }

        self.current = Some(state);
        (WasmBlock(0), param_vals)
    }

    fn end_function(&mut self) {
        let mut state = self
            .current
            .take()
            .expect("end_function called outside a function");

        // Multi-block functions need a PC local for the dispatcher.
        if state.blocks.len() > 1 {
            state.ensure_pc_local();
        }

        let type_idx = self.intern_type(&state.wasm_param_tys, &state.wasm_result_tys);
        // Defined functions follow imports in the function index space.
        // next_func_idx tracks imports only until we start completing functions;
        // we don't need the defined func index until export time. Lowering
        // itself is deferred to `finish` — see `CompletedFunction::state`.
        let name = state.name.clone();
        self.completed.push(CompletedFunction {
            name,
            type_idx,
            state,
        });
    }

    fn create_block(&mut self) -> WasmBlock {
        let state = self.state();
        let id = state.blocks.len() as u32;
        state.blocks.push(BlockState {
            param_locals: Vec::new(),
            ops: Vec::new(),
            terminated: false,
        });
        // Ensure PC exists once we have multiple blocks.
        if state.blocks.len() > 1 {
            state.ensure_pc_local();
        }
        WasmBlock(id)
    }

    fn add_block_param(&mut self, block: WasmBlock, ty: LirType) -> WasmValue {
        let v = self.state().alloc_value(ty);
        let locals = self.state().locals_of(v).to_vec();
        self.state().blocks[block.0 as usize]
            .param_locals
            .extend_from_slice(&locals);
        v
    }

    fn switch_to_block(&mut self, block: WasmBlock) {
        self.state().current_block = Some(block.0);
    }

    fn iconst(&mut self, ty: LirType, val: i64) -> WasmValue {
        let dest = self.state().alloc_value(ty.clone());
        let dests = self.state().locals_of(dest).to_vec();
        self.state().push_op(Op::Iconst { dests, ty, val });
        dest
    }

    fn add(&mut self, lhs: WasmValue, rhs: WasmValue) -> WasmValue {
        self.binop(lhs, rhs, BinOpKind::Add)
    }
    fn sub(&mut self, lhs: WasmValue, rhs: WasmValue) -> WasmValue {
        self.binop(lhs, rhs, BinOpKind::Sub)
    }
    fn mul(&mut self, lhs: WasmValue, rhs: WasmValue) -> WasmValue {
        self.binop(lhs, rhs, BinOpKind::Mul)
    }
    fn udiv(&mut self, lhs: WasmValue, rhs: WasmValue) -> WasmValue {
        self.binop(lhs, rhs, BinOpKind::Udiv)
    }
    fn sdiv(&mut self, lhs: WasmValue, rhs: WasmValue) -> WasmValue {
        self.binop(lhs, rhs, BinOpKind::Sdiv)
    }

    fn and(&mut self, lhs: WasmValue, rhs: WasmValue) -> WasmValue {
        self.binop(lhs, rhs, BinOpKind::And)
    }
    fn or(&mut self, lhs: WasmValue, rhs: WasmValue) -> WasmValue {
        self.binop(lhs, rhs, BinOpKind::Or)
    }
    fn xor(&mut self, lhs: WasmValue, rhs: WasmValue) -> WasmValue {
        self.binop(lhs, rhs, BinOpKind::Xor)
    }
    fn not(&mut self, val: WasmValue) -> WasmValue {
        let ty = self.state().ty_of(val).clone();
        let dest = self.state().alloc_value(ty.clone());
        let dests = self.state().locals_of(dest).to_vec();
        let vals = self.state().locals_of(val).to_vec();
        self.push_not_components(&ty, &dests, &vals);
        dest
    }
    fn shl(&mut self, val: WasmValue, shift: WasmValue) -> WasmValue {
        self.binop(val, shift, BinOpKind::Shl)
    }
    fn lshr(&mut self, val: WasmValue, shift: WasmValue) -> WasmValue {
        self.binop(val, shift, BinOpKind::Lshr)
    }
    fn ashr(&mut self, val: WasmValue, shift: WasmValue) -> WasmValue {
        self.binop(val, shift, BinOpKind::Ashr)
    }

    fn icmp(&mut self, pred: IcmpPred, lhs: WasmValue, rhs: WasmValue) -> WasmValue {
        let dest = self.state().alloc_value(LirType::Bool);
        let dest_l = self.state().local_of(dest);
        let lhs_l = self.state().local_of(lhs);
        let rhs_l = self.state().local_of(rhs);
        self.state().push_op(Op::Icmp {
            dest: dest_l,
            pred,
            lhs: lhs_l,
            rhs: rhs_l,
        });
        dest
    }

    fn zext(&mut self, val: WasmValue, dst_ty: LirType) -> WasmValue {
        let dest = self.state().alloc_value(dst_ty.clone());
        let dest_l = self.state().local_of(dest);
        let val_l = self.state().local_of(val);
        self.state().push_op(Op::Zext {
            dest: dest_l,
            val: val_l,
            dst_ty,
        });
        dest
    }
    fn sext(&mut self, val: WasmValue, dst_ty: LirType) -> WasmValue {
        let dest = self.state().alloc_value(dst_ty.clone());
        let dest_l = self.state().local_of(dest);
        let val_l = self.state().local_of(val);
        self.state().push_op(Op::Sext {
            dest: dest_l,
            val: val_l,
            dst_ty,
        });
        dest
    }
    fn trunc(&mut self, val: WasmValue, dst_ty: LirType) -> WasmValue {
        let dest = self.state().alloc_value(dst_ty.clone());
        let dest_l = self.state().local_of(dest);
        let val_l = self.state().local_of(val);
        self.state().push_op(Op::Trunc {
            dest: dest_l,
            val: val_l,
            dst_ty,
        });
        dest
    }

    fn select(&mut self, cond: WasmValue, then_val: WasmValue, else_val: WasmValue) -> WasmValue {
        let ty = self.state().ty_of(then_val).clone();
        let dest = self.state().alloc_value(ty);
        let dests = self.state().locals_of(dest).to_vec();
        let cond_l = self.state().local_of(cond);
        let then_vals = self.state().locals_of(then_val).to_vec();
        let else_vals = self.state().locals_of(else_val).to_vec();
        assert_eq!(
            then_vals.len(),
            else_vals.len(),
            "select operand ABI width mismatch"
        );
        self.state().push_op(Op::Select {
            dests,
            cond: cond_l,
            then_vals,
            else_vals,
        });
        dest
    }

    fn value_scalar_type(&self, val: &WasmValue) -> LirType {
        self.current
            .as_ref()
            .expect("value_scalar_type: not inside a function")
            .values[val.0 as usize]
            .ty
            .clone()
    }

    fn call_extern(
        &mut self,
        name: &str,
        arg_tys: &[LirType],
        args: &[WasmValue],
        ret_ty: Option<LirType>,
    ) -> Vec<WasmValue> {
        self.call_extern_results(name, arg_tys, args, ret_ty.as_slice())
    }

    /// Call a function defined in this same module.
    ///
    /// Unlike [`call_extern`](Self::call_extern) (which resolves to a WASM
    /// *import*), this defers resolution to [`finish`](Self::finish) via
    /// [`Op::CallSibling`] — the callee's final function index depends on
    /// the module's total import count and every defined function's
    /// position, neither of which is settled until every `begin_function`/
    /// `end_function` pair has run, so forward references and mutual
    /// recursion are supported.
    fn call(
        &mut self,
        name: &str,
        arg_tys: &[LirType],
        args: &[WasmValue],
        ret_ty: Option<LirType>,
    ) -> Vec<WasmValue> {
        let flat_arg_tys: Vec<LirType> = arg_tys
            .iter()
            .flat_map(|ty| self.flatten_scalar_tys(ty))
            .collect();
        assert_eq!(
            flat_arg_tys.len(),
            args.len(),
            "call: flat arg count mismatch"
        );
        let flat_rets = ret_ty
            .as_ref()
            .map(|ty| self.flatten_scalar_tys(ty))
            .unwrap_or_default();
        let resolved_name = self.name_config.apply(name);

        let mut dests = Vec::new();
        let mut dest_locals = Vec::new();
        for ty in &flat_rets {
            let v = self.state().alloc_value(ty.clone());
            dest_locals.extend_from_slice(self.state().locals_of(v));
            dests.push(v);
        }
        let arg_locals = self.flatten_value_locals(args);
        self.state().push_op(Op::CallSibling {
            dests: dest_locals,
            name: resolved_name,
            args: arg_locals,
        });
        dests
    }

    fn jump(&mut self, target: WasmBlock, branch: BranchTarget<WasmValue>) {
        let args = self.flatten_value_locals(&branch.args);
        self.state().push_op(Op::Jump {
            target: target.0,
            args,
        });
    }

    fn branch(
        &mut self,
        cond: WasmValue,
        then_block: WasmBlock,
        then_branch: BranchTarget<WasmValue>,
        else_block: WasmBlock,
        else_branch: BranchTarget<WasmValue>,
    ) {
        let cond_l = self.state().local_of(cond);
        let then_args = self.flatten_value_locals(&then_branch.args);
        let else_args = self.flatten_value_locals(&else_branch.args);
        self.state().push_op(Op::Branch {
            cond: cond_l,
            then_block: then_block.0,
            then_args,
            else_block: else_block.0,
            else_args,
        });
    }

    fn ret(&mut self, vals: &[WasmValue]) {
        let locals = self.flatten_value_locals(vals);
        self.state().push_op(Op::Ret { vals: locals });
    }

    /// Lowered to a chain of `if (index == key) {...} else {...}` — see
    /// [`Op::Table`]. Every case's (and the default's) target block already
    /// participates in this function's `pc_local`/`br_table` dispatcher
    /// (built once by [`lower_function`] for any function with more than one
    /// block), so `switch` only needs to pick the right `pc` value and let
    /// the existing dispatcher do the rest.
    fn switch(
        &mut self,
        index: WasmValue,
        cases: &[(i64, WasmBlock, BranchTarget<WasmValue>)],
        default_block: WasmBlock,
        default_branch: BranchTarget<WasmValue>,
    ) {
        let index_l = self.state().local_of(index);
        let cases: Vec<(i64, u32, Vec<u32>)> = cases
            .iter()
            .map(|(key, block, branch)| {
                let args = self.flatten_value_locals(&branch.args);
                (*key, block.0, args)
            })
            .collect();
        let default_args = self.flatten_value_locals(&default_branch.args);
        self.state().push_op(Op::Table {
            index: index_l,
            cases,
            default_block: default_block.0,
            default_args,
        });
    }

    fn block_ordinal(&self, block: &WasmBlock) -> i64 {
        block.0 as i64
    }

    fn oracle(
        &mut self,
        name: &str,
        arg_tys: &[LirType],
        args: &[WasmValue],
        ret_tys: &[LirType],
    ) -> Vec<WasmValue> {
        self.call_extern_results(&alloc::format!("oracle_{name}"), arg_tys, args, ret_tys)
    }

    fn action(
        &mut self,
        name: &str,
        guard: WasmValue,
        arg_tys: &[LirType],
        args: &[WasmValue],
        fallbacks: &[WasmValue],
        ret_tys: &[LirType],
    ) -> Vec<WasmValue> {
        let results =
            self.call_extern_results(&alloc::format!("action_{name}"), arg_tys, args, ret_tys);
        results
            .iter()
            .zip(fallbacks.iter())
            .map(|(&r, &fb)| self.select(guard, r, fb))
            .collect()
    }

    fn rng(&mut self, ty: LirType) -> WasmValue {
        // Import: `rng_fn(out_ptr: i32, len: i32)` is awkward without memory.
        // The portable ABI instead returns the (possibly flattened) value
        // directly, just like named Volar RNG sources do.
        let name = self.rng_fn.clone();
        self.call_extern(&name, &[], &[], Some(ty))
            .into_iter()
            .next()
            .expect("RNG must return exactly one value")
    }

    fn rng_named(&mut self, name: &str, ty: LirType) -> WasmValue {
        self.call_extern(&alloc::format!("rng_{name}"), &[], &[], Some(ty))
            .into_iter()
            .next()
            .expect("named RNG must return exactly one value")
    }

    // StackAllocExt intentionally unsupported for now.
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;
    use volar_ir::{
        boolar::{BIrBlock, BIrBlocks, BIrStmt, BIrTarget, BIrTerminator, LaneId},
        ir::{
            IRBlock, IRBlockTargetId, IRBlocks, IRBranchTarget, IRStmt, IRTerminator, IRType,
            IRTypes, IRVarId,
        },
    };
    use volar_ir_common::{ActionTarget, Node, StorageId, Type};
    use volar_ir_passes::lower_lir::{lower_biir, lower_ir};

    fn external_bit_fixture() -> BIrBlocks {
        BIrBlocks {
            blocks: vec![BIrBlock {
                params: 4,
                stmts: vec![
                    Node::new(
                        BIrStmt::OracleBit {
                            name: "lookup".into(),
                            args: vec![IRVarId(1)],
                            bit: 2,
                            occurrence: 7,
                        },
                        (),
                        None,
                    ),
                    Node::new(
                        BIrStmt::RngBit {
                            name: "nonce".into(),
                            bit: 3,
                            occurrence: 8,
                        },
                        (),
                        None,
                    ),
                    Node::new(
                        BIrStmt::ActionStoreBit {
                            name: "commit".into(),
                            guard: IRVarId(0),
                            args: vec![IRVarId(4), IRVarId(5)],
                            fallback: IRVarId(3),
                            storage: StorageId(9),
                            lane: LaneId(4),
                            addr: vec![IRVarId(2)],
                            bit: 1,
                            occurrence: 9,
                        },
                        (),
                        None,
                    ),
                ],
                terminator: BIrTerminator::Jmp(BIrTarget {
                    block: IRBlockTargetId::Return,
                    args: vec![IRVarId(4), IRVarId(5)],
                }),
            }],
            pre_init: vec![],
        }
    }

    #[test]
    fn lowers_direct_external_bits_to_configured_wasm_imports() {
        let mut remap = BTreeMap::new();
        remap.insert("oracle_lookup".into(), "host_lookup_bit".into());
        remap.insert("rng_nonce".into(), "host_nonce_bit".into());
        remap.insert("action_commit".into(), "host_commit_bit".into());
        let mut backend = WasmBackend::new()
            .with_import_module("volar-host")
            .with_name_config(NameConfig {
                prefix: String::new(),
                remap,
            });
        lower_biir(&external_bit_fixture(), "run", &mut backend);
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, backend.finish()).unwrap();
        let imports: Vec<_> = module
            .imports()
            .map(|import| (import.module().to_string(), import.name().to_string()))
            .collect();
        assert_eq!(
            imports,
            vec![
                ("volar-host".into(), "host_lookup_bit".into()),
                ("volar-host".into(), "host_nonce_bit".into()),
                ("volar-host".into(), "host_commit_bit".into()),
            ]
        );
    }

    #[test]
    fn high_volar_wide_and_vector_values_use_flat_wasm_multivalue_abi() {
        fn rng_program(ty: volar_ir::ir::IRTypeId, name: &str) -> IRBlocks {
            let mut block = IRBlock {
                params: vec![],
                stmts: vec![],
                terminator: IRTerminator::Jmp {
                    target: IRBranchTarget::new(IRBlockTargetId::Return, vec![IRVarId(0)]),
                },
            };
            block.push_stmt(
                IRStmt::Rng {
                    name: name.into(),
                    ty,
                },
                (),
            );
            IRBlocks::new(vec![block])
        }

        let mut types = IRTypes::new();
        let wide = types.primitive(Type::_256);
        let word = types.primitive(Type::_32);
        let vector = types.push(IRType::Vec(4, word));
        let mut backend = WasmBackend::new().with_import_module("volar-host");
        lower_ir(&rng_program(wide, "wide"), &types, "run_wide", &mut backend);
        lower_ir(
            &rng_program(vector, "vector"),
            &types,
            "run_vector",
            &mut backend,
        );

        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, backend.finish()).unwrap();
        let export_results = |name| match module.get_export(name).unwrap() {
            wasmtime::ExternType::Func(function) => function.results().collect::<Vec<_>>(),
            other => panic!("{name} is not a function export: {other:?}"),
        };
        let wide_results = export_results("run_wide");
        let vector_results = export_results("run_vector");
        assert_eq!(wide_results.len(), 4);
        assert!(
            wide_results
                .iter()
                .all(|ty| matches!(ty, wasmtime::ValType::I64))
        );
        assert_eq!(vector_results.len(), 4);
        assert!(
            vector_results
                .iter()
                .all(|ty| matches!(ty, wasmtime::ValType::I32))
        );
        let imports: Vec<_> = module
            .imports()
            .map(|import| (import.module().to_string(), import.name().to_string()))
            .collect();
        assert_eq!(
            imports,
            vec![
                ("volar-host".into(), "rng_wide".into()),
                ("volar-host".into(), "rng_vector".into()),
            ]
        );
    }

    #[test]
    fn packed_wide_and_vector_adds_execute_with_the_flat_wasm_abi() {
        let mut backend = WasmBackend::new();

        let (entry, params) = backend.begin_function(
            "add_u128",
            &[LirType::U128, LirType::U128],
            Some(LirType::U128),
        );
        backend.switch_to_block(entry);
        let sum = backend.add(params[0][0], params[1][0]);
        backend.ret(&[sum]);
        backend.end_function();

        let lanes = LirType::Vector(Box::new(LirType::U64), 2);
        let (entry, params) =
            backend.begin_function("add_lanes", &[lanes.clone(), lanes.clone()], Some(lanes));
        backend.switch_to_block(entry);
        let sum = backend.add(params[0][0], params[1][0]);
        backend.ret(&[sum]);
        backend.end_function();

        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, backend.finish()).unwrap();
        let mut store = wasmtime::Store::new(&engine, ());
        let instance = wasmtime::Instance::new(&mut store, &module, &[]).unwrap();
        let add_u128 = instance
            .get_typed_func::<(i64, i64, i64, i64), (i64, i64)>(&mut store, "add_u128")
            .unwrap();
        assert_eq!(add_u128.call(&mut store, (-1, 0, 1, 0)).unwrap(), (0, 1));
        let add_lanes = instance
            .get_typed_func::<(i64, i64, i64, i64), (i64, i64)>(&mut store, "add_lanes")
            .unwrap();
        assert_eq!(add_lanes.call(&mut store, (3, 11, 4, 9)).unwrap(), (7, 20));
    }

    #[test]
    fn high_volar_externals_use_flat_wasm_multivalue_results_and_direct_storage() {
        let mut types = IRTypes::new();
        let bit = types.bit();
        let wide = types.primitive(Type::_256);
        let word = types.primitive(Type::_32);
        let lanes = types.push(IRType::Vec(4, word));
        let pair = types.push(IRType::Tuple(vec![wide, lanes]));
        let address = types.primitive(Type::_64);

        let mut oracle_block = IRBlock {
            params: vec![wide],
            stmts: vec![],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, vec![IRVarId(2)]),
            },
        };
        oracle_block.push_stmt(
            IRStmt::OracleCall {
                name: "pair".into(),
                args: vec![IRVarId(0)],
                output_tys: vec![wide, lanes],
                result_ty: pair,
            },
            (),
        );
        oracle_block.push_stmt(
            IRStmt::OracleOutput {
                call: IRVarId(1),
                idx: 0,
                ty: wide,
            },
            (),
        );

        let mut action_block = IRBlock {
            params: vec![bit, wide, wide, address],
            stmts: vec![],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, vec![IRVarId(1)]),
            },
        };
        action_block.push_stmt(
            IRStmt::ActionStore {
                name: "commit".into(),
                guard: IRVarId(0),
                args: vec![IRVarId(1)],
                fallbacks: vec![IRVarId(2)],
                output_tys: vec![wide],
                targets: vec![ActionTarget {
                    storage: StorageId(7),
                    addr: IRVarId(3),
                }],
            },
            (),
        );

        let mut backend = WasmBackend::new().with_import_module("volar-host");
        lower_ir(
            &IRBlocks::new(vec![oracle_block]),
            &types,
            "oracle_wrapper",
            &mut backend,
        );
        lower_ir(
            &IRBlocks::new(vec![action_block]),
            &types,
            "action_wrapper",
            &mut backend,
        );
        let engine = wasmtime::Engine::default();
        let module = wasmtime::Module::new(&engine, backend.finish()).unwrap();
        let mut imports = module.imports();
        let oracle = imports.next().unwrap();
        assert_eq!(oracle.module(), "volar-host");
        assert_eq!(oracle.name(), "oracle_pair");
        let results = match oracle.ty() {
            wasmtime::ExternType::Func(function) => function.results().collect::<Vec<_>>(),
            other => panic!("oracle import is not a function: {other:?}"),
        };
        assert_eq!(results.len(), 8);
        assert!(
            results[..4]
                .iter()
                .all(|ty| matches!(ty, wasmtime::ValType::I64))
        );
        assert!(
            results[4..]
                .iter()
                .all(|ty| matches!(ty, wasmtime::ValType::I32))
        );
        let action = imports.next().unwrap();
        assert_eq!(action.module(), "volar-host");
        assert_eq!(action.name(), "action_commit");
        assert!(matches!(action.ty(), wasmtime::ExternType::Func(_)));
        assert!(imports.next().is_none());
    }
}
