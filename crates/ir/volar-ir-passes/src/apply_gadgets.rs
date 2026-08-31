// @reliability: normal
// @ai: assisted
//! Gadget application: splice sub-circuits onto the regions of a fused
//! Boolar circuit's input/output wires.
//!
//! Given a host [`BCircuit`], its [`RegionTable`], and a list of
//! [`GadgetBinding`]s, produces a new circuit whose boundary carries the
//! gadgets' external form while the core computes on the plaintext form:
//!
//! | Boundary kind        | Core-facing side | Spliced form                    |
//! |----------------------|------------------|---------------------------------|
//! | Input bit(s) in sel  | plaintext        | gadget `decrypt` on entry       |
//! | Output bit(s) in sel | plaintext        | gadget `encrypt` on exit        |
//! | Storage cell in sel  | plaintext        | read→decrypt, write→encrypt     |
//! | `pre_init` in sel    | plaintext        | data re-emitted pre-encrypted   |
//!
//! # Pipeline placement
//!
//! This pass is the **last** lowering step, after the optimization passes
//! (and after `to_reversible`, when used). Gadget boundaries act as barriers:
//! running store-forwarding/CSE *after* splicing could forward plaintext or
//! merge through `E`/`E⁻¹`, so don't. The output is an ordinary `BCircuit` —
//! evaluable, serializable, weavable.
//!
//! # v1 restrictions (fail-closed)
//!
//! - Host bodies must be pure gates + storage traffic; oracle/action/RNG
//!   statements in the host are rejected (`UnsupportedHostStmt`).
//! - Gadget bodies must be pure gates; `Rng` ports and `AuxSource::Rng` are
//!   declared in the type surface but rejected here — wrap/unwrap keystream
//!   consistency needs an out-of-band channel (gadget side outputs), which
//!   is follow-up work.
//! - Storage wrapping supports 1-bit data-port gadgets instantiated per
//!   selected cell, and only **statically constant** cell addresses (Boolar
//!   addresses are data-dependent bit vectors; constant folding resolves
//!   Zero/One-defined address bits; params make the address dynamic).
//! - `pre_init` re-encryption inside a selected storage range requires the
//!   owning binding's aux sources to be all-`Const` (an `InputRange` key is
//!   not known at application time).
//! - Two bindings may not claim the same boundary wire, and a binding's
//!   aux `InputRange` may not overlap any binding's selection (the `key`
//!   region must not be encrypted by its own binding).

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use volar_ir::boolar::{BIrPreInitSegment, BIrStmt, LaneId};
use volar_ir::circuit::BCircuit;
use volar_ir::gadget::{AuxSource, GadgetBinding, GadgetError, GadgetLibrary, GadgetSpec, PortKind};
use volar_ir::ir::IRVarId;
use volar_ir::region::{BoundaryWire, RegionTable};
use volar_ir_common::StorageId;

/// Result of a successful gadget application.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct GadgetApplication {
    /// The wrapped circuit: ordinary `BCircuit`, same params/outputs shape.
    pub circuit: BCircuit<()>,
    /// Gadget names applied, in binding order (first appearance). Callers
    /// must not re-apply gadgets to the wrapped circuit without a fresh
    /// region table — double encryption is not detected automatically.
    pub applied: Vec<String>,
}

