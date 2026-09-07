# A bounded LLVM loop vectorizer: research rationale and design report

- **Audience:** compiler-course readers, implementers, and reviewers
- **Research cut-off:** 8 September 2026
- **Scope:** loop-vectorization legality, profitability, SIMD utilization, and
  validation as they apply to the inspected LLVM 21/Rust implementation.
  Results from prior work are kept in their original experimental scope; local
  results are reported only with their recorded host and protocol.

## Executive answer

Automatic vectorization is not principally an exercise in replacing scalar
opcodes with vector opcodes. It is a proof-and-planning problem: a compiler must
show that executing several loop iterations together preserves the scalar
program's dependences, choose a vectorization strategy that is likely to repay
its setup and remainder costs, and then produce valid IR without weakening the
source semantics.

This project implements that idea as an LLVM 21 New Pass Manager plugin whose
analysis and transformation are written in Rust. Its deliberately small domain
is a strength, not a claim of parity with LLVM's production Loop Vectorizer. It
accepts canonical, single-basic-block, zero-based `i64` loops with a unit
induction step, one-dimensional contiguous memory accesses, a limited set of
side-effect-free scalar operations, and statically proven alias relationships.
It rejects everything it cannot establish safely. For accepted loops it emits a
fixed-width vector loop, retains the original loop as a scalar fallback and
remainder, and records a per-loop decision and timing when report mode is used.

The literature supports four conclusions that shape the implementation:

1. Dependence preservation is the non-negotiable legality condition.
2. Fast special-case tests should prove a narrow result and return “unknown”
   outside that result; passing a necessary test is not proof of independence.
3. Profitability is target- and workload-dependent, so a small static model
   should be described as a heuristic and checked against measured execution.
4. Passing examples and LLVM's IR verifier are necessary but not equivalent to
   translation validation. Stronger validation remains future work.

## 1. The compiler problem

For a scalar loop, iteration `i` completes before iteration `i + 1`. A vector
loop changes that order by evaluating `VF` iterations as lanes of one vector
operation. The rewrite is legal only if the changed schedule respects every
relevant dependence. It is useful only if work saved by SIMD exceeds dispatch,
remainder, code-size, register-pressure, and target-specific instruction costs.

These questions must remain separate:

- **Legality:** can the transformation preserve the observable behavior of the
  input program?
- **Profitability:** among legal alternatives, is any one expected to improve a
  chosen objective such as execution time or code size?
- **Execution:** can the chosen alternative be materialized while maintaining
  valid SSA form, control flow, memory semantics, and instruction flags?

