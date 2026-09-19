# Library mode and host integration

`orchestrate build --lib main.orch -o generated/scripts` generates and checks a Rust
crate named `scripts`. Add it as a Cargo path dependency in a Rust application.
The host owns its Tokio runtime; the library never creates a runtime or exits the
host process.

Generated crates use edition 2024 and `rust-version = "1.89"`, so the host needs Rust
1.89 or newer. Pass `build --lib --rust-version <x.y[.z]>` to declare a different one;
edition 2024 makes 1.85 the floor. They build inside another Cargo workspace: the compiler's cache crate
declares its own empty `[workspace]`, and the output crate joins the host's workspace as
an ordinary path dependency. Python pipe landlines require Python 3.10+. TypeScript
landlines and FFI require TypeScript 7 and Bun while compiling; their generated
executables are packaged with the result.

Run the complete example from the repository root:

```sh
cargo run --bin orchestrate -- build --lib examples/library_host/scripts.orch -o examples/library_host/generated/scripts
cargo run --manifest-path examples/library_host/Cargo.toml
```

## Host lifecycle

```rust
let mut scripts = scripts::start(runtime.handle(), AppHost::new())?;
scripts.ready().await?;
while !scripts.stop_requested() {
    scripts.tick(dt).await?;
}
scripts.shutdown().await?;
```

