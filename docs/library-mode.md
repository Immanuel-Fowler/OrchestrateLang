# Library mode and host integration

`orchestrate build --lib main.orch -o generated/scripts` generates and checks a Rust
crate named `scripts`. Add it as a Cargo path dependency in a Rust application.
The host owns its Tokio runtime; the library never creates a runtime or exits the
host process. Python pipe landlines require Python 3.10+ (`ORCH_PYTHON` selects the
executable, otherwise `python3`).

Run the complete example from the repository root:

```sh
cargo run --bin orchestrate -- build --lib examples/library_host/scripts.orch -o examples/library_host/generated/scripts
cargo run --manifest-path examples/library_host/Cargo.toml
```

## Host lifecycle

```rust
let mut scripts = scripts::start(runtime.handle(), AppHost::new())?;
scripts.ready().await?;
while !scripts.stop_requested() {
    scripts.tick(dt).await?;
}
scripts.shutdown().await?;
```

The generated `start(&tokio::runtime::Handle, impl Host)` returns
`Result<Scripts, String>`. It initializes an independent instance and schedules its
coordinator on the supplied runtime. Use `()` as the host when there are no host
functions. Current-thread and multithreaded Tokio runtimes both work; keep the
runtime running while awaiting library operations.

- `ready().await` waits for top-level bindings, `on_start`, and event registration.
  It does not wait for the orchestrator body or every sidecar handshake to finish.
- `tick(dt).await` executes the `on_tick` hooks once, in declaration order. The
  method requires mutable access, so ticks from one handle are serialized. Its
  result acknowledges hook completion; it is not a batch of foreign return values.
- `stop_orch()` sets this instance's stop-request flag. It does not exit Rust,
  automatically shut down the library, or affect other instances. A subsequent
  tick returns an error; the host should call `shutdown()`.
- `shutdown().await` cancels a pending tick, stops the orchestrator task, runs
  `on_stop`, aborts owned workers, and closes sidecars. Idle pipe children receive
  BYE; children with unfinished calls may be terminated. Cleanup allows three
  seconds for sidecar tasks before aborting them. It leaves the host runtime alive.
- Dropping `Scripts` requests shutdown in the background. Await `shutdown()` for
  deterministic cleanup and error reporting, before dropping the Tokio runtime.

Lifecycle hooks and host declarations belong in the entry file. Top-level bindings
are shared by its lifecycle hooks. The `main` orchestrator accepts no parameters or
one `process[]` parameter for worker supervision. Event registries, owned tasks,
assets, host implementations, and stop flags are isolated per instance.

The host API is asynchronous to avoid blocking or nesting a Tokio runtime.
Synchronous hosts can call `runtime.block_on(...)` from outside async code.
Lifecycle code and synchronous host implementations must cooperate with the
runtime: CPU-bound loops, blocking host methods, or a stuck `on_start`/`on_stop`
hook can delay shutdown. A landline `budget` bounds slow Python calls, but not Rust
host methods or the hooks themselves.

## Declaring and granting host functions

```orchestrate
host world {
    fn record(value: int) -> int
    fn reset()
}
serverlet Counter via python(source: "counter.py") {
    grant call world.record
    on tick() -> int
}
```

The library exposes one `Host` trait method per declaration:

```rust
impl scripts::Host for AppHost {
    fn world_record(&self, value: i64) -> Result<i64, String> {
        Ok(value)
    }
    fn world_reset(&self) -> Result<(), String> {
        Ok(())
    }
}
```

`Host` is `Send + Sync + 'static`; methods use `&self`. Use interior mutability for
state. Methods execute synchronously on the runtime thread; keep them short and
avoid waiting on a call into the same serverlet. Host errors and panics during a
Python callback become failed HOST_REPLY messages and raise Python `RuntimeError`.
A panic still invokes Rust's panic hook; abort-on-panic builds cannot recover it.

Python calls `self.host.world.record(value)` from a handler. Only explicitly granted
functions appear on that proxy. Rust checks every received function ID against the
serverlet's grants, so forging an ID cannot invoke an ungranted method. Grants may
appear on landlines in imported modules, but refer to the entry file's host groups.
Calls in constructors or outside an active handler are rejected.

Host functions use the existing binary wire types: primitives, arrays, and
same-file structs represented by Python dataclasses. Matching dataclass types must
be defined/imported in the Python implementation. Their field order must match the
OrchestrateLang struct. Host names must not begin with `_` or produce colliding
`group_function` Rust method names.

OrchestrateLang code can also call `world.record(...)` directly. A returned host
error is logged and produces a default value. Python exceptions retain the normal
landline behavior: log the error, return a default value, and preserve process state.

Grants constrain the generated host-call interface. They do not sandbox Python or
restrict its OS permissions.

## Packaging

The output directory is compiler-owned and marked `.orchestrate-library`.
Generation refuses to overwrite an existing unmarked directory. Regenerate from
`.orch` sources rather than editing generated files.

Python sources/SDK files and compiled secret children are embedded as library
assets. Each instance extracts its own temporary directory, then removes it on
shutdown. The linked host binary does not depend on the original `.orch`/Python
source locations. Python itself and third-party packages remain external. Secret
children are compiled for the compiler machine's target; cross-compiling that
bundle is not supported. Existing foreign C/C++ build scripts may still reference
source files and require their toolchain when the host builds the generated crate.

Bound slow landline calls made from `on_tick` with a landline `budget` and `late` policy,
and batch per-item work by passing arrays; see
[landline-serverlets.md](design/landline-serverlets.md) §5. A latency benchmark harness
is not implemented yet.
