# Orchestrate

> **A compiled, asynchronous orchestration language that transpiles to native Rust.**

Orchestrate (`.orch`) is a purpose-built programming language for writing **concurrent system coordinators** — programs that manage multiple background workers, respond to events, and communicate with external processes, all in a clean and readable syntax. Orchestrate scripts compile directly to Rust source code and are executed via the Tokio async runtime, producing **native machine-speed binaries** with no interpreter overhead.

---

## Getting Started (For Beginners)

If you're new to programming or command-line tools, don't worry! Follow these steps to get Orchestrate running on your computer.

### Step 1: Install Rust (The Engine)
Orchestrate runs on top of a language called Rust. You need to install Rust first.
- **Windows:** Download and run the [Rust Installer for Windows (rustup-init.exe)](https://win.rustup.rs/). 
  > *If it asks to install Visual Studio Build Tools, say yes, and make sure "Desktop development with C++" is checked.*
- **Mac / Linux:** Open your "Terminal" app, paste the following command, and press Enter:
  ```bash
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```

*Note: After installing Rust, you must close and reopen your Terminal/Command Prompt for the changes to take effect.*

### Step 2: Download Orchestrate
Next, you'll download the Orchestrate code. In your Terminal or Command Prompt, run:
```bash
git clone https://github.com/YOUR_USERNAME/OrchestrateLang
cd OrchestrateLang
```
*(If you get an error that `git` is not recognized, you'll need to [install Git](https://git-scm.com/downloads) first).*

### Step 3: Install the Compiler
Now, tell Rust to build and install the Orchestrate tool. Run this command:
```bash
cargo install --path .
```
This might take a few minutes. Once it finishes, you'll have a new command available called `orchestrate`.

To check if it worked, type:
```bash
orchestrate --help
```

### Step 4: Run Your First Program
We've included several examples. To run the "Hello World" example, type:
```bash
orchestrate run examples/hello.orch
```
*Note: The very first time you run a program, it might take 30-60 seconds to set things up. After that, it will be lightning fast!*

### Try the Other Examples

The `examples/` folder contains more ready-to-run programs:
```bash
orchestrate run examples/pipeline.orch                # Pipeline operator demo
orchestrate run examples/multi_process.orch           # Multiple concurrent workers
orchestrate run examples/async_parallel.orch          # Parallel task execution
orchestrate run examples/test_serverlet.orch          # Serverlet actor model
orchestrate run examples/persistent_serverlet_test.orch  # Stateful actor across iterations
orchestrate run examples/task_demo.orch               # Full demo with modules and events
orchestrate run examples/test_parallel_capture_combo.orch # Parallel execution with outer variable capture
```

---

## Why Orchestrate?

Modern distributed systems require gluing together background workers, event listeners, service clients, database connectors, and real-time pipelines. In conventional languages this coordination logic becomes deeply nested async code littered with channels, mutexes, Arc clones, and task handles. Orchestrate treats **concurrency as a first-class language concept** — the syntax itself models workers, events, and service actors directly.

```orchestrate
let monitor = automatic {
    let temp = sensor.read_temperature()
    if temp > 90 {
        trigger overheat_alert(temp)
    }
    sleep(500)
}

let alert_handler = on overheat_alert(temp: int) {
    print("WARNING: Temperature critical at " + to_string(temp))
    trigger update_orchestrator([])   // Shut down all workers
}

orchestrator main(procs: process[]) { }
```

This 14-line program starts a persistent polling loop, defines an event listener, and wires them together — with zero boilerplate for thread spawning, channel setup, or mutex management.

> **Note:** The `process[]` parameter with explicit names like `process[monitor]` seeds those specific processes at startup. Use `trigger update_orchestrator([...])` at the top level for a more dynamic boot-time selection.

---

## Tech Stack

### Compiler

The Orchestrate **compiler is itself written in Rust** and lives entirely in the `src/` directory. It is a classic single-pass pipeline:

| Stage | File | Responsibility |
| :--- | :--- | :--- |
| **Lexer** | `src/lexer.rs` | Tokenizes raw `.orch` source text into a flat `Vec<Token>` stream |
| **Parser** | `src/parser.rs` | Recursive-descent Pratt parser that builds a typed AST |
| **AST** | `src/ast.rs` | Enum-based Abstract Syntax Tree node definitions |
| **Typechecker** | `src/typechecker.rs` | Single-pass type inference and checking. Runs before codegen — catches type mismatches in `let` statements, binary operations, and function calls. Module and serverlet signatures are registered first so cross-module return types are correctly inferred. |
| **Code Generator** | `src/codegen.rs` | Traverses the AST and emits valid Rust source code as a `String` |
| **Driver** | `src/main.rs` | CLI entry point; coordinates module resolution, `load` merging, C/C++ FFI compilation via `cc-rs`, and invokes `cargo` |

### Runtime

Orchestrate has **no custom runtime VM**. Generated Rust code is compiled by Cargo and executed directly on:

- **[Tokio](https://tokio.rs/)** — Rust's industry-standard async runtime. All process blocks, serverlet actors, and event channels run as Tokio tasks.
- **[Cargo](https://doc.rust-lang.org/cargo/)** — Used internally to compile and link generated Rust files into native binaries.
- **Rust standard library** — `Arc`, `Mutex`, `OnceLock`, and `mpsc` channels power the event registry and process management system.

### Dependency Graph

```
Orchestrate Compiler (Rust binary)
    │
    ├── Lexer  ─── produces ──→  Token Stream
    ├── Parser ─── consumes ──→  Token Stream, produces ──→ AST
    └── Codegen ── consumes ──→  AST, produces ──→  .rs file
                                                         │
                                            Cargo + rustc (compilation)
                                                         │
                                              Tokio runtime (execution)
                                              ┌──────────────────────┐
                                              │ tokio = { version =  │
                                              │ "1.35", features =   │
                                              │ ["full"] }           │
                                              └──────────────────────┘
```

---

## Compilation Model

When you run `orchestrate run main.orch`, the following happens:

1. The `.orch` source is read from disk.
2. The **Lexer** breaks it into tokens (identifiers, keywords, operators, literals).
3. The **Parser** builds a recursive AST of `Stmt` and `Expr` nodes.
4. Any `use module` imports trigger the **module compiler** — each referenced directory's `module.orch` (and any `load`-ed subfiles) is parsed separately and code-generated into its own `.rs` file.
5. The **Codegen** traverses the main AST and emits a `main.rs` file with all Tokio infrastructure wired up.
6. All generated `.rs` files are written into a hidden `.orch_cache/` Cargo project.
7. `cargo run` (or `cargo build --release` for the `build` subcommand) is invoked automatically.
8. The resulting native binary runs directly.

```
orchestrate run main.orch
        │
        ├─[Lex]──→ Tokens
        ├─[Parse]──→ AST
        ├─[Codegen]──→ .orch_cache/src/main.rs
        │              .orch_cache/src/counter.rs   (module)
        │              .orch_cache/Cargo.toml
        └─[cargo run]──→ Native Binary Output
```

---

## Language Overview

### Expression-Oriented

Orchestrate is fully **expression-oriented**. Blocks, `if` expressions, and pipelines all return values. There are no statement/expression splits — everything evaluates to something.

```orchestrate
let result = if score > 100 { "Pass" } else { "Fail" }
```

### Static Type System

Orchestrate is **statically typed**. All variables and parameters carry a type at compile time. Types are inferred from context or explicitly annotated:

| Type | Description | Rust Equivalent |
| :--- | :--- | :--- |
| `int` | 64-bit signed integer | `i64` |
| `float` | 64-bit floating point | `f64` |
| `string` | UTF-8 text value | `String` |
| `bool` | Boolean true/false | `bool` |
| `void` | No return value | `()` |
| `process` | First-class process block handle | `Arc<dyn Fn() -> JoinHandle<()> + Send + Sync>` |
| `process[]` | Managed array of process handles | `Vec<ProcessRef>` |

### Built-in Functions

| Function | Description |
| :--- | :--- |
| `print(val)` | Prints any value to stdout |
| `to_string(val)` | Converts any value to a string |
| `sleep(ms)` | Asynchronously sleeps for N milliseconds |
| `stop_orch()` | Immediately exits the program (`std::process::exit(0)`) |
| `length(arr)` | Returns the number of elements in an array |
| `append(arr, val)` | Appends a value to the end of an array in place |
| `remove(arr, index)` | Removes the element at the given index from an array in place |

### Debugging Tip: `ORCH_SHOW_GENERATED`

When the compiler reports a Rust-level error you can't immediately decipher, set `ORCH_SHOW_GENERATED=1` to dump the full cargo output and the generated `.orch_cache/src/main.rs`:

```bash
# PowerShell
$env:ORCH_SHOW_GENERATED=1; orchestrate run main.orch

# bash / zsh
ORCH_SHOW_GENERATED=1 orchestrate run main.orch
```

---

## Syntax Reference

### Variables

```orchestrate
let name = "Orchestrate"         // inferred type: string
let count: int = 0               // explicit type annotation
let active: bool = true
```

### Functions

```orchestrate
fn add(a: int, b: int) -> int {
    return a + b
}
```

Compiles to a synchronous Rust `fn`. For async operations, use `task`:

```orchestrate
task fetch_data(url: string) -> string {
    sleep(200)
    return "data from " + url
}
```

Compiles to `async fn`. Calls to tasks inside process blocks are automatically `.await`-ed.

### Pipeline Operator (`|>`)

The pipeline operator chains functions left-to-right, passing the result as the first argument:

```orchestrate
let cleaned = raw_input |> trim() |> to_uppercase() |> validate()
// equivalent to: validate(to_uppercase(trim(raw_input)))
```

### Control Flow

```orchestrate
if count > 10 {
    print("High")
} else {
    print("Low")
}

while active {
    let val = poll()
    count = count + 1
}
```

### Parallel Execution

```orchestrate
parallel {
    let a = fetch_service_a()
    let b = fetch_service_b()
    let c = fetch_service_c()
}
// a, b, c are resolved concurrently via tokio::join!
```

### Array Literals

```orchestrate
trigger update_orchestrator([worker_a, worker_c])
```

Array literals (`[expr, expr, ...]`) compile to Rust `vec![...]`. They are primarily used with the built-in `update_orchestrator` trigger.

---

## Process Blocks

Process blocks are **the core concurrency primitive** of Orchestrate. There are two kinds.

### Automatic Process Blocks

Declared at the **top level** (outside any function). The compiler automatically collects all automatic blocks and passes them into the orchestrator's managed process array at startup. Each one runs in an **infinite loop** on its own Tokio task.

```orchestrate
let data_poller = automatic {
    let result = db.query("SELECT * FROM events")
    print("Got: " + result)
    sleep(1000)   // wait 1 second between iterations
}
```

**Compiled to (approximately):**
```rust
let data_poller: ProcessRef = std::sync::Arc::new(move || {
    tokio::spawn(async move {
        loop {
            // ... body ...
        }
    })
});
```

### Triggered Process Blocks

Declared at the **top level**. Auto-register their event listener on boot — no `start` needed. Execute concurrently each time the named event fires.

```orchestrate
let on_error = on error_event(code: int, msg: string) {
    print("Error " + to_string(code) + ": " + msg)
}
```

**Compiled to (approximately):**
```rust
let (tx, mut rx) = tokio::sync::mpsc::channel::<(i64, String)>(100);
get_registry_error_event().lock().unwrap().push(tx);
tokio::spawn(async move {
    while let Some((code, msg)) = rx.recv().await {
        tokio::spawn(async move {
            // ... body ...
        });
    }
});
```

### The Managed Process Array (`process[]`)

When the orchestrator declares a `process[]` parameter, you control which processes start by either:

1. **Naming them explicitly** in the brackets: `process[alpha, beta]`
2. **Firing a top-level trigger** before the orchestrator body runs: `trigger update_orchestrator([alpha, beta])`

The orchestrator then:

3. Spawns each seeded process as a Tokio task
4. Registers a listener for the built-in `update_orchestrator` event to hot-swap the running set

```orchestrate
let alpha = automatic { print("alpha") sleep(500) }
let beta  = automatic { print("beta")  sleep(500) }

orchestrator main(procs: process[alpha, beta]) {
    // Both alpha and beta are running — named explicitly in the type annotation
}
```

---

## The Orchestrator

The `orchestrator main()` declaration is the **entry point** of every Orchestrate program. It compiles to a Rust `#[tokio::main]` async function that:

- Sets up all event registries (`OnceLock<Mutex<Vec<Sender<T>>>>`)
- Initializes and auto-registers all top-level triggered blocks
- Starts the process array and spawns the `update_orchestrator` watcher
- Runs the orchestrator body
- Blocks forever in a keep-alive loop (`loop { sleep(3600s) }`)

Non-main orchestrators compile to plain `async fn` and can be called by other orchestrators.

### Built-in: `update_orchestrator`

Any code can fire `trigger update_orchestrator([...])` to atomically replace the running process set:

```orchestrate
let cleanup = on shutdown() {
    // Remove beta — only alpha continues
    trigger update_orchestrator([alpha])
}
```

**Behavior:**
- Processes **removed** from the new array → `JoinHandle::abort()` called immediately
- Processes **added** to the new array and not already running → new Tokio task spawned
- Processes in **both** arrays → continue uninterrupted (identified by `Arc::ptr_eq`)

---

## Event System

Events are declared implicitly by triggered blocks. The compiler **scans the AST** and generates a global `OnceLock<Mutex<Vec<Sender<T>>>>` registry for each unique event name discovered. Any number of listeners can subscribe to the same event and all receive each payload simultaneously (multicast).

```orchestrate
// Two listeners on the same event:
let log_handler = on data_ready(payload: string) {
    print("LOG: " + payload)
}
let cache_handler = on data_ready(payload: string) {
    print("CACHE: storing " + payload)
}

// Firing it:
trigger data_ready("sensor_reading_42")
// Both log_handler and cache_handler execute concurrently
```

**Generated registry (per event):**
```rust
static REGISTRY_DATA_READY: OnceLock<Mutex<Vec<Sender<String>>>> = OnceLock::new();
fn get_registry_data_ready() -> &'static Mutex<Vec<Sender<String>>> {
    REGISTRY_DATA_READY.get_or_init(|| Mutex::new(Vec::new()))
}
```

---

## Module System

Modules are **directories** containing a `module.orch` entry file. They are imported with:

```orchestrate
use module alias: "./path/to/dir"
```

Or by using **PROM** (the Personal Registry for Orchestrator Modules), which allows you to import registered modules by a short name:

```orchestrate
use module alias: "short_name"
```
*(See `LANGUAGE_REFERENCE.md` for full details on registering modules with `orchestrate prom add`)*

### Combined Process (No Serverlet)

Module functions are compiled into a sibling `.rs` file and linked directly into the binary. Zero call overhead.

```
my_project/
├── main.orch
└── utils/
    ├── module.orch
    └── math.orch       ← merged via `load "math.orch"`
```

```orchestrate
// utils/math.orch
fn square(n: int) -> int { return n * n }

// utils/module.orch
load "math.orch"
fn hypotenuse(a: int, b: int) -> int { return square(a) + square(b) }

// main.orch
use module utils: "./utils"
let result = utils.hypotenuse(3, 4)   // direct native function call
```

### Foreign Functions (`load_foreign`)

Module files can also load functions from Rust, C, or C++ source files directly into the module's namespace:

```orchestrate
// math/module.orch
load_foreign "rust" "./geometry.rs"    // auto-scanned, no sidecar needed
load_foreign "c"    "./fastmath.c"     // requires fastmath.orch_ffi sidecar
load_foreign "cpp"  "./stats.cpp"      // requires stats.orch_ffi sidecar
```

- **Rust:** `pub fn`s are injected verbatim; signatures are auto-scanned for the typechecker.
- **C/C++:** a `.orch_ffi` sidecar file declares function signatures; the compiler generates `extern "C"` bindings and compiles the source via `cc-rs`.

See [`LANGUAGE_REFERENCE.md §6.4`](LANGUAGE_REFERENCE.md) for the full sidecar format and type mappings.

### Separate Process (Serverlet)

For modules backed by a separate OS process (a Python service, a Go binary, a Node.js API), a **Serverlet** acts as an in-process actor gateway.

```orchestrate
// database/module.orch
serverlet DatabaseConnector {
    let connected = false

    on connect(url: string) -> bool {
        connected = true
        return true
    }

    on query(sql: string) -> string {
        return "rows for: " + sql
    }
}

// main.orch
use module db: "./database"

let worker = automatic {
    let client = start db.DatabaseConnector()
    let ok = client.connect("postgres://localhost/prod")
    let rows = client.query("SELECT * FROM logs")
    print(rows)
    stop_orch()
}

orchestrator main(procs: process[]) { }
```

**Serverlet internals (generated Rust):**
- An enum `DatabaseConnectorMsg` with one variant per handler
- A `DatabaseConnectorClient` struct with `async fn` methods wrapping `Sender` + `oneshot::channel` reply
- A `start_DatabaseConnector()` function that spawns the message-loop Tokio task and returns the client

## Documentation

- **[`README.md`](README.md)** — this file; overview, syntax reference, and architecture
- **[`LANGUAGE_REFERENCE.md`](LANGUAGE_REFERENCE.md)** — complete language specification including all generated Rust patterns, the event system internals, serverlet actor model, and operator precedence

---



## Cross-Process Event Isolation

Event registries are **process-local** — each compiled binary has its own isolated `OnceLock` singletons. Two separately compiled Orchestrate programs running as different OS processes cannot directly trigger each other's events. To communicate across process boundaries, use a Serverlet as an IPC bridge.

---

## Design Philosophy

| Principle | Implementation |
| :--- | :--- |
| **Concurrency is structural** | Workers and event handlers are declared as top-level language constructs, not library calls |
| **No hidden runtime** | Generated Rust compiles to native code; execution is fully transparent |
| **Zero-boilerplate async** | Tokio task spawning, channel wiring, and `Arc` management are compiler-generated |
| **Separation of concerns** | Automatic blocks own looping logic; triggered blocks own reaction logic; the orchestrator owns lifecycle |
| **Interoperability** | Serverlets decouple Orchestrate from external technology stacks at a well-defined message boundary |
| **Predictable performance** | All types resolve at compile time; no garbage collector; no interpreter overhead |

---

## File Structure of a Typical Project

```
my_orchestration_project/
│
├── main.orch                   ← Entry point
│
├── analytics/                  ← Module (no serverlet)
│   ├── module.orch
│   └── helpers.orch
│
├── database/                   ← Module (with serverlet)
│   ├── module.orch
│   └── query_builder.orch
│
└── .orch_cache/                ← Auto-generated, do not edit
    ├── Cargo.toml
    └── src/
        ├── main.rs             ← Generated from main.orch
        ├── analytics.rs        ← Generated from analytics/module.orch
        └── database.rs         ← Generated from database/module.orch
```
