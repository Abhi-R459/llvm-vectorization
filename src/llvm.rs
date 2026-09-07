//! Narrow safe-ish wrappers around the LLVM C API.
//!
//! Every handle is borrowed from the module owned by `opt`. This module never
//! disposes values, blocks, functions, or the module itself.

use std::ffi::{CStr, c_void};

use llvm_sys::core::{
    LLVMCountBasicBlocks, LLVMGetBasicBlockName, LLVMGetFirstBasicBlock, LLVMGetFirstFunction,
    LLVMGetFirstInstruction, LLVMGetNextBasicBlock, LLVMGetNextFunction, LLVMGetNextInstruction,
    LLVMGetValueName2,
};
use llvm_sys::prelude::{LLVMBasicBlockRef, LLVMModuleRef, LLVMTypeRef, LLVMValueRef};

unsafe extern "C" {
    fn rv_wrap_module(module: *mut c_void) -> LLVMModuleRef;
    fn rv_phi_set_incoming_block(phi: *mut c_void, index: u32, block: *mut c_void);
    fn rv_loop_vectorization_disabled(instruction: *mut c_void) -> bool;
    fn rv_vector_memory_layout_is_packed(
        module: *mut c_void,
        base_pointer: *mut c_void,
        element_type: *mut c_void,
        vector_factor: u32,
    ) -> bool;
}

pub(crate) fn loop_vectorization_disabled(instruction: LLVMValueRef) -> bool {
    // SAFETY: the caller supplies a live branch instruction. The bridge only
    // inspects its loop metadata.
    unsafe { rv_loop_vectorization_disabled(instruction.cast()) }
}

pub(crate) fn vector_memory_layout_is_packed(
    module: LLVMModuleRef,
    base_pointer: LLVMValueRef,
    element_type: LLVMTypeRef,
    vector_factor: u32,
) -> bool {
    // SAFETY: all handles belong to the live module and the bridge performs a
    // read-only DataLayout query for a fixed vector type.
    unsafe {
        rv_vector_memory_layout_is_packed(
            module.cast(),
            base_pointer.cast(),
            element_type.cast(),
            vector_factor,
        )
    }
}

pub(crate) unsafe fn wrap_module(module: *mut c_void) -> LLVMModuleRef {
    // SAFETY: upheld by the caller and forwarded to the C++ bridge.
    unsafe { rv_wrap_module(module) }
}

pub(crate) fn set_phi_incoming_block(phi: LLVMValueRef, index: u32, block: LLVMBasicBlockRef) {
    // SAFETY: the vectorizer only calls this for a PHI and one of its existing
    // incoming indices, with a block owned by the same function.
    unsafe { rv_phi_set_incoming_block(phi.cast(), index, block.cast()) }
}

pub(crate) fn functions(module: LLVMModuleRef) -> Vec<LLVMValueRef> {
    let mut result = Vec::new();
    // SAFETY: module is borrowed and live for this pass invocation.
    let mut function = unsafe { LLVMGetFirstFunction(module) };
    while !function.is_null() {
        result.push(function);
        // SAFETY: function came from this module's function list.
        function = unsafe { LLVMGetNextFunction(function) };
    }
    result
}

pub(crate) fn blocks(function: LLVMValueRef) -> Vec<LLVMBasicBlockRef> {
    // SAFETY: `function` is a live LLVM function value.
    if unsafe { LLVMCountBasicBlocks(function) } == 0 {
        return Vec::new();
    }
    let mut result = Vec::new();
    // SAFETY: `function` has at least one block.
    let mut block = unsafe { LLVMGetFirstBasicBlock(function) };
    while !block.is_null() {
        result.push(block);
        // SAFETY: block came from this function's block list.
        block = unsafe { LLVMGetNextBasicBlock(block) };
    }
    result
}

pub(crate) fn instructions(block: LLVMBasicBlockRef) -> Vec<LLVMValueRef> {
    let mut result = Vec::new();
    // SAFETY: block is live; null denotes an empty block.
    let mut instruction = unsafe { LLVMGetFirstInstruction(block) };
    while !instruction.is_null() {
        result.push(instruction);
        // SAFETY: instruction came from this block's instruction list.
        instruction = unsafe { LLVMGetNextInstruction(instruction) };
    }
    result
}

pub(crate) fn value_name(value: LLVMValueRef) -> String {
    let mut length = 0_usize;
    // SAFETY: value is live and LLVM owns the returned byte slice.
    let pointer = unsafe { LLVMGetValueName2(value, &raw mut length) };
    if pointer.is_null() || length == 0 {
        return "<anonymous>".to_owned();
    }
    // SAFETY: LLVM guarantees exactly `length` bytes at `pointer`.
    String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(pointer.cast(), length) }).into()
}

pub(crate) fn block_name(block: LLVMBasicBlockRef) -> String {
    // SAFETY: block is live and LLVM returns a NUL-terminated borrowed name.
    let pointer = unsafe { LLVMGetBasicBlockName(block) };
    if pointer.is_null() {
        return "<anonymous>".to_owned();
    }
    // SAFETY: guaranteed NUL-terminated by LLVM.
    unsafe { CStr::from_ptr(pointer) }
        .to_string_lossy()
        .into_owned()
}

pub(crate) fn value_key(value: LLVMValueRef) -> usize {
    value as usize
}
