; ModuleID = 'array_read.5eb9d443fd587df5-cgu.0'
source_filename = "array_read.5eb9d443fd587df5-cgu.0"
target datalayout = "e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-n32:64-S128-Fn32"
target triple = "arm64-apple-macosx11.0.0"

@alloc_1c0597dfce713536a1e1c5cbca2af567 = private unnamed_addr constant [14 x i8] c"array_read.rs\00", align 1, !guid !0
@alloc_49f5dac0243b429755767deb1bf03f51 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_1c0597dfce713536a1e1c5cbca2af567, [16 x i8] c"\0D\00\00\00\00\00\00\00\08\00\00\00\05\00\00\00" }>, align 8, !guid !1

; __rustc::rust_begin_unwind
; Function Attrs: noreturn uwtable
define void @_RNvCsdtSMxSA38aw_7___rustc17rust_begin_unwind(ptr align 8 %_1) unnamed_addr #0 !guid !6 {
start:
  br label %bb1

bb1:                                              ; preds = %bb1, %start
  br label %bb1
}

; Function Attrs: uwtable
define i64 @read_idx(i64 %a0, i64 %a1, i64 %a2, i64 %a3, i64 %idx) unnamed_addr #1 !guid !7 {
start:
  %arr = alloca [32 x i8], align 8
  %0 = getelementptr inbounds nuw i64, ptr %arr, i64 0
  store i64 %a0, ptr %0, align 8
  %1 = getelementptr inbounds nuw i64, ptr %arr, i64 1
  store i64 %a1, ptr %1, align 8
  %2 = getelementptr inbounds nuw i64, ptr %arr, i64 2
  store i64 %a2, ptr %2, align 8
  %3 = getelementptr inbounds nuw i64, ptr %arr, i64 3
  store i64 %a3, ptr %3, align 8
  %_8 = and i64 %idx, 3
  %_9 = icmp ult i64 %_8, 4
  br i1 %_9, label %bb1, label %panic

bb1:                                              ; preds = %start
  %4 = getelementptr inbounds nuw i64, ptr %arr, i64 %_8
  %_0 = load i64, ptr %4, align 8
  ret i64 %_0

panic:                                            ; preds = %start
; call core::panicking::panic_bounds_check
  call void @_RNvNtCsjIpoRzUjyHQ_4core9panicking18panic_bounds_check(i64 %_8, i64 4, ptr align 8 @alloc_49f5dac0243b429755767deb1bf03f51) #3
  unreachable
}

; core::panicking::panic_bounds_check
; Function Attrs: cold minsize noinline noreturn optsize uwtable
declare void @_RNvNtCsjIpoRzUjyHQ_4core9panicking18panic_bounds_check(i64, i64, ptr align 8) unnamed_addr #2

attributes #0 = { noreturn uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #1 = { uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #2 = { cold minsize noinline noreturn optsize uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #3 = { noinline noreturn }

!llvm.module.flags = !{!2, !3, !4}
!llvm.ident = !{!5}

!0 = !{i64 -1609708361930140441}
!1 = !{i64 5812849555661930555}
!2 = !{i32 8, !"PIC Level", i32 2}
!3 = !{i32 7, !"uwtable", i32 2}
!4 = !{i32 7, !"frame-pointer", i32 1}
!5 = !{!"rustc version 1.99.0 (b940084d7 2026-09-28)"}
!6 = !{i64 877453575045248192}
!7 = !{i64 3802821463023770324}
