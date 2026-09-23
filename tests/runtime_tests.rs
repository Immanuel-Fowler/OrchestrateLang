/// Runtime integration tests: compile .orch source to a binary, run it, assert stdout.
///
/// These tests are slower (~5–15s each) because they invoke cargo build internally.
/// Run selectively with: cargo test --test runtime_tests
use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn orchestrate_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_orchestrate"))
}

/// A test's build directory under the system temp dir. A plain case leaves about
/// 140 MB behind and a sandbox case 400–600 MB, so it is removed when the test passes;
/// a failing test keeps it for inspection, and `ORCH_KEEP_TEST_BUILDS=1` keeps every
/// one, which makes a rerun fast.
struct Scratch(PathBuf);
impl Scratch {
    fn new(test_name: &str) -> Scratch {
        let path = std::env::temp_dir().join(format!("orch_runtime_{}", test_name));
        fs::create_dir_all(&path).unwrap();
        Scratch(path)
    }
    fn path(&self) -> &std::path::Path {
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

/// Write source to a temp dir, compile+run it, return stdout. The build directory is
/// gone when this returns; a test that needs it afterwards uses `run_orch_keeping`.
fn run_orch(test_name: &str, source: &str) -> String {
    run_orch_keeping(test_name, source).0
}

/// `run_orch`, handing back the build directory's guard so the test can use what the
/// build left there; it is removed when the guard drops.
fn run_orch_keeping(test_name: &str, source: &str) -> (String, Scratch) {
    let scratch = Scratch::new(test_name);
    let src_file = scratch.path().join("test.orch");
    fs::write(&src_file, source).unwrap();

    let out = Command::new(orchestrate_bin())
        .args(["run", src_file.to_str().unwrap()])
        .output()
        .expect("failed to run orchestrate");

    let raw_stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    assert!(out.status.success(),
        "Program '{}' failed:\nstdout: {}\nstderr: {}", test_name, raw_stdout, stderr);
    // Filter out orchestrate's own progress lines so we only see program output.
    let stdout = raw_stdout.lines()
        .filter(|l| !l.starts_with("[orchestrate]"))
        .collect::<Vec<_>>()
        .join("\n");
    (stdout, scratch)
}

/// True when `program` runs; FFI tests for optional toolchains skip otherwise.
fn has_tool(program: &str, version_arg: &str) -> bool {
    let found = Command::new(program).arg(version_arg).output().is_ok();
    if !found {
        eprintln!("skipping: `{}` is not on PATH", program);
    }
    found
}

/// Writes a `load_foreign` module (source + sidecar) into the test's temp dir.
fn write_foreign_module(test_name: &str, module: &str, language: &str, file: &str, source: &str, sidecar: &str) {
    let dir = std::env::temp_dir().join(format!("orch_runtime_{}", test_name)).join(module);
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("module.orch"), format!("load_foreign \"{}\" \"./{}\"\n", language, file)).unwrap();
    fs::write(dir.join(file), source).unwrap();
    fs::write(dir.join(file).with_extension("orch_ffi"), sidecar).unwrap();
}

/// The list functions are generic over the element type: strings, floats, and nested
/// arrays go through the same sidecar signatures the int versions used to own.
#[test]
fn runtime_stdlib_lists_are_generic() {
    let out = run_orch("stdlib_generic_lists", r#"
use module lists: "lists"

orchestrator main() {
    let names = lists.reverse(["b", "a", "c"])
    print(names[0])
    let sorted = lists.sort(["pear", "apple"])
    print(sorted[0])
    match lists.head([2.5, 1.0]) {
        option::Some(v) => print(to_string(v))
        option::None => print("none")
    }
    let flat = lists.flatten([["x"], ["y", "z"]])
    print(to_string(length(flat)))
    print(to_string(length(lists.unique([1, 1, 2]))))
    print(to_string(lists.sum_float([0.5, 0.25])))
    print(to_string(lists.max([4, 9, 2])))
    stop_orch()
}
"#);
    assert_eq!(out.trim(), "c\napple\n2.5\n3\n2\n0.75\n9");
}

/// A result's error can be any type: an enum error is matched on, propagated with `?`
/// between functions that share it, and caught with `catch e: Failure`; a plain
/// `catch e` still binds a string.
#[test]
fn runtime_result_with_typed_errors() {
    let out = run_orch("result_typed_errors", r#"
enum Failure {
    DivideByZero,
    TooLarge(int),
}

fn divide(a: int, b: int) -> result<int, Failure> {
    if b == 0 {
        err(Failure::DivideByZero)
    } else {
        if a > 100 { err(Failure::TooLarge(a)) } else { ok(a / b) }
    }
}

fn describe(r: result<int, Failure>) -> string {
    match r {
        result::Ok(v) => "ok " + to_string(v)
        result::Err(e) => match e {
            Failure::DivideByZero => "divide by zero"
            Failure::TooLarge(n) => "too large: " + to_string(n)
        }
    }
}

fn twice(a: int, b: int) -> result<int, Failure> {
    let half = divide(a, b)?
    ok(half * 2)
}

orchestrator main() {
    print(describe(divide(10, 2)))
    print(describe(divide(1, 0)))
    print(describe(twice(500, 5)))
    print(describe(twice(50, 5)))
    let recovered = try { divide(7, 0)? } catch e: Failure { 0 - 1 }
    print(to_string(recovered))
    let plain = try { parse_int("x")? } catch e { 42 }
    print(to_string(plain))
    stop_orch()
}
"#);
    assert_eq!(out.trim(), "ok 5\ndivide by zero\ntoo large: 500\nok 20\n-1\n42");
}

/// Strings cross the C ABI under one ownership rule, and a handle is an opaque native
/// object that the sidecar's `drop` function releases when its last owner drops: here
/// when `count_twice` returns, before its result is printed.
#[test]
fn runtime_ffi_c_strings_and_handles() {
    write_foreign_module("ffi_c_strings_handles", "native", "c", "native.c", r#"
#include <stdlib.h>
#include <string.h>
#include <stdio.h>
char* greet(const char* name) {
    size_t n = strlen(name);
    char* out = malloc(n + 7);
    memcpy(out, "hello ", 6);
    memcpy(out + 6, name, n + 1);
    return out;
}
long long text_length(const char* s) { return (long long)strlen(s); }
struct counter { long long value; };
void* make_counter(long long start) { struct counter* c = malloc(sizeof *c); c->value = start; return c; }
long long bump(void* c) { struct counter* k = c; k->value += 1; return k->value; }
void release_counter(void* c) { printf("released %lld\n", ((struct counter*)c)->value); fflush(stdout); free(c); }
"#,
        "greet(name: string) -> string\ntext_length(s: string) -> int\nmake_counter(initial: int) -> handle\nbump(c: handle) -> int\ndrop release_counter(c: handle)\n");
    let out = run_orch("ffi_c_strings_handles", r#"
use module native: "./native"

fn count_twice(initial: int) -> int {
    let c = native.make_counter(initial)
    let a = native.bump(c)
    let b = native.bump(c)
    return a + b
}

orchestrator main() {
    print(native.greet("world"))
    print(to_string(native.text_length("héllo")))
    print(to_string(count_twice(10)))
    print("after")
    stop_orch()
}
"#);
    assert_eq!(out.trim(), "hello world\n6\nreleased 12\n23\nafter");
}

#[test]
fn runtime_ffi_zig() {
    if !has_tool("zig", "version") { return; }
    write_foreign_module("ffi_zig", "zmath", "zig", "math.zig",
        "export fn add(a: i64, b: i64) i64 { return a + b; }\nexport fn half(x: f64) f64 { return x / 2.0; }\nexport fn is_even(n: i64) bool { return @mod(n, 2) == 0; }\n",
        "add(a: int, b: int) -> int\nhalf(x: float) -> float\nis_even(n: int) -> bool\n");
    let out = run_orch("ffi_zig", r#"
use module zmath: "./zmath"
let worker = automatic {
    print(to_string(zmath.add(2, 3)))
    print(to_string(zmath.half(5.0)))
    print(to_string(zmath.is_even(10)))
    stop_orch()
}
orchestrator main(procs: process[worker]) { }
"#);
    assert_eq!(out.trim(), "5\n2.5\ntrue");
}

#[test]
fn runtime_ffi_swift() {
    if !has_tool("swiftc", "--version") { return; }
    write_foreign_module("ffi_swift", "smath", "swift", "math.swift",
        "@_cdecl(\"add\")\npublic func add(_ a: Int64, _ b: Int64) -> Int64 { a + b }\n@_cdecl(\"half\")\npublic func half(_ x: Double) -> Double { x / 2 }\n@_cdecl(\"is_even\")\npublic func isEven(_ n: Int64) -> Bool { n % 2 == 0 }\n",
        "add(a: int, b: int) -> int\nhalf(x: float) -> float\nis_even(n: int) -> bool\n");
    let out = run_orch("ffi_swift", r#"
use module smath: "./smath"
let worker = automatic {
    print(to_string(smath.add(2, 3)))
    print(to_string(smath.half(5.0)))
    print(to_string(smath.is_even(10)))
    stop_orch()
}
orchestrator main(procs: process[worker]) { }
"#);
    assert_eq!(out.trim(), "5\n2.5\ntrue");
}

/// Two Zig and two Swift libraries plus C in one program: library names must not collide.
#[test]
fn runtime_ffi_mixed_c_zig_swift() {
    if !has_tool("zig", "version") || !has_tool("swiftc", "--version") { return; }
    let name = "ffi_mixed_c_zig_swift";
    write_foreign_module(name, "c1", "c", "math.c", "long long c_twice(long long n) { return n * 2; }\n", "c_twice(n: int) -> int\n");
    write_foreign_module(name, "z1", "zig", "math.zig", "export fn z_one(n: i64) i64 { return n + 1; }\n", "z_one(n: int) -> int\n");
    write_foreign_module(name, "z2", "zig", "math.zig", "export fn z_two(n: i64) i64 { return n + 2; }\n", "z_two(n: int) -> int\n");
    write_foreign_module(name, "s1", "swift", "math.swift", "@_cdecl(\"s_ten\")\npublic func sTen(_ n: Int64) -> Int64 { n * 10 }\n", "s_ten(n: int) -> int\n");
    write_foreign_module(name, "s2", "swift", "math.swift", "@_cdecl(\"s_hundred\")\npublic func sHundred(_ n: Int64) -> Int64 { n * 100 }\n", "s_hundred(n: int) -> int\n");
    let out = run_orch(name, r#"
use module c1: "./c1"
use module z1: "./z1"
use module z2: "./z2"
use module s1: "./s1"
use module s2: "./s2"
let worker = automatic {
    print(to_string(c1.c_twice(z1.z_one(z2.z_two(s1.s_ten(s2.s_hundred(1)))))))
    stop_orch()
}
orchestrator main(procs: process[worker]) { }
"#);
    assert_eq!(out.trim(), "2006");
}

#[test]
fn runtime_basic_task_add() {
    let src = r#"
task add(a: int, b: int) -> int {
    return a + b
}
orchestrator main() {
    let result = add(3, 4)
    print(to_string(result))
    stop_orch()
}
"#;
    let stdout = run_orch("basic_task_add", src);
    assert_eq!(stdout.trim(), "7");
}

#[test]
fn runtime_pipeline() {
    let src = r#"
fn square(x: int) -> int {
    return x * x
}
orchestrator main() {
    let r = 5 |> square
    print(to_string(r))
    stop_orch()
}
"#;
    let stdout = run_orch("pipeline", src);
    assert_eq!(stdout.trim(), "25");
}

#[test]
fn runtime_string_concat() {
    let src = r#"
orchestrator main() {
    let greeting = "Hello" + ", " + "World!"
    print(greeting)
    stop_orch()
}
"#;
    let stdout = run_orch("string_concat", src);
    assert_eq!(stdout.trim(), "Hello, World!");
}

#[test]
fn runtime_if_else() {
    let src = r#"
fn bigger(a: int, b: int) -> string {
    if a > b {
        return "a"
    } else {
        return "b"
    }
}
orchestrator main() {
    print(bigger(10, 3))
    print(bigger(1, 9))
    stop_orch()
}
"#;
    let stdout = run_orch("if_else", src);
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines[0], "a");
    assert_eq!(lines[1], "b");
}

#[test]
fn runtime_struct_field_access() {
    let src = r#"
struct Point {
    x: int,
    y: int,
}
orchestrator main() {
    let p = Point { x: 10, y: 20 }
    print(to_string(p.x))
    print(to_string(p.y))
    stop_orch()
}
"#;
    let stdout = run_orch("struct_field_access", src);
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines[0], "10");
    assert_eq!(lines[1], "20");
}

#[test]
fn runtime_while_loop() {
    let src = r#"
orchestrator main() {
    let i = 0
    while i < 3 {
        print(to_string(i))
        i = i + 1
    }
    stop_orch()
}
"#;
    let stdout = run_orch("while_loop", src);
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines, vec!["0", "1", "2"]);
}

#[test]
fn runtime_unary_minus_and_modulo() {
    let src = r#"
orchestrator main() {
    let a = -7
    let b = 10 % 3
    let c = -2.5 * 2.0
    let d = -a % 4
    print(to_string(a))
    print(to_string(b))
    print(to_string(c))
    print(to_string(d))
    print(to_string(3 - -1))
    stop_orch()
}
"#;
    let stdout = run_orch("unary_minus_modulo", src);
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines, vec!["-7", "1", "-5", "3", "4"]);
}

#[test]
fn runtime_array_indexing() {
    let src = r#"
orchestrator main() {
    let nums = [10, 20, 30]
    let words = ["a", "b", "c"]
    let grid = [[1, 2], [3, 4]]
    let i = 2
    print(to_string(nums[0] + nums[i]))
    nums[1] = 99
    print(to_string(nums[1]))
    print(words[length(words) - 1])
    print(to_string(grid[1][0]))
    print(to_string(-nums[0]))
    stop_orch()
}
"#;
    let stdout = run_orch("array_indexing", src);
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines, vec!["40", "99", "c", "3", "-10"]);
}

#[test]
fn runtime_secret_serverlet_structs_and_arrays() {
    let src = r#"
struct Point {
    x: int,
    y: float,
}
serverlet Geo secret {
    let total = 0
    on shift(p: Point, dx: int) -> Point {
        total = total + dx
        return Point { x: p.x + dx, y: p.y * 2.0 }
    }
    on sum(values: int[]) -> int {
        let s = 0
        for v in values {
            s = s + v
        }
        return s
    }
    on names() -> string[] {
        return ["a", "b"]
    }
}
orchestrator main() {
    let g = start Geo()
    let q = g.shift(Point { x: 1, y: 1.5 }, 4)
    print(to_string(q.x))
    print(to_string(q.y))
    print(to_string(g.sum([1, 2, 3])))
    let ns = g.names()
    print(ns[1])
    stop_orch()
}
"#;
    let stdout = run_orch("secret_structs_arrays", src);
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines, vec!["5", "3", "6", "b"]);
}

#[test]
fn runtime_secret_serverlet_handler_panic_is_isolated() {
    // A panicking handler sends an ERROR reply; the caller gets a default value and the
    // serverlet keeps its state and keeps serving.
    let src = r#"
serverlet Calc secret {
    let calls = 0
    on div(a: int, b: int) -> int {
        calls = calls + 1
        return a / b
    }
    on count() -> int {
        return calls
    }
}
orchestrator main() {
    let c = start Calc()
    print(to_string(c.div(10, 2)))
    print(to_string(c.div(1, 0)))
    print(to_string(c.div(9, 3)))
    print(to_string(c.count()))
    stop_orch()
}
"#;
    let stdout = run_orch("secret_handler_panic", src);
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines, vec!["5", "0", "3", "3"]);
}

