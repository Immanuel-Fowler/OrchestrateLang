# Design Philosophy

The principles OrchestrateLang is built on. Check new features and design docs against
them; when a principle and a proposal disagree, change one of them on purpose.

## 1. Concurrency is structural

Workers, event handlers, and services are language constructs (`automatic`, `on`,
`serverlet`, `orchestrator`), not library calls. The compiler writes the Tokio tasks,
channels, and `Arc`s. Automatic blocks own looping, triggered blocks own reactions,
serverlets own state, and the orchestrator owns lifecycle.

## 2. General-purpose first

OrchestrateLang is a language in its own right. A project that adopts it — the first is a
Rust game engine — decides **what gets built next**, never **how it is designed**. Every
feature must make sense for any Rust host: a simulation, a desktop app, a server. Say
"host" and "tick", not "engine" and "frame".

## 3. Compile to Rust; no hidden runtime

OrchestrateLang code compiles to native Rust with no VM, interpreter, or garbage
collector, and all types resolve at compile time. Be precise about the edges: code in
another language brings its own runtime (a Python interpreter, the .NET GC), and that
cost belongs to the boundary that chose it.

## 4. Use the cheapest boundary that works

Reaching into other code has a ladder of costs. Pick the lowest rung that meets the
need:

| Rung | Mechanism | Choose it when |
|---|---|---|
| 1 | **FFI** (`load_foreign`) | You need a function. In-process, nanoseconds, no copies. Preferred whenever the language can export C-callable functions. |
| 2 | **Serverlet** | You need state or a long-lived service. |
| 3 | **Secret / landline serverlet** (separate process) | The language can't do FFI, or the code should crash separately, stay private, or reload. |
| 4 | **Sandboxed serverlet** | The code is untrusted. |

## 5. FFI is stateless; serverlets own state

`load_foreign` is a plain function call and stays that way. State lives in serverlets,
which can call FFI functions. Before adding a new serverlet kind (for example a
"stateful FFI serverlet"), show that a serverlet plus FFI — with the right types, such as
opaque handles — is not enough.

## 6. Wrap, don't build

Build on proven technology — Tokio, Cargo, `cc-rs`, wasmtime, each language's own
toolchain — instead of inventing runtimes, sandboxes, or package formats. Guarantees are
as good as the wrapped technology, no better.

## 7. Be honest about guarantees

Names, docs, and diagnostics must not promise more than the implementation delivers.
"Secret" is not encryption; a separate process is not a security boundary; a grant is not
a sandbox. A feature that parses but isn't enforced yet warns loudly.

## 8. Boundaries are explicit and uniform

What the orchestrator may touch is declared, not implied: `module.orch` marks a consent
boundary, `grant call` lists each host function a landline may use, and interfaces are
checked at startup. Capability markers are boring and greppable on purpose.

## 9. The host is in charge

When OrchestrateLang is embedded as a library, the host owns the main loop, the Tokio
runtime, logging, and process lifetime. The library never exits the process, starts its
own runtime, or writes where the host can't see. Behavior the host depends on — tick
order, event order, deterministic replays — is defined and tested.

## 10. Measure before promising

Performance claims come from benchmarks (`benchmarks/`), including the slow tail, and are
stated as measurements on a given machine. Budgets bound what can't be predicted.

## 11. Finish one story at a time

A small set of fully working, tested, documented features beats a sprawl of half-built
ones. A feature is done when it has tests, docs, and a CHANGELOG entry, and every example
still runs.
