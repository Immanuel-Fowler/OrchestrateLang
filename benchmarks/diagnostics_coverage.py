#!/usr/bin/env python3
"""Run the diagnostics corpus through `orchestrate check` and `orchestrate build`, and
write how many invalid programs each one rejects.

Usage: python3 benchmarks/diagnostics_coverage.py [--out DIR] [--no-build]
Set ORCHESTRATE to the compiler binary (default: `orchestrate` on PATH).

Every `.orch` file at the top of tests/error_cases/diagnostics/ is a program that must
be rejected. For each, this records whether `check` rejects it, and, when it does not,
whether `build` rejects it before Cargo runs, after Cargo runs without a rustc error code,
or by leaking a rustc error against generated code. The counts are the ones the paper
states; the per-case rows are in the CSV and JSON.

Writes diagnostics_coverage.csv, diagnostics_coverage.json, and diagnostics_coverage.md
under --out (default benchmarks/results/). `--no-build` classifies with `check` only,
which takes a second; the build pass compiles every program `check` accepts.
"""
import argparse
import collections
import datetime
import json
import os
import pathlib
import platform
import re
import subprocess
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parent
CORPUS = REPO / "tests" / "error_cases" / "diagnostics"


def run_text(args, **kwargs):
    try:
        result = subprocess.run(args, capture_output=True, text=True, timeout=60, **kwargs)
    except (OSError, subprocess.TimeoutExpired):
        return None
    return (result.stdout or result.stderr).strip() if result.returncode == 0 else None


def git(*args):
    return run_text(["git", *args], cwd=REPO)


def first_error_line(text):
    for line in text.splitlines():
        line = line.strip()
        if line and not line.startswith("[orchestrate]"):
            return line[:200]
    return ""


def classify(orchestrate, case, build):
    check = subprocess.run([orchestrate, "check", str(case)], capture_output=True, text=True, cwd=REPO)
    combined = check.stdout + check.stderr
    if check.returncode != 0:
        return "check", first_error_line(combined)
    if not build:
        return "accepted-by-check", ""
    with tempfile.TemporaryDirectory() as tmp:
        built = subprocess.run([orchestrate, "build", str(case), "-o", str(pathlib.Path(tmp) / "probe")],
                               capture_output=True, text=True, cwd=REPO)
    combined = built.stdout + built.stderr
    if built.returncode == 0:
        return "accepted", ""
    if "error[E" in combined or "rustc --explain" in combined:
        match = re.search(r"error\[E\d+\][^\n]*", combined)
        return "rustc", (match.group(0) if match else "")[:200]
    if "Cargo compilation failed" in combined:
        return "cargo", first_error_line("\n".join(l for l in combined.splitlines() if "error" in l.lower()))
    return "build", first_error_line(combined)


def markdown(env, rows, build):
    total = len(rows)
    by = collections.Counter(r["rejected_by"] for r in rows)
    lines = ["## Diagnostics coverage", "",
             f"{total} deliberately invalid programs in `tests/error_cases/diagnostics/`, run on "
             f"{env['timestamp_utc']} at commit `{(env['commit'] or 'unknown')[:12]}` with {env['orchestrate']}.", "",
             "| Rejected by | Programs | Meaning |", "|---|---:|---|",
             f"| `orchestrate check` | {by['check']} | the typechecker's own error, before any code is generated |"]
    if build:
        lines += [f"| build, before Cargo | {by['build']} | codegen or the driver, still the compiler's own error |",
                  f"| Cargo, no rustc code | {by['cargo']} | a `compile_error!` the generator planted, reported through Cargo |",
                  f"| rustc | {by['rustc']} | leaked: a rustc error against generated code |",
                  f"| nothing | {by['accepted']} | wrongly accepted |"]
    else:
        lines += [f"| not by `check` | {by['accepted-by-check']} | classified no further (`--no-build`) |"]
    lines += ["", f"**{by['check']} of {total} invalid programs are rejected by `orchestrate check`.**"]
    if build:
        own = by["check"] + by["build"]
        lines += [f"{own} of {total} are rejected as the compiler's own error before Cargo runs; "
                  f"{by['cargo']} more are rejected through Cargo without a rustc error code; "
                  f"{by['rustc']} leak a rustc error; {by['accepted']} are wrongly accepted."]
    lines += ["", "| Case | Rejected by | Message |", "|---|---|---|"]
    for r in rows:
        lines.append(f"| `{r['case']}` | {r['rejected_by']} | {r['message'].replace('|', '\\|')} |")
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", default=str(REPO / "benchmarks" / "results"))
    parser.add_argument("--no-build", action="store_true", help="classify with `check` only")
    args = parser.parse_args()
    orchestrate = os.environ.get("ORCHESTRATE", "orchestrate")
    cases = sorted(p for p in CORPUS.glob("*.orch"))
    if not cases:
        sys.exit(f"no cases under {CORPUS}")
    rows = []
    for case in cases:
        rejected_by, message = classify(orchestrate, case, not args.no_build)
        rows.append(collections.OrderedDict([("case", case.stem), ("rejected_by", rejected_by), ("message", message)]))
    help_text = run_text([orchestrate, "--help"]) or ""
    env = collections.OrderedDict([
        ("benchmark", "diagnostics_coverage"),
        ("timestamp_utc", datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")),
        ("commit", git("rev-parse", "HEAD")),
        ("dirty", bool(git("status", "--porcelain"))),
        ("os", platform.platform()),
        ("orchestrate", help_text.splitlines()[0] if help_text else None),
        ("rustc", run_text(["rustc", "--version"])),
        ("build_pass", not args.no_build),
        ("corpus", str(CORPUS.relative_to(REPO))),
    ])
    out = pathlib.Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    with open(out / "diagnostics_coverage.csv", "w") as f:
        f.write("case,rejected_by,message\n")
        for r in rows:
            f.write(f"{r['case']},{r['rejected_by']},\"{r['message'].replace(chr(34), chr(39))}\"\n")
    counts = collections.Counter(r["rejected_by"] for r in rows)
    with open(out / "diagnostics_coverage.json", "w") as f:
        json.dump({"environment": env, "counts": dict(counts), "total": len(rows), "results": rows}, f, indent=2)
        f.write("\n")
    text = markdown(env, rows, not args.no_build)
    (out / "diagnostics_coverage.md").write_text(text)
    print(text)
    print(f"written to {out}")


if __name__ == "__main__":
    main()