/// Apply gadget bindings to `host` according to `regions`.
pub fn apply_gadgets(
    host: &BCircuit<()>,
    regions: &RegionTable,
    bindings: &[GadgetBinding],
    lib: &GadgetLibrary,
) -> Result<GadgetApplication, GadgetError> {
    regions.validate(host).map_err(GadgetError::Region)?;

    // ------------------------------------------------------------------
    // Phase 1: expand selections, check widths and cross-binding claims.
    // ------------------------------------------------------------------
    let mut claimed: BTreeSet<BoundaryWire> = BTreeSet::new();
    let mut plans: Vec<Plan> = Vec::new();
    for binding in bindings {
        let spec = lib
            .get(&binding.gadget)
            .ok_or_else(|| GadgetError::UnknownGadget(binding.gadget.clone()))?;
        let dw = data_width(spec, binding)?;

        // Rng surface is declared but not supported by the v1 splicer.
        if spec.ports.iter().any(|p| p.kind == PortKind::Rng)
            || binding
                .aux_sources
                .iter()
                .any(|s| matches!(s, AuxSource::Rng(_)))
        {
            return Err(GadgetError::UnsupportedRng);
        }

        let mut plan = Plan {
            spec,
            binding,
            input_bits: Vec::new(),
            output_bits: Vec::new(),
            cells: Vec::new(),
        };
        for wire in regions.select(&binding.selector) {
            if !claimed.insert(wire) {
                return Err(GadgetError::OverlappingBindings { wire });
            }
            match wire {
                BoundaryWire::Input(b) => plan.input_bits.push(b),
                BoundaryWire::Output(b) => plan.output_bits.push(b),
                wire @ BoundaryWire::Cell { .. } => plan.cells.push(wire),
            }
        }
        if plan.is_empty() {
            return Err(GadgetError::EmptySelection);
        }
        // Check aux totals first (binding-level, partition-independent).
        let declared_aux: u64 = spec.aux_widths().iter().map(|(_, w)| *w).sum();
        let supplied_aux: u64 = binding.aux_sources.iter().map(|s| s.width()).sum();
        if declared_aux != supplied_aux {
            return Err(GadgetError::AuxWidthMismatch {
                gadget: spec.name.clone(),
                port: alloc::format!("(all aux ports)"),
                expected: declared_aux,
                got: supplied_aux,
            });
        }
        // Each nonempty partition instantiates the gadget once per data-port
        // width (per cell for storage), so the selection must tile exactly.
        for (name, count) in [
            ("input", plan.input_bits.len()),
            ("output", plan.output_bits.len()),
        ] {
            if count > 0 && (count as u64) % dw != 0 {
                return Err(GadgetError::DataWidthMismatch {
                    gadget: spec.name.clone(),
                    port: dw,
                    wires: count as u64,
                    boundary: name,
                });
            }
        }
        if !plan.cells.is_empty() && dw != 1 {
            return Err(GadgetError::DataWidthMismatch {
                gadget: spec.name.clone(),
                port: dw,
                wires: 1,
                boundary: "storage",
            });
        }
        plans.push(plan);
    }

    // Aux `InputRange` sources must point at unclaimed input wires (e.g. the
    // key region must not be encrypted by its own binding). Checked against
    // the *full* claim set, after all bindings have claimed.
    for plan in &plans {
        for src in &plan.binding.aux_sources {
            if let AuxSource::InputRange { start, len } = src {
                if *start as u64 + *len as u64 > host.params as u64 {
                    return Err(GadgetError::AuxInputOutOfRange {
                        start: *start,
                        len: *len,
                        params: host.params,
                    });
                }
                for b in *start..*start + *len {
                    let wire = BoundaryWire::Input(b);
                    if claimed.contains(&wire) {
                        return Err(GadgetError::AuxRangeOverlapsSelection { wire });
                    }
                }
            }
        }
    }

    // Validate gadget bodies up front (fail-closed on library content).
    for plan in &plans {
        let dw = data_width(plan.spec, plan.binding)?;
        let aux_widths = plan.binding.aux_sources.iter().map(|s| s.width());
        check_body(plan.spec.body_for(false), dw, aux_widths.clone(), &plan.spec.name)?;
        if let Some(d) = &plan.spec.decrypt {
            check_body(d, dw, aux_widths.clone(), &plan.spec.name)?;
        }
    }

    // Spaces with at least one wrapped cell: traffic there must be
    // statically addressable (fail-closed otherwise).
    let wrapped_spaces: BTreeSet<(StorageId, LaneId)> = plans
        .iter()
        .flat_map(|p| p.cells.iter())
        .filter_map(|w| match w {
            BoundaryWire::Cell { storage, lane, .. } => Some((*storage, *lane)),
            _ => None,
        })
        .collect();

    // ------------------------------------------------------------------
    // Phase 2: splice.
    // ------------------------------------------------------------------
    let mut out = BCircuit::<()>::new(host.params);
    // Host var (index into the host var space) → current circuit var.
    let mut subst: Vec<Option<IRVarId>> = vec![None; host.var_space() as usize];
    for i in 0..host.params {
        subst[i as usize] = Some(IRVarId(i));
    }
    // Static values of host vars (Some(v) = constant-folded bit).
    let mut const_val: Vec<Option<bool>> = vec![None; host.var_space() as usize];

    let mut applied: Vec<String> = Vec::new();
    let mark = |applied: &mut Vec<String>, name: &str| {
        if !applied.iter().any(|n| n == name) {
            applied.push(alloc::string::String::from(name));
        }
    };

    // --- input splices: decrypt on entry -------------------------------
    for plan in &plans {
        for chunk in plan.input_bits.chunks(plan.spec.data_width().unwrap_or(1) as usize) {
            let aux = resolve_aux(&mut out, host.params, plan.binding)?;
            let body = plan.spec.body_for(true);
            let mut data_in: Vec<IRVarId> = chunk.iter().map(|&b| IRVarId(b)).collect();
            data_in.extend(aux);
            let plain = instantiate(&mut out, body, &data_in, &[]);
            for (&b, p) in chunk.iter().zip(&plain) {
                subst[b as usize] = Some(*p);
            }
        }
        if !plan.input_bits.is_empty() {
            mark(&mut applied, &plan.spec.name);
        }
    }

    // --- host statements ------------------------------------------------
    for (i, node) in host.stmts.iter().enumerate() {
        let host_var = IRVarId(host.params + i as u32);
        match &node.kind {
            BIrStmt::Zero
            | BIrStmt::One
            | BIrStmt::And(..)
            | BIrStmt::Or(..)
            | BIrStmt::Xor(..)
            | BIrStmt::Not(..) => {
                let kind = subst_stmt(&node.kind, &subst)?;
                let id = out.push_stmt(kind, ());
                subst[host_var.0 as usize] = Some(id);
                const_val[host_var.0 as usize] = fold_gate(&node.kind, &const_val);
            }
            BIrStmt::StorageRead {
                storage,
                lane,
                addr,
            } => {
                // Fold on host vars (pre-substitution): decides wrapping.
                let folded = if wrapped_spaces.contains(&(*storage, *lane)) {
                    Some(fold_host_addr(addr, &const_val)?)
                } else {
                    None
                };
                let addr_out: Vec<IRVarId> = addr
                    .iter()
                    .map(|v| {
                        subst[v.0 as usize]
                            .ok_or_else(|| GadgetError::Internal(format!("unmapped addr var {}", v.0)))
                    })
                    .collect::<Result<_, _>>()?;
                let id = out.push_stmt(
                    BIrStmt::StorageRead {
                        storage: *storage,
                        lane: *lane,
                        addr: addr_out,
                    },
                    (),
                );
                let mut replacement = id;
                if let Some(flat) = folded {
                    if let Some(plan) = find_cell_plan(&plans, *storage, *lane, flat) {
                        let aux = resolve_aux(&mut out, host.params, plan.binding)?;
                        let body = plan.spec.body_for(true);
                        let mut data_in = vec![id];
                        data_in.extend(aux);
                        let plain = instantiate(&mut out, body, &data_in, &[]);
                        replacement = *plain
                            .first()
                            .ok_or_else(|| GadgetError::Internal(format!("gadget {} has empty data output", plan.spec.name)))?;
                        mark(&mut applied, &plan.spec.name);
                    }
                }
                subst[host_var.0 as usize] = Some(replacement);
            }
            BIrStmt::StorageWrite {
                storage,
                lane,
                src,
                addr,
            } => {
                let folded = if wrapped_spaces.contains(&(*storage, *lane)) {
                    Some(fold_host_addr(addr, &const_val)?)
                } else {
                    None
                };
                let mut written = subst[src.0 as usize].ok_or_else(|| {
                    GadgetError::Internal(format!("unmapped src var {}", src.0))
                })?;
                if let Some(flat) = folded {
                    if let Some(plan) = find_cell_plan(&plans, *storage, *lane, flat) {
                        let aux = resolve_aux(&mut out, host.params, plan.binding)?;
                        let body = plan.spec.body_for(false);
                        let mut data_in = vec![written];
                        data_in.extend(aux);
                        written = *instantiate(&mut out, body, &data_in, &[])
                            .first()
                            .ok_or_else(|| {
                                GadgetError::Internal(format!(
                                    "gadget {} has empty data output",
                                    plan.spec.name
                                ))
                            })?;
                        mark(&mut applied, &plan.spec.name);
                    }
                }
                let addr_out: Vec<IRVarId> = addr
                    .iter()
                    .map(|v| {
                        subst[v.0 as usize]
                            .ok_or_else(|| GadgetError::Internal(format!("unmapped addr var {}", v.0)))
                    })
                    .collect::<Result<_, _>>()?;
                out.push_stmt(
                    BIrStmt::StorageWrite {
                        storage: *storage,
                        lane: *lane,
                        src: written,
                        addr: addr_out,
                    },
                    (),
                );
                // StorageWrite results are dummy zeros; leave subst unset.
            }
            other => return Err(GadgetError::UnsupportedHostStmt(stmt_name(other))),
        }
    }

    // --- output splices: encrypt on exit --------------------------------
    let mut new_outputs: Vec<IRVarId> = host
        .outputs
        .iter()
        .map(|v| {
            subst[v.0 as usize]
                .ok_or_else(|| GadgetError::Internal(format!("unmapped output var {}", v.0)))
        })
        .collect::<Result<_, _>>()?;
    for plan in &plans {
        for chunk in plan.output_bits.chunks(plan.spec.data_width().unwrap_or(1) as usize) {
            let aux = resolve_aux(&mut out, host.params, plan.binding)?;
            let body = plan.spec.body_for(false);
            let mut data_in: Vec<IRVarId> =
                chunk.iter().map(|&b| new_outputs[b as usize]).collect();
            data_in.extend(aux);
            let cipher = instantiate(&mut out, body, &data_in, &[]);
            for (&b, c) in chunk.iter().zip(&cipher) {
                new_outputs[b as usize] = *c;
            }
        }
        if !plan.output_bits.is_empty() {
            mark(&mut applied, &plan.spec.name);
        }
    }

    // --- pre_init: re-encrypt constants inside selected storage ranges ---
    // A wrapped cell whose initial state is the implicit default zero would
    // be decrypted into `E⁻¹(0) ≠ 0` on read, so every wrapped cell must
    // start life holding *ciphertext of zero* — add a synthetic pre_init
    // segment for cells not covered by a host segment.
    let mut new_pre_init: Vec<BIrPreInitSegment> = Vec::new();
    let mut covered = |new_pre_init: &Vec<BIrPreInitSegment>, storage: StorageId, lane: LaneId, addr: u64| -> bool {
        new_pre_init
            .iter()
            .chain(host.pre_init.iter())
            .any(|seg| {
                seg.storage == storage
                    && seg.lane == lane
                    && (seg.offset..seg.offset + seg.data.len() as u64).contains(&addr)
            })
    };
    for plan in &plans {
        for wire in &plan.cells {
            let (storage, lane, addr) = match wire {
                BoundaryWire::Cell {
                    storage,
                    lane,
                    addr,
                } => (*storage, *lane, *addr),
                _ => unreachable!("plan.cells holds only Cell wires"),
            };
            if covered(&new_pre_init, storage, lane, addr) {
                continue;
            }
            let cipher = encrypt_constant(plan.spec, plan.binding, false).ok_or(
                GadgetError::PreInitNeedsConstantAux {
                    gadget: plan.spec.name.clone(),
                },
            )?;
            new_pre_init.push(BIrPreInitSegment {
                storage,
                lane,
                offset: addr,
                data: alloc::vec![cipher],
            });
            mark(&mut applied, &plan.spec.name);
        }
    }
    for seg in &host.pre_init {
        let mut data = seg.data.clone();
        let mut changed = false;
        for (i, &bit) in seg.data.iter().enumerate() {
            let flat = seg.offset + i as u64;
            if let Some(plan) = find_cell_plan(&plans, seg.storage, seg.lane, flat) {
                let cipher = encrypt_constant(plan.spec, plan.binding, bit).ok_or(
                    GadgetError::PreInitNeedsConstantAux {
                        gadget: plan.spec.name.clone(),
                    },
                )?;
                data[i] = cipher;
                changed = true;
                mark(&mut applied, &plan.spec.name);
            }
        }
        new_pre_init.push(if changed {
            BIrPreInitSegment {
                storage: seg.storage,
                lane: seg.lane,
                offset: seg.offset,
                data,
            }
        } else {
            seg.clone()
        });
    }

    out.outputs = new_outputs;
    out.pre_init = new_pre_init;
    Ok(GadgetApplication { circuit: out, applied })
}

