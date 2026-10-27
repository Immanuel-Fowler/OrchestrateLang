# Limitations notes

Gaps found while preparing the paper submission, and since then by building programs on
the language, whether or not they were fixed. Each entry says where the gap is, how it
was found, and what its status is. This file feeds
the paper's limitations section; `docs/roadmap.md` and the changelog stay the user-facing
record.

## Where the unchanged-call-site claim does not yet hold

The call site is the same on every boundary: `start X()`, then `x.handler(args)`. What
differs is which handler signatures each boundary accepts. This table is the honest
version of the claim.

| Value | In-process | Secret | Python / TypeScript landline | Sandboxed (WASM) |
|---|---|---|---|---|
| `int`, `float`, `bool`, `string` | yes | yes | yes | yes |
| `int[]`, `float[]`, `bool[]` | yes | yes | yes, as one block copy | yes |
| struct of numbers and booleans | yes | yes | yes, a dataclass checked field by field | yes |
| struct with a `string` field | yes | yes | yes | **no** |
| `string[]`, array of structs, array of arrays | yes | yes | yes | **no** |
| `result<T>` as a handler's return, of a `T` above | yes | yes | yes: the implementation returns `T`, and raising or throwing is its `err` | yes |
| `option<T>`, closures, `result` anywhere else | yes | no | no | no |
| `handle` | yes | no | no | no |
| `map<K, V>` of the values above | yes | yes | yes: a Python `dict`, a TypeScript `Map` | **no** |
| `S.client` | yes, as a parameter; a return only as `result<S.client>` | no | no | no |
| an enum, or a struct holding one, as a handler's return | only as `result<T>` | no | no | no |

- The sandbox carries exactly what the C ABI carries, by design: one ownership rule
  (borrowed parameters, callee-allocated returns copied and freed) rather than a second
  encoding. A nested array, a string inside a struct, or a map would need its own ownership
  story. The three process-based boundaries use the wire codec instead, which nests freely.
- A client is an address in this process, so it crosses no process or sandbox boundary;
  `check` says so rather than naming it an unsupported type.
- A `check` names the offending handler and type for the sandbox; for a secret or landline
  serverlet the same class of error is also the compiler's own.

**What a caller learns when a call fails.** A handler declared `-> result<T>` answers
`err` with a message saying why; any other handler answers with its return type's
default, as before, and the failure goes to stderr and `on_crash`. A zero the handler
computed is `ok(0)`, so the two can no longer be confused. Each row is a runtime test that
makes the callee fail and checks the caller sees `err`:
`serverlet_result_says_a_call_failed` (in-process and secret),
`python_result_handlers_say_a_call_failed`, `typescript_result_handlers_say_a_call_failed`,
and `runtime_sandbox_result_says_a_call_failed`.

| What happened | In-process | Secret | Python / TypeScript landline | Sandboxed (WASM) |
|---|---|---|---|---|
| The handler said no | its own `err(...)` | its own `err(...)` | what it raised or threw: `err("ValueError: ...")` | its own `err(...)` |
| The handler panicked or trapped | `serverlet 'S' handler 'h' panicked: ...` | `secret serverlet 'S' handler 'h' failed: ...` | (a raise is the handler's answer, above) | `wasm call 'S::h' failed: ...`, for a trap, the timeout, or the memory cap |
| The process died during the call | n/a | `secret serverlet 'S' was lost during a call to 'h': ...` | `landline 'S' crashed during a call to 'h': ...` | n/a |
| It could not start | n/a | `secret serverlet 'S' is not running: it could not start (...)` | `landline 'S' is not running: its startup failed (...)` | `serverlet 'S' stopped before it answered 'h'` |
| The call missed its budget | n/a | n/a | `landline 'S' handler 'h' missed its budget of 5ms`; with `late: "latest"`, the latest answer instead, `ok` or `err` | n/a: the timeout is a trap |

- A process-based boundary that could not start is tried again, with backoff, at a later
  call; until then each call fails at once rather than waiting.
- A handler that does not return `result` still cannot say it failed: a program that must
  tell, and that uses any boundary but in-process, declares `result<T>`. Nothing warns
  about a handler that could have.
- A foreign function (`load_foreign`) has no `result` return. A TypeScript FFI function
  that throws, or whose process dies, panics in its caller, as a C or Rust function that
  panics does.

**Host reach per boundary**, after the grants branch:

| Boundary | `grant call` | `on_crash` | Ungranted host call |
|---|---|---|---|
| In-process | not needed: trusted code, calls the host directly | on a handler panic | n/a |
| Secret | no host functions (a child process has no `Host`) | when the child dies, noticed at the next call; the child is then restarted | n/a |
| Landline | yes | on transport failure | rejected by the mirror at run time |
| Sandboxed | yes, library builds only | on a trap (timeout, memory, self-stop) | refused by `orchestrate check`, and undefined in the linker |

- A grant on a sandboxed serverlet counts the host method's time against the call's
  `timeout`, because the guest is still inside its call while the host runs. A landline
  grant has no such coupling. Documented; not a defect.
- `on_crash` for the sandbox cannot touch the serverlet's state (it is inside the
  instance being replaced), where an in-process `on_crash` can. Refused at compile time
  with the binding named.

