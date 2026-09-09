//! Opt-in vc-spec WAFFLE→VAFFLE lowering: tagged args, mem writes/reveals,
//! VCI reveal/wait.

use portal_pc_waffle_ir::entity::EntityVec;
use portal_pc_waffle_ir::{
    FuncDecl, Memory, MemoryArg, MemoryData, MemorySegment, Module as WModule, Operator, Signature,
    SignatureData, Terminator as WTerminator, Type as WType,
};
use vaffle::{FuncDecl as VaffleFuncDecl, Value};
use volar_ir::typed_gadget::{TypedAnchor, TypedRegionEntry};
use volar_ir_common::Stmt;
use volar_side::SideHandler;
use volar_vaffle_target::vaffle_regions::validate_vaffle_regions;
use volar_vaffle_target::{
    VcArg, VcConfig, VcMemReveal, VcMemWrite, VcProtection, VaffleTarget, WaffleImportConfig,
    lower_waffle_module, lower_waffle_module_with_vc,
};

fn empty_module() -> WModule<'static> {
    WModule {
        orig_bytes: None,
        funcs: EntityVec::default(),
        signatures: EntityVec::default(),
        globals: EntityVec::default(),
        tables: EntityVec::default(),
        imports: vec![],
        exports: vec![],
        memories: EntityVec::default(),
        control_tags: EntityVec::default(),
        start_func: None,
        debug: Default::default(),
        debug_map: Default::default(),
        custom_sections: Default::default(),
    }
}

fn push_func_sig(
    module: &mut WModule,
    params: Vec<WType>,
    returns: Vec<WType>,
) -> Signature {
    module.signatures.push(SignatureData::Func {
        params,
        returns,
        shared: false,
    })
}

/// `(func (param i32 i32) (result i32) (i32.mul p0 p1))`
fn build_multiply_module() -> WModule<'static> {
    let mut module = empty_module();
    let sig = push_func_sig(&mut module, vec![WType::I32, WType::I32], vec![WType::I32]);
    let mut body = portal_pc_waffle_ir::FunctionBody::new(&module, sig);
    let entry = body.entry;
    let p0 = body.blocks[entry].params[0].1;
    let p1 = body.blocks[entry].params[1].1;
    let prod = body.add_op(entry, Operator::I32Mul, &[p0, p1], &[WType::I32]);
    body.set_terminator(
        entry,
        WTerminator::Return {
            values: vec![prod],
        },
    );
    module.funcs.push(FuncDecl::Body(sig, "multiply".into(), body));
    module
}

fn entry_param_sides(target: &VaffleTarget, name: &str) -> Vec<Option<volar_side::SideId>> {
    let fid = target.module.exports[name];
    let VaffleFuncDecl::Body(body) = &target.module.funcs[fid.0] else {
        panic!("expected body");
    };
    let entry = &body.blocks[body.entry.0];
    entry
        .params
        .iter()
        .map(|(vid, _)| body.values[vid.0].side)
        .collect()
}

fn region_named(table: &volar_ir::typed_gadget::TypedRegionTable, name: &str) -> volar_ir::region::RegionId {
    table
        .names
        .iter()
        .find_map(|(id, n)| (n.as_str() == name).then_some(*id))
        .unwrap_or_else(|| panic!("missing region {name}"))
}

#[test]
fn default_lowering_leaves_params_untagged() {
    let wasm = build_multiply_module();
    let mut target = VaffleTarget::new();
    let errors = lower_waffle_module(&wasm, &mut target, &WaffleImportConfig::default());
    assert!(errors.is_empty(), "{errors:?}");
    let sides = entry_param_sides(&target, "multiply");
    assert!(sides.iter().all(|s| s.is_none()), "{sides:?}");
}

