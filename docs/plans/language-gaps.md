# Language gaps: `result<T, E>`, C-ABI strings and handles, a generic stdlib, sandbox honesty

> Status: **sections 1–3 shipped in v0.8.0 (2026-09-18); section 4 open.** Found in the
> 2026-09-18 review of the language after v0.6.0. Where the shipped design differs from
> the plan below: `try`/`catch` names a non-string error type explicitly,
> `catch e: Failure`, because codegen has no type information and Rust cannot infer the
> closure's error type (the typechecker verifies the annotation against what the block
> propagates); the release function is declared on its own line, `drop release(c: handle)`,
> and handle arguments are passed by reference so a call does not move the caller's
> handle; and arrays and structs across the C ABI remain planned. Sizes were estimates
> for one person. None changes the behaviour of a program that compiled before.

## 1. `result<T, E>`

### What is missing

`result<T>` always carries a `string` error. `err("division by zero")` is the only form
`err` accepts, `catch e` binds a `string`, and there is no way to carry structured error
data — a language whose point is coordinating work that fails cannot say *how* it failed
except in prose.

### Weak points in the code

| File | Location | Problem |
|---|---|---|
| `src/ast.rs` | `Type::Result(Box<Type>)`; `display_name` prints `result<T>` | One type parameter |
| `src/parser.rs` | type parsing for `result<…>` | Accepts one argument |
| `src/typechecker.rs` | `ErrLiteral`: "err() argument must be a string"; `Propagate` unwraps `Result(ok)`; `TryCatch` defines `err_name` as `Type::Str` | Error type is fixed everywhere |
| `src/codegen/core.rs` | `compile_type`: `Result<{T}, String>` | Hard-coded `String` |
| `src/codegen/expr.rs` | `TryCatch`: closure `-> Result<_, String>`, handler `|{name}: String|` | Same |
| `src/ffi_rust.rs` | `parse_sidecar_type` accepts `result<T>` for Rust sidecars | Maps to `Result<T, String>` |

### Design

- `result<T, E>`, with `result<T>` meaning `result<T, string>`. Existing programs keep
  their meaning and their generated code byte for byte.