## Found and fixed while preparing the paper (0.13.0–0.15.1)

- **Sandbox string arguments leaked guest memory.** Every string the host wrote into the
  guest stayed allocated. Under an 8mb cap, 20,000 one-kilobyte calls failed four times,
  each failure resetting the guest's state. Fixed: the host frees what it wrote once the
  call returns. Regression test: `runtime_sandbox_frees_what_the_host_writes`.
- **A handler could not return its own array or string state.** `return seen` reached
  rustc as a moved value on all three OrchestrateLang-bodied boundaries (in-process
  E0382, secret, sandbox E0507). Fixed: a returned state binding is a copy. Regression
  test: `runtime_serverlet_handler_returns_its_state`.
- **Sandbox type errors were reported by the guest's cargo build**, as a `compile_error!`
  after codegen, so `orchestrate check` accepted a program the build rejected. Fixed for
  entry-file serverlets in the typechecker; for a serverlet inside an imported module the
  gate ran in codegen, before Cargo, until 0.15.1 made `check` walk module bodies too.

- **`shared let` did not compile in three ordinary positions.** A block whose tail
  statement touched shared state — a `while` body, an `if` branch, or an `on_tick` body
  — was compiled without the lock and reached rustc as an unknown `__shared`; and a
  `let` whose initialiser read shared state was wrapped whole in the lock's block, so
  the binding was gone on the next line. Both shipped in 0.13.0 and were found by the
  tick benchmark's `shared let` case. Fixed on the tick-cost branch: a block tail takes
  the lock as a statement does, and the lock wraps only a `let`'s initialiser.
  Regression tests: `runtime_shared_state_in_a_block_tail_takes_the_lock`,
  `library_shared_state_in_hooks`.

- **A value passed to a serverlet call was moved.** `let n = s.shout(payload)` followed
  by any later use of `payload` reached rustc as E0382 ("borrow of moved value"), because
  call arguments were moved into the message; the same for an array or a struct, on
  every boundary. Fixed after the paper branches landed: a binding or a field in argument
  position is copied at the call site, which is one place in codegen for all five
  boundaries, so neither the message nor a wire protocol changed. Regression tests, one
  per boundary: `runtime_serverlet_call_keeps_the_callers_arguments`,
  `runtime_secret_serverlet_call_keeps_the_callers_arguments`,
  `runtime_sandbox_serverlet_call_keeps_the_callers_arguments`,
  `python_landline_call_keeps_the_callers_arguments`,
  `typescript_landline_call_keeps_the_callers_arguments`.

- **`closure_wrong_arity.orch` in the diagnostics corpus tested the parser, not arity.**
  It used `(a: int) => a`, which is not closure syntax in this language, so it was
  rejected as a syntax error before any arity check ran. Fixed on the diagnostics-corpus
  branch: the case is `fn(x: int) -> int { x }` called with two arguments, and `check`
  rejects it with "closure 'f' expects 1 arguments, got 2"; the arrow form is its own
  case, `closure_arrow_syntax.orch`. Covered by `diagnostics_never_leak_rustc`.

- **The runtime tests kept their build scratch.** Each `runtime_tests` case built its
  program in its own directory under the system temp dir and, except for the two sandbox
  cases, never removed it: about 140 MB per plain case and 400–600 MB per sandbox case
  (wasmtime), roughly 7 GB a full run, and a nearly full disk saw the linker fail with
  "no space left on device" mid-run, which looks like a test failure. Fixed after the
  paper branches landed: a `Scratch` guard removes the directory when the case passes,
  keeps it when the case fails, and keeps every one under `ORCH_KEEP_TEST_BUILDS=1`
  (documented in CONTRIBUTING.md section 6). Covered by every `runtime_tests` case; the
  guard is the same shape `library_tests` and `typescript_tests` already used.

