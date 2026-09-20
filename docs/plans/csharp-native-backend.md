# C# in-process: the lowest boundary that works

> Status: **planned, not started, and not yet probed.** Everything under "What is already
> settled" is from .NET's own documentation and issue tracker, read on 2026-09-19; the
> numbers that decide this design do not exist yet and are listed under "What the probe
> must establish". Nothing here should be built before that probe runs. Ship as a minor
> release when it does.

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
| NativeAOT **static** archive, linked in | lowest, unmeasured | **Only one per process.** See below. |
| NativeAOT **shared** library | lowest + a dynamic call | A .NET GC in your process; a multi-MB library |
| C# → wasm, through `load_foreign "wasm"` | **~0.1 µs, measured** | .NET's wasm runtime in the guest; **works today** |
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
works and should be extended to say that it does not compose; re-score it when this lands.

### The route that already exists

NativeAOT-LLVM compiles C# to a standalone wasm "reactor" module — imports and exports, no
entry point — and exports `[UnmanagedCallersOnly]` methods as plain wasm functions
([dotnet/runtimelab#2204](https://github.com/dotnet/runtimelab/issues/2204),
[MinimalDotNetWasmNativeAOT](https://github.com/SteveSandersonMS/MinimalDotNetWasmNativeAOT)).

Since 0.9.0 that is a boundary this language already has. A C# module compiled to wasm is
callable through `load_foreign "wasm"` with **no compiler work at all**, at the ~0.1 µs a
wasm call was measured to cost, and with the containment that comes free: the guest has its
own linear memory, reaches nothing, and brings no GC into the host process.

This should be written up and tested before any native backend is built, because it may be
enough. Twenty times slower than a native call is still 100 ns; a hot loop calling it 500
times a frame spends 50 µs of a 16.6 ms budget.

## What the probe must establish

The TypeScript backend was designed around one measurement — 5.1 ns — taken before any
compiler work. C# gets the same treatment. None of these numbers are published, and each
one can change the design.

1. **The call cost.** A `[UnmanagedCallersOnly]` export in a `NativeLib=Shared` library,
   called a million times from a C program with blittable arguments. A reverse P/Invoke
   enters cooperative GC mode on the way in and leaves it on the way out; `SuppressGCTransition`
   is documented as making a *forward* call as cheap as a direct one but is explicitly not
   for anything that re-enters the runtime, so it does not apply here. The size of that
   transition is the number that decides whether this route beats wasm.
2. **Whether two shared libraries coexist.** Build two, load both, call both, interleaved.
   Static is out; if shared has a variant of the same problem, the whole native route is
   out and wasm is the answer.
3. **Startup.** When the runtime initialises, how long it takes, and whether the first call
   is much more expensive than the rest. A 50 ms first call is a different feature from a
   50 µs one.
4. **GC behaviour under an allocating handler.** Measure p50 and p99 with a handler that
   allocates, and with one that does not. See below.
5. **Size and build time.** The archive's size on disk and how long `dotnet publish` takes,
   because both land on every build of every program that uses the feature.
6. **`bool`.** Confirm the known workaround: it is not blittable in an export signature, so
   the sidecar's `bool` must cross as `byte` and be converted on both sides.
7. **Strings.** Whether UTF-8 pointer plus length can cross without a managed copy, and
   which side frees.

The probe is a C program and a `.csproj`, not a compiler change, and it either produces a
table of numbers or a reason this is not worth building.

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

If the probe shows p99 spikes under an allocating handler, the honest answer is that the
native backend is for tools and non-realtime work, and the wasm or landline route is what a
frame loop should use. The documentation must say so plainly rather than quoting the median.

## Design, if the probe supports it

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
- **An adapter, not the user's file.** As with TypeScript's native backend, the compiler
  stages an adapter that carries the `[UnmanagedCallersOnly(EntryPoint = "...")]`
  attributes and the `bool`/`byte` and string conversions, so the user's `.cs` stays
  ordinary C# with ordinary signatures.
- **Type mapping** (the sidecar types that can cross without a managed copy):

  | `.orch_ffi` | C# export signature | Rust side |
  |---|---|---|
  | `int` | `long` | `i64` |
  | `float` | `double` | `f64` |
  | `bool` | `byte`, 0 or 1 | `i8 != 0` |
  | `string` | `byte*` + `int` | pointer and length |
  | `void` | `void` | `()` |

  Arrays, structs, and `handle` do not cross in a first version, the same line every other
  C-ABI language started behind.
- **`check-foreign`** runs `dotnet build` on the file, which is the nearest thing C# has to
  a syntax-only check.
- **Not a landline.** `via csharp(...)` is a separate feature with a separate design; this
  is `load_foreign` only, and the declaration should say so if someone tries it.

## Steps

1. **Probe.** The seven measurements above, recorded in this file under "What the probe
   established", replacing this section. No compiler changes.
2. **The wasm route, documented and tested.** A C# module compiled to wasm, called through
   `load_foreign "wasm"`, as a runtime test and a documented recipe. This ships C# support
   with no new compiler surface and gives the native route something to beat.
3. **Decide.** If the probe's numbers do not clearly beat step 2 for a realistic workload —
   including p99 under allocation — stop here and say so in the roadmap.
4. **`ForeignSource::CSharp`,** a toolchain entry, the generated `.csproj`, the adapter
   emitter, and the `build.rs` line that links the library. Four edits, the same shape as
   every other C-ABI language.
5. **Runtime tests** covering each type in the table, two modules in one program, and
   startup.
6. **Docs:** `language-reference.md` §6.4, the roadmap entry, the scorecard's `build` and
   `runtime` notes, and the ladder on the website.

## Out of scope

A C# landline serverlet, arrays and structs across the boundary, cross-compiling to another
target, and hosting the full CoreCLR runtime rather than a NativeAOT library.
