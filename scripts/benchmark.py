#!/usr/bin/env python3
"""Build and compare scalar, custom-vectorized, and LLVM-vectorized kernels.

The benchmark deliberately forks all three variants from one scalar LLVM
bitcode file. It is a bounded microbenchmark, not a universal performance or
compile-time claim; the emitted JSON records the important caveats.
"""

from __future__ import annotations

import argparse
import csv
import hashlib
import json
import math
import os
import platform
import random
import re
import shlex
import statistics
import subprocess
import sys
import time
from collections import defaultdict
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterable, Sequence


EXPECTED_CLONE_LOOPS = 128
VARIANTS = ("scalar", "custom", "llvm")
VECTOR_TYPE_RE = re.compile(
    r"<(?:vscale\s+x\s+)?\d+\s+x\s+(?:i\d+|half|bfloat|float|double|ptr)>"
)
CUSTOM_REMARK_RE = re.compile(
    r"^rv-vectorize:\s+function=(?P<function>\S+)\s+loop=(?P<loop>\S+)\s+"
    r"decision=(?P<decision>\S+)\s+reason=(?P<reason>\S+)\s+"
    r"vf=(?P<vf>\d+)\s+"
    r"estimated_vector_coverage=(?P<vector_coverage>\d+)%\s+"
    r"issued_lane_utilization=(?P<lane_utilization>\d+)%\s+"
    r"analysis_us=(?P<analysis>[0-9.]+)\s+"
    r"transform_us=(?P<transform>[0-9.]+)$"
)


class BenchmarkError(RuntimeError):
    """A benchmark setup, command, verification, or correctness failure."""


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Compare one canonical scalar LLVM IR input under no vectorizer, "
            "rust-loop-vectorize, and LLVM LoopVectorize."
        )
    )
    parser.add_argument(
        "--llvm-prefix",
        type=Path,
        help="LLVM 21 prefix; otherwise use LLVM_SYS_211_PREFIX/Homebrew/system paths",
    )
    parser.add_argument(
        "--output-dir",
        type=Path,
        help="artifact directory (default: build/benchmark)",
    )
    parser.add_argument("--elements", type=int, default=32771)
    parser.add_argument("--warmups", type=int, default=2)
    parser.add_argument("--samples", type=int, default=9)
    parser.add_argument("--inner-calls", type=int, default=12)
    parser.add_argument(
        "--timing-runs",
        type=int,
        default=7,
        help="process-level repetitions for cloned-loop pass timing",
    )
    parser.add_argument(
        "--skip-build",
        action="store_true",
        help="reuse the existing release plugin instead of running cargo build",
    )
    args = parser.parse_args()
    if args.elements <= 0:
        parser.error("--elements must be positive")
    if args.warmups < 0:
        parser.error("--warmups cannot be negative")
    if args.samples < 3:
        parser.error("--samples must be at least 3")
    if args.inner_calls <= 0:
        parser.error("--inner-calls must be positive")
    if args.timing_runs < 3:
        parser.error("--timing-runs must be at least 3")
    return args


