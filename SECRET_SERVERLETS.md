# Secret Serverlets — Design & Build Plan

> Status: **✅ SHIPPED (v1).** A secret serverlet runs **out of process**: its
> logic lives in a separate binary the orchestrator never contains, and the
> orchestrator talks to a thin **mirror** of it over IPC. Implemented across the
> lexer/parser (`secret` contextual modifier), codegen (mirror + standalone child
> program), and driver (child binaries written to `.orch_cache/src/bin/` and copied
> next to built binaries). Covered by snapshot + runtime tests. Next up: sandboxed
> serverlets (`SANDBOXED_SERVERLETS.md`).

---

## 0. The three serverlet types (taxonomy)

| Type | Where the body runs | Primary property | Status |
|---|---|---|---|
| **Serverlet** | In-process tokio actor | Speed, simplicity (you wrote it, you trust it) | ✅ Shipped |
| **Secret serverlet** | Separate OS process, talked to via a mirror | **Secrecy + isolation** — orchestrator never holds the code | ✅ Shipped (this doc) |
| **Sandboxed serverlet** | WASM guest (`wasmtime`) | **Containment** — hostile code genuinely can't touch the host | 📋 `SANDBOXED_SERVERLETS.md` |

They solve different problems and are **not** substitutes:

- *Secret* hides and decouples the implementation; it does **not** contain a
  hostile implementation (see §2).
- *Sandboxed* contains a hostile implementation; it does **not** hide the code.
- A future "both" mode (out-of-process **and** WASM-contained) is possible and
  noted in §8, but each type ships independently first.

---

## 1. The problem it solves

Orchestrate's distribution vision (`SERVERLET_FILES.md`) is: a software author
ships a prebuilt serverlet, and a third party interacts with it through a declared
contract **without seeing the internals.** A normal serverlet can't do that — it
compiles inline into the orchestrator's own binary, so its logic is *in* the
program that uses it.

A secret serverlet breaks that coupling. The body is compiled into its **own
program**; the orchestrator receives only a **mirror** — a stub that has the same
message interface but no logic. The mirror relays messages to the separate process
over IPC. Three concrete wins:

- **Secrecy** — when distributed as a compiled artifact, the consumer's
  orchestrator never contains the serverlet's source or logic. The author can ship
  the binary + an interface manifest and keep the implementation private.
- **Crash isolation** — if the serverlet segfaults or panics, it takes down its
  own process, not the orchestrator.
- **Decoupling / polyglot-by-protocol** — anything that speaks the mirror's IPC
  protocol can *be* a secret serverlet, regardless of language.

---

## 2. What it is and is NOT (be precise)

This naming has to be honest or it will mislead:

- **"Secret" means the code is not shared with / not linked into the consumer's
  orchestrator.** It does **not** mean cryptographically protected. A distributed
  binary can still be reverse-engineered or disassembled by a determined party.
  Secrecy here is "you don't ship source and the orchestrator never holds your
  logic," not DRM.
- **Out-of-process is NOT a security boundary.** The separate process runs as the
  **same OS user** with the **same ambient authority** as the orchestrator — it can
  read the same files, open the network, exhaust memory. A secret serverlet is the
  right tool for *your own or a trusted partner's* code that you want decoupled and
  private. It is the **wrong** tool for running *hostile* code — that is what the
  sandboxed serverlet exists for.

> One-line summary for user docs: *"A secret serverlet hides and isolates code you
> trust; it does not contain code you don't."*

### Honest note on when secrecy actually materializes

If you author a secret serverlet in your own source tree and compile the whole
thing yourself, *you* obviously still see the code — the secrecy benefit is a
**distribution** property. v1 ships the out-of-process **mechanism** (the real,
immediately-useful wins are crash isolation and decoupling). Full
secrecy-in-distribution — shipping a prebuilt secret serverlet binary + interface
manifest that a *different* person's orchestrator consumes — lands when the
serverlet-file format does (`SERVERLET_FILES.md`, syntax still open). The two
features are designed to meet there.

---

## 3. Architecture

The compiler emits **two artifacts** from one secret serverlet declaration:

1. **The serverlet program** — a standalone binary whose `main` runs the actor
   loop (reuse of the existing `start_X` / `XMsg` machinery), reading messages from
   IPC instead of an in-process channel, and writing replies back.
