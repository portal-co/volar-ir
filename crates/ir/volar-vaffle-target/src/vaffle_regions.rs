// @reliability: experimental
// @ai: assisted
//! VAFFLE-level typed region tables: validation and lowering onto the
//! Volar-IR level (see `docs/typed-gadgets-and-region-threading-plan.md`,
//! Workstream B).
//!
//! A VAFFLE [`TypedRegionTable`] anchors on **function-scoped** boundaries:
//! `FuncInput { func, param }` / `FuncOutput { func, result }` (bit ranges
//! within the signature type's layout, per [`SigDecl`]) and program-scoped
//! `Storage { storage, ty }` (validated against module traffic and typed
//! `pre_init`). [`translate_vaffle_regions`] lowers the function-scoped
//! anchors onto `lower_vaffle_to_ir`'s entry block, where the standard
//! typed-table pipeline (`volar_ir_passes::region_lowering`) takes over.
//!
//! Fail-closed rules:
//! - anchors on `FuncDecl::Import`ed functions are rejected (no body to
//!   wrap; wrap the *caller* boundary instead);
//! - anchors on functions that no longer exist error (`UnknownCarrier`);
//! - block-param (`BlockInput`) and entry-level (`Input`/`Output`) anchors
//!   are rejected at this level — author them against the lowered
//!   `IRBlocks`/`VCircuit` instead.

use alloc::vec::Vec;

use vaffle::{FuncDecl, FuncId, Module};
use volar_ir::ir::IRTypeId;
use volar_ir::typed_gadget::{
    typed_bit_width, TypedAnchor, TypedRegionEntry, TypedRegionError, TypedRegionTable,
};
use volar_ir_common::{PreInitSegment, StorageId};

/// Errors from VAFFLE-level region-table handling.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum VaffleRegionError {
    /// Structural validation of the typed table failed.
    Region(TypedRegionError),
    /// The anchor's function does not exist (or was inlined away).
    UnknownFunction { anchor: TypedAnchor, func: u32 },
    /// The anchor's param/result position does not exist in the signature.
    UnknownCarrier { anchor: TypedAnchor },
    /// The anchor kind is not expressible at the VAFFLE level.
    UnsupportedAnchor { anchor: TypedAnchor },
    /// The function is imported — there is no body to wrap.
    ImportedFunction { anchor: TypedAnchor, func: u32 },
    /// The anchor's type has no Boolean width (e.g. `Z3`).
    UnsupportedWidth { anchor: TypedAnchor, ty: IRTypeId },
    /// A storage anchor's `(StorageId, TypeId)` space has no traffic in the
    /// module (statements or typed `pre_init`).
    UnknownStorageSpace { storage: StorageId, ty: IRTypeId },
}

impl From<TypedRegionError> for VaffleRegionError {
    fn from(e: TypedRegionError) -> Self {
        VaffleRegionError::Region(e)
    }
}

/// Validate a VAFFLE-level typed region table against `module`.
pub fn validate_vaffle_regions(
    table: &TypedRegionTable,
    module: &Module,
) -> Result<(), VaffleRegionError> {
    table.validate_structure()?;
    for entry in &table.entries {
        match &entry.anchor {
            TypedAnchor::FuncInput { func, param, start, len } => {
                let width = func_param_width(table, module, *func, *param, entry)?;
                check_range(entry, *start, *len, width)?;
            }
            TypedAnchor::FuncOutput { func, result, start, len } => {
                let width = func_result_width(module, *func, *result)
                    .ok_or_else(|| carrier(entry))?;
                check_range(entry, *start, *len, width)?;
            }
            TypedAnchor::Storage { storage, ty, .. } => {
                if !module_uses_storage(module, *storage, *ty) {
                    return Err(VaffleRegionError::UnknownStorageSpace {
                        storage: *storage,
                        ty: *ty,
                    });
                }
            }
            TypedAnchor::Input { .. }
            | TypedAnchor::BlockInput { .. }
            | TypedAnchor::Output { .. } => {
                return Err(VaffleRegionError::UnsupportedAnchor {
                    anchor: entry.anchor.clone(),
                });
            }
        }
    }
    Ok(())
}

fn carrier(entry: &TypedRegionEntry) -> VaffleRegionError {
    VaffleRegionError::UnknownCarrier {
        anchor: entry.anchor.clone(),
    }
}

fn check_range(
    entry: &TypedRegionEntry,
    start: u16,
    len: u16,
    width: usize,
) -> Result<(), VaffleRegionError> {
    if (start as usize).saturating_add(len as usize) > width {
        return Err(VaffleRegionError::Region(
            TypedRegionError::RangeOutOfRange {
                anchor: entry.anchor.clone(),
                start,
                len,
                width,
            },
        ));
    }
    Ok(())
}

