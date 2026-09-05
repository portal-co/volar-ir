// @reliability: normal
// @ai: assisted

use std::collections::BTreeMap;

use volar_ir::boolar::{BIrPreInitSegment, LaneId};
use volar_ir::ir::{IRTypes, IRVarId};
use volar_ir_common::{Constant, PolyCoeffs, StorageId, TypeId};

/// A named SSA wire, plus whether the backend must emit a binding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedWire {
    pub id: IRVarId,
    pub name: String,
    /// `true` when this wire is an input, an explicit name, `name_all_wires`,
    /// multi-use, or an effectful statement.
    pub bind: bool,
}

/// One Boolar statement after naming.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedBoolStmt {
    pub dst: NamedWire,
    pub op: NamedBoolOp,
}

/// Supported Boolar ops, plus a fail-closed external bucket.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NamedBoolOp {
    Zero,
    One,
    And(IRVarId, IRVarId),
    Or(IRVarId, IRVarId),
    Xor(IRVarId, IRVarId),
    Not(IRVarId),
    StorageRead {
        storage: StorageId,
        lane: LaneId,
        addr: Vec<IRVarId>,
    },
    StorageWrite {
        storage: StorageId,
        lane: LaneId,
        src: IRVarId,
        addr: Vec<IRVarId>,
    },
    External {
        kind: ExternalKind,
        name: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalKind {
    Oracle,
    Action,
    Rng,
}

impl NamedBoolOp {
    pub fn is_storage(&self) -> bool {
        matches!(
            self,
            NamedBoolOp::StorageRead { .. } | NamedBoolOp::StorageWrite { .. }
        )
    }

    pub fn is_effect(&self) -> bool {
        matches!(self, NamedBoolOp::StorageWrite { .. } | NamedBoolOp::External { .. })
    }

    pub fn is_external(&self) -> bool {
        matches!(self, NamedBoolOp::External { .. })
    }

    pub fn operands(&self) -> Vec<IRVarId> {
        match self {
            NamedBoolOp::Zero | NamedBoolOp::One | NamedBoolOp::External { .. } => Vec::new(),
            NamedBoolOp::And(a, b) | NamedBoolOp::Or(a, b) | NamedBoolOp::Xor(a, b) => {
                vec![*a, *b]
            }
            NamedBoolOp::Not(a) => vec![*a],
            NamedBoolOp::StorageRead { addr, .. } => addr.clone(),
            NamedBoolOp::StorageWrite { src, addr, .. } => {
                let mut v = addr.clone();
                v.push(*src);
                v
            }
        }
    }
}

/// Named Boolar circuit ready for a language backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedBoolCircuit {
    pub inputs: Vec<NamedWire>,
    pub stmts: Vec<NamedBoolStmt>,
    pub outputs: Vec<IRVarId>,
    pub pre_init: Vec<BIrPreInitSegment>,
    pub storage_names: BTreeMap<StorageId, String>,
    pub wires: BTreeMap<IRVarId, NamedWire>,
}

impl NamedBoolCircuit {
    pub fn has_storage(&self) -> bool {
        self.stmts.iter().any(|s| s.op.is_storage()) || !self.pre_init.is_empty()
    }

    pub fn has_externals(&self) -> bool {
        self.stmts.iter().any(|s| s.op.is_external())
    }

    pub fn wire(&self, id: IRVarId) -> Option<&NamedWire> {
        self.wires.get(&id)
    }

    pub fn wire_name(&self, id: IRVarId) -> Option<&str> {
        self.wire(id).map(|w| w.name.as_str())
    }

    pub fn stmt_defining(&self, id: IRVarId) -> Option<&NamedBoolStmt> {
        self.stmts.iter().find(|s| s.dst.id == id)
    }

    pub fn is_input(&self, id: IRVarId) -> bool {
        (id.0 as usize) < self.inputs.len()
    }
}

