#include "llvm/ADT/ArrayRef.h"
#include "llvm/ADT/StringRef.h"
#include "llvm/Analysis/LoopInfo.h"
#include "llvm/IR/BasicBlock.h"
#include "llvm/IR/Constants.h"
#include "llvm/IR/DataLayout.h"
#include "llvm/IR/DerivedTypes.h"
#include "llvm/IR/Instructions.h"
#include "llvm/IR/Module.h"
#include "llvm/IR/PassManager.h"
#include "llvm/Passes/PassBuilder.h"

#include <cstdint>

extern "C" {

struct RVPassConfig {
  std::uint32_t heuristic;
  std::uint32_t forced_vf;
  std::uint32_t emit_remarks;
};

bool rv_run_module(void *module, const RVPassConfig *config);

void *rv_wrap_module(void *module) {
  return llvm::wrap(static_cast<llvm::Module *>(module));
}

void rv_phi_set_incoming_block(void *phi, unsigned index, void *block) {
  auto *node = llvm::cast<llvm::PHINode>(llvm::unwrap(
      reinterpret_cast<LLVMValueRef>(phi)));
  node->setIncomingBlock(
      index, llvm::unwrap(reinterpret_cast<LLVMBasicBlockRef>(block)));
}

bool rv_loop_vectorization_disabled(void *instruction) {
  auto *terminator = llvm::unwrap<llvm::Instruction>(
      reinterpret_cast<LLVMValueRef>(instruction));
  auto *loopID = terminator->getMetadata(llvm::LLVMContext::MD_loop);
  if (!loopID)
    return false;
  auto *option =
      llvm::findOptionMDForLoopID(loopID, "llvm.loop.vectorize.enable");
  if (!option || option->getNumOperands() < 2)
    return false;
  auto *enabled =
      llvm::mdconst::dyn_extract<llvm::ConstantInt>(option->getOperand(1));
  return enabled && enabled->isZero();
}

bool rv_vector_memory_layout_is_packed(void *module, void *basePointer,
                                       void *elementType,
                                       unsigned vectorFactor) {
  auto *llvmModule = static_cast<llvm::Module *>(module);
  auto *baseValue =
      llvm::unwrap(reinterpret_cast<LLVMValueRef>(basePointer));
  auto *scalarType = llvm::unwrap(reinterpret_cast<LLVMTypeRef>(elementType));
  auto *pointerType = llvm::dyn_cast<llvm::PointerType>(baseValue->getType());
  if (!pointerType || !scalarType->isSized() || vectorFactor < 2)
    return false;

  // DataLayout("") exposes generic fallback values, but those are not a
  // promise about the target that will eventually lower this module. Refuse
  // to turn an absent target contract into a memory-equivalence proof.
  if (llvmModule->getDataLayoutStr().empty())
    return false;

  const auto &layout = llvmModule->getDataLayout();
  // The affine proof is over the i64 induction's modular domain. A different
  // GEP index width would truncate or extend before address arithmetic and can
  // change wrap-boundary adjacency.
  if (layout.getIndexSizeInBits(pointerType->getAddressSpace()) != 64)
    return false;
  const auto scalarAllocation = layout.getTypeAllocSize(scalarType);
  const auto scalarStore = layout.getTypeStoreSize(scalarType);
  const auto vectorStore = layout.getTypeStoreSize(
      llvm::FixedVectorType::get(scalarType, vectorFactor));
  if (scalarAllocation.isScalable() || scalarStore.isScalable() ||
      vectorStore.isScalable())
    return false;

  // A vector memory operation is equivalent to VF adjacent scalar operations
  // only when the target layout inserts no per-element allocation padding and
  // the fixed vector itself stores exactly those VF element strides.
  return scalarAllocation.getFixedValue() == scalarStore.getFixedValue() &&
         vectorStore.getFixedValue() ==
             vectorFactor * scalarAllocation.getFixedValue();
}

} // extern "C"

namespace {

class RustLoopVectorizePass
    : public llvm::PassInfoMixin<RustLoopVectorizePass> {
public:
  explicit RustLoopVectorizePass(RVPassConfig config) : Config(config) {}

  llvm::PreservedAnalyses run(llvm::Module &module,
                              llvm::ModuleAnalysisManager &) {
    return rv_run_module(&module, &Config)
               ? llvm::PreservedAnalyses::none()
               : llvm::PreservedAnalyses::all();
  }

private:
  RVPassConfig Config;
};

bool parsePassName(llvm::StringRef name, RVPassConfig &config) {
  config = {/* heuristic = balanced */ 1, /* forced_vf = automatic */ 0,
            /* emit_remarks = false */ 0};

  if (name == "rust-loop-vectorize" ||
      name == "rust-loop-vectorize-balanced")
    return true;
  if (name == "rust-loop-vectorize-report") {
    config.emit_remarks = 1;
    return true;
  }
  if (name == "rust-loop-vectorize-conservative") {
    config.heuristic = 0;
    return true;
  }
  if (name == "rust-loop-vectorize-conservative-report") {
    config.heuristic = 0;
    config.emit_remarks = 1;
    return true;
  }
  if (name == "rust-loop-vectorize-aggressive") {
    config.heuristic = 2;
    return true;
  }
  if (name == "rust-loop-vectorize-aggressive-report") {
    config.heuristic = 2;
    config.emit_remarks = 1;
    return true;
  }
  if (name == "rust-loop-vectorize-force-vf2") {
    config.forced_vf = 2;
    return true;
  }
  if (name == "rust-loop-vectorize-force-vf2-report") {
    config.forced_vf = 2;
    config.emit_remarks = 1;
    return true;
  }
  if (name == "rust-loop-vectorize-force-vf4") {
    config.forced_vf = 4;
    return true;
  }
  if (name == "rust-loop-vectorize-force-vf4-report") {
    config.forced_vf = 4;
    config.emit_remarks = 1;
    return true;
  }
  if (name == "rust-loop-vectorize-force-vf8") {
    config.forced_vf = 8;
    return true;
  }
  if (name == "rust-loop-vectorize-force-vf8-report") {
    config.forced_vf = 8;
    config.emit_remarks = 1;
    return true;
  }
  if (name == "rust-loop-vectorize-force-vf16") {
    config.forced_vf = 16;
    return true;
  }
  if (name == "rust-loop-vectorize-force-vf16-report") {
    config.forced_vf = 16;
    config.emit_remarks = 1;
    return true;
  }
  return false;
}

} // namespace

extern "C" void rv_register_pass_builder_callbacks(void *raw_builder) {
  auto *builder = static_cast<llvm::PassBuilder *>(raw_builder);
  builder->registerPipelineParsingCallback(
      [](llvm::StringRef name, llvm::ModulePassManager &manager,
         llvm::ArrayRef<llvm::PassBuilder::PipelineElement>) {
        RVPassConfig config{};
        if (!parsePassName(name, config))
          return false;
        manager.addPass(RustLoopVectorizePass(config));
        return true;
      });
}
