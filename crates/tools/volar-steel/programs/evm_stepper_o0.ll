; ModuleID = 'evm_stepper.6040c2cbbc1ab58f-cgu.0'
source_filename = "evm_stepper.6040c2cbbc1ab58f-cgu.0"
target datalayout = "e-m:o-p270:32:32-p271:32:32-p272:64:64-i64:64-i128:128-n32:64-S128-Fn32"
target triple = "arm64-apple-macosx11.0.0"

@alloc_18dbe6d2806025687ee2dfd52e6ffdca = private unnamed_addr constant [15 x i8] c"evm_stepper.rs\00", align 1, !guid !0
@alloc_bf83b7dca7e9fab41a739c9cf515ae85 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\07\01\00\00\1F\00\00\00" }>, align 8, !guid !1
@alloc_2897906619ac8877d83bac3ed51d5a20 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\09\01\00\00\1B\00\00\00" }>, align 8, !guid !2
@alloc_7cc0e1dee8ed51b0cf75fa0a272bc20b = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\0B\01\00\00\1B\00\00\00" }>, align 8, !guid !3
@alloc_b3d6fdcf89fcb39e3061fca3c4c64c55 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\10\01\00\00\1E\00\00\00" }>, align 8, !guid !4
@alloc_b0a56e61d171b69ee9c1c0ac39b43458 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\12\01\00\00\1B\00\00\00" }>, align 8, !guid !5
@alloc_8be5e1691db01b10faa40a0a1a4e158b = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\14\01\00\00\1B\00\00\00" }>, align 8, !guid !6
@alloc_9764f52201d7cb3c433fc1929eca6133 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\1E\01\00\00\11\00\00\00" }>, align 8, !guid !7
@alloc_78e493ce24bebd476b47708e3ee1bb7c = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\F1\00\00\00\1F\00\00\00" }>, align 8, !guid !8
@alloc_8900e265d9d8ee0a55d3086a3206a5bb = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\F3\00\00\00\1B\00\00\00" }>, align 8, !guid !9
@alloc_40e157ef838361a14478e835d630ea55 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\F5\00\00\00\1B\00\00\00" }>, align 8, !guid !10
@alloc_92243982ce6edb7515585dbef0974098 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\FB\00\00\00\14\00\00\00" }>, align 8, !guid !11
@alloc_30fcafb1d6d170a1f661937e15f3e3d5 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\FD\00\00\00\1B\00\00\00" }>, align 8, !guid !12
@alloc_066ab0d36ab70c3b939a0d99d37f7b39 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\FF\00\00\00\1B\00\00\00" }>, align 8, !guid !13
@alloc_ef5836299800a74e2e1c6a65ab567864 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\B1\00\00\00\1C\00\00\00" }>, align 8, !guid !14
@alloc_0e70d99da70d7a7439e7c9b64dc9f14f = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\B3\00\00\00\1B\00\00\00" }>, align 8, !guid !15
@alloc_1bdc23c8b9471322798493151cc85064 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\B5\00\00\00\1B\00\00\00" }>, align 8, !guid !16
@alloc_167aa47c356faec77ce87279579cb772 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\BA\00\00\00\1C\00\00\00" }>, align 8, !guid !17
@alloc_378fec6f626ecff928e8f8203c66dd18 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\BC\00\00\00\1B\00\00\00" }>, align 8, !guid !18
@alloc_a5a932eaa6a3d999cec255c9482a13f3 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\BE\00\00\00\1B\00\00\00" }>, align 8, !guid !19
@alloc_683e5002b389180249336bf3800ff5a5 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\CE\00\00\00\19\00\00\00" }>, align 8, !guid !20
@alloc_5c927e99c5162555792021074754ef9d = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\E4\00\00\00\14\00\00\00" }>, align 8, !guid !21
@alloc_c0e0aa6692bef22700dd3ea90834461f = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\E6\00\00\00\1B\00\00\00" }>, align 8, !guid !22
@alloc_7011358ff686acdf8d92531013aa1625 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\E8\00\00\00\1B\00\00\00" }>, align 8, !guid !23
@alloc_aa807ddf0a878228f0a086291772f796 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\ED\00\00\00\11\00\00\00" }>, align 8, !guid !24
@alloc_17e198bb92b5ed3edaa27c6fc38bd3b3 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\94\00\00\00\1C\00\00\00" }>, align 8, !guid !25
@alloc_3146500d0277d3515eb862d7ced37ccc = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\96\00\00\00\1B\00\00\00" }>, align 8, !guid !26
@alloc_8832e5318cf1e88dfd98e947c2a9d82a = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\98\00\00\00\1B\00\00\00" }>, align 8, !guid !27
@alloc_42486cd9a8b573ead7a73a351d0081c8 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\9E\00\00\00\14\00\00\00" }>, align 8, !guid !28
@alloc_ba374e33b0efef6e6e34b12b5e370b78 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\A0\00\00\00\1B\00\00\00" }>, align 8, !guid !29
@alloc_a4aeed3fb79f2948381f902b1c0ad747 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\A2\00\00\00\1B\00\00\00" }>, align 8, !guid !30
@alloc_efc31314f82b13d3ff43ef0ef08ec387 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00l\00\00\00\1F\00\00\00" }>, align 8, !guid !31
@alloc_2a282c33ed5c0cf9a37394a3a790c70b = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00n\00\00\00\1B\00\00\00" }>, align 8, !guid !32
@alloc_d5fe0760f33049fcbe44db8556ac9796 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00p\00\00\00\1B\00\00\00" }>, align 8, !guid !33
@alloc_f60a96a0e29df3d529f89f6de4ac23a6 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00{\00\00\00'\00\00\00" }>, align 8, !guid !34
@alloc_117cec81fff21b2bfc8ff71d6efbdac3 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00}\00\00\00#\00\00\00" }>, align 8, !guid !35
@alloc_6366e9d44da4008ec44de40b36806cdf = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\7F\00\00\00#\00\00\00" }>, align 8, !guid !36
@alloc_38f41394bd16f786ffcdfe6082d22002 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\84\00\00\00\19\00\00\00" }>, align 8, !guid !37
@alloc_b5dc86b180f54dd805f6694f881537e0 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00v\00\00\00\15\00\00\00" }>, align 8, !guid !38
@alloc_c147417fbda79ec5d1404bb17190ed30 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\8C\00\00\00\15\00\00\00" }>, align 8, !guid !39
@alloc_1767a730e315034eae319434d1c3ed82 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00J\00\00\00\0D\00\00\00" }>, align 8, !guid !40
@alloc_54cd4afdd2c7ef60ddffe8d938223c0a = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00g\00\00\00\11\00\00\00" }>, align 8, !guid !41
@alloc_b6b146a477835ff0ec69b2bc0f8e41d1 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00!\01\00\00\09\00\00\00" }>, align 8, !guid !42
@alloc_8fe0bbbccb6494d3ac6256338819e107 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00\22\01\00\00\09\00\00\00" }>, align 8, !guid !43
@alloc_f37f11ff84fd154fde82d50e49452aac = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00&\01\00\00\0C\00\00\00" }>, align 8, !guid !44
@alloc_fa15e43a869f20c78539a9e032b64050 = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00(\01\00\00\13\00\00\00" }>, align 8, !guid !45
@alloc_25d016295500fc89453dbe007bbdf52a = private unnamed_addr constant <{ ptr, [16 x i8] }> <{ ptr @alloc_18dbe6d2806025687ee2dfd52e6ffdca, [16 x i8] c"\0E\00\00\00\00\00\00\00*\01\00\00\13\00\00\00" }>, align 8, !guid !46

; __rustc::rust_begin_unwind
; Function Attrs: noreturn uwtable
define void @_RNvCsdtSMxSA38aw_7___rustc17rust_begin_unwind(ptr align 8 %_1) unnamed_addr #0 !guid !51 {
start:
  br label %bb1

bb1:                                              ; preds = %bb1, %start
  br label %bb1
}

