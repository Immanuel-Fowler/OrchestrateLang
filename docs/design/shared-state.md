# Shared state — declared, not implied

> Status: **shipped in 0.13.0.** The open questions at the end were answered as this
> document recommended, and §2 records where the built semantics differ from the original
> proposal.

## 1. The philosophy: more control for no cost

OrchestrateLang's answer to "which mechanism should I use?" has never been to pick for the
developer. It is to **make each mechanism declare itself, and charge only for what is
declared**. That principle is already everywhere in the language:

| You write | The backend generates | You pay for |
|---|---|---|
| `serverlet X { }` | a Tokio task and a channel | a channel round trip |
| `serverlet X secret { }` | a separate native executable and a mirror | a process boundary |
| `serverlet X sandbox(...)` | a WASM guest under wasmtime | containment |
| `load_foreign "c"` | a direct C call | nothing |
| `load_foreign "wasm"` | a wasmtime instance | ~100 ns and isolation |
| `fn` / `task` | a synchronous fn / an `async fn` | suspension, or not |

Every one of those is the same shape: **one construct, several backends, selected by a word
in the declaration.** [Design philosophy §8](../design-philosophy.md) states the rule —
*"What the orchestrator may touch is declared, not implied. Capability markers are boring
and greppable on purpose"* — and §4 states the goal: *use the cheapest boundary that works*,
with the developer choosing the rung.

A language that instead picks one behaviour for everybody has to pick badly for somebody.
If it picks the fast one, concurrent access is unsound. If it picks the safe one, every
program pays for synchronisation it may never use. The marker escapes that trade entirely:
**the default stays free, and the capability is available to anyone who asks for it in
writing.**

That is what this feature is. `let` does not change. `shared let` is a new rung.

### Why this is not just a workaround

Today a top-level `let` is a field of `__OrchProgram`, which the instance owns and a running
hook borrows mutably. A hook reads it as `__program.counter` — a struct field offset, no
indirection, no synchronisation. That is why a tick costs about 9 ns.

It is also why a `task` cannot touch it: a spawned worker is a live Tokio task that can run
*while* a tick holds that borrow. Letting both reach the same field would be a data race,
not a missing feature. The restriction is load-bearing.

So the language today offers two rungs and nothing between them:

| | Cost per access | Who can touch it |
|---|---|---|
| top-level `let` | ~0 (a field offset) | hooks only |
| a serverlet | **~7.5 µs** (measured) | anything, one call at a time |

Seven and a half microseconds to increment a counter that a worker and a hook share is not
a trade-off, it is a missing rung. `shared let` is that rung: one uncontended mutex instead
of a channel round trip, and still exact.

## 2. The design

```orchestrate
let counter = 0          // unchanged: instance-owned, hooks only, free
shared let hits = 0      // opt-in: any task, any fn, synchronised
```

`shared` is a prefix on a top-level `let`. It does not change what the binding *is*; it
changes where the backend puts it:

- A plain `let` that a hook reaches stays a field of `__OrchProgram`, exactly as now.
- A `shared let` becomes a field of a separate `__OrchShared` struct behind one mutex,
  reachable from anywhere in the program. A standalone program keeps it in a `static`; a
  library keeps it on the instance's context, so two libraries started in one process do
  not share it.

Nothing about the existing path changes. A program with no `shared` bindings generates
byte-identical code to today, which the existing determinism test already guards.

### One mutex, not one per binding

All `shared` bindings live behind **a single mutex**, not one each. Three reasons:

1. **No lock ordering, so no deadlocks.** A statement touching two shared bindings takes one
   lock, not two in an order that another statement might reverse.
2. **The generated code stays simple** — one guard, taken once, at a known place.
3. **The guarantee is one sentence**, which matters more than the contention it costs. A
   program with enough `shared` bindings for contention to matter has outgrown this rung and
   wants a serverlet.

### The guarantee

> **A statement that touches shared state is atomic with respect to all shared state.**

The guard is taken once at the start of such a statement and dropped at its end. That makes
`hits = hits + 1` correct without the developer thinking about it, which is the whole point
— an atomic-per-*access* design would compile that into a separate load and store and lose
updates silently, and a design with no guarantee at all would be a footgun wearing a
capability marker.

**Reads clone.** The original proposal said a read would borrow, because the guard spans
the statement. It cannot: a read produces a value, and a value cannot be moved out of a
mutex guard. For a number the clone is a copy and costs nothing; for a string or an array
it is the copy the reader was going to get anyway. Writes are still places, so
`hits = hits + 1` reads a copy, adds, and writes back under one lock.

### Waiting and holding are separate

A guard must never be held across an `await`: it would be unsound, it would not compile
(the guard is not `Send`), and it would let a worker block a tick.