- `err(value)` takes any value; its type is the `E` of the `result` the expression is
  checked against (the enclosing function's return type, a `let` annotation, or a `try`
  block's inferred result). With no expectation, `err("text")` still infers `string`.
- `?` on `result<T, E>` inside a function returning `result<U, E2>` requires `E == E2`;
  no automatic conversion in this step. `?` on `option<T>` is unchanged.
- `try { … } catch e { … }` binds `e: E`, where `E` is the error type of the `result`
  values the block propagates; propagating two different `E`s in one block is an error
  that names both.
- `match` on a result keeps `result::Ok(v)` and `result::Err(e)`; `e` gets `E`.
- `E` is any type: `string`, `int`, a struct, or an enum — an enum is the expected shape
  for a real error set.
- Rust sidecars: `result<T, E>` maps to `Result<T, E>` for the types sidecars already
  accept; `result<T>` stays `Result<T, String>`. `result` remains a non-wire type for
  serverlets and landlines.

### Steps

1. `Type::Result(Box<Type>, Box<Type>)`; parser accepts one or two arguments and fills in
   `string`; `display_name` prints `result<T, E>` only when `E` is not `string`, so error
   messages for existing code do not change.
2. Typechecker: expected-type threading for `err`, the `?` rule, the `try` rule, and the
   match binding. `types_compatible` on both parameters.
3. Codegen: `compile_type` and the `TryCatch` closure use the compiled `E`.
4. Sidecars: `parse_sidecar_type` accepts the second argument.
5. Tests: parser and typechecker unit tests (defaulting, a struct error, mismatched `E`
   through `?`, mismatched `E`s in one `try`), a runtime test that returns an enum error
   and matches on it, and the existing snapshots unchanged.
6. Docs: the `option`/`result` section of the language reference; CHANGELOG.

About a day. Minor release, since it is a language addition.

## 2. `string` across the C ABI, then opaque handles

### What is missing

`load_foreign` for C, C++, Zig, and Swift accepts `int`, `float`, `bool`, and `void`
(`ffi_parser.rs` rejects `string` with "string type is not supported in C-ABI FFI
signatures"). The boundary that costs nanoseconds carries the fewest types; the
TypeScript bridge, which costs microseconds, carries strings, arrays, and structs. And
there is no way to hold a native object across calls without wrapping it in a serverlet.

### Weak points in the code

| File | Location | Problem |
|---|---|---|
| `src/ffi_parser.rs` | `parse_ffi_and_generate_bindings`; the `string` rejection | Type table and wrapper generation stop at scalars |
| `src/driver.rs` | `build.rs` generation for C/C++ (`cc`), Zig, Swift | Unchanged for strings; handles need nothing here |
| `src/typechecker.rs` | `register_foreign_function` | Needs the `handle` type registered as opaque |
| `docs/language-reference.md` | §6.4 Foreign C, Zig, Swift | Documents the scalar limit |

### Design

- **Strings, one rule.** A `string` parameter is passed as a NUL-terminated
  `const char *` that is valid only for the call; a `string` return is a NUL-terminated
  `char *` the foreign side allocated with `malloc` (C, C++), `std.heap.c_allocator`
  (Zig), or `strdup` (Swift). The generated wrapper copies it into a `String` and calls
  `free`. One allocator, one direction of ownership, and every one of the four languages
  can meet it without a helper library. Non-UTF-8 bytes are replaced rather than
  rejected, as the wire codec does.
- **Handles.** A sidecar type `handle` is an opaque pointer (`*mut c_void`) the foreign
  side creates and OrchestrateLang never dereferences. A sidecar names the release
  function once, `handle drop: release_thing`, and the generated wrapper wraps the pointer
  in a struct whose `Drop` calls it, so a native object lives exactly as long as the
  OrchestrateLang value that owns it. Handles are `Send` but not cloneable, so a worker
  that captures one moves it — the same rule the language applies to serverlet clients.
  This is the "stateful FFI" a host has asked for, without a serverlet or a process.
- **Arrays and structs** come after, on the same ownership rule (a length-prefixed buffer
  the callee allocates for returns; a pointer and length for parameters), once strings
  and handles have shown the rule holds up across the four toolchains.

### Steps

1. `ffi_parser.rs`: accept `string` in parameters and returns; generate the `CString` /
   copy-and-`free` wrapper; document the rule in the generated `extern "C"` comment.
2. Tests: C, C++, Zig, and Swift fixtures that take and return strings, including an
   empty string and non-ASCII, under `tests/runtime_tests.rs` and the library suite's
   Zig/Swift test; `check-foreign` unchanged.
3. `handle` type: sidecar syntax, the `Drop` wrapper, typechecker registration; a Zig
   fixture that creates, mutates, and releases a counter, with the release observed
   through a host call.
4. Docs: language reference §6.4 (types table per language, the allocator rule, handles);
   CHANGELOG; the README language table's "Stateful service" column can then say
   "handle" for the C-ABI languages.

A day for strings, a day for handles; arrays and structs one to two days more.

## 3. The standard library beyond `int`

### What is missing

`stdlib/lists` is entirely `int[]`: `head(items: int[]) -> option<int>`, `sum`, `sort`,
`unique`, `flatten`. The implementations in `impl.rs` could be generic, but a Rust
`.orch_ffi` sidecar cannot say so, because `register_rust_ffi_from_sidecar` registers
monomorphic signatures — even though the typechecker already resolves generic functions
(`generic_functions`, `unify_type_param`) and codegen already emits the bounds.

### Design

- Type parameters in sidecar signatures: `reverse<T>(items: T[]) -> T[]`,
  `head<T>(items: T[]) -> option<T>`, `flatten<T>(lists: T[][]) -> T[]`. The Rust
  implementation is written generically (`pub fn reverse<T: Clone>(items: Vec<T>) ->
  Vec<T>`), and codegen already calls it as `lists::reverse(items)`, so Rust infers `T`.
- Numeric functions stay concrete: `sum`, `min`, `max` for `int`, and `sum_float`,
  `min_float`, `max_float` for `float`. Overloading by type is a bigger change than this
  and not worth it for six functions.
- `strings` is already string-typed and unchanged.

### Steps

1. `ffi_rust.rs`: parse `<T, U>` after the function name; register with the typechecker
   as a generic function so calls unify `T` from the arguments, as user generics do.
2. `stdlib/lists/impl.rs` and its sidecar: generic versions of the structural functions,
   the `float` variants of the numeric ones.
3. Tests: a runtime test that reverses a `string[]`, takes `head` of a `float[]`, and
   flattens a `string[][]`; the installed-layout library test still passes.
4. Docs: the standard library section the language reference does not have yet — this is
   the moment to add it.

Half a day to a day. Patch or minor at the maintainer's call: new signatures, no changed
behaviour.

## 4. Sandboxed serverlets: honest first, contained second

### What is missing

`serverlet X sandbox(...)` parses, validates, and builds a `wasm32-wasip1` guest — and
then runs the serverlet in-process with no isolation, with a warning. Steps 3–7 of
[design/sandboxed-serverlets.md](../design/sandboxed-serverlets.md) (wasmtime wiring,
memory limits, epoch timeouts, string marshaling, grants as narrow host functions) are the
real work and are planned there in detail; this section does not repeat them.

### The interim step

Until step 3 lands, the syntax promises containment it does not deliver. Make the
declaration an error unless the run opts in:

- `orchestrate run`/`build`/`build --lib` fail on a `sandbox(...)` serverlet with:
  "sandboxed serverlets are not isolated yet; pass `--allow-unsandboxed` to run it
  in-process, or use `secret` for process separation".
- `--allow-unsandboxed` keeps today's behaviour, warning included.
- `check` and `check-foreign` are unaffected.

That is `driver.rs`'s `warn_sandbox_serverlets` becoming a check with a flag, a CLI flag
in `cli.rs`, one test in `tests/error_case_tests.rs`, and a note in the language reference
and CHANGELOG. Half a day, and it removes the one place the language currently overstates
a guarantee.

## Order

1. Sandbox interim flag (half a day; removes a false promise).
2. Generic stdlib (half a day to a day; small, and the language reference gains its
   standard-library section).
3. `result<T, E>` (a day).
4. C-ABI strings, then handles (a day each) — this one also answers the host's stateful
   FFI question without a serverlet.
