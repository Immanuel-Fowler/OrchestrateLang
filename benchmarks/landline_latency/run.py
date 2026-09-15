#!/usr/bin/env python3
"""Build the landline latency benchmark in release mode, run it, and print percentiles.

Usage: python3 benchmarks/landline_latency/run.py
Set ORCHESTRATE to the compiler binary (default: `orchestrate` on PATH).
"""
import collections
import os
import pathlib
import platform
import subprocess
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parent


def percentile(sorted_values, fraction):
    return sorted_values[min(len(sorted_values) - 1, int(fraction * len(sorted_values)))]


def main():
    orchestrate = os.environ.get("ORCHESTRATE", "orchestrate")
    with tempfile.TemporaryDirectory() as tmp:
        binary = pathlib.Path(tmp) / "landline_latency"
        subprocess.run([orchestrate, "build", str(HERE / "main.orch"), "-o", str(binary)], check=True)
        result = subprocess.run([str(binary)], capture_output=True, text=True)
    if result.returncode != 0:
        sys.stderr.write(result.stderr)
        sys.exit(result.returncode)

    samples = collections.OrderedDict()
    for line in result.stdout.splitlines():
        parts = line.split(",")
        if len(parts) == 3 and parts[2].isdigit():
            samples.setdefault((parts[0], parts[1]), []).append(int(parts[2]))
    if not samples:
        sys.exit("no samples in benchmark output")

    print(f"Round-trip latency in microseconds ({platform.system()} {platform.machine()}, "
          f"Python {platform.python_version()})")
    print()
    print("| Serverlet | Payload | Samples | p50 | p90 | p99 | max |")
    print("|---|---|---:|---:|---:|---:|---:|")
    for (kind, payload), values in samples.items():
        values.sort()
        print(f"| {kind} | {payload} | {len(values)} | {percentile(values, 0.5)} | "
              f"{percentile(values, 0.9)} | {percentile(values, 0.99)} | {values[-1]} |")


if __name__ == "__main__":
    main()