- **A name that is a Rust keyword reached rustc.** `on move(p: Point)` generated
  `pub extern "C" fn move(...)` in the sandbox guest and `pub async fn move(...)` on the
  in-process client; either is a rustc syntax error in generated code, and the same held
  for a `fn type()`, a struct field `ref`, a `let mut`, and every other Rust keyword that
  is not an OrchestrateLang keyword. Only `gen` was escaped, by a post-pass over library
  output. Fixed after the paper branches landed: one keyword list (editions 2021 and
  2024) and one `rust_ident` helper escape a name wherever codegen emits it as a Rust
  identifier; the post-pass is gone. Names that cross a boundary keep their spelling
  there — a sandbox export uses `#[export_name]`, a landline handler and a wire field are
  the plain name, a C symbol keeps `#[link_name]`. `self`, `Self`, `super`, and `crate`
  cannot be raw identifiers, so the lexer refuses them as names. Struct and enum *type*
  names were left out at first, on the reasoning that types are PascalCase; 0.15.1
  escapes them too, in every type position, while the sandbox codec functions built
  from a type's name keep the plain spelling (`runtime_keyword_type_names_*`).
  Regression tests:
  `runtime_keyword_names_in_process`, `runtime_keyword_names_on_a_secret_serverlet`,
  `runtime_keyword_names_in_a_sandbox`, `runtime_keyword_names_on_a_c_sidecar_and_module`,
  `python_landline_keyword_names`, `typescript_landline_keyword_names`,
  `library_keyword_names`; corpus cases `name_self`, `name_upper_self`, `name_super`,
  `name_crate`.

- **`fn_missing_return` reached rustc.** `fn f(n: int) -> int { if n > 0 { return 1 } }`
  reached rustc as E0317, because the typechecker typed a bare `if` as `void` and never
  compared a body that ends in a statement with its declared return type. Fixed after the
  paper branches landed: a body with a return type must end in a value or return on every
  path (both branches of an `if`/`else`, every arm of a `match`, both sides of a
  `try`/`catch`; a loop never counts, as in rustc), for functions, tasks, processes, and
  handlers alike. Every example, benchmark program, and test program still checks.
  Covered by `diagnostics_never_leak_rustc`, which now holds the corpus to one leak.

- **`generic_arg_conflict` reached rustc.** `same(1, "x")` against `fn same<T>(a: T,
  b: T)` reached rustc as E0308: `unify_type_param` kept the first binding of `T` and
  never refused a second. Fixed for 0.15.1: a second, incompatible binding is a `check`
  error naming the argument. Covered by `diagnostics_never_leak_rustc`; `KNOWN_LEAKS.txt`
  is empty.

## Found and fixed for 0.16

Three programs written against 0.15.1 — a backtest engine, a control loop, and a
multi-tenant workflow runtime — turned up the defects below. Each has a regression test
named in `CHANGELOG.md`.

- **In-place changes to a `shared let` were lost.** Element and field assignment,
  `append`, and `remove` changed a copy of the binding that was then dropped; a program
  built, ran, and did nothing, with no sign anywhere. The worst kind: a control loop lost
  every sensor reading this way and produced a plausible, wrong result on every tick.
- **Value semantics held only at serverlet calls.** Everywhere else a binding whose value
  was taken was moved, so ordinary programs — `print(a)` twice on a string, `let b = a`
  on an array — reached rustc. One helper, `compile_value`, now copies at every such
  position; `diagnostics_never_leak_rustc` cannot catch this class, since the programs
  are valid, so `tests/programs/value_semantics.orch` holds it, with its output checked.
- **A top-level serverlet client was usable only where it was started**, the design
  document's example did not compile, **and a bare standalone `on` handler was never
  registered**, so its events went nowhere, silently.
- **A library's event handler could not write instance state**, which the reference said
  it could.
- **A struct named in two files was two types**, and a C sidecar's struct declared only
  in its module did not build.
- **`check` refused a valid module grant** that the build accepted.
- **A write from a worker, a handler, or a closure to a binding outside it** reached
  rustc, or, for a closure, compiled and was lost. It is now a `check` error.
