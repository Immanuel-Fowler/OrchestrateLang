# Changelog

All notable changes to OrchestrateLang are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this
project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html). See
[CONTRIBUTING.md](CONTRIBUTING.md) for the release process. The VS Code extension has its
own changelog in [editors/vscode/CHANGELOG.md](editors/vscode/CHANGELOG.md).

## [Unreleased]

### Fixed
- **The embedded standard library no longer leaves a temp folder per run.** An installed
  `orchestrate` with no `stdlib/` beside it unpacked `lists` and `strings` into a fresh
  `orchestrate_stdlib_<pid>_<n>` folder in every process and never removed it; 38 had
  piled up on one machine. It now unpacks one copy per compiler build, named by the
  version and a hash of the embedded sources, shared by every process and renamed into
  place whole, so a racing process never reads a half-written copy. Patch-level: how a
  module resolves and compiles is unchanged.
- **Conflicting generic arguments are refused by `orchestrate check`.** `same(1, "x")`
  against `fn same<T>(a: T, b: T)` passed `check` and reached rustc as E0308 against
  generated code, the last program in the diagnostics corpus that did. The typechecker
  now binds each type parameter to one type and names the argument that disagrees; an
  argument whose type is not known yet, such as `[]` or `none`, binds nothing.
  `KNOWN_LEAKS.txt` is empty. Patch-level: a wrong program is refused earlier, and no
  valid program changes.

## [0.15.0] - 2026-09-21

The sandbox catches up with the C ABI, and every claim is measured.

### Added
- **Arrays and structs across the sandbox boundary.** A sandboxed serverlet carried `int`,
  `float`, `bool`, and `string`; it now carries `int[]`, `float[]`, `bool[]`, and structs
  whose fields are numbers, booleans, or such structs — the set the C ABI carries since
  0.14.0, under the same rules. An array crosses as a pointer and a count into the guest's
  memory, a struct as its `#[repr(C)]` bytes, each field at its own offset and padding
  zero, so neither side reads padding or depends on the other's endianness. What the host
  writes for a call it frees after the call; what the guest returns the host copies and
  frees. The guest's allocator is now 8-byte aligned. A handler's state may be of these
  types, and a handler may return it.

  An array of strings or structs, or a struct holding one, does not cross the sandbox,
  though the in-process, secret, and landline boundaries carry them. That gap is written
  down in `docs/limitations-notes.md`.
- **`grant call` on a sandboxed serverlet.** In a library build, a sandboxed serverlet
  takes the same grant lines a landline does, and there they are the only way the guest
  reaches the host at all: each grant is exactly one import defined in the guest's
  wasmtime linker, behind which the `Host` trait method runs; nothing is defined for
  anything ungranted, so it does not exist inside the guest. A handler that calls an
  ungranted host function is refused by `orchestrate check`, which names the call and the
  grant that would allow it. Arguments and results cross in the wire encoding landline
  grants use — numbers, booleans, strings, arrays, and structs. A host error or panic
  fails that one call inside the guest, which logs it and continues with the default. A
  host call counts against the serverlet's `timeout`, since the guest is still inside its
  call while the host runs. Step 7 of `docs/features/sandboxed-serverlets.md`, and the
  point where containment and consent become one mechanism.
- **`on_crash` on a sandboxed serverlet** runs on the host after a call ran past its
  timeout, exhausted its memory, or stopped itself, with the trap's message bound, before
  the guest is replaced and the caller gets the default. It cannot reach the guest's
  state, which is inside the instance being thrown away; a handler that names a state
  binding is a compile error saying so.

- **The boundary ladder benchmark covers every rung.** `benchmarks/landline_latency`
  measures the same echo handlers on the in-process, sandboxed, secret, Python, and
  TypeScript boundaries, and the two rungs below a serverlet call, `shared let` and a
  plain `let`, timed per batch of a thousand statements. Beside the payload sweep there
  is a call-rate sweep — the `int` payload paced at 100 Hz and 1 kHz with the caller
  asleep between calls, and at 10 kHz spinning — and a concurrent-callers sweep with 2, 4,
  and 8 callers sharing one serverlet. `run.py` writes CSV, JSON, and Markdown under
  `benchmarks/results/` with an environment header (CPU, OS, `rustc`, Python, the
  TypeScript toolchain, wasmtime, sample counts, commit SHA), and the README's tables are
  pasted from that output rather than typed. The TypeScript rung is skipped, and said to
  be, when Bun or TypeScript 7 is missing.