So the rule is the same shape as `fn` versus `task`, and it should be stated the same way:

> **A statement may wait, or it may touch shared state. Not both.**

A statement that calls a serverlet *and* reads a `shared` binding is a compile error telling
the developer to split it into two statements. That keeps the guarantee, keeps the speed,
and keeps the diagnostic in OrchestrateLang instead of in rustc's `Send` bounds.

### What it unlocks beyond tasks

Because a `std::sync::Mutex` lock is synchronous, **a plain `fn` can touch shared state
too.** `fn` still cannot call a serverlet — that genuinely requires waiting — but the state
half of the restriction goes away for anyone who asks for it. A synchronous helper called
from a Rust host, from a hook, and from a worker can all see the same counter.

## 3. What it costs

| Access | Cost | Measured? |
|---|---|---|
| plain `let` from a hook | ~0, a field offset | yes, implied by the 9 ns tick |
| `shared let`, uncontended | one uncontended mutex per statement | not measured in isolation |
| a serverlet call | ~7.5 µs | yes |

A program with no `shared` bindings pays nothing: no mutex, no `Arc`, no field, no code.
This is the same rule as `wasmtime` for programs without a sandbox and `.NET` for programs
without C#.

Per design philosophy §10 the changelog quotes no per-access figure, because none was
measured in isolation. What was measured is the behaviour that matters: three workers and a
synchronous `fn` incrementing one binding, with every increment landing — covered by
`runtime_shared_state_survives_concurrent_workers`.

## 4. How it interacts with what exists

- **Determinism.** Deterministic mode already asserts that no workers or serverlets are
  spawned, so in that mode nothing runs concurrently with a tick and shared state cannot be
  raced. Replays stay reproducible. Shared state is *not* a way around that assert.
- **Startup order.** The shared struct is built the first time anything reaches it, from
  the initialisers written on the declarations, so there is no window in which a worker can
  see it half-built.
- **`on_stop`.** Shutdown unpacks `__OrchProgram` into locals today. Shared bindings are not
  in that struct, so `on_stop` reads them through the same lock as everything else.
- **Library mode.** The host never sees shared state directly. If a host should read it,
  that is a `host` function or a typed tick return — not a new public field. Design
  philosophy §9 keeps the host in charge through a declared interface.
- **The existing `fn` diagnostic** (shipped in 0.10.1) stays exactly as it is: `fn` still
  cannot wait. This feature narrows what that error has to cover, it does not remove it.

## 5. The open questions, as answered

Each was decided as this document recommended:

1. **Which types may be shared?** Data: `int`, `float`, `bool`, `string`, arrays, options,
   and structs. A serverlet client, a `process`, or a closure is rejected by name — the
   first two are already safe to use concurrently and the third is not data.
2. **Is `shared` the right word?** Kept. It is accurate and greppable, and it is
   contextual, so a binding may still be called `shared`.
3. **A lock-free path when a program provably has no workers?** No. One behaviour.
4. **`parallel` and shared state.** A `parallel` block waits for its branches, so a
   statement inside one that touches shared state hits the "wait or share" rule and is
   refused, as any other waiting statement is.
5. **Read-only sharing.** Still out of scope, and the syntax leaves room for a separate
   marker later.

### One thing the build added

A `shared let` is **never captured** by a worker or an event handler. Those bodies clone the
free variables they close over; cloning a shared binding would copy the value and quietly
undo the sharing, so shared names are excluded from capture and reached through the lock
wherever they are used. `runtime_shared_state_is_not_copied_into_workers` covers it.

## 6. Build steps, as built

1. **Parse `shared let`** — a prefix on a top-level `let`, recorded on the AST. No behaviour
   change; a snapshot test proves the generated code is unchanged when the prefix is absent.
2. **Generate `__OrchShared` and the context field** — the struct, its initialisation during
   startup, and the `Arc<Mutex<…>>` on `OrchContext`. Still nothing reads it.
3. **Extend the name rewrite.** `StateRewrite` resolves a name to a local or a
   `__program` field today; it gains a third answer, a shared binding, and emits the guard.
4. **Statement-scoped guards**, with the "wait or share, not both" error.
5. **Reject the types decided in question 1**, with a message naming the binding.
6. **Measure**, replace the estimate in this document and in the changelog with the number.
7. **Docs** — `language-reference.md` §2.1 alongside `let`, the ladder in
   `design-philosophy.md` §4, the roadmap, and the website's boundary ladder.

## 7. Out of scope

Shared state across instances (each `start` is its own program and stays that way), shared
state visible to the Rust host as a field, lock-free atomics as a separate marker,
read-only sharing, and any change to how a plain `let` behaves.
