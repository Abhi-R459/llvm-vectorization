//! Loop discovery, legality, planning, and fixed-width IR generation.

use std::collections::{HashMap, HashSet};
use std::ffi::c_char;
use std::fmt::Write as _;
use std::time::{Duration, Instant};

use llvm_sys::LLVMIntPredicate::{LLVMIntEQ, LLVMIntNE, LLVMIntUGE, LLVMIntULT};
use llvm_sys::LLVMOpcode::{
    LLVMAShr, LLVMAdd, LLVMAddrSpaceCast, LLVMAnd, LLVMBitCast, LLVMBr, LLVMFAdd, LLVMFCmp,
    LLVMFDiv, LLVMFMul, LLVMFNeg, LLVMFPExt, LLVMFPToSI, LLVMFPToUI, LLVMFPTrunc, LLVMFRem,
    LLVMFSub, LLVMGetElementPtr, LLVMICmp, LLVMIntToPtr, LLVMLShr, LLVMLoad, LLVMMul, LLVMOr,
    LLVMPHI, LLVMPtrToInt, LLVMSDiv, LLVMSExt, LLVMSIToFP, LLVMSRem, LLVMSelect, LLVMShl,
    LLVMStore, LLVMSub, LLVMTrunc, LLVMUDiv, LLVMUIToFP, LLVMURem, LLVMXor, LLVMZExt,
};
use llvm_sys::LLVMTypeKind::{
    LLVMDoubleTypeKind, LLVMFloatTypeKind, LLVMHalfTypeKind, LLVMIntegerTypeKind,
};
use llvm_sys::core::{
    LLVMAddIncoming, LLVMBuildAdd, LLVMBuildAnd, LLVMBuildBinOp, LLVMBuildCast, LLVMBuildCondBr,
    LLVMBuildFCmp, LLVMBuildFNeg, LLVMBuildGEP2, LLVMBuildICmp, LLVMBuildInsertElement,
    LLVMBuildLoad2, LLVMBuildPhi, LLVMBuildSelect, LLVMBuildShuffleVector, LLVMBuildStore,
    LLVMCanValueUseFastMathFlags, LLVMConstInt, LLVMConstIntGetSExtValue, LLVMConstIntGetZExtValue,
    LLVMConstVector, LLVMCountIncoming, LLVMCreateBuilderInContext, LLVMDisposeBuilder,
    LLVMGetAlignment, LLVMGetBasicBlockTerminator, LLVMGetCondition, LLVMGetEnumAttributeAtIndex,
    LLVMGetEnumAttributeKindForName, LLVMGetExact, LLVMGetFCmpPredicate, LLVMGetFastMathFlags,
    LLVMGetFirstParam, LLVMGetFirstUse, LLVMGetGEPSourceElementType, LLVMGetICmpPredicate,
    LLVMGetIncomingBlock, LLVMGetIncomingValue, LLVMGetInstructionOpcode, LLVMGetInstructionParent,
    LLVMGetIntTypeWidth, LLVMGetModuleContext, LLVMGetNSW, LLVMGetNUW, LLVMGetNextParam,
    LLVMGetNextUse, LLVMGetNumOperands, LLVMGetNumSuccessors, LLVMGetOperand, LLVMGetPoison,
    LLVMGetSuccessor, LLVMGetTypeContext, LLVMGetTypeKind, LLVMGetUser, LLVMGetVolatile,
    LLVMInsertBasicBlockInContext, LLVMInt32TypeInContext, LLVMIsAArgument, LLVMIsAConstantInt,
    LLVMIsAGetElementPtrInst, LLVMIsAGlobalVariable, LLVMIsAInstruction, LLVMIsAtomic,
    LLVMIsInBounds, LLVMPositionBuilderAtEnd, LLVMSetAlignment, LLVMSetExact, LLVMSetFastMathFlags,
    LLVMSetIsInBounds, LLVMSetNSW, LLVMSetNUW, LLVMSetSuccessor, LLVMTypeOf, LLVMVectorType,
};
use llvm_sys::prelude::{
    LLVMBasicBlockRef, LLVMBuilderRef, LLVMContextRef, LLVMModuleRef, LLVMTypeRef, LLVMValueRef,
};
use llvm_sys::{LLVMAttributeFunctionIndex, LLVMOpcode};

use crate::config::PassConfig;
use crate::cost::{LoopCosts, Plan, choose_plan};
use crate::dependence::{AccessKind, AffineAccess, Dependence, classify};
use crate::llvm;
use crate::mark_ir_mutated;

const ESTIMATED_DYNAMIC_TRIP_COUNT: u64 = 64;
const MAX_BODY_INSTRUCTIONS: usize = 96;
const MAX_MEMORY_ACCESSES: usize = 24;

#[derive(Clone, Copy)]
struct MemoryAccess {
    base: LLVMValueRef,
    offset: i64,
    kind: AccessKind,
    element_type: LLVMTypeRef,
}

struct Candidate {
    header: LLVMBasicBlockRef,
    preheader: LLVMBasicBlockRef,
    exit: LLVMBasicBlockRef,
    induction: LLVMValueRef,
    induction_next: LLVMValueRef,
    latch_compare: LLVMValueRef,
    trip_count: LLVMValueRef,
    outside_phi_index: u32,
    preheader_successor_index: u32,
    body: Vec<LLVMValueRef>,
    address_only: HashSet<usize>,
    widen_induction: bool,
    minimum_trip_count: u64,
}

