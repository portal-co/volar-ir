// @reliability: normal
//! @ai: assisted
// Circuit-fused single-block forms of Volar IR (`VCircuit`) and Boolar IR
// (`BCircuit`). These types make the "exactly one block whose only exit is
// Return" invariant structural instead of a runtime predicate
// (`IRBlocks::is_circuit` / `BIrBlocks::is_circuit`).
//
// Pure data structure definitions plus one validating constructor per type;
// no cryptographic claims.

use alloc::vec::Vec;
use volar_ir_common::Node;

mod generated;
pub use generated::{BCircuit, VCircuit};

use crate::{
    boolar::{BIrBlock, BIrBlocks, BIrStmt},
    ir::{IRBlockTargetId, IRBlocks, IRBranchTarget, IRTerminator, IRTypeId, IRVarId},
};

/// Why an [`IRBlocks`] / [`BIrBlocks`] could not be fused into a circuit.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum CircuitFusionError {
    /// Fusing requires exactly one block (the movfuscated shape).
    NotSingleBlock {
        /// Number of blocks actually present.
        found: usize,
    },
    /// The single block's terminator was not `Jmp { dest: Return }`.
    NotReturnTerminator,
    /// The program carries module-level declaration state (oracles, actions,
    /// RNG sources, or pre-initialised storage segments). [`VCircuit`] cannot
    /// carry such tables without silently dropping them on round-trip.
    ModuleLevelStateUnsupported,
    /// An output variable does not name a var in the block's var space
    /// (params followed by statement results).
    OutputVarOutOfRange {
        var: u32,
        /// Size of the valid var space (`params + stmts`).
        var_space: u32,
    },
}

impl core::fmt::Display for CircuitFusionError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CircuitFusionError::NotSingleBlock { found } => {
                write!(
                    f,
                    "circuit fusion requires exactly one block, found {found}"
                )
            }
            CircuitFusionError::NotReturnTerminator => {
                write!(
                    f,
                    "circuit fusion requires a `Jmp {{ dest: Return }}` terminator"
                )
            }
            CircuitFusionError::ModuleLevelStateUnsupported => write!(
                f,
                "VCircuit fusion rejects oracle/action/RNG declarations and pre-init segments"
            ),
            CircuitFusionError::OutputVarOutOfRange { var, var_space } => {
                write!(
                    f,
                    "output var {var} out of range (var space size {var_space})"
                )
            }
        }
    }
}

// ============================================================================
// Circuit-fused Volar IR
// ============================================================================

impl<P: Clone> VCircuit<P> {
    /// Construct an empty fused circuit with the given input types.
    pub fn new(params: Vec<IRTypeId>) -> Self {
        VCircuit {
            params,
            stmts: Vec::new(),
            outputs: Vec::new(),
        }
    }

    /// Append a statement with provenance and no side tag.
    /// Returns the [`IRVarId`] for this statement (= index in the var space).
    pub fn push_stmt(&mut self, stmt: crate::ir::IRStmt, prov: P) -> IRVarId {
        self.push_stmt_with_side(stmt, prov, None)
    }

    /// Append a statement with provenance and an optional side tag.
    /// Returns the [`IRVarId`] for this statement (= index in the var space).
    pub fn push_stmt_with_side(
        &mut self,
        stmt: crate::ir::IRStmt,
        prov: P,
        side: Option<volar_side::SideId>,
    ) -> IRVarId {
        let id = IRVarId(self.params.len() as u32 + self.stmts.len() as u32);
        self.stmts.push(Node::new(stmt, prov, side));
        id
    }

    /// Validate and fuse a movfuscated Volar program into a [`VCircuit`].
    ///
    /// This is the only place the circuit invariant is checked; downstream
    /// code can rely on it structurally. Callers holding a multi-block or
    /// non-`Return`-terminated program must run movfuscation / loop lowering
    /// first — fusion validates, it does not produce the shape itself.
    pub fn try_from_ir(blocks: &IRBlocks<P>) -> Result<Self, CircuitFusionError> {
        check_no_module_level_state(
            blocks.oracles.is_empty(),
            blocks.actions.is_empty(),
            blocks.rngs.is_empty(),
            blocks.pre_init.is_empty(),
        )?;
        let [block] = &blocks.blocks[..] else {
            return Err(CircuitFusionError::NotSingleBlock {
                found: blocks.blocks.len(),
            });
        };
        let outputs = match &block.terminator {
            IRTerminator::Jmp { target } => match target.dest {
                IRBlockTargetId::Return => target.args.clone(),
                _ => return Err(CircuitFusionError::NotReturnTerminator),
            },
            _ => return Err(CircuitFusionError::NotReturnTerminator),
        };
        let var_space = block.params.len() as u32 + block.stmts.len() as u32;
        for v in &outputs {
            if v.0 >= var_space {
                return Err(CircuitFusionError::OutputVarOutOfRange {
                    var: v.0,
                    var_space,
                });
            }
        }
        Ok(VCircuit {
            params: block.params.clone(),
            stmts: block.stmts.clone(),
            outputs,
        })
    }

    /// Map provenance annotations using a [`ProvenanceHandler`]. `side` is
    /// untouched — provenance and side are independent axes.
    pub fn map_prov_with_handler<H: crate::ProvenanceHandler<P>>(
        self,
        handler: &H,
    ) -> VCircuit<H::Output> {
        VCircuit {
            params: self.params,
            stmts: self
                .stmts
                .into_iter()
                .map(|n| n.map_prov(|p| handler.map(&p)))
                .collect(),
            outputs: self.outputs,
        }
    }

