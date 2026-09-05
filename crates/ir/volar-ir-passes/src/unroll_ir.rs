// @reliability: experimental
// @ai: assisted
//! Concrete-control unroll-everything pass for Volar IR.
//!
//! Dual of [`crate::movfuscate`]: movfuscation accepts arbitrary (including
//! symbolic) control flow and produces a single *looping* block
//! (`is_movfuscated()`). This pass requires the same concrete-control contract
//! as LLVM-direct import (`volar-llvm-import-core` / `ControlMode::Concrete`):
//! every branch discriminator and every storage address along the walked path
//! must fold to a compile-time constant, and the expansion must be finite.
//! Data values may stay symbolic. The result is one **non-looped** block
//! (`is_circuit()`, terminator `Jmp Return`).
//!
//! This is *not* [`crate::lower_to_circuit`], which MUX-unrolls a movfuscated
//! self-loop with a numeric budget and is valid for a symbolic PC.
//!
//! Error cases align with `volar-circuit-exec-core::ControlError`:
//! `NonFiniteControl` (repeating a `(block, concrete-env)` key) and
//! `ResourceLimit` (compiler-resource caps, not a bounded unroll of a
//! still-symbolic loop).

use alloc::{
    collections::{BTreeMap, BTreeSet},
    vec,
    vec::Vec,
};
use core::convert::Infallible;
use core::fmt;

use volar_ir::ir::{
    IRBlock, IRBlockId, IRBlockTargetId, IRBlocks, IRBranchTarget, IRTerminator, IRTypeId,
    IRTypes, IRVarId,
};
use volar_ir_common::{Constant, PolyCoeffs, Stmt, StorageId};
use volar_ir_opt::common::{
    constant_or, constant_rol, constant_ror, constant_shl, mask_constant, stmt_output_type,
    type_bit_width,
};
use volar_ir_opt::ir::fold_ir_blocks;

/// Why [`unroll_ir_everything`] could not produce a combinational circuit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnrollError {
    /// `JumpCond` discriminator did not fold to a constant.
    SymbolicBranch { block: IRBlockId },
    /// `JumpTable` index did not fold to a constant, or no matching case.
    SymbolicSwitch { block: IRBlockId },
    /// A `StorageRead`/`StorageWrite` address did not fold to a constant.
    SymbolicAddress { block: IRBlockId },
    /// Re-entered a block with the same concrete parameter fingerprint
    /// before returning — a loop whose trip does not depend on a changing
    /// compile-time constant (or an infinite loop).
    NonFiniteControl,
    /// [`UnrollLimits`] was exceeded. Compiler-resource only; does not
    /// change program semantics.
    ResourceLimit,
}

impl fmt::Display for UnrollError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnrollError::SymbolicBranch { block } => {
                write!(f, "symbolic branch condition in block {}", block.0)
            }
            UnrollError::SymbolicSwitch { block } => {
                write!(f, "symbolic switch index in block {}", block.0)
            }
            UnrollError::SymbolicAddress { block } => {
                write!(f, "symbolic storage address in block {}", block.0)
            }
            UnrollError::NonFiniteControl => {
                write!(f, "control is not statically finite")
            }
            UnrollError::ResourceLimit => {
                write!(f, "unroll resource limit exceeded")
            }
        }
    }
}

impl core::error::Error for UnrollError {}

/// Compiler-resource caps for [`unroll_ir_everything_with_limits`].
///
/// These never change execution semantics: exceeding them is
/// [`UnrollError::ResourceLimit`], not a budgeted MUX-unroll.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct UnrollLimits {
    /// Maximum distinct `(block, concrete-env)` keys visited.
    pub max_states: usize,
    /// Maximum block-entry steps admitted to the expansion.
    pub max_steps: usize,
}

impl Default for UnrollLimits {
    fn default() -> Self {
        Self {
            max_states: 4096,
            max_steps: 100_000,
        }
    }
}

/// Unroll `blocks` into a single combinational circuit under the default
/// resource caps. See [`unroll_ir_everything_with_limits`].
pub fn unroll_ir_everything<P: Clone>(
    blocks: &IRBlocks<P>,
    types: &IRTypes,
) -> Result<IRBlocks<P>, UnrollError> {
    unroll_ir_everything_with_limits(blocks, types, UnrollLimits::default())
}

