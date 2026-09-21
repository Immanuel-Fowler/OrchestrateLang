# OrchestrateLang — Planned Features

This document outlines planned features for the OrchestrateLang language and ecosystem, what problem each one solves, and a rough sketch of how it would work.

> **Key:** Features marked **[SHIPPED]** are fully implemented and documented in `language-reference.md`.

---

## 1. Polyglot Modules (Two Forms)

**Problem it solves:** Right now, modules must be written in OrchestrateLang. Real orchestration work often needs to call into existing Python (ML/data), C/C++ (perf-critical or legacy code), or other Rust crates.

There are two distinct ways this could show up in the language, and they serve different needs:

### 1a. Polyglot Serverlets (actor-style, stateful)

> **Current plan:** [design/landline-serverlets.md](design/landline-serverlets.md). It
> supersedes the sketch below: handler bodies run over a pipe or embedded in-process, with
> a binary protocol and an interface check at startup, and OrchestrateLang can build as a
> library a host application links. First adopter: a Rust game engine's scripting layer.

Extends the existing `serverlet` concept — a serverlet is already "an actor with a message-passing interface," so the *language semantics* don't need to change, only what's running inside the actor. Good for stateful services, long-running connections, or anything that benefits from the actor/message-passing model.

Proposed syntax direction:

```orchestrate
serverlet PyScorer via "python" {
    on score(input: string) -> float {
        // dispatched to a Python function/process instead of compiled OrchestrateLang
    }
}
```

- **Python**: start with subprocess + stdin/stdout JSON (simplest, lowest risk). PyO3 embedding could come later as a "fast path."
- **C/C++**: via FFI bindings (e.g. `bindgen`-style). Higher complexity — calling conventions, memory ownership across the boundary, build complexity.
- **Rust**: likely the easiest — could potentially just be another `Combined Process` module pattern, since it's already native.

### 1b. Loaded Foreign Modules (direct function-call style, stateless) — **[SHIPPED for Rust, C, C++, Zig, Swift, C#, TypeScript, WebAssembly]**

A second, simpler module type: a `module.orch` that directly loads a Rust, C/C++, Zig,
Swift, or TypeScript source/library, where the **only interactable code from
OrchestrateLang's side is the functions exposed by that loaded module** — no serverlet,
no actor, no message passing. This is the "Combined Process" pattern (see Module System,
Pattern A) extended to non-OrchestrateLang languages.

**Implemented syntax:**

```orchestrate
// math/module.orch
load_foreign "rust" "./geometry.rs"
load_foreign "c"    "./fastmath.c"      // requires geometry.orch_ffi sidecar
load_foreign "cpp"  "./stats.cpp"       // requires stats.orch_ffi sidecar
load_foreign "zig"  "./vectors.zig"     // requires vectors.orch_ffi sidecar
load_foreign "swift" "./calendar.swift" // requires calendar.orch_ffi sidecar
load_foreign "typescript" "./math.ts"   // requires math.orch_ffi sidecar
```

- **Rust**: fully implemented. The `.rs` file's `pub fn`s are injected verbatim into the generated module; type signatures are auto-scanned and registered into the typechecker.
- **C/C++**: fully implemented via `cc-rs` for compilation and `.orch_ffi` sidecar files that declare the function signatures OrchestrateLang exposes to callers. Functions compile to `unsafe extern "C"` wrappers with safe Rust signatures.
- **Zig / Swift**: implemented with the same `.orch_ffi` sidecars and C-ABI types. The generated `build.rs` compiles each file to a static library with `zig build-obj` plus the platform archiver (`export fn`) or `swiftc` (`@_cdecl`, or `@c` on Swift 6.3+) and links the Swift runtime. Host target only.
- **Python**: not possible as FFI, because Python needs its interpreter. Use a landline serverlet (1a).
- **TypeScript**: implemented as a generated executable bridge. TypeScript 7 checks the
  source, then the compiler tries scriptc and falls back to `bun build --compile` for the
  same persistent protocol executable, one per imported module, so module state survives
  between calls. Each `load_foreign` or `via typescript(...)` declaration can name its
  backend; `ORCH_TS_BACKEND` is the project-wide default. TypeScript landlines remain the
  path for independently started instances, host callbacks, ticks, and budgets.

