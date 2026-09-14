#!/bin/sh
set -eu

repo_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
llvm_prefix=${LLVM_SYS_211_PREFIX:-}
if [ -z "$llvm_prefix" ]; then
  for candidate in /opt/homebrew/opt/llvm /usr/local/opt/llvm /usr/lib/llvm-21; do
    if [ -x "$candidate/bin/llvm-config" ]; then
      llvm_prefix=$candidate
      break
    fi
  done
fi
if [ -z "$llvm_prefix" ]; then
  printf '%s\n' 'LLVM 21 not found; set LLVM_SYS_211_PREFIX.' >&2
  exit 1
fi

export LLVM_SYS_211_PREFIX="$llvm_prefix"
opt="$llvm_prefix/bin/opt"
clang="$llvm_prefix/bin/clang"

case $(uname -s) in
  Darwin) plugin="$repo_dir/target/release/librust_loop_vectorizer.dylib" ;;
  *)      plugin="$repo_dir/target/release/librust_loop_vectorizer.so" ;;
esac

build_dir="$repo_dir/build/test"
mkdir -p "$build_dir"

cd "$repo_dir"
cargo test --quiet
cargo build --release --quiet

"$opt" \
  -load-pass-plugin="$plugin" \
  -passes='rust-loop-vectorize-report,verify' \
  -S tests/fixtures/vectorizable.ll \
  -o "$build_dir/vectorized.ll" \
  2>"$build_dir/vectorized.remarks"

vector_loops=$(grep -c 'decision=vectorized' "$build_dir/vectorized.remarks")
[ "$vector_loops" -eq 9 ]
grep -q 'load <4 x float>' "$build_dir/vectorized.ll"
grep -q 'load <4 x i32>' "$build_dir/vectorized.ll"
grep -q 'store <2 x i64>' "$build_dir/vectorized.ll"
grep -q 'zext <4 x i16>' "$build_dir/vectorized.ll"
grep -q 'select <4 x i1>' "$build_dir/vectorized.ll"
grep -q 'function=increment_in_place .*decision=vectorized' "$build_dir/vectorized.remarks"
grep -q 'function=increment_in_place_ne .*decision=vectorized' "$build_dir/vectorized.remarks"
grep -q 'function=increment_in_place_ult .*decision=vectorized' "$build_dir/vectorized.remarks"
grep -q 'function=ordered_double_store .*decision=vectorized' "$build_dir/vectorized.remarks"

"$opt" \
  -load-pass-plugin="$plugin" \
  -passes='rust-loop-vectorize-report,verify' \
  -S tests/fixtures/rejected.ll \
  -o "$build_dir/rejected-output.ll" \
  2>"$build_dir/rejected.remarks"

if grep -q 'rv.vector.body' "$build_dir/rejected-output.ll"; then
  printf '%s\n' 'a known-unsafe loop was vectorized' >&2
  exit 1
fi
grep -q 'reason=loop-carried-memory-dependence' "$build_dir/rejected.remarks"
grep -q 'reason=possible-pointer-alias' "$build_dir/rejected.remarks"
grep -q 'reason=volatile-or-atomic-memory' "$build_dir/rejected.remarks"
grep -q 'reason=unsupported-instruction' "$build_dir/rejected.remarks"
grep -q 'reason=disabled-by-loop-metadata' "$build_dir/rejected.remarks"
grep -q 'reason=loop-value-live-out' "$build_dir/rejected.remarks"
grep -q 'reason=preheader-terminator-must-be-branch' "$build_dir/rejected.remarks"
grep -q 'reason=affine-offset-overflow' "$build_dir/rejected.remarks"
grep -q 'reason=latch-condition-must-be-icmp' "$build_dir/rejected.remarks"
grep -q 'function=raw_recurrence .*reason=loop-carried-memory-dependence' "$build_dir/rejected.remarks"
grep -q 'function=war_recurrence .*reason=loop-carried-memory-dependence' "$build_dir/rejected.remarks"
grep -q 'function=shifted_waw_recurrence .*reason=loop-carried-memory-dependence' "$build_dir/rejected.remarks"
grep -q 'function=induction_next_data_use .*reason=induction-next-has-data-use' "$build_dir/rejected.remarks"
grep -q 'function=latch_compare_data_use .*reason=latch-compare-has-data-use' "$build_dir/rejected.remarks"
if grep 'function=optimization_disabled ' "$build_dir/rejected.remarks" >/dev/null; then
  printf '%s\n' 'an optnone function was analyzed' >&2
  exit 1
fi

"$opt" \
  -load-pass-plugin="$plugin" \
  -passes='rust-loop-vectorize-report,verify' \
  -disable-output tests/fixtures/padded-layout.ll \
  2>"$build_dir/padded-layout.remarks"
grep -q 'reason=incompatible-target-memory-layout' "$build_dir/padded-layout.remarks"