2. **The mirror** — a stub inside the orchestrator with the **same `XClient`
   surface** as a normal serverlet, so callers are unchanged. Instead of an inline
   tokio task, the mirror spawns/connects to the serverlet program and relays each
   call over IPC.

```
orchestrator (host)                         secret serverlet (separate process)
┌─────────────────────────┐                 ┌────────────────────────────────┐
│ caller: x.charge(100)   │                 │  actor loop                     │
│        │                │                 │   match msg { Charge {..} => …} │
│        ▼                │   IPC (pipe/    │        ▲          │            │
│   mirror XClient  ──────┼──  socket,  ────┼────────┘          ▼            │
│   (no logic, relays)  ◀─┼──  framed    ◀──┼──── reply                       │
│                         │   messages)     │                                 │
└─────────────────────────┘                 └────────────────────────────────┘
        the orchestrator binary never contains the serverlet's code
```

### Transport

Start with the **simplest portable option**: child process over `stdin`/`stdout`
with length-prefixed framed messages (JSON payloads to begin). It needs no ports,
no socket files, no cleanup, and works identically on macOS/Linux/Windows. A unix
socket / localhost TCP variant can come later if multiple orchestrators need to
share one serverlet, but that is explicitly out of scope for v1.

### Serialization

Each handler call marshals its args to a framed message; the reply marshals back.
Start with JSON (works for `int`/`float`/`bool`/`string` and, later, structs via
their derived `Debug`/serde). Keep the wire format an internal detail so it can be
swapped for something tighter later without changing user-facing syntax.

---

## 4. Lifecycle & the ping protocol

The orchestrator owns the serverlet process's lifetime:

- **Start:** when the mirror is first created (or first used), the orchestrator
  spawns the serverlet process.