**Next FFI work** (preferred over landlines; see the [design philosophy](design-philosophy.md) §4):

- **TypeScript in-process, through scriptc's library mode.** `scriptc build --lib
  --profile` (scriptc 0.1.1) emits a C-ABI static archive from a TypeScript module. A
  probe on 2026-09-18 linked one into a C program: module state persisted in-process and a
  call cost **5.1 ns**, the Zig/Swift class, against ~60 µs over today's pipe bridge.
  Constraints found: the static tier has no `bigint` (an `int` crosses as `i64`
  parameters, while an `i64` return needs a range scriptc can prove at compile time),
  strings cross as pointer + length with out-parameters, arrays and structs are deferred
  by scriptc, and `wasm32-wasi` rejects library mode. This would ship as
  `load_foreign "typescript" "math.ts" (backend: "native")`; the plan is
  [plans/typescript-native-backend.md](plans/typescript-native-backend.md).
- Arrays and structs across the C ABI shipped in 0.14.0, on the same ownership rule as
  `string` and `handle` (0.8.0): an array is a pointer and a count, a returned array is
  `malloc`'d and freed by the wrapper, and a struct crosses by value as `#[repr(C)]`.
- Cross-compiling Zig and Swift sources for `build --lib --target`.
- **Go** works through `-buildmode=c-archive`, but only one Go library can be loaded per process.

See `language-reference.md` §6.4 and §6.5 for full documentation.

---

## 2. Sandboxed Serverlets (Wrap, Don't Build) — **[SHIPPED in 0.9.0, except grants]**

**Problem it solves:** Running untrusted or semi-trusted code (plugins, user-submitted logic, downloaded modules) safely, without OrchestrateLang needing to invent its own sandboxing/security model.

**Core principle:** Wrap existing, audited sandbox technology — `wasmtime` — do not build a custom sandbox. The guarantee is wasmtime's, no better and no worse, and the docs say exactly what is and is not contained.

**What shipped:**

```orchestrate
serverlet UntrustedPlugin sandbox(memory_limit: "64mb", timeout: "5s") {
    on execute(input: string) -> string {
        // compiled to wasm32-wasip1, run inside a wasmtime instance
    }
}
```

- The handlers become their own crate, compiled to `wasm32-wasip1` and embedded in the program. Each call crosses into a wasmtime instance; from the caller's side nothing changes.
- `memory_limit` caps linear memory and `timeout` bounds a single call, through epoch interruption. State lives inside the guest and persists between calls.
- Imports are denied by default. The guest is given two diagnostic functions — one carrying its own stderr out, one letting it stop itself — and nothing else, so a contained failure can still say what it was.
- A call that exceeds a limit is logged, answered with the return type's default, and the guest is replaced, since a trap abandons it mid-call. That means a failed call resets the serverlet's state.
- `int`, `float`, `bool`, `string`, `int[]`, `float[]`, `bool[]`, structs of numbers,
  booleans, and such structs, and `void` cross, under the C ABI's ownership and layout
  rules: pointer and count for an array, `#[repr(C)]` bytes for a struct, and what one
  side writes for the other it frees after the call.
- Only a program that uses the feature gains the `wasmtime` dependency.

**Still open:** `grant` on a sandboxed serverlet. Each grant has to become one narrow, mediated host function in the wasmtime linker — the point where sandboxing and the consent model become the same mechanism. Until that exists, a grant on a sandboxed serverlet is a compile error rather than a hole that opens quietly. `on_crash` is likewise a compile error. Arrays of strings or structs, structs holding them, and non-WASM isolation backends remain out of scope.

**Connects to Feature 1:** Sandboxed serverlets, polyglot serverlets (1a), and loaded foreign modules (1b) are all variations on the same underlying theme — *handler/function bodies implemented by something other than native compiled OrchestrateLang code, with the compiler generating the integration glue.* `load_foreign "wasm"` (1b) and this feature share one wasmtime host.

