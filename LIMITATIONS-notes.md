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
