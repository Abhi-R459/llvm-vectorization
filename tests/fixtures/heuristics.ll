; Eight iterations are accepted by the balanced/aggressive policies for i32,
; rejected by the conservative 4*VF threshold, and usable with forced VF=8.

define void @eight_element_increment(ptr noalias %data) {
entry:
  br label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %pointer = getelementptr inbounds i32, ptr %data, i64 %i
  %value = load i32, ptr %pointer, align 4
  %incremented = add i32 %value, 1
  store i32 %incremented, ptr %pointer, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, 8
  br i1 %done, label %exit, label %loop

exit:
  ret void
}
