//! The command line itself: what it says about itself, and how it behaves in a pipeline.
use std::io::Read;
use std::process::{Command, Stdio};

fn orchestrate() -> Command {
    Command::new(env!("CARGO_BIN_EXE_orchestrate"))
}

/// `--version` names the compiler's version and the commit it was built from; it was
/// "Unknown command" before.
#[test]
fn version_names_the_version_and_the_commit() {
    for flag in ["--version", "-V", "version"] {
        let output = orchestrate().arg(flag).output().unwrap();
        assert!(output.status.success(), "{flag}");
        let text = String::from_utf8_lossy(&output.stdout);
        let expected = format!("OrchestrateLang compiler v{} (commit ", env!("CARGO_PKG_VERSION"));
        assert!(text.starts_with(&expected), "{flag}: {text}");
    }
}

/// `orchestrate --help | head -1` panicked with a broken pipe once `head` closed it. A
/// command-line tool stops quietly there.
#[test]
fn help_into_a_pipe_that_closes_does_not_panic() {
    let mut child = orchestrate()
        .arg("--help")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Closed before the child has started writing — process startup takes far longer
    // than this — so its first write finds the pipe closed, as `head` leaves it.
    drop(child.stdout.take());
    let mut stderr = String::new();
    child.stderr.take().unwrap().read_to_string(&mut stderr).unwrap();
    child.wait().unwrap();
    assert!(!stderr.contains("panicked"), "{stderr}");
}

/// A struct field named with a Rust keyword compiles, escaped as `r#box`, and a library's
/// Rust host has to write it that way; `check --lib` warns, naming it, and still passes.
/// A standalone program has no host, and says nothing.
#[test]
fn check_lib_warns_of_names_a_host_writes_raw() {
    let dir = std::env::temp_dir().join(format!("orch_cli_raw_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("main.orch");
    std::fs::write(&file, "struct Settings { box: float, count: int }\nenum Mode { Fast, Slow }\nhost app { fn note(message: string) }\non_tick(dt: float, input: Settings) -> Settings {\n    app.note(\"tick\")\n    return input\n}\norchestrator main() {}\n").unwrap();
    let output = orchestrate().arg("check").arg("--lib").arg(&file).output().unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("struct 'Settings' field 'box' reaches a Rust host as `r#box`"), "{stderr}");
    assert_eq!(stderr.matches("warning").count(), 1, "{stderr}");
    std::fs::write(&file, "struct Settings { box: float }\norchestrator main() {\n    let s = Settings { box: 1.0 }\n    print(to_string(s.box))\n}\n").unwrap();
    let output = orchestrate().arg("check").arg(&file).output().unwrap();
    assert!(output.status.success() && !String::from_utf8_lossy(&output.stderr).contains("warning"));
    let _ = std::fs::remove_dir_all(&dir);
}

/// `build --guest` builds one sandboxed serverlet with bodies, and says what is wrong with a
/// file that has none, or several.
#[test]
fn build_guest_needs_one_guest() {
    let dir = std::env::temp_dir().join(format!("orch_cli_guest_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("guest.orch");
    for (source, expected) in [
        ("serverlet A sandbox(memory_limit: \"8mb\", timeout: \"20ms\") { on f() -> int }\n", "this file declares none"),
        ("serverlet A sandbox(memory_limit: \"8mb\", timeout: \"20ms\") { on f() -> int { return 1 } }\nserverlet B sandbox(memory_limit: \"8mb\", timeout: \"20ms\") { on f() -> int { return 2 } }\n", "this file declares 2: A, B"),
    ] {
        std::fs::write(&file, source).unwrap();
        let output = orchestrate().args(["build", "--guest"]).arg(&file).output().unwrap();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(!output.status.success() && stderr.contains(expected), "{stderr}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}