; Function Attrs: uwtable
define i64 @evm_step(i64 %code0, i64 %code1, i64 %code2, i64 %code3, i64 %code4, i64 %code5, i64 %code6, i64 %code7, i64 %storage0, i64 %storage1, i64 %steps) unnamed_addr #1 !guid !52 {
start:
  %val3 = alloca [8 x i8], align 8
  %_287 = alloca [8 x i8], align 8
  %val = alloca [8 x i8], align 8
  %_251 = alloca [8 x i8], align 8
  %r2 = alloca [8 x i8], align 8
  %b = alloca [8 x i8], align 8
  %a1 = alloca [8 x i8], align 8
  %r = alloca [8 x i8], align 8
  %a = alloca [8 x i8], align 8
  %cond = alloca [8 x i8], align 8
  %do_jump = alloca [1 x i8], align 1
  %dest = alloca [8 x i8], align 8
  %imm = alloca [8 x i8], align 8
  %op = alloca [8 x i8], align 8
  %i = alloca [8 x i8], align 8
  %pc = alloca [8 x i8], align 8
  %store1 = alloca [8 x i8], align 8
  %store0 = alloca [8 x i8], align 8
  %sp = alloca [8 x i8], align 8
  %s3 = alloca [8 x i8], align 8
  %s2 = alloca [8 x i8], align 8
  %s1 = alloca [8 x i8], align 8
  %s0 = alloca [8 x i8], align 8
  %_0 = alloca [8 x i8], align 8
  store i64 0, ptr %s0, align 8
  store i64 0, ptr %s1, align 8
  store i64 0, ptr %s2, align 8
  store i64 0, ptr %s3, align 8
  store i64 0, ptr %sp, align 8
  store i64 %storage0, ptr %store0, align 8
  store i64 %storage1, ptr %store1, align 8
  store i64 0, ptr %pc, align 8
  store i64 0, ptr %i, align 8
  br label %bb1

bb1:                                              ; preds = %bb221, %bb78, %start
  %_22 = load i64, ptr %i, align 8
  %_21 = icmp ult i64 %_22, %steps
  br i1 %_21, label %bb2, label %bb222

bb222:                                            ; preds = %bb18, %bb2, %bb1
  %_320 = load i64, ptr %sp, align 8
  %_319 = icmp ugt i64 %_320, 0
  br i1 %_319, label %bb223, label %bb233

bb2:                                              ; preds = %bb1
  %_24 = load i64, ptr %pc, align 8
  %_23 = icmp ult i64 %_24, 8
  br i1 %_23, label %bb3, label %bb222

bb3:                                              ; preds = %bb2
  %_27 = load i64, ptr %pc, align 8
  %_26 = icmp eq i64 %_27, 0
  br i1 %_26, label %bb4, label %bb5

bb5:                                              ; preds = %bb3
  %_29 = load i64, ptr %pc, align 8
  %_28 = icmp eq i64 %_29, 1
  br i1 %_28, label %bb6, label %bb7

bb4:                                              ; preds = %bb3
  store i64 %code0, ptr %op, align 8
  br label %bb18

bb7:                                              ; preds = %bb5
  %_31 = load i64, ptr %pc, align 8
  %_30 = icmp eq i64 %_31, 2
  br i1 %_30, label %bb8, label %bb9

bb6:                                              ; preds = %bb5
  store i64 %code1, ptr %op, align 8
  br label %bb18

bb9:                                              ; preds = %bb7
  %_33 = load i64, ptr %pc, align 8
  %_32 = icmp eq i64 %_33, 3
  br i1 %_32, label %bb10, label %bb11

bb8:                                              ; preds = %bb7
  store i64 %code2, ptr %op, align 8
  br label %bb18

bb11:                                             ; preds = %bb9
  %_35 = load i64, ptr %pc, align 8
  %_34 = icmp eq i64 %_35, 4
  br i1 %_34, label %bb12, label %bb13

bb10:                                             ; preds = %bb9
  store i64 %code3, ptr %op, align 8
  br label %bb18

bb13:                                             ; preds = %bb11
  %_37 = load i64, ptr %pc, align 8
  %_36 = icmp eq i64 %_37, 5
  br i1 %_36, label %bb14, label %bb15

bb12:                                             ; preds = %bb11
  store i64 %code4, ptr %op, align 8
  br label %bb18

bb15:                                             ; preds = %bb13
  %_39 = load i64, ptr %pc, align 8
  %_38 = icmp eq i64 %_39, 6
  br i1 %_38, label %bb16, label %bb17

bb14:                                             ; preds = %bb13
  store i64 %code5, ptr %op, align 8
  br label %bb18

bb17:                                             ; preds = %bb15
  store i64 %code7, ptr %op, align 8
  br label %bb18

bb16:                                             ; preds = %bb15
  store i64 %code6, ptr %op, align 8
  br label %bb18

bb18:                                             ; preds = %bb4, %bb6, %bb8, %bb10, %bb12, %bb14, %bb16, %bb17
  %_41 = load i64, ptr %op, align 8
  %_40 = icmp eq i64 %_41, 0
  br i1 %_40, label %bb222, label %bb19

bb19:                                             ; preds = %bb18
  %_43 = load i64, ptr %op, align 8
  %_42 = icmp eq i64 %_43, 96
  br i1 %_42, label %bb20, label %bb46

bb46:                                             ; preds = %bb19
  %_74 = load i64, ptr %op, align 8
  %_73 = icmp eq i64 %_74, 86
  br i1 %_73, label %bb48, label %bb47

bb20:                                             ; preds = %bb19
  %0 = load i64, ptr %pc, align 8
  %_44.0 = add i64 %0, 1
  %_44.1 = icmp ult i64 %_44.0, %0
  br i1 %_44.1, label %panic43, label %bb21

bb47:                                             ; preds = %bb46
  %_76 = load i64, ptr %op, align 8
  %_75 = icmp eq i64 %_76, 87
  br i1 %_75, label %bb48, label %bb79

bb48:                                             ; preds = %bb47, %bb46
  %_78 = load i64, ptr %sp, align 8
  %_77 = icmp uge i64 %_78, 1
  br i1 %_77, label %bb49, label %bb219

bb79:                                             ; preds = %bb47
  %_117 = load i64, ptr %op, align 8
  %_116 = icmp eq i64 %_117, 91
  br i1 %_116, label %bb219, label %bb80

bb80:                                             ; preds = %bb79
  %_119 = load i64, ptr %op, align 8
  %_118 = icmp eq i64 %_119, 21
  br i1 %_118, label %bb81, label %bb105

bb219:                                            ; preds = %bb45, %bb36, %bb76, %bb48, %bb97, %bb100, %bb103, %bb104, %bb81, %bb166, %bb112, %bb184, %bb187, %bb190, %bb191, %bb168, %bb218, %bb193, %bb192, %bb79
  %1 = load i64, ptr %pc, align 8
  %_317.0 = add i64 %1, 1
  %_317.1 = icmp ult i64 %_317.0, %1
  br i1 %_317.1, label %panic45, label %bb220

bb105:                                            ; preds = %bb80
  %_155 = load i64, ptr %op, align 8
  %_154 = icmp eq i64 %_155, 1
  br i1 %_154, label %bb112, label %bb106

bb81:                                             ; preds = %bb80
  %_121 = load i64, ptr %sp, align 8
  %_120 = icmp uge i64 %_121, 1
  br i1 %_120, label %bb82, label %bb219

bb106:                                            ; preds = %bb105
  %_157 = load i64, ptr %op, align 8
  %_156 = icmp eq i64 %_157, 2
  br i1 %_156, label %bb112, label %bb107

bb112:                                            ; preds = %bb111, %bb110, %bb109, %bb108, %bb107, %bb106, %bb105
  %_169 = load i64, ptr %sp, align 8
  %_168 = icmp uge i64 %_169, 2
  br i1 %_168, label %bb113, label %bb219

