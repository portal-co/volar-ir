; ModuleID = 'loop_array.359e074cfa780b52-cgu.0'
source_filename = "loop_array.359e074cfa780b52-cgu.0"
target datalayout = "e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-n32:64-S128-Fn32"
target triple = "arm64-apple-macosx11.0.0"

@alloc_2b7c69e99a4341e0ee12e0416e3d633f = private unnamed_addr constant [14 x i8] c"loop_array.rs\00", align 1, !guid !0
@alloc_5151db52d4770f08359fb98477ed6f90 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_2b7c69e99a4341e0ee12e0416e3d633f, [16 x i8] c"\0D\00\00\00\00\00\00\00\13\00\00\00\1F\00\00\00" }>, align 8, !guid !1
@alloc_8742e2f292bd96576091bb40ca04bcbb = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_2b7c69e99a4341e0ee12e0416e3d633f, [16 x i8] c"\0D\00\00\00\00\00\00\00\13\00\00\00\19\00\00\00" }>, align 8, !guid !2
@alloc_adda0e24c5e06d82f33664f283c0399a = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_2b7c69e99a4341e0ee12e0416e3d633f, [16 x i8] c"\0D\00\00\00\00\00\00\00\13\00\00\00:\00\00\00" }>, align 8, !guid !3
@alloc_4d8de201e099dac3eea8aa47d6461b33 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_2b7c69e99a4341e0ee12e0416e3d633f, [16 x i8] c"\0D\00\00\00\00\00\00\00\13\00\00\004\00\00\00" }>, align 8, !guid !4
@alloc_4bc0c13fee63dfda6ff446a35cb648c9 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_2b7c69e99a4341e0ee12e0416e3d633f, [16 x i8] c"\0D\00\00\00\00\00\00\00\13\00\00\00\0F\00\00\00" }>, align 8, !guid !5
@alloc_60958747f8334949dbadb110d4f901c1 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_2b7c69e99a4341e0ee12e0416e3d633f, [16 x i8] c"\0D\00\00\00\00\00\00\00\13\00\00\00\09\00\00\00" }>, align 8, !guid !6
@alloc_e77caedfacdec47028af96ca5520e22e = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_2b7c69e99a4341e0ee12e0416e3d633f, [16 x i8] c"\0D\00\00\00\00\00\00\00\14\00\00\00\09\00\00\00" }>, align 8, !guid !7
@alloc_d193f2a2843412cd1d6cf91e839d5758 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_2b7c69e99a4341e0ee12e0416e3d633f, [16 x i8] c"\0D\00\00\00\00\00\00\00\17\00\00\00\0F\00\00\00" }>, align 8, !guid !8
@alloc_355d2e4a5a86354f795c7957c3ef4db8 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_2b7c69e99a4341e0ee12e0416e3d633f, [16 x i8] c"\0D\00\00\00\00\00\00\00\17\00\00\00\09\00\00\00" }>, align 8, !guid !9
@alloc_dee5dd8745ea5c35fb12d6d68e01fb9d = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_2b7c69e99a4341e0ee12e0416e3d633f, [16 x i8] c"\0D\00\00\00\00\00\00\00\0D\00\00\00\0D\00\00\00" }>, align 8, !guid !10
@alloc_ab80fe78a3597fdaba1a91d5fca4ddf5 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_2b7c69e99a4341e0ee12e0416e3d633f, [16 x i8] c"\0D\00\00\00\00\00\00\00\0E\00\00\00\0D\00\00\00" }>, align 8, !guid !11
@alloc_b571a06b98b22c34345c436af2552a43 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_2b7c69e99a4341e0ee12e0416e3d633f, [16 x i8] c"\0D\00\00\00\00\00\00\00\10\00\00\00\09\00\00\00" }>, align 8, !guid !12

; __rustc::rust_begin_unwind
; Function Attrs: noreturn uwtable
define void @_RNvCsdtSMxSA38aw_7___rustc17rust_begin_unwind(ptr align 8 %_1) unnamed_addr #0 !guid !17 {
start:
  br label %bb1

bb1:                                              ; preds = %bb1, %start
  br label %bb1
}

; Function Attrs: uwtable
define i64 @push_pop(i64 %a, i64 %b) unnamed_addr #1 !guid !18 {
start:
  %_10 = alloca [8 x i8], align 8
  %i = alloca [8 x i8], align 8
  %sp = alloca [8 x i8], align 8
  %stack = alloca [32 x i8], align 8
  %_0 = alloca [8 x i8], align 8
  call void @llvm.memset.p0.i64(ptr align 8 %stack, i8 0, i64 32, i1 false)
  store i64 0, ptr %sp, align 8
  store i64 0, ptr %i, align 8
  br label %bb1

bb1:                                              ; preds = %bb10, %start
  %_7 = load i64, ptr %i, align 8
  %_6 = icmp ult i64 %_7, 2
  br i1 %_6, label %bb2, label %bb11

