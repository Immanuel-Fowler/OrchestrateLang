//! Web builds (`build --lib --web`) and fuel-bounded sandboxes.
//!
//! The runtime test compiles a host for `wasm32-unknown-unknown` and runs it in Bun's
//! WebAssembly engine, so the library really runs where a browser would run it: no
//! threads, no clock, no file system, and its sandboxed guests on wasmi.

use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// Removed when its test passes; a failing test keeps the directory for inspection.
struct TempRoot(PathBuf);
impl std::ops::Deref for TempRoot {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}
impl Drop for TempRoot {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
fn root(name: &str) -> TempRoot {
    let path = std::env::temp_dir().join(format!("orch_web_test_{}_{}", std::process::id(), name));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    TempRoot(path)
}

fn orchestrate(args: &[&str], cwd: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_orchestrate")).args(args).current_dir(cwd).output().unwrap()
}

fn succeeded(output: &std::process::Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn cargo(args: &[&str], cwd: &Path) -> std::process::Output {
    let output = Command::new("cargo").args(args).current_dir(cwd).output().unwrap();
    succeeded(&output);
    output
}

const PROGRAM: &str = r#"
host world {
    fn toggle(entity: int, component: int) -> int
    fn note(value: int)
}
struct Input { clicked: int[] }
struct Output { toggled: int, scored: int, plugin: int }

serverlet Scorer sandbox(memory_limit: "16mb", fuel: 200000) {
    grant call world.note
    let calls = 0
    on score(entity: int) -> int {
        calls = calls + 1
        world.note(entity * 10)
        return entity * 2 + calls
    }
    on spin(n: int) -> int {
        let i = 0
        while true { i = i + 1 }
        return i
    }
}

// Implemented by a guest the host hands over while the program runs.
serverlet Plugin sandbox(memory_limit: "8mb", fuel: 100000) {
    on score(value: int) -> int
}

let scorer = start Scorer()
let plugin_value = 0
let total = 0
on hit(entity: int) {
    world.toggle(entity, 7)
    total = total + 1
}
on install(path: string) {
    match start Plugin from path {
        result::Ok(plugin) => plugin_value = plugin.score(21)
        result::Err(why) => print("not loaded: " + why)
    }
}
on_tick(dt: float, input: Input) -> Output {
    let n = 0
    let s = 0
    for e in input.clicked {
        world.toggle(e, 1)
        s = s + scorer.score(e)
        n = n + 1
    }
    if n == 3 { world.note(scorer.spin(1)) }
    return Output { toggled: n + total, scored: s, plugin: plugin_value }
}
orchestrator main() {}
"#;

const GUEST: &str = r#"
serverlet Plugin sandbox(memory_limit: "8mb", fuel: 100000) {
    on score(value: int) -> int { return value * 2 }
}
"#;

const WEB_HOST: &str = r#"
use std::sync::Mutex;
static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());
fn push(line: String) { LOG.lock().unwrap().push(line); }
struct Host;
impl scripts::Host for Host {
    fn log(&self, level: scripts::LogLevel, message: &str) { push(format!("log {:?}: {}", level, message)); }
    fn load_guest(&self, path: &str) -> Result<Vec<u8>, String> {
        match path {
            "plugin://double" => Ok(include_bytes!("../../plugin.wasm").to_vec()),
            other => Err(format!("no guest named {other}")),
        }
    }
    fn world_toggle(&self, entity: i64, component: i64) -> Result<i64, String> { push(format!("toggle {entity} {component}")); Ok(1) }
    fn world_note(&self, value: i64) -> Result<(), String> { push(format!("note {value}")); Ok(()) }
}
static mut OUT: Vec<u8> = Vec::new();
#[unsafe(no_mangle)]
pub extern "C" fn run() -> u64 {
    assert!(scripts::WEB);
    // Neither driver: a browser has no clock or IO for Tokio to drive.
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let mut scripts = scripts::start(runtime.handle(), Host).unwrap();
    scripts.ready_blocking(&runtime).unwrap();
    scripts.trigger_hit(5).unwrap();
    scripts.trigger_install("plugin://missing".to_string()).unwrap();
    scripts.trigger_install("plugin://double".to_string()).unwrap();
    for clicked in [vec![1, 2], vec![4, 5, 6], vec![9]] {
        let out = scripts.tick_sync(&runtime, 0.016, scripts::Input { clicked }).unwrap();
        push(format!("out toggled={} scored={} plugin={}", out.toggled, out.scored, out.plugin));
    }
    scripts.shutdown_blocking(&runtime).unwrap();
    push("shutdown".into());
    let text = LOG.lock().unwrap().join("\n");
    unsafe {
        OUT = text.into_bytes();
        let out = &raw const OUT;
        (((*out).as_ptr() as u64) << 32) | (*out).len() as u64
    }
}
"#;

const RUNNER: &str = r#"
const bytes = await Bun.file(process.argv[2]).arrayBuffer();
const module = new WebAssembly.Module(bytes);
if (WebAssembly.Module.imports(module).length !== 0) {
    throw new Error("the host imports " + JSON.stringify(WebAssembly.Module.imports(module)));
}
const instance = new WebAssembly.Instance(module, {});
const packed = instance.exports.run();
const pointer = Number(packed >> 32n), length = Number(packed & 0xffffffffn);
console.log(new TextDecoder().decode(new Uint8Array(instance.exports.memory.buffer, pointer, length)));
"#;

#[test]
fn web_library_runs_in_a_webassembly_engine() {
    if Command::new("bun").arg("--version").output().is_err() {
        eprintln!("skipping: `bun` is not on PATH");
        return;
    }
    let root = root("engine");
    fs::write(root.join("main.orch"), PROGRAM).unwrap();
    fs::write(root.join("plugin.orch"), GUEST).unwrap();
    succeeded(&orchestrate(&["build", "--guest", "plugin.orch", "-o", "plugin.wasm"], &root));
    succeeded(&orchestrate(&["build", "--lib", "--web", "main.orch", "-o", "scripts"], &root));
    let manifest = fs::read_to_string(root.join("scripts/Cargo.toml")).unwrap();
    assert!(manifest.contains("wasmi") && !manifest.contains("wasmtime"), "{manifest}");
    assert!(!manifest.contains("\"full\""), "{manifest}");

    fs::create_dir_all(root.join("host/src")).unwrap();
    fs::write(
        root.join("host/Cargo.toml"),
        "[package]\nname=\"web_host\"\nversion=\"0.1.0\"\nedition=\"2024\"\n[lib]\ncrate-type=[\"cdylib\"]\n[dependencies]\nscripts={path=\"../scripts\"}\ntokio={version=\"1.35\",default-features=false,features=[\"rt\"]}\n[workspace]\n",
    )
    .unwrap();
    fs::write(root.join("host/src/lib.rs"), WEB_HOST).unwrap();
    cargo(&["build", "--quiet", "--release", "--target", "wasm32-unknown-unknown"], &root.join("host"));
    fs::write(root.join("run.js"), RUNNER).unwrap();
    let wasm = root.join("host/target/wasm32-unknown-unknown/release/web_host.wasm");
    let output = Command::new("bun").arg(root.join("run.js")).arg(&wasm).output().unwrap();
    succeeded(&output);
    let text = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines,
        [
            "toggle 5 7",
            "log Info: not loaded: sandboxed serverlet 'Plugin': cannot read 'plugin://missing': no guest named plugin://missing",
            // Guest state persists between calls, and each call reaches the host only
            // through its grant.
            "toggle 1 1", "note 10", "toggle 2 1", "note 20",
            "out toggled=3 scored=9 plugin=42",
            "toggle 4 1", "note 40", "toggle 5 1", "note 50", "toggle 6 1", "note 60",
            // The runaway call is stopped by its fuel, answers its default, and the
            // guest is replaced, which resets its state.
            "log Error: [orchestrate] wasm call 'Scorer::spin' failed: the guest used up its fuel",
            "note 0",
            "out toggled=4 scored=42 plugin=42",
            "toggle 9 1", "note 90",
            "out toggled=2 scored=19 plugin=42",
            "shutdown",
        ],
        "{text}"
    );
}

