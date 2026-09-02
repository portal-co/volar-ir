// @reliability: normal
// @ai: assisted
//! Host-side interpretation of the POD2 gate encodings (Product / Sum).
//!
//! Used to check that a Boolar witness satisfies the generated predicates
//! without depending on the nightly `pod2` crate.

use std::collections::BTreeMap;

use volar_circuit_source::{
    bits_to_u64, const_bit, BoolStorageMap, EmitError, NamedBoolCircuit, NamedBoolOp,
};
use volar_ir::ir::IRVarId;

pub fn product(a: i64, b: i64) -> i64 {
    a * b
}

pub fn sum(a: i64, b: i64) -> i64 {
    a + b
}

pub fn check_bool_and(a: bool, b: bool, out: bool) -> bool {
    product(i(a), i(b)) == i(out)
}

pub fn check_bool_not(a: bool, out: bool) -> bool {
    sum(i(out), i(a)) == 1
}

/// `out = a + b - 2ab` via Sum/Product (see generated `bool_xor`).
pub fn check_bool_xor(a: bool, b: bool, out: bool) -> bool {
    let and_ab = product(i(a), i(b));
    let two_ab = sum(and_ab, and_ab);
    let sum_ab = sum(i(a), i(b));
    sum(i(out), two_ab) == sum_ab
}

/// `out = a + b - ab` via Sum/Product (see generated `bool_or`).
pub fn check_bool_or(a: bool, b: bool, out: bool) -> bool {
    let and_ab = product(i(a), i(b));
    let sum_ab = sum(i(a), i(b));
    sum(i(out), and_ab) == sum_ab
}

fn i(b: bool) -> i64 {
    if b { 1 } else { 0 }
}

/// Evaluate the circuit, then check every gate against the Product/Sum encoding.
pub fn verify_named_bool(
    circuit: &NamedBoolCircuit,
    inputs: &[bool],
) -> Result<bool, EmitError> {
    let mut storage = BoolStorageMap::new();
    let mut vals: BTreeMap<IRVarId, bool> = BTreeMap::new();
    if inputs.len() != circuit.inputs.len() {
        return Err(EmitError::unsupported(
            "verify arity",
            format!(
                "expected {} inputs, got {}",
                circuit.inputs.len(),
                inputs.len()
            ),
        ));
    }
    for (inp, &bit) in circuit.inputs.iter().zip(inputs.iter()) {
        vals.insert(inp.id, bit);
    }
    for stmt in &circuit.stmts {
        if stmt.op.is_external() {
            return Err(EmitError::unsupported(
                "oracle/action/rng",
                "POD2 verifier rejects externals",
            ));
        }
        let computed = eval_op(&stmt.op, &vals, &mut storage)?;
        if !gate_holds(&stmt.op, &vals, computed, &storage)? {
            return Ok(false);
        }
        vals.insert(stmt.dst.id, computed);
    }
    Ok(true)
}

fn eval_op(
    op: &NamedBoolOp,
    vals: &BTreeMap<IRVarId, bool>,
    storage: &mut BoolStorageMap,
) -> Result<bool, EmitError> {
    let get = |id: IRVarId| vals.get(&id).copied().ok_or(EmitError::UnknownVar { id });
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
            let idx = addr_u64(addr, vals)?;
            Ok(storage.get(&((*sid, *lane), idx)).copied().unwrap_or(false))
        }
        NamedBoolOp::StorageWrite {
            storage: sid,
            lane,
            src,
            addr,
        } => {
            let idx = addr_u64(addr, vals)?;
            storage.insert(((*sid, *lane), idx), get(*src)?);
            Ok(false)
        }
        NamedBoolOp::External { .. } => Err(EmitError::unsupported(
            "oracle/action/rng",
            "POD2 verifier rejects externals",
        )),
    }
}

fn gate_holds(
    op: &NamedBoolOp,
    vals: &BTreeMap<IRVarId, bool>,
    out: bool,
    storage: &BoolStorageMap,
) -> Result<bool, EmitError> {
    let get = |id: IRVarId| vals.get(&id).copied().ok_or(EmitError::UnknownVar { id });
    Ok(match op {
        NamedBoolOp::Zero => !out,
        NamedBoolOp::One => out,
        NamedBoolOp::And(a, b) => check_bool_and(get(*a)?, get(*b)?, out),
        NamedBoolOp::Or(a, b) => check_bool_or(get(*a)?, get(*b)?, out),
        NamedBoolOp::Xor(a, b) => check_bool_xor(get(*a)?, get(*b)?, out),
        NamedBoolOp::Not(a) => check_bool_not(get(*a)?, out),
        NamedBoolOp::StorageRead {
            storage: sid,
            lane,
            addr,
        } => {
            let idx = addr_u64(addr, vals)?;
            storage.get(&((*sid, *lane), idx)).copied().unwrap_or(false) == out
        }
        NamedBoolOp::StorageWrite { src, .. } => {
            // Dummy result is 0; the write itself is a ContainerUpdate.
            !out && get(*src).is_ok()
        }
        NamedBoolOp::External { .. } => false,
    })
}

fn addr_u64(addr: &[IRVarId], vals: &BTreeMap<IRVarId, bool>) -> Result<u64, EmitError> {
    let bits: Result<Vec<bool>, EmitError> = addr
        .iter()
        .map(|id| vals.get(id).copied().ok_or(EmitError::UnknownVar { id: *id }))
        .collect();
    bits_to_u64(&bits?)
}

/// Fold a storage address when every bit is a `Zero`/`One` statement.
pub fn folded_addr(circuit: &NamedBoolCircuit, addr: &[IRVarId]) -> Result<Option<u64>, EmitError> {
    let mut bits = Vec::with_capacity(addr.len());
    for id in addr {
        match const_bit(circuit, *id) {
            Some(b) => bits.push(b),
            None => return Ok(None),
        }
    }
    Ok(Some(bits_to_u64(&bits)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xor_encoding_matches_bit_xor() {
        for a in [false, true] {
            for b in [false, true] {
                assert!(check_bool_xor(a, b, a ^ b));
                assert!(!check_bool_xor(a, b, !(a ^ b)));
            }
        }
    }

    #[test]
    fn or_encoding_matches_bit_or() {
        for a in [false, true] {
            for b in [false, true] {
                assert!(check_bool_or(a, b, a | b));
                assert!(!check_bool_or(a, b, !(a | b)));
            }
        }
    }
}
