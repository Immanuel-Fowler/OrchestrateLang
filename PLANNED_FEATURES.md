# Orchestrate — Planned Features

This document outlines planned features for the Orchestrate language and ecosystem, what problem each one solves, and a rough sketch of how it would work.

> **Key:** Features marked **[SHIPPED]** are fully implemented and documented in `LANGUAGE_REFERENCE.md`.

---

## 1. Polyglot Modules (Two Forms)

**Problem it solves:** Right now, modules must be written in Orchestrate. Real orchestration work often needs to call into existing Python (ML/data), C/C++ (perf-critical or legacy code), or other Rust crates.

There are two distinct ways this could show up in the language, and they serve different needs:

### 1a. Polyglot Serverlets (actor-style, stateful)

Extends the existing `serverlet` concept — a serverlet is already "an actor with a message-passing interface," so the *language semantics* don't need to change, only what's running inside the actor. Good for stateful services, long-running connections, or anything that benefits from the actor/message-passing model.

Proposed syntax direction:

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

### 1b. Loaded Foreign Modules (direct function-call style, stateless) — **[SHIPPED for Rust, C, C++]**

A second, simpler module type: a `module.orch` that directly loads a Rust, C/C++, or Python source/library, where the **only interactable code from Orchestrate's side is the functions exposed by that loaded module** — no serverlet, no actor, no message passing. This is the "Combined Process" pattern (see Module System, Pattern A) extended to non-Orchestrate languages.

**Implemented syntax:**

```orchestrate
// math/module.orch
load_foreign "rust" "./geometry.rs"
load_foreign "c"    "./fastmath.c"      // requires geometry.orch_ffi sidecar
load_foreign "cpp"  "./stats.cpp"       // requires stats.orch_ffi sidecar
```

- **Rust**: fully implemented. The `.rs` file's `pub fn`s are injected verbatim into the generated module; type signatures are auto-scanned and registered into the typechecker.
- **C/C++**: fully implemented via `cc-rs` for compilation and `.orch_ffi` sidecar files that declare the function signatures Orchestrate exposes to callers. Functions compile to `unsafe extern "C"` wrappers with safe Rust signatures.
- **Python**: not yet implemented — trickiest for a direct-call model since Python isn't natively callable from Rust without an embedded interpreter.

See `LANGUAGE_REFERENCE.md` §6.4 and §6.5 for full documentation.

---

## 2. Sandboxed Serverlets (Wrap, Don't Build)

**Problem it solves:** Running untrusted or semi-trusted code (plugins, user-submitted logic, downloaded modules) safely, without Orchestrate needing to invent its own sandboxing/security model.

**Core principle:** Wrap existing, audited sandbox technology (e.g. `wasmtime` for WASM) — do not build a custom sandbox. Security guarantees are "as good as the wrapped tech," not better, and docs should be precise about what is/isn't isolated (e.g., compute/memory sandboxing vs. any host functions you expose).

**How it would work:**

- Proposed syntax direction — `runtime` is omitted since all sandboxed serverlets run over WASM initially; `memory_limit` and `timeout` are passed through as params:

```orchestrate
serverlet UntrustedPlugin sandbox(memory_limit: "64mb", timeout: "5s") {
    on execute(input: string) -> string {
        // body compiles to a call into the wasm guest
    }
}
```

- The compiler generates the `wasmtime` boilerplate: engine/store/instance setup, memory limits, fuel/timeout enforcement, marshaling inputs/outputs across the boundary, and trap/error handling.
- User writes one line of config; compiler generates the correct integration glue (likely the single biggest codegen feature in the language so far — bigger than typechecker or current codegen work combined).
- If/when non-WASM runtimes (e.g. Firecracker microVMs) are added later, `runtime` can become an optional param defaulting to `"wasm"` without breaking existing sandboxed serverlets.

**Connects to Feature 1:** Sandboxed serverlets, polyglot serverlets (1a), and loaded foreign modules (1b) are all variations on the same underlying theme — *handler/function bodies implemented by something other than native compiled Orchestrate code, with the compiler generating the integration glue.* Keeping the syntax for these conceptually related (even if the keywords differ — `via`, `sandbox`, `load_foreign`) keeps the language coherent rather than feature-creeped.

**Connects to Feature 4 (OPM):** If a downloaded third-party module can optionally run as a sandboxed serverlet, that's a concrete security story for the package ecosystem: "untrusted third-party modules can be isolated at the language level."

---

## 3. PROM — Personal Registry for Orchestrator Modules — **[SHIPPED]**

**Problem it solves:** `use module alias: "./path/to/dir"` is relative-path-based, making it awkward to share modules across projects or reference "a module that lives somewhere on this machine."

