use std::process::Command;
use std::path::PathBuf;

fn get_orchestrate_bin() -> PathBuf {
    assert!(PathBuf::from(env!("CARGO_BIN_EXE_orchestrate")).exists());
    PathBuf::from(env!("CARGO_BIN_EXE_orchestrate"))
}

fn assert_compilation_fails(file: &str, expected_msg: &str) {
    let output = Command::new(get_orchestrate_bin())
        .arg("run")
        .arg(format!("tests/error_cases/{}", file))
        .output()
        .expect("Failed to run orchestrate process");

    assert!(
        !output.status.success(),
        "Expected {} to fail compilation, but it succeeded",
        file
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let combined_output = format!("{}\n{}", stdout, stderr);

    assert!(
        combined_output.contains(expected_msg),
        "Expected {} to contain error message '{}', but output was:\n{}",
        file,
        expected_msg,
        combined_output
    );
}

#[test]
fn test_error_cases() {
    assert_compilation_fails("err_test.orch", "is not supported currently");
    assert_compilation_fails("err_test2.orch", "no sidecar file found");
    assert_compilation_fails("test_type_errors.orch", "Type Error");
    assert_compilation_fails("test_type_mismatch.orch", "Type Error");
    assert_compilation_fails("test_type_mismatch.orch", "Mismatch");
    assert_compilation_fails("test_undefined_var.orch", "Type Error");
    assert_compilation_fails("test_undefined_var.orch", "undefined variable");
}