/// Unroll `blocks` into a single `is_circuit()` block by walking only
/// concrete-control edges and splicing taken paths.
///
/// Entry-block parameters stay symbolic (free circuit inputs). If `blocks`
/// is already a circuit, it is cloned unchanged.
pub fn unroll_ir_everything_with_limits<P: Clone>(
    blocks: &IRBlocks<P>,
    types: &IRTypes,
    limits: UnrollLimits,
) -> Result<IRBlocks<P>, UnrollError> {
    if blocks.is_circuit() {
        return Ok(blocks.clone());
    }
    if blocks.blocks.is_empty() {
        return Ok(blocks.clone());
    }

    let entry = &blocks.blocks[0];
    let mut dest = IRBlock {
        params: entry.params.clone(),
        stmts: vec![],
        terminator: IRTerminator::Jmp {
            target: IRBranchTarget::new(IRBlockTargetId::Return, vec![]),
        },
    };

    let entry_args: Vec<IRVarId> = (0..entry.params.len()).map(|i| IRVarId(i as u32)).collect();

    let mut visited: BTreeSet<(u32, Vec<Option<Constant>>)> = BTreeSet::new();
    let mut steps = 0usize;

    let return_args = walk(
        blocks,
        types,
        &mut dest,
        IRBlockId(0),
        &entry_args,
        &mut visited,
        &mut steps,
        limits,
    )?;

    dest.terminator = IRTerminator::Jmp {
        target: IRBranchTarget::new(IRBlockTargetId::Return, return_args),
    };

    let mut out = IRBlocks {
        oracles: blocks.oracles.clone(),
        actions: blocks.actions.clone(),
        rngs: blocks.rngs.clone(),
        blocks: vec![dest],
        pre_init: blocks.pre_init.clone(),
    };
    fold_ir_blocks(&mut out, types);
    Ok(out)
}

fn walk<P: Clone>(
    blocks: &IRBlocks<P>,
    types: &IRTypes,
    dest: &mut IRBlock<P>,
    block_id: IRBlockId,
    args: &[IRVarId],
    visited: &mut BTreeSet<(u32, Vec<Option<Constant>>)>,
    steps: &mut usize,
    limits: UnrollLimits,
) -> Result<Vec<IRVarId>, UnrollError> {
    if *steps >= limits.max_steps {
        return Err(UnrollError::ResourceLimit);
    }
    *steps += 1;

    let block = blocks
        .blocks
        .get(block_id.0 as usize)
        .ok_or(UnrollError::NonFiniteControl)?;
    if args.len() != block.params.len() {
        return Err(UnrollError::NonFiniteControl);
    }

    let fingerprint: Vec<Option<Constant>> = {
        let consts = concrete_consts(dest, types);
        args.iter().map(|&v| consts.get(&v.0).copied()).collect()
    };
    let key = (block_id.0, fingerprint);
    if visited.contains(&key) {
        return Err(UnrollError::NonFiniteControl);
    }
    if visited.len() >= limits.max_states {
        return Err(UnrollError::ResourceLimit);
    }
    visited.insert(key);

    let mut remap: BTreeMap<u32, IRVarId> = BTreeMap::new();
    for (i, &arg) in args.iter().enumerate() {
        remap.insert(i as u32, arg);
    }

    for (i, stmt) in block.stmts.iter().enumerate() {
        let old_vid = block.params.len() as u32 + i as u32;
        let remapped = stmt
            .kind
            .clone()
            .map_var(
                &mut remap,
                &mut |r, v: IRVarId| Ok::<_, Infallible>(*r.get(&v.0).unwrap_or(&v)),
                &mut |_, ty| Ok(ty),
                &mut |_, s| Ok(s),
            )
            .unwrap();

        let new_vid = dest.push_stmt_with_side(remapped, stmt.prov.clone(), stmt.side);
        remap.insert(old_vid, new_vid);
    }

    fold_dest_mut(dest, types);
    let consts = concrete_consts(dest, types);
    for i in 0..dest.stmts.len() {
        let addr = match &dest.stmts[i].kind {
            Stmt::StorageRead { addr, .. } | Stmt::StorageWrite { addr, .. } => Some(*addr),
            _ => None,
        };
        if let Some(addr) = addr {
            if consts.get(&addr.0).is_none() {
                return Err(UnrollError::SymbolicAddress { block: block_id });
            }
        }
    }

    let term = remap_terminator(&block.terminator, &remap);
    dispatch_terminator(blocks, types, dest, block_id, &term, visited, steps, limits)
}