bb107:                                            ; preds = %bb106
  %_159 = load i64, ptr %op, align 8
  %_158 = icmp eq i64 %_159, 3
  br i1 %_158, label %bb112, label %bb108

bb108:                                            ; preds = %bb107
  %_161 = load i64, ptr %op, align 8
  %_160 = icmp eq i64 %_161, 4
  br i1 %_160, label %bb112, label %bb109

bb109:                                            ; preds = %bb108
  %_163 = load i64, ptr %op, align 8
  %_162 = icmp eq i64 %_163, 16
  br i1 %_162, label %bb112, label %bb110

bb110:                                            ; preds = %bb109
  %_165 = load i64, ptr %op, align 8
  %_164 = icmp eq i64 %_165, 17
  br i1 %_164, label %bb112, label %bb111

bb111:                                            ; preds = %bb110
  %_167 = load i64, ptr %op, align 8
  %_166 = icmp eq i64 %_167, 20
  br i1 %_166, label %bb112, label %bb167

bb167:                                            ; preds = %bb111
  %_247 = load i64, ptr %op, align 8
  %_246 = icmp eq i64 %_247, 84
  br i1 %_246, label %bb168, label %bb192

bb192:                                            ; preds = %bb167
  %_283 = load i64, ptr %op, align 8
  %_282 = icmp eq i64 %_283, 85
  br i1 %_282, label %bb193, label %bb219

bb168:                                            ; preds = %bb167
  %_249 = load i64, ptr %sp, align 8
  %_248 = icmp uge i64 %_249, 1
  br i1 %_248, label %bb169, label %bb219

bb193:                                            ; preds = %bb192
  %_285 = load i64, ptr %sp, align 8
  %_284 = icmp uge i64 %_285, 2
  br i1 %_284, label %bb194, label %bb219

bb194:                                            ; preds = %bb193
  %_290 = load i64, ptr %sp, align 8
  %_291.0 = sub i64 %_290, 1
  %_291.1 = icmp ult i64 %_290, 1
  br i1 %_291.1, label %panic, label %bb195

bb195:                                            ; preds = %bb194
  %_288 = icmp eq i64 %_291.0, 0
  br i1 %_288, label %bb196, label %bb197

panic:                                            ; preds = %bb194
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_bf83b7dca7e9fab41a739c9cf515ae85) #3
  unreachable

bb197:                                            ; preds = %bb195
  %_294 = load i64, ptr %sp, align 8
  %_295.0 = sub i64 %_294, 1
  %_295.1 = icmp ult i64 %_294, 1
  br i1 %_295.1, label %panic4, label %bb198

bb196:                                            ; preds = %bb195
  %2 = load i64, ptr %s0, align 8
  store i64 %2, ptr %_287, align 8
  br label %bb204

bb198:                                            ; preds = %bb197
  %_292 = icmp eq i64 %_295.0, 1
  br i1 %_292, label %bb199, label %bb200

panic4:                                           ; preds = %bb197
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_2897906619ac8877d83bac3ed51d5a20) #3
  unreachable

bb200:                                            ; preds = %bb198
  %_298 = load i64, ptr %sp, align 8
  %_299.0 = sub i64 %_298, 1
  %_299.1 = icmp ult i64 %_298, 1
  br i1 %_299.1, label %panic5, label %bb201

bb199:                                            ; preds = %bb198
  %3 = load i64, ptr %s1, align 8
  store i64 %3, ptr %_287, align 8
  br label %bb204

bb201:                                            ; preds = %bb200
  %_296 = icmp eq i64 %_299.0, 2
  br i1 %_296, label %bb202, label %bb203

panic5:                                           ; preds = %bb200
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_7cc0e1dee8ed51b0cf75fa0a272bc20b) #3
  unreachable

bb203:                                            ; preds = %bb201
  %4 = load i64, ptr %s3, align 8
  store i64 %4, ptr %_287, align 8
  br label %bb204

bb202:                                            ; preds = %bb201
  %5 = load i64, ptr %s2, align 8
  store i64 %5, ptr %_287, align 8
  br label %bb204

bb204:                                            ; preds = %bb196, %bb199, %bb202, %bb203
  %6 = load i64, ptr %_287, align 8
  %key = and i64 %6, 1
  %_303 = load i64, ptr %sp, align 8
  %_304.0 = sub i64 %_303, 2
  %_304.1 = icmp ult i64 %_303, 2
  br i1 %_304.1, label %panic6, label %bb205

bb205:                                            ; preds = %bb204
  %_301 = icmp eq i64 %_304.0, 0
  br i1 %_301, label %bb206, label %bb207

panic6:                                           ; preds = %bb204
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_b3d6fdcf89fcb39e3061fca3c4c64c55) #3
  unreachable

bb207:                                            ; preds = %bb205
  %_307 = load i64, ptr %sp, align 8
  %_308.0 = sub i64 %_307, 2
  %_308.1 = icmp ult i64 %_307, 2
  br i1 %_308.1, label %panic7, label %bb208

bb206:                                            ; preds = %bb205
  %7 = load i64, ptr %s0, align 8
  store i64 %7, ptr %val3, align 8
  br label %bb214

bb208:                                            ; preds = %bb207
  %_305 = icmp eq i64 %_308.0, 1
  br i1 %_305, label %bb209, label %bb210

panic7:                                           ; preds = %bb207
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_b0a56e61d171b69ee9c1c0ac39b43458) #3
  unreachable

bb210:                                            ; preds = %bb208
  %_311 = load i64, ptr %sp, align 8
  %_312.0 = sub i64 %_311, 2
  %_312.1 = icmp ult i64 %_311, 2
  br i1 %_312.1, label %panic8, label %bb211

bb209:                                            ; preds = %bb208
  %8 = load i64, ptr %s1, align 8
  store i64 %8, ptr %val3, align 8
  br label %bb214

bb211:                                            ; preds = %bb210
  %_309 = icmp eq i64 %_312.0, 2
  br i1 %_309, label %bb212, label %bb213

panic8:                                           ; preds = %bb210
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_8be5e1691db01b10faa40a0a1a4e158b) #3
  unreachable

bb213:                                            ; preds = %bb211
  %9 = load i64, ptr %s3, align 8
  store i64 %9, ptr %val3, align 8
  br label %bb214

bb212:                                            ; preds = %bb211
  %10 = load i64, ptr %s2, align 8
  store i64 %10, ptr %val3, align 8
  br label %bb214

bb214:                                            ; preds = %bb206, %bb209, %bb212, %bb213
  %_313 = icmp eq i64 %key, 0
  br i1 %_313, label %bb215, label %bb216

bb216:                                            ; preds = %bb214
  %_315 = load i64, ptr %val3, align 8
  store i64 %_315, ptr %store1, align 8
  br label %bb217

bb215:                                            ; preds = %bb214
  %_314 = load i64, ptr %val3, align 8
  store i64 %_314, ptr %store0, align 8
  br label %bb217

bb217:                                            ; preds = %bb215, %bb216
  %11 = load i64, ptr %sp, align 8
  %_316.0 = sub i64 %11, 2
  %_316.1 = icmp ult i64 %11, 2
  br i1 %_316.1, label %panic9, label %bb218

bb218:                                            ; preds = %bb217
  store i64 %_316.0, ptr %sp, align 8
  br label %bb219

panic9:                                           ; preds = %bb217
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_9764f52201d7cb3c433fc1929eca6133) #3
  unreachable

bb169:                                            ; preds = %bb168
  %_254 = load i64, ptr %sp, align 8
  %_255.0 = sub i64 %_254, 1
  %_255.1 = icmp ult i64 %_254, 1
  br i1 %_255.1, label %panic10, label %bb170

bb170:                                            ; preds = %bb169
  %_252 = icmp eq i64 %_255.0, 0
  br i1 %_252, label %bb171, label %bb172

panic10:                                          ; preds = %bb169
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_78e493ce24bebd476b47708e3ee1bb7c) #3
  unreachable

