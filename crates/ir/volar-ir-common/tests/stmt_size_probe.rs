use volar_ir_common::Stmt;

/// The `Stmt` enum's stride is resident memory at whole-program scale
/// (tens of millions of statements). The near-zero-population
/// Oracle/Action variants carry 3-6 heap handles each and are boxed so
/// they don't set the stride for every statement; the stride is set by
/// `Poly` (64 bytes, align 16) instead. If a new unboxed variant grows
/// past this, either shrink it (box its payload) or consciously raise
/// this bound with a comment naming the memory cost.
#[test]
fn stmt_enum_stays_compact() {
    assert!(
        core::mem::size_of::<Stmt<u32>>() <= 64,
        "Stmt enum grew to {} bytes — box the new variant's payload (see OracleCallData)",
        core::mem::size_of::<Stmt<u32>>()
    );
    assert_eq!(core::mem::size_of::<Stmt<u32>>(), 64);
}
