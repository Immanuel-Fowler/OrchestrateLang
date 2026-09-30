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

/// A top-level client is usable from a hook, an event handler, a worker, and the
/// orchestrator body, and every call reaches the one serverlet. A handler writes
/// instance state — a number and an array — inside the tick that drains it, and the next
/// tick reads what it wrote; `on_start` written in the orchestrator body writes it too,
/// and a hook copies state into a local without taking it from the instance. Each of
/// these reached rustc before (E0507, E0525, E0594, E0425).
#[test]
fn library_clients_and_instance_state_from_every_context() {
    let root = root("clients_everywhere");
    build(&root, r#"
host world { fn record(n: int) }
serverlet Counter {
    let n = 0
    on bump(k: int) -> int {
        n = n + k
        return n
    }
}
let counter = start Counter()
let count = 0
let names: string[] = []
let seed = 0
on hit(k: int) {
    count = count + k
    append(names, "hit")
    world.record(counter.bump(k))
}
let worker = automatic {
    counter.bump(100)
    sleep(100000)
}
on_tick(dt: float) {
    let copy = names
    world.record(count)
    world.record(length(copy))
    world.record(seed)
    trigger hit(1)
}
orchestrator main(procs: process[worker]) {
    on_start { seed = 7 }
    counter.bump(1000)
}
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
    // The worker and the orchestrator body run beside the ticks; let both call first.
    runtime.block_on(async { tokio::time::sleep(std::time::Duration::from_millis(200)).await });
    for _ in 0..3 { scripts.tick_sync(&runtime, 0.016).unwrap(); }
    scripts.shutdown_blocking(&runtime).unwrap();
    println!("{:?}", recorded.lock().unwrap());
}
"#);
    assert_eq!(output.trim(), "[0, 0, 7, 1101, 1, 1, 7, 1102, 2, 2, 7, 1103]");
}

