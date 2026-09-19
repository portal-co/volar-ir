//! Manual large-fixture virtualization regression harness.
//!
//! The fixtures are generated and owned by the sibling `retrop` repository;
//! this test deliberately neither vendors them nor guesses a checkout path.
//! Invoke it explicitly with `VOLAR_IR_RETROP_FIXTURE_DIR` pointing to
//! `retrop-emit-volar/fixtures`.

use std::collections::BTreeSet;
use std::path::Path;

use volar_fuzz::interpreter::biir::eval_biir;
use volar_ir::boolar::{BIrBlocks, BIrStmt};
use volar_ir::ir::StorageId;
use volar_ir_common::StorageAccess;
use volar_ir_virt::{DispatchMode, VirtualizeConfig, virtualize_bir};

fn fixture_dir() -> std::path::PathBuf {
    let path = std::env::var_os("VOLAR_IR_RETROP_FIXTURE_DIR")
        .map(std::path::PathBuf::from)
        .expect("set VOLAR_IR_RETROP_FIXTURE_DIR to an absolute retrop-emit-volar/fixtures path");
    assert!(
        path.is_absolute(),
        "VOLAR_IR_RETROP_FIXTURE_DIR must be absolute"
    );
    path
}

fn load(path: &Path) -> BIrBlocks<()> {
    let bytes = std::fs::read(path).expect("read retrop Boolar fixture");
    let mut aligned = rkyv::util::AlignedVec::<16>::with_capacity(bytes.len());
    aligned.extend_from_slice(&bytes);
    rkyv::from_bytes::<BIrBlocks<()>, rkyv::rancor::Error>(&aligned)
        .expect("deserialize retrop Boolar fixture")
}

fn storage_ids(blocks: &BIrBlocks<()>) -> BTreeSet<StorageId> {
    let mut ids = BTreeSet::new();
    for segment in &blocks.pre_init {
        ids.insert(segment.storage);
    }
    for block in &blocks.blocks {
        for stmt in &block.stmts {
            match &stmt.kind {
                BIrStmt::StorageRead { storage, .. }
                | BIrStmt::StorageWrite { storage, .. }
                | BIrStmt::ActionStoreBit { storage, .. } => {
                    ids.insert(*storage);
                }
                _ => {}
            }
        }
    }
    ids
}

fn written_storage_ids(blocks: &BIrBlocks<()>) -> BTreeSet<StorageId> {
    blocks
        .blocks
        .iter()
        .flat_map(|block| block.stmts.iter())
        .filter_map(|stmt| match &stmt.kind {
            BIrStmt::StorageWrite { storage, .. } | BIrStmt::ActionStoreBit { storage, .. } => {
                Some(*storage)
            }
            _ => None,
        })
        .collect()
}

fn check_fixture(name: &str) {
    let path = fixture_dir().join(name);
    let bytes = std::fs::metadata(&path)
        .expect("stat retrop Boolar fixture")
        .len();
    let circuit = load(&path);
    let stmts: usize = circuit.blocks.iter().map(|block| block.stmts.len()).sum();
    let storages = storage_ids(&circuit);

    assert_eq!(
        circuit.blocks.len(),
        1,
        "retrop fixtures are one-step circuits"
    );
    let params = circuit.blocks[0].params as usize;
    let config = VirtualizeConfig {
        dispatch: DispatchMode::Public,
        ..VirtualizeConfig::default()
    };
    let virtualized = virtualize_bir(&circuit, &config);

    // A one-block fixture exercises deserialization and transformation scale,
    // but cannot show a handler-deduplication win.
    assert_eq!(virtualized.blocks_in, 1);
    assert_eq!(virtualized.n_handlers, 1);
    let readonly: BTreeSet<_> = virtualized
        .storage_access
        .entries
        .iter()
        .filter(|entry| entry.access == StorageAccess::ReadOnly)
        .map(|entry| entry.storage)
        .collect();
    assert!(
        !readonly.is_empty(),
        "virt must declare its bytecode read-only"
    );
    assert!(readonly.is_disjoint(&written_storage_ids(&virtualized.blocks)));

    let inputs = vec![false; params];
    let expected = eval_biir(&circuit, &inputs).expect("original fixture evaluates");
    let actual = eval_biir(&virtualized.blocks, &inputs)
        .expect("public-dispatch virtualized fixture evaluates");
    assert_eq!(actual, expected, "virtualization changed {name} semantics");

    eprintln!(
        "{name}: {bytes} bytes, {} params, {stmts} statements, {} source storages; \
         virtualized to {} blocks / {} handlers",
        params,
        storages.len(),
        virtualized.blocks.blocks.len(),
        virtualized.n_handlers,
    );
}

#[test]
#[ignore = "large retrop fixture; set VOLAR_IR_RETROP_FIXTURE_DIR and invoke explicitly"]
fn m6502_step_virtualizes_without_native_codegen() {
    check_fixture("m6502_step.biir");
}

#[test]
#[ignore = "large retrop fixture; set VOLAR_IR_RETROP_FIXTURE_DIR and invoke explicitly"]
fn z80_step_virtualizes_without_native_codegen() {
    check_fixture("z80_step.biir");
}