bb172:                                            ; preds = %bb170
  %_258 = load i64, ptr %sp, align 8
  %_259.0 = sub i64 %_258, 1
  %_259.1 = icmp ult i64 %_258, 1
  br i1 %_259.1, label %panic11, label %bb173

bb171:                                            ; preds = %bb170
  %12 = load i64, ptr %s0, align 8
  store i64 %12, ptr %_251, align 8
  br label %bb179

bb173:                                            ; preds = %bb172
  %_256 = icmp eq i64 %_259.0, 1
  br i1 %_256, label %bb174, label %bb175

panic11:                                          ; preds = %bb172
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_8900e265d9d8ee0a55d3086a3206a5bb) #3
  unreachable

bb175:                                            ; preds = %bb173
  %_262 = load i64, ptr %sp, align 8
  %_263.0 = sub i64 %_262, 1
  %_263.1 = icmp ult i64 %_262, 1
  br i1 %_263.1, label %panic12, label %bb176

bb174:                                            ; preds = %bb173
  %13 = load i64, ptr %s1, align 8
  store i64 %13, ptr %_251, align 8
  br label %bb179

bb176:                                            ; preds = %bb175
  %_260 = icmp eq i64 %_263.0, 2
  br i1 %_260, label %bb177, label %bb178

panic12:                                          ; preds = %bb175
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_40e157ef838361a14478e835d630ea55) #3
  unreachable

bb178:                                            ; preds = %bb176
  %14 = load i64, ptr %s3, align 8
  store i64 %14, ptr %_251, align 8
  br label %bb179

bb177:                                            ; preds = %bb176
  %15 = load i64, ptr %s2, align 8
  store i64 %15, ptr %_251, align 8
  br label %bb179

bb179:                                            ; preds = %bb171, %bb174, %bb177, %bb178
  %16 = load i64, ptr %_251, align 8
  %key13 = and i64 %16, 1
  %_265 = icmp eq i64 %key13, 0
  br i1 %_265, label %bb180, label %bb181

bb181:                                            ; preds = %bb179
  %17 = load i64, ptr %store1, align 8
  store i64 %17, ptr %val, align 8
  br label %bb182

bb180:                                            ; preds = %bb179
  %18 = load i64, ptr %store0, align 8
  store i64 %18, ptr %val, align 8
  br label %bb182

bb182:                                            ; preds = %bb180, %bb181
  %_268 = load i64, ptr %sp, align 8
  %_269.0 = sub i64 %_268, 1
  %_269.1 = icmp ult i64 %_268, 1
  br i1 %_269.1, label %panic14, label %bb183

bb183:                                            ; preds = %bb182
  %_266 = icmp eq i64 %_269.0, 0
  br i1 %_266, label %bb184, label %bb185

panic14:                                          ; preds = %bb182
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_92243982ce6edb7515585dbef0974098) #3
  unreachable

bb185:                                            ; preds = %bb183
  %_273 = load i64, ptr %sp, align 8
  %_274.0 = sub i64 %_273, 1
  %_274.1 = icmp ult i64 %_273, 1
  br i1 %_274.1, label %panic15, label %bb186

bb184:                                            ; preds = %bb183
  %_270 = load i64, ptr %val, align 8
  store i64 %_270, ptr %s0, align 8
  br label %bb219

bb186:                                            ; preds = %bb185
  %_271 = icmp eq i64 %_274.0, 1
  br i1 %_271, label %bb187, label %bb188

panic15:                                          ; preds = %bb185
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_30fcafb1d6d170a1f661937e15f3e3d5) #3
  unreachable

bb188:                                            ; preds = %bb186
  %_278 = load i64, ptr %sp, align 8
  %_279.0 = sub i64 %_278, 1
  %_279.1 = icmp ult i64 %_278, 1
  br i1 %_279.1, label %panic16, label %bb189

bb187:                                            ; preds = %bb186
  %_275 = load i64, ptr %val, align 8
  store i64 %_275, ptr %s1, align 8
  br label %bb219

bb189:                                            ; preds = %bb188
  %_276 = icmp eq i64 %_279.0, 2
  br i1 %_276, label %bb190, label %bb191

panic16:                                          ; preds = %bb188
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_066ab0d36ab70c3b939a0d99d37f7b39) #3
  unreachable

bb191:                                            ; preds = %bb189
  %_281 = load i64, ptr %val, align 8
  store i64 %_281, ptr %s3, align 8
  br label %bb219

bb190:                                            ; preds = %bb189
  %_280 = load i64, ptr %val, align 8
  store i64 %_280, ptr %s2, align 8
  br label %bb219

bb113:                                            ; preds = %bb112
  %_173 = load i64, ptr %sp, align 8
  %_174.0 = sub i64 %_173, 1
  %_174.1 = icmp ult i64 %_173, 1
  br i1 %_174.1, label %panic17, label %bb114

bb114:                                            ; preds = %bb113
  %_171 = icmp eq i64 %_174.0, 0
  br i1 %_171, label %bb115, label %bb116

panic17:                                          ; preds = %bb113
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_ef5836299800a74e2e1c6a65ab567864) #3
  unreachable

bb116:                                            ; preds = %bb114
  %_177 = load i64, ptr %sp, align 8
  %_178.0 = sub i64 %_177, 1
  %_178.1 = icmp ult i64 %_177, 1
  br i1 %_178.1, label %panic18, label %bb117

bb115:                                            ; preds = %bb114
  %19 = load i64, ptr %s0, align 8
  store i64 %19, ptr %a1, align 8
  br label %bb123

bb117:                                            ; preds = %bb116
  %_175 = icmp eq i64 %_178.0, 1
  br i1 %_175, label %bb118, label %bb119

panic18:                                          ; preds = %bb116
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_0e70d99da70d7a7439e7c9b64dc9f14f) #3
  unreachable

bb119:                                            ; preds = %bb117
  %_181 = load i64, ptr %sp, align 8
  %_182.0 = sub i64 %_181, 1
  %_182.1 = icmp ult i64 %_181, 1
  br i1 %_182.1, label %panic19, label %bb120

bb118:                                            ; preds = %bb117
  %20 = load i64, ptr %s1, align 8
  store i64 %20, ptr %a1, align 8
  br label %bb123

bb120:                                            ; preds = %bb119
  %_179 = icmp eq i64 %_182.0, 2
  br i1 %_179, label %bb121, label %bb122

panic19:                                          ; preds = %bb119
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_1bdc23c8b9471322798493151cc85064) #3
  unreachable

bb122:                                            ; preds = %bb120
  %21 = load i64, ptr %s3, align 8
  store i64 %21, ptr %a1, align 8
  br label %bb123

bb121:                                            ; preds = %bb120
  %22 = load i64, ptr %s2, align 8
  store i64 %22, ptr %a1, align 8
  br label %bb123

bb123:                                            ; preds = %bb115, %bb118, %bb121, %bb122
  %_186 = load i64, ptr %sp, align 8
  %_187.0 = sub i64 %_186, 2
  %_187.1 = icmp ult i64 %_186, 2
  br i1 %_187.1, label %panic20, label %bb124

bb124:                                            ; preds = %bb123
  %_184 = icmp eq i64 %_187.0, 0
  br i1 %_184, label %bb125, label %bb126

panic20:                                          ; preds = %bb123
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_167aa47c356faec77ce87279579cb772) #3
  unreachable

bb126:                                            ; preds = %bb124
  %_190 = load i64, ptr %sp, align 8
  %_191.0 = sub i64 %_190, 2
  %_191.1 = icmp ult i64 %_190, 2
  br i1 %_191.1, label %panic21, label %bb127

bb125:                                            ; preds = %bb124
  %23 = load i64, ptr %s0, align 8
  store i64 %23, ptr %b, align 8
  br label %bb133

bb127:                                            ; preds = %bb126
  %_188 = icmp eq i64 %_191.0, 1
  br i1 %_188, label %bb128, label %bb129

panic21:                                          ; preds = %bb126
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_378fec6f626ecff928e8f8203c66dd18) #3
  unreachable

