#!/usr/bin/env python3
"""Build the boundary ladder benchmark in release mode, run it, and write the results.

Usage: python3 benchmarks/landline_latency/run.py [--out DIR] [--no-typescript]
Set ORCHESTRATE to the compiler binary (default: `orchestrate` on PATH).

Writes, under --out (default benchmarks/results/):
  boundary_ladder_samples.csv   every measured sample: kind,payload,rate,value
  boundary_ladder.csv           one row per case: percentiles and the unit
  boundary_ladder.json          the same rows plus the environment header
  boundary_ladder.md            the tables the README and the paper paste in
The Markdown is also printed. Nothing here is retyped: the tables come from the JSON.
"""
import argparse
import collections
import datetime
import json
import os
import pathlib
import platform
import re
import shutil
import subprocess
import sys
import tempfile

HERE = pathlib.Path(__file__).resolve().parent
REPO = HERE.parent.parent
KIND_ORDER = ["local-let", "shared-let", "loop-only", "in-process", "sandbox", "secret", "python", "typescript"]
PAYLOAD_ORDER = ["int", "string-1KB", "int[1000]", "statement"]
RATE_ORDER = ["unpaced", "100hz", "1khz", "10khz", "callers-1", "callers-2", "callers-4", "callers-8", "batch-1000"]


def percentile(sorted_values, fraction):
    return sorted_values[min(len(sorted_values) - 1, int(fraction * len(sorted_values)))]


def run_text(args, **kwargs):
    """stdout of a command, or None when it cannot run."""
    try:
        result = subprocess.run(args, capture_output=True, text=True, timeout=60, **kwargs)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if result.returncode != 0:
        return None
    return (result.stdout or result.stderr).strip()


def tool_version(env_var, default):
    program = os.environ.get(env_var, default)
    text = run_text([program, "--version"])
    return text.splitlines()[0] if text else None


def cpu_name():
    if sys.platform == "darwin":
        text = run_text(["sysctl", "-n", "machdep.cpu.brand_string"])
        if text:
            return text
    if sys.platform.startswith("linux"):
        try:
            for line in open("/proc/cpuinfo"):
                if line.lower().startswith("model name"):
                    return line.split(":", 1)[1].strip()
        except OSError:
            pass
    return platform.processor() or platform.machine()


def git(*args):
    return run_text(["git", *args], cwd=REPO)


def wasmtime_version(cache_dir):
    lock = cache_dir / "Cargo.lock"
    if not lock.exists():
        return None
    text = lock.read_text()
    match = re.search(r'name = "wasmtime"\nversion = "([^"]+)"', text)
    return match.group(1) if match else None


def counts(source):
    """The warm-up and total constants the program declares, so the header states them."""
    found = {}
    for name in ("warmup", "total", "slow_warmup", "slow_total", "mid_warmup", "mid_total", "callers_warmup", "callers_total"):
        match = re.search(rf"let {name} = (\d+)", source)
        if match:
            found[name] = int(match.group(1))
    return found


