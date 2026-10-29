; ModuleID = 'v5.899bf7dcd45d47ce-cgu.0'
source_filename = "v5.899bf7dcd45d47ce-cgu.0"
target datalayout = "e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-n32:64-S128-Fn32"
target triple = "arm64-apple-macosx11.0.0"

@alloc_58ffb6057d8cef681b5cbb4d4fae61bd = private unnamed_addr constant [6 x i8] c"v5.rs\00", align 1, !guid !0
@alloc_25ad3984367ecc83724b12e4dfc6eacf = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_58ffb6057d8cef681b5cbb4d4fae61bd, [16 x i8] c"\05\00\00\00\00\00\00\00\1A\00\00\00\1C\00\00\00" }>, align 8, !guid !1
@alloc_45d03375086194dd1ed36454faa86386 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_58ffb6057d8cef681b5cbb4d4fae61bd, [16 x i8] c"\05\00\00\00\00\00\00\00\1B\00\00\00\1C\00\00\00" }>, align 8, !guid !2
@alloc_e099397c09badf31d2b2b70a444d5e05 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_58ffb6057d8cef681b5cbb4d4fae61bd, [16 x i8] c"\05\00\00\00\00\00\00\00\1D\00\00\00\14\00\00\00" }>, align 8, !guid !3
@alloc_a21a5f97d218440b92d7185739fae224 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_58ffb6057d8cef681b5cbb4d4fae61bd, [16 x i8] c"\05\00\00\00\00\00\00\00\1E\00\00\00\11\00\00\00" }>, align 8, !guid !4
@alloc_6dc42682853c59aa1635a823052f70dd = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_58ffb6057d8cef681b5cbb4d4fae61bd, [16 x i8] c"\05\00\00\00\00\00\00\00\12\00\00\00\0D\00\00\00" }>, align 8, !guid !5
@alloc_d7c94b5ccd18bf87d4ae5077951a94a0 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_58ffb6057d8cef681b5cbb4d4fae61bd, [16 x i8] c"\05\00\00\00\00\00\00\00\16\00\00\00\11\00\00\00" }>, align 8, !guid !6
@alloc_f1ea7314b44a72bdc277a3713f68e7d1 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_58ffb6057d8cef681b5cbb4d4fae61bd, [16 x i8] c"\05\00\00\00\00\00\00\00!\00\00\00\09\00\00\00" }>, align 8, !guid !7
@alloc_5e18d0c453824918e7d8307f775acfa8 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_58ffb6057d8cef681b5cbb4d4fae61bd, [16 x i8] c"\05\00\00\00\00\00\00\00\22\00\00\00\09\00\00\00" }>, align 8, !guid !8
@alloc_d4764c437c996997935de830b4424597 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_58ffb6057d8cef681b5cbb4d4fae61bd, [16 x i8] c"\05\00\00\00\00\00\00\00%\00\00\00\0C\00\00\00" }>, align 8, !guid !9

; __rustc::rust_begin_unwind
; Function Attrs: noreturn uwtable
define void @_RNvCsdtSMxSA38aw_7___rustc17rust_begin_unwind(ptr align 8 %_1) unnamed_addr #0 !guid !14 {
start:
  br label %bb1

bb1:                                              ; preds = %bb1, %start
  br label %bb1
}

; Function Attrs: uwtable
define i64 @step_v5(i64 %code0, i64 %code1, i64 %code2, i64 %code3, i64 %code4, i64 %steps) unnamed_addr #1 !guid !15 {
start:
  %b = alloca [8 x i8], align 8
  %a = alloca [8 x i8], align 8
  %imm = alloca [8 x i8], align 8
  %op = alloca [8 x i8], align 8
  %i = alloca [8 x i8], align 8
  %pc = alloca [8 x i8], align 8
  %sp = alloca [8 x i8], align 8
  %s1 = alloca [8 x i8], align 8
  %s0 = alloca [8 x i8], align 8
  %_0 = alloca [8 x i8], align 8
  store i64 0, ptr %s0, align 8
  store i64 0, ptr %s1, align 8
  store i64 0, ptr %sp, align 8
  store i64 0, ptr %pc, align 8
  store i64 0, ptr %i, align 8
  br label %bb1

