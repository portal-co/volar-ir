// @reliability: normal
//! End-to-end tests for `lower_vaffle_module_to_lir_chunks`: a multi-function
//! VAFFLE module with a real call chain, split into several
//! independently-compiled translation units (C files / LLVM modules +
//! object files), linked back together, and run — proving that a call
//! crossing a chunk boundary needs no special handling beyond what
//! `LirTarget::call`'s existing forward-declaration behavior already
//! provides.
//!
//! Known limitation (see `lower_vaffle_module_to_lir_chunks`'s doc comment):
//! this does not exercise `LirType::Struct` crossing a chunk boundary —
//! VAFFLE never produces `LirType::Struct` values, only (sometimes)
//! `LirType::Arr`, so this is out of scope here.

use std::collections::BTreeMap;

use vaffle::{Block, BlockId, FuncBody, FuncDecl, FuncId, Module, SigDecl, SigId, Terminator, Value, ValueId};
use volar_ir_common::{Constant, Node, Stmt, Type, TypeTable};
use volar_ssa_lir_replay::lower_vaffle_module_to_lir_chunks;

fn node(kind: Value) -> Node<Value, ()> {
    Node::new(kind, (), None)
}

// ============================================================================
// Fixture: a 6-function call chain, f0 -> f1 -> f2 -> f3 -> f4 -> f5, where
// f0..f4 forward their argument to the next function and f5 ignores its
// argument and returns a constant. With near-uniform per-function weight,
// `assign_chunks`'s greedy bin-packing spreads consecutive functions across
// different chunks, so every hop in the chain is (with high likelihood, and
// deterministically given `assign_chunks`'s stable ordering) a cross-chunk
// call — exactly the case that needs each backend's `call` forward-
// declaration to behave like a real `extern` reference.
// ============================================================================

fn build_chain_module() -> Module<()> {
    let mut types = TypeTable::new();
    let u8_ty = types.primitive(Type::_8);

    const N: usize = 6;
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
    // Last function in the chain: ignore the argument, return 77.
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
            node(Value::Op(Stmt::Const(Constant { hi: 0, lo: 77 }, u8_ty))),
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
fn chunked_chain_c_backend() {
    use volar_c_backend::CBackend;
    use volar_lir_test_corpus::compile_and_run_multi;

    let module = build_chain_module();
    let chunks = lower_vaffle_module_to_lir_chunks(&module, 3);
    assert_eq!(chunks.len(), 3, "expected exactly 3 non-empty chunks");

    let c_srcs: Vec<String> = chunks
        .iter()
        .map(|chunk| {
            let mut b = CBackend::new();
            chunk.replay(&mut b);
            b.finish()
        })
        .collect();
    let c_src_refs: Vec<&str> = c_srcs.iter().map(String::as_str).collect();

    let out = compile_and_run_multi(
        &c_src_refs,
        r#"  uint8_t vmain(uint8_t);
  printf("%d\n", vmain(5));"#,
    );
    assert_eq!(out.trim(), "77");
}

#[test]
fn chunked_chain_llvm_backend() {
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

    let module = build_chain_module();
    let chunks = lower_vaffle_module_to_lir_chunks(&module, 3);
    assert_eq!(chunks.len(), 3, "expected exactly 3 non-empty chunks");

    init_target();
    let dir = TempDir::new().expect("tempdir");

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

    let mut obj_paths = Vec::new();
    for (i, chunk) in chunks.iter().enumerate() {
        // Each chunk gets its own, independent `Context` — cross-chunk
        // calls rely only on ordinary object-level external-symbol
        // resolution at link time, not on any shared LLVM type identity.
        let ctx = Context::create();
        let mut b = LlvmBackend::new(&ctx, &format!("chunk{i}"));
        chunk.replay(&mut b);
        let llvm_module = b.finish();
        llvm_module.verify().expect("LLVM module verify failed");

        let obj_path = dir.path().join(format!("chunk{i}.o"));
        machine
            .write_to_file(&llvm_module, FileType::Object, &obj_path)
            .expect("write object file");
        obj_paths.push(obj_path);
    }

    let c_path = dir.path().join("main.c");
    let exe_path = dir.path().join("test");
    let c_src = "#include <stdio.h>\n#include <stdint.h>\nuint8_t vmain(uint8_t);\nint main(void) { printf(\"%d\\n\", vmain(5)); return 0; }\n";
    fs::write(&c_path, c_src).expect("write C main");

    let status = Command::new("cc")
        .arg("-o")
        .arg(&exe_path)
        .args(&obj_paths)
        .arg(&c_path)
        .status()
        .expect("cc not found");
    assert!(status.success(), "linking failed");

    let output = Command::new(&exe_path)
        .output()
        .expect("failed to run compiled program");
    let stdout = String::from_utf8(output.stdout).expect("non-UTF8 output");
    assert_eq!(stdout.trim(), "77");
}