- **No out-of-process call could say it failed.** A trap, a crash, a dead child, and a
  missed budget all answered with the return type's default, which a caller could not
  tell from a real answer. A handler declared `-> result<T>` now answers `err`, on every
  boundary; the table above says what each failure reads as.
- **A struct holding an enum did not compile anywhere**, since every struct derived
  `Default` and an enum has none; **nor did a serverlet handler returning an enum**, whose
  failed call had no default to answer with. Found while adding `result<T>` handlers. A
  struct derives `Default` now only when its fields have one, and a handler returning a
  type without one is a `check` error that says to declare `result<T>`.
- **An in-process `on_crash` bound `Any { .. }`**, the panic payload's debug form, rather
  than the panic's message.
- **Values that could not be shown or compared passed `check`.** `print`, `to_string`, and
  `{...}` of an array, an option, or a struct, `==` on a struct or an enum, and a generic
  function showing or comparing its `T` all reached rustc. Structs and enums now derive
  equality where they can, generic functions are bounded by what their bodies need, and the
  rest is refused by `check` with what to do instead. Found while adding client types, when
  a client's `==` and `print` had to be refused.
- **A sandbox guest took the file's structs but not its enums**, so any struct holding an
  enum, anywhere in the file, broke the guest's build. Found while testing structs with enum
  fields.
- **Names inside a string's `{...}` were reported at their position in the snippet**, line
  1 and a column counted from the brace.

- **A Python dataclass with the struct's fields in another order was accepted** and
  decoded into the wrong fields, silently: the handshake named a struct and nothing else.
  It now spells out each struct's fields on both sides and refuses a mismatch.
- **A landline's arrays crossed an element at a time**, about 300 ns per number in Python,
  so the boundary recommended for batched work could not carry a batch. They cross as one
  block copy now, 10–16 times faster on the boundary ladder.
- **A landline was one file** and could not import a module beside it, and **its
  declaration had to name every public method, in order.** The build follows a Python
  source's imports; the handshake finds declared handlers by name.
- **A sandboxed call made a timeout after the previous one trapped before it started**,
  while its arguments were copied in under the previous call's deadline.
- **A struct or enum with a one-letter name was read as a type parameter** in a signature.
- **Nothing could be loaded while a program ran.** A sandboxed guest now can, with
  `start X from path`, and a Python landline can run a source named at run time.

## Found, not fixed

- **A secret handler that calls a top-level `fn` passes `check` and fails in rustc**
  (E0425): the child binary does not include the parent's functions. The rule is right;
  `check` should refuse the call, naming it.
- **`option<T>` and enums do not cross a process boundary.** A secret or landline handler
  returning `option<int>` is refused by `check`; `result<T>` crosses, and `option` could
  cross the same way, as a flag byte and the value.
- **The sandbox does not carry a string inside a struct, an array of strings or structs,
  or a map.** Each needs an ownership rule of its own in the C ABI.
- **`stop_orch()` exits without running `on_stop`** in a standalone program; only Ctrl+C
  runs it.
- **A landline restarts without a configurable limit**; `restart:` in its configuration
  is refused.
- **`start X from path` for a Python landline checks only that the file exists.** Whether
  it implements the declaration is found in the handshake, so a mismatch fails each call
  rather than the start.
- **The landline block codec's big-endian path is untested.** It byte-swaps on a
  big-endian host; no such host runs the tests.

- **Struct layout across the sandbox assumes a little-endian host.** Struct bytes are
  written field by field in little-endian order on both sides, so a big-endian host would
  still be correct; but no such host is tested, and the C ABI path (0.14.0) passes
  structs by value in native order, so the two would disagree there, not here.

## Benchmark caveats (boundary ladder)

- **The plain `let` rung reads as 0 ns.** `count = count + 1` a thousand times is folded
  by LLVM into one addition, so the row shows the floor of the batch method rather than
  the cost of a field write. The honest statement is "below the resolution of this
  method"; the `shared let` row (one uncontended mutex per statement, about 9 ns on an
  Apple M2) is the real number the ladder needs.
- **`clock_micros` has microsecond resolution.** The state rungs are batched to get
  nanoseconds; every other row is a single round trip, so a 1 µs quantisation sits under
  every p50 in the low tens of microseconds. `clock_nanos()` exists since 0.16; the
  ladder still uses `clock_micros` so its rows stay comparable with earlier runs.
