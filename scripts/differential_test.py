#!/usr/bin/env python3
"""Differentially test scalar and custom-vectorized LLVM IR.

The default is a quick development check. ``--full`` reproduces the bounded
397,824-execution stress campaign used for the repository's validation.
Generated sources, IR, diagnostics, and executables stay in
``build/differential``.
"""

from __future__ import annotations

import argparse
import os
import platform
import re
import shlex
import shutil
import subprocess
import sys
from pathlib import Path
from typing import Sequence


REPO = Path(__file__).resolve().parent.parent
ROOT = REPO / "build" / "differential"
EXPECTED_REJECTIONS = {
    "stride_two": "induction-step-must-be-one",
    "i32_induction": "induction-type-must-be-i64",
    "shifted_dependence": "loop-carried-memory-dependence",
    "may_alias": "possible-pointer-alias",
    "i1_memory": "unsupported-memory-element-type",
    "scaled_index": "non-affine-index",
    "has_call": "unsupported-instruction",
    "signed_latch": "unsupported-latch-predicate",
}
REMARK_RE = re.compile(
    r"^rv-vectorize: function=(?P<function>\S+) loop=\S+ "
    r"decision=(?P<decision>\S+) reason=(?P<reason>\S+) "
    r"vf=(?P<vf>\d+) "
)
NATURAL_VFS = {
    "vec_i8_wrapping": 16,
    "vec_i16_wrapping": 8,
    "vec_i32_mix_eq": 4,
    "vec_affine_offsets": 4,
    "vec_i64_signed_divrem": 2,
    "vec_commuted_affine": 2,
}


class DifferentialError(RuntimeError):
    """A tool, verifier, legality, or native comparison failed."""