bb129:                                            ; preds = %bb127
  %_194 = load i64, ptr %sp, align 8
  %_195.0 = sub i64 %_194, 2
  %_195.1 = icmp ult i64 %_194, 2
  br i1 %_195.1, label %panic22, label %bb130

bb128:                                            ; preds = %bb127
  %24 = load i64, ptr %s1, align 8
  store i64 %24, ptr %b, align 8
  br label %bb133

bb130:                                            ; preds = %bb129
  %_192 = icmp eq i64 %_195.0, 2
  br i1 %_192, label %bb131, label %bb132

panic22:                                          ; preds = %bb129
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_a5a932eaa6a3d999cec255c9482a13f3) #3
  unreachable

bb132:                                            ; preds = %bb130
  %25 = load i64, ptr %s3, align 8
  store i64 %25, ptr %b, align 8
  br label %bb133

bb131:                                            ; preds = %bb130
  %26 = load i64, ptr %s2, align 8
  store i64 %26, ptr %b, align 8
  br label %bb133

bb133:                                            ; preds = %bb125, %bb128, %bb131, %bb132
  %_198 = load i64, ptr %op, align 8
  %_197 = icmp eq i64 %_198, 1
  br i1 %_197, label %bb134, label %bb135

bb135:                                            ; preds = %bb133
  %_202 = load i64, ptr %op, align 8
  %_201 = icmp eq i64 %_202, 2
  br i1 %_201, label %bb136, label %bb137

bb134:                                            ; preds = %bb133
  %_199 = load i64, ptr %a1, align 8
  %_200 = load i64, ptr %b, align 8
  %_0.i = add i64 %_199, %_200
  store i64 %_0.i, ptr %r2, align 8
  br label %bb155

bb137:                                            ; preds = %bb135
  %_206 = load i64, ptr %op, align 8
  %_205 = icmp eq i64 %_206, 3
  br i1 %_205, label %bb138, label %bb139

bb136:                                            ; preds = %bb135
  %_203 = load i64, ptr %a1, align 8
  %_204 = load i64, ptr %b, align 8
  %_0.i50 = mul i64 %_203, %_204
  store i64 %_0.i50, ptr %r2, align 8
  br label %bb155

bb139:                                            ; preds = %bb137
  %_210 = load i64, ptr %op, align 8
  %_209 = icmp eq i64 %_210, 4
  br i1 %_209, label %bb140, label %bb144

bb138:                                            ; preds = %bb137
  %_207 = load i64, ptr %a1, align 8
  %_208 = load i64, ptr %b, align 8
  %_0.i51 = sub i64 %_207, %_208
  store i64 %_0.i51, ptr %r2, align 8
  br label %bb155

bb144:                                            ; preds = %bb139
  %_217 = load i64, ptr %op, align 8
  %_216 = icmp eq i64 %_217, 16
  br i1 %_216, label %bb145, label %bb148

bb140:                                            ; preds = %bb139
  %_212 = load i64, ptr %b, align 8
  %_211 = icmp eq i64 %_212, 0
  br i1 %_211, label %bb141, label %bb142

bb148:                                            ; preds = %bb144
  %_222 = load i64, ptr %op, align 8
  %_221 = icmp eq i64 %_222, 17
  br i1 %_221, label %bb149, label %bb152

bb145:                                            ; preds = %bb144
  %_219 = load i64, ptr %b, align 8
  %_220 = load i64, ptr %a1, align 8
  %_218 = icmp ult i64 %_219, %_220
  br i1 %_218, label %bb146, label %bb147

bb152:                                            ; preds = %bb148
  %_227 = load i64, ptr %a1, align 8
  %_228 = load i64, ptr %b, align 8
  %_226 = icmp eq i64 %_227, %_228
  br i1 %_226, label %bb153, label %bb154

bb149:                                            ; preds = %bb148
  %_224 = load i64, ptr %b, align 8
  %_225 = load i64, ptr %a1, align 8
  %_223 = icmp ugt i64 %_224, %_225
  br i1 %_223, label %bb150, label %bb151

bb154:                                            ; preds = %bb152
  store i64 0, ptr %r2, align 8
  br label %bb155

bb153:                                            ; preds = %bb152
  store i64 1, ptr %r2, align 8
  br label %bb155

bb155:                                            ; preds = %bb134, %bb136, %bb138, %bb141, %bb143, %bb146, %bb147, %bb150, %bb151, %bb153, %bb154
  %_231 = load i64, ptr %sp, align 8
  %_232.0 = sub i64 %_231, 2
  %_232.1 = icmp ult i64 %_231, 2
  br i1 %_232.1, label %panic24, label %bb156

bb151:                                            ; preds = %bb149
  store i64 0, ptr %r2, align 8
  br label %bb155

bb150:                                            ; preds = %bb149
  store i64 1, ptr %r2, align 8
  br label %bb155

bb147:                                            ; preds = %bb145
  store i64 0, ptr %r2, align 8
  br label %bb155

bb146:                                            ; preds = %bb145
  store i64 1, ptr %r2, align 8
  br label %bb155

bb142:                                            ; preds = %bb140
  %_213 = load i64, ptr %a1, align 8
  %_214 = load i64, ptr %b, align 8
  %_215 = icmp eq i64 %_214, 0
  br i1 %_215, label %panic23, label %bb143

bb141:                                            ; preds = %bb140
  store i64 0, ptr %r2, align 8
  br label %bb155

bb143:                                            ; preds = %bb142
  %27 = udiv i64 %_213, %_214
  store i64 %27, ptr %r2, align 8
  br label %bb155

panic23:                                          ; preds = %bb142
; call core::panicking::panic_const::panic_const_div_by_zero
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const23panic_const_div_by_zero(ptr align 8 @alloc_683e5002b389180249336bf3800ff5a5) #3
  unreachable

bb156:                                            ; preds = %bb155
  %_229 = icmp eq i64 %_232.0, 0
  br i1 %_229, label %bb157, label %bb158

panic24:                                          ; preds = %bb155
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_5c927e99c5162555792021074754ef9d) #3
  unreachable

bb158:                                            ; preds = %bb156
  %_236 = load i64, ptr %sp, align 8
  %_237.0 = sub i64 %_236, 2
  %_237.1 = icmp ult i64 %_236, 2
  br i1 %_237.1, label %panic25, label %bb159

bb157:                                            ; preds = %bb156
  %_233 = load i64, ptr %r2, align 8
  store i64 %_233, ptr %s0, align 8
  br label %bb165

bb159:                                            ; preds = %bb158
  %_234 = icmp eq i64 %_237.0, 1
  br i1 %_234, label %bb160, label %bb161

panic25:                                          ; preds = %bb158
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_c0e0aa6692bef22700dd3ea90834461f) #3
  unreachable

bb161:                                            ; preds = %bb159
  %_241 = load i64, ptr %sp, align 8
  %_242.0 = sub i64 %_241, 2
  %_242.1 = icmp ult i64 %_241, 2
  br i1 %_242.1, label %panic26, label %bb162

bb160:                                            ; preds = %bb159
  %_238 = load i64, ptr %r2, align 8
  store i64 %_238, ptr %s1, align 8
  br label %bb165

bb162:                                            ; preds = %bb161
  %_239 = icmp eq i64 %_242.0, 2
  br i1 %_239, label %bb163, label %bb164

panic26:                                          ; preds = %bb161
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_7011358ff686acdf8d92531013aa1625) #3
  unreachable

bb164:                                            ; preds = %bb162
  %_244 = load i64, ptr %r2, align 8
  store i64 %_244, ptr %s3, align 8
  br label %bb165

bb163:                                            ; preds = %bb162
  %_243 = load i64, ptr %r2, align 8
  store i64 %_243, ptr %s2, align 8
  br label %bb165

bb165:                                            ; preds = %bb157, %bb160, %bb163, %bb164
  %28 = load i64, ptr %sp, align 8
  %_245.0 = sub i64 %28, 1
  %_245.1 = icmp ult i64 %28, 1
  br i1 %_245.1, label %panic27, label %bb166