#[test]
fn web_rules_refuse_what_a_browser_cannot_carry() {
    let root = root("rules");
    fs::write(root.join("worker.py"), "class Worker:\n    def run(self):\n        return 1\n").unwrap();
    let cases = [
        ("serverlet W via python(source: \"worker.py\") { on run() -> int }\n", "python landline, which runs as a child process"),
        ("serverlet S secret { on run() -> int { return 1 } }\n", "is secret, which runs as a child process"),
        ("serverlet G sandbox(memory_limit: \"8mb\", timeout: \"5s\") { on run() -> int { return 1 } }\n", "bound each call with `fuel: <amount>` instead"),
        ("measure serverlets\n", "`measure` times calls on the wall clock"),
        ("on_tick(dt: float) { print(to_string(clock_micros())) }\n", "`clock_micros` reads the wall clock"),
        ("let w = automatic { print(\"x\") }\n", "an `automatic` worker runs beside the ticks"),
    ];
    for (source, expected) in cases {
        fs::write(root.join("main.orch"), format!("{source}orchestrator main() {{}}\n")).unwrap();
        let output = orchestrate(&["check", "--lib", "--web", "main.orch"], &root);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success() && stderr.contains(expected) && stderr.contains("a web build cannot carry it"), "{source}: {stderr}");
        // The same program is fine outside the browser, or fails there for its own reasons.
        let native = orchestrate(&["check", "--lib", "main.orch"], &root);
        assert!(!String::from_utf8_lossy(&native.stderr).contains("web build"), "{source}");
    }

    fs::write(root.join("main.orch"), "orchestrator main() {}\n").unwrap();
    let output = orchestrate(&["check", "--web", "main.orch"], &root);
    assert!(String::from_utf8_lossy(&output.stderr).contains("--web applies to check --lib"));
    let output = orchestrate(&["build", "--web", "main.orch"], &root);
    assert!(String::from_utf8_lossy(&output.stderr).contains("--web applies to build --lib"));
    let output = orchestrate(&["build", "--lib", "--web", "--target", "x86_64-unknown-linux-gnu", "main.orch", "-o", "scripts"], &root);
    assert!(String::from_utf8_lossy(&output.stderr).contains("--web builds for wasm32-unknown-unknown"));
    let output = orchestrate(&["check", "main.orch"], &root);
    succeeded(&output);
}

