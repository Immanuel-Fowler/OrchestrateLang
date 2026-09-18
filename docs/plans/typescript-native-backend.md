# TypeScript in-process: the `native` backend

> Status: **planned, not started.** Everything in "What the probe established" was
> measured on 2026-09-18 with scriptc 0.1.1 on macOS; everything under "Steps" is the
> proposal. Ship as a minor release (`backend: "native"` is a new value, and no existing
> program changes behaviour).

## Why

`load_foreign "typescript"` runs a persistent child process and talks to it over pipes:
about 60 µs per call since v0.6.0, down from a process spawn per call before it. That is
fine for tools and event handlers, and two orders of magnitude too slow for a function a
game calls per entity per frame. Rust, C, C++, Zig, and Swift foreign functions cost
1–5 ns because they are linked into the generated crate. TypeScript could not be, because
no TypeScript compiler produced a linkable library — until scriptc's library mode.

## What the probe established

`scriptc build --lib --profile <p.json> -o <archive>` compiles the profile's entry module
to a static archive that exports the C symbols the profile declares. It is undocumented on
scriptc.dev; the schema below was recovered from the validator's own messages.

```json
{"profile_format": 1, "name": "math", "entry": "./math.ts", "emission": "llvm",
 "abi": {"prefix": "orch_", "init_symbol": "orch_init", "sink_register_symbol": "orch_sink"},
 "exports": [{"export": "twice", "symbol": "orch_twice", "params": ["f64"], "returns": "f64"},
             {"export": "next",  "symbol": "orch_next",  "params": [],      "returns": "f64"}]}
```

- Every symbol must start with `abi.prefix`; `emission` is `llvm` (ships) or `c` (readable).
- Marshalling classes: `f64`, `bool`, `string`, `bytes` (`Uint8Array`/`Buffer`), `u8`,
  `u32`, `i32` (parameters only), `i64`, `u64`. The static tier has **no `bigint`**: a
  TypeScript `number` is what `i64` parameters arrive as, and an `i64` *return* is refused
  unless scriptc can prove the value stays in range (`return a + b` on two numbers is
  refused with SC4003 and the hint "bound the value before the boundary").
