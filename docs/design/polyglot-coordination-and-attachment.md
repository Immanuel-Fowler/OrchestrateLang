# Polyglot: coordination and attachment

> One of the four foundational principles. The others are
> [concurrency is the structure](concurrency-is-the-structure.md),
> [asynchronous first, synchronous second](asynchronous-first-synchronous-second.md), and
> [choices made in syntax](choices-in-syntax.md). The operational principles are in
> [design-philosophy.md](../design-philosophy.md).

## The claim

"Polyglot" usually means one runtime hosting many languages, and paying that runtime's
price everywhere. OrchestrateLang splits the idea in two, because reaching into other
code is two different needs wearing one word:

- **Attachment.** You need a *function*. It has no state of its own, you want it in this
  process, and you want it to cost what a function call costs. This is `load_foreign`.
- **Coordination.** You need a *service*. It owns state, it may outlive a call, it may
  need its own runtime, it may be untrusted, and it may crash without taking you with it.
  This is a `serverlet`.

Both are polyglot. They differ in what is crossing the boundary — a call, or a
conversation — and picking the wrong one is the usual way a polyglot system becomes slow.

## Attachment: `load_foreign`

A module directory names a foreign source; a `.orch_ffi` sidecar declares the signatures
the rest of the program may call.

```orchestrate
// math/module.orch
load_foreign "rust"       "./geometry.rs"
load_foreign "c"          "./fastmath.c"
load_foreign "cpp"        "./stats.cpp"
load_foreign "zig"        "./vectors.zig"
load_foreign "swift"      "./calendar.swift"
load_foreign "csharp"     "./Math.cs"
load_foreign "typescript" "./tools.ts"
load_foreign "wasm"       "./plugin.wasm"
```

| Language | How it attaches | A call costs |
|---|---|---|
| Rust | Injected into the generated module; signatures scanned from the sidecar | Nothing: it is the same crate |
| C, C++ | Compiled by `cc-rs`, linked as a static library | 1–2 ns |
| Zig, Swift | The language's own compiler to an object or static library | 1–2 ns |
| C# | .NET Native AOT to a shared library, one per module | About 5 ns |
| WebAssembly | Embedded and run under wasmtime; the sidecar is checked against the module's export table | Sub-microsecond |
| TypeScript | A compiled executable per module, driven over a pipe | Tens of microseconds |

Two of those rows deserve their asterisk in the open. **C# brings .NET's garbage
collector into the process**, which is a cost that belongs to the boundary that chose it;
for a host with a frame budget, keep the hot path allocation-free. **TypeScript FFI is
not in-process**: `scriptc` or `bun build --compile` produces a persistent executable per
imported module, so module state survives between calls and startup is paid once, but a
call is a protocol round trip rather than a jump. The in-process TypeScript path is a
plan, not a feature.

What crosses is the C ABI's set — `int`, `float`, `bool`, `string`, `handle`, arrays of
numbers and booleans, and structs of those — under one ownership rule: what the foreign
side returns is allocated by it and copied and freed by the generated wrapper, and a
parameter is borrowed for the call and must not be kept. `handle` is the exception that
proves the rule about state: an opaque native object, released through the sidecar's
`drop` function when its last owner drops, so a stateful native thing can be held without
a serverlet around it.

## Coordination: one serverlet, four boundaries

A serverlet is a typed service: handlers, private state, and a generated client. Which
boundary its body runs behind is one word in the declaration, and the call site does not
change.

| Declaration | Body runs | Gives you |
|---|---|---|
| `serverlet X { ... }` | An in-process Tokio actor | Speed; you wrote it, you trust it |
| `serverlet X secret { ... }` | A separate native executable | Crash separation, and an implementation the orchestrator binary never contains |
| `serverlet X via python(...)` / `via typescript(...)` | A persistent foreign process | Another language's libraries and state |
| `serverlet X sandbox(...)` | A wasmtime guest, capped and timed | Containment of code you do not trust |

Measured round trips, one caller, on an Apple M2; the full tables, their caveats, and the
machine are in [benchmarks/README.md](../../benchmarks/README.md):

| Boundary | p50 across payloads |
|---|---|
| In-process | 7–14 µs |
| Sandboxed | 7–13 µs |
| Secret child | 17–25 µs |
| Python landline | 24 µs, and 568 µs for a 1,000-element array |
| TypeScript landline | 45 µs, and 314 µs for a 1,000-element array |

The sandbox costing about what an in-process actor costs is the useful surprise: the
wasmtime call is small next to the channel round trip that both pay. The Python array row
is the honest one: its SDK encodes element by element, and that is the boundary's cost,
not the format's.

## Dependencies follow the declaration

A polyglot language that made every program pay for every runtime would not be usable for
any of them. So the generated `Cargo.toml` is a function of the program:

| The program contains | The generated crate gains |
|---|---|
| Nothing foreign | `tokio` |
| C or C++ | `cc` as a build dependency |
| Zig, Swift, or C# | Nothing; their compilers run from `build.rs` |
| A `.wasm` module or a sandboxed serverlet | `wasmtime` |
| A Rust sidecar with `[dependencies]` | Exactly the crates it declares |

Nothing else is added, and generated code is a pure function of the program: event
registries and captured variables are emitted in name order so two builds of one program
produce byte-identical Rust and a host's build cache is not defeated. A library build can
be handed more from outside with `--dependency` and `--dependencies`, where a relative
path resolves against the working directory and is recorded absolute.

## Consent is part of the boundary

Crossing outward is declared too. A landline or a sandboxed serverlet reaches the host
only through `grant call world.record`, one line per function. In a sandbox that grant is
the whole mechanism: each one becomes exactly one import in the guest's wasmtime linker,
nothing is defined for anything ungranted, and a handler that calls an ungranted function
is refused by `orchestrate check` rather than trapping at run time. This is the point
where containment and consent stop being two ideas.

## What it costs

- **The type sets are not identical.** An array of strings or structs, and a struct with
  a string field, cross the in-process, secret, and landline boundaries but not the
  sandbox, which carries what the C ABI carries. `option`, `result`, closures, and
  `handle` cross only in-process. The per-boundary table is in
  [limitations-notes.md](../limitations-notes.md).
- **`on_crash` means something different per boundary**: a handler panic in-process, a
  transport failure on a landline, a trap in a sandbox — where it also cannot reach the
  guest's state, because that state is inside the instance being replaced.
- **A sandbox call's `timeout` includes any host call it makes**, since the guest is
  still inside its call while the host runs. A landline budget has no such coupling.
- **Foreign toolchains are the author's business.** Zig and Swift build for the host
  target only; a TypeScript executable is host-target only; Go works through
  `-buildmode=c-archive` but only one Go library can be loaded per process, which is why
  it is not offered.