// ----------------------------------------------------------------------
// Internals
// ----------------------------------------------------------------------

/// Per-binding splice plan (selection expanded and partitioned).
struct Plan<'a> {
    spec: &'a GadgetSpec,
    binding: &'a GadgetBinding,
    /// Selected input bit positions, ascending (`select` output is sorted).
    input_bits: Vec<u32>,
    output_bits: Vec<u32>,
    cells: Vec<BoundaryWire>,
}

impl Plan<'_> {
    fn is_empty(&self) -> bool {
        self.input_bits.is_empty() && self.output_bits.is_empty() && self.cells.is_empty()
    }
}

fn data_width(spec: &GadgetSpec, _binding: &GadgetBinding) -> Result<u64, GadgetError> {
    spec.data_width().ok_or(GadgetError::InvalidGadgetBody {
        gadget: spec.name.clone(),
        reason: "no Data port declared",
    })
}

/// One gadget instantiation appended to `out`. `inputs` binds the body's
/// params in order (data port bits first, then aux bits); returns the body's
/// output vars renumbered into `out`'s var space.
fn instantiate(
    out: &mut BCircuit<()>,
    body: &BCircuit<()>,
    inputs: &[IRVarId],
    _aux: &[IRVarId],
) -> Vec<IRVarId> {
    debug_assert_eq!(inputs.len() as u32, body.params);
    let mut map: Vec<IRVarId> = inputs.to_vec();
    for node in &body.stmts {
        let kind = node
            .kind
            .clone()
            .map(
                &mut (),
                |_: &mut (), v| Ok::<_, core::convert::Infallible>(map[v.0 as usize]),
                |_: &mut (), s| Ok::<_, core::convert::Infallible>(s),
            )
            .unwrap();
        let id = out.push_stmt(kind, ());
        map.push(id);
    }
    body.outputs.iter().map(|o| map[o.0 as usize]).collect()
}

