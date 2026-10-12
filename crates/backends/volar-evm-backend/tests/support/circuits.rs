//! Small Boolar circuit fixtures, mirroring retrop's `builtins`.

use volar_ir::boolar::{BIrBlock, BIrBlocks, BIrStmt, BIrTarget, BIrTerminator, LaneId};
use volar_ir::circuit::BCircuit;
use volar_ir::ir::{IRBlockTargetId, IRVarId};
use volar_ir_common::StorageId;

/// Wrap a `BCircuit` back into single-block `BIrBlocks` for `volar_fuzz`'s
/// BIIR evaluator.
pub fn to_blocks(circuit: &BCircuit<()>) -> BIrBlocks<()> {
    BIrBlocks {
        blocks: vec![BIrBlock {
            params: circuit.params,
            stmts: circuit.stmts.clone(),
            terminator: BIrTerminator::Jmp(BIrTarget {
                block: IRBlockTargetId::Return,
                args: circuit.outputs.clone(),
            }),
        }],
        pre_init: circuit.pre_init.clone(),
    }
}

/// `sum = a ^ b ^ cin`, `cout = (a & b) | (cin & (a ^ b))`.
pub fn full_adder() -> BCircuit<()> {
    let mut c = BCircuit::new(3);
    let (a, b, cin) = (IRVarId(0), IRVarId(1), IRVarId(2));
    c.push_stmt(BIrStmt::Xor(a, b), ()); // v3
    c.push_stmt(BIrStmt::Xor(IRVarId(3), cin), ()); // v4 = sum
    c.push_stmt(BIrStmt::And(a, b), ()); // v5
    c.push_stmt(BIrStmt::And(cin, IRVarId(3)), ()); // v6
    c.push_stmt(BIrStmt::Or(IRVarId(5), IRVarId(6)), ()); // v7 = cout
    c.outputs = vec![IRVarId(4), IRVarId(7)];
    c
}

/// `((!a & b) | c) & 1`.
pub fn and_or_tree() -> BCircuit<()> {
    let mut c = BCircuit::new(3);
    let (a, b, cin) = (IRVarId(0), IRVarId(1), IRVarId(2));
    c.push_stmt(BIrStmt::Not(a), ()); // v3
    c.push_stmt(BIrStmt::And(IRVarId(3), b), ()); // v4
    c.push_stmt(BIrStmt::Or(IRVarId(4), cin), ()); // v5
    c.push_stmt(BIrStmt::One, ()); // v6
    c.push_stmt(BIrStmt::And(IRVarId(5), IRVarId(6)), ()); // v7
    c.outputs = vec![IRVarId(7)];
    c
}

/// XOR-reduce of `n` params into one output.
pub fn xor_reduce(n: u32) -> BCircuit<()> {
    assert!(n >= 2, "need at least two inputs to reduce");
    let mut c = BCircuit::new(n);
    let mut acc = IRVarId(0);
    for i in 1..n {
        let dst = IRVarId(n + i - 1);
        c.push_stmt(BIrStmt::Xor(acc, IRVarId(i)), ());
        acc = dst;
    }
    c.outputs = vec![acc];
    c
}

/// Constant gates: `0`, `1`, `!a`, `1 ^ a`.
pub fn const_gates() -> BCircuit<()> {
    let mut c = BCircuit::new(1);
    c.push_stmt(BIrStmt::Zero, ()); // v1
    c.push_stmt(BIrStmt::One, ()); // v2
    c.push_stmt(BIrStmt::Not(IRVarId(0)), ()); // v3
    c.push_stmt(BIrStmt::Xor(IRVarId(2), IRVarId(0)), ()); // v4
    c.outputs = vec![IRVarId(1), IRVarId(2), IRVarId(3), IRVarId(4)];
    c
}

/// A storage-bearing circuit, for rejection tests.
pub fn storage_read() -> BCircuit<()> {
    let mut c = BCircuit::new(1);
    c.push_stmt(
        BIrStmt::StorageRead {
            storage: StorageId(0),
            lane: LaneId(0),
            addr: vec![IRVarId(0)],
        },
        (),
    );
    c.outputs = vec![IRVarId(1)];
    c
}
