// @reliability: experimental
// @ai: assisted
//! VAFFLE `Module` → Volar IR `IRBlocks` lowering with a Dyn-based recursion
//! stack.
//!
//! # Bit packing
//!
//! Individual GF(2) bits are packed into `Vec(PACK_W, Bit)` words (default
//! `PACK_W = 64`) at every boundary:
//!
//! * **Block parameters**: `ceil(pointer_bits / PACK_W)` packed words instead
//!   of `pointer_bits` individual `Bit` params.
//! * **Spill / reload**: `ceil(N / PACK_W)` `StorageWrite` / `StorageRead`
//!   ops instead of `N`.
//! * **Frame arguments / return**: packed word slots.
//! * **Jump / Dyn arguments**: packed SP + values.
//!
//! Packing is performed via `Merge` (pack) and `Shuffle` (unpack).
//! Computation within a block operates on individual `Bit`-typed vars as
//! before — only boundary crossings use packed words.
//!
//! # Storage model
//!
//! Storages are keyed by `(StorageId, TypeId, addr)`.  The `Block`-typed
//! continuation lives in its own type lane at the same address as the
//! `Bit`-typed parameters — zero extra frame space.
//!
//! # Call protocol
//!
//! **Caller** at a `Value::Call` site:
//! 1. Spill all values defined so far to own frame's spill slots.
//! 2. Write callee's arguments into `STACK[sp + param_off]` (Bit lane).
//! 3. Write continuation block ref into `STACK[sp]` (Block lane).
//! 4. Advance SP by callee frame size.
//! 5. Jump to callee's entry block with new SP bits.
//!
//! **Callee** entry:
//! 1. Read arguments from `STACK[sp - frame_size + param_off]` (Bit lane).
//! 2. Execute body.
//!
//! **Callee** return:
//! 1. Read continuation from `STACK[sp - frame_size]` (Block lane).
//! 2. Retreat SP by frame size.
//! 3. `Dyn(continuation)` with args `[sp_bits…, ret_bits…]`.
//!
//! **Continuation** block (in caller):
//! 1. Receives `[sp_bits…, ret_bits…]` as block params.
//! 2. Reloads spilled values from own frame's spill slots.
//! 3. Continues with remaining stmts.

use alloc::{
    collections::{BTreeMap, BTreeSet},
    vec,
    vec::Vec,
};

use vaffle::{
    Block, BlockId, FuncBody, FuncDecl, FuncId, Module, SigDecl, Target, Terminator, Value, ValueId,
};
use volar_ir::ir::{
    ActionDecl, IRBlock, IRBlockId, IRBlockTargetId, IRBlocks, IRBranchTarget, IRStmt,
    IRTerminator, IRTypeId, IRTypes, IRVarId, OracleDecl,
};
use volar_ir_common::{Constant, IrType, Stmt, StorageId, Type, TypeId};
use volar_lir::circuits::{
    bc_add, frame_read_cont, frame_reload, frame_spill, frame_write_cont, frame_write_ret, n_packs,
    pack_bits, unpack_words, BitCircuitBuilder, FrameLayout, StackPtr, StorageEmitter, PACK_W,
};

/// Number of bits packed into a single `Vec(PACK_W, Bit)` word at block
/// boundaries, spill/reload slots, and frame argument slots.
///
/// Choosing 64 means that a 128-bit parameter occupies 2 packed block
/// params instead of 128 individual `Bit` params, and spilling 200
/// values costs 4 storage ops instead of 200.
// Re-exported from volar_lir::circuits::PACK_W.

// ============================================================================
// Public entry point
// ============================================================================

/// Lower a VAFFLE [`Module`] into a single [`IRBlocks`] with a Dyn-based
/// recursion stack.
///
/// Every VAFFLE function becomes a contiguous range of IR blocks (possibly
/// expanded by call-site splitting).  The first function in the module is
/// treated as the entry point.
pub fn lower_vaffle_to_ir<P: Clone>(module: &Module<P>) -> (IRBlocks<P>, IRTypes) {
    let ssa_module = crate::vaffle_ssa::ssa_ify_module(module);
    let mut ctx = LowerCtx::new(&ssa_module);
    ctx.lower_all();
    ctx.finish()
}

/// Consuming counterpart to [`lower_vaffle_to_ir`].
///
/// Builder pipelines own their VAFFLE module at this boundary, so this route
/// transfers its bodies into the SSA rewrite rather than cloning the complete
/// input before lower-to-IR begins.
pub fn lower_vaffle_to_ir_owned<P: Clone>(module: Module<P>) -> (IRBlocks<P>, IRTypes) {
    let ssa_module = crate::vaffle_ssa::ssa_ify_module_owned(module);
    let mut ctx = LowerCtx::new(&ssa_module);
    ctx.lower_all();
    ctx.finish()
}

/// Lower a VAFFLE module that may contain a statement-free entry body.
///
/// `control_prov` must be the existing frontend/control provenance for the
/// enclosing module. It is used only for lowering infrastructure when no
/// source value can supply a provenance; this lowering never invents one.
pub fn lower_vaffle_to_ir_with_control_provenance<P: Clone>(
    module: &Module<P>,
    control_prov: &P,
) -> (IRBlocks<P>, IRTypes) {
    let ssa_module = crate::vaffle_ssa::ssa_ify_module(module);
    let mut ctx = LowerCtx::new_with_control_provenance(&ssa_module, control_prov.clone());
    ctx.lower_all();
    ctx.finish()
}

/// Like [`lower_vaffle_to_ir`], but first runs `volar_ir_opt::inline_vaffle`
/// on the module, splicing eligible calls directly into their call sites so
/// fewer calls reach this file's on-stack call convention (see the module
/// doc's "Call protocol" section). Takes `module` by value (rather than
/// `&Module<P>` like its siblings) because inlining mutates in place and
/// [`Module`] does not implement `Clone`.
pub fn lower_vaffle_to_ir_with_inlining<P: Clone>(
    mut module: Module<P>,
    budget: volar_ir_opt::inline_vaffle::InlineBudget,
) -> (IRBlocks<P>, IRTypes) {
    volar_ir_opt::inline_vaffle::inline_vaffle_module(&mut module, budget);
    lower_vaffle_to_ir_owned(module)
}

/// Like [`lower_vaffle_to_ir_with_inlining`], but runs
/// [`volar_ir_opt::inline_vaffle::inline_vaffle_everything`] so every
/// non-recursive intra-module call (including tail calls) is spliced before
/// IR lowering. Fails closed on recursion or leftover Body-to-Body calls.
pub fn lower_vaffle_to_ir_fully_inlined<P: Clone>(
    mut module: Module<P>,
    entries: &[vaffle::FuncId],
) -> Result<(IRBlocks<P>, IRTypes), volar_ir_opt::inline_vaffle::InlineEverythingError> {
    volar_ir_opt::inline_vaffle::inline_vaffle_everything(&mut module, entries)?;
    Ok(lower_vaffle_to_ir_owned(module))
}

/// Temporary diagnostic variant of [`lower_vaffle_to_ir`] that also returns
/// every cross-block spill/reload's own `(vaffle_block, vid, address)` log
/// line -- lets a caller cross-reference write vs. read sites for a given
/// `vid` without re-running the (slow) circuit simulator. Not used by any
/// real pipeline.
pub fn lower_vaffle_to_ir_with_spill_trace<P: Clone>(
    module: &Module<P>,
) -> (IRBlocks<P>, IRTypes, alloc::vec::Vec<alloc::string::String>) {
    let ssa_module = crate::vaffle_ssa::ssa_ify_module(module);
    let mut ctx = LowerCtx::new(&ssa_module);
    ctx.lower_all();
    let trace = ctx.spill_trace.clone();
    let (blocks, types) = ctx.finish();
    (blocks, types, trace)
}

// ============================================================================
// Well-known type IDs (indices into the type table)
// ============================================================================

const BIT_TID: TypeId = TypeId(0);
const ADDR_TID: TypeId = TypeId(1); // Vec(module.pointer_width, Bit)
/// `Vec(PACK_W, Bit)` — the packed word type.  Index 2 in the type table.
const PACK_TID: TypeId = TypeId(2);

// ============================================================================
// BlockEmitter — implements BitCircuitBuilder + StorageEmitter
// ============================================================================

struct BlockEmitter<P: Clone = ()> {
    params: Vec<IRTypeId>,
    stmts: Vec<volar_ir_common::Node<IRStmt, P>>,
    current_prov: Option<P>,
    next_var: u32,
}

impl<P: Clone> BlockEmitter<P> {
    fn new(params: Vec<IRTypeId>) -> Self {
        let next_var = params.len() as u32;
        BlockEmitter {
            params,
            stmts: Vec::new(),
            current_prov: None,
            next_var,
        }
    }
    fn set_prov(&mut self, prov: P) {
        self.current_prov = Some(prov);
    }
    fn emit(&mut self, stmt: IRStmt) -> IRVarId {
        let id = IRVarId(self.next_var);
        self.next_var += 1;
        let prov = self.current_prov.clone()
            .expect("BlockEmitter::emit called before set_prov — every emitted stmt must trace back to a source value's provenance");
        self.stmts
            .push(volar_ir_common::Node::new(stmt, prov, None));
        id
    }
    fn finish(self, terminator: IRTerminator) -> IRBlock<P> {
        IRBlock {
            params: self.params,
            stmts: self.stmts,
            terminator,
        }
    }

    /// Number of packed words needed for `n` bits.
    fn n_packs(n: usize) -> usize {
        volar_lir::n_packs(n)
    }
}

impl<P: Clone> BitCircuitBuilder for BlockEmitter<P> {
    type Bit = IRVarId;
    fn bc_const(&mut self, val: bool) -> IRVarId {
        self.emit(IRStmt::Const(
            Constant {
                hi: 0,
                lo: val as u128,
            },
            BIT_TID,
        ))
    }
    fn bc_poly(&mut self, coeffs: BTreeMap<Vec<IRVarId>, u8>, constant: u128) -> IRVarId {
        self.emit(IRStmt::Poly {
            ty: BIT_TID,
            coeffs,
            constant: Constant {
                hi: 0,
                lo: constant,
            },
        })
    }
    fn bc_carry3(&mut self, a: IRVarId, b: IRVarId, c: IRVarId) -> IRVarId {
        let mut ab = vec![a, b];
        ab.sort();
        let mut ac = vec![a, c];
        ac.sort();
        let mut bc_ = vec![b, c];
        bc_.sort();
        let mut coeffs = BTreeMap::new();
        coeffs.insert(ab, 1u8);
        coeffs.insert(ac, 1u8);
        coeffs.insert(bc_, 1u8);
        self.bc_poly(coeffs, 0)
    }
}

impl<P: Clone> StorageEmitter for BlockEmitter<P> {
    fn compose_address(&mut self, bits: &[IRVarId]) -> IRVarId {
        if bits.len() == 1 {
            return bits[0];
        }
        self.emit(IRStmt::Merge {
            parts: bits.to_vec(),
            ty: ADDR_TID,
        })
    }
    fn compose_pack(&mut self, bits: &[IRVarId]) -> IRVarId {
        if bits.len() == 1 {
            return bits[0];
        }
        self.emit(IRStmt::Merge {
            parts: bits.to_vec(),
            ty: PACK_TID,
        })
    }
    fn extract_bit(&mut self, word: IRVarId, idx: u8) -> IRVarId {
        self.emit(IRStmt::Shuffle {
            result_bits: vec![(idx, word)],
            ty: BIT_TID,
        })
    }
    fn emit_read(&mut self, storage: StorageId, ty: TypeId, addr_bits: &[IRVarId]) -> IRVarId {
        let addr = self.compose_address(addr_bits);
        self.emit(IRStmt::StorageRead { storage, ty, addr })
    }
    fn emit_write(&mut self, storage: StorageId, src: IRVarId, ty: TypeId, addr_bits: &[IRVarId]) {
        let addr = self.compose_address(addr_bits);
        self.emit(IRStmt::StorageWrite {
            storage,
            src,
            ty,
            addr,
        });
    }
}

// ============================================================================
// Type-table remapping: VAFFLE TypeId → IR TypeId
// ============================================================================

/// Recursively intern VAFFLE type `vtid` into `ir_types`, returning the
/// corresponding IR `TypeId`.  Uses `map`/`done` for memoization so each
/// entry is processed at most once (handles shared structure and avoids
/// infinite loops for any forward-declared but well-formed type graph).
fn remap_type_id(
    vtid: TypeId,
    vaffle_types: &volar_ir_common::TypeTable,
    ir_types: &mut IRTypes,
    map: &mut Vec<TypeId>,
    done: &mut Vec<bool>,
) -> TypeId {
    if done[vtid.0 as usize] {
        return map[vtid.0 as usize];
    }
    // Mark before recursing to break cycles (result is a placeholder until we
    // overwrite below — cycles in IrType are not valid, so this is safe).
    done[vtid.0 as usize] = true;
    let vty = vaffle_types.0[vtid.0 as usize].clone();
    let ity = match vty {
        IrType::Primitive(p) => IrType::Primitive(p),
        IrType::Vec(k, inner) => {
            let inner_ir = remap_type_id(inner, vaffle_types, ir_types, map, done);
            IrType::Vec(k, inner_ir)
        }
        IrType::Tuple(parts) => {
            let parts_ir: Vec<TypeId> = parts
                .iter()
                .map(|&p| remap_type_id(p, vaffle_types, ir_types, map, done))
                .collect();
            IrType::Tuple(parts_ir)
        }
        IrType::Block { params } => {
            let params_ir: Vec<TypeId> = params
                .iter()
                .map(|&p| remap_type_id(p, vaffle_types, ir_types, map, done))
                .collect();
            IrType::Block { params: params_ir }
        }
        IrType::Func { params, results } => {
            let params_ir: Vec<TypeId> = params
                .iter()
                .map(|&p| remap_type_id(p, vaffle_types, ir_types, map, done))
                .collect();
            let results_ir: Vec<TypeId> = results
                .iter()
                .map(|&r| remap_type_id(r, vaffle_types, ir_types, map, done))
                .collect();
            IrType::Func {
                params: params_ir,
                results: results_ir,
            }
        }
        _ => panic!("remap_type_id: unhandled IrType variant — add type mapping for this variant"),
    };
    let ir_tid = ir_types.intern(ity);
    map[vtid.0 as usize] = ir_tid;
    ir_tid
}

// ============================================================================
// LowerCtx
// ============================================================================