#[test]
fn multiply_private_blind_tags_params_and_output_regions() {
    let wasm = build_multiply_module();
    let vc = VcConfig::new().with_call("multiply", vec![VcArg::Private, VcArg::Blind]);
    let mut target = VaffleTarget::new();
    let (errors, artifact) =
        lower_waffle_module_with_vc(&wasm, &mut target, &WaffleImportConfig::default(), &vc);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(
        validate_vaffle_regions(&artifact.regions, &target.module),
        Ok(())
    );

    let local = artifact.handler.local;
    let remote = artifact.handler.remote;
    let sides = entry_param_sides(&target, "multiply");
    assert!(sides.len() >= 64, "two i32 params flatten to 64 bits, got {}", sides.len());
    assert!(
        sides[..32].iter().all(|s| *s == Some(local)),
        "param 0 should be local/private: {:?}",
        &sides[..32]
    );
    assert!(
        sides[32..64].iter().all(|s| *s == Some(remote)),
        "param 1 should be remote/blind: {:?}",
        &sides[32..64]
    );

    let private = region_named(&artifact.regions, "private");
    let blind = region_named(&artifact.regions, "blind");
    let public = region_named(&artifact.regions, "public");
    assert!(artifact.regions.entries.iter().any(|e| matches!(
        e.anchor,
        TypedAnchor::FuncInput { param: 0, .. }
    ) && e.regions.contains(&private)));
    assert!(artifact.regions.entries.iter().any(|e| matches!(
        e.anchor,
        TypedAnchor::FuncInput { param: 32, .. }
    ) && e.regions.contains(&blind)));
    assert!(artifact.regions.entries.iter().any(|e| matches!(
        e.anchor,
        TypedAnchor::FuncOutput { result: 0, .. }
    ) && e.regions.contains(&public)));

    assert_eq!(
        artifact.handler.protection(Some(local)),
        VcProtection::Private
    );
    assert_eq!(
        artifact.handler.protection(Some(remote)),
        VcProtection::Blind
    );
    assert_eq!(
        artifact.handler.protection(Some(artifact.handler.public)),
        VcProtection::Public
    );
}

#[test]
fn stmt_sides_survive_into_ir_blocks() {
    // A-stage: the vaffle→IRBlocks lowering (lower_vaffle_to_ir_owned) must
    // carry each derived op's side from the vaffle arena into the lowered
    // IRBlocks stmt, computing the operand-side join at the boundary. Use
    // private × private so both operands share the `local` side and the
    // multiply's join is `local` (a private × blind multiply would correctly
    // join to None — a mixed wire is unattributable to a single side).
    let wasm = build_multiply_module();
    let vc = VcConfig::new().with_call("multiply", vec![VcArg::Private, VcArg::Private]);
    let mut target = VaffleTarget::new();
    let (errors, artifact) =
        lower_waffle_module_with_vc(&wasm, &mut target, &WaffleImportConfig::default(), &vc);
    assert!(errors.is_empty(), "{errors:?}");
    let local = artifact.handler.local;

    let (blocks, _types) = volar_vaffle_target::lower_vaffle_to_ir_owned(target.module);

    // The multiply is a derived op over the two local-sided params; its
    // lowered IR stmts must carry the joined `local` side (not the legacy
    // all-None), proving side propagation is live across the boundary.
    let any_local = blocks
        .blocks
        .iter()
        .any(|b| b.stmts.iter().any(|n| n.side == Some(local)));
    assert!(
        any_local,
        "expected local-tagged derived stmts in lowered IRBlocks; \
         side propagation was dropped at the vaffle→IRBlocks boundary"
    );
}

#[test]
fn default_lowering_drops_no_explicit_sides_into_ir_blocks() {
    // Without vc tagging, nothing stamps sides, so the lowered IRBlocks are
    // all None (the side channel is inert but present).
    let wasm = build_multiply_module();
    let mut target = VaffleTarget::new();
    let errors = lower_waffle_module(&wasm, &mut target, &WaffleImportConfig::default());
    assert!(errors.is_empty(), "{errors:?}");
    let (blocks, _types) = volar_vaffle_target::lower_vaffle_to_ir_owned(target.module);
    let any_tagged = blocks.blocks.iter().any(|b| b.stmts.iter().any(|n| n.side.is_some()));
    assert!(!any_tagged, "untagged module must lower to all-None sides");
}

