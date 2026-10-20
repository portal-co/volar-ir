//! Concrete evaluator for VAFFLE `Module`.
//!
//! All values are represented as `Vec<bool>` (LSB-first bit vectors), the
//! same representation used by [`crate::interpreter::ir`].
//!
//! # Limitations
//! - `StackAlloc`, `PtrLoad`, `PtrStore`, `PtrOffset` **panic** — not generated.
//! - `ReturnCall` **panics** — not emitted by the generator.
//! - `Value::Call` and `Value::Output` are supported (single-level only).
//! - Oracle calls use the FNV-1a hash oracle (deterministic, pure).
//! - Action, RNG statements **panic** — not generated.

use std::collections::BTreeMap;

use vaffle::{FuncDecl, FuncId, Module, Terminator, Value, ValueId};
use volar_ir_common::{Constant, OracleDecl, PolyCoeffs, Stmt, TypeId, TypeTable};

use crate::generators::oracle::hash_oracle;
use crate::interpreter::ir::{
    IrValue, StorageMap, bit_width, bits_to_u64, const_to_bits, eval_typed_poly, rotate_left,
    rotate_right, transmute_bits,
};

// ============================================================================
// Public API
// ============================================================================

/// Maximum number of times the entry block may be re-entered before the
/// interpreter gives up.
pub const MAX_ITERS: usize = 512;
/// Total block-execution guard across a whole function call — a robust
/// infinite-loop bound. (The `MAX_ITERS` entry-block check alone cannot catch
/// a native loop whose header is a separate block.)
const MAX_TOTAL_ITERS: usize = 20_000_000;
/// Default call-depth guard for [`eval_vaffle`] (a fuzzing DoS bound).
pub const MAX_CALL_DEPTH: usize = 4;

/// Evaluate function `func_id` in `module` with the given inputs.
///
/// `inputs[i]` must have exactly `bit_width(entry_param_types[i])` bits.
///
/// Returns the values listed in the `Return` terminator, or `None` if the
/// iteration guard is exceeded.
pub fn eval_vaffle(module: &Module, func_id: FuncId, inputs: &[IrValue]) -> Option<Vec<IrValue>> {
    eval_vaffle_with_limits(module, func_id, inputs, MAX_CALL_DEPTH)
}

/// [`eval_vaffle`] with an explicit call-depth guard. Compiled (not fuzzed)
/// modules with legitimately deep static call chains — the spec WARP genesis
/// path nests `genesis → hash → sponge → permute` past the fuzzing default —
/// use this to raise the bound without changing the fuzzing default.
pub fn eval_vaffle_with_limits(
    module: &Module,
    func_id: FuncId,
    inputs: &[IrValue],
    max_call_depth: usize,
) -> Option<Vec<IrValue>> {
    let mut storage = BTreeMap::new();
    // Seed module-level `pre_init` segments (e.g. the shared stack pointer's
    // initial value) — the IR interpreter's `apply_pre_init` equivalent.
    for seg in &module.pre_init {
        let w = bit_width(seg.ty, &module.types);
        for (i, c) in seg.data.iter().enumerate() {
            let addr = (seg.offset + i) as u64;
            storage.insert((seg.storage, seg.ty, addr), const_to_bits(c, w));
        }
    }
    eval_vaffle_depth(module, func_id, inputs, &mut storage, 0, max_call_depth)
}

