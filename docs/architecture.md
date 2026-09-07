# Architecture and correctness contract

## 1. Scope

`rust-loop-vectorizer` is an LLVM 21 New Pass Manager module pass. A thin C++17
adapter registers pipeline names and provides a few C++ API operations; Rust
performs loop discovery, legality analysis, dependence classification,
profitability planning, diagnostics, and LLVM IR construction through
`llvm-sys`.

The pass has one governing rule: **uncertainty is a rejection, not permission to
transform**. This permits a compact implementation and bounded candidate cost,
but it intentionally leaves many loops to LLVM's production vectorizer.

The major files are:

| File | Responsibility |
|---|---|
| [`native/pass_plugin.cpp`](../native/pass_plugin.cpp) | New PM registration; pass-name parsing; C++ bridges for module wrapping, PHI incoming-block updates, loop metadata, and `DataLayout` checks. |
| [`src/lib.rs`](../src/lib.rs) | Dynamic-plugin ABI, FFI configuration, panic containment, and hand-off to the Rust pass. |
| [`src/vectorizer.rs`](../src/vectorizer.rs) | Candidate discovery, complete legality preflight, cost input collection, IR widening, scalar-tail construction, and remarks. |
| [`src/dependence.rs`](../src/dependence.rs) | Pure Rust affine-pair classification and unit tests. |
| [`src/cost.rs`](../src/cost.rs) | Vector-factor selection, trip thresholds, scalar/vector score, and estimated vector coverage. |
| [`src/config.rs`](../src/config.rs) | Conservative, balanced, aggressive, and forced-width policies. |
| [`src/llvm.rs`](../src/llvm.rs) | Narrow wrappers around borrowed LLVM handles and C++ bridge functions. |

## 2. End-to-end pipeline

```text
LLVM Module
    │
    ├─ scan defined functions and basic blocks
    │
    ├─ discover a conditional self-edge
    │      (candidate one-block loop)
    │
    ├─ LEGAL
    │    ├─ canonical CFG and induction
    │    ├─ supported instructions and dataflow order
    │    ├─ affine memory extraction
    │    ├─ alias and dependence checks
    │    └─ target DataLayout compatibility
    │
    ├─ PLAN
    │    ├─ choose fixed VF from widest scalar type
    │    ├─ estimate scalar, vector, setup, and tail work
    │    └─ reject or return Candidate + Plan
    │
    └─ EXECUTE
         ├─ insert dispatch
         ├─ emit full-width vector loop
         ├─ connect scalar remainder / fallback
         └─ update original induction PHI
```

