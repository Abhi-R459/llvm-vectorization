; Canonical loops accepted by rust-loop-vectorizer. The runtime harness calls
; every function with trip counts that exercise scalar-only, exact-vector, and
; vector-plus-remainder paths.
target datalayout = "e-p:64:64:64:64-i64:64-n8:16:32:64-S128"

define void @add_f32(ptr noalias %out, ptr noalias %left, ptr noalias %right, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %left.ptr = getelementptr inbounds float, ptr %left, i64 %i
  %left.value = load float, ptr %left.ptr, align 4
  %right.ptr = getelementptr inbounds float, ptr %right, i64 %i
  %right.value = load float, ptr %right.ptr, align 4
  %sum = fadd float %left.value, %right.value
  %out.ptr = getelementptr inbounds float, ptr %out, i64 %i
  store float %sum, ptr %out.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

define void @scale_i32(ptr noalias %out, ptr noalias %input, i32 %factor, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %input.ptr = getelementptr inbounds i32, ptr %input, i64 %i
  %input.value = load i32, ptr %input.ptr, align 4
  %product = mul i32 %input.value, %factor
  %out.ptr = getelementptr inbounds i32, ptr %out, i64 %i
  store i32 %product, ptr %out.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

define void @write_index(ptr noalias %out, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %out.ptr = getelementptr inbounds i64, ptr %out, i64 %i
  store i64 %i, ptr %out.ptr, align 8
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

define void @clamp_min_i32(ptr noalias %out, ptr noalias %input, i32 %minimum, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %input.ptr = getelementptr inbounds i32, ptr %input, i64 %i
  %input.value = load i32, ptr %input.ptr, align 4
  %too.small = icmp slt i32 %input.value, %minimum
  %clamped = select i1 %too.small, i32 %minimum, i32 %input.value
  %out.ptr = getelementptr inbounds i32, ptr %out, i64 %i
  store i32 %clamped, ptr %out.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

define void @widen_i16(ptr noalias %out, ptr noalias %input, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %input.ptr = getelementptr inbounds i16, ptr %input, i64 %i
  %input.value = load i16, ptr %input.ptr, align 2
  %wide = zext i16 %input.value to i32
  %adjusted = add i32 %wide, 7
  %out.ptr = getelementptr inbounds i32, ptr %out, i64 %i
  store i32 %adjusted, ptr %out.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

define void @increment_in_place(ptr noalias %data, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %element.ptr = getelementptr inbounds i32, ptr %data, i64 %i
  %old = load i32, ptr %element.ptr, align 4
  %new = add i32 %old, 1
  store i32 %new, ptr %element.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; Same legality shape as increment_in_place, with the canonical `ne` latch
; orientation (continue while next != trip count).
define void @increment_in_place_ne(ptr noalias %data, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %element.ptr = getelementptr inbounds i32, ptr %data, i64 %i
  %old = load i32, ptr %element.ptr, align 4
  %new = add i32 %old, 2
  store i32 %new, ptr %element.ptr, align 4
  %next = add nuw i64 %i, 1
  %continue = icmp ne i64 %next, %n
  br i1 %continue, label %loop, label %exit

exit:
  ret void
}

; Canonical unsigned counted latch (continue while next < trip count).
define void @increment_in_place_ult(ptr noalias %data, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %element.ptr = getelementptr inbounds i32, ptr %data, i64 %i
  %old = load i32, ptr %element.ptr, align 4
  %new = add i32 %old, 3
  store i32 %new, ptr %element.ptr, align 4
  %next = add nuw i64 %i, 1
  %continue = icmp ult i64 %next, %n
  br i1 %continue, label %loop, label %exit

exit:
  ret void
}

; Widening must retain program order for multiple same-iteration stores.
define void @ordered_double_store(ptr noalias %data, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %element.ptr = getelementptr inbounds i32, ptr %data, i64 %i
  store i32 111, ptr %element.ptr, align 4
  store i32 222, ptr %element.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}