fn func_param_width(
    _table: &TypedRegionTable,
    module: &Module,
    func: u32,
    param: u32,
    entry: &TypedRegionEntry,
) -> Result<usize, VaffleRegionError> {
    let Some(sig_types) = func_param_types(module, func) else {
        return Err(match module.funcs.get(func as usize) {
            Some(FuncDecl::Import { .. }) => VaffleRegionError::ImportedFunction {
                anchor: entry.anchor.clone(),
                func,
            },
            _ => VaffleRegionError::UnknownFunction {
                anchor: entry.anchor.clone(),
                func,
            },
        });
    };
    let Some(&ty) = sig_types.get(param as usize) else {
        return Err(carrier(entry));
    };
    typed_bit_width(ty, &module.types).ok_or(VaffleRegionError::UnsupportedWidth {
        anchor: entry.anchor.clone(),
        ty,
    })
}

/// The param types of `func`'s body signature, or `None` if the function is
/// imported or does not exist.
fn func_param_types(module: &Module, func: u32) -> Option<Vec<IRTypeId>> {
    let body = match module.funcs.get(func as usize) {
        Some(FuncDecl::Body(body)) => body,
        _ => return None,
    };
    let sig = module.sigs.get(body.sig.0)?;
    Some(sig.params.clone())
}

fn func_result_width(module: &Module, func: u32, result: u32) -> Option<usize> {
    let body = match module.funcs.get(func as usize) {
        Some(FuncDecl::Body(body)) => body,
        _ => return None,
    };
    let sig = module.sigs.get(body.sig.0)?;
    let ty = sig.results.get(result as usize)?;
    typed_bit_width(*ty, &module.types)
}

fn module_uses_storage(module: &Module, storage: StorageId, ty: IRTypeId) -> bool {
    let uses = |stmt: &volar_ir_common::Stmt<vaffle::ValueId>| {
        matches!(
            stmt,
            volar_ir_common::Stmt::StorageRead {
                storage: s,
                ty: t,
                ..
            }
            | volar_ir_common::Stmt::StorageWrite {
                storage: s,
                ty: t,
                ..
            } if *s == storage && *t == ty
        )
    };
    for f in &module.funcs {
        let FuncDecl::Body(body) = f else {
            continue;
        };
        if body
            .values
            .iter()
            .any(|value| matches!(&value.kind, vaffle::Value::Op(stmt) if uses(stmt)))
        {
            return true;
        }
    }
    module
        .pre_init
        .iter()
        .any(|seg: &PreInitSegment| seg.storage == storage && seg.ty == ty)
}