**Connects to Feature 4 (OPM):** A downloaded third-party module can now run as a sandboxed serverlet, which is a concrete security story for the package ecosystem: untrusted third-party modules can be isolated at the language level.

---

## 2b. Shared state (`shared let`) — **[PROPOSED]**

**Problem it solves:** a top-level `let` is owned by the instance and only hooks can reach
it, because a spawned worker can run while a tick holds it. The existing answer for state
several concurrent things touch is a serverlet, at about 7.5 µs a call. There is nothing
between "free but hooks only" and "safe but microseconds".

**How it would work:** `shared let hits = 0` puts the binding behind one mutex on the
context instead of in the program struct, reachable from any task and any `fn`, at an
estimated ~15–20 ns. A plain `let` does not change and a program without the marker
generates the same code it does today.

This is the language's own idiom — one construct, several backends, chosen by a word in the
declaration, the way `secret`, `sandbox`, and `via` already work. The design, the guarantee
(a statement touching shared state is atomic with respect to all shared state), and the open
questions are in [design/shared-state.md](design/shared-state.md).

---

## 3. PROM — Personal Registry for Orchestrator Modules — **[SHIPPED]**

**Problem it solves:** `use module alias: "./path/to/dir"` is relative-path-based, making it awkward to share modules across projects or reference "a module that lives somewhere on this machine."

**Implemented:** PROM is fully operational. Use:
```bash
orchestrate prom add <name> <path>
orchestrate prom list
orchestrate prom remove <name>
```
The compiler resolves bare (non-path) module names against the local registry automatically. See `language-reference.md` §6.2 for full documentation.

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

## 5. Core language gaps (found in the 2026-09-18 review)

Each is small enough to land on its own; the plan with a design and steps for all four
is [plans/language-gaps.md](plans/language-gaps.md).

- **`result<T, E>` — shipped in 0.8.0.** `result<T>` used to carry only a `string` error.
  The second parameter defaults to `string`, so every existing program kept compiling;
  `err(value)` takes any type, `?` requires matching error types, and
  `catch e: Failure` names a non-string error a `try` block propagates.
