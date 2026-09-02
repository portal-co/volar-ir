// @reliability: normal
// @ai: assisted

use std::collections::{BTreeMap, BTreeSet};

use volar_circuit_source::{
    EmitError, EmitOptions, NamedBoolCircuit, NamedBoolOp, SourceFile, SourcePackage,
};
use volar_ir::ir::IRVarId;

use crate::check::folded_addr;
use crate::ident::sanitize_ident;

/// Max native statements per custom predicate (POD2 default arity is 5;
/// leave one slot of slack).
const CHUNK: usize = 4;

pub fn emit_bool(
    circuit: &NamedBoolCircuit,
    opt: &EmitOptions,
) -> Result<SourcePackage, EmitError> {
    if circuit.has_externals() {
        return Err(EmitError::unsupported(
            "oracle/action/rng",
            "v1 POD2 backend rejects externals",
        ));
    }

    let pkg = sanitize_ident(&opt.package_name)?;
    let root = sanitize_ident(&opt.circuit_name)?;

    let mut stmts = Vec::new();
    let mut privates: BTreeSet<String> = BTreeSet::new();
    let mut storage_cur: BTreeMap<volar_ir_common::StorageId, String> = BTreeMap::new();
    let mut storage_write_count: BTreeMap<volar_ir_common::StorageId, usize> = BTreeMap::new();

    for inp in &circuit.inputs {
        stmts.push(format!(
            "DictContains(wires, \"{}\", {})",
            inp.name, inp.name
        ));
        privates.insert(inp.name.clone());
    }

    for stmt in &circuit.stmts {
        match &stmt.op {
            NamedBoolOp::Zero => {
                stmts.push(format!("Equal({}, 0)", stmt.dst.name));
                privates.insert(stmt.dst.name.clone());
            }
            NamedBoolOp::One => {
                stmts.push(format!("Equal({}, 1)", stmt.dst.name));
                privates.insert(stmt.dst.name.clone());
            }
            NamedBoolOp::And(a, b) => {
                stmts.push(format!(
                    "bool_and({}, {}, {})",
                    wire(circuit, *a)?,
                    wire(circuit, *b)?,
                    stmt.dst.name
                ));
                privates.insert(stmt.dst.name.clone());
            }
            NamedBoolOp::Or(a, b) => {
                stmts.push(format!(
                    "bool_or({}, {}, {})",
                    wire(circuit, *a)?,
                    wire(circuit, *b)?,
                    stmt.dst.name
                ));
                privates.insert(stmt.dst.name.clone());
            }
            NamedBoolOp::Xor(a, b) => {
                stmts.push(format!(
                    "bool_xor({}, {}, {})",
                    wire(circuit, *a)?,
                    wire(circuit, *b)?,
                    stmt.dst.name
                ));
                privates.insert(stmt.dst.name.clone());
            }
            NamedBoolOp::Not(a) => {
                stmts.push(format!(
                    "bool_not({}, {})",
                    wire(circuit, *a)?,
                    stmt.dst.name
                ));
                privates.insert(stmt.dst.name.clone());
            }
            NamedBoolOp::StorageRead {
                storage,
                addr,
                ..
            } => {
                let mem = current_storage(&mut storage_cur, circuit, *storage)?;
                let idx = addr_term(circuit, addr, &mut stmts, &mut privates)?;
                stmts.push(format!(
                    "ArrayContains({mem}, {idx}, {})",
                    stmt.dst.name
                ));
                privates.insert(stmt.dst.name.clone());
            }
            NamedBoolOp::StorageWrite {
                storage,
                src,
                addr,
                ..
            } => {
                let mem = current_storage(&mut storage_cur, circuit, *storage)?;
                let idx = addr_term(circuit, addr, &mut stmts, &mut privates)?;
                let src_n = wire(circuit, *src)?;
                let n = storage_write_count.entry(*storage).or_insert(0);
                *n += 1;
                let next = format!("{mem}_s{n}");
                stmts.push(format!("ArrayUpdate({mem}, {idx}, {src_n}, {next})"));
                stmts.push(format!("Equal({}, 0)", stmt.dst.name));
                privates.insert(stmt.dst.name.clone());
                privates.insert(next.clone());
                storage_cur.insert(*storage, next);
            }
            NamedBoolOp::External { kind, name } => {
                return Err(EmitError::unsupported(
                    format!("{kind:?} `{name}`"),
                    "v1 POD2 backend rejects externals",
                ));
            }
        }
        if stmt.dst.bind || circuit.outputs.contains(&stmt.dst.id) {
            stmts.push(format!(
                "DictContains(wires, \"{}\", {})",
                stmt.dst.name, stmt.dst.name
            ));
        }
    }

    for &oid in &circuit.outputs {
        let name = wire(circuit, oid)?;
        privates.insert(name);
    }

    let mut publics = vec![String::from("wires")];
    for (sid, name) in &circuit.storage_names {
        publics.push(name.clone());
        if storage_write_count.get(sid).copied().unwrap_or(0) > 0 {
            if let Some(final_root) = storage_cur.get(sid) {
                if final_root != name {
                    publics.push(final_root.clone());
                    privates.remove(final_root);
                }
            }
        }
    }

    let mut out = String::from(
        "// Generated by volar-pod2-backend. Reusable Podlang module.\n\
         // Embedders import by batch hash (`use module 0x… as alias`) and apply\n\
         // the root predicate. This *verifies* a witness of wire values.\n\
         //\n\
         // MainPod slot limits (defaults): 5 statements / predicate, 5 predicates\n\
         // / batch, 2 batches, 5 custom-predicate verifications. Large circuits\n\
         // are chunked; compose leftover chunks with recursive MainPods.\n\n",
    );
    out.push_str(GATE_LIB);

    let preds = emit_chunk_tree(&root, &publics, &privates, &stmts);
    out.push_str(&preds);

    let embed = format!(
        "// Embedder-owned request. Replace 0x… with the module Merkle root.\n\
         use module 0x0000000000000000000000000000000000000000000000000000000000000000 as {pkg}\n\
         REQUEST(\n\
           {pkg}::{root}(wires)\n\
         )\n"
    );

    Ok(SourcePackage::new(vec![
        SourceFile::new(format!("{pkg}.podlang"), out),
        SourceFile::new("examples/embed.podlang", embed),
    ]))
}