/// Lower a VAFFLE-level typed region table onto the Volar-IR level: the
/// entry function's `FuncInput` anchors become `Input` anchors on the
/// lowered entry block's params (same positional order —
/// `lower_vaffle_to_ir` preserves signature order), `FuncOutput` anchors
/// are rejected (the lowered module has no module-level outputs; author
/// output regions against the unrolled circuit), and `Storage` anchors pass
/// through unchanged.
///
/// `entry_func` selects which function's boundary becomes the lowered
/// module's entry boundary (`lower_vaffle_to_ir` lowers one export entry;
/// callers must use the same choice).
pub fn translate_vaffle_regions(
    table: &TypedRegionTable,
    module: &Module,
    entry_func: FuncId,
) -> Result<TypedRegionTable, VaffleRegionError> {
    table.validate_structure()?;
    if entry_func.0 >= module.funcs.len() {
        return Err(VaffleRegionError::UnknownFunction {
            anchor: TypedAnchor::FuncInput { func: entry_func.0 as u32, param: 0, start: 0, len: 0 },
            func: entry_func.0 as u32,
        });
    }
    let mut entries = Vec::with_capacity(table.entries.len());
    for entry in &table.entries {
        match &entry.anchor {
            TypedAnchor::FuncInput {
                func,
                param,
                start,
                len,
            } => {
                if *func != entry_func.0 as u32 {
                    // Non-entry function boundaries dissolve during lowering
                    // (calls become jumps); wrapping them at the IR level is
                    // the caller's job via the IR-level table.
                    return Err(VaffleRegionError::UnknownFunction {
                        anchor: entry.anchor.clone(),
                        func: *func,
                    });
                }
                // Param position is preserved by the lowering (signature
                // order). The bit range carries over verbatim.
                entries.push(TypedRegionEntry {
                    anchor: TypedAnchor::Input {
                        param: *param,
                        start: *start,
                        len: *len,
                    },
                    regions: entry.regions.clone(),
                });
            }
            TypedAnchor::FuncOutput { .. } => {
                return Err(VaffleRegionError::UnsupportedAnchor {
                    anchor: entry.anchor.clone(),
                });
            }
            TypedAnchor::Storage { .. } => entries.push(entry.clone()),
            anchor @ (TypedAnchor::Input { .. }
            | TypedAnchor::BlockInput { .. }
            | TypedAnchor::Output { .. }) => {
                return Err(VaffleRegionError::UnsupportedAnchor {
                    anchor: anchor.clone(),
                });
            }
        }
    }
    Ok(TypedRegionTable {
        entries,
        names: table.names.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;
    use alloc::vec;
    use vaffle::{Block, BlockId, FuncBody, SigDecl, SigId, Value, ValueId};
    use volar_ir::ir::IRType;
    use volar_ir_common::{Type, TypeTable};
    type BTreeSet<T> = alloc::collections::BTreeSet<T>;

    fn module_with_entry() -> (Module, TypeTable) {
        let mut types = TypeTable::new();
        let _bit = types.intern(IRType::Primitive(Type::Bit));
        let w8 = types.intern(IRType::Primitive(Type::_8));
        // Entry function: params [w8, w8], one block, terminator Return.
        let sig = SigDecl {
            params: vec![w8, w8],
            results: vec![w8],
        };
        let body = FuncBody {
            sig: SigId(0),
            blocks: vec![Block {
                params: vec![
                    (ValueId(0), w8),
                    (ValueId(1), w8),
                ],
                stmts: vec![],
                terminator: vaffle::Terminator::Return {
                    values: vec![ValueId(0)],
                },
            }],
            values: vec![],
            entry: BlockId(0),
        };
        let module = Module {
            pointer_width: vaffle::PointerWidth::Bits64,
            types: types.clone(),
            oracles: vec![],
            actions: vec![],
            funcs: vec![vaffle::FuncDecl::Body(body)],
            sigs: vec![sig],
            exports: BTreeMap::new(),
            pre_init: vec![],
        };
        let _ = w8;
        (module, types)
    }

    #[test]
    fn vaffle_validate_and_translate() {
        let (module, _types) = module_with_entry();
        let table = TypedRegionTable {
            entries: vec![
                TypedRegionEntry {
                    anchor: TypedAnchor::FuncInput { func: 0, param: 0, start: 1, len: 3 },
                    regions: BTreeSet::from([volar_ir::region::RegionId(0)]),
                },
                TypedRegionEntry {
                    anchor: TypedAnchor::FuncOutput { func: 0, result: 0, start: 0, len: 8 },
                    regions: BTreeSet::from([volar_ir::region::RegionId(1)]),
                },
            ],
            names: BTreeMap::new(),
        };
        assert_eq!(validate_vaffle_regions(&table, &module), Ok(()));

        // Translation: FuncInput(0) on the entry function → Input{param 0};
        // FuncOutput is rejected at the IR level (no module outputs — author
        // output regions against the unrolled circuit).
        let inputs_only = TypedRegionTable {
            entries: vec![table.entries[0].clone()],
            names: BTreeMap::new(),
        };
        let lowered = translate_vaffle_regions(&inputs_only, &module, FuncId(0)).expect("translates");
        assert!(matches!(
            &lowered.entries[0].anchor,
            TypedAnchor::Input { param: 0, start: 1, len: 3 }
        ));
        assert_eq!(lowered.entries.len(), 1);
        assert!(lowered.validate_structure().is_ok());
        assert!(matches!(
            translate_vaffle_regions(&table, &module, FuncId(0)),
            Err(VaffleRegionError::UnsupportedAnchor { .. })
        ));
    }

    #[test]
    fn vaffle_fail_closed_paths() {
        let (module, _types) = module_with_entry();

        // Non-entry function anchor: unknown function 1.
        let table = TypedRegionTable {
            entries: vec![TypedRegionEntry {
                anchor: TypedAnchor::FuncInput { func: 1, param: 0, start: 0, len: 1 },
                regions: BTreeSet::from([volar_ir::region::RegionId(0)]),
            }],
            names: BTreeMap::new(),
        };
        assert!(matches!(
            validate_vaffle_regions(&table, &module),
            Err(VaffleRegionError::UnknownFunction { func: 1, .. })
        ));
        assert!(matches!(
            translate_vaffle_regions(&table, &module, FuncId(0)),
            Err(VaffleRegionError::UnknownFunction { func: 1, .. })
        ));

        // Out-of-range param bits (w8 = 8 bits).
        let table = TypedRegionTable {
            entries: vec![TypedRegionEntry {
                anchor: TypedAnchor::FuncInput { func: 0, param: 0, start: 6, len: 4 },
                regions: BTreeSet::from([volar_ir::region::RegionId(0)]),
            }],
            names: BTreeMap::new(),
        };
        assert!(matches!(
            validate_vaffle_regions(&table, &module),
            Err(VaffleRegionError::Region(TypedRegionError::RangeOutOfRange { width: 8, .. }))
        ));

        // Unsupported anchor level.
        let table = TypedRegionTable {
            entries: vec![TypedRegionEntry {
                anchor: TypedAnchor::Input { param: 0, start: 0, len: 1 },
                regions: BTreeSet::from([volar_ir::region::RegionId(0)]),
            }],
            names: BTreeMap::new(),
        };
        assert!(matches!(
            validate_vaffle_regions(&table, &module),
            Err(VaffleRegionError::UnsupportedAnchor { .. })
        ));

        // Unknown storage space.
        let table = TypedRegionTable {
            entries: vec![TypedRegionEntry {
                anchor: TypedAnchor::Storage {
                    storage: StorageId(9),
                    ty: IRTypeId(1),
                    addr_start: 0,
                    addr_len: 1,
                },
                regions: BTreeSet::from([volar_ir::region::RegionId(0)]),
            }],
            names: BTreeMap::new(),
        };
        assert!(matches!(
            validate_vaffle_regions(&table, &module),
            Err(VaffleRegionError::UnknownStorageSpace { .. })
        ));
    }
}
