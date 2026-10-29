; ModuleID = 'v3.ee998e5f74152af8-cgu.0'
source_filename = "v3.ee998e5f74152af8-cgu.0"
target datalayout = "e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-n32:64-S128-Fn32"
target triple = "arm64-apple-macosx11.0.0"

@alloc_0bae5105ae32b39fa1ef2a0315840f16 = private unnamed_addr constant [6 x i8] c"v3.rs\00", align 1, !guid !0
@alloc_2da8bced4cf5a487e8489a5064808ba9 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_0bae5105ae32b39fa1ef2a0315840f16, [16 x i8] c"\05\00\00\00\00\00\00\00\12\00\00\00\0D\00\00\00" }>, align 8, !guid !1
@alloc_790ac407ca5d1133187b126290d0e2d2 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_0bae5105ae32b39fa1ef2a0315840f16, [16 x i8] c"\05\00\00\00\00\00\00\00\16\00\00\00\11\00\00\00" }>, align 8, !guid !2
@alloc_e3fbde9061f2ec2f9b8ec993911f214c = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_0bae5105ae32b39fa1ef2a0315840f16, [16 x i8] c"\05\00\00\00\00\00\00\00\19\00\00\00\09\00\00\00" }>, align 8, !guid !3
@alloc_8ef7bec5fff31ade8b5eb4e3b0036915 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_0bae5105ae32b39fa1ef2a0315840f16, [16 x i8] c"\05\00\00\00\00\00\00\00\1A\00\00\00\09\00\00\00" }>, align 8, !guid !4
@alloc_a7b97d1d4f9a6ee4171a602393ea0617 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_0bae5105ae32b39fa1ef2a0315840f16, [16 x i8] c"\05\00\00\00\00\00\00\00\1D\00\00\00\0C\00\00\00" }>, align 8, !guid !5

; __rustc::rust_begin_unwind
; Function Attrs: noreturn uwtable
define void @_RNvCsdtSMxSA38aw_7___rustc17rust_begin_unwind(ptr align 8 %_1) unnamed_addr #0 !guid !10 {
start:
  br label %bb1

bb1:                                              ; preds = %bb1, %start
  br label %bb1
}

; Function Attrs: uwtable
define i64 @step_v3(i64 %code0, i64 %code1, i64 %code2, i64 %code3, i64 %code4, i64 %steps) unnamed_addr #1 !guid !11 {
start:
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

bb1:                                              ; preds = %bb32, %start
  %_13 = load i64, ptr %i, align 8
  %_12 = icmp ult i64 %_13, %steps
  br i1 %_12, label %bb2, label %bb33

bb33:                                             ; preds = %bb12, %bb2, %bb1
  %_49 = load i64, ptr %sp, align 8
  %_48 = icmp ugt i64 %_49, 0
  br i1 %_48, label %bb34, label %bb38

bb2:                                              ; preds = %bb1
  %_15 = load i64, ptr %pc, align 8
  %_14 = icmp ult i64 %_15, 5
  br i1 %_14, label %bb3, label %bb33

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
  br i1 %_25, label %bb33, label %bb13

bb13:                                             ; preds = %bb12
  %_28 = load i64, ptr %op, align 8
  %_27 = icmp eq i64 %_28, 96
  br i1 %_27, label %bb14, label %bb30

bb30:                                             ; preds = %bb29, %bb24, %bb13
  %0 = load i64, ptr %pc, align 8
  %_46.0 = add i64 %0, 1
  %_46.1 = icmp ult i64 %_46.0, %0
  br i1 %_46.1, label %panic2, label %bb31

bb14:                                             ; preds = %bb13
  %1 = load i64, ptr %pc, align 8
  %_29.0 = add i64 %1, 1
  %_29.1 = icmp ult i64 %_29.0, %1
  br i1 %_29.1, label %panic, label %bb15

bb15:                                             ; preds = %bb14
  store i64 %_29.0, ptr %pc, align 8
  %_32 = load i64, ptr %pc, align 8
  %_31 = icmp eq i64 %_32, 0
  br i1 %_31, label %bb16, label %bb17

