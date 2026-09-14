# Landline Serverlets — Design & Build Plan

> Status: **📋 DESIGN — not implemented.** Syntax, protocol details, and SDK names
> below are proposals. What *is* settled: a serverlet's handler bodies can be written in
> another language and run behind a **landline** (a connection with no sockets);
> OrchestrateLang can build as a Rust library that a host application links; and the
> design has to serve both event-driven and high-frequency calls.
>
> **First adopter:** the scripting layer of a Rust game engine (Python, C#, TypeScript,
> and C++ scripts). It sets the *priority* of this work, not its shape — nothing below
> is game-specific. "Host" means any Rust application that links OrchestrateLang, and
> "tick" means any update loop the host drives.

---

## 0. The serverlet family (taxonomy)

| Type | Where the body runs | Primary property | Status |
|---|---|---|---|
| **Serverlet** | In-process tokio actor, written in OrchestrateLang | Speed, simplicity | ✅ Shipped |
| **Secret serverlet** | Separate OS process, written in OrchestrateLang | Secrecy + crash isolation | ✅ Shipped ([secret-serverlets.md](secret-serverlets.md)) |
| **Sandboxed serverlet** | WASM guest (`wasmtime`) | Containment of hostile code | 🚧 Steps 1–2 of 8 ([sandboxed-serverlets.md](sandboxed-serverlets.md)) |
| **Landline serverlet** | A foreign runtime — Python, TypeScript, C#, C++ — over a pipe or embedded in-process | **Polyglot handler bodies** | 📋 This doc |

A landline serverlet is the actor-style polyglot path from
[roadmap.md](../roadmap.md) §1a. It keeps the serverlet contract — callers use the same
generated `XClient` — and swaps what sits at the other end of the line.

---

## 1. The problem it solves

OrchestrateLang exists to coordinate code that runs in separate runtimes
([serverlet-files.md](serverlet-files.md) §1). Today a serverlet's body can only be
written in OrchestrateLang, and `load_foreign` covers stateless calls into Rust, C, and
C++. There is no way to write a **stateful, long-lived** piece of a program in Python,
TypeScript, or C# and talk to it the way you talk to any other serverlet.

Landline serverlets close that gap. OrchestrateLang declares each foreign serverlet's
interface, starts and supervises its runtime, routes calls and events to it, and
compiles all of that coordination into Rust.

**Example — the first adopter.** A Rust game engine wants gameplay scripts in the
language that suits each job: Python for quests and dialogue, C# for gameplay code,
TypeScript for UI and tools, C++ for low-level plugins. OrchestrateLang becomes the
coordinator of that scripting layer, compiled into a crate the engine links. The same
shape fits any Rust host with a plugin or scripting layer: simulations, desktop apps,
servers.

---

## 2. What compiles to Rust (and what can't)

Be precise about this, because "it all compiles to Rust" is only true of the middle:

- **Compiles to Rust:** the orchestration (events, processes, supervision), every
  landline's client and protocol code, the host API that foreign code calls back into,
  and any serverlet written in OrchestrateLang or Rust.
- **Linked natively:** C and C++ via FFI — no runtime, no pipe.
- **Does not compile to Rust:** Python, C#, and TypeScript. They run in CPython, .NET,
  and a JavaScript engine (Node or Bun, or an embedded V8). TypeScript is compiled to
  JavaScript first; the TypeScript 7 compiler produces JavaScript, not native code.

The host sees one Rust crate. The polyglot part happens at the edges of it.

---

## 3. Architecture