const GATE_LIB: &str = "\
bool_and(a, b, out) = AND(\n\
  Product(a, b, out)\n\
)\n\
\n\
bool_or(a, b, out) = AND(\n\
  Product(a, b, and_ab)\n\
  Sum(a, b, sum_ab)\n\
  Sum(out, and_ab, sum_ab)\n\
)\n\
\n\
bool_not(a, out) = AND(\n\
  Sum(out, a, 1)\n\
)\n\
\n\
bool_xor(a, b, out) = AND(\n\
  Product(a, b, and_ab)\n\
  Sum(and_ab, and_ab, two_ab)\n\
  Sum(a, b, sum_ab)\n\
  Sum(out, two_ab, sum_ab)\n\
)\n\n";

fn wire(circuit: &NamedBoolCircuit, id: IRVarId) -> Result<String, EmitError> {
    circuit
        .wire_name(id)
        .map(str::to_string)
        .ok_or(EmitError::UnknownVar { id })
}

fn current_storage(
    cur: &mut BTreeMap<volar_ir_common::StorageId, String>,
    circuit: &NamedBoolCircuit,
    sid: volar_ir_common::StorageId,
) -> Result<String, EmitError> {
    if let Some(name) = cur.get(&sid) {
        return Ok(name.clone());
    }
    let name = circuit
        .storage_names
        .get(&sid)
        .cloned()
        .ok_or_else(|| EmitError::unsupported("storage", format!("unnamed storage {}", sid.0)))?;
    cur.insert(sid, name.clone());
    Ok(name)
}