bb11:                                             ; preds = %bb1
  %_18 = load i64, ptr %sp, align 8
  %_17 = icmp uge i64 %_18, 2
  br i1 %_17, label %bb12, label %bb21

bb2:                                              ; preds = %bb1
  %_9 = load i64, ptr %sp, align 8
  %_8 = icmp ult i64 %_9, 4
  br i1 %_8, label %bb3, label %bb9

bb21:                                             ; preds = %bb20, %bb11
  %_36 = load i64, ptr %sp, align 8
  %_35 = icmp ugt i64 %_36, 0
  br i1 %_35, label %bb22, label %bb25

bb12:                                             ; preds = %bb11
  %_22 = load i64, ptr %sp, align 8
  %_23.0 = sub i64 %_22, 1
  %_23.1 = icmp ult i64 %_22, 1
  br i1 %_23.1, label %panic, label %bb13

bb13:                                             ; preds = %bb12
  %_24 = icmp ult i64 %_23.0, 4
  br i1 %_24, label %bb14, label %panic1

panic:                                            ; preds = %bb12
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_5151db52d4770f08359fb98477ed6f90) #5
  unreachable

bb14:                                             ; preds = %bb13
  %0 = getelementptr inbounds nuw i64, ptr %stack, i64 %_23.0
  %_20 = load i64, ptr %0, align 8
  %_27 = load i64, ptr %sp, align 8
  %_28.0 = sub i64 %_27, 2
  %_28.1 = icmp ult i64 %_27, 2
  br i1 %_28.1, label %panic2, label %bb15

panic1:                                           ; preds = %bb13
; call core::panicking::panic_bounds_check
  call void @_RNvNtCsjIpoRzUjyHQ_4core9panicking18panic_bounds_check(i64 %_23.0, i64 4, ptr align 8 @alloc_8742e2f292bd96576091bb40ca04bcbb) #5
  unreachable

bb15:                                             ; preds = %bb14
  %_29 = icmp ult i64 %_28.0, 4
  br i1 %_29, label %bb16, label %panic3

panic2:                                           ; preds = %bb14
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_adda0e24c5e06d82f33664f283c0399a) #5
  unreachable

bb16:                                             ; preds = %bb15
  %1 = getelementptr inbounds nuw i64, ptr %stack, i64 %_28.0
  %_25 = load i64, ptr %1, align 8
  %_0.i = add i64 %_20, %_25
  %_31 = load i64, ptr %sp, align 8
  %_32.0 = sub i64 %_31, 2
  %_32.1 = icmp ult i64 %_31, 2
  br i1 %_32.1, label %panic4, label %bb18

panic3:                                           ; preds = %bb15
; call core::panicking::panic_bounds_check
  call void @_RNvNtCsjIpoRzUjyHQ_4core9panicking18panic_bounds_check(i64 %_28.0, i64 4, ptr align 8 @alloc_4d8de201e099dac3eea8aa47d6461b33) #5
  unreachable

bb18:                                             ; preds = %bb16
  %_33 = icmp ult i64 %_32.0, 4
  br i1 %_33, label %bb19, label %panic5

panic4:                                           ; preds = %bb16
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_4bc0c13fee63dfda6ff446a35cb648c9) #5
  unreachable

bb19:                                             ; preds = %bb18
  %2 = getelementptr inbounds nuw i64, ptr %stack, i64 %_32.0
  store i64 %_0.i, ptr %2, align 8
  %3 = load i64, ptr %sp, align 8
  %_34.0 = sub i64 %3, 1
  %_34.1 = icmp ult i64 %3, 1
  br i1 %_34.1, label %panic6, label %bb20

panic5:                                           ; preds = %bb18
; call core::panicking::panic_bounds_check
  call void @_RNvNtCsjIpoRzUjyHQ_4core9panicking18panic_bounds_check(i64 %_32.0, i64 4, ptr align 8 @alloc_60958747f8334949dbadb110d4f901c1) #5
  unreachable

bb20:                                             ; preds = %bb19
  store i64 %_34.0, ptr %sp, align 8
  br label %bb21

panic6:                                           ; preds = %bb19
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_e77caedfacdec47028af96ca5520e22e) #5
  unreachable

bb25:                                             ; preds = %bb21
  store i64 0, ptr %_0, align 8
  br label %bb26

bb22:                                             ; preds = %bb21
  %_38 = load i64, ptr %sp, align 8
  %_39.0 = sub i64 %_38, 1
  %_39.1 = icmp ult i64 %_38, 1
  br i1 %_39.1, label %panic7, label %bb23

bb26:                                             ; preds = %bb24, %bb25
  %4 = load i64, ptr %_0, align 8
  ret i64 %4

bb23:                                             ; preds = %bb22
  %_40 = icmp ult i64 %_39.0, 4
  br i1 %_40, label %bb24, label %panic8

panic7:                                           ; preds = %bb22
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_d193f2a2843412cd1d6cf91e839d5758) #5
  unreachable