```
Host application (Rust, owns its main loop) — e.g. a game engine
 └─ scripts crate  ← compiled from .orch files ("library mode", §6)
     ├─ orchestration: events, automatic/triggered processes, supervision
     ├─ host API: host functions foreign code is granted access to
     ├─ Rust / C++ code ─────────────── linked in directly
     ├─ line: "embedded" ────────────── runtime hosted inside the host process
     │     ├─ Python (pyo3)             ← high-frequency calls
     │     ├─ JavaScript (V8)
     │     └─ .NET (hosted CoreCLR)
     └─ line: "pipe" ────────────────── stdin/stdout to a child process (sidecar)
           ├─ python  quests.py         ← event-driven calls
           ├─ node    ui.js
           └─ dotnet  Plugins.dll
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
| **Good for** | Event handlers, services, tools, UI (e.g. quests, mod hooks) | High-frequency and per-item work (e.g. per-frame logic) |

Both lines share the same OrchestrateLang syntax and generated client, so moving a
serverlet from `pipe` to `embedded` when profiling says so is a one-word change.

### How much already exists

- **Pipe lines** generalize the shipped secret serverlet transport: a long-lived child,
  a readiness handshake, length-prefixed frames on stdio, EOF as shutdown, and crash
  detection. Today both ends are generated Rust; a landline puts a Python, Node, or .NET
  process on the far end, with a small SDK that speaks the protocol.
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

serverlet EnemyBrain via csharp(source: "./EnemyBrain.csproj", line: "embedded") {
    on think(input: FrameInput) -> EnemyCommands
}

serverlet Hud via typescript(source: "./hud.ts") {
    on show_toast(message: string)
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

The TypeScript and C# SDKs follow the same pattern: implement the handlers as
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
naive design fails, so they get explicit rules.

1. **Batch per tick, never per item over a pipe.** The host calls the orchestration
   once per tick; the orchestration sends **one** tick message per runtime carrying all
   the inputs, and gets one batched reply. The cost scales with the number of runtimes,
   not the number of items.
2. **Every per-tick call has a budget.** If a serverlet misses it, the tick continues
   without that result. A late result is either applied on the next tick or dropped,
   per serverlet:

   ```orchestrate
   serverlet EnemyBrain via python(source: "./enemy.py", budget: "2ms", late: "next_tick") {
       on think(input: FrameInput) -> EnemyCommands
   }
   ```

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

1. **Data across the line.** The wire format today carries only `int`, `float`, `bool`,
   and `string`, each encoded as text. Real interfaces need structs and arrays, and
   per-tick batches need a binary encoding. **Plan:** protocol v1 (below) with typed
   binary values before any per-tick work.
2. **Interface drift.** A Python method renamed without updating the `.orch`
   declaration must fail loudly at startup, not silently at the first call. **Plan:** the
   handshake carries the foreign side's handler names and types; the orchestrator
   compares them with the declaration and refuses to start on a mismatch.
3. **Calls in both directions on one pipe.** A handler for `complete` may call
   `world.spawn` before replying. **Plan:** every frame carries a kind and a call id, so
   host calls and replies can interleave.
4. **Runtime discovery and shipping.** End users won't necessarily have Python or .NET
   installed. **Plan:** configurable runtime paths during development; bundling (an
   embedded runtime, or a runtime shipped with the application) is a separate, later
   decision.
5. **Embedded runtimes are heavy dependencies.** pyo3, V8, and .NET hosting each add
   build complexity and binary size. **Plan:** each is opt-in and only linked when a
   serverlet uses `line: "embedded"` for that runtime.
6. **Not a security boundary.** A pipe landline runs as the same OS user as the host,
   and an embedded one shares its memory. Untrusted third-party plugins need sandboxed
   serverlets. Docs must say this as plainly as the secret serverlet docs do.

### Protocol v1 (proposal)

Frame: `[u32 length][u8 kind][u32 call_id][payload]`, little-endian.

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

A foreign exception becomes an `ERROR` reply that the caller receives as an error. A
dead process triggers `on_crash` and the serverlet's restart policy.

---

## 8. Build steps (ordered, each independently shippable)

The order follows what the first adopter needs soonest. Every step is a general language
feature.

**v0.2.0 — first working landline**

1. **Language prerequisites.** Array indexing, unary minus, and `%`. Foreign-code glue
   hits all three immediately.
2. **Structs and arrays over the serverlet wire**, on secret serverlets first, where
   both ends are generated Rust and easy to test.
3. **Protocol v1** with the handshake and interface check; secret serverlets move to it.
4. **`via python(source: ...)` with body-less handlers** — parser, typechecker, codegen.
5. **Python SDK + first pipe landline end to end.** *Runtime test: a Python serverlet
   keeps state across calls, and a renamed handler fails at startup.*

**v0.3.0 — host integration**

6. **Library mode** (`build --lib`, `start` / `tick` / `shutdown`, `on_tick`).
7. **Host API + grants** (`host` blocks, `grant call`, `HOST_CALL` frames).
8. **Tick batching, budgets, and late-result policy.**
9. **Benchmark harness** — round-trip latency, including the slow tail, per runtime and
   payload size.

**v0.4.0 — more languages**

10. **TypeScript SDK** (Node or Bun on a pipe).
11. **C# SDK** (.NET on a pipe).
12. **Stateful C++ serverlets in-process**, built on the existing C++ FFI.

**v0.5.0 — high-frequency paths**

13. **`line: "embedded"`** — Python via pyo3 first, then JavaScript via V8, then hosted
    .NET.
14. **Hot reload for pipe landlines** — restart a child with new code when its interface
    hasn't changed.

---

## 9. Definition of done (v0.2.0 slice)

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
