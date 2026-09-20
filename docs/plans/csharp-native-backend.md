# C# in-process: the lowest boundary that works

> Status: **shipped in 0.10.0 for `load_foreign "csharp"`.** The route was chosen before
> the build, and the numbers below were measured on 2026-09-19 with .NET 9.0.318 on an
> Apple M2 once it worked. What remains unbuilt is listed under "Not built".

## Why

C# is the largest ecosystem OrchestrateLang cannot reach. It has been on the roadmap as
"Native AOT exports with `[UnmanagedCallersOnly]`" since the polyglot work began, and the
[foreign-language scorecard](../../website/polyglot.html) rates it *fits with caveats*:
exports cleanly, huge ecosystem, heavy toolchain.

The goal here is narrower than "support C#": it is **the lowest-latency boundary that can
carry C# without breaking the module model**. A boundary that is fast but only works for
one module per program is not a boundary this language can offer.

## The four routes

| Route | Cost per call | What it costs you |
|---|---|---|
| NativeAOT **static** archive, linked in | unmeasured | **Only one per process.** Disqualified; see below. |
| NativeAOT **shared** library | **~5 ns, measured** | A .NET GC in your process; a multi-MB library. **This is what shipped.** |
| C# → wasm, through `load_foreign "wasm"` | ~0.1 µs, measured | .NET's wasm runtime in the guest; no compiler work |
| Landline serverlet (`via csharp`) | ~17–25 µs by analogy | A second process; no in-process GC; full .NET |

### Static is the fast one and it is disqualified