def discover_llvm(explicit: Path | None) -> tuple[Path, str]:
    candidates: list[Path] = []
    if explicit is not None:
        candidates.append(explicit.expanduser())
    configured = os.environ.get("LLVM_SYS_211_PREFIX")
    if configured:
        candidates.append(Path(configured).expanduser())
    candidates.extend(
        Path(value)
        for value in (
            "/opt/homebrew/opt/llvm",
            "/usr/local/opt/llvm",
            "/usr/lib/llvm-21",
        )
    )

    checked: list[str] = []
    for prefix in candidates:
        llvm_config = prefix / "bin" / "llvm-config"
        if not llvm_config.is_file():
            checked.append(str(prefix))
            continue
        completed = subprocess.run(
            [str(llvm_config), "--version"],
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        version = completed.stdout.strip()
        if completed.returncode == 0 and version.split(".", 1)[0] == "21":
            return prefix.resolve(), version
        checked.append(f"{prefix} (version {version or 'unknown'})")
    raise BenchmarkError(
        "LLVM 21 not found; set LLVM_SYS_211_PREFIX or pass --llvm-prefix. "
        f"Checked: {', '.join(checked)}"
    )


def command_text(command: Sequence[str]) -> str:
    return shlex.join(str(part) for part in command)


def run_checked(
    command: Sequence[str],
    *,
    cwd: Path,
    env: dict[str, str],
) -> tuple[subprocess.CompletedProcess[str], int]:
    start = time.perf_counter_ns()
    completed = subprocess.run(
        [str(part) for part in command],
        cwd=cwd,
        env=env,
        check=False,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    elapsed = time.perf_counter_ns() - start
    if completed.returncode != 0:
        raise BenchmarkError(
            f"command failed ({completed.returncode}): {command_text(command)}\n"
            f"stdout:\n{completed.stdout[-8000:]}\n"
            f"stderr:\n{completed.stderr[-8000:]}"
        )
    return completed, elapsed


def percentile(values: Sequence[float], fraction: float) -> float:
    if not values:
        raise BenchmarkError("cannot compute a percentile of an empty sample")
    ordered = sorted(values)
    if len(ordered) == 1:
        return float(ordered[0])
    position = (len(ordered) - 1) * fraction
    lower = math.floor(position)
    upper = math.ceil(position)
    if lower == upper:
        return float(ordered[lower])
    weight = position - lower
    return float(ordered[lower] * (1.0 - weight) + ordered[upper] * weight)


def distribution(values: Iterable[float]) -> dict[str, float | int]:
    materialized = [float(value) for value in values]
    if not materialized:
        return {"count": 0, "p50": 0.0, "p95": 0.0, "p99": 0.0}
    return {
        "count": len(materialized),
        "min": min(materialized),
        "p50": percentile(materialized, 0.50),
        "p95": percentile(materialized, 0.95),
        "p99": percentile(materialized, 0.99),
        "max": max(materialized),
        "mean": statistics.fmean(materialized),
    }


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def sdk_flags() -> list[str]:
    if platform.system() != "Darwin":
        return []
    candidates: list[Path] = []
    try:
        xcrun = subprocess.run(
            ["xcrun", "--show-sdk-path"],
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )
        if xcrun.returncode == 0 and xcrun.stdout.strip():
            candidates.append(Path(xcrun.stdout.strip()))
    except FileNotFoundError:
        pass
    candidates.extend(
        [
            Path(
                "/Applications/Xcode.app/Contents/Developer/Platforms/"
                "MacOSX.platform/Developer/SDKs/MacOSX.sdk"
            ),
            Path("/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk"),
        ]
    )
    for candidate in candidates:
        if candidate.is_dir():
            return ["-isysroot", str(candidate.resolve())]
    raise BenchmarkError(
        "no usable macOS SDK found through xcrun, Xcode, or CommandLineTools"
    )


def plugin_path(repo: Path) -> Path:
    suffix = ".dylib" if platform.system() == "Darwin" else ".so"
    return repo / "target" / "release" / f"librust_loop_vectorizer{suffix}"


def parse_custom_remarks(text: str) -> list[dict[str, Any]]:
    records: list[dict[str, Any]] = []
    for line in text.splitlines():
        match = CUSTOM_REMARK_RE.match(line.strip())
        if match is None:
            continue
        fields = match.groupdict()
        records.append(
            {
                "function": fields["function"],
                "loop": fields["loop"],
                "decision": fields["decision"],
                "reason": fields["reason"],
                "vf": int(fields["vf"]),
                "estimated_vector_coverage_percent": int(
                    fields["vector_coverage"]
                ),
                "issued_lane_utilization_percent": int(
                    fields["lane_utilization"]
                ),
                "analysis_us": float(fields["analysis"]),
                "transform_us": float(fields["transform"]),
            }
        )
    return records


def vector_typed_ir_lines(llvm_dis: Path, bitcode: Path, cwd: Path,
                          env: dict[str, str]) -> int:
    completed, _ = run_checked(
        [str(llvm_dis), "-o", "-", str(bitcode)], cwd=cwd, env=env
    )
    return sum(
        1
        for line in completed.stdout.splitlines()
        if VECTOR_TYPE_RE.search(line) is not None
    )


def parse_runtime_output(text: str) -> tuple[dict[str, list[float]], str]:
    samples: dict[str, list[float]] = defaultdict(list)
    status = "missing"
    for line in text.splitlines():
        if not line.strip():
            continue
        try:
            record = json.loads(line)
        except json.JSONDecodeError as error:
            raise BenchmarkError(f"invalid harness JSON line: {line}") from error
        if record.get("kind") == "sample":
            inner = int(record["inner_calls"])
            samples[str(record["kernel"])].append(
                float(record["elapsed_ns"]) / inner
            )
        elif record.get("kind") == "status":
            status = str(record.get("correctness"))
    if status != "ok":
        raise BenchmarkError(f"runtime harness correctness status was {status!r}")
    if not samples:
        raise BenchmarkError("runtime harness emitted no timing samples")
    return dict(samples), status


def write_csv(path: Path, rows: Sequence[dict[str, Any]]) -> None:
    fieldnames = [
        "variant",
        "kernel",
        "median_ns_per_call",
        "p95_ns_per_call",
        "p99_ns_per_call",
        "median_ns_per_element",
        "speedup_vs_scalar",
        "samples",
        "warmups",
        "inner_calls",
        "elements",
        "correctness",
    ]
    with path.open("w", newline="", encoding="utf-8") as destination:
        writer = csv.DictWriter(destination, fieldnames=fieldnames)
        writer.writeheader()
        writer.writerows(rows)


def main() -> int:
    args = parse_args()
    repo = Path(__file__).resolve().parents[1]
    output_dir = (
        args.output_dir.expanduser().resolve()
        if args.output_dir is not None
        else repo / "build" / "benchmark"
    )
    output_dir.mkdir(parents=True, exist_ok=True)

    llvm_prefix, llvm_version = discover_llvm(args.llvm_prefix)
    tools = {
        name: llvm_prefix / "bin" / name
        for name in ("clang", "opt", "llc", "llvm-dis")
    }
    missing = [str(path) for path in tools.values() if not path.is_file()]
    if missing:
        raise BenchmarkError(f"LLVM tool(s) missing: {', '.join(missing)}")

    env = os.environ.copy()
    env["LLVM_SYS_211_PREFIX"] = str(llvm_prefix)
    env["PATH"] = f"{llvm_prefix / 'bin'}{os.pathsep}{env.get('PATH', '')}"
    env["LC_ALL"] = "C"
    sdk = sdk_flags()
    if sdk:
        env["SDKROOT"] = sdk[1]
        explicit_sysroot = command_text(sdk)
        for variable in ("CFLAGS", "CXXFLAGS"):
            existing = env.get(variable, "").strip()
            env[variable] = f"{existing} {explicit_sysroot}".strip()
    plugin = plugin_path(repo)
    build_plugin_ms = 0.0
    if not args.skip_build:
        _, elapsed = run_checked(
            ["cargo", "build", "--release", "--quiet"], cwd=repo, env=env
        )
        build_plugin_ms = elapsed / 1_000_000.0
    if not plugin.is_file():
        raise BenchmarkError(
            f"pass plugin not found at {plugin}; rerun without --skip-build"
        )

    target_completed, _ = run_checked(
        [str(tools["clang"]), *sdk, "-print-target-triple"], cwd=repo, env=env
    )
    target_triple = target_completed.stdout.strip()
    rustc_completed, _ = run_checked(
        ["rustc", "--version"], cwd=repo, env=env
    )
    rustc_version = rustc_completed.stdout.strip()

    compile_flags = [
        *sdk,
        "-std=c11",
        "-O1",
        "-fno-vectorize",
        "-fno-slp-vectorize",
        "-fno-unroll-loops",
        "-ffp-contract=off",
        "-fno-discard-value-names",
        "-emit-llvm",
        "-c",
    ]
    opt_target_flags = [f"--mtriple={target_triple}", "--mcpu=native"]

    canonical = output_dir / "kernels.canonical.scalar.bc"
    canonical_command = [
        str(tools["clang"]),
        *compile_flags,
        str(repo / "benchmarks" / "kernels.c"),
        "-o",
        str(canonical),
    ]
    _, canonical_elapsed = run_checked(canonical_command, cwd=repo, env=env)
    run_checked(
        [str(tools["opt"]), "-passes=verify", "-disable-output", str(canonical)],
        cwd=repo,
        env=env,
    )
    canonical_vector_lines = vector_typed_ir_lines(
        tools["llvm-dis"], canonical, repo, env
    )
    if canonical_vector_lines != 0:
        raise BenchmarkError(
            "canonical input unexpectedly contains vector-typed IR despite both "
            f"vectorizers being disabled ({canonical_vector_lines} lines)"
        )

    variant_bitcode = {
        name: output_dir / f"kernels.{name}.bc" for name in VARIANTS
    }
    custom_remarks_path = output_dir / "kernels.custom.remarks.txt"
    llvm_remarks_path = output_dir / "kernels.llvm.remarks.yaml"
    if llvm_remarks_path.exists():
        llvm_remarks_path.unlink()

    transform_commands: dict[str, list[str]] = {
        "scalar": [
            str(tools["opt"]),
            *opt_target_flags,
            "-passes=verify",
            str(canonical),
            "-o",
            str(variant_bitcode["scalar"]),
        ],
        "custom": [
            str(tools["opt"]),
            *opt_target_flags,
            f"-load-pass-plugin={plugin}",
            "-passes=rust-loop-vectorize-report,verify",
            str(canonical),
            "-o",
            str(variant_bitcode["custom"]),
        ],
        "llvm": [
            str(tools["opt"]),
            *opt_target_flags,
            "--pass-remarks=loop-vectorize",
            "--pass-remarks-missed=loop-vectorize",
            "--pass-remarks-analysis=loop-vectorize",
            f"--pass-remarks-output={llvm_remarks_path}",
            "-passes=loop-vectorize,verify",
            str(canonical),
            "-o",
            str(variant_bitcode["llvm"]),
        ],
    }
    transform_ms: dict[str, float] = {}
    custom_variant_records: list[dict[str, Any]] = []
    for variant in VARIANTS:
        completed, elapsed = run_checked(
            transform_commands[variant], cwd=repo, env=env
        )
        transform_ms[variant] = elapsed / 1_000_000.0
        if variant == "custom":
            custom_remarks_path.write_text(completed.stderr, encoding="utf-8")
            custom_variant_records = parse_custom_remarks(completed.stderr)
        run_checked(
            [
                str(tools["opt"]),
                "-passes=verify",
                "-disable-output",
                str(variant_bitcode[variant]),
            ],
            cwd=repo,
            env=env,
        )

    vector_lines = {
        variant: vector_typed_ir_lines(
            tools["llvm-dis"], variant_bitcode[variant], repo, env
        )
        for variant in VARIANTS
    }
    if vector_lines["scalar"] != 0:
        raise BenchmarkError("scalar comparison arm contains vector-typed IR")
    if vector_lines["custom"] == 0:
        raise BenchmarkError("custom pass produced no vector-typed IR")
    if vector_lines["llvm"] == 0:
        raise BenchmarkError("LLVM LoopVectorize produced no vector-typed IR")

    llvm_remarks = (
        llvm_remarks_path.read_text(encoding="utf-8")
        if llvm_remarks_path.exists()
        else ""
    )
    llvm_remark_counts = {
        "passed": llvm_remarks.count("--- !Passed"),
        "missed": llvm_remarks.count("--- !Missed"),
        "analysis": llvm_remarks.count("--- !Analysis"),
    }

    harness_object = output_dir / "harness.o"
    harness_command = [
        str(tools["clang"]),
        *sdk,
        "-std=c11",
        "-O2",
        "-fno-vectorize",
        "-fno-slp-vectorize",
        "-fno-lto",
        "-c",
        str(repo / "benchmarks" / "harness.c"),
        "-o",
        str(harness_object),
    ]
    _, harness_elapsed = run_checked(harness_command, cwd=repo, env=env)

    executables: dict[str, Path] = {}
    codegen_ms: dict[str, float] = {}
    link_ms: dict[str, float] = {}
    codegen_commands: dict[str, list[str]] = {}
    link_commands: dict[str, list[str]] = {}
    for variant in VARIANTS:
        kernel_object = output_dir / f"kernels.{variant}.o"
        executable = output_dir / f"runtime-{variant}"
        codegen_command = [
            str(tools["llc"]),
            "-O=3",
            "--mcpu=native",
            "--filetype=obj",
            str(variant_bitcode[variant]),
            "-o",
            str(kernel_object),
        ]
        _, elapsed = run_checked(codegen_command, cwd=repo, env=env)
        codegen_ms[variant] = elapsed / 1_000_000.0
        link_command = [
            str(tools["clang"]),
            *sdk,
            "-fno-lto",
            str(harness_object),
            str(kernel_object),
            "-o",
            str(executable),
        ]
        _, elapsed = run_checked(link_command, cwd=repo, env=env)
        link_ms[variant] = elapsed / 1_000_000.0
        codegen_commands[variant] = codegen_command
        link_commands[variant] = link_command
        executables[variant] = executable

    runtime_order = list(VARIANTS)
    random.Random(0x5EED).shuffle(runtime_order)
    runtime_samples: dict[str, dict[str, list[float]]] = {}
    runtime_process_ms: dict[str, float] = {}
    runtime_status: dict[str, str] = {}
    for variant in runtime_order:
        completed, elapsed = run_checked(
            [
                str(executables[variant]),
                str(args.elements),
                str(args.warmups),
                str(args.samples),
                str(args.inner_calls),
            ],
            cwd=repo,
            env=env,
        )
        samples, status = parse_runtime_output(completed.stdout)
        runtime_samples[variant] = samples
        runtime_status[variant] = status
        runtime_process_ms[variant] = elapsed / 1_000_000.0

    kernel_names = sorted(runtime_samples["scalar"])
    for variant in VARIANTS:
        if sorted(runtime_samples[variant]) != kernel_names:
            raise BenchmarkError(f"runtime kernel set differs for {variant}")
        for kernel in kernel_names:
            if len(runtime_samples[variant][kernel]) != args.samples:
                raise BenchmarkError(
                    f"expected {args.samples} samples for {variant}/{kernel}, got "
                    f"{len(runtime_samples[variant][kernel])}"
                )

    runtime_rows: list[dict[str, Any]] = []
    overall_speedups: dict[str, float] = {}
    for variant in VARIANTS:
        speedups: list[float] = []
        for kernel in kernel_names:
            scalar_median = statistics.median(runtime_samples["scalar"][kernel])
            stats = distribution(runtime_samples[variant][kernel])
            median_ns = float(stats["p50"])
            speedup = scalar_median / median_ns
            speedups.append(speedup)
            runtime_rows.append(
                {
                    "variant": variant,
                    "kernel": kernel,
                    "median_ns_per_call": round(median_ns, 3),
                    "p95_ns_per_call": round(float(stats["p95"]), 3),
                    "p99_ns_per_call": round(float(stats["p99"]), 3),
                    "median_ns_per_element": round(
                        median_ns / args.elements, 6
                    ),
                    "speedup_vs_scalar": round(speedup, 6),
                    "samples": args.samples,
                    "warmups": args.warmups,
                    "inner_calls": args.inner_calls,
                    "elements": args.elements,
                    "correctness": runtime_status[variant],
                }
            )
        overall_speedups[variant] = math.exp(
            statistics.fmean(math.log(value) for value in speedups)
        )

    clones_canonical = output_dir / "clones.canonical.scalar.bc"
    clones_compile_command = [
        str(tools["clang"]),
        *compile_flags,
        str(repo / "benchmarks" / "cloned_kernels.c"),
        "-o",
        str(clones_canonical),
    ]
    _, clones_compile_elapsed = run_checked(
        clones_compile_command, cwd=repo, env=env
    )
    run_checked(
        [
            str(tools["opt"]),
            "-passes=verify",
            "-disable-output",
            str(clones_canonical),
        ],
        cwd=repo,
        env=env,
    )
    clone_vector_lines = vector_typed_ir_lines(
        tools["llvm-dis"], clones_canonical, repo, env
    )
    if clone_vector_lines != 0:
        raise BenchmarkError("cloned-loop canonical input contains vector-typed IR")

    clone_report_command = [
        str(tools["opt"]),
        *opt_target_flags,
        f"-load-pass-plugin={plugin}",
        "-passes=rust-loop-vectorize-report,verify",
        "-disable-output",
        str(clones_canonical),
    ]
    clone_report, clone_report_elapsed = run_checked(
        clone_report_command, cwd=repo, env=env
    )
    clone_remarks_path = output_dir / "clones.custom.remarks.txt"
    clone_remarks_path.write_text(clone_report.stderr, encoding="utf-8")
    clone_records = parse_custom_remarks(clone_report.stderr)
    if len(clone_records) < EXPECTED_CLONE_LOOPS // 2:
        raise BenchmarkError(
            f"only {len(clone_records)} cloned loops produced timing remarks; "
            f"expected approximately {EXPECTED_CLONE_LOOPS}"
        )
    vectorized_clone_records = [
        record for record in clone_records if record["decision"] == "vectorized"
    ]
    if not vectorized_clone_records:
        raise BenchmarkError("none of the cloned loops was transformed")

    process_commands: dict[str, list[str]] = {
        "no_op_verify": [
            str(tools["opt"]),
            *opt_target_flags,
            "-passes=verify",
            "-disable-output",
            str(clones_canonical),
        ],
        "plugin_load_no_op_verify": [
            str(tools["opt"]),
            *opt_target_flags,
            f"-load-pass-plugin={plugin}",
            "-passes=verify",
            "-disable-output",
            str(clones_canonical),
        ],
        "custom_pass_verify": [
            str(tools["opt"]),
            *opt_target_flags,
            f"-load-pass-plugin={plugin}",
            "-passes=rust-loop-vectorize,verify",
            "-disable-output",
            str(clones_canonical),
        ],
        "llvm_loop_vectorize_verify": [
            str(tools["opt"]),
            *opt_target_flags,
            "-passes=loop-vectorize,verify",
            "-disable-output",
            str(clones_canonical),
        ],
    }
    for command in process_commands.values():
        run_checked(command, cwd=repo, env=env)

    process_timings_us: dict[str, list[float]] = {
        name: [] for name in process_commands
    }
    process_rng = random.Random(0xC011EC7)
    for _ in range(args.timing_runs):
        names = list(process_commands)
        process_rng.shuffle(names)
        for name in names:
            _, elapsed = run_checked(process_commands[name], cwd=repo, env=env)
            process_timings_us[name].append(elapsed / 1000.0)

    time_passes_files: dict[str, str] = {}
    for name, command in process_commands.items():
        timed_command = [command[0], "--time-passes", *command[1:]]
        completed, _ = run_checked(timed_command, cwd=repo, env=env)
        timing_path = output_dir / f"clones.{name}.time-passes.txt"
        timing_path.write_text(completed.stderr, encoding="utf-8")
        time_passes_files[name] = str(timing_path)

    process_stats = {
        name: distribution(values) for name, values in process_timings_us.items()
    }
    no_op_median = float(process_stats["no_op_verify"]["p50"])
    plugin_no_op_median = float(
        process_stats["plugin_load_no_op_verify"]["p50"]
    )
    custom_median = float(process_stats["custom_pass_verify"]["p50"])
    llvm_median = float(process_stats["llvm_loop_vectorize_verify"]["p50"])
    clone_count = len(clone_records)

    caveats = [
        "Runtime results are cache-hot kernel microbenchmarks, not application-level speedups.",
        "The script performs warmup and repeated high-resolution measurements but does not disable ASLR, frequency scaling, Turbo Boost, background services, or pin CPU affinity.",
        "Custom analysis_us/transform_us values come from in-pass Rust timers; they exclude opt startup, bitcode parsing, plugin loading, verification, serialization, and remark I/O.",
        "Process-level times include process startup, parsing, target setup, and verification. Baseline subtraction is an approximate per-loop estimate and can be distorted by fixed costs and noise.",
        "The 128 cloned loops are deliberately small, isomorphic, and bounded; their latency distribution cannot establish a universal microsecond bound for arbitrary LLVM IR.",
        "LLVM and custom variants share byte-identical input bitcode and identical backend/link flags, but each transformation may expose different downstream code-generation opportunities.",
        "Estimated vector coverage and issued-lane utilization are static compiler estimates; neither is equivalent to active hardware-lane occupancy or measured speedup.",
    ]

    report = {
        "schema_version": 2,
        "generated_at_utc": datetime.now(timezone.utc).isoformat(),
        "host": {
            "system": platform.system(),
            "release": platform.release(),
            "machine": platform.machine(),
            "python": platform.python_version(),
        },
        "toolchain": {
            "llvm_prefix": str(llvm_prefix),
            "llvm_version": llvm_version,
            "rustc_version": rustc_version,
            "target_triple": target_triple,
            "plugin": str(plugin),
            "plugin_sha256": sha256(plugin),
        },
        "configuration": {
            "elements": args.elements,
            "warmups": args.warmups,
            "samples": args.samples,
            "inner_calls": args.inner_calls,
            "timing_runs": args.timing_runs,
            "runtime_execution_order": runtime_order,
            "canonical_compile_flags": compile_flags,
            "common_backend_flags": ["-O=3", "--mcpu=native", "--filetype=obj"],
            "common_link_flags": [*sdk, "-fno-lto"],
        },
        "canonical_ir": {
            "path": str(canonical),
            "sha256": sha256(canonical),
            "vector_typed_ir_lines": canonical_vector_lines,
            "compiled_once": True,
            "command": canonical_command,
        },
        "variants": {
            variant: {
                "bitcode": str(variant_bitcode[variant]),
                "bitcode_sha256": sha256(variant_bitcode[variant]),
                "input_bitcode_sha256": sha256(canonical),
                "transform_command": transform_commands[variant],
                "vector_typed_ir_lines": vector_lines[variant],
                "correctness": runtime_status[variant],
            }
            for variant in VARIANTS
        },
        "vectorization_decisions": {
            "custom": {
                "loops_reported": len(custom_variant_records),
                "vectorized": sum(
                    record["decision"] == "vectorized"
                    for record in custom_variant_records
                ),
                "rejected": sum(
                    record["decision"] != "vectorized"
                    for record in custom_variant_records
                ),
                "estimated_vector_coverage_percent": distribution(
                    record["estimated_vector_coverage_percent"]
                    for record in custom_variant_records
                ),
                "issued_lane_utilization_percent": distribution(
                    record["issued_lane_utilization_percent"]
                    for record in custom_variant_records
                ),
                "remarks_file": str(custom_remarks_path),
            },
            "llvm": {
                **llvm_remark_counts,
                "remarks_file": str(llvm_remarks_path),
            },
        },
        "runtime": {
            "unit": "nanoseconds per kernel call",
            "rows": runtime_rows,
            "geomean_speedup_vs_scalar": {
                name: round(value, 6)
                for name, value in overall_speedups.items()
            },
            "harness_process_ms": runtime_process_ms,
        },
        "build_time": {
            "unit": "milliseconds",
            "plugin_build": build_plugin_ms,
            "canonical_c_to_scalar_ir": canonical_elapsed / 1_000_000.0,
            "harness_compile": harness_elapsed / 1_000_000.0,
            "variant_ir_transform_and_serialize": transform_ms,
            "variant_backend_codegen": codegen_ms,
            "variant_link": link_ms,
            "clone_c_to_scalar_ir": clones_compile_elapsed / 1_000_000.0,
        },
        "cloned_loop_compile_time": {
            "expected_source_loops": EXPECTED_CLONE_LOOPS,
            "loops_reported": clone_count,
            "loops_vectorized": len(vectorized_clone_records),
            "reporting_run_process_us": clone_report_elapsed / 1000.0,
            "custom_internal_per_loop_us": {
                "analysis": distribution(
                    record["analysis_us"] for record in clone_records
                ),
                "transform_vectorized_only": distribution(
                    record["transform_us"] for record in vectorized_clone_records
                ),
                "analysis_plus_transform": distribution(
                    record["analysis_us"] + record["transform_us"]
                    for record in clone_records
                ),
                "sum": sum(
                    record["analysis_us"] + record["transform_us"]
                    for record in clone_records
                ),
            },
            "custom_reported_simd_metrics_percent": {
                "estimated_vector_coverage": distribution(
                    record["estimated_vector_coverage_percent"]
                    for record in clone_records
                ),
                "issued_lane_utilization": distribution(
                    record["issued_lane_utilization_percent"]
                    for record in clone_records
                ),
            },
            "process_wall_time_us": process_stats,
            "baseline_adjusted_median_us_per_reported_loop": {
                "custom_minus_plugin_load_no_op": max(
                    custom_median - plugin_no_op_median, 0.0
                )
                / clone_count,
                "llvm_minus_no_op": max(llvm_median - no_op_median, 0.0)
                / clone_count,
            },
            "custom_remarks_file": str(clone_remarks_path),
            "time_passes_reports": time_passes_files,
            "commands": process_commands,
        },
        "commands": {
            "harness_compile": harness_command,
            "codegen": codegen_commands,
            "link": link_commands,
            "clone_compile": clones_compile_command,
            "clone_report": clone_report_command,
        },
        "caveats": caveats,
    }

    json_path = output_dir / "results.json"
    csv_path = output_dir / "runtime.csv"
    json_path.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    write_csv(csv_path, runtime_rows)

    print(f"LLVM {llvm_version}; target {target_triple}")
    print(f"Canonical scalar IR: {canonical} (sha256 {sha256(canonical)[:12]}...)")
    print("Runtime medians (microseconds/call; speedup vs scalar):")
    row_by_key = {
        (row["variant"], row["kernel"]): row for row in runtime_rows
    }
    print(f"{'kernel':<20} {'scalar':>12} {'custom':>20} {'LLVM':>20}")
    for kernel in kernel_names:
        scalar_us = row_by_key[("scalar", kernel)]["median_ns_per_call"] / 1000.0
        custom_row = row_by_key[("custom", kernel)]
        llvm_row = row_by_key[("llvm", kernel)]
        print(
            f"{kernel:<20} {scalar_us:>12.3f} "
            f"{custom_row['median_ns_per_call'] / 1000.0:>10.3f} "
            f"({custom_row['speedup_vs_scalar']:>6.3f}x) "
            f"{llvm_row['median_ns_per_call'] / 1000.0:>10.3f} "
            f"({llvm_row['speedup_vs_scalar']:>6.3f}x)"
        )
    print(
        "Geomean speedup: "
        f"custom={overall_speedups['custom']:.3f}x, "
        f"LLVM={overall_speedups['llvm']:.3f}x"
    )

    internal = report["cloned_loop_compile_time"]["custom_internal_per_loop_us"]
    print(
        f"Custom cloned-loop timings ({clone_count} reported, "
        f"{len(vectorized_clone_records)} vectorized), microseconds:"
    )
    for name in ("analysis", "transform_vectorized_only", "analysis_plus_transform"):
        stats = internal[name]
        print(
            f"  {name}: p50={stats['p50']:.3f} "
            f"p95={stats['p95']:.3f} p99={stats['p99']:.3f}"
        )
    simd_metrics = report["cloned_loop_compile_time"][
        "custom_reported_simd_metrics_percent"
    ]
    print("Custom cloned-loop static SIMD estimates, percent:")
    for name in ("estimated_vector_coverage", "issued_lane_utilization"):
        stats = simd_metrics[name]
        print(
            f"  {name}: p50={stats['p50']:.1f} "
            f"p95={stats['p95']:.1f} p99={stats['p99']:.1f}"
        )
    print("Cloned-loop opt process wall time, milliseconds:")
    for name, stats in process_stats.items():
        print(
            f"  {name}: p50={stats['p50'] / 1000.0:.3f} "
            f"p95={stats['p95'] / 1000.0:.3f} "
            f"p99={stats['p99'] / 1000.0:.3f}"
        )
    adjusted = report["cloned_loop_compile_time"][
        "baseline_adjusted_median_us_per_reported_loop"
    ]
    print(
        "Approximate baseline-adjusted median per loop: "
        f"custom={adjusted['custom_minus_plugin_load_no_op']:.3f} us, "
        f"LLVM={adjusted['llvm_minus_no_op']:.3f} us"
    )
    print(f"Reports: {json_path} and {csv_path}")
    print("Caveats:")
    for caveat in caveats:
        print(f"  - {caveat}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except BenchmarkError as error:
        print(f"benchmark error: {error}", file=sys.stderr)
        raise SystemExit(1) from error