struct FuncInfo {
    /// Index of the function's entry block in the global blocks array.
    /// Declaration-only imports have no body and therefore no entry block.
    entry_block: Option<usize>,
    /// Reserved abort sink for a declaration-only import. It accepts the
    /// import's packed call arguments and returns zero-valued entry results,
    /// rather than letting a call jump to an unreserved block index.
    abort_block: Option<usize>,
    /// Frame layout for calls *into* this function.
    callee_layout: FrameLayout,
    /// Frame layout of this function's own frame (for spilling).
    own_layout: FrameLayout,
    /// Number of packed words carrying parameters as entry-block params.
    /// Callers append these after the SP words in the jump args.
    n_param_words: usize,
    /// Number of individual parameter bits in the function signature.
    n_params: usize,
    /// Total bit-width of all return values (sum of bit-widths of sig.results).
    total_ret_bits: usize,
    /// VAFFLE `ValueId.0` indices that need spilling/reloading across block
    /// boundaries (see [`compute_cross_block_values`]) -- disjoint from
    /// `own_layout`'s own packed-word call-spill region: individually
    /// `Bit`-typed slots at `cross_block_base + ValueId.0`.
    cross_block_values: BTreeSet<usize>,
    cross_block_base: u64,
    /// Total `StorageId::STACK` bit-budget this function's own `alloca`s
    /// need (see `compute_alloca_budget`), zero if it has none. Distinct
    /// from `own_layout.size` (the calling-convention's own register
    /// region): a caller advancing SP to make a nested call must skip past
    /// *both* its own `own_layout.size` *and* this budget, or the callee's
    /// frame would overlap this function's still-live alloca storage.
    alloca_budget: u64,
}

pub(crate) struct LowerCtx<'m, P: Clone = ()> {
    module: &'m Module<P>,
    pointer_bits: usize,
    types: IRTypes,
    /// Maps VAFFLE TypeId → IR TypeId (index = VAFFLE TypeId.0).
    type_map: Vec<TypeId>,
    func_info: Vec<FuncInfo>,
    blocks: Vec<IRBlock<P>>,
    oracles: Vec<OracleDecl>,
    actions: Vec<ActionDecl>,
    /// Extra blocks generated by call-site splitting (appended after all
    /// function blocks).  Each entry is `(block_def)`.
    extra_blocks: Vec<IRBlock<P>>,
    /// Total block count reserved by `plan_functions` (the module entry and
    /// exit blocks, plus every function's own `body.blocks.len()`) -- the
    /// final value of its own `block_offset` counter. A call site computing
    /// where its continuation will land (`cont_block_idx`, see
    /// `lower_function`) must use *this*, not `self.blocks.len()`:
    /// functions are lowered in sequence, so `self.blocks.len()` only
    /// reflects progress consumed by functions processed *so far* -- a
    /// call from an earlier function can't see that a later function's own
    /// reserved entry-block slot still sits unfilled ahead of it.
    total_blocks: usize,
    pre_init: alloc::vec::Vec<volar_ir_common::PreInitSegment>,
    /// Temporary diagnostic: logs every cross-block spill/reload's own
    /// `(vaffle_block, vid, address)`, for tracing write-vs-read mismatches
    /// without re-running the (slow) circuit simulator. Not read by any
    /// real pipeline; drained via `lower_vaffle_to_ir_with_spill_trace`.
    spill_trace: Vec<alloc::string::String>,
    /// Explicit frontend/control provenance for infrastructure in a
    /// statement-free function. It is supplied by the caller, never invented.
    control_prov: Option<P>,
}

impl<'m, P: Clone> LowerCtx<'m, P> {
    pub(crate) fn new(module: &'m Module<P>) -> Self {
        let pointer_bits = module.pointer_width.bits();
        let mut types = IRTypes::new();
        types.push(IrType::Primitive(Type::Bit)); // index 0 = BIT_TID
        types.push(IrType::Vec(pointer_bits, BIT_TID)); // index 1 = ADDR_TID
        types.push(IrType::Vec(PACK_W, BIT_TID)); // index 2 = PACK_TID

        // Build a mapping from VAFFLE TypeId → IR TypeId by interning each
        // VAFFLE type into the IR type table (recursively remapping inner refs).
        let n = module.types.0.len();
        let mut type_map = alloc::vec![TypeId(0); n];
        let mut done = alloc::vec![false; n];
        for i in 0..n {
            remap_type_id(
                TypeId(i as u32),
                &module.types,
                &mut types,
                &mut type_map,
                &mut done,
            );
        }

        // Remap TypeIds in pre_init segments to be valid in the IR type table.
        let pre_init = module
            .pre_init
            .iter()
            .map(|seg| volar_ir_common::PreInitSegment {
                storage: seg.storage,
                ty: type_map[seg.ty.0 as usize],
                offset: seg.offset,
                data: seg.data.clone(),
            })
            .collect();

        LowerCtx {
            module,
            pointer_bits,
            types,
            type_map,
            func_info: Vec::new(),
            blocks: Vec::new(),
            oracles: module.oracles.clone(),
            actions: module.actions.clone(),
            extra_blocks: Vec::new(),
            total_blocks: 0,
            pre_init,
            spill_trace: Vec::new(),
            control_prov: None,
        }
    }

    fn new_with_control_provenance(module: &'m Module<P>, control_prov: P) -> Self {
        let mut ctx = Self::new(module);
        ctx.control_prov = Some(control_prov);
        ctx
    }

    /// Intern a Block type for a continuation that receives packed
    /// `[sp_words…, ret_words…]`.
    fn intern_cont_block_type(&mut self, n_ret_bits: usize) -> TypeId {
        let sp_words = n_packs(self.pointer_bits);
        let ret_words = n_packs(n_ret_bits);
        let params = vec![PACK_TID; sp_words + ret_words];
        self.types.intern(IrType::Block { params })
    }

    fn import_func_info(&mut self, sig: &SigDecl, abort_block: usize) -> FuncInfo {
        let n_params: usize = sig
            .params
            .iter()
            .map(|&vtid| ir_type_bit_width(&self.types, self.type_map[vtid.0 as usize]))
            .sum();
        let total_ret_bits: usize = sig
            .results
            .iter()
            .map(|&vtid| ir_type_bit_width(&self.types, self.type_map[vtid.0 as usize]))
            .sum();
        let n_param_words = n_packs(n_params);
        let n_ret_words = n_packs(total_ret_bits);
        let ret = (total_ret_bits > 0).then_some((0, n_ret_words as u64, PACK_TID));
        let cont_ty = Some(self.intern_cont_block_type(total_ret_bits));
        let callee_layout = FrameLayout {
            params: vec![],
            ret,
            cont_ty,
            spill_base: n_ret_words as u64,
            n_spill: 0,
            size: n_ret_words as u64,
            storage: StorageId::STACK,
        };

        FuncInfo {
            entry_block: None,
            abort_block: Some(abort_block),
            callee_layout: callee_layout.clone(),
            own_layout: callee_layout,
            n_param_words,
            n_params,
            total_ret_bits,
            cross_block_values: BTreeSet::new(),
            cross_block_base: 0,
            alloca_budget: 0,
        }
    }

    // ---- Planning ----------------------------------------------------------

    pub(crate) fn plan_functions(&mut self) {
        // Reserve block 0 for the module entry; block 1 for the exit continuation.
        let mut block_offset = 2usize;

        for func_decl in &self.module.funcs {
            let body = match func_decl {
                FuncDecl::Body(b) => b,
                FuncDecl::Import { sig, .. } => {
                    let import_info = self.import_func_info(&self.module.sigs[sig.0], block_offset);
                    self.func_info.push(import_info);
                    block_offset += 1;
                    continue;
                }
                _ => continue,
            };

            let sig = &self.module.sigs[body.sig.0];
            // Total bit-width of all *entry-block* params -- deliberately
            // NOT `sig.params` (a producer is free to make the entry
            // block's own param list wider than the function's declared
            // signature; `vaffle_ssa::ssa_ify_function` does exactly this,
            // threading an extra `SPILL_ADDR_BITS`-wide SP param onto every
            // non-entry function's entry block -- see its own module doc
            // -- without touching `module.sigs`, since that SP is a
            // lowering-internal detail, not a real logical parameter).
            // `lower_vaffle_to_ir` always runs `ssa_ify_module` first, and
            // `wire_call_sites` appends that same SP's bits to every
            // `Value::Call`/`Terminator::ReturnCall`'s own `args` -- so the
            // call site's packed arg-word count and the callee's own
            // declared entry-param-word count must agree on *this* wider
            // shape, or the callee's entry block receives the wrong number
            // of packed words (confirmed: computing this from `sig.params`
            // instead reproducibly desyncs by exactly `SPILL_ADDR_BITS`
            // bits for any non-entry function, i.e. any function reachable
            // via an internal call). For the entry function itself (no SP
            // threading, see `ssa_ify_function`'s "Phase 1" comment) the
            // entry block's own params are exactly the logical params, so
            // this agrees with `sig.params` there too -- reading from the
            // block is strictly more correct, never different when the two
            // could otherwise agree.
            let n_params: usize = body.blocks[body.entry.0]
                .params
                .iter()
                .map(|&(_vid, vtid)| {
                    let ir_tid = self.type_map[vtid.0 as usize];
                    ir_type_bit_width(&self.types, ir_tid)
                })
                .sum();
            let n_values = body.values.len();

            // Total bit-width of all return values (supports multi-bit types).
            let total_ret_bits: usize = sig
                .results
                .iter()
                .map(|&vtid| {
                    let ir_tid = self.type_map[vtid.0 as usize];
                    ir_type_bit_width(&self.types, ir_tid)
                })
                .sum();

            // Callee layout: params are now passed as entry-block params (not
            // written to the frame), so the frame only needs ret + cont slots.
            let n_param_words = n_packs(n_params);
            let n_ret_words = n_packs(total_ret_bits);

            let mut offset = 0u64;
            let ret = if total_ret_bits > 0 {
                let o = offset;
                offset += n_ret_words as u64;
                Some((o, n_ret_words as u64, PACK_TID))
            } else {
                None
            };
            let cont_ty = Some(self.intern_cont_block_type(total_ret_bits));
            let callee_size = offset;

            let callee_layout = FrameLayout {
                params: vec![],
                ret,
                cont_ty,
                spill_base: 0,
                n_spill: 0,
                size: callee_size,
                storage: StorageId::STACK,
            };

            // Own layout: this function's call-spill slots (packed words).
            let n_spill_words = n_packs(n_values) as u64;
            let spill_base = callee_size;

            // Cross-block value slots: a genuinely separate, individually
            // `Bit`-typed region placed right after the packed call-spill
            // region (see `compute_cross_block_values`'s own doc comment).
            let cross_block_values = compute_cross_block_values(body);
            let cross_block_base = spill_base + n_spill_words;
            let own_size = cross_block_base + n_values as u64;

            let own_layout = FrameLayout {
                params: vec![],
                ret: callee_layout.ret,
                cont_ty,
                spill_base,
                n_spill: n_spill_words,
                size: own_size,
                storage: StorageId::STACK,
            };

            let alloca_budget = compute_alloca_budget(body, &self.types, &self.type_map);

            self.func_info.push(FuncInfo {
                entry_block: Some(block_offset),
                abort_block: None,
                callee_layout,
                own_layout,
                n_param_words,
                n_params,
                cross_block_values,
                cross_block_base,
                total_ret_bits,
                alloca_budget,
            });
            block_offset += body.blocks.len();
        }
        self.total_blocks = block_offset;
    }

    fn entry_return_bits(&self) -> usize {
        self.func_info
            .first()
            .expect("lowering a nonempty module must plan its entry function")
            .total_ret_bits
    }

    fn append_import_abort_return_bits(&self, em: &mut BlockEmitter<P>, args: &mut Vec<IRVarId>) {
        for _ in 0..self.entry_return_bits() {
            args.push(em.bc_const(false));
        }
    }

