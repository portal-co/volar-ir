// @reliability: normal
// @ai: assisted

use std::collections::BTreeMap;

use volar_ir::boolar::LaneId;
use volar_ir::ir::IRVarId;
use volar_ir_common::StorageId;

use crate::error::EmitError;
use crate::named::{NamedBoolCircuit, NamedBoolOp};

/// Storage map used by [`eval_named_bool`]: `((StorageId, LaneId), addr)`.
pub type BoolStorageMap = BTreeMap<((StorageId, LaneId), u64), bool>;

/// Evaluate a named Boolar circuit. Address bits collapse LSB-first to `u64`.
pub fn eval_named_bool(
    circuit: &NamedBoolCircuit,
    inputs: &[bool],
    storage: &mut BoolStorageMap,
) -> Result<Vec<bool>, EmitError> {
    if inputs.len() != circuit.inputs.len() {
        return Err(EmitError::unsupported(
            "eval input arity",
            format!(
                "expected {} inputs, got {}",
                circuit.inputs.len(),
                inputs.len()
            ),
        ));
    }
    apply_pre_init(circuit, storage);

    let mut vals: BTreeMap<IRVarId, bool> = BTreeMap::new();
    for (i, inp) in circuit.inputs.iter().enumerate() {
        vals.insert(inp.id, inputs[i]);
    }

    for stmt in &circuit.stmts {
        if stmt.op.is_external() {
            return Err(EmitError::unsupported(
                "oracle/action/rng",
                "v1 circuit-source evaluators reject externals",
            ));
        }
        let v = eval_op(&stmt.op, &vals, storage)?;
        vals.insert(stmt.dst.id, v);
    }

    let mut outs = Vec::with_capacity(circuit.outputs.len());
    for &id in &circuit.outputs {
        let bit = vals
            .get(&id)
            .copied()
            .ok_or(EmitError::UnknownVar { id })?;
        outs.push(bit);
    }
    Ok(outs)
}

/// Collapse LSB-first address bits to a `u64`. Fails above 64 bits.
pub fn bits_to_u64(bits: &[bool]) -> Result<u64, EmitError> {
    if bits.len() > 64 {
        return Err(EmitError::AddressTooWide { bits: bits.len() });
    }
    let mut result = 0u64;
    for (i, &b) in bits.iter().enumerate() {
        if b {
            result |= 1u64 << i;
        }
    }
    Ok(result)
}

fn apply_pre_init(circuit: &NamedBoolCircuit, storage: &mut BoolStorageMap) {
    for seg in &circuit.pre_init {
        for (i, &bit) in seg.data.iter().enumerate() {
            storage.insert(((seg.storage, seg.lane), seg.offset + i as u64), bit);
        }
    }
}

fn eval_op(
    op: &NamedBoolOp,
    vals: &BTreeMap<IRVarId, bool>,
    storage: &mut BoolStorageMap,
) -> Result<bool, EmitError> {
    let get = |id: IRVarId| -> Result<bool, EmitError> {
        vals.get(&id).copied().ok_or(EmitError::UnknownVar { id })
    };
    match op {
        NamedBoolOp::Zero => Ok(false),
        NamedBoolOp::One => Ok(true),
        NamedBoolOp::And(a, b) => Ok(get(*a)? & get(*b)?),
        NamedBoolOp::Or(a, b) => Ok(get(*a)? | get(*b)?),
        NamedBoolOp::Xor(a, b) => Ok(get(*a)? ^ get(*b)?),
        NamedBoolOp::Not(a) => Ok(!get(*a)?),
        NamedBoolOp::StorageRead {
            storage: sid,
            lane,
            addr,
        } => {
            let bits = addr_bits(addr, get)?;
            let idx = bits_to_u64(&bits)?;
            Ok(storage.get(&((*sid, *lane), idx)).copied().unwrap_or(false))
        }
        NamedBoolOp::StorageWrite {
            storage: sid,
            lane,
            src,
            addr,
        } => {
            let bits = addr_bits(addr, get)?;
            let idx = bits_to_u64(&bits)?;
            storage.insert(((*sid, *lane), idx), get(*src)?);
            Ok(false)
        }
        NamedBoolOp::External { .. } => Err(EmitError::unsupported(
            "oracle/action/rng",
            "v1 circuit-source evaluators reject externals",
        )),
    }
}

fn addr_bits(
    addr: &[IRVarId],
    mut get: impl FnMut(IRVarId) -> Result<bool, EmitError>,
) -> Result<Vec<bool>, EmitError> {
    let _ = ();
    addr.iter().map(|id| get(*id)).collect()
}

/// Look up a const `Zero`/`One` definition. Used to fold POD2 storage indices.
pub fn const_bit(circuit: &NamedBoolCircuit, id: IRVarId) -> Option<bool> {
    match circuit.stmt_defining(id).map(|s| &s.op) {
        Some(NamedBoolOp::Zero) => Some(false),
        Some(NamedBoolOp::One) => Some(true),
        _ => None,
    }
}