`dotnet publish -p:NativeLib=Static` produces a real static archive, and linking it the way
Zig and Swift archives are linked would give the cheapest possible call. But **two
NativeAOT static archives cannot be mixed in one loadable module**: each embeds its own
copy of the runtime, and the linker cannot reconcile them
([dotnet/runtime#77951](https://github.com/dotnet/runtime/issues/77951),
[dotnet/runtimelab#1827](https://github.com/dotnet/runtimelab/issues/1827)).

This is the Go constraint that keeps Go at *exploring* — one library per process — and it
should disqualify static here for the same reason. A program that imports two modules that
each `load_foreign "csharp"` would fail to link, and the failure would arrive as a wall of
duplicate-symbol errors from the linker, not as a diagnostic the compiler could write.

So: **shared, not static.** The scorecard's `build` note is right that static publishing
works and should be extended to say that it does not compose.

### The route that already exists

NativeAOT-LLVM compiles C# to a standalone wasm "reactor" module — imports and exports, no
entry point — and exports `[UnmanagedCallersOnly]` methods as plain wasm functions
([dotnet/runtimelab#2204](https://github.com/dotnet/runtimelab/issues/2204),
[MinimalDotNetWasmNativeAOT](https://github.com/SteveSandersonMS/MinimalDotNetWasmNativeAOT)).

Since 0.9.0 that is a boundary this language already has. A C# module compiled to wasm is
callable through `load_foreign "wasm"` with **no compiler work at all**, at the ~0.1 µs a
wasm call was measured to cost, and with the containment that comes free: the guest has its
own linear memory, reaches nothing, and brings no GC into the host process.

It remains the right boundary for a C# module that should not share a heap with the host,
and it is 20× slower than what shipped: 100 ns against 5 ns. It is not written up yet.

## What the probe established

1. **A call costs about 5 ns.** Two million calls of `long Double(long)` through
   `[UnmanagedCallersOnly]` in a `NativeLib=Shared` library, against the same function in C
   in the same program: **C# 4–7 ns, C 1–2 ns**. The difference is the transition a reverse
   P/Invoke makes entering and leaving managed code. That is 20× cheaper than the same
   function reached as a WebAssembly module (~100 ns), which settles the choice.
2. **Two shared libraries coexist.** Two C# modules in one program, each its own published
   library, both called: covered by `runtime_ffi_csharp_two_modules_in_one_program`. This
   is the test that would fail if the backend ever moved to static archives.
3. **Naming.** Native AOT names the library after the assembly with no `lib` prefix, and a
   Unix linker given `-l<name>` looks for `lib<name>`. The build stages a copy under the
   expected name and, on macOS, rewrites its install name to `@rpath/...` so the copy is
   what loads.
4. **`bool`.** Confirmed not blittable in an export signature. A sidecar `bool` is a C#
   `byte` returning 0 or 1 — the same byte a C `_Bool` returns, so the existing wrapper
   reads it unchanged.
5. **Build cost.** The first publish downloads the Native AOT compiler, which is slow and
   large; later builds reuse it. `dotnet publish` runs per module per build.

Not measured, and still the open question: **GC behaviour under an allocating handler.**
Nothing here allocates. See below.

## The question that matters most here

Every other foreign boundary this language has is either garbage-collector-free (C, C++,
Zig, Swift, Rust, wasm) or in another process (Python, TypeScript landlines). A NativeAOT
library is the first that would **put a garbage collector inside the host process**.

The first adopter is a game engine driving the library per frame. A collection that pauses
the engine's own thread is a frame hitch, and it would be caused by a foreign module rather
than by anything the engine did. That is a real cost to weigh against a few nanoseconds,
and it is why route three is listed as a serious contender rather than a fallback:

- wasm keeps the guest's memory and its collector inside the guest, bounded by the sandbox;
- a landline keeps them in another process entirely;
- only the native routes share a heap with the host.

This was not measured, and it is the open question the 5 ns does not answer. Until someone
measures p99 under an allocating handler, the honest guidance — which the language
reference gives — is to keep a hot path allocation-free, or to use a boundary that keeps
the collector elsewhere.

## Design, as built

```orchestrate
// math/module.orch
load_foreign "csharp" "./Math.cs"
```

```
// math/Math.orch_ffi — the same sidecar contract as every other language
double(n: int) -> int
scale(x: float, by: float) -> float
```

- **Shared library.** The compiler generates a throwaway `.csproj` beside the source,
  publishes it with `-p:NativeLib=Shared`, and links the result, the way Zig and Swift
  sources each become one library today.
- **The user's own file, annotated.** The `.cs` carries its own
  `[UnmanagedCallersOnly(EntryPoint = "...")]` attributes; the compiler does not stage an
  adapter. That is the simpler half of the design and the reason `bool` surfaces as `byte`
  in user code rather than being hidden. An adapter is still the better end state.
- **Type mapping** (the sidecar types that can cross without a managed copy):

  | `.orch_ffi` | C# export signature | Rust side |
  |---|---|---|
  | `int` | `long` | `i64` |
  | `float` | `double` | `f64` |
  | `bool` | `byte`, 0 or 1 | `i8 != 0` |
  | `void` | `void` | `()` |

  Strings, arrays, structs, and `handle` do not cross yet, the same line every other C-ABI
  language started behind.
- **No `check-foreign` entry.** `dotnet` has no syntax-only check for a single file outside
  a project, so C# is absent from that command; `dotnet publish` reports its errors during
  the build instead.
- **Not a landline.** `via csharp(...)` is a separate feature with a separate design; this
  is `load_foreign` only, and the declaration should say so if someone tries it.

## Not built

- **Strings, arrays, structs, `handle`.** Only `int`, `float`, `bool` and `void` cross.
- **An adapter.** The user writes `[UnmanagedCallersOnly(EntryPoint = "...")]` themselves
  rather than the compiler generating a wrapper around plain C# methods. That is the
  simpler half of the design and it is what shipped; an adapter would also let `bool` be
  written as `bool`.
- **`check-foreign`.** `dotnet` has no syntax-only check for one file outside a project.
- **Cross-compilation**, as for Zig and Swift.
- **A C# landline serverlet**, which is a separate feature with a separate design.

## Out of scope

A C# landline serverlet, arrays and structs across the boundary, cross-compiling to another
target, and hosting the full CoreCLR runtime rather than a NativeAOT library.
