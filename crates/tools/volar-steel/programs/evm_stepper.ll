; ModuleID = 'evm_stepper.6040c2cbbc1ab58f-cgu.0'
source_filename = "evm_stepper.6040c2cbbc1ab58f-cgu.0"
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

; Function Attrs: nofree norecurse nosync nounwind memory(none) uwtable
define noundef i64 @evm_step(i64 noundef %code0, i64 noundef %code1, i64 noundef %code2, i64 noundef %code3, i64 noundef %code4, i64 noundef %code5, i64 noundef %code6, i64 noundef %code7, i64 noundef %0, i64 noundef %1, i64 noundef %steps) unnamed_addr #1 !guid !5 {
start:
  %_1895.not = icmp eq i64 %steps, 0
  br i1 %_1895.not, label %bb160, label %bb3

bb160:                                            ; preds = %bb157, %bb24, %start
  %sp.sroa.0.0.lcssa = phi i64 [ 0, %start ], [ %sp.sroa.0.1, %bb157 ], [ %sp.sroa.0.098, %bb24 ]
  %s3.sroa.0.0.lcssa = phi i64 [ 0, %start ], [ %s3.sroa.0.2, %bb157 ], [ %s3.sroa.0.099, %bb24 ]
  %s2.sroa.0.0.lcssa = phi i64 [ 0, %start ], [ %s2.sroa.0.2, %bb157 ], [ %s2.sroa.0.0100, %bb24 ]
  %s1.sroa.0.0.lcssa = phi i64 [ 0, %start ], [ %s1.sroa.0.2, %bb157 ], [ %s1.sroa.0.0101, %bb24 ]
  %s0.sroa.0.0.lcssa = phi i64 [ 0, %start ], [ %s0.sroa.0.3, %bb157 ], [ %s0.sroa.0.0102, %bb24 ]
  switch i64 %sp.sroa.0.0.lcssa, label %bb167 [
    i64 0, label %bb172
    i64 1, label %bb172.fold.split
    i64 2, label %bb164
    i64 3, label %bb166
  ]

bb3:                                              ; preds = %start, %bb157
  %storage0.sroa.0.0104 = phi i64 [ %storage0.sroa.0.1, %bb157 ], [ %0, %start ]
  %storage1.sroa.0.0103 = phi i64 [ %storage1.sroa.0.1, %bb157 ], [ %1, %start ]
  %s0.sroa.0.0102 = phi i64 [ %s0.sroa.0.3, %bb157 ], [ 0, %start ]
  %s1.sroa.0.0101 = phi i64 [ %s1.sroa.0.2, %bb157 ], [ 0, %start ]
  %s2.sroa.0.0100 = phi i64 [ %s2.sroa.0.2, %bb157 ], [ 0, %start ]
  %s3.sroa.0.099 = phi i64 [ %s3.sroa.0.2, %bb157 ], [ 0, %start ]
  %sp.sroa.0.098 = phi i64 [ %sp.sroa.0.1, %bb157 ], [ 0, %start ]
  %pc.sroa.0.097 = phi i64 [ %6, %bb157 ], [ 0, %start ]
  %i.sroa.0.096 = phi i64 [ %7, %bb157 ], [ 0, %start ]
  switch i64 %pc.sroa.0.097, label %bb17 [
    i64 0, label %bb24
    i64 1, label %bb6
    i64 2, label %bb8
    i64 3, label %bb10
    i64 4, label %bb12
    i64 5, label %bb14
    i64 6, label %bb16
  ]

bb24:                                             ; preds = %bb3, %bb16, %bb17, %bb14, %bb12, %bb10, %bb8, %bb6
  %op.sroa.0.0 = phi i64 [ %code7, %bb17 ], [ %code1, %bb6 ], [ %code2, %bb8 ], [ %code3, %bb10 ], [ %code4, %bb12 ], [ %code5, %bb14 ], [ %code6, %bb16 ], [ %code0, %bb3 ]
  switch i64 %op.sroa.0.0, label %bb61 [
    i64 0, label %bb160
    i64 96, label %bb27
  ]

bb6:                                              ; preds = %bb3
  br label %bb24

bb8:                                              ; preds = %bb3
  br label %bb24

bb10:                                             ; preds = %bb3
  br label %bb24

bb12:                                             ; preds = %bb3
  br label %bb24

bb14:                                             ; preds = %bb3
  br label %bb24

bb16:                                             ; preds = %bb3
  br label %bb24

