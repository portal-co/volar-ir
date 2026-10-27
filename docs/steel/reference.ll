; ModuleID = 'reference.8ccc9fe3a98130f4-cgu.0'
source_filename = "reference.8ccc9fe3a98130f4-cgu.0"
target datalayout = "e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-n32:64-S128-Fn32"
target triple = "arm64-apple-macosx11.0.0"

; __rustc::rust_begin_unwind
; Function Attrs: nofree norecurse noreturn nosync nounwind memory(none) uwtable
define void @_RNvCsdtSMxSA38aw_7___rustc17rust_begin_unwind(ptr noalias nofree noundef readonly align 8 captures(none) dereferenceable(24) %_1) unnamed_addr #0 !guid !4 {
start:
  br label %bb1

bb1:                                              ; preds = %bb1, %start
  br label %bb1
}

; Function Attrs: mustprogress nofree norecurse nosync nounwind willreturn memory(none) uwtable
define noundef i64 @steel_view_call(i64 noundef %key, i64 noundef %value, i64 noundef %path0, i64 noundef %path1, i64 noundef %path2, i64 noundef %path3) unnamed_addr #1 !guid !5 {
start:
  %0 = xor i64 %value, %key
  %_11 = mul i64 %0, -7046029254386353131
  %1 = xor i64 %path0, %_11
  %_11.1 = mul i64 %1, -7046029254386353131
  %2 = xor i64 %path1, %_11.1
  %_11.2 = mul i64 %2, -7046029254386353131
  %3 = xor i64 %path2, %_11.2
  %_11.3 = mul i64 %3, -7046029254386353131
  %4 = xor i64 %path3, %_11.3
  ret i64 %4
}

attributes #0 = { nofree norecurse noreturn nosync nounwind memory(none) uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #1 = { mustprogress nofree norecurse nosync nounwind willreturn memory(none) uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }

!llvm.module.flags = !{!0, !1, !2}
!llvm.ident = !{!3}

!0 = !{i32 8, !"PIC Level", i32 2}
!1 = !{i32 7, !"uwtable", i32 2}
!2 = !{i32 7, !"frame-pointer", i32 1}
!3 = !{!"rustc version 1.99.0 (b940084d7 2026-09-28)"}
!4 = !{i64 877453575045248192}
!5 = !{i64 3290611648164888249}