bb1:                                              ; preds = %bb49, %start
  %_13 = load i64, ptr %i, align 8
  %_12 = icmp ult i64 %_13, %steps
  br i1 %_12, label %bb2, label %bb50

bb50:                                             ; preds = %bb12, %bb2, %bb1
  %_71 = load i64, ptr %sp, align 8
  %_70 = icmp ugt i64 %_71, 0
  br i1 %_70, label %bb51, label %bb55

bb2:                                              ; preds = %bb1
  %_15 = load i64, ptr %pc, align 8
  %_14 = icmp ult i64 %_15, 5
  br i1 %_14, label %bb3, label %bb50

bb3:                                              ; preds = %bb2
  %_18 = load i64, ptr %pc, align 8
  %_17 = icmp eq i64 %_18, 0
  br i1 %_17, label %bb4, label %bb5

bb5:                                              ; preds = %bb3
  %_20 = load i64, ptr %pc, align 8
  %_19 = icmp eq i64 %_20, 1
  br i1 %_19, label %bb6, label %bb7

bb4:                                              ; preds = %bb3
  store i64 %code0, ptr %op, align 8
  br label %bb12

bb7:                                              ; preds = %bb5
  %_22 = load i64, ptr %pc, align 8
  %_21 = icmp eq i64 %_22, 2
  br i1 %_21, label %bb8, label %bb9

bb6:                                              ; preds = %bb5
  store i64 %code1, ptr %op, align 8
  br label %bb12

bb9:                                              ; preds = %bb7
  %_24 = load i64, ptr %pc, align 8
  %_23 = icmp eq i64 %_24, 3
  br i1 %_23, label %bb10, label %bb11

bb8:                                              ; preds = %bb7
  store i64 %code2, ptr %op, align 8
  br label %bb12

bb11:                                             ; preds = %bb9
  store i64 %code4, ptr %op, align 8
  br label %bb12

bb10:                                             ; preds = %bb9
  store i64 %code3, ptr %op, align 8
  br label %bb12

bb12:                                             ; preds = %bb4, %bb6, %bb8, %bb10, %bb11
  %_26 = load i64, ptr %op, align 8
  %_25 = icmp eq i64 %_26, 0
  br i1 %_25, label %bb50, label %bb13

bb13:                                             ; preds = %bb12
  %_28 = load i64, ptr %op, align 8
  %_27 = icmp eq i64 %_28, 96
  br i1 %_27, label %bb14, label %bb30

bb30:                                             ; preds = %bb13
  %_47 = load i64, ptr %op, align 8
  %_46 = icmp eq i64 %_47, 1
  br i1 %_46, label %bb31, label %bb47

bb14:                                             ; preds = %bb13
  %0 = load i64, ptr %pc, align 8
  %_29.0 = add i64 %0, 1
  %_29.1 = icmp ult i64 %_29.0, %0
  br i1 %_29.1, label %panic4, label %bb15

bb47:                                             ; preds = %bb29, %bb24, %bb46, %bb31, %bb30
  %1 = load i64, ptr %pc, align 8
  %_68.0 = add i64 %1, 1
  %_68.1 = icmp ult i64 %_68.0, %1
  br i1 %_68.1, label %panic6, label %bb48

bb31:                                             ; preds = %bb30
  %_49 = load i64, ptr %sp, align 8
  %_48 = icmp uge i64 %_49, 2
  br i1 %_48, label %bb32, label %bb47

bb32:                                             ; preds = %bb31
  %_53 = load i64, ptr %sp, align 8
  %_54.0 = sub i64 %_53, 1
  %_54.1 = icmp ult i64 %_53, 1
  br i1 %_54.1, label %panic, label %bb33

bb33:                                             ; preds = %bb32
  %_51 = icmp eq i64 %_54.0, 0
  br i1 %_51, label %bb34, label %bb35

panic:                                            ; preds = %bb32
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_25ad3984367ecc83724b12e4dfc6eacf) #3
  unreachable

bb35:                                             ; preds = %bb33
  %2 = load i64, ptr %s1, align 8
  store i64 %2, ptr %a, align 8
  br label %bb36

bb34:                                             ; preds = %bb33
  %3 = load i64, ptr %s0, align 8
  store i64 %3, ptr %a, align 8
  br label %bb36