fn fold_dest_mut<P: Clone>(dest: &mut IRBlock<P>, types: &IRTypes) {
    let mut tmp = IRBlocks::new(vec![IRBlock {
        params: dest.params.clone(),
        stmts: core::mem::take(&mut dest.stmts),
        terminator: dest.terminator.clone(),
    }]);
    fold_ir_blocks(&mut tmp, types);
    dest.stmts = tmp.blocks.pop().unwrap().stmts;
}

#[cfg(test)]
fn lookup_const<P: Clone>(dest: &IRBlock<P>, vid: IRVarId, types: &IRTypes) -> Option<Constant> {
    concrete_consts(dest, types).get(&vid.0).copied()
}

/// Constants visible in `dest`: `Const` stmts, const Merge/Shuffle/Poly,
/// and values recovered by forwarding a `StorageWrite` of a constant to a
/// later `StorageRead` at the same concrete address. Needed so Dyn
/// continuations and stack addresses from `lower_vaffle_to_ir` fold
/// during the walk (`fold_ir_blocks` does not rewrite Shuffle to Const).
fn concrete_consts<P: Clone>(dest: &IRBlock<P>, types: &IRTypes) -> BTreeMap<u32, Constant> {
    let mut consts = BTreeMap::new();
    let mut type_map: BTreeMap<u32, IRTypeId> = BTreeMap::new();
    let mut mem: BTreeMap<(StorageId, IRTypeId, u128), Constant> = BTreeMap::new();
    let base = dest.params.len() as u32;
    for (i, &tid) in dest.params.iter().enumerate() {
        type_map.insert(i as u32, tid);
    }
    for (i, stmt) in dest.stmts.iter().enumerate() {
        let vid = base + i as u32;
        if let Some(ty) = stmt_output_type(&stmt.kind) {
            type_map.insert(vid, ty);
        }
        match &stmt.kind {
            Stmt::Const(c, _) => {
                consts.insert(vid, *c);
            }
            Stmt::Merge { parts, ty } => {
                if let Some(c) = eval_merge(parts, *ty, &consts, &type_map, types) {
                    consts.insert(vid, c);
                }
            }
            Stmt::Shuffle { result_bits, .. } => {
                if let Some(c) = eval_shuffle(result_bits, &consts) {
                    consts.insert(vid, c);
                }
            }
            Stmt::Transmute { src, dst_ty, .. } => {
                if let (Some(&c), Some(w)) = (consts.get(&src.0), type_bit_width(*dst_ty, types)) {
                    consts.insert(vid, mask_constant(c, w));
                }
            }
            Stmt::Splat { src, ty } => {
                if let (Some(&c), Some(w)) = (consts.get(&src.0), type_bit_width(*ty, types)) {
                    let result = if c.lo & 1 != 0 {
                        mask_constant(
                            Constant {
                                hi: u128::MAX,
                                lo: u128::MAX,
                            },
                            w,
                        )
                    } else {
                        Constant { hi: 0, lo: 0 }
                    };
                    consts.insert(vid, result);
                }
            }
            Stmt::Rol { src, ty, n } => {
                if let (Some(&c), Some(w)) = (consts.get(&src.0), type_bit_width(*ty, types)) {
                    consts.insert(vid, constant_rol(c, w, *n));
                }
            }
            Stmt::Ror { src, ty, n } => {
                if let (Some(&c), Some(w)) = (consts.get(&src.0), type_bit_width(*ty, types)) {
                    consts.insert(vid, constant_ror(c, w, *n));
                }
            }
            Stmt::Poly {
                coeffs,
                constant,
                ty,
            } => {
                if let Some(c) = eval_poly(coeffs, *constant, *ty, &consts, types) {
                    consts.insert(vid, c);
                }
            }
            Stmt::StorageWrite {
                storage,
                src,
                ty,
                addr,
            } => {
                let addr_c = consts.get(&addr.0).copied();
                let src_c = consts.get(&src.0).copied();
                if let Some(addr_c) = addr_c {
                    if let Some(src_c) = src_c {
                        mem.insert((*storage, *ty, addr_c.lo), src_c);
                    } else {
                        mem.remove(&(*storage, *ty, addr_c.lo));
                    }
                }
            }
            Stmt::StorageRead { storage, ty, addr } => {
                if let Some(addr_c) = consts.get(&addr.0) {
                    if let Some(src_c) = mem.get(&(*storage, *ty, addr_c.lo)) {
                        consts.insert(vid, *src_c);
                    }
                }
            }
            _ => {}
        }
    }
    consts
}

