# Landline Serverlets — Design & Build Plan

> Status: **🚧 IN PROGRESS — Python and TypeScript pipes, TypeScript FFI, Rust library
> mode, host callbacks, call budgets, a latency benchmark, and host integration for the
> first adopter are implemented (§8). Other runtimes remain planned.** The
> [Python SDK guide](../../sdk/python/README.md) and [TypeScript SDK guide](../../sdk/typescript/README.md)
> describe the implemented subsets;
> the [library guide](../library-mode.md) describes host integration. Embedded runtimes below remain proposals. What *is* settled: a serverlet's handler bodies can be written in
> another language and run behind a **landline** (a connection with no sockets);
> OrchestrateLang can build as a Rust library that a host application links; and the
> design has to serve both event-driven and high-frequency calls.
>
> **First adopter:** the scripting layer of a Rust game engine (Python, C#, TypeScript,
> and C++ gameplay and tool scripts). It sets the *priority* of this work, not its shape — nothing below
> is game-specific. "Host" means any Rust application that links OrchestrateLang, and
> "tick" means any update loop the host drives.

---

## 0. The serverlet family (taxonomy)

| Type | Where the body runs | Primary property | Status |
|---|---|---|---|
| **Serverlet** | In-process tokio actor, written in OrchestrateLang | Speed, simplicity | ✅ Shipped |
| **Secret serverlet** | Separate OS process, written in OrchestrateLang | Secrecy + crash isolation | ✅ Shipped ([secret-serverlets.md](secret-serverlets.md)) |
| **Sandboxed serverlet** | WASM guest (`wasmtime`) | Containment of hostile code | 🚧 Steps 1–2 of 8 ([sandboxed-serverlets.md](sandboxed-serverlets.md)) |
| **Landline serverlet** | Code in another language, over a pipe or embedded in-process (Python and TypeScript today; languages that can export C functions should prefer FFI) | **Polyglot handler bodies** | ✅ Python and TypeScript pipes; other runtimes planned |

A landline serverlet is the actor-style polyglot path from
[roadmap.md](../roadmap.md) §1a. It keeps the serverlet contract — callers use the same
generated `XClient` — and swaps what sits at the other end of the line.

---

## 1. The problem it solves

OrchestrateLang exists to coordinate code that runs in separate runtimes
([serverlet-files.md](serverlet-files.md) §1). Today a serverlet's body can only be
written in OrchestrateLang, and `load_foreign` covers stateless calls into Rust, C, C++,
Zig, Swift, and TypeScript. There is no way to write a **stateful, long-lived** piece of a
program in a language such as Python or TypeScript and talk to it the way you talk to any
other serverlet.

Landlines are not the first choice for every language. A language that can export
C-callable functions (C#, Zig, Swift) should use FFI instead; see the
[design philosophy](../design-philosophy.md) §4 and §8 below. Python needs its interpreter;
TypeScript also has a persistent executable FFI bridge, but its calls are synchronous and
serialized through one process per imported module. Python and TypeScript use landlines
for independently started instances, asynchronous handlers, host callbacks, and budgets.

Landline serverlets close that gap. OrchestrateLang declares each foreign serverlet's
interface, starts and supervises its runtime, routes calls and events to it, and
compiles all of that coordination into Rust.

**Example — the first adopter.** A Rust game engine wants gameplay scripts in the
language that suits each job: Python for quests and dialogue, C# and TypeScript for
gameplay code and tools, C++ for low-level plugins. OrchestrateLang becomes the
coordinator of that scripting layer, compiled into a crate the engine links. The same
shape fits any Rust host with a plugin or scripting layer: simulations, desktop apps,
servers.

---

## 2. What compiles to Rust (and what can't)

Be precise about this, because "it all compiles to Rust" is only true of the middle:

- **Compiles to Rust:** the orchestration (events, processes, supervision), every
  landline's client and protocol code, the host API that foreign code calls back into,
  and any serverlet written in OrchestrateLang or Rust.
- **Linked natively (FFI):** Rust, C, C++, Zig, and Swift today. Other languages that
  compile to a native library with C-callable functions can join them, such as C# (.NET
  Native AOT). Some bring their own runtime into the process, such as the Swift runtime
  or the .NET garbage collector.
- **Neither compiles to Rust nor links through the C ABI:** Python always needs its
  interpreter (over a pipe or embedded). TypeScript is checked by TypeScript 7 and uses a
  generated executable bridge for synchronous FFI or a persistent landline; scriptc is
  attempted first and Bun is used when the source needs its runtime features. Each
  `load_foreign` and `via typescript(...)` declaration can name its backend
  (`backend: "auto" | "scriptc" | "bun"`); `ORCH_TS_BACKEND` is only the project-wide
  default.

