#define _POSIX_C_SOURCE 200809L

#include <errno.h>
#include <inttypes.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

void add_f32(float *restrict out, const float *restrict left,
             const float *restrict right, size_t n);
void mul_add_f32(float *restrict out, const float *restrict left,
                 const float *restrict right, size_t n);
void scale_i32(int32_t *restrict out, const int32_t *restrict input,
               int32_t factor, size_t n);
void write_index_u64(uint64_t *restrict out, size_t n);
void conditional_adjust_i32(int32_t *restrict out,
                            const int32_t *restrict input, int32_t pivot,
                            size_t n);
void widen_u16(int32_t *restrict out, const uint16_t *restrict input,
               size_t n);
void increment_i32(int32_t *restrict data, size_t n);

static volatile uint64_t benchmark_sink;

static uint64_t monotonic_ns(void) {
  struct timespec now;
  if (clock_gettime(CLOCK_MONOTONIC, &now) != 0) {
    perror("clock_gettime");
    exit(90);
  }
  return (uint64_t)now.tv_sec * UINT64_C(1000000000) +
         (uint64_t)now.tv_nsec;
}

static void compiler_barrier(void) {
#if defined(__clang__) || defined(__GNUC__)
  __asm__ volatile("" ::: "memory");
#endif
}

static void *allocate_aligned(size_t count, size_t element_size) {
  if (element_size != 0 && count > SIZE_MAX / element_size) {
    fputs("allocation size overflow\n", stderr);
    exit(91);
  }
  void *memory = NULL;
  int error = posix_memalign(&memory, 64, count * element_size);
  if (error != 0) {
    fprintf(stderr, "posix_memalign: %s\n", strerror(error));
    exit(92);
  }
  return memory;
}

static int check_small_length(size_t n) {
  float left[80], right[80], float_out[80];
  int32_t input[80], int_out[80], in_place[80];
  uint16_t narrow[80];
  uint64_t indices[80];

  for (size_t i = 0; i < 80; ++i) {
    left[i] = (float)(i * 2);
    right[i] = (float)(100 - i);
    float_out[i] = -1234.0f;
    input[i] = (int32_t)i - 19;
    int_out[i] = INT32_C(-999999);
    in_place[i] = (int32_t)(3 * i);
    narrow[i] = (uint16_t)(i * 7);
    indices[i] = UINT64_MAX;
  }

  add_f32(float_out, left, right, n);
  for (size_t i = 0; i < n; ++i)
    if (float_out[i] != left[i] + right[i])
      return 10;
  for (size_t i = n; i < 80; ++i)
    if (float_out[i] != -1234.0f)
      return 11;

  for (size_t i = 0; i < 80; ++i)
    float_out[i] = -1234.0f;
  mul_add_f32(float_out, left, right, n);
  for (size_t i = 0; i < n; ++i)
    if (float_out[i] != left[i] * 2.0f + right[i])
      return 20;
  for (size_t i = n; i < 80; ++i)
    if (float_out[i] != -1234.0f)
      return 21;

  scale_i32(int_out, input, -3, n);
  for (size_t i = 0; i < n; ++i)
    if (int_out[i] != input[i] * -3)
      return 30;

  write_index_u64(indices, n);
  for (size_t i = 0; i < n; ++i)
    if (indices[i] != i)
      return 40;
  for (size_t i = n; i < 80; ++i)
    if (indices[i] != UINT64_MAX)
      return 41;

  conditional_adjust_i32(int_out, input, 4, n);
  for (size_t i = 0; i < n; ++i) {
    int32_t expected = input[i] < 4 ? input[i] + 3 : input[i] - 2;
    if (int_out[i] != expected)
      return 50;
  }

  widen_u16(int_out, narrow, n);
  for (size_t i = 0; i < n; ++i)
    if (int_out[i] != (int32_t)narrow[i] + 7)
      return 60;

  increment_i32(in_place, n);
  for (size_t i = 0; i < n; ++i)
    if (in_place[i] != (int32_t)(3 * i + 1))
      return 70;
  for (size_t i = n; i < 80; ++i)
    if (in_place[i] != (int32_t)(3 * i))
      return 71;

  return 0;
}

static int check_edge_cases(void) {
  static const size_t lengths[] = {0,  1,  2,  3,  4,  7,  8,  9,  15,
                                   16, 17, 31, 32, 33, 63, 64, 65};
  for (size_t i = 0; i < sizeof(lengths) / sizeof(lengths[0]); ++i) {
    int result = check_small_length(lengths[i]);
    if (result != 0) {
      fprintf(stderr, "correctness failure: n=%zu code=%d\n", lengths[i],
              result);
      return result;
    }
  }
  return 0;
}