bb36:                                             ; preds = %bb34, %bb35
  %_58 = load i64, ptr %sp, align 8
  %_59.0 = sub i64 %_58, 2
  %_59.1 = icmp ult i64 %_58, 2
  br i1 %_59.1, label %panic1, label %bb37

bb37:                                             ; preds = %bb36
  %_56 = icmp eq i64 %_59.0, 0
  br i1 %_56, label %bb38, label %bb39

panic1:                                           ; preds = %bb36
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_45d03375086194dd1ed36454faa86386) #3
  unreachable

bb39:                                             ; preds = %bb37
  %4 = load i64, ptr %s1, align 8
  store i64 %4, ptr %b, align 8
  br label %bb40

bb38:                                             ; preds = %bb37
  %5 = load i64, ptr %s0, align 8
  store i64 %5, ptr %b, align 8
  br label %bb40

bb40:                                             ; preds = %bb38, %bb39
  %_61 = load i64, ptr %a, align 8
  %_62 = load i64, ptr %b, align 8
  %_0.i = add i64 %_61, %_62
  %_65 = load i64, ptr %sp, align 8
  %_66.0 = sub i64 %_65, 2
  %_66.1 = icmp ult i64 %_65, 2
  br i1 %_66.1, label %panic2, label %bb42

bb42:                                             ; preds = %bb40
  %_63 = icmp eq i64 %_66.0, 0
  br i1 %_63, label %bb43, label %bb44

panic2:                                           ; preds = %bb40
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_e099397c09badf31d2b2b70a444d5e05) #3
  unreachable

bb44:                                             ; preds = %bb42
  store i64 %_0.i, ptr %s1, align 8
  br label %bb45

bb43:                                             ; preds = %bb42
  store i64 %_0.i, ptr %s0, align 8
  br label %bb45

bb45:                                             ; preds = %bb43, %bb44
  %6 = load i64, ptr %sp, align 8
  %_67.0 = sub i64 %6, 1
  %_67.1 = icmp ult i64 %6, 1
  br i1 %_67.1, label %panic3, label %bb46

bb46:                                             ; preds = %bb45
  store i64 %_67.0, ptr %sp, align 8
  br label %bb47

panic3:                                           ; preds = %bb45
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_a21a5f97d218440b92d7185739fae224) #3
  unreachable

bb15:                                             ; preds = %bb14
  store i64 %_29.0, ptr %pc, align 8
  %_32 = load i64, ptr %pc, align 8
  %_31 = icmp eq i64 %_32, 0
  br i1 %_31, label %bb16, label %bb17

panic4:                                           ; preds = %bb14
; call core::panicking::panic_const::panic_const_add_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8 @alloc_6dc42682853c59aa1635a823052f70dd) #3
  unreachable

bb17:                                             ; preds = %bb15
  %_34 = load i64, ptr %pc, align 8
  %_33 = icmp eq i64 %_34, 1
  br i1 %_33, label %bb18, label %bb19

bb16:                                             ; preds = %bb15
  store i64 %code0, ptr %imm, align 8
  br label %bb24

bb19:                                             ; preds = %bb17
  %_36 = load i64, ptr %pc, align 8
  %_35 = icmp eq i64 %_36, 2
  br i1 %_35, label %bb20, label %bb21

bb18:                                             ; preds = %bb17
  store i64 %code1, ptr %imm, align 8
  br label %bb24

bb21:                                             ; preds = %bb19
  %_38 = load i64, ptr %pc, align 8
  %_37 = icmp eq i64 %_38, 3
  br i1 %_37, label %bb22, label %bb23

bb20:                                             ; preds = %bb19
  store i64 %code2, ptr %imm, align 8
  br label %bb24

bb23:                                             ; preds = %bb21
  store i64 %code4, ptr %imm, align 8
  br label %bb24

bb22:                                             ; preds = %bb21
  store i64 %code3, ptr %imm, align 8
  br label %bb24

bb24:                                             ; preds = %bb16, %bb18, %bb20, %bb22, %bb23
  %_40 = load i64, ptr %sp, align 8
  %_39 = icmp ult i64 %_40, 2
  br i1 %_39, label %bb25, label %bb47

bb25:                                             ; preds = %bb24
  %_42 = load i64, ptr %sp, align 8
  %_41 = icmp eq i64 %_42, 0
  br i1 %_41, label %bb26, label %bb27

