#include <stddef.h>
#include <stdint.h>

#if defined(__clang__) || defined(__GNUC__)
#define BENCH_NOINLINE __attribute__((noinline))
#else
#define BENCH_NOINLINE
#endif

// Keep these loops deliberately conventional. scripts/benchmark.py compiles
// this translation unit exactly once with both LLVM vectorizers disabled, then
// gives the resulting bitcode to each competing transformation.

BENCH_NOINLINE void add_f32(float *restrict out, const float *restrict left,
                            const float *restrict right, size_t n) {
  for (size_t i = 0; i < n; ++i)
    out[i] = left[i] + right[i];
}

BENCH_NOINLINE void mul_add_f32(float *restrict out,
                                const float *restrict left,
                                const float *restrict right, size_t n) {
  for (size_t i = 0; i < n; ++i)
    out[i] = left[i] * 2.0f + right[i];
}

BENCH_NOINLINE void scale_i32(int32_t *restrict out,
                              const int32_t *restrict input, int32_t factor,
                              size_t n) {
  for (size_t i = 0; i < n; ++i)
    out[i] = input[i] * factor;
}

BENCH_NOINLINE void write_index_u64(uint64_t *restrict out, size_t n) {
  for (size_t i = 0; i < n; ++i)
    out[i] = (uint64_t)i;
}

BENCH_NOINLINE void conditional_adjust_i32(int32_t *restrict out,
                                           const int32_t *restrict input,
                                           int32_t pivot, size_t n) {
  for (size_t i = 0; i < n; ++i)
    out[i] = input[i] < pivot ? input[i] + 3 : input[i] - 2;
}

BENCH_NOINLINE void widen_u16(int32_t *restrict out,
                              const uint16_t *restrict input, size_t n) {
  for (size_t i = 0; i < n; ++i)
    out[i] = (int32_t)input[i] + 7;
}

BENCH_NOINLINE void increment_i32(int32_t *restrict data, size_t n) {
  for (size_t i = 0; i < n; ++i)
    data[i] += 1;
}