panic:                                            ; preds = %bb14
; call core::panicking::panic_const::panic_const_add_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8 @alloc_2da8bced4cf5a487e8489a5064808ba9) #3
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
  br i1 %_39, label %bb25, label %bb30

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
  %2 = load i64, ptr %sp, align 8
  %_45.0 = add i64 %2, 1
  %_45.1 = icmp ult i64 %_45.0, %2
  br i1 %_45.1, label %panic1, label %bb29

bb29:                                             ; preds = %bb28
  store i64 %_45.0, ptr %sp, align 8
  br label %bb30

panic1:                                           ; preds = %bb28
; call core::panicking::panic_const::panic_const_add_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8 @alloc_790ac407ca5d1133187b126290d0e2d2) #3
  unreachable

bb31:                                             ; preds = %bb30
  store i64 %_46.0, ptr %pc, align 8
  %3 = load i64, ptr %i, align 8
  %_47.0 = add i64 %3, 1
  %_47.1 = icmp ult i64 %_47.0, %3
  br i1 %_47.1, label %panic3, label %bb32

panic2:                                           ; preds = %bb30
; call core::panicking::panic_const::panic_const_add_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8 @alloc_e3fbde9061f2ec2f9b8ec993911f214c) #3
  unreachable

bb32:                                             ; preds = %bb31
  store i64 %_47.0, ptr %i, align 8
  br label %bb1

panic3:                                           ; preds = %bb31
; call core::panicking::panic_const::panic_const_add_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8 @alloc_8ef7bec5fff31ade8b5eb4e3b0036915) #3
  unreachable

bb38:                                             ; preds = %bb33
  store i64 0, ptr %_0, align 8
  br label %bb39

bb34:                                             ; preds = %bb33
  %_52 = load i64, ptr %sp, align 8
  %_53.0 = sub i64 %_52, 1
  %_53.1 = icmp ult i64 %_52, 1
  br i1 %_53.1, label %panic4, label %bb35

bb39:                                             ; preds = %bb36, %bb37, %bb38
  %4 = load i64, ptr %_0, align 8
  ret i64 %4

bb35:                                             ; preds = %bb34
  %_50 = icmp eq i64 %_53.0, 0
  br i1 %_50, label %bb36, label %bb37

panic4:                                           ; preds = %bb34
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_a7b97d1d4f9a6ee4171a602393ea0617) #3
  unreachable

bb37:                                             ; preds = %bb35
  %5 = load i64, ptr %s1, align 8
  store i64 %5, ptr %_0, align 8
  br label %bb39

bb36:                                             ; preds = %bb35
  %6 = load i64, ptr %s0, align 8
  store i64 %6, ptr %_0, align 8
  br label %bb39
}

; core::panicking::panic_const::panic_const_add_overflow
; Function Attrs: cold noinline noreturn uwtable
declare void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8) unnamed_addr #2

; core::panicking::panic_const::panic_const_sub_overflow
; Function Attrs: cold noinline noreturn uwtable
declare void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8) unnamed_addr #2

attributes #0 = { noreturn uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #1 = { uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #2 = { cold noinline noreturn uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #3 = { noinline noreturn }

!llvm.module.flags = !{!6, !7, !8}
!llvm.ident = !{!9}

!0 = !{i64 3880934834055491986}
!1 = !{i64 2620642863444342709}
!2 = !{i64 5973797941600011323}
!3 = !{i64 307327640192265603}
!4 = !{i64 -9018943054649131450}
!5 = !{i64 -766812685732651375}
!6 = !{i32 8, !"PIC Level", i32 2}
!7 = !{i32 7, !"uwtable", i32 2}
!8 = !{i32 7, !"frame-pointer", i32 1}
!9 = !{!"rustc version 1.99.0 (b940084d7 2026-09-28)"}
!10 = !{i64 877453575045248192}
!11 = !{i64 217234588272168725}
