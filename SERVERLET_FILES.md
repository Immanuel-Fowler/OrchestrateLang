# Serverlet Files — Design & Roadmap

> Status: **design ~90% done — not yet implemented.** The *model* is settled
> (consent boundary, manifest-as-extended-`module.orch`, two polyglot paths, grant
> enforcement as the load-bearing wall). What is **not** finalized: the concrete
> **syntax of the serverlet file itself** — the `serverlet ... via ... { grant ... }`
> shape in §3 is a sketch, not a decision. Lock that syntax last, *after*
> sandboxed serverlets ship (see `SANDBOXED_SERVERLETS.md`), so the file format can
> account for `sandbox(...)` from the start instead of being retrofitted.
>
> This doc captures the agreed-on model, the concrete steps to build it, and *why*
> the recent language changes (struct support, FFI sidecars, fail-slow errors,
> codegen tests) were prerequisites rather than unrelated cleanup.

---

## 1. The vision in one paragraph

Orchestrate exists to build background apps and services that coordinate scripts
running in **separate runtimes** with a deliberate polyglot approach — without
Docker images or bespoke connectors. The end state: a software author ships an
app, and a third party (modder, solo dev, hacker) drops a **prebuilt serverlet**
into a known location and interacts with it through a stable, declared contract.
The serverlet is the unit you distribute and compose; the modder never has to
understand the host app's internals to extend it.

---

## 1.5 Serverlets are not the only way to go polyglot

Important framing so this document isn't misread: **a module can interface with
other languages without being a serverlet at all.** There are two distinct
polyglot paths, and they serve different needs (this mirrors
`PLANNED_FEATURES.md` 1a vs 1b):

| | `load_foreign` (shipped) | Serverlet |
|---|---|---|
| **Style** | Stateless, direct function call | Stateful, message-passing actor |
| **You get** | Call a function, get a value back | A living service you talk to |
| **State** | None — pure call across the boundary | Owns and persists internal state |
| **Lifetime** | Per-call | Long-running, supervised |
| **Use when** | You just need a Rust/C/C++ function | You need a connection, not a call |

`load_foreign "rust"/"c"/"cpp"` is the right tool when you simply want to reach
into another language for a computation. It does **not** require a serverlet, an
actor, or message passing — and it never should.

A **serverlet is for a special type of connection**: a stateful, long-lived,
supervised actor you communicate with over time — a little bit of modern flair
for the API age, where the unit of integration is increasingly "a service you
talk to" rather than "a function you call." Everything below about distribution,
manifests, and consent boundaries is about *that* special connection. The
stateless `load_foreign` path stands on its own and is not subsumed by it.

---

## 2. The core design decision (and why it's a decision, not a limitation)

A serverlet lives inside a **module directory** — a folder containing a
`module.orch` file. It is *not* imported as a loose single file. This was
challenged during design as an awkward limitation. It is not. The reasoning:

### `module.orch` is a consent token, not a code-organization artifact

The instinct is to read `module.orch` as "the entry file" and complain that the
name carries no information (every module has one, named identically). That is
the wrong category. `module.orch` is an **expressed-consent boundary**: its
presence is the directory's grant that the orchestrator may read, write, and
spawn the resources that directory references.

Under that reading, the *uniformity is the point*:

- You do **not** want capability boundaries to be creatively named. You want them
  unambiguous and greppable — like `.git` marking a repo boundary or a mount
  point marking a filesystem boundary.
- A modder who sees `module.orch` in a folder knows *instantly, without reading
  it*, that this folder is a consent surface the orchestrator is allowed to touch.
- The legible, unique contract is the **path** (aliased by PROM, see below). The
  `module.orch` file is the *capability* layer, not the *naming* layer. They sit
  at different levels and don't compete.

### Why serverlets and modules belong together

A serverlet is a runtime-bearing, supervised, message-passing actor — the thing
that will actually read files, write state, and spawn foreign runtimes on someone
else's machine. An actor with no declared capability boundary is exactly what a
governance model exists to prevent. So pairing serverlet ⇄ module isn't
convenience: **there is no safe distributable serverlet without a consent
boundary, and the directory is that boundary.**

### PROM closes the naming gap

`module.orch` deliberately says nothing unique. The OS already provides a unique
contract — the absolute path — and PROM (`orchestrate prom add <name> <path>`)
aliases that path to a name. So:

- **Intra-app**: `./`-relative paths keep an app's own serverlets portable when
  the repo is cloned.
- **Cross-app**: PROM aliases a machine-local path to a short name for reuse.

Two scopes, two mechanisms, no contradiction.

---

## 3. What a serverlet file looks like (proposed — SYNTAX NOT FINAL)