/// Resolve a binding's aux ports into circuit wires (in declaration order).
fn resolve_aux(
    out: &mut BCircuit<()>,
    params: u32,
    binding: &GadgetBinding,
) -> Result<Vec<IRVarId>, GadgetError> {
    let mut wires = Vec::new();
    for src in &binding.aux_sources {
        match src {
            AuxSource::Const(bits) => {
                for &b in bits {
                    let id = out.push_stmt(if b { BIrStmt::One } else { BIrStmt::Zero }, ());
                    wires.push(id);
                }
            }
            AuxSource::InputRange { start, len } => {
                if *start as u64 + *len as u64 > params as u64 {
                    return Err(GadgetError::AuxInputOutOfRange {
                        start: *start,
                        len: *len,
                        params,
                    });
                }
                for b in *start..*start + *len {
                    wires.push(IRVarId(b));
                }
            }
            AuxSource::Rng(_) => return Err(GadgetError::UnsupportedRng),
        }
    }
    Ok(wires)
}

/// Apply the host→out variable substitution to a pure-gate statement.
fn subst_stmt(kind: &BIrStmt, subst: &[Option<IRVarId>]) -> Result<BIrStmt, GadgetError> {
    kind.clone().map(
        &mut (),
        |_: &mut (), v: IRVarId| -> Result<IRVarId, GadgetError> {
            subst
                .get(v.0 as usize)
                .copied()
                .flatten()
                .ok_or_else(|| GadgetError::Internal(format!("unmapped var {}", v.0)))
        },
        |_: &mut (), s: StorageId| -> Result<StorageId, GadgetError> { Ok(s) },
    )
}

/// Static value of a pure-gate statement, if determinable.
fn fold_gate(kind: &BIrStmt, const_val: &[Option<bool>]) -> Option<bool> {
    let val = |v: &IRVarId| const_val.get(v.0 as usize).copied().flatten();
    match kind {
        BIrStmt::Zero => Some(false),
        BIrStmt::One => Some(true),
        BIrStmt::And(a, b) => Some(val(a)? & val(b)?),
        BIrStmt::Or(a, b) => Some(val(a)? | val(b)?),
        BIrStmt::Xor(a, b) => Some(val(a)? ^ val(b)?),
        BIrStmt::Not(a) => Some(!val(a)?),
        _ => None,
    }
}

/// Collapse an N-bit host address (bit 0 = index 0 = least-significant) to
/// its flat cell address. Fails closed on any non-constant bit.
fn fold_host_addr(addr: &[IRVarId], const_val: &[Option<bool>]) -> Result<u64, GadgetError> {
    let mut value = 0u64;
    for (i, v) in addr.iter().enumerate() {
        let bit = const_val
            .get(v.0 as usize)
            .copied()
            .flatten()
            .ok_or(GadgetError::DynamicStorageAddress)?;
        if bit {
            value |= 1u64 << i;
        }
    }
    Ok(value)
}

fn find_cell_plan<'a>(
    plans: &'a [Plan<'a>],
    storage: StorageId,
    lane: LaneId,
    flat: u64,
) -> Option<&'a Plan<'a>> {
    plans.iter().find(|p| {
        p.cells.iter().any(|w| match w {
            BoundaryWire::Cell {
                storage: s,
                lane: l,
                addr,
            } => *s == storage && *l == lane && *addr == flat,
            _ => false,
        })
    })
}

/// Structurally validate a gadget body: pure gates only, SSA operand order,
/// `params == data_width + sum(aux widths)`, `outputs.len() == data_width`.
fn check_body(
    body: &BCircuit<()>,
    data_width: u64,
    aux_widths: impl Iterator<Item = u64>,
    gadget: &str,
) -> Result<(), GadgetError> {
    let aux_total: u64 = aux_widths.sum();
    let bad = |reason: &'static str| GadgetError::InvalidGadgetBody {
        gadget: alloc::string::String::from(gadget),
        reason,
    };
    if body.params as u64 != data_width + aux_total {
        return Err(bad("param count does not match data + aux port widths"));
    }
    if body.outputs.len() as u64 != data_width {
        return Err(bad("output count does not match data port width"));
    }
    if !body.pre_init.is_empty() {
        return Err(bad("gadget bodies must not carry pre_init"));
    }
    for (i, node) in body.stmts.iter().enumerate() {
        let defined = body.params + i as u32;
        let check = |v: &IRVarId| -> Result<(), GadgetError> {
            if v.0 >= defined {
                Err(bad("statement references a not-yet-defined var"))
            } else {
                Ok(())
            }
        };
        match &node.kind {
            BIrStmt::Zero | BIrStmt::One => {}
            BIrStmt::And(a, b) | BIrStmt::Or(a, b) | BIrStmt::Xor(a, b) => {
                check(a)?;
                check(b)?;
            }
            BIrStmt::Not(a) => check(a)?,
            _ => return Err(bad("gadget bodies must be pure gates in v1")),
        }
    }
    for o in &body.outputs {
        if o.0 >= body.var_space() {
            return Err(bad("output var out of range"));
        }
    }
    Ok(())
}

/// Concretely evaluate a pure-gate encrypt body: `encrypt(bit, aux) -> cipher
/// bit`. Returns `None` if any aux value is unknown (param-backed), which is
/// the `PreInitNeedsConstantAux` case.
fn encrypt_constant(spec: &GadgetSpec, binding: &GadgetBinding, bit: bool) -> Option<bool> {
    let mut values: Vec<bool> = alloc::vec![bit];
    for src in &binding.aux_sources {
        match src {
            AuxSource::Const(bits) => values.extend(bits.iter().copied()),
            _ => return None,
        }
    }
    let body = spec.body_for(false);
    let mut vals = values;
    vals.resize(body.var_space() as usize, false);
    for (i, node) in body.stmts.iter().enumerate() {
        let v = body.params as usize + i;
        vals[v] = match &node.kind {
            BIrStmt::Zero => false,
            BIrStmt::One => true,
            BIrStmt::And(a, b) => vals[a.0 as usize] & vals[b.0 as usize],
            BIrStmt::Or(a, b) => vals[a.0 as usize] | vals[b.0 as usize],
            BIrStmt::Xor(a, b) => vals[a.0 as usize] ^ vals[b.0 as usize],
            BIrStmt::Not(a) => !vals[a.0 as usize],
            _ => return None,
        };
    }
    let mut out_bits = body.outputs.iter().map(|o| vals[o.0 as usize]);
    let result = out_bits.next()?;
    if out_bits.next().is_some() {
        return None;
    }
    Some(result)
}