- **The tick cost is a committed benchmark.** `benchmarks/tick_cost/run.py` builds six
  small library programs — an empty tick, one and five host calls, an instance `let`
  increment, a `shared let` increment, and one host-fired event per tick — drives each
  through `tick_sync` on a current-thread runtime in release mode, and reports the median
  of 21 rounds of 200,000 ticks beside the `Host` trait method called through its vtable
  alone. It writes CSV, JSON, and Markdown with the same environment header as the
  boundary ladder. The 8 ns tick and 1.5 ns host-call figures from the 0.8.1 notes come
  from this measurement now, not from a release note.

- **Deterministic mode is tested at length.** One scripted sequence of 10,000 ticks and
  events — host-fired events, events fired by handlers, handlers that sleep on host time
  across ticks, and instance state — replayed on fresh instances, on a current-thread and
  a multithreaded runtime, through `tick_blocking` and `tick_sync`, produces a
  byte-identical host-call trace. A second test asserts that each documented exclusion
  (a spawned worker, a serverlet, a landline, a `sleep` outside an event handler) fails
  the way `docs/library-mode.md` now says, with the deterministic-mode message.

- **The diagnostics corpus covers the language.** `tests/error_cases/diagnostics/` grew
  from 30 to 88 deliberately invalid programs, across types, events, serverlets of every
  kind, processes, C, Rust, and WebAssembly sidecars, and module boundaries, with the
  module fixtures they import beside them. `diagnostics_never_leak_rustc` now runs each
  through `orchestrate check` first and reports the count — 81 of 88 are rejected by
  `check`, 6 more by the build before Cargo, 1 reaches rustc — and holds the leaks to
  `KNOWN_LEAKS.txt`, so a leak that appears or disappears fails the test until the list
  says so. `benchmarks/diagnostics_coverage.py` writes the same classification as CSV,
  JSON, and Markdown.

- **One command regenerates every table.** `python3 benchmarks/run_all.py` builds the
  release compiler and runs the boundary ladder, the tick cost, and the diagnostics
  coverage in order, writing every CSV, JSON, and Markdown table under
  `benchmarks/results/` and joining the reports into `results.md`. `benchmarks/README.md`
  documents it and the three scripts.
- **What `orchestrate check` guarantees is written down**, in one paragraph of
  `docs/language-reference.md` before "Debugging Generated Code": what a check does and
  finds, and what it does not — no codegen, no Cargo, library-only rules unenforced,
  module serverlet bodies unwalked, foreign sources unchecked.

### Changed
- **The limitations notes live under `docs/`.** `LIMITATIONS-notes.md` sat at the repo
  root, where CONTRIBUTING.md allows only the README, changelog, contributing guide, and
  license; it is now `docs/limitations-notes.md`, and every reference to it, including
  the comment in `KNOWN_LEAKS.txt`, follows.
- **The documentation says what the language is before it says what it does.** The four
  foundational principles each have a document under `docs/design/`: concurrency is the
  structure, asynchronous first and synchronous second, polyglot as coordination and
  attachment, and as many choices as possible made in syntax. Each states the claim, what
  it buys, and what it costs, with examples that typecheck and numbers taken from
  `benchmarks/`. `docs/design-philosophy.md` is now the index of those four plus the
  eight principles that govern how the language is built.

  The per-feature design documents moved from `docs/design/` to `docs/features/`, where
  `README.md` lists them with their status. Every cross-reference in the repository was
  updated, so no link moved without its target.
- **The README is a front door rather than a second reference.** Installing comes first,
  then the feature set, then the benchmarks, then the four ideas in thirty seconds and
  the principles they rest on. The features, benchmarks, and philosophy sections are
  self-contained: the benchmark tables are the generated output spliced in, not a pointer
  at `benchmarks/`, and nothing in those three sections sends a reader to another page to
  learn what the section is about. The 430-line syntax walkthrough it used to contain is
  in `docs/language-reference.md`, where it was already documented.