#[test]
fn runtime_clock_micros_measures_elapsed_time() {
    let src = r#"
orchestrator main() {
    let began = clock_micros()
    sleep(20)
    let elapsed = clock_micros() - began
    print(to_string(elapsed >= 15000 && elapsed < 5000000))
    stop_orch()
}
"#;
    assert_eq!(run_orch("clock_micros", src).trim(), "true");
}

#[test]
fn benchmark_programs_typecheck() {
    let out = Command::new(orchestrate_bin())
        .args(["check", "benchmarks/landline_latency/main.orch"])
        .output()
        .expect("failed to run orchestrate");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
}

#[test]
fn runtime_sandbox_serverlet_runs_in_wasm_with_state() {
    // A sandboxed serverlet keeps state between calls, carries strings both ways, and
    // does it from inside the guest: the artifact must exist and the answers must be the
    // ones the handlers compute.
    let test_name = "sandbox_wasm";
    let _ = fs::remove_dir_all(std::env::temp_dir().join(format!("orch_runtime_{}", test_name)));
    let scratch = Scratch::new(test_name);
    let tmp = scratch.path();
    let src_file = tmp.join("test.orch");
    fs::write(&src_file, r#"
serverlet Plugin sandbox(memory_limit: "64mb", timeout: "5s") {
    let count = 0
    on tally(n: int) -> int {
        count = count + n
        return count
    }
    on shout(text: string) -> string { return text + "!" }
}
orchestrator main() {
    let p = start Plugin()
    print(to_string(p.tally(7)))
    print(to_string(p.tally(5)))
    print(p.shout("quiet"))
    stop_orch()
}
"#).unwrap();

    let out = Command::new(orchestrate_bin())
        .args(["run", src_file.to_str().unwrap()])
        .output()
        .expect("failed to run orchestrate");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "compile/run failed:\nstdout: {}\nstderr: {}", stdout, stderr);

    let wasm = tmp.join(".orch_cache/sandbox_Plugin/target/wasm32-wasip1/release/sandbox_Plugin.wasm");
    assert!(wasm.exists(), "expected wasm guest artifact at {:?}", wasm);

    let lines: Vec<&str> = stdout.lines().filter(|l| !l.starts_with("[orchestrate]")).collect();
    // 12, not 5, is the proof that the guest kept its state between the two calls.
    assert_eq!(lines, vec!["7", "12", "quiet!"], "stdout: {}", stdout);
    // Containment is real now, so nothing should claim otherwise.
    assert!(!stderr.contains("WITHOUT ISOLATION"), "stale no-isolation warning: {}", stderr);
}

