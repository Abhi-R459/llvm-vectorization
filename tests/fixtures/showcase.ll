; Breadth showcase: loops accepted by rust-loop-vectorizer that exercise vector
; factors, element types, and opcode families not already demonstrated by
; vectorizable.ll. Every function here must report decision=vectorized at the
; vector factor named in its comment.
target datalayout = "e-p:64:64:64:64-i64:64-n8:16:32:64-S128"

@g_source = external global [1024 x i32]
@g_result = external global [1024 x i32]

; VF 2: 64-bit elements halve the lane count on a 128-bit vector.
define void @mul_add_f64(ptr noalias %out, ptr noalias %left, ptr noalias %right, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %left.ptr = getelementptr inbounds double, ptr %left, i64 %i
  %left.value = load double, ptr %left.ptr, align 8
  %right.ptr = getelementptr inbounds double, ptr %right, i64 %i
  %right.value = load double, ptr %right.ptr, align 8
  %scaled = fmul double %left.value, 2.500000e+00
  %sum = fadd double %scaled, %right.value
  %out.ptr = getelementptr inbounds double, ptr %out, i64 %i
  store double %sum, ptr %out.ptr, align 8
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; VF 16: the widest lane count the 128-bit policy can select.
define void @mask_bits_i8(ptr noalias %out, ptr noalias %input, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %input.ptr = getelementptr inbounds i8, ptr %input, i64 %i
  %value = load i8, ptr %input.ptr, align 1
  %low = and i8 %value, 15
  %tagged = or i8 %low, -128
  %out.ptr = getelementptr inbounds i8, ptr %out, i64 %i
  store i8 %tagged, ptr %out.ptr, align 1
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; VF 8: half-precision elements.
define void @add_half(ptr noalias %out, ptr noalias %left, ptr noalias %right, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %left.ptr = getelementptr inbounds half, ptr %left, i64 %i
  %left.value = load half, ptr %left.ptr, align 2
  %right.ptr = getelementptr inbounds half, ptr %right, i64 %i
  %right.value = load half, ptr %right.ptr, align 2
  %sum = fadd half %left.value, %right.value
  %out.ptr = getelementptr inbounds half, ptr %out, i64 %i
  store half %sum, ptr %out.ptr, align 2
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; VF 8: 16-bit integers with a bitwise fold.
define void @xor_fold_i16(ptr noalias %out, ptr noalias %input, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %input.ptr = getelementptr inbounds i16, ptr %input, i64 %i
  %value = load i16, ptr %input.ptr, align 2
  %folded = xor i16 %value, -21846
  %out.ptr = getelementptr inbounds i16, ptr %out, i64 %i
  store i16 %folded, ptr %out.ptr, align 2
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; Floating negation is a supported unary operation.
define void @negate_f32(ptr noalias %out, ptr noalias %input, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %input.ptr = getelementptr inbounds float, ptr %input, i64 %i
  %value = load float, ptr %input.ptr, align 4
  %negated = fneg float %value
  %out.ptr = getelementptr inbounds float, ptr %out, i64 %i
  store float %negated, ptr %out.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; Division and remainder are supported but carry a higher cost weight.
define void @unsigned_divrem_i32(ptr noalias %out, ptr noalias %input, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %input.ptr = getelementptr inbounds i32, ptr %input, i64 %i
  %value = load i32, ptr %input.ptr, align 4
  %quotient = udiv i32 %value, 7
  %remainder = urem i32 %value, 11
  %combined = add i32 %quotient, %remainder
  %out.ptr = getelementptr inbounds i32, ptr %out, i64 %i
  store i32 %combined, ptr %out.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; All three shift forms in one body.
define void @shift_mix_i32(ptr noalias %out, ptr noalias %input, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %input.ptr = getelementptr inbounds i32, ptr %input, i64 %i
  %value = load i32, ptr %input.ptr, align 4
  %high = shl i32 %value, 3
  %logical = lshr i32 %value, 5
  %arithmetic = ashr i32 %value, 9
  %merged = or i32 %high, %logical
  %result = xor i32 %merged, %arithmetic
  %out.ptr = getelementptr inbounds i32, ptr %out, i64 %i
  store i32 %result, ptr %out.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; Floating compare feeding a select becomes a vector mask plus vector select.
define void @min_f32(ptr noalias %out, ptr noalias %left, ptr noalias %right, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %left.ptr = getelementptr inbounds float, ptr %left, i64 %i
  %left.value = load float, ptr %left.ptr, align 4
  %right.ptr = getelementptr inbounds float, ptr %right, i64 %i
  %right.value = load float, ptr %right.ptr, align 4
  %smaller = fcmp olt float %left.value, %right.value
  %chosen = select i1 %smaller, float %left.value, float %right.value
  %out.ptr = getelementptr inbounds float, ptr %out, i64 %i
  store float %chosen, ptr %out.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; Integer to floating conversion inside the loop body.
define void @int_to_float(ptr noalias %out, ptr noalias %input, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %input.ptr = getelementptr inbounds i32, ptr %input, i64 %i
  %value = load i32, ptr %input.ptr, align 4
  %converted = sitofp i32 %value to float
  %scaled = fmul float %converted, 5.000000e-01
  %out.ptr = getelementptr inbounds float, ptr %out, i64 %i
  store float %scaled, ptr %out.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; Widening float to double caps the vector factor at the wider end of the cast.
define void @widen_f32_to_f64(ptr noalias %out, ptr noalias %input, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %input.ptr = getelementptr inbounds float, ptr %input, i64 %i
  %value = load float, ptr %input.ptr, align 4
  %widened = fpext float %value to double
  %sum = fadd double %widened, 1.000000e+00
  %out.ptr = getelementptr inbounds double, ptr %out, i64 %i
  store double %sum, ptr %out.ptr, align 8
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; Narrowing double to float exercises the opposite cast direction.
define void @narrow_f64_to_f32(ptr noalias %out, ptr noalias %input, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %input.ptr = getelementptr inbounds double, ptr %input, i64 %i
  %value = load double, ptr %input.ptr, align 8
  %narrowed = fptrunc double %value to float
  %out.ptr = getelementptr inbounds float, ptr %out, i64 %i
  store float %narrowed, ptr %out.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; Two reads of the same base at different affine offsets. Read/read pairs never
; block vectorization, so this stencil is accepted.
define void @neighbor_sum_i32(ptr noalias %out, ptr noalias %input, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %here.ptr = getelementptr inbounds i32, ptr %input, i64 %i
  %here = load i32, ptr %here.ptr, align 4
  %ahead.index = add nuw i64 %i, 1
  %ahead.ptr = getelementptr inbounds i32, ptr %input, i64 %ahead.index
  %ahead = load i32, ptr %ahead.ptr, align 4
  %sum = add i32 %here, %ahead
  %out.ptr = getelementptr inbounds i32, ptr %out, i64 %i
  store i32 %sum, ptr %out.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; Distinct global variables are provably disjoint without noalias arguments.
define void @scale_globals_i32(i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %source.ptr = getelementptr inbounds i32, ptr @g_source, i64 %i
  %value = load i32, ptr %source.ptr, align 4
  %scaled = mul i32 %value, 3
  %result.ptr = getelementptr inbounds i32, ptr @g_result, i64 %i
  store i32 %scaled, ptr %result.ptr, align 4
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}

; A chained widening cast from the narrowest to the widest supported integer.
define void @widen_i8_to_i64(ptr noalias %out, ptr noalias %input, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %input.ptr = getelementptr inbounds i8, ptr %input, i64 %i
  %value = load i8, ptr %input.ptr, align 1
  %widened = zext i8 %value to i64
  %scaled = mul i64 %widened, 1000003
  %out.ptr = getelementptr inbounds i64, ptr %out, i64 %i
  store i64 %scaled, ptr %out.ptr, align 8
  %next = add nuw i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop

exit:
  ret void
}
