# As many choices as possible are made in syntax

> One of the four foundational principles. The others are
> [concurrency is the structure](concurrency-is-the-structure.md),
> [asynchronous first, synchronous second](asynchronous-first-synchronous-second.md), and
> [polyglot coordination and attachment](polyglot-coordination-and-attachment.md). The
> operational principles are in [design-philosophy.md](../design-philosophy.md).

## The claim

Every system of any size accumulates choices: which transport, which runtime, how much
memory, what may be reached, how long to wait. The question is only where those choices
are written down. The usual answers are a configuration file, an environment variable, a
builder chain, or a flag on the build command — all of which sit somewhere other than the
thing they describe, and all of which drift.

OrchestrateLang's answer is that a choice belongs in the declaration it applies to, as a
word you can see. Not because syntax is prettier, but because a word in a declaration has
properties config does not:

- It is **local**. The cost of a thing is written where the thing is.
- It is **greppable**. `grep -rn 'sandbox(' src/` is a complete inventory, and so is
  `grep -rn 'grant call'`.
- It is **diffable**. Changing a boundary is a reviewable line in a pull request, not an
  environment difference that shows up in production.
- It is **checkable**. The compiler reads the same text you do, so a mistake in it is a
  compile error rather than a run-time surprise.

## The catalogue

| You write | You get | You pay |
|---|---|---|
| `serverlet X { }` | An in-process actor | A channel round trip |
| `serverlet X secret { }` | A separate native executable | A pipe and a protocol |
| `serverlet X via python(source: …)` | A persistent Python process | An interpreter, and its encoding |
| `serverlet X sandbox(memory_limit: …, timeout: …)` | A wasmtime guest | A cap, a deadline, and a copy per value |
| `grant call world.record` | One mediated host function | Exactly that function, and nothing else |
| `budget: "2ms"`, `late: "latest"` | A call that cannot hold the tick | A default or a stale result when it is late |
| `automatic(restart: 3) { } on_crash e { }` | A supervised worker | A task |
| `shared let x = 0` | State any task may touch | One mutex, about 9 ns a statement |
| `let x = 0` | Instance state | Nothing; only hooks reach it |
| `fn` / `task` | Synchronous / asynchronous | A `fn` may not wait, and is told so |
| `load_foreign "c" "./m.c"` | A linked C function | A build dependency on `cc` |
| `load_foreign "wasm" "./m.wasm"` | A checked, embedded module | A `wasmtime` dependency |
| `(backend: "bun")` | That file's TypeScript compiler | Bun's runtime for that module |
| `handle` + `drop release(h: handle)` | A native object with a lifetime | A release call when the last owner drops |
| `use module m: "./dir"` | A consent boundary | Nothing |
| `process[a, b]` | The worker set an orchestrator owns | Nothing |
| `on_tick(dt: float)` / `on_fixed_tick(step: float)` | A hook the host drives | A library build |

Read down the middle column and you have the language's feature list. Read down the
right-hand column and you have its cost model. They are the same table because they are
written in the same place.

## The call site does not move

This is what the principle buys. Four programs, four boundaries, one call:

```orchestrate
serverlet Plugin { on run(input: string) -> string { return input } }
serverlet Plugin secret { on run(input: string) -> string { return input } }
serverlet Plugin via python(source: "plugin.py") { on run(input: string) -> string }
serverlet Plugin sandbox(memory_limit: "64mb", timeout: "5s") {
    on run(input: string) -> string { return input }
}
```

In all four, the caller writes:

```orchestrate
let plugin = start Plugin()
print(plugin.run("hello"))
```

Changing where a module's code runs — in this process, in a child, in another language,
inside a sandbox — is one word on one line, and no caller is touched. What differs
between them is not the interface but the guarantees, and those are documented per
boundary rather than hidden behind a common denominator.

## The compiler charges only for what is declared

The same principle, viewed from the generated side. A program that does not write a word
does not pay for the machinery behind it:

- No `shared let` → no mutex, no field, no code.
- No sandboxed serverlet and no `.wasm` module → no `wasmtime` dependency, and the
  `wasm32-wasip1` target is not needed.
- No landline and no secret serverlet → the generated library does not require Tokio's IO
  driver, and says so through `NEEDS_IO_DRIVER`.
- No `handle` in any sidecar → the handle type is not emitted.
- No C or C++ source → no `cc` build dependency, and no `build.rs` at all.

A word you did not write costs nothing. That is what makes it reasonable to have many of
them.

## Where the choice is not in syntax, and why

The principle is "as many as possible", not "all", and the exceptions are the interesting
part. Each of these is deliberately *not* in the source:

- **`StartOptions { deterministic: true }`** is the host's choice, not the program's. The
  same script is driven normally by one host and deterministically by another, and a
  program cannot know which it will be. Putting it in the source would be a lie about who
  owns it.
- **`ORCH_PYTHON`, `ORCH_DOTNET`, `ORCH_TSC`, `ORCH_BUN`, `ORCH_SCRIPTC`** locate
  toolchains. Where an interpreter lives is a fact about a machine, not about a program,
  and it changes between a developer's laptop and CI without the program changing.
- **`ORCH_TS_BACKEND`** is the project-wide default for TypeScript declarations that say
  nothing — and a declaration that does say `(backend: "bun")` overrides it. That
  ordering is the principle: syntax wins where it is written, and the environment only
  fills silence.
- **`build` versus `build --lib`** is not a choice in the source either, and for the same
  reason as deterministic mode: it is the question of who owns the loop, which belongs to
  whoever is embedding. One source, two outputs, covered in
  [asynchronous first, synchronous second](asynchronous-first-synchronous-second.md).

## What it costs

- **Combinations have to be enumerated.** `via` cannot be combined with `secret` or
  `sandbox`, because there is no implementation of a Python process that is also a WASM
  guest. The parser refuses it by name rather than silently preferring one.
- **Some declarations are only valid in one mode.** `host`, `on_tick`, and `grant call`
  require a library build; in a standalone program they are a compile error, since the
  host functions they name do not exist.
- **Adding a backend means adding syntax**, and syntax is forever. That is a real brake
  on the feature list, and it is meant to be: a boundary that is not worth a keyword is
  not worth shipping.
- **A word can promise more than the implementation delivers**, which is why this
  principle needs the honest-guarantees one beside it. `secret` is not encryption, and a
  separate process is not a sandbox; the names are chosen to say only what is true.
