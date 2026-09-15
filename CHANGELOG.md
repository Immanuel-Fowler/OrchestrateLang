# Changelog

All notable changes to OrchestrateLang are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). See
[CONTRIBUTING.md](CONTRIBUTING.md) for the release process. The VS Code extension has its
own changelog in [editors/vscode/CHANGELOG.md](editors/vscode/CHANGELOG.md).

## [Unreleased]

### Changed
- Generated library crates declare `rust-version = "1.89"` instead of `1.98.1`, so hosts
  that support Rust 1.89 can depend on them. `build --lib --rust-version <x.y[.z]>`
  declares another version, down to the 1.85 that edition 2024 requires.

## [0.3.0] - 2026-09-15

Embedding in a Rust engine: synchronous driving, host-fired events, typed and fixed-step
ticks, deterministic replays, host logging, call budgets, and a latency benchmark. See
[docs/library-mode.md](docs/library-mode.md).

### Added
- `clock_micros()` built-in: microseconds on a monotonic clock, for measuring elapsed time.
- Latency benchmark in `benchmarks/landline_latency`: round-trip p50/p90/p99/max for Python
  landlines, secret serverlets, and in-process serverlets with int, string, and array
  payloads (`python3 benchmarks/landline_latency/run.py`).
- Landline call budgets: `via python(source: "...", budget: "2ms")` makes a call that gets
  no reply in time return right away. `late: "drop"` (the default) returns the default
  value; `late: "latest"` returns the handler's most recent completed result, and a late
  reply becomes that result. A queued call whose caller has already given up is not sent.
- Synchronous hosts can drive library mode: `ready_blocking`, `tick_blocking`,
  `fixed_tick_blocking`, and `shutdown_blocking` take the host's Tokio runtime. With a
  current-thread runtime, nothing runs between those calls.
- `Scripts::trigger_<event>(...)` fires `on <event>` blocks from Rust. Events queue per
  instance, run in order before and after each tick, and are never dropped.
- Typed ticks: `on_tick(dt: float, input: Input) -> Output` generates
  `tick(dt, input) -> Result<Output, String>`. A Python landline handler named `tick` is
  sent as a TICK message carrying the tick number and `dt` (`self.tick_number`,
  `self.tick_dt`).
- `on_fixed_tick(step: float)` and `Scripts::fixed_tick(step)` for fixed-rate updates.
- `start_with_options` and `StartOptions`: `deterministic` (host-driven time for lifecycle
  and event hooks), `python` (interpreter path), and `shutdown_grace` (zero allowed).
- `Host::log(LogLevel, &str)` receives `print` output, runtime diagnostics, and child
  process stderr in library mode.
- `build --lib --target <triple>` builds secret serverlet children and checks the library
  for that target.
- The standard library is embedded in the compiler, so `use module lists: "lists"` works
  from an installed `orchestrate`.
- Rust FFI sidecars accept array, `option<T>`, and `result<T>` types.

### Changed
- Generated library crates use edition 2024 and declare a minimum `rust-version`. C/C++
  bindings use `unsafe extern "C"`, and the identifier `gen` is escaped.
- Generated cache crates declare their own empty `[workspace]`, so `build --lib` works
  inside another Cargo workspace; the output crate still joins the host's workspace as a
  path dependency.
- In library mode, events triggered by scripts are queued and handled at tick boundaries
  instead of on a task per handler.

### Known limitations
- Python is the only landline runtime. TypeScript, C#, and C++ landlines and embedded
  runtimes are planned.
- Library mode depends on Tokio's full feature set and child processes, so it does not
  build for `wasm32-unknown-unknown` yet.
- Deterministic mode excludes spawned workers, serverlets and landlines, and `sleep`
  outside event handlers.
- Grants limit which host functions a landline can call; they do not sandbox Python or
  restrict its OS permissions.
- Python itself is not bundled. Hosts ship an interpreter and point
  `StartOptions::python` at it.
- The Python SDK encodes arrays one element at a time, so large arrays are slow (about
  0.6 ms for 1,000 ints in the sample benchmark).
- Adding a string variable to itself (`s + s`) fails to compile.
- A line break does not end an expression, so a line that starts with an operator
  continues the previous line.
- The typechecker accepts payload patterns on unit enum variants (`Color::Red(v)`); the
  Rust build then fails.
- `start <process>` inside an orchestrator is not supported. List processes in
  `process[...]` instead.
- A `try` block cannot call a `task`, because the generated closure is not async. Call
  `fn`s inside `try`.
- Outside library mode, `on_stop` runs only on Ctrl+C; `stop_orch()` exits immediately
  without running it.
- In library mode, Rust's process-wide panic hook still writes to stderr.
- Sandboxed serverlets provide no isolation yet.
- `docs/language-reference.md` does not yet cover string interpolation, generics,
  `option` / `result`, `try` / `catch`, supervision, `check`, the language server, or the
  standard library. See `examples/` for working code.

## [0.2.0] - 2026-09-14

Polyglot serverlets and embedding in a Rust host. See
[docs/library-mode.md](docs/library-mode.md) and
[sdk/python/README.md](sdk/python/README.md).

### Added
- Library mode (`build --lib`): a generated Rust crate with per-instance lifecycle,
  async `ready`/`tick`/`shutdown`, and a stop-request flag that leaves the host alive.
- Host declarations and per-landline grants, a generated Rust `Host` trait, and
  Python-to-Rust callbacks over the existing pipe, including typed error replies.
- Python pipe landlines: `via python(source: "...")` declarations, a bundled typed
  Python SDK, interface checks, persistent state, exception replies, `on_crash`
  recovery after transport failure, and source/SDK bundles beside built binaries.
  Python 3.10+ is required; `ORCH_PYTHON` selects the executable.
