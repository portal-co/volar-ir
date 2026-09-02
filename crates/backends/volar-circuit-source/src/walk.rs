// @reliability: normal
// @ai: assisted

use std::collections::{BTreeMap, BTreeSet};

use volar_ir::boolar::BIrStmt;
use volar_ir::circuit::{BCircuit, VCircuit};
use volar_ir::ir::{IRStmt, IRTypes, IRVarId};
use volar_ir_common::{StorageId, Stmt};

use crate::error::EmitError;
use crate::named::{
    ExternalKind, NamedBoolCircuit, NamedBoolOp, NamedBoolStmt, NamedVolarCircuit, NamedVolarOp,
    NamedVolarParam, NamedVolarStmt, NamedWire,
};
use crate::names::{EmitOptions, require_ident};
use crate::types::bit_type_id;

/// Build a named Boolar circuit. `sanitize` is the backend's identifier rule.
pub fn name_bool_circuit<P: Clone>(
    circuit: &BCircuit<P>,
    opt: &EmitOptions,
    sanitize: impl Fn(&str) -> Result<String, EmitError>,
) -> Result<NamedBoolCircuit, EmitError> {
    let n_params = circuit.params;
    let var_space = circuit.var_space();
    let mut ops: Vec<NamedBoolOp> = Vec::with_capacity(circuit.stmts.len());
    let mut storages: BTreeSet<StorageId> = BTreeSet::new();

    for node in &circuit.stmts {
        let op = bool_op(&node.kind, &mut storages);
        ops.push(op);
    }

    let mut use_count: BTreeMap<IRVarId, usize> = BTreeMap::new();
    let bump = |map: &mut BTreeMap<IRVarId, usize>, id: IRVarId| {
        *map.entry(id).or_insert(0) += 1;
    };
    for op in &ops {
        for v in op.operands() {
            bump(&mut use_count, v);
        }
    }
    for &o in &circuit.outputs {
        bump(&mut use_count, o);
    }

    let mut used_names: BTreeMap<String, IRVarId> = BTreeMap::new();
    let mut wires: BTreeMap<IRVarId, NamedWire> = BTreeMap::new();
    let mut inputs = Vec::with_capacity(n_params as usize);

    for i in 0..n_params {
        let id = IRVarId(i);
        let raw = opt
            .wires
            .var(id)
            .map(str::to_string)
            .unwrap_or_else(|| format!("in{i}"));
        let name = take_name(id, &raw, &sanitize, &mut used_names)?;
        let wire = NamedWire {
            id,
            name,
            bind: true,
        };
        inputs.push(wire.clone());
        wires.insert(id, wire);
    }

    let mut stmts = Vec::with_capacity(ops.len());
    for (idx, op) in ops.into_iter().enumerate() {
        let id = IRVarId(n_params + idx as u32);
        if id.0 >= var_space {
            return Err(EmitError::UnknownVar { id });
        }
        let explicit = opt.wires.is_explicit_var(id);
        let uses = use_count.get(&id).copied().unwrap_or(0);
        let bind = opt.name_all_wires || explicit || uses != 1 || op.is_effect();
        let raw = opt
            .wires
            .var(id)
            .map(str::to_string)
            .unwrap_or_else(|| format!("w{}", id.0));
        let name = take_name(id, &raw, &sanitize, &mut used_names)?;
        let dst = NamedWire { id, name, bind };
        wires.insert(id, dst.clone());
        stmts.push(NamedBoolStmt { dst, op });
    }

    for &o in &circuit.outputs {
        if o.0 >= var_space {
            return Err(EmitError::UnknownVar { id: o });
        }
    }

    let mut storage_names = BTreeMap::new();
    for sid in storages {
        let raw = opt
            .wires
            .storage(sid)
            .map(str::to_string)
            .unwrap_or_else(|| format!("mem{}", sid.0));
        let name = sanitize(&raw)?;
        require_ident(&name, "storage name")?;
        if storage_names.values().any(|n| n == &name) {
            return Err(EmitError::NameCollision { name });
        }
        storage_names.insert(sid, name);
    }

    Ok(NamedBoolCircuit {
        inputs,
        stmts,
        outputs: circuit.outputs.clone(),
        pre_init: circuit.pre_init.clone(),
        storage_names,
        wires,
    })
}