/// Internal recursive evaluator with a call-depth guard and shared storage.
fn eval_vaffle_depth(
    module: &Module,
    func_id: FuncId,
    inputs: &[IrValue],
    storage: &mut StorageMap,
    depth: usize,
    max_call_depth: usize,
) -> Option<Vec<IrValue>> {
    if depth > max_call_depth {
        return None;
    }

    let body = match &module.funcs[func_id.0] {
        FuncDecl::Body(b) => b,
        FuncDecl::Import { .. } => panic!("eval_vaffle: cannot evaluate an imported function"),
        _ => panic!("eval_vaffle: unhandled FuncDecl variant — add evaluation for this variant"),
    };

    let mut value_table: Vec<Option<IrValue>> = vec![None; body.values.len()];
    let mut value_tys: Vec<Option<TypeId>> = vec![None; body.values.len()];
    // Side-table for OracleCall aggregates: vid → Vec<IrValue>.
    let mut oracle_agg: BTreeMap<usize, Vec<IrValue>> = BTreeMap::new();
    // Side-table for Value::Call aggregates: vid → Vec<IrValue>.
    let mut call_agg: BTreeMap<usize, Vec<IrValue>> = BTreeMap::new();

    // Seed the entry block's params from `inputs`.
    let entry_block = &body.blocks[body.entry.0];
    if inputs.len() != entry_block.params.len() {
        let name = module
            .exports
            .iter()
            .find(|(_, f)| f.0 == func_id.0)
            .map(|(n, _)| n.clone())
            .unwrap_or_else(|| "?".to_string());
        eprintln!(
            "[eval] arity mismatch at func {name} (id {}): expects {} params, got {} inputs",
            func_id.0,
            entry_block.params.len(),
            inputs.len()
        );
    }
    assert_eq!(
        inputs.len(),
        entry_block.params.len(),
        "eval_vaffle: expected {} inputs, got {}",
        entry_block.params.len(),
        inputs.len(),
    );
    for ((vid, tid), val) in entry_block.params.iter().zip(inputs.iter()) {
        value_table[vid.0] = Some(val.clone());
        value_tys[vid.0] = Some(*tid);
    }

    let mut current_block_id = body.entry;
    let mut loop_count = 0usize;
    // Total block executions across the whole function — a robust infinite-loop
    // guard (the `loop_count`/`MAX_ITERS` check above only counts *entry*-block
    // re-entries, which a native loop with a separate header block never hits).
    let mut total_iters = 0usize;
    let throughput = std::env::var("VAFFLE_EVAL_TP").is_ok();
    let tp_start = std::time::Instant::now();
    let mut tp_count = 0usize;

    loop {
        total_iters += 1;
        if total_iters > MAX_TOTAL_ITERS {
            return None;
        }
        if current_block_id == body.entry {
            loop_count += 1;
            if loop_count > MAX_ITERS {
                return None;
            }
        }

        let block = &body.blocks[current_block_id.0];

        // Evaluate each stmt value in this block.
        for &vid in &block.stmts {
            let val = eval_vaffle_value(
                vid,
                &body.values,
                module,
                &mut value_table,
                &mut value_tys,
                &mut oracle_agg,
                &mut call_agg,
                storage,
                depth,
                max_call_depth,
            )?;
            value_table[vid.0] = Some(val);
            if let Value::Op(stmt) = &body.values[vid.0].kind
                && let Some(ty) = volar_ir_opt::common::stmt_output_type(stmt)
            {
                value_tys[vid.0] = Some(ty);
            }
            if throughput {
                tp_count += 1;
                if tp_count.is_multiple_of(5_000_000) {
                    let el = tp_start.elapsed().as_secs_f64();
                    eprintln!(
                        "[tp] func={} evals={}M rate={:.0}K/s depth={}",
                        func_id.0,
                        tp_count / 1_000_000,
                        tp_count as f64 / el / 1000.0,
                        depth
                    );
                }
            }
        }

        // Dispatch the terminator.
        match &block.terminator {
            Terminator::Return { values } => {
                return Some(
                    values
                        .iter()
                        .map(|vid| get_val(&value_table, *vid))
                        .collect(),
                );
            }
            Terminator::Jump(target) => {
                let args: Vec<IrValue> = target
                    .args
                    .iter()
                    .map(|vid| get_val(&value_table, *vid))
                    .collect();
                let next_block = &body.blocks[target.block.0];
                for ((param_vid, tid), val) in next_block.params.iter().zip(args.iter()) {
                    value_table[param_vid.0] = Some(val.clone());
                    value_tys[param_vid.0] = Some(*tid);
                }
                current_block_id = target.block;
            }
            Terminator::IfNonzero {
                cond,
                then_target,
                else_target,
            } => {
                let cond_val = get_val(&value_table, *cond);
                let target = if cond_val.first().copied().unwrap_or(false) {
                    then_target
                } else {
                    else_target
                };
                let args: Vec<IrValue> = target
                    .args
                    .iter()
                    .map(|vid| get_val(&value_table, *vid))
                    .collect();
                let next_block = &body.blocks[target.block.0];
                for ((param_vid, tid), val) in next_block.params.iter().zip(args.iter()) {
                    value_table[param_vid.0] = Some(val.clone());
                    value_tys[param_vid.0] = Some(*tid);
                }
                current_block_id = target.block;
            }
            Terminator::ReturnCall { .. } => panic!("eval_vaffle: ReturnCall not supported"),
            Terminator::Table {
                index,
                targets,
                default_target,
            } => {
                let index_val = get_val(&value_table, *index);
                let idx = bits_to_u64(&index_val) as usize;
                let target = if idx < targets.len() {
                    &targets[idx]
                } else {
                    default_target
                };
                let args: Vec<IrValue> = target
                    .args
                    .iter()
                    .map(|vid| get_val(&value_table, *vid))
                    .collect();
                let next_block = &body.blocks[target.block.0];
                for ((param_vid, tid), val) in next_block.params.iter().zip(args.iter()) {
                    value_table[param_vid.0] = Some(val.clone());
                    value_tys[param_vid.0] = Some(*tid);
                }
                current_block_id = target.block;
            }
            _ => panic!(
                "eval_vaffle: unhandled Terminator variant — add evaluation for this variant"
            ),
        }
    }
}

