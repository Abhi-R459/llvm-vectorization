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