**Implemented:** PROM is fully operational. Use:
```bash
orchestrate prom add <name> <path>
orchestrate prom list
orchestrate prom remove <name>
```
The compiler resolves bare (non-path) module names against the local registry automatically. See `LANGUAGE_REFERENCE.md` §6.2 for full documentation.

**Design question to resolve:** Is PROM purely personal/local config (as the name implies), or does it need a per-project mode for reproducibility (so someone cloning the repo doesn't get a confusing "module not found")? If purely personal, document clearly that PROM entries are machine-local and not part of the shared project.

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

---

## Feature Implementation Timeline

Given the combined scope of these four features, recommend picking **one end-to-end story** and finishing it well before layering on the next, rather than having several features half-built simultaneously:

1. ~~**PROM** first — smallest, self-contained, validates registry plumbing.~~ **[SHIPPED]**
2. ~~**Loaded foreign Rust module** (1b, Rust only) — validates the "non-Orchestrate module" pattern with the lowest possible risk (no FFI, no embedded interpreter).~~ **[SHIPPED]**
3. ~~**Loaded foreign C/C++ module** (1b, C and C++) — via `.orch_ffi` sidecar and `cc-rs`.~~ **[SHIPPED]**
4. **Basic polyglot serverlet** (1a, Python via subprocess+JSON) — validates the actor-style "non-native serverlet body" pattern.
5. **OPM (git-based, no hosted index)** — builds on PROM's name→location mapping.
6. **Sandboxed serverlets (wasmtime)** — largest single feature; benefits from #4's pattern and gives OPM a security story.

A smaller set of fully-working, well-documented features is a stronger result (and more likely to see real use) than a sprawling set of partially-built ones.

---

## Pipedream Ideas

Far-fetched / exploratory ideas — not on the roadmap, no commitment to build, but worth keeping written down in case they become feasible or inspire something more tractable later.

### Serverlet-as-File (`.srvlt`) — Live-Editable Serverlets

**The idea:** A `.srvlt` file extension representing a single serverlet, defined and loaded separately from the main `.orch` program. While an orchestration is running, a `.srvlt` file could be edited and the running serverlet hot-swapped — live-editing actor logic without restarting the whole orchestrator.

**Why it's appealing:** Serverlets are already isolated, message-passing actors with their own state — conceptually, an actor is a reasonable unit of "hot-reloadable" code, since its interface (message types) is what the rest of the program depends on, not its internals.

**Why it's a pipedream (not a near-term feature):**

- Orchestrate compiles to native Rust — there's no running interpreter to swap code into. "Hot reload" for compiled code generally means either (a) dynamic linking (`dlopen`/shared libraries, recompiling and reloading a `.so`/`.dll` at runtime) or (b) re-running the whole compile-and-relaunch cycle, which isn't really "live."
- If the serverlet's *message enum* (its `on handler(...)` signatures) changes during a live edit, every other part of the program that calls it via the generated `*Client` would need to handle a mismatched interface — either gracefully erroring or requiring the signature to stay frozen across live edits (which limits what "live editing" can actually mean).
- State migration: if a serverlet has accumulated state (e.g. the `CounterService` example), reloading its code raises the question of what happens to that state — reset it, attempt to migrate it, or only allow live-editing of *stateless* serverlets.
- This edges into territory that's its own deep area (Erlang/OTP hot code swapping, hot module replacement in JS bundlers) — each of which exists *because* their runtimes were designed around it from day one. Bolting it onto a "transpile once, run as a native binary" model is a fundamentally different (and harder) problem.

**If ever pursued**, the most plausible path would likely be: compile each serverlet to its own dynamically-loaded library (`.so`/`.dll`), have the orchestrator load serverlets via `dlopen`-equivalent, and support reload-by-recompiling-just-that-library — with hot-swap restricted to serverlets whose message enum hasn't changed and whose state is either empty or explicitly serializable/migratable. That's a substantial architectural shift (native dynamic linking, ABI stability concerns between Rust versions, etc.) — interesting, but a different project in many ways.

### Native LLM-as-Orchestrator + `axiom.orch` Governance Files

**The idea:** Load an open-weights LLM directly into an Orchestrate program, where the model has access to all the modules the orchestrator has — effectively, the LLM becomes a tool-calling agent with the orchestrator's module functions *as* its tools, natively, without a separate agent framework.

This naturally implies a second piece: **`axiom.orch`** — a file (or section within the main orchestrator script) that defines, per module, (1) what the module *does* (a description for the LLM, used to generate tool schemas) and (2) **runtime-enforced policy** — what the LLM is and isn't allowed to call, under what conditions, regardless of what the model itself decides to do. Axioms at the main-script level establish global policy/constraints across all modules the LLM has access to.

**Why it's appealing:** This is "agentic tool-calling with governance" as a *language-level* concept rather than a framework bolted onto Python (LangChain/etc. style). Orchestrate already has a module system with clear function boundaries (serverlets, loaded modules) — those boundaries are a natural fit for "tools an LLM can call," and `axiom.orch` would be a declarative, auditable policy layer the *orchestrator itself* enforces, rather than instructions hoping the model complies.

**Restructured model — axioms as enforcement, not instruction:**

The key design shift: an axiom is **not a system prompt**. A system prompt is *advice to the model*; an axiom is *a rule the orchestrator checks before dispatching any tool call the model proposes* — the model's compliance is irrelevant to whether the rule holds.

Concretely, the agent loop becomes:

```
LLM proposes tool call (module.function, args)
        │
        ▼
Orchestrator checks proposed call against axiom policy
        │
   ┌────┴────┐
   │         │
 ALLOWED   DENIED
   │         │
   ▼         ▼
execute   return policy-violation
the call  result to LLM (call never
   │      runs — no side effects)
   ▼
return result to LLM, continue loop
```

This means a prompt-injected or adversarially-steered LLM **cannot** bypass an axiom by being convinced, tricked, or "jailbroken" — the check happens outside the model entirely, on the orchestrator side, against the actual proposed call.

**Proposed `axiom.orch` shape:**

```orchestrate
// axiom.orch

// Per-module description (used to build the LLM's tool schema)
describe module db: "Provides read access to the orders database."

// Runtime-enforced policy — checked before every dispatch, not advisory
axiom db {
    allow query        // db.query(...) may be called
    deny  delete_table // db.delete_table(...) is never dispatched, regardless of LLM request
    deny  drop_database

    // Conditional policy — e.g. rate limits, argument constraints
    allow update where rows_affected < 100
}

// Global policy — applies across all modules the LLM has access to
axiom global {
    max_calls_per_session: 50
    deny_if_module_not_described  // LLM can't call modules with no `describe` entry at all
}
```

- `describe` blocks generate the tool/function schema exposed to the LLM (auto-derivable from the module's existing AST function signatures — genuinely tractable, since Orchestrate already has this information).
- `axiom` blocks compile into **runtime checks** the orchestrator runs against every proposed tool call *before* dispatch — an allow/deny/conditional policy engine, not prompt text. A denied call never executes; the LLM receives a structured "denied by policy" result and continues the loop, but no side effect occurred.
- Axioms at the main-script level (`axiom global { ... }`) apply across the whole session/agent loop — e.g. call budgets, default-deny for undescribed modules, etc.

**Why it's still a pipedream (not a near-term feature):**

- **Open-weights model loading is a heavy runtime dependency.** Running an LLM locally means bundling/managing model weights (gigabytes), an inference runtime (e.g. llama.cpp/ggml-style, or candle for a Rust-native option), and hardware considerations (CPU vs GPU, memory requirements far beyond anything else in the language). This is a different order of magnitude from anything else in Orchestrate — it turns "lightweight native binary" into "ships with or downloads a multi-GB model and an inference engine."
- **The policy engine itself is non-trivial.** Even "allow/deny per function" is straightforward, but conditional policies (`where rows_affected < 100`) require the orchestrator to inspect *proposed arguments* against arbitrary expressions before dispatch — essentially a small expression evaluator operating on the LLM's proposed call, separate from (but reusing pieces of) the existing compiler/interpreter machinery.
- **Tool-calling protocol**: the agent loop (propose → check → execute/deny → return → continue) needs to be built into the runtime, including handling the LLM's response format, retries, and the "denied" feedback path in a way the model can productively use (e.g., the model should be able to learn "that's not allowed" and try a different approach, not just loop forever retrying the same denied call).
- **This connects to sandboxing**: even with runtime-enforced axioms, running LLM-callable modules as sandboxed serverlets adds defense-in-depth — "the orchestrator won't dispatch disallowed calls, AND the calls that *are* dispatched run in a sandbox" covers both "wrong call attempted" and "allowed call has unexpected side effects" failure modes.

**If ever pursued**, the most plausible entry point is probably: (1) auto-generate tool/function schemas from existing module declarations via `describe` (tractable now, reflection over the AST), (2) support calling out to an *external* LLM (API-based, not locally-loaded weights) first, sidestepping the multi-GB runtime problem entirely, (3) build the `axiom.orch` policy engine starting with simple allow/deny (no conditionals) as a runtime-enforced allowlist *before* worrying about local model weights or conditional expressions — local open-weights loading and conditional policy expressions would each be much later, separate, and substantial undertakings in their own right.