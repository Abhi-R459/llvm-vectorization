// vectorizer_playground.c
//
// A single, self-contained test file for exercising rv-vectorize end to
// end. It is deliberately NOT wired into benchmarks/ or the automated
// test suite -- it's a hand-editable playground for demos and manual
// testing, in two parts:
//
//   1. "ACCEPTED" kernels: canonical loops the pass is designed to
//      vectorize (see the "Supported loop subset" section of the
//      project README).
//   2. "REJECTED" kernels: loops that hit one of the documented
//      unsupported shapes (reductions, calls, multi-block control flow,
//      non-unit/descending induction, unsafe same-base dependence) and
//      must correctly stay scalar.
//
// main() is a plain, non-vectorized correctness check: it runs every
// kernel with ordinary scalar C and asserts the results, independent of
// this project entirely. That's step 1 of testing this file -- see the
// "How to test this file" instructions wherever you copied this from.
//
// Step 2 is feeding it through the actual pass, e.g.:
//
//   clang -O1 -fno-vectorize -fno-slp-vectorize -fno-unroll-loops \
//       -ffp-contract=off -emit-llvm -S examples/vectorizer_playground.c \
//       -o build/playground.ll
//   target/release/rv-vectorize --report build/playground.ll \
//       -o build/playground-vectorized.ll
//
// The ACCEPTED functions below should report decision=vectorized; the
// REJECTED functions should report decision=rejected (or simply never
// appear as a report line, depending on why they were skipped).

#include <assert.h>
#include <math.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

#if defined(__clang__) || defined(__GNUC__)
#define PLAYGROUND_NOINLINE __attribute__((noinline))
#else
#define PLAYGROUND_NOINLINE
#endif

// An odd, non-power-of-two length so every kernel below exercises both
// a full-vector-width path and a scalar remainder tail once vectorized.
#define PLAYGROUND_N 37

// ---------------------------------------------------------------------
// ACCEPTED: canonical loops the pass should vectorize.
// ---------------------------------------------------------------------

// Elementwise float add: two loads, one fadd, one store.
PLAYGROUND_NOINLINE void add_f32(float *restrict out, const float *restrict a,
                                  const float *restrict b, size_t n) {
  for (size_t i = 0; i < n; ++i)
    out[i] = a[i] + b[i];
}

// Elementwise int32 subtract.
PLAYGROUND_NOINLINE void sub_i32(int32_t *restrict out,
                                  const int32_t *restrict a,
                                  const int32_t *restrict b, size_t n) {
  for (size_t i = 0; i < n; ++i)
    out[i] = a[i] - b[i];
}

// Elementwise double multiply.
PLAYGROUND_NOINLINE void mul_f64(double *restrict out,
                                  const double *restrict a,
                                  const double *restrict b, size_t n) {
  for (size_t i = 0; i < n; ++i)
    out[i] = a[i] * b[i];
}

// out[i] = scale * x[i] + y[i] -- classic AXPY, two loads plus a scalar
// broadcast.
PLAYGROUND_NOINLINE void axpy_f32(float *restrict out, float scale,
                                   const float *restrict x,
                                   const float *restrict y, size_t n) {
  for (size_t i = 0; i < n; ++i)
    out[i] = scale * x[i] + y[i];
}

// Fused-looking multiply-add across three separate arrays.
PLAYGROUND_NOINLINE void fma_f32(float *restrict out, const float *restrict a,
                                  const float *restrict b,
                                  const float *restrict c, size_t n) {
  for (size_t i = 0; i < n; ++i)
    out[i] = a[i] * b[i] + c[i];
}

// Integer scale-and-shift: exercises mul, add, and a scalar-broadcast
// constant together.
PLAYGROUND_NOINLINE void scale_and_shift_i32(int32_t *restrict out,
                                              const int32_t *restrict in,
                                              int32_t factor, int32_t offset,
                                              size_t n) {
  for (size_t i = 0; i < n; ++i)
    out[i] = in[i] * factor + offset;
}

// Zero out anything outside [lo, hi]. Written as a two-comparison
// select rather than a textbook `a < b ? a : b` clamp on purpose: that
// exact shape gets canonicalized by LLVM into an `llvm.smin`/`llvm.smax`
// intrinsic call at -O1, which this pass treats as an unsupported call
// and rejects -- a good reminder that "no calls/intrinsics" can bite
// even innocuous-looking arithmetic. This OR-of-comparisons form stays
// a plain `select` instead, so it's genuinely vectorizable.
PLAYGROUND_NOINLINE void zero_out_of_range_i32(int32_t *restrict out,
                                                const int32_t *restrict in,
                                                int32_t lo, int32_t hi,
                                                size_t n) {
  for (size_t i = 0; i < n; ++i) {
    int32_t value = in[i];
    out[i] = (value < lo || value > hi) ? 0 : value;
  }
}

