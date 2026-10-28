; ModuleID = 'frame_scalar.5efb8ade2a0a4ae6-cgu.0'
source_filename = "frame_scalar.5efb8ade2a0a4ae6-cgu.0"
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
define noundef i64 @sum_with_array(i64 noundef %a0, i64 noundef %a1, i64 noundef %n) unnamed_addr #1 !guid !5 {
start:
  %arr = alloca [16 x i8], align 8
  call void @llvm.lifetime.start.p0(ptr nonnull %arr)
  store i64 %a0, ptr %arr, align 8
  %0 = getelementptr inbounds nuw i8, ptr %arr, i64 8
  store i64 %a1, ptr %0, align 8
  %_64.not = icmp eq i64 %n, 0
  br i1 %_64.not, label %bb5, label %bb3.preheader

bb3.preheader:                                    ; preds = %start
  %xtraiter = and i64 %n, 3
  %1 = icmp ult i64 %n, 4
  br i1 %1, label %bb3.epil.preheader, label %bb3.preheader.new

bb3.preheader.new:                                ; preds = %bb3.preheader
  %unroll_iter = and i64 %n, -4
  br label %bb3

bb5.loopexit.unr-lcssa:                           ; preds = %bb3
  %bin.rdx = add i64 %_0.i.1, %_0.i
  %bin.rdx9 = add i64 %_0.i.2, %bin.rdx
  %bin.rdx10 = add i64 %_0.i.3, %bin.rdx9
  %lcmp.mod.not = icmp eq i64 %xtraiter, 0
  br i1 %lcmp.mod.not, label %bb5, label %bb3.epil.preheader

bb3.epil.preheader:                               ; preds = %bb5.loopexit.unr-lcssa, %bb3.preheader
  %acc.sroa.0.06.epil.init = phi i64 [ 0, %bb3.preheader ], [ %bin.rdx10, %bb5.loopexit.unr-lcssa ]
  %i.sroa.0.05.epil.init = phi i64 [ 0, %bb3.preheader ], [ %4, %bb5.loopexit.unr-lcssa ]
  %lcmp.mod8 = icmp ne i64 %xtraiter, 0
  tail call void @llvm.assume(i1 %lcmp.mod8)
  br label %bb3.epil

bb3.epil:                                         ; preds = %bb3.epil, %bb3.epil.preheader
  %acc.sroa.0.06.epil = phi i64 [ %_0.i.epil, %bb3.epil ], [ %acc.sroa.0.06.epil.init, %bb3.epil.preheader ]
  %i.sroa.0.05.epil = phi i64 [ %3, %bb3.epil ], [ %i.sroa.0.05.epil.init, %bb3.epil.preheader ]
  %epil.iter = phi i64 [ %epil.iter.next, %bb3.epil ], [ 0, %bb3.epil.preheader ]
  %_11.epil = and i64 %i.sroa.0.05.epil, 1
  %2 = getelementptr inbounds nuw [8 x i8], ptr %arr, i64 %_11.epil
  %_9.epil = load i64, ptr %2, align 8, !noundef !6
  %_0.i.epil = add i64 %_9.epil, %acc.sroa.0.06.epil
  %3 = add nuw i64 %i.sroa.0.05.epil, 1
  %epil.iter.next = add i64 %epil.iter, 1
  %epil.iter.cmp.not = icmp eq i64 %epil.iter.next, %xtraiter
  br i1 %epil.iter.cmp.not, label %bb5, label %bb3.epil, !llvm.loop !7

bb5:                                              ; preds = %bb5.loopexit.unr-lcssa, %bb3.epil, %start
  %acc.sroa.0.0.lcssa = phi i64 [ 0, %start ], [ %bin.rdx10, %bb5.loopexit.unr-lcssa ], [ %_0.i.epil, %bb3.epil ]
  call void @llvm.lifetime.end.p0(ptr nonnull %arr)
  ret i64 %acc.sroa.0.0.lcssa

bb3:                                              ; preds = %bb3, %bb3.preheader.new
  %acc.sroa.0.06.1 = phi i64 [ %_0.i.1, %bb3 ], [ 0, %bb3.preheader.new ]
  %acc.sroa.0.06.2 = phi i64 [ %_0.i.2, %bb3 ], [ 0, %bb3.preheader.new ]
  %acc.sroa.0.06.3 = phi i64 [ %_0.i.3, %bb3 ], [ 0, %bb3.preheader.new ]
  %acc.sroa.0.06 = phi i64 [ %_0.i, %bb3 ], [ 0, %bb3.preheader.new ]
  %i.sroa.0.05 = phi i64 [ %4, %bb3 ], [ 0, %bb3.preheader.new ]
  %niter = phi i64 [ %niter.next.3, %bb3 ], [ 0, %bb3.preheader.new ]
  %_0.i = add i64 %a0, %acc.sroa.0.06
  %_0.i.1 = add i64 %a1, %acc.sroa.0.06.1
  %_0.i.2 = add i64 %a0, %acc.sroa.0.06.2
  %_0.i.3 = add i64 %a1, %acc.sroa.0.06.3
  %4 = add nuw i64 %i.sroa.0.05, 4
  %niter.next.3 = add i64 %niter, 4
  %niter.ncmp.3 = icmp eq i64 %niter.next.3, %unroll_iter
  br i1 %niter.ncmp.3, label %bb5.loopexit.unr-lcssa, label %bb3
}

; Function Attrs: mustprogress nocallback nofree nosync nounwind willreturn memory(argmem: readwrite)
declare void @llvm.lifetime.start.p0(ptr captures(none)) #2

; Function Attrs: mustprogress nocallback nofree nosync nounwind willreturn memory(argmem: readwrite)
declare void @llvm.lifetime.end.p0(ptr captures(none)) #2

; Function Attrs: nocallback nofree nosync nounwind willreturn memory(inaccessiblemem: write)
declare void @llvm.assume(i1 noundef) #3

attributes #0 = { nofree norecurse noreturn nosync nounwind memory(none) uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #1 = { nofree norecurse nosync nounwind memory(none) uwtable "frame-pointer"="non-leaf" "probe-stack"="inline-asm" "target-cpu"="apple-m1" }
attributes #2 = { mustprogress nocallback nofree nosync nounwind willreturn memory(argmem: readwrite) }
attributes #3 = { nocallback nofree nosync nounwind willreturn memory(inaccessiblemem: write) }

!llvm.module.flags = !{!0, !1, !2}
!llvm.ident = !{!3}

!0 = !{i32 8, !"PIC Level", i32 2}
!1 = !{i32 7, !"uwtable", i32 2}
!2 = !{i32 7, !"frame-pointer", i32 1}
!3 = !{!"rustc version 1.99.0 (b940084d7 2026-09-28)"}
!4 = !{i64 877453575045248192}
!5 = !{i64 4713771214125645659}
!6 = !{}
!7 = distinct !{!7, !8}
!8 = !{!"llvm.loop.unroll.disable"}