    fn lower_import_abort_sink(&mut self, func_idx: usize) {
        let info = &self.func_info[func_idx];
        let abort_block = info
            .abort_block
            .expect("only declaration-only imports have an abort sink");
        debug_assert_eq!(self.blocks.len(), abort_block);

        let sp_words = n_packs(self.pointer_bits);
        let return_start = sp_words + info.n_param_words;
        let mut params = vec![PACK_TID; return_start];
        params.extend(vec![BIT_TID; self.entry_return_bits()]);
        let return_args = (return_start..params.len())
            .map(|idx| IRVarId(idx as u32))
            .collect();
        self.blocks.push(IRBlock {
            params,
            stmts: vec![],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, return_args),
            },
        });
    }

    // ---- Lowering ----------------------------------------------------------

    fn lower_all(&mut self) {
        self.plan_functions();

        // Block 0: module entry — SP=0, jump to first function's entry.
        // Block 1: exit continuation — receives [SP, ret], emits Return.
        self.emit_entry_and_exit();

        for (func_idx, func_decl) in self.module.funcs.iter().enumerate() {
            match func_decl {
                FuncDecl::Body(body) => self.lower_function(func_idx, body),
                FuncDecl::Import { .. } => self.lower_import_abort_sink(func_idx),
                _ => continue,
            }
        }

        // Append extra blocks (continuations created by call splitting).
        self.blocks.append(&mut self.extra_blocks);
    }

    pub(crate) fn emit_entry_and_exit(&mut self) {
        // Block 0: module trampoline. Circuit inputs are the first function's
        // packed parameter words (empty when that function is nullary). Body:
        // SP = const 0, write the exit continuation, jump to func 0 with
        // `[sp_words…, param_words…]` so the callee entry arity matches.
        if self.func_info.is_empty() {
            let em = BlockEmitter::new(vec![]);
            self.blocks.push(em.finish(IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, vec![]),
            }));
            self.blocks.push(IRBlock {
                params: vec![],
                stmts: vec![],
                terminator: IRTerminator::Jmp {
                    target: IRBranchTarget::new(IRBlockTargetId::Return, vec![]),
                },
            });
            return;
        }

        let n_param_words = self.func_info[0].n_param_words;
        let mut em = BlockEmitter::new(vec![PACK_TID; n_param_words]);

        // The entry block's infrastructure statements (SP init, continuation
        // write) have no VAFFLE source statement of their own — they scaffold
        // the jump into func 0's body, so they inherit that function's first
        // value's provenance.
        let entry_prov = match &self.module.funcs[0] {
            FuncDecl::Body(b) => b.values.first().map(|n| n.prov.clone()),
            _ => None,
        }.or_else(|| self.control_prov.clone())
            .expect("emit_entry_and_exit: entry function has no source value; supply explicit control provenance");
        em.set_prov(entry_prov.clone());

        let param_words: Vec<IRVarId> = (0..n_param_words as u32).map(IRVarId).collect();

        let sp = StackPtr::<IRVarId>::from_const(&mut em, 0, self.pointer_bits);

        let info = &self.func_info[0];
        let callee_layout = &info.callee_layout;

        // Write the exit continuation (block 1) into the callee's Block lane.
        let exit_block_idx = 1u32;
        let cont_ty = callee_layout.cont_ty.unwrap_or(BIT_TID);
        let cont_var = em.emit(IRStmt::Const(
            Constant {
                hi: 0,
                lo: exit_block_idx as u128,
            },
            cont_ty,
        ));
        frame_write_cont(&mut em, &sp, callee_layout, cont_var);

        // Advance SP by the callee's *own* frame size (callee_layout.size plus
        // the callee's own spill words) so the callee can find its frame base by
        // retreating own_layout.size from the received SP.  Advancing by only
        // callee_layout.size would leave the received SP too low, causing the
        // callee's `frame_sp.retreat(own_layout.size)` to under-shoot the address
        // where the continuation was written.
        let own_size = info.own_layout.size;
        let mut new_sp = sp.clone();
        new_sp.advance(own_size);
        let new_sp_bits = new_sp.materialize(&mut em);

        // Pack SP bits into words for the jump; append the trampoline's own
        // packed parameter words so the callee entry sees SP + params.
        let mut jump_args = pack_bits(&mut em, &new_sp_bits, PACK_W);
        jump_args.extend(param_words);

        let entry_target = IRBlockId(
            info.entry_block
                .expect("the first lowered function must have a body") as u32,
        );
        self.blocks.push(em.finish(IRTerminator::Jmp {
            target: IRBranchTarget::new(IRBlockTargetId::Block(entry_target), jump_args),
        }));

        // Block 1: exit continuation.  Packed params: [sp_words, ret_words].
        // Use the actual total bit-width of the function's return values.
        let total_ret_bits = self.func_info[0].total_ret_bits;
        let sp_packs = n_packs(self.pointer_bits);
        let ret_packs = n_packs(total_ret_bits);
        let exit_params: Vec<IRTypeId> = vec![PACK_TID; sp_packs + ret_packs];
        let mut exit_em = BlockEmitter::new(exit_params);
        exit_em.set_prov(entry_prov);

        // Unpack all return bits and forward them as individual Bit args.
        let ret_word_ids: Vec<IRVarId> = (sp_packs as u32..(sp_packs + ret_packs) as u32)
            .map(IRVarId)
            .collect();
        let ret_bits = unpack_words(&mut exit_em, &ret_word_ids, total_ret_bits, PACK_W);

        self.blocks.push(exit_em.finish(IRTerminator::Jmp {
            target: IRBranchTarget::new(IRBlockTargetId::Return, ret_bits),
        }));
    }

    pub(crate) fn lower_function(&mut self, func_idx: usize, body: &FuncBody<P>) {
        let info = &self.func_info[func_idx];
        let entry_block_offset = info
            .entry_block
            .expect("lower_function requires a function body");
        let own_layout = info.own_layout.clone();
        let callee_layout = info.callee_layout.clone();

        let sp_packs = n_packs(self.pointer_bits);
        let n_param_words = info.n_param_words;
        let n_params = info.n_params;
        let cross_block_values = info.cross_block_values.clone();
        let alloca_budget = info.alloca_budget;

        for (vaffle_bi, vaffle_block) in body.blocks.iter().enumerate() {
            let ir_bi = entry_block_offset + vaffle_bi;
            let is_entry = vaffle_bi == body.entry.0;

            // Every block takes SP as packed words.
            // Entry blocks also take n_param_words packed parameter words
            // (passed directly by the caller instead of written to the frame).
            // Non-entry blocks take individual VAFFLE block params.
            let mut params: Vec<IRTypeId> = vec![PACK_TID; sp_packs];
            if is_entry {
                params.extend(vec![PACK_TID; n_param_words]);
            } else {
                for &(_vid, ty_id) in &vaffle_block.params {
                    params.push(self.type_map[ty_id.0 as usize]);
                }
            }

            let mut em = BlockEmitter::new(params);
            // Infrastructure stmts (SP unpack, param unpack) get the provenance
            // of the first VAFFLE stmt in this block; blocks with no stmts of
            // their own (e.g. an immediate unconditional jump) fall back to the
            // function's first value, mirroring `emit_entry_and_exit`'s rule.
            let block_prov = vaffle_block.stmts.first()
                .map(|&first_vid| body.values[first_vid.0].prov.clone())
                .or_else(|| body.values.first().map(|n| n.prov.clone()))
                .or_else(|| self.control_prov.clone())
                .expect("lower_function: function has no source value; supply explicit control provenance");
            em.set_prov(block_prov);

            // Unpack SP from packed words.
            let sp_word_ids: Vec<IRVarId> = (0..sp_packs as u32).map(IRVarId).collect();
            let sp_bits: Vec<IRVarId> =
                unpack_words(&mut em, &sp_word_ids, self.pointer_bits, PACK_W);

            let mut frame_sp = StackPtr::new(sp_bits.clone());
            frame_sp.retreat(own_layout.size);

            let mut val_map: BTreeMap<usize, IRVarId> = BTreeMap::new();
            // Per-bit result of each `Value::Call` processed so far in this
            // vaffle block, keyed by the call's own `ValueId.0` -- consulted
            // by `Value::Output { value, idx }` (see its own match arm
            // below), which is how a multi-bit call result's individual
            // bits are actually referenced downstream (e.g. by the LLVM
            // importer's own convention of emitting one `Output` projection
            // per result bit). `val_map` alone can't carry this: a call's
            // own `svid` never gets a *single* `IRVarId` (there is no one
            // wire representing "the whole return value" once it's wider
            // than 1 bit), so each `Output` needs to pick its own bit out
            // of the full set the call actually produced.
            let mut call_ret_bits: BTreeMap<usize, Vec<IRVarId>> = BTreeMap::new();

            // Entry block: param words arrive directly as block params after SP.
            if is_entry {
                let param_word_ids: Vec<IRVarId> = (sp_packs as u32
                    ..(sp_packs + n_param_words) as u32)
                    .map(IRVarId)
                    .collect();
                let all_bits = unpack_words(&mut em, &param_word_ids, n_params, PACK_W);
                // Each VAFFLE param slot is `ir_type_bit_width(its own type)`
                // bits wide, not always exactly 1 -- both real producers
                // (`volar-llvm-vaffle-import`, `VaffleTarget::begin_function`)
                // happen to always emit 1-bit params, but nothing in VAFFLE's
                // type system requires that, and `Terminator::Return`'s own
                // handling (`explode_to_bits`, above) already treats a
                // returned value's width generically -- entry params should
                // too, via the same `Merge`-to-compose idiom `compose_address`
                // already uses in this file.
                let mut bit_offset = 0usize;
                for &(vid, vtid) in vaffle_block.params.iter() {
                    let ir_tid = self.type_map[vtid.0 as usize];
                    let w = ir_type_bit_width(&self.types, ir_tid);
                    if bit_offset + w > all_bits.len() {
                        break;
                    }
                    let param_bits = &all_bits[bit_offset..bit_offset + w];
                    let composed = if w == 1 {
                        param_bits[0]
                    } else {
                        em.emit(IRStmt::Merge {
                            parts: param_bits.to_vec(),
                            ty: ir_tid,
                        })
                    };
                    val_map.insert(vid.0, composed);
                    bit_offset += w;
                }
            } else {
                // Non-entry: map block params (after packed SP words).
                for (pi, &(vid, _ty)) in vaffle_block.params.iter().enumerate() {
                    val_map.insert(vid.0, IRVarId((sp_packs + pi) as u32));
                }
            }

            // Cross-block value handling (see `compute_cross_block_values`):
            // spill any of this block's own params that a later block needs
            // via VAFFLE's flat, dominance-based value space (not re-threaded
            // as an explicit arg/param), then reload only what *this*
            // specific block actually references (via its own uses, not the
            // whole function's cross-block set -- reloading everything
            // needed *anywhere* at the start of *every* block scales as
            // n_blocks * n_cross_block_values and blows up on real-sized
            // functions).
            // `frame_spill`/`frame_reload` add `layout.spill_base`
            // internally (see their own doc comments: `idx` is a
            // zero-based "spill slot" offset *within* the spill region,
            // matching the real-call-spill call sites below which pass a
            // bare zero-based word index `wi`). `cross_block_base` already
            // *includes* `spill_base` (`cross_block_base = spill_base +
            // n_spill_words`), so passing it directly here double-counts
            // `spill_base`. Pass the region-relative offset instead.
            for (&vid, &var) in val_map.iter() {
                if cross_block_values.contains(&vid) {
                    self.spill_trace.push(alloc::format!(
                        "SPILL(entry) vaffle_bi={vaffle_bi} vid={vid} addr={} src_var={}",
                        own_layout.n_spill + vid as u64,
                        var.0
                    ));
                    frame_spill(
                        &mut em,
                        &frame_sp,
                        &own_layout,
                        own_layout.n_spill + vid as u64,
                        var,
                        BIT_TID,
                    );
                }
            }
            // Count every operand use in this block once. As source
            // statements are translated below, their operands are consumed;
            // at a call site the remaining counts are exactly the values used
            // by the untranslated suffix plus the terminator. This avoids
            // rebuilding a BTreeSet over every suffix at every call.
            let mut future_uses =
                collect_use_counts(&body.values, &vaffle_block.stmts, &vaffle_block.terminator);
            for &vid in &cross_block_values {
                if future_uses.contains(vid) && !val_map.contains_key(&vid) {
                    self.spill_trace.push(alloc::format!(
                        "RELOAD(entry) vaffle_bi={vaffle_bi} vid={vid} addr={}",
                        own_layout.n_spill + vid as u64
                    ));
                    let reloaded = frame_reload(
                        &mut em,
                        &frame_sp,
                        &own_layout,
                        own_layout.n_spill + vid as u64,
                        BIT_TID,
                    );
                    val_map.insert(vid, reloaded);
                }
            }

            let mut remaining_stmts: &[ValueId] = &vaffle_block.stmts;
            let mut current_em = em;
            let mut current_sp_bits = sp_bits.clone();
            let mut current_frame_sp = frame_sp.clone();

            while !remaining_stmts.is_empty() {
                let (before_call, at_call, after_call) = find_call(remaining_stmts, body);

                for &svid in before_call.iter() {
                    future_uses.consume_value(&body.values[svid.0].kind);
                    current_em.set_prov(body.values[svid.0].prov.clone());
                    match &body.values[svid.0].kind {
                        Value::Op(Stmt::StorageRead {
                            storage: StorageId::ALLOCA,
                            ty,
                            addr,
                        }) => {
                            let local_addr = val_map.get(&addr.0).copied().unwrap_or(IRVarId(0));
                            let real_addr =
                                rebase_stack_addr(
                                    &mut current_em,
                                    &current_sp_bits,
                                    local_addr,
                                    self.pointer_bits,
                                );
                            let id = current_em.emit(IRStmt::StorageRead {
                                storage: StorageId::STACK,
                                ty: self.type_map[ty.0 as usize],
                                addr: real_addr,
                            });
                            val_map.insert(svid.0, id);
                        }
                        Value::Op(Stmt::StorageWrite {
                            storage: StorageId::ALLOCA,
                            src,
                            ty,
                            addr,
                        }) => {
                            let local_addr = val_map.get(&addr.0).copied().unwrap_or(IRVarId(0));
                            let real_addr =
                                rebase_stack_addr(
                                    &mut current_em,
                                    &current_sp_bits,
                                    local_addr,
                                    self.pointer_bits,
                                );
                            let ir_src = val_map.get(&src.0).copied().unwrap_or(IRVarId(0));
                            let id = current_em.emit(IRStmt::StorageWrite {
                                storage: StorageId::STACK,
                                src: ir_src,
                                ty: self.type_map[ty.0 as usize],
                                addr: real_addr,
                            });
                            val_map.insert(svid.0, id);
                        }
                        Value::Op(stmt) => {
                            let ir_stmt = translate_stmt(stmt, &val_map, &self.type_map);
                            let id = current_em.emit(ir_stmt);
                            val_map.insert(svid.0, id);
                        }
                        Value::StackAlloc { base_slot, .. } => {
                            // `base_slot` is a genuine u64 known here (not an
                            // operand to look up) -- stamp it at the module's
                            // pointer width
                            // Const first (wide enough that `extract_bit`
                            // reads real bits, not the 1-bit-truncation bug
                            // `addr_tid` fixed elsewhere), then rebase like
                            // any other stack address.
                            let base_tid = self.types.primitive(match self.pointer_bits {
                                32 => Type::_32,
                                64 => Type::_64,
                                _ => unreachable!("VAFFLE pointer width is validated by its ABI"),
                            });
                            let base_const = current_em.emit(IRStmt::Const(
                                Constant {
                                    hi: 0,
                                    lo: *base_slot as u128,
                                },
                                base_tid,
                            ));
                            let addr =
                                rebase_stack_addr(
                                    &mut current_em,
                                    &current_sp_bits,
                                    base_const,
                                    self.pointer_bits,
                                );
                            val_map.insert(svid.0, addr);
                        }
                        Value::PtrLoad { ptr, .. } => {
                            let s = val_map.get(&ptr.0).copied().unwrap_or(IRVarId(0));
                            val_map.insert(svid.0, s);
                        }
                        Value::PtrStore { .. } => {}
                        Value::PtrOffset { idx, .. } => {
                            let s = val_map.get(&idx.0).copied().unwrap_or(IRVarId(0));
                            val_map.insert(svid.0, s);
                        }
                        Value::Output { value, idx } => {
                            let bit = call_ret_bits
                                .get(&value.0)
                                .and_then(|bits| bits.get(*idx))
                                .copied()
                                .unwrap_or(IRVarId(0));
                            val_map.insert(svid.0, bit);
                        }
                        _ => {}
                    }
                    // Cross-block spill: this value's own VAFFLE-level
                    // dominance scope extends past this block (see
                    // `compute_cross_block_values`), so a later block that
                    // references it directly (not via an explicit param/arg)
                    // needs to be able to reload it.
                    if cross_block_values.contains(&svid.0) {
                        if let Some(&var) = val_map.get(&svid.0) {
                            self.spill_trace.push(alloc::format!(
                                "SPILL(stmt) vaffle_bi={vaffle_bi} vid={} addr={} src_var={}",
                                svid.0,
                                own_layout.n_spill + svid.0 as u64,
                                var.0
                            ));
                            frame_spill(
                                &mut current_em,
                                &current_frame_sp,
                                &own_layout,
                                own_layout.n_spill + svid.0 as u64,
                                var,
                                BIT_TID,
                            );
                        }
                    }
                }

                match at_call {
                    Some(call_vid) => {
                        future_uses.consume_value(&body.values[call_vid.0].kind);
                        current_em.set_prov(body.values[call_vid.0].prov.clone());
                        if let Value::Call {
                            func: callee_fid,
                            args: call_args,
                        } = &body.values[call_vid.0].kind
                        {
                            let callee_idx = callee_fid.0;
                            let callee_info = &self.func_info[callee_idx];
                            let cl = callee_info.callee_layout.clone();

                            // 1. Selective spill: only values still referenced
                            //    by the untranslated suffix or block terminator
                            //    need to survive this call. `future_uses` was
                            //    built once for the block and consumed through
                            //    this call. Iterate its live-use worklist
                            //    rather than every value ever translated into
                            //    `val_map`: LLVM blocks often contain thousands
                            //    of already-dead values between calls. StackAlloc
                            //    addresses are compile-time constants and never
                            //    need to be spilled.
                            let mut spill_keys = Vec::new();
                            for key in future_uses.live_values() {
                                if matches!(&body.values[key].kind, Value::StackAlloc { .. }) {
                                    continue;
                                }
                                if val_map.contains_key(&key) {
                                    spill_keys.push(key);
                                }
                            }
                            let spill_bits: Vec<IRVarId> =
                                spill_keys.iter().map(|key| val_map[key]).collect();
                            let spill_words = pack_bits(&mut current_em, &spill_bits, PACK_W);
                            for (wi, &word) in spill_words.iter().enumerate() {
                                frame_spill(
                                    &mut current_em,
                                    &current_frame_sp,
                                    &own_layout,
                                    wi as u64,
                                    word,
                                    PACK_TID,
                                );
                            }

                            // 2. Pack callee args — passed as entry-block params, not frame writes.
                            let arg_bits: Vec<IRVarId> =
                                call_args.iter().map(|vid| val_map[&vid.0]).collect();
                            let arg_words = pack_bits(&mut current_em, &arg_bits, PACK_W);

                            // 3. Write continuation.
                            //
                            // `cont_block_idx` must predict the *final*
                            // absolute index the continuation block (built
                            // below, step 5) will land at once every
                            // function has been lowered and `extra_blocks`
                            // is appended after `self.blocks` (see
                            // `total_blocks`'s own doc comment -- functions
                            // lower in sequence, so `self.blocks.len()`
                            // alone only reflects progress from functions
                            // already processed, not the full reserved
                            // range). Between here and the continuation's
                            // own push, exactly one more piece — the
                            // callee-jump block being built right below —
                            // gets emitted; it only consumes an
                            // `extra_blocks` slot (shifting the
                            // continuation's own index by one) when this
                            // vaffle block's *own* reserved `self.blocks`
                            // slot is already spoken for by an earlier
                            // piece (an earlier call in the same original
                            // block) -- i.e. exactly the same condition the
                            // callee-jump block's own push below checks.
                            // The actual bit width of the callee's return
                            // value (e.g. 32 for an `i32`), NOT a presence
                            // flag -- `unpack_words` below must reconstruct
                            // every returned bit, not just decide whether
                            // any exist. Previously computed as `if n_ret >
                            // 0 { 1 } else { 0 }` (`n_ret` being the packed
                            // *word* count, always 1 for any return up to
                            // 64 bits) -- silently truncating every
                            // multi-bit call result to its own bit 0,
                            // undetected because every prior test's callee
                            // happened to return exactly 1 bit.
                            let n_ret_bits_orig = callee_info.total_ret_bits;
                            let callee_jump_takes_extra_slot = self.blocks.len() != ir_bi;
                            let cont_block_idx = self.total_blocks
                                + self.extra_blocks.len()
                                + usize::from(callee_jump_takes_extra_slot);
                            let cont_ir_idx = cont_block_idx as u32;
                            let cont_ty = cl.cont_ty.unwrap_or(BIT_TID);
                            let cont_var = current_em.emit(IRStmt::Const(
                                Constant {
                                    hi: 0,
                                    lo: cont_ir_idx as u128,
                                },
                                cont_ty,
                            ));
                            // The callee's frame actually starts at
                            // `current_sp_bits + alloca_budget` (see the
                            // `new_sp.advance` below) -- the continuation
                            // must be written at that same base, or the
                            // callee's own `frame_read_cont` (retreating
                            // from *its* received SP by *its own*
                            // `own_layout.size` alone, with no way to know
                            // this caller's `alloca_budget`) reads back a
                            // different address than this write landed at.
                            // Confirmed via `unroll_ir` (only reachable
                            // once cross-function calls could unroll at
                            // all -- see docs/llvm-array-alloca.md's
                            // "Cross-function call numeric correctness"):
                            // a nonzero `alloca_budget` made the callee's
                            // own continuation read fail to fold to the
                            // same constant the caller wrote, surfacing as
                            // `SymbolicBranch` resolving the callee's
                            // return `Dyn` jump.
                            let mut callee_frame_sp = StackPtr::new(current_sp_bits.clone());
                            callee_frame_sp.advance(alloca_budget);
                            frame_write_cont(&mut current_em, &callee_frame_sp, &cl, cont_var);

                            // 4. Advance SP by *this* function's own alloca budget (its
                            // still-live local storage, past `current_sp_bits`, must not be
                            // overlapped by the callee's frame -- see `alloca_budget`'s doc
                            // comment) plus the callee's own size (callee_layout + spill),
                            // pack, jump — args appended after SP words.
                            let mut new_sp = StackPtr::new(current_sp_bits.clone());
                            new_sp.advance(alloca_budget + callee_info.own_layout.size);
                            let new_sp_bits = new_sp.materialize(&mut current_em);
                            let mut sp_words = pack_bits(&mut current_em, &new_sp_bits, PACK_W);
                            sp_words.extend(arg_words);

                            let callee_target = match callee_info.entry_block {
                                Some(entry_block) => IRBlockId(entry_block as u32),
                                None => {
                                    self.append_import_abort_return_bits(
                                        &mut current_em,
                                        &mut sp_words,
                                    );
                                    IRBlockId(
                                        callee_info
                                            .abort_block
                                            .expect("an import must reserve an abort sink")
                                            as u32,
                                    )
                                }
                            };
                            let block = current_em.finish(IRTerminator::Jmp {
                                target: IRBranchTarget::new(
                                    IRBlockTargetId::Block(callee_target),
                                    sp_words,
                                ),
                            });
                            if self.blocks.len() == ir_bi {
                                self.blocks.push(block);
                            } else {
                                self.extra_blocks.push(block);
                            }

                            // 5. Create continuation block.
                            //    Params: [packed_sp_words, packed_ret_words].
                            let ret_packs = n_packs(n_ret_bits_orig);
                            let cont_params: Vec<IRTypeId> = vec![PACK_TID; sp_packs + ret_packs];
                            let mut cont_em = BlockEmitter::new(cont_params);
                            // Continuation infrastructure gets the call stmt's provenance.
                            cont_em.set_prov(body.values[call_vid.0].prov.clone());

                            // Unpack SP.
                            let cont_sp_word_ids: Vec<IRVarId> =
                                (0..sp_packs as u32).map(IRVarId).collect();
                            let cont_sp_bits =
                                unpack_words(
                                    &mut cont_em,
                                    &cont_sp_word_ids,
                                    self.pointer_bits,
                                    PACK_W,
                                );

                            // Unpack return value.
                            if n_ret_bits_orig > 0 {
                                let ret_word_ids: Vec<IRVarId> = (sp_packs as u32
                                    ..(sp_packs + ret_packs) as u32)
                                    .map(IRVarId)
                                    .collect();
                                let ret_bits = unpack_words(
                                    &mut cont_em,
                                    &ret_word_ids,
                                    n_ret_bits_orig,
                                    PACK_W,
                                );
                                // `call_vid` itself never carries a single
                                // wire once the return is wider than 1 bit
                                // -- downstream code reaches each bit via
                                // its own `Value::Output { value: call_vid,
                                // idx }` node (see that match arm above).
                                // Still record bit 0 under `call_vid`
                                // directly too, for any producer (e.g.
                                // `VaffleTarget`) that references a 1-bit
                                // call result without an `Output` wrapper.
                                val_map.insert(call_vid.0, ret_bits[0]);
                                call_ret_bits.insert(call_vid.0, ret_bits);
                            }

                            // Packed reload.
                            let mut cont_frame_sp = StackPtr::new(cont_sp_bits.clone());
                            cont_frame_sp.retreat(own_layout.size);

                            // Cross-block spill of the call's own result (see
                            // the `before_call` loop's identical comment).
                            if cross_block_values.contains(&call_vid.0) {
                                if let Some(&var) = val_map.get(&call_vid.0) {
                                    frame_spill(
                                        &mut cont_em,
                                        &cont_frame_sp,
                                        &own_layout,
                                        own_layout.n_spill + call_vid.0 as u64,
                                        var,
                                        BIT_TID,
                                    );
                                }
                            }

                            let n_spill_words = spill_words.len();
                            let mut reloaded_words = Vec::with_capacity(n_spill_words);
                            for wi in 0..n_spill_words {
                                let w = frame_reload(
                                    &mut cont_em,
                                    &cont_frame_sp,
                                    &own_layout,
                                    wi as u64,
                                    PACK_TID,
                                );
                                reloaded_words.push(w);
                            }
                            let reloaded_bits = unpack_words(
                                &mut cont_em,
                                &reloaded_words,
                                spill_keys.len(),
                                PACK_W,
                            );
                            for (ki, &key) in spill_keys.iter().enumerate() {
                                if key == call_vid.0 {
                                    continue;
                                }
                                val_map.insert(key, reloaded_bits[ki]);
                            }

                            // The callee retreated *its own* `own_layout.size`
                            // off the SP it received -- which included this
                            // caller's `alloca_budget` on top of the callee's
                            // own frame size (see the call-site SP-advance
                            // above) -- so `cont_sp_bits` is still short by
                            // exactly that `alloca_budget` of landing back on
                            // this function's real, pre-call SP. The callee
                            // has no way to know this caller's own
                            // `alloca_budget` (a different caller could have
                            // a different one), so undo it here, on the
                            // caller's own side, instead. Confirmed via
                            // `unroll_ir`: without this, this function's own
                            // post-call code -- both its own alloca rebasing
                            // and its own eventual `frame_read_cont` -- was
                            // computed against the wrong (shifted) base,
                            // surfacing as `SymbolicBranch` resolving this
                            // function's own return `Dyn` jump.
                            let mut restored_sp = StackPtr::new(cont_sp_bits.clone());
                            restored_sp.retreat(alloca_budget);
                            let restored_sp_bits = restored_sp.materialize(&mut cont_em);

                            current_em = cont_em;
                            current_sp_bits = restored_sp_bits;
                            current_frame_sp = StackPtr::new(current_sp_bits.clone());
                            current_frame_sp.retreat(own_layout.size);
                        }
                        remaining_stmts = after_call;
                    }
                    None => {
                        remaining_stmts = &[];
                    }
                }
            }

            // Terminator.
            let terminator = self.translate_terminator(
                &vaffle_block.terminator,
                &val_map,
                func_idx,
                &current_sp_bits,
                &current_frame_sp,
                &own_layout,
                &mut current_em,
            );

            let block = current_em.finish(terminator);
            if self.blocks.len() == ir_bi {
                self.blocks.push(block);
            } else {
                self.extra_blocks.push(block);
            }
        }
    }

    fn translate_terminator(
        &self,
        term: &Terminator,
        val_map: &BTreeMap<usize, IRVarId>,
        func_idx: usize,
        sp_bits: &[IRVarId],
        frame_sp: &StackPtr<IRVarId>,
        own_layout: &FrameLayout,
        em: &mut BlockEmitter<P>,
    ) -> IRTerminator {
        let s = |vid: &ValueId| val_map.get(&vid.0).copied().unwrap_or(IRVarId(0));
        let entry_off = self.func_info[func_idx]
            .entry_block
            .expect("translate_terminator requires a function body");

        let body = match &self.module.funcs[func_idx] {
            FuncDecl::Body(b) => b,
            _ => {
                return IRTerminator::Jmp {
                    target: IRBranchTarget::new(IRBlockTargetId::Return, vec![]),
                };
            }
        };

        match term {
            Terminator::Return { values } => {
                // Explode each returned VAFFLE value into individual Bit vars,
                // supporting multi-bit types (e.g. _8, _16, _32, _64, _128).
                let mut all_ret_bits: Vec<IRVarId> = Vec::new();
                for vid in values {
                    let ir_var = s(vid);
                    let vtid = vaffle_value_vtid(self.module, &body.values, *vid);
                    let ir_tid = self.type_map[vtid.0 as usize];
                    let n_bits = ir_type_bit_width(&self.types, ir_tid);
                    all_ret_bits.extend(explode_to_bits(em, ir_var, n_bits));
                }
                // Pack all return bits into words.
                let ret_words = pack_bits(em, &all_ret_bits, PACK_W);
                // frame_write_ret expects one value per ret slot;
                // our layout has ceil(total_ret_bits/PACK_W) slots of PACK_TID.
                frame_write_ret(em, frame_sp, own_layout, &ret_words);

                let cont_var =
                    frame_read_cont(em, frame_sp, own_layout).expect("return without continuation");

                let mut retreated_sp = StackPtr::new(sp_bits.to_vec());
                retreated_sp.retreat(own_layout.size);
                let retreated_bits = retreated_sp.materialize(em);

                // Pack SP + ret words for the Dyn jump.
                let sp_words = pack_bits(em, &retreated_bits, PACK_W);
                let mut dyn_args = sp_words;
                dyn_args.extend(ret_words);

                IRTerminator::Jmp {
                    target: IRBranchTarget::new(IRBlockTargetId::Dyn(cont_var), dyn_args),
                }
            }
            Terminator::Jump(target) => {
                let ir_block = IRBlockId((entry_off + target.block.0) as u32);
                // Pack SP, append unpacked VAFFLE block args (internal).
                let sp_words = pack_bits(em, sp_bits, PACK_W);
                let mut args: Vec<IRVarId> = sp_words;
                args.extend(target.args.iter().map(|v| s(v)));
                IRTerminator::Jmp {
                    target: IRBranchTarget {
                        dest: IRBlockTargetId::Block(ir_block),
                        args,
                        reentry: target.reentry.clone(),
                    },
                }
            }
            Terminator::IfNonzero {
                cond,
                then_target,
                else_target,
            } => {
                let then_block = IRBlockId((entry_off + then_target.block.0) as u32);
                let else_block = IRBlockId((entry_off + else_target.block.0) as u32);
                let sp_words = pack_bits(em, sp_bits, PACK_W);
                let mut then_args: Vec<IRVarId> = sp_words.clone();
                then_args.extend(then_target.args.iter().map(|v| s(v)));
                let mut else_args: Vec<IRVarId> = sp_words;
                else_args.extend(else_target.args.iter().map(|v| s(v)));
                IRTerminator::JumpCond {
                    condition: s(cond),
                    then_target: IRBranchTarget {
                        dest: IRBlockTargetId::Block(then_block),
                        args: then_args,
                        reentry: then_target.reentry.clone(),
                    },
                    else_target: IRBranchTarget {
                        dest: IRBlockTargetId::Block(else_block),
                        args: else_args,
                        reentry: else_target.reentry.clone(),
                    },
                }
            }
            Terminator::ReturnCall {
                func: callee_fid,
                args: call_args,
            } => {
                let callee_idx = callee_fid.0;
                if callee_idx >= self.func_info.len() {
                    return IRTerminator::Jmp {
                        target: IRBranchTarget::new(IRBlockTargetId::Return, vec![]),
                    };
                }
                let callee_info = &self.func_info[callee_idx];
                let cl = callee_info.callee_layout.clone();

                // The tail call reuses F's continuation, which F's caller wrote at
                // F's frame base.  With the fix where callers advance SP by
                // own_layout.size, F's frame base == frame_sp (already computed as
                // received_sp - own_layout.size).  G starts its frame at the same
                // address so G can find the continuation when it retreats own_size.
                let callee_frame_sp = frame_sp.clone();

                // Pack G's args — passed as entry-block params, not frame writes.
                let arg_bits: Vec<IRVarId> = call_args
                    .iter()
                    .map(|vid| val_map.get(&vid.0).copied().unwrap_or(IRVarId(0)))
                    .collect();
                let arg_words = pack_bits(em, &arg_bits, PACK_W);

                // Advance SP by G's own size and jump — args after SP words.
                let mut new_sp = callee_frame_sp.clone();
                new_sp.advance(callee_info.own_layout.size);
                let new_sp_bits = new_sp.materialize(em);
                let mut sp_words = pack_bits(em, &new_sp_bits, PACK_W);
                sp_words.extend(arg_words);

                let callee_target = match callee_info.entry_block {
                    Some(entry_block) => IRBlockId(entry_block as u32),
                    None => {
                        self.append_import_abort_return_bits(em, &mut sp_words);
                        IRBlockId(
                            callee_info
                                .abort_block
                                .expect("an import must reserve an abort sink")
                                as u32,
                        )
                    }
                };
                IRTerminator::Jmp {
                    target: IRBranchTarget::new(IRBlockTargetId::Block(callee_target), sp_words),
                }
            }
            Terminator::Table {
                index,
                targets,
                default_target,
            } => {
                // Dense positional dispatch (`targets[idx]`, else default),
                // matching the fuzz interpreter and `VaffleTarget::switch`:
                // the selector is `0..targets.len()-1` for an explicit case
                // and `targets.len()` (out of bounds) for the default.
                let sp_words = pack_bits(em, sp_bits, PACK_W);
                let mut cases = BTreeMap::new();
                for (i, t) in targets.iter().enumerate() {
                    let ir_block = IRBlockId((entry_off + t.block.0) as u32);
                    let mut args: Vec<IRVarId> = sp_words.clone();
                    args.extend(t.args.iter().map(|v| s(v)));
                    cases.insert(
                        Constant {
                            hi: 0,
                            lo: i as u128,
                        },
                        IRBranchTarget {
                            dest: IRBlockTargetId::Block(ir_block),
                            args,
                            reentry: t.reentry.clone(),
                        },
                    );
                }
                let default_block = IRBlockId((entry_off + default_target.block.0) as u32);
                let mut default_args: Vec<IRVarId> = sp_words;
                default_args.extend(default_target.args.iter().map(|v| s(v)));
                cases.insert(
                    Constant {
                        hi: 0,
                        lo: targets.len() as u128,
                    },
                    IRBranchTarget {
                        dest: IRBlockTargetId::Block(default_block),
                        args: default_args,
                        reentry: default_target.reentry.clone(),
                    },
                );
                IRTerminator::JumpTable {
                    index: s(index),
                    cases,
                }
            }
            _ => IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, vec![]),
            },
        }
    }

    pub(crate) fn finish(self) -> (IRBlocks<P>, IRTypes) {
        (
            IRBlocks {
                oracles: self.oracles,
                actions: self.actions,
                rngs: alloc::vec![],
                blocks: self.blocks,
                pre_init: self.pre_init,
            },
            self.types,
        )
    }

    /// Number of primary blocks committed so far. Used by the lazy-plan
    /// driver (`plan.rs`) to bracket which blocks a given `lower_function`
    /// call just added, for sub-element tracing.
    pub(crate) fn blocks_len(&self) -> usize {
        self.blocks.len()
    }

    /// Number of extra (call-split continuation) blocks committed so far.
    pub(crate) fn extra_blocks_len(&self) -> usize {
        self.extra_blocks.len()
    }

    /// Total `IRStmt`s appended to either `blocks` or `extra_blocks` since
    /// `(blocks_before, extra_before)` were captured (typically via
    /// `blocks_len`/`extra_blocks_len` just before a `lower_function` call).
    pub(crate) fn stmt_count_since(&self, blocks_before: usize, extra_before: usize) -> usize {
        self.blocks[blocks_before..]
            .iter()
            .map(|b| b.stmts.len())
            .sum::<usize>()
            + self.extra_blocks[extra_before..]
                .iter()
                .map(|b| b.stmts.len())
                .sum::<usize>()
    }

    /// Append accumulated call-split continuation blocks to the primary
    /// sequence -- the same step `lower_all` performs before `finish()`.
    /// Must be called (once) after every function has been either lowered
    /// or placeholder-reserved.
    pub(crate) fn append_extra_blocks(&mut self) {
        self.blocks.append(&mut self.extra_blocks);
    }

    /// Push `count` placeholder (unreachable) `IRBlock`s directly into the
    /// primary block sequence -- used by the lazy-plan driver to preserve
    /// `plan_functions`'s precomputed block-offset arithmetic for a function
    /// that was not in the requested/reachable closure, without paying for
    /// its real `lower_function` translation cost. Matches the same
    /// "nothing real to jump to" trap shape `emit_entry_and_exit` already
    /// uses for its own degenerate case.
    pub(crate) fn reserve_placeholder_blocks(&mut self, count: usize) {
        for _ in 0..count {
            self.blocks.push(IRBlock {
                params: vec![],
                stmts: vec![],
                terminator: IRTerminator::Jmp {
                    target: IRBranchTarget::new(IRBlockTargetId::Return, vec![]),
                },
            });
        }
    }
}