// ============================================================================
// Value evaluation
// ============================================================================

#[allow(clippy::too_many_arguments)]
fn eval_vaffle_value(
    vid: ValueId,
    values: &[volar_ir_common::Node<Value>],
    module: &Module,
    value_table: &mut [Option<IrValue>],
    value_tys: &mut [Option<TypeId>],
    oracle_agg: &mut BTreeMap<usize, Vec<IrValue>>,
    call_agg: &mut BTreeMap<usize, Vec<IrValue>>,
    storage: &mut StorageMap,
    depth: usize,
    max_call_depth: usize,
) -> Option<IrValue> {
    let val = match &values[vid.0].kind {
        Value::Param { ty, .. } => {
            value_tys[vid.0] = Some(*ty);
            get_val(value_table, vid)
        }
        Value::Op(stmt) => eval_vaffle_stmt(
            stmt,
            vid.0,
            &module.types,
            &module.oracles,
            value_table,
            value_tys,
            oracle_agg,
            storage,
        ),
        Value::Call { func, args } => {
            // Collect argument values.
            let arg_vals: Vec<IrValue> = args.iter().map(|a| get_val(value_table, *a)).collect();
            // Recursive eval with depth guard.
            let results =
                eval_vaffle_depth(module, *func, &arg_vals, storage, depth + 1, max_call_depth)?;
            call_agg.insert(vid.0, results);
            vec![] // projected by Value::Output
        }
        Value::Output { value, idx } => {
            // Record the projection's type from the callee's signature —
            // `stmt_output_type` only sees `Value::Op`, and a `Poly` factor
            // referencing an unprojected call output must still resolve.
            let kind = &values[value.0].kind;
            if let Value::Call { func, .. } = kind
                && let vaffle::FuncDecl::Body(body) = &module.funcs[func.0]
            {
                let sig = &module.sigs[body.sig.0];
                if let Some(ty) = sig.results.get(*idx) {
                    value_tys[vid.0] = Some(*ty);
                }
            }
            call_agg
                .get(&value.0)
                .and_then(|agg| agg.get(*idx))
                .cloned()
                .unwrap_or_default()
        }
        Value::StackAlloc {
            elem_ty,
            count,
            storage: _,
            sp,
        } => {
            // The result is the site's virtual stack pointer (pre-bump),
            // matching `lower_vaffle_to_ir`'s explicit read-add-write (see
            // the vaffle `Value::StackAlloc` doc). The VAFFLE level has no
            // bump stmts, so the interpreter applies the bump itself.
            let addr_ty = module
                .pre_init
                .iter()
                .find(|seg| seg.storage == *sp)
                .map(|seg| seg.ty)
                .expect("StackAlloc: sp site must have a pre_init segment");
            let ptr_bits = module.pointer_width.bits();
            let key = (*sp, addr_ty, 0u64);
            let base = storage
                .get(&key)
                .cloned()
                .unwrap_or_else(|| vec![false; ptr_bits]);
            let elem_bits = bit_width(*elem_ty, &module.types);
            let bump = (*count * elem_bits) as u64;
            let bumped = bits_to_u64(&base).wrapping_add(bump);
            let mut bumped_bits = Vec::with_capacity(ptr_bits);
            for i in 0..ptr_bits {
                bumped_bits.push((bumped >> i) & 1 == 1);
            }
            storage.insert(key, bumped_bits);
            let mut result = base;
            result.resize(ptr_bits, false);
            result
        }
        // Bookkeeping markers: the real memory effects are the
        // `Stmt::StorageRead`/`Stmt::StorageWrite` values the target emits
        // around them; the markers' own outputs are unused.
        Value::PtrLoad { .. } => vec![],
        Value::PtrStore { .. } => vec![],
        Value::PtrOffset { .. } => vec![],
        _ => panic!("eval_vaffle_value: unhandled Value variant — add evaluation for this variant"),
    };
    Some(val)
}

