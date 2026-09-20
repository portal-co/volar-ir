// @reliability: experimental
// @ai: assisted
//! Shared glue for the virtualisation pass — the [`VirtOutput`] wrapper
//! and the dedup-index table used by both the IR and BIR impls.

use alloc::{collections::BTreeMap, vec::Vec};

use volar_ir::{
    boolar::{BIrBlocks, BIrStmt},
    ir::{IRBlocks, IRTypeId},
};
use volar_ir_common::{Constant, Stmt, StorageAccess, StorageId, StorageTable};

use crate::bytecode::{BytecodeEntry, HandlerImmSchema, VirtBytecode};
use crate::canon::{BlockImmediates, HandlerKey};

/// A storage mutation contradicts a read-only sidecar declaration.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StorageAccessViolation {
    pub storage: StorageId,
    pub operation: &'static str,
}

/// Verify that a Volar IR module does not write a sidecar-read-only storage.
pub fn validate_ir_storage_access<P: Clone>(
    blocks: &IRBlocks<P>,
    storage_access: &StorageTable,
) -> Result<(), StorageAccessViolation> {
    for block in &blocks.blocks {
        for node in &block.stmts {
            match &node.kind {
                Stmt::StorageWrite { storage, .. }
                    if storage_access.access_of(*storage) == StorageAccess::ReadOnly =>
                {
                    return Err(StorageAccessViolation {
                        storage: *storage,
                        operation: "StorageWrite",
                    });
                }
                Stmt::ActionStore { targets, .. } => {
                    for target in targets {
                        if storage_access.access_of(target.storage) == StorageAccess::ReadOnly {
                            return Err(StorageAccessViolation {
                                storage: target.storage,
                                operation: "ActionStore",
                            });
                        }
                    }
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// Verify that a Boolar module does not write a sidecar-read-only storage.
pub fn validate_bir_storage_access<P: Clone>(
    blocks: &BIrBlocks<P>,
    storage_access: &StorageTable,
) -> Result<(), StorageAccessViolation> {
    for block in &blocks.blocks {
        for node in &block.stmts {
            match &node.kind {
                BIrStmt::StorageWrite { storage, .. }
                    if storage_access.access_of(*storage) == StorageAccess::ReadOnly =>
                {
                    return Err(StorageAccessViolation {
                        storage: *storage,
                        operation: "StorageWrite",
                    });
                }
                BIrStmt::ActionStoreBit { storage, .. }
                    if storage_access.access_of(*storage) == StorageAccess::ReadOnly =>
                {
                    return Err(StorageAccessViolation {
                        storage: *storage,
                        operation: "ActionStoreBit",
                    });
                }
                _ => {}
            }
        }
    }
    Ok(())
}

/// Output of [`crate::virtualize_ir`] / [`crate::virtualize_bir`].
#[derive(Clone, Debug)]
pub struct VirtOutput<B> {
    /// The rewritten IR (or BIR) module.
    pub blocks: B,
    /// Compatibility sidecar describing storage mutation guarantees. The
    /// legacy IR carrier remains unchanged; consumers opt into these facts.
    pub storage_access: StorageTable,
    /// Structured bytecode table derived from the same data as `pre_init`.
    pub bytecode: Option<VirtBytecode>,
    /// Number of unique handlers after deduplication.
    pub n_handlers: usize,
    /// Number of original blocks in the input module.
    pub blocks_in: usize,
    /// Appended bytecode regions (SharedCore / RerollLoop); zero when adaptive
    /// split is disabled or no regions were selected.
    pub n_appended_regions: usize,
    /// Key parameters prepended to the entry block when a keyed
    /// [`crate::CommitmentConfig`] was supplied.
    ///
    /// `key_params[i] = (constant_value, ir_type_id)` for the i-th key word.
    /// The caller must pass these values as the **first arguments** to the
    /// virtualised module (before the original entry-block arguments).
    /// Empty when no keyed commitment was requested.
    pub key_params: Vec<(Constant, IRTypeId)>,
}

/// Intermediate table built by canonicalising every block.
///
/// `per_block[b]` holds `(handler_idx, immediates)` for the original
/// block at index `b`.  `handler_keys[h]` is the canonical block key
/// whose body is emitted as handler `h`.
#[derive(Clone, Debug)]
pub struct DedupTable<K: HandlerKey> {
    pub per_block: Vec<(u32, BlockImmediates)>,
    pub handler_keys: Vec<K>,
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use super::*;
    use crate::{VirtualizeConfig, virtualize_ir};
    use volar_ir::{
        boolar::{BIrBlock, BIrTarget, BIrTerminator, LaneId},
        ir::{
            IRBlock, IRBlockTargetId, IRBlocks, IRBranchTarget, IRTerminator, IRType, IRTypeId,
            IRTypes, IRVarId,
        },
    };

    #[test]
    fn rejects_ir_storage_write_to_readonly_sidecar() {
        let storage = StorageId(9);
        let blocks = IRBlocks::new(vec![IRBlock {
            params: vec![IRTypeId(0), IRTypeId(0)],
            stmts: vec![volar_ir_common::Node::new(
                Stmt::StorageWrite {
                    storage,
                    src: IRVarId(0),
                    ty: IRTypeId(0),
                    addr: IRVarId(1),
                },
                (),
                None,
            )],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, vec![]),
            },
        }]);
        let mut sidecar = StorageTable::new();
        sidecar.set(storage, StorageAccess::ReadOnly);
        assert_eq!(
            validate_ir_storage_access(&blocks, &sidecar),
            Err(StorageAccessViolation {
                storage,
                operation: "StorageWrite",
            })
        );
    }

    #[test]
    fn caller_storage_access_is_preserved_conservatively() {
        let bit = IRTypeId(0);
        let mut types = IRTypes(vec![IRType::Primitive(volar_ir_common::Type::Bit)]);
        let source: IRBlocks<()> = IRBlocks::new(vec![IRBlock {
            params: vec![bit],
            stmts: vec![],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, vec![IRVarId(0)]),
            },
        }]);
        let mut config = VirtualizeConfig::default();
        config
            .storage_access
            .set(StorageId(77), StorageAccess::ReadOnly);
        config
            .storage_access
            .set(config.bytecode_storage, StorageAccess::ReadWrite);
        let output = virtualize_ir(&source, &mut types, &config);
        assert_eq!(
            output.storage_access.access_of(StorageId(77)),
            StorageAccess::ReadOnly
        );
        assert_eq!(
            output.storage_access.access_of(config.bytecode_storage),
            StorageAccess::ReadWrite,
            "a conflicting caller claim must conservatively weaken the generated fact"
        );
    }

    #[test]
    fn virtualized_register_storage_is_not_readonly() {
        let bit = IRTypeId(0);
        let mut types = IRTypes(vec![IRType::Primitive(volar_ir_common::Type::Bit)]);
        let source: IRBlocks<()> = IRBlocks::new(vec![IRBlock {
            params: vec![bit],
            stmts: vec![],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(IRBlockTargetId::Return, vec![IRVarId(0)]),
            },
        }]);
        let output = virtualize_ir(&source, &mut types, &VirtualizeConfig::default());
        assert!(validate_ir_storage_access(&output.blocks, &output.storage_access).is_ok());
        assert!(
            output
                .storage_access
                .entries
                .iter()
                .any(|entry| entry.access == StorageAccess::ReadWrite)
        );
    }

    #[test]
    fn rejects_boolar_action_store_to_readonly_sidecar() {
        let storage = StorageId(5);
        let blocks = BIrBlocks {
            blocks: vec![BIrBlock {
                params: 2,
                stmts: vec![volar_ir_common::Node::new(
                    BIrStmt::ActionStoreBit {
                        name: "action".into(),
                        guard: IRVarId(0),
                        args: vec![],
                        fallback: IRVarId(1),
                        storage,
                        lane: LaneId(0),
                        addr: vec![],
                        bit: 0,
                        occurrence: 0,
                    },
                    (),
                    None,
                )],
                terminator: BIrTerminator::Jmp(BIrTarget {
                    block: IRBlockTargetId::Return,
                    args: vec![],
                }),
            }],
            pre_init: vec![],
        };
        let mut sidecar = StorageTable::new();
        sidecar.set(storage, StorageAccess::ReadOnly);
        assert_eq!(
            validate_bir_storage_access(&blocks, &sidecar),
            Err(StorageAccessViolation {
                storage,
                operation: "ActionStoreBit",
            })
        );
    }
}