// ============================================================================
// Helpers
// ============================================================================

/// Find the first `Value::Call` in `stmts`, returning (before, Some(call), after).
/// If no call, returns (stmts, None, &[]).
fn find_call<'a, P: Clone>(
    stmts: &'a [ValueId],
    body: &FuncBody<P>,
) -> (&'a [ValueId], Option<ValueId>, &'a [ValueId]) {
    for (i, &svid) in stmts.iter().enumerate() {
        if matches!(&body.values[svid.0].kind, Value::Call { .. }) {
            return (&stmts[..i], Some(svid), &stmts[i + 1..]);
        }
    }
    (stmts, None, &[])
}

/// Collect each operand use at most once, using a caller-owned dense mark
/// table.  VAFFLE value IDs are arena indices, so this avoids allocating and
/// searching a B-tree node for every bit-level operand during SSA rewriting.
///
/// `mark` must be nonzero and distinct from the marks used for earlier calls
/// with the same table.  Results are returned in first-use order; callers
/// which expose construction order should sort them explicitly.
pub(crate) fn collect_unique_uses<P: Clone>(
    values: &[volar_ir_common::Node<Value, P>],
    stmt_ids: &[ValueId],
    term: &Terminator,
    marks: &mut [usize],
    mark: usize,
    out: &mut Vec<usize>,
) {
    assert_ne!(mark, 0, "collect_unique_uses requires a nonzero mark");
    out.clear();
    let mut sink = UniqueUseSink { marks, mark, out };
    collect_uses_into(values, stmt_ids, term, &mut sink);
}

