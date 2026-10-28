#![no_std]
// @reliability: experimental
// @ai: assisted
//! Virtualization transform for Volar IR (`IRBlocks`) and Boolar IR
//! (`BIrBlocks`).
//!
//! The pass converts a multi-block module into a bounded set of
//! *instruction handlers* (one per unique block skeleton, modulo constant
//! values and jump target ids) plus an interpreter loop that fetches
//! `(handler_idx, immediates)` tuples from a bytecode table.  The main
//! compile-time win is that the backend prints (or lowers) one body per
//! unique handler rather than one body per original block.
//!
//! See [`VirtualizeConfig`] for the knobs and [`virtualize_ir`] /
//! [`virtualize_bir`] for the concrete entry points.
//!
//! # Dispatch modes
//!
//! * [`DispatchMode::Public`] (default) — the dispatcher emits an
//!   [`IRTerminator::JumpTable`] (IR) or a balanced `CondJmp` tree (BIR) on
//!   the handler index.  Assumes the PC is public (i.e. the computed
//!   `handler_idx` does not depend on any witness wire); this is true of
//!   every Volar program whose control flow is known to all parties.
//! * [`DispatchMode::Oblivious`] — the dispatcher runs every handler every
//!   step and multiplexes outputs with `is_active · val + (1-is_active) ·
//!   fallback` accumulators.  Safe for witness-dependent control flow at a
//!   larger per-step cost.
//!
//! # Storage initialization
//!
//! Static bytecode table and per-slot storage lanes are emitted as
//! [`PreInitSegment`] entries on the output module's `pre_init` field
//! (same construct WASM data segments and the VOLE weaver use).  The setup
//! block only performs dynamic work (keyed commitment params, entry-param
//! register routing).  A structured [`VirtBytecode`] side artifact is always
//! returned alongside the module for tests and backends.

extern crate alloc;

pub mod adaptive_cfg;
pub mod adaptive_emit;
pub mod bir;
pub mod bytecode;
pub mod canon;
pub mod cfg_hints;
pub mod ctx;
pub mod hash;
pub mod ir;
pub mod layout;
pub mod preinit;
pub mod split;

pub use adaptive_cfg::AdaptiveSplitConfig;
pub use bir::virtualize_bir;
pub use bytecode::{
    AppendedRegionKind, AppendedRegionMeta, BytecodeEntry, BytecodeRowKind, HandlerImmSchema,
    OperandMode, TripCount, VirtBytecode,
};
pub use canon::{BirHandlerKey, BlockImmediates, HandlerKey, ImmediateKind, IrHandlerKey};
pub use ctx::{
    StorageAccessViolation, VirtOutput, validate_bir_storage_access, validate_ir_storage_access,
};
pub use hash::{CommitmentConfig, IrEmitter, IrHashAlgorithm, SipHash48, XorFoldHash32};
pub use ir::{virtualize_ir, virtualize_ir_committed};
pub use split::plan_adaptive_split;

use volar_ir_common::{StorageId, StorageTable};

/// How the dispatcher routes control to the active handler every step.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DispatchMode {
    /// Cheap public dispatch via `IRTerminator::JumpTable` (IR) or a balanced
    /// `CondJmp` tree (BIR).  Assumes the handler index is a public value.
    Public,
    /// Oblivious dispatch via movfuscate-style accumulators — every handler
    /// runs every step, outputs combined with `is_active · val + fallback`.
    Oblivious,
}

/// How aggressively blocks are canonicalised before deduplication.
///
/// Only [`DedupPolicy::ConstantsAndTargets`] is implemented in v1.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[non_exhaustive]
pub enum DedupPolicy {
    /// Lift `Stmt::Const` values, the constant term of `Stmt::Poly`, and
    /// every jump target block id into per-block immediates.  Everything
    /// else is structural.
    ConstantsAndTargets,
    /// Also lift scalar fields that only change by parameter (e.g.
    /// `Rol.n`, `Ror.n`, `Shuffle.result_bits`, BIR storage ids, per-call
    /// names).  Reserved for a future pass — currently panics.
    Maximal,
}