#[test]
fn constants_are_public_under_vc() {
    let mut module = empty_module();
    let sig = push_func_sig(&mut module, vec![], vec![WType::I32]);
    let mut body = portal_pc_waffle_ir::FunctionBody::new(&module, sig);
    let c = body.add_op(
        body.entry,
        Operator::I32Const { value: 7 },
        &[],
        &[WType::I32],
    );
    body.set_terminator(
        body.entry,
        WTerminator::Return { values: vec![c] },
    );
    module.funcs.push(FuncDecl::Body(sig, "k".into(), body));

    let mut target = VaffleTarget::new();
    let (errors, artifact) =
        lower_waffle_module_with_vc(&module, &mut target, &WaffleImportConfig::default(), &VcConfig::new());
    assert!(errors.is_empty(), "{errors:?}");
    let fid = target.module.exports["k"];
    let VaffleFuncDecl::Body(body) = &target.module.funcs[fid.0] else {
        panic!("body");
    };
    let public = artifact.handler.public;
    let const_sides: Vec<_> = body
        .values
        .iter()
        .filter(|n| matches!(n.kind, Value::Op(Stmt::Const(..))))
        .map(|n| n.side)
        .collect();
    assert!(
        const_sides.iter().any(|s| *s == Some(public)),
        "expected public-sided Const nodes, got {const_sides:?}"
    );
}

fn build_mem_module(with_load: bool, data: Option<(usize, Vec<u8>)>) -> WModule<'static> {
    let mut module = empty_module();
    module.memories.push(MemoryData {
        initial_pages: 1,
        maximum_pages: None,
        segments: match data {
            Some((offset, bytes)) => vec![MemorySegment {
                offset,
                data: bytes,
            }],
            None => vec![],
        },
        memory64: false,
        shared: false,
        page_size_log2: None,
    });
    let sig = push_func_sig(&mut module, vec![WType::I32], vec![WType::I32]);
    let mut body = portal_pc_waffle_ir::FunctionBody::new(&module, sig);
    let entry = body.entry;
    let addr = body.blocks[entry].params[0].1;
    let ret = if with_load {
        body.add_op(
            entry,
            Operator::I32Load {
                memory: MemoryArg {
                    align: 2,
                    offset: 0,
                    memory: Memory::from(0u32),
                },
            },
            &[addr],
            &[WType::I32],
        )
    } else {
        body.add_op(entry, Operator::I32Const { value: 0 }, &[], &[WType::I32])
    };
    body.set_terminator(
        entry,
        WTerminator::Return {
            values: vec![ret],
        },
    );
    module.funcs.push(FuncDecl::Body(sig, "f".into(), body));
    module
}

fn storage_entries(artifact: &volar_vaffle_target::VcArtifact) -> Vec<&TypedRegionEntry> {
    artifact
        .regions
        .entries
        .iter()
        .filter(|e| matches!(e.anchor, TypedAnchor::Storage { .. }))
        .collect()
}

#[test]
fn public_mem_write_bakes_pre_init_and_public_region() {
    let wasm = build_mem_module(true, None);
    let vc = VcConfig::new().with_mem_write(VcMemWrite::public(0, 8, vec![1, 2, 3, 4]));
    let mut target = VaffleTarget::new();
    let (errors, artifact) =
        lower_waffle_module_with_vc(&wasm, &mut target, &WaffleImportConfig::default(), &vc);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(
        validate_vaffle_regions(&artifact.regions, &target.module),
        Ok(())
    );
    assert!(
        target.module.pre_init.iter().any(|s| s.offset == 8 && s.data.len() == 4),
        "public mem_write should appear in pre_init: {:?}",
        target.module.pre_init
    );
    let public = region_named(&artifact.regions, "public");
    assert!(storage_entries(&artifact).iter().any(|e| {
        matches!(
            e.anchor,
            TypedAnchor::Storage {
                addr_start: 8,
                addr_len: 4,
                ..
            }
        ) && e.regions.contains(&public)
    }));
}

#[test]
fn private_and_blind_mem_writes_are_regions_not_pre_init() {
    let wasm = build_mem_module(true, None);
    let vc = VcConfig::new()
        .with_mem_write(VcMemWrite::private(0, 16, 4))
        .with_mem_write(VcMemWrite::blind(0, 32, 4));
    let mut target = VaffleTarget::new();
    let (errors, artifact) =
        lower_waffle_module_with_vc(&wasm, &mut target, &WaffleImportConfig::default(), &vc);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(
        validate_vaffle_regions(&artifact.regions, &target.module),
        Ok(())
    );
    assert!(
        target
            .module
            .pre_init
            .iter()
            .all(|s| s.offset != 16 && s.offset != 32),
        "private/blind must not bake pre_init: {:?}",
        target.module.pre_init
    );
    let private = region_named(&artifact.regions, "private");
    let blind = region_named(&artifact.regions, "blind");
    assert!(storage_entries(&artifact).iter().any(|e| {
        matches!(
            e.anchor,
            TypedAnchor::Storage {
                addr_start: 16,
                addr_len: 4,
                ..
            }
        ) && e.regions.contains(&private)
    }));
    assert!(storage_entries(&artifact).iter().any(|e| {
        matches!(
            e.anchor,
            TypedAnchor::Storage {
                addr_start: 32,
                addr_len: 4,
                ..
            }
        ) && e.regions.contains(&blind)
    }));
}

