// @reliability: normal
//! Tests for `StackAllocExt` — stack allocation + pointer operations in `CBackend`.
//!
//! Each test that produces code writes C to a tempdir, compiles with
//! `cc -O0 -std=c99`, runs the binary, and checks stdout.

use volar_c_backend::CBackend;
use volar_lir::{LirTarget, LirType, StackAllocExt};
use volar_lir_test_corpus::compile_and_run;

// ============================================================================
// Test 1: stack_alloc_ext returns Some for CBackend
// ============================================================================

#[test]
fn test_stack_alloc_ext_returns_some() {
    let mut b = CBackend::new();
    assert!(b.stack_alloc_ext().is_some());
}

// ============================================================================
// Test 2: alloca + ptr_store + ptr_load roundtrip
//
// Allocates a single-element U32 slot, stores 42, loads it back, returns it.
// ============================================================================

#[test]
fn test_alloca_store_load() {
    let mut b = CBackend::new();

    let (entry, _) = b.begin_function("alloca_store_load", &[], Some(LirType::U32));
    b.switch_to_block(entry);

    // alloca, store, load — calling StackAllocExt methods directly on CBackend
    // avoids borrow-checker friction (no trait-object lifetime to manage).
    let ptr = b.alloca(LirType::U32, 1);
    let val = b.iconst(LirType::U32, 42);
    b.ptr_store(ptr, val);
    let loaded = b.ptr_load(ptr, LirType::U32);
    b.ret(&[loaded]);
    b.end_function();

    let c_src = b.finish();
    let output = compile_and_run(&c_src, r#"  printf("%u\n", alloca_store_load());"#);
    assert_eq!(output.trim(), "42");
}

// ============================================================================
// Test 3: alloca multiple elements, store via ptr_offset, sum via ptr_offset
//
// Allocates U32[4], writes 10/20/30/40, reads all four back and sums them.
// Expected result: 100.
// ============================================================================

#[test]
fn test_alloca_multiple_elements() {
    let mut b = CBackend::new();

    let (entry, _) = b.begin_function("alloca_multi", &[], Some(LirType::U32));
    b.switch_to_block(entry);

    let ptr = b.alloca(LirType::U32, 4);

    // Store 10, 20, 30, 40 at indices 0–3.
    let store_vals: [i64; 4] = [10, 20, 30, 40];
    for (i, &v) in store_vals.iter().enumerate() {
        let idx = b.iconst(LirType::U32, i as i64);
        let elem_ptr = b.ptr_offset(ptr, idx);
        let val = b.iconst(LirType::U32, v);
        b.ptr_store(elem_ptr, val);
    }

    // Load each element and accumulate.
    let mut acc = b.iconst(LirType::U32, 0);
    for i in 0i64..4 {
        let idx = b.iconst(LirType::U32, i);
        let elem_ptr = b.ptr_offset(ptr, idx);
        let loaded = b.ptr_load(elem_ptr, LirType::U32);
        acc = b.add(acc, loaded);
    }
    b.ret(&[acc]);
    b.end_function();

    let c_src = b.finish();
    let output = compile_and_run(&c_src, r#"  printf("%u\n", alloca_multi());"#);
    assert_eq!(output.trim(), "100"); // 10 + 20 + 30 + 40
}

// ============================================================================
// Test 4: ptr_offset — write i at index i, read back index 5
// ============================================================================

#[test]
fn test_ptr_offset() {
    let mut b = CBackend::new();

    let (entry, _) = b.begin_function("ptr_offset_test", &[], Some(LirType::U8));
    b.switch_to_block(entry);

    let ptr = b.alloca(LirType::U8, 8);

    // Store byte value i at position i.
    for i in 0i64..8 {
        let idx = b.iconst(LirType::U32, i);
        let elem_ptr = b.ptr_offset(ptr, idx);
        let val = b.iconst(LirType::U8, i);
        b.ptr_store(elem_ptr, val);
    }

    // Load index 5 — expect value 5.
    let idx5 = b.iconst(LirType::U32, 5);
    let p5 = b.ptr_offset(ptr, idx5);
    let result = b.ptr_load(p5, LirType::U8);
    b.ret(&[result]);
    b.end_function();

    let c_src = b.finish();
    let output = compile_and_run(&c_src, r#"  printf("%u\n", (unsigned)ptr_offset_test());"#);
    assert_eq!(output.trim(), "5");
}

// ============================================================================
// Test 5: alloca declaration appears in the preamble (before block0 label)
//
// The `slot_vN` array must be declared with function scope so that the pointer
// to it does not dangle.  The C backend achieves this by writing into
// `preamble` rather than `body`.
// ============================================================================

#[test]
fn test_alloca_in_preamble() {
    let mut b = CBackend::new();

    let (entry, _) = b.begin_function("preamble_test", &[], Some(LirType::U32));
    b.switch_to_block(entry);

    let ptr = b.alloca(LirType::U32, 1);
    let val = b.iconst(LirType::U32, 7);
    b.ptr_store(ptr, val);
    let loaded = b.ptr_load(ptr, LirType::U32);
    b.ret(&[loaded]);
    b.end_function();

    let c_src = b.finish();

    // Locate the slot declaration and the first block label in the C output.
    let slot_pos = c_src
        .find("slot_v")
        .unwrap_or_else(|| panic!("no 'slot_v' found in C output:\n{c_src}"));
    let block_label_pos = c_src
        .find("block0:")
        .unwrap_or_else(|| panic!("no 'block0:' label found in C output:\n{c_src}"));

    assert!(
        slot_pos < block_label_pos,
        "slot_v declaration must appear before block0: label (preamble scope)\n\
         slot_pos={slot_pos}, block_label_pos={block_label_pos}\n---\n{c_src}"
    );
}

// ============================================================================
// Regression: `Ptr(Arr(..))` values must register the array typedef.
//
// A struct field of type `Ptr(Arr(U8, 32))` (e.g. a Vec fat-pointer's `data`
// field) renders as `Arr_U8_32* data;`; the referenced typedef must be
// emitted alongside the struct or the translation unit does not compile.
// ============================================================================

#[test]
fn test_ptr_to_array_field_registers_typedef() {
    use volar_lir::{FieldDef, StructDef};

    let mut b = CBackend::new();

    // struct Vec_U8x32 { Arr_U8_32* data; uint64_t len; }
    let vec_id = b.define_struct(StructDef {
        name: "Vec_U8x32".to_owned(),
        fields: vec![
            FieldDef {
                name: "data".to_owned(),
                ty: LirType::Ptr(Box::new(LirType::Arr(
                    Box::new(LirType::U8),
                    32,
                ))),
            },
            FieldDef {
                name: "len".to_owned(),
                ty: LirType::U64,
            },
        ],
    });

    // The struct definition (emitted in `finish`) must contain the
    // `Arr_U8_32` typedef — assert before compiling so the failure message
    // is precise.
    let (entry, params) = b.begin_function(
        "fat_ptr_len",
        &[LirType::Struct(vec_id)],
        Some(LirType::U64),
    );
    b.switch_to_block(entry);
    // Unpack the struct param: param 0 has 2 flat scalars (ptr, len).
    let scalars = &params[0];
    b.ret(&[scalars[1].clone()]);
    b.end_function();

    let c_src = b.finish();
    assert!(
        c_src.contains("} Arr_U8_32;"),
        "Ptr(Arr) struct field must register the array typedef:\n{c_src}"
    );
    let output = compile_and_run(
        &c_src,
        r#"
  static uint8_t backing[32];
  Vec_U8x32 v = { .data = backing, .len = 32 };
  printf("%llu\n", (unsigned long long)fat_ptr_len(v));
"#,
    );
    assert_eq!(output.trim(), "32");
}

// ============================================================================
// Regression: `alloca` of a nested-array element type must register the
// element typedef (the declaration renders `Arr_U8_4 slot[2];`).
// ============================================================================

#[test]
fn test_alloca_nested_array_registers_typedef() {
    let mut b = CBackend::new();

    let (entry, _) = b.begin_function("nested_slot_sum", &[], Some(LirType::U64));
    b.switch_to_block(entry);

    // [[u8; 4]; 2] slot: alloca element type is itself an array. Each
    // `ptr_index_store` writes one whole row (4 scalars).
    let inner = LirType::Arr(Box::new(LirType::U8), 4);
    let ptr = b.alloca(inner.clone(), 2);
    let idx0 = b.iconst(LirType::U64, 0);
    let idx1 = b.iconst(LirType::U64, 1);
    let zero = b.iconst(LirType::U8, 0);
    let three = b.iconst(LirType::U8, 3);
    let four = b.iconst(LirType::U8, 4);
    b.ptr_index_store(ptr.clone(), idx0.clone(), &[three, zero.clone(), zero.clone(), zero.clone()], &inner);
    b.ptr_index_store(ptr.clone(), idx1.clone(), &[four, zero.clone(), zero.clone(), zero.clone()], &inner);
    // Read back rows 0 and 1, sum their first cells.
    let a = b.ptr_index_load(ptr.clone(), idx0, &inner);
    let c = b.ptr_index_load(ptr, idx1, &inner);
    let a0 = b.zext(a[0].clone(), LirType::U64);
    let c0 = b.zext(c[0].clone(), LirType::U64);
    let sum = b.add(a0, c0);
    b.ret(&[sum]);
    b.end_function();

    let c_src = b.finish();
    assert!(
        c_src.contains("} Arr_U8_4;"),
        "alloca of nested-array element type must register the element typedef:\n{c_src}"
    );
    let output = compile_and_run(
        &c_src,
        r#"  printf("%llu\n", (unsigned long long)nested_slot_sum());"#,
    );
    assert_eq!(output.trim(), "7");
}