The host sees one Rust crate. The polyglot part happens at the edges of it.

---

## 3. Architecture

```
Host application (Rust, owns its main loop) — e.g. a game engine
 └─ scripts crate  ← compiled from .orch files ("library mode", §6)
     ├─ orchestration: events, automatic/triggered processes, supervision
     ├─ host API: host functions foreign code is granted access to
     ├─ FFI ─────────────────────────── linked in directly (preferred)
     │     ├─ Rust / C / C++ / Zig / Swift  ← today
     │     └─ C# (Native AOT)           ← planned
     ├─ line: "embedded" ────────────── runtime hosted inside the host process
     │     └─ Python (pyo3)             ← high-frequency calls
     └─ line: "pipe" ────────────────── stdin/stdout to a child process (sidecar)
           ├─ python  quests.py         ← event-driven calls
           ├─ serverlet tools.ts        ← compiled TypeScript executable
           └─ any program that speaks the protocol
```

### Two kinds of line

"Landline" means **no sockets**: no ports, no network stack, nothing to firewall or
clean up. There are two ways to run a line, and a program can use both.

| | `line: "pipe"` (default) | `line: "embedded"` |
|---|---|---|
| **Runtime lives in** | A child process next to the host | The host process |
| **Transport** | Length-prefixed frames over stdin/stdout | Direct calls through the runtime's embedding API |
| **Per-call cost** | A process round trip plus serialization | A function call plus value conversion |
| **Crash isolation** | Yes — a crash kills its process, `on_crash` restarts it | No — a runtime crash takes the host down |
| **Hot reload** | Easy — restart the child with new code | Harder — depends on the runtime |
| **Good for** | Event handlers, services, tools (e.g. quests, mod hooks) | High-frequency and per-item work (e.g. per-frame logic) |

Both lines share the same OrchestrateLang syntax and generated client, so moving a
serverlet from `pipe` to `embedded` when profiling says so is a one-word change.

### How much already exists

- **Pipe lines** generalize the shipped secret serverlet transport: a long-lived child,
  a readiness handshake, length-prefixed frames on stdio, EOF as shutdown, and crash
  detection. Today both ends are generated Rust; a landline puts a Python process (or any
  program that speaks the protocol) on the far end, with a small SDK that speaks the protocol.
- **C/C++ in-process** builds on `load_foreign "cpp"` (`cc-rs` + `.orch_ffi` sidecars).
- **Library mode** builds on module codegen, which already emits Rust files that don't
  own `main`.

---

## 4. Proposed syntax

### Declaring a landline (in OrchestrateLang)

Handlers are declared **without bodies** — the body lives in the foreign source. The
examples use the first adopter's domain:

```orchestrate
// scripts/module.orch

serverlet QuestLog via python(source: "./quests.py") {
    on complete(quest_id: int) -> bool
    on active_count() -> int
}

serverlet EnemyBrain via python(source: "./enemy_brain.py", line: "embedded") {
    on think(input: FrameInput) -> EnemyCommands
}
```

- `via <runtime>(...)` is a modifier on the serverlet, a sibling of `secret` and
  `sandbox(...)`, and uses the same `key: "value"` config style as `sandbox(...)`.
- `line` defaults to `"pipe"`.
- Callers don't change: `let quests = start QuestLog()` then `quests.complete(42)`.

### Implementing it (in the foreign language)

```python
# scripts/quests.py
from orchestratelang import landline

class QuestLog(landline.Serverlet):
    def __init__(self):
        self.done = set()

    def complete(self, quest_id: int) -> bool:
        if quest_id in self.done:
            return False
        self.done.add(quest_id)
        self.host.world.spawn("reward_chest", 0.0, 0.0)
        return True

    def active_count(self) -> int:
        return len(self.done)

landline.serve(QuestLog)
```

An SDK for another pipe language follows the same pattern: implement the handlers as
methods, then call `serve`.

### Calling back into the host (host API)

Foreign code usually needs to act on the host, not just answer questions. The host
exposes functions, and each serverlet is granted only what it needs — the same `grant`
idea as [serverlet-files.md](serverlet-files.md):

```orchestrate
host world {
    fn spawn(kind: string, x: float, y: float) -> int
    fn play_sound(name: string)
}

serverlet QuestLog via python(source: "./quests.py") {
    grant call world.spawn
    on complete(quest_id: int) -> bool
}
```

An ungranted host function does not exist on the foreign side. The host API is
implemented by the host application in Rust (see library mode, §6).