CASES = [
    (
        "i32_mix_eq",
        "eq",
        "int",
        """  %ap = getelementptr inbounds i32, ptr %a, i64 %i
  %av = load i32, ptr %ap, align 4
  %bp = getelementptr inbounds i32, ptr %b, i64 %i
  %bv = load i32, ptr %bp, align 4
  %sum = add i32 %av, %bv
  %mixed = xor i32 %sum, 1515870810
  %result = mul i32 %mixed, 3
  %op = getelementptr inbounds i32, ptr %out, i64 %i
  store i32 %result, ptr %op, align 4""",
    ),
    (
        "i32_logic_ne",
        "ne",
        "int",
        """  %ap = getelementptr inbounds i32, ptr %a, i64 %i
  %av = load i32, ptr %ap, align 4
  %bp = getelementptr inbounds i32, ptr %b, i64 %i
  %bv = load i32, ptr %bp, align 4
  %d = sub i32 %av, %bv
  %x = and i32 %d, 2147483647
  %result = or i32 %x, 65536
  %op = getelementptr inbounds i32, ptr %out, i64 %i
  store i32 %result, ptr %op, align 4""",
    ),
    (
        "i32_shift_ult",
        "ult",
        "int",
        """  %ap = getelementptr inbounds i32, ptr %a, i64 %i
  %av = load i32, ptr %ap, align 4
  %bp = getelementptr inbounds i32, ptr %b, i64 %i
  %bv = load i32, ptr %bp, align 4
  %lo = lshr i32 %av, 3
  %hi = shl i32 %bv, 1
  %result = or i32 %lo, %hi
  %op = getelementptr inbounds i32, ptr %out, i64 %i
  store i32 %result, ptr %op, align 4""",
    ),
    (
        "i32_ashr",
        "eq",
        "int",
        """  %ap = getelementptr inbounds i32, ptr %a, i64 %i
  %av = load i32, ptr %ap, align 4
  %result = ashr i32 %av, 5
  %op = getelementptr inbounds i32, ptr %out, i64 %i
  store i32 %result, ptr %op, align 4""",
    ),
    (
        "i64_signed_divrem",
        "ne",
        "int",
        """  %ap = getelementptr inbounds i64, ptr %a, i64 %i
  %av = load i64, ptr %ap, align 8
  %q = sdiv i64 %av, 7
  %r = srem i64 %av, 11
  %result = add i64 %q, %r
  %op = getelementptr inbounds i64, ptr %out, i64 %i
  store i64 %result, ptr %op, align 8""",
    ),
    (
        "i64_unsigned_divrem",
        "ult",
        "int",
        """  %ap = getelementptr inbounds i64, ptr %a, i64 %i
  %av = load i64, ptr %ap, align 8
  %q = udiv i64 %av, 13
  %r = urem i64 %av, 17
  %result = xor i64 %q, %r
  %op = getelementptr inbounds i64, ptr %out, i64 %i
  store i64 %result, ptr %op, align 8""",
    ),
    (
        "i16_wrapping",
        "eq",
        "int",
        """  %ap = getelementptr inbounds i16, ptr %a, i64 %i
  %av = load i16, ptr %ap, align 2
  %sum = add i16 %av, 12345
  %result = mul i16 %sum, 17
  %op = getelementptr inbounds i16, ptr %out, i64 %i
  store i16 %result, ptr %op, align 2""",
    ),
    (
        "i8_wrapping",
        "ne",
        "int",
        """  %ap = getelementptr inbounds i8, ptr %a, i64 %i
  %av = load i8, ptr %ap, align 1
  %sum = add i8 %av, 91
  %result = xor i8 %sum, -91
  %op = getelementptr inbounds i8, ptr %out, i64 %i
  store i8 %result, ptr %op, align 1""",
    ),
    (
        "f32_arithmetic",
        "ult",
        "f32",
        """  %ap = getelementptr inbounds float, ptr %a, i64 %i
  %av = load float, ptr %ap, align 4
  %bp = getelementptr inbounds float, ptr %b, i64 %i
  %bv = load float, ptr %bp, align 4
  %product = fmul float %av, 2.500000e+00
  %sum = fadd float %product, %bv
  %result = fneg float %sum
  %op = getelementptr inbounds float, ptr %out, i64 %i
  store float %result, ptr %op, align 4""",
    ),
    (
        "f64_arithmetic",
        "eq",
        "f64",
        """  %ap = getelementptr inbounds double, ptr %a, i64 %i
  %av = load double, ptr %ap, align 8
  %bp = getelementptr inbounds double, ptr %b, i64 %i
  %bv = load double, ptr %bp, align 8
  %difference = fsub double %av, %bv
  %result = fdiv double %difference, 3.250000e+00
  %op = getelementptr inbounds double, ptr %out, i64 %i
  store double %result, ptr %op, align 8""",
    ),
    (
        "f32_remainder",
        "ne",
        "f32",
        """  %ap = getelementptr inbounds float, ptr %a, i64 %i
  %av = load float, ptr %ap, align 4
  %result = frem float %av, 7.750000e+00
  %op = getelementptr inbounds float, ptr %out, i64 %i
  store float %result, ptr %op, align 4""",
    ),
    (
        "select_i32",
        "ult",
        "int",
        """  %ap = getelementptr inbounds i32, ptr %a, i64 %i
  %av = load i32, ptr %ap, align 4
  %bp = getelementptr inbounds i32, ptr %b, i64 %i
  %bv = load i32, ptr %bp, align 4
  %choose = icmp slt i32 %av, %bv
  %result = select i1 %choose, i32 %av, i32 %bv
  %op = getelementptr inbounds i32, ptr %out, i64 %i
  store i32 %result, ptr %op, align 4""",
    ),
    (
        "select_f32",
        "eq",
        "f32",
        """  %ap = getelementptr inbounds float, ptr %a, i64 %i
  %av = load float, ptr %ap, align 4
  %bp = getelementptr inbounds float, ptr %b, i64 %i
  %bv = load float, ptr %bp, align 4
  %choose = fcmp olt float %av, %bv
  %result = select i1 %choose, float %av, float %bv
  %op = getelementptr inbounds float, ptr %out, i64 %i
  store float %result, ptr %op, align 4""",
    ),
    (
        "zext_i8_i32",
        "ne",
        "int",
        """  %ap = getelementptr inbounds i8, ptr %a, i64 %i
  %av = load i8, ptr %ap, align 1
  %wide = zext i8 %av to i32
  %result = add i32 %wide, 1000
  %op = getelementptr inbounds i32, ptr %out, i64 %i
  store i32 %result, ptr %op, align 4""",
    ),
    (
        "sext_i16_i64",
        "ult",
        "int",
        """  %ap = getelementptr inbounds i16, ptr %a, i64 %i
  %av = load i16, ptr %ap, align 2
  %wide = sext i16 %av to i64
  %result = sub i64 %wide, 777
  %op = getelementptr inbounds i64, ptr %out, i64 %i
  store i64 %result, ptr %op, align 8""",
    ),
    (
        "trunc_i64_i16",
        "eq",
        "int",
        """  %ap = getelementptr inbounds i64, ptr %a, i64 %i
  %av = load i64, ptr %ap, align 8
  %narrow = trunc i64 %av to i16
  %result = xor i16 %narrow, 21930
  %op = getelementptr inbounds i16, ptr %out, i64 %i
  store i16 %result, ptr %op, align 2""",
    ),
    (
        "sitofp_i32_f32",
        "ne",
        "small_i32",
        """  %ap = getelementptr inbounds i32, ptr %a, i64 %i
  %av = load i32, ptr %ap, align 4
  %result = sitofp i32 %av to float
  %op = getelementptr inbounds float, ptr %out, i64 %i
  store float %result, ptr %op, align 4""",
    ),
    (
        "uitofp_i32_f64",
        "ult",
        "small_u32",
        """  %ap = getelementptr inbounds i32, ptr %a, i64 %i
  %av = load i32, ptr %ap, align 4
  %result = uitofp i32 %av to double
  %op = getelementptr inbounds double, ptr %out, i64 %i
  store double %result, ptr %op, align 8""",
    ),
    (
        "fptosi_f32_i32",
        "eq",
        "f32",
        """  %ap = getelementptr inbounds float, ptr %a, i64 %i
  %av = load float, ptr %ap, align 4
  %result = fptosi float %av to i32
  %op = getelementptr inbounds i32, ptr %out, i64 %i
  store i32 %result, ptr %op, align 4""",
    ),
    (
        "fptoui_f64_i64",
        "ne",
        "f64_positive",
        """  %ap = getelementptr inbounds double, ptr %a, i64 %i
  %av = load double, ptr %ap, align 8
  %result = fptoui double %av to i64
  %op = getelementptr inbounds i64, ptr %out, i64 %i
  store i64 %result, ptr %op, align 8""",
    ),
    (
        "fpext_f32_f64",
        "ult",
        "f32",
        """  %ap = getelementptr inbounds float, ptr %a, i64 %i
  %av = load float, ptr %ap, align 4
  %result = fpext float %av to double
  %op = getelementptr inbounds double, ptr %out, i64 %i
  store double %result, ptr %op, align 8""",
    ),
    (
        "fptrunc_f64_f32",
        "eq",
        "f64",
        """  %ap = getelementptr inbounds double, ptr %a, i64 %i
  %av = load double, ptr %ap, align 8
  %result = fptrunc double %av to float
  %op = getelementptr inbounds float, ptr %out, i64 %i
  store float %result, ptr %op, align 4""",
    ),
    (
        "affine_offsets",
        "ne",
        "int",
        """  %am3 = sub i64 %i, 3
  %ap = getelementptr inbounds i32, ptr %a, i64 %am3
  %av = load i32, ptr %ap, align 4
  %ip2 = add i64 %i, 2
  %bp = getelementptr inbounds i32, ptr %b, i64 %ip2
  %bv = load i32, ptr %bp, align 4
  %sum = add i32 %av, %bv
  %ip1 = add i64 %i, 1
  %op = getelementptr inbounds i32, ptr %out, i64 %ip1
  store i32 %sum, ptr %op, align 4""",
    ),
    (
        "commuted_affine",
        "ult",
        "int",
        """  %idx = add i64 4, %i
  %ap = getelementptr inbounds i64, ptr %a, i64 %idx
  %av = load i64, ptr %ap, align 8
  %op = getelementptr inbounds i64, ptr %out, i64 %i
  store i64 %av, ptr %op, align 8""",
    ),
    (
        "ordered_two_stores",
        "eq",
        "int",
        """  %op = getelementptr inbounds i32, ptr %out, i64 %i
  store i32 111, ptr %op, align 4
  store i32 222, ptr %op, align 4""",
    ),
    (
        "ordered_store_load_store",
        "ne",
        "int",
        """  %op = getelementptr inbounds i32, ptr %out, i64 %i
  store i32 1001, ptr %op, align 4
  %first = load i32, ptr %op, align 4
  %ap = getelementptr inbounds i32, ptr %a, i64 %i
  %av = load i32, ptr %ap, align 4
  %result = add i32 %first, %av
  store i32 %result, ptr %op, align 4""",
    ),
    (
        "induction_as_data",
        "ult",
        "int",
        """  %scaled = mul i64 %i, 3
  %result = add i64 %scaled, 17
  %op = getelementptr inbounds i64, ptr %out, i64 %i
  store i64 %result, ptr %op, align 8""",
    ),
    (
        "multi_array_stores",
        "eq",
        "int",
        """  %ap = getelementptr inbounds i32, ptr %a, i64 %i
  %av = load i32, ptr %ap, align 4
  %r1 = add i32 %av, 1
  %r2 = sub i32 %av, 1
  %op = getelementptr inbounds i32, ptr %out, i64 %i
  %bp = getelementptr inbounds i32, ptr %b, i64 %i
  store i32 %r1, ptr %op, align 4
  store i32 %r2, ptr %bp, align 4""",
    ),
    (
        "select_induction",
        "ne",
        "int",
        """  %odd = and i64 %i, 1
  %test = icmp eq i64 %odd, 0
  %choice = select i1 %test, i64 %i, i64 999
  %op = getelementptr inbounds i64, ptr %out, i64 %i
  store i64 %choice, ptr %op, align 8""",
    ),
    (
        "reversed_eq_latch",
        "eq_rev",
        "int",
        """  %ap = getelementptr inbounds i32, ptr %a, i64 %i
  %av = load i32, ptr %ap, align 4
  %result = add i32 %av, 19
  %op = getelementptr inbounds i32, ptr %out, i64 %i
  store i32 %result, ptr %op, align 4""",
    ),
    (
        "reversed_ne_latch",
        "ne_rev",
        "int",
        """  %ap = getelementptr inbounds i16, ptr %a, i64 %i
  %av = load i16, ptr %ap, align 2
  %result = sub i16 %av, 23
  %op = getelementptr inbounds i16, ptr %out, i64 %i
  store i16 %result, ptr %op, align 2""",
    ),
    (
        "invariant_trip_operand",
        "ult",
        "int",
        """  %ap = getelementptr inbounds i64, ptr %a, i64 %i
  %av = load i64, ptr %ap, align 8
  %result = add i64 %av, %n
  %op = getelementptr inbounds i64, ptr %out, i64 %i
  store i64 %result, ptr %op, align 8""",
    ),
]