/// Build a named Volar circuit. `sanitize` is the backend's identifier rule.
pub fn name_volar_circuit<P: Clone>(
    circuit: &VCircuit<P>,
    types: &IRTypes,
    opt: &EmitOptions,
    sanitize: impl Fn(&str) -> Result<String, EmitError>,
) -> Result<NamedVolarCircuit, EmitError> {
    let n_params = circuit.params.len() as u32;
    let var_space = n_params + circuit.stmts.len() as u32;
    let bit_ty = bit_type_id(types).ok();

    let mut ops: Vec<NamedVolarOp> = Vec::with_capacity(circuit.stmts.len());
    let mut storages: BTreeSet<StorageId> = BTreeSet::new();
    let mut type_of: BTreeMap<IRVarId, volar_ir_common::TypeId> = BTreeMap::new();

    for (i, ty) in circuit.params.iter().enumerate() {
        type_of.insert(IRVarId(i as u32), *ty);
    }

    for node in &circuit.stmts {
        let op = volar_op(&node.kind, &mut storages, bit_ty);
        ops.push(op);
    }

    let mut use_count: BTreeMap<IRVarId, usize> = BTreeMap::new();
    let bump = |map: &mut BTreeMap<IRVarId, usize>, id: IRVarId| {
        *map.entry(id).or_insert(0) += 1;
    };
    for op in &ops {
        for v in op.operands() {
            bump(&mut use_count, v);
        }
    }
    for &o in &circuit.outputs {
        bump(&mut use_count, o);
    }

    let mut used_names: BTreeMap<String, IRVarId> = BTreeMap::new();
    let mut wires: BTreeMap<IRVarId, NamedWire> = BTreeMap::new();
    let mut inputs = Vec::with_capacity(circuit.params.len());

    for (i, ty) in circuit.params.iter().enumerate() {
        let id = IRVarId(i as u32);
        let raw = opt
            .wires
            .var(id)
            .map(str::to_string)
            .unwrap_or_else(|| format!("in{i}"));
        let name = take_name(id, &raw, &sanitize, &mut used_names)?;
        let wire = NamedWire {
            id,
            name,
            bind: true,
        };
        inputs.push(NamedVolarParam {
            wire: wire.clone(),
            ty: *ty,
        });
        wires.insert(id, wire);
    }

    let mut stmts = Vec::with_capacity(ops.len());
    for (idx, op) in ops.into_iter().enumerate() {
        let id = IRVarId(n_params + idx as u32);
        if let Some(ty) = result_ty(&op) {
            type_of.insert(id, ty);
        }
        let explicit = opt.wires.is_explicit_var(id);
        let uses = use_count.get(&id).copied().unwrap_or(0);
        let bind = opt.name_all_wires || explicit || uses != 1 || op.is_effect();
        let raw = opt
            .wires
            .var(id)
            .map(str::to_string)
            .unwrap_or_else(|| format!("w{}", id.0));
        let name = take_name(id, &raw, &sanitize, &mut used_names)?;
        let dst = NamedWire { id, name, bind };
        wires.insert(id, dst.clone());
        stmts.push(NamedVolarStmt { dst, op });
    }

    for &o in &circuit.outputs {
        if o.0 >= var_space {
            return Err(EmitError::UnknownVar { id: o });
        }
    }

    let mut storage_names = BTreeMap::new();
    for sid in storages {
        let raw = opt
            .wires
            .storage(sid)
            .map(str::to_string)
            .unwrap_or_else(|| format!("mem{}", sid.0));
        let name = sanitize(&raw)?;
        require_ident(&name, "storage name")?;
        if storage_names.values().any(|n| n == &name) {
            return Err(EmitError::NameCollision { name });
        }
        storage_names.insert(sid, name);
    }

    Ok(NamedVolarCircuit {
        types: types.clone(),
        inputs,
        stmts,
        outputs: circuit.outputs.clone(),
        storage_names,
        wires,
        type_of,
    })
}

fn take_name(
    id: IRVarId,
    raw: &str,
    sanitize: impl Fn(&str) -> Result<String, EmitError>,
    used: &mut BTreeMap<String, IRVarId>,
) -> Result<String, EmitError> {
    let name = sanitize(raw)?;
    require_ident(&name, "wire name")?;
    if let Some(prev) = used.get(&name) {
        if *prev != id {
            return Err(EmitError::NameCollision { name });
        }
    }
    used.insert(name.clone(), id);
    Ok(name)
}