- Arrays, records, and unions are deferred by scriptc ("when the contract-sidecar
  conversation lands"), so `int[]`, `string[]`, and structs cannot cross this boundary.
- The archive's C surface for the profile above, from the emitted `.lib.ll`:
  `void orch_init(void)`, `void orch_sink(void *fn, void *ctx)`,
  `double orch_twice(double)`, `double orch_next(void)`; a `string` parameter is
  `(const uint8_t *ptr, int64_t len)` and a `string` return is two out-parameters
  `(void *out, void *out_len)`; `bool` is `int8_t`.
- Linked into a C program with `cc -O2 main.c libmath` and no other libraries on macOS:
  `twice(3.5) = 7`, `next() = 1` then `2` (module state persists in the process), and
  **5.1 ns per call** over one million calls.
- `SCRIPTC_TARGET=wasm32-wasi` builds a WASI *executable*; library mode is rejected there
  (SC3002). WASM is a containment story for TypeScript, not this one.

## Design

```orchestrate
load_foreign "typescript" "math.ts" (backend: "native")
```

- `native` is explicit. `auto` keeps choosing between the two process backends until a
  release of `native` has proven the constraints below are livable; then `auto` can try
  `native` first for eligible sidecars.
- `native` applies to `load_foreign` only. A landline (`via typescript`) is a stateful
  service with asynchronous handlers, ticks, and host callbacks; none of that fits a
  synchronous C export. `via typescript(... backend: "native")` is an error that says so.
- The compiler stages an **adapter module** and exports the adapter, not the user's
  functions, exactly as the process backends stage `main.ts`. The adapter converts
  errors and types at the boundary so the user's file stays ordinary TypeScript.
- Type mapping for a `native` sidecar, chosen to be exact where the process backends are
  exact and refused where they would be silent:

  | `.orch_ffi` | adapter parameter | C class | Rust side |
  |---|---|---|---|
  | `float` | `number` | `f64` | `f64` |
  | `bool` | `boolean` | `bool` | `i8 != 0` |
  | `int` parameter | `number` | `i64` | `i64` |
  | `int` return | `number` | `f64` | `as i64`, exact to 2⁵³; documented as the native path's limit |
  | `string` | `string` | `string` | pointer + length in, out-parameters copied to `String` |
  | `void` | — | void | `()` |
  | arrays, structs | — | — | refused: "use backend scriptc or bun" |

  Returning `int` as `f64` sidesteps the range proof without clamping anything: the
  adapter returns the number as is, and a value beyond 2⁵³ was already inexact in
  TypeScript. The process backends carry full-width `int` as `bigint`; the docs say which
  path has which limit.
- Errors: the adapter wraps each call in `try`/`catch` and reports failure through a
  status the Rust wrapper turns into the same `panic!("TypeScript FFI: …")` the bridge
  raises, so a failing call fails only that call and the caller sees one behaviour.
- Console output: the adapter forwards `console.*` through the sink the profile registers;
  the Rust side routes it to stderr in a binary and to `Host::log` in library mode, which
  is where the process backends' stderr already goes.
- The archive is copied into the generated crate (`native/<lib>.a`) and linked from
  `build.rs` with `cargo:rustc-link-search` and `cargo:rustc-link-lib=static`, the Zig
  pattern, so a `build --lib` output directory is self-contained and needs neither scriptc
  nor the `.ts` file on the host. Host target only, with the same clear error on
  `--target` that Zig and Swift give.
- `NEEDS_IO_DRIVER` is unaffected: a native module is a function call.

## Weak points in the code

| File | Location | What changes |
|---|---|---|
| `src/parser.rs` | `typescript_backend` (accepts `auto`, `scriptc`, `bun`) and the landline `backend` key | Accept `native` on `load_foreign`; reject it on `via typescript` with a message |
| `src/typescript.rs` | `build` picks scriptc-executable or Bun; `ffi_bindings` emits the bridge module around `typescript_ffi.rs.txt`; `ts_type` maps `int` to `bigint` | Add `build_native` (adapter + profile + `scriptc build --lib`), `ffi_bindings_native`, and a native type map |
| `src/driver.rs` | `ForeignSource` (C, Cpp, Zig, Swift) and the `build.rs` generator; the module loop's `load_foreign "typescript"` arm; `run_build_library_for_target`'s host-target check via `backend.txt` | New `ForeignSource::TypeScript`; copy the archive into the output crate; count `native` in the host-target check |
| `src/foreign_check.rs` | TypeScript is checked by `tsc` against its contract | Same check; under `--deep`, also run `scriptc build --lib` so a static-tier refusal is a check failure, not a build failure |
| `tests/typescript_tests.rs` | `available()` needs TypeScript 7 and Bun | Gate native tests on `scriptc` as well; CI already installs it |
| `docs/language-reference.md`, `sdk/typescript/README.md`, `docs/library-mode.md`, `README.md` | Describe two process backends | Third backend, its type limits, and the packaging note |

## Steps

Each step is independently reviewable; 1–3 can land before 4–6 without changing any
existing behaviour.

1. **Pin and probe (½ day).** Gate on `scriptc --version` ≥ 0.1.1 with a clear install
   hint, since the profile schema is unversioned beyond `profile_format: 1`. Write a C
   harness (as the 2026-09-18 probe did) that settles the four unknowns and record the
   answers in this file:
   - who owns the buffer a `string` return writes to `out`, and how it is released;
   - what an uncaught `throw` does at the C boundary (this decides whether the adapter's
     `try`/`catch` is a nicety or a requirement);
   - whether calls from several threads are safe, or must be serialized (a `Mutex<()>` in
     the wrapper costs ~20 ns and would still leave the path 1,000× cheaper than the bridge);
   - the sink callback's signature, and whether `init` may be called more than once.
2. **Parser (small).** `native` on `load_foreign "typescript"`; the landline refusal;
   unit tests beside `test_parser_load_foreign_backend`.
3. **`typescript::build_native`.** Stage `native.ts` (adapter) beside the checked source,
   generate the profile from the sidecar's handlers, run `scriptc build --lib --profile`,
   map `SC4001`/`SC4003`/`SC2001` to `[orchestrate]` diagnostics that quote scriptc's own
   hint, write `backend.txt = native`. Refuse array and struct types before calling
   scriptc, naming the two backends that carry them.
4. **Link and package.** `ForeignSource::TypeScript(archive, lib_name)`; `build.rs`
   search path and static link; copy the archive into the generated crate for `build
   --lib`; `run`/`build` link from the cache. Extend the host-target check.
5. **Bindings.** `mod __orch_ts_<asset>` with `extern "C"` declarations, a `OnceLock`
   that calls `init` and registers the sink once per process, the marshalling in the
   table above, the error status turned into the bridge's panic, and the same
   `pub fn r#name(args) -> T` surface as the bridge so no caller changes.
6. **check-foreign.** The default check is unchanged (`tsc` against the contract);
   `--deep` adds the library build.
7. **Tests** (`tests/typescript_tests.rs`, gated on scriptc; `tests/foreign_check_tests.rs`):
   native build and calls; state persisting across calls; `string` round trip; `int`
   parameters and returns at the exact edge (2⁵³) and beyond it (documented inexactness);
   a thrown error failing only its call; console output reaching `Host::log`; refusal
   messages for arrays, structs, and `via typescript(... native)`; a `build --lib` output
   directory moved elsewhere still building its host; `NEEDS_IO_DRIVER` false; and a
   timing sanity check that a native call is cheaper than a bridge call to the same
   function (no absolute numbers — CI machines vary).
8. **Docs and release.** Language reference (TypeScript 7 section: three backends and the
   type table), SDK guide, library-mode "Runtime drivers" row ("`native`: in-process,
   time driver only"), README language table ("TypeScript: native or persistent adapter"),
   CHANGELOG. Release as a minor.

Later, once `native` has shipped: `auto` tries `native` first for eligible sidecars;
arrays and structs when scriptc lands records; a `bytes` mapping if a use for raw buffers
appears.

## Risks

- scriptc is 0.1.x and its library mode is undocumented; the schema or the C surface may
  change. The version gate and the probe harness (kept under `tests/` as a fixture) turn
  a change into a clear failure rather than a mysterious one.
- The static tier compiles a subset of TypeScript. A file that uses `bigint`, `any`, or
  an unsupported library member fails the native build; the diagnostic must say which
  construct and that the process backends still take the file.
- A native module shares the process: a crash in TypeScript-compiled code takes the host
  down, as a Zig or Swift crash does. That is the deal every FFI language makes here, and
  the docs say so.

## Out of scope

Landlines on the native backend, WASM (a sandboxing story, see
[design/sandboxed-serverlets.md](../design/sandboxed-serverlets.md)), arrays and structs
until scriptc carries them, cross-target archives.