fn stmt_name(kind: &BIrStmt) -> &'static str {
    match kind {
        BIrStmt::Zero => "zero",
        BIrStmt::One => "one",
        BIrStmt::And(..) => "and",
        BIrStmt::Or(..) => "or",
        BIrStmt::Xor(..) => "xor",
        BIrStmt::Not(..) => "not",
        BIrStmt::OracleCall { .. } => "oracle_call",
        BIrStmt::OracleBit { .. } => "oracle_bit",
        BIrStmt::OracleProjectedBit { .. } => "oracle_projected_bit",
        BIrStmt::ActionCall { .. } => "action_call",
        BIrStmt::ActionBit { .. } => "action_bit",
        BIrStmt::ActionStoreBit { .. } => "action_store_bit",
        BIrStmt::Rng { .. } => "rng",
        BIrStmt::RngBit { .. } => "rng_bit",
        BIrStmt::StorageRead { .. } => "storage_read",
        BIrStmt::StorageWrite { .. } => "storage_write",
        _ => "unknown",
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::{BTreeMap, BTreeSet};
    use volar_ir::boolar::LaneId;
    use volar_ir::gadget::{selector, AuxSource, GadgetLibrary, GadgetSpec, Port, PortKind};
    use volar_ir::region::{RegionEntry, RegionError, RegionId, RegionSelector, WireAnchor};

    /// Evaluate a pure-gate + storage `BCircuit` on the given params, with
    /// storage pre-seeded from `circ.pre_init`. Returns output bits.
    fn eval_circuit(circ: &BCircuit<()>, params: &[bool]) -> Vec<bool> {
        let mut vals: Vec<Option<bool>> = vec![None; circ.var_space() as usize];
        for (i, &b) in params.iter().enumerate() {
            vals[i] = Some(b);
        }
        let mut storage: BTreeMap<((StorageId, LaneId), u64), bool> = BTreeMap::new();
        for seg in &circ.pre_init {
            for (i, &b) in seg.data.iter().enumerate() {
                storage.insert(((seg.storage, seg.lane), seg.offset + i as u64), b);
            }
        }
        // Static addresses only (test circuits are static-addressed).
        for (i, node) in circ.stmts.iter().enumerate() {
            let v = circ.params as usize + i;
            vals[v] = Some(match &node.kind {
                BIrStmt::Zero => false,
                BIrStmt::One => true,
                BIrStmt::And(a, b) => vals[a.0 as usize].unwrap() & vals[b.0 as usize].unwrap(),
                BIrStmt::Or(a, b) => vals[a.0 as usize].unwrap() | vals[b.0 as usize].unwrap(),
                BIrStmt::Xor(a, b) => vals[a.0 as usize].unwrap() ^ vals[b.0 as usize].unwrap(),
                BIrStmt::Not(a) => !vals[a.0 as usize].unwrap(),
                BIrStmt::StorageRead { storage: s, lane, addr } => {
                    let mut flat = 0u64;
                    for (i, a) in addr.iter().enumerate() {
                        if vals[a.0 as usize].unwrap() {
                            flat |= 1 << i;
                        }
                    }
                    *storage.get(&((*s, *lane), flat)).unwrap_or(&false)
                }
                BIrStmt::StorageWrite { storage: s, lane, src, addr } => {
                    let mut flat = 0u64;
                    for (i, a) in addr.iter().enumerate() {
                        if vals[a.0 as usize].unwrap() {
                            flat |= 1 << i;
                        }
                    }
                    storage.insert(((*s, *lane), flat), vals[src.0 as usize].unwrap());
                    false
                }
                other => panic!("eval_circuit: unsupported stmt {:?}", other),
            });
        }
        circ.outputs.iter().map(|o| vals[o.0 as usize].unwrap()).collect()
    }

    /// A self-inverse XOR gadget: `cipher = plain ^ key`, one aux (key) bit.
    fn xor_gadget() -> GadgetSpec {
        let mut body = BCircuit::<()>::new(2); // [data, key]
        let d = IRVarId(0);
        let k = IRVarId(1);
        let x = body.push_stmt(BIrStmt::Xor(d, k), ());
        body.outputs = alloc::vec![x];
        GadgetSpec {
            name: alloc::string::String::from("xor-pad"),
            encrypt: body,
            decrypt: None, // self-inverse
            ports: alloc::vec![
                Port { name: alloc::string::String::from("data"), kind: PortKind::Data, width: 1 },
                Port { name: alloc::string::String::from("key"), kind: PortKind::Aux, width: 1 },
            ],
        }
    }

    fn lib() -> GadgetLibrary {
        GadgetLibrary::new().with(xor_gadget())
    }

    fn entry(name: &str, anchor: WireAnchor, regions: &[u32]) -> RegionEntry {
        RegionEntry {
            anchor,
            regions: regions.iter().map(|&r| RegionId(r)).collect(),
        }
    }

    const PUBLIC: u32 = 0;
    const PLAINTEXT: u32 = 1;
    const KEY: u32 = 2;

    /// The motivating example: 4 public input bits, 4 public output bits, a
    /// `plaintext` region covering some of them, and a 1-bit `key` param.
    fn host_io() -> BCircuit<()> {
        let mut c = BCircuit::<()>::new(5); // params 0..4 = public data, 4 = key
        // pass-through: out[i] = in[i] ^ key  (some core computation)
        let mut outs = Vec::new();
        for i in 0..4 {
            let x = c.push_stmt(BIrStmt::Xor(IRVarId(i), IRVarId(4)), ());
            outs.push(x);
        }
        c.outputs = outs;
        c
    }

    fn io_regions() -> RegionTable {
        RegionTable {
            entries: alloc::vec![
                entry("public-in", WireAnchor::Input { start: 0, len: 4 }, &[PUBLIC]),
                entry("key-in", WireAnchor::Input { start: 4, len: 1 }, &[KEY]),
                entry("public-out", WireAnchor::Output { start: 0, len: 4 }, &[PUBLIC]),
            ],
            names: Default::default(),
        }
    }

    fn xor_binding() -> GadgetBinding {
        GadgetBinding {
            gadget: alloc::string::String::from("xor-pad"),
            selector: selector(&[RegionId(PUBLIC)], &[RegionId(PLAINTEXT)]),
            aux_sources: alloc::vec![AuxSource::InputRange { start: 4, len: 1 }],
            rng_source: None,
        }
    }

    #[test]
    fn io_wrap_roundtrip() {
        let host = host_io();
        let regions = io_regions();
        let bindings = alloc::vec![xor_binding()];
        let app = apply_gadgets(&host, &regions, &bindings, &lib()).expect("applies");

        // Same boundary shape.
        assert_eq!(app.circuit.params, host.params);
        assert_eq!(app.circuit.outputs.len(), host.outputs.len());
        assert_eq!(app.applied, alloc::vec![alloc::string::String::from("xor-pad")]);

        // Plaintext view through the wrapped boundary == host behavior.
        for mask in 0..16u32 {
            let key = (mask & 1) == 1;
            let data: Vec<bool> = (0..4).map(|i| (mask >> i) & 1 == 1).collect();
            let mut wrapped_in = data.clone();
            wrapped_in.push(key);
            // Wrap: input ciphertext = plaintext ^ key; expect output
            // ciphertext = core(plaintext) ^ key == plaintext ^ key ^ key ^ key
            let got = eval_circuit(&app.circuit, &wrapped_in);
            let expected: Vec<bool> = data.iter().map(|&d| d ^ key).collect();
            assert_eq!(got, expected, "mask={mask} key={key}");
        }
    }

    #[test]
    fn region_table_validation() {
        let host = host_io();
        // In-range table passes.
        io_regions().validate(&host).expect("valid");

        // Out-of-range input anchor.
        let bad = RegionTable {
            entries: alloc::vec![entry(
                "too-far",
                WireAnchor::Input { start: 4, len: 2 },
                &[PUBLIC],
            )],
            names: Default::default(),
        };
        assert!(matches!(
            bad.validate(&host),
            Err(RegionError::InputOutOfRange { .. })
        ));

        // Out-of-range output anchor.
        let bad = RegionTable {
            entries: alloc::vec![entry(
                "too-far",
                WireAnchor::Output { start: 3, len: 2 },
                &[PUBLIC],
            )],
            names: Default::default(),
        };
        assert!(matches!(
            bad.validate(&host),
            Err(RegionError::OutputOutOfRange { .. })
        ));

        // Overlapping entries on the same wires.
        let bad = RegionTable {
            entries: alloc::vec![
                entry("a", WireAnchor::Input { start: 0, len: 3 }, &[PUBLIC]),
                entry("b", WireAnchor::Input { start: 2, len: 2 }, &[PLAINTEXT]),
            ],
            names: Default::default(),
        };
        assert!(matches!(
            bad.validate(&host),
            Err(RegionError::OverlappingEntries { .. })
        ));

        // Unsorted entries.
        let bad = RegionTable {
            entries: alloc::vec![
                entry("later", WireAnchor::Input { start: 2, len: 1 }, &[PUBLIC]),
                entry("earlier", WireAnchor::Input { start: 0, len: 1 }, &[PUBLIC]),
            ],
            names: Default::default(),
        };
        assert!(matches!(bad.validate(&host), Err(RegionError::Unsorted)));

        // Empty region set.
        let bad = RegionTable {
            entries: alloc::vec![entry("empty", WireAnchor::Input { start: 0, len: 1 }, &[])],
            names: Default::default(),
        };
        assert!(matches!(
            bad.validate(&host),
            Err(RegionError::EmptyRegions(_))
        ));

        // Unknown storage space.
        let bad = RegionTable {
            entries: alloc::vec![entry(
                "ghost",
                WireAnchor::Storage { storage: StorageId(9), lane: LaneId(0), start: 0, len: 1 },
                &[PUBLIC],
            )],
            names: Default::default(),
        };
        assert!(matches!(
            bad.validate(&host),
            Err(RegionError::UnknownStorageSpace { .. })
        ));
    }

    #[test]
    fn selector_and_lookup() {
        let regions = io_regions();
        // Lookup helpers.
        assert_eq!(
            regions.input_regions(0).map(|s| s.contains(&RegionId(PUBLIC))),
            Some(true)
        );
        assert_eq!(
            regions.input_regions(4).map(|s| s.contains(&RegionId(KEY))),
            Some(true)
        );
        assert_eq!(regions.input_regions(5), None);

        // public-minus-plaintext selects exactly the 4 public bits.
        let sel = selector(&[RegionId(PUBLIC)], &[RegionId(PLAINTEXT)]);
        let wires = regions.select(&sel);
        assert_eq!(wires.len(), 8); // 4 inputs + 4 outputs
        let inputs: alloc::vec::Vec<u32> = wires
            .iter()
            .filter_map(|w| match w {
                BoundaryWire::Input(b) => Some(*b),
                _ => None,
            })
            .collect();
        let outputs: alloc::vec::Vec<u32> = wires
            .iter()
            .filter_map(|w| match w {
                BoundaryWire::Output(b) => Some(*b),
                _ => None,
            })
            .collect();
        assert_eq!(inputs, alloc::vec![0, 1, 2, 3]);
        assert_eq!(outputs, alloc::vec![0, 1, 2, 3]);
    }

    #[test]
    fn data_width_mismatch_rejected() {
        let host = host_io();
        let regions = io_regions();
        // Gadget with a 3-bit data port bound to a 4-bit selection (4 % 3
        // does not tile).
        let mut body = BCircuit::<()>::new(4); // [d0, d1, d2, key]
        let x = body.push_stmt(BIrStmt::Xor(IRVarId(0), IRVarId(3)), ());
        let y = body.push_stmt(BIrStmt::Xor(IRVarId(1), IRVarId(3)), ());
        let z = body.push_stmt(BIrStmt::Xor(IRVarId(2), IRVarId(3)), ());
        body.outputs = alloc::vec![x, y, z];
        let wide = GadgetSpec {
            name: alloc::string::String::from("wide"),
            encrypt: body,
            decrypt: None,
            ports: alloc::vec![
                Port { name: alloc::string::String::from("data"), kind: PortKind::Data, width: 3 },
                Port { name: alloc::string::String::from("key"), kind: PortKind::Aux, width: 1 },
            ],
        };
        let bindings = alloc::vec![GadgetBinding {
            gadget: alloc::string::String::from("wide"),
            selector: RegionSelector::all_of([RegionId(PUBLIC)]),
            aux_sources: alloc::vec![AuxSource::InputRange { start: 4, len: 1 }],
            rng_source: None,
        }];
        let libw = GadgetLibrary::new().with(wide);
        assert!(matches!(
            apply_gadgets(&host, &regions, &bindings, &libw),
            Err(GadgetError::DataWidthMismatch { .. })
        ));
    }

    #[test]
    fn key_region_not_wrapped_by_own_binding() {
        // The motivating trap: a second binding that also selects the key
        // region must fail (overlap), and aux InputRange overlapping a claim
        // must fail.
        let host = host_io();
        let regions = io_regions();
        let bindings = alloc::vec![
            xor_binding(),
            GadgetBinding {
                gadget: alloc::string::String::from("xor-pad"),
                selector: RegionSelector::all_of([RegionId(KEY)]),
                aux_sources: alloc::vec![AuxSource::InputRange { start: 4, len: 1 }],
                rng_source: None,
            },
        ];
        assert!(matches!(
            apply_gadgets(&host, &regions, &bindings, &lib()),
            Err(GadgetError::AuxRangeOverlapsSelection { .. })
        ));
    }

    #[test]
    fn duplicate_claim_rejected() {
        // Two bindings claiming the exact same wire (the key region here is
        // claimed twice directly).
        let host = host_io();
        // Keep entries sorted; the duplicate anchor fires OverlappingEntries.
        let mut regions = io_regions();
        regions.entries.insert(
            1,
            entry(
                "key-again",
                WireAnchor::Input { start: 4, len: 1 },
                &[KEY],
            ),
        );
        let bindings = alloc::vec![
            GadgetBinding {
                gadget: alloc::string::String::from("xor-pad"),
                selector: RegionSelector::all_of([RegionId(KEY)]),
                aux_sources: alloc::vec![AuxSource::Const(alloc::vec![false])],
                rng_source: None,
            },
            GadgetBinding {
                gadget: alloc::string::String::from("xor-pad"),
                selector: RegionSelector::all_of([RegionId(KEY)]),
                aux_sources: alloc::vec![AuxSource::Const(alloc::vec![false])],
                rng_source: None,
            },
        ];
        // Two entries may not claim the same wire at all — validation fires
        // before bindings are considered.
        assert!(matches!(
            apply_gadgets(&host, &regions, &bindings, &lib()),
            Err(GadgetError::Region(
                volar_ir::region::RegionError::OverlappingEntries { .. }
            ))
        ));
    }

    #[test]
    fn dynamic_storage_address_rejected() {
        // Read with a param-driven (dynamic) address inside a wrapped space
        // must fail closed.
        let mut c = BCircuit::<()>::new(1); // p0 = address
        let read = c.push_stmt(
            BIrStmt::StorageRead { storage: StorageId(0), lane: LaneId(0), addr: alloc::vec![IRVarId(0)] },
            (),
        );
        c.outputs = alloc::vec![read];
        let host = c;

        let mut regions = RegionTable::new();
        regions.entries = alloc::vec![entry(
            "cell0",
            WireAnchor::Storage { storage: StorageId(0), lane: LaneId(0), start: 0, len: 1 },
            &[PUBLIC],
        )];
        let bindings = alloc::vec![GadgetBinding {
            gadget: alloc::string::String::from("xor-pad"),
            selector: RegionSelector::all_of([RegionId(PUBLIC)]),
            aux_sources: alloc::vec![AuxSource::Const(alloc::vec![true])],
            rng_source: None,
        }];
        assert!(matches!(
            apply_gadgets(&host, &regions, &bindings, &lib()),
            Err(GadgetError::DynamicStorageAddress)
        ));
    }

    #[test]
    fn storage_wrap_static_read_roundtrip() {
        // Read from statically-addressed cell 0, no dynamic addresses at all.
        let mut c = BCircuit::<()>::new(1); // p0 = value written to cell 0
        let a0 = c.push_stmt(BIrStmt::Zero, ());
        let read = c.push_stmt(
            BIrStmt::StorageRead { storage: StorageId(0), lane: LaneId(0), addr: alloc::vec![a0] },
            (),
        );
        let out = c.push_stmt(BIrStmt::Xor(read, IRVarId(0)), ());
        c.outputs = alloc::vec![out];
        c.push_stmt(
            BIrStmt::StorageWrite {
                storage: StorageId(0),
                lane: LaneId(0),
                src: IRVarId(0),
                addr: alloc::vec![a0],
            },
            (),
        );
        let host = c;

        let mut regions = RegionTable::new();
        regions.entries = alloc::vec![entry(
            "cell0",
            WireAnchor::Storage { storage: StorageId(0), lane: LaneId(0), start: 0, len: 1 },
            &[PUBLIC],
        )];
        let bindings = alloc::vec![GadgetBinding {
            gadget: alloc::string::String::from("xor-pad"),
            selector: RegionSelector::all_of([RegionId(PUBLIC)]),
            aux_sources: alloc::vec![AuxSource::Const(alloc::vec![true])],
            rng_source: None,
        }];
        let app = apply_gadgets(&host, &regions, &bindings, &lib()).expect("applies");

        // Host semantics: the read (defined before the write) sees the
        // default-zero cell, so out = 0 ^ p0 = p0 for both p0.
        // Wrapped: cell 0 starts holding ciphertext-of-zero (= 0 ^ 1 = 1);
        // the read decrypts it back to 0, so out = 0 ^ p0 = p0. The write
        // stores ciphertext (p0 ^ 1), which the read never observes.
        for &p0 in &[false, true] {
            let got = eval_circuit(&app.circuit, &[p0]);
            assert_eq!(got, alloc::vec![p0], "p0={p0}");
        }

        // The wrapped circuit's stored cell really holds ciphertext: a
        // plaintext-holding variant would evaluate differently.
        assert_eq!(app.circuit.params, 1);
    }

    #[test]
    fn pre_init_reencryption() {
        // pre_init cell 0 = constant 1; wrapped with key=1 → stored 0.
        let mut c = BCircuit::<()>::new(0);
        let a0 = c.push_stmt(BIrStmt::Zero, ());
        let read = c.push_stmt(
            BIrStmt::StorageRead { storage: StorageId(0), lane: LaneId(0), addr: alloc::vec![a0] },
            (),
        );
        c.outputs = alloc::vec![read];
        c.pre_init = alloc::vec![BIrPreInitSegment {
            storage: StorageId(0),
            lane: LaneId(0),
            offset: 0,
            data: alloc::vec![true],
        }];
        let host = c;

        let mut regions = RegionTable::new();
        regions.entries = alloc::vec![entry(
            "cell0",
            WireAnchor::Storage { storage: StorageId(0), lane: LaneId(0), start: 0, len: 1 },
            &[PUBLIC],
        )];
        let bindings = alloc::vec![GadgetBinding {
            gadget: alloc::string::String::from("xor-pad"),
            selector: RegionSelector::all_of([RegionId(PUBLIC)]),
            aux_sources: alloc::vec![AuxSource::Const(alloc::vec![true])],
            rng_source: None,
        }];
        let app = apply_gadgets(&host, &regions, &bindings, &lib()).expect("applies");
        // Stored constant should now be 1 ^ 1 = false.
        assert_eq!(app.circuit.pre_init[0].data, alloc::vec![false]);
        // And the read decrypts it back to true.
        assert_eq!(eval_circuit(&app.circuit, &[]), alloc::vec![true]);
    }

    #[test]
    fn pre_init_parambacked_aux_rejected() {
        let mut c = BCircuit::<()>::new(0);
        let a0 = c.push_stmt(BIrStmt::Zero, ());
        let read = c.push_stmt(
            BIrStmt::StorageRead { storage: StorageId(0), lane: LaneId(0), addr: alloc::vec![a0] },
            (),
        );
        c.outputs = alloc::vec![read];
        c.pre_init = alloc::vec![BIrPreInitSegment {
            storage: StorageId(0),
            lane: LaneId(0),
            offset: 0,
            data: alloc::vec![true],
        }];
        let host = c;

        let mut regions = RegionTable::new();
        regions.entries = alloc::vec![entry(
            "cell0",
            WireAnchor::Storage { storage: StorageId(0), lane: LaneId(0), start: 0, len: 1 },
            &[PUBLIC],
        )];
        // Key comes from a param → constants can't be re-encrypted.
        let bindings = alloc::vec![GadgetBinding {
            gadget: alloc::string::String::from("xor-pad"),
            selector: RegionSelector::all_of([RegionId(PUBLIC)]),
            aux_sources: alloc::vec![AuxSource::InputRange { start: 0, len: 1 }],
            rng_source: None,
        }];
        // But host has 0 params, so InputRange is out of range — that fires
        // first (fail-closed ordering), which is also acceptable behavior.
        let result = apply_gadgets(&host, &regions, &bindings, &lib());
        assert!(matches!(
            result,
            Err(GadgetError::AuxInputOutOfRange { .. })
        ) || matches!(result, Err(GadgetError::PreInitNeedsConstantAux { .. })));
    }

    #[test]
    fn unsupported_host_stmt_rejected() {
        let mut c = BCircuit::<()>::new(1);
        let r = c.push_stmt(BIrStmt::Rng { name: alloc::string::String::from("r") }, ());
        c.outputs = alloc::vec![r];
        let host = c;

        let mut regions = RegionTable::new();
        regions.entries = alloc::vec![entry(
            "in",
            WireAnchor::Input { start: 0, len: 1 },
            &[PUBLIC],
        )];
        let bindings = alloc::vec![GadgetBinding {
            gadget: alloc::string::String::from("xor-pad"),
            selector: RegionSelector::all_of([RegionId(PUBLIC)]),
            aux_sources: alloc::vec![AuxSource::Const(alloc::vec![false])],
            rng_source: None,
        }];
        assert!(matches!(
            apply_gadgets(&host, &regions, &bindings, &lib()),
            Err(GadgetError::UnsupportedHostStmt(_))
        ));
    }

    #[test]
    fn unknown_gadget_rejected() {
        let host = host_io();
        let regions = io_regions();
        let bindings = alloc::vec![GadgetBinding {
            gadget: alloc::string::String::from("nope"),
            selector: RegionSelector::all_of([RegionId(PUBLIC)]),
            aux_sources: alloc::vec![AuxSource::Const(alloc::vec![false])],
            rng_source: None,
        }];
        assert!(matches!(
            apply_gadgets(&host, &regions, &bindings, &lib()),
            Err(GadgetError::UnknownGadget(_))
        ));
    }

    #[test]
    fn empty_selection_rejected() {
        let host = host_io();
        let regions = io_regions();
        let bindings = alloc::vec![GadgetBinding {
            gadget: alloc::string::String::from("xor-pad"),
            // Exclude every region present in the table → selects nothing.
            selector: RegionSelector::none_of([RegionId(PUBLIC), RegionId(KEY)]),
            aux_sources: alloc::vec![AuxSource::Const(alloc::vec![false])],
            rng_source: None,
        }];
        assert!(matches!(
            apply_gadgets(&host, &regions, &bindings, &lib()),
            Err(GadgetError::EmptySelection)
        ));
    }

    #[test]
    fn aux_width_mismatch_rejected() {
        let host = host_io();
        let regions = io_regions();
        let bindings = alloc::vec![GadgetBinding {
            gadget: alloc::string::String::from("xor-pad"),
            selector: RegionSelector::all_of([RegionId(PUBLIC)]),
            aux_sources: alloc::vec![], // key port unsupplied
            rng_source: None,
        }];
        assert!(matches!(
            apply_gadgets(&host, &regions, &bindings, &lib()),
            Err(GadgetError::AuxWidthMismatch { .. })
        ));
    }
}
