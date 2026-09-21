use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// Scoped to the test process, so an interrupted run cannot leave a half-built
/// `.orch_cache` behind for the next one to hang on. Removed when its test passes; a
/// failing test keeps the directory for inspection.
struct TempRoot(PathBuf);
impl std::ops::Deref for TempRoot {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}
impl AsRef<Path> for TempRoot {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}
impl AsRef<std::ffi::OsStr> for TempRoot {
    fn as_ref(&self) -> &std::ffi::OsStr {
        self.0.as_os_str()
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
    let path = std::env::temp_dir().join(format!(
        "orch_library_test_{}_{}",
        std::process::id(),
        name
    ));
    fs::create_dir_all(&path).unwrap();
    TempRoot(path)
}
fn build(root: &Path, source: &str) {
    fs::write(root.join("main.orch"), source).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_orchestrate"))
        .args(["build", "--lib"])
        .arg(root.join("main.orch"))
        .arg("-o")
        .arg(root.join("scripts"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
fn host(root: &Path, source: &str) -> String {
    host_in(root, source, false)
}
/// Runs a host program against the library built in `root`, optimized when `release` is
/// set, and returns its stdout.
fn host_in(root: &Path, source: &str, release: bool) -> String {
    fs::create_dir_all(root.join("host/src")).unwrap();
    fs::write(root.join("host/Cargo.toml"), "[package]\nname=\"host_test\"\nversion=\"0.1.0\"\nedition=\"2021\"\n[dependencies]\nscripts={path=\"../scripts\"}\ntokio={version=\"1.35\",features=[\"full\"]}\n").unwrap();
    fs::write(root.join("host/src/main.rs"), source).unwrap();
    let mut args = vec!["run", "--quiet"];
    if release {
        args.push("--release");
    }
    let result = Command::new("cargo")
        .args(args)
        .current_dir(root.join("host"))
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8_lossy(&result.stdout).into_owned()
}

#[test]
fn library_host_callbacks_grants_instances_and_lifecycle() {
    let root = root("host_callbacks");
    fs::write(
        root.join("impl.py"),
        r#"
from dataclasses import dataclass
from orchestratelang import landline
@dataclass
class Point:
    x: int
class Python(landline.Serverlet):
    def exercise(self) -> int:
        assert not hasattr(self.host.world, 'denied')
        # A forged function id must also be denied by the Rust dispatcher.
        landline._write_frame(self.host._writer, 6, 999, landline._encode(int, 99))
        frame = landline._read_frame(self.host._reader)
        assert frame[:2] == (7, 999) and frame[2][0] == 0
        for value in [-1, -2]:
            try:
                self.host.world.record(value)
                raise AssertionError('expected host error')
            except RuntimeError:
                pass
        result = self.host.world.shift(Point(5))
        self.host.world.record(result.x)
        return self.host.world.record(7)
landline.serve(Python)
"#,
    )
    .unwrap();
    build(
        &root,
        r#"
struct Point { x: int }
host world {
    fn record(n: int) -> int
    fn shift(point: Point) -> Point
    fn denied() -> int
}
serverlet Python via python(source: "impl.py") {
    grant call world.record
    grant call world.shift
    on exercise() -> int
}
let p = start Python()
on_start { world.record(1) }
on pulse(n: int) { world.record(n) }
on_tick(dt: float) {
    p.exercise()
    trigger pulse(20)
    sleep(20)
    stop_orch()
}
on_stop { world.record(100) }
orchestrator main() {}
"#,
    );
    // The generated crate embeds its assets; original sources are no longer needed.
    fs::remove_file(root.join("impl.py")).unwrap();
    let output = host(
        &root,
        r#"
use std::sync::{Arc, Mutex};
struct Host(Arc<Mutex<Vec<i64>>>, i64);
impl scripts::Host for Host {
    fn world_record(&self, n: i64) -> Result<i64, String> {
        if n == -1 { return Err("expected error".into()); }
        if n == -2 { panic!("expected panic"); }
        self.0.lock().unwrap().push(n);
        Ok(n)
    }
    fn world_shift(&self, p: scripts::Point) -> Result<scripts::Point, String> { Ok(scripts::Point { x: p.x + self.1 }) }
    fn world_denied(&self) -> Result<i64, String> { panic!("ungranted host function was called") }
}
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    runtime.block_on(async {
        let a = Arc::new(Mutex::new(Vec::new()));
        let b = Arc::new(Mutex::new(Vec::new()));
        let mut first = scripts::start(runtime.handle(), Host(a.clone(), 10)).unwrap();
        let mut second = scripts::start(runtime.handle(), Host(b.clone(), 30)).unwrap();
        first.ready().await.unwrap(); second.ready().await.unwrap();
        first.tick(0.1).await.unwrap();
        assert!(first.stop_requested());
        assert!(!second.stop_requested());
        assert_eq!(*b.lock().unwrap(), vec![1]);
        assert!(first.tick(0.1).await.is_err());
        first.shutdown().await.unwrap();
        second.tick(0.2).await.unwrap();
        second.shutdown().await.unwrap();
        assert_eq!(*a.lock().unwrap(), vec![1,15,7,20,100]);
        assert_eq!(*b.lock().unwrap(), vec![1,35,7,20,100]);
        println!("host survived both instances");
    });
}
"#,
    );
    assert!(output.contains("host survived both instances"));
}

/// A sandboxed serverlet's grants are mediated host functions: each `grant call` is one
/// import in the guest's linker, behind which the `Host` method runs, and nothing else
/// is defined. An int, a struct, and an array cross through them; a host error fails the
/// one call inside the guest, which logs it and continues with the default; the guest's
/// state survives across ticks.
#[test]
fn library_sandbox_grants_are_mediated_host_functions() {
    let root = root("sandbox_grants");
    build(&root, r#"
struct Point { x: int, y: int }

host world {
    fn record(n: int) -> int
    fn shift(p: Point) -> Point
    fn tally(items: int[]) -> int
    fn denied() -> int
    fn fail() -> int
}

serverlet Plugin sandbox(memory_limit: "16mb", timeout: "2s") {
    grant call world.record
    grant call world.shift
    grant call world.tally
    grant call world.fail
    let total = 0

    on bump(n: int) -> int {
        total = total + world.record(n)
        return total
    }
    on relocate(p: Point) -> Point {
        return world.shift(p)
    }
    on sum(items: int[]) -> int {
        return world.tally(items)
    }
    on failing() -> int {
        return world.fail() + 1
    }
}

let plugin = start Plugin()
on_tick(dt: float) {
    world.record(plugin.bump(5))
    let moved = plugin.relocate(Point { x: 1, y: 2 })
    world.record(moved.x * 10 + moved.y)
    world.record(plugin.sum([1, 2, 3]))
    world.record(plugin.failing())
}
orchestrator main() {}
"#);
    let output = host(&root, r#"
use std::sync::{Arc, Mutex};
struct Host(Arc<Mutex<Vec<String>>>);
impl scripts::Host for Host {
    fn world_record(&self, n: i64) -> Result<i64, String> { self.0.lock().unwrap().push(format!("record {n}")); Ok(n * 2) }
    fn world_shift(&self, p: scripts::Point) -> Result<scripts::Point, String> { self.0.lock().unwrap().push(format!("shift {} {}", p.x, p.y)); Ok(scripts::Point { x: p.x + 1, y: p.y + 1 }) }
    fn world_tally(&self, items: Vec<i64>) -> Result<i64, String> { self.0.lock().unwrap().push(format!("tally {}", items.len())); Ok(items.iter().sum()) }
    fn world_denied(&self) -> Result<i64, String> { panic!("an ungranted host function was called") }
    fn world_fail(&self) -> Result<i64, String> { Err("host says no".into()) }
    fn log(&self, _: scripts::LogLevel, message: &str) { self.0.lock().unwrap().push(format!("log {message}")); }
}
fn main() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let mut scripts = scripts::start(runtime.handle(), Host(calls.clone())).unwrap();
    scripts.ready_blocking(&runtime).unwrap();
    scripts.tick_blocking(&runtime, 0.016).unwrap();
    scripts.tick_blocking(&runtime, 0.016).unwrap();
    scripts.shutdown_blocking(&runtime).unwrap();
    for line in calls.lock().unwrap().iter() { println!("{line}"); }
}
"#);
    let tick = |total: i64| format!(
        "record 5\nrecord {total}\nshift 1 2\nrecord 23\ntally 3\nrecord 6\nlog [orchestrate] sandbox guest: [orchestrate] host call world.fail failed: host says no\nrecord 1"
    );
    // `bump` records 5 inside the guest and adds the doubled reply to its state, so the
    // second tick's total shows the guest kept its state across a host call and a tick.
    assert_eq!(output.trim(), format!("{}\n{}", tick(10), tick(20)));
}

/// `shared let` reached from a hook: the tick body is a block whose tail statement touches
/// shared state, so it takes the lock through the context the hook is bound to.
#[test]
fn library_shared_state_in_hooks() {
    let root = root("shared_in_hooks");
    build(&root, r#"
host world { fn record(n: int) }
shared let hits = 0
on_tick(dt: float) { hits = hits + 1 }
on_stop { world.record(hits) }
orchestrator main() {}
"#);
    let output = host(&root, r#"
use std::sync::{Arc, Mutex};
struct Host(Arc<Mutex<Vec<i64>>>);
impl scripts::Host for Host { fn world_record(&self, n: i64) -> Result<(), String> { self.0.lock().unwrap().push(n); Ok(()) } }
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let mut scripts = scripts::start(runtime.handle(), Host(recorded.clone())).unwrap();
    scripts.ready_blocking(&runtime).unwrap();
    for _ in 0..3 { scripts.tick_sync(&runtime, 0.016).unwrap(); }
    scripts.tick_blocking(&runtime, 0.016).unwrap();
    scripts.shutdown_blocking(&runtime).unwrap();
    println!("{:?}", recorded.lock().unwrap());
}
"#);
    assert_eq!(output.trim(), "[4]");
}

#[test]
fn library_secret_and_drop_shutdown() {
    let root = root("secret");
    build(
        &root,
        r#"
serverlet Counter secret { on add(n: int) -> int { return n + 1 } }
let c = start Counter()
on_tick(dt: float) { print(to_string(c.add(8))) }
on_stop { print("stopped") }
orchestrator main() {}
"#,
    );
    let output = host(
        &root,
        r#"
fn main() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let mut instance = scripts::start(runtime.handle(), ()).unwrap();
        instance.tick(0.1).await.unwrap();
        instance.shutdown().await.unwrap();
        let mut dropped = scripts::start(runtime.handle(), ()).unwrap();
        dropped.ready().await.unwrap();
        drop(dropped);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        println!("alive");
    });
}
"#,
    );
    assert_eq!(output.lines().filter(|s| *s == "stopped").count(), 2);
    assert!(output.contains("9") && output.contains("alive"));
}

#[test]
fn library_module_host_struct_callback() {
    let root = root("module_host");
    fs::create_dir_all(root.join("service")).unwrap();
    fs::write(
        root.join("service/module.orch"),
        r#"
serverlet Worker via python(source: "impl.py") {
    grant call world.shift
    on run() -> int
}
"#,
    )
    .unwrap();
    fs::write(root.join("service/impl.py"), "from dataclasses import dataclass\nfrom orchestratelang import landline\n@dataclass\nclass Point:\n    x: int\nclass Worker(landline.Serverlet):\n    def run(self) -> int: return self.host.world.shift(Point(4)).x\nlandline.serve(Worker)\n").unwrap();
    build(
        &root,
        r#"
use module service: "./service"
struct Point { x: int }
host world { fn shift(p: Point) -> Point }
let worker = start service.Worker()
on_tick(dt: float) { print(to_string(worker.run())) }
orchestrator main() {}
"#,
    );
    let output = host(
        &root,
        r#"
struct Host;
impl scripts::Host for Host {
    fn world_shift(&self, p: scripts::Point) -> Result<scripts::Point, String> { Ok(scripts::Point { x: p.x + 8 }) }
}
fn main() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let mut scripts = scripts::start(runtime.handle(), Host).unwrap();
        scripts.tick(0.1).await.unwrap();
        scripts.shutdown().await.unwrap();
    });
}
"#,
    );
    assert_eq!(output.trim(), "12");
}