#[derive(Debug)]
struct Rejection(&'static str);

struct Builder(LLVMBuilderRef);

impl Builder {
    fn new(context: LLVMContextRef) -> Self {
        // SAFETY: context belongs to the live module.
        Self(unsafe { LLVMCreateBuilderInContext(context) })
    }

    const fn raw(&self) -> LLVMBuilderRef {
        self.0
    }
}

impl Drop for Builder {
    fn drop(&mut self) {
        // SAFETY: this builder was created by LLVM and is disposed exactly once.
        unsafe { LLVMDisposeBuilder(self.0) }
    }
}

pub(crate) fn run(module: LLVMModuleRef, config: PassConfig) -> bool {
    let mut changed = false;
    let functions = llvm::functions(module);

    for function in functions {
        if function_has_enum_attribute(function, LLVMAttributeFunctionIndex, b"optnone") {
            continue;
        }
        let headers = discover_single_block_loop_headers(function);
        for header in headers {
            let analysis_start = Instant::now();
            let analysis = analyze_candidate(module, function, header, config);
            let analysis_time = analysis_start.elapsed();

            match analysis {
                Ok((candidate, plan)) => {
                    let transform_start = Instant::now();
                    // SAFETY: analysis proved the structural invariants consumed
                    // by the transformer, and all handles belong to `module`.
                    unsafe { transform(module, &candidate, plan) };
                    let transform_time = transform_start.elapsed();
                    changed = true;
                    if config.emit_remarks {
                        report(
                            function,
                            header,
                            "vectorized",
                            "legal-and-profitable",
                            Some(plan),
                            analysis_time,
                            transform_time,
                        );
                    }
                }
                Err(rejection) if config.emit_remarks => report(
                    function,
                    header,
                    "rejected",
                    rejection.0,
                    None,
                    analysis_time,
                    Duration::ZERO,
                ),
                Err(_) => {}
            }
        }
    }
    changed
}

fn discover_single_block_loop_headers(function: LLVMValueRef) -> Vec<LLVMBasicBlockRef> {
    llvm::blocks(function)
        .into_iter()
        .filter(|&block| {
            // SAFETY: block is live and belongs to `function`.
            let terminator = unsafe { LLVMGetBasicBlockTerminator(block) };
            if terminator.is_null()
                // SAFETY: terminator is non-null in the remaining expression.
                || unsafe { LLVMGetInstructionOpcode(terminator) } != LLVMBr
                // SAFETY: branch belongs to this block.
                || unsafe { LLVMGetNumSuccessors(terminator) } != 2
            {
                return false;
            }
            // SAFETY: the branch has two successors.
            unsafe {
                LLVMGetSuccessor(terminator, 0) == block || LLVMGetSuccessor(terminator, 1) == block
            }
        })
        .collect()
}

#[allow(clippy::too_many_lines)]
fn analyze_candidate(
    module: LLVMModuleRef,
    function: LLVMValueRef,
    header: LLVMBasicBlockRef,
    config: PassConfig,
) -> Result<(Candidate, Plan), Rejection> {
    let body = llvm::instructions(header);
    if body.len() > MAX_BODY_INSTRUCTIONS {
        return Err(Rejection("instruction-budget-exceeded"));
    }
    validate_no_live_outs(&body, header)?;

    // SAFETY: discovery only returns blocks with a conditional branch.
    let terminator = unsafe { LLVMGetBasicBlockTerminator(header) };
    if llvm::loop_vectorization_disabled(terminator) {
        return Err(Rejection("disabled-by-loop-metadata"));
    }
    // SAFETY: the branch has exactly two successors.
    let first_successor = unsafe { LLVMGetSuccessor(terminator, 0) };
    // SAFETY: the branch has exactly two successors.
    let second_successor = unsafe { LLVMGetSuccessor(terminator, 1) };
    let exit = if first_successor == header {
        second_successor
    } else {
        first_successor
    };

    reject_exit_phis(exit)?;
    let (induction, induction_next, preheader, outside_phi_index) = find_induction(&body, header)?;
    let preheader_successor_index = validate_predecessors(function, header, preheader)?;
    if llvm::block_name(preheader).starts_with("rv.") {
        return Err(Rejection("already-vectorized"));
    }

    // SAFETY: terminator is a conditional branch.
    let latch_compare = unsafe { LLVMGetCondition(terminator) };
    let trip_count = match_latch(
        latch_compare,
        induction_next,
        header,
        first_successor,
        second_successor,
    )?;
    validate_control_uses(induction, induction_next, latch_compare, terminator)?;

    let mut memory = Vec::new();
    let mut address_only = HashSet::new();
    let mut widest_element_bits = 0;
    let mut scalar_cost = 0_u64;
    let mut vector_cost = 0_u64;

    // Discover affine index helpers before costing in program order. Their
    // i64 type describes address arithmetic that the vector loop rebuilds once
    // per vector iteration; it must not cap the data VF or count as lane-wise
    // work merely because its defining add appears before its GEP use.
    for &instruction in &body {
        // SAFETY: value is an instruction from `body`.
        if unsafe { LLVMGetInstructionOpcode(instruction) } != LLVMGetElementPtr {
            continue;
        }
        let (_, _, index_node, _) = analyze_gep(instruction, induction, header)?;
        if let Some(index_node) = index_node {
            validate_address_index_uses(index_node, header)?;
            address_only.insert(llvm::value_key(index_node));
        }
    }

    for &instruction in &body {
        if instruction == induction
            || instruction == induction_next
            || instruction == latch_compare
            || instruction == terminator
            || address_only.contains(&llvm::value_key(instruction))
        {
            continue;
        }
        // SAFETY: value is an instruction from `body`.
        let opcode = unsafe { LLVMGetInstructionOpcode(instruction) };
        match opcode {
            LLVMGetElementPtr => {
                let (base, offset, _, element_type) = analyze_gep(instruction, induction, header)?;
                widest_element_bits = widest_element_bits.max(type_bits(element_type)?);
                validate_gep_uses(instruction, header)?;
                let _ = (base, offset); // Recorded when its load/store is visited.
            }
            LLVMLoad | LLVMStore => {
                let access = analyze_memory(instruction, induction, header)?;
                widest_element_bits = widest_element_bits.max(type_bits(access.element_type)?);
                memory.push(access);
                scalar_cost += 2;
                vector_cost += 2;
            }
            opcode if is_supported_compute(opcode) => {
                widest_element_bits = widest_element_bits.max(instruction_width(instruction)?);
                scalar_cost += instruction_cost(opcode);
                vector_cost += instruction_cost(opcode);
                validate_compute_operands(instruction, header, induction)?;
            }
            _ => return Err(Rejection("unsupported-instruction")),
        }
    }

    if memory.is_empty() {
        return Err(Rejection("no-memory-access"));
    }
    if memory.len() > MAX_MEMORY_ACCESSES {
        return Err(Rejection("memory-access-budget-exceeded"));
    }
    validate_dependences(function, &memory)?;
    if scalar_cost == 0 || widest_element_bits == 0 {
        return Err(Rejection("empty-vector-body"));
    }

    let widen_induction = induction_is_data(
        &body,
        induction,
        induction_next,
        latch_compare,
        &address_only,
    );
    validate_widening_order(
        &body,
        header,
        induction,
        induction_next,
        latch_compare,
        terminator,
        &address_only,
        widen_induction,
    )?;

    // SAFETY: trip_count is a live integer value.
    let estimated_trip_count =
        constant_unsigned(trip_count).unwrap_or(ESTIMATED_DYNAMIC_TRIP_COUNT);
    let costs = LoopCosts {
        scalar_iteration: scalar_cost,
        vector_iteration: vector_cost,
        setup: 6,
    };
    let plan = choose_plan(config, widest_element_bits, estimated_trip_count, costs)
        .ok_or(Rejection("not-profitable"))?;
    if memory.iter().any(|access| {
        !llvm::vector_memory_layout_is_packed(module, access.base, access.element_type, plan.vf)
    }) {
        return Err(Rejection("incompatible-target-memory-layout"));
    }

    Ok((
        Candidate {
            header,
            preheader,
            exit,
            induction,
            induction_next,
            latch_compare,
            trip_count,
            outside_phi_index,
            preheader_successor_index,
            body,
            address_only,
            widen_induction,
            minimum_trip_count: config.minimum_trip_count(plan.vf),
        },
        plan,
    ))
}

fn reject_exit_phis(exit: LLVMBasicBlockRef) -> Result<(), Rejection> {
    for instruction in llvm::instructions(exit) {
        // SAFETY: instruction is live.
        if unsafe { LLVMGetInstructionOpcode(instruction) } == LLVMPHI {
            return Err(Rejection("live-out-phi"));
        }
    }
    Ok(())
}

fn find_induction(
    body: &[LLVMValueRef],
    header: LLVMBasicBlockRef,
) -> Result<(LLVMValueRef, LLVMValueRef, LLVMBasicBlockRef, u32), Rejection> {
    let mut match_found = None;
    for &instruction in body {
        // SAFETY: instruction is live.
        if unsafe { LLVMGetInstructionOpcode(instruction) } != LLVMPHI {
            continue;
        }
        // Restrict the control arithmetic to i64 so every synthesized mask,
        // threshold, lane offset, and VF increment is representable exactly.
        // SAFETY: instruction is a live PHI.
        let induction_type = unsafe { LLVMTypeOf(instruction) };
        // SAFETY: induction_type is live.
        if unsafe { LLVMGetTypeKind(induction_type) } != LLVMIntegerTypeKind
            // SAFETY: the right-hand side runs only for an integer type.
            || unsafe { LLVMGetIntTypeWidth(induction_type) } != 64
        {
            return Err(Rejection("induction-type-must-be-i64"));
        }
        // SAFETY: instruction is a PHI.
        if unsafe { LLVMCountIncoming(instruction) } != 2 {
            return Err(Rejection("non-canonical-phi"));
        }
        let mut inside_index = None;
        let mut outside_index = None;
        for index in 0..2 {
            // SAFETY: PHI has two incoming entries.
            if unsafe { LLVMGetIncomingBlock(instruction, index) } == header {
                inside_index = Some(index);
            } else {
                outside_index = Some(index);
            }
        }
        let (Some(inside_index), Some(outside_index)) = (inside_index, outside_index) else {
            return Err(Rejection("non-canonical-phi"));
        };
        // SAFETY: indices are within the PHI's incoming array.
        let start = unsafe { LLVMGetIncomingValue(instruction, outside_index) };
        if constant_signed(start) != Some(0) {
            return Err(Rejection("induction-must-start-at-zero"));
        }
        // SAFETY: indices are within the PHI's incoming array.
        let next = unsafe { LLVMGetIncomingValue(instruction, inside_index) };
        if !is_add_one(next, instruction) {
            return Err(Rejection("induction-step-must-be-one"));
        }
        // SAFETY: index is within the PHI and the block is live.
        let preheader = unsafe { LLVMGetIncomingBlock(instruction, outside_index) };
        if match_found.is_some() {
            return Err(Rejection("multiple-header-phis"));
        }
        match_found = Some((instruction, next, preheader, outside_index));
    }
    match_found.ok_or(Rejection("missing-canonical-induction"))
}

fn validate_control_uses(
    induction: LLVMValueRef,
    induction_next: LLVMValueRef,
    latch_compare: LLVMValueRef,
    terminator: LLVMValueRef,
) -> Result<(), Rejection> {
    // The increment is not widened as ordinary data. It may feed only the
    // induction PHI and latch comparison.
    // SAFETY: induction_next is live.
    let mut usage = unsafe { LLVMGetFirstUse(induction_next) };
    while !usage.is_null() {
        // SAFETY: usage is live.
        let user = unsafe { LLVMGetUser(usage) };
        if user != induction && user != latch_compare {
            return Err(Rejection("induction-next-has-data-use"));
        }
        // SAFETY: usage is live.
        usage = unsafe { LLVMGetNextUse(usage) };
    }

    // SAFETY: latch_compare is live.
    let mut usage = unsafe { LLVMGetFirstUse(latch_compare) };
    while !usage.is_null() {
        // SAFETY: usage is live.
        if unsafe { LLVMGetUser(usage) } != terminator {
            return Err(Rejection("latch-compare-has-data-use"));
        }
        // SAFETY: usage is live.
        usage = unsafe { LLVMGetNextUse(usage) };
    }
    Ok(())
}

fn is_add_one(value: LLVMValueRef, induction: LLVMValueRef) -> bool {
    // SAFETY: value is live; non-instructions simply do not match LLVMAdd.
    if unsafe { LLVMIsAInstruction(value) }.is_null()
        // SAFETY: value is an instruction in the remaining expression.
        || unsafe { LLVMGetInstructionOpcode(value) } != LLVMAdd
    {
        return false;
    }
    // SAFETY: binary add has two operands.
    let left = unsafe { LLVMGetOperand(value, 0) };
    // SAFETY: binary add has two operands.
    let right = unsafe { LLVMGetOperand(value, 1) };
    (left == induction && constant_signed(right) == Some(1))
        || (right == induction && constant_signed(left) == Some(1))
}

fn validate_predecessors(
    function: LLVMValueRef,
    header: LLVMBasicBlockRef,
    preheader: LLVMBasicBlockRef,
) -> Result<u32, Rejection> {
    // Re-targeting an indirectbr (or another exotic terminator) is not the
    // same operation as splitting a normal branch edge: its blockaddress may
    // continue to name the old destination. Keep the supported CFG contract
    // explicit and mechanically checkable.
    // SAFETY: preheader is a live basic block.
    let preheader_terminator = unsafe { LLVMGetBasicBlockTerminator(preheader) };
    if preheader_terminator.is_null()
        // SAFETY: the non-null value is a live terminator instruction.
        || unsafe { LLVMGetInstructionOpcode(preheader_terminator) } != LLVMBr
    {
        return Err(Rejection("preheader-terminator-must-be-branch"));
    }

    let mut external_edges = Vec::new();
    for block in llvm::blocks(function) {
        // SAFETY: block is live.
        let terminator = unsafe { LLVMGetBasicBlockTerminator(block) };
        if terminator.is_null() {
            continue;
        }
        // SAFETY: terminator is live.
        let successor_count = unsafe { LLVMGetNumSuccessors(terminator) };
        for index in 0..successor_count {
            // SAFETY: index is within the successor array.
            if unsafe { LLVMGetSuccessor(terminator, index) } == header && block != header {
                external_edges.push((block, index));
            }
        }
    }
    match external_edges.as_slice() {
        [(block, index)] if *block == preheader => Ok(*index),
        _ => Err(Rejection("loop-needs-unique-preheader")),
    }
}

fn match_latch(
    compare: LLVMValueRef,
    induction_next: LLVMValueRef,
    header: LLVMBasicBlockRef,
    true_successor: LLVMBasicBlockRef,
    false_successor: LLVMBasicBlockRef,
) -> Result<LLVMValueRef, Rejection> {
    // A conditional branch may legally use an argument or constant i1. The
    // opcode query is defined only for instructions, so establish that FFI
    // precondition before asking whether this is an icmp.
    // SAFETY: branch condition is a live LLVM value.
    if unsafe { LLVMIsAInstruction(compare) }.is_null()
        // SAFETY: the remaining value is an instruction.
        || unsafe { LLVMGetInstructionOpcode(compare) } != LLVMICmp
    {
        return Err(Rejection("latch-condition-must-be-icmp"));
    }
    // SAFETY: compare has two operands.
    let left = unsafe { LLVMGetOperand(compare, 0) };
    // SAFETY: compare has two operands.
    let right = unsafe { LLVMGetOperand(compare, 1) };
    let trip = if left == induction_next {
        right
    } else if right == induction_next {
        left
    } else {
        return Err(Rejection("latch-must-compare-next-induction"));
    };
    if !is_loop_invariant(trip, header) {
        return Err(Rejection("trip-count-not-loop-invariant"));
    }

    // SAFETY: compare is an integer comparison.
    let predicate = unsafe { LLVMGetICmpPredicate(compare) };
    let valid_direction = match predicate {
        LLVMIntEQ => true_successor != header && false_successor == header,
        LLVMIntNE | LLVMIntULT => true_successor == header && false_successor != header,
        _ => false,
    };
    if !valid_direction || (predicate == LLVMIntULT && left != induction_next) {
        return Err(Rejection("unsupported-latch-predicate"));
    }
    Ok(trip)
}

fn analyze_gep(
    gep: LLVMValueRef,
    induction: LLVMValueRef,
    header: LLVMBasicBlockRef,
) -> Result<(LLVMValueRef, i64, Option<LLVMValueRef>, LLVMTypeRef), Rejection> {
    // SAFETY: GEP is live.
    if unsafe { LLVMGetNumOperands(gep) } != 2 {
        return Err(Rejection("gep-must-have-one-index"));
    }
    // SAFETY: GEP has base and one index.
    let base = unsafe { LLVMGetOperand(gep, 0) };
    if !is_loop_invariant(base, header) {
        return Err(Rejection("pointer-base-not-loop-invariant"));
    }
    // Keep alias reasoning tied to LLVM objects with clear identity. A later
    // version can strip casts and invariant GEPs before applying this test.
    // SAFETY: base is live.
    if unsafe { LLVMIsAArgument(base) }.is_null()
        // SAFETY: base is live.
        && unsafe { LLVMIsAGlobalVariable(base) }.is_null()
    {
        return Err(Rejection("unsupported-pointer-base"));
    }
    // SAFETY: GEP has base and one index.
    let index = unsafe { LLVMGetOperand(gep, 1) };
    let (offset, index_node) = parse_affine_index(index, induction)?;
    // SAFETY: value is a GEP.
    let element_type = unsafe { LLVMGetGEPSourceElementType(gep) };
    if !is_supported_memory_type(element_type) {
        return Err(Rejection("unsupported-memory-element-type"));
    }
    Ok((base, offset, index_node, element_type))
}

fn parse_affine_index(
    index: LLVMValueRef,
    induction: LLVMValueRef,
) -> Result<(i64, Option<LLVMValueRef>), Rejection> {
    if index == induction {
        return Ok((0, None));
    }
    // SAFETY: index is live; non-instructions do not match.
    if unsafe { LLVMIsAInstruction(index) }.is_null() {
        return Err(Rejection("non-affine-index"));
    }
    // SAFETY: index is an instruction.
    let opcode = unsafe { LLVMGetInstructionOpcode(index) };
    if !matches!(opcode, LLVMAdd | LLVMSub) {
        return Err(Rejection("non-affine-index"));
    }
    // SAFETY: add/sub have two operands.
    let left = unsafe { LLVMGetOperand(index, 0) };
    // SAFETY: add/sub have two operands.
    let right = unsafe { LLVMGetOperand(index, 1) };
    if left == induction {
        let constant = constant_signed(right).ok_or(Rejection("non-constant-affine-offset"))?;
        let offset = if opcode == LLVMSub {
            constant
                .checked_neg()
                .ok_or(Rejection("affine-offset-overflow"))?
        } else {
            constant
        };
        return Ok((offset, Some(index)));
    }
    if opcode == LLVMAdd && right == induction {
        let constant = constant_signed(left).ok_or(Rejection("non-constant-affine-offset"))?;
        return Ok((constant, Some(index)));
    }
    Err(Rejection("non-affine-index"))
}

fn validate_gep_uses(gep: LLVMValueRef, header: LLVMBasicBlockRef) -> Result<(), Rejection> {
    // SAFETY: GEP is live.
    let mut usage = unsafe { LLVMGetFirstUse(gep) };
    if usage.is_null() {
        return Err(Rejection("unused-gep"));
    }
    while !usage.is_null() {
        // SAFETY: usage is live.
        let user = unsafe { LLVMGetUser(usage) };
        // SAFETY: user is live.
        let opcode = unsafe { LLVMGetInstructionOpcode(user) };
        // SAFETY: user is an instruction.
        let is_pointer_operand = match opcode {
            // SAFETY: a load has a pointer operand at index zero.
            LLVMLoad => (unsafe { LLVMGetOperand(user, 0) }) == gep,
            // SAFETY: a store has a pointer operand at index one.
            LLVMStore => (unsafe { LLVMGetOperand(user, 1) }) == gep,
            _ => false,
        };
        if !is_pointer_operand || unsafe { LLVMGetInstructionParent(user) } != header {
            return Err(Rejection("gep-has-non-memory-use"));
        }
        // SAFETY: usage is live.
        usage = unsafe { LLVMGetNextUse(usage) };
    }
    Ok(())
}

fn validate_no_live_outs(
    body: &[LLVMValueRef],
    header: LLVMBasicBlockRef,
) -> Result<(), Rejection> {
    for &instruction in body {
        // SAFETY: instruction is live.
        let mut usage = unsafe { LLVMGetFirstUse(instruction) };
        while !usage.is_null() {
            // SAFETY: usage is live.
            let user = unsafe { LLVMGetUser(usage) };
            // Non-instruction users and users outside the loop would no longer
            // be dominated when the no-remainder vector path bypasses header.
            // SAFETY: user is a live LLVM value.
            let user_instruction = unsafe { LLVMIsAInstruction(user) };
            if user_instruction.is_null()
                // SAFETY: non-null user_instruction is an instruction.
                || unsafe { LLVMGetInstructionParent(user_instruction) } != header
            {
                return Err(Rejection("loop-value-live-out"));
            }
            // SAFETY: usage is live.
            usage = unsafe { LLVMGetNextUse(usage) };
        }
    }
    Ok(())
}

fn validate_address_index_uses(
    index_value: LLVMValueRef,
    header: LLVMBasicBlockRef,
) -> Result<(), Rejection> {
    // Address-only affine helpers are omitted from the widened compute graph.
    // Ensure doing so cannot strand a non-address use.
    // SAFETY: index_value is a live instruction.
    let mut usage = unsafe { LLVMGetFirstUse(index_value) };
    while !usage.is_null() {
        // SAFETY: usage is live.
        let user = unsafe { LLVMGetUser(usage) };
        // SAFETY: user is a live instruction.
        if unsafe { LLVMGetInstructionParent(user) } != header
            || unsafe { LLVMGetInstructionOpcode(user) } != LLVMGetElementPtr
        {
            return Err(Rejection("affine-index-has-data-use"));
        }
        // SAFETY: usage is live.
        usage = unsafe { LLVMGetNextUse(usage) };
    }
    Ok(())
}

fn induction_is_data(
    body: &[LLVMValueRef],
    induction: LLVMValueRef,
    induction_next: LLVMValueRef,
    latch_compare: LLVMValueRef,
    address_only: &HashSet<usize>,
) -> bool {
    body.iter().copied().any(|instruction| {
        if instruction == induction_next
            || instruction == latch_compare
            || address_only.contains(&llvm::value_key(instruction))
        {
            return false;
        }
        // SAFETY: instruction is live.
        if unsafe { LLVMGetInstructionOpcode(instruction) } == LLVMGetElementPtr {
            return false;
        }
        // SAFETY: instruction is live.
        let operand_count = u32::try_from(unsafe { LLVMGetNumOperands(instruction) })
            .expect("LLVM operand counts are non-negative");
        (0..operand_count).any(|index| {
            // SAFETY: index lies within the operand array.
            (unsafe { LLVMGetOperand(instruction, index) }) == induction
        })
    })
}

#[allow(clippy::too_many_arguments)]
fn validate_widening_order(
    body: &[LLVMValueRef],
    header: LLVMBasicBlockRef,
    induction: LLVMValueRef,
    induction_next: LLVMValueRef,
    latch_compare: LLVMValueRef,
    terminator: LLVMValueRef,
    address_only: &HashSet<usize>,
    widen_induction: bool,
) -> Result<(), Rejection> {
    let mut available = HashSet::new();
    if widen_induction {
        available.insert(llvm::value_key(induction));
    }

    for &instruction in body {
        if instruction == induction
            || instruction == induction_next
            || instruction == latch_compare
            || instruction == terminator
            || address_only.contains(&llvm::value_key(instruction))
        {
            continue;
        }
        // SAFETY: instruction is live.
        let opcode = unsafe { LLVMGetInstructionOpcode(instruction) };
        match opcode {
            LLVMGetElementPtr => {}
            LLVMLoad => {
                // SAFETY: load has one pointer operand.
                let pointer = unsafe { LLVMGetOperand(instruction, 0) };
                if !available.contains(&llvm::value_key(pointer)) {
                    return Err(Rejection("load-pointer-not-widened"));
                }
            }
            LLVMStore => {
                // SAFETY: store has value and pointer operands.
                let value = unsafe { LLVMGetOperand(instruction, 0) };
                let pointer = unsafe { LLVMGetOperand(instruction, 1) };
                if !available.contains(&llvm::value_key(pointer))
                    || (!is_loop_invariant(value, header)
                        && !available.contains(&llvm::value_key(value)))
                {
                    return Err(Rejection("store-input-not-widened"));
                }
            }
            opcode if is_supported_compute(opcode) => {
                let operand_count = u32::try_from(unsafe { LLVMGetNumOperands(instruction) })
                    .expect("LLVM operand counts are non-negative");
                for index in 0..operand_count {
                    // SAFETY: index is within the operand array.
                    let operand = unsafe { LLVMGetOperand(instruction, index) };
                    if !is_loop_invariant(operand, header)
                        && !available.contains(&llvm::value_key(operand))
                    {
                        return Err(Rejection("compute-input-not-widened"));
                    }
                }
            }
            _ => return Err(Rejection("unsupported-instruction")),
        }
        if opcode != LLVMStore {
            available.insert(llvm::value_key(instruction));
        }
    }
    Ok(())
}

fn analyze_memory(
    instruction: LLVMValueRef,
    induction: LLVMValueRef,
    header: LLVMBasicBlockRef,
) -> Result<MemoryAccess, Rejection> {
    // SAFETY: instruction is a load or store.
    if unsafe { LLVMGetVolatile(instruction) } != 0 || unsafe { LLVMIsAtomic(instruction) } != 0 {
        return Err(Rejection("volatile-or-atomic-memory"));
    }
    // SAFETY: instruction is live.
    let opcode = unsafe { LLVMGetInstructionOpcode(instruction) };
    // SAFETY: load pointer is operand 0; store pointer is operand 1.
    let pointer = unsafe { LLVMGetOperand(instruction, u32::from(opcode == LLVMStore)) };
    // SAFETY: pointer is live.
    if unsafe { LLVMIsAGetElementPtrInst(pointer) }.is_null()
        // SAFETY: pointer is an instruction in the remaining expression.
        || unsafe { LLVMGetInstructionParent(pointer) } != header
    {
        return Err(Rejection("memory-pointer-must-be-loop-gep"));
    }
    let (base, offset, _, element_type) = analyze_gep(pointer, induction, header)?;
    // SAFETY: load result or stored value gives the accessed scalar type.
    let accessed_type = if opcode == LLVMLoad {
        unsafe { LLVMTypeOf(instruction) }
    } else {
        unsafe { LLVMTypeOf(LLVMGetOperand(instruction, 0)) }
    };
    if accessed_type != element_type {
        return Err(Rejection("mixed-size-memory-access"));
    }
    Ok(MemoryAccess {
        base,
        offset,
        kind: if opcode == LLVMLoad {
            AccessKind::Read
        } else {
            AccessKind::Write
        },
        element_type,
    })
}

fn validate_compute_operands(
    instruction: LLVMValueRef,
    header: LLVMBasicBlockRef,
    induction: LLVMValueRef,
) -> Result<(), Rejection> {
    // SAFETY: instruction is live.
    let operand_count = u32::try_from(unsafe { LLVMGetNumOperands(instruction) })
        .expect("LLVM operand counts are non-negative");
    for index in 0..operand_count {
        // SAFETY: index is within the operand array.
        let operand = unsafe { LLVMGetOperand(instruction, index) };
        if operand == induction || is_loop_invariant(operand, header) {
            continue;
        }
        // Loop-local operands are valid only when they are ordinary supported
        // instructions that will already have been widened in SSA order.
        // SAFETY: operand is live.
        if unsafe { LLVMIsAInstruction(operand) }.is_null() {
            return Err(Rejection("unsupported-compute-operand"));
        }
        // SAFETY: operand is an instruction.
        let opcode = unsafe { LLVMGetInstructionOpcode(operand) };
        if !matches!(opcode, LLVMLoad | LLVMGetElementPtr) && !is_supported_compute(opcode) {
            return Err(Rejection("unsupported-compute-dependency"));
        }
    }
    Ok(())
}

fn validate_dependences(
    function: LLVMValueRef,
    accesses: &[MemoryAccess],
) -> Result<(), Rejection> {
    for (left_index, left) in accesses.iter().enumerate() {
        for right in &accesses[left_index + 1..] {
            if left.kind == AccessKind::Read && right.kind == AccessKind::Read {
                continue;
            }
            if left.base != right.base {
                if bases_are_statically_disjoint(function, left.base, right.base) {
                    continue;
                }
                return Err(Rejection("possible-pointer-alias"));
            }
            if left.element_type != right.element_type {
                return Err(Rejection("mixed-type-memory-dependence"));
            }
            let result = classify(
                AffineAccess {
                    base: llvm::value_key(left.base),
                    coefficient: 1,
                    offset: left.offset,
                    kind: left.kind,
                },
                AffineAccess {
                    base: llvm::value_key(right.base),
                    coefficient: 1,
                    offset: right.offset,
                    kind: right.kind,
                },
            );
            if !matches!(result, Dependence::Independent | Dependence::SameIteration) {
                return Err(Rejection("loop-carried-memory-dependence"));
            }
        }
    }
    Ok(())
}

fn bases_are_statically_disjoint(
    function: LLVMValueRef,
    left: LLVMValueRef,
    right: LLVMValueRef,
) -> bool {
    // SAFETY: values are live.
    if !unsafe { LLVMIsAGlobalVariable(left) }.is_null()
        && !unsafe { LLVMIsAGlobalVariable(right) }.is_null()
    {
        return true;
    }
    argument_is_noalias(function, left) && argument_is_noalias(function, right)
}

fn argument_is_noalias(function: LLVMValueRef, value: LLVMValueRef) -> bool {
    // SAFETY: value is live.
    if unsafe { LLVMIsAArgument(value) }.is_null() {
        return false;
    }
    let mut position = 1_u32;
    // SAFETY: function is live.
    let mut argument = unsafe { LLVMGetFirstParam(function) };
    while !argument.is_null() {
        if argument == value {
            return function_has_enum_attribute(function, position, b"noalias");
        }
        position += 1;
        // SAFETY: argument came from this function.
        argument = unsafe { LLVMGetNextParam(argument) };
    }
    false
}

fn function_has_enum_attribute(function: LLVMValueRef, index: u32, name: &[u8]) -> bool {
    // SAFETY: name points to `name.len()` readable bytes.
    let kind = unsafe { LLVMGetEnumAttributeKindForName(name.as_ptr().cast(), name.len()) };
    // SAFETY: index is either LLVM's function index or a validated parameter
    // position belonging to `function`.
    !unsafe { LLVMGetEnumAttributeAtIndex(function, index, kind) }.is_null()
}

fn instruction_width(instruction: LLVMValueRef) -> Result<u32, Rejection> {
    // SAFETY: instruction is live.
    let opcode = unsafe { LLVMGetInstructionOpcode(instruction) };
    let mut width = if matches!(opcode, LLVMICmp | LLVMFCmp) {
        // SAFETY: comparison has at least one operand.
        type_bits(unsafe { LLVMTypeOf(LLVMGetOperand(instruction, 0)) })?
    } else {
        // SAFETY: instruction is live.
        type_bits(unsafe { LLVMTypeOf(instruction) })?
    };
    // Casts may widen from a smaller source.
    if is_cast(opcode) {
        // SAFETY: cast has one operand.
        width = width.max(type_bits(unsafe {
            LLVMTypeOf(LLVMGetOperand(instruction, 0))
        })?);
    }
    Ok(width)
}

const fn instruction_cost(opcode: LLVMOpcode) -> u64 {
    match opcode {
        LLVMUDiv | LLVMSDiv | LLVMURem | LLVMSRem | LLVMFDiv | LLVMFRem => 4,
        _ => 1,
    }
}

const fn is_supported_compute(opcode: LLVMOpcode) -> bool {
    matches!(
        opcode,
        LLVMAdd
            | LLVMFAdd
            | LLVMSub
            | LLVMFSub
            | LLVMMul
            | LLVMFMul
            | LLVMUDiv
            | LLVMSDiv
            | LLVMFDiv
            | LLVMURem
            | LLVMSRem
            | LLVMFRem
            | LLVMShl
            | LLVMLShr
            | LLVMAShr
            | LLVMAnd
            | LLVMOr
            | LLVMXor
            | LLVMFNeg
            | LLVMTrunc
            | LLVMZExt
            | LLVMSExt
            | LLVMFPToUI
            | LLVMFPToSI
            | LLVMUIToFP
            | LLVMSIToFP
            | LLVMFPTrunc
            | LLVMFPExt
            | LLVMICmp
            | LLVMFCmp
            | LLVMSelect
    )
}

const fn is_cast(opcode: LLVMOpcode) -> bool {
    matches!(
        opcode,
        LLVMTrunc
            | LLVMZExt
            | LLVMSExt
            | LLVMFPToUI
            | LLVMFPToSI
            | LLVMUIToFP
            | LLVMSIToFP
            | LLVMFPTrunc
            | LLVMFPExt
            | LLVMPtrToInt
            | LLVMIntToPtr
            | LLVMBitCast
            | LLVMAddrSpaceCast
    )
}

fn type_bits(value_type: LLVMTypeRef) -> Result<u32, Rejection> {
    // SAFETY: type is live.
    match unsafe { LLVMGetTypeKind(value_type) } {
        LLVMIntegerTypeKind => {
            // SAFETY: type is an integer.
            Ok(unsafe { LLVMGetIntTypeWidth(value_type) })
        }
        LLVMHalfTypeKind => Ok(16),
        LLVMFloatTypeKind => Ok(32),
        LLVMDoubleTypeKind => Ok(64),
        _ => Err(Rejection("unsupported-scalar-type")),
    }
}

fn is_supported_scalar_type(value_type: LLVMTypeRef) -> bool {
    // SAFETY: type is live.
    matches!(
        unsafe { LLVMGetTypeKind(value_type) },
        LLVMIntegerTypeKind | LLVMHalfTypeKind | LLVMFloatTypeKind | LLVMDoubleTypeKind
    )
}

fn is_supported_memory_type(value_type: LLVMTypeRef) -> bool {
    // Packed fixed-vector memory layout matches consecutive scalar layout for
    // these deliberately supported primitive widths. Odd-width integers (most
    // importantly i1) are excluded because allocation and packed-vector store
    // sizes need not have the same stride.
    // SAFETY: type is live.
    match unsafe { LLVMGetTypeKind(value_type) } {
        LLVMIntegerTypeKind => {
            // SAFETY: value_type is an integer.
            matches!(unsafe { LLVMGetIntTypeWidth(value_type) }, 8 | 16 | 32 | 64)
        }
        LLVMHalfTypeKind | LLVMFloatTypeKind | LLVMDoubleTypeKind => true,
        _ => false,
    }
}

fn is_loop_invariant(value: LLVMValueRef, header: LLVMBasicBlockRef) -> bool {
    // SAFETY: value is live.
    let instruction = unsafe { LLVMIsAInstruction(value) };
    // SAFETY: instruction is non-null in the right-hand expression.
    instruction.is_null() || unsafe { LLVMGetInstructionParent(instruction) } != header
}

fn constant_signed(value: LLVMValueRef) -> Option<i64> {
    // SAFETY: value is live.
    if unsafe { LLVMIsAConstantInt(value) }.is_null() {
        None
    } else {
        // SAFETY: value is a ConstantInt; this truncates wider-than-i64 values,
        // which are rejected elsewhere because supported indices are <=64-bit.
        Some(unsafe { LLVMConstIntGetSExtValue(value) })
    }
}

fn constant_unsigned(value: LLVMValueRef) -> Option<u64> {
    // SAFETY: value is live.
    if unsafe { LLVMIsAConstantInt(value) }.is_null() {
        None
    } else {
        // SAFETY: value is a ConstantInt.
        Some(unsafe { LLVMConstIntGetZExtValue(value) })
    }
}

#[allow(clippy::too_many_lines)]
unsafe fn transform(module: LLVMModuleRef, candidate: &Candidate, plan: Plan) {
    // From this point onward an unexpected panic must be fail-stop: the FFI
    // boundary cannot truthfully report `PreservedAnalyses::all()` after a
    // partial CFG rewrite.
    mark_ir_mutated();
    // SAFETY: module is live.
    let context = unsafe { LLVMGetModuleContext(module) };
    let builder = Builder::new(context);
    let name = |bytes: &'static [u8]| bytes.as_ptr().cast::<c_char>();

    // SAFETY: function/header are live and names are NUL-terminated.
    let dispatch = unsafe {
        LLVMInsertBasicBlockInContext(context, candidate.header, name(b"rv.vector.dispatch\0"))
    };
    // SAFETY: same as above.
    let vector_body = unsafe {
        LLVMInsertBasicBlockInContext(context, candidate.header, name(b"rv.vector.body\0"))
    };
    // SAFETY: same as above.
    let vector_exit = unsafe {
        LLVMInsertBasicBlockInContext(context, candidate.header, name(b"rv.vector.exit\0"))
    };

    // Rewire the unique incoming scalar edge through the dispatch block.
    // SAFETY: preheader has the validated successor index.
    let preheader_terminator = unsafe { LLVMGetBasicBlockTerminator(candidate.preheader) };
    // SAFETY: validated index and same-function destination.
    unsafe {
        LLVMSetSuccessor(
            preheader_terminator,
            candidate.preheader_successor_index,
            dispatch,
        );
    };
    llvm::set_phi_incoming_block(candidate.induction, candidate.outside_phi_index, dispatch);

    // Dispatch computes a power-of-two vector trip count and protects short
    // loops according to the selected heuristic.
    // SAFETY: builder and blocks are live.
    unsafe { LLVMPositionBuilderAtEnd(builder.raw(), dispatch) };
    // SAFETY: induction is an integer PHI.
    let index_type = unsafe { LLVMTypeOf(candidate.induction) };
    let minimum_trip = candidate_minimum_trip(plan.vf, candidate, plan);
    // SAFETY: index_type is an integer type.
    let minimum = unsafe { LLVMConstInt(index_type, minimum_trip, 0) };
    // SAFETY: trip count has the same type as induction by latch validation.
    let enough = unsafe {
        LLVMBuildICmp(
            builder.raw(),
            LLVMIntUGE,
            candidate.trip_count,
            minimum,
            name(b"rv.enough.iterations\0"),
        )
    };
    let mask_value = !(u64::from(plan.vf) - 1);
    // SAFETY: constant is truncated to index_type's width by LLVM.
    let mask = unsafe { LLVMConstInt(index_type, mask_value, 0) };
    // SAFETY: operands share the integer type.
    let vector_trip = unsafe {
        LLVMBuildAnd(
            builder.raw(),
            candidate.trip_count,
            mask,
            name(b"rv.vector.trip.count\0"),
        )
    };
    // SAFETY: condition is i1 and destinations belong to the function.
    unsafe { LLVMBuildCondBr(builder.raw(), enough, vector_body, candidate.header) };

    // Vector body.
    // SAFETY: builder and block are live.
    unsafe { LLVMPositionBuilderAtEnd(builder.raw(), vector_body) };
    // SAFETY: index_type is a first-class integer type.
    let vector_index = unsafe { LLVMBuildPhi(builder.raw(), index_type, name(b"rv.index\0")) };
    // SAFETY: index_type is integer.
    let zero = unsafe { LLVMConstInt(index_type, 0, 0) };
    let mut initial_values = [zero];
    let mut initial_blocks = [dispatch];
    // SAFETY: PHI and incoming block/value are compatible.
    unsafe {
        LLVMAddIncoming(
            vector_index,
            initial_values.as_mut_ptr(),
            initial_blocks.as_mut_ptr(),
            1,
        );
    };

    let mut values = HashMap::new();
    let mut splats = HashMap::new();
    if candidate.widen_induction {
        let induction_vector = unsafe {
            build_induction_vector(
                builder.raw(),
                vector_index,
                index_type,
                plan.vf,
                name(b"rv.lane.indices\0"),
            )
        };
        values.insert(llvm::value_key(candidate.induction), induction_vector);
    }

    for &instruction in &candidate.body {
        if instruction == candidate.induction
            || instruction == candidate.induction_next
            || instruction == candidate.latch_compare
            // SAFETY: candidate header has a terminator.
            || instruction == unsafe { LLVMGetBasicBlockTerminator(candidate.header) }
            || candidate.address_only.contains(&llvm::value_key(instruction))
        {
            continue;
        }
        // SAFETY: instruction is live.
        let opcode = unsafe { LLVMGetInstructionOpcode(instruction) };
        let widened = match opcode {
            LLVMGetElementPtr => {
                let Ok((base, offset, _, element_type)) =
                    analyze_gep(instruction, candidate.induction, candidate.header)
                else {
                    lowering_invariant_failed("GEP no longer matches the analyzed recipe");
                };
                unsafe {
                    build_vector_gep(
                        builder.raw(),
                        base,
                        vector_index,
                        offset,
                        element_type,
                        instruction,
                    )
                }
            }
            LLVMLoad => {
                // SAFETY: load pointer is operand 0.
                let old_pointer = unsafe { LLVMGetOperand(instruction, 0) };
                let Some(&pointer) = values.get(&llvm::value_key(old_pointer)) else {
                    lowering_invariant_failed("load pointer was not widened in SSA order");
                };
                // SAFETY: load result type is the scalar element type.
                let vector_type = unsafe { LLVMVectorType(LLVMTypeOf(instruction), plan.vf) };
                // SAFETY: pointer names at least VF contiguous elements by legality.
                let load = unsafe {
                    LLVMBuildLoad2(builder.raw(), vector_type, pointer, name(b"rv.wide.load\0"))
                };
                // SAFETY: both are memory operations.
                unsafe { LLVMSetAlignment(load, LLVMGetAlignment(instruction)) };
                load
            }
            LLVMStore => {
                // SAFETY: store has value and pointer operands.
                let old_value = unsafe { LLVMGetOperand(instruction, 0) };
                // SAFETY: store has value and pointer operands.
                let old_pointer = unsafe { LLVMGetOperand(instruction, 1) };
                let Some(value) = (unsafe {
                    widen_operand(
                        builder.raw(),
                        old_value,
                        plan.vf,
                        candidate.header,
                        &values,
                        &mut splats,
                    )
                }) else {
                    lowering_invariant_failed("store value was not widenable after preflight");
                };
                let Some(&pointer) = values.get(&llvm::value_key(old_pointer)) else {
                    lowering_invariant_failed("store pointer was not widened in SSA order");
                };
                // SAFETY: pointer names VF contiguous elements and value is a vector.
                let store = unsafe { LLVMBuildStore(builder.raw(), value, pointer) };
                // SAFETY: both are memory operations.
                unsafe { LLVMSetAlignment(store, LLVMGetAlignment(instruction)) };
                store
            }
            opcode if is_supported_compute(opcode) => {
                let Some(value) = (unsafe {
                    widen_compute(
                        builder.raw(),
                        instruction,
                        opcode,
                        plan.vf,
                        candidate.header,
                        &values,
                        &mut splats,
                    )
                }) else {
                    lowering_invariant_failed("compute node was not widenable after preflight");
                };
                value
            }
            _ => lowering_invariant_failed("unsupported opcode reached lowering"),
        };
        values.insert(llvm::value_key(instruction), widened);
    }

    // SAFETY: index_type is integer.
    let vf_constant = unsafe { LLVMConstInt(index_type, u64::from(plan.vf), 0) };
    // SAFETY: operands have identical integer type.
    let vector_next = unsafe {
        LLVMBuildAdd(
            builder.raw(),
            vector_index,
            vf_constant,
            name(b"rv.index.next\0"),
        )
    };
    // SAFETY: operands have identical integer type.
    let vector_done = unsafe {
        LLVMBuildICmp(
            builder.raw(),
            LLVMIntEQ,
            vector_next,
            vector_trip,
            name(b"rv.vector.done\0"),
        )
    };
    // SAFETY: condition and destinations are valid.
    unsafe { LLVMBuildCondBr(builder.raw(), vector_done, vector_exit, vector_body) };
    let mut next_values = [vector_next];
    let mut next_blocks = [vector_body];
    // SAFETY: adds the backedge incoming value to the vector PHI.
    unsafe {
        LLVMAddIncoming(
            vector_index,
            next_values.as_mut_ptr(),
            next_blocks.as_mut_ptr(),
            1,
        );
    };

    // Vector exit either enters the scalar remainder or bypasses it.
    // SAFETY: builder and block are live.
    unsafe { LLVMPositionBuilderAtEnd(builder.raw(), vector_exit) };
    // SAFETY: operands share the index type.
    let has_remainder = unsafe {
        LLVMBuildICmp(
            builder.raw(),
            LLVMIntNE,
            vector_trip,
            candidate.trip_count,
            name(b"rv.has.remainder\0"),
        )
    };
    // SAFETY: valid condition and destinations.
    unsafe {
        LLVMBuildCondBr(
            builder.raw(),
            has_remainder,
            candidate.header,
            candidate.exit,
        )
    };
    let mut remainder_values = [vector_trip];
    let mut remainder_blocks = [vector_exit];
    // SAFETY: scalar induction PHI accepts the index-typed vector trip count.
    unsafe {
        LLVMAddIncoming(
            candidate.induction,
            remainder_values.as_mut_ptr(),
            remainder_blocks.as_mut_ptr(),
            1,
        );
    };
}

#[cold]
#[inline(never)]
fn lowering_invariant_failed(message: &str) -> ! {
    panic!("vectorization legality/lowering invariant failed: {message}")
}

const fn candidate_minimum_trip(_vf: u32, candidate: &Candidate, _plan: Plan) -> u64 {
    candidate.minimum_trip_count
}

unsafe fn build_induction_vector(
    builder: LLVMBuilderRef,
    scalar_index: LLVMValueRef,
    index_type: LLVMTypeRef,
    vf: u32,
    name: *const c_char,
) -> LLVMValueRef {
    // SAFETY: index_type is integer and vf is a supported fixed width.
    let vector_type = unsafe { LLVMVectorType(index_type, vf) };
    let mut lanes = Vec::with_capacity(vf as usize);
    for lane in 0..vf {
        // SAFETY: index_type is integer.
        lanes.push(unsafe { LLVMConstInt(index_type, u64::from(lane), 0) });
    }
    // SAFETY: all constants have the same type and the slice has vf entries.
    let lane_offsets = unsafe { LLVMConstVector(lanes.as_mut_ptr(), vf) };
    // SAFETY: scalar_index is a first-class scalar integer.
    let splat = unsafe { build_splat(builder, scalar_index, vector_type, vf) };
    // SAFETY: operands are vectors of identical integer type.
    unsafe { LLVMBuildAdd(builder, splat, lane_offsets, name) }
}

unsafe fn build_vector_gep(
    builder: LLVMBuilderRef,
    base: LLVMValueRef,
    vector_index: LLVMValueRef,
    offset: i64,
    element_type: LLVMTypeRef,
    original: LLVMValueRef,
) -> LLVMValueRef {
    // SAFETY: vector_index is integer.
    let index_type = unsafe { LLVMTypeOf(vector_index) };
    let index = if offset == 0 {
        vector_index
    } else {
        // SAFETY: index_type is integer.
        let encoded_offset = u64::from_ne_bytes(offset.to_ne_bytes());
        let constant = unsafe { LLVMConstInt(index_type, encoded_offset, 1) };
        // SAFETY: operands share an integer type.
        unsafe {
            LLVMBuildAdd(
                builder,
                vector_index,
                constant,
                c"rv.address.index".as_ptr(),
            )
        }
    };
    let mut indices = [index];
    // SAFETY: base and source element type came from a validated scalar GEP.
    let gep = unsafe {
        LLVMBuildGEP2(
            builder,
            element_type,
            base,
            indices.as_mut_ptr(),
            1,
            c"rv.wide.ptr".as_ptr(),
        )
    };
    // SAFETY: both values are GEP instructions.
    unsafe { LLVMSetIsInBounds(gep, LLVMIsInBounds(original)) };
    gep
}

unsafe fn widen_compute(
    builder: LLVMBuilderRef,
    instruction: LLVMValueRef,
    opcode: LLVMOpcode,
    vf: u32,
    header: LLVMBasicBlockRef,
    values: &HashMap<usize, LLVMValueRef>,
    splats: &mut HashMap<usize, LLVMValueRef>,
) -> Option<LLVMValueRef> {
    let result = match opcode {
        LLVMFNeg => {
            // SAFETY: unary instruction has one operand.
            let operand = unsafe { LLVMGetOperand(instruction, 0) };
            let value = unsafe { widen_operand(builder, operand, vf, header, values, splats) }?;
            // SAFETY: value is a floating-point vector.
            unsafe { LLVMBuildFNeg(builder, value, c"rv.fneg".as_ptr()) }
        }
        LLVMICmp => {
            // SAFETY: comparison has two operands.
            let left = unsafe { LLVMGetOperand(instruction, 0) };
            let right = unsafe { LLVMGetOperand(instruction, 1) };
            let left = unsafe { widen_operand(builder, left, vf, header, values, splats) }?;
            let right = unsafe { widen_operand(builder, right, vf, header, values, splats) }?;
            // SAFETY: original is ICmp and widened operands match.
            unsafe {
                LLVMBuildICmp(
                    builder,
                    LLVMGetICmpPredicate(instruction),
                    left,
                    right,
                    c"rv.icmp".as_ptr(),
                )
            }
        }
        LLVMFCmp => {
            // SAFETY: comparison has two operands.
            let left = unsafe { LLVMGetOperand(instruction, 0) };
            let right = unsafe { LLVMGetOperand(instruction, 1) };
            let left = unsafe { widen_operand(builder, left, vf, header, values, splats) }?;
            let right = unsafe { widen_operand(builder, right, vf, header, values, splats) }?;
            // SAFETY: original is FCmp and widened operands match.
            unsafe {
                LLVMBuildFCmp(
                    builder,
                    LLVMGetFCmpPredicate(instruction),
                    left,
                    right,
                    c"rv.fcmp".as_ptr(),
                )
            }
        }
        LLVMSelect => {
            // SAFETY: select has condition, true, and false operands.
            let condition = unsafe { LLVMGetOperand(instruction, 0) };
            let on_true = unsafe { LLVMGetOperand(instruction, 1) };
            let on_false = unsafe { LLVMGetOperand(instruction, 2) };
            let condition =
                unsafe { widen_operand(builder, condition, vf, header, values, splats) }?;
            let on_true = unsafe { widen_operand(builder, on_true, vf, header, values, splats) }?;
            let on_false = unsafe { widen_operand(builder, on_false, vf, header, values, splats) }?;
            // SAFETY: widened values have compatible vector types.
            unsafe { LLVMBuildSelect(builder, condition, on_true, on_false, c"rv.select".as_ptr()) }
        }
        opcode if is_cast(opcode) => {
            // SAFETY: cast has one operand.
            let operand = unsafe { LLVMGetOperand(instruction, 0) };
            let operand = unsafe { widen_operand(builder, operand, vf, header, values, splats) }?;
            // SAFETY: instruction result is a supported scalar type.
            let destination = unsafe { LLVMVectorType(LLVMTypeOf(instruction), vf) };
            // SAFETY: opcode and source/destination mirror the legal scalar cast.
            unsafe { LLVMBuildCast(builder, opcode, operand, destination, c"rv.cast".as_ptr()) }
        }
        _ => {
            // SAFETY: supported binary operation has two operands.
            let left = unsafe { LLVMGetOperand(instruction, 0) };
            let right = unsafe { LLVMGetOperand(instruction, 1) };
            let left = unsafe { widen_operand(builder, left, vf, header, values, splats) }?;
            let right = unsafe { widen_operand(builder, right, vf, header, values, splats) }?;
            // SAFETY: opcode is a supported binary op and vector operands match.
            unsafe { LLVMBuildBinOp(builder, opcode, left, right, c"rv.binop".as_ptr()) }
        }
    };
    // Preserve semantic instruction flags where the API permits them.
    // SAFETY: original and result are corresponding instruction kinds.
    unsafe { copy_instruction_flags(instruction, result, opcode) };
    Some(result)
}

unsafe fn widen_operand(
    builder: LLVMBuilderRef,
    operand: LLVMValueRef,
    vf: u32,
    header: LLVMBasicBlockRef,
    values: &HashMap<usize, LLVMValueRef>,
    splats: &mut HashMap<usize, LLVMValueRef>,
) -> Option<LLVMValueRef> {
    if let Some(&mapped) = values.get(&llvm::value_key(operand)) {
        return Some(mapped);
    }
    if !is_loop_invariant(operand, header) {
        return None;
    }
    if let Some(&splat) = splats.get(&llvm::value_key(operand)) {
        return Some(splat);
    }
    // SAFETY: operand is a supported first-class scalar.
    let scalar_type = unsafe { LLVMTypeOf(operand) };
    if !is_supported_scalar_type(scalar_type) {
        return None;
    }
    // SAFETY: valid scalar element and supported vf.
    let vector_type = unsafe { LLVMVectorType(scalar_type, vf) };
    // SAFETY: types satisfy build_splat's preconditions.
    let splat = unsafe { build_splat(builder, operand, vector_type, vf) };
    splats.insert(llvm::value_key(operand), splat);
    Some(splat)
}

unsafe fn build_splat(
    builder: LLVMBuilderRef,
    scalar: LLVMValueRef,
    vector_type: LLVMTypeRef,
    vf: u32,
) -> LLVMValueRef {
    // SAFETY: vector_type is a fixed vector and scalar is its element type.
    let poison = unsafe { LLVMGetPoison(vector_type) };
    // SAFETY: i32 exists in the builder's context.
    let context = unsafe { LLVMGetTypeContext(vector_type) };
    let index_type = unsafe { LLVMInt32TypeInContext(context) };
    let zero = unsafe { LLVMConstInt(index_type, 0, 0) };
    let inserted = unsafe {
        LLVMBuildInsertElement(builder, poison, scalar, zero, c"rv.splat.insert".as_ptr())
    };
    let mut mask = Vec::with_capacity(vf as usize);
    for _ in 0..vf {
        // SAFETY: index_type is integer.
        mask.push(unsafe { LLVMConstInt(index_type, 0, 0) });
    }
    // SAFETY: mask contains vf i32 elements.
    let mask = unsafe { LLVMConstVector(mask.as_mut_ptr(), vf) };
    // SAFETY: input vectors and mask have compatible fixed widths.
    unsafe { LLVMBuildShuffleVector(builder, inserted, poison, mask, c"rv.splat".as_ptr()) }
}

unsafe fn copy_instruction_flags(
    original: LLVMValueRef,
    widened: LLVMValueRef,
    opcode: LLVMOpcode,
) {
    if matches!(opcode, LLVMAdd | LLVMSub | LLVMMul | LLVMShl) {
        // SAFETY: both instructions support nowrap flags.
        unsafe {
            LLVMSetNUW(widened, LLVMGetNUW(original));
            LLVMSetNSW(widened, LLVMGetNSW(original));
        }
    }
    if matches!(opcode, LLVMUDiv | LLVMSDiv | LLVMLShr | LLVMAShr) {
        // SAFETY: both instructions support exact where applicable.
        unsafe { LLVMSetExact(widened, LLVMGetExact(original)) };
    }
    // SAFETY: predicate checks whether both operations support FMF.
    if unsafe { LLVMCanValueUseFastMathFlags(original) } != 0
        && unsafe { LLVMCanValueUseFastMathFlags(widened) } != 0
    {
        // SAFETY: both instructions support fast-math flags.
        unsafe { LLVMSetFastMathFlags(widened, LLVMGetFastMathFlags(original)) };
    }
}

fn report(
    function: LLVMValueRef,
    header: LLVMBasicBlockRef,
    decision: &str,
    reason: &str,
    plan: Option<Plan>,
    analysis: Duration,
    transform: Duration,
) {
    let (vf, vector_coverage, issued_lane_utilization) =
        plan.map_or((0, 0, 0), |plan| (plan.vf, plan.vector_coverage_x100, 100));
    eprintln!(
        "rv-vectorize: function={} loop={} decision={} reason={} vf={} estimated_vector_coverage={}% issued_lane_utilization={}% analysis_us={:.3} transform_us={:.3}",
        report_atom(&llvm::value_name(function)),
        report_atom(&llvm::block_name(header)),
        decision,
        reason,
        vf,
        vector_coverage,
        issued_lane_utilization,
        duration_micros(analysis),
        duration_micros(transform),
    );
}

fn duration_micros(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000_000.0
}

fn report_atom(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.' | b'$') {
            encoded.push(char::from(byte));
        } else {
            write!(encoded, "%{byte:02X}").expect("writing to a String is infallible");
        }
    }
    encoded
}

#[cfg(test)]
mod report_tests {
    use super::report_atom;

    #[test]
    fn diagnostic_names_are_single_percent_encoded_atoms() {
        assert_eq!(
            report_atom("loop with%newline\n"),
            "loop%20with%25newline%0A"
        );
    }
}
