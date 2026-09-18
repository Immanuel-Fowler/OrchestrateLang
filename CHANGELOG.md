# Changelog

All notable changes to OrchestrateLang are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). See
[CONTRIBUTING.md](CONTRIBUTING.md) for the release process. The VS Code extension has its
own changelog in [editors/vscode/CHANGELOG.md](editors/vscode/CHANGELOG.md).

## [Unreleased]

### Added
- Cargo dependencies for Rust foreign modules. A `.orch_ffi` sidecar declares what its
  `.rs` file needs under `[dependencies]`, in Cargo's inline form, and `build --lib`
  accepts more from the host with `--dependency '<name> = <spec>'` and
  `--dependencies <file.toml>`. A relative `path` resolves against the sidecar or the
  working directory, never the output directory, and is recorded absolute. The same crate
  declared twice must be identical, and the generated manifest lists dependencies in name
  order. Until now a foreign Rust file could use only std and tokio.
- `tick_sync` and `fixed_tick_sync` on generated library crates run the tick on the
  calling thread. A body that finishes without waiting returns with no coordinator task,
  command channel, reply, or park; one that has to wait finishes under `block_on`, so the
  outcome matches `tick_blocking`. The program's top-level bindings now live in a struct
  the instance owns rather than on the coordinator task's stack, which is what lets both
  paths reach them; `tick`, `fixed_tick`, shutdown, and deterministic mode are unchanged.
  An idle tick no longer takes the event-queue locks.

### Fixed
- `check-foreign` reports a call to an undeclared function in a C source as an error
  with gcc as well as clang. gcc only warned, so the check passed a file that could not
  link; the check now passes `-Werror=implicit-function-declaration`, and the CI job on
  Linux, which had failed on exactly that test since 0.5.1, passes.

## [0.6.1] - 2026-09-18

Generated code that never changes on its own, and a library that says which Tokio
drivers it needs.

A patch under the rule in CONTRIBUTING.md section 5: nothing that already worked changes
behaviour. The constant is new, the start-time error replaces a panic and a hang, and the
ordering fix changes the generated text, not what it does.

### Added
- `NEEDS_IO_DRIVER` in generated library crates: `true` when a declaration anywhere in the
  program — the entry file or an imported module — starts a child process through Tokio,
  which is a Python or TypeScript landline or a secret serverlet. A host can build its
  runtime from it. `start` and `start_with_options` now check the runtime before spawning
  anything and return an error naming those declarations when it lacks the IO driver, or
  the time driver every program needs, where a missing driver used to panic inside a task
  and leave the next call waiting forever. `build --lib` prints the same list.
  `docs/library-mode.md` gains a "Runtime drivers" section that says, per declaration,
  whether code runs in the host process or as a persistent child and what the runtime must
  enable; TypeScript FFI runs its child over `std` pipes and needs no IO driver.

### Fixed
- Generated code is a pure function of the program. Event registries were emitted in
  HashMap order, and the variables a worker or event handler captures in HashSet order;
  both change between runs, so the same program produced different Rust on consecutive
  builds and a host rebuilt the generated crate every time. Both are emitted in name order
  now, and a test builds a library twice and compares the outputs byte for byte.

## [0.6.0] - 2026-09-17

TypeScript FFI keeps its process, and each TypeScript file chooses its compiler.

### Added
- `backend` on TypeScript declarations: `load_foreign "typescript" "math.ts"
  (backend: "bun")` and `via typescript(source: "tools.ts", backend: "scriptc")` choose
  that file's compiler in source, where the choice belongs to the file rather than to
  whoever runs the build. `auto`, `scriptc`, and `bun` are accepted; `ORCH_TS_BACKEND`
  remains the project-wide default for declarations that say nothing.
- `docs/language-reference.md` now covers closures and function types, generics, enums
  and `match`, `option` / `result` with `?` and `try` / `catch`, string interpolation, and
  the array functions that take closures.

### Changed
- TypeScript FFI keeps one protocol process alive per imported module instead of starting
  a scalar-only executable for every call. Module state persists between calls, and
  process startup and the handshake are paid once rather than on every call. Both scriptc
  and Bun compile the same wire-protocol adapter, so the two backends behave alike.

  A handler that throws fails only its own call: the error reaches the caller as it did
  before, and the module's process and state survive it. Only a broken frame discards the
  process, and the next call starts a fresh one.

### Fixed
- A `parallel` branch that called a synchronous function — a plain `fn` or a foreign
  function — failed to compile, because every branch was joined as if it were a future.
  Each branch is now its own future, so synchronous and awaited calls mix freely, and a
  `parallel` with a single branch is awaited rather than left as an unawaited future.
- The TypeScript and library test suites remove a test's temporary directory when it
  passes. Each run built its own copy of tokio there and nothing removed it, so a day of
  runs filled the disk. A failing test still keeps its directory for inspection.

## [0.5.1] - 2026-09-16

Foreign code is checked by the language that owns it.

### Added
- `orchestrate check-foreign <file.orch>` checks every foreign source a program pulls in
  using that language's own checker, with no codegen and no Cargo invocation: C and C++
  through `cc -fsyntax-only`, Zig through `zig build-obj -fno-emit-bin`, Swift through
  `swiftc -typecheck`, TypeScript through `tsc`, and Python landlines through
  `python -m py_compile`. Each language reports its own diagnostics, so a Zig type error
  reads as a Zig type error.

  TypeScript is checked against its contract, not just on its own: the adapter the check
  stages declares an interface built from the `.orch_ffi` sidecar or the serverlet's
  handlers, so an implementation that compiles but returns the wrong type, or omits a
  declared function, fails the check.

  The command is separate from `build` and `run`, which are unchanged.