bb27:                                             ; preds = %bb25
  %_44 = load i64, ptr %imm, align 8
  store i64 %_44, ptr %s1, align 8
  br label %bb28

bb26:                                             ; preds = %bb25
  %_43 = load i64, ptr %imm, align 8
  store i64 %_43, ptr %s0, align 8
  br label %bb28

bb28:                                             ; preds = %bb26, %bb27
  %7 = load i64, ptr %sp, align 8
  %_45.0 = add i64 %7, 1
  %_45.1 = icmp ult i64 %_45.0, %7
  br i1 %_45.1, label %panic5, label %bb29

bb29:                                             ; preds = %bb28
  store i64 %_45.0, ptr %sp, align 8
  br label %bb47

panic5:                                           ; preds = %bb28
; call core::panicking::panic_const::panic_const_add_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8 @alloc_d7c94b5ccd18bf87d4ae5077951a94a0) #3
  unreachable

bb48:                                             ; preds = %bb47
  store i64 %_68.0, ptr %pc, align 8
  %8 = load i64, ptr %i, align 8
  %_69.0 = add i64 %8, 1
  %_69.1 = icmp ult i64 %_69.0, %8
  br i1 %_69.1, label %panic7, label %bb49

panic6:                                           ; preds = %bb47
; call core::panicking::panic_const::panic_const_add_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8 @alloc_f1ea7314b44a72bdc277a3713f68e7d1) #3
  unreachable

bb49:                                             ; preds = %bb48
  store i64 %_69.0, ptr %i, align 8
  br label %bb1

panic7:                                           ; preds = %bb48
; call core::panicking::panic_const::panic_const_add_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8 @alloc_5e18d0c453824918e7d8307f775acfa8) #3
  unreachable

bb55:                                             ; preds = %bb50
  store i64 0, ptr %_0, align 8
  br label %bb56

bb51:                                             ; preds = %bb50
  %_74 = load i64, ptr %sp, align 8
  %_75.0 = sub i64 %_74, 1
  %_75.1 = icmp ult i64 %_74, 1
  br i1 %_75.1, label %panic8, label %bb52

bb56:                                             ; preds = %bb53, %bb54, %bb55
  %9 = load i64, ptr %_0, align 8
  ret i64 %9

bb52:                                             ; preds = %bb51
  %_72 = icmp eq i64 %_75.0, 0
  br i1 %_72, label %bb53, label %bb54

panic8:                                           ; preds = %bb51
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_d4764c437c996997935de830b4424597) #3
  unreachable

bb54:                                             ; preds = %bb52
  %10 = load i64, ptr %s1, align 8
  store i64 %10, ptr %_0, align 8
  br label %bb56

bb53:                                             ; preds = %bb52
  %11 = load i64, ptr %s0, align 8
  store i64 %11, ptr %_0, align 8
  br label %bb56
}

; core::panicking::panic_const::panic_const_sub_overflow
; Function Attrs: cold noinline noreturn uwtable
declare void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8) unnamed_addr #2

; core::panicking::panic_const::panic_const_add_overflow
; Function Attrs: cold noinline noreturn uwtable
declare void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8) unnamed_addr #2

attributes #0 = { noreturn uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #1 = { uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #2 = { cold noinline noreturn uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #3 = { noinline noreturn }

!llvm.module.flags = !{!10, !11, !12}
!llvm.ident = !{!13}

!0 = !{i64 2700601298755496239}
!1 = !{i64 6737334514681336445}
!2 = !{i64 1541668711385346474}
!3 = !{i64 -6131453288041659570}
!4 = !{i64 -2474909374549716074}
!5 = !{i64 304800015499419402}
!6 = !{i64 2222040935271484064}
!7 = !{i64 -2127065006263806244}
!8 = !{i64 -1374412464027138899}
!9 = !{i64 128947880266256786}
!10 = !{i32 8, !"PIC Level", i32 2}
!11 = !{i32 7, !"uwtable", i32 2}
!12 = !{i32 7, !"frame-pointer", i32 1}
!13 = !{!"rustc version 1.99.0 (b940084d7 2026-09-28)"}
!14 = !{i64 877453575045248192}
!15 = !{i64 -6741811289025776913}
