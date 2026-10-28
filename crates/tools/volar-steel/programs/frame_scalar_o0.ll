; ModuleID = 'frame_scalar.5efb8ade2a0a4ae6-cgu.0'
source_filename = "frame_scalar.5efb8ade2a0a4ae6-cgu.0"
target datalayout = "e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-n32:64-S128-Fn32"
target triple = "arm64-apple-macosx11.0.0"

@alloc_afd00322150f667bdc3e87dde28b9cde = private unnamed_addr constant [16 x i8] c"frame_scalar.rs\00", align 1, !guid !0
@alloc_f309084a0b7f6a7ebe644243998058d2 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_afd00322150f667bdc3e87dde28b9cde, [16 x i8] c"\0F\00\00\00\00\00\00\00\0E\00\00\00 \00\00\00" }>, align 8, !guid !1
@alloc_1a9378752a0fa4507b6593f84700d3e9 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_afd00322150f667bdc3e87dde28b9cde, [16 x i8] c"\0F\00\00\00\00\00\00\00\0F\00\00\00\09\00\00\00" }>, align 8, !guid !2

; __rustc::rust_begin_unwind
; Function Attrs: noreturn uwtable
define void @_RNvCsdtSMxSA38aw_7___rustc17rust_begin_unwind(ptr align 8 %_1) unnamed_addr #0 !guid !7 {
start:
  br label %bb1

bb1:                                              ; preds = %bb1, %start
  br label %bb1
}

; Function Attrs: uwtable
define i64 @sum_with_array(i64 %a0, i64 %a1, i64 %n) unnamed_addr #1 !guid !8 {
start:
  %acc = alloca [8 x i8], align 8
  %i = alloca [8 x i8], align 8
  %arr = alloca [16 x i8], align 8
  %0 = getelementptr inbounds nuw i64, ptr %arr, i64 0
  store i64 %a0, ptr %0, align 8
  %1 = getelementptr inbounds nuw i64, ptr %arr, i64 1
  store i64 %a1, ptr %1, align 8
  store i64 0, ptr %i, align 8
  store i64 0, ptr %acc, align 8
  br label %bb1

bb1:                                              ; preds = %bb5, %start
  %_8 = load i64, ptr %i, align 8
  %_7 = icmp ult i64 %_8, %n
  br i1 %_7, label %bb2, label %bb6

bb6:                                              ; preds = %bb1
  %_0 = load i64, ptr %acc, align 8
  ret i64 %_0

bb2:                                              ; preds = %bb1
  %_10 = load i64, ptr %acc, align 8
  %_14 = load i64, ptr %i, align 8
  %_13 = and i64 %_14, 1
  %_15 = icmp ult i64 %_13, 2
  br i1 %_15, label %bb3, label %panic

bb3:                                              ; preds = %bb2
  %2 = getelementptr inbounds nuw i64, ptr %arr, i64 %_13
  %_11 = load i64, ptr %2, align 8
  %_0.i = add i64 %_10, %_11
  store i64 %_0.i, ptr %acc, align 8
  %3 = load i64, ptr %i, align 8
  %_16.0 = add i64 %3, 1
  %_16.1 = icmp ult i64 %_16.0, %3
  br i1 %_16.1, label %panic1, label %bb5

panic:                                            ; preds = %bb2
; call core::panicking::panic_bounds_check
  call void @_RNvNtCsjIpoRzUjyHQ_4core9panicking18panic_bounds_check(i64 %_13, i64 2, ptr align 8 @alloc_f309084a0b7f6a7ebe644243998058d2) #4
  unreachable

bb5:                                              ; preds = %bb3
  store i64 %_16.0, ptr %i, align 8
  br label %bb1

panic1:                                           ; preds = %bb3
; call core::panicking::panic_const::panic_const_add_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8 @alloc_1a9378752a0fa4507b6593f84700d3e9) #4
  unreachable
}

; core::panicking::panic_bounds_check
; Function Attrs: cold minsize noinline noreturn optsize uwtable
declare void @_RNvNtCsjIpoRzUjyHQ_4core9panicking18panic_bounds_check(i64, i64, ptr align 8) unnamed_addr #2

; core::panicking::panic_const::panic_const_add_overflow
; Function Attrs: cold noinline noreturn uwtable
declare void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8) unnamed_addr #3

attributes #0 = { noreturn uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #1 = { uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #2 = { cold minsize noinline noreturn optsize uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #3 = { cold noinline noreturn uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #4 = { noinline noreturn }

!llvm.module.flags = !{!3, !4, !5}
!llvm.ident = !{!6}

!0 = !{i64 -8523453074821844618}
!1 = !{i64 -5999999426334702610}
!2 = !{i64 -4830425204707567867}
!3 = !{i32 8, !"PIC Level", i32 2}
!4 = !{i32 7, !"uwtable", i32 2}
!5 = !{i32 7, !"frame-pointer", i32 1}
!6 = !{!"rustc version 1.99.0 (b940084d7 2026-09-28)"}
!7 = !{i64 877453575045248192}
!8 = !{i64 4713771214125645659}
