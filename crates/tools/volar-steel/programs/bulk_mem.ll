; ModuleID = 'bulk_mem.91bef36245487f67-cgu.0'
source_filename = "bulk_mem.91bef36245487f67-cgu.0"
target datalayout = "e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-n32:64-S128-Fn32"
target triple = "arm64-apple-macosx11.0.0"

@alloc_e75cb0725e25eb16c7e1475458311733 = private unnamed_addr constant [12 x i8] c"bulk_mem.rs\00", align 1, !guid !0
@alloc_322b6ccc2e1a2f68a6da1c833910f73f = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_e75cb0725e25eb16c7e1475458311733, [16 x i8] c"\0B\00\00\00\00\00\00\00\14\00\00\00\12\00\00\00" }>, align 8, !guid !1

; Function Attrs: nounwind
define i32 @bulk_sum(i32 %sel) unnamed_addr #0 !guid !5 {
start:
  %b = alloca [300 x i8], align 1
  %a = alloca [300 x i8], align 1
  call void @llvm.memset.p0.i64(ptr align 1 %a, i8 0, i64 300, i1 false)
  call void @llvm.memset.p0.i64(ptr align 1 %b, i8 7, i64 300, i1 false)
  %_5 = and i32 %sel, 1
  %_4 = icmp ne i32 %_5, 0
  br i1 %_4, label %bb1, label %bb6

bb6:                                              ; preds = %bb1, %start
  %0 = getelementptr inbounds nuw i8, ptr %b, i64 0
  %_21 = load i8, ptr %0, align 1
  %_20 = zext i8 %_21 to i32
  %_19 = shl i32 %_20, 16
  %1 = getelementptr inbounds nuw i8, ptr %b, i64 1
  %_26 = load i8, ptr %1, align 1
  %_25 = zext i8 %_26 to i32
  %_24 = shl i32 %_25, 8
  %_18 = or i32 %_19, %_24
  %2 = getelementptr inbounds nuw i8, ptr %b, i64 299
  %_31 = load i8, ptr %2, align 1
  %_30 = zext i8 %_31 to i32
  %_29 = shl i32 %_30, 4
  %_17 = or i32 %_18, %_29
  %3 = getelementptr inbounds nuw i8, ptr %a, i64 1
  %_35 = load i8, ptr %3, align 1
  %_34 = zext i8 %_35 to i32
  %_0 = or i32 %_17, %_34
  ret i32 %_0

bb1:                                              ; preds = %start
  %4 = getelementptr inbounds nuw i8, ptr %a, i64 0
  store i8 3, ptr %4, align 1
  %5 = getelementptr inbounds nuw i8, ptr %a, i64 299
  store i8 5, ptr %5, align 1
  call void @llvm.memcpy.p0.p0.i64(ptr align 1 %b, ptr align 1 %a, i64 300, i1 false)
  br label %bb6
}

; Function Attrs: nocallback nofree nosync nounwind willreturn memory(argmem: readwrite)
declare void @llvm.memcpy.p0.p0.i64(ptr noalias writeonly captures(none), ptr noalias readonly captures(none), i64, i1 immarg) #1

; Function Attrs: nocallback nofree nosync nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr writeonly captures(none), i8, i64, i1 immarg) #2

attributes #0 = { nounwind "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #1 = { nocallback nofree nosync nounwind willreturn memory(argmem: readwrite) }
attributes #2 = { nocallback nofree nosync nounwind willreturn memory(argmem: write) }

!llvm.module.flags = !{!2, !3}
!llvm.ident = !{!4}

!0 = !{i64 7358285369854764296}
!1 = !{i64 -1210050471223849042}
!2 = !{i32 8, !"PIC Level", i32 2}
!3 = !{i32 7, !"frame-pointer", i32 1}
!4 = !{!"rustc version 1.99.0 (b940084d7 2026-09-28)"}
!5 = !{i64 1955057840911874546}