/// Configuration knobs for [`virtualize_ir`] / [`virtualize_bir`].
#[derive(Clone, Debug)]
pub struct VirtualizeConfig {
    pub dispatch: DispatchMode,
    pub dedup: DedupPolicy,
    /// Storage space used to hold the bytecode table (seeded via `pre_init`).
    /// Must not collide with any `StorageId` the input module already reads
    /// from or writes to.
    pub bytecode_storage: StorageId,
    /// When `true`, handlers jump directly to their successor handler via an
    /// inline `JumpTable`, eliminating the two-block dispatcher→dispatch
    /// indirection on the hot path.  Each handler arm emits one extra
    /// dispatch sub-block.  Only supported for IR (`virtualize_ir`); BIR
    /// direct dispatch is not yet implemented (would cause O(n²) block growth).
    pub direct_dispatch: bool,
    /// Adaptive split: SharedCore cross-block dedup and RerollLoop intra-block
    /// rerolling. See [`AdaptiveSplitConfig`] and `docs/agent-context/virt-adaptive-split-adr.md`.
    pub adaptive_split: AdaptiveSplitConfig,
    /// Compatibility sidecar for caller-owned storage facts. Virtualization
    /// conservatively merges these declarations with its generated layout;
    /// absent declarations remain read-write.
    pub storage_access: StorageTable,
}

impl Default for VirtualizeConfig {
    fn default() -> Self {
        Self {
            dispatch: DispatchMode::Public,
            dedup: DedupPolicy::ConstantsAndTargets,
            bytecode_storage: StorageId::VIRT_BYTECODE,
            direct_dispatch: false,
            adaptive_split: AdaptiveSplitConfig::default(),
            storage_access: StorageTable::new(),
        }
    }
}

#[cfg(test)]
mod cutover_repro_tests {
    //! Repro for the adaptive-split "var not found" bug the `Type::_n`
    //! cutover surfaced: when the source IR is ALL `Bit` (the fuzzer's
    //! generator after the cutover), the adaptive split's handler emit has a
    //! var-numbering issue.
    use super::*;
    use alloc::vec;
    use alloc::vec::Vec;
    use volar_ir::ir::{
        IRBlock, IRBlockTargetId, IRBlocks, IRBranchTarget, IRStmt, IRTerminator, IRType, IRVarId,
    };
    use volar_ir_common::{Constant, Node, PolyCoeffs, TypeId, TypeTable};

