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
