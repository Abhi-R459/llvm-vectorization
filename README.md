# Rust Loop Vectorizer for LLVM 21

A correctness-first automatic loop-vectorization pass whose discovery,
dependence analysis, profitability model, and LLVM IR generation are written in
Rust. A small C++ adapter only registers the pass with LLVM's New Pass Manager
and bridges APIs that LLVM does not expose through C.

The project implements the requested end-to-end path:

- detects canonical count-up loops;
- proves a deliberately bounded legality contract;
- performs affine same-base dependence and conservative alias analysis;
- selects a fixed vector factor with conservative, balanced, or aggressive
  profitability policies;
- emits real fixed-width LLVM vector loads, stores, arithmetic, casts,
  comparisons, and selects;
- retains the scalar loop for short inputs and exact scalar remainders;
- reports vector coverage, issued-lane utilization, rejection reasons, and
  per-loop analysis/transformation latency; and
- compares identical scalar input IR against LLVM's built-in Loop Vectorizer
  with native correctness and timing harnesses.

This is a research and teaching implementation for compiler optimization,
code generation, and parallelism modules. It intentionally rejects IR it
cannot prove safe; it is not presented as a replacement for LLVM's production
vectorizer.

## Measured result

On the checked-in 128-loop compile-time corpus, a release build measured
**2.355 µs p50**, **8.790 µs p95**, and **31.404 µs p99** for the custom pass's
combined in-process analysis and transformation. The run used LLVM 21.1.3 on
an Apple M5 and transformed 128/128 loops.