/// Per-value count of operand occurrences remaining in a block. This is
/// consumed in source order by `lower_function`, making call-site liveness
/// queries constant-time per candidate instead of rebuilding every suffix.
struct UseCounts {
    // ValueId is a dense index into this function's `values` arena. A flat
    // counter vector avoids allocating one BTree node per bit-level operand
    // in large LLVM-imported arithmetic blocks.
    counts: Vec<usize>,
    // A dense bitmap of values with nonzero counts. It gives call sites the
    // old ascending-ValueId spill order without ordered-set maintenance or a
    // full `val_map` traversal. One word covers 64 arena values.
    live_words: Vec<u64>,
}

impl UseCounts {
    fn new(n_values: usize) -> Self {
        Self {
            counts: vec![0; n_values],
            live_words: vec![0; n_values.div_ceil(64)],
        }
    }

    fn contains(&self, value: usize) -> bool {
        self.counts[value] != 0
    }

    fn live_values(&self) -> impl Iterator<Item = usize> + '_ {
        LiveValues {
            words: &self.live_words,
            word_idx: 0,
            current: 0,
        }
    }

    fn consume_value(&mut self, value: &Value) {
        let mut consumer = UseCountConsumer { counts: self };
        collect_value_uses(value, &mut consumer);
    }

    fn add(&mut self, value: usize) {
        if self.counts[value] == 0 {
            self.live_words[value / 64] |= 1u64 << (value % 64);
        }
        self.counts[value] += 1;
    }

    fn remove(&mut self, value: usize) {
        let count = &mut self.counts[value];
        assert!(*count > 0, "consumed VAFFLE use was not counted");
        *count -= 1;
        if *count == 0 {
            let word = &mut self.live_words[value / 64];
            let mask = 1u64 << (value % 64);
            assert!(*word & mask != 0, "live VAFFLE use was not indexed");
            *word &= !mask;
        }
    }
}

/// Ascending iterator over a [`UseCounts`] live bitmap.
struct LiveValues<'a> {
    words: &'a [u64],
    word_idx: usize,
    current: u64,
}

impl Iterator for LiveValues<'_> {
    type Item = usize;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if self.current != 0 {
                let bit = self.current.trailing_zeros() as usize;
                self.current &= self.current - 1;
                return Some((self.word_idx - 1) * 64 + bit);
            }
            self.current = *self.words.get(self.word_idx)?;
            self.word_idx += 1;
        }
    }
}

trait UseSink {
    fn add_use(&mut self, value: usize);
}

struct UniqueUseSink<'a> {
    marks: &'a mut [usize],
    mark: usize,
    out: &'a mut Vec<usize>,
}

impl UseSink for UniqueUseSink<'_> {
    fn add_use(&mut self, value: usize) {
        let slot = self
            .marks
            .get_mut(value)
            .unwrap_or_else(|| panic!("VAFFLE operand ValueId {value} is outside its value arena"));
        if *slot != self.mark {
            *slot = self.mark;
            self.out.push(value);
        }
    }
}

impl UseSink for UseCounts {
    fn add_use(&mut self, value: usize) {
        self.add(value);
    }
}

struct UseCountConsumer<'a> {
    counts: &'a mut UseCounts,
}

impl UseSink for UseCountConsumer<'_> {
    fn add_use(&mut self, value: usize) {
        self.counts.remove(value);
    }
}

fn collect_use_counts<P: Clone>(
    values: &[volar_ir_common::Node<Value, P>],
    stmt_ids: &[ValueId],
    term: &Terminator,
) -> UseCounts {
    let mut uses = UseCounts::new(values.len());
    collect_uses_into(values, stmt_ids, term, &mut uses);
    uses
}

fn collect_uses_into<P: Clone, S: UseSink>(
    values: &[volar_ir_common::Node<Value, P>],
    stmt_ids: &[ValueId],
    term: &Terminator,
    uses: &mut S,
) {
    for &vid in stmt_ids {
        collect_value_uses(&values[vid.0].kind, uses);
    }
    collect_terminator_uses(term, uses);
}

fn collect_value_uses<S: UseSink>(val: &Value, out: &mut S) {
    match val {
        Value::Op(stmt) => collect_stmt_uses(stmt, out),
        Value::Call { args, .. } => {
            for a in args {
                out.add_use(a.0);
            }
        }
        Value::Output { value, .. } => {
            out.add_use(value.0);
        }
        Value::PtrLoad { ptr, .. } => {
            out.add_use(ptr.0);
        }
        Value::PtrStore { ptr, val } => {
            out.add_use(ptr.0);
            out.add_use(val.0);
        }
        Value::PtrOffset { ptr, idx, .. } => {
            out.add_use(ptr.0);
            out.add_use(idx.0);
        }
        // Defining occurrences — no operands to record.
        Value::Param { .. } | Value::StackAlloc { .. } => {}
        _ => {}
    }
}

fn collect_stmt_uses<S: UseSink>(stmt: &Stmt<ValueId>, out: &mut S) {
    match stmt {
        Stmt::Const(..) | Stmt::Rng { .. } => {}
        Stmt::Poly { coeffs, .. } => {
            for vars in coeffs.keys() {
                for v in vars {
                    out.add_use(v.0);
                }
            }
        }
        Stmt::Merge { parts, .. } => {
            for p in parts {
                out.add_use(p.0);
            }
        }
        Stmt::Splat { src, .. } => {
            out.add_use(src.0);
        }
        Stmt::Transmute { src, .. } => {
            out.add_use(src.0);
        }
        Stmt::Rol { src, .. } | Stmt::Ror { src, .. } => {
            out.add_use(src.0);
        }
        Stmt::Shuffle { result_bits, .. } => {
            for (_, v) in result_bits {
                out.add_use(v.0);
            }
        }
        Stmt::StorageRead { addr, .. } => {
            out.add_use(addr.0);
        }
        Stmt::StorageWrite { src, addr, .. } => {
            out.add_use(src.0);
            out.add_use(addr.0);
        }
        Stmt::OracleCall { args, .. } => {
            for a in args {
                out.add_use(a.0);
            }
        }
        Stmt::OracleOutput { call, .. } => {
            out.add_use(call.0);
        }
        Stmt::ActionCall {
            guard,
            args,
            fallbacks,
            ..
        } => {
            out.add_use(guard.0);
            for a in args {
                out.add_use(a.0);
            }
            for f in fallbacks {
                out.add_use(f.0);
            }
        }
        Stmt::ActionOutput { call, .. } => {
            out.add_use(call.0);
        }
        _ => {}
    }
}

fn collect_terminator_uses<S: UseSink>(term: &Terminator, out: &mut S) {
    match term {
        Terminator::Return { values } => {
            for v in values {
                out.add_use(v.0);
            }
        }
        Terminator::Jump(target) => {
            for v in &target.args {
                out.add_use(v.0);
            }
        }
        Terminator::ReturnCall { args, .. } => {
            for a in args {
                out.add_use(a.0);
            }
        }
        Terminator::IfNonzero {
            cond,
            then_target,
            else_target,
        } => {
            out.add_use(cond.0);
            for v in &then_target.args {
                out.add_use(v.0);
            }
            for v in &else_target.args {
                out.add_use(v.0);
            }
        }
        Terminator::Table {
            index,
            targets,
            default_target,
        } => {
            out.add_use(index.0);
            for t in targets {
                for v in &t.args {
                    out.add_use(v.0);
                }
            }
            for v in &default_target.args {
                out.add_use(v.0);
            }
        }
        _ => {}
    }
}

