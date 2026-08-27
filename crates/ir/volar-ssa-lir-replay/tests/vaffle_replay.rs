// @reliability: normal
//! End-to-end tests for `lower_vaffle_module`: hand-built `vaffle::Module`
//! fixtures replayed into `CBackend` (exercising the synthetic
//! `block_addr`/`dyn_jump` fallback and native C `switch`) and `LlvmBackend`
//! (exercising native `blockaddress`/`indirectbr`), compiled and executed
//! for real.

use std::collections::BTreeMap;

use vaffle::{
    Block, BlockId, FuncBody, FuncDecl, FuncId, Module, SigDecl, SigId, Target as VTarget,
    Terminator, Value, ValueId,
};
use volar_ir_common::{Constant, Node, Stmt, Type, TypeTable};
use volar_ssa_lir_replay::lower_vaffle_module;

fn node(kind: Value) -> Node<Value, ()> {
    Node::new(kind, (), None)
}

fn target(block: BlockId, args: Vec<ValueId>) -> VTarget<ValueId> {
    VTarget {
        block,
        args,
        reentry: None,
    }
}

// ============================================================================
// Fixture: sibling calls. `main` calls `helper`, which is defined *after*
// `main` in `module.funcs` (index 1) — a forward reference, exercising each
// backend's own `call()` forward-declaration handling.
// ============================================================================

fn build_sibling_module() -> Module<()> {
    let mut types = TypeTable::new();
    let u8_ty = types.primitive(Type::_8);

    let sigs = vec![
        SigDecl {
            params: vec![u8_ty],
            results: vec![u8_ty],
        }, // 0: main
        SigDecl {
            params: vec![u8_ty],
            results: vec![u8_ty],
        }, // 1: helper
    ];

    // main(x): return helper(x)
    let main_body = FuncBody {
        sig: SigId(0),
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
                func: FuncId(1),
                args: vec![ValueId(0)],
            }),
            node(Value::Output {
                value: ValueId(1),
                idx: 0,
            }),
        ],
    };

    // helper(x): ignore x, return 99 — proves the callee's own body ran.
    let helper_body = FuncBody {
        sig: SigId(1),
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
            node(Value::Op(Stmt::Const(Constant { hi: 0, lo: 99 }, u8_ty))),
        ],
    };

    let mut exports = BTreeMap::new();
    exports.insert("vmain".to_string(), FuncId(0));

    Module {
        types,
        oracles: vec![],
        actions: vec![],
        funcs: vec![FuncDecl::Body(main_body), FuncDecl::Body(helper_body)],
        sigs,
        exports,
        pre_init: vec![],
    }
}