fn addr_term(
    circuit: &NamedBoolCircuit,
    addr: &[IRVarId],
    stmts: &mut Vec<String>,
    privates: &mut BTreeSet<String>,
) -> Result<String, EmitError> {
    if let Some(idx) = folded_addr(circuit, addr)? {
        return Ok(idx.to_string());
    }
    if addr.len() > 64 {
        return Err(EmitError::AddressTooWide { bits: addr.len() });
    }
    // idx = Σ addr_bit[i] * 2^i
    let mut acc = String::from("0");
    for (i, bit) in addr.iter().enumerate() {
        let b = wire(circuit, *bit)?;
        let w = 1u64 << i;
        let term = format!("{b}_w{i}");
        stmts.push(format!("Product({b}, {w}, {term})"));
        privates.insert(term.clone());
        let next = format!("{b}_s{i}");
        stmts.push(format!("Sum({acc}, {term}, {next})"));
        privates.insert(next.clone());
        acc = next;
    }
    Ok(acc)
}

fn emit_chunk_tree(
    root: &str,
    publics: &[String],
    privates: &BTreeSet<String>,
    stmts: &[String],
) -> String {
    if stmts.is_empty() {
        return format!(
            "{root}({}) = AND(\n  Equal(1, 1)\n)\n",
            publics.join(", ")
        );
    }

    let priv_list: Vec<String> = privates.iter().cloned().collect();
    let all_args = |pubs: &[String], privs: &[String]| -> String {
        if privs.is_empty() {
            pubs.join(", ")
        } else if pubs.is_empty() {
            format!("private: {}", privs.join(", "))
        } else {
            format!("{}, private: {}", pubs.join(", "), privs.join(", "))
        }
    };

    let chunks: Vec<&[String]> = stmts.chunks(CHUNK).collect();
    if chunks.len() == 1 {
        return format!(
            "{root}({}) = AND(\n{}\n)\n",
            all_args(publics, &priv_list),
            format_and_body(chunks[0])
        );
    }

    let mut out = String::new();
    let mut child_names = Vec::new();
    for (i, chunk) in chunks.iter().enumerate() {
        let name = format!("{root}_c{i}");
        out.push_str(&format!(
            "{name}({}) = AND(\n{}\n)\n\n",
            all_args(publics, &priv_list),
            format_and_body(chunk)
        ));
        child_names.push(name);
    }

    // Combine children in a tree of AND-of-4.
    let mut level = child_names;
    let mut level_id = 0u32;
    while level.len() > 1 {
        let mut next = Vec::new();
        for (i, group) in level.chunks(CHUNK).enumerate() {
            let name = if level.len() <= CHUNK && i == 0 && group.len() == level.len() {
                root.to_string()
            } else if group.len() == level.len() && group.len() <= CHUNK {
                root.to_string()
            } else {
                format!("{root}_n{level_id}_{i}")
            };
            let calls: Vec<String> = group
                .iter()
                .map(|c| format!("{}({})", c, call_args(publics, &priv_list)))
                .collect();
            out.push_str(&format!(
                "{name}({}) = AND(\n{}\n)\n\n",
                all_args(publics, &priv_list),
                format_and_body(&calls)
            ));
            next.push(name);
        }
        if next.len() == 1 && next[0] == root {
            break;
        }
        if next.len() == 1 {
            // wrap as root
            let only = &next[0];
            if only != root {
                out.push_str(&format!(
                    "{root}({}) = AND(\n  {}({})\n)\n",
                    all_args(publics, &priv_list),
                    only,
                    call_args(publics, &priv_list)
                ));
            }
            break;
        }
        level = next;
        level_id += 1;
    }
    out
}

fn call_args(publics: &[String], privates: &[String]) -> String {
    let mut args = publics.to_vec();
    args.extend(privates.iter().cloned());
    args.join(", ")
}

fn format_and_body(stmts: &[String]) -> String {
    stmts
        .iter()
        .map(|s| format!("  {s}"))
        .collect::<Vec<_>>()
        .join("\n")
}
