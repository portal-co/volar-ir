// @reliability: experimental
// @ai: assisted
//! Integration tests for the storage-registry entry points:
//! `virtualize_ir_with_registry`, `virtualize_ir_committed_with_registry`
//! is deferred (needs a hash algorithm fixture), and
//! `virtualize_bir_with_registry`.
//!
//! Two invariants are checked, per the packed-storages plan:
//!
//! 1. **Uniqueness + purpose tagging** — every storage the virtualised
//!    module consumes is allocated from the registry, recorded under a
//!    `StoragePurpose::Virt` purpose, and disjoint from storages other
//!    consumers already claimed.
//! 2. **Reported == used** — `VirtOutput::consumed_storages` is exactly
//!    the set of `StorageId`s the output module reads, writes, or
//!    pre-initialises (a structural invariant, legitimately shape-checked).
//!
//! Plus semantics preservation: registry-mode output evaluates to the
//! same results as the original module and the legacy-mode output.

use std::collections::BTreeSet;

use volar_fuzz::interpreter::biir::eval_biir;
use volar_fuzz::interpreter::ir::eval_ir;
use volar_ir::boolar::{BIrBlock, BIrBlocks, BIrTarget, BIrTerminator};
use volar_ir::ir::{
    IRBlock, IRBlockId, IRBlockTargetId, IRBlocks, IRBranchTarget, IRTerminator, IRTypes,
};
use volar_ir_common::{
    StorageId, StoragePurpose, StorageRegistry, Stmt, VirtStorageRole,
};
use volar_ir_virt::{
    AdaptiveSplitConfig, DispatchMode, VirtualizeConfig, virtualize_bir,
    virtualize_bir_with_registry, virtualize_ir, virtualize_ir_with_registry,
};

/// `(a, b) ↦ a`, as a two-block module exercising the per-type register
/// file (a `Bit` param threaded across a block boundary).
fn two_block_ir() -> (IRBlocks<()>, IRTypes) {
    let mut types = IRTypes::new();
    let bit = types.bit();
    let blocks = IRBlocks::new(vec![
        IRBlock {
            params: vec![bit, bit],
            stmts: vec![],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(
                    IRBlockTargetId::Block(IRBlockId(1)),
                    vec![volar_ir::ir::IRVarId(0)],
                ),
            },
        },
        IRBlock {
            params: vec![bit],
            stmts: vec![],
            terminator: IRTerminator::Jmp {
                target: IRBranchTarget::new(
                    IRBlockTargetId::Return,
                    vec![volar_ir::ir::IRVarId(0)],
                ),
            },
        },
    ]);
    (blocks, types)
}

/// Same shape at the BIR level (params are bit counts).
fn two_block_bir() -> BIrBlocks<()> {
    BIrBlocks {
        blocks: vec![
            BIrBlock {
                params: 2,
                stmts: vec![],
                terminator: BIrTerminator::Jmp(BIrTarget {
                    block: IRBlockTargetId::Block(IRBlockId(1)),
                    args: vec![volar_ir::ir::IRVarId(0), volar_ir::ir::IRVarId(1)],
                }),
            },
            BIrBlock {
                // `virtualize_bir` requires a uniform param count across
                // blocks; the second param is simply unused here.
                params: 2,
                stmts: vec![],
                terminator: BIrTerminator::Jmp(BIrTarget {
                    block: IRBlockTargetId::Return,
                    args: vec![volar_ir::ir::IRVarId(0)],
                }),
            },
        ],
        pre_init: vec![],
    }
}

fn cfg_public() -> VirtualizeConfig {
    VirtualizeConfig {
        dispatch: DispatchMode::Public,
        ..VirtualizeConfig::default()
    }
}

fn virt_purpose(role: VirtStorageRole, detail: u32) -> StoragePurpose {
    StoragePurpose::Virt { role, detail }
}

/// Collect every `StorageId` an IR module touches (statement reads/writes
/// plus pre-init segments).
fn ir_used_storages(blocks: &IRBlocks<()>) -> BTreeSet<StorageId> {
    let mut out = BTreeSet::new();
    for b in &blocks.blocks {
        for s in &b.stmts {
            match &s.kind {
                Stmt::StorageRead { storage, .. } | Stmt::StorageWrite { storage, .. } => {
                    out.insert(*storage);
                }
                _ => {}
            }
        }
    }
    for seg in &blocks.pre_init {
        out.insert(seg.storage);
    }
    out
}