/// A sandboxed serverlet declared in a module may grant a host function of the program
/// that imports it. `check` and `check --lib` refused that in 0.15.1 while the build
/// accepted it; now all three accept it, and the grant works under a host.
#[test]
fn library_module_guest_grant_names_the_entry_files_host() {
    let root = root("module_guest_grant");
    fs::create_dir_all(root.join("gmod")).unwrap();
    fs::write(root.join("gmod/module.orch"), r#"
serverlet Guest sandbox(memory_limit: "8mb", timeout: "20ms") {
    grant call world.note
    on go(n: int) -> int {
        world.note(n)
        return n + 1
    }
}
"#).unwrap();
    let source = r#"
host world { fn note(n: int) }
use module g: "./gmod"
let guest = start g.Guest()
on_tick(dt: float) { world.note(guest.go(41)) }
orchestrator main() {}
"#;
    fs::write(root.join("main.orch"), source).unwrap();
    let check = Command::new(env!("CARGO_BIN_EXE_orchestrate")).args(["check", "--lib"]).arg(root.join("main.orch")).output().unwrap();
    assert!(check.status.success(), "{}", String::from_utf8_lossy(&check.stderr));
    build(&root, source);
    let output = host(&root, r#"
use std::sync::{Arc, Mutex};
struct Host(Arc<Mutex<Vec<i64>>>);
impl scripts::Host for Host { fn world_note(&self, n: i64) -> Result<(), String> { self.0.lock().unwrap().push(n); Ok(()) } }
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let noted = Arc::new(Mutex::new(Vec::new()));
    let mut scripts = scripts::start(runtime.handle(), Host(noted.clone())).unwrap();
    scripts.ready_blocking(&runtime).unwrap();
    scripts.tick_sync(&runtime, 0.016).unwrap();
    scripts.shutdown_blocking(&runtime).unwrap();
    println!("{:?}", noted.lock().unwrap());
}
"#);
    assert_eq!(output.trim(), "[41, 42]");
}

/// A hook swaps a library's worker set: `on_tick` names both workers from instance state,
/// which reached rustc (E0507) because the set moved them out of the program.
#[test]
fn library_tick_swaps_the_worker_set() {
    let root = root("tick_swaps_workers");
    build(&root, r#"
host world { fn record(n: int) }
let a = automatic { world.record(1)  sleep(10) }
let b = automatic { world.record(2)  sleep(10) }
let ticks = 0
on_tick(dt: float) {
    ticks = ticks + 1
    if ticks == 2 { trigger update_orchestrator([b]) }
}
orchestrator main(procs: process[a]) {}
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
    let pause = |ms| runtime.block_on(async move { tokio::time::sleep(std::time::Duration::from_millis(ms)).await });
    scripts.tick_sync(&runtime, 0.016).unwrap();
    pause(100);
    let before: Vec<i64> = recorded.lock().unwrap().drain(..).collect();
    scripts.tick_sync(&runtime, 0.016).unwrap();
    pause(50);
    recorded.lock().unwrap().clear();
    pause(100);
    let after: Vec<i64> = recorded.lock().unwrap().drain(..).collect();
    scripts.shutdown_blocking(&runtime).unwrap();
    println!("before: a {} b {}", before.contains(&1), before.contains(&2));
    println!("after: a {} b {}", after.contains(&1), after.contains(&2));
}
"#);
    assert_eq!(output.trim(), "before: a true b false\nafter: a false b true");
}

/// A host can have its events handled between ticks, without running a tick hook: before
/// `handle_events` an event waited for the next tick.
#[test]
fn library_events_are_handled_between_ticks() {
    let root = root("events_between_ticks");
    build(&root, r#"
host world { fn record(n: int) }
let total = 0
on hit(n: int) {
    total = total + n
    world.record(total)
}
on_tick(dt: float) { world.record(-1) }
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
    scripts.trigger_hit(3).unwrap();
    scripts.trigger_hit(4).unwrap();
    scripts.handle_events_blocking(&runtime).unwrap();
    println!("{:?}", recorded.lock().unwrap());
    scripts.tick_blocking(&runtime, 0.016).unwrap();
    scripts.shutdown_blocking(&runtime).unwrap();
    println!("{:?}", recorded.lock().unwrap());
}
"#);
    assert_eq!(output.trim(), "[3, 7]\n[3, 7, -1]");
}

/// A hook that panics under `tick_sync` does not unwind into the host's thread: the tick
/// returns an error with the panic's message, and the instance stops, so later ticks say
/// why. An index into an empty array a budgeted landline had returned ended a host this way.
#[test]
fn library_tick_sync_contains_a_panicking_hook() {
    let root = root("tick_sync_panic");
    build(&root, r#"
host world { fn record(n: int) }
let xs: int[] = []
let ticks = 0
on_tick(dt: float) {
    ticks = ticks + 1
    if ticks == 2 { let boom = xs[5] }
    world.record(ticks)
}
orchestrator main() {}
"#);
    let output = host(&root, r#"
use std::sync::{Arc, Mutex};
struct Host(Arc<Mutex<Vec<i64>>>);
impl scripts::Host for Host { fn world_record(&self, n: i64) -> Result<(), String> { self.0.lock().unwrap().push(n); Ok(()) } }
fn main() {
    std::panic::set_hook(Box::new(|_| {}));
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let mut scripts = scripts::start(runtime.handle(), Host(recorded.clone())).unwrap();
    scripts.ready_blocking(&runtime).unwrap();
    println!("{:?}", scripts.tick_sync(&runtime, 0.016));
    let second = scripts.tick_sync(&runtime, 0.016).unwrap_err();
    println!("{}", second.contains("tick panicked") && second.contains("index out of bounds"));
    let third = scripts.tick_sync(&runtime, 0.016).unwrap_err();
    println!("{}", third.contains("stopped after a tick panicked"));
    println!("{:?}", recorded.lock().unwrap());
}
"#);
    assert_eq!(output.trim(), "Ok(())\ntrue\ntrue\n[1]");
}

/// A host can ask which child processes an instance has running — a secret serverlet's
/// here — rather than walking the process table.
#[test]
fn library_reports_its_child_processes() {
    let root = root("child_processes");
    build(&root, r#"
serverlet Vault secret { on add(n: int) -> int { return n + 1 } }
let v = start Vault()
on_tick(dt: float) { print(to_string(v.add(1))) }
orchestrator main() {}
"#);
    let output = host(&root, r#"
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut scripts = scripts::start(runtime.handle(), ()).unwrap();
    scripts.ready_blocking(&runtime).unwrap();
    println!("before {:?}", scripts.child_processes());
    scripts.tick_blocking(&runtime, 0.016).unwrap();
    let children = scripts.child_processes();
    let alive = children.iter().all(|(_, pid)| std::process::Command::new("kill").args(["-0", &pid.to_string()]).status().map(|s| s.success()).unwrap_or(false));
    println!("after {:?} {}", children.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(), alive);
    scripts.shutdown_blocking(&runtime).unwrap();
}
"#);
    assert!(output.contains("before []"), "{output}");
    assert!(output.contains("after [\"Vault\"] true"), "{output}");
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
// Two boundaries inside the replay: an in-process serverlet, and a sandboxed one whose
// guest keeps state of its own. Each is called from on_tick and from an event handler.
serverlet Counter {
    let n = 0
    on add(k: int) -> int {
        n = n + k
        return n
    }
}
serverlet Mixer sandbox(memory_limit: "8mb", timeout: "50ms") {
    let seen = 0
    on mix(a: int, b: int) -> int {
        seen = seen + 1
        return (a * 31 + b) % 1000 + seen
    }
}
let counter = start Counter()
let mixer = start Mixer()
let ticks = 0
let fired = 0
on hit(n: int) {
    world.record(1, n)
    world.record(4, counter.add(n))
    sleep(5)
    world.record(2, n + 100)
    world.record(6, mixer.mix(n, 1))
}
on pulse(a: int, b: int) {
    world.record(3, a * b)
    trigger hit(a + b)
}
on_tick(dt: float) {
    ticks = ticks + 1
    world.record(0, ticks)
    world.record(5, mixer.mix(ticks, counter.add(1)))
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
    assert_eq!(count(5), TICKS + 10, "the sandboxed serverlet answered every tick");
    assert!(count(4) == count(1) && count(6) == count(2), "both serverlets answered every handler");
    println!("trace {} bytes, {} records", reference.len(), records.len());
}
"#);
    assert!(output.starts_with("trace "), "{output}");
    println!("{}", output.trim());
}

/// Clients held in instance state as values — an array of them, and a module's client
/// type — called from a hook and from an event handler, which reach the array by index.
#[test]
fn library_clients_as_values_in_instance_state() {
    let root = root("clients_as_values");
    fs::create_dir_all(root.join("desk")).unwrap();
    fs::write(root.join("desk/module.orch"), r#"
serverlet Ledger {
    let entries = 0
    on note(n: int) -> int {
        entries = entries + n
        return entries
    }
}
"#).unwrap();
    build(&root, r#"
use module desk: "./desk"
host world { fn record(n: int) }
serverlet Tenant {
    let total = 0
    on add(n: int) -> int {
        total = total + n
        return total
    }
}
let tenants: Tenant.client[] = [start Tenant(), start Tenant(), start Tenant()]
let ledger: desk.Ledger.client = start desk.Ledger()
task settle(book: desk.Ledger.client, n: int) -> int { return book.note(n) }
on hit(i: int, n: int) {
    world.record(tenants[i].add(n))
    world.record(settle(ledger, n))
}
on_tick(dt: float) {
    world.record(tenants[0].add(1))
}
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
    scripts.tick_sync(&runtime, 0.01).unwrap();
    scripts.trigger_hit(2, 5).unwrap();
    scripts.trigger_hit(0, 3).unwrap();
    scripts.tick_sync(&runtime, 0.01).unwrap();
    scripts.shutdown_blocking(&runtime).unwrap();
    println!("{:?}", recorded.lock().unwrap());
}
"#);
    assert_eq!(output.trim(), "[1, 5, 5, 4, 8, 5]");
}

/// A cadenced serverlet declared in a module: its instances register from the module's
/// code, and `latest(...)` in the entry file reads them.
#[test]
fn library_module_serverlet_cadence() {
    let root = root("module_cadence");
    fs::create_dir_all(root.join("plan")).unwrap();
    fs::write(root.join("plan/module.orch"), r#"
serverlet Planner sandbox(memory_limit: "8mb", timeout: "50ms", every: "100ms") {
    let n = 0
    on tick(dt: float) -> int {
        n = n + 1
        return n
    }
}
"#).unwrap();
    build(&root, r#"
use module plan: "./plan"
host world { fn record(n: int) }
let planner = start plan.Planner()
on_tick(dt: float) {
    match latest(planner.tick) {
        result::Ok(v) => world.record(v)
        result::Err(why) => world.record(0)
    }
}
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
    for _ in 0..12 { scripts.tick_sync(&runtime, 0.02).unwrap(); }
    scripts.shutdown_blocking(&runtime).unwrap();
    println!("{:?}", recorded.lock().unwrap());
}
"#);
    assert_eq!(output.trim(), "[1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 3, 3]");
}

/// A shared queue per instance. In a deterministic run a handler waiting in `take` gives up
/// its turn and is served, in order, by a later handler's push; a full `drop_oldest` queue
/// makes room and its push says so. The trace is the same on both tick paths.
#[test]
fn library_shared_queue_between_handlers() {
    let root = root("shared_queue");
    build(&root, r#"
host world { fn record(tag: int, value: int) }
shared queue orders: int (capacity: 4, full: "drop_oldest")
on consume(tag: int) {
    let order = take(orders)
    world.record(tag, order)
}
on produce(n: int) {
    let kept = push(orders, n)
    if !kept { world.record(9, n) }
}
on_tick(dt: float) {
    world.record(0, length(orders))
}
orchestrator main() {}
"#);
    let output = host(&root, r#"
use std::sync::{Arc, Mutex};
struct Host(Arc<Mutex<Vec<(i64, i64)>>>);
impl scripts::Host for Host { fn world_record(&self, tag: i64, value: i64) -> Result<(), String> { self.0.lock().unwrap().push((tag, value)); Ok(()) } }
fn run(runtime: &tokio::runtime::Runtime, sync: bool) -> Vec<(i64, i64)> {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let options = scripts::StartOptions { deterministic: true, ..Default::default() };
    let mut scripts = scripts::start_with_options(runtime.handle(), Host(recorded.clone()), options).unwrap();
    scripts.ready_blocking(runtime).unwrap();
    let tick = |s: &mut scripts::Scripts| if sync { s.tick_sync(runtime, 0.01).unwrap() } else { s.tick_blocking(runtime, 0.01).unwrap() };
    scripts.trigger_consume(1).unwrap();
    scripts.trigger_consume(2).unwrap();
    scripts.trigger_produce(10).unwrap();
    tick(&mut scripts);
    scripts.trigger_produce(20).unwrap();
    tick(&mut scripts);
    for n in 1..=5 { scripts.trigger_produce(100 + n).unwrap(); }
    tick(&mut scripts);
    scripts.shutdown_blocking(runtime).unwrap();
    let out = recorded.lock().unwrap().clone();
    out
}
fn main() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let reference = run(&runtime, false);
    assert_eq!(reference, run(&runtime, true), "tick_sync diverged from tick_blocking");
    println!("{:?}", reference);
}
"#);
    assert_eq!(output.trim(), "[(0, 1), (1, 10), (0, 1), (2, 20), (9, 105), (0, 4)]");
}

/// `measure` in a library, read by the host with `Scripts::stats()`: a handler, an event
/// handler that waits (so no CPU time), a cadenced hook counted only when it runs, and an
/// `unmeasured` hook left out.
#[test]
fn library_measure_records_hooks_events_and_handlers() {
    let root = root("measure");
    build(&root, r#"
measure serverlets, hooks
host world { fn record(n: int) }
serverlet Book {
    let filled = 0
    on place(qty: int) -> int {
        filled = filled + qty
        return filled
    }
}
let book = start Book()
on hit(n: int) {
    world.record(book.place(n))
}
on_tick(dt: float) every 3 ticks {
    world.record(0)
}
on_tick(dt: float) {
    world.record(1)
}
on_tick(dt: float) unmeasured {
    world.record(2)
}
orchestrator main() {}
"#);
    let output = host(&root, r#"
struct Host;
impl scripts::Host for Host { fn world_record(&self, _n: i64) -> Result<(), String> { Ok(()) } }
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut scripts = scripts::start(runtime.handle(), Host).unwrap();
    scripts.ready_blocking(&runtime).unwrap();
    for i in 0..9 {
        if i % 2 == 0 { scripts.trigger_hit(i).unwrap(); }
        scripts.tick_sync(&runtime, 0.01).unwrap();
    }
    for s in scripts.stats() {
        println!("{}|{}|{}|{}|{}|{}|{}", s.unit, s.kind, s.calls, s.failures, s.duration.is_some(), s.start_delay.is_some(), s.cpu.is_some());
    }
    scripts.shutdown_blocking(&runtime).unwrap();
}
"#);
    assert_eq!(output.trim(), [
        "Book.place|handler|5|0|true|true|true",
        "on hit|event|5|0|true|true|false",
        "on_tick|hook|9|0|true|true|true",
        "on_tick every 3 ticks|hook|3|0|true|true|true",
    ].join("\n"));
}

/// A measured typed hook: its output crosses the record, a `return` in it included.
#[test]
fn library_measure_keeps_a_typed_hooks_output() {
    let root = root("measure_typed");
    build(&root, r#"
measure hooks
struct Frame { n: int }
on_tick(dt: float, input: Frame) -> int {
    if input.n < 0 { return 0 }
    return input.n * 2
}
orchestrator main() {}
"#);
    let output = host(&root, r#"
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut scripts = scripts::start(runtime.handle(), ()).unwrap();
    scripts.ready_blocking(&runtime).unwrap();
    let a = scripts.tick_sync(&runtime, 0.01, scripts::Frame { n: 21 }).unwrap();
    let b = scripts.tick_sync(&runtime, 0.01, scripts::Frame { n: -4 }).unwrap();
    let stats = scripts.stats();
    println!("{a} {b} {} {}", stats[0].unit, stats[0].calls);
    scripts.shutdown_blocking(&runtime).unwrap();
}
"#);
    assert_eq!(output.trim(), "42 0 on_tick 2");
}

/// A cadence is a divisor of the host's clock. Over 10,000 ticks of varying `dt`, a
/// sandboxed serverlet declared `every: "100ms"`, a hook `every "250ms"`, and a hook
/// `every 7 ticks` each run exactly as often as a schedule computed here from the same
/// `dt`s says, each is passed the host time since it last ran, and the hooks read the
/// serverlet's answer from the same tick. The trace is byte-identical across both tick
/// paths and both runtimes, deterministic or not: nothing in it depends on wall time.
#[test]
fn engine_cadence_follows_host_time() {
    let root = root("cadence");
    build(&root, r#"
host world { fn record(tag: int, value: int) }
serverlet Planner sandbox(memory_limit: "8mb", timeout: "50ms", every: "100ms") {
    let plans = 0
    on tick(dt: float) -> int {
        plans = plans + 1
        return plans * 1000000 + to_int(round(dt * 1000000.0))
    }
}
let planner = start Planner()
on_tick(dt: float) every "250ms" {
    world.record(2, to_int(round(dt * 1000000.0)))
}
on_tick(dt: float) every 7 ticks {
    world.record(3, to_int(round(dt * 1000000.0)))
}
on_tick(dt: float) {
    match latest(planner.tick) {
        result::Ok(answer) => world.record(1, answer)
        result::Err(why) => world.record(0, 0)
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
fn dt(i: usize) -> f64 { 0.001 + (i % 5) as f64 * 0.001 }
fn run(runtime: &tokio::runtime::Runtime, deterministic: bool, sync: bool) -> Vec<u8> {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let options = scripts::StartOptions { deterministic, ..Default::default() };
    let mut scripts = scripts::start_with_options(runtime.handle(), Host(trace.clone()), options).unwrap();
    scripts.ready_blocking(runtime).unwrap();
    for i in 0..TICKS {
        if sync { scripts.tick_sync(runtime, dt(i)).unwrap(); } else { scripts.tick_blocking(runtime, dt(i)).unwrap(); }
    }
    scripts.shutdown_blocking(runtime).unwrap();
    let result = trace.lock().unwrap().clone();
    result
}
/// When a hook with this period runs, and the host time in microseconds since its last run:
/// the first tick, then once each period on a schedule fixed to it, never twice in a tick.
fn schedule(period_nanos: u64) -> Vec<i64> {
    let (mut clock, mut last, mut due) = (0u64, 0u64, 0u64);
    let mut runs = Vec::new();
    for i in 0..TICKS {
        clock += (dt(i) * 1e9).round_ties_even() as u64;
        if i > 0 && clock < due { continue; }
        due = if i == 0 { clock + period_nanos } else { due + period_nanos * ((clock - due) / period_nanos + 1) };
        runs.push(((clock - last) as f64 / 1e3).round() as i64);
        last = clock;
    }
    runs
}
fn main() {
    let single = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let multi = tokio::runtime::Runtime::new().unwrap();
    let reference = run(&single, true, false);
    assert_eq!(reference, run(&single, true, true), "tick_sync diverged from tick_blocking");
    assert_eq!(reference, run(&multi, true, false), "the multithreaded runtime diverged");
    assert_eq!(reference, run(&multi, false, true), "a run that is not deterministic diverged: the cadence read wall time");
    let records: Vec<(i64, i64)> = reference.chunks_exact(16).map(|c| (
        i64::from_le_bytes(c[..8].try_into().unwrap()), i64::from_le_bytes(c[8..].try_into().unwrap()),
    )).collect();
    let of = |tag: i64| records.iter().filter(|(t, _)| *t == tag).map(|(_, v)| *v).collect::<Vec<_>>();
    // The planner answered before the first tick's hooks, so every tick read an answer.
    let answers = of(1);
    assert_eq!(answers.len(), TICKS, "a tick read no answer: {} records of tag 0", of(0).len());
    let mut calls: Vec<i64> = answers.clone();
    calls.dedup();
    let expected = schedule(100_000_000);
    assert_eq!(calls.iter().map(|a| a / 1_000_000).collect::<Vec<_>>(), (1..=expected.len() as i64).collect::<Vec<_>>(), "the planner's calls, in order");
    assert_eq!(calls.iter().map(|a| a % 1_000_000).collect::<Vec<_>>(), expected, "the host time each planner call was passed");
    assert_eq!(of(2), schedule(250_000_000), "the hook every 250ms");
    let sevens = of(3);
    assert_eq!(sevens.len(), (TICKS + 6) / 7, "the every 7 ticks hook");
    println!("{} planner calls, {} slow hooks, {} every-seventh hooks", expected.len(), of(2).len(), sevens.len());
}
"#);
    assert!(output.contains("planner calls"), "{output}");
    println!("{}", output.trim());
}

/// Deterministic mode across the process boundaries: a secret serverlet and a Python
/// landline whose every call misses its budget, called from on_tick and from a handler.
/// A deterministic run ignores the budget, so each call answers; replayed through both
/// tick paths, the trace is byte-identical. Outside deterministic mode the budget holds,
/// unless the host lifts it with `ignore_budgets`.
#[test]
fn engine_deterministic_replay_across_process_boundaries() {
    let root = root("deterministic_processes");
    fs::write(root.join("slow.py"), r#"
import time
from orchestratelang import landline
class Slow(landline.Serverlet):
    def echo(self, n: int) -> int:
        time.sleep(0.02)
        return n * 2
landline.serve(Slow)
"#).unwrap();
    build(&root, r#"
host world { fn record(tag: int, value: int) }
serverlet Vault secret {
    let n = 0
    on add(k: int) -> int {
        n = n + k
        return n
    }
}
serverlet Slow via python(source: "slow.py", budget: "5ms") { on echo(n: int) -> int }
let vault = start Vault()
let slow = start Slow()
let ticks = 0
on hit(n: int) { world.record(2, vault.add(n)) }
on_tick(dt: float) {
    ticks = ticks + 1
    world.record(0, vault.add(1))
    world.record(1, slow.echo(ticks))
    if ticks % 3 == 0 { trigger hit(ticks) }
}
orchestrator main() {}
"#);
    let output = host(&root, r#"
use std::sync::{Arc, Mutex};
struct Host(Arc<Mutex<Vec<(i64, i64)>>>);
impl scripts::Host for Host {
    fn world_record(&self, tag: i64, value: i64) -> Result<(), String> { self.0.lock().unwrap().push((tag, value)); Ok(()) }
}
fn run(runtime: &tokio::runtime::Runtime, options: scripts::StartOptions, sync: bool) -> Vec<(i64, i64)> {
    let trace = Arc::new(Mutex::new(Vec::new()));
    let mut scripts = scripts::start_with_options(runtime.handle(), Host(trace.clone()), options).unwrap();
    scripts.ready_blocking(runtime).unwrap();
    for _ in 0..30 {
        if sync { scripts.tick_sync(runtime, 0.016).unwrap(); } else { scripts.tick_blocking(runtime, 0.016).unwrap(); }
    }
    scripts.shutdown_blocking(runtime).unwrap();
    let result = trace.lock().unwrap().clone();
    result
}
fn echoes(trace: &[(i64, i64)]) -> Vec<i64> { trace.iter().filter(|(t, _)| *t == 1).map(|(_, v)| *v).collect() }
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let deterministic = scripts::StartOptions { deterministic: true, ..Default::default() };
    let reference = run(&runtime, deterministic.clone(), false);
    assert_eq!(reference, run(&runtime, deterministic, true), "tick_sync diverged from tick_blocking");
    let expected: Vec<i64> = (1..=30).map(|n| n * 2).collect();
    assert_eq!(echoes(&reference), expected, "a deterministic run waits for every reply");
    assert_eq!(reference.iter().filter(|(t, _)| *t == 2).count(), 10, "every handler called the secret serverlet");
    let lifted = run(&runtime, scripts::StartOptions { ignore_budgets: true, ..Default::default() }, false);
    assert_eq!(echoes(&lifted), expected, "ignore_budgets lifts the budget");
    let budgeted = run(&runtime, scripts::StartOptions::default(), false);
    assert!(echoes(&budgeted).iter().all(|v| *v == 0), "the budget holds by default: {:?}", echoes(&budgeted));
    println!("replayed {} records", reference.len());
}
"#);
    assert!(output.starts_with("replayed "), "{output}");
}

/// The documented exclusions from deterministic mode fail the way the documentation says:
/// a spawned worker, named, and a `sleep` outside an event handler each stop the library
/// with the deterministic-mode message. On the coordinator's task the panic surfaces to
/// the host as an error from `ready` or `tick` that carries the message; under `tick_sync`
/// the body runs on the host's own thread, where the panic is caught and returned too.
#[test]
fn engine_deterministic_mode_refuses_what_it_excludes() {
    // Serverlets and landlines are not here: since 0.16 a deterministic run holds their
    // actors and runs them in call order, inside the tick.
    let programs = [
        ("worker", "host world { fn record(n: int) }\nlet worker = automatic { world.record(1) }\non_tick(dt: float) { }\norchestrator main(procs: process[worker]) {}\n", "startup"),
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
    // The error carries the panic's own message, so the host learns why, not only that.
    if failure == "startup" {{
        let error = ready.unwrap_err();
        assert!(error.starts_with("library startup task failed: ") && error.contains("deterministic"), "{{error}}");
        // The refusal names the declaration it hit.
        assert!(error.contains("'worker'"), "{{error}}");
    }} else {{
        ready.unwrap();
        let error = tick.unwrap_err();
        assert!(error.starts_with("tick task failed: ") && error.contains("deterministic"), "{{error}}");
    }}
    drop(scripts);
    // The synchronous path for a tick-time failure: the body runs on this thread, and the
    // panic is caught there and returned, rather than unwinding into the host.
    if failure == "tick" {{
        let mut scripts = scripts::start_with_options(runtime.handle(), Host, options).unwrap();
        scripts.ready_blocking(&runtime).unwrap();
        let error = scripts.tick_sync(&runtime, 0.016).unwrap_err();
        assert!(error.starts_with("tick panicked: ") && error.contains("deterministic"), "{{error}}");
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

/// Keyword names in a library: program state `type`, shared state `dyn`, a struct with
/// fields `ref` and `type`, a tick parameter `move`, and a host function `move` whose
/// trait method is `world_move` either way.
#[test]
fn library_keyword_names() {
    let root = root("keyword_names");
    build(&root, r#"
struct Point { ref: int, type: int }
host world { fn move(ref: int) -> int }
let type = 0
shared let dyn = 0
let gen = Point { ref: 1, type: 2 }
on_tick(move: float) { type = type + 1  dyn = dyn + 1  print(to_string(world.move(type + gen.ref + dyn))) }
"#);
    let output = host(&root, r#"
struct Host;
impl scripts::Host for Host {
    fn world_move(&self, n: i64) -> Result<i64, String> { Ok(n * 10) }
    fn log(&self, _: scripts::LogLevel, message: &str) { println!("{message}"); }
}
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut scripts = scripts::start(runtime.handle(), Host).unwrap();
    scripts.tick_blocking(&runtime, 0.1).unwrap();
    scripts.tick_blocking(&runtime, 0.1).unwrap();
    scripts.shutdown_blocking(&runtime).unwrap();
}
"#);
    assert_eq!(output.trim(), "30\n50");
}

/// A sandboxed serverlet whose handlers have no bodies is a guest the program is handed
/// while it runs: `start Model from path` reads the `.wasm`, instantiates it under the
/// declaration's limits and grants, and checks it implements the declaration, answering
/// `err` if not. Guests are kept by name and replaced by loading another file under it;
/// the same run replays in deterministic mode.
#[test]
fn library_loads_a_guest_while_it_runs() {
    let root = root("loaded_guest");
    let guest = |file: &str, source: &str| {
        fs::write(root.join(format!("{file}.orch")), source).unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_orchestrate"))
            .args(["build", "--guest"]).arg(root.join(format!("{file}.orch")))
            .arg("-o").arg(root.join(format!("{file}.wasm")))
            .output().unwrap();
        assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    };
    guest("scale", r#"
host world { fn note(message: string) }
serverlet Model sandbox(memory_limit: "8mb", timeout: "20ms") {
    grant call world.note
    on radial(r2: float[], params: float[]) -> float[] {
        world.note("scale saw " + to_string(length(r2)))
        return map(r2, fn(x: float) -> float { params[0] * x })
    }
}
"#);
    guest("negate", r#"
serverlet Model sandbox(memory_limit: "8mb", timeout: "20ms") {
    on radial(r2: float[], params: float[]) -> float[] { return map(r2, fn(x: float) -> float { 0.0 - x }) }
    on unused() -> int { return 1 }
}
"#);
    guest("wrong", r#"
serverlet Model sandbox(memory_limit: "8mb", timeout: "20ms") {
    on radial(r2: float[]) -> float[] { return r2 }
}
"#);
    build(&root, r#"
host world { fn note(message: string) fn record(value: float) }
serverlet Model sandbox(memory_limit: "8mb", timeout: "200ms") {
    grant call world.note
    on radial(r2: float[], params: float[]) -> result<float[]>
}
let models: map<string, Model.client> = map {}
on load_model(name: string, path: string) {
    match start Model from path {
        result::Ok(model) => models[name] = model
        result::Err(why) => world.note("not loaded: " + why)
    }
}
on_tick(dt: float) {
    for name in keys(models) {
        let model = models[name]
        match model.radial([2.0], [3.0]) {
            result::Ok(forces) => world.record(forces[0])
            result::Err(why) => world.note(why)
        }
    }
}
orchestrator main() {}
"#);
    let dir = root.to_string_lossy().to_string();
    let output = host(&root, &format!(r#"
use std::sync::{{Arc, Mutex}};
struct Host(Arc<Mutex<Vec<String>>>);
impl scripts::Host for Host {{
    fn world_note(&self, message: String) -> Result<(), String> {{ self.0.lock().unwrap().push(message); Ok(()) }}
    fn world_record(&self, value: f64) -> Result<(), String> {{ self.0.lock().unwrap().push(format!("{{value}}")); Ok(()) }}
}}
fn run(deterministic: bool) -> Vec<String> {{
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let log = Arc::new(Mutex::new(Vec::new()));
    let options = scripts::StartOptions {{ deterministic, ..Default::default() }};
    let mut scripts = scripts::start_with_options(runtime.handle(), Host(log.clone()), options).unwrap();
    scripts.ready_blocking(&runtime).unwrap();
    scripts.trigger_load_model("a".to_string(), "{dir}/scale.wasm".to_string()).unwrap();
    scripts.tick_sync(&runtime, 0.1).unwrap();
    scripts.trigger_load_model("b".to_string(), "{dir}/wrong.wasm".to_string()).unwrap();
    scripts.trigger_load_model("c".to_string(), "{dir}/missing.wasm".to_string()).unwrap();
    scripts.tick_sync(&runtime, 0.1).unwrap();
    scripts.trigger_load_model("a".to_string(), "{dir}/negate.wasm".to_string()).unwrap();
    scripts.tick_sync(&runtime, 0.1).unwrap();
    scripts.shutdown_blocking(&runtime).unwrap();
    let log = log.lock().unwrap().clone();
    log
}}
fn main() {{
    let normal = run(false);
    for line in &normal {{ println!("{{line}}"); }}
    assert_eq!(run(true), normal, "a deterministic run gave another log");
    println!("replayed");
}}
"#));
    let lines: Vec<&str> = output.lines().collect();
    assert_eq!(lines[0..3], ["scale saw 1", "6", "not loaded: wasm module 'Model': interface mismatch: handler 'radial' is declared as radial(float[],float[])->float[] but the guest defines radial(float[])->float[]"], "{output}");
    assert!(lines[3].starts_with("not loaded: sandboxed serverlet 'Model': cannot read '") && lines[3].contains("missing.wasm"), "{output}");
    assert_eq!(lines[4..], ["scale saw 1", "6", "-2", "replayed"], "{output}");
}

/// What `start X from path` accepts, and what it refuses by name: a loaded sandbox must
/// be started from a file and a compiled-in one cannot be, a TypeScript landline is built
/// in, the path is a string, and a sandbox's handlers all have bodies or none do.
#[test]
fn loaded_guest_diagnostics() {
    use orchestrate_lib::{lexer::Lexer, parser::Parser, typechecker::TypeChecker};
    let check = |source: &str| -> Result<(), String> {
        let ast = Parser::new(Lexer::new(source).tokenize().unwrap()).parse()?;
        TypeChecker::new().type_check(&ast).map(|_| ())
    };
    let loaded = "serverlet M sandbox(memory_limit: \"8mb\", timeout: \"20ms\") { on f(x: int) -> int }\n";
    let built = "serverlet B sandbox(memory_limit: \"8mb\", timeout: \"20ms\") { on f(x: int) -> int { return x } }\n";
    assert!(check(&format!("{loaded}orchestrator main() {{ match start M from \"m.wasm\" {{ result::Ok(m) => print(to_string(m.f(1))) result::Err(e) => print(e) }} }}")).is_ok());
    // `from` on the next line is a statement of its own, not a start from a file.
    assert!(check("orchestrator main() {\n    let from = 1\n    let w = automatic { sleep(10) }\n    start w\n    from = 2\n    print(to_string(from))\n}\n").is_ok());
    let cases = [
        (format!("{loaded}orchestrator main() {{ let m = start M() }}"), "is loaded from a file"),
        (format!("{built}orchestrator main() {{ let b = start B from \"b.wasm\" }}"), "is compiled into the program"),
        (format!("{loaded}orchestrator main() {{ let m = start M from 3 }}"), "takes the path of a file, a string; this is int"),
        ("serverlet T via typescript(source: \"t.ts\") { on f() -> int }\norchestrator main() { let t = start T from \"t.ts\" }".to_string(), "is a TypeScript landline"),
        ("serverlet X sandbox(memory_limit: \"8mb\", timeout: \"20ms\") { on f() -> int { return 1 } on g() -> int }".to_string(), "either all have bodies"),
        ("serverlet X sandbox(memory_limit: \"8mb\", timeout: \"20ms\") { let n = 0 on g() -> int }".to_string(), "its state is the guest's own"),
    ];
    for (source, expected) in cases {
        let error = check(&source).unwrap_err();
        assert!(error.contains(expected), "{source}\n{error}");
    }
}

/// Instance state named in a struct literal a hook returns is copied into it: a string
/// field was moved out of `&mut` state and reached rustc (E0507).
#[test]
fn library_struct_literal_copies_instance_state() {
    let root = root("struct_literal_state");
    build(&root, r#"
struct Report { note: string, count: int }
host app { fn show(n: int) }
let note = "ready"
let count = 0
on_tick(dt: float) -> Report {
    count = count + 1
    return Report { note: note, count: count }
}
orchestrator main() {}
"#);
    let output = host(&root, r#"
struct Host;
impl scripts::Host for Host { fn app_show(&self, _: i64) -> Result<(), String> { Ok(()) } }
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let mut scripts = scripts::start(runtime.handle(), Host).unwrap();
    for _ in 0..2 {
        let report = scripts.tick_blocking(&runtime, 0.1).unwrap();
        println!("{} {}", report.note, report.count);
    }
}
"#);
    assert_eq!(output.trim(), "ready 1\nready 2", "{output}");
}
