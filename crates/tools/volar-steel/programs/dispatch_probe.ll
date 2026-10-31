; ModuleID = 'dispatch_probe.58be68a94be77794-cgu.0'
source_filename = "dispatch_probe.58be68a94be77794-cgu.0"
target datalayout = "e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-n32:64-S128-Fn32"
target triple = "arm64-apple-macosx11.0.0"

@BUF = global [32 x i8] zeroinitializer, align 4, !guid !0
@GPTR = global [8 x i8] zeroinitializer, align 8, !guid !1
@alloc_061320dd2889b91368ae98b317b26353 = private unnamed_addr constant [18 x i8] c"dispatch_probe.rs\00", align 1, !guid !2
@alloc_1e65f23673fec3a743d03dd04dea8881 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_061320dd2889b91368ae98b317b26353, [16 x i8] c"\11\00\00\00\00\00\00\00\13\00\00\00\13\00\00\00" }>, align 8, !guid !3

; Function Attrs: nounwind
define i32 @dispatch_probe(ptr %p, i32 %v, i32 %w) unnamed_addr #0 !guid !7 {
start:
  %local = alloca [4 x i8], align 4
  store i32 0, ptr %local, align 4
  store ptr %local, ptr @GPTR, align 8
  store i32 %v, ptr %p, align 4
  %_0.i = getelementptr inbounds i32, ptr %p, i64 1
  store i32 %w, ptr %_0.i, align 4
  %_9 = load i32, ptr %p, align 4
  %_11 = load i32, ptr %_0.i, align 4
  %_10 = shl i32 %_11, 1
  %_0.i1 = add i32 %_9, %_10
  ret i32 %_0.i1
}

attributes #0 = { nounwind "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }

!llvm.module.flags = !{!4, !5}
!llvm.ident = !{!6}

!0 = !{i64 -5073173232549912528}
!1 = !{i64 -119981791378981607}
!2 = !{i64 -1711000177620118226}
!3 = !{i64 -7018562041972609173}
!4 = !{i32 8, !"PIC Level", i32 2}
!5 = !{i32 7, !"frame-pointer", i32 1}
!6 = !{!"rustc version 1.99.0 (b940084d7 2026-09-28)"}
!7 = !{i64 -8747870787676758524}