def latch(kind: str) -> str:
    if kind == "eq":
        return "  %done = icmp eq i64 %next, %n\n  br i1 %done, label %exit, label %loop"
    if kind == "ne":
        return "  %continue = icmp ne i64 %next, %n\n  br i1 %continue, label %loop, label %exit"
    if kind == "ult":
        return "  %continue = icmp ult i64 %next, %n\n  br i1 %continue, label %loop, label %exit"
    if kind == "eq_rev":
        return "  %done = icmp eq i64 %n, %next\n  br i1 %done, label %exit, label %loop"
    if kind == "ne_rev":
        return "  %continue = icmp ne i64 %n, %next\n  br i1 %continue, label %loop, label %exit"
    raise AssertionError(kind)


def function(prefix: str, name: str, latch_kind: str, body: str, reference: bool) -> str:
    attribute = " #0" if reference else ""
    return f"""define void @{prefix}_{name}(ptr noalias %out, ptr noalias %a, ptr noalias %b, i64 %n){attribute} {{
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
{body}
  %next = add nuw i64 %i, 1
{latch(latch_kind)}

exit:
  ret void
}}
"""


def write_ir(target_preamble: str) -> None:
    pieces = [f"; generated deterministic differential corpus\n{target_preamble}\n"]
    for name, latch_kind, _, body in CASES:
        pieces.append(function("ref", name, latch_kind, body, True))
        pieces.append(function("vec", name, latch_kind, body, False))
    pieces.append("attributes #0 = { noinline optnone }\n")
    (ROOT / "corpus.ll").write_text("\n".join(pieces))


