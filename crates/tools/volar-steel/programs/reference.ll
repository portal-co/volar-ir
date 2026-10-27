; ModuleID = 'reference.8ccc9fe3a98130f4-cgu.0'
source_filename = "reference.8ccc9fe3a98130f4-cgu.0"
target datalayout = "e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-n32:64-S128-Fn32"
target triple = "arm64-apple-macosx11.0.0"

; Function Attrs: uwtable
define i64 @steel_view_call(i64 %key, i64 %value, i64 %n, i64 %path0, i64 %path1, i64 %path2, i64 %path3) unnamed_addr #0 !guid !4 {
start:
  %acc = alloca [8 x i8], align 8
  %0 = xor i64 %key, %value
  store i64 %0, ptr %acc, align 8
  %_9 = icmp ugt i64 %n, 0
  br i1 %_9, label %bb1, label %bb3

bb3:                                              ; preds = %bb1, %start
  %_12 = icmp ugt i64 %n, 1
  br i1 %_12, label %bb4, label %bb6

bb1:                                              ; preds = %start
  %_11 = load i64, ptr %acc, align 8
  %_0.i3 = mul i64 %_11, -7046029254386353131
  %1 = xor i64 %_0.i3, %path0
  store i64 %1, ptr %acc, align 8
  br label %bb3

bb6:                                              ; preds = %bb4, %bb3
  %_15 = icmp ugt i64 %n, 2
  br i1 %_15, label %bb7, label %bb9

bb4:                                              ; preds = %bb3
  %_14 = load i64, ptr %acc, align 8
  %_0.i2 = mul i64 %_14, -7046029254386353131
  %2 = xor i64 %_0.i2, %path1
  store i64 %2, ptr %acc, align 8
  br label %bb6

bb9:                                              ; preds = %bb7, %bb6
  %_18 = icmp ugt i64 %n, 3
  br i1 %_18, label %bb10, label %bb12

bb7:                                              ; preds = %bb6
  %_17 = load i64, ptr %acc, align 8
  %_0.i1 = mul i64 %_17, -7046029254386353131
  %3 = xor i64 %_0.i1, %path2
  store i64 %3, ptr %acc, align 8
  br label %bb9

bb12:                                             ; preds = %bb10, %bb9
  %_0 = load i64, ptr %acc, align 8
  ret i64 %_0

bb10:                                             ; preds = %bb9
  %_20 = load i64, ptr %acc, align 8
  %_0.i = mul i64 %_20, -7046029254386353131
  %4 = xor i64 %_0.i, %path3
  store i64 %4, ptr %acc, align 8
  br label %bb12
}

attributes #0 = { uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }

!llvm.module.flags = !{!0, !1, !2}
!llvm.ident = !{!3}

!0 = !{i32 8, !"PIC Level", i32 2}
!1 = !{i32 7, !"uwtable", i32 2}
!2 = !{i32 7, !"frame-pointer", i32 1}
!3 = !{!"rustc version 1.99.0 (b940084d7 2026-09-28)"}
!4 = !{i64 3290611648164888249}