- **Stop:** the orchestrator sends an explicit **shutdown ping**; the serverlet
  drains in-flight work and exits. (This matches your "turn-off ping instead of a
  stay-on ping" idea — no constant keep-alive needed for normal operation.)

### Liveness — the one correction to "turn-off only"

If the orchestrator *only* sends shutdown pings, it can't tell when the serverlet
dies **unexpectedly** (crash, hang, OOM-kill) — it would relay messages into a dead
process. The fix keeps your model but adds a cheap signal in the other direction:

- The orchestrator **watches the child PID / process handle** (no polling protocol
  needed — the OS tells you when a child exits), **or**
- the serverlet emits a lightweight heartbeat *to* the orchestrator.

Either way: **silence/exit = "it died," explicit ping = "turn off."** You get clean
shutdown *and* crash detection without a chatty are-you-awake loop.

---

## 5. What's genuinely hard (so we plan around it)

1. **Process lifecycle hygiene.** Spawned children must not become zombies or
   orphans if the orchestrator crashes. **Plan:** orchestrator reaps on shutdown;
   children exit if their stdin closes (parent died). Test the parent-dies path
   explicitly.
2. **Startup ordering / readiness.** The mirror must not relay before the child is
   up. **Plan:** a one-time readiness handshake (child sends "ready" first) before
   the mirror accepts calls.
3. **Serialization fidelity.** JSON is forgiving but lossy for some numeric edge
   cases. **Plan:** primitives first, lock the encoding, add structs later behind
   the same wire format.
4. **Backpressure across IPC.** The existing in-process channel caps at 100 and
   drops silently — over IPC, a slow child shouldn't silently lose messages. **Plan:**
   framed request/response with explicit await on the reply; surface a clear error
   if the child is gone, rather than dropping.
5. **Per-call vs persistent process.** A secret serverlet is *stateful*, so it must
   be **one long-lived process**, not spawned per message. **Plan:** spawn once on
   mirror creation; reuse for every call (this is what makes it a serverlet and not
   just `load_foreign`).

---

## 6. Syntax (IMPLEMENTED)

> Shipped as below. Kept deliberately parallel to the sandboxed serverlet's
> `sandbox(...)` modifier so the family stays coherent.

Authoring a secret serverlet (compiler splits it into process + mirror):

```orchestrate
serverlet PaymentProcessor secret {
    let balance = 0

    on charge(amount: int) -> bool {
        // runs in a separate process; orchestrator never contains this body
    }

    on get_balance() -> int {
        return balance
    }
}
```

- `secret` is a modifier on the serverlet declaration, sibling to `sandbox(...)`
  and the polyglot `via "<runtime>"`.
- From the caller's side, `payments.charge(100)` is **identical** to a normal
  serverlet — the mirror preserves the `XClient` interface.

Consuming a *prebuilt, third-party* secret serverlet (binary + interface manifest,
no source) depends on the serverlet-file format, which is **not finalized**
(`SERVERLET_FILES.md`). v1 targets the **authoring/mechanism** side; the consuming
syntax lands with serverlet-files.

---

## 7. Build steps — status

> Built on the shipped plain-serverlet codegen (`src/codegen/stmt.rs`:
> `XMsg` / `XClient` / `start_X`).

1. ✅ **Parse `secret` modifier.** `secret` is a contextual keyword (no reserved
   word added — won't break identifiers). AST: `secret: bool` on
   `StmtNode::Serverlet` (`src/ast.rs`). Parser: accepts `secret` after the name
   (`src/parser.rs`).

2. ✅ **Serverlet program as a second binary.** A `secret` serverlet is codegen'd
   into a standalone Rust program (`Codegen::compile_secret_program`) — owns the
   state, reads framed requests from `stdin`, dispatches by handler index, writes
   framed replies to `stdout`, with `print` routed to **stderr** (stdout is the IPC
   channel). The driver writes it to `.orch_cache/src/bin/secret_<name>.rs`, so
   cargo builds it as a second binary automatically.

3. ✅ **Mirror + spawn + relay.** `Codegen::compile_secret_mirror` emits a
   `start_X` that spawns the child once (`tokio::process`), performs the readiness
   handshake, and relays each `XClient` call over the child's stdio. The
   `XClient` / `XMsg` surface is byte-identical to a normal serverlet, so callers
   are unchanged.

4. ✅ **Lifecycle: shutdown + crash detection.** Stop = the orchestrator drops the
   child's stdin → the child reads EOF and exits (this also covers parent-death).
   Crash detection = on a read error/EOF from the child, the mirror logs
   `secret serverlet '<name>' exited unexpectedly` and returns rather than hanging.

5. ✅ **Marshaling.** Length-prefixed framing (dependency-free, handles arbitrary
   bytes including newlines). `int`, `float`, `bool`, `string` params/returns and
   `void` returns all round-trip. Unsupported types emit a clear `compile_error!`
   in both the mirror and the child.

6. ✅ **Docs.** Secret serverlet section added to `LANGUAGE_REFERENCE.md` with the
   §2 honesty statement.

---

## 8. Definition of done — met

- ✅ `serverlet X secret { on h(...) -> ... { } }` compiles into a separate process
  + an orchestrator-side mirror; callers use the unchanged `XClient` interface.
- ✅ The serverlet body is **not present** in the orchestrator binary (verified:
  handler string literals appear only in `secret_<name>`, not `orch_generated`).
- ✅ One long-lived child per secret serverlet; reused across calls; state persists
  between messages (runtime test: `add(5)→5`, `add(10)→15`, `add(2)→17`).
- ✅ Clean shutdown via EOF and crash detection (no hang on child death).
- ✅ `int`, `float`, `bool`, `string` params/returns and `void` returns over IPC.
- ✅ Snapshot test (`snapshot_secret_serverlet`) + runtime tests
  (`runtime_secret_serverlet_*`) cover the feature; honesty wording in docs.

### Known v1 limitations (documented, not bugs)

- **Primitives only across the wire.** Handlers using structs (or other non-primitive
  types) as params/returns produce a clear `compile_error!`. Structs-over-the-wire
  is future work.
- **Self-contained handlers.** A secret serverlet's handlers/state may use builtins
  (`print` → stderr, `to_string`, arithmetic) and primitives, but cannot call
  top-level functions or module functions from the parent program — the child is a
  separate binary and does not include them.
- **Unique names.** The child binary is `secret_<name>`; secret serverlet names must
  be unique across the program.
- **`print` goes to stderr** inside a secret serverlet (stdout is the IPC channel).

Out of scope for v1: structs over the wire, socket/TCP transport, consuming
prebuilt third-party secret serverlets (waits on serverlet-files), and the
combined **secret + sandboxed** mode (out-of-process *and* WASM-contained — a
natural future once both ship independently).