/// Collect every `StorageId` a BIR module touches.
fn bir_used_storages(blocks: &BIrBlocks<()>) -> BTreeSet<StorageId> {
    let mut out = BTreeSet::new();
    for b in &blocks.blocks {
        for s in &b.stmts {
            match &s.kind {
                volar_ir::boolar::BIrStmt::StorageRead { storage, .. }
                | volar_ir::boolar::BIrStmt::StorageWrite { storage, .. } => {
                    out.insert(*storage);
                }
                _ => {}
            }
        }
    }
    for seg in &blocks.pre_init {
        out.insert(seg.storage);
    }
    out
}

/// Assert the registry-mode invariants on a `VirtOutput`.
fn assert_consumed_invariants(
    consumed: &[(StorageId, VirtStorageRole)],
    used: BTreeSet<StorageId>,
    registry: &StorageRegistry<StoragePurpose>,
    foreign_claim: StorageId,
) {
    // Non-empty, unique.
    assert!(!consumed.is_empty(), "consumed_storages must be populated");
    let ids: BTreeSet<StorageId> = consumed.iter().map(|(id, _)| *id).collect();
    assert_eq!(
        ids.len(),
        consumed.len(),
        "consumed_storages must not repeat an id"
    );
    // Disjoint from the foreign claim.
    assert!(
        !ids.contains(&foreign_claim),
        "registry allocation must skip ids another consumer claimed"
    );
    // Every consumed id is registered with a Virt purpose.
    for (id, role) in consumed {
        match registry.purpose_of(*id) {
            Some(StoragePurpose::Virt { role: r, .. }) => assert_eq!(r, role),
            other => panic!("consumed id {id:?} has non-Virt or missing purpose: {other:?}"),
        }
    }
    // Reported == used (the input modules here touch no storage).
    let reported: BTreeSet<StorageId> = ids;
    assert_eq!(
        reported, used,
        "consumed_storages must be exactly the storages the output uses"
    );
}

#[test]
fn ir_registry_mode_allocates_unique_tagged_storages() {
    let (blocks, mut types) = two_block_ir();
    let mut registry = StorageRegistry::<StoragePurpose>::new();
    // Simulate another consumer already owning the legacy bytecode id.
    registry
        .claim(
            StorageId::VIRT_BYTECODE,
            StoragePurpose::Other("foreign".to_string()),
        )
        .unwrap();

    let virt = virtualize_ir_with_registry(
        &blocks,
        &mut types,
        &cfg_public(),
        &mut registry,
        virt_purpose,
    );

    assert_consumed_invariants(
        &virt.consumed_storages,
        ir_used_storages(&virt.blocks),
        &registry,
        StorageId::VIRT_BYTECODE,
    );
}

#[test]
fn ir_legacy_mode_reports_consumed_too() {
    let (blocks, mut types) = two_block_ir();
    let virt = virtualize_ir::<()>(&blocks, &mut types, &cfg_public());
    let reported: BTreeSet<StorageId> = virt
        .consumed_storages
        .iter()
        .map(|(id, _)| *id)
        .collect();
    assert_eq!(
        reported.len(),
        virt.consumed_storages.len(),
        "legacy consumed_storages must not repeat an id"
    );
    assert_eq!(reported, ir_used_storages(&virt.blocks));
}

#[test]
fn ir_registry_mode_preserves_semantics() {
    let (blocks, types) = two_block_ir();

    let mut types_legacy = types.clone();
    let legacy = virtualize_ir::<()>(&blocks, &mut types_legacy, &cfg_public());

    let mut registry = StorageRegistry::<StoragePurpose>::new();
    registry
        .claim(
            StorageId::VIRT_BYTECODE,
            StoragePurpose::Other("foreign".to_string()),
        )
        .unwrap();
    let mut types_registry = types.clone();
    let registered = virtualize_ir_with_registry(
        &blocks,
        &mut types_registry,
        &cfg_public(),
        &mut registry,
        virt_purpose,
    );

    for a in [false, true] {
        for b in [false, true] {
            let inputs = vec![vec![a], vec![b]];
            let want = eval_ir(&blocks, &types, &inputs).expect("original evaluates");
            let got_legacy =
                eval_ir(&legacy.blocks, &types_legacy, &inputs).expect("legacy evaluates");
            let got_registry = eval_ir(&registered.blocks, &types_registry, &inputs)
                .expect("registry-mode evaluates");
            assert_eq!(want, got_legacy, "legacy virt changed semantics ({a}, {b})");
            assert_eq!(
                want, got_registry,
                "registry-mode virt changed semantics ({a}, {b})"
            );
        }
    }
}