fn eval_merge(
    parts: &[IRVarId],
    ty: IRTypeId,
    consts: &BTreeMap<u32, Constant>,
    type_map: &BTreeMap<u32, IRTypeId>,
    types: &IRTypes,
) -> Option<Constant> {
    let total_w = type_bit_width(ty, types)?;
    if !parts.iter().all(|v| consts.contains_key(&v.0)) {
        return None;
    }
    let mut result = Constant { hi: 0, lo: 0 };
    let mut offset = 0usize;
    for v in parts {
        let part_c = *consts.get(&v.0)?;
        let part_w = type_map
            .get(&v.0)
            .and_then(|&tid| type_bit_width(tid, types))
            .unwrap_or(1);
        result = constant_or(result, constant_shl(mask_constant(part_c, part_w), offset));
        offset += part_w;
        if offset >= total_w {
            break;
        }
    }
    Some(mask_constant(result, total_w))
}

fn eval_shuffle(
    result_bits: &[(u8, IRVarId)],
    consts: &BTreeMap<u32, Constant>,
) -> Option<Constant> {
    let mut lo = 0u128;
    let mut hi = 0u128;
    for (i, (bit_idx, var)) in result_bits.iter().enumerate() {
        let c = consts.get(&var.0)?;
        let src_bit = if *bit_idx < 128 {
            (c.lo >> bit_idx) & 1
        } else {
            (c.hi >> (*bit_idx - 128)) & 1
        };
        if src_bit != 0 {
            if i < 128 {
                lo |= 1u128 << i;
            } else if i < 256 {
                hi |= 1u128 << (i - 128);
            }
        }
    }
    Some(Constant { hi, lo })
}

fn eval_poly(
    coeffs: &PolyCoeffs<IRVarId>,
    constant: Constant,
    ty: IRTypeId,
    consts: &BTreeMap<u32, Constant>,
    types: &IRTypes,
) -> Option<Constant> {
    for mono in coeffs.keys() {
        for v in mono {
            consts.get(&v.0)?;
        }
    }
    let width = type_bit_width(ty, types).unwrap_or(1);
    let mut acc = mask_constant(constant, width);
    for (mono, coeff) in coeffs {
        if coeff & 1 == 0 {
            continue;
        }
        let mut term = Constant { hi: 0, lo: 1 };
        for v in mono {
            let c = *consts.get(&v.0)?;
            // GF(2) selector: a zero bit kills the term; a one-bit is identity.
            if width == 1 {
                term.lo &= c.lo & 1;
            } else {
                term.lo &= c.lo;
                term.hi &= c.hi;
            }
        }
        if width == 1 {
            acc.lo ^= term.lo & 1;
        } else {
            acc.lo ^= term.lo;
            acc.hi ^= term.hi;
        }
    }
    Some(mask_constant(acc, width))
}

fn remap_terminator(term: &IRTerminator, remap: &BTreeMap<u32, IRVarId>) -> IRTerminator {
    term.clone()
        .map(&mut (), |_, v: IRVarId| {
            Ok::<_, Infallible>(*remap.get(&v.0).unwrap_or(&v))
        })
        .unwrap()
}

fn dispatch_terminator<P: Clone>(
    blocks: &IRBlocks<P>,
    types: &IRTypes,
    dest: &mut IRBlock<P>,
    block_id: IRBlockId,
    term: &IRTerminator,
    visited: &mut BTreeSet<(u32, Vec<Option<Constant>>)>,
    steps: &mut usize,
    limits: UnrollLimits,
) -> Result<Vec<IRVarId>, UnrollError> {
    fold_dest_mut(dest, types);
    let consts = concrete_consts(dest, types);

    match term {
        IRTerminator::Jmp { target } => follow_target(
            blocks, types, dest, block_id, target, visited, steps, limits,
        ),
        IRTerminator::JumpCond {
            condition,
            then_target,
            else_target,
        } => {
            let c = consts
                .get(&condition.0)
                .copied()
                .ok_or(UnrollError::SymbolicBranch { block: block_id })?;
            let taken = if c.lo & 1 != 0 {
                then_target
            } else {
                else_target
            };
            follow_target(blocks, types, dest, block_id, taken, visited, steps, limits)
        }
        IRTerminator::JumpTable { index, cases } => {
            let c = consts
                .get(&index.0)
                .copied()
                .ok_or(UnrollError::SymbolicSwitch { block: block_id })?;
            let taken = cases
                .get(&c)
                .ok_or(UnrollError::SymbolicSwitch { block: block_id })?;
            follow_target(blocks, types, dest, block_id, taken, visited, steps, limits)
        }
        _ => Err(UnrollError::SymbolicBranch { block: block_id }),
    }
}