> ⚠️ **The syntax below is a sketch, not a decision.** The *principle* (one
> artifact carrying public surface + runtime + grants) is settled; the exact
> keywords and layout are deliberately left open until sandboxed serverlets ship,
> so `sandbox(...)` can be designed into the file format rather than bolted on.

The serverlet's public surface, runtime, and consent grants are declared in **one
artifact** — an *extended* `module.orch` — rather than fragmented across a
separate "interface file." Fragmenting them would split the consent boundary,
which defeats the purpose.

```orchestrate
// payments/module.orch
//
// Three things, one declaration:
//   1. public surface  — what callers may invoke
//   2. runtime         — what this serverlet runs in (polyglot dispatch)
//   3. consent grants  — what the orchestrator is permitted to touch

serverlet PaymentProcessor via "rust" {
    // --- consent grants: the enforced capability boundary ---
    grant read  "./rates.json"
    grant write "./ledger.db"

    // --- state (private to the actor) ---
    let balance = 0

    // --- public handlers: the contract third parties depend on ---
    on charge(amount: int) -> bool {
        // ...
    }

    on get_balance() -> int {
        return balance
    }
}
```

A modder drops `payments/` into the host app, runs
`orchestrate prom add payments ./payments` (or references it by relative path),
and calls `payments.charge(100)` from the orchestrator. They never read the
implementation — only the declared handlers.

---

## 4. Why the recent language changes were prerequisites

These four changes were not unrelated cleanup. Each removes a specific blocker on
the serverlet-file model. Listed most-essential first, and honest about how
direct the connection is.

### 4.1 Struct support — **directly required**

A distributable serverlet exchanges *data*, not just scalars. A `charge(amount)`
that can only take an `int` is a toy; a real handler takes a `Payment { amount,
currency, account }`. Before structs, serverlet state and message payloads could
only hold primitives — meaning the message enum and `*Client` interface a third
party depends on couldn't carry structured data. Structs are the type-system
foundation that lets a serverlet's *contract* be expressive enough to be worth
distributing.

> Codegen note: the generated `XMsg` enum and `XClient` methods
> (`src/codegen/stmt.rs`) already map handler params to Rust types via
> `compile_type`. Adding `Type::Named` means struct payloads now flow through that
> machinery — but this path needs explicit tests (see 4.4) before it's trustworthy
> as a public interface.

### 4.2 Rust FFI sidecar (`.orch_ffi`) — **the architectural template for the manifest**

The serverlet manifest (section 3) — "a declaration of an interface contract,
separate from the implementation" — is *exactly* the pattern the FFI sidecar
already establishes. Replacing the old brittle line-scanner with a real sidecar
parser (`src/ffi_rust.rs` → `register_rust_ffi_from_sidecar`) was the first
working instance of "Orchestrate reads a declared signature contract and registers
it without parsing the implementation." The serverlet manifest generalizes this:
the `via "rust"` / `via "python"` runtime tag plus the public-handler list is the
same idea — a declared contract the compiler trusts, with the body implemented by
something else. Building the sidecar correctly *first* means the manifest isn't
inventing a new mechanism; it's extending a proven one.

> Note the two stay distinct: `load_foreign` remains the *stateless* polyglot path
> (call a function, get a value). The serverlet's `via "<runtime>"` is the
> *stateful* path (talk to a living actor whose body runs elsewhere). The sidecar
> is the shared *template* for "declared contract, foreign implementation" — not a
> sign that one absorbs the other.

### 4.3 Fail-slow error collection — **required for third-party usability**

When the serverlet author and the person debugging are the *same* person,
fail-on-first-error is merely annoying. When a modder drops in a serverlet that
doesn't compile against *their* app, the author isn't there to iterate. Reporting
*all* parse/type errors at once (parser and typechecker now accumulate into a
`Vec<String>` and report together) is what makes a broken third-party serverlet
diagnosable in one pass instead of a frustrating fix-recompile-repeat loop the
modder can't shortcut. An ecosystem with third-party authors needs errors that
explain the whole problem, not just the first symptom.

### 4.4 Codegen snapshot + runtime tests — **required because the interface is now a public contract**

The moment a serverlet's generated `XMsg` / `XClient` is something *other people
depend on*, its codegen output is a public API surface — and silent changes to it
break downstream callers. Snapshot tests (`tests/codegen_snapshot_tests.rs`) lock
the generated Rust so an accidental codegen change is caught as a test failure,
not discovered by a modder whose serverlet stopped linking. Runtime tests
(`tests/runtime_tests.rs`) prove the actor actually works end-to-end. Before
serverlet files, codegen drift only hurt you; after, it hurts everyone who built
on your contract.

### 4.5 (Minor) `--help` / CLI polish — **ecosystem legibility**