bb17:                                             ; preds = %bb3
  br label %bb24

bb172.fold.split:                                 ; preds = %bb160
  br label %bb172

bb172:                                            ; preds = %bb160, %bb172.fold.split, %bb166, %bb167, %bb164
  %s0.sroa.0.1 = phi i64 [ %sp.sroa.0.0.lcssa, %bb160 ], [ %s1.sroa.0.0.lcssa, %bb164 ], [ %s2.sroa.0.0.lcssa, %bb166 ], [ %s3.sroa.0.0.lcssa, %bb167 ], [ %s0.sroa.0.0.lcssa, %bb172.fold.split ]
  ret i64 %s0.sroa.0.1

bb164:                                            ; preds = %bb160
  br label %bb172

bb166:                                            ; preds = %bb160
  br label %bb172

bb167:                                            ; preds = %bb160
  br label %bb172

bb27:                                             ; preds = %bb24
  %2 = add nuw nsw i64 %pc.sroa.0.097, 1
  %3 = icmp eq i64 %pc.sroa.0.097, 0
  br i1 %3, label %bb48, label %bb31

bb61:                                             ; preds = %bb24
  %4 = icmp eq i64 %op.sroa.0.0, 1
  %or.cond3 = icmp ult i64 %op.sroa.0.0, 3
  br i1 %or.cond3, label %bb65, label %bb101

bb48:                                             ; preds = %bb31, %bb27, %bb40, %bb41, %bb38, %bb36, %bb34
  %imm.sroa.0.0 = phi i64 [ %code7, %bb41 ], [ %code6, %bb40 ], [ %code1, %bb27 ], [ %code3, %bb34 ], [ %code4, %bb36 ], [ %code5, %bb38 ], [ %code2, %bb31 ]
  %_40 = icmp ult i64 %sp.sroa.0.098, 4
  br i1 %_40, label %bb49, label %bb157

bb31:                                             ; preds = %bb27
  switch i64 %2, label %bb41 [
    i64 2, label %bb48
    i64 3, label %bb34
    i64 4, label %bb36
    i64 5, label %bb38
    i64 6, label %bb40
  ]

bb34:                                             ; preds = %bb31
  br label %bb48

bb36:                                             ; preds = %bb31
  br label %bb48

bb38:                                             ; preds = %bb31
  br label %bb48

bb40:                                             ; preds = %bb31
  br label %bb48

bb41:                                             ; preds = %bb31
  br label %bb48

bb49:                                             ; preds = %bb48
  switch i64 %sp.sroa.0.098, label %bb55 [
    i64 0, label %bb58
    i64 1, label %bb52
    i64 2, label %bb54
  ]

bb58:                                             ; preds = %bb49, %bb54, %bb55, %bb52
  %s3.sroa.0.1 = phi i64 [ %imm.sroa.0.0, %bb55 ], [ %s3.sroa.0.099, %bb52 ], [ %s3.sroa.0.099, %bb54 ], [ %s3.sroa.0.099, %bb49 ]
  %s2.sroa.0.1 = phi i64 [ %s2.sroa.0.0100, %bb55 ], [ %s2.sroa.0.0100, %bb52 ], [ %imm.sroa.0.0, %bb54 ], [ %s2.sroa.0.0100, %bb49 ]
  %s1.sroa.0.1 = phi i64 [ %s1.sroa.0.0101, %bb55 ], [ %imm.sroa.0.0, %bb52 ], [ %s1.sroa.0.0101, %bb54 ], [ %s1.sroa.0.0101, %bb49 ]
  %s0.sroa.0.2 = phi i64 [ %s0.sroa.0.0102, %bb55 ], [ %s0.sroa.0.0102, %bb52 ], [ %s0.sroa.0.0102, %bb54 ], [ %imm.sroa.0.0, %bb49 ]
  %5 = add nuw nsw i64 %sp.sroa.0.098, 1
  br label %bb157

bb52:                                             ; preds = %bb49
  br label %bb58

bb54:                                             ; preds = %bb49
  br label %bb58

bb55:                                             ; preds = %bb49
  br label %bb58