#[test]
fn data_segment_and_mem_reveal_cover_public_storage() {
    let wasm = build_mem_module(true, Some((0, vec![9, 8, 7, 6])));
    let vc = VcConfig::new()
        .with_mem_write(VcMemWrite::private(0, 0, 4))
        .with_mem_reveal(VcMemReveal {
            memory: 0,
            offset: 0,
            len: 4,
        });
    let mut target = VaffleTarget::new();
    let (errors, artifact) =
        lower_waffle_module_with_vc(&wasm, &mut target, &WaffleImportConfig::default(), &vc);
    assert!(errors.is_empty(), "{errors:?}");
    assert_eq!(
        validate_vaffle_regions(&artifact.regions, &target.module),
        Ok(())
    );
    assert!(
        target.module.pre_init.iter().any(|s| s.offset == 0 && s.data.len() == 4),
        "data segment must remain in pre_init"
    );
    let public = region_named(&artifact.regions, "public");
    let private = region_named(&artifact.regions, "private");
    let entries = storage_entries(&artifact);
    let cover = entries
        .iter()
        .find(|e| {
            matches!(
                e.anchor,
                TypedAnchor::Storage {
                    addr_start: 0,
                    addr_len: 4,
                    ..
                }
            )
        })
        .expect("merged [0,4) storage cover");
    assert!(cover.regions.contains(&public), "{cover:?}");
    assert!(cover.regions.contains(&private), "{cover:?}");
}

fn build_reveal_module() -> WModule<'static> {
    let mut module = empty_module();
    let reveal_sig = push_func_sig(&mut module, vec![WType::I32], vec![WType::I32]);
    let wait_sig = push_func_sig(&mut module, vec![WType::I32], vec![WType::I32]);
    let caller_sig = push_func_sig(&mut module, vec![WType::I32], vec![WType::I32]);
    let reveal_f = module
        .funcs
        .push(FuncDecl::Import(reveal_sig, "reveal_i32".into()));
    let wait_f = module
        .funcs
        .push(FuncDecl::Import(wait_sig, "reveal_i32_wait".into()));
    let mut body = portal_pc_waffle_ir::FunctionBody::new(&module, caller_sig);
    let entry = body.entry;
    let p0 = body.blocks[entry].params[0].1;
    let h = body.add_op(
        entry,
        Operator::Call {
            function_index: reveal_f,
        },
        &[p0],
        &[WType::I32],
    );
    let out = body.add_op(
        entry,
        Operator::Call {
            function_index: wait_f,
        },
        &[h],
        &[WType::I32],
    );
    body.set_terminator(
        entry,
        WTerminator::Return {
            values: vec![out],
        },
    );
    module
        .funcs
        .push(FuncDecl::Body(caller_sig, "caller".into(), body));
    module
}

fn build_double_wait_module() -> WModule<'static> {
    let mut module = empty_module();
    let reveal_sig = push_func_sig(&mut module, vec![WType::I32], vec![WType::I32]);
    let wait_sig = push_func_sig(&mut module, vec![WType::I32], vec![WType::I32]);
    let caller_sig = push_func_sig(&mut module, vec![WType::I32], vec![WType::I32]);
    let reveal_f = module
        .funcs
        .push(FuncDecl::Import(reveal_sig, "reveal_i32".into()));
    let wait_f = module
        .funcs
        .push(FuncDecl::Import(wait_sig, "reveal_i32_wait".into()));
    let mut body = portal_pc_waffle_ir::FunctionBody::new(&module, caller_sig);
    let entry = body.entry;
    let p0 = body.blocks[entry].params[0].1;
    let h = body.add_op(
        entry,
        Operator::Call {
            function_index: reveal_f,
        },
        &[p0],
        &[WType::I32],
    );
    let _first = body.add_op(
        entry,
        Operator::Call {
            function_index: wait_f,
        },
        &[h],
        &[WType::I32],
    );
    let second = body.add_op(
        entry,
        Operator::Call {
            function_index: wait_f,
        },
        &[h],
        &[WType::I32],
    );
    body.set_terminator(
        entry,
        WTerminator::Return {
            values: vec![second],
        },
    );
    module
        .funcs
        .push(FuncDecl::Body(caller_sig, "caller".into(), body));
    module
}