In the seven-kernel runtime comparison, all scalar/custom/LLVM variants passed
correctness. The recorded geometric-mean speedups over the scalar control were
**3.009× for this pass** and **2.974× for LLVM LoopVectorize**. These are
cache-hot results from one machine, not universal performance claims. See the
[machine-readable snapshot](docs/benchmark-results.json) and
[research report](docs/research-report.md#53-local-controlled-result) for the
protocol, per-kernel results, and caveats.

## Requirements

- Rust 1.85 or newer
- LLVM 21.x, including `llvm-config`, `opt`, `clang`, `llc`, and `llvm-dis`
- a C++17 compiler compatible with the installed LLVM build
- Python 3.10 or newer for the benchmark driver
- macOS or Linux for the native test/benchmark scripts

For Homebrew LLVM:

```sh
export LLVM_SYS_211_PREFIX="$(brew --prefix llvm)"
export PATH="$LLVM_SYS_211_PREFIX/bin:$PATH"
```

On other systems, set `LLVM_SYS_211_PREFIX` to the LLVM 21 installation prefix
and put that installation's `bin` directory on `PATH`. `LLVM_CONFIG_PATH` may
additionally select the exact `llvm-config` used by this project's C++ bridge,
but the `llvm-sys` dependency itself discovers LLVM through
`LLVM_SYS_211_PREFIX` or `PATH`. Build the plugin and load it with tools from
the same LLVM installation; LLVM's C++ pass-plugin ABI is not a cross-install
compatibility boundary.

## Build and run

```sh
cargo build --release
```

On macOS:

```sh
/opt/homebrew/opt/llvm/bin/opt \
  -load-pass-plugin=target/release/librust_loop_vectorizer.dylib \
  -passes='rust-loop-vectorize-report,verify' \
  -S input.ll -o output.ll
```

On Linux, use `librust_loop_vectorizer.so` and the path to the LLVM 21 `opt`.
Report mode emits one parseable line per discovered candidate, for example:

```text
rv-vectorize: function=add_f32 loop=loop decision=vectorized reason=legal-and-profitable vf=4 estimated_vector_coverage=100% issued_lane_utilization=100% analysis_us=2.834 transform_us=10.708
```

To create suitable scalar IR from C before applying the pass:

```sh
clang -O1 -fno-vectorize -fno-slp-vectorize -fno-unroll-loops \
  -ffp-contract=off -emit-llvm -S kernel.c -o kernel.ll
```

## Policies and pass names

| Pass | Behavior |
|---|---|
| `rust-loop-vectorize` | Balanced policy; automatic power-of-two VF within a 128-bit baseline. |
| `rust-loop-vectorize-report` | Balanced policy with diagnostics and internal timings. |
| `rust-loop-vectorize-conservative` | Requires at least `4*VF` estimated iterations and 1.25× estimated gain. |
| `rust-loop-vectorize-aggressive` | Requires at least `VF` iterations and break-even estimated cost. |
| `rust-loop-vectorize-force-vf2` | Forces VF 2 after all legality checks. |
| `rust-loop-vectorize-force-vf4` | Forces VF 4 after all legality checks. |
| `rust-loop-vectorize-force-vf8` | Forces VF 8 after all legality checks. |
| `rust-loop-vectorize-force-vf16` | Forces VF 16 after all legality checks. |

Append `-report` to the conservative, aggressive, or forced-VF names for
diagnostics. Forced widths bypass the profitability ratio, never legality.

## Supported loop subset

Accepted loops have:

- one loop block, one exit, and one ordinary branch preheader edge;
- one zero-based `i64` induction PHI with unit positive step;
- `eq`, `ne`, or canonical unsigned-`lt` latch comparison against an invariant
  trip count;
- no loop value used outside the loop and no exit PHI;
- direct argument/global bases and one-index `i + constant` GEPs;
- nonvolatile, nonatomic contiguous memory operations using `i8/i16/i32/i64`,
  `half`, `float`, or `double` elements;
- distinct globals, pairs of `noalias` arguments, or proven safe same-base
  dependence distances; and
- a target layout with packed scalar/vector storage and exactly 64-bit GEP
  index arithmetic in the relevant address space. A missing module data layout
  is rejected because generic defaults are not a target contract.

The pass supports lane-wise arithmetic, division/remainder, shifts, bitwise
operations, casts, comparisons, `select`, floating negation, loads, and stores.
It preserves applicable fast-math, integer nowrap, exact, GEP inbounds, and
memory-alignment properties.

Not yet supported: runtime alias versioning, reductions, calls/intrinsics,
multi-block control flow and if-conversion, nonunit/descending induction,
masked tails, scalable vectors, interleaving, outer-loop vectorization, or
TargetTransformInfo-based costing. Each becomes a safe future extension only
with its corresponding legality proof and tests.

## Verification

Run the complete suite:

```sh
./scripts/test.sh
```

It currently covers 16 Rust tests, nine positive LLVM loops, conservative
rejection fixtures, policy selection, forced VF, LLVM's verifier, native
differential execution over boundary trip counts, all three supported latch
forms, memory-order cases, and guard-page detection of tail over-read/write.

Run the generated differential corpus in quick mode:

```sh
make differential
# differential-test: ok mode=quick ... cases=32 pass-configs=7 \
#   runtime-configs=2 executions=8192 unsupported-probes=9
```

`make differential-full` expands this to 397,824 byte-for-byte scalar/vector
comparisons across natural and forced VFs, `-O0`/`-O2`, boundary and randomized
trip counts, and a UBSan run. Both modes verify every generated module and
confirm that all nine unsupported-shape probes remain scalar. Reproducible
artifacts are written only to `build/differential/`.

Run the reproducible comparison:

```sh
./scripts/benchmark.py --warmups 3 --samples 21 \
  --inner-calls 64 --timing-runs 15
```

The script compiles the kernel source to scalar bitcode once with both LLVM
vectorizers disabled. Scalar, custom, and LLVM variants then fork from that
byte-identical bitcode and use the same backend/link flags. It verifies every
variant, tests edge lengths, records repeated runtime samples, times 128 cloned
loops, and writes full JSON/CSV artifacts to `build/benchmark/`.

Useful development commands:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
make test
```

## Design and research

- [Architecture and correctness contract](docs/architecture.md)
- [Research report and evidence ledger](docs/research-report.md)
- [Recorded benchmark snapshot](docs/benchmark-results.json)

The research report connects this implementation to foundational dependence
work by Allen and Kennedy, Goff–Kennedy–Tseng, and Maydan–Hennessy–Lam; LLVM's
LoopAccessAnalysis and VPlan architecture; target-aware and throttled
profitability research; TSVC evaluation; and translation-validation lessons.

## Repository map

```text
native/pass_plugin.cpp       LLVM New PM adapter and narrow C++ bridges
src/vectorizer.rs            discovery, legality, planning inputs, IR rewrite
src/dependence.rs            affine GCD/exact-distance classifier
src/cost.rs                  VF selection, cost score, vector coverage
src/config.rs                heuristic profiles
src/llvm.rs                  borrowed LLVM handle wrappers
tests/                       positive, negative, runtime, and layout tests
benchmarks/                  fair scalar/custom/LLVM kernel corpus and harness
scripts/test.sh              end-to-end verifier/runtime suite
scripts/differential_test.py generated scalar/vector stress campaign
scripts/benchmark.py         reproducible comparison and JSON/CSV reporting
docs/                        architecture, research, and measured snapshot
```