bb157:                                            ; preds = %bb117, %bb112, %bb91, %bb95, %bb94, %bb84, %bb127, %bb147, %bb102, %bb121, %bb120, %bb65, %bb58, %bb48
  %pc.sroa.0.1 = phi i64 [ %2, %bb58 ], [ %2, %bb48 ], [ %pc.sroa.0.097, %bb127 ], [ %pc.sroa.0.097, %bb65 ], [ %pc.sroa.0.097, %bb91 ], [ %pc.sroa.0.097, %bb112 ], [ %pc.sroa.0.097, %bb120 ], [ %pc.sroa.0.097, %bb121 ], [ %pc.sroa.0.097, %bb102 ], [ %pc.sroa.0.097, %bb147 ], [ %pc.sroa.0.097, %bb84 ], [ %pc.sroa.0.097, %bb94 ], [ %pc.sroa.0.097, %bb95 ], [ %pc.sroa.0.097, %bb117 ]
  %sp.sroa.0.1 = phi i64 [ %5, %bb58 ], [ %sp.sroa.0.098, %bb48 ], [ %sp.sroa.0.098, %bb127 ], [ %sp.sroa.0.098, %bb65 ], [ %_50, %bb91 ], [ %sp.sroa.0.098, %bb112 ], [ %sp.sroa.0.098, %bb120 ], [ %sp.sroa.0.098, %bb121 ], [ 0, %bb102 ], [ %_99, %bb147 ], [ %_50, %bb84 ], [ %_50, %bb94 ], [ %_50, %bb95 ], [ %sp.sroa.0.098, %bb117 ]
  %s3.sroa.0.2 = phi i64 [ %s3.sroa.0.1, %bb58 ], [ %s3.sroa.0.099, %bb48 ], [ %s3.sroa.0.099, %bb127 ], [ %s3.sroa.0.099, %bb65 ], [ %s3.sroa.0.099, %bb91 ], [ %s3.sroa.0.099, %bb112 ], [ %s3.sroa.0.099, %bb120 ], [ %storage0.sroa.0.0.storage1.sroa.0.0, %bb121 ], [ %s3.sroa.0.099, %bb102 ], [ %s3.sroa.0.099, %bb147 ], [ %s3.sroa.0.099, %bb84 ], [ %s3.sroa.0.099, %bb94 ], [ %r.sroa.0.0, %bb95 ], [ %s3.sroa.0.099, %bb117 ]
  %s2.sroa.0.2 = phi i64 [ %s2.sroa.0.1, %bb58 ], [ %s2.sroa.0.0100, %bb48 ], [ %s2.sroa.0.0100, %bb127 ], [ %s2.sroa.0.0100, %bb65 ], [ %s2.sroa.0.0100, %bb91 ], [ %s2.sroa.0.0100, %bb112 ], [ %storage0.sroa.0.0.storage1.sroa.0.0, %bb120 ], [ %s2.sroa.0.0100, %bb121 ], [ %s2.sroa.0.0100, %bb102 ], [ %s2.sroa.0.0100, %bb147 ], [ %s2.sroa.0.0100, %bb84 ], [ %r.sroa.0.0, %bb94 ], [ %s2.sroa.0.0100, %bb95 ], [ %s2.sroa.0.0100, %bb117 ]
  %s1.sroa.0.2 = phi i64 [ %s1.sroa.0.1, %bb58 ], [ %s1.sroa.0.0101, %bb48 ], [ %s1.sroa.0.0101, %bb127 ], [ %s1.sroa.0.0101, %bb65 ], [ %r.sroa.0.0, %bb91 ], [ %s1.sroa.0.0101, %bb112 ], [ %s1.sroa.0.0101, %bb120 ], [ %s1.sroa.0.0101, %bb121 ], [ %s1.sroa.0.0101, %bb102 ], [ %s1.sroa.0.0101, %bb147 ], [ %s1.sroa.0.0101, %bb84 ], [ %s1.sroa.0.0101, %bb94 ], [ %s1.sroa.0.0101, %bb95 ], [ %storage0.sroa.0.0.storage1.sroa.0.0, %bb117 ]
  %s0.sroa.0.3 = phi i64 [ %s0.sroa.0.2, %bb58 ], [ %s0.sroa.0.0102, %bb48 ], [ %s0.sroa.0.0102, %bb127 ], [ %s0.sroa.0.0102, %bb65 ], [ %s0.sroa.0.0102, %bb91 ], [ %storage0.sroa.0.0.storage1.sroa.0.0, %bb112 ], [ %s0.sroa.0.0102, %bb120 ], [ %s0.sroa.0.0102, %bb121 ], [ %s0.sroa.0.0102, %bb102 ], [ %s0.sroa.0.0102, %bb147 ], [ %r.sroa.0.0, %bb84 ], [ %s0.sroa.0.0102, %bb94 ], [ %s0.sroa.0.0102, %bb95 ], [ %s0.sroa.0.0102, %bb117 ]
  %storage1.sroa.0.1 = phi i64 [ %storage1.sroa.0.0103, %bb58 ], [ %storage1.sroa.0.0103, %bb48 ], [ %storage1.sroa.0.0103, %bb127 ], [ %storage1.sroa.0.0103, %bb65 ], [ %storage1.sroa.0.0103, %bb91 ], [ %storage1.sroa.0.0103, %bb112 ], [ %storage1.sroa.0.0103, %bb120 ], [ %storage1.sroa.0.0103, %bb121 ], [ %storage1.sroa.0.0103, %bb102 ], [ %storage1.sroa.0.0.val1.sroa.0.0, %bb147 ], [ %storage1.sroa.0.0103, %bb84 ], [ %storage1.sroa.0.0103, %bb94 ], [ %storage1.sroa.0.0103, %bb95 ], [ %storage1.sroa.0.0103, %bb117 ]
  %storage0.sroa.0.1 = phi i64 [ %storage0.sroa.0.0104, %bb58 ], [ %storage0.sroa.0.0104, %bb48 ], [ %storage0.sroa.0.0104, %bb127 ], [ %storage0.sroa.0.0104, %bb65 ], [ %storage0.sroa.0.0104, %bb91 ], [ %storage0.sroa.0.0104, %bb112 ], [ %storage0.sroa.0.0104, %bb120 ], [ %storage0.sroa.0.0104, %bb121 ], [ %storage0.sroa.0.0104, %bb102 ], [ %val1.sroa.0.0.storage0.sroa.0.0, %bb147 ], [ %storage0.sroa.0.0104, %bb84 ], [ %storage0.sroa.0.0104, %bb94 ], [ %storage0.sroa.0.0104, %bb95 ], [ %storage0.sroa.0.0104, %bb117 ]
  %6 = add i64 %pc.sroa.0.1, 1
  %7 = add nuw i64 %i.sroa.0.096, 1
  %_18 = icmp ult i64 %7, %steps
  %_20 = icmp ult i64 %6, 8
  %or.cond = select i1 %_18, i1 %_20, i1 false
  br i1 %or.cond, label %bb3, label %bb160

