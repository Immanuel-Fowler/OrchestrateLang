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
    assert_compilation_fails("unsupported_foreign_language.orch", "is not supported currently");
    assert_compilation_fails("missing_ffi_sidecar.orch", "no sidecar file found");
    assert_compilation_fails("type_errors.orch", "Type Error");
    assert_compilation_fails("type_mismatch.orch", "Type Error");
    assert_compilation_fails("type_mismatch.orch", "mismatch");
    assert_compilation_fails("undefined_variable.orch", "Type Error");
    assert_compilation_fails("undefined_variable.orch", "undefined variable");
    assert_compilation_fails("index_not_int.orch", "Type Error");
    assert_compilation_fails("index_not_int.orch", "array index must be int");
    // The message has to name the function and say what to do, not leak `await` from
    // generated Rust the user never wrote.
    assert_compilation_fails("sync_fn_awaits.orch", "fn 'bump' calls 'counter.add'");
    assert_compilation_fails("sync_fn_awaits.orch", "declare it as a `task` instead");
    assert_compilation_fails("enum_unit_variant_binds.orch", "carries no value");
    assert_compilation_fails("enum_unit_variant_binds.orch", "Signal::Stop");
    assert_compilation_fails("task_uses_top_level_state.orch", "which is top-level state");
    assert_compilation_fails("task_uses_top_level_state.orch", "pass it in as a parameter");
}

/// Every wrong program must fail as OrchestrateLang, never as rustc.
///
/// The compiler generates Rust, so any check it does not make itself becomes a rustc error
/// against code the user never wrote — with line numbers into generated source and advice
/// about traits and moves that means nothing in this language. Each file in
/// `tests/error_cases/diagnostics/` is a program that must be rejected, and this asserts
/// the rejection is the compiler's own.
#[test]
fn diagnostics_never_leak_rustc() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/error_cases/diagnostics");
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("diagnostics directory")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|e| e == "orch"))
        .collect();
    cases.sort();
    assert!(cases.len() >= 25, "expected the audit corpus, found {} files", cases.len());

    let mut leaked = Vec::new();
    let mut accepted = Vec::new();
    for case in &cases {
        let output = Command::new(get_orchestrate_bin())
            .arg("build")
            .arg(case)
            .arg("-o")
            .arg(std::env::temp_dir().join("orch_diagnostics_probe"))
            .output()
            .expect("failed to run orchestrate");
        let name = case.file_stem().unwrap().to_string_lossy().to_string();
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if output.status.success() {
            accepted.push(name);
        } else if combined.contains("error[E") || combined.contains("rustc --explain") {
            let line = combined
                .lines()
                .find(|l| l.contains("error[E"))
                .unwrap_or("")
                .trim()
                .to_string();
            leaked.push(format!("{name}: {line}"));
        }
    }
    assert!(accepted.is_empty(), "these wrong programs were accepted: {accepted:?}");
    assert!(leaked.is_empty(), "these reached rustc instead of an OrchestrateLang error:\n  {}", leaked.join("\n  "));
}