#[test]
fn vci_reveal_wait_result_is_public() {
    let wasm = build_reveal_module();
    let vc = VcConfig::new().with_call("caller", vec![VcArg::Private]);
    let mut target = VaffleTarget::new();
    let (errors, artifact) =
        lower_waffle_module_with_vc(&wasm, &mut target, &WaffleImportConfig::default(), &vc);
    assert!(errors.is_empty(), "{errors:?}");
    let fid = target.module.exports["caller"];
    let VaffleFuncDecl::Body(body) = &target.module.funcs[fid.0] else {
        panic!("body");
    };
    let public = artifact.handler.public;
    // Wait lowers to xor-with-zero under the public side, so some Poly/xor
    // result bits (the terminator args) should carry public.
    let vaffle::Terminator::Return { values } = &body.blocks[body.entry.0].terminator else {
        panic!("expected return");
    };
    assert!(
        values
            .iter()
            .any(|vid| body.values[vid.0].side == Some(public)),
        "wait result bits should be public-sided"
    );
}

#[test]
fn vci_double_wait_is_unsupported() {
    let wasm = build_double_wait_module();
    let mut target = VaffleTarget::new();
    let (errors, _artifact) =
        lower_waffle_module_with_vc(&wasm, &mut target, &WaffleImportConfig::default(), &VcConfig::new());
    assert!(
        errors.iter().any(|(name, err)| name == "caller" && err.0.contains("handle")),
        "double-wait must fail closed: {errors:?}"
    );
}

// ============================================================================
// vc-spec annihilator + select taint refinements (D1)
// ============================================================================

/// Collect the sides of a named function's returned (terminator) bits.
fn result_bit_sides(target: &VaffleTarget, name: &str) -> Vec<Option<volar_side::SideId>> {
    let fid = target.module.exports[name];
    let VaffleFuncDecl::Body(body) = &target.module.funcs[fid.0] else {
        panic!("expected body");
    };
    let vaffle::Terminator::Return { values } = &body.blocks[body.entry.0].terminator else {
        panic!("expected return");
    };
    values.iter().map(|vid| body.values[vid.0].side).collect()
}

/// `(func (param i32) (result i32) (i32.mul p0 (i32.const 0)))`
/// vc-spec annihilator: imul by concrete 0 ⇒ concrete 0, public.
#[test]
fn annihilator_imul_by_zero_is_public() {
    let mut module = empty_module();
    let sig = push_func_sig(&mut module, vec![WType::I32], vec![WType::I32]);
    let mut body = portal_pc_waffle_ir::FunctionBody::new(&module, sig);
    let entry = body.entry;
    let p0 = body.blocks[entry].params[0].1;
    let zero = body.add_op(entry, Operator::I32Const { value: 0 }, &[], &[WType::I32]);
    let prod = body.add_op(entry, Operator::I32Mul, &[p0, zero], &[WType::I32]);
    body.set_terminator(entry, WTerminator::Return { values: vec![prod] });
    module.funcs.push(FuncDecl::Body(sig, "mulz".into(), body));

    let vc = VcConfig::new().with_call("mulz", vec![VcArg::Private]);
    let mut target = VaffleTarget::new();
    let (errors, artifact) =
        lower_waffle_module_with_vc(&module, &mut target, &WaffleImportConfig::default(), &vc);
    assert!(errors.is_empty(), "{errors:?}");
    let public = artifact.handler.public;
    let sides = result_bit_sides(&target, "mulz");
    assert!(!sides.is_empty());
    assert!(
        sides.iter().all(|s| *s == Some(public)),
        "imul by concrete 0 must be public on every result bit: {sides:?}"
    );
}

