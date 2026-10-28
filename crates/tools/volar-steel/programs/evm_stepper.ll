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
  %_18179199.not = icmp eq i64 %steps, 0
  br i1 %_18179199.not, label %bb262, label %bb3.lr.ph

bb3.lr.ph:                                        ; preds = %start, %bb259
  %storage0.sroa.0.0.ph208 = phi i64 [ %storage0.sroa.0.1, %bb259 ], [ %0, %start ]
  %storage1.sroa.0.0.ph207 = phi i64 [ %storage1.sroa.0.1, %bb259 ], [ %1, %start ]
  %s0.sroa.0.0.ph206 = phi i64 [ %s0.sroa.0.3, %bb259 ], [ 0, %start ]
  %s1.sroa.0.0.ph205 = phi i64 [ %s1.sroa.0.2, %bb259 ], [ 0, %start ]
  %s2.sroa.0.0.ph204 = phi i64 [ %s2.sroa.0.2, %bb259 ], [ 0, %start ]
  %s3.sroa.0.0.ph203 = phi i64 [ %s3.sroa.0.2, %bb259 ], [ 0, %start ]
  %sp.sroa.0.0.ph202 = phi i64 [ %sp.sroa.0.1, %bb259 ], [ 0, %start ]
  %pc.sroa.0.0.ph201 = phi i64 [ %7, %bb259 ], [ 0, %start ]
  %i.sroa.0.0.ph200 = phi i64 [ %8, %bb259 ], [ 0, %start ]
  br label %bb3

bb262:                                            ; preds = %bb259, %bb89, %bb24, %start
  %s3.sroa.0.0.ph.lcssa = phi i64 [ %s3.sroa.0.0.ph203, %bb89 ], [ 0, %start ], [ %s3.sroa.0.0.ph203, %bb24 ], [ %s3.sroa.0.2, %bb259 ]
  %s2.sroa.0.0.ph.lcssa = phi i64 [ %s2.sroa.0.0.ph204, %bb89 ], [ 0, %start ], [ %s2.sroa.0.0.ph204, %bb24 ], [ %s2.sroa.0.2, %bb259 ]
  %s1.sroa.0.0.ph.lcssa = phi i64 [ %s1.sroa.0.0.ph205, %bb89 ], [ 0, %start ], [ %s1.sroa.0.0.ph205, %bb24 ], [ %s1.sroa.0.2, %bb259 ]
  %s0.sroa.0.0.ph.lcssa = phi i64 [ %s0.sroa.0.0.ph206, %bb89 ], [ 0, %start ], [ %s0.sroa.0.0.ph206, %bb24 ], [ %s0.sroa.0.3, %bb259 ]
  %sp.sroa.0.0.lcssa = phi i64 [ %sp.sroa.0.2, %bb89 ], [ 0, %start ], [ %sp.sroa.0.0184, %bb24 ], [ %sp.sroa.0.1, %bb259 ]
  switch i64 %sp.sroa.0.0.lcssa, label %bb269 [
    i64 0, label %bb274
    i64 1, label %bb274.fold.split
    i64 2, label %bb266
    i64 3, label %bb268
  ]

