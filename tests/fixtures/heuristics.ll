; Eight iterations are accepted by the balanced/aggressive policies for i32,
; rejected by the conservative 4*VF threshold, and usable with forced VF=8.
target datalayout = "e-p:64:64:64:64-i64:64-n8:16:32:64-S128"

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

; The i64 affine helper is address-only. It must not make this i32 data loop
; look 64-bit-wide to the natural-VF or profitability calculation.
define void @offset_copy_i32(ptr noalias %out, ptr noalias %input) {
entry:
  br label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %source.index = add nuw i64 %i, 1
  %source.pointer = getelementptr inbounds i32, ptr %input, i64 %source.index
  %destination.pointer = getelementptr inbounds i32, ptr %out, i64 %i
  %value = load i32, ptr %source.pointer, align 4
  store i32 %value, ptr %destination.pointer, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, 8
  br i1 %done, label %exit, label %loop

exit:
  ret void
}