`start(&tokio::runtime::Handle, impl Host)` returns `Result<Scripts, String>`; use
`start_with_options` to pass [`StartOptions`](#start-options). Each call initializes an
independent instance and schedules its coordinator on the supplied runtime. Use `()` as
the host when there are no host functions.

- `ready().await` waits for top-level bindings, `on_start`, and event registration.
  It does not wait for the orchestrator body or every sidecar handshake to finish.
- `tick(dt).await` handles queued events, runs the `on_tick` hooks once in declaration
  order, then handles the events those hooks queued. The method requires mutable access,
  so ticks from one handle are serialized.
- `fixed_tick(step).await` does the same for `on_fixed_tick(step: float)` hooks, for
  fixed-rate updates alongside the per-frame `tick`. Each call runs only its own hooks.
- `stop_orch()` sets this instance's stop-request flag. It does not exit Rust,
  automatically shut down the library, or affect other instances. A later tick returns
  an error; the host should call `shutdown()`.
- `shutdown().await` cancels a pending tick, stops the orchestrator task, runs
  `on_stop`, aborts owned workers, and closes sidecars. Idle pipe children receive BYE;
  children with unfinished calls are terminated after `StartOptions::shutdown_grace`.
  It leaves the host runtime alive.
- Dropping `Scripts` requests shutdown in the background. Await `shutdown()` for
  deterministic cleanup and error reporting, before dropping the Tokio runtime.

Lifecycle hooks and host declarations belong in the entry file. Top-level bindings
are shared by its lifecycle hooks. The `main` orchestrator accepts no parameters or
one `process[]` parameter for worker supervision. Event registries, owned tasks,
assets, host implementations, and stop flags are isolated per instance.

### Start options

| Field | Default | Meaning |
|---|---|---|
| `deterministic` | `false` | Run lifecycle and event hooks on host-driven time ([below](#deterministic-mode)) |
| `python` | `None` | Python executable for Python landlines; otherwise `ORCH_PYTHON`, then `python3` |
| `shutdown_grace` | 3 seconds | How long shutdown waits for sidecar tasks before aborting them; zero is allowed |

```rust
let options = scripts::StartOptions {
    shutdown_grace: std::time::Duration::ZERO,
    ..Default::default()
};
let mut scripts = scripts::start_with_options(runtime.handle(), AppHost::new(), options)?;
```

## Driving from synchronous code

Hosts without async code call the blocking methods with the runtime they own:

```rust
let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
let mut scripts = scripts::start(runtime.handle(), AppHost::new())?;
scripts.ready_blocking(&runtime)?;
while !scripts.stop_requested() {
    scripts.tick_blocking(&runtime, dt)?;
}
scripts.shutdown_blocking(&runtime)?;
```

`ready_blocking`, `tick_blocking`, `fixed_tick_blocking`, and `shutdown_blocking` call
`runtime.block_on` for you. With a current-thread runtime, all library work (workers,
event handlers, landline I/O) runs only while one of these calls is in progress; nothing
runs between them. A multithreaded runtime keeps running background work between calls.

Lifecycle code and host implementations must cooperate with the runtime: CPU-bound
loops, blocking host methods, or a stuck `on_start`/`on_stop` hook can delay a tick or
shutdown. A landline `budget` bounds slow Python or TypeScript calls, but not Rust host methods or the
hooks themselves.

### Ticking on the calling thread

`tick_blocking` sends a command to the library's coordinator task and parks until the reply
arrives: a runtime entry, a task wake, a channel send, and a oneshot per frame, plus the
runtime's poll syscall when it parks. `tick_sync` runs the same tick on the calling thread
instead:

```rust
let output = scripts.tick_sync(&runtime, dt)?;   // tick_sync(&runtime, dt, input) for a typed tick
scripts.fixed_tick_sync(&runtime, step)?;
```

The hooks run inside the caller, without entering the runtime: the library spawns tasks
and creates timers through the handle it was started with, so the calling thread needs no
Tokio context of its own. When the body finishes without waiting on anything — no
serverlet or landline round trip, no `sleep` — the call returns with no task, channel, or
park involved. When it has to wait, the rest of the tick finishes under `runtime.block_on`,
so the result and the side effects are the same as `tick_blocking` would produce; only
where the work runs differs. Events queued before the tick, events the hooks queue,
deterministic mode, and `stop_orch()` behave exactly as they do through `tick_blocking`.
Idle ticks do not touch the event queues at all: a queue is inspected only after something
was put on it.

The glue around the hooks is a few nanoseconds. Every hook body binds its instance once,
so a host call inside it is the trait call and one branch on its result; the program's
state is entered with two flag stores and two loads rather than a lock; the clock advances
with an integer add; and an idle tick builds no event drain. On an Apple M2, an empty
`on_tick` costs about 8 ns per `tick_sync`, and each host call in the body costs what the
host method costs on its own, about 2 ns. The ignored test
`cargo test --test library_tests glue_cost -- --ignored --nocapture` measures this on your
machine.

`tick_sync` is for hosts that drive the library from synchronous code, like the blocking
methods; call it from outside any Tokio context, since a waiting body blocks the caller.
The channel path stays available and unchanged for hosts that prefer it, including hosts
that need to cancel a tick with `shutdown()`.

## Runtime drivers

Every library needs Tokio's time driver: `sleep`, landline budgets, and the shutdown grace
period use it. A program needs the IO driver only when a declaration starts a child process
through Tokio, and the entry file alone cannot tell a host that, because the declaration
may sit in an imported module. Where each kind of declaration runs, and what that asks of
the host's runtime:

| Declaration | Runs as | Runtime needs |
|---|---|---|
| Functions, workers, events, in-process serverlets | Tasks on the host runtime | time |
| `load_foreign` Rust, C, C++, Zig, Swift | In-process native call | time |
| `load_foreign "typescript"` (`backend: "scriptc"` or `"bun"`) | One persistent child per module, driven synchronously over `std` pipes; a call blocks its thread until the reply | time |
| `serverlet X via python(...)` (`line: "pipe"`, the only line today) | Persistent child driven through `tokio::process` | time and IO |
| `serverlet X via typescript(...)` (`backend: "scriptc"` or `"bun"`) | Persistent child driven through `tokio::process` | time and IO |
| `serverlet X secret` | Separate native child driven through `tokio::process` | time and IO |

Neither TypeScript backend runs in the host process; `backend` chooses which compiler builds
the child. The generated crate exposes what the whole program needs:

```rust
let mut builder = tokio::runtime::Builder::new_current_thread();
builder.enable_time();
if scripts::NEEDS_IO_DRIVER { builder.enable_io(); }
let runtime = builder.build()?;
```

`scripts::NEEDS_IO_DRIVER` is `true` when any declaration in the entry file or an imported
module is in the last three rows. `build --lib` prints the same list, and `start` and
`start_with_options` check the runtime before spawning anything: a runtime without a driver
the program needs makes them return an error naming the declarations —
`this program needs Tokio's IO driver: python landline serverlet 'Quests' start child
processes through tokio::process; build the runtime with enable_io() or enable_all()` —
where a missing driver used to panic inside a task and leave the next call waiting forever.

Tokio reports a missing driver by panicking, so the check registers a throwaway handle on a
thread named `orchestrate IO driver check` (or `time driver check`) and catches the panic.
When the check fails, the host's panic hook also prints that panic under that thread name,
and a host built with `panic = "abort"` aborts there instead of receiving the error.

## Events from the host

Every `on <event>(...)` block gets a typed method on `Scripts`:

```orchestrate
on hit(damage: int) { world.record(damage) }
```

```rust
scripts.trigger_hit(3)?; // handled during the next tick or fixed tick
```

`trigger_<event>` queues the event for that instance only, and returns an error after
shutdown. Events fired by the host and by scripts (`trigger hit(3)`) wait in a
per-instance queue that is handled in order before and after each tick and fixed tick.
Queued events are never dropped.

Each of those two passes handles the events that were queued when it started, and no
more. An event a handler triggers is handled by the next pass — the one after the tick
hooks, or the one at the start of the next tick — so a handler that triggers its own
event runs a bounded number of times per tick instead of holding the tick open. In
deterministic mode, an event whose handler is still sleeping stays queued for a later
pass in the same way.

## Typed ticks

A single `on_tick` hook may take an input and return an output:

```orchestrate
struct Input { values: int[] }
struct Output { total: int }
on_tick(dt: float, input: Input) -> Output { return brain.tick(input) }
```

The generated method becomes `tick(dt, input: Input) -> Result<Output, String>`, and
`tick_blocking(&runtime, dt, input)`. A typed hook must be the only `on_tick` in the
program.

Calls to a Python or TypeScript landline handler named `tick` are sent as a TICK message
that also carries the tick number and `dt`. Python reads them from `self.tick_number` and
`self.tick_dt`; TypeScript reads `context.tickNumber` and `context.tickDt`. Put arrays in
the input to batch per-item work into one call.

## Deterministic mode

With `StartOptions { deterministic: true, .. }`, the same sequence of `tick(dt)` values
and `trigger_*` calls produces the same host calls, on current-thread and multithreaded
runtimes.

- Time comes from the host: each tick adds its `dt`, and `sleep` inside an event handler
  waits until enough later tick time has passed. No wall clock is consulted.
- Ready events run in FIFO order; events that are sleeping stay queued.
- The orchestrator body runs to completion during startup instead of on its own task.
- Spawned workers (`automatic` blocks), serverlets and landlines, and `sleep` outside
  event handlers are not supported and panic.
- Host implementations must be deterministic themselves.

## Declaring and granting host functions

```orchestrate
host world {
    fn record(value: int) -> int
    fn reset()
}
serverlet Counter via python(source: "counter.py") {
    grant call world.record
    on tick() -> int
}
```

The library exposes one `Host` trait method per declaration:

```rust
impl scripts::Host for AppHost {
    fn world_record(&self, value: i64) -> Result<i64, String> {
        Ok(value)
    }
    fn world_reset(&self) -> Result<(), String> {
        Ok(())
    }
}
```

`Host` is `Send + Sync + 'static`; methods use `&self`. Use interior mutability for
state. Methods execute synchronously on the runtime thread; keep them short and
avoid waiting on a call into the same serverlet. Host errors and panics during a
Python callback become failed HOST_REPLY messages and raise Python `RuntimeError`.
A panic still invokes Rust's panic hook; abort-on-panic builds cannot recover it.

Python calls `self.host.world.record(value)` from a handler. Only explicitly granted
functions appear on that proxy. Rust checks every received function ID against the
serverlet's grants, so forging an ID cannot invoke an ungranted method. Grants may
appear on landlines in imported modules, but refer to the entry file's host groups.
Calls in constructors or outside an active handler are rejected.

Host functions use the existing binary wire types: primitives, arrays, and same-file
structs. Python uses matching dataclasses; TypeScript uses matching object shapes. Their
field order must match the OrchestrateLang struct. Host names must not begin with `_` or
produce colliding `group_function` Rust method names.

OrchestrateLang code can also call `world.record(...)` directly. A returned host
error is logged and produces a default value. Python and TypeScript exceptions retain the
normal landline behavior: log the error, return a default value, and preserve process state.

Grants constrain the generated host-call interface. They do not sandbox Python or
TypeScript or restrict their OS permissions.

## Logging

`Host` has a `log` method whose default prints `Info` messages to stdout and other levels
to stderr. Override it to route output to the host's logger:

```rust
impl scripts::Host for AppHost {
    fn log(&self, level: scripts::LogLevel, message: &str) {
        // forward to the host's logging system
    }
}
```

In library mode, `print` output, runtime diagnostics (such as a failed host call), and
the stderr of Python, TypeScript, and secret serverlet children all go through `log`. `LogLevel` is
`Info`, `Warning`, or `Error`. Rust's process-wide panic hook is outside this logger.

## Packaging

The output directory is compiler-owned and marked `.orchestrate-library`.
Generation refuses to overwrite an existing unmarked directory. Regenerate from
`.orch` sources rather than editing generated files.

Python sources/SDK files, compiled TypeScript executables, and compiled secret children
are embedded as library assets. Each instance extracts its own temporary directory, then
removes it on shutdown. The linked host binary does not depend on the original `.orch`,
Python, or TypeScript source locations. Python itself and third-party packages remain
external; point `StartOptions::python` at the interpreter to use. TypeScript assets do not
need Bun, TypeScript, or scriptc on the player's machine, but data and native dependencies
the implementation uses still need packaging. Foreign C, C++, Zig, and Swift sources are
compiled by the generated crate's build script, so they still reference their source files
and require their toolchain (a C/C++ compiler, `zig`, or `swiftc`) when the host builds it.

A Rust foreign module may declare Cargo dependencies in its sidecar, and `build --lib`
adds more with `--dependency '<name> = <spec>'` or `--dependencies <file.toml>`; see the
[language reference](language-reference.md#cargo-dependencies-for-foreign-rust). They go
into the generated `Cargo.toml` in name order. A relative `path` is resolved when the crate
is generated — against the sidecar for a sidecar declaration, against the working directory
for the command line — and recorded absolute, so the output crate builds wherever it is
placed, but it references that directory rather than carrying a copy.

Secret children are compiled for the compiler machine's target by default. Pass
`build --lib --target <triple>` to build them, and check the library, for another target;
the matching Rust target and linker must be installed. The standard library (`lists`,
`strings`) is embedded in the compiler, so it works from an installed `orchestrate`.

TypeScript executables are built for the compiler host. A library containing a TypeScript
FFI module or landline rejects a different `--target` rather than packaging an incompatible
executable. Zig and Swift FFI also build for the host target only; their build script fails
with a clear message when the target differs.

Bound slow landline calls made from `on_tick` with a landline `budget` and `late` policy,
and batch per-item work by passing arrays; see
[landline-serverlets.md](design/landline-serverlets.md) §5. To measure call latency on
your machine, see [benchmarks/README.md](../benchmarks/README.md).