- **The first sweep pays for being first.** The in-process `int` row runs first and shows
  a higher median than the same kind's string row; the actor and the allocator are still
  warming even after 200 warm-up calls. Read the payload sweep as rows within a kind
  rather than across the first row.
- **TypeScript maxima.** The TypeScript landline shows maxima of 0.6–1.6 ms on every
  payload; the median is steady. The executable is Bun's (or scriptc's), and its
  garbage collector is inside the boundary that chose it.
- **Concurrent callers are branches of one `parallel` block**, so they are concurrent
  futures on the runtime, not OS threads. That measures callers queueing on one
  serverlet, which is the question; it does not measure two cores hammering one actor.

## Benchmark caveats (tick cost)

- **Medians overlap between cases within a few nanoseconds.** On an Apple M2 the OS
  moves the thread between performance and efficiency cores and between clock states
  during a run, so the `instance_let` median can come out below `empty`. The fastest
  round is reported beside the median and is the figure to quote; the derived per-call
  cost uses it. Run on an idle machine and compare cases within one run.
- **The event case is a different question.** One host-fired event per tick costs about
  200 ns: the trigger allocates the event future, the drain runs it, and the handler's
  host call is inside. It is on the table because a host that fires events every tick
  should know the price, not because it belongs to the tick-glue claim.

## Deterministic mode

- **An exclusion surfaced to the host without its reason.** A worker, serverlet, or
  landline started in deterministic mode, or a `sleep` in a tick, panics inside the
  library with a message naming deterministic mode; the host saw only "library startup
  task failed" or "tick task failed", and the message reached nothing but the process's
  panic hook. Fixed for 0.15.1: the supervisor records the coordinator's panic message
  before cleanup, and `ready`, `tick`, and `fixed_tick` append it to their error. Under
  `tick_sync` the panic still reaches the calling thread. Regression test:
  `engine_deterministic_mode_refuses_what_it_excludes`.
- **The exclusions are checked at run time, not by `check`.** A program that starts a
  worker is a fine program in normal mode; only the host's `StartOptions` makes it an
  error. `check` cannot know which mode the host will choose.
- **Serverlets were excluded until 0.16**, so the guarantee covered the part of a program
  that needs no isolation and none of the part that does: two library programs split a
  deterministic subset off into a second entry file to say anything about replay. A
  deterministic run now holds every serverlet's actor and runs it in call order, inside
  the tick, and ignores what measures wall time (budgets, sandbox timeouts). Workers and
  `sleep` outside a handler are still excluded; a child that reads the clock is outside
  what any replay can see.

## Diagnostics corpus: what still reaches rustc

Nothing. 121 invalid programs; all 121 rejected by `orchestrate check`, none by the
build, none by rustc, none accepted (`benchmarks/results/diagnostics_coverage.md`, run at
e85d785), and `tests/error_cases/diagnostics/KNOWN_LEAKS.txt` is empty.

For 0.15.1, six mistakes that used to pass `check` and be caught by the driver or the
generator became `check` errors: a `load_foreign` language the compiler does not know,
`host` or `on_tick` outside a library build (plain `check` now applies the standalone
rules, `check --lib` the library ones), a `fn` that calls a task through a module, a
sandboxed handler that waits, a `shared let` of an unshareable type, and a statement
that both waits and touches shared state. The typechecker's rule for what waits is
codegen's, from `require_async`, with one difference: a call counts as a serverlet call
only when its receiver is known to hold a client, so the language server, which
registers no modules, never takes a module call for one. `generic_arg_conflict`, the
last rustc leak, is fixed; see above.

Three candidates turned out to be **valid programs that fail** and are not in the
corpus, because the corpus is invalid programs:

- a name that is a Rust keyword (`on move(...)`, since fixed, see above; the four names
  that cannot be escaped are in the corpus as `name_*`);
- a string passed to a serverlet call and used again (`let a = c.shout(text)  print(text)`),
  E0382, since fixed, see above;
- a `task` that touches `shared let` — this one compiles and is correct; it was
  a wrong candidate, listed here so nobody adds it back.

Since 0.15.1 `check` also walks each imported module's own declarations, in the module's
own scope, so a serverlet declared in a module is typechecked by `check` rather than
first by codegen at build time. `module_serverlet_body_type_error`, a handler inside a
module that returns a string where it declares `int`, passed `check` and failed in Cargo
before that.