#[test]
fn ir_registry_mode_adaptive_split_matches_semantics() {
    let (blocks, types) = two_block_ir();
    let cfg = VirtualizeConfig {
        dispatch: DispatchMode::Public,
        adaptive_split: AdaptiveSplitConfig {
            enabled: true,
            ..AdaptiveSplitConfig::default()
        },
        ..VirtualizeConfig::default()
    };

    let mut registry = StorageRegistry::<StoragePurpose>::new();
    let mut types_registry = types.clone();
    let registered = virtualize_ir_with_registry(
        &blocks,
        &mut types_registry,
        &cfg,
        &mut registry,
        virt_purpose,
    );

    // Whether or not the heuristics appended regions for this tiny module,
    // the consumed set must be unique + Virt-tagged and semantics preserved.
    let ids: BTreeSet<StorageId> = registered
        .consumed_storages
        .iter()
        .map(|(id, _)| *id)
        .collect();
    assert_eq!(ids.len(), registered.consumed_storages.len());
    for (id, role) in &registered.consumed_storages {
        match registry.purpose_of(*id) {
            Some(StoragePurpose::Virt { role: r, .. }) => assert_eq!(r, role),
            other => panic!("consumed id {id:?} has non-Virt or missing purpose: {other:?}"),
        }
    }

    let inputs = vec![vec![true], vec![false]];
    let want = eval_ir(&blocks, &types, &inputs).expect("original evaluates");
    let got = eval_ir(&registered.blocks, &types_registry, &inputs).expect("virt evaluates");
    assert_eq!(want, got);
}

#[test]
fn bir_registry_mode_allocates_unique_tagged_storages() {
    let blocks = two_block_bir();
    let mut registry = StorageRegistry::<StoragePurpose>::new();
    registry
        .claim(
            StorageId::VIRT_BYTECODE,
            StoragePurpose::Other("foreign".to_string()),
        )
        .unwrap();

    let virt = virtualize_bir_with_registry(&blocks, &cfg_public(), &mut registry, virt_purpose);

    assert_consumed_invariants(
        &virt.consumed_storages,
        bir_used_storages(&virt.blocks),
        &registry,
        StorageId::VIRT_BYTECODE,
    );
}

#[test]
fn bir_legacy_mode_reports_consumed_too() {
    let blocks = two_block_bir();
    let virt = virtualize_bir::<()>(&blocks, &cfg_public());
    let reported: BTreeSet<StorageId> = virt
        .consumed_storages
        .iter()
        .map(|(id, _)| *id)
        .collect();
    assert_eq!(reported.len(), virt.consumed_storages.len());
    assert_eq!(reported, bir_used_storages(&virt.blocks));
}

#[test]
fn bir_registry_mode_preserves_semantics() {
    let blocks = two_block_bir();

    let legacy = virtualize_bir::<()>(&blocks, &cfg_public());

    let mut registry = StorageRegistry::<StoragePurpose>::new();
    registry
        .claim(
            StorageId::VIRT_BYTECODE,
            StoragePurpose::Other("foreign".to_string()),
        )
        .unwrap();
    let registered = virtualize_bir_with_registry(&blocks, &cfg_public(), &mut registry, virt_purpose);

    for a in [false, true] {
        for b in [false, true] {
            let inputs = vec![a, b];
            let want = eval_biir(&blocks, &inputs).expect("original evaluates");
            let got_legacy = eval_biir(&legacy.blocks, &inputs).expect("legacy evaluates");
            let got_registry =
                eval_biir(&registered.blocks, &inputs).expect("registry-mode evaluates");
            assert_eq!(want, got_legacy);
            assert_eq!(want, got_registry);
        }
    }
}