#[test]
fn sibling_call_c_backend() {
    use volar_c_backend::CBackend;
    use volar_lir_test_corpus::compile_and_run;

    let module = build_sibling_module();
    let mut b = CBackend::new();
    lower_vaffle_module(&module, &mut b);
    let c_src = b.finish();

    let out = compile_and_run(&c_src, r#"  printf("%d\n", vmain(5));"#);
    assert_eq!(out.trim(), "99");
}

// ============================================================================
// Fixture: ordinary data-driven `Table` dispatch (not `BlockAddr`-derived) —
// must lower to `LirTarget::switch`, keyed positionally per VAFFLE's own
// `Table` semantics (`targets[idx]`, or `default_target` out of range).
// ============================================================================

fn build_switch_module() -> Module<()> {
    let mut types = TypeTable::new();
    let u8_ty = types.primitive(Type::_8);

    let sigs = vec![SigDecl {
        params: vec![u8_ty],
        results: vec![u8_ty],
    }];

    // `values` is a per-*function* arena (not per-block), so each block's
    // own `Const` gets a distinct `ValueId`: 1 (block1), 2 (block2), 3 (block3).
    let body = FuncBody {
        sig: SigId(0),
        entry: BlockId(0),
        blocks: vec![
            Block {
                params: vec![(ValueId(0), u8_ty)],
                stmts: vec![],
                terminator: Terminator::Table {
                    index: ValueId(0),
                    targets: vec![target(BlockId(1), vec![]), target(BlockId(2), vec![])],
                    default_target: target(BlockId(3), vec![]),
                },
            },
            Block {
                params: vec![],
                stmts: vec![ValueId(1)],
                terminator: Terminator::Return {
                    values: vec![ValueId(1)],
                },
            },
            Block {
                params: vec![],
                stmts: vec![ValueId(2)],
                terminator: Terminator::Return {
                    values: vec![ValueId(2)],
                },
            },
            Block {
                params: vec![],
                stmts: vec![ValueId(3)],
                terminator: Terminator::Return {
                    values: vec![ValueId(3)],
                },
            },
        ],
        values: vec![
            node(Value::Param {
                block: BlockId(0),
                ty: u8_ty,
                idx: 0,
            }),
            node(Value::Op(Stmt::Const(Constant { hi: 0, lo: 10 }, u8_ty))),
            node(Value::Op(Stmt::Const(Constant { hi: 0, lo: 20 }, u8_ty))),
            node(Value::Op(Stmt::Const(Constant { hi: 0, lo: 30 }, u8_ty))),
        ],
    };

    let mut exports = BTreeMap::new();
    exports.insert("classify".to_string(), FuncId(0));

    Module {
        types,
        oracles: vec![],
        actions: vec![],
        funcs: vec![FuncDecl::Body(body)],
        sigs,
        exports,
        pre_init: vec![],
    }
}

#[test]
fn table_switch_c_backend() {
    use volar_c_backend::CBackend;
    use volar_lir_test_corpus::compile_and_run;

    let module = build_switch_module();
    let mut b = CBackend::new();
    lower_vaffle_module(&module, &mut b);
    let c_src = b.finish();

    let out = compile_and_run(
        &c_src,
        r#"  printf("%d %d %d\n", classify(0), classify(1), classify(99));"#,
    );
    assert_eq!(out.trim(), "10 20 30");
}

// ============================================================================
// Fixture: `BlockAddr`-derived `Table` — must lower to `LirTarget::dyn_jump`
// (with `block_addr` for the value), not `switch`. Deterministic: the
// address always resolves to block 2, so control always lands there
// regardless of which backend's dispatch strategy (native indirectbr vs.
// synthetic switch) is used.
// ============================================================================

fn build_dyn_jump_module() -> Module<()> {
    let mut types = TypeTable::new();
    let u8_ty = types.primitive(Type::_8);

    let sigs = vec![SigDecl {
        params: vec![],
        results: vec![u8_ty],
    }];

    let body = FuncBody {
        sig: SigId(0),
        entry: BlockId(0),
        blocks: vec![
            Block {
                params: vec![],
                stmts: vec![ValueId(0)],
                terminator: Terminator::Table {
                    index: ValueId(0),
                    targets: vec![target(BlockId(2), vec![])],
                    default_target: target(BlockId(1), vec![]),
                },
            },
            Block {
                params: vec![],
                stmts: vec![ValueId(1)],
                terminator: Terminator::Return {
                    values: vec![ValueId(1)],
                },
            },
            Block {
                params: vec![],
                stmts: vec![ValueId(2)],
                terminator: Terminator::Return {
                    values: vec![ValueId(2)],
                },
            },
        ],
        values: vec![
            node(Value::BlockAddr { block: BlockId(2) }),
            node(Value::Op(Stmt::Const(Constant { hi: 0, lo: 10 }, u8_ty))),
            node(Value::Op(Stmt::Const(Constant { hi: 0, lo: 20 }, u8_ty))),
        ],
    };

    let mut exports = BTreeMap::new();
    exports.insert("dispatch".to_string(), FuncId(0));

    Module {
        types,
        oracles: vec![],
        actions: vec![],
        funcs: vec![FuncDecl::Body(body)],
        sigs,
        exports,
        pre_init: vec![],
    }
}

#[test]
fn dyn_jump_c_backend_synthetic_dispatch() {
    use volar_c_backend::CBackend;
    use volar_lir_test_corpus::compile_and_run;

    let module = build_dyn_jump_module();
    let mut b = CBackend::new();
    lower_vaffle_module(&module, &mut b);
    let c_src = b.finish();

    let out = compile_and_run(&c_src, r#"  printf("%d\n", dispatch());"#);
    assert_eq!(out.trim(), "20");
}

#[test]
fn dyn_jump_llvm_backend_native_indirectbr() {
    use inkwell::OptimizationLevel;
    use inkwell::context::Context;
    use inkwell::targets::{
        CodeModel, FileType, InitializationConfig, RelocMode, Target as LlvmTarget, TargetMachine,
    };
    use std::{fs, process::Command};
    use tempfile::TempDir;
    use volar_llvm_backend::LlvmBackend;

    fn init_target() {
        static ONCE: std::sync::Once = std::sync::Once::new();
        ONCE.call_once(|| {
            LlvmTarget::initialize_native(&InitializationConfig::default())
                .expect("init native target");
        });
    }

    let module = build_dyn_jump_module();
    let ctx = Context::create();
    let mut b = LlvmBackend::new(&ctx, "dyn_jump_mod");
    lower_vaffle_module(&module, &mut b);

    init_target();
    let llvm_module = b.finish();
    llvm_module.verify().expect("LLVM module verify failed");
    let ir_text = llvm_module.print_to_string().to_string();
    assert!(
        ir_text.contains("blockaddress("),
        "expected native blockaddress constant:\n{ir_text}"
    );
    assert!(
        ir_text.contains("indirectbr "),
        "expected native indirectbr terminator:\n{ir_text}"
    );

    let dir = TempDir::new().expect("tempdir");
    let obj_path = dir.path().join("test.o");
    let c_path = dir.path().join("main.c");
    let exe_path = dir.path().join("test");

    let triple = TargetMachine::get_default_triple();
    let machine = LlvmTarget::from_triple(&triple)
        .expect("target from triple")
        .create_target_machine(
            &triple,
            "generic",
            "",
            OptimizationLevel::None,
            RelocMode::Default,
            CodeModel::Default,
        )
        .expect("create target machine");
    machine
        .write_to_file(&llvm_module, FileType::Object, &obj_path)
        .expect("write object file");

    let c_src = "#include <stdio.h>\n#include <stdint.h>\nuint8_t dispatch(void);\nint main(void) { printf(\"%d\\n\", dispatch()); return 0; }\n";
    fs::write(&c_path, c_src).expect("write C main");

    let status = Command::new("cc")
        .arg("-o")
        .arg(&exe_path)
        .arg(&obj_path)
        .arg(&c_path)
        .status()
        .expect("cc not found");
    assert!(status.success(), "linking failed");

    let output = Command::new(&exe_path)
        .output()
        .expect("failed to run compiled program");
    let stdout = String::from_utf8(output.stdout).expect("non-UTF8 output");
    assert_eq!(stdout.trim(), "20");
}
