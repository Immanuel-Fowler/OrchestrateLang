# Sandboxed Serverlets — Design & Build Plan

> Status: **design document — implementation not started.** Ships *after* the
> secret serverlet (`SECRET_SERVERLETS.md`) and *before* finalizing serverlet-file
> syntax (`SERVERLET_FILES.md`). Goal: run untrusted or semi-trusted serverlet
> logic with enforced memory and time limits, by wrapping audited sandbox
> technology (`wasmtime`) — not by building a sandbox ourselves.

---

## 0. The three serverlet types (taxonomy)

| Type | Where the body runs | Primary property | Status |
|---|---|---|---|
| **Serverlet** | In-process tokio actor | Speed, simplicity (you wrote it, you trust it) | ✅ Shipped |
| **Secret serverlet** | Separate OS process, talked to via a mirror | **Secrecy + isolation** — orchestrator never holds the code | ✅ Shipped (`SECRET_SERVERLETS.md`) |
| **Sandboxed serverlet** | WASM guest (`wasmtime`) | **Containment** — hostile code genuinely can't touch the host | 📋 This doc |

These solve different problems and are **not** substitutes. *Secret* hides and
decouples code you trust but does **not** contain a hostile implementation.
*Sandboxed* (this doc) contains a hostile implementation but does **not** hide the
code. A future combined mode (out-of-process **and** WASM-contained) is possible
once both ship independently.

---

## 1. The problem it solves

Orchestrate is meant to compose code from anywhere — local, downloaded, or
written by third parties (modders, plugin authors, OPM packages later). The
moment you run code you didn't write, you need a way to **contain** it: cap its
memory, cap its runtime, and deny it access to the host unless explicitly granted.

A sandboxed serverlet is a normal serverlet — same actor model, same
message-passing `XClient` interface — except its handler logic executes inside a
WASM guest with hard resource limits, and it can touch *nothing* on the host that
wasn't explicitly handed to it.

---

## 2. Core principle: wrap, don't build

**We do not invent a sandbox.** We wrap `wasmtime` (a mature, audited WASM
runtime). The security guarantee is therefore precise and honest:

> A sandboxed serverlet's compute and memory are isolated *as well as wasmtime
> isolates them* — no better, no worse. It is **not** a security boundary against
> anything we expose to the guest via host functions.

This honesty matters and must live in the user-facing docs:

- **Isolated by default:** CPU (via fuel/epoch interruption), memory (via a
  configured limit), and the absence of ambient host access (no files, no network,
  no clock unless granted).
- **NOT isolated:** anything you deliberately expose. Every host function you add
  is a hole you punched in the wall on purpose. Default-deny; grants are opt-in.

---

## 3. The hard architectural question (answered honestly)

An Orchestrate serverlet body compiles to **Rust**. So "run the handler in a
sandbox" really means: *get that Rust into a WASM guest and call it from the host.*
There is no interpreter to drop the code into — it's compiled.

The coherent path, and the one this plan commits to:

1. A sandboxed serverlet's handlers are codegen'd into a **separate Rust crate**
   that targets `wasm32-wasip1` (WASI preview 1 — the simplest stable target).
2. That crate is compiled to a `.wasm` module as part of `orchestrate build`.
3. The **host** program (the main orchestrator) embeds/loads that `.wasm` via
   `wasmtime`, sets memory + fuel limits, and the serverlet's actor loop dispatches
   each message by **calling an exported guest function**, marshaling args in and
   the reply out.

From the caller's side, **nothing changes**: `payments.charge(100)` still goes
through the generated `XClient`. The difference is entirely behind the actor loop —
the handler body runs in the guest instead of inline on the host.

```
host orchestrator (native Rust + tokio)
        │  msg: Charge { amount, reply_to }
        ▼
  sandboxed actor loop
        │  marshal args → guest memory
        ▼
  wasmtime instance (the serverlet's handlers, compiled to wasm32-wasip1)
        │  run with memory limit + fuel limit
        ▼
  marshal return value ← guest memory
        │
        ▼  reply_to.send(result)
```