fn follow_target<P: Clone>(
    blocks: &IRBlocks<P>,
    types: &IRTypes,
    dest: &mut IRBlock<P>,
    block_id: IRBlockId,
    target: &IRBranchTarget,
    visited: &mut BTreeSet<(u32, Vec<Option<Constant>>)>,
    steps: &mut usize,
    limits: UnrollLimits,
) -> Result<Vec<IRVarId>, UnrollError> {
    match &target.dest {
        IRBlockTargetId::Return => Ok(target.args.clone()),
        IRBlockTargetId::Block(b) => walk(
            blocks,
            types,
            dest,
            *b,
            &target.args,
            visited,
            steps,
            limits,
        ),
        IRBlockTargetId::Dyn(v) => {
            let c = concrete_consts(dest, types)
                .get(&v.0)
                .copied()
                .ok_or(UnrollError::SymbolicBranch { block: block_id })?;
            let dest_block = IRBlockId(c.lo as u32);
            walk(
                blocks,
                types,
                dest,
                dest_block,
                &target.args,
                visited,
                steps,
                limits,
            )
        }
        _ => Err(UnrollError::SymbolicBranch { block: block_id }),
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::*;
    use volar_ir::ir::{IRBlock, IRStmt, IRType, IRTypeId};
    use volar_ir_common::{Node, Type};

    fn bit_types() -> IRTypes {
        IRTypes(std::vec![IRType::Primitive(Type::Bit)])
    }

    fn bit() -> IRTypeId {
        IRTypeId(0)
    }

    fn const_bit(lo: u128) -> IRStmt {
        IRStmt::Const(Constant { hi: 0, lo }, bit())
    }

    fn jmp(dest: IRBlockTargetId, args: std::vec::Vec<IRVarId>) -> IRTerminator {
        IRTerminator::Jmp {
            target: IRBranchTarget::new(dest, args),
        }
    }

    #[test]
    fn already_circuit_is_identity() {
        let types = bit_types();
        let blocks: IRBlocks<()> = IRBlocks::new(std::vec![IRBlock {
            params: std::vec![bit()],
            stmts: std::vec![],
            terminator: jmp(IRBlockTargetId::Return, std::vec![IRVarId(0)]),
        }]);
        let out = unroll_ir_everything(&blocks, &types).unwrap();
        assert!(out.is_circuit());
        assert_eq!(out.blocks[0].params, blocks.blocks[0].params);
        assert!(out.blocks[0].stmts.is_empty());
    }

    #[test]
    fn const_branch_takes_then_path() {
        let types = bit_types();
        // block 0: c = 1; JumpCond c -> block1 / block2
        // block 1: return const 1
        // block 2: return const 0
        let blocks: IRBlocks<()> = IRBlocks::new(std::vec![
            IRBlock {
                params: std::vec![],
                stmts: std::vec![Node::new(const_bit(1), (), None)],
                terminator: IRTerminator::JumpCond {
                    condition: IRVarId(0),
                    then_target: IRBranchTarget::new(
                        IRBlockTargetId::Block(IRBlockId(1)),
                        std::vec![],
                    ),
                    else_target: IRBranchTarget::new(
                        IRBlockTargetId::Block(IRBlockId(2)),
                        std::vec![],
                    ),
                },
            },
            IRBlock {
                params: std::vec![],
                stmts: std::vec![Node::new(const_bit(1), (), None)],
                terminator: jmp(IRBlockTargetId::Return, std::vec![IRVarId(0)]),
            },
            IRBlock {
                params: std::vec![],
                stmts: std::vec![Node::new(const_bit(0), (), None)],
                terminator: jmp(IRBlockTargetId::Return, std::vec![IRVarId(0)]),
            },
        ]);
        let out = unroll_ir_everything(&blocks, &types).unwrap();
        assert!(out.is_circuit());
        match &out.blocks[0].terminator {
            IRTerminator::Jmp { target } => {
                assert_eq!(target.dest, IRBlockTargetId::Return);
                assert_eq!(target.args.len(), 1);
                let ret = target.args[0];
                assert_eq!(
                    lookup_const(&out.blocks[0], ret, &types),
                    Some(Constant { hi: 0, lo: 1 })
                );
            }
            other => panic!("expected Jmp Return, got {other:?}"),
        }
    }

    #[test]
    fn symbolic_branch_is_rejected() {
        let types = bit_types();
        let blocks: IRBlocks<()> = IRBlocks::new(std::vec![IRBlock {
            params: std::vec![bit()],
            stmts: std::vec![],
            terminator: IRTerminator::JumpCond {
                condition: IRVarId(0),
                then_target: IRBranchTarget::new(IRBlockTargetId::Return, std::vec![IRVarId(0)]),
                else_target: IRBranchTarget::new(IRBlockTargetId::Return, std::vec![IRVarId(0)]),
            },
        }]);
        let err = unroll_ir_everything(&blocks, &types).unwrap_err();
        assert_eq!(
            err,
            UnrollError::SymbolicBranch {
                block: IRBlockId(0)
            }
        );
    }

    #[test]
    fn symbolic_branch_is_accepted_by_movfuscate() {
        let mut types = bit_types();
        let blocks: IRBlocks<()> = IRBlocks::new(std::vec![
            IRBlock {
                params: std::vec![bit()],
                stmts: std::vec![Node::new(const_bit(0), (), None)],
                terminator: IRTerminator::JumpCond {
                    condition: IRVarId(0),
                    then_target: IRBranchTarget::new(
                        IRBlockTargetId::Block(IRBlockId(1)),
                        std::vec![IRVarId(0)],
                    ),
                    else_target: IRBranchTarget::new(
                        IRBlockTargetId::Block(IRBlockId(1)),
                        std::vec![IRVarId(0)],
                    ),
                },
            },
            IRBlock {
                params: std::vec![bit()],
                stmts: std::vec![Node::new(const_bit(0), (), None)],
                terminator: jmp(IRBlockTargetId::Return, std::vec![IRVarId(0)]),
            },
        ]);
        assert!(matches!(
            unroll_ir_everything(&blocks, &types),
            Err(UnrollError::SymbolicBranch { .. })
        ));
        let moved = crate::movfuscate_ir(&blocks, &mut types);
        assert!(moved.is_movfuscated());
        assert!(!moved.is_circuit());
    }

    #[test]
    fn finite_loop_with_changing_const_flag() {
        let types = bit_types();
        // block 0: jmp header with flag=1
        // header: JumpCond flag -> body / exit
        // body: jmp header with flag=0
        // exit: return flag
        let blocks: IRBlocks<()> = IRBlocks::new(std::vec![
            IRBlock {
                params: std::vec![],
                stmts: std::vec![Node::new(const_bit(1), (), None)],
                terminator: jmp(IRBlockTargetId::Block(IRBlockId(1)), std::vec![IRVarId(0)],),
            },
            IRBlock {
                params: std::vec![bit()],
                stmts: std::vec![],
                terminator: IRTerminator::JumpCond {
                    condition: IRVarId(0),
                    then_target: IRBranchTarget::new(
                        IRBlockTargetId::Block(IRBlockId(2)),
                        std::vec![],
                    ),
                    else_target: IRBranchTarget::new(
                        IRBlockTargetId::Return,
                        std::vec![IRVarId(0)],
                    ),
                },
            },
            IRBlock {
                params: std::vec![],
                stmts: std::vec![Node::new(const_bit(0), (), None)],
                terminator: jmp(IRBlockTargetId::Block(IRBlockId(1)), std::vec![IRVarId(0)],),
            },
        ]);
        let out = unroll_ir_everything(&blocks, &types).unwrap();
        assert!(out.is_circuit());
        match &out.blocks[0].terminator {
            IRTerminator::Jmp { target } => {
                assert_eq!(target.dest, IRBlockTargetId::Return);
                let ret = target.args[0];
                assert_eq!(
                    lookup_const(&out.blocks[0], ret, &types),
                    Some(Constant { hi: 0, lo: 0 })
                );
            }
            other => panic!("expected Jmp Return, got {other:?}"),
        }
    }

    #[test]
    fn repeating_concrete_env_is_non_finite() {
        let types = bit_types();
        // Unconditional jump back to self with no changing constant.
        let blocks: IRBlocks<()> = IRBlocks::new(std::vec![IRBlock {
            params: std::vec![],
            stmts: std::vec![],
            terminator: jmp(IRBlockTargetId::Block(IRBlockId(0)), std::vec![]),
        }]);
        let err = unroll_ir_everything(&blocks, &types).unwrap_err();
        assert_eq!(err, UnrollError::NonFiniteControl);
    }
}