"$opt" \
  -load-pass-plugin="$plugin" \
  -passes='rust-loop-vectorize-report,verify' \
  -disable-output tests/fixtures/narrow-index-layout.ll \
  2>"$build_dir/narrow-index-layout.remarks"
grep -q 'reason=incompatible-target-memory-layout' "$build_dir/narrow-index-layout.remarks"

"$opt" \
  -load-pass-plugin="$plugin" \
  -passes='rust-loop-vectorize-report,verify' \
  -disable-output tests/fixtures/wide-index-layout.ll \
  2>"$build_dir/wide-index-layout.remarks"
grep -q 'reason=incompatible-target-memory-layout' "$build_dir/wide-index-layout.remarks"

"$opt" \
  -load-pass-plugin="$plugin" \
  -passes='rust-loop-vectorize-report,verify' \
  -disable-output tests/fixtures/missing-layout.ll \
  2>"$build_dir/missing-layout.remarks"
grep -q 'reason=incompatible-target-memory-layout' "$build_dir/missing-layout.remarks"

"$opt" \
  -load-pass-plugin="$plugin" \
  -passes='rust-loop-vectorize-report,verify' \
  -disable-output tests/fixtures/heuristics.ll \
  2>"$build_dir/balanced-heuristics.remarks"
grep -q 'function=offset_copy_i32 .*decision=vectorized.*vf=4' \
  "$build_dir/balanced-heuristics.remarks"

"$opt" \
  -load-pass-plugin="$plugin" \
  -passes='rust-loop-vectorize-conservative-report,verify' \
  -S tests/fixtures/heuristics.ll \
  -o "$build_dir/conservative.ll" \
  2>"$build_dir/conservative.remarks"
grep -q 'decision=rejected reason=not-profitable' "$build_dir/conservative.remarks"
if grep -q 'rv.vector.body' "$build_dir/conservative.ll"; then
  printf '%s\n' 'conservative policy vectorized a below-threshold loop' >&2
  exit 1
fi

"$opt" \
  -load-pass-plugin="$plugin" \
  -passes='rust-loop-vectorize-aggressive-report,verify' \
  -disable-output tests/fixtures/heuristics.ll \
  2>"$build_dir/aggressive.remarks"
grep -q 'decision=vectorized' "$build_dir/aggressive.remarks"

"$opt" \
  -load-pass-plugin="$plugin" \
  -passes='rust-loop-vectorize-force-vf8-report,verify' \
  -S tests/fixtures/heuristics.ll \
  -o "$build_dir/forced-vf8.ll" \
  2>"$build_dir/forced-vf8.remarks"
grep -q 'decision=vectorized.*vf=8' "$build_dir/forced-vf8.remarks"
grep -q 'load <8 x i32>' "$build_dir/forced-vf8.ll"

cli="$repo_dir/target/release/rv-vectorize"
"$cli" --opt "$opt" --plugin "$plugin" --report tests/fixtures/vectorizable.ll -o "$build_dir/cli-vectorized.ll" 2>"$build_dir/cli-vectorized.remarks"
grep -q 'load <4 x float>' "$build_dir/cli-vectorized.ll"
grep -q 'decision=vectorized' "$build_dir/cli-vectorized.remarks"

"$cli" --opt "$opt" --plugin "$plugin" --vf 8 tests/fixtures/heuristics.ll -o "$build_dir/cli-forced-vf8.ll"
grep -q 'load <8 x i32>' "$build_dir/cli-forced-vf8.ll"

sdk_flags=
if [ "$(uname -s)" = Darwin ] && command -v xcrun >/dev/null 2>&1; then
  sdk_path=$(xcrun --show-sdk-path)
  sdk_flags="-isysroot $sdk_path"
fi

cxx="$llvm_prefix/bin/clang++"
llvm_cxxflags=$("$llvm_prefix/bin/llvm-config" --cxxflags)
# shellcheck disable=SC2086
"$cxx" $sdk_flags $llvm_cxxflags -std=c++17 -Wall -Wextra -Werror \
  -Wno-unused-parameter -fsyntax-only native/pass_plugin.cpp

# Keep checked-in native fixtures and benchmarks warning-clean as ordinary C.
# shellcheck disable=SC2086
"$clang" $sdk_flags -std=c11 -Wall -Wextra -Werror -fsyntax-only \
  tests/runtime_harness.c benchmarks/kernels.c benchmarks/cloned_kernels.c \
  benchmarks/harness.c

# shellcheck disable=SC2086
"$clang" $sdk_flags -O2 -Wno-override-module -fno-vectorize -fno-slp-vectorize \
  "$build_dir/vectorized.ll" tests/runtime_harness.c \
  -o "$build_dir/runtime-check"
"$build_dir/runtime-check"

printf 'verified: %s vectorized loops, CLI, conservative bailouts, LLVM IR verifier, runtime tails\n' "$vector_loops"