#[test]
fn sandbox_config_needs_a_bound_on_each_call() {
    let root = root("bounds");
    for (config, expected) in [
        ("memory_limit: \"8mb\"", "a bound on each call: 'timeout', 'fuel', or both"),
        ("memory_limit: \"8mb\", fuel: 0", "Invalid sandbox fuel"),
        ("memory_limit: \"8mb\", fuel: \"10\"", "Invalid sandbox fuel"),
    ] {
        fs::write(root.join("main.orch"), format!("serverlet G sandbox({config}) {{ on run() -> int {{ return 1 }} }}\norchestrator main() {{}}\n")).unwrap();
        let output = orchestrate(&["check", "main.orch"], &root);
        assert!(String::from_utf8_lossy(&output.stderr).contains(expected), "{config}: {}", String::from_utf8_lossy(&output.stderr));
    }
}

/// On wasmtime, fuel bounds a call alongside the timeout, and unlike the timeout a
/// deterministic replay enforces it, since it counts work rather than wall time.
#[test]
fn native_sandbox_fuel_stops_a_runaway_call_in_live_and_replayed_runs() {
    let root = root("native_fuel");
    fs::write(
        root.join("main.orch"),
        r#"
host world { fn note(value: int) }
serverlet Guest sandbox(memory_limit: "16mb", timeout: "30s", fuel: 500000) {
    let calls = 0
    on add(n: int) -> int { calls = calls + 1  return n + calls }
    on spin(n: int) -> int {
        let i = 0
        while true { i = i + 1 }
        return i
    }
}
let guest = start Guest()
on_tick(dt: float) {
    world.note(guest.add(10))
    world.note(guest.spin(1))
    world.note(guest.add(10))
}
orchestrator main() {}
"#,
    )
    .unwrap();
    succeeded(&orchestrate(&["build", "--lib", "main.orch", "-o", "scripts"], &root));
    fs::create_dir_all(root.join("host/src")).unwrap();
    fs::write(
        root.join("host/Cargo.toml"),
        "[package]\nname=\"fuel_host\"\nversion=\"0.1.0\"\nedition=\"2021\"\n[dependencies]\nscripts={path=\"../scripts\"}\ntokio={version=\"1.35\",features=[\"full\"]}\n[workspace]\n",
    )
    .unwrap();
    fs::write(
        root.join("host/src/main.rs"),
        r#"
use std::sync::{Arc, Mutex};
struct Host(Arc<Mutex<Vec<String>>>);
impl scripts::Host for Host {
    fn log(&self, _level: scripts::LogLevel, message: &str) { self.0.lock().unwrap().push(message.to_string()); }
    fn world_note(&self, value: i64) -> Result<(), String> { self.0.lock().unwrap().push(value.to_string()); Ok(()) }
}
fn run(deterministic: bool) -> Vec<String> {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let options = scripts::StartOptions { deterministic, ..Default::default() };
    let mut scripts = scripts::start_with_options(runtime.handle(), Host(log.clone()), options).unwrap();
    let started = std::time::Instant::now();
    scripts.tick_blocking(&runtime, 0.1).unwrap();
    // Fuel stops the loop long before the 30 second timeout would.
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    scripts.shutdown_blocking(&runtime).unwrap();
    let lines = log.lock().unwrap().clone();
    lines
}
fn main() {
    let live = run(false);
    let replay = run(true);
    assert_eq!(live, replay);
    println!("{}", live.join("|"));
}
"#,
    )
    .unwrap();
    let output = cargo(&["run", "--quiet"], &root.join("host"));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        "11|[orchestrate] wasm call 'Guest::spin' failed: the guest used up its fuel|0|11"
    );
}

