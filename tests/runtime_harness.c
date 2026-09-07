#include <stdint.h>
#include <stddef.h>

void add_f32(float *out, const float *left, const float *right, uint64_t n);
void scale_i32(int32_t *out, const int32_t *input, int32_t factor, uint64_t n);
void write_index(uint64_t *out, uint64_t n);
void clamp_min_i32(int32_t *out, const int32_t *input, int32_t minimum, uint64_t n);
void widen_i16(int32_t *out, const uint16_t *input, uint64_t n);
void increment_in_place(int32_t *data, uint64_t n);

static int test_length(uint64_t n) {
  float left[80], right[80], float_out[80];
  int32_t input[80], int_out[80], in_place[80];
  uint16_t narrow[80];
  uint64_t indices[80];

  for (size_t i = 0; i < 80; ++i) {
    left[i] = (float)(i * 2);
    right[i] = (float)(100 - i);
    float_out[i] = -1.0f;
    input[i] = (int32_t)i - 19;
    int_out[i] = -9999;
    in_place[i] = (int32_t)(3 * i);
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

  return 0;
}

int main(void) {
  static const uint64_t lengths[] = {
      0, 1, 2, 3, 4, 7, 8, 9, 15, 16, 17, 31, 32, 33, 63, 64, 65,
  };
  for (size_t i = 0; i < sizeof(lengths) / sizeof(lengths[0]); ++i) {
    int result = test_length(lengths[i]);
    if (result != 0) return result;
  }
  return 0;
}

