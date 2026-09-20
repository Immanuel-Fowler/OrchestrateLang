# Shared state — declared, not implied

> Status: **proposed, not built.** The philosophy is settled; the semantics below are the
> proposal, and the open questions at the end need answers before anyone writes code. The
> costs marked *(estimate)* have not been measured on this machine yet, and per
> [design philosophy](../design-philosophy.md) §10 nothing here should be promised until
> they are.

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
a trade-off, it is a missing rung. `shared let` is that rung, at an estimated ~15–20 ns —
roughly 400× cheaper than a serverlet and still exact.

## 2. The design

```orchestrate
let counter = 0          // unchanged: instance-owned, hooks only, free
shared let hits = 0      // opt-in: any task, any fn, synchronised
```

`shared` is a prefix on a top-level `let`. It does not change what the binding *is*; it
changes where the backend puts it:

- A plain `let` that a hook reaches stays a field of `__OrchProgram`, exactly as now.
- A `shared let` becomes a field of a separate `__OrchShared` struct, held once in
  `Arc<Mutex<__OrchShared>>` on the context and reachable from anywhere in the program.

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

Because the guard spans the statement, reads **borrow** rather than clone. Reading a
`shared` string or array does not copy it.

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
| `shared let`, uncontended | ~15–20 ns | **no — estimate** |
| a serverlet call | ~7.5 µs | yes |

A program with no `shared` bindings pays nothing: no mutex, no `Arc`, no field, no code.
This is the same rule as `wasmtime` for programs without a sandbox and `.NET` for programs
without C#.

Per design philosophy §10, the estimate must become a measurement before the feature ships,
and the changelog should quote the measurement, not the estimate.

## 4. How it interacts with what exists

- **Determinism.** Deterministic mode already asserts that no workers or serverlets are
  spawned, so in that mode nothing runs concurrently with a tick and shared state cannot be
  raced. Replays stay reproducible. Shared state is *not* a way around that assert.
- **Startup order.** Top-level `let`s run during startup, before workers are spawned and
  before `ready` returns, so a `shared` binding is initialised before anything can reach it.
- **`on_stop`.** Shutdown unpacks `__OrchProgram` into locals today. Shared bindings are not
  in that struct, so `on_stop` reads them through the same lock as everything else.
- **Library mode.** The host never sees shared state directly. If a host should read it,
  that is a `host` function or a typed tick return — not a new public field. Design
  philosophy §9 keeps the host in charge through a declared interface.
- **The existing `fn` diagnostic** (shipped in 0.10.1) stays exactly as it is: `fn` still
  cannot wait. This feature narrows what that error has to cover, it does not remove it.

## 5. Open questions

These need answers before implementation, and each one changes the code:

1. **Which types may be shared?** Scalars, `string`, arrays, and structs are straightforward.
   A serverlet client is already cheap to clone and its calls are async, so sharing one is
   probably pointless — reject it, or allow it and say nothing? A closure or a `process`
   reference is almost certainly a mistake.
2. **Is `shared` the right word?** It is accurate and greppable. `atomic` would overpromise
   (this is a mutex, not an atomic). `global` would describe scope rather than the capability.
3. **Should a `shared` binding be readable without the lock when the program provably has no
   workers?** It could be, but the analysis is whole-program and fragile, and the win is
   ~15 ns. Recommended: no. Keep one behaviour.
4. **Does a statement that touches shared state inside a `parallel` block make sense?** The
   branches run concurrently; each branch's statements would serialise on the one lock. It
   works, but it deserves a sentence in the reference so nobody expects parallel speedup.
5. **Read-only sharing.** Is there a case for a binding that is written once at startup and
   only read afterwards? That needs no lock at all and could be a separate marker later.
   Out of scope here; note it so the syntax leaves room.

## 6. Build steps

Each is independently shippable and testable.

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