/// Compute the set of VAFFLE `ValueId.0` indices that are referenced by a
/// block other than the one that defines them (as a param or a stmt).
///
/// VAFFLE (like WAFFLE) uses a flat, dominance-based value space where any
/// block may reference any value computed by a dominating block directly by
/// ID, without it being re-threaded as an explicit block param -- but
/// `lower_function`'s own per-block `val_map` is reset fresh for each
/// translated block (mirroring the target `IRBlock`'s own explicit-param-only
/// scoping). Left unhandled, such a cross-block reference silently resolves
/// to `IRVarId(0)` via `translate_stmt`/`translate_terminator`'s `s(vid)`
/// fallback instead of the real value -- exactly the bug this function's
/// result (`lower_function`'s spill/reload of every value in this set) is
/// built to fix.
pub(crate) fn compute_cross_block_values<P: Clone>(body: &FuncBody<P>) -> BTreeSet<usize> {
    let owner = compute_owner(&body.blocks, body.values.len());

    let mut cross: BTreeSet<usize> = BTreeSet::new();
    let mut marks = vec![0usize; body.values.len()];
    let mut uses = Vec::new();
    for (bi, block) in body.blocks.iter().enumerate() {
        collect_unique_uses(
            &body.values,
            &block.stmts,
            &block.terminator,
            &mut marks,
            bi + 1,
            &mut uses,
        );
        for &u in &uses {
            if owner[u] != bi {
                cross.insert(u);
            }
        }
    }
    cross
}

/// Map every VAFFLE `ValueId.0` to the index of the block that owns its
/// defining occurrence (as a param or a stmt). Shared by
/// [`compute_cross_block_values`] and `vaffle_ssa`'s param-threading pass.
///
/// `ValueId` is a dense arena index, so the result is a dense table. Missing
/// definitions are represented by `usize::MAX` and fail loudly at their use
/// site, rather than quietly behaving like a cross-block value.
pub(crate) fn compute_owner(blocks: &[Block], n_values: usize) -> Vec<usize> {
    let mut owner = vec![usize::MAX; n_values];
    for (bi, block) in blocks.iter().enumerate() {
        for &(vid, _ty) in &block.params {
            *owner
                .get_mut(vid.0)
                .unwrap_or_else(|| panic!("VAFFLE parameter ValueId {} is outside its value arena", vid.0)) =
                bi;
        }
        for &vid in &block.stmts {
            *owner
                .get_mut(vid.0)
                .unwrap_or_else(|| panic!("VAFFLE statement ValueId {} is outside its value arena", vid.0)) =
                bi;
        }
    }
    owner
}

/// Compute the bit-width of an IR type recursively.
fn ir_type_bit_width(types: &IRTypes, tid: TypeId) -> usize {
    match &types.0[tid.0 as usize] {
        IrType::Primitive(p) => match p {
            volar_ir_common::Type::Bit => 1,
            volar_ir_common::Type::_8 | volar_ir_common::Type::AES8 => 8,
            volar_ir_common::Type::_16 => 16,
            volar_ir_common::Type::_32 => 32,
            volar_ir_common::Type::_64 | volar_ir_common::Type::Galois64 => 64,
            volar_ir_common::Type::_128 => 128,
            volar_ir_common::Type::_256 => 256,
            volar_ir_common::Type::Z3 => 2,
            _ => 1, // unknown primitive — treat as 1 bit
        },
        IrType::Vec(n, inner) => *n * ir_type_bit_width(types, *inner),
        IrType::Tuple(parts) => parts.iter().map(|&p| ir_type_bit_width(types, p)).sum(),
        IrType::Block { .. } | IrType::Func { .. } => 32,
        _ => panic!("ir_type_bit_width: unhandled IrType variant — add bit-width calculation"),
    }
}

/// Return the VAFFLE TypeId of a value in a function body.
///
/// A direct single-result `Call` needs the callee's own [`SigDecl`] to type
/// correctly (a `Call`'s own values entry carries no type of its own).
/// `Output` instead is the flat, per-bit projection used by the VAFFLE
/// producers, so its `idx` is a bit index rather than a `SigDecl::results`
/// index. Falls back to `BIT_TID` for it and for shapes with no well-defined
/// single-value scalar type (`StackAlloc`/`PtrLoad`/`PtrStore`/`PtrOffset`,
/// which `lower_function` already treats as `Bit`-typed addresses
/// independently of this helper).
pub(crate) fn vaffle_value_vtid<P: Clone>(
    module: &Module<P>,
    values: &[volar_ir_common::Node<Value, P>],
    vid: ValueId,
) -> TypeId {
    match &values[vid.0].kind {
        Value::Param { ty, .. } => *ty,
        Value::Op(stmt) => stmt_result_vtid(stmt),
        Value::Output { .. } => TypeId(0),
        Value::Call { func, .. } => {
            let results = &sig_of(module, *func).results;
            // A multi-result Call referenced directly (not through an
            // Output) is a malformed value graph — callers should only
            // ever reference a single-result Call this way.
            if results.len() == 1 {
                results[0]
            } else {
                TypeId(0)
            }
        }
        _ => TypeId(0),
    }
}

/// Look up a callee's signature declaration by [`FuncId`].
fn sig_of<P: Clone>(module: &Module<P>, func: FuncId) -> &SigDecl {
    let sig_id = match &module.funcs[func.0] {
        FuncDecl::Import { sig, .. } => *sig,
        FuncDecl::Body(b) => b.sig,
        _ => panic!("sig_of: unexpected FuncDecl variant"),
    };
    &module.sigs[sig_id.0]
}

/// Return the result TypeId of a VAFFLE Stmt.
pub(crate) fn stmt_result_vtid(stmt: &Stmt<ValueId>) -> TypeId {
    match stmt {
        Stmt::Const(_, ty) => *ty,
        Stmt::Poly { ty, .. } => *ty,
        Stmt::Merge { ty, .. } => *ty,
        Stmt::Splat { ty, .. } => *ty,
        Stmt::Transmute { dst_ty, .. } => *dst_ty,
        Stmt::Rol { ty, .. } => *ty,
        Stmt::Ror { ty, .. } => *ty,
        Stmt::Shuffle { ty, .. } => *ty,
        Stmt::StorageRead { ty, .. } => *ty,
        Stmt::StorageWrite { .. } => TypeId(0),
        Stmt::Rng { ty, .. } => *ty,
        Stmt::OracleCall { result_ty, .. } => *result_ty,
        Stmt::OracleOutput { ty, .. } => *ty,
        Stmt::ActionCall { result_ty, .. } => *result_ty,
        Stmt::ActionOutput { ty, .. } => *ty,
        _ => TypeId(0),
    }
}

/// Emit stmts that extract `n_bits` individual `Bit`-typed vars from `ir_var`.
/// If `n_bits == 1` the var is returned as-is (it's already Bit-typed).
fn explode_to_bits<P: Clone>(
    em: &mut BlockEmitter<P>,
    ir_var: IRVarId,
    n_bits: usize,
) -> Vec<IRVarId> {
    if n_bits <= 1 {
        return vec![ir_var];
    }
    (0..n_bits)
        .map(|i| {
            em.emit(IRStmt::Shuffle {
                result_bits: vec![(i as u8, ir_var)],
                ty: BIT_TID,
            })
        })
        .collect()
}

/// Total `StorageId::ALLOCA`-rebased bit-budget a function's own `alloca`s
/// need (see `StorageId::ALLOCA`'s own doc comment for why `alloca` uses a
/// dedicated id rather than `StorageId::STACK` directly): the highest
/// `base_slot + count * bit_width(elem_ty)` across every `Value::StackAlloc`
/// in this function's body; zero for a function with none. Read directly
/// off `Value::StackAlloc`'s own fields rather than scanning `StorageRead`/
/// `StorageWrite` address expressions -- a producer could represent an
/// address as a single scalar `Stmt::Const` or as a `Merge` of individual
/// bit-consts (see `rebase_stack_addr`), but every producer's `StackAlloc`
/// marker carries its own allocation size directly, with no representation
/// ambiguity.
///
/// Distinct from (and additional to) `own_layout.size`, which only covers
/// the calling convention's own register region (params/ret/spill/cross-
/// block-values). A caller advancing SP to make a nested call must skip
/// past *both* -- this is `plan_functions`'s `FuncInfo::alloca_budget`,
/// consulted at every call site (see `lower_function`).
fn compute_alloca_budget<P: Clone>(body: &FuncBody<P>, types: &IRTypes, type_map: &[TypeId]) -> u64 {
    let mut budget = 0u64;
    for node in &body.values {
        if let Value::StackAlloc {
            elem_ty,
            count,
            base_slot,
        } = &node.kind
        {
            let ir_ty = type_map[elem_ty.0 as usize];
            let w = ir_type_bit_width(types, ir_ty) as u64;
            budget = budget.max(base_slot + w * (*count as u64));
        }
    }
    budget
}

/// Rebase a `StorageId::ALLOCA` address value onto this activation's real
/// runtime frame -- `StorageId::STACK` (see that constant's own doc
/// comment): `sp_bits + local_addr`, via the same `bc_add` machinery the
/// calling convention's own spill/reload/param/return addressing already
/// uses (see this file's module doc).
///
/// `local_addr` is whatever the producer already translated the address
/// operand to -- a `Primitive` scalar (`volar-llvm-vaffle-import`'s single
/// `Stmt::Const`) or a `Vec(pointer_bits, Bit)` (a `Merge`-composed bit vector,
/// as `VaffleTarget::alloca` builds one, should a future producer route
/// through `StorageId::ALLOCA` the same way); both decompose to individual
/// bits the same way via `extract_bit`/`Shuffle`, so no producer-specific
/// handling is needed. `sp_bits` is this block's own incoming SP -- exactly where this
/// function's own `own_layout.size` bit-slots end (`frame_sp =
/// StackPtr::new(sp_bits).retreat(own_layout.size)`, see `lower_function`)
/// -- so a fresh alloca's storage starts right past this frame's own
/// region. Critically, this tracks the *actual* runtime SP rather than a
/// fixed literal: on a recursive call each activation's `sp_bits` differs,
/// so each gets its own alloca storage instead of every recursion depth
/// aliasing the same fixed address (which a compile-time-constant "big
/// reserved offset" scheme could never prevent, only defer).
fn rebase_stack_addr<P: Clone>(
    em: &mut BlockEmitter<P>,
    sp_bits: &[IRVarId],
    local_addr: IRVarId,
    pointer_bits: usize,
) -> IRVarId {
    let local_bits: Vec<IRVarId> = (0..pointer_bits as u8)
        .map(|i| em.extract_bit(local_addr, i))
        .collect();
    let real_bits = bc_add(em, &local_bits, sp_bits, false);
    em.compose_address(&real_bits)
}

