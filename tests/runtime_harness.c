#if defined(__linux__)
#define _GNU_SOURCE
#endif

#include <stdint.h>
#include <stddef.h>

#if defined(__APPLE__) || defined(__linux__)
#include <sys/mman.h>
#include <unistd.h>
#endif

void add_f32(float *out, const float *left, const float *right, uint64_t n);
void scale_i32(int32_t *out, const int32_t *input, int32_t factor, uint64_t n);
void write_index(uint64_t *out, uint64_t n);
void clamp_min_i32(int32_t *out, const int32_t *input, int32_t minimum, uint64_t n);
void widen_i16(int32_t *out, const uint16_t *input, uint64_t n);
void increment_in_place(int32_t *data, uint64_t n);
void increment_in_place_ne(int32_t *data, uint64_t n);
void increment_in_place_ult(int32_t *data, uint64_t n);
void ordered_double_store(int32_t *data, uint64_t n);

static int test_length(uint64_t n) {
  float left[80], right[80], float_out[80];
  int32_t input[80], int_out[80], in_place[80], ne_data[80], ult_data[80];
  int32_t ordered[80];
  uint16_t narrow[80];
  uint64_t indices[80];

  for (size_t i = 0; i < 80; ++i) {
    left[i] = (float)(i * 2);
    right[i] = (float)(100 - i);
    float_out[i] = -1.0f;
    input[i] = (int32_t)i - 19;
    int_out[i] = -9999;
    in_place[i] = (int32_t)(3 * i);
    ne_data[i] = (int32_t)(5 * i);
    ult_data[i] = (int32_t)(7 * i);
    ordered[i] = -12345;
    narrow[i] = (uint16_t)(i * 7);
    indices[i] = UINT64_MAX;
  }

  add_f32(float_out, left, right, n);
  for (uint64_t i = 0; i < n; ++i)
    if (float_out[i] != left[i] + right[i]) return 10;
  for (uint64_t i = n; i < 80; ++i)
    if (float_out[i] != -1.0f) return 11;

  scale_i32(int_out, input, -3, n);
  for (uint64_t i = 0; i < n; ++i)
    if (int_out[i] != input[i] * -3) return 20;

  write_index(indices, n);
  for (uint64_t i = 0; i < n; ++i)
    if (indices[i] != i) return 30;
  for (uint64_t i = n; i < 80; ++i)
    if (indices[i] != UINT64_MAX) return 31;

  clamp_min_i32(int_out, input, 4, n);
  for (uint64_t i = 0; i < n; ++i) {
    int32_t expected = input[i] < 4 ? 4 : input[i];
    if (int_out[i] != expected) return 40;
  }

  widen_i16(int_out, narrow, n);
  for (uint64_t i = 0; i < n; ++i)
    if (int_out[i] != (int32_t)narrow[i] + 7) return 50;

  increment_in_place(in_place, n);
  for (uint64_t i = 0; i < n; ++i)
    if (in_place[i] != (int32_t)(3 * i + 1)) return 60;
  for (uint64_t i = n; i < 80; ++i)
    if (in_place[i] != (int32_t)(3 * i)) return 61;

  increment_in_place_ne(ne_data, n);
  for (uint64_t i = 0; i < n; ++i)
    if (ne_data[i] != (int32_t)(5 * i + 2)) return 70;
  for (uint64_t i = n; i < 80; ++i)
    if (ne_data[i] != (int32_t)(5 * i)) return 71;

  increment_in_place_ult(ult_data, n);
  for (uint64_t i = 0; i < n; ++i)
    if (ult_data[i] != (int32_t)(7 * i + 3)) return 80;
  for (uint64_t i = n; i < 80; ++i)
    if (ult_data[i] != (int32_t)(7 * i)) return 81;

  ordered_double_store(ordered, n);
  for (uint64_t i = 0; i < n; ++i)
    if (ordered[i] != 222) return 90;
  for (uint64_t i = n; i < 80; ++i)
    if (ordered[i] != -12345) return 91;

  return 0;
}

/* Place both arrays immediately before inaccessible pages. For n=2*VF+1, a
 * correct full-vector-plus-scalar-tail lowering stays in bounds and passes the
 * balanced policy's 2*VF threshold; any rounded-up tail load or store crosses
 * a guard page and terminates the test process. */
static int test_guarded_tail_access(void) {
#if defined(__APPLE__) || defined(__linux__)
  const long page_size = sysconf(_SC_PAGESIZE);
  if (page_size <= 0) return 100;
  const size_t mapping_size = (size_t)page_size * 2;
  void *input_mapping = mmap(NULL, mapping_size, PROT_READ | PROT_WRITE,
                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  if (input_mapping == MAP_FAILED) return 101;
  void *output_mapping = mmap(NULL, mapping_size, PROT_READ | PROT_WRITE,
                              MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  if (output_mapping == MAP_FAILED) {
    (void)munmap(input_mapping, mapping_size);
    return 102;
  }
  if (mprotect((char *)input_mapping + page_size, (size_t)page_size, PROT_NONE) != 0 ||
      mprotect((char *)output_mapping + page_size, (size_t)page_size, PROT_NONE) != 0) {
    (void)munmap(input_mapping, mapping_size);
    (void)munmap(output_mapping, mapping_size);
    return 103;
  }

  enum { element_count = 9 }; /* i32 VF=4, hence one scalar remainder. */
  int32_t *input = (int32_t *)((char *)input_mapping + page_size) - element_count;
  int32_t *output = (int32_t *)((char *)output_mapping + page_size) - element_count;
  for (size_t i = 0; i < element_count; ++i) {
    input[i] = (int32_t)(i + 1);
    output[i] = -1;
  }
  scale_i32(output, input, 9, element_count);
  for (size_t i = 0; i < element_count; ++i) {
    if (output[i] != input[i] * 9) {
      (void)munmap(input_mapping, mapping_size);
      (void)munmap(output_mapping, mapping_size);
      return 104;
    }
  }
  (void)munmap(input_mapping, mapping_size);
  (void)munmap(output_mapping, mapping_size);
#endif
  return 0;
}

int main(void) {
  static const uint64_t lengths[] = {
      0, 1, 2, 3, 4, 5, 7, 8, 9, 15, 16, 17, 31, 32, 33, 63, 64, 65,
  };
  for (size_t i = 0; i < sizeof(lengths) / sizeof(lengths[0]); ++i) {
    int result = test_length(lengths[i]);
    if (result != 0) return result;
  }
  return test_guarded_tail_access();
}