#[test]
fn library_declaration_diagnostics() {
    use orchestrate_lib::{lexer::Lexer, parser::Parser, typechecker::TypeChecker};
    for source in [
        "host world { fn record(n: int) } on_tick(dt: float) { world.record(true) }",
        "host world { fn record(n: int) } on_tick(dt: float) { world.missing(1) }",
        "host world { fn record() } serverlet P via python(source: \"x.py\") { grant call world.missing on f() }",
        "host world { fn record() fn record() }",
    ] {
        let ast = Parser::new(Lexer::new(source).tokenize().unwrap()).parse().unwrap();
        assert!(TypeChecker::new().type_check(&ast).is_err(), "{}", source);
    }
    let mut parser = Parser::new(Lexer::new("on_tick(dt: int) {}").tokenize().unwrap());
    assert!(parser.parse().unwrap_err().contains("must be float"));
    let root = root("invalid_output");
    fs::write(root.join("main.orch"), "on_tick(dt: float) {}\n").unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_orchestrate"))
        .arg("build")
        .arg(root.join("main.orch"))
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("require build --lib"));
    let result = Command::new(env!("CARGO_BIN_EXE_orchestrate"))
        .args(["build", "--lib"])
        .arg(root.join("main.orch"))
        .arg("-o")
        .arg(&root)
        .output()
        .unwrap();
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("already exists"));
}

#[test]
fn library_shutdown_cancels_pending_tick() {
    let root = root("cancel_tick");
    build(
        &root,
        "on_tick(dt: float) { sleep(60000) }\non_stop { print(\"cleanup hook\") }\n",
    );
    let output = host(
        &root,
        r#"
fn main() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        let mut scripts = scripts::start(runtime.handle(), ()).unwrap();
        scripts.ready().await.unwrap();
        assert!(tokio::time::timeout(std::time::Duration::from_millis(20), scripts.tick(0.1)).await.is_err());
        tokio::time::timeout(std::time::Duration::from_secs(2), scripts.shutdown()).await.unwrap().unwrap();
        println!("shutdown completed");
    });
}
"#,
    );
    assert!(output.contains("cleanup hook") && output.contains("shutdown completed"));
}

#[test]
fn engine_workspace_and_supported_rust_metadata() {
    let root = root("engine_workspace");
    fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = []\nresolver = \"3\"\n").unwrap();
    build(&root, "serverlet C secret { on value() -> int { return 1 } }\nlet c = start C()\non_tick(dt: float) { c.value() }\n");
    fs::write(root.join("Cargo.toml"), "[workspace]\nmembers = [\"scripts\"]\nresolver = \"3\"\n").unwrap();
    let rust_version = |root: &Path| {
        let result = Command::new("cargo").args(["metadata", "--no-deps", "--format-version", "1"])
            .arg("--manifest-path").arg(root.join("scripts/Cargo.toml")).output().unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        let metadata: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        let package = metadata["packages"][0].clone();
        assert_eq!(package["edition"], "2024");
        package["rust_version"].as_str().unwrap().to_owned()
    };
    assert_eq!(rust_version(&root), "1.89");

    let library = |root: &Path, version: &str| {
        Command::new(env!("CARGO_BIN_EXE_orchestrate"))
            .args(["build", "--lib", "--rust-version", version])
            .arg(root.join("main.orch")).arg("-o").arg(root.join("scripts")).output().unwrap()
    };
    let result = library(&root, "1.85.0");
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(rust_version(&root), "1.85.0");
    // Edition 2024 needs 1.85, so an older request is refused rather than generated.
    let result = library(&root, "1.84");
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("below 1.85"));
    assert!(String::from_utf8_lossy(&library(&root, "stable").stderr).contains("x.y or x.y.z"));
}

