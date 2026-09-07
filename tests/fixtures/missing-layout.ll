; A syntactically vectorizable loop without a declared target DataLayout.
; Generic DataLayout defaults are not a target contract, so the pass must
; reject rather than assume that vector storage matches scalar GEP strides.

define void @missing_target_layout(ptr noalias %data) {
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
