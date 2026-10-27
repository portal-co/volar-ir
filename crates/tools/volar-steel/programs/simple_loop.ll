; ModuleID = 'simple_loop.8052e4c96d10e0e3-cgu.0'
source_filename = "simple_loop.8052e4c96d10e0e3-cgu.0"
target datalayout = "e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-n32:64-S128-Fn32"
target triple = "arm64-apple-macosx11.0.0"

@alloc_2498b878eeedf3c7f52d4bf6624da980 = private unnamed_addr constant [15 x i8] c"simple_loop.rs\00", align 1, !guid !0
@alloc_75263526c284aab0b5b35f9707e96201 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_2498b878eeedf3c7f52d4bf6624da980, [16 x i8] c"\0E\00\00\00\00\00\00\00\0A\00\00\00\09\00\00\00" }>, align 8, !guid !1

; __rustc::rust_begin_unwind
; Function Attrs: noreturn uwtable
define void @_RNvCsdtSMxSA38aw_7___rustc17rust_begin_unwind(ptr align 8 %_1) unnamed_addr #0 !guid !6 {
start:
  br label %bb1

bb1:                                              ; preds = %bb1, %start
  br label %bb1
}

; Function Attrs: uwtable
define i64 @count_to(i64 %n) unnamed_addr #1 !guid !7 {
start:
  %acc = alloca [8 x i8], align 8
  %i = alloca [8 x i8], align 8
  store i64 0, ptr %i, align 8
  store i64 0, ptr %acc, align 8
  br label %bb1

bb1:                                              ; preds = %bb4, %start
  %_5 = load i64, ptr %i, align 8
  %_4 = icmp ult i64 %_5, %n
  br i1 %_4, label %bb2, label %bb5

bb5:                                              ; preds = %bb1
  %_0 = load i64, ptr %acc, align 8
  ret i64 %_0

bb2:                                              ; preds = %bb1
  %_7 = load i64, ptr %acc, align 8
  %_8 = load i64, ptr %i, align 8
  %_0.i = add i64 %_7, %_8
  store i64 %_0.i, ptr %acc, align 8
  %0 = load i64, ptr %i, align 8
  %_9.0 = add i64 %0, 1
  %_9.1 = icmp ult i64 %_9.0, %0
  br i1 %_9.1, label %panic, label %bb4

bb4:                                              ; preds = %bb2
  store i64 %_9.0, ptr %i, align 8
  br label %bb1

panic:                                            ; preds = %bb2
; call core::panicking::panic_const::panic_const_add_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8 @alloc_75263526c284aab0b5b35f9707e96201) #3
  unreachable
}

; core::panicking::panic_const::panic_const_add_overflow
; Function Attrs: cold noinline noreturn uwtable
declare void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8) unnamed_addr #2

attributes #0 = { noreturn uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #1 = { uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #2 = { cold noinline noreturn uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #3 = { noinline noreturn }

!llvm.module.flags = !{!2, !3, !4}
!llvm.ident = !{!5}

!0 = !{i64 8718311474104381416}
!1 = !{i64 9123253756760950551}
!2 = !{i32 8, !"PIC Level", i32 2}
!3 = !{i32 7, !"uwtable", i32 2}
!4 = !{i32 7, !"frame-pointer", i32 1}
!5 = !{!"rustc version 1.99.0 (b940084d7 2026-09-28)"}
!6 = !{i64 877453575045248192}
!7 = !{i64 274513090655881413}