---

## 5. High-frequency calls

Calls made every tick — every frame in a game, every step in a simulation — are where a
naive design fails, so they get explicit rules. Budgets are implemented (§8 step 8).

1. **Batch per tick, never per item over a pipe.** Declare handlers that take and return
   arrays, and call them once per tick with every item. That is one round trip per tick
   per serverlet, so the cost scales with the number of serverlets, not the number of
   items. In library mode, a Python handler named `tick` is sent as a TICK message
   (kind 9) that also carries the tick number and `dt`. The first measurements
   ([benchmarks](../../benchmarks/README.md)) put pipe framing at tens of microseconds,
   while the Python SDK's per-element list encoding dominates large arrays.
2. **Give per-tick calls a budget.** `budget` bounds how long a call waits; a call with
   no reply in time returns right away:

   ```orchestrate
   serverlet EnemyBrain via python(source: "./enemy.py", budget: "2ms", late: "latest") {
       on think(inputs: FrameInput[]) -> EnemyCommand[]
   }
   ```

   - `late: "drop"` (default): the call returns the default value, and the reply is
     discarded when it arrives.
   - `late: "latest"`: the call returns the handler's most recent completed result (the
     default until one exists), and a late reply becomes that result, so a slow answer
     is used on the next tick instead of stalling this one.

   A queued call whose caller has already given up is never sent, so a slow serverlet
   doesn't build a backlog. Don't budget handlers whose side effects must always run.

3. **Hot per-item code goes in-process.** When profiling shows a serverlet can't fit its
   budget over a pipe, switch it to `line: "embedded"`, or move it to Rust or C++.
4. **Measure before promising numbers.** Round-trip latency — especially the slow tail
   that causes hitches — depends on the OS, the payload size, and the runtime. A
   benchmark harness is a build step (§8), not an assumption.

Rule of thumb:

| The code… | Put it on |
|---|---|
| Reacts to events (a quest completes, a request arrives, a file changes) | `pipe` |
| Runs once per tick with a small, batched input | `pipe` with a budget, or `embedded` |
| Runs per item per tick, or does heavy math | `embedded`, Rust, or C++ |

---

## 6. Library mode

Implemented API and packaging: [library-mode.md](../library-mode.md). The sketch below
is the longer-term direction; current `start` returns a Result, `tick(dt)` and
`shutdown()` are async, and batching/budgets are not implemented yet. Assets are
embedded and extracted per instance rather than copied beside the host binary.

Today `orchestrate build` produces a program that owns `main` (`#[tokio::main]`) and
`stop_orch()` calls `std::process::exit`. A host application — a game engine, a
simulation, a desktop app — owns its own main loop, so:

- `orchestrate build --lib scripts/main.orch -o crates/scripts` emits a Rust crate the
  host adds to its `Cargo.toml`.
- The crate exposes a small, stable surface:

  ```rust
  // host/src/main.rs
  let scripts = scripts::start(runtime.handle(), AppHost::new());
  loop {
      let input = collect_tick_input(&state);
      let output = scripts.tick(dt, input);   // bounded by each serverlet's budget
      apply(&mut state, output);
  }
  scripts.shutdown();                        // runs on_stop, closes every line
  ```

- `AppHost` implements a generated `Host` trait — one method per `host` function.
- An `on_tick(dt: float) { }` lifecycle hook (a sibling of `on_start` / `on_stop`) is
  the orchestration's per-tick entry point. Hosts without a loop never call `tick`.
- `stop_orch()` in library mode asks the host to stop; it never exits the process.
- Sidecar sources and their SDKs are copied next to the host binary at build time, the
  same way secret serverlet binaries are today.

---

## 7. What's genuinely hard (so we plan around it)

1. **Data across the line.** The secret serverlet wire now carries `int`, `float`, `bool`,
   `string`, arrays, and same-file structs in binary. Real interfaces need structs and arrays, and
   per-tick batches need a binary encoding. **Plan:** protocol v1 (below) with typed
   binary values before any per-tick work.
2. **Interface drift.** A Python method renamed without updating the `.orch`
   declaration must fail loudly at startup, not silently at the first call. **Plan:** the
   handshake carries the foreign side's handler names and types; the orchestrator
   compares them with the declaration and refuses to start on a mismatch.
3. **Calls in both directions on one pipe.** A handler for `complete` may call
   `world.spawn` before replying. **Plan:** every frame carries a kind and a call id, so
   host calls and replies can interleave.
4. **Runtime discovery and shipping.** End users won't necessarily have Python
   installed. **Plan:** configurable runtime paths during development; bundling (an
   embedded runtime, or a runtime shipped with the application) is a separate, later
   decision.