bb166:                                            ; preds = %bb165
  store i64 %_245.0, ptr %sp, align 8
  br label %bb219

panic27:                                          ; preds = %bb165
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_aa807ddf0a878228f0a086291772f796) #3
  unreachable

bb82:                                             ; preds = %bb81
  %_125 = load i64, ptr %sp, align 8
  %_126.0 = sub i64 %_125, 1
  %_126.1 = icmp ult i64 %_125, 1
  br i1 %_126.1, label %panic28, label %bb83

bb83:                                             ; preds = %bb82
  %_123 = icmp eq i64 %_126.0, 0
  br i1 %_123, label %bb84, label %bb85

panic28:                                          ; preds = %bb82
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_17e198bb92b5ed3edaa27c6fc38bd3b3) #3
  unreachable

bb85:                                             ; preds = %bb83
  %_129 = load i64, ptr %sp, align 8
  %_130.0 = sub i64 %_129, 1
  %_130.1 = icmp ult i64 %_129, 1
  br i1 %_130.1, label %panic29, label %bb86

bb84:                                             ; preds = %bb83
  %29 = load i64, ptr %s0, align 8
  store i64 %29, ptr %a, align 8
  br label %bb92

bb86:                                             ; preds = %bb85
  %_127 = icmp eq i64 %_130.0, 1
  br i1 %_127, label %bb87, label %bb88

panic29:                                          ; preds = %bb85
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_3146500d0277d3515eb862d7ced37ccc) #3
  unreachable

bb88:                                             ; preds = %bb86
  %_133 = load i64, ptr %sp, align 8
  %_134.0 = sub i64 %_133, 1
  %_134.1 = icmp ult i64 %_133, 1
  br i1 %_134.1, label %panic30, label %bb89

bb87:                                             ; preds = %bb86
  %30 = load i64, ptr %s1, align 8
  store i64 %30, ptr %a, align 8
  br label %bb92

bb89:                                             ; preds = %bb88
  %_131 = icmp eq i64 %_134.0, 2
  br i1 %_131, label %bb90, label %bb91

panic30:                                          ; preds = %bb88
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_8832e5318cf1e88dfd98e947c2a9d82a) #3
  unreachable

bb91:                                             ; preds = %bb89
  %31 = load i64, ptr %s3, align 8
  store i64 %31, ptr %a, align 8
  br label %bb92

bb90:                                             ; preds = %bb89
  %32 = load i64, ptr %s2, align 8
  store i64 %32, ptr %a, align 8
  br label %bb92

bb92:                                             ; preds = %bb84, %bb87, %bb90, %bb91
  %_137 = load i64, ptr %a, align 8
  %_136 = icmp eq i64 %_137, 0
  br i1 %_136, label %bb93, label %bb94

bb94:                                             ; preds = %bb92
  store i64 0, ptr %r, align 8
  br label %bb95

bb93:                                             ; preds = %bb92
  store i64 1, ptr %r, align 8
  br label %bb95

bb95:                                             ; preds = %bb93, %bb94
  %_140 = load i64, ptr %sp, align 8
  %_141.0 = sub i64 %_140, 1
  %_141.1 = icmp ult i64 %_140, 1
  br i1 %_141.1, label %panic31, label %bb96

bb96:                                             ; preds = %bb95
  %_138 = icmp eq i64 %_141.0, 0
  br i1 %_138, label %bb97, label %bb98

panic31:                                          ; preds = %bb95
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_42486cd9a8b573ead7a73a351d0081c8) #3
  unreachable

bb98:                                             ; preds = %bb96
  %_145 = load i64, ptr %sp, align 8
  %_146.0 = sub i64 %_145, 1
  %_146.1 = icmp ult i64 %_145, 1
  br i1 %_146.1, label %panic32, label %bb99

bb97:                                             ; preds = %bb96
  %_142 = load i64, ptr %r, align 8
  store i64 %_142, ptr %s0, align 8
  br label %bb219

bb99:                                             ; preds = %bb98
  %_143 = icmp eq i64 %_146.0, 1
  br i1 %_143, label %bb100, label %bb101

panic32:                                          ; preds = %bb98
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_ba374e33b0efef6e6e34b12b5e370b78) #3
  unreachable

bb101:                                            ; preds = %bb99
  %_150 = load i64, ptr %sp, align 8
  %_151.0 = sub i64 %_150, 1
  %_151.1 = icmp ult i64 %_150, 1
  br i1 %_151.1, label %panic33, label %bb102

bb100:                                            ; preds = %bb99
  %_147 = load i64, ptr %r, align 8
  store i64 %_147, ptr %s1, align 8
  br label %bb219

bb102:                                            ; preds = %bb101
  %_148 = icmp eq i64 %_151.0, 2
  br i1 %_148, label %bb103, label %bb104

panic33:                                          ; preds = %bb101
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_a4aeed3fb79f2948381f902b1c0ad747) #3
  unreachable

bb104:                                            ; preds = %bb102
  %_153 = load i64, ptr %r, align 8
  store i64 %_153, ptr %s3, align 8
  br label %bb219

bb103:                                            ; preds = %bb102
  %_152 = load i64, ptr %r, align 8
  store i64 %_152, ptr %s2, align 8
  br label %bb219

bb49:                                             ; preds = %bb48
  %_82 = load i64, ptr %sp, align 8
  %_83.0 = sub i64 %_82, 1
  %_83.1 = icmp ult i64 %_82, 1
  br i1 %_83.1, label %panic34, label %bb50

bb50:                                             ; preds = %bb49
  %_80 = icmp eq i64 %_83.0, 0
  br i1 %_80, label %bb51, label %bb52

panic34:                                          ; preds = %bb49
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_efc31314f82b13d3ff43ef0ef08ec387) #3
  unreachable

bb52:                                             ; preds = %bb50
  %_86 = load i64, ptr %sp, align 8
  %_87.0 = sub i64 %_86, 1
  %_87.1 = icmp ult i64 %_86, 1
  br i1 %_87.1, label %panic35, label %bb53

bb51:                                             ; preds = %bb50
  %33 = load i64, ptr %s0, align 8
  store i64 %33, ptr %dest, align 8
  br label %bb59

bb53:                                             ; preds = %bb52
  %_84 = icmp eq i64 %_87.0, 1
  br i1 %_84, label %bb54, label %bb55

panic35:                                          ; preds = %bb52
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_2a282c33ed5c0cf9a37394a3a790c70b) #3
  unreachable

bb55:                                             ; preds = %bb53
  %_90 = load i64, ptr %sp, align 8
  %_91.0 = sub i64 %_90, 1
  %_91.1 = icmp ult i64 %_90, 1
  br i1 %_91.1, label %panic36, label %bb56

bb54:                                             ; preds = %bb53
  %34 = load i64, ptr %s1, align 8
  store i64 %34, ptr %dest, align 8
  br label %bb59

bb56:                                             ; preds = %bb55
  %_88 = icmp eq i64 %_91.0, 2
  br i1 %_88, label %bb57, label %bb58

panic36:                                          ; preds = %bb55
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_d5fe0760f33049fcbe44db8556ac9796) #3
  unreachable

bb58:                                             ; preds = %bb56
  %35 = load i64, ptr %s3, align 8
  store i64 %35, ptr %dest, align 8
  br label %bb59

bb57:                                             ; preds = %bb56
  %36 = load i64, ptr %s2, align 8
  store i64 %36, ptr %dest, align 8
  br label %bb59

bb59:                                             ; preds = %bb51, %bb54, %bb57, %bb58
  %_94 = load i64, ptr %op, align 8
  %_93 = icmp eq i64 %_94, 86
  br i1 %_93, label %bb60, label %bb62

bb62:                                             ; preds = %bb59
  %_97 = load i64, ptr %sp, align 8
  %_96 = icmp uge i64 %_97, 2
  br i1 %_96, label %bb63, label %bb75