def write_harness(random_seeds: int, random_trips: int) -> None:
    declarations = []
    rows = []
    kind_map = {
        "int": "KIND_INT",
        "f32": "KIND_F32",
        "f64": "KIND_F64",
        "small_i32": "KIND_SMALL_I32",
        "small_u32": "KIND_SMALL_U32",
        "f64_positive": "KIND_F64_POSITIVE",
    }
    for name, _, kind, _ in CASES:
        declarations.append(f"extern void ref_{name}(void *, void *, void *, uint64_t);")
        declarations.append(f"extern void vec_{name}(void *, void *, void *, uint64_t);")
        rows.append(
            f'    {{"{name}", ref_{name}, vec_{name}, {kind_map[kind]}}},'
        )

    source = f"""#include <inttypes.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define BUFFER_BYTES 4096
#define BASE_OFFSET 512
#define RANDOM_SEEDS {random_seeds}

typedef void (*kernel_fn)(void *, void *, void *, uint64_t);
enum input_kind {{ KIND_INT, KIND_F32, KIND_F64, KIND_SMALL_I32, KIND_SMALL_U32, KIND_F64_POSITIVE }};
struct test_case {{ const char *name; kernel_fn ref; kernel_fn vec; enum input_kind kind; }};

{chr(10).join(declarations)}

static const struct test_case cases[] = {{
{chr(10).join(rows)}
}};

static uint64_t state;
static uint64_t next_random(void) {{
    state ^= state << 13;
    state ^= state >> 7;
    state ^= state << 17;
    return state;
}}

static void initialize(unsigned char *out, unsigned char *a, unsigned char *b,
                       enum input_kind kind, uint64_t seed) {{
    state = seed | UINT64_C(1);
    for (size_t i = 0; i < BUFFER_BYTES; ++i) {{
        out[i] = (unsigned char)next_random();
        a[i] = (unsigned char)next_random();
        b[i] = (unsigned char)next_random();
    }}
    if (kind == KIND_F32) {{
        float *af = (float *)(a + BASE_OFFSET);
        float *bf = (float *)(b + BASE_OFFSET);
        for (int i = -16; i < 256; ++i) {{
            af[i] = (float)((int32_t)(next_random() % 200001) - 100000) / 257.0f;
            bf[i] = (float)((int32_t)(next_random() % 200001) - 100000) / 509.0f;
        }}
    }} else if (kind == KIND_F64 || kind == KIND_F64_POSITIVE) {{
        double *ad = (double *)(a + BASE_OFFSET);
        double *bd = (double *)(b + BASE_OFFSET);
        for (int i = -16; i < 256; ++i) {{
            double av = (double)(next_random() % 1000001) / 37.0;
            double bv = (double)(next_random() % 1000001) / 71.0;
            ad[i] = kind == KIND_F64_POSITIVE ? av : av - 13513.0;
            bd[i] = kind == KIND_F64_POSITIVE ? bv : bv - 7042.0;
        }}
    }} else if (kind == KIND_SMALL_I32 || kind == KIND_SMALL_U32) {{
        uint32_t *au = (uint32_t *)(a + BASE_OFFSET);
        for (int i = -16; i < 256; ++i) {{
            uint32_t value = (uint32_t)(next_random() % 1000001);
            au[i] = kind == KIND_SMALL_I32 ? value - 500000U : value;
        }}
    }}
}}

static int compare_buffer(const char *which, const struct test_case *test,
                          uint64_t seed, uint64_t n,
                          const unsigned char *expected, const unsigned char *actual) {{
    if (memcmp(expected, actual, BUFFER_BYTES) == 0) return 0;
    for (size_t i = 0; i < BUFFER_BYTES; ++i) {{
        if (expected[i] != actual[i]) {{
            fprintf(stderr,
                    "mismatch case=%s buffer=%s seed=%" PRIu64 " n=%" PRIu64
                    " byte=%zu expected=%u actual=%u\\n",
                    test->name, which, seed, n, i,
                    (unsigned)expected[i], (unsigned)actual[i]);
            return 1;
        }}
    }}
    return 1;
}}

int main(void) {{
    static const uint64_t fixed_trips[] = {{
        0, 1, 2, 3, 4, 5, 7, 8, 9, 15, 16, 17, 23, 31, 32, 33,
        47, 63, 64, 65, 79, 95, 96, 97, 111, 127, 128, 129, 137
    }};
    unsigned char *ref_out = aligned_alloc(64, BUFFER_BYTES);
    unsigned char *ref_a = aligned_alloc(64, BUFFER_BYTES);
    unsigned char *ref_b = aligned_alloc(64, BUFFER_BYTES);
    unsigned char *vec_out = aligned_alloc(64, BUFFER_BYTES);
    unsigned char *vec_a = aligned_alloc(64, BUFFER_BYTES);
    unsigned char *vec_b = aligned_alloc(64, BUFFER_BYTES);
    if (!ref_out || !ref_a || !ref_b || !vec_out || !vec_a || !vec_b) return 2;

    uint64_t executions = 0;
    for (size_t c = 0; c < sizeof(cases) / sizeof(cases[0]); ++c) {{
        const struct test_case *test = &cases[c];
        for (uint64_t seed_index = 0; seed_index < RANDOM_SEEDS; ++seed_index) {{
            uint64_t seed = UINT64_C(0x9e3779b97f4a7c15) * (seed_index + 1)
                          ^ UINT64_C(0xd1b54a32d192ed03) * (c + 3);
            uint64_t trips[{29 + random_trips}];
            size_t trip_count = sizeof(fixed_trips) / sizeof(fixed_trips[0]);
            memcpy(trips, fixed_trips, sizeof(fixed_trips));
            state = seed;
            for (size_t k = 0; k < {random_trips}; ++k) trips[trip_count++] = next_random() % 138;

            for (size_t t = 0; t < trip_count; ++t) {{
                uint64_t n = trips[t];
                initialize(ref_out, ref_a, ref_b, test->kind, seed ^ (n << 32) ^ t);
                memcpy(vec_out, ref_out, BUFFER_BYTES);
                memcpy(vec_a, ref_a, BUFFER_BYTES);
                memcpy(vec_b, ref_b, BUFFER_BYTES);
                test->ref(ref_out + BASE_OFFSET, ref_a + BASE_OFFSET,
                          ref_b + BASE_OFFSET, n);
                test->vec(vec_out + BASE_OFFSET, vec_a + BASE_OFFSET,
                          vec_b + BASE_OFFSET, n);
                if (compare_buffer("out", test, seed, n, ref_out, vec_out)
                    || compare_buffer("a", test, seed, n, ref_a, vec_a)
                    || compare_buffer("b", test, seed, n, ref_b, vec_b)) return 1;
                ++executions;
            }}
        }}
    }}
    printf("differential-ok cases=%zu seeds=%d trips-per-seed={29 + random_trips} executions=%" PRIu64 "\\n",
           sizeof(cases) / sizeof(cases[0]), RANDOM_SEEDS, executions);
    free(ref_out); free(ref_a); free(ref_b);
    free(vec_out); free(vec_a); free(vec_b);
    return 0;
}}
"""
    (ROOT / "harness.c").write_text(source)


