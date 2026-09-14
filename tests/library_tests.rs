use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn root(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("orch_library_test_{}", name));
    fs::create_dir_all(&path).unwrap();
    path
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
