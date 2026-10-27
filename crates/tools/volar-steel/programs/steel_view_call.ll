; ModuleID = 'steel_view_call.a52bb50323642b57-cgu.0'
source_filename = "steel_view_call.a52bb50323642b57-cgu.0"
target datalayout = "e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-n32:64-S128-Fn32"
target triple = "arm64-apple-macosx11.0.0"

; __rustc::rust_begin_unwind
; Function Attrs: noreturn uwtable
define void @_RNvCsdtSMxSA38aw_7___rustc17rust_begin_unwind(ptr align 8 %_1) unnamed_addr #0 !guid !4 {
start:
  br label %bb1

bb1:                                              ; preds = %bb1, %start
  br label %bb1
}

; steel_view_call::hash_pair
; Function Attrs: inlinehint uwtable
define internal i64 @_RNvCsebcovx7V7sF_15steel_view_call9hash_pairB1_(i64 %l, i64 %r) unnamed_addr #1 !guid !5 {
start:
  %x = alloca [8 x i8], align 8
  %_0.i1 = mul i64 %l, -7046029254386353131
  %0 = xor i64 %_0.i1, %r
  store i64 %0, ptr %x, align 8
  %_6 = load i64, ptr %x, align 8
  %_0.i = mul i64 %_6, -4658895280553007687
  store i64 %_0.i, ptr %x, align 8
  %_7 = load i64, ptr %x, align 8
  %_9 = load i64, ptr %x, align 8
  %_8 = lshr i64 %_9, 29
  %_0 = xor i64 %_7, %_8
  ret i64 %_0
}

; Function Attrs: uwtable
define i64 @steel_storage_read(i64 %key, i64 %value, i64 %n, i64 %path0, i64 %path1, i64 %path2, i64 %path3) unnamed_addr #2 !guid !6 {
start:
  %node = alloca [8 x i8], align 8
; call steel_view_call::hash_pair
  %0 = call i64 @_RNvCsebcovx7V7sF_15steel_view_call9hash_pairB1_(i64 %key, i64 %value) #3
  store i64 %0, ptr %node, align 8
  %_9 = icmp ugt i64 %n, 0
  br i1 %_9, label %bb2, label %bb4

bb4:                                              ; preds = %bb2, %start
  %_12 = icmp ugt i64 %n, 1
  br i1 %_12, label %bb5, label %bb7

bb2:                                              ; preds = %start
  %_11 = load i64, ptr %node, align 8
; call steel_view_call::hash_pair
  %_10 = call i64 @_RNvCsebcovx7V7sF_15steel_view_call9hash_pairB1_(i64 %_11, i64 %path0) #3
  store i64 %_10, ptr %node, align 8
  br label %bb4

bb7:                                              ; preds = %bb5, %bb4
  %_15 = icmp ugt i64 %n, 2
  br i1 %_15, label %bb8, label %bb10

bb5:                                              ; preds = %bb4
  %_14 = load i64, ptr %node, align 8
; call steel_view_call::hash_pair
  %_13 = call i64 @_RNvCsebcovx7V7sF_15steel_view_call9hash_pairB1_(i64 %_14, i64 %path1) #3
  store i64 %_13, ptr %node, align 8
  br label %bb7

bb10:                                             ; preds = %bb8, %bb7
  %_18 = icmp ugt i64 %n, 3
  br i1 %_18, label %bb11, label %bb13

bb8:                                              ; preds = %bb7
  %_17 = load i64, ptr %node, align 8
; call steel_view_call::hash_pair
  %_16 = call i64 @_RNvCsebcovx7V7sF_15steel_view_call9hash_pairB1_(i64 %_17, i64 %path2) #3
  store i64 %_16, ptr %node, align 8
  br label %bb10

bb13:                                             ; preds = %bb11, %bb10
  %_0 = load i64, ptr %node, align 8
  ret i64 %_0

bb11:                                             ; preds = %bb10
  %_20 = load i64, ptr %node, align 8
; call steel_view_call::hash_pair
  %_19 = call i64 @_RNvCsebcovx7V7sF_15steel_view_call9hash_pairB1_(i64 %_20, i64 %path3) #3
  store i64 %_19, ptr %node, align 8
  br label %bb13
}

attributes #0 = { noreturn uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #1 = { inlinehint uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #2 = { uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #3 = { inlinehint }

!llvm.module.flags = !{!0, !1, !2}
!llvm.ident = !{!3}

!0 = !{i32 8, !"PIC Level", i32 2}
!1 = !{i32 7, !"uwtable", i32 2}
!2 = !{i32 7, !"frame-pointer", i32 1}
!3 = !{!"rustc version 1.99.0 (b940084d7 2026-09-28)"}
!4 = !{i64 877453575045248192}
!5 = !{i64 -8411002683079271655}
!6 = !{i64 -3637969922422733815}
