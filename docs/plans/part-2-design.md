# Part 2 design notes

Four features chosen from Part 2 of the gap brief. This file records what was decided and
where each stands; the language reference is where the built ones are documented.

| Feature | Syntax chosen | Status |
|---|---|---|
| Cadence (Priority 5) | A: `on_tick(dt: float) every "100ms"` / `every 5 ticks`; C: `every: "100ms"` in a landline's or sandbox's configuration, read with `latest(p.tick)`; D: `automatic(every: "20ms")` | built (commit "cadences") |
| Client as a value (A3) | `S.client`, module `alias.S.client` | built |
| Map (B3) | `map<K, V>`, literal `map { "a": 1 }`, `map {}` | built |
| Instances by key (A1) | falls out of the two above: `map<string, Tenant.client>` | built |
| Instrumentation (B4 + B5a) | M2: `measure serverlets, hooks, workers`, read with `stats()`; E1: `unmeasured` on the thing left out | built |
| Shared queue (B1) | Q2: `shared queue jobs: Job (capacity: 256, full: "reject")` | built |

---

## Cadence

- Time is host time, the sum of the `dt`s the host passes, so a cadence replays exactly.
  A cadenced hook runs on the first tick, then once per period on a schedule fixed to that
  run; a tick that finds several periods gone runs it once.
- C is limited to landline and sandbox declarations, the ones with a configuration list.
  An in-process or secret serverlet is called from a cadenced hook instead.
- C calls each instance's `tick` before the tick's hooks, under its own budget. `latest(p.tick)`
  is `ok` with the last answer, or `err` saying why there is none, so a skipped or failed
  call is never a stale value in disguise.
- D is wall time: a worker is not driven by a host.

## Clients and maps

- A client is an address: copies reach one instance. It crosses no process or sandbox
  boundary, cannot be printed or compared, and is returned by a handler only as
  `result<S.client>`. A `shared let` may hold clients: a statement copies one out under the
  lock, and the call goes in a statement of its own.
- A map is ordered by key (a B-tree) so iteration replays. `m[k]` on a missing key stops the
  program naming the key; `get` asks first. It crosses to secret serverlets, Python (`dict`)
  and TypeScript (`Map`), not into a sandbox.

## Instrumentation

- Declared in the source, so a program that does not declare it pays nothing: codegen emits
  the timing only for what is measured.
- Per unit — a serverlet handler, a tick hook, an event handler, a worker: calls, failures
  (2.2's), duration, and start delay (sent-to-started for a handler, triggered-to-started for
  an event, due-to-started for a paced worker), with p50/p90/p99/max from a fixed histogram.
  CPU time for in-process code only; a child process's CPU time is its own.
- A library host reads `Scripts::stats()`; a standalone program prints `stats()`.
- `unmeasured` goes where the thing is declared: on a serverlet (every handler), on one
  handler, on a hook or an event handler, on a worker.

## Shared queue

- `push` returns `false` when something was lost — the new item under `full: "reject"`, the
  oldest under `"drop_oldest"` — so a dropped item is always stated. `pop`, `drain`,
  `length` are each one lock. `take` waits for an item, and is refused where waiting is.
- In a deterministic library run, a `take` on an empty queue gives up its turn and is served
  in order by a later handler's `push`, as a deterministic `sleep` waits on host time.
