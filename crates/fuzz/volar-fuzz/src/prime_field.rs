//! Semantics of prime-field polynomials and native-field lowering.

use volar_ir::ir::{IRBlock, IRBlockTargetId, IRBlocks, IRBranchTarget, IRTerminator, IRVarId};
use volar_ir_common::{Constant, IrType, Node, PolyCoeffs, Stmt, TypeId, TypeTable};
use volar_ir_passes::{NativeLowering, lower_ir_to_boolar, lower_to_native, raise_bits_to_z3};

use crate::interpreter::biir::eval_biir;
use crate::interpreter::ir::{IrValue, bit_flatten, eval_ir};

fn bits(value: u128, width: usize) -> IrValue {
    (0..width).map(|bit| (value >> bit) & 1 == 1).collect()
}

fn as_int(bits: &[bool]) -> u128 {
    bits.iter()
        .enumerate()
        .fold(0u128, |acc, (i, bit)| {
            if *bit { acc | (1u128 << i) } else { acc }
        })
}

fn ret(params: Vec<TypeId>, stmts: Vec<Stmt<IRVarId>>, result: u32) -> IRBlocks<()> {
    IRBlocks::new(vec![IRBlock {
        params,
        stmts: stmts
            .into_iter()
            .map(|stmt| Node::new(stmt, (), None))
            .collect(),
        terminator: IRTerminator::Jmp {
            target: IRBranchTarget::new(IRBlockTargetId::Return, vec![IRVarId(result)]),
        },
    }])
}

fn poly(ty: TypeId, terms: Vec<(Vec<u32>, u8)>, constant: u128) -> Stmt<IRVarId> {
    let mut coeffs = PolyCoeffs::new();
    for (vars, coeff) in terms {
        let mut key: Vec<IRVarId> = vars.into_iter().map(IRVarId).collect();
        key.sort();
        coeffs.insert(key, coeff);
    }
    Stmt::Poly {
        ty,
        coeffs,
        constant: Constant {
            hi: 0,
            lo: constant,
        },
    }
}

fn assert_ir_matches_boolar(blocks: &IRBlocks<()>, types: &TypeTable, inputs: &[IrValue]) {
    let ir_out = eval_ir(blocks, types, inputs).expect("ir evaluates");
    let boolar = lower_ir_to_boolar(blocks, types);
    let flat = eval_biir(&boolar, &bit_flatten(inputs)).expect("boolar evaluates");
    let mut offset = 0;
    for value in &ir_out {
        let got = &flat[offset..offset + value.len()];
        assert_eq!(got, value.as_slice(), "boolar drifted from the interpreter");
        offset += value.len();
    }
    assert_eq!(offset, flat.len());
}

#[test]
fn gf3_product_and_square_match_boolar() {
    let mut types = TypeTable::new();
    let z3 = types.z3();
    let blocks = ret(
        vec![z3, z3],
        vec![poly(z3, vec![(vec![0, 1], 1), (vec![0, 0], 1)], 0)],
        2,
    );
    for a in 0..3 {
        for b in 0..3 {
            assert_ir_matches_boolar(&blocks, &types, &[bits(a, 2), bits(b, 2)]);
            let out = eval_ir(&blocks, &types, &[bits(a, 2), bits(b, 2)]).unwrap();
            let expect = (a * b + a * a) % 3;
            assert_eq!(as_int(&out[0]), expect, "a={a} b={b}");
        }
    }
}

#[test]
fn vec_of_z3_multiplies_per_lane() {
    let mut types = TypeTable::new();
    let z3 = types.z3();
    let lanes = types.intern(IrType::Vec(2, z3));
    let blocks = ret(vec![lanes, lanes], vec![poly(lanes, vec![(vec![0, 1], 1)], 0)], 2);
    // Lane 0: 2 * 2 = 1. Lane 1: 1 * 2 = 2.
    let left = bits((2 << 0) | (1 << 2), 4);
    let right = bits((2 << 0) | (2 << 2), 4);
    assert_ir_matches_boolar(&blocks, &types, &[left.clone(), right.clone()]);
    let out = eval_ir(&blocks, &types, &[left, right]).unwrap();
    assert_eq!(as_int(&out[0]), (1 << 0) | (2 << 2));
}

#[test]
fn aes8_product_matches_boolar() {
    let mut types = TypeTable::new();
    let field = types.aes8();
    let blocks = ret(
        vec![field, field],
        vec![poly(field, vec![(vec![0, 1], 1)], 0)],
        2,
    );
    for a in [0u128, 1, 0x13, 0x57, 0xfe] {
        for b in [0u128, 1, 0x13, 0x57, 0xfe] {
            assert_ir_matches_boolar(&blocks, &types, &[bits(a, 8), bits(b, 8)]);
        }
    }
}

#[test]
fn raise_bits_agrees_with_the_bit_polynomial_on_boolean_inputs() {
    let mut types = TypeTable::new();
    let bit = types.bit();
    let source = ret(
        vec![bit, bit],
        vec![poly(bit, vec![(vec![0], 1), (vec![1], 1), (vec![0, 1], 1)], 1)],
        2,
    );
    let raised = raise_bits_to_z3(&source, &mut types);
    for a in 0..2 {
        for b in 0..2 {
            let bit_out = eval_ir(&source, &types, &[bits(a, 1), bits(b, 1)]).unwrap();
            let z3_out = eval_ir(&raised, &types, &[bits(a, 1), bits(b, 1)]).unwrap();
            assert_eq!(as_int(&bit_out[0]), as_int(&z3_out[0]));
            assert!(as_int(&z3_out[0]) < 2);
        }
    }
}

#[test]
fn native_z3_product_of_two_is_one() {
    let mut types = TypeTable::new();
    let z3 = types.z3();
    let blocks = ret(
        vec![],
        vec![
            Stmt::Const(Constant { hi: 0, lo: 2 }, z3),
            Stmt::Const(Constant { hi: 0, lo: 2 }, z3),
            poly(z3, vec![(vec![0, 1], 1)], 0),
        ],
        2,
    );
    let NativeLowering::Ir(lowered, lowered_types) = lower_to_native(&blocks, &types, z3) else {
        panic!("z3 lowering should stay in Volar IR");
    };
    let out = eval_ir(&lowered, &lowered_types, &[]).expect("lowered ir evaluates");
    assert_eq!(as_int(&out[0]), 1);
}

#[test]
fn bit_and_xor_embed_into_native_z3() {
    let mut types = TypeTable::new();
    let bit = types.bit();
    let z3 = types.z3();
    let and = ret(vec![bit, bit], vec![poly(bit, vec![(vec![0, 1], 1)], 0)], 2);
    let xor = ret(
        vec![bit, bit],
        vec![poly(bit, vec![(vec![0], 1), (vec![1], 1)], 0)],
        2,
    );
    let NativeLowering::Ir(and_ir, and_types) = lower_to_native(&and, &types, z3) else {
        panic!("expected IR");
    };
    let NativeLowering::Ir(xor_ir, xor_types) = lower_to_native(&xor, &types, z3) else {
        panic!("expected IR");
    };
    for a in 0..2u128 {
        for b in 0..2u128 {
            let inputs = [bits(a, 2), bits(b, 2)];
            let and_out = eval_ir(&and_ir, &and_types, &inputs).unwrap();
            let xor_out = eval_ir(&xor_ir, &xor_types, &inputs).unwrap();
            assert_eq!(as_int(&and_out[0]), a * b);
            let embedded = (a + b + 3 - (2 * a * b) % 3) % 3;
            assert_eq!(as_int(&xor_out[0]), embedded, "a={a} b={b}");
        }
    }
}