    /// Build a minimal all-Bit IR: 2 params, a Poly (XOR) stmt, return both
    /// params + the stmt.
    fn all_bit_ir() -> (IRBlocks<()>, TypeTable) {
        let bit = TypeId(0);
        let types = TypeTable(vec![IRType::Primitive(volar_ir_common::Type::Bit)]);
        let mk = |kind: IRStmt| Node::new(kind, (), None);
        let mut coeffs = PolyCoeffs::new();
        coeffs.insert(vec![IRVarId(0), IRVarId(1)], 1u8);
        let block = IRBlock {
            params: vec![bit, bit],
            stmts: vec![mk(IRStmt::Poly {
                ty: bit,
                coeffs,
                constant: Constant { hi: 0, lo: 0 },
            })],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(
                    IRBlockTargetId::Return,
                    vec![IRVarId(0), IRVarId(1), IRVarId(2)],
                ),
            },
        };
        (IRBlocks::new(vec![block]), types)
    }

    #[test]
    fn repro_adaptive_split_consts_only_var_not_found() {
        // The minimal proptest failing case: a block with 1 Bit param and
        // several Const statements. The adaptive split's handler emit has a
        // var-numbering issue on this shape.
        let bit = TypeId(0);
        let types = TypeTable(vec![IRType::Primitive(volar_ir_common::Type::Bit)]);
        let mk = |kind: IRStmt| Node::new(kind, (), None);
        let block: IRBlock<()> = IRBlock {
            params: vec![bit],
            stmts: vec![
                mk(IRStmt::Const(Constant { hi: 0, lo: 0 }, bit)),
                mk(IRStmt::Const(Constant { hi: 0, lo: 1 }, bit)),
                mk(IRStmt::Const(Constant { hi: 0, lo: 0 }, bit)),
                mk(IRStmt::Const(Constant { hi: 0, lo: 1 }, bit)),
            ],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(
                    IRBlockTargetId::Return,
                    vec![IRVarId(0), IRVarId(1), IRVarId(2), IRVarId(3), IRVarId(4)],
                ),
            },
        };
        let ir: IRBlocks<()> = IRBlocks::new(vec![block]);
        let mut types = types;
        let cfg = VirtualizeConfig {
            dispatch: DispatchMode::Public,
            adaptive_split: crate::AdaptiveSplitConfig {
                enabled: true,
                ..crate::AdaptiveSplitConfig::default()
            },
            ..VirtualizeConfig::default()
        };
        let virt = virtualize_ir::<()>(&ir, &mut types, &cfg);
        // Evaluate the virtualized IR; it must not panic with "var not found".
        let inputs = vec![vec![true]];
        let out = volar_fuzz::interpreter::ir::eval_ir(&virt.blocks, &types, &inputs);
        assert!(out.is_some(), "the virtualized IR must evaluate");
    }

    #[test]
    fn repro_adaptive_split_empty_block_var_not_found() {
        // The shrunk failing case: an EMPTY block (no params, no stmts,
        // Jmp(Return, [])). The virt pass's adaptive split emits a virtualized
        // IR with a dangling var.
        let types = TypeTable(vec![]);
        let block: IRBlock<()> = IRBlock {
            params: vec![],
            stmts: vec![],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, vec![]),
            },
        };
        let ir: IRBlocks<()> = IRBlocks::new(vec![block]);
        let mut types = types;
        let cfg = VirtualizeConfig {
            dispatch: DispatchMode::Public,
            adaptive_split: crate::AdaptiveSplitConfig {
                enabled: true,
                ..crate::AdaptiveSplitConfig::default()
            },
            ..VirtualizeConfig::default()
        };
        let virt = virtualize_ir::<()>(&ir, &mut types, &cfg);
        // Check every var referenced in each block is defined.
        for (bid, b) in virt.blocks.blocks.iter().enumerate() {
            let n_defined = b.params.len() + b.stmts.len();
            let check = |v: IRVarId, what: &str| {
                assert!(
                    (v.0 as usize) < n_defined,
                    "block {bid}: {what} references undefined var {} (block has {n_defined} vars)",
                    v.0
                );
            };
            for s in &b.stmts {
                match &s.kind {
                    IRStmt::Poly { coeffs, .. } => {
                        for (mono, _) in coeffs {
                            for v in mono {
                                check(*v, "Poly");
                            }
                        }
                    }
                    IRStmt::StorageRead { addr, .. } => check(*addr, "StorageRead addr"),
                    IRStmt::StorageWrite { src, addr, .. } => {
                        check(*src, "StorageWrite src");
                        check(*addr, "StorageWrite addr");
                    }
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn repro_adaptive_split_all_bit_var_not_found() {
        let (ir, mut types) = all_bit_ir();
        let cfg = VirtualizeConfig {
            dispatch: DispatchMode::Public,
            adaptive_split: crate::AdaptiveSplitConfig {
                enabled: true,
                ..crate::AdaptiveSplitConfig::default()
            },
            ..VirtualizeConfig::default()
        };
        // This should not produce a virtualized IR with a dangling var.
        let virt = virtualize_ir::<()>(&ir, &mut types, &cfg);
        // Check every var referenced in each block is defined.
        for (bid, b) in virt.blocks.blocks.iter().enumerate() {
            let mut defined: alloc::collections::BTreeSet<u32> =
                (0..b.params.len() as u32).collect();
            for s in &b.stmts {
                if let IRStmt::Poly { coeffs, .. } = &s.kind {
                    for (mono, _) in coeffs {
                        for v in monomial_iter(monomial_key(mono)) {
                            assert!(
                                defined.contains(&v.0),
                                "block {bid}: Poly references undefined var {}",
                                v.0
                            );
                        }
                    }
                }
                // The stmt defines a new var.
                defined.insert(
                    b.params.len() as u32 + /* stmt idx */ {
                    // Find the stmt's index.
                    b.stmts.iter().position(|x| core::ptr::eq(x, s)).unwrap_or(0) as u32
                },
                );
            }
        }
    }

    fn monomial_key(m: &Vec<IRVarId>) -> &Vec<IRVarId> {
        m
    }
    fn monomial_iter(m: &Vec<IRVarId>) -> impl Iterator<Item = IRVarId> + '_ {
        m.iter().copied()
    }
}
