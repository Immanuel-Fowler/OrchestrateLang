use orchestrate_lib::{lexer::Lexer, parser::Parser, typechecker::TypeChecker};
use std::{fs, path::PathBuf, process::Command};

fn parse(source: &str) -> Result<Vec<orchestrate_lib::ast::Stmt>, String> {
    Parser::new(Lexer::new(source).tokenize()?).parse()
}

#[test]
fn landline_syntax_and_types() {
    let ast = parse(
        r#"serverlet X via python(source: "x.py", line: "pipe") {
        on add(n: int) -> int;
        on clear()
    }"#,
    )
    .unwrap();
    TypeChecker::new().type_check(&ast).unwrap();
    for (source, error) in [
        (r#"serverlet X via python() {}"#, "requires source"),
        (
            r#"serverlet X via python(source: "x", source: "y") {}"#,
            "Duplicate",
        ),
        (
            r#"serverlet X via python(source: "x", line: "embedded") {}"#,
            "Unsupported",
        ),
        (r#"serverlet X via node(source: "x") {}"#, "Expected python or typescript"),
        (
            r#"serverlet X secret via python(source: "x") {}"#,
            "cannot be combined",
        ),
        (
            r#"serverlet X via python(source: "x") { let n = 0 }"#,
            "state belongs",
        ),
        (
            r#"serverlet X via python(source: "x") { on f() {} }"#,
            "must not have bodies",
        ),
        (
            r#"serverlet X via python(source: "x", budget: "fast") {}"#,
            "Invalid landline budget",
        ),
        (
            r#"serverlet X via python(source: "x", budget: "0us") {}"#,
            "Invalid landline budget",
        ),
        (
            r#"serverlet X via python(source: "x", late: "latest") {}"#,
            "requires a budget",
        ),
        (
            r#"serverlet X via python(source: "x", budget: "2ms", late: "next") {}"#,
            "Unsupported",
        ),
    ] {
        assert!(parse(source).unwrap_err().contains(error), "{}", source);
    }
    for source in [
        r#"serverlet X via python(source: "x") { on f(a: option<int>) }"#,
        r#"serverlet X via python(source: "x") { on f() on f() }"#,
    ] {
        assert!(TypeChecker::new()
            .type_check(&parse(source).unwrap())
            .is_err());
    }
    use orchestrate_lib::ast::{LatePolicy, StmtNode};
    for (source, micros, late) in [
        (r#"serverlet X via python(source: "x") {}"#, None, LatePolicy::Drop),
        (r#"serverlet X via python(source: "x", budget: "250us") {}"#, Some(250), LatePolicy::Drop),
        (r#"serverlet X via python(source: "x", budget: "2ms", late: "latest") {}"#, Some(2_000), LatePolicy::Latest),
        (r#"serverlet X via python(source: "x", budget: "0.5s", late: "drop") {}"#, Some(500_000), LatePolicy::Drop),
    ] {
        match &parse(source).unwrap()[0].node {
            StmtNode::Serverlet { landline: Some(config), .. } => {
                assert_eq!((config.budget_micros, config.late), (micros, late), "{}", source);
            }
            _ => panic!("expected a landline serverlet: {}", source),
        }
    }
}

#[test]
fn python_budgets_and_late_results() {
    // Even calls sleep past the 100ms budget. `latest` returns the most recent completed
    // result (a late reply becomes it); `drop` returns the default, and a queued call
    // whose caller already gave up is never sent, so `calls()` counts only one compute.
    let (stdout, stderr) = run(
        &directory("budgets"),
        r#"
serverlet Latest via python(source: "impl.py", budget: "100ms", late: "latest") {
    on compute(n: int) -> int
    on calls() -> int
}
serverlet Dropper via python(source: "impl.py", budget: "100ms") {
    on compute(n: int) -> int
    on calls() -> int
}
orchestrator main() {
    let fresh = start Latest()
    let dropper = start Dropper()
    sleep(1500)
    print(to_string(fresh.compute(1)))
    print(to_string(fresh.compute(2)))
    sleep(1000)
    print(to_string(fresh.compute(4)))
    sleep(1000)
    print(to_string(fresh.compute(5)))
    print(to_string(dropper.compute(2)))
    print(to_string(dropper.compute(1)))
    sleep(1000)
    print(to_string(dropper.calls()))
    print(to_string(dropper.compute(3)))
    stop_orch()
}
"#,
        r#"
import time
from orchestratelang import landline
class Worker(landline.Serverlet):
    def __init__(self): self.count = 0
    def compute(self, n: int) -> int:
        self.count += 1
        if n % 2 == 0:
            time.sleep(0.6)
        return n * 10
    def calls(self) -> int: return self.count
landline.serve(Worker)
"#,
    );
    assert_eq!(stdout, "10\n10\n20\n50\n0\n0\n1\n30", "{}", stderr);
}

/// A test's working directory under the system temp dir. It is removed when the test
/// passes; a failing test keeps it, and `ORCH_KEEP_TEST_BUILDS=1` keeps every one.
struct Scratch(PathBuf);
impl std::ops::Deref for Scratch {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        &self.0
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let keep = std::env::var_os("ORCH_KEEP_TEST_BUILDS").is_some_and(|v| v == "1");
        if !std::thread::panicking() && !keep {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

fn directory(name: &str) -> Scratch {
    let dir = std::env::temp_dir().join(format!("orch_landline_{}", name));
    fs::create_dir_all(&dir).unwrap();
    Scratch(dir)
}

fn run(dir: &std::path::Path, source: &str, python: &str) -> (String, String) {
    fs::write(dir.join("main.orch"), source).unwrap();
    fs::write(dir.join("impl.py"), python).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_orchestrate"))
        .arg("run")
        .arg(dir.join("main.orch"))
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(output.status.success(), "{}", stderr);
    let stdout = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.starts_with("[orchestrate]"))
        .collect::<Vec<_>>()
        .join("\n");
    (stdout, stderr)
}

#[test]
fn python_state_values_errors_and_crash_recovery() {
    let (stdout, stderr) = run(
        &directory("values"),
        r#"
struct Point { x: int, label: string }
serverlet Python via python(source: "impl.py") {
    on add(n: int) -> int
    on points(items: Point[]) -> Point[]
    on scalar(f: float, b: bool, s: string) -> string
    on fail() -> int
    on crash() -> int
    on clear()
    on_crash error { print("restarted") }
}
orchestrator main() {
    let p = start Python()
    print(to_string(p.add(3)))
    print(to_string(p.add(4)))
    let points = p.points([Point { x: -2, label: "héllo" }])
    print(to_string(points[0].x))
    print(points[0].label)
    print(p.scalar(1.5, true, "世界"))
    print(to_string(p.fail()))
    print(to_string(p.add(1)))
    print(to_string(p.crash()))
    print(to_string(p.add(2)))
    p.clear()
    print(to_string(p.add(1)))
    stop_orch()
}
"#,
        r#"
from dataclasses import dataclass
import os
from orchestratelang import landline
print("startup log")
@dataclass
class Point:
    x: int
    label: str
class Python(landline.Serverlet):
    def __init__(self): self.total = 0
    def add(self, n: int) -> int:
        self.total += n
        return self.total
    def points(self, items: list[Point]) -> list[Point]: return items
    def scalar(self, f: float, b: bool, s: str) -> str:
        print("python log")
        return f"{f}:{b}:{s}"
    def fail(self) -> int: raise ValueError("expected failure")
    def crash(self) -> int: os._exit(7)
    def clear(self) -> None: self.total = 0
landline.serve(Python)
"#,
    );
    assert_eq!(
        stdout,
        "3\n7\n-2\nhéllo\n1.5:True:世界\n0\n8\nrestarted\n0\n2\n1"
    );
    assert!(
        stderr.contains("ValueError: expected failure"),
        "{}",
        stderr
    );
    assert!(stderr.contains("python log"));
    assert!(stderr.contains("startup log"));
}

#[test]
fn python_interface_mismatch_is_reported() {
    let (stdout, stderr) = run(&directory("mismatch"), r#"
serverlet P via python(source: "impl.py") { on expected() -> int }
orchestrator main() {
    let p = start P()
    print(to_string(p.expected()))
    stop_orch()
}
"#, "from orchestratelang import landline\nclass P(landline.Serverlet):\n    def renamed(self) -> int: return 1\nlandline.serve(P)\n");
    assert_eq!(stdout, "0");
    assert!(
        stderr.contains("interface mismatch")
            && stderr.contains("expected()->int")
            && stderr.contains("renamed()->int"),
        "{}",
        stderr
    );
}

#[test]
fn python_module_release_bundle_is_portable() {
    let dir = directory("release_module");
    let module = dir.join("service");
    fs::create_dir_all(module.join("nested")).unwrap();
    fs::write(module.join("module.orch"), "load \"nested/service.orch\"\n").unwrap();
    fs::write(
        module.join("nested/service.orch"),
        r#"serverlet Counter via python(source: "impl.py") {
        on add(n: int) -> int
    }"#,
    )
    .unwrap();
    fs::write(module.join("nested/impl.py"), "from orchestratelang import landline\nclass Counter(landline.Serverlet):\n    def add(self, n: int) -> int: return n + 10\nlandline.serve(Counter)\n").unwrap();
    fs::write(
        dir.join("main.orch"),
        r#"
use module service: "./service"
orchestrator main() {
    let c = start service.Counter()
    print(to_string(c.add(2)))
    stop_orch()
}
"#,
    )
    .unwrap();
    let output_dir = dir.join("dist");
    fs::create_dir_all(&output_dir).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_orchestrate"))
        .arg("build")
        .arg(dir.join("main.orch"))
        .arg("-o")
        .arg(output_dir.join("app"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_file(module.join("nested/impl.py")).unwrap();
    assert!(output_dir
        .join("landline_Counter/orchestratelang/landline.py")
        .exists());
    let output = Command::new(output_dir.join(format!("app{}", std::env::consts::EXE_SUFFIX)))
        .current_dir(std::env::temp_dir())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "12");
}

#[test]
fn python_landline_call_keeps_the_callers_arguments() {
    // A string, an array, and a struct are each used again after the call that
    // carried them; the call copies, it does not move.
    let (stdout, stderr) = run(
        &directory("keeps_arguments"),
        r#"
struct Point { x: int, y: int }
serverlet Echo via python(source: "impl.py") {
    on shout(text: string) -> string
    on total(xs: int[]) -> int
    on sum(p: Point) -> int
}
orchestrator main() {
    let e = start Echo()
    let payload = "hi"
    let nums = [1, 2, 3]
    let pt = Point { x: 4, y: 5 }
    let a = e.shout(payload)
    let b = e.total(nums)
    let c = e.sum(pt)
    print(a + " " + payload)
    print(to_string(b) + " " + to_string(length(nums)))
    print(to_string(c) + " " + to_string(pt.x))
    stop_orch()
}
"#,
        r#"
from dataclasses import dataclass
from orchestratelang import landline
@dataclass
class Point:
    x: int
    y: int
class Echo(landline.Serverlet):
    def shout(self, text: str) -> str: return text + "!"
    def total(self, xs: list[int]) -> int: return sum(xs)
    def sum(self, p: Point) -> int: return p.x + p.y
landline.serve(Echo)
"#,
    );
    assert_eq!(stdout, "hi! hi\n6 3\n9 4", "{}", stderr);
}

#[test]
fn python_landline_keyword_names() {
    // Handler and parameter names that are Rust keywords reach Python by their own
    // spelling; only the generated Rust escapes them.
    let (stdout, stderr) = run(
        &directory("keyword_names"),
        r#"
struct Point { ref: int, type: int }
serverlet Echo via python(source: "impl.py") {
    on move(dyn: string) -> string
    on type(ref: int) -> int
    on loop(impl: Point) -> int
}
orchestrator main() {
    let e = start Echo()
    let static = "s"
    let mut = Point { ref: 1, type: 2 }
    print(e.move(static))
    print(to_string(e.type(mut.ref)))
    print(to_string(e.loop(mut)))
    stop_orch()
}
"#,
        r#"
from dataclasses import dataclass
from orchestratelang import landline
@dataclass
class Point:
    ref: int
    type: int
class Echo(landline.Serverlet):
    def move(self, dyn: str) -> str: return dyn + "!"
    def type(self, ref: int) -> int: return ref + 1
    def loop(self, impl: Point) -> int: return impl.ref + impl.type
landline.serve(Echo)
"#,
    );
    assert_eq!(stdout, "s!\n2\n3", "{}", stderr);
}