- Unary minus (`-x`, `-1.5`) and the `%` operator.
- Array indexing: `items[i]` reads an element and `items[i] = x` replaces one. The index
  must be an `int`, an out-of-range index panics, and `[` only indexes when it is on the
  same line as the value before it.
- Secret serverlet handlers can take and return arrays and structs declared in the same
  file. Values cross the process boundary in a binary encoding.
- Secret serverlets speak serverlet protocol v1: a startup handshake that checks the
  protocol version and every handler signature, a call id on each request, an error reply
  when a handler panics (the serverlet keeps running), and a clean shutdown message.

### Changed
- The secret serverlet wire format is now protocol v1. Rebuild any secret serverlet
  binaries built with 0.1.0; an old child fails the startup handshake.

### Known limitations
- Python is the only landline runtime. TypeScript, C#, and embedded runtimes are planned.
- Tick batching, per-call time budgets, and late-result handling are not implemented, and
  there is no latency benchmark yet.
- Grants limit which host functions a landline can call; they do not sandbox Python or
  restrict its OS permissions.
- In library mode, secret serverlet children are built for the compiler machine's
  target, so cross-compiling the generated crate is not supported.
- A line break does not end an expression, so a line that starts with an operator
  continues the previous line.
- The typechecker accepts payload patterns on unit enum variants (`Color::Red(v)`); the
  Rust build then fails.
- `start <process>` inside an orchestrator is not supported. List processes in
  `process[...]` instead.
- A `try` block cannot call a `task`, because the generated closure is not async. Call
  `fn`s inside `try`.
- Outside library mode, `on_stop` runs only on Ctrl+C; `stop_orch()` exits immediately
  without running it.
- Standard library modules are found only next to the `orchestrate` binary or in the
  current directory, so they are not importable after `cargo install` yet.
- Sandboxed serverlets provide no isolation yet.
- `docs/language-reference.md` does not yet cover string interpolation, generics,
  `option` / `result`, `try` / `catch`, supervision, `check`, the language server, or the
  standard library. See `examples/` for working code.

## [0.1.0] - 2026-09-14

First tagged release.

### Language
- Top-level `automatic` process blocks and `on <event>(...)` triggered blocks, with
  `trigger` for multicast events.
- `orchestrator main(...)` entry point with managed `process[...]` arrays,
  `update_orchestrator` hot-swapping, and `on_start` / `on_stop` hooks.
- `fn` (sync) and `task` (async) functions, the `|>` pipeline operator, and
  `parallel { }` blocks.
- Types: `int`, `float`, `string`, `bool`, `void`, arrays (`T[]`), structs, enums with
  payloads, `option<T>`, `result<T>`, and function types.
- Control flow: `if`/`else`, `while`, `for x in xs`, `for i, x in xs`, `range(...)`,
  `break`, `continue`, and `match` with literal and guard patterns plus exhaustiveness
  checking.
- Error handling: `ok` / `err` / `some` / `none`, `?` propagation, and
  `try { } catch e { }`.
- Closures and higher-order built-ins: `map`, `filter`, `reduce`, `find`, `any`, `all`.
- Generic functions, e.g. `fn identity<T>(x: T) -> T`.
- String interpolation: `"Hello, {name}!"`.
- Conversion built-ins: `to_int`, `to_float`, `parse_int`, `parse_float`.
- Supervision: `automatic(restart: N | always | never) { } on_crash e { }`, and
  `on_crash` handlers inside serverlets.

### Modules and interop
- Directory modules (`use module x: "./path"`), `load` for sub-files, and PROM
  (`orchestrate prom add | list | remove`) for machine-local module names.
- `load_foreign "rust" | "c" | "cpp"` with `.orch_ffi` signature sidecars.
- Standard library modules: `lists`, `strings`.
- Serverlets (in-process actors) and `secret` serverlets (a separate OS process over
  framed stdio IPC).
- **Experimental:** `sandbox(memory_limit: "...", timeout: "...")` serverlets compile their
  handlers to a `wasm32-wasip1` guest, but the guest is not loaded yet. The serverlet
  still runs in-process **without isolation**, and the compiler warns about it.

### Tooling
- `orchestrate run`, `orchestrate build [-o name]`, and `orchestrate check`
  (type-check only).
- `orchestrate-lsp` language server with hover type information.
- Rust compile errors are remapped to `.orch` source locations;
  `ORCH_SHOW_GENERATED=1` prints the generated Rust.
- VS Code extension in `editors/vscode/`.
- Codegen snapshot tests, runtime tests, error-case tests, and CI on Linux and macOS.

### Known limitations
- No array indexing (`items[i]`). Iterate with `for` instead.
- No unary minus (`-1`) or modulo (`%`). Write `0 - 1`.
- A line break does not end an expression, so a line that starts with an operator
  continues the previous line.
- The typechecker accepts payload patterns on unit enum variants (`Color::Red(v)`); the
  Rust build then fails.
- `start <process>` inside an orchestrator is not supported. List processes in
  `process[...]` instead.
- A `try` block cannot call a `task`, because the generated closure is not async. Call
  `fn`s inside `try`.
- `on_stop` runs only on Ctrl+C; `stop_orch()` exits immediately without running it.
- Standard library modules are found only next to the `orchestrate` binary or in the
  current directory, so they are not importable after `cargo install` yet.
- Sandboxed serverlets provide no isolation yet. The generated guest also re-creates
  state on every call and passes `string` across an `extern "C"` boundary.
- `docs/language-reference.md` does not yet cover string interpolation, generics,
  `option` / `result`, `try` / `catch`, supervision, `check`, the language server, or the
  standard library. See `examples/` for working code.

[Unreleased]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Immanuel-Fowler/OrchestrateLang/releases/tag/v0.1.0