def write_rejections(target_preamble: str) -> None:
    source = r"""; generated unsupported-shape probes

define void @stride_two(ptr noalias %out, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %p = getelementptr i32, ptr %out, i64 %i
  store i32 1, ptr %p
  %next = add i64 %i, 2
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop
exit:
  ret void
}

define void @i32_induction(ptr noalias %out, i32 %n) {
entry:
  %empty = icmp eq i32 %n, 0
  br i1 %empty, label %exit, label %loop
loop:
  %i = phi i32 [ 0, %entry ], [ %next, %loop ]
  %p = getelementptr i32, ptr %out, i32 %i
  store i32 2, ptr %p
  %next = add i32 %i, 1
  %done = icmp eq i32 %next, %n
  br i1 %done, label %exit, label %loop
exit:
  ret void
}

define void @shifted_dependence(ptr noalias %data, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %src = getelementptr i32, ptr %data, i64 %i
  %v = load i32, ptr %src
  %j = add i64 %i, 1
  %dst = getelementptr i32, ptr %data, i64 %j
  store i32 %v, ptr %dst
  %next = add i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop
exit:
  ret void
}

define void @may_alias(ptr %out, ptr %in, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %src = getelementptr i32, ptr %in, i64 %i
  %v = load i32, ptr %src
  %dst = getelementptr i32, ptr %out, i64 %i
  store i32 %v, ptr %dst
  %next = add i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop
exit:
  ret void
}

define void @i1_memory(ptr noalias %out, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %p = getelementptr i1, ptr %out, i64 %i
  store i1 true, ptr %p
  %next = add i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop
exit:
  ret void
}

define void @scaled_index(ptr noalias %out, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %j = mul i64 %i, 2
  %p = getelementptr i32, ptr %out, i64 %j
  store i32 3, ptr %p
  %next = add i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop
exit:
  ret void
}

declare i32 @opaque(i32)
define void @has_call(ptr noalias %out, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %v = call i32 @opaque(i32 1)
  %p = getelementptr i32, ptr %out, i64 %i
  store i32 %v, ptr %p
  %next = add i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %loop
exit:
  ret void
}

define void @signed_latch(ptr noalias %out, i64 %n) {
entry:
  %empty = icmp eq i64 %n, 0
  br i1 %empty, label %exit, label %loop
loop:
  %i = phi i64 [ 0, %entry ], [ %next, %loop ]
  %p = getelementptr i32, ptr %out, i64 %i
  store i32 4, ptr %p
  %next = add i64 %i, 1
  %continue = icmp slt i64 %next, %n
  br i1 %continue, label %loop, label %exit
exit:
  ret void
}

define void @two_block_loop(ptr noalias %out, i64 %n) {
entry:
  br label %head
head:
  %i = phi i64 [ 0, %entry ], [ %next, %latch ]
  %p = getelementptr i32, ptr %out, i64 %i
  store i32 5, ptr %p
  br label %latch
latch:
  %next = add i64 %i, 1
  %done = icmp eq i64 %next, %n
  br i1 %done, label %exit, label %head
exit:
  ret void
}
"""
    (ROOT / "unsupported.ll").write_text(f"{target_preamble}\n\n{source}")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--full",
        action="store_true",
        help="run the 397,824-execution VF/optimization/sanitizer matrix",
    )
    parser.add_argument(
        "--llvm-prefix",
        type=Path,
        help="LLVM 21 prefix; otherwise inspect LLVM_SYS_211_PREFIX and standard paths",
    )
    parser.add_argument(
        "--skip-build",
        action="store_true",
        help="reuse the existing release plugin",
    )
    return parser.parse_args()


