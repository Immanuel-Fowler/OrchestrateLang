use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
};

/// Removed when its test passes; a failing test keeps the directory for inspection.
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
    let p = std::env::temp_dir().join(format!("orch_typescript_{}_{name}", std::process::id()));
    fs::create_dir_all(&p).unwrap();
    TempRoot(p)
}
fn available() -> bool {
    let tsc = std::env::var_os("ORCH_TSC").unwrap_or_else(|| "tsc".into());
    let bun = std::env::var_os("ORCH_BUN").unwrap_or_else(|| "bun".into());
    let ok = Command::new(tsc).arg("--version").output().is_ok_and(|o| {
        o.status.success() && String::from_utf8_lossy(&o.stdout).starts_with("Version 7.")
    }) && Command::new(bun)
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !ok {
        eprintln!("TypeScript integration needs Bun and TypeScript 7 (install with bun add --dev typescript)");
    }
    ok
}
fn compile(root: &Path, source: &str, lib: bool, backend: &str) -> Output {
    fs::write(root.join("main.orch"), source).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_orchestrate"));
    cmd.arg(if lib { "build" } else { "run" });
    if lib {
        cmd.arg("--lib");
    }
    cmd.arg(root.join("main.orch"));
    if lib {
        cmd.arg("-o").arg(root.join("scripts"));
    }
    cmd.env("ORCH_TS_BACKEND", backend)
        .env("CARGO_NET_OFFLINE", "true");
    cmd.output().unwrap()
}
fn success(o: &Output) -> String {
    assert!(
        o.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).into_owned()
}
#[test]
fn typescript_interfaces_and_diagnostics() {
    use orchestrate_lib::{lexer::Lexer, parser::Parser, typescript::sidecar};
    let source = "serverlet Worker via typescript(source: \"worker.ts\", budget: \"2ms\", late: \"latest\") { on tick(n: int) -> int }";
    let ast = Parser::new(Lexer::new(source).tokenize().unwrap())
        .parse()
        .unwrap();
    match &ast[0].node {
        orchestrate_lib::ast::StmtNode::Serverlet {
            landline: Some(c), ..
        } => {
            assert_eq!(c.runtime, "typescript");
            assert_eq!(c.budget_micros, Some(2000));
        }
        _ => panic!("missing config"),
    }
    assert_eq!(
        sidecar("echo(n: int[]) -> int[]\nname() -> string")
            .unwrap()
            .len(),
        2
    );
    for bad in [
        "bad(n: void)",
        "bad(n: option<int>)",
        "bad(n: float",
        "a()\na()",
    ] {
        assert!(sidecar(bad).is_err(), "{bad}");
    }
}
#[test]
fn typescript_bun_ffi_and_type_errors() {
    if !available() {
        return;
    }
    let root = root("ffi");
    let module = root.join("math");
    fs::create_dir_all(&module).unwrap();
    fs::write(
        module.join("module.orch"),
        "load_foreign \"typescript\" \"math.ts\"",
    )
    .unwrap();
    fs::write(
        module.join("math.orch_ffi"),
        "echo(n: int[]) -> int[]\ntext(s: string) -> string\nfail() -> int",
    )
    .unwrap();
    fs::write(module.join("math.ts"), "console.log('module log');\nexport function echo(n: bigint[]): bigint[] { return n; }\nexport async function text(s: string): Promise<string> { return s + '✓'; }\nexport function fail(): bigint { throw new Error('expected failure'); }").unwrap();
    let src = "use module math: \"./math\"\norchestrator main() { let values = math.echo([9007199254740993, -7]); print(values[0]); print(values[1]); print(math.text(\"hello\")); stop_orch() }";
    let output = success(&compile(&root, src, false, "bun"));
    assert!(output.contains("9007199254740993"), "{output}");
    assert!(output.contains("hello✓"));
    let failed = compile(
        &root,
        "use module math: \"./math\"\norchestrator main() { print(math.fail()); stop_orch() }",
        false,
        "bun",
    );
    assert!(!failed.status.success());
    assert!(String::from_utf8_lossy(&failed.stderr).contains("expected failure"));
    fs::write(module.join("math.ts"), "export function echo(n: number[]): number[] { return n; } export function text(s: string): string {return s;} export function fail(): bigint {return 0n;}").unwrap();
    let bad = compile(&root, src, false, "auto");
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("TypeScript 7 check failed"));
}
/// One process per module: state persists across calls, survives a handler error, and
/// stays separate from other modules. The backend named in source beats the environment.
#[test]
fn typescript_ffi_persistent_state_errors_and_source_backend() {
    if !available() {
        return;
    }
    let root = root("persistent_ffi");
    let math = root.join("math");
    fs::create_dir_all(&math).unwrap();
    fs::write(math.join("module.orch"), "load_foreign \"typescript\" \"math.ts\"").unwrap();
    fs::write(
        math.join("math.orch_ffi"),
        "twice(n: float) -> float\nnext() -> float\nfail(n: int) -> int",
    )
    .unwrap();
    fs::write(
        math.join("math.ts"),
        "let calls = 0; export function twice(n: number): number { return n * 2; } export function next(): number { calls += 1; return calls; } export function fail(n: bigint): bigint { if (n === 0n) { throw new Error('boom'); } return n; }",
    )
    .unwrap();
    let counter = root.join("counter");
    fs::create_dir_all(&counter).unwrap();
    fs::write(
        counter.join("module.orch"),
        "load_foreign \"typescript\" \"counter.ts\" (backend: \"bun\")",
    )
    .unwrap();
    fs::write(counter.join("counter.orch_ffi"), "next() -> float").unwrap();
    fs::write(
        counter.join("counter.ts"),
        "let n = 0; export function next(): number { n += 1; return n; }",
    )
    .unwrap();
    let src = r#"use module math: "./math"
use module counter: "./counter"
let faulty = automatic(restart: never) {
    sleep(150)
    let x = math.fail(0)
} on_crash error {
    print("crashed")
}
orchestrator main(workers: process[faulty]) {
    print(math.twice(3.5)); print(math.twice(-0.0)); print(math.twice(1.0 / 0.0))
    print("math {math.next()} {math.next()}")
    print("counter {counter.next()} {counter.next()}")
    sleep(2000)
    print("after {math.next()} {counter.next()}")
    stop_orch()
}"#;
    let output = success(&compile(&root, src, false, "auto"));
    let values = output
        .lines()
        .filter(|line| !line.starts_with("[orchestrate]"))
        .collect::<Vec<_>>();
    // The worker's crash lands somewhere among the first prints, depending on how long
    // the first call's process start takes; what matters is that it precedes "after".
    let crashed = values.iter().position(|line| *line == "crashed").expect("worker crashed");
    let after = values.iter().position(|line| line.starts_with("after")).unwrap();
    assert!(crashed < after, "{values:?}");
    let sequence = values.iter().filter(|line| **line != "crashed").copied().collect::<Vec<_>>();
    assert_eq!(sequence, ["7", "-0", "inf", "math 1 2", "counter 1 2", "after 3 3"]);
    let backend =
        fs::read_to_string(root.join(".orch_cache/landlines/ffi_math_0/backend.txt")).unwrap();
    assert!(matches!(backend.as_str(), "scriptc\n" | "bun\n"), "{backend}");
    assert_eq!(
        fs::read_to_string(root.join(".orch_cache/landlines/ffi_counter_0/backend.txt")).unwrap(),
        "bun\n"
    );
}
#[test]
fn typescript_library_ticks_grants_state_errors_and_packaging() {
    if !available() {
        return;
    }
    let root = root("library");
    fs::write(root.join("worker.ts"), r#"
declare const Bun: { version: string };
type Context = {tickNumber: bigint; tickDt: number; host: Record<string, Record<string, (...args: any[]) => any>>};
export default class Worker {
    total = 0n;
    constructor(public context: Context) {}
    async tick(input: {values: bigint[]}): Promise<{total: bigint; frame: bigint}> {
        if (!Bun.version) throw new Error('Bun runtime unavailable');
        console.log('typescript tick');
        for (const n of input.values) this.total += n;
        if (this.context.tickDt !== 0.25) throw new Error('wrong dt');
        this.context.host.world.record(this.total);
        return {total: this.total, frame: this.context.tickNumber};
    }
    fail(): bigint { throw new Error('expected handler failure'); }
}
"#).unwrap();
    fs::write(root.join("slow.ts"), "export default class Slow { async value(): Promise<bigint> { await new Promise<void>(resolve => setTimeout(resolve, 300)); return 8n; } }").unwrap();
    let src = r#"
struct Input { values: int[] }
struct Output { total: int, frame: int }
host world { fn record(n: int) -> void }
serverlet Worker via typescript(source: "worker.ts") {
    grant call world.record
    on tick(input: Input) -> Output
    on fail() -> int
}
serverlet Slow via typescript(source: "slow.ts", budget: "20ms", late: "latest") { on value() -> int }
let slow = start Slow()
let worker = start Worker()
on_tick(dt: float, input: Input) -> Output { worker.tick(input) }
on_fixed_tick(step: float) { print(worker.fail()); world.record(slow.value()) }
orchestrator main() {}
"#;
    success(&compile(&root, src, true, "auto"));
    // The player only needs the generated crate; implementation source is gone.
    fs::remove_file(root.join("worker.ts")).unwrap();
    fs::create_dir_all(root.join("host/src")).unwrap();
    fs::write(root.join("host/Cargo.toml"), "[package]\nname=\"typescript_host\"\nversion=\"0.1.0\"\nedition=\"2024\"\n[dependencies]\nscripts={path=\"../scripts\"}\ntokio={version=\"1.35\",features=[\"full\"]}\n[workspace]\n").unwrap();
    fs::write(root.join("host/src/main.rs"), r#"
use std::sync::{Arc, Mutex};
#[derive(Clone, Default)] struct Host { calls: Arc<Mutex<Vec<i64>>>, logs: Arc<Mutex<Vec<String>>> }
impl scripts::Host for Host {
    fn world_record(&self, n: i64) -> Result<(), String> { self.calls.lock().unwrap().push(n); Ok(()) }
    fn log(&self, _: scripts::LogLevel, text: &str) { self.logs.lock().unwrap().push(text.to_string()); }
}
fn main() {
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let host = Host::default();
    let mut a = { let _guard = runtime.enter(); scripts::start(runtime.handle(), host.clone()).unwrap() };
    let mut b = { let _guard = runtime.enter(); scripts::start(runtime.handle(), host.clone()).unwrap() };
    a.ready_blocking(&runtime).unwrap(); b.ready_blocking(&runtime).unwrap();
    let first = a.tick_blocking(&runtime, 0.25, scripts::Input {values: vec![9007199254740993]}).unwrap();
    assert_eq!((first.total, first.frame), (9007199254740993, 1));
    let second = a.tick_blocking(&runtime, 0.25, scripts::Input {values: vec![2]}).unwrap();
    assert_eq!((second.total, second.frame), (9007199254740995, 2));
    let other = b.tick_blocking(&runtime, 0.25, scripts::Input {values: vec![7]}).unwrap();
    assert_eq!((other.total, other.frame), (7, 1));
    let before = std::time::Instant::now();
    a.fixed_tick_blocking(&runtime, 0.01).unwrap();
    assert!(before.elapsed() < std::time::Duration::from_millis(200));
    runtime.block_on(async {tokio::time::sleep(std::time::Duration::from_millis(400)).await});
    a.fixed_tick_blocking(&runtime, 0.01).unwrap();
    runtime.block_on(async {tokio::time::sleep(std::time::Duration::from_millis(20)).await});
    assert!(host.logs.lock().unwrap().iter().any(|s| s.contains("expected handler failure")));
    assert!(host.logs.lock().unwrap().iter().any(|s| s.contains("typescript tick")));
    a.shutdown_blocking(&runtime).unwrap(); b.shutdown_blocking(&runtime).unwrap();
    assert_eq!(*host.calls.lock().unwrap(), vec![9007199254740993, 9007199254740995, 7, 0, 8]);
}
"#).unwrap();
    let o = Command::new("cargo")
        .args(["run", "--quiet"])
        .current_dir(root.join("host"))
        .env("CARGO_NET_OFFLINE", "true")
        .output()
        .unwrap();
    assert_eq!(success(&o), "");
}
