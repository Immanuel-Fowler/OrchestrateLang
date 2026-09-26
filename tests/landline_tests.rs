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

/// A landline whose startup fails is not closed for good. The first start exits before
/// the handshake; calls meanwhile answer with the default, and the next try, after a short
/// backoff, starts it and serves the calls after it.
#[test]
fn python_startup_failure_is_retried() {
    let dir = directory("startup_retry");
    let marker = dir.join("fail_first_start");
    fs::write(&marker, "").unwrap();
    let (stdout, stderr) = run(
        &dir,
        r#"
serverlet Echo via python(source: "impl.py") { on echo(n: int) -> int }
orchestrator main() {
    let e = start Echo()
    print(to_string(e.echo(7)))
    sleep(1500)
    print(to_string(e.echo(8)))
    stop_orch()
}
"#,
        &format!(r#"
import os
from orchestratelang import landline
MARKER = {:?}
if os.path.exists(MARKER):
    os.remove(MARKER)
    raise SystemExit(1)
class Echo(landline.Serverlet):
    def echo(self, n: int) -> int: return n
landline.serve(Echo)
"#, marker.display().to_string()),
    );
    assert_eq!(stdout, "0\n8", "{}", stderr);
    assert!(stderr.contains("startup failed") && stderr.contains("tried again"), "{}", stderr);
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
        .join("app.children/landline_Counter/orchestratelang/landline.py")
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

/// A declaration names the handlers a program calls. The source may define more, in any
/// order; a declared handler whose signature differs, or that the source lacks, is refused
/// at startup with its name.
#[test]
fn python_landline_declares_a_subset_in_any_order() {
    let source = r#"
from orchestratelang import landline
class Echo(landline.Serverlet):
    def describe(self) -> str: return "echo"
    def crunch(self, xs: list[float]) -> list[float]: return [x * 2.0 for x in xs]
    def count(self) -> int: return 3
landline.serve(Echo)
"#;
    let (stdout, stderr) = run(&directory("subset"), r#"
serverlet Echo via python(source: "impl.py") {
    on count() -> int
    on crunch(xs: float[]) -> float[]
}
orchestrator main() {
    let e = start Echo()
    let r = e.crunch([1.0, 2.5])
    print(to_string(r[0]) + " " + to_string(r[1]) + " " + to_string(e.count()))
    stop_orch()
}
"#, source);
    assert_eq!(stdout, "2 5 3", "{}", stderr);

    let (stdout, stderr) = run(&directory("subset_mismatch"), r#"
serverlet Echo via python(source: "impl.py") { on crunch(xs: int[]) -> float[] }
orchestrator main() {
    let e = start Echo()
    print(to_string(length(e.crunch([1, 2]))))
    stop_orch()
}
"#, source);
    assert_eq!(stdout, "0");
    assert!(stderr.contains("interface mismatch: handler 'crunch' is declared as crunch(int[])->float[] but the source defines crunch(float[])->float[]"), "{}", stderr);
}

/// A dataclass is matched to a struct by its fields, not only its name: one whose fields
/// are in another order is refused at startup instead of decoded into the wrong fields. A
/// field named for a Python keyword is written with a trailing underscore.
#[test]
fn python_landline_checks_struct_fields() {
    let program = r#"
struct Pt { a: float, b: int, lambda: int }
serverlet Echo via python(source: "impl.py") { on bump(p: Pt) -> Pt }
orchestrator main() {
    let e = start Echo()
    let q = e.bump(Pt { a: 1.5, b: 7, lambda: 2 })
    print(to_string(q.a) + " " + to_string(q.b) + " " + to_string(q.lambda))
    stop_orch()
}
"#;
    let (stdout, stderr) = run(&directory("struct_fields"), program, r#"
from dataclasses import dataclass
from orchestratelang import landline
@dataclass
class Pt:
    a: float
    b: int
    lambda_: int
class Echo(landline.Serverlet):
    def bump(self, p: Pt) -> Pt: return Pt(a=p.a, b=p.b + 1, lambda_=p.lambda_ * 10)
landline.serve(Echo)
"#);
    assert_eq!(stdout, "1.5 8 20", "{}", stderr);

    let (stdout, stderr) = run(&directory("struct_fields_order"), program, r#"
from dataclasses import dataclass
from orchestratelang import landline
@dataclass
class Pt:
    b: int
    a: float
    lambda_: int
class Echo(landline.Serverlet):
    def bump(self, p: Pt) -> Pt: return Pt(b=p.b + 1, a=p.a, lambda_=p.lambda_)
landline.serve(Echo)
"#);
    assert_eq!(stdout, "0 0 0", "{}", stderr);
    assert!(stderr.contains("but the source defines bump(Pt{b:int,a:float,lambda:int})"), "{}", stderr);
}

fn python_available_module(module: &str) -> bool {
    let python = std::env::var_os("ORCH_PYTHON").unwrap_or_else(|| "python3".into());
    Command::new(python).arg("-c").arg(format!("import {module}")).output().is_ok_and(|o| o.status.success())
}

/// An array of floats, ints or bools crosses as one block. The block path must write the
/// same bytes as the per-element path, read the same values back, and refuse what it
/// refused, with the same error; a numpy array annotation gets a writable numpy array.
#[test]
fn python_array_blocks_match_the_element_path() {
    let script = r#"
import struct
from orchestratelang import landline as L

def both(ty, value):
    L._BLOCKS = True
    fast = L._encode(ty, value)
    L._BLOCKS = False
    slow = L._encode(ty, value)
    assert fast == slow, (ty, value)
    r = L._Reader(slow); want = r.value(ty); r.finish()
    L._BLOCKS = True
    r = L._Reader(fast); got = r.value(ty); r.finish()
    assert repr(got) == repr(want) and type(got) is list, (got, want)

both(list[float], [0.0, -0.0, 1.5, float("inf"), float("-inf"), float("nan"), 5e-324, 1.7976931348623157e308])
both(list[int], [0, -1, 2**63 - 1, -2**63, 42])
both(list[bool], [True, False, True])
both(list[float], [])
both(list[list[float]], [[1.0, 2.0], [], [3.5]])
both(list[list[int]], [[1], [2, 3]])

def fails(ty, value):
    outcomes = []
    for blocks in (True, False):
        L._BLOCKS = blocks
        try:
            L._encode(ty, value)
            outcomes.append(None)
        except TypeError as error:
            outcomes.append(str(error))
        except (OverflowError, struct.error):
            outcomes.append("out of range")
    L._BLOCKS = True
    assert outcomes[0] is not None and outcomes[0] == outcomes[1], (ty, value, outcomes)

fails(list[float], [1.0, 2])
fails(list[float], [1.0, True])
fails(list[int], [1, 2.0])
fails(list[int], [1, True])
fails(list[int], [2**63])
fails(list[bool], [True, 1])
fails(list[float], (1.0, 2.0))

for blocks in (True, False):
    L._BLOCKS = blocks
    for payload, ty in ((struct.pack("<I", 2) + bytes([1, 2]), list[bool]), (struct.pack("<I", 3) + bytes(16), list[float])):
        try:
            L._Reader(payload).value(ty)
            raise SystemExit(f"accepted {payload!r} as {ty}")
        except ValueError:
            pass
L._BLOCKS = True

try:
    import numpy
    from numpy.typing import NDArray
except ImportError:
    numpy = None
if numpy is not None:
    assert L._type_name(NDArray[numpy.float64]) == "float[]"
    assert L._type_name(NDArray[numpy.int64]) == "int[]"
    assert L._type_name(NDArray[numpy.bool_]) == "bool[]"
    try:
        L._type_name(NDArray[numpy.float32])
        raise SystemExit("accepted float32")
    except TypeError:
        pass
    wire = L._encode(list[float], [1.0, -2.5, 3.0])
    assert L._encode(NDArray[numpy.float64], numpy.array([1.0, -2.5, 3.0])) == wire
    assert L._encode(list[float], numpy.array([1.0, -2.5, 3.0])) == wire
    got = L._Reader(wire).value(NDArray[numpy.float64])
    assert type(got) is numpy.ndarray and got.dtype == numpy.float64 and list(got) == [1.0, -2.5, 3.0]
    got[0] = 9.0
    flags = L._Reader(L._encode(list[bool], [True, False])).value(NDArray[numpy.bool_])
    assert flags.dtype == numpy.bool_ and list(flags) == [True, False]
    grid = numpy.array([[1.0, 2.0], [3.0, 4.0]])
    assert L._encode(list[list[float]], grid) == L._encode(list[list[float]], [[1.0, 2.0], [3.0, 4.0]])
    for wrong in (numpy.array([1, 2]), numpy.zeros((2, 2))):
        try:
            L._encode(list[float], wrong)
            raise SystemExit(f"accepted {wrong!r}")
        except TypeError:
            pass
print("ok")
"#;
    let python = std::env::var_os("ORCH_PYTHON").unwrap_or_else(|| "python3".into());
    let output = Command::new(python)
        .env("PYTHONPATH", std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("sdk/python"))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .arg("-c")
        .arg(script)
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "ok", "{}", String::from_utf8_lossy(&output.stderr));
}

/// Arrays of each block type, nested arrays, and arrays inside a struct cross a landline
/// both ways; so does a numpy array, for a handler annotated with one.
#[test]
fn python_landline_carries_arrays() {
    let program = r#"
struct Batch { xs: float[], ids: int[], flags: bool[] }
serverlet Numbers via python(source: "impl.py") {
    on scale(xs: float[], by: float) -> float[]
    on shift(ids: int[]) -> int[]
    on flip(flags: bool[]) -> bool[]
    on rows(grid: float[][]) -> float[][]
    on batch(b: Batch) -> Batch
}
orchestrator main() {
    let n = start Numbers()
    let xs = n.scale([1.0, -2.5, 0.25], 2.0)
    print(to_string(xs[0]) + " " + to_string(xs[1]) + " " + to_string(xs[2]))
    let ids = n.shift([-5, 0, 1000000000000])
    print(to_string(ids[0]) + " " + to_string(ids[2]) + " " + to_string(length(ids)))
    let flags = n.flip([true, false, false])
    print(to_string(flags[0]) + " " + to_string(flags[1]))
    let grid = n.rows([[1.0, 2.0], [], [3.0]])
    print(to_string(length(grid)) + " " + to_string(length(grid[1])) + " " + to_string(grid[2][0]))
    let b = n.batch(Batch { xs: [0.5], ids: [7, 8], flags: [true] })
    print(to_string(b.xs[0]) + " " + to_string(b.ids[1]) + " " + to_string(b.flags[0]))
    stop_orch()
}
"#;
    let expected = "2 -5 0.5\n-4 1000000000001 3\nfalse true\n3 0 30\n1.5 16 false";
    let (stdout, stderr) = run(&directory("arrays"), program, r#"
from dataclasses import dataclass
from orchestratelang import landline
@dataclass
class Batch:
    xs: list[float]
    ids: list[int]
    flags: list[bool]
class Numbers(landline.Serverlet):
    def scale(self, xs: list[float], by: float) -> list[float]: return [x * by for x in xs]
    def shift(self, ids: list[int]) -> list[int]: return [i + 1 for i in ids]
    def flip(self, flags: list[bool]) -> list[bool]: return [not f for f in flags]
    def rows(self, grid: list[list[float]]) -> list[list[float]]: return [[x * 10.0 for x in r] for r in grid]
    def batch(self, b: Batch) -> Batch: return Batch([x + 1.0 for x in b.xs], [i * 2 for i in b.ids], [not f for f in b.flags])
landline.serve(Numbers)
"#);
    assert_eq!(stdout, expected, "{}", stderr);
    if !python_available_module("numpy") {
        eprintln!("numpy is not installed: the numpy-annotated half of this test is skipped");
        return;
    }
    let (stdout, stderr) = run(&directory("arrays_numpy"), program, r#"
from dataclasses import dataclass
import numpy
from numpy.typing import NDArray
from orchestratelang import landline
@dataclass
class Batch:
    xs: NDArray[numpy.float64]
    ids: NDArray[numpy.int64]
    flags: NDArray[numpy.bool_]
class Numbers(landline.Serverlet):
    def scale(self, xs: NDArray[numpy.float64], by: float) -> NDArray[numpy.float64]:
        xs *= by
        return xs
    def shift(self, ids: NDArray[numpy.int64]) -> NDArray[numpy.int64]: return ids + 1
    def flip(self, flags: NDArray[numpy.bool_]) -> NDArray[numpy.bool_]: return ~flags
    def rows(self, grid: list[NDArray[numpy.float64]]) -> list[NDArray[numpy.float64]]: return [r * 10.0 for r in grid]
    def batch(self, b: Batch) -> Batch: return Batch(b.xs + 1.0, b.ids * 2, ~b.flags)
landline.serve(Numbers)
"#);
    assert_eq!(stdout, expected, "{}", stderr);
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

/// A `result<T>` handler tells its caller when a call failed. What the handler raised is
/// its `err`; a process that died during the call, a call that missed its budget, and a
/// call made while the process could not start are `err` too, each saying which. A zero
/// the handler returned is `ok(0)`, so the two cannot be confused.
#[test]
fn python_result_handlers_say_a_call_failed() {
    let dir = directory("result_failures");
    let marker = dir.join("fail_first_start");
    fs::write(&marker, "").unwrap();
    let (stdout, stderr) = run(
        &dir,
        r#"
serverlet Strategy via python(source: "impl.py") {
    on decide(price: int, size: int) -> result<int>
    on slow() -> result<int>
    on crash() -> result<int>
    on count() -> result<int>
}
serverlet Hurried via python(source: "impl.py", budget: "200ms") {
    on decide(price: int, size: int) -> result<int>
    on slow() -> result<int>
    on crash() -> result<int>
    on count() -> result<int>
}
fn describe(answer: result<int>) -> string {
    match answer {
        result::Ok(v) => "ok " + to_string(v)
        result::Err(e) => "err: " + e
    }
}
orchestrator main() {
    let s = start Strategy()
    // The first start exits before its handshake.
    print(describe(s.count()))
    sleep(1500)
    print(describe(s.decide(0, 4)))
    print(describe(s.decide(10, 0 - 1)))
    print(describe(s.crash()))
    // A fresh process, whose state starts over.
    print(describe(s.count()))
    let h = start Hurried()
    sleep(1500)
    print(describe(h.slow()))
    stop_orch()
}
"#,
        &format!(r#"
import os
import time
from orchestratelang import landline
MARKER = {:?}
if os.path.exists(MARKER):
    os.remove(MARKER)
    raise SystemExit(1)
class Strategy(landline.Serverlet):
    def __init__(self): self.calls = 0
    def decide(self, price: int, size: int) -> int:
        self.calls += 1
        if size < 0:
            raise ValueError("size cannot be negative")
        return price // size
    def slow(self) -> int:
        time.sleep(3)
        return 1
    def crash(self) -> int: os._exit(7)
    def count(self) -> int: return self.calls
landline.serve(Strategy)
"#, marker.display().to_string()),
    );
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 6, "{}\n{}", stdout, stderr);
    assert!(lines[0].starts_with("err: landline 'Strategy' is not running: its startup failed ("), "{}\n{}", stdout, stderr);
    assert_eq!(
        &lines[1..],
        [
            "ok 0",
            "err: ValueError: size cannot be negative",
            "err: landline 'Strategy' crashed during a call to 'crash': process exited",
            "ok 0",
            "err: landline 'Hurried' handler 'slow' missed its budget of 200ms",
        ],
        "{}",
        stderr
    );
}

/// A map crosses to Python as a dict and back, in either direction, keyed by any of the
/// key types; the host reads it in key order whatever order Python built it in.
#[test]
fn python_landline_carries_maps() {
    let (stdout, stderr) = run(
        &directory("maps"),
        r#"
serverlet Stock via python(source: "impl.py") {
    on restock(levels: map<string, int>, extra: int) -> map<string, int>
    on flags(ids: int[]) -> map<int, bool>
}
orchestrator main() {
    let stock = start Stock()
    let after = stock.restock(map { "pear": 2, "apple": 5 }, 3)
    for name in keys(after) { print(name + " " + to_string(after[name])) }
    let odd = stock.flags([3, 1, 2])
    print(to_string(odd[1]) + " " + to_string(odd[2]) + " " + to_string(length(odd)))
    stop_orch()
}
"#,
        r#"
from orchestratelang import landline
class Stock(landline.Serverlet):
    def restock(self, levels: dict[str, int], extra: int) -> dict[str, int]:
        out = {name: count + extra for name, count in levels.items()}
        out["zucchini"] = extra
        return out
    def flags(self, ids: list[int]) -> dict[int, bool]:
        return {i: i % 2 == 1 for i in ids}
landline.serve(Stock)
"#,
    );
    assert_eq!(stdout, "apple 8\npear 5\nzucchini 3\ntrue false 3", "{}", stderr);
}
