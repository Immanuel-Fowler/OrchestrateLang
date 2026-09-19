#!/usr/bin/env python3
"""Snapshot the Rust the compiler generates for each example, for the website's
"generated Rust" view on the examples page.

    cargo build --release
    python3 website/generate-rust.py

For every examples/*.orch the compiler is asked to build it; the generated
src/main.rs is copied to website/generated/<name>.rs before Cargo would run.
Cargo is deliberately kept off PATH so nothing is compiled: the snapshot is
the compiler's output, not a build. Examples whose foreign toolchain is
missing (Zig, Swift, TypeScript) are recorded in manifest.json as skipped.
"""
import json
import os
import shutil
import subprocess
import sys
from datetime import date
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
EXAMPLES = ROOT / "examples"
OUT = ROOT / "website" / "generated"
ORCHESTRATE = os.environ.get("ORCHESTRATE", str(ROOT / "target" / "release" / "orchestrate"))

if not Path(ORCHESTRATE).exists():
    sys.exit(f"compiler not found at {ORCHESTRATE}; run `cargo build --release` or set ORCHESTRATE")

version = subprocess.run([ORCHESTRATE, "--help"], capture_output=True, text=True).stdout
version = next((line.split()[-1] for line in version.splitlines() if "version" in line.lower()), None)
if not version:
    for line in (ROOT / "Cargo.toml").read_text().splitlines():
        if line.startswith("version"):
            version = line.split('"')[1]

OUT.mkdir(parents=True, exist_ok=True)
for old in OUT.glob("*.rs"):
    old.unlink()

# A PATH without cargo: the compiler writes src/main.rs, then fails to spawn cargo.
env = dict(os.environ)
env["PATH"] = "/usr/bin:/bin"

manifest = {"compiler": version, "generated_on": date.today().isoformat(), "examples": {}}
cache = EXAMPLES / ".orch_cache"

# prom_demo imports the counter module by its registered name; register it for the run
# and remove the entry afterwards unless it was already there.
registered = subprocess.run([ORCHESTRATE, "prom", "list"], capture_output=True, text=True, env=env).stdout
added_prom = "counter" not in registered.split()
if added_prom:
    subprocess.run([ORCHESTRATE, "prom", "add", "counter", str(EXAMPLES / "modules" / "counter")], capture_output=True, env=env)
for orch in sorted(EXAMPLES.glob("*.orch")):
    name = orch.stem
    shutil.rmtree(cache, ignore_errors=True)
    proc = subprocess.run([ORCHESTRATE, "build", str(orch)], capture_output=True, text=True, env=env, cwd=ROOT)
    main_rs = cache / "src" / "main.rs"
    if main_rs.exists():
        shutil.copy(main_rs, OUT / f"{name}.rs")
        manifest["examples"][name] = {"status": "ok", "lines": sum(1 for _ in open(main_rs))}
        print(f"ok      {name}")
    else:
        reason = (proc.stderr.strip() or proc.stdout.strip()).splitlines()
        reason = reason[-1] if reason else "no output"
        manifest["examples"][name] = {"status": "skipped", "reason": reason}
        print(f"skipped {name}: {reason}")
shutil.rmtree(cache, ignore_errors=True)
if added_prom:
    subprocess.run([ORCHESTRATE, "prom", "remove", "counter"], capture_output=True, env=env)
(OUT / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
print(f"wrote {OUT / 'manifest.json'}")