Honest framing: this one is tangential, not a hard prerequisite. But a tool
intended for modders and solo devs needs a real CLI surface — distinct
`--help` output, clear command errors — because the audience is no longer just
the language author. It's table stakes for a tool other people are expected to
pick up cold.

---

## 5. The load-bearing open problem: enforcement

The `grant read/write` syntax in section 3 is **decoration until something
enforces it.** Today the driver reads and writes files freely
(`src/driver.rs`). The moment `module.orch` *means* "only these paths are
grantable," the runtime must actually refuse access outside the grant — otherwise
the contract is honored only by well-behaved code, which is precisely the code you
didn't need to constrain.

This is not a flaw in the design; it is the **load-bearing wall** of it. Design
implication: build the `grant` *syntax* with enforcement in mind from day one,
even if the enforcement layer ships later, so the declaration never promises
something the runtime doesn't keep. This is also the natural bridge to the
`axiom.orch` governance model in `PLANNED_FEATURES.md` — both are "the
orchestrator checks a declared policy before performing an action," one for file
capabilities, one for LLM tool calls.

---

## 6. Tangible build steps (ordered)

Each step is independently shippable and testable. Do not start a step before the
previous one is green.

1. **Struct payloads in serverlet handlers — verify & test.**
   Structs exist; confirm they flow through `XMsg`/`XClient` codegen. Add a
   snapshot test for a serverlet with a struct param and a runtime test that
   sends a struct to a handler and asserts the reply. *Closes the gap flagged in
   4.1.*

2. **Runtime tag parsing: `serverlet X via "rust" { ... }`.**
   Lexer: no new token needed (`via` can be a contextual keyword or reuse
   `Identifier`). AST: add an optional `runtime: Option<String>` to
   `StmtNode::Serverlet`. Parser: accept `via "<str>"` after the name. Codegen:
   for `via "rust"` (or absent), behavior is unchanged. Validate end-to-end with
   an existing serverlet plus the tag. *No behavior change yet — this is the hook
   the polyglot dispatch hangs on.*

3. **Single-directory serverlet loading polish.**
   Confirm `use module x: "./payments"` resolves a `module.orch` containing only a
   serverlet, and that the generated `start_X()` / `XClient` are reachable from the
   host orchestrator. Add an integration test that imports a serverlet-only module
   and calls a handler. *Proves the distribution unit works before adding grants.*

4. **`grant read/write "<path>"` — syntax only, parsed and stored.**
   Lexer: `grant` contextual keyword. AST: `grants: Vec<Grant>` on the serverlet.
   Parser + typechecker: parse and validate paths are string literals. Codegen:
   emit the grants as metadata/comments for now (no enforcement). *Lands the
   declaration so the contract is expressible; enforcement is step 6.*

5. **Polyglot dispatch for one non-native runtime (Python via subprocess+JSON).**
   This is `PLANNED_FEATURES.md` item 4 and the first real test of `via`. For
   `via "python"`, codegen emits subprocess spawn + stdin/stdout JSON marshaling
   instead of an inline Rust handler body. Lowest-risk polyglot path (no embedded
   interpreter, no FFI). *First proof that a **stateful** serverlet body can run in
   a separate runtime — distinct from the already-shipped stateless `load_foreign`
   path, and the heart of the actor-style side of the vision.*

6. **Grant enforcement.**
   Make `grant` mean something: the runtime mediates file access against the
   declared grants and refuses out-of-grant reads/writes. This is the load-bearing
   wall (section 5) and the bridge to `axiom.orch`. Start with default-deny +
   explicit allowlist; no conditional policy yet.

7. **(Later) `.srvlt` extension & the hot-reload pipedream.**
   Only after 1–6. Per `PLANNED_FEATURES.md`, true hot-swap of a native-compiled
   serverlet requires dynamic linking (`dlopen` a per-serverlet `.so`/`.dll`) and
   message-enum stability constraints — a substantial architectural shift, kept as
   a pipedream until the rest is solid.

---

## 7. Summary

- The directory + `module.orch` requirement is a **deliberate consent boundary**,
  not a limitation. Its uniformity is what makes it legible as a capability marker.
- The unique, legible contract is the **path** (aliased by PROM); `module.orch` is
  the **capability** layer. Different levels, no conflict.
- The serverlet manifest is an **extension of `module.orch`**, not a competing
  file — keeping the consent boundary whole.
- Recent language work was **prerequisite, not cleanup**: structs make the
  contract expressive, the FFI sidecar is the manifest's template, fail-slow
  errors make third-party serverlets diagnosable, and codegen tests protect the
  now-public interface.
- **Enforcement of grants is the load-bearing wall** — design the syntax for it
  now, even if it ships later.