impl<K: HandlerKey> DedupTable<K> {
    /// Build a dedup table from a list of canonicalised per-block results.
    pub fn build(per_block_canon: Vec<(K, BlockImmediates)>) -> Self {
        let mut keys_to_idx: BTreeMap<K, u32> = BTreeMap::new();
        let mut handler_keys: Vec<K> = Vec::new();
        let mut per_block = Vec::with_capacity(per_block_canon.len());

        for (key, imm) in per_block_canon {
            let idx = if let Some(&i) = keys_to_idx.get(&key) {
                i
            } else {
                let i = handler_keys.len() as u32;
                keys_to_idx.insert(key.clone(), i);
                handler_keys.push(key);
                i
            };
            per_block.push((idx, imm));
        }

        Self {
            per_block,
            handler_keys,
        }
    }

    /// Number of unique handlers.
    pub fn n_handlers(&self) -> usize {
        self.handler_keys.len()
    }

    /// Assemble the external bytecode artefact from this table.
    pub fn to_bytecode(&self) -> VirtBytecode {
        let handler_schemas: Vec<HandlerImmSchema> = self
            .handler_keys
            .iter()
            .map(|k| HandlerImmSchema {
                kinds: k.immediate_schema(),
            })
            .collect();
        let entries: Vec<BytecodeEntry> = self
            .per_block
            .iter()
            .map(|(h, imm)| BytecodeEntry::outer(*h, imm.consts.clone(), imm.targets.clone()))
            .collect();
        let outer_block_count = entries.len();
        VirtBytecode {
            n_handlers: self.handler_keys.len(),
            handler_schemas,
            entries,
            outer_block_count,
            regions: Vec::new(),
        }
    }
}
