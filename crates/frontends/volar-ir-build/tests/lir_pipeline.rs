// @reliability: experimental
//! First test coverage for `Pipeline<LirStage>`: structural round-trip
//! (mirroring `volar-lir-saved/tests/round_trip.rs`'s idiom), a real
//! numeric-correctness check via `CBackend`, and (under the `vaffle`
//! feature) a multi-translation-unit chunked-compile-and-link check via
//! `Pipeline::<VaffleStage>::lower_to_lir_chunks`.

use volar_ir::ir::{
    IRBlock, IRBlockTargetId, IRBlocks, IRBranchTarget, IRStmt, IRTerminator, IRType, IRTypeId,
    IRTypes, IRVarId,
};
use volar_ir_build::Pipeline;
use volar_ir_common::{Constant, Node, Type};

/// `fn answer() -> u8 { return 42; }`
fn build_answer_ir() -> (IRBlocks, IRTypes) {
    let types = IRTypes(vec![IRType::Primitive(Type::_8)]);
    let blocks = IRBlocks::new(vec![IRBlock {
        params: vec![],
        stmts: vec![Node::new(
            IRStmt::Const(Constant { hi: 0, lo: 42 }, IRTypeId(0)),
            (),
            None,
        )],
        terminator: IRTerminator::Jmp {
            target: IRBranchTarget::new(IRBlockTargetId::Return, vec![IRVarId(0)]),
        },
    }]);
    (blocks, types)
}

#[test]
fn lower_to_lir_named_round_trip() {
    let (blocks, types) = build_answer_ir();
    let original = Pipeline::from_volar_ir_blocks(blocks, types)
        .lower_to_lir_named("answer")
        .expect("lower_to_lir_named")
        .to_lir();

    let bytes = rkyv::to_bytes::<rkyv::rancor::Error>(&original).expect("rkyv::to_bytes failed");
    let deserialized: volar_lir_saved::SavedLirModule = unsafe {
        rkyv::from_bytes_unchecked::<volar_lir_saved::SavedLirModule, rkyv::rancor::Error>(
            bytes.as_slice(),
        )
        .expect("rkyv::from_bytes_unchecked failed")
    };

    let mut replay_rec = volar_lir_saved::RecordingTarget::new();
    deserialized.replay(&mut replay_rec);
    let replayed = replay_rec.finish();

    assert_eq!(
        original, replayed,
        "round-tripped LIR module does not match original"
    );
}

#[test]
fn lower_to_lir_named_uses_the_given_name() {
    let (blocks, types) = build_answer_ir();
    let saved = Pipeline::from_volar_ir_blocks(blocks, types)
        .lower_to_lir_named("answer")
        .expect("lower_to_lir_named")
        .to_lir();

    let mut b = volar_c_backend::CBackend::new();
    saved.replay(&mut b);
    let c_src = b.finish();
    assert!(
        c_src.contains("answer("),
        "expected a function named `answer` in generated C:\n{c_src}"
    );
}

#[test]
fn lower_to_lir_named_numeric_correctness() {
    use volar_lir_test_corpus::compile_and_run;

    let (blocks, types) = build_answer_ir();
    let saved = Pipeline::from_volar_ir_blocks(blocks, types)
        .lower_to_lir_named("answer")
        .expect("lower_to_lir_named")
        .to_lir();

    let mut b = volar_c_backend::CBackend::new();
    saved.replay(&mut b);
    let c_src = b.finish();

    let out = compile_and_run(&c_src, r#"  printf("%d\n", answer());"#);
    assert_eq!(out.trim(), "42");
}

#[test]
fn default_name_is_volar_module() {
    let (blocks, types) = build_answer_ir();
    let saved: volar_lir_saved::SavedLirModule = Pipeline::from_volar_ir_blocks(blocks, types)
        .lower_to_lir()
        .expect("lower_to_lir")
        .to_lir();

    let mut b = volar_c_backend::CBackend::new();
    saved.replay(&mut b);
    let c_src = b.finish();
    assert!(
        c_src.contains("volar_module("),
        "default lower_to_lir() should still name the function `volar_module`:\n{c_src}"
    );
}

#[cfg(feature = "vaffle")]
mod vaffle_chunked {
    use std::collections::BTreeMap;

    use vaffle::{Block, BlockId, FuncBody, FuncDecl, FuncId, Module, SigDecl, SigId, Terminator, Value, ValueId};
    use volar_ir_build::Pipeline;
    use volar_ir_common::{Constant, Node, Stmt, Type, TypeTable};

    fn node(kind: Value) -> Node<Value, ()> {
        Node::new(kind, (), None)
    }

    /// `f0(x) -> f1(x) -> f2(x)`, where `f2` ignores `x` and returns 7.
    fn build_chain_module() -> Module<()> {
        let mut types = TypeTable::new();
        let u8_ty = types.primitive(Type::_8);

        const N: usize = 3;
        let sigs = vec![
            SigDecl {
                params: vec![u8_ty],
                results: vec![u8_ty],
            };
            N
        ];

        let mut funcs = Vec::with_capacity(N);
        for i in 0..N - 1 {
            funcs.push(FuncDecl::Body(FuncBody {
                sig: SigId(i),
                entry: BlockId(0),
                blocks: vec![Block {
                    params: vec![(ValueId(0), u8_ty)],
                    stmts: vec![ValueId(1), ValueId(2)],
                    terminator: Terminator::Return {
                        values: vec![ValueId(2)],
                    },
                }],
                values: vec![
                    node(Value::Param {
                        block: BlockId(0),
                        ty: u8_ty,
                        idx: 0,
                    }),
                    node(Value::Call {
                        func: FuncId(i + 1),
                        args: vec![ValueId(0)],
                    }),
                    node(Value::Output {
                        value: ValueId(1),
                        idx: 0,
                    }),
                ],
            }));
        }
        funcs.push(FuncDecl::Body(FuncBody {
            sig: SigId(N - 1),
            entry: BlockId(0),
            blocks: vec![Block {
                params: vec![(ValueId(0), u8_ty)],
                stmts: vec![ValueId(1)],
                terminator: Terminator::Return {
                    values: vec![ValueId(1)],
                },
            }],
            values: vec![
                node(Value::Param {
                    block: BlockId(0),
                    ty: u8_ty,
                    idx: 0,
                }),
                node(Value::Op(Stmt::Const(Constant { hi: 0, lo: 7 }, u8_ty))),
            ],
        }));

        let mut exports = BTreeMap::new();
        exports.insert("vmain".to_string(), FuncId(0));

        Module {
            pointer_width: vaffle::PointerWidth::Bits64,
            types,
            oracles: vec![],
            actions: vec![],
            funcs,
            sigs,
            exports,
            pre_init: vec![],
        }
    }

    #[test]
    fn lower_to_lir_chunks_via_pipeline() {
        use volar_lir_test_corpus::compile_and_run_multi;

        let module = build_chain_module();
        let chunks = Pipeline::from_vaffle_module(module).lower_to_lir_chunks(2);
        assert_eq!(chunks.len(), 2, "expected exactly 2 non-empty chunks");

        let c_srcs: Vec<String> = chunks
            .iter()
            .map(|chunk| {
                let mut b = volar_c_backend::CBackend::new();
                chunk.replay(&mut b);
                b.finish()
            })
            .collect();
        let c_src_refs: Vec<&str> = c_srcs.iter().map(String::as_str).collect();

        let out = compile_and_run_multi(
            &c_src_refs,
            r#"  uint8_t vmain(uint8_t);
  printf("%d\n", vmain(3));"#,
        );
        assert_eq!(out.trim(), "7");
    }
}
