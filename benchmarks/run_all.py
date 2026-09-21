#!/usr/bin/env python3
"""Regenerate every table the paper uses, in one command.

Usage: python3 benchmarks/run_all.py [--out DIR] [--no-typescript] [--skip NAME ...]
Set ORCHESTRATE to the compiler binary; the default builds and uses target/release/orchestrate.

Runs, in order:
  benchmarks/landline_latency/run.py   the boundary ladder (payload, call-rate, callers, state)
  benchmarks/tick_cost/run.py          the library-mode tick cost
  benchmarks/diagnostics_coverage.py   how many invalid programs `orchestrate check` rejects

Each writes CSV, JSON, and Markdown under --out (default benchmarks/results/), with an
environment header. This script then writes results.md there, the three Markdown
reports in one file, and prints where everything went. Run it on an idle machine: the
first two are timing benchmarks.
"""
import argparse
import os
import pathlib
import subprocess
import sys

REPO = pathlib.Path(__file__).resolve().parent.parent
STEPS = [
    ("boundary_ladder", "benchmarks/landline_latency/run.py"),
    ("tick_cost", "benchmarks/tick_cost/run.py"),
    ("diagnostics_coverage", "benchmarks/diagnostics_coverage.py"),
]


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", default=str(REPO / "benchmarks" / "results"))
    parser.add_argument("--no-typescript", action="store_true", help="skip the TypeScript rung of the ladder")
    parser.add_argument("--skip", nargs="*", default=[], choices=[name for name, _ in STEPS], help="steps to leave out")
    args = parser.parse_args()

    orchestrate = os.environ.get("ORCHESTRATE")
    if not orchestrate:
        built = subprocess.run(["cargo", "build", "--release", "--quiet"], cwd=REPO)
        if built.returncode != 0:
            sys.exit("cargo build --release failed")
        orchestrate = str(REPO / "target" / "release" / "orchestrate")
    env = dict(os.environ, ORCHESTRATE=orchestrate)
    out = pathlib.Path(args.out)
    out.mkdir(parents=True, exist_ok=True)

    for name, script in STEPS:
        if name in args.skip:
            print(f"skipping {name}")
            continue
        command = [sys.executable, str(REPO / script), "--out", str(out)]
        if name == "boundary_ladder" and args.no_typescript:
            command.append("--no-typescript")
        print(f"== {name}: {' '.join(command[1:])}", flush=True)
        result = subprocess.run(command, cwd=REPO, env=env)
        if result.returncode != 0:
            sys.exit(f"{name} failed")

    reports = [out / f"{name}.md" for name, _ in STEPS if (out / f"{name}.md").exists()]
    combined = "\n\n".join(p.read_text().rstrip() for p in reports) + "\n"
    (out / "results.md").write_text(combined)
    print(f"\nall tables regenerated under {out}:")
    for p in sorted(out.iterdir()):
        print(f"  {p.name}")


if __name__ == "__main__":
    main()
