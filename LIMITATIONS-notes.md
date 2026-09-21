# Limitations notes

Gaps found while preparing the paper submission, whether or not they were fixed. Each
entry says where the gap is, how it was found, and what its status is. This file feeds
the paper's limitations section; `docs/roadmap.md` and the changelog stay the user-facing
record.

## Where the unchanged-call-site claim does not yet hold

The call site is the same on every boundary: `start X()`, then `x.handler(args)`. What
differs is which handler signatures each boundary accepts. This table is the honest
version of the claim.

| Value | In-process | Secret | Python / TypeScript landline | Sandboxed (WASM) |
|---|---|---|---|---|
| `int`, `float`, `bool`, `string` | yes | yes | yes | yes |
| `int[]`, `float[]`, `bool[]` | yes | yes | yes | yes (since this branch) |
| struct of numbers and booleans | yes | yes | yes | yes (since this branch) |
| struct with a `string` field | yes | yes | yes | **no** |
| `string[]`, array of structs, array of arrays | yes | yes | yes | **no** |
| `option<T>`, `result<T>`, closures | yes | no | no | no |
| `handle` | yes | no | no | no |

- The sandbox carries exactly what the C ABI carries, by design: one ownership rule
  (borrowed parameters, callee-allocated returns copied and freed) rather than a second
  encoding. A nested array or a string inside a struct would need its own ownership story.
  The three process-based boundaries use the wire codec instead, which nests freely.
- A `check` names the offending handler and type for the sandbox; for a secret or landline
  serverlet the same class of error is also the compiler's own.

**Host reach per boundary**, after the grants branch:

| Boundary | `grant call` | `on_crash` | Ungranted host call |
|---|---|---|---|
| In-process | not needed: trusted code, calls the host directly | on a handler panic | n/a |
| Secret | no host functions (a child process has no `Host`) | no | n/a |
| Landline | yes | on transport failure | rejected by the mirror at run time |
| Sandboxed | yes, library builds only | on a trap (timeout, memory, self-stop) | refused by `orchestrate check`, and undefined in the linker |

- A grant on a sandboxed serverlet counts the host method's time against the call's
  `timeout`, because the guest is still inside its call while the host runs. A landline
  grant has no such coupling. Documented; not a defect.
- `on_crash` for the sandbox cannot touch the serverlet's state (it is inside the
  instance being replaced), where an in-process `on_crash` can. Refused at compile time
  with the binding named.

## Found and fixed on this branch

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
  gate runs in codegen, before Cargo, because module bodies are not walked by the
  typechecker.

- **`shared let` did not compile in three ordinary positions.** A block whose tail
  statement touched shared state — a `while` body, an `if` branch, or an `on_tick` body
  — was compiled without the lock and reached rustc as an unknown `__shared`; and a
  `let` whose initialiser read shared state was wrapped whole in the lock's block, so
  the binding was gone on the next line. Both shipped in 0.13.0 and were found by the
  tick benchmark's `shared let` case. Fixed on the tick-cost branch: a block tail takes
  the lock as a statement does, and the lock wraps only a `let`'s initialiser.
  Regression tests: `runtime_shared_state_in_a_block_tail_takes_the_lock`,
  `library_shared_state_in_hooks`.

## Found, not fixed

- **A string passed to a serverlet call is moved.** `let n = s.shout(payload)` followed
  by any later use of `payload` fails in rustc with E0382 ("borrow of moved value"),
  because call arguments are moved into the message. A valid program is rejected by the
  wrong compiler. Workaround: index a one-element array (`texts[0]`) to get a copy at
  each use. Not fixed here because the fix touches every call path, not the sandbox.
- **Struct layout across the sandbox assumes a little-endian host.** Struct bytes are
  written field by field in little-endian order on both sides, so a big-endian host would
  still be correct; but no such host is tested, and the C ABI path (0.14.0) passes
  structs by value in native order, so the two would disagree there, not here.
- **`closure_wrong_arity.orch` in the diagnostics corpus tests the parser, not arity.**
  It uses `(a: int) => a`, which is not closure syntax in this language (`fn(a: int) ->
  int { a }` is), so it is rejected as a syntax error before any arity check runs.
  To be replaced in the diagnostics-corpus branch.
