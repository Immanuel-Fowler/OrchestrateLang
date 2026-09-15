# TypeScript 7 landlines and FFI

OrchestrateLang compiles TypeScript implementations into executables. It checks every
implementation with TypeScript 7 before compiling it. The compiler first attempts
`scriptc` for a native executable, then uses `bun build --compile` when the code or its
wire types need Bun. The chosen backend is printed during the build.

Install toolchain dependencies with Bun in the project that owns the TypeScript source:

```sh
bun add --dev typescript scriptc @types/bun
```

The compiler finds `node_modules/.bin` by walking from the source directory. Set
`ORCH_TSC`, `ORCH_SCRIPTC`, or `ORCH_BUN` to override a tool path. `ORCH_TS_BACKEND`
accepts `auto` (default), `scriptc`, or `bun`. A forced `scriptc` build fails instead of
falling back. OrchestrateLang never installs packages while compiling.

> **Note:** `ORCH_TS_BACKEND` is a project-wide environment variable. Selecting the
> backend per-file or per-serverlet directly in `.orch` source is planned for a future
> release.

## Landline serverlets

Declare a serverlet in `.orch` and default-export a TypeScript class:

```orchestrate
serverlet Counter via typescript(source: "counter.ts") {
    on add(amount: int) -> int
}
```

```ts
export default class Counter {
  total = 0n;

  add(amount: bigint): bigint {
    this.total += amount;
    return this.total;
  }
}
```

Handler methods must have the names and wire types declared in `.orch`. They may return a
value or a `Promise`. The constructor may accept one context argument:

```ts
type Context = {
  tickNumber: bigint;
  tickDt: number;
  host: Record<string, Record<string, (...args: unknown[]) => unknown>>;
};
```

`tickNumber` and `tickDt` are populated for a handler named `tick` invoked from a typed
library `on_tick`. A grant such as `grant call world.record` exposes
`context.host.world.record(...)` while a handler is running. Host errors throw in
TypeScript. Landline `console.log`, `console.info`, and `console.debug` go to stderr and,
in library mode, reach `Host::log`; stdout is reserved for protocol frames.

The wire mapping is `int` → `bigint`, `float` → `number`, `bool` → `boolean`, `string` →
`string`, arrays → `T[]`, and same-file `.orch` structs → matching TypeScript object
shapes. Values are checked at the boundary. Optional/result/function types are not wire
types. Like Python landlines, TypeScript landlines run as the host OS user and are not a
sandbox. They are also excluded from deterministic library mode.

`budget` and `late` work the same way as Python landlines. A long-running TypeScript call
can return a default immediately when it exceeds its budget; use `late: "latest"` to read
the most recently completed result on later calls.

`run` and standalone `build` place the executable in `landline_<Name>/` beside the
program. Library builds embed and extract it per script instance. Players therefore do
not need Bun, TypeScript, scriptc, or the original `.ts` file; package any application data
and native dependencies used by the implementation separately. Cross-target TypeScript
artifacts are currently rejected because the executable is built for the compiler host.

## FFI bridge

Use a normal foreign module plus a `.orch_ffi` sidecar:

```orchestrate
// math/module.orch
load_foreign "typescript" "math.ts"
```

```text
// math/math.orch_ffi
twice(value: float) -> float
```

```ts
// math/math.ts
export function twice(value: number): number {
  return value * 2;
}
```

TypeScript FFI calls are synchronous. The native `scriptc` fast path supports scalar
`float`, `bool`, and `void` functions. The Bun transport handles full-width `int` values
as `bigint`, strings, arrays, and structs. A fresh process serves each FFI call, so use a
landline for persistent state, asynchronous work, or calls that need budgets.