bb65:                                             ; preds = %bb61
  %_47 = icmp ugt i64 %sp.sroa.0.098, 1
  br i1 %_47, label %bb66, label %bb157

bb101:                                            ; preds = %bb61
  %8 = icmp eq i64 %op.sroa.0.0, 84
  br i1 %8, label %bb102, label %bb127

bb66:                                             ; preds = %bb65
  %_50 = add i64 %sp.sroa.0.098, -1
  switch i64 %_50, label %bb72 [
    i64 1, label %bb75
    i64 2, label %bb71
  ]

bb75:                                             ; preds = %bb66, %bb71, %bb72
  %a.sroa.0.0 = phi i64 [ %s3.sroa.0.099, %bb72 ], [ %s2.sroa.0.0100, %bb71 ], [ %s1.sroa.0.0101, %bb66 ]
  %_57 = add i64 %sp.sroa.0.098, -2
  %9 = icmp eq i64 %_57, 0
  br i1 %9, label %bb84, label %bb77

bb71:                                             ; preds = %bb66
  br label %bb75

bb72:                                             ; preds = %bb66
  br label %bb75

bb77:                                             ; preds = %bb75
  switch i64 %_57, label %bb81 [
    i64 1, label %bb84
    i64 2, label %bb80
  ]

bb84:                                             ; preds = %bb77, %bb75, %bb80, %bb81
  %b.sroa.0.0 = phi i64 [ %s3.sroa.0.099, %bb81 ], [ %s0.sroa.0.0102, %bb75 ], [ %s2.sroa.0.0100, %bb80 ], [ %s1.sroa.0.0101, %bb77 ]
  %_0.i = add i64 %b.sroa.0.0, %a.sroa.0.0
  %_0.i94 = mul i64 %b.sroa.0.0, %a.sroa.0.0
  %r.sroa.0.0 = select i1 %4, i64 %_0.i, i64 %_0.i94
  br i1 %9, label %bb157, label %bb91

bb80:                                             ; preds = %bb77
  br label %bb84

bb81:                                             ; preds = %bb77
  br label %bb84

bb91:                                             ; preds = %bb84
  switch i64 %_57, label %bb95 [
    i64 1, label %bb157
    i64 2, label %bb94
  ]