/// Translate a VAFFLE `Stmt<ValueId>` to an `IRStmt<IRVarId>`.
///
/// `val_map` maps VAFFLE `ValueId` → IR `IRVarId`.
/// `type_map` maps VAFFLE `TypeId` → IR `TypeId` (produced by [`remap_type_id`]).
fn translate_stmt(
    stmt: &volar_ir_common::Stmt<ValueId>,
    val_map: &BTreeMap<usize, IRVarId>,
    type_map: &[TypeId],
) -> IRStmt {
    let s = |vid: &ValueId| val_map.get(&vid.0).copied().unwrap_or(IRVarId(0));
    let t = |tid: &TypeId| type_map[tid.0 as usize];
    let tv = |tids: &[TypeId]| tids.iter().map(t).collect::<Vec<_>>();
    match stmt {
        volar_ir_common::Stmt::Const(c, ty) => IRStmt::Const(*c, t(ty)),
        volar_ir_common::Stmt::Poly {
            ty,
            coeffs,
            constant,
        } => IRStmt::Poly {
            ty: t(ty),
            coeffs: coeffs
                .iter()
                .map(|(vars, &coeff)| {
                    let mut nv: Vec<IRVarId> = vars.iter().map(s).collect();
                    nv.sort();
                    (nv, coeff)
                })
                .collect(),
            constant: *constant,
        },
        volar_ir_common::Stmt::Merge { parts, ty } => IRStmt::Merge {
            parts: parts.iter().map(s).collect(),
            ty: t(ty),
        },
        volar_ir_common::Stmt::Splat { src, ty } => IRStmt::Splat {
            src: s(src),
            ty: t(ty),
        },
        volar_ir_common::Stmt::Transmute {
            src,
            src_ty,
            dst_ty,
        } => IRStmt::Transmute {
            src: s(src),
            src_ty: t(src_ty),
            dst_ty: t(dst_ty),
        },
        volar_ir_common::Stmt::Rol { src, ty, n } => IRStmt::Rol {
            src: s(src),
            ty: t(ty),
            n: *n,
        },
        volar_ir_common::Stmt::Ror { src, ty, n } => IRStmt::Ror {
            src: s(src),
            ty: t(ty),
            n: *n,
        },
        volar_ir_common::Stmt::Shuffle { result_bits, ty } => IRStmt::Shuffle {
            result_bits: result_bits.iter().map(|(b, v)| (*b, s(v))).collect(),
            ty: t(ty),
        },
        volar_ir_common::Stmt::StorageRead { storage, ty, addr } => IRStmt::StorageRead {
            storage: *storage,
            ty: t(ty),
            addr: s(addr),
        },
        volar_ir_common::Stmt::StorageWrite {
            storage,
            src,
            ty,
            addr,
        } => IRStmt::StorageWrite {
            storage: *storage,
            src: s(src),
            ty: t(ty),
            addr: s(addr),
        },
        volar_ir_common::Stmt::Rng { name, ty } => IRStmt::Rng {
            name: name.clone(),
            ty: t(ty),
        },
        volar_ir_common::Stmt::OracleCall {
            name,
            args,
            output_tys,
            result_ty,
        } => IRStmt::OracleCall {
            name: name.clone(),
            args: args.iter().map(s).collect(),
            output_tys: tv(output_tys),
            result_ty: t(result_ty),
        },
        volar_ir_common::Stmt::OracleOutput { call, idx, ty } => IRStmt::OracleOutput {
            call: s(call),
            idx: *idx,
            ty: t(ty),
        },
        volar_ir_common::Stmt::ActionCall {
            name,
            guard,
            args,
            fallbacks,
            output_tys,
            result_ty,
        } => IRStmt::ActionCall {
            name: name.clone(),
            guard: s(guard),
            args: args.iter().map(s).collect(),
            fallbacks: fallbacks.iter().map(s).collect(),
            output_tys: tv(output_tys),
            result_ty: t(result_ty),
        },
        volar_ir_common::Stmt::ActionOutput { call, idx, ty } => IRStmt::ActionOutput {
            call: s(call),
            idx: *idx,
            ty: t(ty),
        },
        _ => panic!("translate_stmt: unhandled Stmt variant — add translation for this variant"),
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    extern crate std;
    use super::*;
    use crate::target::VaffleTarget;
    use volar_lir::{LirTarget, LirType, StackAllocExt};

    #[test]
    fn test_use_counts_follow_the_untranslated_suffix() {
        let bit_tid = TypeId(0);
        let values = vec![
            volar_ir_common::Node::new(
                Value::Param {
                    block: BlockId(0),
                    ty: bit_tid,
                    idx: 0,
                },
                (),
                None,
            ),
            volar_ir_common::Node::new(
                Value::Param {
                    block: BlockId(0),
                    ty: bit_tid,
                    idx: 1,
                },
                (),
                None,
            ),
            // Use v0 twice and v1 once before the call.
            volar_ir_common::Node::new(
                Value::Op(Stmt::Merge {
                    parts: vec![ValueId(0), ValueId(0), ValueId(1)],
                    ty: bit_tid,
                }),
                (),
                None,
            ),
            // The call itself consumes the final use of v0.
            volar_ir_common::Node::new(
                Value::Call {
                    func: FuncId(0),
                    args: vec![ValueId(0)],
                },
                (),
                None,
            ),
        ];
        let term = Terminator::Return {
            values: vec![ValueId(1)],
        };
        let mut uses = collect_use_counts(&values, &[ValueId(2), ValueId(3)], &term);

        assert_eq!(uses.counts[0], 3);
        assert_eq!(uses.counts[1], 2);
        assert_eq!(uses.live_values().collect::<Vec<_>>(), vec![0, 1]);

        uses.consume_value(&values[2].kind);
        assert_eq!(uses.counts[0], 1);
        assert_eq!(uses.counts[1], 1);

        uses.consume_value(&values[3].kind);
        assert!(!uses.contains(0));
        assert!(uses.contains(1), "the terminator still needs v1");
        assert_eq!(uses.live_values().collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn test_use_counts_iterates_live_values_in_value_id_order() {
        let mut uses = UseCounts::new(4);
        uses.add(0);
        uses.add(2);
        uses.add(1);
        uses.add(2);
        assert_eq!(uses.live_values().collect::<Vec<_>>(), vec![0, 1, 2]);

        uses.remove(2);
        assert_eq!(uses.live_values().collect::<Vec<_>>(), vec![0, 1, 2]);
        uses.remove(2);
        assert!(!uses.contains(2));
        assert_eq!(uses.live_values().collect::<Vec<_>>(), vec![0, 1]);

        uses.remove(0);
        assert_eq!(uses.live_values().collect::<Vec<_>>(), vec![1]);
    }

    #[test]
    fn test_lower_vaffle_with_stack_alloc() {
        let mut t = VaffleTarget::new();
        let (entry, params) = t.begin_function("test_alloc", &[LirType::U32], Some(LirType::U32));
        t.switch_to_block(entry);

        let input = params[0][0].clone();

        // Allocate, store the input, load it back.
        let ptr = t.alloca(LirType::U32, 1);
        t.ptr_store(ptr.clone(), input);
        let loaded = t.ptr_load(ptr, LirType::U32);

        t.ret(&[loaded]);
        t.end_function();

        // Lower to IR — this should not panic.
        let (ir_blocks, _ir_types) = lower_vaffle_to_ir(&t.module);

        // Should have at least the entry block, exit continuation, and function blocks.
        assert!(
            ir_blocks.blocks.len() >= 3,
            "expected at least 3 IR blocks, got {}",
            ir_blocks.blocks.len()
        );

        // Verify that STACK StorageRead/Write appear in the lowered IR.
        let has_stack_ops = ir_blocks.blocks.iter().any(|block| {
            block.stmts.iter().any(|stmt| {
                matches!(
                    &stmt.kind,
                    IRStmt::StorageRead { storage, .. } | IRStmt::StorageWrite { storage, .. }
                        if *storage == StorageId::STACK
                )
            })
        });
        assert!(has_stack_ops, "lowered IR should contain STACK storage ops");
    }

    /// Regression test for the `alloca` / calling-convention frame collision:
    /// a function with its own `alloca` that *also* makes a nested call must
    /// have that call's SP advancement skip past its own alloca budget, not
    /// just the callee's own register region -- otherwise the callee's own
    /// frame (params/ret/spill/cont) would be placed on top of the caller's
    /// still-live alloca storage. See `FuncInfo::alloca_budget` and its use
    /// at the call site in `lower_function`.
    ///
    /// `func0` allocates 1 stack bit (`base_slot = 0`), stores its own param
    /// there, calls `func1`, then reloads from the same address. There is no
    /// end-to-end numeric evaluator for call-preserving cross-function Volar
    /// IR yet (`eval_ir`/`unroll_ir`/`movfuscate` all reject *any* real
    /// inter-function call as "not statically finite" -- a pre-existing gap,
    /// unrelated to alloca, confirmed reproducible with zero allocas
    /// involved), so this checks `plan_functions`'s computed budget directly
    /// and that lowering the full call+alloca combination doesn't panic.
    #[test]
    fn test_alloca_budget_reserved_across_nested_call() {
        use vaffle::*;
        use volar_ir_common::Stmt;

        let mut types = volar_ir_common::TypeTable::new();
        let bit_tid = types.intern(volar_ir_common::IrType::Primitive(
            volar_ir_common::Type::Bit,
        ));
        let addr_tid = types.intern(volar_ir_common::IrType::Primitive(
            volar_ir_common::Type::_32,
        ));

        let sig0 = SigDecl {
            params: vec![bit_tid],
            results: vec![bit_tid],
        };
        let sig1 = SigDecl {
            params: vec![bit_tid],
            results: vec![bit_tid],
        };

        // func1: identity (return param).
        let body1 = FuncBody {
            sig: SigId(1),
            blocks: std::vec![Block {
                params: std::vec![(ValueId(0), bit_tid)],
                stmts: std::vec![],
                terminator: Terminator::Return {
                    values: std::vec![ValueId(0)],
                },
            }],
            values: std::vec![volar_ir_common::Node::new(
                Value::Param {
                    block: BlockId(0),
                    ty: bit_tid,
                    idx: 0,
                },
                (),
                None,
            )],
            entry: BlockId(0),
        };

        // func0: alloca 1 bit at base_slot 0; store its own param there;
        // call func1; reload from the same address; return the reloaded bit.
        let mut vals0 = std::vec::Vec::new();
        vals0.push(Value::Param {
            block: BlockId(0),
            ty: bit_tid,
            idx: 0,
        }); // 0
        vals0.push(Value::StackAlloc {
            elem_ty: bit_tid,
            count: 1,
            base_slot: 0,
        }); // 1
        vals0.push(Value::Op(Stmt::Const(
            Constant { hi: 0, lo: 0 },
            addr_tid,
        ))); // 2: store address
        vals0.push(Value::Op(Stmt::StorageWrite {
            storage: StorageId::ALLOCA,
            src: ValueId(0),
            ty: bit_tid,
            addr: ValueId(2),
        })); // 3
        vals0.push(Value::Call {
            func: FuncId(1),
            args: std::vec![ValueId(0)],
        }); // 4
        vals0.push(Value::Op(Stmt::Const(
            Constant { hi: 0, lo: 0 },
            addr_tid,
        ))); // 5: reload address
        vals0.push(Value::Op(Stmt::StorageRead {
            storage: StorageId::ALLOCA,
            ty: bit_tid,
            addr: ValueId(5),
        })); // 6
        let body0 = FuncBody {
            sig: SigId(0),
            blocks: std::vec![Block {
                params: std::vec![(ValueId(0), bit_tid)],
                stmts: std::vec![
                    ValueId(1),
                    ValueId(2),
                    ValueId(3),
                    ValueId(4),
                    ValueId(5),
                    ValueId(6),
                ],
                terminator: Terminator::Return {
                    values: std::vec![ValueId(6)],
                },
            }],
            values: vals0
                .into_iter()
                .map(|v| volar_ir_common::Node::new(v, (), None))
                .collect(),
            entry: BlockId(0),
        };

        let module = vaffle::Module {
            pointer_width: vaffle::PointerWidth::Bits64,
            types,
            oracles: std::vec![],
            actions: std::vec![],
            funcs: std::vec![vaffle::FuncDecl::Body(body0), vaffle::FuncDecl::Body(body1)],
            sigs: std::vec![sig0, sig1],
            exports: alloc::collections::BTreeMap::new(),
            pre_init: std::vec![],
        };

        let mut ctx = LowerCtx::new(&module);
        ctx.plan_functions();
        assert_eq!(
            ctx.func_info[0].alloca_budget, 1,
            "func0's only stack access is bit address 0 -> budget 1"
        );
        assert_eq!(
            ctx.func_info[1].alloca_budget, 0,
            "func1 has no StorageId::ALLOCA access of its own"
        );

        let (ir_blocks, _ir_types) = lower_vaffle_to_ir(&module);
        assert!(
            ir_blocks.blocks.len() >= 4,
            "expected >=4 blocks (entry + exit + func0 + func1), got {}",
            ir_blocks.blocks.len()
        );

        // The lowered output must have re-tagged every `StorageId::ALLOCA`
        // access as `StorageId::STACK` (that's genuinely where the rebased
        // data lives) -- and must contain none of the original `ALLOCA`
        // tag, which only exists pre-lowering as a rebasing marker.
        let has_rebased_stack_op = ir_blocks.blocks.iter().any(|b| {
            b.stmts.iter().any(|s| {
                matches!(&s.kind,
                    IRStmt::StorageRead { storage, .. } | IRStmt::StorageWrite { storage, .. }
                    if *storage == StorageId::STACK)
            })
        });
        assert!(
            has_rebased_stack_op,
            "expected the alloca's StorageId::ALLOCA access to be re-tagged StorageId::STACK"
        );
        let has_leftover_alloca_tag = ir_blocks.blocks.iter().any(|b| {
            b.stmts.iter().any(|s| {
                matches!(&s.kind,
                    IRStmt::StorageRead { storage, .. } | IRStmt::StorageWrite { storage, .. }
                    if *storage == StorageId::ALLOCA)
            })
        });
        assert!(
            !has_leftover_alloca_tag,
            "StorageId::ALLOCA must not leak into the lowered output"
        );
    }

    /// Verify that block params use packed words (PACK_TID) instead of
    /// individual Bit types.
    #[test]
    fn test_packed_block_params() {
        let mut t = VaffleTarget::new();
        let (entry, _params) = t.begin_function("packed", &[LirType::Bool], None);
        t.switch_to_block(entry);
        t.ret(&[]);
        t.end_function();

        let (ir_blocks, ir_types) = lower_vaffle_to_ir(&t.module);

        // Block 0 is the module trampoline: packed entry-function params
        // (circuit inputs). Block 2 is the function's entry block.  Its
        // params should be ceil(pointer_bits / PACK_W) packed SP words +
        // ceil(1 / PACK_W) param words (Bool = 1 bit, packed into 1 PACK_TID
        // word). The trampoline jump must pass SP + those same param words.
        assert!(ir_blocks.blocks.len() >= 3);
        assert_eq!(
            ir_blocks.blocks[0].params.len(),
            1,
            "trampoline should carry the packed Bool param"
        );
        match &ir_blocks.blocks[0].terminator {
            IRTerminator::Jmp { target } => {
                assert_eq!(
                    target.args.len(),
                    ir_blocks.blocks[2].params.len(),
                    "trampoline jump arity must match function entry params"
                );
            }
            other => panic!("expected trampoline Jmp, got {other:?}"),
        }
        let func_entry = &ir_blocks.blocks[2];
        let sp_packs = (t.pointer_width().bits() + PACK_W - 1) / PACK_W;
        let bool_packs = 1_usize; // ceil(1 bit / PACK_W) = 1
        assert_eq!(
            func_entry.params.len(),
            sp_packs + bool_packs,
            "function entry block should have {} packed params (SP + Bool), got {}",
            sp_packs + bool_packs,
            func_entry.params.len()
        );
        // All params should be PACK_TID.
        for (i, &tid) in func_entry.params.iter().enumerate() {
            assert_eq!(
                tid, PACK_TID,
                "param {} should be PACK_TID (Vec({}, Bit))",
                i, PACK_W
            );
        }
    }

    /// Verify that the Merge and Shuffle stmts appear in the lowered IR
    /// (evidence of pack/unpack operations).
    #[test]
    fn test_pack_unpack_stmts_present() {
        let mut t = VaffleTarget::new();
        let (entry, _params) = t.begin_function("pu", &[], None);
        t.switch_to_block(entry);
        t.ret(&[]);
        t.end_function();

        let (ir_blocks, _) = lower_vaffle_to_ir_with_control_provenance(&t.module, &());

        // The entry block (block 0) should contain at least one Merge
        // (packing SP bits for the jump to the function entry).
        let has_merge = ir_blocks.blocks[0]
            .stmts
            .iter()
            .any(|s| matches!(&s.kind, IRStmt::Merge { ty, .. } if *ty == PACK_TID));
        assert!(
            has_merge,
            "entry block should contain a Merge (pack) with PACK_TID"
        );

        // The function entry block should contain Shuffle stmts (unpacking SP).
        let func_entry = &ir_blocks.blocks[2];
        let has_shuffle = func_entry
            .stmts
            .iter()
            .any(|s| matches!(&s.kind, IRStmt::Shuffle { ty, .. } if *ty == BIT_TID));
        assert!(
            has_shuffle,
            "function entry should contain Shuffle (unpack) stmts"
        );
    }

    /// Verify that spill/reload uses packed words, and that only values that
    /// are actually live after a call are spilled (not all values in val_map).
    ///
    /// Scenario: func0 calls func1 with its only param and immediately returns
    /// the result.  At the call site the param is consumed as a call argument;
    /// it is *not* referenced after the call.  So zero values should be spilled.
    #[test]
    fn test_packed_spill_reload() {
        use vaffle::*;
        use volar_ir_common::Stmt;

        // Build a VAFFLE module with two functions; func0 calls func1.
        let mut types = volar_ir_common::TypeTable::new();
        let bit_tid = types.intern(volar_ir_common::IrType::Primitive(
            volar_ir_common::Type::Bit,
        ));

        let sig0 = SigDecl {
            params: vec![bit_tid],
            results: vec![bit_tid],
        };
        let sig1 = SigDecl {
            params: vec![bit_tid],
            results: vec![bit_tid],
        };

        // func1: identity (return param)
        let mut vals1 = std::vec::Vec::new();
        vals1.push(Value::Param {
            block: BlockId(0),
            ty: bit_tid,
            idx: 0,
        });
        let body1 = FuncBody {
            sig: SigId(1),
            blocks: std::vec![Block {
                params: std::vec![(ValueId(0), bit_tid)],
                stmts: std::vec![],
                terminator: Terminator::Return {
                    values: std::vec![ValueId(0)]
                },
            }],
            values: vals1
                .into_iter()
                .map(|v| volar_ir_common::Node::new(v, (), None))
                .collect(),
            entry: BlockId(0),
        };

        // func0: call func1 with its param, return result.
        // The param (ValueId(0)) is NOT used after the call — so zero spills.
        let mut vals0 = std::vec::Vec::new();
        vals0.push(Value::Param {
            block: BlockId(0),
            ty: bit_tid,
            idx: 0,
        });
        vals0.push(Value::Call {
            func: FuncId(1),
            args: std::vec![ValueId(0)],
        });
        vals0.push(Value::Output {
            value: ValueId(1),
            idx: 0,
        });
        let body0 = FuncBody {
            sig: SigId(0),
            blocks: std::vec![Block {
                params: std::vec![(ValueId(0), bit_tid)],
                stmts: std::vec![ValueId(1), ValueId(2)],
                terminator: Terminator::Return {
                    values: std::vec![ValueId(2)]
                },
            }],
            values: vals0
                .into_iter()
                .map(|v| volar_ir_common::Node::new(v, (), None))
                .collect(),
            entry: BlockId(0),
        };

        let module = vaffle::Module {
            pointer_width: vaffle::PointerWidth::Bits64,
            types,
            oracles: std::vec![],
            actions: std::vec![],
            funcs: std::vec![vaffle::FuncDecl::Body(body0), vaffle::FuncDecl::Body(body1),],
            sigs: std::vec![sig0, sig1],
            exports: alloc::collections::BTreeMap::new(),
            pre_init: std::vec![],
        };

        let (ir_blocks, _ir_types) = lower_vaffle_to_ir(&module);

        assert!(
            ir_blocks.blocks.len() >= 4,
            "expected ≥4 blocks (entry + exit + func0 + func1), got {}",
            ir_blocks.blocks.len()
        );

        // With selective spilling, the number of STACK+PACK_TID StorageWrites
        // should equal the number from arg-pushes and ret-writes only (the
        // frame protocol), with zero additional writes from spills.
        //
        // Concretely: func0 pushes 1 call-arg word and writes 1 ret word;
        // func1 writes 1 ret word → 3 total.  Before this optimisation there
        // would have been extra spill writes on top of that.  We verify that
        // the total does not exceed 3 (i.e. no spill writes were added).
        let stack_pack_writes: std::vec::Vec<_> = ir_blocks
            .blocks
            .iter()
            .flat_map(|b| b.stmts.iter())
            .filter(|s| {
                matches!(&s.kind, IRStmt::StorageWrite { storage, ty, .. }
                if *storage == StorageId::STACK && *ty == PACK_TID)
            })
            .collect();
        assert!(
            stack_pack_writes.len() <= 3,
            "expected at most 3 STACK+PACK_TID writes (arg + 2 rets); \
             got {} — likely a regression adding unnecessary spill writes",
            stack_pack_writes.len()
        );
    }

    /// Verify that a value which IS live across a call gets spilled and
    /// reloaded using packed words.
    ///
    /// Scenario: func0 has a local value `local` (NOT an arg to the call) that
    /// is used AFTER the call returns.  At the call site `local` must be
    /// spilled; the reload should use PACK_TID-typed StorageRead.
    #[test]
    fn test_selective_spill() {
        use vaffle::*;
        use volar_ir_common::{Constant, Stmt};

        let mut types = volar_ir_common::TypeTable::new();
        let bit_tid = types.intern(volar_ir_common::IrType::Primitive(
            volar_ir_common::Type::Bit,
        ));

        // sig0: (bit) -> bit
        // sig1: (bit) -> bit  (identity)
        let sig0 = SigDecl {
            params: vec![bit_tid],
            results: vec![bit_tid],
        };
        let sig1 = SigDecl {
            params: vec![bit_tid],
            results: vec![bit_tid],
        };

        // func1: identity — return its param.
        let mut vals1 = std::vec::Vec::new();
        vals1.push(Value::Param {
            block: BlockId(0),
            ty: bit_tid,
            idx: 0,
        });
        let body1 = FuncBody {
            sig: SigId(1),
            blocks: std::vec![Block {
                params: std::vec![(ValueId(0), bit_tid)],
                stmts: std::vec![],
                terminator: Terminator::Return {
                    values: std::vec![ValueId(0)]
                },
            }],
            values: vals1
                .into_iter()
                .map(|v| volar_ir_common::Node::new(v, (), None))
                .collect(),
            entry: BlockId(0),
        };

        // func0:
        //   param   = ValueId(0)   (bit input)
        //   local   = ValueId(1)   = const 1  (NOT used as call arg)
        //   call    = ValueId(2)   = call func1(param)
        //   out     = ValueId(3)   = Output(call, 0)
        //   xor_res = ValueId(4)   = local XOR out   (uses local AFTER the call)
        //   return xor_res
        //
        // At the call site, `local` is in val_map and is referenced by
        // ValueId(4) which comes after the call → `local` IS live → must spill.
        let mut vals0 = std::vec::Vec::new();
        vals0.push(Value::Param {
            block: BlockId(0),
            ty: bit_tid,
            idx: 0,
        }); // 0
        vals0.push(Value::Op(Stmt::Const(
            // 1 = local
            Constant { hi: 0, lo: 1 },
            bit_tid,
        )));
        vals0.push(Value::Call {
            func: FuncId(1),
            args: std::vec![ValueId(0)],
        }); // 2
        vals0.push(Value::Output {
            value: ValueId(2),
            idx: 0,
        }); // 3 = out
        {
            // xor_res = local XOR out  (degree-1 polynomial: local + out)
            let mut coeffs = alloc::collections::BTreeMap::new();
            coeffs.insert(std::vec![ValueId(1)], 1u8);
            coeffs.insert(std::vec![ValueId(3)], 1u8);
            vals0.push(Value::Op(Stmt::Poly {
                // 4 = xor_res
                ty: bit_tid,
                coeffs,
                constant: volar_ir_common::Constant { hi: 0, lo: 0 },
            }));
        }

        let body0 = FuncBody {
            sig: SigId(0),
            blocks: std::vec![Block {
                params: std::vec![(ValueId(0), bit_tid)],
                stmts: std::vec![ValueId(1), ValueId(2), ValueId(3), ValueId(4)],
                terminator: Terminator::Return {
                    values: std::vec![ValueId(4)]
                },
            }],
            values: vals0
                .into_iter()
                .map(|v| volar_ir_common::Node::new(v, (), None))
                .collect(),
            entry: BlockId(0),
        };

        let module = vaffle::Module {
            pointer_width: vaffle::PointerWidth::Bits64,
            types,
            oracles: std::vec![],
            actions: std::vec![],
            funcs: std::vec![vaffle::FuncDecl::Body(body0), vaffle::FuncDecl::Body(body1),],
            sigs: std::vec![sig0, sig1],
            exports: alloc::collections::BTreeMap::new(),
            pre_init: std::vec![],
        };

        let (ir_blocks, _ir_types) = lower_vaffle_to_ir(&module);

        assert!(
            ir_blocks.blocks.len() >= 4,
            "expected ≥4 IR blocks, got {}",
            ir_blocks.blocks.len()
        );

        // `local` (ValueId(1)) is live across the call → at least one
        // PACK_TID-typed StorageWrite to STACK must appear (the spill).
        let spill_writes: std::vec::Vec<_> = ir_blocks
            .blocks
            .iter()
            .flat_map(|b| b.stmts.iter())
            .filter(|s| {
                matches!(&s.kind, IRStmt::StorageWrite { storage, ty, .. }
                if *storage == StorageId::STACK && *ty == PACK_TID)
            })
            .collect();
        assert!(
            !spill_writes.is_empty(),
            "`local` is live across the call and must be spilled (PACK_TID StorageWrite)"
        );

        // Matching reload: PACK_TID-typed StorageRead from STACK in the
        // continuation block.
        let reload_reads: std::vec::Vec<_> = ir_blocks
            .blocks
            .iter()
            .flat_map(|b| b.stmts.iter())
            .filter(|s| {
                matches!(&s.kind, IRStmt::StorageRead { storage, ty, .. }
                if *storage == StorageId::STACK && *ty == PACK_TID)
            })
            .collect();
        assert!(
            !reload_reads.is_empty(),
            "spilled `local` must be reloaded (PACK_TID StorageRead)"
        );
    }

    /// A `Terminator::ReturnCall` should lower to a direct jump to the callee's
    /// entry block, not to a Dyn continuation.
    ///
    /// Scenario: func0 tail-calls func1.  The lowered IR for func0's block
    /// should terminate with a `Jmp` to `Block(func1_entry)`, NOT `Dyn(...)`.
    #[test]
    fn test_return_call_lowers_to_direct_jump() {
        use vaffle::*;

        let mut types = volar_ir_common::TypeTable::new();
        let bit_tid = types.intern(volar_ir_common::IrType::Primitive(
            volar_ir_common::Type::Bit,
        ));

        let sig0 = SigDecl {
            params: std::vec![bit_tid],
            results: std::vec![bit_tid],
        };
        let sig1 = SigDecl {
            params: std::vec![bit_tid],
            results: std::vec![bit_tid],
        };

        // func1: identity function.
        let mut vals1 = std::vec::Vec::new();
        vals1.push(Value::Param {
            block: BlockId(0),
            ty: bit_tid,
            idx: 0,
        });
        let body1: FuncBody<()> = FuncBody {
            sig: SigId(1),
            blocks: std::vec![Block {
                params: std::vec![(ValueId(0), bit_tid)],
                stmts: std::vec![],
                terminator: Terminator::Return {
                    values: std::vec![ValueId(0)]
                },
            }],
            values: vals1
                .into_iter()
                .map(|v| volar_ir_common::Node::new(v, (), None))
                .collect(),
            entry: BlockId(0),
        };

        // func0: tail-calls func1 with its own param.
        let mut vals0 = std::vec::Vec::new();
        vals0.push(Value::Param {
            block: BlockId(0),
            ty: bit_tid,
            idx: 0,
        });
        let body0: FuncBody<()> = FuncBody {
            sig: SigId(0),
            blocks: std::vec![Block {
                params: std::vec![(ValueId(0), bit_tid)],
                stmts: std::vec![],
                terminator: Terminator::ReturnCall {
                    func: FuncId(1),
                    args: std::vec![ValueId(0)],
                },
            }],
            values: vals0
                .into_iter()
                .map(|v| volar_ir_common::Node::new(v, (), None))
                .collect(),
            entry: BlockId(0),
        };

        let module = vaffle::Module {
            pointer_width: vaffle::PointerWidth::Bits64,
            types,
            oracles: std::vec![],
            actions: std::vec![],
            funcs: std::vec![vaffle::FuncDecl::Body(body0), vaffle::FuncDecl::Body(body1),],
            sigs: std::vec![sig0, sig1],
            exports: alloc::collections::BTreeMap::new(),
            pre_init: std::vec![],
        };

        let (ir_blocks, _ir_types) = lower_vaffle_to_ir(&module);

        // Layout: block 0 = module entry, block 1 = exit cont,
        //         block 2 = func0 entry, block 3 = func1 entry.
        assert!(
            ir_blocks.blocks.len() >= 4,
            "expected ≥4 IR blocks, got {}",
            ir_blocks.blocks.len()
        );

        // func1's entry block index should be 3.
        let func1_entry = IRBlockId(3);

        // func0's entry block (block 2) terminator must be a direct Jmp to func1.
        let func0_block = &ir_blocks.blocks[2];
        match &func0_block.terminator {
            IRTerminator::Jmp {
                target:
                    IRBranchTarget {
                        dest: IRBlockTargetId::Block(target),
                        ..
                    },
            } => {
                assert_eq!(
                    *target, func1_entry,
                    "ReturnCall should jump directly to func1's entry block"
                );
            }
            other => panic!("expected Jmp to Block(func1_entry), got {:?}", other),
        }

        // No extra blocks should be added (no call-site continuation split).
        assert_eq!(
            ir_blocks.blocks.len(),
            4,
            "tail call should not generate extra continuation blocks"
        );
    }

    /// A `Terminator::ReturnCall` should not emit any STACK+PACK_TID
    /// StorageWrites for spilling (there is no live-across-call state to
    /// preserve), and should not emit a continuation-write (the caller's
    /// continuation is reused).
    #[test]
    fn test_return_call_no_spill_no_cont_write() {
        use vaffle::*;

        let mut types = volar_ir_common::TypeTable::new();
        let bit_tid = types.intern(volar_ir_common::IrType::Primitive(
            volar_ir_common::Type::Bit,
        ));

        let sig0 = SigDecl {
            params: std::vec![bit_tid],
            results: std::vec![bit_tid],
        };
        let sig1 = SigDecl {
            params: std::vec![bit_tid],
            results: std::vec![bit_tid],
        };

        let mut vals1 = std::vec::Vec::new();
        vals1.push(Value::Param {
            block: BlockId(0),
            ty: bit_tid,
            idx: 0,
        });
        let body1: FuncBody<()> = FuncBody {
            sig: SigId(1),
            blocks: std::vec![Block {
                params: std::vec![(ValueId(0), bit_tid)],
                stmts: std::vec![],
                terminator: Terminator::Return {
                    values: std::vec![ValueId(0)]
                },
            }],
            values: vals1
                .into_iter()
                .map(|v| volar_ir_common::Node::new(v, (), None))
                .collect(),
            entry: BlockId(0),
        };

        let mut vals0 = std::vec::Vec::new();
        vals0.push(Value::Param {
            block: BlockId(0),
            ty: bit_tid,
            idx: 0,
        });
        let body0: FuncBody<()> = FuncBody {
            sig: SigId(0),
            blocks: std::vec![Block {
                params: std::vec![(ValueId(0), bit_tid)],
                stmts: std::vec![],
                terminator: Terminator::ReturnCall {
                    func: FuncId(1),
                    args: std::vec![ValueId(0)],
                },
            }],
            values: vals0
                .into_iter()
                .map(|v| volar_ir_common::Node::new(v, (), None))
                .collect(),
            entry: BlockId(0),
        };

        let module = vaffle::Module {
            pointer_width: vaffle::PointerWidth::Bits64,
            types,
            oracles: std::vec![],
            actions: std::vec![],
            funcs: std::vec![vaffle::FuncDecl::Body(body0), vaffle::FuncDecl::Body(body1),],
            sigs: std::vec![sig0, sig1],
            exports: alloc::collections::BTreeMap::new(),
            pre_init: std::vec![],
        };

        let (ir_blocks, _) = lower_vaffle_to_ir(&module);

        // Count Block-typed StorageWrites in func0's block (block 2).
        // A regular call writes one Block-typed cont; a tail call must NOT.
        let func0_block = &ir_blocks.blocks[2];
        let block_writes = func0_block
            .stmts
            .iter()
            .filter(|s| {
                matches!(
                    &s.kind, IRStmt::StorageWrite { ty, .. } if *ty != PACK_TID && *ty != BIT_TID
                )
            })
            .count();
        assert_eq!(
            block_writes, 0,
            "tail call must not write a new continuation (Block-lane write count = {})",
            block_writes
        );
    }
}
