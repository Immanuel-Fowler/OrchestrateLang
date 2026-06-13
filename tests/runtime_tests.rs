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
        .filter(|l| !l.starts_with("[Orchestrate]"))
        .collect::<Vec<_>>()
        .join("\n")
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