- **The runtime tests keep their build scratch.** Each `runtime_tests` case builds its
  program in its own directory under the system temp dir and, except for the two sandbox
  cases, never removes it: about 140 MB per plain case and 400–600 MB per sandbox case
  (wasmtime). A full run leaves roughly 7 GB behind, and a second machine with a nearly
  full disk saw the linker fail with "no space left on device" mid-run, which looks like
  a test failure. Left as is, because a kept directory makes a rerun fast; clean with
  `rm -rf "$TMPDIR"/orch_*` when space matters.
- **A name that is a Rust keyword reaches rustc.** `on move(p: Point)` generated
  `pub extern "C" fn move(...)` in the sandbox guest and would generate
  `pub async fn move(...)` on the in-process client; either is a rustc syntax error in
  generated code. The same holds for a `fn type()`, a `let match = 1`, a struct field
  named `ref`, and every other Rust keyword that is not an OrchestrateLang keyword.
  Only `gen` is escaped today (for edition 2024 library crates). Found by naming a
  handler `move` in the grants test; not fixed here, since the fix is a systematic
  `r#` escape across codegen. Candidate for the diagnostics corpus as a leak.

## Benchmark caveats (boundary ladder)

- **The plain `let` rung reads as 0 ns.** `count = count + 1` a thousand times is folded
  by LLVM into one addition, so the row shows the floor of the batch method rather than
  the cost of a field write. The honest statement is "below the resolution of this
  method"; the `shared let` row (one uncontended mutex per statement, about 9 ns on an
  Apple M2) is the real number the ladder needs.
- **`clock_micros` has microsecond resolution.** The state rungs are batched to get
  nanoseconds; every other row is a single round trip, so a 1 µs quantisation sits under
  every p50 in the low tens of microseconds. Adding a nanosecond clock would be new
  language surface, which this work does not add.
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

- **An exclusion surfaces to the host without its reason.** A worker, serverlet, or
  landline started in deterministic mode, or a `sleep` in a tick, panics inside the
  library with a message naming deterministic mode; the host sees `ready` fail with
  "library startup task failed" or `tick` fail with "tick task failed", and the message
  only reaches the process's panic hook. Under `tick_sync` the panic reaches the calling
  thread. Documented precisely now; carrying the reason into the error would need the
  coordinator to catch the unwind, which this work did not do.
- **The exclusions are checked at run time, not by `check`.** A program that starts a
  serverlet is a fine program in normal mode; only the host's `StartOptions` makes it an
  error. `check` cannot know which mode the host will choose.

## Diagnostics corpus: what still reaches rustc

84 invalid programs; 76 rejected by `orchestrate check`, 6 by the build before Cargo,
2 by rustc, 0 accepted (`benchmarks/results/diagnostics_coverage.md`). The build-time six
are compiler errors too, they just live in the driver or the generator: a `load_foreign`
language the compiler does not know, `host` in a module or outside a library build, a
`fn` that calls a task through a module, a sandboxed handler that waits, a `shared let`
of an unshareable type, and a statement that both waits and touches shared state. The
two leaks, listed in `tests/error_cases/diagnostics/KNOWN_LEAKS.txt`:

- **`fn_missing_return`**: `fn f(n: int) -> int { if n > 0 { return 1 } }` reaches rustc
  as E0317 ("`if` may be missing an `else` clause"). Catching it needs a definite-return
  analysis over blocks, which the typechecker does not have; its block typing treats a
  bare `if` as `void` and does not compare that with the declared return type when the
  body ends in a statement.
- **`generic_arg_conflict`**: `same(1, "x")` against `fn same<T>(a: T, b: T)` reaches
  rustc as E0308. `unify_type_param` records the first binding of `T` and does not
  refuse a second, incompatible one.

Three candidates turned out to be **valid programs that fail** and are not in the
corpus, because the corpus is invalid programs:

- a name that is a Rust keyword (`on move(...)`, see above);
- a string passed to a serverlet call and used again (`let a = c.shout(text)  print(text)`),
  E0382, see above;
- a `task` that touches `shared let` — this one compiles and is correct; it was
  a wrong candidate, listed here so nobody adds it back.

The count `check` reports is for the entry file's declarations. A serverlet declared
inside an imported module is not walked by the typechecker, so its handler bodies are
checked by codegen at build time, never by `check`.