    /// Un-fuse back into the general [`IRBlocks`] form: one block whose sole
    /// terminator is `Jmp { dest: Return, args: outputs }`, with no
    /// module-level declarations (fused circuits never carry any — see
    /// [`CircuitFusionError::ModuleLevelStateUnsupported`]).
    pub fn to_ir_blocks(self) -> IRBlocks<P> {
        let terminator = IRTerminator::Jmp {
            target: IRBranchTarget {
                dest: IRBlockTargetId::Return,
                args: self.outputs,
                reentry: None,
            },
        };
        IRBlocks::new(alloc::vec![crate::ir::IRBlock {
            params: self.params,
            stmts: self.stmts,
            terminator,
        }])
    }
}

impl<P: Clone + PartialEq> TryFrom<&IRBlocks<P>> for VCircuit<P> {
    type Error = CircuitFusionError;

    fn try_from(blocks: &IRBlocks<P>) -> Result<Self, Self::Error> {
        VCircuit::try_from_ir(blocks)
    }
}

impl<P: Clone> From<VCircuit<P>> for IRBlocks<P> {
    fn from(circuit: VCircuit<P>) -> Self {
        circuit.to_ir_blocks()
    }
}

// ============================================================================
// Circuit-fused Boolar IR
// ============================================================================

impl<P: Clone> BCircuit<P> {
    /// Construct an empty fused circuit with `params` input bits.
    pub fn new(params: u32) -> Self {
        BCircuit {
            params,
            stmts: Vec::new(),
            pre_init: Vec::new(),
            outputs: Vec::new(),
        }
    }

    /// Append a statement with provenance and no side tag.
    /// Returns the [`IRVarId`] for this statement (= index in the var space).
    pub fn push_stmt(&mut self, stmt: BIrStmt, prov: P) -> IRVarId {
        self.push_stmt_with_side(stmt, prov, None)
    }

    /// Append a statement with provenance and an optional side tag.
    /// Returns the [`IRVarId`] for this statement (= index in the var space).
    pub fn push_stmt_with_side(
        &mut self,
        stmt: BIrStmt,
        prov: P,
        side: Option<volar_side::SideId>,
    ) -> IRVarId {
        let id = IRVarId(self.params + self.stmts.len() as u32);
        self.stmts.push(Node::new(stmt, prov, side));
        id
    }

    /// Total number of vars in the var space (params followed by stmt results).
    pub fn var_space(&self) -> u32 {
        self.params + self.stmts.len() as u32
    }

    /// Validate and fuse a movfuscated Boolar program into a [`BCircuit`].
    ///
    /// See [`VCircuit::try_from_ir`]: this is the single implementation of the
    /// fused-circuit invariant for Boolar; passes call it, never re-check
    /// inline.
    pub fn try_from_ir(blocks: &BIrBlocks<P>) -> Result<Self, CircuitFusionError> {
        let [block] = &blocks.blocks[..] else {
            return Err(CircuitFusionError::NotSingleBlock {
                found: blocks.blocks.len(),
            });
        };
        let outputs = match &block.terminator {
            crate::boolar::BIrTerminator::Jmp(target) => match target.block {
                IRBlockTargetId::Return => target.args.clone(),
                _ => return Err(CircuitFusionError::NotReturnTerminator),
            },
            _ => return Err(CircuitFusionError::NotReturnTerminator),
        };
        let var_space = block.params + block.stmts.len() as u32;
        for v in &outputs {
            if v.0 >= var_space {
                return Err(CircuitFusionError::OutputVarOutOfRange {
                    var: v.0,
                    var_space,
                });
            }
        }
        Ok(BCircuit {
            params: block.params,
            stmts: block.stmts.clone(),
            pre_init: blocks.pre_init.clone(),
            outputs,
        })
    }

    /// Map provenance annotations using a [`ProvenanceHandler`]. `side` is
    /// untouched — provenance and side are independent axes.
    pub fn map_prov_with_handler<H: crate::ProvenanceHandler<P>>(
        self,
        handler: &H,
    ) -> BCircuit<H::Output> {
        BCircuit {
            params: self.params,
            stmts: self
                .stmts
                .into_iter()
                .map(|n| n.map_prov(|p| handler.map(&p)))
                .collect(),
            pre_init: self.pre_init,
            outputs: self.outputs,
        }
    }

    /// Un-fuse back into the general [`BIrBlocks`] form: one block whose sole
    /// terminator is `Jmp(Return, args: outputs)`. Unlike [`VCircuit`], this
    /// form retains bit-granular pre-initialised storage segments.
    pub fn to_bir_blocks(self) -> BIrBlocks<P> {
        use crate::boolar::{BIrTarget, BIrTerminator};
        let terminator = BIrTerminator::Jmp(BIrTarget {
            block: IRBlockTargetId::Return,
            args: self.outputs,
        });
        BIrBlocks {
            blocks: alloc::vec![BIrBlock {
                params: self.params,
                stmts: self.stmts,
                terminator
            }],
            pre_init: self.pre_init,
        }
    }
}

impl<P: Clone> TryFrom<&BIrBlocks<P>> for BCircuit<P> {
    type Error = CircuitFusionError;

    fn try_from(blocks: &BIrBlocks<P>) -> Result<Self, Self::Error> {
        BCircuit::try_from_ir(blocks)
    }
}

impl<P: Clone> From<BCircuit<P>> for BIrBlocks<P> {
    fn from(circuit: BCircuit<P>) -> Self {
        circuit.to_bir_blocks()
    }
}

// ============================================================================
// Shared validation helpers
// ============================================================================

fn check_no_module_level_state(
    no_oracles: bool,
    no_actions: bool,
    no_rngs: bool,
    no_pre_init: bool,
) -> Result<(), CircuitFusionError> {
    if no_oracles && no_actions && no_rngs && no_pre_init {
        Ok(())
    } else {
        Err(CircuitFusionError::ModuleLevelStateUnsupported)
    }
}
