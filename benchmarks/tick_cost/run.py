#!/usr/bin/env python3
"""Measure the glue around a synchronous library tick, and write the results.

Usage: python3 benchmarks/tick_cost/run.py [--out DIR] [--ticks N] [--rounds R]
Set ORCHESTRATE to the compiler binary (default: `orchestrate` on PATH).

Each case is a small program built with `build --lib` and driven by one Rust host through
`tick_sync` on a current-thread runtime, in release mode. A case is warmed up with five
rounds first, then timed for `rounds` rounds of `ticks` ticks; the number reported is the
median round, in nanoseconds per tick, with the fastest and the p90 round beside it. The
`host method alone` row is the floor: the trait method called through its vtable with no
tick around it.

Cases:
  empty          on_tick with an empty body
  one_call       one host call in the tick
  five_calls     five host calls, so each further call is (five - one) / 4
  instance_let   a top-level `let` incremented in the tick (a struct field)
  shared_let     a `shared let` incremented in the tick (one mutex, taken once)
  event          one host-fired event handled before the tick, with one host call

Writes tick_cost.csv, tick_cost.json, and tick_cost.md under --out (default
benchmarks/results/), with the same environment header as the boundary ladder.
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
REPO = HERE.parent.parent

CASES = collections.OrderedDict([
    ("empty", "host world { fn count() }\non_tick(dt: float) { }\norchestrator main() {}\n"),
    ("one_call", "host world { fn count() }\non_tick(dt: float) { world.count() }\norchestrator main() {}\n"),
    ("five_calls", "host world { fn count() }\non_tick(dt: float) { world.count() world.count() world.count() world.count() world.count() }\norchestrator main() {}\n"),
    ("instance_let", "host world { fn count() }\nlet ticks = 0\non_tick(dt: float) { ticks = ticks + 1 }\non_stop { if ticks < 0 { world.count() } }\norchestrator main() {}\n"),
    ("shared_let", "host world { fn count() }\nshared let hits = 0\non_tick(dt: float) { hits = hits + 1 }\non_stop { if hits < 0 { world.count() } }\norchestrator main() {}\n"),
    ("event", "host world { fn count() }\non hit(n: int) { world.count() }\non_tick(dt: float) { }\norchestrator main() {}\n"),
])

HOST_TEMPLATE = r'''
use std::sync::atomic::{AtomicU64, Ordering};

struct Host(AtomicU64);
@IMPLS@

fn stats(mut rounds: Vec<f64>) -> (f64, f64, f64) {
    rounds.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let p = |f: f64| rounds[((rounds.len() - 1) as f64 * f).round() as usize];
    (p(0.5), rounds[0], p(0.9))
}

macro_rules! case {
    ($name:literal, $krate:ident, $ticks:expr, $rounds:expr, $before:expr) => {{
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let mut scripts = $krate::start(runtime.handle(), Host(AtomicU64::new(0))).unwrap();
        scripts.ready_blocking(&runtime).unwrap();
        let ticks: u64 = $ticks;
        // What the host does before each tick: nothing, or firing the case's event.
        let before: fn(&mut $krate::Scripts) = $before;
        // Warmed up first, so the clock ramp does not land on whichever case runs first.
        for _ in 0..ticks * 5 {
            before(&mut scripts);
            scripts.tick_sync(&runtime, 0.016).unwrap();
        }
        let mut rounds = Vec::new();
        for _ in 0..$rounds {
            let start = std::time::Instant::now();
            for _ in 0..ticks {
                before(&mut scripts);
                scripts.tick_sync(&runtime, 0.016).unwrap();
            }
            rounds.push(start.elapsed().as_nanos() as f64 / ticks as f64);
        }
        scripts.shutdown_blocking(&runtime).unwrap();
        let (median, min, p90) = stats(rounds);
        println!("{{\"case\":\"{}\",\"ns_per_tick\":{:.2},\"min\":{:.2},\"p90\":{:.2},\"ticks_per_round\":{},\"rounds\":{}}}", $name, median, min, p90, ticks, $rounds);
    }};
}

fn main() {
    let ticks: u64 = @TICKS@;
    let rounds: usize = @ROUNDS@;
    // The trait method through the same vtable is the floor for a host call.
    let host: std::sync::Arc<dyn tick_one_call::Host> = std::hint::black_box(std::sync::Arc::new(Host(AtomicU64::new(0))));
    for _ in 0..ticks * 5 { std::hint::black_box(host.world_count()).unwrap(); }
    let mut calls = Vec::new();
    for _ in 0..rounds {
        let start = std::time::Instant::now();
        for _ in 0..ticks { std::hint::black_box(host.world_count()).unwrap(); }
        calls.push(start.elapsed().as_nanos() as f64 / ticks as f64);
    }
    let (median, min, p90) = stats(calls);
    println!("{{\"case\":\"host_method_alone\",\"ns_per_tick\":{:.2},\"min\":{:.2},\"p90\":{:.2},\"ticks_per_round\":{},\"rounds\":{}}}", median, min, p90, ticks, rounds);
@CASES@
}
'''


def run_text(args, **kwargs):
    try:
        result = subprocess.run(args, capture_output=True, text=True, timeout=60, **kwargs)
    except (OSError, subprocess.TimeoutExpired):
        return None
    if result.returncode != 0:
        return None
    return (result.stdout or result.stderr).strip()


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


def environment(orchestrate, ticks, rounds):
    help_text = run_text([orchestrate, "--help"]) or ""
    return collections.OrderedDict([
        ("benchmark", "tick_cost"),
        ("timestamp_utc", datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds")),
        ("commit", git("rev-parse", "HEAD")),
        ("dirty", bool(git("status", "--porcelain"))),
        ("cpu", cpu_name()),
        ("cpu_count", os.cpu_count()),
        ("os", platform.platform()),
        ("arch", platform.machine()),
        ("orchestrate", help_text.splitlines()[0] if help_text else None),
        ("rustc", run_text(["rustc", "--version"])),
        ("cargo", run_text(["cargo", "--version"])),
        ("profile", "release"),
        ("python", platform.python_version()),
        ("runtime", "tokio current-thread, tick_sync"),
        ("counts", {"ticks_per_round": ticks, "rounds": rounds, "warmup_rounds": 5}),
    ])


def markdown(env, rows):
    lines = ["## Tick cost", "",
             f"One run on {env['cpu']} ({env['os']}, {env['arch']}), {env['timestamp_utc']}, commit "
             f"`{(env['commit'] or 'unknown')[:12]}`{' (dirty tree)' if env['dirty'] else ''}. {env['rustc']}. "
             f"Nanoseconds per `tick_sync` on a current-thread runtime: the median of {env['counts']['rounds']} rounds of "
             f"{env['counts']['ticks_per_round']:,} ticks, after {env['counts']['warmup_rounds']} warm-up rounds. Your numbers will differ.",
             "", "| Case | ns per tick (median) | fastest round | p90 round |", "|---|---:|---:|---:|"]
    for r in rows:
        lines.append(f"| {r['case']} | {r['ns_per_tick']:.2f} | {r['min']:.2f} | {r['p90']:.2f} |")
    fastest = {r["case"]: r["min"] for r in rows}
    if all(k in fastest for k in ("empty", "one_call", "five_calls", "host_method_alone")):
        per_call = (fastest["five_calls"] - fastest["one_call"]) / 4.0
        lines += ["", f"Derived from the fastest rounds, which move least between runs: an empty tick costs "
                  f"{fastest['empty']:.2f} ns; each host call in a tick costs {per_call:.2f} ns "
                  f"(five calls minus one, over four), against {fastest['host_method_alone']:.2f} ns for the trait "
                  f"method called through its vtable alone. The empty tick and the one-call tick are within noise "
                  f"of each other at this resolution.",
                  "",
                  "Medians overlap between cases within a few nanoseconds: the OS moves the thread between core "
                  "types and clock states during a run, which is why the fastest round is reported beside them. "
                  "Run on an idle machine, and compare cases within one run."]
    return "\n".join(lines) + "\n"


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--out", default=str(REPO / "benchmarks" / "results"))
    parser.add_argument("--ticks", type=int, default=200_000, help="ticks per round")
    parser.add_argument("--rounds", type=int, default=21, help="timed rounds per case")
    args = parser.parse_args()
    orchestrate = os.environ.get("ORCHESTRATE", "orchestrate")

    with tempfile.TemporaryDirectory(prefix="orch_tick_cost_") as tmp:
        tmp = pathlib.Path(tmp)
        deps, impls, calls = [], [], []
        for case, source in CASES.items():
            crate = f"tick_{case}"
            src_dir = tmp / f"src_{case}"
            src_dir.mkdir()
            (src_dir / "main.orch").write_text(source)
            build = subprocess.run([orchestrate, "build", "--lib", str(src_dir / "main.orch"), "-o", str(tmp / crate)],
                                   capture_output=True, text=True)
            if build.returncode != 0:
                sys.stdout.write(build.stdout)
                sys.stderr.write(build.stderr)
                sys.exit(f"build of case '{case}' failed")
            # A generated crate's own warnings are not the benchmark's business.
            deps.append(f'{crate} = {{ path = "../{crate}" }}')
            impls.append(f"impl {crate}::Host for Host {{ fn world_count(&self) -> Result<(), String> {{ self.0.fetch_add(1, Ordering::Relaxed); Ok(()) }} }}")
            before = "|s| { s.trigger_hit(1).unwrap(); }" if case == "event" else "|_| {}"
            calls.append(f'    case!("{case}", {crate}, ticks, rounds, {before});')
        host_dir = tmp / "host"
        (host_dir / "src").mkdir(parents=True)
        (host_dir / "Cargo.toml").write_text(
            "[package]\nname = \"tick_cost_host\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\n"
            "tokio = { version = \"1.35\", features = [\"full\"] }\n" + "\n".join(deps) + "\n\n[profile.release]\ndebug = false\n"
        )
        program = (HOST_TEMPLATE.replace("@IMPLS@", "\n".join(impls)).replace("@CASES@", "\n".join(calls))
                   .replace("@TICKS@", str(args.ticks)).replace("@ROUNDS@", str(args.rounds)))
        (host_dir / "src" / "main.rs").write_text(program)
        built = subprocess.run(["cargo", "build", "--release", "--quiet"], cwd=host_dir, capture_output=True, text=True)
        if built.returncode != 0:
            sys.stderr.write(built.stderr)
            sys.exit("host build failed")
        # Run the binary itself, so nothing of cargo's is measured or printed.
        run = subprocess.run([str(host_dir / "target" / "release" / "tick_cost_host")], capture_output=True, text=True)
        if run.returncode != 0:
            sys.stdout.write(run.stdout)
            sys.stderr.write(run.stderr)
            sys.exit("host run failed")

    rows = [json.loads(line) for line in run.stdout.splitlines() if line.startswith("{")]
    order = ["host_method_alone"] + list(CASES)
    rows.sort(key=lambda r: order.index(r["case"]) if r["case"] in order else 99)
    env = environment(orchestrate, args.ticks, args.rounds)
    out = pathlib.Path(args.out)
    out.mkdir(parents=True, exist_ok=True)
    with open(out / "tick_cost.csv", "w") as f:
        f.write("case,ns_per_tick,min,p90,ticks_per_round,rounds\n")
        for r in rows:
            f.write(f"{r['case']},{r['ns_per_tick']},{r['min']},{r['p90']},{r['ticks_per_round']},{r['rounds']}\n")
    with open(out / "tick_cost.json", "w") as f:
        json.dump({"environment": env, "results": rows}, f, indent=2)
        f.write("\n")
    text = markdown(env, rows)
    (out / "tick_cost.md").write_text(text)
    print(text)
    print(f"written to {out}")


if __name__ == "__main__":
    main()