bb94:                                             ; preds = %bb91
  br label %bb157

bb95:                                             ; preds = %bb91
  br label %bb157

bb102:                                            ; preds = %bb101
  %_71.not = icmp eq i64 %sp.sroa.0.098, 0
  br i1 %_71.not, label %bb157, label %bb103

bb127:                                            ; preds = %bb101
  %10 = icmp eq i64 %op.sroa.0.0, 85
  %_88 = icmp ugt i64 %sp.sroa.0.098, 1
  %or.cond4 = select i1 %10, i1 %_88, i1 false
  br i1 %or.cond4, label %bb129, label %bb157

bb103:                                            ; preds = %bb102
  %_75 = add i64 %sp.sroa.0.098, -1
  %11 = icmp eq i64 %_75, 0
  br i1 %11, label %bb112, label %bb105

bb105:                                            ; preds = %bb103
  switch i64 %_75, label %bb109 [
    i64 1, label %bb112
    i64 2, label %bb108
  ]

bb112:                                            ; preds = %bb105, %bb103, %bb108, %bb109
  %_74.sroa.0.0 = phi i64 [ %s3.sroa.0.099, %bb109 ], [ %s0.sroa.0.0102, %bb103 ], [ %s2.sroa.0.0100, %bb108 ], [ %s1.sroa.0.0101, %bb105 ]
  %key = and i64 %_74.sroa.0.0, 1
  %12 = icmp eq i64 %key, 0
  %storage0.sroa.0.0.storage1.sroa.0.0 = select i1 %12, i64 %storage0.sroa.0.0104, i64 %storage1.sroa.0.0103
  br i1 %11, label %bb157, label %bb117

bb108:                                            ; preds = %bb105
  br label %bb112

bb109:                                            ; preds = %bb105
  br label %bb112

bb117:                                            ; preds = %bb112
  switch i64 %_75, label %bb121 [
    i64 1, label %bb157
    i64 2, label %bb120
  ]

bb120:                                            ; preds = %bb117
  br label %bb157

bb121:                                            ; preds = %bb117
  br label %bb157

bb129:                                            ; preds = %bb127
  switch i64 %sp.sroa.0.098, label %bb135 [
    i64 2, label %bb138
    i64 3, label %bb134
  ]

bb138:                                            ; preds = %bb129, %bb134, %bb135
  %_91.sroa.0.0 = phi i64 [ %s3.sroa.0.099, %bb135 ], [ %s2.sroa.0.0100, %bb134 ], [ %s1.sroa.0.0101, %bb129 ]
  %key2 = and i64 %_91.sroa.0.0, 1
  %_99 = add i64 %sp.sroa.0.098, -2
  switch i64 %_99, label %bb144 [
    i64 0, label %bb147
    i64 1, label %bb141
    i64 2, label %bb143
  ]

bb134:                                            ; preds = %bb129
  br label %bb138

bb135:                                            ; preds = %bb129
  br label %bb138

bb147:                                            ; preds = %bb138, %bb143, %bb144, %bb141
  %val1.sroa.0.0 = phi i64 [ %s3.sroa.0.099, %bb144 ], [ %s1.sroa.0.0101, %bb141 ], [ %s2.sroa.0.0100, %bb143 ], [ %s0.sroa.0.0102, %bb138 ]
  %13 = icmp eq i64 %key2, 0
  %storage1.sroa.0.0.val1.sroa.0.0 = select i1 %13, i64 %storage1.sroa.0.0103, i64 %val1.sroa.0.0
  %val1.sroa.0.0.storage0.sroa.0.0 = select i1 %13, i64 %val1.sroa.0.0, i64 %storage0.sroa.0.0104
  br label %bb157

bb141:                                            ; preds = %bb138
  br label %bb147

bb143:                                            ; preds = %bb138
  br label %bb147

bb144:                                            ; preds = %bb138
  br label %bb147
}

attributes #0 = { nofree norecurse noreturn nosync nounwind memory(none) uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #1 = { nofree norecurse nosync nounwind memory(none) uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }

!llvm.module.flags = !{!0, !1, !2}
!llvm.ident = !{!3}

!0 = !{i32 8, !"PIC Level", i32 2}
!1 = !{i32 7, !"uwtable", i32 2}
!2 = !{i32 7, !"frame-pointer", i32 1}
!3 = !{!"rustc version 1.99.0 (b940084d7 2026-09-28)"}
!4 = !{i64 877453575045248192}
!5 = !{i64 -4000697234280174477}