### Why WASI preview 1 first

`wasm32-wasip1` is a stable, well-supported Rust target. It gives the guest a
*minimal* standard library (the parts that don't need host capabilities) without
ambient authority. We start here and explicitly do **not** wire up WASI's file or
clock access — the guest gets compute and memory only, until grants exist.

---

## 4. Proposed syntax (refined from PLANNED_FEATURES.md)

```orchestrate
serverlet UntrustedPlugin sandbox(memory_limit: "64mb", timeout: "5s") {
    on execute(input: string) -> string {
        // compiled to wasm32-wasip1, run inside a wasmtime instance
    }
}
```

- `sandbox(...)` is a modifier on the serverlet declaration, parallel to the
  `secret` modifier (`SECRET_SERVERLETS.md`) and the `via "<runtime>"` modifier
  proposed for polyglot serverlets. They are conceptually siblings: each says "this
  handler body is not plain inline host Rust" — `secret` = different process,
  `sandbox` = WASM guest, `via` = different language.
- `memory_limit` and `timeout` are config params passed straight through to the
  wasmtime store (memory limiter + fuel/epoch deadline).
- `runtime` is intentionally **omitted** — all sandboxed serverlets run over WASM
  initially. If a non-WASM isolation backend (e.g. Firecracker microVM) is ever
  added, `runtime` becomes an optional param defaulting to `"wasm"`, without
  breaking existing sandboxed serverlets.

### Relationship to grants (the consent model)

A sandboxed serverlet that needs *any* host capability declares it explicitly —
the same `grant` mechanism described in `SERVERLET_FILES.md`:

```orchestrate
serverlet UntrustedPlugin sandbox(memory_limit: "64mb", timeout: "5s") {
    grant read "./config.json"   // exposed to the guest as a host function; nothing else
    on execute(input: string) -> string { ... }
}
```

Each grant becomes a **single, narrow host function** wired into the wasmtime
linker. No grant → that capability simply does not exist inside the guest. This is
where sandboxing and the consent boundary become the *same mechanism* viewed from
two sides: the grant declares intent; the wasmtime linker enforces it.

---

## 5. What's genuinely hard (so we plan around it)

1. **Marshaling across the WASM boundary.** WASM core types are just i32/i64/f32/
   f64. Anything richer (strings, structs) must be copied through linear memory
   with an agreed layout. **Plan:** ship primitives first (`int`, `float`, `bool`),
   then `string` (ptr+len into guest memory), then structs (serialize — likely
   JSON or a simple length-prefixed encoding) much later.

2. **Timeout enforcement.** A guest can loop forever. wasmtime offers *fuel*
   (deterministic instruction budget) and *epoch interruption* (wall-clock-ish).
   **Plan:** use epoch interruption for `timeout`, backed by a background timer
   thread bumping the epoch — closer to "5 seconds of wall time" than fuel, which
   is instruction-count and harder to map to seconds.

3. **Build complexity.** A sandboxed serverlet requires a *second* compile pass to
   `wasm32-wasip1`, the target installed (`rustup target add wasm32-wasip1`), and
   the resulting `.wasm` embedded into the host build. **Plan:** the driver detects
   sandboxed serverlets, emits the guest crate into `.orch_cache/`, runs the extra
   `cargo build --target wasm32-wasip1`, and `include_bytes!`s the result into the
   host. Detect a missing target early and print an actionable error.

4. **New heavy dependency in *generated* programs.** The compiler itself stays
   dependency-free, but any program using a sandboxed serverlet now pulls
   `wasmtime` into its generated `Cargo.toml` (a large crate). **Plan:** only add
   the `wasmtime` dependency when at least one sandboxed serverlet is present —
   programs that don't use the feature stay lean.

5. **Host functions are the real attack surface.** Every grant is a hole. **Plan:**
   default-deny, grants opt-in and narrow (one host fn per grant), and the
   user-facing docs state plainly that the sandbox protects against the guest's
   *own* compute/memory misbehavior, not against whatever a grant exposes.

---

## 6. Build steps (ordered, each independently shippable)

> Prerequisite already met: plain serverlets ship today, so the actor loop /
> `XMsg` / `XClient` codegen this builds on already exists in
> `src/codegen/stmt.rs`.

1. **Parse `sandbox(...)` — syntax only, no behavior change.**
   Lexer: `sandbox` contextual keyword. AST: add
   `sandbox: Option<SandboxConfig>` to `StmtNode::Serverlet` where
   `SandboxConfig { memory_limit: String, timeout: String }`. Parser: accept
   `sandbox(memory_limit: "..", timeout: "..")` after the serverlet name.
   Typechecker: validate the config keys/values are string literals. Codegen:
   ignore the field for now — a sandboxed serverlet still compiles as a normal
   serverlet. *Lands the surface syntax; nothing runs in wasm yet. Add a snapshot
   test proving the AST/codegen are unchanged in behavior.*

2. **Guest crate codegen (no host integration yet).**
   For a sandboxed serverlet, emit a standalone Rust crate under
   `.orch_cache/sandbox_<name>/` whose lib exports one function per handler,
   bodies = the existing handler codegen, targeting `wasm32-wasip1`. Drive
   `cargo build --target wasm32-wasip1` from the driver. *Verifiable in isolation:
   assert the `.wasm` artifact is produced. Primitives only.*

3. **Host-side wasmtime wiring for primitive handlers.**
   Add `wasmtime` to the generated `Cargo.toml` (only when a sandboxed serverlet
   exists). In the actor loop, replace the inline handler call with: instantiate
   the embedded `.wasm`, call the exported guest fn with the message args, return
   the result over `reply_to`. *First end-to-end: a sandboxed serverlet with an
   `int -> int` handler runs in wasm and returns the right value. Runtime test.*

4. **Memory limit enforcement.**
   Wire `memory_limit` into the wasmtime `Store` via a `ResourceLimiter`. *Test: a
   guest that tries to allocate past the limit traps cleanly and the host surfaces
   a structured error to the caller rather than crashing.*

5. **Timeout enforcement (epoch interruption).**
   Wire `timeout` to epoch deadlines + a background epoch-bumping timer. *Test: a
   guest that loops forever is interrupted at ~timeout and the caller gets a
   timeout error.*

6. **`string` marshaling across the boundary.**
   ptr+len protocol into guest linear memory. *Test: `string -> string` handler
   round-trips correctly.*

7. **Grants as narrow host functions.**
   `grant read/write "<path>"` → exactly one mediated host function per grant in
   the wasmtime linker; ungranted capabilities are absent from the guest. This is
   the shared mechanism with `SERVERLET_FILES.md` grant enforcement. *Test: a guest
   can read a granted path and CANNOT read a non-granted one (the host fn doesn't
   exist for it).*

8. **Docs.** Update `LANGUAGE_REFERENCE.md` with the sandboxed serverlet section
   and the precise, honest security statement from §2. Flip the
   `PLANNED_FEATURES.md` entry to **[SHIPPED]**.

---

## 7. Definition of done (for the first shippable version)

- `serverlet X sandbox(memory_limit, timeout) { on h(...) -> ... { } }` compiles
  and runs, handler logic executing in a wasmtime guest.
- Memory limit and timeout are enforced and surface clean errors, not crashes.
- `int`, `float`, `bool`, and `string` handler params/returns work.
- No host access from the guest unless a `grant` exists; each grant is one narrow
  host function.
- Programs without sandboxed serverlets do **not** gain the `wasmtime` dependency.
- Snapshot + runtime tests cover steps 1–7; security wording landed in docs.

Structs across the boundary, OPM integration, and non-WASM isolation backends are
explicitly **out of scope** for the first version.
