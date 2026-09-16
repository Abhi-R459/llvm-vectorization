; Additional fail-closed cases. Each loop is legal C-level code that the pass
; must refuse to transform, and each one names a distinct rejection reason that
; rejected.ll does not already cover. A rejection leaves the IR unchanged.
target datalayout = "e-p:64:64:64:64-i64:64-n8:16:32:64-S128"

; induction-must-start-at-zero: the contract requires a zero-based induction.
define void @nonzero_start(ptr noalias %out, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 1, %entry ], [ %next, %loop ]
  %out.ptr = getelementptr inbounds i32, ptr %out, i64 %i
  store i32 7, ptr %out.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; induction-step-must-be-one: a stride of two makes the accesses non-contiguous,
; so one wide load would no longer cover the same elements.
define void @stride_two(ptr noalias %out, ptr noalias %input, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %input.ptr = getelementptr inbounds i32, ptr %input, i64 %i
  %value = load i32, ptr %input.ptr, align 4
  %out.ptr = getelementptr inbounds i32, ptr %out, i64 %i
  store i32 %value, ptr %out.ptr, align 4
  %next = add nuw i64 %i, 2
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; induction-type-must-be-i64: the affine proof is stated over the i64 domain, so
; a narrower induction with different wrap behavior is refused.
define void @i32_induction(ptr noalias %out, i32 %n) {
entry:
  %empty = icmp eq i32 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i32 [ 0, %entry ], [ %next, %loop ]
  %widened = zext i32 %i to i64
  %out.ptr = getelementptr inbounds i32, ptr %out, i64 %widened
  store i32 1, ptr %out.ptr, align 4
  %next = add nuw i32 %i, 1
  %done = icmp eq i32 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; unsupported-latch-predicate: a signed comparison admits negative trip counts
; that the unsigned round-down in the dispatch block does not model.
define void @signed_latch(ptr noalias %out, i64 %n) {
entry:
  %empty = icmp slt i64 %n, 1
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %out.ptr = getelementptr inbounds i32, ptr %out, i64 %i
  store i32 3, ptr %out.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp slt i64 %next, %n
  br i1 %done, label %loop, label %exit

exit:
  ret void
}

; live-out-phi: an exit PHI would need a repaired incoming value on the new
; vector-exit edge that bypasses the scalar loop.
define i64 @exit_phi(ptr noalias %out, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %out.ptr = getelementptr inbounds i64, ptr %out, i64 %i
  store i64 5, ptr %out.ptr, align 8
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  %result = phi i64 [ 0, %entry ], [ 9, %loop ]
  ret i64 %result
}

; non-affine-index: a scaled subscript is not of the form i + C.
define void @scaled_index(ptr noalias %out, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %scaled = mul nuw i64 %i, 3
  %out.ptr = getelementptr inbounds i32, ptr %out, i64 %scaled
  store i32 4, ptr %out.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; unsupported-pointer-base: alias identity is only established for direct
; arguments and globals, so a stack object is refused rather than guessed at.
define void @stack_base(i64 %n) {
entry:
  %buffer = alloca [256 x i32], align 16
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %slot = getelementptr inbounds i32, ptr %buffer, i64 %i
  store i32 2, ptr %slot, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; unsupported-memory-element-type: pointer elements are outside the allow-list.
define void @pointer_element(ptr noalias %out, ptr %value, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %out.ptr = getelementptr inbounds ptr, ptr %out, i64 %i
  store ptr %value, ptr %out.ptr, align 8
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; memory-pointer-must-be-loop-gep: every iteration writes the same address, so
; there is no contiguous chunk to widen.
define void @invariant_store_pointer(ptr noalias %out, ptr noalias %input, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %input.ptr = getelementptr inbounds i32, ptr %input, i64 %i
  %value = load i32, ptr %input.ptr, align 4
  store i32 %value, ptr %out, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; multiple-header-phis: both PHIs are individually valid canonical inductions,
; so the pass has no unique induction to rewrite and refuses to pick one.
define void @two_header_phis(ptr noalias %out, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %mirror = phi i64 [ 0, %entry ], [ %mirror.next, %loop ]
  %out.ptr = getelementptr inbounds i64, ptr %out, i64 %i
  store i64 %mirror, ptr %out.ptr, align 8
  %mirror.next = add nuw i64 %mirror, 1
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; no-memory-access: there is nothing to widen and nothing to prove.
define void @no_memory(i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; mixed-size-memory-access: the loaded type must equal the GEP source element
; type, otherwise the stride and the access width disagree.
define void @mixed_size_access(ptr noalias %out, ptr noalias %input, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %input.ptr = getelementptr inbounds i32, ptr %input, i64 %i
  %value = load i16, ptr %input.ptr, align 2
  %out.ptr = getelementptr inbounds i16, ptr %out, i64 %i
  store i16 %value, ptr %out.ptr, align 2
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; gep-must-have-one-index: multi-dimensional addressing is outside the
; one-dimensional affine recognizer.
define void @two_index_gep(ptr noalias %out, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %row = getelementptr inbounds [4 x i32], ptr %out, i64 %i, i64 0
  store i32 6, ptr %row, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; volatile-or-atomic-memory: atomic ordering is observable, so lane-parallel
; execution is not a valid schedule.
define void @atomic_access(ptr noalias %out, ptr noalias %input, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %input.ptr = getelementptr inbounds i32, ptr %input, i64 %i
  %value = load atomic i32, ptr %input.ptr monotonic, align 4
  %out.ptr = getelementptr inbounds i32, ptr %out, i64 %i
  store i32 %value, ptr %out.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}