#[test]
fn runtime_sandbox_contains_a_runaway_guest() {
    // The two limits a sandbox promises: a guest that will not return is interrupted, and
    // a guest that allocates without bound is stopped. Both leave the host running and
    // the serverlet usable, because a trapped guest is replaced rather than kept.
    let test_name = "sandbox_limits";
    let _ = fs::remove_dir_all(std::env::temp_dir().join(format!("orch_runtime_{}", test_name)));
    let scratch = Scratch::new(test_name);
    let tmp = scratch.path();
    let src_file = tmp.join("test.orch");
    fs::write(&src_file, r#"
serverlet Hostile sandbox(memory_limit: "16mb", timeout: "300ms") {
    let calls = 0
    on spin() -> int {
        while true { calls = calls + 1 }
        return calls
    }
    on devour() -> int {
        let blob = []
        while true { append(blob, 1) }
        return 1
    }
    on behave(n: int) -> int { return n + 1 }
}
// The timeout is a duration from the call's own start: a runaway call is never stopped
// before it, wherever in the epoch's 10 ms tick the call began. That bound holds however
// loaded the machine is, so it is safe to check with a clock.
serverlet Timed sandbox(memory_limit: "8mb", timeout: "40ms") {
    on spin() -> int {
        while true { }
        return 1
    }
}
orchestrator main() {
    let h = start Hostile()
    print(to_string(h.behave(1)))
    print(to_string(h.spin()))
    print(to_string(h.behave(2)))
    print(to_string(h.devour()))
    print(to_string(h.behave(3)))
    print("alive")
    let t = start Timed()
    for i in range(4) {
        sleep(i * 3)
        let started = clock_micros()
        let r = t.spin()
        let took = clock_micros() - started
        if took >= 40000 { print("not early") } else { print("stopped early, after {took} us") }
    }
    stop_orch()
}
"#).unwrap();

    let out = Command::new(orchestrate_bin())
        .args(["run", src_file.to_str().unwrap()])
        .output()
        .expect("failed to run orchestrate");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "run failed:\nstdout: {}\nstderr: {}", stdout, stderr);

    let lines: Vec<&str> = stdout.lines().filter(|l| !l.starts_with("[orchestrate]")).collect();
    // The two contained calls answer with a default; the calls around them still work,
    // which is what "the host survives" has to mean.
    assert_eq!(lines, vec!["2", "0", "3", "0", "4", "alive", "not early", "not early", "not early", "not early"], "stdout: {}\nstderr: {}", stdout, stderr);
    assert!(stderr.contains("ran past its timeout"), "expected a timeout report: {}", stderr);
    assert!(stderr.contains("memory allocation") || stderr.contains("stopped itself"),
        "expected the guest's own failure reported: {}", stderr);
    // No stopwatch: a loop that was not stopped never returns, so a bound checked after
    // the run could not catch it, and a run that returns has already shown the loop was
    // stopped — by its timeout, as stderr says. A wall-clock bound only measured how busy
    // the machine was: under load, compiling the guest alone took minutes.
}

#[test]
fn runtime_secret_serverlet_state_persists() {
    // The body runs in a separate process; state must survive across calls,
    // proving the child is long-lived and reused.
    let src = r#"
serverlet Counter secret {
    let count = 0
    on add(n: int) -> int {
        count = count + n
        return count
    }
}
orchestrator main() {
    let c = start Counter()
    print(to_string(c.add(5)))
    print(to_string(c.add(10)))
    print(to_string(c.add(2)))
    stop_orch()
}
"#;
    let stdout = run_orch("secret_state", src);
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines, vec!["5", "15", "17"]);
}

#[test]
fn runtime_secret_serverlet_string_roundtrip() {
    let src = r#"
serverlet Greeter secret {
    on greet(who: string) -> string {
        return "hello " + who
    }
}
orchestrator main() {
    let g = start Greeter()
    print(g.greet("world"))
    print(g.greet("orchestrate"))
    stop_orch()
}
"#;
    let stdout = run_orch("secret_string", src);
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines, vec!["hello world", "hello orchestrate"]);
}

#[test]
fn runtime_secret_serverlet_float_and_bool() {
    let src = r#"
serverlet Calc secret {
    on scale(x: float) -> float {
        return x * 2.0
    }
    on positive(n: int) -> bool {
        return n > 0
    }
}
orchestrator main() {
    let c = start Calc()
    print(to_string(c.scale(1.5)))
    print(to_string(c.positive(3)))
    stop_orch()
}
"#;
    let stdout = run_orch("secret_float_bool", src);
    let lines: Vec<&str> = stdout.trim().lines().collect();
    assert_eq!(lines[0], "3");
    assert_eq!(lines[1], "true");
}

#[test]
fn runtime_regression_programs() {
    // Every program in tests/programs/ must compile and exit successfully, and one with a
    // `<name>.expected` beside it must print exactly that: a program that runs and prints
    // the wrong thing is the failure an exit status cannot see.
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/programs");
    let mut programs: Vec<PathBuf> = fs::read_dir(&dir).unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().map_or(false, |ext| ext == "orch"))
        .collect();
    programs.sort();
    assert!(!programs.is_empty(), "no programs found in {:?}", dir);

    for program in programs {
        let out = Command::new(orchestrate_bin())
            .args(["run", program.to_str().unwrap()])
            .output()
            .expect("failed to run orchestrate");
        assert!(out.status.success(),
            "{:?} failed:\nstdout: {}\nstderr: {}", program,
            String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        if let Ok(expected) = fs::read_to_string(program.with_extension("expected")) {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let printed: Vec<&str> = stdout.lines().filter(|l| !l.starts_with("[orchestrate]")).collect();
            assert_eq!(printed, expected.lines().collect::<Vec<_>>(), "{:?} printed the wrong thing", program);
        }
    }
    // These programs build beside their sources, into one shared cache of about 4 GB;
    // a passing run leaves nothing in the source tree.
    if !std::env::var_os("ORCH_KEEP_TEST_BUILDS").is_some_and(|v| v == "1") {
        let _ = fs::remove_dir_all(dir.join(".orch_cache"));
    }
}

#[test]
fn runtime_secret_protocol_frames() {
    use std::io::{Read, Write};
    use std::process::Stdio;
    let source = r#"
serverlet Protocol secret {
    on echo(n: int) -> int { return n }
}
orchestrator main() {
    let p = start Protocol()
    print(to_string(p.echo(7)))
    stop_orch()
}
"#;
    let (_, scratch) = run_orch_keeping("protocol_frames", source);
    let binary = scratch.path().join(".orch_cache/target/debug")
        .join(format!("secret_Protocol{}", std::env::consts::EXE_SUFFIX));
    let mut child = Command::new(&binary).stdin(Stdio::piped()).stdout(Stdio::piped()).spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = child.stdout.take().unwrap();
    fn read_frame(r: &mut impl Read) -> (u8, u32, Vec<u8>) {
        let mut len = [0; 4];
        r.read_exact(&mut len).unwrap();
        let mut frame = vec![0; u32::from_le_bytes(len) as usize];
        r.read_exact(&mut frame).unwrap();
        (frame[0], u32::from_le_bytes(frame[1..5].try_into().unwrap()), frame[5..].to_vec())
    }
    fn write_frame(w: &mut impl Write, kind: u8, id: u32, payload: &[u8]) {
        w.write_all(&((payload.len() + 5) as u32).to_le_bytes()).unwrap();
        w.write_all(&[kind]).unwrap();
        w.write_all(&id.to_le_bytes()).unwrap();
        w.write_all(payload).unwrap();
        w.flush().unwrap();
    }
    let (kind, id, hello) = read_frame(&mut output);
    assert_eq!((kind, id), (1, 0));
    assert_eq!(i64::from_le_bytes(hello[..8].try_into().unwrap()), 1);
    assert!(hello.windows(b"echo(int)->int".len()).any(|w| w == b"echo(int)->int"));
    write_frame(&mut input, 2, 0, &[]);
    // Missing arguments and unknown handlers yield correlated errors, not defaults.
    for (id, handler) in [(41, 0i64), (42, 99i64)] {
        write_frame(&mut input, 3, id, &handler.to_le_bytes());
        let (kind, reply_id, _) = read_frame(&mut output);
        assert_eq!((kind, reply_id), (5, id));
    }
    let mut payload = 0i64.to_le_bytes().to_vec();
    payload.extend_from_slice(&123i64.to_le_bytes());
    write_frame(&mut input, 3, 43, &payload);
    assert_eq!(read_frame(&mut output), (4, 43, 123i64.to_le_bytes().to_vec()));
    write_frame(&mut input, 8, 0, &[]);
    drop(input);
    assert!(child.wait().unwrap().success());

    // Replace the child with a stale interface and verify the actual parent rejects it.
    let cache = scratch.path().join(".orch_cache");
    let child_source = cache.join("src/bin/secret_Protocol.rs");
    let generated = fs::read_to_string(&child_source).unwrap();
    for (old, new) in [("echo(int)->int", "renamed(int)->int"),
                       ("ORCH_WIRE_VERSION: i64 = 1", "ORCH_WIRE_VERSION: i64 = 2")] {
        fs::write(&child_source, generated.replace(old, new)).unwrap();
        let compiled = Command::new("cargo").args(["build", "--quiet", "--bin", "secret_Protocol"])
            .current_dir(&cache).output().unwrap();
        assert!(compiled.status.success(), "{}", String::from_utf8_lossy(&compiled.stderr));
        let parent = Command::new(cache.join("target/debug").join(format!("orch_generated{}", std::env::consts::EXE_SUFFIX)))
            .output().unwrap();
        let stderr = String::from_utf8_lossy(&parent.stderr);
        assert!(stderr.contains("interface mismatch"), "{}", stderr);
        assert!(stderr.contains("echo(int)->int"), "{}", stderr);
    }
}

