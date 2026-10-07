// @reliability: experimental
// @ai: assisted
//! Record-once / replay-many fan-out for multi-backend parallelism.

use volar_lir::{FieldDef, LirTarget, LirType, StructDef};
use volar_lir_saved::{LirCall, RecordingTarget, SavedLirModule};

fn build_add() -> SavedLirModule {
    let mut rec = RecordingTarget::new();
    let (entry, params) =
        rec.begin_function("add", &[LirType::U32, LirType::U32], Some(LirType::U32));
    rec.switch_to_block(entry);
    let sum = rec.add(params[0][0], params[1][0]);
    rec.ret(&[sum]);
    rec.end_function();
    rec.finish()
}

#[test]
fn recording_target_advertises_stack_allocation_only_when_opted_in() {
    let mut portable = RecordingTarget::new();
    assert!(portable.stack_alloc_ext().is_none());

    let mut stack_capable = RecordingTarget::new().with_stack_alloc_ext();
    assert!(stack_capable.stack_alloc_ext().is_some());
}

#[test]
fn aggregate_pointer_load_records_flat_scalar_outputs() {
    let mut rec = RecordingTarget::new();
    let pair = rec.define_struct(StructDef {
        name: "Pair".into(),
        fields: vec![
            FieldDef {
                name: "word".into(),
                ty: LirType::U32,
            },
            FieldDef {
                name: "bytes".into(),
                ty: LirType::Arr(Box::new(LirType::U8), 2),
            },
        ],
    });
    let ptr = rec.iconst(LirType::Ptr(Box::new(LirType::Struct(pair))), 0);
    let idx = rec.iconst(LirType::U64, 0);

    let outputs = rec.ptr_index_load(ptr, idx, &LirType::Struct(pair));
    assert_eq!(outputs.len(), 3);
    assert_eq!(
        outputs
            .iter()
            .map(|value| rec.value_scalar_type(value))
            .collect::<Vec<_>>(),
        vec![LirType::U32, LirType::U8, LirType::U8]
    );

    let saved = rec.finish();
    assert!(saved.calls.iter().any(|call| matches!(
        call,
        LirCall::PtrIndexLoad { outs, .. } if outs.len() == 3
    )));

    let mut replay = RecordingTarget::new();
    saved.replay(&mut replay);
    assert_eq!(saved, replay.finish());
}

#[test]
fn replay_into_many_identical_targets() {
    let saved = build_add();
    let mut a = [
        RecordingTarget::new(),
        RecordingTarget::new(),
        RecordingTarget::new(),
    ];
    saved.replay_into_many(&mut a);
    let finished: Vec<SavedLirModule> = a.into_iter().map(|t| t.finish()).collect();
    assert_eq!(finished[0], finished[1]);
    assert_eq!(finished[1], finished[2]);
    assert_eq!(finished[0], saved);
}

#[test]
fn replay_pair_heterogeneous_recordings() {
    let saved = build_add();
    let mut left = RecordingTarget::new();
    let mut right = RecordingTarget::new();
    saved.replay_pair(&mut left, &mut right);
    assert_eq!(left.finish(), right.finish());
}
