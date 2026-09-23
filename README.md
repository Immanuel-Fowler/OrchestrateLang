# OrchestrateLang

> A compiled language for coordinating concurrent work, long-lived services, events,
> and code written in other languages.

OrchestrateLang (`.orch`) is for the part of a program that decides **what runs, when it
runs, and how the pieces communicate**. Workers, event handlers, supervised processes,
and stateful services are language constructs rather than patterns assembled from async
libraries.

The compiler turns a program into Rust, wires its concurrency through Tokio, and asks
Cargo to produce a native binary. There is no OrchestrateLang VM or interpreter. The same
source can instead be generated as a Rust library, when an application that already owns
its main loop wants to drive it.

It is young. It is a good place to explore a more structural model of orchestration; read
[current boundaries](#current-boundaries) before using it as production infrastructure.

## Install and run

You need [Rust and Cargo](https://rustup.rs/). Clone the repository and install the
compiler:

```bash
git clone https://github.com/Immanuel-Fowler/OrchestrateLang
cd OrchestrateLang
cargo install --path .
```

Then run the smallest example:

```bash
orchestrate run examples/hello.orch
```

The first run builds a generated Cargo project and may take longer; later runs reuse
`.orch_cache/`.

| Command | What it does |
|---|---|
| `orchestrate run main.orch` | Compile and run a program |
| `orchestrate build main.orch -o app` | Build a release binary |
| `orchestrate check main.orch` | Parse and type-check without building |
| `orchestrate check-foreign main.orch` | Run each foreign language's own checker |
| `orchestrate check-foreign main.orch --deep` | Also run mypy, and check the generated Rust |
| `orchestrate build --lib main.orch -o generated/scripts` | Generate an embeddable Rust crate |
| `orchestrate prom add name ./module` | Register a local module under a short name |

`cargo orch` and `cargo orchestrate` accept the same commands and arguments, so the
compiler is usable from inside a Cargo workflow without leaving it.

Examples live in [examples/](examples/) — `serverlet.orch`, `python_landline.orch`,
`sandboxed_plugin.orch`, `supervised_process.orch`, thirty-one in all. Foreign examples
need their own toolchain: Python 3.10+, TypeScript 7 and Bun, Zig, Swift, or the .NET SDK.

## Features

**Types and data.** `int`, `float`, `string`, `bool`, `void`; arrays `T[]`; structs;
enums with payloads; `option<T>`; `result<T, E>`, whose error side may be any type;
function types `fn(A) -> B`; generic functions; and `handle`, an opaque native object
from a foreign module, released through its sidecar's `drop` function when the last owner
drops it. Annotations are optional and inferred from the value.

**Control flow.** `if`/`else`, `while`, `for x in xs`, `for i, x in xs`, `range`,
`break`, `continue`, and `match` with literal and guard patterns and exhaustiveness
checking. Errors travel through `try`/`catch e: Failure` and `?`. Blocks, conditionals,
`match`, and `try` are expressions that produce values. There are closures, the
higher-order builtins `map`, `filter`, `reduce`, `find`, `any`, `all`, the pipeline
operator `|>`, and string interpolation with `"temperature is {reading}"`.

**Orchestration.** `orchestrator main(...)` owns startup, shutdown, and the set of
workers that run. `automatic { ... }` owns recurring work and declares its own restart
policy — `restart: 3`, `always`, `never` — with an `on_crash e { ... }` handler beside
it. `on name(args) { ... }` declares a typed event handler and `trigger name(args)`
multicasts to every handler for that event. `process[a, b]` names the workers an
orchestrator owns, and `trigger update_orchestrator([...])` replaces that set while the
program runs. `on_start` and `on_stop` are lifecycle hooks.

**State, in three rungs.** A `let` at the top level belongs to the instance and only
hooks reach it, because a spawned worker could run while a tick holds it; a `fn`, `task`,
or `process` that names one is a compile error saying so. `shared let` puts a binding
behind one mutex reachable from anywhere, with a one-sentence guarantee: a statement that
touches shared state is atomic with respect to all shared state. A statement may wait, or
touch shared state, not both. A serverlet's state is the third rung: owned by the actor,
touched one call at a time.

**Serverlets: four boundaries, one client.** A serverlet declares handlers and private
state, and `start X()` returns a typed client. Where the body runs is one word in the
declaration and no caller changes:

| Declaration | Body runs | What it gives |
|---|---|---|
| `serverlet X { }` | An in-process Tokio actor | Speed; you wrote it, you trust it |
| `serverlet X secret { }` | A separate native executable | Crash separation, and an implementation the orchestrator binary never contains |
| `serverlet X via python(source: "x.py")` | A persistent Python process | Python's libraries, and state that lives in Python |
| `serverlet X via typescript(source: "x.ts")` | A compiled TypeScript executable | The same, for TypeScript, through scriptc or Bun |
| `serverlet X sandbox(memory_limit: "64mb", timeout: "5s")` | A wasmtime guest | Containment: a memory cap, a per-call timeout, and no import it was not granted |

A landline bounds its own latency with `budget: "2ms"`, and `late: "drop"` or
`late: "latest"` decides what a caller gets when the budget expires. A landline or a
sandboxed serverlet reaches the host only through `grant call world.record`, one line per
function; in a sandbox each grant is exactly one import in the guest's linker, and a
handler calling anything ungranted is refused when the program is checked.

**Modules.** A directory with a `module.orch` is a module and a consent boundary;
`use module analytics: "./analytics"` imports it and `load "helpers.orch"` merges a
sub-file. PROM registers a module under a machine-local name so it can be imported
without a path. The standard library ships `lists` and `strings`, generic over element
types.

**Foreign code.** `load_foreign` attaches functions from eight languages, each declaring
its contract in a `.orch_ffi` sidecar:

| Declaration | How it attaches |
|---|---|
| `load_foreign "rust" "./geometry.rs"` | Injected into the generated module; signatures scanned |
| `load_foreign "c" "./m.c"`, `"cpp"` | Compiled by `cc-rs` and linked |
| `load_foreign "zig" "./v.zig"`, `"swift"` | The language's own compiler, to a static library |
| `load_foreign "csharp" "./Math.cs"` | .NET Native AOT to a shared library, one per module |
| `load_foreign "wasm" "./plugin.wasm"` | Embedded and run under wasmtime, checked against the module's export table |
| `load_foreign "typescript" "./tools.ts"` | A compiled executable per module, over a protocol |

Numbers, booleans, strings, arrays of numbers, structs, and handles cross the C ABI under
one ownership rule: what the foreign side returns is copied and freed by the generated
wrapper, and a parameter is borrowed for the call.

**Library mode.** `orchestrate build --lib` generates a Rust crate instead of a binary.
The host implements a generated `Host` trait, calls `ready`, `tick(dt)`, and `shutdown`,
or their `_blocking` forms, or `tick_sync` to run a tick on its own thread. Ticks can be
typed — `on_tick(dt: float, input: Input) -> Output` — and `on_fixed_tick(step: float)`
runs at a fixed rate. The host fires events with `Scripts::trigger_<event>(...)`, receives
`print` output and diagnostics through `Host::log`, and learns which Tokio drivers the
program needs from `NEEDS_IO_DRIVER`. With `StartOptions { deterministic: true }` the
clock becomes the sum of the host's `dt` values and a tick and event sequence replays
identically.

**Tooling.** `orchestrate check` type-checks without generating code;
`orchestrate check-foreign` runs each foreign language's own checker, and `--deep` adds
mypy and a `cargo check` over the generated Rust. There is an `orchestrate-lsp` language
server with hover types, a VS Code extension, and `cargo orch` for use inside a Cargo
workflow. Rust compile errors are remapped to `.orch` source locations, and
`ORCH_SHOW_GENERATED=1` prints the generated Rust.

## Benchmarks

Every number here comes from a script in `benchmarks/`, regenerated by one command:

```bash
python3 benchmarks/run_all.py
```

It writes CSV, JSON, and Markdown under `benchmarks/results/`, each with an environment
header. The tables below are that output. One run on Apple M2 (macOS-15.3.2-arm64-arm-64bit, arm64), 2026-09-23T21:11:27+00:00, commit `d54e936d93e5` (dirty tree). rustc 1.98.1 (48a229cea 2026-09-01); Python 3.12.7; wasmtime 37.0.3; TypeScript rung included. Round trips in microseconds unless the unit says otherwise; the state rungs are nanoseconds per statement.

### The boundary ladder

The same echo handlers behind every serverlet boundary, and the two rungs below a
serverlet call. Round trips in microseconds: from making the call until the reply
arrives, 200 warm-up calls then 2,000 measured.

#### Payload sweep

Calls back to back, one caller.

| Kind | Payload | Samples | p50 | p90 | p99 | max |
|---|---|---:|---:|---:|---:|---:|
| in-process | int | 2000 | 9 | 12 | 22 | 29 |
| in-process | string-1KB | 2000 | 9 | 13 | 25 | 64 |
| in-process | int[1000] | 2000 | 9 | 12 | 23 | 36 |
| sandbox | int | 2000 | 9 | 13 | 22 | 39 |
| sandbox | string-1KB | 2000 | 10 | 14 | 28 | 48 |
| sandbox | int[1000] | 2000 | 12 | 15 | 21 | 70 |
| secret | int | 2000 | 20 | 40 | 82 | 525 |
| secret | string-1KB | 2000 | 19 | 31 | 39 | 58 |
| secret | int[1000] | 2000 | 24 | 33 | 47 | 81 |
| python | int | 2000 | 20 | 30 | 41 | 167 |
| python | string-1KB | 2000 | 26 | 35 | 50 | 1551 |
| python | int[1000] | 2000 | 607 | 634 | 670 | 715 |
| typescript | int | 2000 | 46 | 59 | 79 | 713 |
| typescript | string-1KB | 2000 | 48 | 61 | 84 | 585 |
| typescript | int[1000] | 2000 | 328 | 1122 | 1648 | 5690 |

The sandbox costing about what an in-process actor costs is the useful surprise: the
wasmtime call is small next to the channel round trip both pay. The Python array row is
the honest one — its SDK encodes element by element, and that cost belongs to the
boundary that chose it. The in-process `int` row runs first in the sweep and can still be
warming; read rows within a kind rather than across the first one.

#### Call-rate sweep

The `int` payload. At 100 Hz and 1 kHz the caller sleeps between calls; at 10 kHz it spins.

| Kind | Rate | Samples | p50 | p90 | p99 | max |
|---|---|---:|---:|---:|---:|---:|
| in-process | unpaced | 2000 | 9 | 12 | 22 | 29 |
| in-process | 100hz | 300 | 16 | 41 | 376 | 689 |
| in-process | 1khz | 1000 | 11 | 24 | 59 | 489 |
| in-process | 10khz | 2000 | 7 | 10 | 21 | 66 |
| sandbox | unpaced | 2000 | 9 | 13 | 22 | 39 |
| sandbox | 100hz | 300 | 18 | 54 | 2569 | 39062 |
| sandbox | 1khz | 1000 | 11 | 15 | 64 | 148 |
| sandbox | 10khz | 2000 | 7 | 21 | 71 | 209 |
| secret | unpaced | 2000 | 20 | 40 | 82 | 525 |
| secret | 100hz | 300 | 39 | 86 | 666 | 1353 |
| secret | 1khz | 1000 | 24 | 66 | 193 | 1896 |
| secret | 10khz | 2000 | 16 | 22 | 44 | 95 |
| python | unpaced | 2000 | 20 | 30 | 41 | 167 |
| python | 100hz | 300 | 61 | 87 | 254 | 340 |
| python | 1khz | 1000 | 28 | 54 | 478 | 41401 |
| python | 10khz | 2000 | 22 | 24 | 35 | 68 |
| typescript | unpaced | 2000 | 46 | 59 | 79 | 713 |
| typescript | 100hz | 300 | 93 | 134 | 239 | 278 |
| typescript | 1khz | 1000 | 59 | 90 | 169 | 870 |
| typescript | 10khz | 2000 | 43 | 51 | 72 | 462 |

This is the table a ticking host should read. A caller that idles between calls pays to
wake the actor or the child every time, and every rung pays it: unpaced medians of 9–46 µs
become 16–93 µs at 100 Hz. The 100 Hz tails come from 300 samples, so three stalls of the
machine set the p99: the sandbox's 2.6 ms p99 in this run was 59 µs in a rerun minutes
later, beside 207 µs for v0.15.1 in the same back-to-back pair.

#### Concurrent callers

The `int` payload with 1, 2, 4, or 8 callers sharing one serverlet; each sample is one caller's round trip.

| Kind | Rate | Samples | p50 | p90 | p99 | max |
|---|---|---:|---:|---:|---:|---:|
| in-process | callers-1 | 2000 | 9 | 12 | 22 | 29 |
| in-process | callers-2 | 2000 | 10 | 15 | 24 | 33 |
| in-process | callers-4 | 4000 | 10 | 16 | 23 | 42 |
| in-process | callers-8 | 8000 | 12 | 19 | 27 | 106 |
| sandbox | callers-1 | 2000 | 9 | 13 | 22 | 39 |
| sandbox | callers-2 | 2000 | 12 | 30 | 323 | 1148 |
| sandbox | callers-4 | 4000 | 24 | 65 | 196 | 426 |
| sandbox | callers-8 | 8000 | 5 | 17 | 53 | 363 |
| secret | callers-1 | 2000 | 20 | 40 | 82 | 525 |
| secret | callers-2 | 2000 | 22 | 36 | 50 | 129 |
| secret | callers-4 | 4000 | 47 | 63 | 79 | 129 |
| secret | callers-8 | 8000 | 101 | 121 | 144 | 212 |
| python | callers-1 | 2000 | 20 | 30 | 41 | 167 |
| python | callers-2 | 2000 | 32 | 40 | 50 | 78 |
| python | callers-4 | 4000 | 66 | 78 | 95 | 130 |
| python | callers-8 | 8000 | 133 | 204 | 473 | 1645 |
| typescript | callers-1 | 2000 | 46 | 59 | 79 | 713 |
| typescript | callers-2 | 2000 | 86 | 170 | 287 | 987 |
| typescript | callers-4 | 4000 | 155 | 189 | 356 | 837 |
| typescript | callers-8 | 8000 | 303 | 347 | 470 | 1166 |

An in-process or sandboxed serverlet keeps its per-call latency under load, because the
actor turns calls over faster than callers queue them. A process-backed boundary does
not, and the queue is visible in every caller's round trip.

#### State rungs

Nanoseconds per statement, each sample a batch of 1,000 statements; `loop-only` is the counting loop by itself.

| Kind | Samples | p50 | p90 | p99 | max |
|---|---:|---:|---:|---:|---:|
| local-let | 2000 | 0 | 0 | 1 | 1 |
| shared-let | 2000 | 10 | 10 | 11 | 32 |
| loop-only | 2000 | 0 | 0 | 1 | 1 |

`local-let` reads zero because LLVM folds the loop; the honest statement is "below the
resolution of this method", and it is what sets the floor for the batch. `shared let` at
about 10 ns a statement is the real rung between free instance state and a serverlet call.

### Library-mode tick cost

Nanoseconds per `tick_sync` on a current-thread runtime, in release mode: the median of
21 rounds of 200,000 ticks after 5 warm-up rounds, next to the `Host` trait method called
through its vtable alone.

| Case | ns per tick (median) | fastest round | p90 round |
|---|---:|---:|---:|
| host_method_alone | 2.09 | 2.01 | 2.23 |
| empty | 7.25 | 6.91 | 7.39 |
| one_call | 10.55 | 8.29 | 11.23 |
| five_calls | 16.19 | 15.92 | 16.93 |
| instance_let | 8.39 | 8.20 | 8.62 |
| shared_let | 13.64 | 13.26 | 13.94 |
| event | 234.49 | 217.57 | 244.99 |
| measured | 111.75 | 109.79 | 115.19 |

Derived from the fastest rounds, which move least between runs: an empty tick costs 6.91 ns; each host call in a tick costs 1.91 ns (five calls minus one, over four), against 2.01 ns for the trait method called through its vtable alone. The empty tick and the one-call tick are within noise of each other at this resolution. Measuring the one hook (`measure hooks`) costs 102.88 ns a tick: two clock readings and the tick's own, three histogram updates, and on one tick in sixteen two readings of the thread's CPU clock.

Medians overlap between cases within a few nanoseconds: the OS moves the thread between core types and clock states during a run, which is why the fastest round is reported beside them. Run on an idle machine, and compare cases within one run.

### Diagnostics coverage

Every program in `tests/error_cases/diagnostics/` is deliberately invalid and must be
rejected. Where it is rejected is the measurement:

| Rejected by | Programs | Meaning |
|---|---:|---|
| `orchestrate check` | 121 | the typechecker's own error, before any code is generated |
| build, before Cargo | 0 | codegen or the driver, still the compiler's own error |
| Cargo, no rustc code | 0 | a `compile_error!` the generator planted, reported through Cargo |
| rustc | 0 | leaked: a rustc error against generated code |
| nothing | 0 | wrongly accepted |

**All 121 invalid programs are rejected by `orchestrate check`**, each with the
compiler's own error before any code is generated; none reaches the build, and none
reaches rustc. `KNOWN_LEAKS.txt`, which the test holds the corpus to exactly, is empty,
so a leak that appears fails the build until it is fixed or listed.

## OrchestrateLang in 30 seconds

Four ideas decide what this language is. Each has a document of its own, linked from its
heading.

### 1. [Concurrency is the structure](docs/design/concurrency-is-the-structure.md)

Five declarations, five owners: `orchestrator` owns lifecycle, `automatic` owns recurring
work, `on` owns reactions, `serverlet` owns state, and `parallel` is a join point. There
is no `spawn` to call and no channel to build.

```orchestrate
fn read_sensor() -> int {
    95
}

let monitor = automatic(restart: always) {
    let reading = read_sensor()
    if reading > 90 {
        trigger overheated(reading)
    }
    sleep(500)
} on_crash error {
    print("monitor crashed: {error}")
}

let alarm = on overheated(reading: int) {
    print("temperature is {reading}")
}

orchestrator main(workers: process[monitor]) {
}
```

The compiler supplies the Tokio tasks, the channels, the event registry, the reply
channels, the panic handling, and the shutdown plumbing.

### 2. [Asynchronous first, synchronous second](docs/design/asynchronous-first-synchronous-second.md)

Coordination compiles to one async core. `fn` is the callable form that cannot wait, and
says so with its own diagnostic rather than letting rustc complain about generated code;
`task` and `process` may wait. The other synchronous edge is ownership: the same source
builds a program that owns its runtime, or a crate that a Rust application drives at
about 7 ns a tick — and in deterministic mode the same tick and event sequence produces
the same host calls byte for byte.

### 3. [Polyglot: coordination and attachment](docs/design/polyglot-coordination-and-attachment.md)

Reaching into other code is two needs, not one. **Attach a function** with
`load_foreign`, in this process, for 1–5 ns. **Coordinate with a service** through a
`serverlet`, which owns state and a lifetime and may live in a child process, another
language, or a sandbox. A program pays only for the runtimes it names: no sandbox means
no `wasmtime` dependency, and no C source means no `cc` and no `build.rs`.

### 4. [As many choices as possible are made in syntax](docs/design/choices-in-syntax.md)

Transport, isolation, budget, capability, and lifetime are words in the declaration they
apply to, not configuration sitting beside it. `secret`, `sandbox(...)`, `via python(...)`,
`shared let`, `grant call`, `budget:`, `restart:` — each is greppable, diffable, and read
by the compiler. Change one and the call site does not move; what changes is the
guarantee, and those are documented per boundary rather than hidden behind a common
denominator.

## Design philosophy

Those four decide what the language is. Eight more decide how it gets built, and they are
the ones a proposal is checked against.

**General-purpose first.** An adopter decides what is built next, never how it is
designed. Every feature has to make sense for any Rust host: a simulation, a desktop app,
a server.

**Compile to Rust; no hidden runtime.** No VM, no garbage collector, no executor of our
own, and all types resolved at compile time. Code in another language brings its own
runtime, and that cost belongs to the boundary that chose it.

**FFI is stateless; serverlets own state.** `load_foreign` is a plain function call and
stays one. Before adding a new kind of serverlet, show that a serverlet plus FFI — with
the right types, such as opaque handles — is not enough.

**Wrap, don't build.** Tokio, Cargo, `cc-rs`, wasmtime, and each language's own
toolchain, instead of inventing runtimes, sandboxes, or package formats. Guarantees are
as good as the wrapped technology, no better.

**Be honest about guarantees.** "Secret" is not encryption; a separate process is not a
security boundary; a grant is not a sandbox. A feature that parses but is not enforced
warns loudly, and gaps found and not yet closed are written down rather than left for
someone to discover.

**The host is in charge.** Embedded as a library, the host owns the main loop, the Tokio
runtime, logging, and process lifetime. The library never exits the process, starts its
own runtime, or writes where the host cannot see.

**Measure before promising.** Performance claims come from benchmarks, including the slow
tail, stated as measurements on a named machine at a named commit. Tables in
documentation are generated from a run, never retyped — including the ones above.

**Finish one story at a time.** A small set of fully working, tested, documented features
beats a sprawl of half-built ones. Done means tests, docs, a changelog entry, and every
example still running.

## Current boundaries

The most important limitations are behavioral, not cosmetic:

- A sandboxed serverlet contains its guest's compute, memory, and reach, as well as
  wasmtime does. It carries what the C ABI carries; an array of strings or structs, or a
  struct holding one, does not cross it, though the other three boundaries carry them.
  Its `grant call` lines are the only way it reaches the host, and only in a library
  build.
- A separate process provides lifecycle and crash separation, not a security boundary.
- Python and TypeScript are the supported landline runtimes today.
- Cross-process serverlet and TypeScript values use an explicit protocol and therefore
  cost more than direct in-process calls.
- Deterministic library mode covers lifecycle and event hooks, not spawned workers or
  serverlets.
- Outside library mode, `on_stop` runs on Ctrl+C; `stop_orch()` exits immediately.
- Cross-compilation of foreign code depends on each foreign toolchain and is limited.
- Some diagnostics can still surface from generated Rust. Set `ORCH_SHOW_GENERATED=1`
  to print the generated source and full Cargo output.

Every gap found and not yet closed is collected in
[docs/limitations-notes.md](docs/limitations-notes.md); release-specific notes are in the
[changelog](CHANGELOG.md).

## Documentation map

- [Language reference](docs/language-reference.md) — syntax, types, compiler behavior,
  and generated-code details
- [Design philosophy](docs/design-philosophy.md) — the principles above in full, indexing
  the four foundations in [docs/design/](docs/design/)
- [Feature designs](docs/features/) — one document per feature: the problem, the design,
  what was hard, and what shipped
- [Library mode](docs/library-mode.md) — host lifecycle, ticks, events, callbacks,
  deterministic execution, and packaging
- [Benchmarks](benchmarks/README.md) — how each measurement above is taken, and its caveats
- [Python SDK](sdk/python/README.md) — Python landline implementation and setup
- [TypeScript SDK](sdk/typescript/README.md) — TypeScript FFI and landlines
- [Roadmap](docs/roadmap.md) — proposed work, clearly separated from shipped behavior
- [Changelog](CHANGELOG.md) — release history and known limitations
- [Contributing](CONTRIBUTING.md) — development and release conventions

## License

OrchestrateLang is available under the [MIT License](LICENSE).