This is inspired by LLVM's **Legal → Plan → Execute** organization
([LLVM VPlan documentation](https://llvm.org/docs/VectorizationPlan.html)), but
the project's `Candidate` and `Plan` are plain Rust structures rather than
VPlan recipes. There is one proposed plan, not a persistent graph of competing
alternatives.

## 3. Plugin boundary and pass surface

LLVM loads the compiled `cdylib` through `llvmGetPassPluginInfo`. The C++
registration callback installs a module pass that invokes the Rust
`rv_run_module` entry point with this ABI-stable-by-construction configuration:

```text
RVPassConfig {
    heuristic: u32,
    forced_vf: u32,
    emit_remarks: u32,
}
```

The small Rust/C++ configuration layout is explicit, but the surrounding LLVM
C++ plugin ABI is not stable across arbitrary LLVM builds. The plugin must be
compiled against and loaded by the same compatible LLVM 21 installation.

The registered pipeline names are:

| Pipeline name | Policy |
|---|---|
| `rust-loop-vectorize` / `rust-loop-vectorize-balanced` | Balanced automatic width. |
| `rust-loop-vectorize-report` | Balanced automatic width plus one diagnostic per discovered candidate. |
| `rust-loop-vectorize-conservative` | Higher trip-count and predicted-gain thresholds. |
| `rust-loop-vectorize-conservative-report` | Conservative policy plus diagnostics. |
| `rust-loop-vectorize-aggressive` | Lowest automatic profitability thresholds. |
| `rust-loop-vectorize-aggressive-report` | Aggressive policy plus diagnostics. |
| `rust-loop-vectorize-force-vf2` | Force fixed `VF=2` after legality checks. |
| `rust-loop-vectorize-force-vf2-report` | Force `VF=2` plus diagnostics. |
| `rust-loop-vectorize-force-vf4` | Force fixed `VF=4` after legality checks. |
| `rust-loop-vectorize-force-vf4-report` | Force `VF=4` plus diagnostics. |
| `rust-loop-vectorize-force-vf8` | Force fixed `VF=8` after legality checks. |
| `rust-loop-vectorize-force-vf8-report` | Force `VF=8` plus diagnostics. |
| `rust-loop-vectorize-force-vf16` | Force fixed `VF=16` after legality checks. |
| `rust-loop-vectorize-force-vf16-report` | Force `VF=16` plus diagnostics. |

Forced widths bypass the cost-ratio check; they do not bypass structural,
memory, dependence, type, or `DataLayout` legality.

LLVM owns every module, function, block, instruction, and type handle. Rust
borrows these handles for a single pass invocation and never disposes or stores
them globally. Rust builders are locally owned and disposed through `Drop`.
`catch_unwind` prevents an unwinding Rust panic from crossing the exported C
ABI boundary. A panic before mutation is contained as an unchanged pass. Once
mutation begins, a thread-local sticky bit makes an unexpected panic fail-stop
rather than returning “unchanged” with invalidated analyses or partial IR. This
is defensive containment, not a substitute for validation or LLVM verification.

## 4. Supported input IR contract

A loop must satisfy every condition below.

### 4.1 Control flow and induction

- The entire loop is one basic block whose conditional branch has a self-edge
  and one exit edge.
- The header has exactly one external incoming edge from the induction
  preheader, and that preheader terminates in an ordinary `br`.
- There is exactly one header PHI. It is an `i64` induction variable with two
  incoming values: constant zero from outside and `i + 1` from the self-edge.
- The latch compares the incremented induction against a loop-invariant trip
  count using a supported `eq`, `ne`, or unsigned-`lt` orientation.
- The increment may be used only by the induction PHI and latch comparison;
  the comparison may be used only by the branch.
- No loop instruction has a use outside the loop, and the exit begins with no
  PHI. The pass therefore has no live-out repair problem.
- The function is not marked `optnone`, the loop does not carry
  `llvm.loop.vectorize.enable = false`, and the preheader name does not identify
  an already generated `rv.*` block.

The pass does not use `LoopInfo` to normalize arbitrary loops. Multiple-block
loops, multiple PHIs, nonzero starts, nonunit or descending induction,
non-`i64` control variables, early exits, switches, irreducible control flow,
and nested/outer-loop vectorization are outside the contract.

### 4.2 Memory accesses

- Every load/store is nonvolatile and nonatomic.
- Its pointer is a loop-local, one-index `getelementptr`.
- The GEP base is loop-invariant and is directly an LLVM argument or global
  variable; pointer casts and nested invariant GEPs are not stripped.
- The index is `i`, `i + C`, `C + i`, or `i - C`, where `C` is an `i64`
  constant representable without negation overflow.
- The accessed scalar type equals the GEP source element type.
- Memory elements are `i8`, `i16`, `i32`, `i64`, `half`, `float`, or `double`.
- The target `DataLayout` must report no per-element allocation padding and a
  fixed-vector store size exactly equal to `VF` scalar allocation strides.
- The module must declare a nonempty `DataLayout`; generic defaults are not
  treated as evidence about the eventual code-generation target.
- The pointer address space's GEP index width must be exactly 64 bits, matching
  the induction domain; narrower or wider modular index arithmetic rejects.
- A GEP may feed only the pointer operand of a load/store in the same loop.
  Affine index helper instructions may feed only GEPs.

The pass copies the scalar load/store alignment to the widened memory
instruction. It does not infer a stronger alignment, emit alignment guards,
form interleaved access groups, handle gathers/scatters, or generate masked
memory operations.

### 4.3 Alias and dependence rules

For each pair containing at least one write:

1. Different bases are independent only if both are distinct globals or both
   are arguments marked `noalias`; otherwise the loop rejects.
2. Same-base accesses must have the same scalar element type.
3. The affine classifier compares coefficient and constant offset. In the
   current IR recognizer every coefficient is one.
4. An exact distance of zero is accepted because the widened instruction order
   retains the scalar statement order lane by lane.
5. A nonzero loop-carried distance or an unresolved result rejects the loop.

Read/read pairs never block vectorization. The pure Rust classifier contains a
general GCD independence filter, but the front-end currently supplies only
coefficient-one functions. The pass neither consumes LLVM
`LoopAccessAnalysis` nor emits run-time pointer checks. See the
[research report](research-report.md#2-dependence-analysis-what-the-foundational-work-establishes)
for the precise distinction from LLVM's production machinery.

### 4.4 Compute and dataflow

Supported data operations are:

- integer/floating add, subtract, multiply, divide, and remainder;
- integer shifts and bitwise AND/OR/XOR;
- floating negation;
- integer and floating widening/narrowing/conversion casts represented in the
  allow-list;
- integer/floating comparisons; and
- `select`.

Calls, invokes, fences, atomics, volatile memory, reductions, additional PHIs,
and any unlisted opcode reject the loop. Loop-local inputs must already have a
widened producer in the original SSA instruction order. Loop-invariant scalar
operands are splatted and cached. The induction is widened into
`base + <0, 1, …, VF-1>` only when used as data; address generation otherwise
uses the scalar base index for the contiguous wide access.

Before execution, `validate_widening_order` simulates the value-map
availability required by the transformer. This preflight prevents a
recognized-but-unmaterializable data dependency from reaching the mutation
stage.

## 5. Planning model

### 5.1 Vector factor

The automatic policy assumes 128 vector bits. Let `W` be the largest bit width
seen among memory elements and compute inputs/results (casts account for both
ends). The natural width is the largest power of two not exceeding `128 / W`.
Therefore the usual choices are:

| Widest scalar width | Natural VF |
|---:|---:|
| 8 | 16 |
| 16 | 8 |
| 32 | 4 |
| 64 | 2 |

Widths below two lanes, non-power-of-two forced widths, and forced widths above
64 reject at planning. Public pass names expose forced widths through 16.

This is a fixed-width IR policy. It does not query register widths, legal vector
types, instruction throughput, or lowering cost from `TargetTransformInfo`, and
it does not generate scalable vectors.

### 5.2 Cost equation and policy profiles

Each load/store contributes 2 cost units. Division/remainder contributes 4;
other supported compute operations contribute 1. Setup contributes 6. With
estimated trip count `T`:

```text
C_scalar = T * C_iteration
C_vector = 6
         + floor(T / VF) * C_vector_iteration
         + (T mod VF) * C_iteration
```

`C_vector_iteration` currently sums the same opcode weights as one scalar
iteration, on the assumption that each widened operation performs `VF` useful
elements. A constant latch bound supplies `T`; otherwise `T=64` is the planning
estimate. Saturating integer arithmetic prevents cost overflow from becoming a
false profitable result.

| Profile | Minimum estimated trip count | Required `C_scalar / C_vector` |
|---|---:|---:|
| Conservative | `4 * VF` | 1.25 |
| Balanced | `2 * VF` | 1.08 |
| Aggressive | `VF` | 1.00 |
| Forced VF | `VF` | bypassed |

The model is intentionally inspectable and cheap. Its score is not a cycle
estimate and should not be compared numerically with LLVM's target-aware cost
without calibration.

### 5.3 Reported utilization

The `estimated_vector_coverage` field is computed as:

```text
100 * floor(T / VF) * VF / T
```

This is the percentage of scalar iterations assigned to full vector chunks.
The separate `issued_lane_utilization` field is 100% for accepted plans because
every issued vector is full and the remainder is scalar. Hardware SIMD
occupancy is not measured.

## 6. IR rewrite

For an accepted loop, execution inserts three blocks immediately before the
original scalar header:

```text
preheader
    │
    ▼
rv.vector.dispatch ── too short ─────────────┐
    │ enough iterations                     │
    ▼                                       │
rv.vector.body ◄── vector backedge          │
    │ vector index reaches rounded bound    │
    ▼                                       │
rv.vector.exit ── remainder exists ─────────┤
    │ no remainder                          ▼
    └──────────────────────────────────► original scalar loop
                                           │
                                           ▼
                                         exit
```

More exactly:

1. The preheader's unique edge to the scalar header is retargeted to
   `rv.vector.dispatch`, and the original induction PHI's outside incoming block
   becomes the dispatch block.
2. Dispatch compares the actual trip count with the policy's minimum threshold
   and computes `vector_trip = trip_count & ~(VF - 1)`. Power-of-two `VF` makes
   this an unsigned round-down.
3. Short loops branch directly to the untouched scalar loop.
4. The vector loop starts its scalar base index at zero, widens instructions in
   original SSA order, and increments its base by `VF` until it equals
   `vector_trip`.
5. The vector exit compares `vector_trip` with the actual trip count. An exact
   multiple bypasses the scalar loop; otherwise it enters that loop at
   `i = vector_trip` through a newly added PHI incoming value.

Wide GEPs point at the first scalar element of each chunk. Loads and stores use
fixed vector types. Loop-invariant scalars become vector splats; data uses of the
induction receive a lane-offset vector. The rewriter propagates GEP `inbounds`,
memory alignment, integer `nuw`/`nsw`, applicable `exact`, and fast-math flags
to their corresponding widened operations.

The original scalar body is retained, so it serves both as the short-loop
fallback and as the epilogue. The current pass does not clone a separate scalar
body, mark generated loops with follow-up metadata, unroll/interleave, or
produce a masked tail.

## 7. Correctness invariants

The legality checks and rewrite are coupled by these invariants:

1. **No mutation before acceptance.** Discovery, shape checking, dataflow
   preflight, dependence analysis, cost selection, and layout checking finish
   before block insertion.
2. **A single rewritable entry edge.** Exactly one ordinary branch edge enters
   the loop from outside, so retargeting it cannot miss another path or corrupt
   `indirectbr` block-address semantics.
3. **Equivalent iteration partition.** For vectorized execution,
   `[0, vector_trip)` is processed once in full chunks and
   `[vector_trip, trip_count)` once by the scalar loop. The dispatch threshold
   prevents entry to a zero-iteration vector loop.
4. **Same element mapping.** Coefficient-one GEPs plus packed target layout make
   one `<VF x T>` access cover the same byte locations as `VF` adjacent scalar
   accesses.
5. **No reordered loop-carried hazard.** Every write-related pair is either
   statically disjoint or same-iteration; unresolved and nonzero-distance pairs
   reject.
6. **No unrepaired SSA live-out.** All loop instruction uses remain in the
   original header, and the exit has no PHI, so bypassing the scalar remainder
   cannot expose an undefined loop-produced value.
7. **Materializable SSA order.** Every non-invariant vector operand is present
   in the preflight value set before its consumer.
8. **Semantic flags are not silently dropped.** Widened arithmetic and memory
   instructions copy the supported flags/alignments whose loss could change
   optimization semantics or code generation.
9. **Target layout is checked, not assumed.** Odd-width integer memory, padded
   layouts, and GEP index widths other than 64 reject instead of relying on
   host ABI intuition.

After the pass, LLVM's verifier should run in the same pipeline. Verification
establishes IR structural validity; it does not itself prove observational
equivalence with the scalar input.

The executor is preflighted but not rollback-transactional. All expected
rejections happen before mutation, and post-commit lowering mismatches are
unreachable invariants rather than recoverable `false` returns. If one is ever
violated (including by future analyzer/lowerer drift), the FFI guard terminates
the process instead of allowing partially built IR to escape. Constructing an
off-CFG typed recipe and atomically committing it would improve availability,
but not the current fail-closed semantic boundary.

## 8. Rejection model and diagnostics

Report mode writes one machine-parseable line per discovered candidate:

```text
rv-vectorize: function=F loop=L decision=D reason=R vf=N \
estimated_vector_coverage=C% issued_lane_utilization=U% \
analysis_us=A transform_us=X
```

Function and block names are percent-encoded into single whitespace-free atoms,
so unusual LLVM quoted identifiers cannot split or inject diagnostic records.

Representative rejection families are:

| Family | Examples |
|---|---|
| Bounds | `instruction-budget-exceeded`, `memory-access-budget-exceeded` |
| CFG/SSA | `loop-needs-unique-preheader`, `preheader-terminator-must-be-branch`, `loop-value-live-out`, `live-out-phi` |
| Induction/latch | `induction-type-must-be-i64`, `induction-must-start-at-zero`, `induction-step-must-be-one`, `unsupported-latch-predicate` |
| Memory shape | `memory-pointer-must-be-loop-gep`, `non-affine-index`, `unsupported-pointer-base`, `unsupported-memory-element-type` |
| Safety | `volatile-or-atomic-memory`, `possible-pointer-alias`, `loop-carried-memory-dependence`, `incompatible-target-memory-layout` |
| Dataflow/opcodes | `unsupported-instruction`, `compute-input-not-widened`, `store-input-not-widened` |
| Policy | `disabled-by-loop-metadata`, `not-profitable`, `already-vectorized` |

Functions marked `optnone` are skipped rather than reported. A rejection leaves
the original IR unchanged because it occurs before Execute.

## 9. Complexity and latency budget

The design uses explicit per-candidate ceilings:

- at most 96 instructions in the single-block body;
- at most 24 memory accesses;
- therefore at most `24*23/2 = 276` unordered memory pairs;
- power-of-two `VF` no greater than 64; and
- one linear widening pass over the accepted body.

Within one candidate, instruction validation and widening are linear in body
size apart from pairwise dependence analysis, which is `O(M²)` but capped at
276 pairs. Use-list checks scale with the uses of candidate instructions.
Finding the unique external edge scans the function's blocks and successors.
Whole-module time still scales with functions and blocks, and allocation,
LLVM API, diagnostics, and cache state prevent a hard real-time guarantee.

The microsecond objective applies only to in-process per-loop analysis and
transformation timers. It excludes process startup, dynamic plugin loading, IR
parsing, verification, serialization, and remark I/O. End-to-end compilation
must be reported separately.

The 8 September 2026 release-build run on Apple M5 with LLVM 21.1.3 reported
the following distributions over 128 transformed cloned loops (report mode):

| In-process time (µs/loop) | p50 | p95 | p99 | max |
|---|---:|---:|---:|---:|
| Analysis | 0.771 | 2.473 | 13.820 | 17.375 |
| Transformation | 1.583 | 5.381 | 22.769 | 33.167 |
| Total | 2.355 | 8.790 | 31.404 | 36.708 |

Median end-to-end `opt` times for the 128-loop module were 9.733 ms no-op,
10.099 ms plugin-load/no-op, 10.684 ms custom pass, and 21.633 ms LLVM
LoopVectorize. These include fixed process and verification costs. The
baseline-adjusted estimates were 4.572 µs/custom loop and 92.968 µs/LLVM loop,
but subtraction amplifies noise and is secondary evidence. See the
[recorded snapshot](benchmark-results.json) and [research report](research-report.md#53-local-controlled-result)
for configuration and interpretation.

## 10. Verification boundary

The checked-in verification strategy is layered:

- Rust unit tests exercise dependence and cost edge cases.
- Positive IR fixtures check emitted vector types and operations.
- Negative fixtures check fail-closed behavior for dependence, aliasing,
  volatile memory, calls, disabled metadata, live-outs, exotic preheaders,
  overflowing offsets, and incompatible `DataLayout`.
- LLVM's `verify` pass checks both transformed and rejected output.
- A native harness checks supported kernels over trip counts that cover zero,
  scalar-only, exact-vector, and vector-plus-remainder paths and checks that the
  pass does not overwrite beyond `n`.
- `make differential-full` generates 32 accepted kernel shapes, checks all
  seven policy/forced-VF configurations plus nine unsupported probes, and runs
  397,824 byte-for-byte scalar/vector comparisons across boundary and seeded
  trip counts, `-O0`/`-O2`, and UBSan.

This establishes substantial confidence in the stated subset, not a formal
translation proof. The architectural next step is independent before/after
translation validation, property-based structural generation beyond the fixed
corpus, and multi-target sanitizer/CI runs. Capability expansion—runtime alias
checks, reductions, predication, scalable vectors, and multi-block loops—should
follow only after the corresponding invariants and validation oracles exist.
