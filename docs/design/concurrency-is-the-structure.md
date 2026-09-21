# Concurrency is the structure

> One of the four foundational principles. The others are
> [asynchronous first, synchronous second](asynchronous-first-synchronous-second.md),
> [polyglot coordination and attachment](polyglot-coordination-and-attachment.md), and
> [choices made in syntax](choices-in-syntax.md). The operational principles are in
> [design-philosophy.md](../design-philosophy.md).

## The claim

In most languages concurrency is a library you call. You spawn a task, you keep its
handle somewhere, you wire a channel to it, and the shape of the running program lives in
your head rather than in the source.

In OrchestrateLang the shape *is* the source. A program declares what kinds of thing it
contains, and the compiler writes the concurrency those kinds imply. There is no `spawn`
to call, no channel to construct, no handle to store, and no shared pointer to reach for.
Reading the top level of a file tells you what runs, what it owns, and how it can fail.

## Five declarations, five owners

| Declaration | Owns | Runs |
|---|---|---|
| `orchestrator main(...)` | Startup, shutdown, and the active worker set | Once, at the top |
| `automatic { ... }` | Recurring work, its restart policy, its crash handler | On its own task, repeatedly |
| `on <event>(...) { ... }` | One typed reaction | Whenever the event is triggered |
| `serverlet X { ... }` | State, and the order calls touch it | On its own task, one call at a time |
| `parallel { ... }` | A join point for independent work | Concurrently, until all branches finish |

```orchestrate
serverlet Scoreboard {
    let total = 0

    on add(n: int) -> int {
        total = total + n
        return total
    }
}

let scores = start Scoreboard()

let ticker = automatic(restart: 3) {
    scores.add(1)
    sleep(1000)
} on_crash e {
    print("ticker stopped: " + e)
}

let audit = on user_created(id: int, name: string) {
    print("created {id}: {name}")
}

orchestrator main(procs: process[ticker, audit]) {
    trigger user_created(42, "Mina")
}
```

Nine lines of declarations describe a program with a supervised worker restarting up to
three times, a typed multicast event with one subscriber, and a service whose state no
two callers can race on. Nothing in it is a library call.

## What the compiler writes instead

The generated Rust for that program contains a `tokio::spawn` per worker and per
serverlet, an `mpsc` channel and a message enum per serverlet, a `oneshot` reply channel
per call, an event registry per event name, `catch_unwind` around every handler body, and
abort handles the orchestrator uses on shutdown. None of it appears in the source,
because none of it is a decision the source needs to make twice.

The rule this follows: **a construct that implies concurrency generates it; a construct
that does not, does not.** A program with no `automatic`, no `serverlet`, and no event
gains no tasks and no channels at all.

## State follows the same rule

Who may touch a binding is a property of how it is declared, not a convention:

| You write | Who reaches it | Cost |
|---|---|---|
| `let x = 0` inside a serverlet | That serverlet's handlers, one at a time | A field access |
| `let x = 0` at the top level | The instance's hooks: `on_start`, `on_tick`, `on_stop`, event handlers | A field access |
| `shared let x = 0` | Any task, any `fn`, any worker | About 9 ns per statement |
| A serverlet call | Anything holding the client | 7–25 µs, depending on the boundary |

The middle two rungs exist because they answer different questions. A top-level `let` is
owned by the instance and a spawned worker can run while a tick holds it, so a `fn`,
`task`, or `process` that names one is refused by the compiler with the binding named.
Marking it `shared` puts it behind one mutex and makes it reachable from everywhere, and
the guarantee is one sentence: *a statement that touches shared state is atomic with
respect to all shared state.* A serverlet is the rung above: state plus a protocol, at
the cost of a round trip.

Those costs are measured, not asserted; the tables and the machine they were taken on are
in [benchmarks/README.md](../../benchmarks/README.md).

## Failure is structural too

A worker declares what happens when its body panics — `automatic(restart: 3)`,
`always`, `never` — and an `on_crash` handler beside it receives the error. A serverlet
declares its own `on_crash`. What that handler means is a property of the boundary, which
is the subject of [polyglot coordination and attachment](polyglot-coordination-and-attachment.md):
a panic in-process, a transport failure on a landline, a trap in a sandbox. In every case
the failure is contained to the declaration that owns it, and the rest of the program
keeps running.

## What it costs

- **A top-level `let` is not reachable from a `fn`, `task`, or `process`.** That is
  deliberate: a spawned worker could run while a tick holds the binding. The way out is
  a parameter, or `shared let`.
- **A statement may wait, or touch shared state, not both.** Holding the lock across a
  wait would block every other reader, so it is a compile error that says to split the
  statement.
- **A serverlet handles one call at a time.** That is the guarantee, and it is also the
  limit: eight concurrent callers on one serverlet queue behind each other, and on a
  process-backed boundary the queue is visible in every caller's latency.
- **Worker sets are replaced, not mutated.** `trigger update_orchestrator([...])` swaps
  the set the orchestrator owns; there is no handle to abort by hand.
