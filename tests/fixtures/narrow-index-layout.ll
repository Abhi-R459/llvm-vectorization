; Pointer arithmetic in this layout truncates GEP indices to 32 bits. The
; transform's affine proof is over i64 and therefore must not assume that two
; distant iterations remain distinct after target-specific truncation.
target datalayout = "e-p:64:64:64:32-i32:32"

define void @narrow_gep_index(ptr noalias %out, ptr noalias %input, i64 %n) {
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