typedef struct {
  float *left;
  float *right;
  float *float_out;
  int32_t *input;
  int32_t *int_out;
  int32_t *in_place;
  uint16_t *narrow;
  uint64_t *indices;
} Buffers;

static Buffers create_buffers(size_t n) {
  Buffers buffers = {
      .left = allocate_aligned(n, sizeof(float)),
      .right = allocate_aligned(n, sizeof(float)),
      .float_out = allocate_aligned(n, sizeof(float)),
      .input = allocate_aligned(n, sizeof(int32_t)),
      .int_out = allocate_aligned(n, sizeof(int32_t)),
      .in_place = allocate_aligned(n, sizeof(int32_t)),
      .narrow = allocate_aligned(n, sizeof(uint16_t)),
      .indices = allocate_aligned(n, sizeof(uint64_t)),
  };
  return buffers;
}

static void destroy_buffers(Buffers *buffers) {
  free(buffers->left);
  free(buffers->right);
  free(buffers->float_out);
  free(buffers->input);
  free(buffers->int_out);
  free(buffers->in_place);
  free(buffers->narrow);
  free(buffers->indices);
}

static void prepare_buffers(Buffers *buffers, size_t n) {
  for (size_t i = 0; i < n; ++i) {
    buffers->left[i] = (float)(i % 97);
    buffers->right[i] = (float)(200 - (i % 89));
    buffers->float_out[i] = -1234.0f;
    buffers->input[i] = (int32_t)(i % 1000) - 500;
    buffers->int_out[i] = INT32_C(-999999);
    buffers->in_place[i] = (int32_t)(i % 1000) - 500;
    buffers->narrow[i] = (uint16_t)(i % 60000);
    buffers->indices[i] = UINT64_MAX;
  }
}

typedef enum {
  K_ADD_F32,
  K_MUL_ADD_F32,
  K_SCALE_I32,
  K_WRITE_INDEX_U64,
  K_CONDITIONAL_ADJUST_I32,
  K_WIDEN_U16,
  K_INCREMENT_I32,
  KERNEL_COUNT
} Kernel;

static const char *const kernel_names[KERNEL_COUNT] = {
    "add_f32",     "mul_add_f32",          "scale_i32",
    "write_index_u64", "conditional_adjust_i32", "widen_u16",
    "increment_i32",
};

static uint64_t run_timed(Kernel kernel, Buffers *buffers, size_t n,
                          size_t inner) {
  compiler_barrier();
  uint64_t start = monotonic_ns();
  switch (kernel) {
  case K_ADD_F32:
    for (size_t i = 0; i < inner; ++i)
      add_f32(buffers->float_out, buffers->left, buffers->right, n);
    break;
  case K_MUL_ADD_F32:
    for (size_t i = 0; i < inner; ++i)
      mul_add_f32(buffers->float_out, buffers->left, buffers->right, n);
    break;
  case K_SCALE_I32:
    for (size_t i = 0; i < inner; ++i)
      scale_i32(buffers->int_out, buffers->input, -3, n);
    break;
  case K_WRITE_INDEX_U64:
    for (size_t i = 0; i < inner; ++i)
      write_index_u64(buffers->indices, n);
    break;
  case K_CONDITIONAL_ADJUST_I32:
    for (size_t i = 0; i < inner; ++i)
      conditional_adjust_i32(buffers->int_out, buffers->input, 7, n);
    break;
  case K_WIDEN_U16:
    for (size_t i = 0; i < inner; ++i)
      widen_u16(buffers->int_out, buffers->narrow, n);
    break;
  case K_INCREMENT_I32:
    for (size_t i = 0; i < inner; ++i)
      increment_i32(buffers->in_place, n);
    break;
  case KERNEL_COUNT:
    abort();
  }
  compiler_barrier();
  return monotonic_ns() - start;
}