def environment(orchestrate, cache_dir, typescript, backend_lines, source):
    return collections.OrderedDict([
        ("benchmark", "boundary_ladder"),
        ("timestamp_utc", datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")),
        ("commit", git("rev-parse", "HEAD")),
        ("dirty", bool(git("status", "--porcelain"))),
        ("cpu", cpu_name()),
        ("cpu_count", os.cpu_count()),
        ("os", platform.platform()),
        ("arch", platform.machine()),
        ("orchestrate", (run_text([orchestrate, "--help"]) or "").splitlines()[0] if run_text([orchestrate, "--help"]) else None),
        ("rustc", run_text(["rustc", "--version"])),
        ("cargo", run_text(["cargo", "--version"])),
        ("profile", "release"),
        ("python", platform.python_version()),
        ("landline_python", tool_version("ORCH_PYTHON", "python3")),
        ("typescript", typescript),
        ("tsc", tool_version("ORCH_TSC", "tsc")),
        ("bun", tool_version("ORCH_BUN", "bun")),
        ("scriptc", tool_version("ORCH_SCRIPTC", "scriptc")),
        ("typescript_backend", backend_lines),
        ("wasmtime", wasmtime_version(cache_dir)),
        ("counts", counts(source)),
    ])


def strip_typescript(source):
    """The program without its TypeScript rung, for a machine without Bun or TypeScript 7."""
    lines = source.splitlines(keepends=True)
    kept, skipping = [], False
    for line in lines:
        stripped = line.strip()
        if stripped == "// @typescript-start":
            skipping = True
            continue
        if stripped == "// @typescript-end":
            skipping = False
            continue
        if not skipping:
            kept.append(line)
    return "".join(kept)


def typescript_available():
    return bool(tool_version("ORCH_TSC", "tsc")) and bool(tool_version("ORCH_BUN", "bun"))


def unit_for(payload):
    return "ns/statement" if payload == "statement" else "us"


def sort_key(row):
    def rank(order, value):
        return order.index(value) if value in order else len(order)
    return (rank(KIND_ORDER, row["kind"]), rank(PAYLOAD_ORDER, row["payload"]), rank(RATE_ORDER, row["rate"]))


def markdown(env, rows):
    def table(title, note, selected, columns):
        out = [f"### {title}", "", note, "", "| " + " | ".join(columns) + " | Samples | p50 | p90 | p99 | max |",
               "|" + "|".join(["---"] * len(columns)) + "|---:|---:|---:|---:|---:|"]
        for r in selected:
            cells = [str(r[c.lower()]) for c in columns]
            out.append(f"| {' | '.join(cells)} | {r['samples']} | {r['p50']} | {r['p90']} | {r['p99']} | {r['max']} |")
        return "\n".join(out) + "\n"

    header = ["## Boundary ladder", "",
              f"One run on {env['cpu']} ({env['os']}, {env['arch']}), {env['timestamp_utc']}, commit "
              f"`{(env['commit'] or 'unknown')[:12]}`{' (dirty tree)' if env['dirty'] else ''}. "
              f"{env['rustc']}; Python {env['python']}; wasmtime {env['wasmtime'] or 'n/a'}; "
              f"TypeScript rung {'included' if env['typescript'] else 'skipped: Bun or TypeScript 7 not found'}. "
              f"Round trips in microseconds unless the unit says otherwise; the state rungs are nanoseconds per statement. "
              f"Your numbers will differ.", ""]
    payload = [r for r in rows if r["rate"] == "unpaced" and r["payload"] != "statement"]
    rate = [r for r in rows if r["payload"] == "int" and r["rate"] in ("unpaced", "100hz", "1khz", "10khz")]
    callers = [r for r in rows if r["payload"] == "int" and (r["rate"].startswith("callers-") or r["rate"] == "unpaced")]
    state = [r for r in rows if r["payload"] == "statement"]
    for r in callers:
        r = r
    def with_callers_label(r):
        c = dict(r)
        if c["rate"] == "unpaced":
            c["rate"] = "callers-1"
        return c
    callers = [with_callers_label(r) for r in callers]
    callers.sort(key=lambda r: (KIND_ORDER.index(r["kind"]) if r["kind"] in KIND_ORDER else 99, int(r["rate"].split("-")[1])))
    parts = header
    parts.append(table("Payload sweep", "Calls back to back, one caller.", payload, ["Kind", "Payload"]))
    parts.append(table("Call-rate sweep", "The `int` payload. At 100 Hz and 1 kHz the caller sleeps between calls; at 10 kHz it spins.", rate, ["Kind", "Rate"]))
    parts.append(table("Concurrent callers", "The `int` payload with 1, 2, 4, or 8 callers sharing one serverlet; each sample is one caller's round trip.", callers, ["Kind", "Rate"]))
    parts.append(table("State rungs", "Nanoseconds per statement, each sample a batch of 1,000 statements; `loop-only` is the counting loop by itself.", state, ["Kind"]))
    return "\n".join(parts)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", default=str(REPO / "benchmarks" / "results"), help="where to write the results")
    parser.add_argument("--no-typescript", action="store_true", help="skip the TypeScript landline rung")
    args = parser.parse_args()

    orchestrate = os.environ.get("ORCHESTRATE", "orchestrate")
    source = (HERE / "main.orch").read_text()
    typescript = not args.no_typescript and typescript_available()
    if typescript:
        program = HERE / "main.orch"
    else:
        # A sibling of main.orch, so `source: "echo.py"` still resolves beside it.
        program = HERE / "main_no_typescript.orch"
        program.write_text(strip_typescript(source))
        print("TypeScript rung skipped: Bun or TypeScript 7 not found (set ORCH_TSC / ORCH_BUN)", file=sys.stderr)

    with tempfile.TemporaryDirectory() as tmp:
        binary = pathlib.Path(tmp) / "landline_latency"
        build = subprocess.run([orchestrate, "build", str(program), "-o", str(binary)], capture_output=True, text=True)
        sys.stderr.write(build.stderr)
        if build.returncode != 0:
            sys.stdout.write(build.stdout)
            sys.exit("build failed")
        backend_lines = [line.strip() for line in build.stdout.splitlines() if "typescript" in line.lower() or "scriptc" in line.lower() or "bun" in line.lower()]
        result = subprocess.run([str(binary)], capture_output=True, text=True)
    if not typescript:
        program.unlink(missing_ok=True)
    if result.returncode != 0:
        sys.stderr.write(result.stderr)
        sys.exit(result.returncode)
    diagnostics = [line for line in result.stderr.splitlines() if line.strip()]

    samples = collections.OrderedDict()
    for line in result.stdout.splitlines():
        parts = line.split(",")
        if len(parts) == 4 and parts[3].lstrip("-").isdigit():
            samples.setdefault((parts[0], parts[1], parts[2]), []).append(int(parts[3]))
    if not samples:
        sys.exit("no samples in benchmark output")

    rows = []
    for (kind, payload, rate), values in samples.items():
        values.sort()
        rows.append(collections.OrderedDict([
            ("kind", kind), ("payload", payload), ("rate", rate), ("unit", unit_for(payload)),
            ("samples", len(values)), ("p50", percentile(values, 0.5)), ("p90", percentile(values, 0.9)),
            ("p99", percentile(values, 0.99)), ("max", values[-1]),
        ]))
    rows.sort(key=sort_key)

    env = environment(orchestrate, HERE / ".orch_cache", typescript, backend_lines, source)
    out = pathlib.Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    with open(out / "boundary_ladder_samples.csv", "w") as f:
        f.write("kind,payload,rate,value,unit\n")
        for (kind, payload, rate), values in samples.items():
            for v in values:
                f.write(f"{kind},{payload},{rate},{v},{unit_for(payload)}\n")
    with open(out / "boundary_ladder.csv", "w") as f:
        f.write("kind,payload,rate,unit,samples,p50,p90,p99,max\n")
        for r in rows:
            f.write(",".join(str(r[k]) for k in ("kind", "payload", "rate", "unit", "samples", "p50", "p90", "p99", "max")) + "\n")
    with open(out / "boundary_ladder.json", "w") as f:
        json.dump({"environment": env, "results": rows, "diagnostics": diagnostics}, f, indent=2)
        f.write("\n")
    text = markdown(env, rows)
    (out / "boundary_ladder.md").write_text(text)
    print(text)
    print(f"written to {out}")


if __name__ == "__main__":
    main()