- `orchestrate check-foreign <file.orch> --deep` adds the two checks that cannot be done
  cheaply. Python is checked with `mypy` rather than only `py_compile`, which parses but
  infers nothing; the landline SDK is staged where mypy can resolve it, so a handler's
  annotations are checked against how the SDK uses them. Rust is checked by generating the
  program's Rust and running `cargo check` over it.

  Rust needs the deep pass rather than a standalone `rustc`: a `load_foreign` file is
  concatenated with its module's generated code and may call into it, so checking it alone
  reports those calls as undefined. Generating the code first checks the file in the
  context a build gives it, which is why the pass cannot be fast — the first run compiles
  dependencies, later runs reuse the `.orch_cache/` build cache.

  `--deep` requires mypy to be installed rather than skipping when it is missing, since it
  is opted into explicitly. Without `--deep` nothing changes, so the default stays fast.

### Changed
- `typescript::build` is split into `stage_and_check` and `build`, so the TypeScript type
  check can run without compiling a Bun or scriptc executable. Build behaviour is
  unchanged.

### Fixed
- `library_tests` scopes its temporary directories to the test process, as the TypeScript
  tests already did. They were shared across runs, so an interrupted run could leave a
  half-built `.orch_cache` that made the next run fail or hang. Each run now builds from
  a clean directory, which is slower but cannot inherit broken state.

## [0.5.0] - 2026-09-16

`orchestrate` is also a cargo subcommand.

### Added
- `cargo orch` and `cargo orchestrate` run every command the `orchestrate` binary does —
  `run`, `build`, `check`, and `prom` — with the same arguments and exit codes. The two
  spellings are interchangeable; pick whichever reads better in your scripts. `cargo
  install --path .` installs the `cargo-orch` and `cargo-orchestrate` binaries alongside
  `orchestrate`, which is all cargo needs to find them.

  OrchestrateLang stays a standalone compiler: these are thin wrappers over the same
  dispatch, not a proc macro, so `.orch` files remain the unit of compilation and
  `orchestrate` keeps working unchanged.

## [0.4.0] - 2026-09-15

More languages: Zig and Swift FFI, TypeScript FFI through scriptc or Bun, and TypeScript
landline serverlets.

### Added
- Zig FFI: `load_foreign "zig" "./file.zig"` with an `.orch_ffi` sidecar. Functions are
  `export fn`s using `i64`, `f64`, `bool`, or `void`; the generated `build.rs` compiles the
  file with `zig build-obj` and packs it with the platform archiver (Zig 0.16+ on `PATH`).
  Example: `examples/foreign_zig_math.orch`.
- Swift FFI: `load_foreign "swift" "./file.swift"` with an `.orch_ffi` sidecar. Functions
  are exported with `@_cdecl("name")` (Swift 5.10+) or `@c` (Swift 6.3+) using `Int64`,
  `Double`, `Bool`, or no return value; the generated `build.rs` compiles the file with
  `swiftc` and links the Swift runtime. Example: `examples/foreign_swift_math.orch`.
- A clear error when `load_foreign "zig"` or `"swift"` can't find its compiler on `PATH`.
- CI installs Zig 0.16.0.
- TypeScript 7 FFI: `load_foreign "typescript" "./file.ts"` with an `.orch_ffi`
  sidecar. Eligible scalar functions use a native `scriptc` executable; functions using
  full-width integers, strings, arrays, structs, or unsupported native features use the
  compiled Bun transport. Example: `examples/foreign_typescript_math.orch`.
- TypeScript landlines: `serverlet X via typescript(source: "x.ts") { ... }` default-export
  a class whose methods implement the declared handlers. The transport supports protocol-v1
  values, typed ticks, host grants, budgets, late results, logging, and library packaging.
  Example: `examples/typescript_landline.orch`.

### Changed
- The `string` error for C-ABI sidecars now reads `string type is not supported in C-ABI
  FFI signatures (use int, float, bool, or void)`.
- TypeScript source is checked with TypeScript 7 before compilation. Automatic backend
  selection tries scriptc and then Bun; `ORCH_TS_BACKEND=auto|scriptc|bun` chooses the
  policy, and `ORCH_TSC`, `ORCH_SCRIPTC`, and `ORCH_BUN` override tool paths.

### Known limitations
- Zig and Swift sources build for the host target only; `build --lib --target` with a
  different target fails.
- TypeScript FFI is synchronous and starts a fresh process for every call. Use a persistent
  TypeScript landline for state, asynchronous work, or calls that need a budget. TypeScript
  FFI and landline executables are host-target only.

## [0.3.1] - 2026-09-15

Two fixes for embedding library mode in a Rust engine.

### Changed
- Generated library crates declare `rust-version = "1.89"` instead of `1.98.1`, so hosts
  that support Rust 1.89 can depend on them. `build --lib --rust-version <x.y[.z]>`
  declares another version, down to the 1.85 that edition 2024 requires.

### Fixed
- An event handler that triggers its own event no longer hangs `tick` and `fixed_tick`.
  Each pass over the queue handles the events that were queued when it started; events a
  handler triggers are handled by the next pass, in both normal and deterministic mode.

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
- Python and TypeScript are the supported landline runtimes. C#, C++, and embedded runtimes
  are planned.
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
- At the v0.2.0 release, Python was the only landline runtime; TypeScript, C#, and
  embedded runtimes were planned.
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

[Unreleased]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.6.1...HEAD
[0.6.1]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.5.1...v0.6.0
[0.5.1]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Immanuel-Fowler/OrchestrateLang/releases/tag/v0.1.0