fn eval_vaffle_stmt(
    stmt: &Stmt<ValueId>,
    stmt_vid: usize,
    types: &TypeTable,
    _oracles: &[OracleDecl],
    value_table: &mut [Option<IrValue>],
    value_tys: &[Option<TypeId>],
    oracle_agg: &mut BTreeMap<usize, Vec<IrValue>>,
    storage: &mut StorageMap,
) -> IrValue {
    let get = |vid: &ValueId| -> IrValue { get_val(value_table, *vid) };

    match stmt {
        Stmt::Const(c, ty) => {
            let w = bit_width(*ty, types);
            const_to_bits(c, w)
        }
        Stmt::Transmute { src, dst_ty, .. } => {
            let src_val = get(src);
            let dst_w = bit_width(*dst_ty, types);
            transmute_bits(&src_val, dst_w)
        }
        Stmt::Poly {
            ty,
            coeffs,
            constant,
        } => {
            // Prime fields multiply modulo p, not by AND — mirror the IR
            // interpreter's routing (`eval_prime_poly`); without this arm a
            // prime-field Poly fell into `eval_typed_poly` →
            // `monomial_product`, which fails closed on prime fields.
            if volar_ir_common::contains_prime_field(*ty, types) {
                let width = bit_width(*ty, types);
                let constant_bits = const_to_bits(constant, width);
                let mut terms = Vec::new();
                for (mono, coeff) in coeffs.iter() {
                    let mut factors = Vec::with_capacity(mono.len());
                    for var in mono {
                        let factor_ty = value_tys[var.0].unwrap_or_else(|| {
                            panic!("eval_vaffle: factor v{} has no type", var.0)
                        });
                        factors.push((factor_ty, get(var)));
                    }
                    terms.push((factors, *coeff));
                }
                let mut sink = volar_ir_common::BoolRing;
                volar_ir_common::eval_prime_poly(*ty, &constant_bits, &terms, types, &mut sink)
            } else if volar_ir_common::mul_is_idempotent(*ty, types) {
                let width = bit_width(*ty, types);
                eval_vaffle_poly(coeffs, constant, width, value_table)
            } else {
                eval_typed_poly(*ty, coeffs, constant, types, |var| {
                    let factor_ty = value_tys[var.0]
                        .unwrap_or_else(|| panic!("eval_vaffle: factor v{} has no type", var.0));
                    (factor_ty, get(var))
                })
            }
        }
        Stmt::Rol { src, ty, n } => {
            let val = get(src);
            let w = bit_width(*ty, types);
            rotate_left(&val, w, *n)
        }
        Stmt::Ror { src, ty, n } => {
            let val = get(src);
            let w = bit_width(*ty, types);
            rotate_right(&val, w, *n)
        }
        Stmt::Merge { parts, ty } => {
            let w = bit_width(*ty, types);
            let mut result = Vec::with_capacity(w);
            for part in parts {
                result.extend_from_slice(&get(part));
            }
            result.truncate(w);
            while result.len() < w {
                result.push(false);
            }
            result
        }
        Stmt::Splat { src, ty } => {
            let bit = get(src).first().copied().unwrap_or(false);
            let w = bit_width(*ty, types);
            vec![bit; w]
        }
        Stmt::Shuffle { result_bits, ty } => {
            let w = bit_width(*ty, types);
            let mut result = vec![false; w];
            for (i, (bit_idx, var)) in result_bits.iter().enumerate() {
                if i < w {
                    let val = get(var);
                    result[i] = val.get(*bit_idx as usize).copied().unwrap_or(false);
                }
            }
            result
        }
        Stmt::OracleCall {
            name,
            args,
            output_tys,
            ..
        } => {
            // Seed the FNV hash oracle from the *name bytes* — the same
            // derivation `eval_biir` uses on lowered Boolar output, where no
            // oracle table is available. Keeps all three interpreters
            // (IR / vaffle / Boolar) consistent on uninterpreted calls.
            let oracle_idx: u32 = name
                .bytes()
                .fold(0u32, |h, b| h.wrapping_mul(31).wrapping_add(b as u32));
            let flat_inputs: Vec<bool> = args.iter().flat_map(&get).collect();
            let output_widths: Vec<usize> =
                output_tys.iter().map(|&ty| bit_width(ty, types)).collect();
            let total_w: usize = output_widths.iter().sum();
            let flat_out = hash_oracle(oracle_idx, &flat_inputs, total_w);
            let mut outputs = Vec::with_capacity(output_tys.len());
            let mut offset = 0;
            for &w in &output_widths {
                outputs.push(flat_out[offset..offset + w].to_vec());
                offset += w;
            }
            oracle_agg.insert(stmt_vid, outputs);
            vec![]
        }
        Stmt::OracleOutput { call, idx, ty } => {
            let w = bit_width(*ty, types);
            oracle_agg
                .get(&call.0)
                .and_then(|agg| agg.get(*idx))
                .cloned()
                .unwrap_or_else(|| vec![false; w])
        }
        Stmt::ActionCall { .. } => panic!("eval_vaffle: ActionCall not supported"),
        Stmt::ActionOutput { .. } => panic!("eval_vaffle: ActionOutput not supported"),
        Stmt::Rng { .. } => panic!("eval_vaffle: Rng not supported"),
        Stmt::StorageRead {
            storage: sid,
            ty,
            addr,
        } => {
            let w = bit_width(*ty, types);
            let addr_u64 = bits_to_u64(&get(addr));
            storage
                .get(&(*sid, *ty, addr_u64))
                .cloned()
                .unwrap_or_else(|| vec![false; w])
        }
        Stmt::StorageWrite {
            storage: sid,
            src,
            ty,
            addr,
        } => {
            let src_val = get(src);
            let addr_u64 = bits_to_u64(&get(addr));
            let w = bit_width(*ty, types);
            let mut val = src_val;
            val.truncate(w);
            while val.len() < w {
                val.push(false);
            }
            storage.insert((*sid, *ty, addr_u64), val);
            vec![] // StorageWrite has no output
        }
        _ => panic!("eval_vaffle_stmt: unhandled Stmt variant — add evaluation for this variant"),
    }
}

