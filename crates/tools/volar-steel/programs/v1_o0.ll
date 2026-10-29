; ModuleID = 'v1.a7bc6f7a6a163c68-cgu.0'
source_filename = "v1.a7bc6f7a6a163c68-cgu.0"
target datalayout = "e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-n32:64-S128-Fn32"
target triple = "arm64-apple-macosx11.0.0"

@alloc_5d96832baced7f22fbf37b074be75bfd = private unnamed_addr constant [6 x i8] c"v1.rs\00", align 1, !guid !0
@alloc_bee89a3a825afe4499e20d4fea5dffe0 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_5d96832baced7f22fbf37b074be75bfd, [16 x i8] c"\05\00\00\00\00\00\00\00\1B\00\00\00\0C\00\00\00" }>, align 8, !guid !1
@alloc_a57215b1be25b7366e370a50c6ac7c3d = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_5d96832baced7f22fbf37b074be75bfd, [16 x i8] c"\05\00\00\00\00\00\00\00\10\00\00\00\0D\00\00\00" }>, align 8, !guid !2
@alloc_b1a9e53e12ae253132bdc5898cde70af = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_5d96832baced7f22fbf37b074be75bfd, [16 x i8] c"\05\00\00\00\00\00\00\00\14\00\00\00\11\00\00\00" }>, align 8, !guid !3
@alloc_d728d924b191ec70eeba0bf55dd2941a = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_5d96832baced7f22fbf37b074be75bfd, [16 x i8] c"\05\00\00\00\00\00\00\00\17\00\00\00\09\00\00\00" }>, align 8, !guid !4
@alloc_f18557dca145a053dfb49fdc1e400418 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_5d96832baced7f22fbf37b074be75bfd, [16 x i8] c"\05\00\00\00\00\00\00\00\18\00\00\00\09\00\00\00" }>, align 8, !guid !5

; __rustc::rust_begin_unwind
; Function Attrs: noreturn uwtable
define void @_RNvCsdtSMxSA38aw_7___rustc17rust_begin_unwind(ptr align 8 %_1) unnamed_addr #0 !guid !10 {
start:
  br label %bb1

bb1:                                              ; preds = %bb1, %start
  br label %bb1
}

; Function Attrs: uwtable
define i64 @step_v1(i64 %code0, i64 %code1, i64 %code2, i64 %code3, i64 %code4, i64 %steps) unnamed_addr #1 !guid !11 {
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

bb1:                                              ; preds = %bb31, %start
  %_13 = load i64, ptr %i, align 8
  %_12 = icmp ult i64 %_13, %steps
  br i1 %_12, label %bb2, label %bb32

bb32:                                             ; preds = %bb2, %bb1
  %_47 = load i64, ptr %sp, align 8
  %_46 = icmp ugt i64 %_47, 0
  br i1 %_46, label %bb33, label %bb34

bb2:                                              ; preds = %bb1
  %_15 = load i64, ptr %pc, align 8
  %_14 = icmp ult i64 %_15, 5
  br i1 %_14, label %bb3, label %bb32

bb3:                                              ; preds = %bb2
  %_18 = load i64, ptr %pc, align 8
  %_17 = icmp eq i64 %_18, 0
  br i1 %_17, label %bb4, label %bb5

bb34:                                             ; preds = %bb32
  store i64 0, ptr %_0, align 8
  br label %bb38

bb33:                                             ; preds = %bb32
  %_50 = load i64, ptr %sp, align 8
  %_51.0 = sub i64 %_50, 1
  %_51.1 = icmp ult i64 %_50, 1
  br i1 %_51.1, label %panic, label %bb35

bb38:                                             ; preds = %bb36, %bb37, %bb34
  %0 = load i64, ptr %_0, align 8
  ret i64 %0

bb35:                                             ; preds = %bb33
  %_48 = icmp eq i64 %_51.0, 0
  br i1 %_48, label %bb36, label %bb37

panic:                                            ; preds = %bb33
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_bee89a3a825afe4499e20d4fea5dffe0) #3
  unreachable

bb37:                                             ; preds = %bb35
  %1 = load i64, ptr %s1, align 8
  store i64 %1, ptr %_0, align 8
  br label %bb38

bb36:                                             ; preds = %bb35
  %2 = load i64, ptr %s0, align 8
  store i64 %2, ptr %_0, align 8
  br label %bb38

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

bb29:                                             ; preds = %bb28, %bb23, %bb12
  %3 = load i64, ptr %pc, align 8
  %_44.0 = add i64 %3, 1
  %_44.1 = icmp ult i64 %_44.0, %3
  br i1 %_44.1, label %panic3, label %bb30

bb13:                                             ; preds = %bb12
  %4 = load i64, ptr %pc, align 8
  %_27.0 = add i64 %4, 1
  %_27.1 = icmp ult i64 %_27.0, %4
  br i1 %_27.1, label %panic1, label %bb14

bb14:                                             ; preds = %bb13
  store i64 %_27.0, ptr %pc, align 8
  %_30 = load i64, ptr %pc, align 8
  %_29 = icmp eq i64 %_30, 0
  br i1 %_29, label %bb15, label %bb16

panic1:                                           ; preds = %bb13
; call core::panicking::panic_const::panic_const_add_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8 @alloc_a57215b1be25b7366e370a50c6ac7c3d) #3
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
  br i1 %_37, label %bb24, label %bb29

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
  %5 = load i64, ptr %sp, align 8
  %_43.0 = add i64 %5, 1
  %_43.1 = icmp ult i64 %_43.0, %5
  br i1 %_43.1, label %panic2, label %bb28

bb28:                                             ; preds = %bb27
  store i64 %_43.0, ptr %sp, align 8
  br label %bb29

panic2:                                           ; preds = %bb27
; call core::panicking::panic_const::panic_const_add_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8 @alloc_b1a9e53e12ae253132bdc5898cde70af) #3
  unreachable

bb30:                                             ; preds = %bb29
  store i64 %_44.0, ptr %pc, align 8
  %6 = load i64, ptr %i, align 8
  %_45.0 = add i64 %6, 1
  %_45.1 = icmp ult i64 %_45.0, %6
  br i1 %_45.1, label %panic4, label %bb31

panic3:                                           ; preds = %bb29
; call core::panicking::panic_const::panic_const_add_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8 @alloc_d728d924b191ec70eeba0bf55dd2941a) #3
  unreachable

bb31:                                             ; preds = %bb30
  store i64 %_45.0, ptr %i, align 8
  br label %bb1

panic4:                                           ; preds = %bb30
; call core::panicking::panic_const::panic_const_add_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8 @alloc_f18557dca145a053dfb49fdc1e400418) #3
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

!llvm.module.flags = !{!6, !7, !8}
!llvm.ident = !{!9}

!0 = !{i64 -1183276740198061028}
!1 = !{i64 2845392917349014524}
!2 = !{i64 5303920330937954907}
!3 = !{i64 6961524516723119756}
!4 = !{i64 -4865080271113187488}
!5 = !{i64 4466610806344292625}
!6 = !{i32 8, !"PIC Level", i32 2}
!7 = !{i32 7, !"uwtable", i32 2}
!8 = !{i32 7, !"frame-pointer", i32 1}
!9 = !{!"rustc version 1.99.0 (b940084d7 2026-09-28)"}
!10 = !{i64 877453575045248192}
!11 = !{i64 506957787090284996}