// A comparison feeding a select, producing a 0/1 mask per element.
PLAYGROUND_NOINLINE void threshold_mask_i32(int32_t *restrict out,
                                             const int32_t *restrict in,
                                             int32_t threshold, size_t n) {
  for (size_t i = 0; i < n; ++i)
    out[i] = in[i] >= threshold ? 1 : 0;
}

// Bitwise AND with a broadcast mask.
PLAYGROUND_NOINLINE void bitwise_and_u32(uint32_t *restrict out,
                                          const uint32_t *restrict in,
                                          uint32_t mask, size_t n) {
  for (size_t i = 0; i < n; ++i)
    out[i] = in[i] & mask;
}

// Left shift by a uniform amount.
PLAYGROUND_NOINLINE void shift_left_u32(uint32_t *restrict out,
                                         const uint32_t *restrict in,
                                         uint32_t amount, size_t n) {
  for (size_t i = 0; i < n; ++i)
    out[i] = in[i] << amount;
}

// Widening cast: narrow load, widen, add a constant, wide store.
PLAYGROUND_NOINLINE void widen_u16_to_i32(int32_t *restrict out,
                                           const uint16_t *restrict in,
                                           size_t n) {
  for (size_t i = 0; i < n; ++i)
    out[i] = (int32_t)in[i] + 1;
}

// In-place update of a single array (no separate output buffer).
PLAYGROUND_NOINLINE void increment_in_place_f32(float *restrict data,
                                                 float delta, size_t n) {
  for (size_t i = 0; i < n; ++i)
    data[i] += delta;
}

// ---------------------------------------------------------------------
// REJECTED: loops that must correctly stay scalar. Each is still
// ordinary, correct C -- just outside what the pass will *prove* safe
// to widen, per the "Not yet supported" list in the README.
// ---------------------------------------------------------------------

// A reduction: every iteration depends on the previous one through
// `total`, so there is no fixed-width vector rewrite without a proper
// horizontal-reduction lowering (not yet implemented).
PLAYGROUND_NOINLINE float sum_reduction_f32(const float *restrict in,
                                             size_t n) {
  float total = 0.0f;
  for (size_t i = 0; i < n; ++i)
    total += in[i];
  return total;
}

// A call inside the loop body. Calls/intrinsics are not modeled by the
// dependence/cost analysis, so the loop is rejected outright.
PLAYGROUND_NOINLINE void call_sqrt_f32(float *restrict out,
                                        const float *restrict in, size_t n) {
  for (size_t i = 0; i < n; ++i)
    out[i] = sqrtf(in[i]);
}

// Genuine multi-block control flow: the `continue` means not every
// iteration writes to `out`, so this cannot be if-converted into a
// single-block `select` the way clamp_i32 above was.
PLAYGROUND_NOINLINE void safe_divide_i32(int32_t *restrict out,
                                          const int32_t *restrict a,
                                          const int32_t *restrict b,
                                          size_t n) {
  for (size_t i = 0; i < n; ++i) {
    if (b[i] == 0) {
      continue;
    }
    out[i] = a[i] / b[i];
  }
}

// Non-unit stride: the induction variable advances by 2, not 1.
PLAYGROUND_NOINLINE void stride2_double_i32(int32_t *restrict out,
                                             const int32_t *restrict in,
                                             size_t n) {
  for (size_t i = 0; i + 1 < n; i += 2)
    out[i] = in[i] * 2;
}

// Descending induction: the loop counts down instead of up from zero.
PLAYGROUND_NOINLINE void countdown_copy_i32(int32_t *restrict out,
                                             const int32_t *restrict in,
                                             size_t n) {
  for (size_t i = n; i-- > 0;)
    out[i] = in[i];
}

// Unsafe same-base dependence: each write depends on the immediately
// preceding element of the *same* array, one iteration back. Widening
// this would read stale, not-yet-written lanes -- it isn't just
// "currently unsupported", it would be a genuine miscompile, which is
// exactly the kind of loop the dependence analysis exists to catch.
PLAYGROUND_NOINLINE void running_increment_i32(int32_t *restrict data,
                                                size_t n) {
  for (size_t i = 1; i < n; ++i)
    data[i] = data[i - 1] + 1;
}

// ---------------------------------------------------------------------
// main(): a plain scalar correctness check, independent of the
// vectorizer. Run this binary directly to confirm every kernel above
// still does the right thing before you ever hand the file to
// rv-vectorize.
// ---------------------------------------------------------------------