bb3:                                              ; preds = %bb3.lr.ph, %bb89
  %sp.sroa.0.0184 = phi i64 [ %sp.sroa.0.0.ph202, %bb3.lr.ph ], [ %sp.sroa.0.2, %bb89 ]
  %pc.sroa.0.0183 = phi i64 [ %pc.sroa.0.0.ph201, %bb3.lr.ph ], [ %dest.sroa.0.0, %bb89 ]
  %i.sroa.0.0182 = phi i64 [ %i.sroa.0.0.ph200, %bb3.lr.ph ], [ %9, %bb89 ]
  switch i64 %pc.sroa.0.0183, label %bb17 [
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
    i64 0, label %bb262
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

bb274.fold.split:                                 ; preds = %bb262
  br label %bb274

bb274:                                            ; preds = %bb262, %bb274.fold.split, %bb268, %bb269, %bb266
  %s0.sroa.0.1 = phi i64 [ %sp.sroa.0.0.lcssa, %bb262 ], [ %s1.sroa.0.0.ph.lcssa, %bb266 ], [ %s2.sroa.0.0.ph.lcssa, %bb268 ], [ %s3.sroa.0.0.ph.lcssa, %bb269 ], [ %s0.sroa.0.0.ph.lcssa, %bb274.fold.split ]
  ret i64 %s0.sroa.0.1

bb266:                                            ; preds = %bb262
  br label %bb274

bb268:                                            ; preds = %bb262
  br label %bb274

bb269:                                            ; preds = %bb262
  br label %bb274

bb27:                                             ; preds = %bb24
  %2 = add nuw nsw i64 %pc.sroa.0.0183, 1
  %3 = icmp eq i64 %pc.sroa.0.0183, 0
  br i1 %3, label %bb48, label %bb31

bb61:                                             ; preds = %bb24
  %4 = icmp eq i64 %op.sroa.0.0, 86
  %5 = and i64 %op.sroa.0.0, -2
  %or.cond5 = icmp eq i64 %5, 86
  br i1 %or.cond5, label %bb65, label %bb93

bb48:                                             ; preds = %bb31, %bb27, %bb40, %bb41, %bb38, %bb36, %bb34
  %imm.sroa.0.0 = phi i64 [ %code7, %bb41 ], [ %code6, %bb40 ], [ %code1, %bb27 ], [ %code3, %bb34 ], [ %code4, %bb36 ], [ %code5, %bb38 ], [ %code2, %bb31 ]
  %_40 = icmp ult i64 %sp.sroa.0.0184, 4
  br i1 %_40, label %bb49, label %bb259

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
  switch i64 %sp.sroa.0.0184, label %bb55 [
    i64 0, label %bb58
    i64 1, label %bb52
    i64 2, label %bb54
  ]

bb58:                                             ; preds = %bb49, %bb54, %bb55, %bb52
  %s3.sroa.0.1 = phi i64 [ %imm.sroa.0.0, %bb55 ], [ %s3.sroa.0.0.ph203, %bb52 ], [ %s3.sroa.0.0.ph203, %bb54 ], [ %s3.sroa.0.0.ph203, %bb49 ]
  %s2.sroa.0.1 = phi i64 [ %s2.sroa.0.0.ph204, %bb55 ], [ %s2.sroa.0.0.ph204, %bb52 ], [ %imm.sroa.0.0, %bb54 ], [ %s2.sroa.0.0.ph204, %bb49 ]
  %s1.sroa.0.1 = phi i64 [ %s1.sroa.0.0.ph205, %bb55 ], [ %imm.sroa.0.0, %bb52 ], [ %s1.sroa.0.0.ph205, %bb54 ], [ %s1.sroa.0.0.ph205, %bb49 ]
  %s0.sroa.0.2 = phi i64 [ %s0.sroa.0.0.ph206, %bb55 ], [ %s0.sroa.0.0.ph206, %bb52 ], [ %s0.sroa.0.0.ph206, %bb54 ], [ %imm.sroa.0.0, %bb49 ]
  %6 = add nuw nsw i64 %sp.sroa.0.0184, 1
  br label %bb259

bb52:                                             ; preds = %bb49
  br label %bb58

bb54:                                             ; preds = %bb49
  br label %bb58

bb55:                                             ; preds = %bb49
  br label %bb58

bb259:                                            ; preds = %bb77, %bb65, %bb87, %bb93, %bb216, %bb211, %bb190, %bb194, %bb193, %bb188, %bb111, %bb106, %bb226, %bb246, %bb201, %bb220, %bb219, %bb135, %bb96, %bb115, %bb114, %bb58, %bb48
  %pc.sroa.0.1 = phi i64 [ %2, %bb58 ], [ %2, %bb48 ], [ %pc.sroa.0.0183, %bb193 ], [ %pc.sroa.0.0183, %bb226 ], [ %pc.sroa.0.0183, %bb194 ], [ %pc.sroa.0.0183, %bb93 ], [ %pc.sroa.0.0183, %bb216 ], [ %pc.sroa.0.0183, %bb106 ], [ %pc.sroa.0.0183, %bb114 ], [ %pc.sroa.0.0183, %bb115 ], [ %pc.sroa.0.0183, %bb96 ], [ %pc.sroa.0.0183, %bb111 ], [ %pc.sroa.0.0183, %bb135 ], [ %pc.sroa.0.0183, %bb190 ], [ %pc.sroa.0.0183, %bb211 ], [ %pc.sroa.0.0183, %bb219 ], [ %pc.sroa.0.0183, %bb220 ], [ %pc.sroa.0.0183, %bb201 ], [ %pc.sroa.0.0183, %bb246 ], [ %pc.sroa.0.0183, %bb188 ], [ %pc.sroa.0.0183, %bb87 ], [ %pc.sroa.0.0183, %bb65 ], [ %pc.sroa.0.0183, %bb77 ]
  %sp.sroa.0.1 = phi i64 [ %6, %bb58 ], [ %sp.sroa.0.0184, %bb48 ], [ %_94, %bb193 ], [ %sp.sroa.0.0184, %bb226 ], [ %_94, %bb194 ], [ %sp.sroa.0.0184, %bb93 ], [ %sp.sroa.0.0184, %bb216 ], [ %sp.sroa.0.0184, %bb106 ], [ %sp.sroa.0.0184, %bb114 ], [ %sp.sroa.0.0184, %bb115 ], [ 0, %bb96 ], [ %sp.sroa.0.0184, %bb111 ], [ %sp.sroa.0.0184, %bb135 ], [ %_94, %bb190 ], [ %sp.sroa.0.0184, %bb211 ], [ %sp.sroa.0.0184, %bb219 ], [ %sp.sroa.0.0184, %bb220 ], [ 0, %bb201 ], [ %_153, %bb246 ], [ %_94, %bb188 ], [ 1, %bb77 ], [ 0, %bb65 ], [ %_60, %bb87 ]
  %s3.sroa.0.2 = phi i64 [ %s3.sroa.0.1, %bb58 ], [ %s3.sroa.0.0.ph203, %bb48 ], [ %s3.sroa.0.0.ph203, %bb193 ], [ %s3.sroa.0.0.ph203, %bb226 ], [ %r2.sroa.0.0, %bb194 ], [ %s3.sroa.0.0.ph203, %bb93 ], [ %s3.sroa.0.0.ph203, %bb216 ], [ %s3.sroa.0.0.ph203, %bb106 ], [ %s3.sroa.0.0.ph203, %bb114 ], [ %., %bb115 ], [ %s3.sroa.0.0.ph203, %bb96 ], [ %s3.sroa.0.0.ph203, %bb111 ], [ %s3.sroa.0.0.ph203, %bb135 ], [ %s3.sroa.0.0.ph203, %bb190 ], [ %s3.sroa.0.0.ph203, %bb211 ], [ %s3.sroa.0.0.ph203, %bb219 ], [ %storage0.sroa.0.0.storage1.sroa.0.0, %bb220 ], [ %s3.sroa.0.0.ph203, %bb201 ], [ %s3.sroa.0.0.ph203, %bb246 ], [ %s3.sroa.0.0.ph203, %bb188 ], [ %s3.sroa.0.0.ph203, %bb87 ], [ %s3.sroa.0.0.ph203, %bb65 ], [ %s3.sroa.0.0.ph203, %bb77 ]
  %s2.sroa.0.2 = phi i64 [ %s2.sroa.0.1, %bb58 ], [ %s2.sroa.0.0.ph204, %bb48 ], [ %r2.sroa.0.0, %bb193 ], [ %s2.sroa.0.0.ph204, %bb226 ], [ %s2.sroa.0.0.ph204, %bb194 ], [ %s2.sroa.0.0.ph204, %bb93 ], [ %s2.sroa.0.0.ph204, %bb216 ], [ %s2.sroa.0.0.ph204, %bb106 ], [ %., %bb114 ], [ %s2.sroa.0.0.ph204, %bb115 ], [ %s2.sroa.0.0.ph204, %bb96 ], [ %s2.sroa.0.0.ph204, %bb111 ], [ %s2.sroa.0.0.ph204, %bb135 ], [ %s2.sroa.0.0.ph204, %bb190 ], [ %s2.sroa.0.0.ph204, %bb211 ], [ %storage0.sroa.0.0.storage1.sroa.0.0, %bb219 ], [ %s2.sroa.0.0.ph204, %bb220 ], [ %s2.sroa.0.0.ph204, %bb201 ], [ %s2.sroa.0.0.ph204, %bb246 ], [ %s2.sroa.0.0.ph204, %bb188 ], [ %s2.sroa.0.0.ph204, %bb87 ], [ %s2.sroa.0.0.ph204, %bb65 ], [ %s2.sroa.0.0.ph204, %bb77 ]
  %s1.sroa.0.2 = phi i64 [ %s1.sroa.0.1, %bb58 ], [ %s1.sroa.0.0.ph205, %bb48 ], [ %s1.sroa.0.0.ph205, %bb193 ], [ %s1.sroa.0.0.ph205, %bb226 ], [ %s1.sroa.0.0.ph205, %bb194 ], [ %s1.sroa.0.0.ph205, %bb93 ], [ %storage0.sroa.0.0.storage1.sroa.0.0, %bb216 ], [ %s1.sroa.0.0.ph205, %bb106 ], [ %s1.sroa.0.0.ph205, %bb114 ], [ %s1.sroa.0.0.ph205, %bb115 ], [ %s1.sroa.0.0.ph205, %bb96 ], [ %., %bb111 ], [ %s1.sroa.0.0.ph205, %bb135 ], [ %r2.sroa.0.0, %bb190 ], [ %s1.sroa.0.0.ph205, %bb211 ], [ %s1.sroa.0.0.ph205, %bb219 ], [ %s1.sroa.0.0.ph205, %bb220 ], [ %s1.sroa.0.0.ph205, %bb201 ], [ %s1.sroa.0.0.ph205, %bb246 ], [ %s1.sroa.0.0.ph205, %bb188 ], [ %s1.sroa.0.0.ph205, %bb87 ], [ %s1.sroa.0.0.ph205, %bb65 ], [ %s1.sroa.0.0.ph205, %bb77 ]
  %s0.sroa.0.3 = phi i64 [ %s0.sroa.0.2, %bb58 ], [ %s0.sroa.0.0.ph206, %bb48 ], [ %s0.sroa.0.0.ph206, %bb193 ], [ %s0.sroa.0.0.ph206, %bb226 ], [ %s0.sroa.0.0.ph206, %bb194 ], [ %s0.sroa.0.0.ph206, %bb93 ], [ %s0.sroa.0.0.ph206, %bb216 ], [ %., %bb106 ], [ %s0.sroa.0.0.ph206, %bb114 ], [ %s0.sroa.0.0.ph206, %bb115 ], [ %s0.sroa.0.0.ph206, %bb96 ], [ %s0.sroa.0.0.ph206, %bb111 ], [ %s0.sroa.0.0.ph206, %bb135 ], [ %s0.sroa.0.0.ph206, %bb190 ], [ %storage0.sroa.0.0.storage1.sroa.0.0, %bb211 ], [ %s0.sroa.0.0.ph206, %bb219 ], [ %s0.sroa.0.0.ph206, %bb220 ], [ %s0.sroa.0.0.ph206, %bb201 ], [ %s0.sroa.0.0.ph206, %bb246 ], [ %r2.sroa.0.0, %bb188 ], [ %s0.sroa.0.0.ph206, %bb87 ], [ %s0.sroa.0.0.ph206, %bb65 ], [ %s0.sroa.0.0.ph206, %bb77 ]
  %storage1.sroa.0.1 = phi i64 [ %storage1.sroa.0.0.ph207, %bb58 ], [ %storage1.sroa.0.0.ph207, %bb48 ], [ %storage1.sroa.0.0.ph207, %bb193 ], [ %storage1.sroa.0.0.ph207, %bb226 ], [ %storage1.sroa.0.0.ph207, %bb194 ], [ %storage1.sroa.0.0.ph207, %bb93 ], [ %storage1.sroa.0.0.ph207, %bb216 ], [ %storage1.sroa.0.0.ph207, %bb106 ], [ %storage1.sroa.0.0.ph207, %bb114 ], [ %storage1.sroa.0.0.ph207, %bb115 ], [ %storage1.sroa.0.0.ph207, %bb96 ], [ %storage1.sroa.0.0.ph207, %bb111 ], [ %storage1.sroa.0.0.ph207, %bb135 ], [ %storage1.sroa.0.0.ph207, %bb190 ], [ %storage1.sroa.0.0.ph207, %bb211 ], [ %storage1.sroa.0.0.ph207, %bb219 ], [ %storage1.sroa.0.0.ph207, %bb220 ], [ %storage1.sroa.0.0.ph207, %bb201 ], [ %storage1.sroa.0.0.val3.sroa.0.0, %bb246 ], [ %storage1.sroa.0.0.ph207, %bb188 ], [ %storage1.sroa.0.0.ph207, %bb87 ], [ %storage1.sroa.0.0.ph207, %bb65 ], [ %storage1.sroa.0.0.ph207, %bb77 ]
  %storage0.sroa.0.1 = phi i64 [ %storage0.sroa.0.0.ph208, %bb58 ], [ %storage0.sroa.0.0.ph208, %bb48 ], [ %storage0.sroa.0.0.ph208, %bb193 ], [ %storage0.sroa.0.0.ph208, %bb226 ], [ %storage0.sroa.0.0.ph208, %bb194 ], [ %storage0.sroa.0.0.ph208, %bb93 ], [ %storage0.sroa.0.0.ph208, %bb216 ], [ %storage0.sroa.0.0.ph208, %bb106 ], [ %storage0.sroa.0.0.ph208, %bb114 ], [ %storage0.sroa.0.0.ph208, %bb115 ], [ %storage0.sroa.0.0.ph208, %bb96 ], [ %storage0.sroa.0.0.ph208, %bb111 ], [ %storage0.sroa.0.0.ph208, %bb135 ], [ %storage0.sroa.0.0.ph208, %bb190 ], [ %storage0.sroa.0.0.ph208, %bb211 ], [ %storage0.sroa.0.0.ph208, %bb219 ], [ %storage0.sroa.0.0.ph208, %bb220 ], [ %storage0.sroa.0.0.ph208, %bb201 ], [ %val3.sroa.0.0.storage0.sroa.0.0, %bb246 ], [ %storage0.sroa.0.0.ph208, %bb188 ], [ %storage0.sroa.0.0.ph208, %bb87 ], [ %storage0.sroa.0.0.ph208, %bb65 ], [ %storage0.sroa.0.0.ph208, %bb77 ]
  %7 = add i64 %pc.sroa.0.1, 1
  %8 = add i64 %i.sroa.0.0182, 1
  %_18179 = icmp ult i64 %8, %steps
  %_20180 = icmp ult i64 %7, 8
  %or.cond181 = select i1 %_18179, i1 %_20180, i1 false
  br i1 %or.cond181, label %bb3.lr.ph, label %bb262

bb65:                                             ; preds = %bb61
  %_47.not = icmp eq i64 %sp.sroa.0.0184, 0
  br i1 %_47.not, label %bb259, label %bb66

bb93:                                             ; preds = %bb61
  switch i64 %op.sroa.0.0, label %bb226 [
    i64 91, label %bb259
    i64 21, label %bb96
    i64 20, label %bb135
    i64 17, label %bb135
    i64 16, label %bb135
    i64 4, label %bb135
    i64 3, label %bb135
    i64 2, label %bb135
    i64 1, label %bb135
    i64 0, label %bb135
    i64 84, label %bb201
  ]

bb66:                                             ; preds = %bb65
  %_50 = add i64 %sp.sroa.0.0184, -1
  switch i64 %_50, label %bb72 [
    i64 0, label %bb75
    i64 1, label %bb69
    i64 2, label %bb71
  ]

bb75:                                             ; preds = %bb66, %bb71, %bb72, %bb69
  %dest.sroa.0.0 = phi i64 [ %s3.sroa.0.0.ph203, %bb72 ], [ %s1.sroa.0.0.ph205, %bb69 ], [ %s2.sroa.0.0.ph204, %bb71 ], [ %s0.sroa.0.0.ph206, %bb66 ]
  br i1 %4, label %bb89, label %bb77

bb69:                                             ; preds = %bb66
  br label %bb75

bb71:                                             ; preds = %bb66
  br label %bb75

bb72:                                             ; preds = %bb66
  br label %bb75

bb77:                                             ; preds = %bb75
  %_57.not = icmp eq i64 %sp.sroa.0.0184, 1
  br i1 %_57.not, label %bb259, label %bb78

bb89:                                             ; preds = %bb75, %bb87
  %sp.sroa.0.2 = phi i64 [ %_60, %bb87 ], [ %_50, %bb75 ]
  %9 = add i64 %i.sroa.0.0182, 1
  %_18 = icmp ult i64 %9, %steps
  %_20 = icmp ult i64 %dest.sroa.0.0, 8
  %or.cond = select i1 %_18, i1 %_20, i1 false
  br i1 %or.cond, label %bb3, label %bb262

bb78:                                             ; preds = %bb77
  %_60 = add i64 %sp.sroa.0.0184, -2
  switch i64 %_60, label %bb84 [
    i64 0, label %bb87
    i64 1, label %bb81
    i64 2, label %bb83
  ]

bb87:                                             ; preds = %bb78, %bb83, %bb84, %bb81
  %cond.sroa.0.0 = phi i64 [ %s3.sroa.0.0.ph203, %bb84 ], [ %s1.sroa.0.0.ph205, %bb81 ], [ %s2.sroa.0.0.ph204, %bb83 ], [ %s0.sroa.0.0.ph206, %bb78 ]
  %.not = icmp eq i64 %cond.sroa.0.0, 0
  br i1 %.not, label %bb259, label %bb89

bb81:                                             ; preds = %bb78
  br label %bb87

bb83:                                             ; preds = %bb78
  br label %bb87

bb84:                                             ; preds = %bb78
  br label %bb87

bb96:                                             ; preds = %bb93
  %_68.not = icmp eq i64 %sp.sroa.0.0184, 0
  br i1 %_68.not, label %bb259, label %bb97

bb97:                                             ; preds = %bb96
  %_71 = add i64 %sp.sroa.0.0184, -1
  %10 = icmp eq i64 %_71, 0
  br i1 %10, label %bb106, label %bb99

bb99:                                             ; preds = %bb97
  switch i64 %_71, label %bb103 [
    i64 1, label %bb106
    i64 2, label %bb102
  ]

bb106:                                            ; preds = %bb99, %bb97, %bb102, %bb103
  %a.sroa.0.0 = phi i64 [ %s3.sroa.0.0.ph203, %bb103 ], [ %s0.sroa.0.0.ph206, %bb97 ], [ %s2.sroa.0.0.ph204, %bb102 ], [ %s1.sroa.0.0.ph205, %bb99 ]
  %11 = icmp eq i64 %a.sroa.0.0, 0
  %. = zext i1 %11 to i64
  br i1 %10, label %bb259, label %bb111

bb102:                                            ; preds = %bb99
  br label %bb106

bb103:                                            ; preds = %bb99
  br label %bb106

bb111:                                            ; preds = %bb106
  switch i64 %_71, label %bb115 [
    i64 1, label %bb259
    i64 2, label %bb114
  ]

bb114:                                            ; preds = %bb111
  br label %bb259

bb115:                                            ; preds = %bb111
  br label %bb259

bb135:                                            ; preds = %bb93, %bb93, %bb93, %bb93, %bb93, %bb93, %bb93, %bb93
  %_91 = icmp ugt i64 %sp.sroa.0.0184, 1
  br i1 %_91, label %bb136, label %bb259

bb136:                                            ; preds = %bb135
  %_94 = add i64 %sp.sroa.0.0184, -1
  switch i64 %_94, label %bb142 [
    i64 1, label %bb145
    i64 2, label %bb141
  ]

bb145:                                            ; preds = %bb136, %bb141, %bb142
  %a1.sroa.0.0 = phi i64 [ %s3.sroa.0.0.ph203, %bb142 ], [ %s2.sroa.0.0.ph204, %bb141 ], [ %s1.sroa.0.0.ph205, %bb136 ]
  %_101 = add i64 %sp.sroa.0.0184, -2
  %12 = icmp eq i64 %_101, 0
  br i1 %12, label %bb154, label %bb147

bb141:                                            ; preds = %bb136
  br label %bb145

bb142:                                            ; preds = %bb136
  br label %bb145

bb147:                                            ; preds = %bb145
  switch i64 %_101, label %bb151 [
    i64 1, label %bb154
    i64 2, label %bb150
  ]

bb154:                                            ; preds = %bb147, %bb145, %bb150, %bb151
  %b.sroa.0.0 = phi i64 [ %s3.sroa.0.0.ph203, %bb151 ], [ %s0.sroa.0.0.ph206, %bb145 ], [ %s2.sroa.0.0.ph204, %bb150 ], [ %s1.sroa.0.0.ph205, %bb147 ]
  switch i64 %op.sroa.0.0, label %bb179 [
    i64 1, label %bb155
    i64 2, label %bb158
    i64 3, label %bb161
    i64 4, label %bb164
    i64 16, label %bb170
    i64 17, label %bb175
  ]

bb150:                                            ; preds = %bb147
  br label %bb154

bb151:                                            ; preds = %bb147
  br label %bb154

bb155:                                            ; preds = %bb154
  %_0.i = add i64 %b.sroa.0.0, %a1.sroa.0.0
  br label %bb188

bb188:                                            ; preds = %bb179, %bb175, %bb170, %bb164, %bb167, %bb161, %bb158, %bb155
  %r2.sroa.0.0 = phi i64 [ %_0.i, %bb155 ], [ %_0.i165, %bb158 ], [ %_0.i166, %bb161 ], [ %.164, %bb179 ], [ %14, %bb167 ], [ 0, %bb164 ], [ %.162, %bb170 ], [ %.163, %bb175 ]
  br i1 %12, label %bb259, label %bb190

bb158:                                            ; preds = %bb154
  %_0.i165 = mul i64 %b.sroa.0.0, %a1.sroa.0.0
  br label %bb188

bb161:                                            ; preds = %bb154
  %_0.i166 = sub i64 %a1.sroa.0.0, %b.sroa.0.0
  br label %bb188

bb164:                                            ; preds = %bb154
  %13 = icmp eq i64 %b.sroa.0.0, 0
  br i1 %13, label %bb188, label %bb167

bb167:                                            ; preds = %bb164
  %14 = udiv i64 %a1.sroa.0.0, %b.sroa.0.0
  br label %bb188

bb170:                                            ; preds = %bb154
  %_115 = icmp ult i64 %b.sroa.0.0, %a1.sroa.0.0
  %.162 = zext i1 %_115 to i64
  br label %bb188

bb175:                                            ; preds = %bb154
  %_116 = icmp ugt i64 %b.sroa.0.0, %a1.sroa.0.0
  %.163 = zext i1 %_116 to i64
  br label %bb188

bb179:                                            ; preds = %bb154
  %_117 = icmp eq i64 %a1.sroa.0.0, %b.sroa.0.0
  %.164 = zext i1 %_117 to i64
  br label %bb188

bb190:                                            ; preds = %bb188
  switch i64 %_101, label %bb194 [
    i64 1, label %bb259
    i64 2, label %bb193
  ]

bb193:                                            ; preds = %bb190
  br label %bb259

bb194:                                            ; preds = %bb190
  br label %bb259

bb201:                                            ; preds = %bb93
  %_125.not = icmp eq i64 %sp.sroa.0.0184, 0
  br i1 %_125.not, label %bb259, label %bb202

bb226:                                            ; preds = %bb93
  %15 = icmp eq i64 %op.sroa.0.0, 85
  %_142 = icmp ugt i64 %sp.sroa.0.0184, 1
  %or.cond12 = select i1 %15, i1 %_142, i1 false
  br i1 %or.cond12, label %bb228, label %bb259

bb202:                                            ; preds = %bb201
  %_129 = add i64 %sp.sroa.0.0184, -1
  %16 = icmp eq i64 %_129, 0
  br i1 %16, label %bb211, label %bb204

bb204:                                            ; preds = %bb202
  switch i64 %_129, label %bb208 [
    i64 1, label %bb211
    i64 2, label %bb207
  ]

bb211:                                            ; preds = %bb204, %bb202, %bb207, %bb208
  %_128.sroa.0.0 = phi i64 [ %s3.sroa.0.0.ph203, %bb208 ], [ %s0.sroa.0.0.ph206, %bb202 ], [ %s2.sroa.0.0.ph204, %bb207 ], [ %s1.sroa.0.0.ph205, %bb204 ]
  %key = and i64 %_128.sroa.0.0, 1
  %17 = icmp eq i64 %key, 0
  %storage0.sroa.0.0.storage1.sroa.0.0 = select i1 %17, i64 %storage0.sroa.0.0.ph208, i64 %storage1.sroa.0.0.ph207
  br i1 %16, label %bb259, label %bb216

bb207:                                            ; preds = %bb204
  br label %bb211

bb208:                                            ; preds = %bb204
  br label %bb211

bb216:                                            ; preds = %bb211
  switch i64 %_129, label %bb220 [
    i64 1, label %bb259
    i64 2, label %bb219
  ]

bb219:                                            ; preds = %bb216
  br label %bb259

bb220:                                            ; preds = %bb216
  br label %bb259

bb228:                                            ; preds = %bb226
  switch i64 %sp.sroa.0.0184, label %bb234 [
    i64 2, label %bb237
    i64 3, label %bb233
  ]

bb237:                                            ; preds = %bb228, %bb233, %bb234
  %_145.sroa.0.0 = phi i64 [ %s3.sroa.0.0.ph203, %bb234 ], [ %s2.sroa.0.0.ph204, %bb233 ], [ %s1.sroa.0.0.ph205, %bb228 ]
  %key4 = and i64 %_145.sroa.0.0, 1
  %_153 = add i64 %sp.sroa.0.0184, -2
  switch i64 %_153, label %bb243 [
    i64 0, label %bb246
    i64 1, label %bb240
    i64 2, label %bb242
  ]

bb233:                                            ; preds = %bb228
  br label %bb237

bb234:                                            ; preds = %bb228
  br label %bb237

bb246:                                            ; preds = %bb237, %bb242, %bb243, %bb240
  %val3.sroa.0.0 = phi i64 [ %s3.sroa.0.0.ph203, %bb243 ], [ %s1.sroa.0.0.ph205, %bb240 ], [ %s2.sroa.0.0.ph204, %bb242 ], [ %s0.sroa.0.0.ph206, %bb237 ]
  %18 = icmp eq i64 %key4, 0
  %storage1.sroa.0.0.val3.sroa.0.0 = select i1 %18, i64 %storage1.sroa.0.0.ph207, i64 %val3.sroa.0.0
  %val3.sroa.0.0.storage0.sroa.0.0 = select i1 %18, i64 %val3.sroa.0.0, i64 %storage0.sroa.0.0.ph208
  br label %bb259

bb240:                                            ; preds = %bb237
  br label %bb246

bb242:                                            ; preds = %bb237
  br label %bb246

bb243:                                            ; preds = %bb237
  br label %bb246
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