5. **Embedded runtimes are heavy dependencies.** pyo3 and other interpreter embeddings add
   build complexity and binary size. **Plan:** each is opt-in and only linked when a
   serverlet uses `line: "embedded"` for that runtime.
6. **Not a security boundary.** A pipe landline runs as the same OS user as the host,
   and an embedded one shares its memory. Untrusted third-party plugins need sandboxed
   serverlets. Docs must say this as plainly as the secret serverlet docs do.

### Protocol v1 (implemented for secret serverlets; landline extensions proposed)

Frame: `[u32 length][u8 kind][u32 call_id][payload]`, little-endian. Length
covers the kind, call ID, and payload (not the length prefix). Implemented kind IDs
are HELLO=1, READY=2, CALL=3, REPLY=4, ERROR=5, HOST_CALL=6, HOST_REPLY=7,
BYE=8, TICK=9.

For secret serverlets and implemented Python/TypeScript landlines, HELLO uses call ID 0 and encodes an i64 version followed by
an array of signature strings such as `echo(int)->int`, in declaration order.
READY has call ID 0 and an empty payload. CALL starts with an i64 handler index
(zero-based), followed by arguments in parameter order. REPLY contains the return
value (empty for void); ERROR contains a string. Both echo the CALL ID.
BYE uses call ID 0 and an empty payload. Struct signatures currently identify types
by name; this handshake does not compare the fields of same-named structs.

| Kind | Direction | Payload |
|---|---|---|
| `HELLO` | foreign → orchestrator | Protocol version, handler names and types |
| `READY` | orchestrator → foreign | Granted host functions |
| `CALL` | orchestrator → foreign | Handler id, arguments |
| `REPLY` / `ERROR` | foreign → orchestrator | Return value, or an error message |
| `HOST_CALL` | foreign → orchestrator | Host function id, arguments |
| `HOST_REPLY` | orchestrator → foreign | Return value |
| `TICK` | orchestrator → foreign | Tick number, dt, batched inputs |
| `BYE` | either | Clean shutdown |

Values: `int` as i64, `float` as f64, `bool` as u8, `string` as u32 length + UTF-8,
arrays as u32 count + elements, and structs as fields in declaration order.

Secret serverlets currently log handler errors and return the return type’s default value;
state mutations before a panic persist.

In library mode, a Python or TypeScript landline handler named `tick` is called with TICK
instead of CALL. Its payload starts with the i64 tick number and the f64 `dt`, followed by
the handler id and arguments exactly as in CALL; the reply is an ordinary REPLY or ERROR.

For Python host integration, READY carries an array of granted signatures such as
`world.record(int)->int` (an empty payload remains valid for no grants). HOST_CALL
contains an i64 index into that grant list followed by typed arguments. HOST_REPLY
echoes the host call ID and contains a bool success flag, then the return value on
success or a string on failure. Host call IDs are independent of CALL IDs.

A foreign exception becomes an `ERROR` reply that the caller receives as an error. A
dead process triggers `on_crash` and the serverlet's restart policy.

---

## 8. Build steps (ordered, each independently shippable)

The order follows what the first adopter needs soonest. Every step is a general language
feature.

**v0.2.0 — first working landline**

1. **[IMPLEMENTED] Language prerequisites.** Array indexing, unary minus, and `%`. Foreign-code glue
   hits all three immediately.
2. **[IMPLEMENTED] Structs and arrays over the serverlet wire**, on secret serverlets first, where
   both ends are generated Rust and easy to test.
3. **[IMPLEMENTED] Protocol v1** with the handshake and interface check; secret serverlets move to it.
4. **[IMPLEMENTED] `via python(source: ...)` with body-less handlers** — parser, typechecker, codegen.
5. **[IMPLEMENTED] Python SDK + first pipe landline end to end.** *Runtime test: a Python serverlet
   keeps state across calls, and a renamed handler fails at startup.*

**v0.3.0 — host integration**

6. **[IMPLEMENTED] Library mode** (`build --lib`, `start` / `tick` / `shutdown`, `on_tick`).
7. **[IMPLEMENTED] Host API + grants** (`host` blocks, `grant call`, `HOST_CALL` frames).
8. **[IMPLEMENTED] Tick batching, budgets, and late-result policy.** Budgets and
   `late: "drop" | "latest"` are landline options; batching uses array handlers (§5).
9. **[IMPLEMENTED] Benchmark harness** — round-trip latency, including the slow tail, per
   runtime and payload size: [benchmarks/landline_latency](../../benchmarks/README.md).