int main(void) {
  const size_t n = PLAYGROUND_N;

  float f_a[PLAYGROUND_N], f_b[PLAYGROUND_N], f_c[PLAYGROUND_N];
  float f_out[PLAYGROUND_N];
  int32_t i_a[PLAYGROUND_N], i_b[PLAYGROUND_N], i_out[PLAYGROUND_N];
  double d_a[PLAYGROUND_N], d_b[PLAYGROUND_N], d_out[PLAYGROUND_N];
  uint32_t u_in[PLAYGROUND_N], u_out[PLAYGROUND_N];
  uint16_t u16_in[PLAYGROUND_N];

  for (size_t i = 0; i < n; ++i) {
    f_a[i] = (float)i * 0.5f;
    f_b[i] = (float)i - 3.0f;
    f_c[i] = 1.5f;
    i_a[i] = (int32_t)i - 18;
    i_b[i] = (int32_t)(i % 5) + 1;
    d_a[i] = (double)i * 1.25;
    d_b[i] = (double)i + 2.0;
    u_in[i] = (uint32_t)i * 3u;
    u16_in[i] = (uint16_t)(i * 7u);
  }

  // --- ACCEPTED kernels ---

  add_f32(f_out, f_a, f_b, n);
  for (size_t i = 0; i < n; ++i)
    assert(f_out[i] == f_a[i] + f_b[i]);

  sub_i32(i_out, i_a, i_b, n);
  for (size_t i = 0; i < n; ++i)
    assert(i_out[i] == i_a[i] - i_b[i]);

  mul_f64(d_out, d_a, d_b, n);
  for (size_t i = 0; i < n; ++i)
    assert(d_out[i] == d_a[i] * d_b[i]);

  axpy_f32(f_out, 2.0f, f_a, f_b, n);
  for (size_t i = 0; i < n; ++i)
    assert(f_out[i] == 2.0f * f_a[i] + f_b[i]);

  fma_f32(f_out, f_a, f_b, f_c, n);
  for (size_t i = 0; i < n; ++i)
    assert(f_out[i] == f_a[i] * f_b[i] + f_c[i]);

  scale_and_shift_i32(i_out, i_a, 3, -1, n);
  for (size_t i = 0; i < n; ++i)
    assert(i_out[i] == i_a[i] * 3 - 1);

  zero_out_of_range_i32(i_out, i_a, -5, 5, n);
  for (size_t i = 0; i < n; ++i) {
    int32_t expected = (i_a[i] < -5 || i_a[i] > 5) ? 0 : i_a[i];
    assert(i_out[i] == expected);
  }

  threshold_mask_i32(i_out, i_a, 0, n);
  for (size_t i = 0; i < n; ++i)
    assert(i_out[i] == (i_a[i] >= 0 ? 1 : 0));

  bitwise_and_u32(u_out, u_in, 0xFu, n);
  for (size_t i = 0; i < n; ++i)
    assert(u_out[i] == (u_in[i] & 0xFu));

  shift_left_u32(u_out, u_in, 2, n);
  for (size_t i = 0; i < n; ++i)
    assert(u_out[i] == (u_in[i] << 2));

  widen_u16_to_i32(i_out, u16_in, n);
  for (size_t i = 0; i < n; ++i)
    assert(i_out[i] == (int32_t)u16_in[i] + 1);

  for (size_t i = 0; i < n; ++i)
    f_out[i] = f_a[i];
  increment_in_place_f32(f_out, 4.0f, n);
  for (size_t i = 0; i < n; ++i)
    assert(f_out[i] == f_a[i] + 4.0f);

  // --- REJECTED kernels (still correct, just not vectorized) ---

  float expected_sum = 0.0f;
  for (size_t i = 0; i < n; ++i)
    expected_sum += f_a[i];
  assert(sum_reduction_f32(f_a, n) == expected_sum);

  call_sqrt_f32(f_out, u_in[0] ? f_a : f_a, n); // silence unused warnings
  for (size_t i = 0; i < n; ++i)
    assert(fabsf(f_out[i] - sqrtf(f_a[i])) < 1e-5f);

  for (size_t i = 0; i < n; ++i)
    i_out[i] = -1;
  safe_divide_i32(i_out, i_a, i_b, n);
  for (size_t i = 0; i < n; ++i) {
    if (i_b[i] == 0) {
      assert(i_out[i] == -1); // untouched, as intended
    } else {
      assert(i_out[i] == i_a[i] / i_b[i]);
    }
  }

  for (size_t i = 0; i < n; ++i)
    i_out[i] = -1;
  stride2_double_i32(i_out, i_a, n);
  for (size_t i = 0; i + 1 < n; i += 2)
    assert(i_out[i] == i_a[i] * 2);

  countdown_copy_i32(i_out, i_a, n);
  for (size_t i = 0; i < n; ++i)
    assert(i_out[i] == i_a[i]);

  int32_t chain[PLAYGROUND_N];
  chain[0] = 100;
  for (size_t i = 1; i < n; ++i)
    chain[i] = 0;
  running_increment_i32(chain, n);
  for (size_t i = 0; i < n; ++i)
    assert(chain[i] == 100 + (int32_t)i);

  printf("vectorizer_playground: all %d kernels produced correct scalar "
         "results (n = %zu).\n",
         12 /* accepted */ + 6 /* rejected */, n);
  return 0;
}