#[test]
fn runtime_wasm_foreign_module_is_called_and_checked() {
    // A module someone else compiled, loaded by `load_foreign "wasm"`: every declared
    // type crosses, and a sidecar that disagrees with the module's export table is a
    // build error rather than a surprise at run time.
    let _ = fs::remove_dir_all(std::env::temp_dir().join("orch_runtime_wasm_foreign"));
    let scratch = Scratch::new("wasm_foreign");
    let tmp = scratch.path();
    fs::create_dir_all(tmp.join("mathmod")).unwrap();

    let guest = tmp.join("math.rs");
    fs::write(&guest, r#"
#[unsafe(no_mangle)]
pub extern "C" fn double(n: i64) -> i64 { n * 2 }
#[unsafe(no_mangle)]
pub extern "C" fn scale(x: f64, by: f64) -> f64 { x * by }
#[unsafe(no_mangle)]
pub extern "C" fn flip(b: i32) -> i32 { if b == 0 { 1 } else { 0 } }
#[unsafe(no_mangle)]
pub extern "C" fn orch_alloc(len: i32) -> i32 {
    if len <= 0 { return 0; }
    let layout = std::alloc::Layout::from_size_align(len as usize, 1).unwrap();
    unsafe { std::alloc::alloc(layout) as i32 }
}
#[unsafe(no_mangle)]
pub extern "C" fn orch_free(ptr: i32, len: i32) {
    if ptr == 0 || len <= 0 { return; }
    let layout = std::alloc::Layout::from_size_align(len as usize, 1).unwrap();
    unsafe { std::alloc::dealloc(ptr as *mut u8, layout) }
}
#[unsafe(no_mangle)]
pub extern "C" fn shout(ptr: i32, len: i32) -> i64 {
    let input = unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) };
    let out = format!("{}!", String::from_utf8_lossy(input)).into_bytes();
    let out_ptr = orch_alloc(out.len() as i32);
    unsafe { std::ptr::copy_nonoverlapping(out.as_ptr(), out_ptr as *mut u8, out.len()) };
    ((out_ptr as i64) << 32) | out.len() as i64
}
"#).unwrap();
    let wasm = tmp.join("mathmod/math.wasm");
    let built = Command::new("rustc")
        .args(["--target", "wasm32-unknown-unknown", "--crate-type", "cdylib", "-O", "-o"])
        .arg(&wasm)
        .arg(&guest)
        .output()
        .expect("failed to run rustc");
    assert!(built.status.success(), "building the wasm module failed: {}", String::from_utf8_lossy(&built.stderr));

    fs::write(tmp.join("mathmod/module.orch"), "load_foreign \"wasm\" \"./math.wasm\"\n").unwrap();
    fs::write(tmp.join("mathmod/math.orch_ffi"), "double(n: int) -> int\nscale(x: float, by: float) -> float\nflip(b: bool) -> bool\nshout(text: string) -> string\n").unwrap();
    let src_file = tmp.join("main.orch");
    fs::write(&src_file, r#"
use module math: "./mathmod"
orchestrator main() {
    print(to_string(math.double(21)))
    print(to_string(math.scale(1.5, 4.0)))
    print(to_string(math.flip(false)))
    print(math.shout("hello"))
    stop_orch()
}
"#).unwrap();

    let out = Command::new(orchestrate_bin())
        .args(["run", src_file.to_str().unwrap()])
        .output()
        .expect("failed to run orchestrate");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "run failed:\nstdout: {}\nstderr: {}", stdout, String::from_utf8_lossy(&out.stderr));
    let lines: Vec<&str> = stdout.lines().filter(|l| !l.starts_with("[orchestrate]")).collect();
    assert_eq!(lines, vec!["42", "6", "true", "hello!"], "stdout: {}", stdout);

    // A sidecar that disagrees with the module is caught by `check`, before any build.
    fs::write(tmp.join("mathmod/math.orch_ffi"), "double(n: float) -> int\n").unwrap();
    let checked = Command::new(orchestrate_bin())
        .args(["check", src_file.to_str().unwrap()])
        .output()
        .expect("failed to run orchestrate check");
    let report = format!("{}{}", String::from_utf8_lossy(&checked.stdout), String::from_utf8_lossy(&checked.stderr));
    assert!(!checked.status.success(), "a mismatched sidecar must not check clean: {}", report);
    assert!(report.contains("declared to take (f64)") && report.contains("exports it taking (i64)"),
        "the error should name both sides: {}", report);
}

/// The .NET SDK, as the compiler and the generated build.rs both resolve it.
fn dotnet() -> String {
    std::env::var("ORCH_DOTNET").unwrap_or_else(|_| "dotnet".to_string())
}