LLVM's VPlan documentation formalizes essentially this split as **Legal → Plan
→ Execute**, with only Execute modifying the input IR
([LLVM, *Vectorization Plan*](https://llvm.org/docs/VectorizationPlan.html)).
The project follows the same separation at a much smaller scale: analysis
returns a validated candidate and one plan; only then does the transformer add
blocks and widen instructions. It does not implement VPlan's recipes,
multi-candidate search, versioning, or target-aware optimization.

## 2. Dependence analysis: what the foundational work establishes

### 2.1 Dependences define the legal schedules

Randy Allen and Ken Kennedy's 1987 account of translating FORTRAN to vector
form states the central criterion directly: a transformed program preserves the
source semantics when it preserves the source program's dependences. Their work
develops data-dependence tests and transformations around that criterion
([Allen and Kennedy, *Automatic Translation of FORTRAN Programs to Vector
Form*](https://doi.org/10.1145/29873.29875), ACM TOPLAS 9(4), 1987; [accessible
paper](https://rsim.cs.illinois.edu/arch/qual_papers/compilers/allen87.pdf)).

For memory, the relevant hazards are usually:

- a write followed by a read of the same location (flow/true dependence),
- a read followed by a write (anti-dependence), and
- two writes to the same location (output dependence).

Read/read pairs do not constrain the ordering. A same-iteration dependence may
be vectorizable when statement order inside each lane is preserved; a
loop-carried dependence can prevent ordinary lane-wise widening because a lane
may require a value produced by another lane.

This project applies that conservative rule to every memory-access pair where
at least one access is a write. It accepts a pair only when the pair is proven
independent or addresses the same element in the same iteration. It rejects a
known nonzero distance and an unresolved result. This policy deliberately
excludes recurrence transformations, reduction recognition, and dependence
cycles that a more capable vectorizer might handle.

### 2.2 The GCD test is an independence filter, not a general proof

For affine subscripts

```text
left(i)  = a*i + b
right(j) = c*j + d
```

an overlap requires an integer solution of `a*i + b = c*j + d`. Therefore
`gcd(|a|, |c|)` must divide `d - b`. If it does not, the accesses are
independent. If it does, the test has only failed to disprove a dependence; it
has not established that an in-bounds solution exists. Loop bounds and the
ordering of iterations still matter.

This distinction is fundamental in classical dependence analysis. Allen and
Kennedy discuss GCD- and Banerjee-style tests as fast filters. Gina Goff, Ken
Kennedy, and Chau-Wen Tseng subsequently organize practical tests by subscript
class and provide stronger treatment of single-index-variable cases
([Goff, Kennedy, and Tseng, *Practical Dependence Testing*](https://doi.org/10.1145/113445.113448),
PLDI 1991). Dror Maydan, John Hennessy, and Monica Lam advocate a portfolio of
efficient algorithms that are exact for common special cases, backed by a more
expensive exact method and memoization
([Maydan, Hennessy, and Lam, *Efficient and Exact Data Dependence
Analysis*](https://doi.org/10.1145/113445.113447), PLDI 1991; [paper
copy](https://citeseerx.ist.psu.edu/document?doi=dc6207bf015aa47aa1890b91d5eb18925fae9e61&repid=rep1&type=pdf)).

The implementation embodies only the small first tier of that progression:

- its reusable Rust classifier contains a GCD rejection test;
- equal nonzero coefficients allow an exact integer distance calculation;
- unequal coefficients that pass the GCD test remain `Potential`; and
- arithmetic overflow also produces `Potential` rather than a wrapped answer.

The current LLVM-IR recognizer is narrower still: it accepts only indices of the
form `i`, `i + constant`, `constant + i`, or `i - constant`, so all production
memory accesses have coefficient one. Consequently, the exact equal-slope
distance case—not general multidimensional affine analysis—is what currently
decides same-base memory legality. The more general GCD branch is tested Rust
infrastructure for a future recognizer, not evidence that the pass already
supports arbitrary affine subscripts.

### 2.3 Alias identity is part of the proof

Subscript analysis is useful only after establishing which accesses may refer
to the same underlying object. For different pointer bases, this project proves
disjointness only for distinct LLVM global variables or when both bases are
function arguments marked `noalias`. An otherwise legal-looking copy between
two ordinary pointer arguments is rejected as `possible-pointer-alias`.

LLVM's production path is materially more capable. `LoopAccessAnalysis` uses
alias analysis and Scalar Evolution to reason about memory accesses, records
dependence distances, and can construct run-time pointer checks when static
proof is insufficient
([LLVM, `LoopAccessAnalysis.h`](https://github.com/llvm/llvm-project/blob/main/llvm/include/llvm/Analysis/LoopAccessAnalysis.h);
[implementation](https://github.com/llvm/llvm-project/blob/main/llvm/lib/Analysis/LoopAccessAnalysis.cpp)).
The Loop Vectorizer can version a loop so non-overlapping ranges take the vector
path and an overlapping case takes the scalar path
([LLVM, *Auto-Vectorization in LLVM*](https://llvm.org/docs/Vectorizers.html#runtime-checks-of-pointers)).

The project's GCD/equal-distance test must therefore not be described as
equivalent to LLVM dependence analysis. It is a fail-closed test for a much
smaller access language, and it intentionally emits no run-time alias checks.

## 3. Planning and profitability

### 3.1 Why “vectorizable” does not mean “worth vectorizing”

Vector execution can lose when the trip count is short, the remainder is large,
vector setup is costly, an operation scalarizes, data movement dominates, or
the selected width increases register pressure. LLVM evaluates alternatives
using target information and costs the candidate vector loop, scalar work,
epilogue, and relevant checks; the planner chooses among vectorization factors
rather than assuming the widest legal vector is best
([LLVM, `LoopVectorize.cpp`](https://github.com/llvm/llvm-project/blob/main/llvm/lib/Transforms/Vectorize/LoopVectorize.cpp);
[`LoopVectorizationPlanner.cpp`](https://github.com/llvm/llvm-project/blob/main/llvm/lib/Transforms/Vectorize/LoopVectorizationPlanner.cpp)).

Nuzman and Henderson's multi-platform work likewise emphasizes separating a
target-independent vector IR from target-aware decisions across distinct SIMD
architectures
([Nuzman and Henderson, *Multi-platform Auto-vectorization*](https://doi.org/10.1109/CGO.2006.25),
CGO 2006). The implication is important here: a fixed 128-bit policy can be a
useful portable teaching baseline, but it is not a substitute for LLVM's
`TargetTransformInfo`.

### 3.2 What this project's model actually computes

The project estimates an unknown dynamic trip count as 64 and uses an exact
constant when the latch compares against one. It assigns:

- cost 2 to each scalar load or store,
- cost 4 to division and remainder,
- cost 1 to other supported compute operations, and
- fixed setup cost 6.

The same per-operation sum is used for one scalar iteration and one vector
iteration; the vector iteration is credited with completing `VF` scalar
iterations. If `T` is the estimated trip count and `C_s`, `C_v`, and `C_setup`
are those costs, the comparison is:

```text
scalar = T * C_s
vector = C_setup + floor(T / VF) * C_v + (T mod VF) * C_s
```

Automatic `VF` starts from a 128-bit register assumption and is the largest
power of two no greater than `128 / widest_element_bits`. Conservative,
balanced, and aggressive profiles require minimum trip counts of `4*VF`,
`2*VF`, and `VF`, respectively, and estimated scalar/vector cost ratios of at
least 1.25, 1.08, and 1.00. Forced widths bypass the profitability ratio but not
legality or the requirement that at least one full vector fits.

These are transparent, deterministic heuristics. They are not learned values,
measured cycle counts, or promises about any microarchitecture.

### 3.3 Lessons from cost-model research

Porpodas and Jones show in the SLP setting that vectorizing a smaller subgraph
can outperform greedily vectorizing all available scalar work because scalar ↔
vector data movement can make parts of a graph harmful. Their Throttled SLP
method reported a 9% average execution-time improvement and up to 14% on the
paper's evaluated kernels relative to its SLP baseline
([Porpodas and Jones, *Throttling Automatic Vectorization: When Less Is
More*](https://doi.org/10.1109/PACT.2015.32), PACT 2015; [paper](https://www.cl.cam.ac.uk/~tmj32/papers/docs/porpodas15-pact.pdf)).
That result concerns a particular SLP algorithm, benchmark set, and machine; it
does not establish those percentages for this loop vectorizer. Its transferable
lesson is to search bounded alternatives and retain “do not transform” as a
real candidate.

Pohl, Cosenza, and Juurlink evaluated compiler vectorization cost predictions
for AVX2, NEON, and SVE. They found that LLVM and GCC often overestimated
speedup and that static predictions had weak-to-medium correlation with
measured gains in their experiments. Their proposed model adds a more detailed
IR feature representation, including memory-access features, and fits it to
target data
([Pohl, Cosenza, and Juurlink, *Vectorization Cost Modeling for NEON, AVX and
SVE*](https://doi.org/10.1016/j.peva.2020.102106), *Performance Evaluation*,
2020; [paper](https://www.cosenza.eu/papers/PohlPEJ20.pdf)).

For this project, those results argue against presenting the simple score as a
performance oracle. The three policy profiles are valuable for controlled
experiments, but the proper comparison is empirical: identical scalar input
IR, identical backend flags, verified outputs, repeated run-time samples, and a
separate accounting of pass latency.

## 4. SIMD utilization: define the denominator

“SIMD utilization” is ambiguous unless the numerator and denominator are
stated. At least three different quantities are useful:

1. **Vector coverage:** the share of scalar iterations performed by the main
   vector loop,

   ```text
   U_coverage = VF * floor(T / VF) / T
   ```

2. **Issued-lane utilization:** useful lanes divided by lane slots in issued
   vector instructions. This project's vector body contains only full vectors,
   so this value is 100% for its main loop; the remainder executes scalar code.
3. **Hardware utilization:** achieved vector-pipeline occupancy or throughput,
   which needs target counters or a carefully justified proxy. It cannot be
   inferred from LLVM vector types alone.

The pass reports the first quantity as `estimated_vector_coverage`. It also
reports `issued_lane_utilization=100%` for accepted plans because it issues only
full vectors. Neither field is measured hardware occupancy. For an unknown trip
count the coverage estimate uses `T = 64`, and that uncertainty must be retained
when comparing policies.

A predicated vector tail would use a different accounting: the last issued
vector can have inactive lanes. LLVM exposes an active-lane-mask intrinsic whose
lane `i` represents whether the corresponding base-plus-lane index is below the
trip count
([LLVM Language Reference, `llvm.get.active.lane.mask`](https://llvm.org/docs/LangRef.html#llvm-get-active-lane-mask-intrinsics)).
This project does not generate masked tails or scalable vectors; it rounds the
vector trip count down and reuses the scalar loop for the remainder.

## 5. Evaluation evidence and an appropriate protocol

### 5.1 Breadth benchmarks answer a different question from speed benchmarks

Maleki, Gao, Garzarán, Wong, and Padua evaluated GCC 4.7, ICC 12.0, and XLC
11.01 using a synthetic suite of 151 loops, two PACT applications, and eight
MediaBench II applications. They reported that 45–71% of the synthetic loops
were vectorized by those compiler versions, while only a few loops in the real
applications were vectorized
([Maleki et al., *An Evaluation of Vectorizing Compilers*](https://doi.org/10.1109/PACT.2011.68),
PACT 2011). LLVM's test suite retains an adapted TSVC derived from that work
([LLVM TSVC README](https://github.com/llvm/llvm-test-suite/blob/main/MultiSource/Benchmarks/TSVC/README)).

The result demonstrates the value of a taxonomy-rich corpus: coverage reveals
which idioms a vectorizer recognizes. It is not a current ranking of compilers;
the evaluated versions and machines are historical. Nor is coverage alone a
speed metric. A project evaluation should report both:

- **capability:** accepted/rejected loops and precise rejection reasons; and
- **quality:** correctness, generated vector IR/assembly, run time, code size,
  and compile-time cost.

### 5.2 Fair comparison with LLVM

A defensible local experiment should compile each kernel once into canonical
scalar LLVM IR with both LLVM vectorizers disabled. From that byte-identical
input, it should produce:

1. a scalar control passed only through verification,
2. a custom-pass variant, and
3. an LLVM `loop-vectorize` variant.

All variants should use identical backend and link flags. The experiment should
run edge-case correctness checks (zero, shorter than `VF`, exactly `VF`, and
non-multiple trip counts), warm up the code, collect repeated samples, and
publish distributions rather than a single best run. For compilation latency,
the in-pass Rust timers must be reported separately from `opt` startup, plugin
loading, IR parsing, verification, serialization, and diagnostic I/O.

### 5.3 Local controlled result

The checked-in protocol completed on 8 September 2026 on an Apple M5 host
(Darwin 25.5.0, `arm64-apple-darwin25.5.0`) with LLVM 21.1.3 and Rust
1.90.0. The command was:

```sh
./scripts/benchmark.py --warmups 3 --samples 21 \
  --inner-calls 64 --timing-runs 15
```

All scalar, custom, and LLVM executables passed the edge/tail and full-buffer
correctness checks. Both vectorizers transformed all seven run-time kernels;
the custom pass also transformed all 128 cloned compile-time loops. The seven-
kernel geometric-mean speedup over the scalar control was **3.009× custom** and
**2.974× LLVM** in this run.

| Kernel | Custom / scalar | LLVM / scalar |
|---|---:|---:|
| `add_f32` | 2.513× | 2.513× |
| `conditional_adjust_i32` | 3.059× | 3.059× |
| `increment_i32` | 4.032× | 4.032× |
| `mul_add_f32` | 2.498× | 2.523× |
| `scale_i32` | 3.865× | 3.924× |
| `widen_u16` | 3.729× | 6.795× |
| `write_index_u64` | 2.000× | 0.986× |

On the cloned corpus, the custom pass's in-process analysis-plus-transform
time was **2.355 µs p50**, **8.790 µs p95**, and **31.404 µs p99** (36.708 µs
maximum). Its analysis alone was 0.771 µs p50 and transformation was 1.583 µs
p50. Median whole-process `opt` time, including verification, was 9.733 ms for
the no-op control, 10.099 ms for plugin-load/no-op, 10.684 ms for the custom
pass, and 21.633 ms for LLVM LoopVectorize. Baseline subtraction gives an
approximate 4.572 µs/custom loop versus 92.968 µs/LLVM loop, but fixed process
costs make that derived comparison much less reliable than the custom pass's
direct timers.

These are cache-hot microbenchmarks on one host, not a claim that the custom
pass generally beats LLVM. The per-kernel table itself shows why a geomean must
not hide individual behavior. The complete reproducible snapshot is
[`benchmark-results.json`](benchmark-results.json); the harness generates the
full raw JSON/CSV under `build/benchmark/`.

### 5.4 Tests are not a proof of equivalence

The current suite combines Rust unit tests, LLVM's IR verifier, positive and
negative fixtures, a guard-page tail harness, and a generated native
differential corpus. The full deterministic campaign exercises 32 accepted
kernel shapes under natural and forced VFs, `-O0`/`-O2`, 48 seeded inputs, 37
boundary or seeded trip counts, and UBSan: 397,824 scalar/vector executions were
byte-identical. Seven pass configurations also verify natural-VF choices, while
nine unsupported shapes remain scalar. Together these layers catch many
structural and functional errors. They do not quantify over every input or
every behavior allowed by LLVM IR.

An AddressSanitizer attempt on the measured macOS host was excluded: the
Homebrew LLVM 21 ASan runtime stalled in dynamic-loader initialization before
the test program reached `main`. UBSan completed, and the separate guard-page
test still checks vector-tail over-read and overwrite, but neither substitutes
for a successful ASan run.

Taneja et al.'s LLM-Vectorizer study is a recent cautionary example: it uses
Alive2 bounded translation validation in addition to execution feedback and
reports that 38.2% of its generated TSVC vectorizations could be verified
([Taneja et al., *LLM-Vectorizer: LLM-based Verified Loop Vectorizer*](https://arxiv.org/abs/2406.04693),
2024). Failure to verify is not automatically proof of a wrong transform—the
validator can encounter boundedness or scalability limits—but the gap shows why
test success should not be relabeled as semantic proof.

For this project, Alive2-compatible before/after validation, property-based
generation of new IR structures, a working ASan environment, and target-diverse
CI remain appropriate next validation layers. The checked-in generated corpus,
seeded data/trip variation, and UBSan run cover a bounded set of structures.

## 6. Design implications adopted by this project

| Research implication | Implemented response | Boundary that remains |
|---|---|---|
| Preserve dependences before changing schedule. | Every write-related memory pair is classified; loop-carried and potential results reject the loop. | Only one-dimensional coefficient-one access functions are recognized. |
| Fast tests must fail closed. | GCD non-divisibility proves independence; equal slopes compute distance; overflow/unsupported forms become `Potential` or a rejection. | No Banerjee bounds, multidimensional tests, ILP fallback, or memoized inter-loop summaries. |
| Alias reasoning precedes subscript reasoning. | Distinct globals and pairs of `noalias` arguments are accepted as disjoint; other different-base pairs reject. | No LLVM AA/SCEV integration or run-time pointer versioning. |
| Plan before mutating. | Candidate validation and one profitability plan complete before the IR rewriter runs. | No persistent VPlan, alternative graph, rollback system, or post-plan re-legalization. |
| Keep “do nothing” available. | Unprofitable, short, unsupported, or uncertain loops are left scalar with a reason in report mode. | Forced-VF modes intentionally override only the cost decision for experiments. |
| Tail behavior is part of the plan. | Full-vector trip count is rounded down; the original loop executes the scalar remainder. | No masked/predicated tail or epilogue vectorization. |
| Cost predictions need empirical calibration. | Three explicit policies and a benchmark protocol make the assumptions inspectable. | No `TargetTransformInfo`, instruction latency/throughput, code-size, register-pressure, or cache model. |
| Validation needs independent layers. | Unit, fixture, verifier, guard-page, generated differential/UBSan, and controlled benchmark checks cover the supported subset. | No translation-validation proof, property-based structural fuzzing, working ASan result, or multi-target CI result yet. |

## 7. Present limitations and research roadmap

The implementation should currently be described as a **bounded educational
vectorizer for canonical affine loops**. It should not be described as a
drop-in replacement for LLVM LoopVectorize or as a complete implementation of
the classical dependence algorithms.

The highest-value extensions, in risk order, are:

1. **Stronger validation before a wider language.** Add Alive2 translation
   checks for generated before/after pairs and property-based generation beyond
   the checked-in 32-shape corpus; run ASan and the existing UBSan/differential
   campaign across multiple targets.
2. **LLVM analysis integration.** Consume `LoopInfo`, `ScalarEvolution`, alias
   results, and/or `LoopAccessAnalysis` through a narrow C++ bridge. Add guarded
   run-time disambiguation with an explicit scalar fallback.
3. **Target-aware multi-plan search.** Query `TargetTransformInfo`; compare
   multiple `VF`/interleave candidates, code-size objectives, scalarization,
   setup, and remainder strategies; retain the scalar plan.
4. **Richer legal constructs.** Model reductions, live-outs, multiple blocks,
   if-conversion, calls with vector mappings, and predicated memory without
   weakening poison, exception, or floating-point semantics.
5. **Scalable and outer-loop vectorization.** Treat scalable vector length and
   active-lane masks as first-class plan properties. Outer-loop vectorization
   is its own planning problem, not a switch on the current single-block pass
   ([Nuzman and Zaks, *Outer-Loop Vectorization: Revisited for Short SIMD
   Architectures*](https://doi.org/10.1145/1454115.1454119), PACT 2008).
6. **Measured cost calibration.** Expand from a small canonical kernel suite to
   categorized TSVC/LLVM-test-suite cases and real application kernels. Report
   missed opportunities and regressions, not only geometric-mean speedup.

The project's compilation-latency goal remains scoped. Fixed limits on loop
instructions and memory accesses bound expensive candidate work, but they do
not make whole-module traversal constant-time or establish a universal
microsecond guarantee. On the stated 128-loop corpus the measured in-process
analysis-plus-transform distribution was in the microsecond range (2.355 µs
p50, 8.790 µs p95, 31.404 µs p99); process startup and IR I/O are separate
costs.

## 8. Evidence ledger

This ledger records what each source supports and prevents results from one
scope being silently generalized to another.

| Source | Date | Claim used here | Scope/caveat |
|---|---:|---|---|
| [Allen & Kennedy, *Automatic Translation of FORTRAN Programs to Vector Form*](https://doi.org/10.1145/29873.29875) | 1987 | Dependence preservation is the semantic basis for vectorizing transformation; classical fast dependence tests. | FORTRAN-to-vector-form framework, not LLVM IR. |
| [Goff, Kennedy & Tseng, *Practical Dependence Testing*](https://doi.org/10.1145/113445.113448) | 1991 | Practical taxonomy and stronger special-case tests, including SIV cases. | Does not imply the project implements the full algorithm. |
| [Maydan, Hennessy & Lam, *Efficient and Exact Data Dependence Analysis*](https://doi.org/10.1145/113445.113447) | 1991 | Layered exact special cases, backup analysis, and memoization can combine precision with practical compile time. | The project adopts the fail-closed tiering idea, not the exact fallback. |
| [LLVM, *Vectorization Plan*](https://llvm.org/docs/VectorizationPlan.html) | living documentation | Legal → Plan → Execute; only execution mutates IR; plan represents alternatives. | Describes LLVM's evolving VPlan architecture. |
| [LLVM, `LoopAccessAnalysis`](https://github.com/llvm/llvm-project/blob/main/llvm/include/llvm/Analysis/LoopAccessAnalysis.h) | current `main` | Production memory-dependence reasoning can use SCEV/AA and run-time checks. | A moving implementation reference, not a stable API contract. |
| [LLVM, *Auto-Vectorization in LLVM*](https://llvm.org/docs/Vectorizers.html) | living documentation | Scalar remainders, run-time pointer checks, reductions, and other production features. | Feature overview; details vary by target and LLVM version. |
| [Nuzman & Henderson, *Multi-platform Auto-vectorization*](https://doi.org/10.1109/CGO.2006.25) | 2006 | Target-neutral vector IR must be paired with platform-sensitive profitability. | Historical architecture study. |
| [Porpodas & Jones, *Throttling Automatic Vectorization*](https://doi.org/10.1109/PACT.2015.32) | 2015 | Less aggressive SLP graph formation can outperform full greedy packing. | SLP, evaluated kernels, and paper platform—not this loop pass. |
| [Pohl, Cosenza & Juurlink, *Vectorization Cost Modeling for NEON, AVX and SVE*](https://doi.org/10.1016/j.peva.2020.102106) | 2020 | Compiler cost estimates can correlate poorly with measured gains; richer target-calibrated features improve prediction in the study. | Results are tied to its targets, corpus, and fitted model. |
| [Maleki et al., *An Evaluation of Vectorizing Compilers*](https://doi.org/10.1109/PACT.2011.68) | 2011 | TSVC-style breadth evaluation exposes recognized and missed loop classes. | Compiler versions are historical; coverage is not speed. |
| [Taneja et al., *LLM-Vectorizer*](https://arxiv.org/abs/2406.04693) | 2024 | Execution feedback should be supplemented with translation validation; 38.2% of generated TSVC vectorizations were verified in that study. | Bounded verification can be inconclusive; the technique is not yet integrated here. |

Research stopped after primary papers, official LLVM documentation/source, and
the inspected implementation jointly covered legality, planning, utilization,
evaluation, and validation. Further sources were unlikely to change the current
design boundary; the principal remaining gaps require implementation and local
measurement rather than more literature.