### Fixed
- **A function that can fall off its end without returning is refused by `check`.**
  `fn f(n: int) -> int { if n > 0 { return 1 } }` reached rustc as E0317 ("`if` may be
  missing an `else` clause"). `orchestrate check` now requires a body that declares a
  return type to end in a value or to return on every path — through both branches of an
  `if`/`else`, every arm of a `match`, both sides of a `try`/`catch` — and says which
  function, task, or handler can reach its end without one. `fn_missing_return` leaves
  `KNOWN_LEAKS.txt`; of the 88 corpus programs, 81 are rejected by `check` and one still
  reaches rustc. Patch-level: an invalid program is refused earlier, and no valid program
  changes — every example, benchmark program, and test program still checks.
- **A name that is a Rust keyword no longer breaks the generated Rust.** A handler
  named `move`, a `fn type()`, a `let mut`, a struct field `ref`, a module called `impl`,
  a C sidecar function `move` — any of the fifty-odd Rust keywords that are not
  OrchestrateLang keywords — reached rustc as a syntax error in generated code; only
  `gen` was escaped, and only in library builds. Every such name is now emitted as a raw
  identifier (`r#move`) from one keyword list covering editions 2021 and 2024, across
  functions, tasks, handlers, parameters, bindings, struct fields, enum variants, match
  bindings, program and shared state, module names, and foreign wrappers. A name that
  crosses a boundary keeps its spelling there: a sandbox export carries
  `#[export_name = "move"]`, a landline handler is `move` on the wire and in Python or
  TypeScript, a C symbol is `move` for the linker. `self`, `Self`, `super`, and `crate`
  cannot be raw identifiers at all, so `orchestrate check` refuses them as names with an
  error that says so; four corpus cases hold that, taking the corpus to 88 programs. Regression tests use keyword names on every boundary, on a C
  sidecar and module, and in a library build. Patch-level: valid programs that were
  rejected now compile, and nothing that compiled before changes.
- **The runtime tests clean up after themselves.** Each `runtime_tests` case built its
  program in its own directory under the system temp dir and left it there, about 140 MB
  a plain case and 400–600 MB a sandbox case, so a full run left roughly 7 GB behind
  and a nearly full disk saw the linker fail mid-run in what looked like a test failure.
  A case now removes its build directory when it passes and keeps it when it fails;
  `ORCH_KEEP_TEST_BUILDS=1` keeps every one for a fast rerun. Documented in
  CONTRIBUTING.md. Patch-level: the tests changed, the compiler did not.
- **A serverlet call no longer consumes the caller's argument.** `let n = s.shout(payload)`
  followed by any later use of `payload` failed in rustc with E0382, because the call
  moved its arguments into the message; the same for an array or a struct, on every
  boundary. A binding or a field passed to a handler is now copied at the call site, so
  the caller keeps it; a temporary is still moved, and neither the message nor any wire
  protocol changed. Regression tests reuse a string, an array, and a struct after a call
  in-process, on a secret serverlet, in a sandbox, and over the Python and TypeScript
  landlines. Patch-level: a valid program that was rejected now compiles, and nothing
  that compiled before behaves differently.
- **Sixteen wrong programs are told so by `orchestrate check` instead of rustc.** The
  corpus found them: a call into a module or a foreign function with the wrong argument
  types, or to a function the module does not have (which now lists what it has); a
  serverlet handler called with the wrong arity or types, where only the handler's name
  was checked before; a handler declared twice in any serverlet, not only a landline; a
  secret serverlet handler type the wire cannot carry, reported by `check` rather than by
  the child's build; `start X(args)` on a serverlet, which takes none; `start` of
  something that is not a process; `for` over a number or a string; `?` in a function
  that returns neither `result` nor `option`; a piped value of the wrong type; a second
  `orchestrator main`; and a sandboxed handler that waits, which a guest cannot do,
  reported at build. `orchestrate check` also registers Rust sidecar signatures now, as
  the build always did, so a call into a Rust foreign module or the standard library is
  typed during a check instead of unknown.
- **A block whose last statement touches shared state takes the lock.** `while c {
  hits = hits + 1 }`, `if c { hits = hits + 1 }`, and `on_tick(dt: float) { hits = hits + 1 }`
  all end in a block's tail expression, which was compiled without the guard and reached
  rustc as an unknown `__shared`. Found by the tick benchmark's `shared let` case. Every
  block tail that touches shared state now takes the lock exactly as a statement does,
  and a tail that also waits is refused with the same message.
- **A `let` that reads shared state keeps its binding in scope.** `let snapshot = hits`
  wrapped the whole declaration in the lock's block, so `snapshot` was gone on the next
  line and rustc reported it missing. The lock now wraps only the initialiser.
- **A sandboxed serverlet no longer leaks guest memory on every string argument.** The
  host allocated each string in guest memory and never freed it, so a serverlet that took
  strings walked into its memory cap and then failed every call: under an 8mb cap, 20,000
  one-kilobyte calls failed four times and reset the guest each time. Every allocation the
  host makes for a call is released after it, and a test sends those 20,000 calls.
- **A handler may return its own state.** `return seen`, where `seen` is an array or a
  string the serverlet declares, moved the value out of the actor, the child, or the guest,
  and rustc refused with a message about moved values in generated code — on every
  boundary. A handler that returns a state binding now returns a copy. For a number the
  copy is what happened before.
- **A sandbox handler type that cannot cross is rejected by `orchestrate check`**, by
  name, as the compiler's own error. It used to surface from the guest crate's build as a
  `compile_error!` after codegen.

## [0.14.0] - 2026-09-20

Arrays and structs cross the fastest boundary.

### Added
- **Arrays and structs across the C ABI.** C, C++, Zig, and Swift sidecars carried `int`,
  `float`, `bool`, `string`, and `handle`; they now carry `int[]`, `float[]`, `bool[]`, and
  structs declared in the program.

  ```
  total(items: int[]) -> int
  scaled(items: float[], by: float) -> float[]
  shift(p: Point) -> Point
  ```

  An array parameter is **two** C parameters — a pointer and a count, in that order. A
  returned array adds one more, a `long long *` the function writes the count through, and
  returns the pointer. The ownership rule is the one strings already follow: what the
  foreign side returns is `malloc`'d, and the generated wrapper copies it and frees it; an
  array parameter is borrowed for the call and must not be kept.

  A struct crosses **by value**, because generated structs are now `#[repr(C)]`. Its fields
  must line up with the foreign declaration, field for field and in order.

  Arrays carry `int`, `float`, and `bool`. An array of strings, of handles, or of arrays
  does not cross yet, and neither does a struct with those fields.

### Changed
- Structs generated from a `struct` declaration are `#[repr(C)]`, so their layout is the
  one a foreign declaration sees. Nothing else about them changed.

## [0.13.0] - 2026-09-20

State a worker can touch, declared rather than assumed.

### Added
- **`shared let`.** A top-level `let` belongs to the instance and only hooks reach it,
  because a spawned worker can run while a tick holds it. The existing answer for state
  several concurrent things touch is a serverlet, at about 7.5 µs a call. `shared let` is
  the rung between: one mutex instead of a channel round trip.

  ```orchestrate
  let counter = 0          // unchanged: instance-owned, hooks only, free
  shared let hits = 0      // any task, any fn, synchronised
  ```

  **The guarantee is one sentence:** a statement that touches shared state is atomic with
  respect to all shared state. All shared bindings live behind a single mutex, taken once
  per statement, so `hits = hits + 1` is correct without anyone thinking about it and there
  is no lock order to get wrong. Reads clone, because a value cannot be moved out of a
  guard; for a number that is a copy.

  **A statement may wait, or touch shared state — not both.** Holding the lock across a
  wait would block every other reader, so it is a compile error saying to split the
  statement. Because the lock is synchronous, **a plain `fn` can touch shared state**, which
  a `fn` could never do with instance state.

  A shared binding is never captured by a worker or an event handler: capturing would copy
  the value and quietly undo the sharing, so it is reached through the lock wherever it is
  used. In library mode it lives on the instance, so two libraries started in one process
  do not share it. Data can be shared — `int`, `float`, `bool`, `string`, arrays, options
  and structs; a serverlet client, a process, or a closure is refused by name.

  A program without `shared let` gains no mutex, no field, and no code. The design, and the
  five questions it opened and how each was answered, are in `docs/features/shared-state.md`.

## [0.12.0] - 2026-09-20

Wrong programs are told so in OrchestrateLang.

### Fixed
An audit wrote twenty-eight deliberately wrong programs across the language and checked how
each one failed. Eleven reached rustc, reporting on generated code the user never wrote,
with advice about traits and moves that means nothing here. All eleven are now the
compiler's own errors, and the corpus is a test so they stay that way.

- **Calls check argument types**, not just how many there are.
- **An unknown function is an error**, not a warning followed by a rustc failure. `print`
  is generic over anything displayable, as the generated code always was.
- **A serverlet client only answers handlers its serverlet declares**, and the error lists
  the ones it has. Handlers reached through a module alias resolve correctly.
- **`start X()` must name a serverlet that exists.**
- **`trigger` must name a declared event**, with the right number and types of arguments.
- **A struct literal must fill every field.**
- **Comparing two unrelated types is an error**, rather than a missing `PartialOrd`.
- **A function declared to return nothing cannot return a value.**
- **`fn`, `task`, and `process` names must be unique**, and an enum cannot repeat a variant.
- **`process[...]` must name processes**, and each name must be declared.

### Added
- `tests/error_cases/diagnostics/` holds the audit corpus, and `diagnostics_never_leak_rustc`
  asserts that every file there is rejected by the compiler rather than by Cargo, and that
  none is accepted.

## [0.11.0] - 2026-09-20

The language owns its own errors.

### Fixed
- **String concatenation no longer consumes its operands.** `a + b` moved both sides, so a
  string could be used exactly once and `s + s` did not compile at all
  (`error[E0382]: use of moved value`). Addition now borrows: `let c = a + b` leaves both
  `a` and `b` usable, and `a + a` works. `int` and `float` addition is unchanged in
  behaviour and in cost.
- **A match on a unit enum variant that binds a value is a type error**, naming the variant
  and how to write it, instead of rustc's "expected tuple struct or tuple variant, found
  unit variant". The mirror case is caught too: a variant that carries a value and is
  matched without binding it. So is a variant that does not exist, which now lists the ones
  that do.
- **A `try` block whose body waits compiles.** `try { fetch(1) } catch e { 0 }`, where
  `fetch` is a task, reached rustc as "`await` is only allowed inside `async` functions"
  because the block generated a synchronous closure. A block that waits now generates an
  async block instead, so `?` still works and the handler may wait too. A block that does
  not wait generates exactly what it did before.
- **A `fn`, `task`, or `process` that names top-level state says so.** Top-level `let`s
  belong to the instance and only hooks reach them — a spawned task can run while a tick
  holds that state, so the restriction is deliberate. It used to surface as rustc's
  "cannot find value in this scope"; it is now a diagnostic naming the function, the
  binding, and the way out.

### Changed
- `docs/roadmap.md` no longer describes sandboxed serverlets as unshipped (0.9.0) or C# as
  upcoming (0.10.0).

## [0.10.1] - 2026-09-20

A `fn` that cannot wait says so in OrchestrateLang.

### Fixed
- A `fn` that calls a serverlet, a landline, a `task`, `sleep`, or a `parallel` block now
  fails with an OrchestrateLang error naming the function and the call, and saying to
  declare it as a `task`. It used to reach Cargo and surface as rustc's
  "`await` is only allowed inside `async` functions", pointing at generated code the user
  never wrote. `fn` compiles to a synchronous Rust function and `task` to an async one,
  which is the rule the message now states; `docs/language-reference.md` §2.5 states it too.

## [0.10.0] - 2026-09-19

C# runs in-process, about five nanoseconds a call.

### Added
- **`load_foreign "csharp"`.** A C# file becomes a native shared library through .NET's
  Native AOT compiler and links into the program like any other foreign module. A call
  costs **about 5 ns** on an Apple M2, against 1–2 ns for the same function in C; the
  difference is the transition a reverse P/Invoke makes entering and leaving managed code.
  That is 20× cheaper than reaching the same C# through a WebAssembly module.

  ```orchestrate
  load_foreign "csharp" "./Math.cs"
  ```

  A method is exported by carrying `[UnmanagedCallersOnly(EntryPoint = "...")]`, and the
  name it gives must match the sidecar; everything else in the file stays ordinary C#.
  `int` is `long`, `float` is `double`, `void` is `void`, and **`bool` is the exception** —
  it is not blittable in an export signature, so the sidecar's `bool` is a C# `byte` that
  is 0 or 1. Strings, arrays, structs, and `handle` do not cross yet.

  Each module publishes its own **shared** library rather than a static archive. .NET can
  publish static, and it would link a little faster, but two Native AOT static archives
  cannot go into one program — each embeds its own runtime — which would cap a program at
  one C# module and fail as duplicate symbols from the linker rather than as a diagnostic.
  A test covers two C# modules in one program.

  The .NET SDK 8 or newer must be on `PATH`, or `ORCH_DOTNET` must point at it. The first
  build downloads the Native AOT compiler. `check-foreign` does not cover C#, because
  `dotnet` has no syntax-only check for one file outside a project.

  A C# module brings .NET's garbage collector into the process. For a host with a frame
  budget, keep the hot path allocation-free, or use a boundary that keeps the collector
  elsewhere. Behaviour under an allocating handler is not measured yet; the plan in
  `docs/plans/csharp-native-backend.md` says so.

## [0.9.0] - 2026-09-19

Untrusted code runs contained, and any `.wasm` module is callable.

### Added
- **Sandboxed serverlets contain their code.** `serverlet X sandbox(memory_limit: "64mb",
  timeout: "5s")` has parsed since 0.5, and since 0.6 its handlers compiled to a
  `wasm32-wasip1` guest, but the serverlet still ran in-process and the compiler warned
  that nothing was isolated. It is isolated now: the guest runs under
  [wasmtime](https://wasmtime.dev), `memory_limit` caps its linear memory, `timeout`
  bounds a single call through epoch interruption, and its state lives inside the guest
  between calls. Imports are denied by default, so a guest that reaches for the host traps
  instead of arriving. From the caller's side nothing changed — same `start`, same client,
  same methods. `int`, `float`, `bool`, `string`, and `void` cross the boundary.

  A call that exceeds a limit or stops itself is logged, answered with the return type's
  default, and the program carries on. The guest is then replaced, because a trap abandons
  it where it stands rather than unwinding it — so **a failed call resets that serverlet's
  state**. The guest is given two host functions and no others: one carries its own stderr
  out, so a panic or an allocation failure is reported as `[orchestrate] sandbox guest:
  ...`, and one lets it stop itself. Neither is a capability; they exist so a contained
  failure can say what it was.

  Not yet: `grant` and `on_crash` on a sandboxed serverlet are compile errors rather than
  holes that open quietly, and arrays and structs do not cross the boundary.
- **`load_foreign "wasm"`.** A module's exports become ordinary functions:

  ```orchestrate
  load_foreign "wasm" "./math.wasm"
  ```

  There is no toolchain to install and no language to name, because a `.wasm` is already
  compiled — whatever produced it is the author's business. The compiler reads the
  module's own export table and checks every sidecar signature against it, so a name that
  is not exported, or a type that does not line up, is a build error naming both sides;
  `orchestrate check` runs the same check without building. The module is embedded in the
  program, and its imports are denied by default, so it reaches nothing it was not given.
  `int` is `i64`, `float` is `f64`, `bool` is an `i32`, and a `string` is a pointer and a
  length, which means a module carrying strings must export `orch_alloc(i32) -> i32` and
  `orch_free(i32, i32)` so both sides use one allocator.

### Changed
- `sandbox(...)` validates `memory_limit` and `timeout` when it parses them, so a typo is
  an error rather than a limit that quietly misses.
- `orchestrate check` now registers C, C++, Zig, and Swift sidecar signatures as well as
  TypeScript's, so a call into one of those modules is typed during a check instead of
  drawing an "unknown method call" warning.
- Programs that use neither a wasm module nor a sandboxed serverlet gain no new
  dependency. Those that do gain `wasmtime`, which is large; building a sandboxed
  serverlet also needs the `wasm32-wasip1` target.

## [0.8.1] - 2026-09-19

The glue around a synchronous tick is a few nanoseconds, down from about forty.

### Changed
- Library mode: the glue around a synchronous tick is a few nanoseconds. `tick_sync` no
  longer enters the runtime, scopes a task-local, or takes an async mutex per frame: the
  program's state sits in a slot entered with two flag stores and two loads, every hook
  body binds its instance once so a host call is the trait call and one branch, the clock
  advances with an integer add, and an idle tick never builds the event drain. On an Apple
  M2 an empty tick went from 45 ns to 8 ns per `tick_sync`, and each host call in a hook
  from about 3 ns to about 1.5 ns, no more than the trait call itself. Nothing observable changed: `tick`,
  `fixed_tick`, the `block_on` fallback for a body that waits, deterministic mode,
  `stop_orch()`, shutdown, event ordering, and the "a tick is already running" error on a
  re-entered tick are as before. The ignored test `glue_cost` in `tests/library_tests.rs`
  measures the three cases.
- Library mode: `sleep`, landline budgets, and workers started inside a hook no longer need
  the calling thread to be inside the runtime's context; the library carries the handle it
  was started with, so `tick_sync` works from any thread that holds the runtime.
- Library mode: a failed host call is reported as
  `[orchestrate] host call <group>.<name> failed: <error>` — the call's name is now part of
  the message — still at error level through `Host::log`, still yielding the return type's
  default, through one shared cold function rather than a closure at every call site.

## [0.8.0] - 2026-09-18

Errors of any type, strings and handles across the C ABI, and a generic standard library.

### Added
- Type parameters in Rust `.orch_ffi` signatures: `reverse<T>(items: T[]) -> T[]`
  registers as a generic function, so a call infers `T` from its arguments and the Rust
  implementation, written generically, infers it too. The standard library's `lists`
  module uses this: `head`, `tail`, `reverse`, `sort`, `unique`, and `flatten` now take
  any element type rather than `int` alone, and `sum_float`, `max_float`, and `min_float`
  join the `int` versions. `docs/language-reference.md` gains the standard-library section
  it lacked.
- `result<T, E>`. A result's error can be any type; `result<T>` still means
  `result<T, string>`, so every existing program keeps its meaning and its generated code.
  `err(value)` takes any value and is checked against the `result` it must produce; `?`
  requires the function's error type to match, without converting between error types;
  `match` binds `result::Err(e)` as `E`; and `try { … } catch e: Failure { … }` names the
  error type a block propagates, while a plain `catch e` still binds a `string`.
- `string` across the C ABI. C, C++, Zig, and Swift sidecars accept `string` parameters
  and returns under one ownership rule: a parameter arrives as a NUL-terminated
  `const char *` valid for the call, and a return is a `malloc`'d `char *` the generated
  wrapper copies and frees. The slowest boundary no longer carries richer types than the
  fastest.
- `handle`: an opaque native object from a C-ABI foreign module. A sidecar that returns
  `handle` names its release function once, `drop release_counter(c: handle)`, and the
  generated value calls it when its last owner drops, so a native object lives as long as
  the OrchestrateLang value holding it — in a local, in program state across ticks, or
  in a struct — with no serverlet or process around it. Handle arguments are passed by
  reference, so a call does not move the caller's handle. C-ABI sidecar signatures are now
  registered with the typechecker, so calls to them are typed and arity-checked.

## [0.7.0] - 2026-09-18

Foreign Rust can depend on the host, and a frame can tick without a task hop.

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

[Unreleased]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.15.0...HEAD
[0.15.0]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.14.0...v0.15.0
[0.14.0]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.13.0...v0.14.0
[0.13.0]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.12.0...v0.13.0
[0.12.0]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.11.0...v0.12.0
[0.11.0]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.10.1...v0.11.0
[0.10.1]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.10.0...v0.10.1
[0.10.0]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.9.0...v0.10.0
[0.9.0]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.8.1...v0.9.0
[0.8.1]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.8.0...v0.8.1
[0.8.0]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.6.1...v0.7.0
[0.6.1]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.5.1...v0.6.0
[0.5.1]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.3.1...v0.4.0
[0.3.1]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.3.0...v0.3.1
[0.3.0]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/Immanuel-Fowler/OrchestrateLang/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/Immanuel-Fowler/OrchestrateLang/releases/tag/v0.1.0