bb60:                                             ; preds = %bb59
  %37 = load i64, ptr %sp, align 8
  %_95.0 = sub i64 %37, 1
  %_95.1 = icmp ult i64 %37, 1
  br i1 %_95.1, label %panic41, label %bb61

bb75:                                             ; preds = %bb62
  store i8 0, ptr %do_jump, align 1
  br label %bb76

bb63:                                             ; preds = %bb62
  %_101 = load i64, ptr %sp, align 8
  %_102.0 = sub i64 %_101, 2
  %_102.1 = icmp ult i64 %_101, 2
  br i1 %_102.1, label %panic37, label %bb64

bb76:                                             ; preds = %bb61, %bb74, %bb75
  %38 = load i8, ptr %do_jump, align 1
  %_113 = trunc nuw i8 %38 to i1
  br i1 %_113, label %bb77, label %bb219

bb64:                                             ; preds = %bb63
  %_99 = icmp eq i64 %_102.0, 0
  br i1 %_99, label %bb65, label %bb66

panic37:                                          ; preds = %bb63
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_f60a96a0e29df3d529f89f6de4ac23a6) #3
  unreachable

bb66:                                             ; preds = %bb64
  %_105 = load i64, ptr %sp, align 8
  %_106.0 = sub i64 %_105, 2
  %_106.1 = icmp ult i64 %_105, 2
  br i1 %_106.1, label %panic38, label %bb67

bb65:                                             ; preds = %bb64
  %39 = load i64, ptr %s0, align 8
  store i64 %39, ptr %cond, align 8
  br label %bb73

bb67:                                             ; preds = %bb66
  %_103 = icmp eq i64 %_106.0, 1
  br i1 %_103, label %bb68, label %bb69

panic38:                                          ; preds = %bb66
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_117cec81fff21b2bfc8ff71d6efbdac3) #3
  unreachable

bb69:                                             ; preds = %bb67
  %_109 = load i64, ptr %sp, align 8
  %_110.0 = sub i64 %_109, 2
  %_110.1 = icmp ult i64 %_109, 2
  br i1 %_110.1, label %panic39, label %bb70

bb68:                                             ; preds = %bb67
  %40 = load i64, ptr %s1, align 8
  store i64 %40, ptr %cond, align 8
  br label %bb73

bb70:                                             ; preds = %bb69
  %_107 = icmp eq i64 %_110.0, 2
  br i1 %_107, label %bb71, label %bb72

panic39:                                          ; preds = %bb69
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_6366e9d44da4008ec44de40b36806cdf) #3
  unreachable

bb72:                                             ; preds = %bb70
  %41 = load i64, ptr %s3, align 8
  store i64 %41, ptr %cond, align 8
  br label %bb73

bb71:                                             ; preds = %bb70
  %42 = load i64, ptr %s2, align 8
  store i64 %42, ptr %cond, align 8
  br label %bb73

bb73:                                             ; preds = %bb65, %bb68, %bb71, %bb72
  %43 = load i64, ptr %sp, align 8
  %_111.0 = sub i64 %43, 2
  %_111.1 = icmp ult i64 %43, 2
  br i1 %_111.1, label %panic40, label %bb74

bb74:                                             ; preds = %bb73
  store i64 %_111.0, ptr %sp, align 8
  %_112 = load i64, ptr %cond, align 8
  %44 = icmp ne i64 %_112, 0
  %45 = zext i1 %44 to i8
  store i8 %45, ptr %do_jump, align 1
  br label %bb76

panic40:                                          ; preds = %bb73
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_38f41394bd16f786ffcdfe6082d22002) #3
  unreachable

bb61:                                             ; preds = %bb60
  store i64 %_95.0, ptr %sp, align 8
  store i8 1, ptr %do_jump, align 1
  br label %bb76

panic41:                                          ; preds = %bb60
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_b5dc86b180f54dd805f6694f881537e0) #3
  unreachable

bb77:                                             ; preds = %bb76
  %_114 = load i64, ptr %dest, align 8
  store i64 %_114, ptr %pc, align 8
  %46 = load i64, ptr %i, align 8
  %_115.0 = add i64 %46, 1
  %_115.1 = icmp ult i64 %_115.0, %46
  br i1 %_115.1, label %panic42, label %bb78

bb78:                                             ; preds = %bb77
  store i64 %_115.0, ptr %i, align 8
  br label %bb1

panic42:                                          ; preds = %bb77
; call core::panicking::panic_const::panic_const_add_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8 @alloc_c147417fbda79ec5d1404bb17190ed30) #3
  unreachable

bb21:                                             ; preds = %bb20
  store i64 %_44.0, ptr %pc, align 8
  %_47 = load i64, ptr %pc, align 8
  %_46 = icmp eq i64 %_47, 0
  br i1 %_46, label %bb22, label %bb23

panic43:                                          ; preds = %bb20
; call core::panicking::panic_const::panic_const_add_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8 @alloc_1767a730e315034eae319434d1c3ed82) #3
  unreachable

bb23:                                             ; preds = %bb21
  %_49 = load i64, ptr %pc, align 8
  %_48 = icmp eq i64 %_49, 1
  br i1 %_48, label %bb24, label %bb25

bb22:                                             ; preds = %bb21
  store i64 %code0, ptr %imm, align 8
  br label %bb36

bb25:                                             ; preds = %bb23
  %_51 = load i64, ptr %pc, align 8
  %_50 = icmp eq i64 %_51, 2
  br i1 %_50, label %bb26, label %bb27

bb24:                                             ; preds = %bb23
  store i64 %code1, ptr %imm, align 8
  br label %bb36

bb27:                                             ; preds = %bb25
  %_53 = load i64, ptr %pc, align 8
  %_52 = icmp eq i64 %_53, 3
  br i1 %_52, label %bb28, label %bb29

bb26:                                             ; preds = %bb25
  store i64 %code2, ptr %imm, align 8
  br label %bb36

bb29:                                             ; preds = %bb27
  %_55 = load i64, ptr %pc, align 8
  %_54 = icmp eq i64 %_55, 4
  br i1 %_54, label %bb30, label %bb31

bb28:                                             ; preds = %bb27
  store i64 %code3, ptr %imm, align 8
  br label %bb36

bb31:                                             ; preds = %bb29
  %_57 = load i64, ptr %pc, align 8
  %_56 = icmp eq i64 %_57, 5
  br i1 %_56, label %bb32, label %bb33

bb30:                                             ; preds = %bb29
  store i64 %code4, ptr %imm, align 8
  br label %bb36

bb33:                                             ; preds = %bb31
  %_59 = load i64, ptr %pc, align 8
  %_58 = icmp eq i64 %_59, 6
  br i1 %_58, label %bb34, label %bb35

bb32:                                             ; preds = %bb31
  store i64 %code5, ptr %imm, align 8
  br label %bb36

bb35:                                             ; preds = %bb33
  store i64 %code7, ptr %imm, align 8
  br label %bb36

bb34:                                             ; preds = %bb33
  store i64 %code6, ptr %imm, align 8
  br label %bb36

bb36:                                             ; preds = %bb22, %bb24, %bb26, %bb28, %bb30, %bb32, %bb34, %bb35
  %_61 = load i64, ptr %sp, align 8
  %_60 = icmp ult i64 %_61, 4
  br i1 %_60, label %bb37, label %bb219

bb37:                                             ; preds = %bb36
  %_63 = load i64, ptr %sp, align 8
  %_62 = icmp eq i64 %_63, 0
  br i1 %_62, label %bb38, label %bb39

bb39:                                             ; preds = %bb37
  %_66 = load i64, ptr %sp, align 8
  %_65 = icmp eq i64 %_66, 1
  br i1 %_65, label %bb40, label %bb41

bb38:                                             ; preds = %bb37
  %_64 = load i64, ptr %imm, align 8
  store i64 %_64, ptr %s0, align 8
  br label %bb44

bb41:                                             ; preds = %bb39
  %_69 = load i64, ptr %sp, align 8
  %_68 = icmp eq i64 %_69, 2
  br i1 %_68, label %bb42, label %bb43

