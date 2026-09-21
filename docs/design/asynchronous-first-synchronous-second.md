# Asynchronous first, synchronous second

> One of the four foundational principles. The others are
> [concurrency is the structure](concurrency-is-the-structure.md),
> [polyglot coordination and attachment](polyglot-coordination-and-attachment.md), and
> [choices made in syntax](choices-in-syntax.md). The operational principles are in
> [design-philosophy.md](../design-philosophy.md).

## The claim

Coordination is asynchronous work. A language for it should say so once, in the generated
core, and then let synchronous callers in at the edge — rather than maintaining two
runtime models that drift apart.

OrchestrateLang compiles to one async core on Tokio. Synchrony appears in exactly two
places: `fn`, the callable form that cannot wait, and the host API, where an application
that owns its own loop drives that same core. Neither is a second implementation.

## The callable forms say which they are

| Form | Compiles to | May wait |
|---|---|---|
| `fn` | A synchronous Rust function | No |
| `task` | An `async fn` | Yes |
| `process` | An `async fn` intended for orchestration work | Yes |

```orchestrate
fn normalize(value: int) -> int {
    value * 10
}

task fetch(id: int) -> string {
    sleep(100)
    return "item {id}"
}

orchestrator main() {
    parallel {
        let first = fetch(1)
        let second = fetch(2)
    }
    print(first + second)
    stop_orch()
}
```

A `fn` that calls a serverlet, a landline, a `task`, `sleep`, or a `parallel` block is a
compile error naming the function and the call, and saying to declare it a `task`. This
matters more than it sounds: the alternative is rustc reporting "`await` is only allowed
inside `async` functions" against generated code the author never wrote.

`parallel` is the one place concurrency is expressed inside a body rather than by a
declaration, and it is still structural: it is a join point, not a spawn. Every branch
runs concurrently and every binding is available after the block.

## Ownership inverts without the program changing

The same `.orch` source compiles two ways. `orchestrate build` produces a standalone
executable that owns its runtime and its `main`. `orchestrate build --lib` produces a
Rust crate that a host application links, where the host owns the runtime, the loop,
logging, and process lifetime — and the library never exits the process or starts a
runtime of its own.

A host drives that crate in whichever shape it already has:

| The host is | It calls | An empty tick costs |
|---|---|---|
| Async | `ready().await`, `tick(dt).await`, `shutdown().await` | A task hop |
| Synchronous | `ready_blocking`, `tick_blocking`, `shutdown_blocking` | A task hop, from `block_on` |
| Synchronous and hot | `tick_sync(&runtime, dt)` | About 6 ns, on the calling thread |

`tick_sync` is the interesting one. It runs the tick body on the caller's thread with no
coordinator task, no command channel, and no park: a body that finishes without waiting
returns straight away, and one that has to wait finishes under `block_on` so the outcome
matches `tick_blocking`. A host call inside a hook costs about 1.4 ns against a 2.0 ns
floor for the same trait method called through its vtable alone. The measurement, its
method, and the machine are in [benchmarks/README.md](../../benchmarks/README.md).

The hooks a host drives are declarations like any other: `on_tick(dt: float)`, a typed
`on_tick(dt: float, input: Input) -> Output`, `on_fixed_tick(step: float)`, `on_start`,
`on_stop`, and `Scripts::trigger_<event>(...)` for events fired from Rust.

## Time can become the host's

With `StartOptions { deterministic: true }` the clock stops being wall time and becomes
the sum of the `dt` values the host passes. Each tick advances it, `sleep` inside an event
handler waits until enough later tick time has passed, ready events run in FIFO order, and
the orchestrator body runs to completion during startup instead of on its own task.

The guarantee that buys: the same sequence of ticks and events produces the same host
calls, byte for byte. It is tested over 10,000 ticks with host-fired events, events fired
by handlers, handlers that sleep across ticks, and instance state, replayed on a
current-thread and a multithreaded runtime and through both `tick_blocking` and
`tick_sync`.

It is a subset, and the exclusions are named: spawned workers, serverlets, landlines, and
`sleep` outside an event handler are not supported and stop the library with a message
naming deterministic mode. Host implementations have to be deterministic themselves.
[library-mode.md](../library-mode.md) is precise about what surfaces where.

## Asynchrony that can be slow is bounded

A boundary that leaves the process can take arbitrarily long, so a landline declares its
own limit:

```orchestrate
serverlet Scorer via python(source: "scorer.py", budget: "2ms", late: "latest") {
    on score(features: float[]) -> float
}
```

A call with no reply inside the budget returns immediately: with `late: "drop"` the return
type's default, with `late: "latest"` the handler's most recent completed result, which a
late reply then replaces. A queued call whose caller has already given up is never sent.
A sandboxed serverlet declares the same thing as `timeout`, enforced by epoch
interruption rather than a timer on the caller's side.

This is what "asynchronous first" has to mean for a host with a frame budget: not that
calls are fast, but that a call which is not fast cannot hold the tick.

## What it costs

- **A generated library states which Tokio drivers it needs.** A program containing a
  landline or a secret serverlet starts child processes, so its host runtime needs the IO
  driver; the crate exports `NEEDS_IO_DRIVER` and `start` fails with a message naming the
  declarations rather than panicking inside a task.
- **Deterministic mode is a subset**, and the reason a worker is excluded is the same
  reason it is useful: it runs whether or not the host ticked.
- **A `try` block that waits generates an async block**, so `?` still works and the
  handler may wait too; one that does not wait generates exactly what it did before.
- **`stop_orch()` differs by ownership.** Standalone it exits; in library mode it sets an
  instance-local flag the host reads through `stop_requested()`, because the library does
  not get to end the host's process.
