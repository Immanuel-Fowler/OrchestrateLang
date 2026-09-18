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
    fs::create_dir_all(root.join("host/src")).unwrap();
    fs::write(root.join("host/Cargo.toml"), "[package]\nname=\"host_test\"\nversion=\"0.1.0\"\nedition=\"2021\"\n[dependencies]\nscripts={path=\"../scripts\"}\ntokio={version=\"1.35\",features=[\"full\"]}\n").unwrap();
    fs::write(root.join("host/src/main.rs"), source).unwrap();
    let result = Command::new("cargo")
        .args(["run", "--quiet"])
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
    scripts.shutdown_blocking(&runtime).unwrap();
}
"#);
    assert_eq!(output.trim(), "fixed\nfixed");
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
fn run(runtime: &tokio::runtime::Runtime) -> Vec<i64> {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let options = scripts::StartOptions { deterministic: true, ..Default::default() };
    let mut scripts = scripts::start_with_options(runtime.handle(), Host(recorded.clone()), options).unwrap();
    let untouched = Arc::new(Mutex::new(Vec::new()));
    let mut other = scripts::start_with_options(runtime.handle(), Host(untouched.clone()), scripts::StartOptions { deterministic: true, ..Default::default() }).unwrap();
    scripts.trigger_hit(3).unwrap();
    scripts.tick_blocking(runtime, 0.001).unwrap();
    assert_eq!(*recorded.lock().unwrap(), vec![3,0]);
    other.tick_blocking(runtime, 0.001).unwrap();
    assert_eq!(*untouched.lock().unwrap(), vec![0], "an event must reach only the instance it was fired on");
    other.shutdown_blocking(runtime).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(30));
    assert_eq!(*recorded.lock().unwrap(), vec![3,0]);
    scripts.trigger_hit(4).unwrap();
    scripts.tick_blocking(runtime, 0.02).unwrap();
    scripts.tick_blocking(runtime, 0.02).unwrap();
    scripts.shutdown_blocking(runtime).unwrap();
    let result = recorded.lock().unwrap().clone();
    result
}
fn main() {
    let a = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let b = tokio::runtime::Runtime::new().unwrap();
    let first = run(&a);
    assert_eq!(first, run(&a));
    assert_eq!(first, run(&b));
    assert_eq!(first, vec![3,0,103,4,0,104,0]);
    println!("replays match");
}
"#);
    assert_eq!(output.trim(), "replays match");
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
