; ModuleID = 'v2.5563e501cbb97b48-cgu.0'
source_filename = "v2.5563e501cbb97b48-cgu.0"
target datalayout = "e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-n32:64-S128-Fn32"
target triple = "arm64-apple-macosx11.0.0"

@alloc_1b06e3b39acccd8eeb554b94fcb9b40b = private unnamed_addr constant [6 x i8] c"v2.rs\00", align 1, !guid !0
@alloc_60eda4c9093e6cf6126592eba618b8ff = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_1b06e3b39acccd8eeb554b94fcb9b40b, [16 x i8] c"\05\00\00\00\00\00\00\00#\00\00\00\0C\00\00\00" }>, align 8, !guid !1
@alloc_e28d5df1b873b139e24bf25f8cd528b7 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_1b06e3b39acccd8eeb554b94fcb9b40b, [16 x i8] c"\05\00\00\00\00\00\00\00\18\00\00\00\1C\00\00\00" }>, align 8, !guid !2
@alloc_8706287b454cf1d058e157719a7c99dc = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_1b06e3b39acccd8eeb554b94fcb9b40b, [16 x i8] c"\05\00\00\00\00\00\00\00\19\00\00\00\1C\00\00\00" }>, align 8, !guid !3
@alloc_e89c74b9a276aeecc2a9ba810185f987 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_1b06e3b39acccd8eeb554b94fcb9b40b, [16 x i8] c"\05\00\00\00\00\00\00\00\1B\00\00\00\14\00\00\00" }>, align 8, !guid !4
@alloc_fad86f0e941aabbb7dfabd775317c85a = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_1b06e3b39acccd8eeb554b94fcb9b40b, [16 x i8] c"\05\00\00\00\00\00\00\00\1C\00\00\00\11\00\00\00" }>, align 8, !guid !5
@alloc_b23e46e4090886bc04199447ce3334ec = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_1b06e3b39acccd8eeb554b94fcb9b40b, [16 x i8] c"\05\00\00\00\00\00\00\00\10\00\00\00\0D\00\00\00" }>, align 8, !guid !6
@alloc_b21b0ede86ab53924c64f326ae3ed7f4 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_1b06e3b39acccd8eeb554b94fcb9b40b, [16 x i8] c"\05\00\00\00\00\00\00\00\14\00\00\00\11\00\00\00" }>, align 8, !guid !7
@alloc_5c1867983e2bc3b8ed32f672d648d8fe = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_1b06e3b39acccd8eeb554b94fcb9b40b, [16 x i8] c"\05\00\00\00\00\00\00\00\1F\00\00\00\09\00\00\00" }>, align 8, !guid !8
@alloc_435bd128cf3654ccbe35ee15f7923a47 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_1b06e3b39acccd8eeb554b94fcb9b40b, [16 x i8] c"\05\00\00\00\00\00\00\00 \00\00\00\09\00\00\00" }>, align 8, !guid !9

; __rustc::rust_begin_unwind
; Function Attrs: noreturn uwtable
define void @_RNvCsdtSMxSA38aw_7___rustc17rust_begin_unwind(ptr align 8 %_1) unnamed_addr #0 !guid !14 {
start:
  br label %bb1

bb1:                                              ; preds = %bb1, %start
  br label %bb1
}

; Function Attrs: uwtable
define i64 @step_v2(i64 %code0, i64 %code1, i64 %code2, i64 %code3, i64 %code4, i64 %steps) unnamed_addr #1 !guid !15 {
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

bb1:                                              ; preds = %bb48, %start
  %_13 = load i64, ptr %i, align 8
  %_12 = icmp ult i64 %_13, %steps
  br i1 %_12, label %bb2, label %bb49

bb49:                                             ; preds = %bb2, %bb1
  %_69 = load i64, ptr %sp, align 8
  %_68 = icmp ugt i64 %_69, 0
  br i1 %_68, label %bb50, label %bb51

bb2:                                              ; preds = %bb1
  %_15 = load i64, ptr %pc, align 8
  %_14 = icmp ult i64 %_15, 5
  br i1 %_14, label %bb3, label %bb49

bb3:                                              ; preds = %bb2
  %_18 = load i64, ptr %pc, align 8
  %_17 = icmp eq i64 %_18, 0
  br i1 %_17, label %bb4, label %bb5

bb51:                                             ; preds = %bb49
  store i64 0, ptr %_0, align 8
  br label %bb55

bb50:                                             ; preds = %bb49
  %_72 = load i64, ptr %sp, align 8
  %_73.0 = sub i64 %_72, 1
  %_73.1 = icmp ult i64 %_72, 1
  br i1 %_73.1, label %panic, label %bb52

bb55:                                             ; preds = %bb53, %bb54, %bb51
  %0 = load i64, ptr %_0, align 8
  ret i64 %0

