# Design Philosophy

The principles OrchestrateLang is built on. Check new features and design docs against
them; when a principle and a proposal disagree, change one of them on purpose.

Four of them decide what the language *is*, and each has its own document under
[design/](design/). The rest decide how it is built, and are stated here.

## The foundations

### 1. [Concurrency is the structure](design/concurrency-is-the-structure.md)

Workers, event handlers, and services are declarations — `automatic`, `on`, `serverlet`,
`orchestrator` — not library calls. The compiler writes the Tokio tasks, channels, and
`Arc`s. Who may touch a binding is a property of how it was declared.

### 2. [Asynchronous first, synchronous second](design/asynchronous-first-synchronous-second.md)

Coordination is asynchronous work, compiled to one async core. Synchrony appears at the
edges: `fn`, the callable form that cannot wait, and the host API, where an application
that owns its own loop drives that same core. Ownership can invert without the program
changing.

### 3. [Polyglot: coordination and attachment](design/polyglot-coordination-and-attachment.md)

Reaching into other code is two needs, not one. Attach a *function* with `load_foreign`,
in-process and at the cost of a call. Coordinate with a *service* through a `serverlet`,
which owns state and may live in another process, another language, or a sandbox. A
program pays only for the runtimes it names.

### 4. [As many choices as possible are made in syntax](design/choices-in-syntax.md)

Transport, isolation, budget, capability, and lifetime are words in the declaration they
apply to, not configuration beside it. A choice written in the source is local,
greppable, diffable, and checked by the compiler — and the call site does not change when
it changes.

## The practice

### 5. General-purpose first

OrchestrateLang is a language in its own right. A project that adopts it — the first is a
Rust game engine — decides **what gets built next**, never **how it is designed**. Every
feature must make sense for any Rust host: a simulation, a desktop app, a server. Say
"host" and "tick", not "engine" and "frame".

### 6. Compile to Rust; no hidden runtime

OrchestrateLang code compiles to native Rust with no VM, interpreter, or garbage
collector, and all types resolve at compile time. Be precise about the edges: code in
another language brings its own runtime (a Python interpreter, the .NET GC), and that
cost belongs to the boundary that chose it.

### 7. FFI is stateless; serverlets own state

`load_foreign` is a plain function call and stays that way. State lives in serverlets,
which can call FFI functions. Before adding a new serverlet kind (for example a
"stateful FFI serverlet"), show that a serverlet plus FFI — with the right types, such as
opaque handles — is not enough.

### 8. Wrap, don't build

Build on proven technology — Tokio, Cargo, `cc-rs`, wasmtime, each language's own
toolchain — instead of inventing runtimes, sandboxes, or package formats. Guarantees are
as good as the wrapped technology, no better.

### 9. Be honest about guarantees

Names, docs, and diagnostics must not promise more than the implementation delivers.
"Secret" is not encryption; a separate process is not a security boundary; a grant is not
a sandbox. A feature that parses but isn't enforced yet warns loudly. Gaps found and not
yet closed are written down in [LIMITATIONS-notes.md](../LIMITATIONS-notes.md) rather
than left for a user to discover.

### 10. The host is in charge

When OrchestrateLang is embedded as a library, the host owns the main loop, the Tokio
runtime, logging, and process lifetime. The library never exits the process, starts its
own runtime, or writes where the host can't see. Behavior the host depends on — tick
order, event order, deterministic replays — is defined and tested.

### 11. Measure before promising

Performance claims come from benchmarks (`benchmarks/`), including the slow tail, and are
stated as measurements on a given machine with the commit they were taken at. Tables in
documentation are generated from a run's output, never retyped. Budgets bound what cannot
be predicted.

### 12. Finish one story at a time

A small set of fully working, tested, documented features beats a sprawl of half-built
ones. A feature is done when it has tests, docs, and a CHANGELOG entry, and every example
still runs.

---

Per-feature design documents live in [features/](features/); planned work is in
[roadmap.md](roadmap.md).