bb40:                                             ; preds = %bb39
  %_67 = load i64, ptr %imm, align 8
  store i64 %_67, ptr %s1, align 8
  br label %bb44

bb43:                                             ; preds = %bb41
  %_71 = load i64, ptr %imm, align 8
  store i64 %_71, ptr %s3, align 8
  br label %bb44

bb42:                                             ; preds = %bb41
  %_70 = load i64, ptr %imm, align 8
  store i64 %_70, ptr %s2, align 8
  br label %bb44

bb44:                                             ; preds = %bb38, %bb40, %bb42, %bb43
  %47 = load i64, ptr %sp, align 8
  %_72.0 = add i64 %47, 1
  %_72.1 = icmp ult i64 %_72.0, %47
  br i1 %_72.1, label %panic44, label %bb45

bb45:                                             ; preds = %bb44
  store i64 %_72.0, ptr %sp, align 8
  br label %bb219

panic44:                                          ; preds = %bb44
; call core::panicking::panic_const::panic_const_add_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8 @alloc_54cd4afdd2c7ef60ddffe8d938223c0a) #3
  unreachable

bb220:                                            ; preds = %bb219
  store i64 %_317.0, ptr %pc, align 8
  %48 = load i64, ptr %i, align 8
  %_318.0 = add i64 %48, 1
  %_318.1 = icmp ult i64 %_318.0, %48
  br i1 %_318.1, label %panic46, label %bb221

panic45:                                          ; preds = %bb219
; call core::panicking::panic_const::panic_const_add_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8 @alloc_b6b146a477835ff0ec69b2bc0f8e41d1) #3
  unreachable

bb221:                                            ; preds = %bb220
  store i64 %_318.0, ptr %i, align 8
  br label %bb1

panic46:                                          ; preds = %bb220
; call core::panicking::panic_const::panic_const_add_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8 @alloc_8fe0bbbccb6494d3ac6256338819e107) #3
  unreachable

bb233:                                            ; preds = %bb222
  store i64 0, ptr %_0, align 8
  br label %bb234

bb223:                                            ; preds = %bb222
  %_323 = load i64, ptr %sp, align 8
  %_324.0 = sub i64 %_323, 1
  %_324.1 = icmp ult i64 %_323, 1
  br i1 %_324.1, label %panic47, label %bb224

bb234:                                            ; preds = %bb225, %bb228, %bb231, %bb232, %bb233
  %49 = load i64, ptr %_0, align 8
  ret i64 %49

bb224:                                            ; preds = %bb223
  %_321 = icmp eq i64 %_324.0, 0
  br i1 %_321, label %bb225, label %bb226

panic47:                                          ; preds = %bb223
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_f37f11ff84fd154fde82d50e49452aac) #3
  unreachable

bb226:                                            ; preds = %bb224
  %_327 = load i64, ptr %sp, align 8
  %_328.0 = sub i64 %_327, 1
  %_328.1 = icmp ult i64 %_327, 1
  br i1 %_328.1, label %panic48, label %bb227

bb225:                                            ; preds = %bb224
  %50 = load i64, ptr %s0, align 8
  store i64 %50, ptr %_0, align 8
  br label %bb234

bb227:                                            ; preds = %bb226
  %_325 = icmp eq i64 %_328.0, 1
  br i1 %_325, label %bb228, label %bb229

panic48:                                          ; preds = %bb226
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_fa15e43a869f20c78539a9e032b64050) #3
  unreachable

bb229:                                            ; preds = %bb227
  %_331 = load i64, ptr %sp, align 8
  %_332.0 = sub i64 %_331, 1
  %_332.1 = icmp ult i64 %_331, 1
  br i1 %_332.1, label %panic49, label %bb230

bb228:                                            ; preds = %bb227
  %51 = load i64, ptr %s1, align 8
  store i64 %51, ptr %_0, align 8
  br label %bb234

bb230:                                            ; preds = %bb229
  %_329 = icmp eq i64 %_332.0, 2
  br i1 %_329, label %bb231, label %bb232

panic49:                                          ; preds = %bb229
; call core::panicking::panic_const::panic_const_sub_overflow
  call void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8 @alloc_25d016295500fc89453dbe007bbdf52a) #3
  unreachable

bb232:                                            ; preds = %bb230
  %52 = load i64, ptr %s3, align 8
  store i64 %52, ptr %_0, align 8
  br label %bb234

bb231:                                            ; preds = %bb230
  %53 = load i64, ptr %s2, align 8
  store i64 %53, ptr %_0, align 8
  br label %bb234
}

; core::panicking::panic_const::panic_const_sub_overflow
; Function Attrs: cold noinline noreturn uwtable
declare void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_sub_overflow(ptr align 8) unnamed_addr #2

; core::panicking::panic_const::panic_const_div_by_zero
; Function Attrs: cold noinline noreturn uwtable
declare void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const23panic_const_div_by_zero(ptr align 8) unnamed_addr #2

; core::panicking::panic_const::panic_const_add_overflow
; Function Attrs: cold noinline noreturn uwtable
declare void @_RNvNtNtCsjIpoRzUjyHQ_4core9panicking11panic_const24panic_const_add_overflow(ptr align 8) unnamed_addr #2

attributes #0 = { noreturn uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #1 = { uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #2 = { cold noinline noreturn uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #3 = { noinline noreturn }

!llvm.module.flags = !{!47, !48, !49}
!llvm.ident = !{!50}

!0 = !{i64 5824440350238051729}
!1 = !{i64 -3381334531096992702}
!2 = !{i64 8003854493654017216}
!3 = !{i64 -1280500820006924715}
!4 = !{i64 1724797629203876220}
!5 = !{i64 -1216398421458883146}
!6 = !{i64 2989914542255460570}
!7 = !{i64 -1794767592952318352}
!8 = !{i64 -608474571927317582}
!9 = !{i64 -4081252444550252041}
!10 = !{i64 -988479698611747222}
!11 = !{i64 -3700784129230261635}
!12 = !{i64 -4073799010763790345}
!13 = !{i64 5164378991000742856}
!14 = !{i64 8259555975642057644}
!15 = !{i64 -5167299321057803124}
!16 = !{i64 7847307990414884445}
!17 = !{i64 720334786539814802}
!18 = !{i64 3016298649999579349}
!19 = !{i64 6040375489757449864}
!20 = !{i64 3461322215173401660}
!21 = !{i64 8793621215416792951}
!22 = !{i64 6090907892908550670}
!23 = !{i64 8828848259492098156}
!24 = !{i64 -7517388267030986025}
!25 = !{i64 -4044581941670583175}
!26 = !{i64 -2372968509884015355}
!27 = !{i64 -1622240821645214909}
!28 = !{i64 5485508868549106297}
!29 = !{i64 -8540519390016254506}
!30 = !{i64 -5376847427924045303}
!31 = !{i64 -7773662750611940954}
!32 = !{i64 -8881558127763714448}
!33 = !{i64 -6622831561530656701}
!34 = !{i64 5284559036422054108}
!35 = !{i64 5654431243694875537}
!36 = !{i64 5874005840345059194}
!37 = !{i64 2420777453489989222}
!38 = !{i64 -934290305196272264}
!39 = !{i64 6266149931268421148}
!40 = !{i64 -1258911981062841269}
!41 = !{i64 751733799696791409}
!42 = !{i64 -6296268416202238855}
!43 = !{i64 5803907360456689746}
!44 = !{i64 -7390675683967601801}
!45 = !{i64 7075392368026983559}
!46 = !{i64 -4662414046750059418}
!47 = !{i32 8, !"PIC Level", i32 2}
!48 = !{i32 7, !"uwtable", i32 2}
!49 = !{i32 7, !"frame-pointer", i32 1}
!50 = !{!"rustc version 1.99.0 (b940084d7 2026-09-28)"}
!51 = !{i64 877453575045248192}
!52 = !{i64 -4000697234280174477}
