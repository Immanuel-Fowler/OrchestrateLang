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

/// Write source to a temp dir, compile+run it, return stdout.
fn run_orch(test_name: &str, source: &str) -> String {
    let tmp = std::env::temp_dir().join(format!("orch_runtime_{}", test_name));
    fs::create_dir_all(&tmp).unwrap();
    let src_file = tmp.join("test.orch");
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
    raw_stdout.lines()
        .filter(|l| !l.starts_with("[orchestrate]"))
        .collect::<Vec<_>>()
        .join("\n")
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
    let tmp = std::env::temp_dir().join(format!("orch_runtime_{}", test_name));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();
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
    let _ = fs::remove_dir_all(&tmp);
}

#[test]
fn runtime_sandbox_contains_a_runaway_guest() {
    // The two limits a sandbox promises: a guest that will not return is interrupted, and
    // a guest that allocates without bound is stopped. Both leave the host running and
    // the serverlet usable, because a trapped guest is replaced rather than kept.
    let test_name = "sandbox_limits";
    let tmp = std::env::temp_dir().join(format!("orch_runtime_{}", test_name));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).unwrap();
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
orchestrator main() {
    let h = start Hostile()
    print(to_string(h.behave(1)))
    print(to_string(h.spin()))
    print(to_string(h.behave(2)))
    print(to_string(h.devour()))
    print(to_string(h.behave(3)))
    print("alive")
    stop_orch()
}
"#).unwrap();

    let started = std::time::Instant::now();
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
    assert_eq!(lines, vec!["2", "0", "3", "0", "4", "alive"], "stdout: {}\nstderr: {}", stdout, stderr);
    assert!(stderr.contains("ran past its timeout"), "expected a timeout report: {}", stderr);
    assert!(stderr.contains("memory allocation") || stderr.contains("stopped itself"),
        "expected the guest's own failure reported: {}", stderr);
    // An unbounded loop that was actually stopped cannot have taken long.
    assert!(started.elapsed() < std::time::Duration::from_secs(300), "the run did not finish promptly");
    let _ = fs::remove_dir_all(&tmp);
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
    // Every program in tests/programs/ must compile and exit successfully.
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
    run_orch("protocol_frames", source);
    let binary = std::env::temp_dir().join("orch_runtime_protocol_frames/.orch_cache/target/debug")
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
    let cache = std::env::temp_dir().join("orch_runtime_protocol_frames/.orch_cache");
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
    let tmp = std::env::temp_dir().join("orch_runtime_wasm_foreign");
    let _ = fs::remove_dir_all(&tmp);
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
    let _ = fs::remove_dir_all(&tmp);
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