/// `(func (param i32) (result i32) (i32.and p0 (i32.const 0)))` ⇒ public 0.
#[test]
fn annihilator_iand_by_zero_is_public() {
    let mut module = empty_module();
    let sig = push_func_sig(&mut module, vec![WType::I32], vec![WType::I32]);
    let mut body = portal_pc_waffle_ir::FunctionBody::new(&module, sig);
    let entry = body.entry;
    let p0 = body.blocks[entry].params[0].1;
    let zero = body.add_op(entry, Operator::I32Const { value: 0 }, &[], &[WType::I32]);
    let r = body.add_op(entry, Operator::I32And, &[p0, zero], &[WType::I32]);
    body.set_terminator(entry, WTerminator::Return { values: vec![r] });
    module.funcs.push(FuncDecl::Body(sig, "andz".into(), body));

    let vc = VcConfig::new().with_call("andz", vec![VcArg::Blind]);
    let mut target = VaffleTarget::new();
    let (errors, artifact) =
        lower_waffle_module_with_vc(&module, &mut target, &WaffleImportConfig::default(), &vc);
    assert!(errors.is_empty(), "{errors:?}");
    let public = artifact.handler.public;
    let sides = result_bit_sides(&target, "andz");
    assert!(sides.iter().all(|s| *s == Some(public)), "{sides:?}");
}

/// `(func (param i32) (result i32) (i32.or p0 (i32.const -1)))` ⇒ public all-ones.
#[test]
fn annihilator_ior_by_all_ones_is_public() {
    let mut module = empty_module();
    let sig = push_func_sig(&mut module, vec![WType::I32], vec![WType::I32]);
    let mut body = portal_pc_waffle_ir::FunctionBody::new(&module, sig);
    let entry = body.entry;
    let p0 = body.blocks[entry].params[0].1;
    let ones = body.add_op(entry, Operator::I32Const { value: 0xFFFF_FFFF }, &[], &[WType::I32]);
    let r = body.add_op(entry, Operator::I32Or, &[p0, ones], &[WType::I32]);
    body.set_terminator(entry, WTerminator::Return { values: vec![r] });
    module.funcs.push(FuncDecl::Body(sig, "oro".into(), body));

    let vc = VcConfig::new().with_call("oro", vec![VcArg::Private]);
    let mut target = VaffleTarget::new();
    let (errors, artifact) =
        lower_waffle_module_with_vc(&module, &mut target, &WaffleImportConfig::default(), &vc);
    assert!(errors.is_empty(), "{errors:?}");
    let public = artifact.handler.public;
    let sides = result_bit_sides(&target, "oro");
    assert!(sides.iter().all(|s| *s == Some(public)), "{sides:?}");
}

/// `(func (param i32 i32) (result i32)
///    (select p0 p1 (i32.const 1)))`
/// Concrete cond ⇒ result takes the *selected* operand's taint (param 0),
/// not the join of both branches.
#[test]
fn select_concrete_cond_takes_selected_taint() {
    let mut module = empty_module();
    let sig = push_func_sig(&mut module, vec![WType::I32, WType::I32], vec![WType::I32]);
    let mut body = portal_pc_waffle_ir::FunctionBody::new(&module, sig);
    let entry = body.entry;
    let p0 = body.blocks[entry].params[0].1;
    let p1 = body.blocks[entry].params[1].1;
    let one = body.add_op(entry, Operator::I32Const { value: 1 }, &[], &[WType::I32]);
    let r = body.add_op(entry, Operator::Select, &[p0, p1, one], &[WType::I32]);
    body.set_terminator(entry, WTerminator::Return { values: vec![r] });
    module.funcs.push(FuncDecl::Body(sig, "sel".into(), body));

    // cond==1 ⇒ selects p0 (Private/local). Result must be local, not a join.
    let vc = VcConfig::new().with_call("sel", vec![VcArg::Private, VcArg::Blind]);
    let mut target = VaffleTarget::new();
    let (errors, artifact) =
        lower_waffle_module_with_vc(&module, &mut target, &WaffleImportConfig::default(), &vc);
    assert!(errors.is_empty(), "{errors:?}");
    let local = artifact.handler.local;
    let sides = result_bit_sides(&target, "sel");
    assert!(
        sides.iter().all(|s| *s == Some(local)),
        "concrete-cond select must take the selected operand's taint: {sides:?}"
    );
}