bb52:                                             ; preds = %bb50
  %_70 = icmp eq i64 %_73.0, 0
  br i1 %_70, label %bb53, label %bb54

panic:                                            ; preds = %bb50
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_60eda4c9093e6cf6126592eba618b8ff) #3
  unreachable

bb54:                                             ; preds = %bb52
  %1 = load i64, ptr %s1, align 8
  store i64 %1, ptr %_0, align 8
  br label %bb55

bb53:                                             ; preds = %bb52
  %2 = load i64, ptr %s0, align 8
  store i64 %2, ptr %_0, align 8
  br label %bb55

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
  %_25 = icmp eq i64 %_26, 96
  br i1 %_25, label %bb13, label %bb29

bb29:                                             ; preds = %bb12
  %_45 = load i64, ptr %op, align 8
  %_44 = icmp eq i64 %_45, 1
  br i1 %_44, label %bb30, label %bb46

bb13:                                             ; preds = %bb12
  %3 = load i64, ptr %pc, align 8
  %_27.0 = add i64 %3, 1
  %_27.1 = icmp ult i64 %_27.0, %3
  br i1 %_27.1, label %panic5, label %bb14

bb46:                                             ; preds = %bb28, %bb23, %bb45, %bb30, %bb29
  %4 = load i64, ptr %pc, align 8
  %_66.0 = add i64 %4, 1
  %_66.1 = icmp ult i64 %_66.0, %4
  br i1 %_66.1, label %panic7, label %bb47

bb30:                                             ; preds = %bb29
  %_47 = load i64, ptr %sp, align 8
  %_46 = icmp uge i64 %_47, 2
  br i1 %_46, label %bb31, label %bb46

bb31:                                             ; preds = %bb30
  %_51 = load i64, ptr %sp, align 8
  %_52.0 = sub i64 %_51, 1
  %_52.1 = icmp ult i64 %_51, 1
  br i1 %_52.1, label %panic1, label %bb32

bb32:                                             ; preds = %bb31
  %_49 = icmp eq i64 %_52.0, 0
  br i1 %_49, label %bb33, label %bb34

panic1:                                           ; preds = %bb31
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_e28d5df1b873b139e24bf25f8cd528b7) #3
  unreachable

bb34:                                             ; preds = %bb32
  %5 = load i64, ptr %s1, align 8
  store i64 %5, ptr %a, align 8
  br label %bb35

bb33:                                             ; preds = %bb32
  %6 = load i64, ptr %s0, align 8
  store i64 %6, ptr %a, align 8
  br label %bb35

bb35:                                             ; preds = %bb33, %bb34
  %_56 = load i64, ptr %sp, align 8
  %_57.0 = sub i64 %_56, 2
  %_57.1 = icmp ult i64 %_56, 2
  br i1 %_57.1, label %panic2, label %bb36

bb36:                                             ; preds = %bb35
  %_54 = icmp eq i64 %_57.0, 0
  br i1 %_54, label %bb37, label %bb38

panic2:                                           ; preds = %bb35
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_8706287b454cf1d058e157719a7c99dc) #3
  unreachable

bb38:                                             ; preds = %bb36
  %7 = load i64, ptr %s1, align 8
  store i64 %7, ptr %b, align 8
  br label %bb39

bb37:                                             ; preds = %bb36
  %8 = load i64, ptr %s0, align 8
  store i64 %8, ptr %b, align 8
  br label %bb39

bb39:                                             ; preds = %bb37, %bb38
  %_59 = load i64, ptr %a, align 8
  %_60 = load i64, ptr %b, align 8
  %_0.i = add i64 %_59, %_60
  %_63 = load i64, ptr %sp, align 8
  %_64.0 = sub i64 %_63, 2
  %_64.1 = icmp ult i64 %_63, 2
  br i1 %_64.1, label %panic3, label %bb41

bb41:                                             ; preds = %bb39
  %_61 = icmp eq i64 %_64.0, 0
  br i1 %_61, label %bb42, label %bb43

panic3:                                           ; preds = %bb39
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_e89c74b9a276aeecc2a9ba810185f987) #3
  unreachable

bb43:                                             ; preds = %bb41
  store i64 %_0.i, ptr %s1, align 8
  br label %bb44

bb42:                                             ; preds = %bb41
  store i64 %_0.i, ptr %s0, align 8
  br label %bb44

bb44:                                             ; preds = %bb42, %bb43
  %9 = load i64, ptr %sp, align 8
  %_65.0 = sub i64 %9, 1
  %_65.1 = icmp ult i64 %9, 1
  br i1 %_65.1, label %panic4, label %bb45

bb45:                                             ; preds = %bb44
  store i64 %_65.0, ptr %sp, align 8
  br label %bb46

