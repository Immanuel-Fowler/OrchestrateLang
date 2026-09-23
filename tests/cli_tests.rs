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