static int validate_large(Kernel kernel, const Buffers *buffers, size_t n,
                          size_t inner) {
  for (size_t i = 0; i < n; ++i) {
    switch (kernel) {
    case K_ADD_F32:
      if (buffers->float_out[i] != buffers->left[i] + buffers->right[i])
        return 1;
      break;
    case K_MUL_ADD_F32:
      if (buffers->float_out[i] !=
          buffers->left[i] * 2.0f + buffers->right[i])
        return 1;
      break;
    case K_SCALE_I32:
      if (buffers->int_out[i] != buffers->input[i] * -3)
        return 1;
      break;
    case K_WRITE_INDEX_U64:
      if (buffers->indices[i] != i)
        return 1;
      break;
    case K_CONDITIONAL_ADJUST_I32: {
      int32_t expected = buffers->input[i] < 7 ? buffers->input[i] + 3
                                               : buffers->input[i] - 2;
      if (buffers->int_out[i] != expected)
        return 1;
      break;
    }
    case K_WIDEN_U16:
      if (buffers->int_out[i] != (int32_t)buffers->narrow[i] + 7)
        return 1;
      break;
    case K_INCREMENT_I32:
      if (buffers->in_place[i] !=
          (int32_t)(i % 1000) - 500 + (int32_t)inner)
        return 1;
      break;
    case KERNEL_COUNT:
      return 1;
    }
  }

  size_t middle = n / 2;
  switch (kernel) {
  case K_ADD_F32:
  case K_MUL_ADD_F32:
    benchmark_sink += (uint64_t)(buffers->float_out[middle] + 4096.0f);
    break;
  case K_SCALE_I32:
  case K_CONDITIONAL_ADJUST_I32:
  case K_WIDEN_U16:
    benchmark_sink += (uint64_t)(uint32_t)buffers->int_out[middle];
    break;
  case K_WRITE_INDEX_U64:
    benchmark_sink += buffers->indices[middle];
    break;
  case K_INCREMENT_I32:
    benchmark_sink += (uint64_t)(uint32_t)buffers->in_place[middle];
    break;
  case KERNEL_COUNT:
    return 1;
  }
  return 0;
}

static size_t parse_size(const char *text, const char *name, int allow_zero) {
  errno = 0;
  char *end = NULL;
  unsigned long long value = strtoull(text, &end, 10);
  if (errno != 0 || end == text || *end != '\0' ||
      (!allow_zero && value == 0) || value > SIZE_MAX) {
    fprintf(stderr, "invalid %s: %s\n", name, text);
    exit(93);
  }
  return (size_t)value;
}

int main(int argc, char **argv) {
  if (argc != 5) {
    fprintf(stderr, "usage: %s ELEMENTS WARMUPS SAMPLES INNER_CALLS\n",
            argv[0]);
    return 2;
  }
  if (sizeof(size_t) != sizeof(uint64_t)) {
    fputs("this benchmark requires a 64-bit target\n", stderr);
    return 3;
  }

  size_t n = parse_size(argv[1], "elements", 0);
  size_t warmups = parse_size(argv[2], "warmups", 1);
  size_t samples = parse_size(argv[3], "samples", 0);
  size_t inner = parse_size(argv[4], "inner calls", 0);

  int edge_result = check_edge_cases();
  if (edge_result != 0)
    return edge_result;

  Buffers buffers = create_buffers(n);
  for (Kernel kernel = K_ADD_F32; kernel < KERNEL_COUNT; ++kernel) {
    for (size_t warmup = 0; warmup < warmups; ++warmup) {
      prepare_buffers(&buffers, n);
      (void)run_timed(kernel, &buffers, n, inner);
      if (validate_large(kernel, &buffers, n, inner) != 0) {
        fprintf(stderr, "warmup correctness failure: kernel=%s\n",
                kernel_names[kernel]);
        destroy_buffers(&buffers);
        return 80 + (int)kernel;
      }
    }
    for (size_t sample = 0; sample < samples; ++sample) {
      prepare_buffers(&buffers, n);
      uint64_t elapsed = run_timed(kernel, &buffers, n, inner);
      if (validate_large(kernel, &buffers, n, inner) != 0) {
        fprintf(stderr, "sample correctness failure: kernel=%s sample=%zu\n",
                kernel_names[kernel], sample);
        destroy_buffers(&buffers);
        return 100 + (int)kernel;
      }
      printf("{\"kind\":\"sample\",\"kernel\":\"%s\","
             "\"sample\":%zu,\"elapsed_ns\":%" PRIu64
             ",\"inner_calls\":%zu,\"elements\":%zu}\n",
             kernel_names[kernel], sample, elapsed, inner, n);
    }
  }

  printf("{\"kind\":\"status\",\"correctness\":\"ok\","
         "\"sink\":%" PRIu64 "}\n",
         benchmark_sink);
  destroy_buffers(&buffers);
  return 0;
}