const BURN: &str = r#"
host world { fn note(value: int) }
serverlet Burner sandbox(memory_limit: "8mb", fuel: 3000) {
    on burn(n: int) -> int {
        let i = 0
        let acc = 0
        while i < n {
            acc = acc + i * 3 % 7
            i = i + 1
        }
        return acc
    }
}
let burner = start Burner()
on_tick(dt: float) {
    let n = 0
    while n <= 2000 {
        world.note(burner.burn(n))
        n = n + 25
    }
}
orchestrator main() {}
"#;

/// The same guest, the same budget, two engines: wasmtime natively and wasmi in a
/// WebAssembly engine stop it at the same instruction, because both run the same metered
/// bytecode. A replay across a native peer and a browser peer depends on exactly this.
#[test]
fn metered_fuel_stops_a_guest_at_the_same_point_on_every_engine() {
    if Command::new("bun").arg("--version").output().is_err() {
        eprintln!("skipping: `bun` is not on PATH");
        return;
    }
    let root = root("cross_engine");
    fs::write(root.join("main.orch"), BURN).unwrap();
    succeeded(&orchestrate(&["build", "--lib", "main.orch", "-o", "native"], &root));
    succeeded(&orchestrate(&["build", "--lib", "--web", "main.orch", "-o", "web"], &root));

    // Natively, on wasmtime.
    fs::create_dir_all(root.join("native_host/src")).unwrap();
    fs::write(root.join("native_host/Cargo.toml"), "[package]\nname=\"native_host\"\nversion=\"0.1.0\"\nedition=\"2021\"\n[dependencies]\nnative={path=\"../native\"}\ntokio={version=\"1.35\",features=[\"full\"]}\n[workspace]\n").unwrap();
    fs::write(root.join("native_host/src/main.rs"), r#"
use std::sync::{Arc, Mutex};
struct Host(Arc<Mutex<Vec<String>>>);
impl native::Host for Host {
    fn log(&self, _: native::LogLevel, message: &str) { self.0.lock().unwrap().push(message.to_string()); }
    fn world_note(&self, value: i64) -> Result<(), String> { self.0.lock().unwrap().push(value.to_string()); Ok(()) }
}
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut scripts = native::start(runtime.handle(), Host(log.clone())).unwrap();
    scripts.tick_blocking(&runtime, 0.1).unwrap();
    scripts.shutdown_blocking(&runtime).unwrap();
    println!("{}", log.lock().unwrap().join("\n"));
}
"#).unwrap();
    let native = String::from_utf8(cargo(&["run", "--quiet"], &root.join("native_host")).stdout).unwrap();

    // In a WebAssembly engine, on wasmi.
    fs::create_dir_all(root.join("web_host/src")).unwrap();
    fs::write(root.join("web_host/Cargo.toml"), "[package]\nname=\"web_host\"\nversion=\"0.1.0\"\nedition=\"2024\"\n[lib]\ncrate-type=[\"cdylib\"]\n[dependencies]\nweb={path=\"../web\"}\ntokio={version=\"1.35\",default-features=false,features=[\"rt\"]}\n[workspace]\n").unwrap();
    fs::write(root.join("web_host/src/lib.rs"), r#"
use std::sync::Mutex;
static LOG: Mutex<Vec<String>> = Mutex::new(Vec::new());
struct Host;
impl web::Host for Host {
    fn log(&self, _: web::LogLevel, message: &str) { LOG.lock().unwrap().push(message.to_string()); }
    fn world_note(&self, value: i64) -> Result<(), String> { LOG.lock().unwrap().push(value.to_string()); Ok(()) }
}
static mut OUT: Vec<u8> = Vec::new();
#[unsafe(no_mangle)]
pub extern "C" fn run() -> u64 {
    let runtime = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let mut scripts = web::start(runtime.handle(), Host).unwrap();
    scripts.tick_sync(&runtime, 0.1).unwrap();
    scripts.shutdown_blocking(&runtime).unwrap();
    unsafe {
        OUT = LOG.lock().unwrap().join("\n").into_bytes();
        let out = &raw const OUT;
        (((*out).as_ptr() as u64) << 32) | (*out).len() as u64
    }
}
"#).unwrap();
    cargo(&["build", "--quiet", "--release", "--target", "wasm32-unknown-unknown"], &root.join("web_host"));
    fs::write(root.join("run.js"), RUNNER).unwrap();
    let output = Command::new("bun").arg(root.join("run.js")).arg(root.join("web_host/target/wasm32-unknown-unknown/release/web_host.wasm")).output().unwrap();
    succeeded(&output);
    let web = String::from_utf8(output.stdout).unwrap();

    let native: Vec<&str> = native.lines().collect();
    let web: Vec<&str> = web.lines().collect();
    assert_eq!(native, web, "the two engines disagree");
    // Some calls finish and later ones run out, so the budget really bites, at one place.
    let first_out = native.iter().position(|line| line.contains("used up its fuel")).expect("a call ran out of fuel");
    assert!(first_out > 5, "{native:?}");
    assert!(native[..first_out].iter().all(|line| line.parse::<i64>().is_ok()));
}