fn bool_op(stmt: &BIrStmt, storages: &mut BTreeSet<StorageId>) -> NamedBoolOp {
    match stmt {
        BIrStmt::Zero => NamedBoolOp::Zero,
        BIrStmt::One => NamedBoolOp::One,
        BIrStmt::And(a, b) => NamedBoolOp::And(*a, *b),
        BIrStmt::Or(a, b) => NamedBoolOp::Or(*a, *b),
        BIrStmt::Xor(a, b) => NamedBoolOp::Xor(*a, *b),
        BIrStmt::Not(a) => NamedBoolOp::Not(*a),
        BIrStmt::StorageRead {
            storage,
            lane,
            addr,
        } => {
            storages.insert(*storage);
            NamedBoolOp::StorageRead {
                storage: *storage,
                lane: *lane,
                addr: addr.clone(),
            }
        }
        BIrStmt::StorageWrite {
            storage,
            lane,
            src,
            addr,
        } => {
            storages.insert(*storage);
            NamedBoolOp::StorageWrite {
                storage: *storage,
                lane: *lane,
                src: *src,
                addr: addr.clone(),
            }
        }
        BIrStmt::OracleCall { name, .. }
        | BIrStmt::OracleBit { name, .. } => NamedBoolOp::External {
            kind: ExternalKind::Oracle,
            name: name.clone(),
        },
        BIrStmt::OracleProjectedBit { .. } => NamedBoolOp::External {
            kind: ExternalKind::Oracle,
            name: String::from("<projected>"),
        },
        BIrStmt::ActionCall { name, .. } | BIrStmt::ActionStoreBit { name, .. } => {
            NamedBoolOp::External {
                kind: ExternalKind::Action,
                name: name.clone(),
            }
        }
        BIrStmt::ActionBit { .. } => NamedBoolOp::External {
            kind: ExternalKind::Action,
            name: String::from("<action-bit>"),
        },
        BIrStmt::Rng { name } | BIrStmt::RngBit { name, .. } => NamedBoolOp::External {
            kind: ExternalKind::Rng,
            name: name.clone(),
        },
        _ => panic!("name_bool: unhandled BIrStmt variant — add lowering for this variant"),
    }
}

fn volar_op(
    stmt: &IRStmt,
    storages: &mut BTreeSet<StorageId>,
    bit_ty: Option<volar_ir_common::TypeId>,
) -> NamedVolarOp {
    match stmt {
        Stmt::Const(value, ty) => NamedVolarOp::Const {
            value: *value,
            ty: *ty,
        },
        Stmt::Transmute {
            src,
            src_ty,
            dst_ty,
        } => NamedVolarOp::Transmute {
            src: *src,
            src_ty: *src_ty,
            dst_ty: *dst_ty,
        },
        Stmt::Poly {
            ty,
            coeffs,
            constant,
        } => NamedVolarOp::Poly {
            ty: *ty,
            coeffs: coeffs.clone(),
            constant: *constant,
        },
        Stmt::Rol { src, ty, n } => NamedVolarOp::Rol {
            src: *src,
            ty: *ty,
            n: *n,
        },
        Stmt::Ror { src, ty, n } => NamedVolarOp::Ror {
            src: *src,
            ty: *ty,
            n: *n,
        },
        Stmt::Merge { parts, ty } => NamedVolarOp::Merge {
            parts: parts.clone(),
            ty: *ty,
        },
        Stmt::Splat { src, ty } => NamedVolarOp::Splat {
            src: *src,
            ty: *ty,
        },
        Stmt::Shuffle { result_bits, ty } => NamedVolarOp::Shuffle {
            result_bits: result_bits.clone(),
            ty: *ty,
        },
        Stmt::StorageRead { storage, ty, addr } => {
            storages.insert(*storage);
            NamedVolarOp::StorageRead {
                storage: *storage,
                ty: *ty,
                addr: *addr,
            }
        }
        Stmt::StorageWrite {
            storage,
            src,
            ty,
            addr,
        } => {
            storages.insert(*storage);
            NamedVolarOp::StorageWrite {
                storage: *storage,
                src: *src,
                ty: *ty,
                addr: *addr,
            }
        }
        Stmt::OracleCall { name, .. } => NamedVolarOp::External {
            kind: ExternalKind::Oracle,
            name: name.clone(),
        },
        Stmt::OracleOutput { .. } => NamedVolarOp::External {
            kind: ExternalKind::Oracle,
            name: String::from("<oracle-output>"),
        },
        Stmt::ActionCall { name, .. } | Stmt::ActionStore { name, .. } => NamedVolarOp::External {
            kind: ExternalKind::Action,
            name: name.clone(),
        },
        Stmt::ActionOutput { .. } => NamedVolarOp::External {
            kind: ExternalKind::Action,
            name: String::from("<action-output>"),
        },
        Stmt::Rng { name, .. } => NamedVolarOp::External {
            kind: ExternalKind::Rng,
            name: name.clone(),
        },
        _ => {
            let _ = bit_ty;
            panic!("name_volar: unhandled Stmt variant — add lowering for this variant")
        }
    }
}

fn result_ty(op: &NamedVolarOp) -> Option<volar_ir_common::TypeId> {
    match op {
        NamedVolarOp::Const { ty, .. }
        | NamedVolarOp::Poly { ty, .. }
        | NamedVolarOp::Rol { ty, .. }
        | NamedVolarOp::Ror { ty, .. }
        | NamedVolarOp::Merge { ty, .. }
        | NamedVolarOp::Splat { ty, .. }
        | NamedVolarOp::Shuffle { ty, .. }
        | NamedVolarOp::StorageRead { ty, .. } => Some(*ty),
        NamedVolarOp::Transmute { dst_ty, .. } => Some(*dst_ty),
        NamedVolarOp::StorageWrite { .. } | NamedVolarOp::External { .. } => None,
    }
}
