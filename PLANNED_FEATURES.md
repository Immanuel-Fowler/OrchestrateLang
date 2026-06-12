# Orchestrate — Planned Features

This document outlines planned features for the Orchestrate language and ecosystem, what problem each one solves, and a rough sketch of how it would work.

---

## 1. Polyglot Module Loading (Python / Rust / C / C++)

**Problem it solves:** Right now, modules must be written in Orchestrate. Real orchestration work often needs to call into existing Python (ML/data), C/C++ (perf-critical or legacy code), or other Rust crates.

**How it would work:**

- Extends the existing `serverlet` concept — a serverlet is already "an actor with a message-passing interface," so the *language semantics* don't need to change, only what's running inside the actor.
- Proposed syntax direction:

```orchestrate
serverlet PyScorer via "python" {
    on score(input: string) -> float {
        // dispatched to a Python function/process instead of compiled Orchestrate
    }
}
```

- **Python**: start with subprocess + stdin/stdout JSON (simplest, lowest risk). PyO3 embedding could come later as a "fast path."
- **C/C++**: via FFI bindings (e.g. `bindgen`-style). Higher complexity — calling conventions, memory ownership across the boundary, build complexity.
- **Rust**: likely the easiest — could potentially just be another `Combined Process` module pattern, since it's already native.

**Key design challenge:** Failure semantics. What happens when the Python process crashes, the C library segfaults, or serialization doesn't round-trip cleanly? Getting *this* right is the actual product — the happy path is the easy part.

**Priority/sequencing note:** Python via subprocess+JSON is the recommended starting point — lowest risk, highest immediate demand, and validates the "serverlet body implemented by something other than native Orchestrate code" pattern that sandboxing (below) also needs.

---

## 2. Sandboxed Serverlets (Wrap, Don't Build)

**Problem it solves:** Running untrusted or semi-trusted code (plugins, user-submitted logic, downloaded modules) safely, without Orchestrate needing to invent its own sandboxing/security model.

**Core principle:** Wrap existing, audited sandbox technology (e.g. `wasmtime` for WASM) — do not build a custom sandbox. Security guarantees are "as good as the wrapped tech," not better, and docs should be precise about what is/isn't isolated (e.g., compute/memory sandboxing vs. any host functions you expose).

**How it would work:**

- Proposed syntax direction:

```orchestrate
serverlet UntrustedPlugin sandbox(runtime: "wasm", memory_limit: "64mb", timeout: "5s") {
    on execute(input: string) -> string {
        // body compiles to a call into the wasm guest
    }
}
```

- The compiler generates the `wasmtime` boilerplate: engine/store/instance setup, memory limits, fuel/timeout enforcement, marshaling inputs/outputs across the boundary, and trap/error handling.
- User writes one line of config; compiler generates the correct integration glue (likely the single biggest codegen feature in the language so far — bigger than typechecker or current codegen work combined).

**Connects to Feature 1:** A "sandboxed serverlet" and a "Python-backed serverlet" are both instances of the same underlying pattern — *a serverlet whose handler body is implemented by something other than native Orchestrate code, with the compiler generating the integration glue.* Designing syntax around this generalization (e.g. `serverlet X via "wasm"` / `serverlet Y via "python"`) keeps the language coherent rather than feature-creeped.

**Connects to Feature 4 (OPM):** If a downloaded third-party module can optionally run as a sandboxed serverlet, that's a concrete security story for the package ecosystem: "untrusted third-party modules can be isolated at the language level."

---

## 3. PROM — Personal Registry for Orchestrator Modules

**Problem it solves:** `use module alias: "./path/to/dir"` is relative-path-based, making it awkward to share modules across projects or reference "a module that lives somewhere on this machine."

**How it would work:**

- A local registry file (e.g. `~/.orchestrate/registry.toml`) mapping `name -> path`.
- CLI subcommands:
  - `orchestrate prom add <name> <path>`
  - `orchestrate prom list`
  - `orchestrate prom remove <name>`
- Compiler change: when `use module alias: "name"` doesn't look like a path (no `./` or `/`), check PROM's registry before erroring.

**Design question to resolve:** Is PROM purely personal/local config (as the name implies), or does it need a per-project mode for reproducibility (so someone cloning the repo doesn't get a confusing "module not found")? If purely personal, document clearly that PROM entries are machine-local and not part of the shared project.

**Scope/risk:** Low. Self-contained — doesn't touch the compiler core, just module resolution in `main.rs`. Good first ecosystem feature: low-stakes, immediately useful, and validates the "name → location" mapping that OPM will also need.

---

## 4. OPM — Orchestrator Package Manager

**Problem it solves:** Acquiring modules written by others by reference name, rather than manual copy/paste or path wrangling.

**How it would work (recommended minimal approach):**

- **No hosted registry/index initially.** Lowest-risk version: `opm install github.com/someone/some-module` clones/downloads from a git URL or release, drops it into a `modules/` directory or wherever PROM/the user points it.
- This avoids the "now you run critical infrastructure" problem of hosting a central package index (moderation, typosquatting, abandoned packages, namespace disputes — the long-term pain points of npm/crates.io-scale ecosystems).
- **Versioning/reproducibility:** Pin to a commit/tag, write a lockfile (e.g. `opm.lock`) so a project's module set is reproducible.
- **Security stance:** Be explicit in docs — OPM does not vet packages; install from sources you trust. Don't imply vetting that doesn't exist.

**Connects to Feature 3 (PROM):** Both need a "name → location" mapping; PROM validates this plumbing on a smaller scale first.

**Connects to Feature 2 (Sandboxing):** Downloaded modules could optionally run sandboxed, giving a real (if partial) answer to the supply-chain risk inherent in any "install code from the internet" tool.

---

## How the Pieces Fit Together

A possible overall narrative for the ecosystem:

- **PROM** — reference modules by name, locally.
- **OPM** — acquire modules by name, from elsewhere (git/releases).
- **Polyglot serverlets** — compose modules written in other languages (Python, C/C++, Rust).
- **Sandboxed serverlets** — isolate modules (especially downloaded/untrusted ones) using existing, audited sandbox tech.

Together: *"Orchestrate lets you compose modules from anywhere — local, downloaded, or written in other languages — reference them simply, and isolate the ones you don't fully trust."*