**Host integration for the first adopter.** Requirements found while embedding the
library in a Rust game engine; each is a general feature for any Rust host.

- **[IMPLEMENTED]** Builds inside another Cargo workspace; generated crates use edition
  2024 and `rust-version = "1.89"`, or the one `build --lib --rust-version` asks for.
- **[IMPLEMENTED]** Synchronous driving: `*_blocking` methods; a current-thread runtime
  runs nothing between calls.
- **[IMPLEMENTED]** Host-fired events: `Scripts::trigger_<event>`, queued per instance and
  handled before and after each tick.
- **[IMPLEMENTED]** Typed ticks: `on_tick(dt, input) -> Output`, with a TICK message for
  Python handlers named `tick`.
- **[IMPLEMENTED]** Deterministic mode for lifecycle and event hooks.
- **[IMPLEMENTED]** `on_fixed_tick(step)` and `Scripts::fixed_tick`.
- **[IMPLEMENTED]** Host logging (`Host::log`) and a configurable shutdown grace period.
- **[IMPLEMENTED]** Embedded standard library, and `build --lib --target <triple>`.
- **Planned:** a web-compatible library subset without child processes, built only with
  Tokio features that compile for `wasm32-unknown-unknown`.

See [library-mode.md](../library-mode.md) for the API.

**More languages, FFI first** — step 12 shipped in v0.4.0; steps 10, 11, and 13 are planned.

Per the [design philosophy](../design-philosophy.md), a language that can export C-callable
functions uses FFI, not a landline.

10. **Richer C-ABI types** — `string`, arrays, and structs for C, C++, Zig, and Swift (today
    only `int`, `float`, and `bool`), plus an opaque handle type whose native object is
    released when its owner stops. A serverlet holding a handle covers stateful native objects.
11. **C# via .NET Native AOT** — `[UnmanagedCallersOnly(EntryPoint = "...")]` exports linked
    into the host; the .NET runtime and garbage collector come with them.
12. **[IMPLEMENTED] More languages (v0.4.0)** — Zig (`export fn`) and Swift (`@_cdecl`, or
    `@c` on Swift 6.3) through `load_foreign`, for the host target. TypeScript FFI and pipe
    landlines: TypeScript 7 checks every source, then scriptc or Bun compiles the same
    persistent protocol executable. See the
    [TypeScript SDK guide](../../sdk/typescript/README.md).
13. **Stateful FFI serverlets, only if needed** — a serverlet whose state lives in a native
    object, if handles (step 10) prove too awkward in practice.

**v0.5.0 — high-frequency paths**

14. **`line: "embedded"`** — Python via pyo3.
15. **Hot reload for pipe landlines** — restart a child with new code when its interface
    hasn't changed.

---

## 9. Definition of done (v0.2.0 slice)

Implemented: Python pipe declarations, typed values, state, signature checking,
exception replies, process restart after a failed call, portable source/SDK bundles,
and snapshot/runtime tests. CI installs Python 3.10. The initial restart behavior is
fixed: invoke `on_crash`, return a default for the failed call without replaying it,
and restart for subsequent calls. Configurable restart policies remain future work.
See the [Python SDK guide](../../sdk/python/README.md) and
[TypeScript SDK guide](../../sdk/typescript/README.md) for current limits.


- `serverlet X via python(source: "...") { on h(...) -> T }` compiles; callers use
  the unchanged `XClient`.
- The Python process is long-lived, keeps state between calls, and restarts per
  `on_crash` and the restart policy when it dies.
- `int`, `float`, `bool`, `string`, arrays, and structs round-trip.
- An interface mismatch between the `.orch` declaration and the foreign source fails at
  startup with a message naming the handler.
- No sockets anywhere.
- Snapshot and runtime tests cover it; CI installs Python.

---

## 10. Open questions

1. **Process granularity.** One child process per serverlet (simplest, like secret
   serverlets), or one per runtime shared by several serverlets (fewer processes, less
   isolation)? Proposal: one per serverlet first, add sharing later.
2. **Body-less handler syntax.** Is `on h(x: int) -> int` without braces clear enough,
   or should foreign handlers be marked explicitly (e.g. `extern on h(...)`)?
3. **SDK names and packaging** — e.g. `orchestratelang` on PyPI,
   `@orchestratelang/landline` on npm, `OrchestrateLang.Landline` on NuGet.
4. **Shipping runtimes to end users** — embed, bundle, or require? Decide per platform
   before the first adopter ships.
5. **Which runtime gets `embedded` first** depends on which high-frequency code the first
   adopter actually has; the order in §8 is a default, not a commitment.