/// One typed Volar IR statement after naming.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedVolarStmt {
    pub dst: NamedWire,
    pub op: NamedVolarOp,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NamedVolarOp {
    Const {
        value: Constant,
        ty: TypeId,
    },
    Transmute {
        src: IRVarId,
        src_ty: TypeId,
        dst_ty: TypeId,
    },
    Poly {
        ty: TypeId,
        coeffs: PolyCoeffs<IRVarId>,
        constant: Constant,
    },
    Rol {
        src: IRVarId,
        ty: TypeId,
        n: usize,
    },
    Ror {
        src: IRVarId,
        ty: TypeId,
        n: usize,
    },
    Merge {
        parts: Vec<IRVarId>,
        ty: TypeId,
    },
    Splat {
        src: IRVarId,
        ty: TypeId,
    },
    Shuffle {
        result_bits: Vec<(u8, IRVarId)>,
        ty: TypeId,
    },
    StorageRead {
        storage: StorageId,
        ty: TypeId,
        addr: IRVarId,
    },
    StorageWrite {
        storage: StorageId,
        src: IRVarId,
        ty: TypeId,
        addr: IRVarId,
    },
    External {
        kind: ExternalKind,
        name: String,
    },
}

impl NamedVolarOp {
    pub fn is_storage(&self) -> bool {
        matches!(
            self,
            NamedVolarOp::StorageRead { .. } | NamedVolarOp::StorageWrite { .. }
        )
    }

    pub fn is_effect(&self) -> bool {
        matches!(
            self,
            NamedVolarOp::StorageWrite { .. } | NamedVolarOp::External { .. }
        )
    }

    pub fn is_external(&self) -> bool {
        matches!(self, NamedVolarOp::External { .. })
    }

    pub fn operands(&self) -> Vec<IRVarId> {
        match self {
            NamedVolarOp::Const { .. } | NamedVolarOp::External { .. } => Vec::new(),
            NamedVolarOp::Transmute { src, .. }
            | NamedVolarOp::Rol { src, .. }
            | NamedVolarOp::Ror { src, .. }
            | NamedVolarOp::Splat { src, .. } => vec![*src],
            NamedVolarOp::Poly { coeffs, .. } => {
                let mut vars = Vec::new();
                for key in coeffs.keys() {
                    vars.extend(key.iter().copied());
                }
                vars.sort();
                vars.dedup();
                vars
            }
            NamedVolarOp::Merge { parts, .. } => parts.clone(),
            NamedVolarOp::Shuffle { result_bits, .. } => {
                result_bits.iter().map(|(_, v)| *v).collect()
            }
            NamedVolarOp::StorageRead { addr, .. } => vec![*addr],
            NamedVolarOp::StorageWrite { src, addr, .. } => vec![*src, *addr],
        }
    }
}

/// Named Volar circuit plus the type table it was named against.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedVolarCircuit {
    pub types: IRTypes,
    pub inputs: Vec<NamedVolarParam>,
    pub stmts: Vec<NamedVolarStmt>,
    pub outputs: Vec<IRVarId>,
    pub storage_names: BTreeMap<StorageId, String>,
    pub wires: BTreeMap<IRVarId, NamedWire>,
    pub type_of: BTreeMap<IRVarId, TypeId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NamedVolarParam {
    pub wire: NamedWire,
    pub ty: TypeId,
}

impl NamedVolarCircuit {
    pub fn has_storage(&self) -> bool {
        self.stmts.iter().any(|s| s.op.is_storage())
    }

    pub fn has_externals(&self) -> bool {
        self.stmts.iter().any(|s| s.op.is_external())
    }

    pub fn wire(&self, id: IRVarId) -> Option<&NamedWire> {
        self.wires.get(&id)
    }

    pub fn wire_name(&self, id: IRVarId) -> Option<&str> {
        self.wire(id).map(|w| w.name.as_str())
    }

    pub fn stmt_defining(&self, id: IRVarId) -> Option<&NamedVolarStmt> {
        self.stmts.iter().find(|s| s.dst.id == id)
    }

    pub fn is_input(&self, id: IRVarId) -> bool {
        (id.0 as usize) < self.inputs.len()
    }
}
