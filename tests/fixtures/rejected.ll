; Each loop exercises a distinct conservative bailout.

define void @loop_carried(ptr noalias %data, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %source.ptr = getelementptr inbounds i32, ptr %data, i64 %i
  %source = load i32, ptr %source.ptr, align 4
  %destination.index = add i64 %i, 1
  %destination.ptr = getelementptr inbounds i32, ptr %data, i64 %destination.index
  store i32 %source, ptr %destination.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

define void @possibly_aliasing(ptr %out, ptr %input, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %input.ptr = getelementptr inbounds i32, ptr %input, i64 %i
  %value = load i32, ptr %input.ptr, align 4
  %out.ptr = getelementptr inbounds i32, ptr %out, i64 %i
  store i32 %value, ptr %out.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

define void @volatile_access(ptr noalias %out, ptr noalias %input, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %input.ptr = getelementptr inbounds i32, ptr %input, i64 %i
  %value = load volatile i32, ptr %input.ptr, align 4
  %out.ptr = getelementptr inbounds i32, ptr %out, i64 %i
  store i32 %value, ptr %out.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

declare i32 @opaque_operation(i32)

define void @contains_call(ptr noalias %out, ptr noalias %input, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %input.ptr = getelementptr inbounds i32, ptr %input, i64 %i
  %value = load i32, ptr %input.ptr, align 4
  %result = call i32 @opaque_operation(i32 %value)
  %out.ptr = getelementptr inbounds i32, ptr %out, i64 %i
  store i32 %result, ptr %out.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

define void @explicitly_disabled(ptr noalias %out, ptr noalias %input, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %input.ptr = getelementptr inbounds i32, ptr %input, i64 %i
  %value = load i32, ptr %input.ptr, align 4
  %out.ptr = getelementptr inbounds i32, ptr %out, i64 %i
  store i32 %value, ptr %out.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop, !llvm.loop !0

exit:
  ret void
}

define i32 @value_live_out(ptr noalias %input) {
entry:
  br label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %input.ptr = getelementptr inbounds i32, ptr %input, i64 %i
  %value = load i32, ptr %input.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, 16
  br i1 %done, label %exit, label %loop

exit:
  ret i32 %value
}

define void @optimization_disabled(ptr noalias %out, ptr noalias %input, i64 %n) #0 {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %input.ptr = getelementptr inbounds i32, ptr %input, i64 %i
  %value = load i32, ptr %input.ptr, align 4
  %out.ptr = getelementptr inbounds i32, ptr %out, i64 %i
  store i32 %value, ptr %out.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

define void @indirect_preheader(ptr noalias %out, i64 %n) {
entry:
  indirectbr ptr blockaddress(@indirect_preheader, %loop), [label %loop]

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %out.ptr = getelementptr inbounds i32, ptr %out, i64 %i
  store i32 7, ptr %out.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

define void @overflowing_affine_offset(ptr noalias %out, i64 %n) {
entry:
  br label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %impossible.offset = sub i64 %i, -9223372036854775808
  %out.ptr = getelementptr i32, ptr %out, i64 %impossible.offset
  store i32 9, ptr %out.ptr, align 4
  %next = add i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

define void @constant_latch_condition(ptr noalias %out) {
entry:
  br label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %out.ptr = getelementptr i32, ptr %out, i64 %i
  store i32 11, ptr %out.ptr, align 4
  %next = add i64 %i, 1
  br i1 true, label %loop, label %exit

exit:
  ret void
}

; A value written in iteration i-1 is read in iteration i (true/RAW
; recurrence), so grouping iterations changes the value observed by the load.
define void @raw_recurrence(ptr noalias %data, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %source.index = sub i64 %i, 1
  %source.ptr = getelementptr i32, ptr %data, i64 %source.index
  %value = load i32, ptr %source.ptr, align 4
  %destination.ptr = getelementptr i32, ptr %data, i64 %i
  store i32 %value, ptr %destination.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; Iteration i reads the location overwritten by iteration i+1 (WAR/anti
; dependence).
define void @war_recurrence(ptr noalias %data, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %source.index = add i64 %i, 1
  %source.ptr = getelementptr i32, ptr %data, i64 %source.index
  %value = load i32, ptr %source.ptr, align 4
  %destination.ptr = getelementptr i32, ptr %data, i64 %i
  store i32 %value, ptr %destination.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; The second store of iteration i aliases the first store of iteration i+1.
define void @shifted_waw_recurrence(ptr noalias %data, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %first.ptr = getelementptr i32, ptr %data, i64 %i
  store i32 1, ptr %first.ptr, align 4
  %second.index = add i64 %i, 1
  %second.ptr = getelementptr i32, ptr %data, i64 %second.index
  store i32 2, ptr %second.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; The transformer synthesizes its own chunk increment and therefore may not
; silently discard an ordinary data use of the scalar increment.
define void @induction_next_data_use(ptr noalias %out, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %out.ptr = getelementptr i64, ptr %out, i64 %i
  %next = add nuw i64 %i, 1
  store i64 %next, ptr %out.ptr, align 8
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; The scalar latch comparison is not part of the widened data graph; an extra
; data use must therefore be rejected before CFG mutation.
define void @latch_compare_data_use(ptr noalias %out, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %out.ptr = getelementptr i8, ptr %out, i64 %i
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  %done.byte = zext i1 %done to i8
  store i8 %done.byte, ptr %out.ptr, align 1
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

!0 = distinct !{!0, !1}
!1 = !{!"llvm.loop.vectorize.enable", i1 false}

attributes #0 = { noinline optnone }