// ============================================================================
// Polynomial evaluation (VAFFLE variant with ValueId keys)
// ============================================================================

fn eval_vaffle_poly(
    coeffs: &PolyCoeffs<ValueId>,
    constant: &Constant,
    width: usize,
    value_table: &[Option<IrValue>],
) -> IrValue {
    let mut result = vec![false; width];
    for k in 0..width {
        let const_bit = if k < 128 {
            ((constant.lo >> k) & 1) != 0
        } else {
            ((constant.hi >> (k - 128)) & 1) != 0
        };
        let mut acc = const_bit;
        for (monomial, coeff) in coeffs {
            if coeff & 1 == 0 {
                continue;
            }
            // Read the single bit in place — `get_val` would clone the whole
            // factor vector just to read one bit (the dominant interpreter
            // cost over a large Boolean circuit).
            let product = monomial.iter().all(|var| {
                value_table[var.0]
                    .as_ref()
                    .and_then(|v| v.get(k))
                    .copied()
                    .unwrap_or(false)
            });
            acc ^= product;
        }
        result[k] = acc;
    }
    result
}

// ============================================================================
// Utilities
// ============================================================================

fn get_val(value_table: &[Option<IrValue>], vid: ValueId) -> IrValue {
    value_table[vid.0]
        .clone()
        .unwrap_or_else(|| panic!("eval_vaffle: value {} not in table", vid.0))
}