/// A handler that triggers its own event must not keep a tick running forever: the
/// events it queues wait for the next drain.
#[test]
fn engine_self_triggering_event_bounds_every_tick() {
    let root = root("self_trigger");
    build(&root, r#"
host world { fn record(n: int) }
on ping(n: int) {
    world.record(n)
    trigger ping(n + 1)
}
on_tick(dt: float) {
}
orchestrator main() {}
"#);
    let output = host(&root, r#"
use std::sync::{Arc, Mutex};
struct Host(Arc<Mutex<Vec<i64>>>);
impl scripts::Host for Host { fn world_record(&self, n: i64) -> Result<(), String> { self.0.lock().unwrap().push(n); Ok(()) } }
fn drive(runtime: &tokio::runtime::Runtime, options: scripts::StartOptions) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut scripts = scripts::start_with_options(runtime.handle(), Host(log.clone()), options).unwrap();
    scripts.ready_blocking(runtime).unwrap();
    scripts.trigger_ping(0).unwrap();
    let mut handled = 0;
    for frame in 0..32 {
        let started = std::time::Instant::now();
        scripts.tick_blocking(runtime, 1.0 / 60.0).unwrap();
        assert!(started.elapsed() < std::time::Duration::from_secs(5), "tick {} took {:?}", frame, started.elapsed());
        let total = log.lock().unwrap().len();
        // One drain before the tick hooks and one after, each running the handler once.
        assert!(total - handled <= 2, "tick {} ran the handler {} times", frame, total - handled);
        handled = total;
    }
    // Every triggered event is handled exactly once, in order, and none is dropped.
    assert_eq!(*log.lock().unwrap(), (0..64).collect::<Vec<i64>>());
    scripts.shutdown_blocking(runtime).unwrap();
}
fn main() {
    // A hung tick never returns, so fail the process instead of blocking the test run.
    std::thread::spawn(|| {
        std::thread::sleep(std::time::Duration::from_secs(60));
        eprintln!("a tick never returned");
        std::process::exit(2);
    });
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    drive(&runtime, scripts::StartOptions::default());
    drive(&runtime, scripts::StartOptions { deterministic: true, ..Default::default() });
}
"#);
    assert!(output.is_empty(), "{}", output);
}

/// Generated crates must build on the oldest Rust they declare, not just on the
/// compiler's own toolchain.
#[test]
fn engine_generated_crate_builds_on_declared_rust_version() {
    let toolchain = Command::new("cargo").args(["+1.89", "--version"]).output();
    if !toolchain.map(|o| o.status.success()).unwrap_or(false) {
        eprintln!("skipping: Rust 1.89 is not installed (rustup toolchain install 1.89 --profile minimal)");
        return;
    }
    let root = root("minimum_rust");
    let module = root.join("c_math");
    fs::create_dir_all(&module).unwrap();
    fs::write(module.join("module.orch"), "load_foreign \"c\" \"math.c\"\n").unwrap();
    fs::write(module.join("math.c"), "long long twice(long long n) { return n * 2; }\n").unwrap();
    fs::write(module.join("math.orch_ffi"), "twice(n: int) -> int\n").unwrap();
    fs::write(root.join("impl.py"), "from dataclasses import dataclass\nfrom orchestratelang import landline\n@dataclass\nclass Input:\n    values: list[int]\n@dataclass\nclass Output:\n    total: int\nclass P(landline.Serverlet):\n    def tick(self, batch: Input) -> Output: return Output(sum(batch.values) + self.host.world.record(1))\nlandline.serve(P)\n").unwrap();
    let source = r#"
use module cm: "./c_math"
struct Input { values: int[] }
struct Output { total: int }
host world { fn record(n: int) -> int }
serverlet Counter secret { on add(n: int) -> int { return n + 1 } }
serverlet P via python(source: "impl.py") {
    grant call world.record
    on tick(batch: Input) -> Output
}
let c = start Counter()
let p = start P()
on hit(n: int) { world.record(n) }
on_fixed_tick(step: float) { print(to_string(cm.twice(2))) world.record(c.add(7)) }
on_tick(dt: float, input: Input) -> Output { return p.tick(input) }
orchestrator main() {}
"#;
    // Generating under 1.89 also compiles the secret serverlet child with it.
    fs::write(root.join("main.orch"), source).unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_orchestrate"))
        .args(["build", "--lib"]).arg(root.join("main.orch")).arg("-o").arg(root.join("scripts"))
        .env("RUSTUP_TOOLCHAIN", "1.89").output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    // And a host on 1.89 can check the generated crate it links against.
    let result = Command::new("cargo").args(["+1.89", "check", "--quiet"])
        .current_dir(root.join("scripts")).output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
}

#[test]
fn engine_latest_edition_c_cpp_and_reserved_identifiers() {
    let root = root("edition_ffi");
    for (name, language, source) in [
        ("c_math", "c", "long long twice(long long n) { return n * 2; }"),
        ("cpp_math", "cpp", "extern \"C\" long long twice(long long n) { return n * 2; }"),
    ] {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("module.orch"), format!("load_foreign \"{language}\" \"math.{language}\"\n")).unwrap();
        fs::write(dir.join(format!("math.{language}")), source).unwrap();
        fs::write(dir.join("math.orch_ffi"), "twice(n: int) -> int\n").unwrap();
    }
    build(&root, r#"
use module c: "./c_math"
use module cpp: "./cpp_math"
let gen = 3
on_tick(dt: float) { print(to_string(c.twice(gen))) print(to_string(cpp.twice(gen))) print("gen") }
"#);
    let output = host(&root, r#"
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut scripts = scripts::start(runtime.handle(), ()).unwrap();
    scripts.tick_blocking(&runtime, 0.1).unwrap();
    scripts.shutdown_blocking(&runtime).unwrap();
}
"#);
    assert_eq!(output.trim(), "6\n6\ngen");
}

/// Zig and Swift static libraries (and the Swift runtime) must reach the host's final link.
#[test]
fn engine_zig_and_swift_ffi() {
    for tool in ["zig", "swiftc"] {
        if Command::new(tool).arg(if tool == "zig" { "version" } else { "--version" }).output().is_err() {
            eprintln!("skipping: `{}` is not on PATH", tool);
            return;
        }
    }
    let root = root("zig_swift_ffi");
    for (name, language, source) in [
        ("zig_math", "zig", "export fn zig_triple(n: i64) i64 { return n * 3; }\n"),
        ("swift_math", "swift", "@_cdecl(\"swift_square\")\npublic func swiftSquare(_ n: Int64) -> Int64 { n * n }\n"),
    ] {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("module.orch"), format!("load_foreign \"{language}\" \"math.{language}\"\n")).unwrap();
        fs::write(dir.join(format!("math.{language}")), source).unwrap();
        let function = if language == "zig" { "zig_triple" } else { "swift_square" };
        fs::write(dir.join("math.orch_ffi"), format!("{function}(n: int) -> int\n")).unwrap();
    }
    build(&root, r#"
use module z: "./zig_math"
use module s: "./swift_math"
on_tick(dt: float) { print(to_string(z.zig_triple(s.swift_square(4)))) }
"#);
    let output = host(&root, r#"
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut scripts = scripts::start(runtime.handle(), ()).unwrap();
    scripts.tick_blocking(&runtime, 0.1).unwrap();
    scripts.shutdown_blocking(&runtime).unwrap();
}
"#);
    assert_eq!(output.trim(), "48");
}

#[test]
fn engine_typed_tick_and_fixed_step() {
    let root = root("typed_tick");
    fs::write(root.join("impl.py"), "from dataclasses import dataclass\nfrom orchestratelang import landline\n@dataclass\nclass Input:\n    values: list[int]\n@dataclass\nclass Output:\n    total: int\nclass P(landline.Serverlet):\n    def tick(self, batch: Input) -> Output:\n        assert self.tick_dt == 0.25\n        return Output(sum(batch.values) + self.tick_number)\nlandline.serve(P)\n").unwrap();
    build(&root, r#"
struct Input { values: int[] }
struct Output { total: int }
serverlet P via python(source: "impl.py") { on tick(batch: Input) -> Output }
let p = start P()
on_fixed_tick(step: float) { print("fixed") }
on_tick(dt: float, input: Input) -> Output { return p.tick(input) }
"#);
    let output = host(&root, r#"
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut scripts = scripts::start(runtime.handle(), ()).unwrap();
    scripts.fixed_tick_blocking(&runtime, 0.01).unwrap();
    let result = scripts.tick_blocking(&runtime, 0.25, scripts::Input { values: vec![2,3] }).unwrap();
    assert_eq!(result.total, 6);
    scripts.fixed_tick_blocking(&runtime, 0.01).unwrap();
    let result = scripts.tick_blocking(&runtime, 0.25, scripts::Input { values: vec![8] }).unwrap();
    assert_eq!(result.total, 10);
    // The synchronous entry points carry the same typed input and output, and finish a
    // landline round trip under block_on when the body has to wait for it.
    scripts.fixed_tick_sync(&runtime, 0.01).unwrap();
    let result = scripts.tick_sync(&runtime, 0.25, scripts::Input { values: vec![1, 2] }).unwrap();
    assert_eq!(result.total, 6, "sum of the batch plus the tick number, which is 3 by now");
    scripts.shutdown_blocking(&runtime).unwrap();
}
"#);
    assert_eq!(output.trim(), "fixed\nfixed\nfixed");
}

#[test]
fn engine_pumped_events_and_deterministic_replays() {
    let root = root("deterministic_events");
    build(&root, r#"
host world { fn record(n: int) }
on hit(n: int) { world.record(n) sleep(10) world.record(n + 100) }
on_tick(dt: float) { world.record(0) }
"#);
    let output = host(&root, r#"
use std::sync::{Arc, Mutex};
struct Host(Arc<Mutex<Vec<i64>>>);
impl scripts::Host for Host { fn world_record(&self, n: i64) -> Result<(), String> { self.0.lock().unwrap().push(n); Ok(()) } }
fn run(runtime: &tokio::runtime::Runtime, sync: bool) -> Vec<i64> {
    let tick = |scripts: &mut scripts::Scripts, dt: f64| if sync { scripts.tick_sync(runtime, dt) } else { scripts.tick_blocking(runtime, dt) };
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let options = scripts::StartOptions { deterministic: true, ..Default::default() };
    let mut scripts = scripts::start_with_options(runtime.handle(), Host(recorded.clone()), options).unwrap();
    let untouched = Arc::new(Mutex::new(Vec::new()));
    let mut other = scripts::start_with_options(runtime.handle(), Host(untouched.clone()), scripts::StartOptions { deterministic: true, ..Default::default() }).unwrap();
    scripts.trigger_hit(3).unwrap();
    tick(&mut scripts, 0.001).unwrap();
    assert_eq!(*recorded.lock().unwrap(), vec![3,0]);
    tick(&mut other, 0.001).unwrap();
    assert_eq!(*untouched.lock().unwrap(), vec![0], "an event must reach only the instance it was fired on");
    other.shutdown_blocking(runtime).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(30));
    assert_eq!(*recorded.lock().unwrap(), vec![3,0]);
    scripts.trigger_hit(4).unwrap();
    tick(&mut scripts, 0.02).unwrap();
    tick(&mut scripts, 0.02).unwrap();
    scripts.shutdown_blocking(runtime).unwrap();
    let result = recorded.lock().unwrap().clone();
    result
}
fn main() {
    let a = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let b = tokio::runtime::Runtime::new().unwrap();
    let first = run(&a, false);
    assert_eq!(first, run(&a, false));
    assert_eq!(first, run(&b, false));
    // The synchronous path replays the same host-driven time and event order.
    assert_eq!(first, run(&a, true));
    assert_eq!(first, run(&b, true));
    assert_eq!(first, vec![3,0,103,4,0,104,0]);
    println!("replays match");
}
"#);
    assert_eq!(output.trim(), "replays match");
}

/// Deterministic mode, at length: the same tick and event sequence, replayed over 10,000
/// ticks on fresh instances, on a current-thread and a multithreaded runtime, through
/// `tick_blocking` and `tick_sync`, produces a byte-identical host-call trace. The trace
/// has to be interesting to mean anything: events fired by the host and by handlers,
/// handlers that sleep on host time across several ticks, and instance state.
#[test]
fn engine_deterministic_trace_is_byte_identical_over_ten_thousand_ticks() {
    let root = root("deterministic_trace");
    build(&root, r#"
host world { fn record(tag: int, value: int) }
let ticks = 0
let fired = 0
on hit(n: int) {
    world.record(1, n)
    sleep(5)
    world.record(2, n + 100)
}
on pulse(a: int, b: int) {
    world.record(3, a * b)
    trigger hit(a + b)
}
on_tick(dt: float) {
    ticks = ticks + 1
    world.record(0, ticks)
    if ticks % 7 == 0 {
        fired = fired + 1
        trigger pulse(ticks, fired)
    }
}
orchestrator main() {}
"#);
    let output = host(&root, r#"
use std::sync::{Arc, Mutex};
struct Host(Arc<Mutex<Vec<u8>>>);
impl scripts::Host for Host {
    fn world_record(&self, tag: i64, value: i64) -> Result<(), String> {
        let mut trace = self.0.lock().unwrap();
        trace.extend_from_slice(&tag.to_le_bytes());
        trace.extend_from_slice(&value.to_le_bytes());
        Ok(())
    }
}
const TICKS: usize = 10_000;
fn run(runtime: &tokio::runtime::Runtime, sync: bool) -> Vec<u8> {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let options = scripts::StartOptions { deterministic: true, ..Default::default() };
    let mut scripts = scripts::start_with_options(runtime.handle(), Host(trace.clone()), options).unwrap();
    scripts.ready_blocking(runtime).unwrap();
    for i in 0..TICKS {
        // A scripted, replayable sequence: events on some ticks, a varying dt on all.
        if i % 3 == 0 { scripts.trigger_hit(i as i64).unwrap(); }
        if i % 11 == 0 { scripts.trigger_pulse(i as i64, 5).unwrap(); }
        let dt = 0.001 + (i % 5) as f64 * 0.001;
        if sync { scripts.tick_sync(runtime, dt).unwrap(); } else { scripts.tick_blocking(runtime, dt).unwrap(); }
    }
    // A few quiet ticks, so the last handlers' sleeps finish on host time before the end.
    for _ in 0..10 {
        if sync { scripts.tick_sync(runtime, 0.005).unwrap(); } else { scripts.tick_blocking(runtime, 0.005).unwrap(); }
    }
    scripts.shutdown_blocking(runtime).unwrap();
    let result = trace.lock().unwrap().clone();
    result
}
fn main() {
    let single = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let multi = tokio::runtime::Runtime::new().unwrap();
    let reference = run(&single, false);
    assert_eq!(reference, run(&single, false), "a second instance on the same runtime diverged");
    assert_eq!(reference, run(&single, true), "tick_sync diverged from tick_blocking");
    assert_eq!(reference, run(&multi, false), "the multithreaded runtime diverged");
    assert_eq!(reference, run(&multi, true), "tick_sync on the multithreaded runtime diverged");
    // The trace must actually contain what the program can do: tick records, host-fired
    // and handler-fired events, and sleeps that completed on host time.
    let records: Vec<(i64, i64)> = reference.chunks_exact(16).map(|c| (
        i64::from_le_bytes(c[..8].try_into().unwrap()), i64::from_le_bytes(c[8..].try_into().unwrap()),
    )).collect();
    let count = |tag: i64| records.iter().filter(|(t, _)| *t == tag).count();
    assert_eq!(count(0), TICKS + 10, "one tick record per tick");
    assert!(count(1) > 3_000 && count(2) == count(1), "every hit slept and finished on host time: {} started, {} finished", count(1), count(2));
    assert!(count(3) > 1_000, "pulses were handled: {}", count(3));
    println!("trace {} bytes, {} records", reference.len(), records.len());
}
"#);
    assert!(output.starts_with("trace "), "{output}");
    println!("{}", output.trim());
}

/// The documented exclusions from deterministic mode fail the way the documentation says:
/// a spawned worker, a serverlet, a landline, and a `sleep` outside an event handler each
/// stop the library with the deterministic-mode message. On the coordinator's task the
/// panic surfaces to the host as an error from `ready` or `tick`; under `tick_sync` the
/// body runs on the host's own thread, so the panic reaches that thread.
#[test]
fn engine_deterministic_mode_refuses_what_it_excludes() {
    let programs = [
        ("worker", "host world { fn record(n: int) }\nlet worker = automatic { world.record(1) }\non_tick(dt: float) { }\norchestrator main(procs: process[worker]) {}\n", "startup"),
        ("serverlet", "host world { fn record(n: int) }\nserverlet Counter { on add(n: int) -> int { return n } }\nlet counter = start Counter()\non_tick(dt: float) { world.record(counter.add(1)) }\norchestrator main() {}\n", "startup"),
        ("landline", "host world { fn record(n: int) }\nserverlet P via python(source: \"impl.py\") { on ping() -> int }\nlet p = start P()\non_tick(dt: float) { world.record(p.ping()) }\norchestrator main() {}\n", "startup"),
        ("sleep", "host world { fn record(n: int) }\non_tick(dt: float) { sleep(1) world.record(1) }\norchestrator main() {}\n", "tick"),
    ];
    for (name, source, failure) in programs {
        let root = root(&format!("deterministic_excludes_{name}"));
        fs::write(root.join("impl.py"), "from orchestratelang import landline\nclass P(landline.Serverlet):\n    def ping(self) -> int: return 1\nlandline.serve(P)\n").unwrap();
        build(&root, source);
        let output = host(&root, &format!(r#"
use std::sync::{{Arc, Mutex}};
struct Host;
impl scripts::Host for Host {{ fn world_record(&self, _: i64) -> Result<(), String> {{ Ok(()) }} }}
fn main() {{
    let panics: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let seen = panics.clone();
    std::panic::set_hook(Box::new(move |info| {{ seen.lock().unwrap().push(info.to_string()); }}));
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let options = scripts::StartOptions {{ deterministic: true, ..Default::default() }};
    let failure = "{failure}";
    // The channel path: the panic is on the coordinator's task and reaches the host as an error.
    let mut scripts = scripts::start_with_options(runtime.handle(), Host, options.clone()).unwrap();
    let ready = scripts.ready_blocking(&runtime);
    let tick = scripts.tick_blocking(&runtime, 0.016);
    if failure == "startup" {{
        assert_eq!(ready.unwrap_err(), "library startup task failed");
    }} else {{
        ready.unwrap();
        assert_eq!(tick.unwrap_err(), "tick task failed");
    }}
    drop(scripts);
    // The synchronous path for a tick-time failure: the body runs on this thread.
    if failure == "tick" {{
        let mut scripts = scripts::start_with_options(runtime.handle(), Host, options).unwrap();
        scripts.ready_blocking(&runtime).unwrap();
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| scripts.tick_sync(&runtime, 0.016)));
        assert!(caught.is_err(), "tick_sync must panic on the calling thread");
        drop(scripts);
    }}
    let panics = panics.lock().unwrap();
    assert!(panics.iter().any(|p| p.contains("deterministic")), "expected the deterministic-mode message, got {{panics:?}}");
    println!("refused as documented: {{}}", panics.iter().find(|p| p.contains("deterministic")).unwrap().lines().last().unwrap_or(""));
}}
"#));
        assert!(output.starts_with("refused as documented"), "{name}: {output}");
        println!("{name}: {}", output.trim());
    }
}

#[test]
fn engine_host_logs_and_fast_shutdown() {
    let root = root("logs_shutdown");
    fs::write(root.join("impl.py"), "import time\nfrom orchestratelang import landline\nclass P(landline.Serverlet):\n    def busy(self) -> int:\n        print('python message')\n        time.sleep(10)\n        return 1\nlandline.serve(P)\n").unwrap();
    build(&root, r#"
host world { fn fail() }
serverlet P via python(source: "impl.py", budget: "20ms") { on busy() -> int }
let p = start P()
on_start { print("startup") world.fail() }
on_tick(dt: float) { p.busy() }
"#);
    let output = host(&root, r#"
use std::sync::{Arc, Mutex};
struct Host(Arc<Mutex<Vec<String>>>);
impl scripts::Host for Host {
    fn world_fail(&self) -> Result<(), String> { Err("host failure".into()) }
    fn log(&self, _: scripts::LogLevel, message: &str) { self.0.lock().unwrap().push(message.to_owned()); }
}
fn main() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let logs = Arc::new(Mutex::new(Vec::new()));
    let options = scripts::StartOptions { shutdown_grace: std::time::Duration::ZERO, ..Default::default() };
    let mut scripts = scripts::start_with_options(runtime.handle(), Host(logs.clone()), options).unwrap();
    scripts.ready_blocking(&runtime).unwrap();
    // Give the Python startup handshake time to finish before measuring a busy call.
    runtime.block_on(async { tokio::time::sleep(std::time::Duration::from_millis(200)).await; });
    let tick = std::time::Instant::now();
    scripts.tick_blocking(&runtime, 0.1).unwrap();
    assert!(tick.elapsed() < std::time::Duration::from_millis(100));
    let shutdown = std::time::Instant::now();
    scripts.shutdown_blocking(&runtime).unwrap();
    assert!(shutdown.elapsed() < std::time::Duration::from_millis(100));
    let messages = logs.lock().unwrap();
    assert!(messages.iter().any(|s| s == "startup"));
    assert!(messages.iter().any(|s| s.contains("host failure")));
}
"#);
    assert!(output.is_empty(), "{}", output);
}

#[test]
fn engine_installed_layout_stdlib_and_target_children() {
    let root = root("installed_stdlib");
    let compiler = root.join(format!("orchestrate{}", std::env::consts::EXE_SUFFIX));
    fs::copy(env!("CARGO_BIN_EXE_orchestrate"), &compiler).unwrap();
    fs::write(root.join("main.orch"), "use module lists: \"lists\"\norchestrator main() { print(to_string(lists.sum([2,3]))) stop_orch() }\n").unwrap();
    let result = Command::new(&compiler).arg("run").arg(root.join("main.orch")).current_dir(&root).output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert!(String::from_utf8_lossy(&result.stdout).lines().any(|s| s == "5"));
    fs::write(root.join("main.orch"), "serverlet C secret { on value() -> int { return 1 } }\n").unwrap();
    let rustc = Command::new("rustc").arg("-vV").output().unwrap();
    let version = String::from_utf8_lossy(&rustc.stdout);
    let target = version.lines().find_map(|line| line.strip_prefix("host: ")).unwrap();
    let result = Command::new(&compiler).args(["build", "--lib", "--target", target]).arg(root.join("main.orch")).arg("-o").arg(root.join("scripts")).output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert!(root.join(".orch_cache/library/target").join(target).join(format!("debug/secret_C{}", std::env::consts::EXE_SUFFIX)).is_file());
}

#[test]
fn engine_current_thread_pumps_workers_events_and_python() {
    let root = root("pumped_runtime");
    fs::write(root.join("impl.py"), "from orchestratelang import landline\nclass P(landline.Serverlet):\n    def ping(self) -> int: return 42\nlandline.serve(P)\n").unwrap();
    build(&root, r#"
host world { fn record(n: int) }
serverlet P via python(source: "impl.py") { on ping() -> int }
let p = start P()
let worker = automatic { world.record(9) sleep(5) }
on hit(n: int) { world.record(n) }
on_tick(dt: float) { world.record(p.ping()) sleep(20) }
orchestrator main(workers: process[worker]) {}
"#);
    let output = host(&root, r#"
use std::sync::{Arc, Mutex};
struct Host(Arc<Mutex<Vec<i64>>>);
impl scripts::Host for Host { fn world_record(&self, n: i64) -> Result<(), String> { self.0.lock().unwrap().push(n); Ok(()) } }
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut scripts = scripts::start(runtime.handle(), Host(log.clone())).unwrap();
    scripts.trigger_hit(3).unwrap();
    assert!(log.lock().unwrap().is_empty());
    scripts.tick_blocking(&runtime, 0.1).unwrap();
    let first = log.lock().unwrap().clone();
    assert!(first.contains(&3) && first.contains(&42) && first.contains(&9));
    std::thread::sleep(std::time::Duration::from_millis(40));
    assert_eq!(*log.lock().unwrap(), first);
    scripts.trigger_hit(4).unwrap();
    assert_eq!(*log.lock().unwrap(), first);
    scripts.tick_blocking(&runtime, 0.1).unwrap();
    assert!(log.lock().unwrap().contains(&4));
    scripts.shutdown_blocking(&runtime).unwrap();
}
"#);
    assert!(output.is_empty());
}

/// Generated code must be a pure function of the program: a host that depends on the
/// crate rebuilds it whenever a file changes, so any order that varies between runs
/// defeats every build cache above the compiler. Events and captured variables are the
/// two places a set used to reach the output.
#[test]
fn library_codegen_is_byte_identical_across_builds() {
    let root = root("deterministic_codegen");
    fs::create_dir_all(root.join("more")).unwrap();
    fs::write(root.join("more/module.orch"), "fn total(x: int, y: int) -> int { x + y }\ntask slow(n: int) -> int { sleep(1)\n return n }\n").unwrap();
    fs::write(root.join("main.orch"), r#"
use module more: "./more"
host world { fn record(n: int) }
let a = 1
let b = 2
let c = 3
let worker = automatic { world.record(a + b + c) sleep(50) }
on alpha(n: int) { world.record(n + a + b) }
on beta(n: int) { world.record(n + b + c) }
on gamma(n: int) { world.record(n + a + c) }
on delta(n: int) { world.record(n + a) }
on epsilon(n: int) { world.record(n + b) }
on zeta(n: int) { world.record(n + c) }
on_tick(dt: float) { trigger alpha(more.total(a, b)) }
orchestrator main(workers: process[worker]) {}
"#).unwrap();
    fn generate(root: &Path, output: &str) -> std::collections::BTreeMap<String, Vec<u8>> {
        let result = Command::new(env!("CARGO_BIN_EXE_orchestrate"))
            .args(["build", "--lib"]).arg(root.join("main.orch")).arg("-o").arg(root.join(output))
            .output().unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
        fs::read_dir(root.join(output).join("src")).unwrap().map(|entry| {
            let path = entry.unwrap().path();
            (path.file_name().unwrap().to_string_lossy().into_owned(), fs::read(&path).unwrap())
        }).collect()
    }
    let first = generate(&root, "scripts_a");
    let second = generate(&root, "scripts_b");
    assert!(first.contains_key("lib.rs") && first.contains_key("more.rs"));
    assert_eq!(first.keys().collect::<Vec<_>>(), second.keys().collect::<Vec<_>>());
    for (name, bytes) in &first {
        assert!(second[name] == *bytes, "{name} differs between two builds of the same program");
    }
}

#[test]
fn engine_io_driver_required_by_landline_in_module() {
    let root = root("io_driver_needed");
    fs::create_dir_all(root.join("svc")).unwrap();
    fs::write(root.join("svc/module.orch"), "serverlet Probe via python(source: \"impl.py\") { on ping() -> int }\n").unwrap();
    fs::write(root.join("svc/impl.py"), "from orchestratelang import landline\nclass Probe(landline.Serverlet):\n    def ping(self) -> int: return 42\nlandline.serve(Probe)\n").unwrap();
    build(&root, r#"
use module svc: "./svc"
host world { fn record(n: int) }
let probe = start svc.Probe()
on_tick(dt: float) { world.record(probe.ping()) }
orchestrator main() {}
"#);
    let output = host(&root, r#"
use std::sync::{Arc, Mutex};
struct Host(Arc<Mutex<Vec<i64>>>);
impl scripts::Host for Host { fn world_record(&self, n: i64) -> Result<(), String> { self.0.lock().unwrap().push(n); Ok(()) } }
fn main() {
    assert!(scripts::NEEDS_IO_DRIVER);
    let timers_only = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let error = scripts::start(timers_only.handle(), Host(Default::default())).err().expect("start must fail without the IO driver");
    assert!(error.contains("IO driver") && error.contains("python landline serverlet 'Probe'"), "{error}");
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut scripts = scripts::start(runtime.handle(), Host(log.clone())).unwrap();
    scripts.tick_blocking(&runtime, 0.1).unwrap();
    assert_eq!(*log.lock().unwrap(), vec![42]);
    scripts.shutdown_blocking(&runtime).unwrap();
}
"#);
    assert!(output.is_empty());
}

#[test]
fn engine_time_only_runtime_runs_in_process_program() {
    let root = root("io_driver_not_needed");
    build(&root, r#"
host world { fn record(n: int) }
let worker = automatic { world.record(7) sleep(5) }
on_tick(dt: float) { world.record(1) }
orchestrator main(workers: process[worker]) {}
"#);
    let output = host(&root, r#"
use std::sync::{Arc, Mutex};
struct Host(Arc<Mutex<Vec<i64>>>);
impl scripts::Host for Host { fn world_record(&self, n: i64) -> Result<(), String> { self.0.lock().unwrap().push(n); Ok(()) } }
fn main() {
    assert!(!scripts::NEEDS_IO_DRIVER);
    let bare = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let error = scripts::start(bare.handle(), Host(Default::default())).err().expect("start must fail without the time driver");
    assert!(error.contains("time driver"), "{error}");
    let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut scripts = scripts::start(runtime.handle(), Host(log.clone())).unwrap();
    scripts.tick_blocking(&runtime, 0.1).unwrap();
    assert!(log.lock().unwrap().contains(&1) && log.lock().unwrap().contains(&7));
    scripts.shutdown_blocking(&runtime).unwrap();
}
"#);
    assert!(output.is_empty());
}

/// A handle held in program state keeps its native object alive across ticks and
/// releases it when the instance shuts down.
#[test]
fn library_handle_in_program_state_lives_until_shutdown() {
    let root = root("handle_state");
    fs::create_dir_all(root.join("native")).unwrap();
    fs::write(root.join("native/module.orch"), "load_foreign \"c\" \"native.c\"\n").unwrap();
    fs::write(root.join("native/native.c"), "#include <stdlib.h>\n#include <stdio.h>\nstruct counter { long long value; };\nvoid* make_counter(long long start) { struct counter* c = malloc(sizeof *c); c->value = start; return c; }\nlong long bump(void* c) { struct counter* k = c; k->value += 1; return k->value; }\nvoid release_counter(void* c) { printf(\"released %lld\\n\", ((struct counter*)c)->value); fflush(stdout); free(c); }\n").unwrap();
    fs::write(root.join("native/native.orch_ffi"), "make_counter(initial: int) -> handle\nbump(c: handle) -> int\ndrop release_counter(c: handle)\n").unwrap();
    build(&root, r#"
use module native: "./native"
host world { fn record(n: int) }
let counter = native.make_counter(5)
on_tick(dt: float) { world.record(native.bump(counter)) }
orchestrator main() {}
"#);
    let output = host(&root, r#"
use std::sync::{Arc, Mutex};
struct Host(Arc<Mutex<Vec<i64>>>);
impl scripts::Host for Host { fn world_record(&self, n: i64) -> Result<(), String> { self.0.lock().unwrap().push(n); Ok(()) } }
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut scripts = scripts::start(runtime.handle(), Host(log.clone())).unwrap();
    scripts.tick_blocking(&runtime, 0.1).unwrap();
    scripts.tick_sync(&runtime, 0.1).unwrap();
    assert_eq!(*log.lock().unwrap(), vec![6, 7]);
    scripts.shutdown_blocking(&runtime).unwrap();
    println!("shut down");
}
"#);
    assert_eq!(output.trim(), "released 7\nshut down");
}

/// A Rust foreign module declares the crates it needs in its sidecar, and a host can add
/// more on the command line; the same crate declared twice must be identical.
#[test]
fn library_rust_dependencies_from_sidecar_and_cli() {
    let root = root("rust_dependencies");
    fs::create_dir_all(root.join("sdk/src")).unwrap();
    fs::write(root.join("sdk/Cargo.toml"), "[package]\nname = \"sdk\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
    fs::write(root.join("sdk/src/lib.rs"), "pub fn triple(n: i64) -> i64 { n * 3 }\n").unwrap();
    fs::create_dir_all(root.join("native")).unwrap();
    fs::write(root.join("native/module.orch"), "load_foreign \"rust\" \"impl.rs\"\n").unwrap();
    fs::write(root.join("native/impl.orch_ffi"), "triple(n: int) -> int\n\n[dependencies]\nsdk = { path = \"../sdk\" }\n").unwrap();
    fs::write(root.join("native/impl.rs"), "pub fn triple(n: i64) -> i64 { sdk::triple(n) }\n").unwrap();
    fs::write(root.join("main.orch"), "use module native: \"./native\"\nhost world { fn record(n: int) }\non_tick(dt: float) { world.record(native.triple(2)) }\norchestrator main() {}\n").unwrap();
    fs::write(root.join("deps.toml"), "[dependencies]\nsdk = { path = \"sdk\" }\n").unwrap();
    // The fragment names the same crate at the same directory, resolved from the working
    // directory rather than the sidecar, so it merges instead of conflicting.
    let result = Command::new(env!("CARGO_BIN_EXE_orchestrate"))
        .args(["build", "--lib"]).arg(root.join("main.orch")).arg("-o").arg(root.join("scripts"))
        .arg("--dependencies").arg(root.join("deps.toml"))
        .current_dir(&root).output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    let manifest = fs::read_to_string(root.join("scripts/Cargo.toml")).unwrap();
    let sdk = fs::canonicalize(root.join("sdk")).unwrap();
    let expected = format!("sdk = {{ path = {:?} }}", sdk.to_string_lossy());
    assert_eq!(manifest.matches("sdk = ").count(), 1, "{manifest}");
    assert!(manifest.contains(&expected), "{manifest}");
    let output = host(&root, r#"
use std::sync::{Arc, Mutex};
struct Host(Arc<Mutex<Vec<i64>>>);
impl scripts::Host for Host { fn world_record(&self, n: i64) -> Result<(), String> { self.0.lock().unwrap().push(n); Ok(()) } }
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut scripts = scripts::start(runtime.handle(), Host(log.clone())).unwrap();
    scripts.tick_blocking(&runtime, 0.1).unwrap();
    assert_eq!(*log.lock().unwrap(), vec![6]);
    scripts.shutdown_blocking(&runtime).unwrap();
}
"#);
    assert!(output.is_empty());
    let conflict = Command::new(env!("CARGO_BIN_EXE_orchestrate"))
        .args(["build", "--lib"]).arg(root.join("main.orch")).arg("-o").arg(root.join("scripts"))
        .arg("--dependency").arg("sdk = \"1.0\"")
        .current_dir(&root).output().unwrap();
    assert!(!conflict.status.success());
    let stderr = String::from_utf8_lossy(&conflict.stderr);
    assert!(stderr.contains("dependency 'sdk'") && stderr.contains("must be identical"), "{stderr}");
    let refused = Command::new(env!("CARGO_BIN_EXE_orchestrate"))
        .args(["build"]).arg(root.join("main.orch")).arg("--dependency").arg("sdk = \"1.0\"")
        .current_dir(&root).output().unwrap();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("apply to build --lib"));
}

/// State across ticks, events queued during a tick, a serverlet round trip the body has
/// to wait for, a stop requested mid-tick, and on_stop reading the final state: the same
/// program observed through the channel path and through tick_sync.
#[test]
fn engine_sync_tick_matches_channel_tick() {
    let root = root("sync_tick_parity");
    build(&root, r#"
host world { fn record(n: int) }
serverlet Counter {
    let total = 0
    on add(n: int) -> int {
        total = total + n
        return total
    }
}
let ticks = 0
let counter = start Counter()
on hit(n: int) { world.record(n * 10) }
on_tick(dt: float) {
    ticks = ticks + 1
    trigger hit(ticks)
    world.record(counter.add(1))
    if ticks == 3 { stop_orch() }
}
on_stop { world.record(ticks * 100) }
orchestrator main() {}
"#);
    let output = host(&root, r#"
use std::sync::{Arc, Mutex};
struct Host(Arc<Mutex<Vec<i64>>>);
impl scripts::Host for Host { fn world_record(&self, n: i64) -> Result<(), String> { self.0.lock().unwrap().push(n); Ok(()) } }
fn drive(runtime: &tokio::runtime::Runtime, sync: bool) -> Vec<i64> {
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut scripts = scripts::start(runtime.handle(), Host(log.clone())).unwrap();
    let mut outcomes = Vec::new();
    for _ in 0..4 {
        let result = if sync { scripts.tick_sync(runtime, 0.1) } else { scripts.tick_blocking(runtime, 0.1) };
        outcomes.push(result.is_ok());
    }
    assert_eq!(outcomes, [true, true, true, false], "the tick after stop_orch fails on both paths");
    scripts.shutdown_blocking(runtime).unwrap();
    let recorded = log.lock().unwrap().clone();
    recorded
}
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let channel = drive(&runtime, false);
    let sync = drive(&runtime, true);
    assert_eq!(channel, vec![1, 10, 2, 20, 3, 30, 300]);
    assert_eq!(sync, channel);
}
"#);
    assert!(output.is_empty());
}

/// The synchronous tick skips the coordinator task, the command channel, the reply, and
/// the park that block_on costs per frame.
#[test]
fn engine_sync_tick_is_cheaper_than_the_channel() {
    let root = root("sync_tick_cost");
    build(&root, r#"
host world { fn count() }
let ticks = 0
on_tick(dt: float) {
    ticks = ticks + 1
    world.count()
}
orchestrator main() {}
"#);
    let output = host(&root, r#"
use std::sync::atomic::{AtomicU64, Ordering};
struct Host(std::sync::Arc<AtomicU64>);
impl scripts::Host for Host { fn world_count(&self) -> Result<(), String> { self.0.fetch_add(1, Ordering::Relaxed); Ok(()) } }
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let count = std::sync::Arc::new(AtomicU64::new(0));
    let mut scripts = scripts::start(runtime.handle(), Host(count.clone())).unwrap();
    scripts.ready_blocking(&runtime).unwrap();
    const N: u64 = 20_000;
    let mut measure = |sync: bool| {
        let mut best = u128::MAX;
        for _ in 0..5 {
            let start = std::time::Instant::now();
            for _ in 0..N {
                if sync { scripts.tick_sync(&runtime, 0.016).unwrap(); } else { scripts.tick_blocking(&runtime, 0.016).unwrap(); }
            }
            best = best.min(start.elapsed().as_nanos() / N as u128);
        }
        best
    };
    let channel = measure(false);
    let sync = measure(true);
    scripts.shutdown_blocking(&runtime).unwrap();
    assert_eq!(count.load(Ordering::Relaxed), N * 10);
    println!("channel {channel} sync {sync}");
    assert!(sync < channel, "a sync tick ({sync} ns) must cost less than a channel tick ({channel} ns)");
}
"#);
    assert!(output.starts_with("channel "), "{output}");
    println!("{}", output.trim());
}

/// The cost of the generated glue around `tick_sync`, against a no-op host: an empty tick,
/// one host call, and five, each the median of 21 rounds, next to the host method called
/// through its vtable alone. A benchmark rather than a check, so it is ignored:
/// `cargo test --test library_tests glue_cost -- --ignored --nocapture`.
#[test]
#[ignore]
fn bench_sync_tick_glue_cost() {
    const HOST: &str = r#"
use std::sync::atomic::{AtomicU64, Ordering};
struct Host(AtomicU64);
impl scripts::Host for Host {
    fn world_count(&self) -> Result<(), String> { self.0.fetch_add(1, Ordering::Relaxed); Ok(()) }
}
fn median(mut samples: Vec<f64>) -> f64 { samples.sort_by(|a, b| a.partial_cmp(b).unwrap()); samples[samples.len() / 2] }
fn main() {
    const N: u64 = 200_000;
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut scripts = scripts::start(runtime.handle(), Host(AtomicU64::new(0))).unwrap();
    scripts.ready_blocking(&runtime).unwrap();
    // Warmed up first, so the clock ramp does not land on whichever program runs first.
    for _ in 0..N * 5 { scripts.tick_sync(&runtime, 0.016).unwrap(); }
    let mut ticks = Vec::new();
    for _ in 0..21 {
        let start = std::time::Instant::now();
        for _ in 0..N { scripts.tick_sync(&runtime, 0.016).unwrap(); }
        ticks.push(start.elapsed().as_nanos() as f64 / N as f64);
    }
    scripts.shutdown_blocking(&runtime).unwrap();
    // The trait method through the same vtable is the floor for a host call.
    let host: std::sync::Arc<dyn scripts::Host> = std::hint::black_box(std::sync::Arc::new(Host(AtomicU64::new(0))));
    for _ in 0..N * 5 { std::hint::black_box(host.world_count()).unwrap(); }
    let mut calls = Vec::new();
    for _ in 0..21 {
        let start = std::time::Instant::now();
        for _ in 0..N { std::hint::black_box(host.world_count()).unwrap(); }
        calls.push(start.elapsed().as_nanos() as f64 / N as f64);
    }
    println!("{:.2} {:.2}", median(ticks), median(calls));
}
"#;
    let mut results = Vec::new();
    for (name, body) in [
        ("empty", ""),
        ("one host call", "world.count()"),
        ("five host calls", "world.count() world.count() world.count() world.count() world.count()"),
    ] {
        let root = root(&format!("glue_{}", name.replace(' ', "_")));
        build(&root, &format!("host world {{ fn count() }}\non_tick(dt: float) {{ {body} }}\norchestrator main() {{}}\n"));
        let output = host_in(&root, HOST, true);
        let mut numbers = output.split_whitespace().map(|n| n.parse::<f64>().unwrap());
        let (tick, method) = (numbers.next().unwrap(), numbers.next().unwrap());
        println!("{name:<16} tick_sync {tick:7.2} ns    host method alone {method:5.2} ns");
        results.push((tick, method));
    }
    let (empty, one, five, method) = (results[0].0, results[1].0, results[2].0, results[1].1);
    let per_call = (five - one) / 4.0;
    println!(
        "empty tick {empty:.2} ns; one call adds {:.2} ns; each further call {per_call:.2} ns, {:.2} ns over the method",
        one - empty,
        per_call - method
    );
}

/// A synchronous tick's body runs on the caller's thread, outside any runtime context,
/// and what it does on its first poll — spawn a worker, queue an event, start a budgeted
/// landline call, or sleep — still works, because the library carries the handle it was
/// started with. The host never enters the runtime itself.
#[test]
fn engine_sync_tick_body_needs_no_runtime_context() {
    let root = root("sync_no_context");
    fs::write(root.join("impl.py"), "from orchestratelang import landline\nclass P(landline.Serverlet):\n    def ping(self) -> int: return 42\nlandline.serve(P)\n").unwrap();
    build(&root, r#"
host world { fn record(n: int) }
serverlet P via python(source: "impl.py", budget: "2s") { on ping() -> int }
let worker = automatic { world.record(9) sleep(1000) }
let p = start P()
let ticks = 0
on hit(n: int) { world.record(n) }
on_tick(dt: float) {
    ticks = ticks + 1
    if ticks == 1 { start worker trigger hit(3) world.record(1) }
    if ticks == 2 { world.record(p.ping()) }
    if ticks == 3 { sleep(1) world.record(5) }
}
orchestrator main() {}
"#);
    let output = host(&root, r#"
use std::sync::{Arc, Mutex};
struct Host(Arc<Mutex<Vec<i64>>>);
impl scripts::Host for Host { fn world_record(&self, n: i64) -> Result<(), String> { self.0.lock().unwrap().push(n); Ok(()) } }
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut scripts = scripts::start(runtime.handle(), Host(log.clone())).unwrap();
    scripts.ready_blocking(&runtime).unwrap();
    assert!(tokio::runtime::Handle::try_current().is_err(), "the host stays outside the runtime");
    for _ in 0..3 { scripts.tick_sync(&runtime, 0.1).unwrap(); }
    let recorded = log.lock().unwrap().clone();
    assert_eq!(&recorded[..2], [1, 3], "the first tick finished on the calling thread: {recorded:?}");
    assert!(recorded.contains(&9) && recorded.contains(&42), "the worker and the landline ran under the fallback: {recorded:?}");
    assert_eq!(recorded.last(), Some(&5), "{recorded:?}");
    assert_eq!(recorded.len(), 5, "{recorded:?}");
    scripts.shutdown_blocking(&runtime).unwrap();
}
"#);
    assert!(output.is_empty(), "{output}");
}

/// A sandboxed serverlet under a Rust host: the guest is staged into the generated crate,
/// embedded, and driven through `tick_sync`, with its state living inside the guest across
/// ticks the way it does for a standalone program.
#[test]
fn library_sandboxed_serverlet_runs_under_a_host() {
    let root = root("sandbox_library");
    build(&root, r#"
host world { fn record(n: int) }
serverlet Plugin sandbox(memory_limit: "16mb", timeout: "300ms") {
    let total = 0
    on tally(n: int) -> int {
        total = total + n
        return total
    }
}
let p = start Plugin()
on_tick(dt: float) { world.record(p.tally(2)) }
orchestrator main() {}
"#);
    // The guest has to travel with the crate, not be left behind in the build cache.
    assert!(root.join("scripts/src/sandbox_Plugin.wasm").is_file(), "the guest was not staged into the crate");
    let output = host(&root, r#"
use std::sync::{Arc, Mutex};
struct Host(Arc<Mutex<Vec<i64>>>);
impl scripts::Host for Host { fn world_record(&self, n: i64) -> Result<(), String> { self.0.lock().unwrap().push(n); Ok(()) } }
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let mut scripts = scripts::start(runtime.handle(), Host(log.clone())).unwrap();
    scripts.ready_blocking(&runtime).unwrap();
    for _ in 0..3 { scripts.tick_sync(&runtime, 0.1).unwrap(); }
    scripts.shutdown_blocking(&runtime).unwrap();
    // 2, 4, 6 rather than 2, 2, 2: the guest kept its state between ticks.
    assert_eq!(*log.lock().unwrap(), vec![2, 4, 6]);
    println!("sandboxed serverlet ran under the host");
}
"#);
    assert!(output.contains("sandboxed serverlet ran under the host"), "{output}");
}