#[test]
fn runtime_ffi_csharp() {
    if !has_tool(&dotnet(), "--version") { return; }
    // `bool` is not blittable in an export signature, so C# returns a byte that is 0 or 1
    // — the same byte a C `_Bool` returns, which is what the generated wrapper reads.
    write_foreign_module("ffi_csharp", "csmath", "csharp", "Math.cs",
        "using System.Runtime.InteropServices;\n\
         public static class Math\n{\n\
         \x20   [UnmanagedCallersOnly(EntryPoint = \"add\")]\n\
         \x20   public static long Add(long a, long b) => a + b;\n\
         \x20   [UnmanagedCallersOnly(EntryPoint = \"half\")]\n\
         \x20   public static double Half(double x) => x / 2.0;\n\
         \x20   [UnmanagedCallersOnly(EntryPoint = \"is_even\")]\n\
         \x20   public static byte IsEven(long n) => (byte)(n % 2 == 0 ? 1 : 0);\n\
         }\n",
        "add(a: int, b: int) -> int\nhalf(x: float) -> float\nis_even(n: int) -> bool\n");
    let out = run_orch("ffi_csharp", r#"
use module csmath: "./csmath"
let worker = automatic {
    print(to_string(csmath.add(2, 3)))
    print(to_string(csmath.half(5.0)))
    print(to_string(csmath.is_even(10)))
    stop_orch()
}
orchestrator main(procs: process[worker]) { }
"#);
    assert_eq!(out.trim(), "5\n2.5\ntrue");
}

/// Two C# modules in one program. A NativeAOT *static* archive embeds its own runtime and
/// two cannot be linked into one module, which is why the backend publishes shared
/// libraries; this is the test that would fail if that ever changed.
#[test]
fn runtime_ffi_csharp_two_modules_in_one_program() {
    if !has_tool(&dotnet(), "--version") { return; }
    let name = "ffi_csharp_two";
    write_foreign_module(name, "first", "csharp", "First.cs",
        "using System.Runtime.InteropServices;\n\
         public static class First\n{\n\
         \x20   [UnmanagedCallersOnly(EntryPoint = \"first_twice\")]\n\
         \x20   public static long Twice(long n) => n * 2;\n\
         }\n",
        "first_twice(n: int) -> int\n");
    write_foreign_module(name, "second", "csharp", "Second.cs",
        "using System.Runtime.InteropServices;\n\
         public static class Second\n{\n\
         \x20   [UnmanagedCallersOnly(EntryPoint = \"second_thrice\")]\n\
         \x20   public static long Thrice(long n) => n * 3;\n\
         }\n",
        "second_thrice(n: int) -> int\n");
    let out = run_orch(name, r#"
use module first: "./first"
use module second: "./second"
let worker = automatic {
    print(to_string(first.first_twice(5)))
    print(to_string(second.second_thrice(5)))
    stop_orch()
}
orchestrator main(procs: process[worker]) { }
"#);
    assert_eq!(out.trim(), "10\n15");
}

/// Concatenation borrows: `a + b` used to move both sides, so a string could be used once
/// and `s + s` did not compile at all.
#[test]
fn runtime_string_concat_leaves_operands_usable() {
    let out = run_orch("concat_borrows", r#"
orchestrator main() {
    let a = "x"
    let b = "y"
    let joined = a + b
    print(a + a)
    print(a)
    print(b)
    print(joined)
    print(to_string(1 + 2))
    print(to_string(1.5 + 2.5))
    stop_orch()
}
"#);
    assert_eq!(out.trim(), "xx\nx\ny\nxy\n3\n4");
}

/// A `try` block whose body waits compiles as an async block rather than a synchronous
/// closure. It used to reach rustc as "`await` is only allowed inside `async` functions".
#[test]
fn runtime_try_block_can_call_a_task() {
    let out = run_orch("try_awaits", r#"
task fetch(n: int) -> int { return n * 2 }

orchestrator main() {
    let waited = try { fetch(21) } catch e { 0 }
    let plain = try { 7 } catch e { 0 }
    print(to_string(waited))
    print(to_string(plain))
    stop_orch()
}
"#);
    assert_eq!(out.trim(), "42\n7");
}

/// `shared let` is reachable from a synchronous `fn` and from concurrent workers, and the
/// lock makes read-modify-write exact: every increment lands.
#[test]
fn runtime_shared_state_survives_concurrent_workers() {
    let out = run_orch("shared_concurrent", r#"
shared let total = 0
shared let done = 0

// Synchronous, because taking the lock does not wait.
fn add_one() {
    total = total + 1
}

let a = automatic { add_one()  done = done + 1  sleep(1) }
let b = automatic { add_one()  done = done + 1  sleep(1) }
let c = automatic { add_one()  done = done + 1  sleep(1) }

let reporter = automatic {
    sleep(400)
    if total == done { print("exact") } else { print("lost updates") }
    if total > 10 { print("ran") } else { print("too slow") }
    stop_orch()
}

orchestrator main(procs: process[a, b, c, reporter]) { }
"#);
    let lines: Vec<&str> = out.lines().filter(|l| !l.starts_with("[orchestrate]")).collect();
    assert_eq!(lines, vec!["exact", "ran"], "{out}");
}

/// A shared binding is not captured by a worker: it is reached through the lock wherever
/// it is used, so a worker sees what another worker wrote.
#[test]
fn runtime_shared_state_is_not_copied_into_workers() {
    let out = run_orch("shared_not_captured", r#"
shared let ledger = "start"

let writer = automatic { ledger = "written"  sleep(5000) }
let reader = automatic {
    sleep(120)
    print(ledger)
    stop_orch()
}

orchestrator main(procs: process[writer, reader]) { }
"#);
    assert_eq!(out.trim(), "written");
}

/// A change made in place — an element, a field, `append`, `remove` — reaches the
/// `shared let` itself. Each of them used to land on a copy that was dropped, so the
/// program built, ran, and quietly did nothing. The orchestrator body, an event handler,
/// and a worker each write their own slot and read it straight back.
#[test]
fn runtime_shared_state_changes_in_place_reach_the_binding() {
    let out = run_orch("shared_in_place", r#"
struct Tally { count: int, names: string[] }
shared let slots = [0, 0, 0]
shared let seen: int[] = []
shared let tally = Tally { count: 0, names: [] }
shared let grid = [[0, 0], [0, 0]]

let listener = on note(n: int) {
    slots[1] = n
    append(seen, n)
    tally.count = tally.count + 1
    let mine = slots[1]
    print("handler reads {mine}")
}

let worker = automatic {
    slots[2] = 30
    append(seen, 30)
    append(tally.names, "worker")
    let mine = slots[2]
    print("worker reads {mine}")
    trigger note(20)
    sleep(10000)
}

orchestrator main(procs: process[worker]) {
    slots[0] = 10
    append(seen, 10)
    grid[1][0] = 9
    let mine = slots[0]
    print("body reads {mine}")
    sleep(500)
    remove(seen, 0)
    let total = slots[0] + slots[1] + slots[2]
    let left = length(seen)
    let count = tally.count
    let names = length(tally.names)
    let cell = grid[1][0]
    print("total {total} left {left} count {count} names {names} cell {cell}")
    stop_orch()
}
"#);
    let mut lines: Vec<&str> = out.lines().collect();
    let last = lines.pop();
    lines.sort();
    assert_eq!(lines, vec!["body reads 10", "handler reads 20", "worker reads 30"], "{out}");
    assert_eq!(last, Some("total 60 left 2 count 1 names 1 cell 9"), "{out}");
}

/// One top-level client, reached from the orchestrator body, an event handler, and two
/// workers: every call lands on the one serverlet, which takes them one at a time. Each of
/// these forms reached rustc before (E0425, E0382, E0525), and a bare `on` handler was
/// never registered at all.
#[test]
fn runtime_top_level_client_is_shared_by_body_handler_and_workers() {
    let out = run_orch("client_everywhere", r#"
serverlet Counter {
    let n = 0
    let busy = false
    let overlaps = 0
    on bump(who: int) -> int {
        if busy { overlaps = overlaps + 1 }
        busy = true
        let spin = 0
        for i in range(2000) { spin = spin + i }
        n = n + 1
        busy = false
        return n
    }
    on report() -> string { return "{n} calls, {overlaps} overlapping" }
}

let counter = start Counter()

let a = automatic {
    for i in range(50) { counter.bump(1) }
    sleep(100000)
}
let b = automatic {
    for i in range(50) { counter.bump(2) }
    sleep(100000)
}
on poke(times: int) {
    for i in range(times) { counter.bump(3) }
}

orchestrator main(procs: process[a, b]) {
    trigger poke(50)
    for i in range(50) { counter.bump(4) }
    sleep(1500)
    print(counter.report())
    stop_orch()
}
"#);
    assert_eq!(out.trim(), "200 calls, 0 overlapping");
}

/// The example in docs/design/concurrency-is-the-structure.md, taken from the document
/// itself so the two cannot drift: it reached rustc from v0.14 to v0.15.1. It never
/// stops on its own, so it runs for a while and is then stopped.
#[test]
fn runtime_design_document_example_runs() {
    let doc = fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/design/concurrency-is-the-structure.md")).unwrap();
    let start = doc.find("```orchestrate\n").expect("an orchestrate block") + "```orchestrate\n".len();
    let source = &doc[start..start + doc[start..].find("```").unwrap()];
    let scratch = Scratch::new("design_doc_example");
    let src_file = scratch.path().join("example.orch");
    fs::write(&src_file, source).unwrap();
    let binary = scratch.path().join("example_bin");
    let built = Command::new(orchestrate_bin())
        .args(["build", src_file.to_str().unwrap(), "-o", binary.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    let mut child = Command::new(&binary).stdout(std::process::Stdio::piped()).spawn().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(1500));
    child.kill().unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "created 42: Mina");
}

/// `restart: N` is at most N restarts after crashes, `never` is none, and `always` has no
/// limit; a worker that has spent its restarts stays stopped. Before, the orchestrator
/// restarted every finished worker once a second with a fresh count, so each of these
/// meant `always`. A declared backoff is the wait before each restart; the default starts
/// at a second, so the last worker has run only twice when the program stops.
#[test]
fn runtime_restart_policy_is_a_limit_and_backoff_is_declarable() {
    let out = run_orch("restart_policy", r#"
shared let never_runs = 0
shared let three_runs = 0
shared let always_runs = 0
shared let default_runs = 0

let never_worker = automatic(restart: never, backoff: "10ms") {
    never_runs = never_runs + 1
    let zero = never_runs - never_runs
    let boom = 1 / zero
}
let three_worker = automatic(restart: 3, backoff: "10ms") {
    three_runs = three_runs + 1
    let zero = three_runs - three_runs
    let boom = 1 / zero
}
let always_worker = automatic(restart: always, backoff: "10ms") {
    always_runs = always_runs + 1
    if always_runs < 8 {
        let zero = always_runs - always_runs
        let boom = 1 / zero
    }
    sleep(100000)
}
let default_worker = automatic(restart: 5) {
    default_runs = default_runs + 1
    let zero = default_runs - default_runs
    let boom = 1 / zero
}

orchestrator main(procs: process[never_worker, three_worker, always_worker, default_worker]) {
    sleep(1500)
    let counts = "{never_runs} {three_runs} {always_runs} {default_runs}"
    print(counts)
    stop_orch()
}
"#);
    assert_eq!(out.trim(), "1 4 8 2");
}

/// The active worker set changes while the program runs, from the orchestrator body, a
/// worker, and twice in a row, and a worker keeps itself in a set it swaps in by naming
/// itself. Each of these reached rustc, and a worker could not name itself at all.
#[test]
fn runtime_worker_set_swaps_from_body_and_worker() {
    let out = run_orch("update_orchestrator_swaps", r#"
shared let a_runs = 0
shared let b_runs = 0
shared let c_runs = 0
shared let swapped = false

let b = automatic { b_runs = b_runs + 1  sleep(10) }
let c = automatic { c_runs = c_runs + 1  sleep(10) }
let a = automatic {
    a_runs = a_runs + 1
    let runs = a_runs
    // From inside a worker, naming itself: a keeps running, c joins.
    if runs == 5 { trigger update_orchestrator([a, c]) }
    sleep(10)
}

orchestrator main(procs: process[a]) {
    sleep(200)
    // Twice from the body, the second from a set built in a variable: only b is left.
    trigger update_orchestrator([a, b])
    let only_b = [b]
    sleep(100)
    trigger update_orchestrator(only_b)
    sleep(50)
    let a_then = a_runs
    let c_then = c_runs
    let b_then = b_runs
    sleep(200)
    let a_now = a_runs
    let c_now = c_runs
    let b_now = b_runs
    print("c joined: {c_then > 0}")
    print("a stopped: {a_now == a_then}")
    print("c stopped: {c_now == c_then}")
    print("b running: {b_now > b_then}")
    stop_orch()
}
"#);
    assert_eq!(out.trim(), "c joined: true\na stopped: true\nc stopped: true\nb running: true");
}

/// Builds `source` into a scratch directory and runs it, calling `during` with the
/// running process's id after `after`, then collects everything it printed.
#[cfg(unix)]
fn build_and_run_with(test_name: &str, source: &str, after: std::time::Duration, during: impl FnOnce(u32, &std::path::Path)) -> (String, String, Scratch) {
    build_prepare_and_run(test_name, source, |_| {}, after, during)
}

/// `build_and_run_with`, with `prepare` run on the build directory before the program
/// starts, for a test whose setup must not race the program's first steps.
fn build_prepare_and_run(test_name: &str, source: &str, prepare: impl FnOnce(&std::path::Path), after: std::time::Duration, during: impl FnOnce(u32, &std::path::Path)) -> (String, String, Scratch) {
    let scratch = Scratch::new(test_name);
    let src_file = scratch.path().join("program.orch");
    fs::write(&src_file, source).unwrap();
    let binary = scratch.path().join("program_bin");
    let built = Command::new(orchestrate_bin())
        .args(["build", src_file.to_str().unwrap(), "-o", binary.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
    prepare(scratch.path());
    let child = Command::new(&binary)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(after);
    during(child.id(), scratch.path());
    let out = child.wait_with_output().unwrap();
    (String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned(), scratch)
}

/// The process ids of `parent`'s children whose command contains `needle`.
#[cfg(unix)]
fn children_named(parent: u32, needle: &str) -> Vec<u32> {
    let listing = Command::new("ps").args(["-A", "-o", "pid=,ppid=,command="]).output().unwrap();
    String::from_utf8_lossy(&listing.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid: u32 = fields.next()?.parse().ok()?;
            let ppid: u32 = fields.next()?.parse().ok()?;
            let command = fields.collect::<Vec<_>>().join(" ");
            (ppid == parent && command.contains(needle)).then_some(pid)
        })
        .collect()
}

/// Two programs built into one directory keep their child processes apart: each binary's
/// are in `<binary>.children/`. Both declare a secret serverlet `Broker` with different
/// handlers; the second build used to overwrite the first's child, which then failed its
/// handshake with an interface mismatch.
#[test]
fn runtime_two_programs_in_one_directory_keep_their_children() {
    let scratch = Scratch::new("children_per_binary");
    let build = |name: &str, source: &str| {
        let src = scratch.path().join(format!("{name}.orch"));
        fs::write(&src, source).unwrap();
        let out = Command::new(orchestrate_bin())
            .args(["build", src.to_str().unwrap(), "-o", scratch.path().join(name).to_str().unwrap()])
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    };
    build("first", r#"
serverlet Broker secret { on quote() -> int { return 1 } }
orchestrator main() { let b = start Broker()  print("first {b.quote()}")  stop_orch() }
"#);
    build("second", r#"
serverlet Broker secret { on quote(n: int) -> int { return n * 2 } }
orchestrator main() { let b = start Broker()  print("second {b.quote(21)}")  stop_orch() }
"#);
    for (name, expected) in [("first", "first 1"), ("second", "second 42")] {
        let out = Command::new(scratch.path().join(name)).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), expected, "{}", String::from_utf8_lossy(&out.stderr));
    }
    assert!(scratch.path().join("first.children/secret_Broker").is_file());
    assert!(scratch.path().join("second.children/secret_Broker").is_file());
}

/// A secret serverlet's child killed from outside is noticed at the next call: `on_crash`
/// runs on the host with the reason, a fresh child is started, and that call is served by
/// it, with its state starting over. Before, every later call answered 0.
#[cfg(unix)]
#[test]
fn runtime_secret_child_killed_is_restarted_and_reported() {
    let (stdout, stderr, _scratch) = build_and_run_with("secret_restart", r#"
serverlet Risk secret {
    let n = 0
    on check(x: int) -> int {
        n = n + 1
        return n
    }
    on_crash e {
        print("on_crash ran")
    }
}
orchestrator main() {
    let r = start Risk()
    for i in range(4) { print("call {r.check(i)}") }
    sleep(3000)
    for i in range(3) { print("call {r.check(i)}") }
    stop_orch()
}
"#, std::time::Duration::from_millis(500), |pid, _| {
        // The first four calls start the child, and are done well before this; wait for
        // the child, however loaded the machine, and kill it during the long sleep.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut children = children_named(pid, "secret_Risk");
        while children.is_empty() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
            children = children_named(pid, "secret_Risk");
        }
        assert_eq!(children.len(), 1, "expected one secret child");
        Command::new("kill").args(["-9", &children[0].to_string()]).status().unwrap();
    });
    assert_eq!(stdout.lines().collect::<Vec<_>>(),
        vec!["call 1", "call 2", "call 3", "call 4", "on_crash ran", "call 1", "call 2", "call 3"], "{stderr}");
    assert!(stderr.contains("exited between calls"), "{stderr}");
}

/// A secret child that cannot start is tried again: calls answer with the default until
/// it can, and the first call after the next try is served. Before, one failed start
/// closed the serverlet for the rest of the run.
#[cfg(unix)]
#[test]
fn runtime_secret_child_that_cannot_start_is_tried_again() {
    // Gone before the program starts, so the first call cannot find it, and back a second
    // later, well before the later call two seconds after that.
    let child = |dir: &std::path::Path| dir.join("program_bin.children/secret_Late");
    let hidden = |dir: &std::path::Path| dir.join("program_bin.children/secret_Late.hidden");
    let (stdout, stderr, _scratch) = build_prepare_and_run("secret_start_retry", r#"
serverlet Late secret {
    let n = 0
    on check() -> int {
        n = n + 1
        return n
    }
}
orchestrator main() {
    let r = start Late()
    print("first {r.check()}")
    sleep(3000)
    print("later {r.check()}")
    stop_orch()
}
"#, |dir| fs::rename(child(dir), hidden(dir)).unwrap(), std::time::Duration::from_millis(1000), |_, dir| {
        fs::rename(hidden(dir), child(dir)).unwrap();
    });
    assert_eq!(stdout.lines().collect::<Vec<_>>(), vec!["first 0", "later 1"], "{stderr}");
    assert!(stderr.contains("could not start"), "{stderr}");
}

/// Arrays and structs across the C ABI. An array parameter is a pointer and a count; an
/// array return is a `malloc`'d pointer plus a count written through an out-parameter,
/// which the wrapper copies and frees — the same ownership rule strings follow. A struct
/// crosses by value, because the generated struct is `#[repr(C)]`.
#[test]
fn runtime_ffi_c_arrays_and_structs() {
    write_foreign_module("ffi_c_arrays", "cmath", "c", "m.c",
        "#include <stdlib.h>\n\
         long long total(const long long *items, long long count) {\n\
         \x20   long long sum = 0;\n\
         \x20   for (long long i = 0; i < count; i++) sum += items[i];\n\
         \x20   return sum;\n\
         }\n\
         double *scaled(const double *items, long long count, double by, long long *out_count) {\n\
         \x20   double *out = malloc(sizeof(double) * (size_t)count);\n\
         \x20   for (long long i = 0; i < count; i++) out[i] = items[i] * by;\n\
         \x20   *out_count = count;\n\
         \x20   return out;\n\
         }\n\
         struct Point { long long x; long long y; };\n\
         struct Point shift(struct Point p) {\n\
         \x20   struct Point moved = { p.x + 1, p.y + 2 };\n\
         \x20   return moved;\n\
         }\n",
        "total(items: int[]) -> int\nscaled(items: float[], by: float) -> float[]\nshift(p: Point) -> Point\n");
    let out = run_orch("ffi_c_arrays", r#"
use module cmath: "./cmath"

struct Point { x: int, y: int }

let worker = automatic {
    print(to_string(cmath.total([1, 2, 3, 4])))
    let doubled = cmath.scaled([1.5, 2.5], 2.0)
    print(to_string(doubled[0]) + "," + to_string(doubled[1]))
    let moved = cmath.shift(Point { x: 10, y: 20 })
    print(to_string(moved.x) + "," + to_string(moved.y))
    stop_orch()
}
orchestrator main(procs: process[worker]) { }
"#);
    assert_eq!(out.trim(), "10\n3,5\n11,22");
}

/// A C++ vendor library that ships a header and a source of its own attaches through its
/// sidecar's `[build]` section — its include directory, its extra source, a definition, a
/// standard, and a library to link — rather than by being `#include`d into one file.
#[test]
fn runtime_ffi_cpp_vendor_library_through_build_options() {
    let root = std::env::temp_dir().join("orch_runtime_ffi_cpp_vendor").join("vendormod");
    fs::create_dir_all(root.join("vendor/include")).unwrap();
    fs::create_dir_all(root.join("vendor/src")).unwrap();
    fs::write(root.join("module.orch"), "load_foreign \"cpp\" \"./shim.cpp\"\n").unwrap();
    fs::write(root.join("vendor/include/driver.hpp"),
        "#pragma once\n#include <vector>\nnamespace vendor {\nstruct Driver {\n    std::vector<long long> readings{4, 5, 6};\n    long long sum() const;\n};\n}\n").unwrap();
    fs::write(root.join("vendor/src/driver.cpp"),
        "#include \"driver.hpp\"\nnamespace vendor {\nlong long Driver::sum() const { long long t = 0; for (auto r : readings) t += r; return t * SCALE; }\n}\n").unwrap();
    fs::write(root.join("shim.cpp"),
        "#include \"driver.hpp\"\n#include <cmath>\nextern \"C\" long long driver_sum() { vendor::Driver d; return d.sum(); }\nextern \"C\" double driver_root() { return std::sqrt(16.0); }\n").unwrap();
    fs::write(root.join("shim.orch_ffi"),
        "driver_sum() -> int\ndriver_root() -> float\n\n[build]\nstd = \"c++17\"\ninclude = [\"vendor/include\"]\nsources = [\"vendor/src/driver.cpp\"]\ndefine = [\"SCALE=10\"]\nlibraries = [\"m\"]\n").unwrap();
    let out = run_orch("ffi_cpp_vendor", r#"
use module v: "./vendormod"
orchestrator main() {
    print("{v.driver_sum()} {v.driver_root()}")
    stop_orch()
}
"#);
    assert_eq!(out.trim(), "150 4");
}

/// The same ABI from Zig, which reaches it through `export fn` rather than C declarations.
#[test]
fn runtime_ffi_zig_arrays() {
    if !has_tool("zig", "version") { return; }
    write_foreign_module("ffi_zig_arrays", "zmath", "zig", "m.zig",
        "const std = @import(\"std\");\n\
         export fn total(items: [*]const i64, count: i64) i64 {\n\
         \x20   var sum: i64 = 0;\n\
         \x20   var i: usize = 0;\n\
         \x20   while (i < @as(usize, @intCast(count))) : (i += 1) sum += items[i];\n\
         \x20   return sum;\n\
         }\n",
        "total(items: int[]) -> int\n");
    let out = run_orch("ffi_zig_arrays", r#"
use module zmath: "./zmath"
let worker = automatic {
    print(to_string(zmath.total([5, 6, 7])))
    stop_orch()
}
orchestrator main(procs: process[worker]) { }
"#);
    assert_eq!(out.trim(), "18");
}

/// Arrays and structs cross the sandbox boundary under the C ABI's rules: an array is a
/// pointer and a count, a struct is its `#[repr(C)]` bytes, what the host writes for a
/// call it frees after it, and what the guest returns the host copies and frees. State
/// of those types lives in the guest like any other, and a handler may return it.
#[test]
fn runtime_sandbox_serverlet_carries_arrays_and_structs() {
    let out = run_orch("sandbox_arrays", r#"
struct Point { x: int, y: int }
struct Sample { weight: float, ok: bool, at: Point }
struct Label { text: string, n: int }

serverlet Plugin sandbox(memory_limit: "16mb", timeout: "2s") {
    let seen = [0]
    let origin = Point { x: 100, y: 200 }

    on total(items: int[]) -> int {
        let sum = 0
        for item in items { sum = sum + item }
        append(seen, sum)
        return sum
    }
    on scaled(items: float[], by: float) -> float[] {
        return map(items, fn(v: float) -> float { v * by })
    }
    on flip(flags: bool[]) -> bool[] {
        return map(flags, fn(f: bool) -> bool { f == false })
    }
    on shift(p: Point) -> Point {
        return Point { x: p.x + origin.x, y: p.y + origin.y }
    }
    on weigh(s: Sample) -> Sample {
        return Sample { weight: s.weight * 2.0, ok: s.ok == false, at: Point { x: s.at.x + 1, y: s.at.y + 1 } }
    }
    on history() -> int[] { return seen }
    on tag(text: string, items: int[]) -> string {
        return text + ":" + to_string(length(items))
    }
    on empty(items: int[]) -> int[] { return items }
}

orchestrator main() {
    let p = start Plugin()
    print(to_string(p.total([1, 2, 3, 4])))
    print(to_string(p.total([10, 20])))
    let doubled = p.scaled([1.5, 2.5], 2.0)
    print(to_string(doubled[0]) + "," + to_string(doubled[1]))
    let flipped = p.flip([true, false, true])
    print(to_string(flipped[0]) + "," + to_string(flipped[1]) + "," + to_string(flipped[2]))
    let moved = p.shift(Point { x: 1, y: 2 })
    print(to_string(moved.x) + "," + to_string(moved.y))
    let weighed = p.weigh(Sample { weight: 1.25, ok: true, at: Point { x: 5, y: 6 } })
    print(to_string(weighed.weight) + "," + to_string(weighed.ok) + "," + to_string(weighed.at.x) + "," + to_string(weighed.at.y))
    let seen = p.history()
    print(to_string(seen[0]) + "," + to_string(seen[1]) + "," + to_string(seen[2]))
    print(p.tag("items", [7, 8, 9]))
    let nothing = p.empty([])
    print(to_string(length(nothing)))
    stop_orch()
}
"#);
    assert_eq!(out.trim(), "10\n30\n3,5\nfalse,true,false\n101,202\n2.5,false,6,7\n0,10,30\nitems:3\n0");
}

/// What the host writes into the guest for a call, it frees after the call. Before this
/// was so, every string argument stayed allocated in guest memory, and a serverlet that
/// took strings walked into its memory cap and then failed every call. 20,000 one-kilobyte
/// strings under an 8mb cap succeed only if each one is released.
#[test]
fn runtime_sandbox_frees_what_the_host_writes() {
    let out = run_orch("sandbox_frees_arguments", r#"
serverlet Echo sandbox(memory_limit: "8mb", timeout: "5s") {
    on shout(text: string) -> string { return text }
    on total(items: int[]) -> int { return length(items) }
}
orchestrator main() {
    let e = start Echo()
    let text = "x"
    for i in range(1023) { text = text + "x" }
    let texts = [text]
    let items = [0]
    for i in range(1, 128) { append(items, i) }
    let arrays = [items]
    let failures = 0
    for i in range(20000) {
        let payload = texts[0]
        if e.shout(payload) != texts[0] { failures = failures + 1 }
        let numbers = arrays[0]
        if e.total(numbers) != 128 { failures = failures + 1 }
    }
    print("failures: " + to_string(failures))
    stop_orch()
}
"#);
    assert_eq!(out.trim(), "failures: 0");
}

/// A handler may return its own state. `return seen` used to move the array out of the
/// actor, the child, or the guest, which rustc refused with a message about moved values
/// in code the user never wrote; it is a copy now, on every boundary.
#[test]
fn runtime_serverlet_handler_returns_its_state() {
    let out = run_orch("serverlet_returns_state", r#"
serverlet Local {
    let seen = [0]
    let label = "local"
    on note(n: int) -> int { append(seen, n)  return length(seen) }
    on history() -> int[] { return seen }
    on name() -> string { label }
}
serverlet Child secret {
    let seen = [0]
    let label = "secret"
    on note(n: int) -> int { append(seen, n)  return length(seen) }
    on history() -> int[] { return seen }
    on name() -> string { label }
}
serverlet Guest sandbox(memory_limit: "16mb", timeout: "2s") {
    let seen = [0]
    let label = "sandbox"
    on note(n: int) -> int { append(seen, n)  return length(seen) }
    on history() -> int[] { return seen }
    on name() -> string { label }
}
orchestrator main() {
    let l = start Local()
    let c = start Child()
    let g = start Guest()
    print(to_string(l.note(1)) + "," + to_string(c.note(2)) + "," + to_string(g.note(3)))
    let lh = l.history()
    let ch = c.history()
    let gh = g.history()
    print(to_string(lh[1]) + "," + to_string(ch[1]) + "," + to_string(gh[1]))
    print(l.name() + "," + c.name() + "," + g.name())
    print(to_string(l.note(4)) + "," + to_string(c.note(5)) + "," + to_string(g.note(6)))
    stop_orch()
}
"#);
    assert_eq!(out.trim(), "2,2,2\n1,2,3\nlocal,secret,sandbox\n3,3,3");
}

/// `on_crash` on a sandboxed serverlet runs on the host after a call trapped, with the
/// trap's message bound, before the guest is replaced and the caller gets the default.
/// The handler cannot reach the guest's state, which is inside the instance being thrown
/// away; a handler that tries is a compile error naming the binding.
#[test]
fn runtime_sandbox_on_crash_runs_on_the_host() {
    let out = run_orch("sandbox_on_crash", r#"
serverlet Hostile sandbox(memory_limit: "16mb", timeout: "300ms") {
    let calls = 0
    on spin() -> int {
        while true { calls = calls + 1 }
        return calls
    }
    on behave(n: int) -> int {
        calls = calls + 1
        return calls + n
    }
    on_crash reason { print("crashed: " + reason) }
}
orchestrator main() {
    let h = start Hostile()
    print(to_string(h.behave(10)))
    print(to_string(h.spin()))
    print(to_string(h.behave(10)))
    stop_orch()
}
"#);
    // 11 twice: the second `behave` runs against a fresh guest, so the count restarted.
    assert_eq!(
        out.trim(),
        "11\ncrashed: wasm call 'Hostile::spin' failed: the guest ran past its timeout\n0\n11"
    );
}

/// A block whose last statement touches shared state takes the lock: a `while` body, an
/// `if` branch, and a hook body all end in a block's tail expression, which used to be
/// compiled without the guard and reach rustc as an unknown `__shared`.
#[test]
fn runtime_shared_state_in_a_block_tail_takes_the_lock() {
    let out = run_orch("shared_block_tail", r#"
shared let hits = 0
shared let label = "none"
fn touch(n: int) {
    if n % 2 == 0 { hits = hits + n } else { label = "odd" }
}
orchestrator main() {
    let k = 0
    while k < 5 { k = k + 1  hits = hits + 1 }
    touch(4)
    touch(3)
    let snapshot = if hits > 5 { hits } else { 0 }
    print(to_string(snapshot) + "," + label)
    stop_orch()
}
"#);
    // 5 from the loop's tail statement and 4 from the if branch: both took the lock.
    assert_eq!(out.trim(), "9,odd");
}

/// A serverlet with one handler per value kind a call can carry, under `header`, and a
/// caller that uses each argument again after the call. The call owns its message, so
/// the caller's binding has to be copied, not moved — on every boundary.
fn arguments_survive_program(header: &str) -> String {
    format!(r#"
struct Point {{ x: int, y: int }}
{header}
    on shout(text: string) -> string {{ return text + "!" }}
    on total(xs: int[]) -> int {{ let t = 0  for x in xs {{ t = t + x }}  return t }}
    on sum(p: Point) -> int {{ return p.x + p.y }}
}}
orchestrator main() {{
    let e = start Echo()
    let payload = "hi"
    let nums = [1, 2, 3]
    let pt = Point {{ x: 4, y: 5 }}
    let a = e.shout(payload)
    let b = e.total(nums)
    let c = e.sum(pt)
    print(a + " " + payload)
    print(to_string(b) + " " + to_string(length(nums)))
    print(to_string(c) + " " + to_string(pt.x))
    stop_orch()
}}
"#)
}
const ARGUMENTS_SURVIVE: &str = "hi! hi\n6 3\n9 4";

#[test]
fn runtime_serverlet_call_keeps_the_callers_arguments() {
    let out = run_orch("call_keeps_arguments", &arguments_survive_program("serverlet Echo {"));
    assert_eq!(out.trim(), ARGUMENTS_SURVIVE);
}

#[test]
fn runtime_secret_serverlet_call_keeps_the_callers_arguments() {
    let out = run_orch("secret_call_keeps_arguments", &arguments_survive_program("serverlet Echo secret {"));
    assert_eq!(out.trim(), ARGUMENTS_SURVIVE);
}

#[test]
fn runtime_sandbox_serverlet_call_keeps_the_callers_arguments() {
    let out = run_orch(
        "sandbox_call_keeps_arguments",
        &arguments_survive_program(r#"serverlet Echo sandbox(memory_limit: "16mb", timeout: "5s") {"#),
    );
    assert_eq!(out.trim(), ARGUMENTS_SURVIVE);
}

/// A program that names things with Rust keywords that are not OrchestrateLang keywords
/// — a struct field `ref` and `type`, a `fn type`, a `task async`, handlers `move`,
/// `loop`, and `trait`, state `mod`, bindings `mut`, `unsafe`, `static`, `box`, `yield`,
/// a closure parameter `macro`, a match binding `become`. Each crosses into generated
/// Rust as a raw identifier; a handler's name keeps its spelling on the boundary.
fn keyword_names_program(header: &str) -> String {
    format!(r#"
struct Point {{ ref: int, type: int }}
enum Mode {{ Loop, Impl(int) }}
fn type(ref: int) -> int {{ return ref + 1 }}
task async(await: string) -> string {{ return await + "!" }}
{header}
    let mod = 0
    on move(p: Point) -> int {{ mod = mod + p.ref + p.type  return mod }}
    on loop(dyn: string) -> string {{ return dyn + "?" }}
    on trait(impl: int[]) -> int {{ let where = 0  for pub in impl {{ where = where + pub }}  return where }}
}}
orchestrator main() {{
    let e = start Echo()
    let mut = Point {{ ref: 1, type: 2 }}
    let unsafe = [1, 2, 3]
    let static = "s"
    let box = Mode::Impl(4)
    let yield = fn(macro: int) -> int {{ macro * 2 }}
    print(to_string(e.move(mut)))
    print(e.loop(static))
    print(to_string(e.trait(unsafe)))
    print(to_string(type(mut.ref)))
    print(async("a"))
    print(to_string(yield(3)))
    match box {{ Mode::Loop => print("loop")  Mode::Impl(become) => print(to_string(become)) }}
    stop_orch()
}}
"#)
}
const KEYWORD_NAMES: &str = "3\ns?\n6\n2\na!\n6\n4";

#[test]
fn runtime_keyword_names_in_process() {
    let out = run_orch("keyword_names", &keyword_names_program("serverlet Echo {"));
    assert_eq!(out.trim(), KEYWORD_NAMES);
}

#[test]
fn runtime_keyword_names_on_a_secret_serverlet() {
    let out = run_orch("keyword_names_secret", &keyword_names_program("serverlet Echo secret {"));
    assert_eq!(out.trim(), KEYWORD_NAMES);
}

#[test]
fn runtime_keyword_names_in_a_sandbox() {
    let out = run_orch(
        "keyword_names_sandbox",
        &keyword_names_program(r#"serverlet Echo sandbox(memory_limit: "16mb", timeout: "5s") {"#),
    );
    assert_eq!(out.trim(), KEYWORD_NAMES);
}

/// A C sidecar function and a module's local name may be Rust keywords too: the symbol
/// keeps its spelling for the linker, the Rust wrapper and module are raw identifiers.
#[test]
fn runtime_keyword_names_on_a_c_sidecar_and_module() {
    write_foreign_module("keyword_names_c", "native", "c", "native.c",
        "long long move(long long ref) { return ref * 10; }\n",
        "move(ref: int) -> int\n");
    let out = run_orch("keyword_names_c", r#"
use module impl: "./native"
orchestrator main() {
    let type = 4
    print(to_string(impl.move(type)))
    stop_orch()
}
"#);
    assert_eq!(out.trim(), "40");
}

/// A struct or an enum may be named with a Rust keyword as well: the type is a raw
/// identifier wherever the generated Rust names it, and the sandbox codec functions
/// built from its name keep the plain spelling, which is always a valid identifier.
fn keyword_type_names_program(header: &str) -> String {
    format!(r#"
struct ref {{ type: int, move: int }}
enum loop {{ Stop, Go(int) }}
{header}
    on sum(p: ref) -> int {{ return p.type + p.move }}
    on flip(p: ref) -> ref {{ return ref {{ type: p.move, move: p.type }} }}
}}
orchestrator main() {{
    let e = start Echo()
    let p = ref {{ type: 1, move: 2 }}
    print(to_string(e.sum(p)))
    let q = e.flip(p)
    print(to_string(q.type))
    let l = loop::Go(3)
    match l {{ loop::Stop => print("stop")  loop::Go(n) => print(to_string(n)) }}
    stop_orch()
}}
"#)
}
const KEYWORD_TYPE_NAMES: &str = "3\n2\n3";

#[test]
fn runtime_keyword_type_names_in_process() {
    let out = run_orch("keyword_type_names", &keyword_type_names_program("serverlet Echo {"));
    assert_eq!(out.trim(), KEYWORD_TYPE_NAMES);
}

#[test]
fn runtime_keyword_type_names_on_a_secret_serverlet() {
    let out = run_orch("keyword_type_names_secret", &keyword_type_names_program("serverlet Echo secret {"));
    assert_eq!(out.trim(), KEYWORD_TYPE_NAMES);
}

#[test]
fn runtime_keyword_type_names_in_a_sandbox() {
    let out = run_orch(
        "keyword_type_names_sandbox",
        &keyword_type_names_program(r#"serverlet Echo sandbox(memory_limit: "16mb", timeout: "5s") {"#),
    );
    assert_eq!(out.trim(), KEYWORD_TYPE_NAMES);
}