panic4:                                           ; preds = %bb44
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_fad86f0e941aabbb7dfabd775317c85a) #3
  unreachable

bb14:                                             ; preds = %bb13
  store i64 %_27.0, ptr %pc, align 8
  %_30 = load i64, ptr %pc, align 8
  %_29 = icmp eq i64 %_30, 0
  br i1 %_29, label %bb15, label %bb16

panic5:                                           ; preds = %bb13
; call core::panicking::panic_const::panic_const_add_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8 @alloc_b23e46e4090886bc04199447ce3334ec) #3
  unreachable

bb16:                                             ; preds = %bb14
  %_32 = load i64, ptr %pc, align 8
  %_31 = icmp eq i64 %_32, 1
  br i1 %_31, label %bb17, label %bb18

bb15:                                             ; preds = %bb14
  store i64 %code0, ptr %imm, align 8
  br label %bb23

bb18:                                             ; preds = %bb16
  %_34 = load i64, ptr %pc, align 8
  %_33 = icmp eq i64 %_34, 2
  br i1 %_33, label %bb19, label %bb20

bb17:                                             ; preds = %bb16
  store i64 %code1, ptr %imm, align 8
  br label %bb23

bb20:                                             ; preds = %bb18
  %_36 = load i64, ptr %pc, align 8
  %_35 = icmp eq i64 %_36, 3
  br i1 %_35, label %bb21, label %bb22

bb19:                                             ; preds = %bb18
  store i64 %code2, ptr %imm, align 8
  br label %bb23

bb22:                                             ; preds = %bb20
  store i64 %code4, ptr %imm, align 8
  br label %bb23

bb21:                                             ; preds = %bb20
  store i64 %code3, ptr %imm, align 8
  br label %bb23

bb23:                                             ; preds = %bb15, %bb17, %bb19, %bb21, %bb22
  %_38 = load i64, ptr %sp, align 8
  %_37 = icmp ult i64 %_38, 2
  br i1 %_37, label %bb24, label %bb46

bb24:                                             ; preds = %bb23
  %_40 = load i64, ptr %sp, align 8
  %_39 = icmp eq i64 %_40, 0
  br i1 %_39, label %bb25, label %bb26

bb26:                                             ; preds = %bb24
  %_42 = load i64, ptr %imm, align 8
  store i64 %_42, ptr %s1, align 8
  br label %bb27

bb25:                                             ; preds = %bb24
  %_41 = load i64, ptr %imm, align 8
  store i64 %_41, ptr %s0, align 8
  br label %bb27

bb27:                                             ; preds = %bb25, %bb26
  %10 = load i64, ptr %sp, align 8
  %_43.0 = add i64 %10, 1
  %_43.1 = icmp ult i64 %_43.0, %10
  br i1 %_43.1, label %panic6, label %bb28

bb28:                                             ; preds = %bb27
  store i64 %_43.0, ptr %sp, align 8
  br label %bb46

panic6:                                           ; preds = %bb27
; call core::panicking::panic_const::panic_const_add_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8 @alloc_b21b0ede86ab53924c64f326ae3ed7f4) #3
  unreachable

bb47:                                             ; preds = %bb46
  store i64 %_66.0, ptr %pc, align 8
  %11 = load i64, ptr %i, align 8
  %_67.0 = add i64 %11, 1
  %_67.1 = icmp ult i64 %_67.0, %11
  br i1 %_67.1, label %panic8, label %bb48

panic7:                                           ; preds = %bb46
; call core::panicking::panic_const::panic_const_add_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8 @alloc_5c1867983e2bc3b8ed32f672d648d8fe) #3
  unreachable

bb48:                                             ; preds = %bb47
  store i64 %_67.0, ptr %i, align 8
  br label %bb1

panic8:                                           ; preds = %bb47
; call core::panicking::panic_const::panic_const_add_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8 @alloc_435bd128cf3654ccbe35ee15f7923a47) #3
  unreachable
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

!0 = !{i64 5636739045759958119}
!1 = !{i64 1930548730910995847}
!2 = !{i64 -2205897672900745628}
!3 = !{i64 -1674242187402559334}
!4 = !{i64 -4965886949940600964}
!5 = !{i64 -8333845175478023537}
!6 = !{i64 -7588400688044955491}
!7 = !{i64 -7296574034564814726}
!8 = !{i64 -6649048754275838211}
!9 = !{i64 4356512189592633222}
!10 = !{i32 8, !"PIC Level", i32 2}
!11 = !{i32 7, !"uwtable", i32 2}
!12 = !{i32 7, !"frame-pointer", i32 1}
!13 = !{!"rustc version 1.99.0 (b940084d7 2026-09-28)"}
!14 = !{i64 877453575045248192}
!15 = !{i64 -5895172528724077534}