- **`string` across the C ABI, then opaque handles — shipped in 0.8.0.** C, C++, Zig,
  and Swift sidecars used to accept only `int`, `float`, and `bool`. Strings cross under
  one ownership rule (NUL-terminated in; `malloc`'d out, copied and freed by the wrapper),
  and `handle` is an opaque native object released through the sidecar's `drop` function
  when its last owner drops — stateful native objects without a serverlet. Arrays and
  structs across the C ABI are still to come.
- **The standard library beyond `int` — shipped in 0.8.0.** `lists` was `int[]`-only
  because Rust sidecars were monomorphic. Sidecar signatures take type parameters,
  `reverse<T>(items: T[]) -> T[]`, so the structural list functions accept any element
  type and the numeric ones gained `float` variants.
- **Sandboxed serverlets contain their code — shipped in 0.9.0.** The guest runs under
  wasmtime with a memory cap, a per-call timeout, and no import it was not granted. Step 7
  of [design/sandboxed-serverlets.md](design/sandboxed-serverlets.md), turning a `grant`
  into a mediated host function, is the one part still open; until it exists a `grant` on a
  sandboxed serverlet is a compile error rather than a hole that opens quietly.
- **Core defects found in the 2026-09-19 review — fixed in 0.11.0.** Four bugs that all
  leaked generated Rust to the user: string concatenation moved its operands, a match on a
  unit enum variant that bound a value reached rustc, a `try` block whose body waited would
  not compile, and a `fn`, `task`, or `process` naming top-level state reported a missing
  Rust binding. Each is now either correct or an OrchestrateLang diagnostic.

---

## How the Pieces Fit Together

A possible overall narrative for the ecosystem:

- **PROM** — reference modules by name, locally.
- **OPM** — acquire modules by name, from elsewhere (git/releases).
- **Polyglot serverlets** — compose modules written in other languages (Python, C/C++, Rust).
- **Sandboxed serverlets** — isolate modules (especially downloaded/untrusted ones) using existing, audited sandbox tech.

Together: *"OrchestrateLang lets you compose modules from anywhere — local, downloaded, or written in other languages — reference them simply, and isolate the ones you don't fully trust."*

---

## Feature Implementation Timeline

Given the combined scope of these four features, recommend picking **one end-to-end story** and finishing it well before layering on the next, rather than having several features half-built simultaneously:

1. ~~**PROM** first — smallest, self-contained, validates registry plumbing.~~ **[SHIPPED]**
2. ~~**Loaded foreign Rust module** (1b, Rust only) — validates the "non-OrchestrateLang module" pattern with the lowest possible risk (no FFI, no embedded interpreter).~~ **[SHIPPED]**
3. ~~**Loaded foreign C/C++ module** (1b, C and C++) — via `.orch_ffi` sidecar and `cc-rs`.~~ **[SHIPPED]**
4. **Landline serverlets** — [build plan](design/landline-serverlets.md): Python and
   TypeScript landlines are implemented, including TypeScript 7 checking, scriptc/Bun
   executable selection, protocol v1, grants, library packaging, budgets, and late-result
   policies. TypeScript FFI, Zig FFI, Swift FFI, C# via Native AOT (0.10.0), and
   WebAssembly modules (0.9.0), and arrays and structs across the C ABI (0.14.0) are also
   implemented. Still open: a web-compatible library subset. This supersedes the
   subprocess+JSON sketch.
5. **OPM (git-based, no hosted index)** — builds on PROM's name→location mapping.
6. ~~**Sandboxed serverlets (wasmtime)** — largest single feature; benefits from #4's pattern and gives OPM a security story.~~ **[SHIPPED in 0.9.0, except grants]**

A smaller set of fully-working, well-documented features is a stronger result (and more likely to see real use) than a sprawling set of partially-built ones.

---

## Pipedream Ideas

Far-fetched / exploratory ideas — not on the roadmap, no commitment to build, but worth keeping written down in case they become feasible or inspire something more tractable later.

### Serverlet-as-File (`.srvlt`) — Live-Editable Serverlets

**The idea:** A `.srvlt` file extension representing a single serverlet, defined and loaded separately from the main `.orch` program. While an orchestration is running, a `.srvlt` file could be edited and the running serverlet hot-swapped — live-editing actor logic without restarting the whole orchestrator.

**Why it's appealing:** Serverlets are already isolated, message-passing actors with their own state — conceptually, an actor is a reasonable unit of "hot-reloadable" code, since its interface (message types) is what the rest of the program depends on, not its internals.

**Why it's a pipedream (not a near-term feature):**

- OrchestrateLang compiles to native Rust — there's no running interpreter to swap code into. "Hot reload" for compiled code generally means either (a) dynamic linking (`dlopen`/shared libraries, recompiling and reloading a `.so`/`.dll` at runtime) or (b) re-running the whole compile-and-relaunch cycle, which isn't really "live."
- If the serverlet's *message enum* (its `on handler(...)` signatures) changes during a live edit, every other part of the program that calls it via the generated `*Client` would need to handle a mismatched interface — either gracefully erroring or requiring the signature to stay frozen across live edits (which limits what "live editing" can actually mean).
- State migration: if a serverlet has accumulated state (e.g. the `CounterService` example), reloading its code raises the question of what happens to that state — reset it, attempt to migrate it, or only allow live-editing of *stateless* serverlets.
- This edges into territory that's its own deep area (Erlang/OTP hot code swapping, hot module replacement in JS bundlers) — each of which exists *because* their runtimes were designed around it from day one. Bolting it onto a "transpile once, run as a native binary" model is a fundamentally different (and harder) problem.

**If ever pursued**, the most plausible path would likely be: compile each serverlet to its own dynamically-loaded library (`.so`/`.dll`), have the orchestrator load serverlets via `dlopen`-equivalent, and support reload-by-recompiling-just-that-library — with hot-swap restricted to serverlets whose message enum hasn't changed and whose state is either empty or explicitly serializable/migratable. That's a substantial architectural shift (native dynamic linking, ABI stability concerns between Rust versions, etc.) — interesting, but a different project in many ways.

### Native LLM-as-Orchestrator + `axiom.orch` Governance Files

**The idea:** Load an open-weights LLM directly into an OrchestrateLang program, where the model has access to all the modules the orchestrator has — effectively, the LLM becomes a tool-calling agent with the orchestrator's module functions *as* its tools, natively, without a separate agent framework.

This naturally implies a second piece: **`axiom.orch`** — a file (or section within the main orchestrator script) that defines, per module, (1) what the module *does* (a description for the LLM, used to generate tool schemas) and (2) **runtime-enforced policy** — what the LLM is and isn't allowed to call, under what conditions, regardless of what the model itself decides to do. Axioms at the main-script level establish global policy/constraints across all modules the LLM has access to.

**Why it's appealing:** This is "agentic tool-calling with governance" as a *language-level* concept rather than a framework bolted onto Python (LangChain/etc. style). OrchestrateLang already has a module system with clear function boundaries (serverlets, loaded modules) — those boundaries are a natural fit for "tools an LLM can call," and `axiom.orch` would be a declarative, auditable policy layer the *orchestrator itself* enforces, rather than instructions hoping the model complies.

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

- `describe` blocks generate the tool/function schema exposed to the LLM (auto-derivable from the module's existing AST function signatures — genuinely tractable, since OrchestrateLang already has this information).
- `axiom` blocks compile into **runtime checks** the orchestrator runs against every proposed tool call *before* dispatch — an allow/deny/conditional policy engine, not prompt text. A denied call never executes; the LLM receives a structured "denied by policy" result and continues the loop, but no side effect occurred.
- Axioms at the main-script level (`axiom global { ... }`) apply across the whole session/agent loop — e.g. call budgets, default-deny for undescribed modules, etc.

**Why it's still a pipedream (not a near-term feature):**

- **Open-weights model loading is a heavy runtime dependency.** Running an LLM locally means bundling/managing model weights (gigabytes), an inference runtime (e.g. llama.cpp/ggml-style, or candle for a Rust-native option), and hardware considerations (CPU vs GPU, memory requirements far beyond anything else in the language). This is a different order of magnitude from anything else in OrchestrateLang — it turns "lightweight native binary" into "ships with or downloads a multi-GB model and an inference engine."
- **The policy engine itself is non-trivial.** Even "allow/deny per function" is straightforward, but conditional policies (`where rows_affected < 100`) require the orchestrator to inspect *proposed arguments* against arbitrary expressions before dispatch — essentially a small expression evaluator operating on the LLM's proposed call, separate from (but reusing pieces of) the existing compiler/interpreter machinery.
- **Tool-calling protocol**: the agent loop (propose → check → execute/deny → return → continue) needs to be built into the runtime, including handling the LLM's response format, retries, and the "denied" feedback path in a way the model can productively use (e.g., the model should be able to learn "that's not allowed" and try a different approach, not just loop forever retrying the same denied call).
- **This connects to sandboxing**: even with runtime-enforced axioms, running LLM-callable modules as sandboxed serverlets adds defense-in-depth — "the orchestrator won't dispatch disallowed calls, AND the calls that *are* dispatched run in a sandbox" covers both "wrong call attempted" and "allowed call has unexpected side effects" failure modes.

**If ever pursued**, the most plausible entry point is probably: (1) auto-generate tool/function schemas from existing module declarations via `describe` (tractable now, reflection over the AST), (2) support calling out to an *external* LLM (API-based, not locally-loaded weights) first, sidestepping the multi-GB runtime problem entirely, (3) build the `axiom.orch` policy engine starting with simple allow/deny (no conditionals) as a runtime-enforced allowlist *before* worrying about local model weights or conditional expressions — local open-weights loading and conditional policy expressions would each be much later, separate, and substantial undertakings in their own right.