def discover_llvm(explicit: Path | None) -> tuple[Path, str]:
    candidates: list[Path] = []
    if explicit is not None:
        candidates.append(explicit.expanduser())
    configured = os.environ.get("LLVM_SYS_211_PREFIX")
    if configured:
        candidates.append(Path(configured).expanduser())
    candidates.extend(
        Path(path)
        for path in (
            "/opt/homebrew/opt/llvm",
            "/usr/local/opt/llvm",
            "/usr/lib/llvm-21",
        )
    )
    checked: list[str] = []
    for prefix in candidates:
        llvm_config = prefix / "bin" / "llvm-config"
        checked.append(str(prefix))
        if not llvm_config.is_file():
            continue
        result = subprocess.run(
            [str(llvm_config), "--version"],
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        version = result.stdout.strip()
        if result.returncode == 0 and version.split(".", 1)[0] == "21":
            return prefix.resolve(), version
    raise DifferentialError(
        "LLVM 21 not found; set LLVM_SYS_211_PREFIX or pass --llvm-prefix "
        f"(checked: {', '.join(checked)})"
    )


def sdk_flags() -> list[str]:
    if platform.system() != "Darwin":
        return []
    try:
        result = subprocess.run(
            ["xcrun", "--show-sdk-path"],
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
    except FileNotFoundError as error:
        raise DifferentialError("xcrun is required to locate the macOS SDK") from error
    sdk = Path(result.stdout.strip())
    if result.returncode != 0 or not sdk.is_dir():
        raise DifferentialError("xcrun did not return a usable macOS SDK")
    return ["-isysroot", str(sdk.resolve())]


def plugin_path() -> Path:
    suffix = ".dylib" if platform.system() == "Darwin" else ".so"
    return REPO / "target" / "release" / f"librust_loop_vectorizer{suffix}"


def command_text(command: Sequence[object]) -> str:
    return shlex.join(str(part) for part in command)


def run_checked(
    command: Sequence[object], env: dict[str, str]
) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(
        [str(part) for part in command],
        cwd=REPO,
        env=env,
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    if result.returncode != 0:
        raise DifferentialError(
            f"command failed ({result.returncode}): {command_text(command)}\n"
            f"stdout:\n{result.stdout[-6000:]}\n"
            f"stderr:\n{result.stderr[-6000:]}"
        )
    return result


def target_preamble(clang: Path, env: dict[str, str]) -> str:
    empty_source = ROOT / "target-probe.c"
    empty_source.write_text("/* target layout probe */\n")
    result = run_checked(
        [clang, *sdk_flags(), "-S", "-emit-llvm", empty_source, "-o", "-"], env
    )
    lines = [
        line
        for line in result.stdout.splitlines()
        if line.startswith("target datalayout =") or line.startswith("target triple =")
    ]
    if len(lines) != 2:
        raise DifferentialError("clang did not emit a target data layout and triple")
    return "\n".join(lines)


def parse_remarks(text: str) -> dict[str, tuple[str, str, int]]:
    records: dict[str, tuple[str, str, int]] = {}
    for line in text.splitlines():
        match = REMARK_RE.match(line)
        if match is None:
            continue
        function = match.group("function")
        if function in records:
            raise DifferentialError(f"duplicate vectorizer remark for {function}")
        records[function] = (
            match.group("decision"),
            match.group("reason"),
            int(match.group("vf")),
        )
    return records


def vectorize(
    opt: Path,
    plugin: Path,
    source: Path,
    output: Path,
    pass_name: str,
    env: dict[str, str],
) -> dict[str, tuple[str, str, int]]:
    result = run_checked(
        [
            opt,
            f"-load-pass-plugin={plugin}",
            f"-passes={pass_name},verify",
            "-S",
            source,
            "-o",
            output,
        ],
        env,
    )
    output.with_suffix(".remarks").write_text(result.stderr)
    return parse_remarks(result.stderr)


def require_supported(
    records: dict[str, tuple[str, str, int]], output: Path, label: str
) -> None:
    expected = {f"vec_{name}" for name, _, _, _ in CASES}
    if set(records) != expected:
        missing = sorted(expected - set(records))
        unexpected = sorted(set(records) - expected)
        raise DifferentialError(
            f"{label}: incomplete supported-corpus remarks; "
            f"missing={missing}, unexpected={unexpected}"
        )
    wrong = {
        function: result
        for function, result in records.items()
        if result[:2] != ("vectorized", "legal-and-profitable")
    }
    if wrong:
        raise DifferentialError(f"{label}: supported loops were rejected: {wrong}")
    vector_blocks = output.read_text().count("rv.vector.body:")
    if vector_blocks != len(CASES):
        raise DifferentialError(
            f"{label}: expected {len(CASES)} vector bodies, found {vector_blocks}"
        )
    if label in {"balanced", "conservative", "aggressive"}:
        observed_vfs = {name: records[name][2] for name in NATURAL_VFS}
        if observed_vfs != NATURAL_VFS:
            raise DifferentialError(
                f"{label}: natural VF regression: "
                f"expected={NATURAL_VFS}, actual={observed_vfs}"
            )
    elif label.startswith("vf"):
        expected_vf = int(label[2:])
        wrong_vfs = {
            name: result[2] for name, result in records.items() if result[2] != expected_vf
        }
        if wrong_vfs:
            raise DifferentialError(f"{label}: forced VF was not honored: {wrong_vfs}")


def require_unsupported(
    records: dict[str, tuple[str, str, int]], output: Path, label: str
) -> None:
    expected = {
        function: ("rejected", reason, 0)
        for function, reason in EXPECTED_REJECTIONS.items()
    }
    if records != expected:
        raise DifferentialError(
            f"{label}: unsupported-corpus decisions differ: "
            f"expected={expected}, actual={records}"
        )
    if "rv.vector.body:" in output.read_text():
        raise DifferentialError(f"{label}: an unsupported loop was vectorized")


RESULT_RE = re.compile(
    r"^differential-ok cases=(?P<cases>\d+) seeds=(?P<seeds>\d+) "
    r"trips-per-seed=(?P<trips>\d+) executions=(?P<executions>\d+)$"
)


def compile_and_run(
    clang: Path,
    ir: Path,
    executable: Path,
    optimization: str,
    seeds: int,
    trips: int,
    env: dict[str, str],
    *,
    ubsan: bool = False,
) -> int:
    command: list[object] = [
        clang,
        *sdk_flags(),
        optimization,
        "-std=c11",
        "-fno-vectorize",
        "-fno-slp-vectorize",
    ]
    if ubsan:
        command.extend(["-fsanitize=undefined", "-fno-omit-frame-pointer"])
    command.extend([ir, ROOT / "harness.c", "-o", executable])
    run_checked(command, env)
    runtime_env = env.copy()
    if ubsan:
        runtime_env["UBSAN_OPTIONS"] = "halt_on_error=1:print_stacktrace=1"
    result = run_checked([executable], runtime_env)
    match = RESULT_RE.match(result.stdout.strip())
    if match is None:
        raise DifferentialError(
            f"malformed differential harness result: {result.stdout!r}"
        )
    observed = {key: int(value) for key, value in match.groupdict().items()}
    expected = {
        "cases": len(CASES),
        "seeds": seeds,
        "trips": trips,
        "executions": len(CASES) * seeds * trips,
    }
    if observed != expected:
        raise DifferentialError(
            f"incorrect differential execution count: expected={expected}, actual={observed}"
        )
    return observed["executions"]


def main() -> int:
    args = parse_args()
    llvm_prefix, llvm_version = discover_llvm(args.llvm_prefix)
    opt = llvm_prefix / "bin" / "opt"
    clang = llvm_prefix / "bin" / "clang"
    if not opt.is_file() or not clang.is_file():
        raise DifferentialError(f"LLVM tools are incomplete under {llvm_prefix}")

    env = os.environ.copy()
    env["LLVM_SYS_211_PREFIX"] = str(llvm_prefix)
    if not args.skip_build:
        run_checked(["cargo", "build", "--release", "--quiet"], env)
    plugin = plugin_path()
    if not plugin.is_file():
        raise DifferentialError(
            f"release plugin not found at {plugin}; remove --skip-build to build it"
        )

    if ROOT.exists():
        shutil.rmtree(ROOT)
    ROOT.mkdir(parents=True)
    random_seeds = 48 if args.full else 4
    random_trips = 8 if args.full else 3
    trips_per_seed = 29 + random_trips
    preamble = target_preamble(clang, env)
    write_ir(preamble)
    write_harness(random_seeds, random_trips)
    write_rejections(preamble)

    corpus = ROOT / "corpus.ll"
    unsupported = ROOT / "unsupported.ll"
    run_checked([opt, "-passes=verify", "-disable-output", corpus], env)
    run_checked([opt, "-passes=verify", "-disable-output", unsupported], env)

    pass_configs = (
        ("balanced", "rust-loop-vectorize-report"),
        ("conservative", "rust-loop-vectorize-conservative-report"),
        ("aggressive", "rust-loop-vectorize-aggressive-report"),
        ("vf2", "rust-loop-vectorize-force-vf2-report"),
        ("vf4", "rust-loop-vectorize-force-vf4-report"),
        ("vf8", "rust-loop-vectorize-force-vf8-report"),
        ("vf16", "rust-loop-vectorize-force-vf16-report"),
    )
    supported_ir: dict[str, Path] = {}
    for label, pass_name in pass_configs:
        output = ROOT / f"supported-{label}.ll"
        records = vectorize(opt, plugin, corpus, output, pass_name, env)
        require_supported(records, output, label)
        supported_ir[label] = output

        rejected_output = ROOT / f"unsupported-{label}.ll"
        rejected_records = vectorize(
            opt, plugin, unsupported, rejected_output, pass_name, env
        )
        require_unsupported(rejected_records, rejected_output, label)

    runtime_labels = ("balanced", "vf2", "vf4", "vf8", "vf16")
    if not args.full:
        runtime_labels = ("balanced", "vf16")
    executions = 0
    for label in runtime_labels:
        executions += compile_and_run(
            clang,
            supported_ir[label],
            ROOT / f"runtime-{label}",
            "-O2",
            random_seeds,
            trips_per_seed,
            env,
        )

    if args.full:
        executions += compile_and_run(
            clang,
            supported_ir["balanced"],
            ROOT / "runtime-balanced-O0",
            "-O0",
            random_seeds,
            trips_per_seed,
            env,
        )
        executions += compile_and_run(
            clang,
            supported_ir["balanced"],
            ROOT / "runtime-balanced-ubsan",
            "-O1",
            random_seeds,
            trips_per_seed,
            env,
            ubsan=True,
        )
        if executions != 397_824:
            raise DifferentialError(
                f"full campaign should execute 397824 comparisons, got {executions}"
            )

    mode = "full" if args.full else "quick"
    print(
        f"differential-test: ok mode={mode} llvm={llvm_version} "
        f"cases={len(CASES)} pass-configs={len(pass_configs)} "
        f"runtime-configs={len(runtime_labels) + (2 if args.full else 0)} "
        f"executions={executions} unsupported-probes=9"
    )
    print(f"artifacts: {ROOT}")
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (DifferentialError, OSError) as error:
        print(f"differential-test: error: {error}", file=sys.stderr)
        sys.exit(1)