bb24:                                             ; preds = %bb23
  %5 = getelementptr inbounds nuw i64, ptr %stack, i64 %_39.0
  %6 = load i64, ptr %5, align 8
  store i64 %6, ptr %_0, align 8
  br label %bb26

panic8:                                           ; preds = %bb23
; call core::panicking::panic_bounds_check
  call void @_RNvNtCsjIpoRzUjyHQ_4core9panicking18panic_bounds_check(i64 %_39.0, i64 4, ptr align 8 @alloc_355d2e4a5a86354f795c7957c3ef4db8) #5
  unreachable

bb9:                                              ; preds = %bb8, %bb2
  %7 = load i64, ptr %i, align 8
  %_16.0 = add i64 %7, 1
  %_16.1 = icmp ult i64 %_16.0, %7
  br i1 %_16.1, label %panic11, label %bb10

bb3:                                              ; preds = %bb2
  %_12 = load i64, ptr %i, align 8
  %_11 = icmp eq i64 %_12, 0
  br i1 %_11, label %bb4, label %bb5

bb5:                                              ; preds = %bb3
  store i64 %b, ptr %_10, align 8
  br label %bb6

bb4:                                              ; preds = %bb3
  store i64 %a, ptr %_10, align 8
  br label %bb6

bb6:                                              ; preds = %bb4, %bb5
  %_13 = load i64, ptr %sp, align 8
  %_14 = icmp ult i64 %_13, 4
  br i1 %_14, label %bb7, label %panic9

bb7:                                              ; preds = %bb6
  %8 = getelementptr inbounds nuw i64, ptr %stack, i64 %_13
  %9 = load i64, ptr %_10, align 8
  store i64 %9, ptr %8, align 8
  %10 = load i64, ptr %sp, align 8
  %_15.0 = add i64 %10, 1
  %_15.1 = icmp ult i64 %_15.0, %10
  br i1 %_15.1, label %panic10, label %bb8

panic9:                                           ; preds = %bb6
; call core::panicking::panic_bounds_check
  call void @_RNvNtCsjIpoRzUjyHQ_4core9panicking18panic_bounds_check(i64 %_13, i64 4, ptr align 8 @alloc_dee5dd8745ea5c35fb12d6d68e01fb9d) #5
  unreachable

bb8:                                              ; preds = %bb7
  store i64 %_15.0, ptr %sp, align 8
  br label %bb9

panic10:                                          ; preds = %bb7
; call core::panicking::panic_const::panic_const_add_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8 @alloc_ab80fe78a3597fdaba1a91d5fca4ddf5) #5
  unreachable

bb10:                                             ; preds = %bb9
  store i64 %_16.0, ptr %i, align 8
  br label %bb1

panic11:                                          ; preds = %bb9
; call core::panicking::panic_const::panic_const_add_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8 @alloc_b571a06b98b22c34345c436af2552a43) #5
  unreachable
}

; Function Attrs: nocallback nofree nosync nounwind willreturn memory(argmem: write)
declare void @llvm.memset.p0.i64(ptr writeonly captures(none), i8, i64, i1 immarg) #2

; core::panicking::panic_const::panic_const_sub_overflow
; Function Attrs: cold noinline noreturn uwtable
declare void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8) unnamed_addr #3

; core::panicking::panic_bounds_check
; Function Attrs: cold minsize noinline noreturn optsize uwtable
declare void @_RNvNtCsjIpoRzUjyHQ_4core9panicking18panic_bounds_check(i64, i64, ptr align 8) unnamed_addr #4

; core::panicking::panic_const::panic_const_add_overflow
; Function Attrs: cold noinline noreturn uwtable
declare void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8) unnamed_addr #3

attributes #0 = { noreturn uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #1 = { uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #2 = { nocallback nofree nosync nounwind willreturn memory(argmem: write) }
attributes #3 = { cold noinline noreturn uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #4 = { cold minsize noinline noreturn optsize uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #5 = { noinline noreturn }

!llvm.module.flags = !{!13, !14, !15}
!llvm.ident = !{!16}

!0 = !{i64 2329269953131209411}
!1 = !{i64 710853068163628818}
!2 = !{i64 8500618921732177904}
!3 = !{i64 1034183476557456773}
!4 = !{i64 1012322370737446613}
!5 = !{i64 -2008938880185029680}
!6 = !{i64 -2733371156669675596}
!7 = !{i64 648740448011510802}
!8 = !{i64 -7762938704667665859}
!9 = !{i64 758564435889190430}
!10 = !{i64 -8052685178584123855}
!11 = !{i64 -7377634875395668338}
!12 = !{i64 683122857937320242}
!13 = !{i32 8, !"PIC Level", i32 2}
!14 = !{i32 7, !"uwtable", i32 2}
!15 = !{i32 7, !"frame-pointer", i32 1}
!16 = !{!"rustc version 1.99.0 (b940084d7 2026-09-28)"}
!17 = !{i64 877453575045248192}
!18 = !{i64 -8948187153682177708}
