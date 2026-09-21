use std::process::Command;
use std::path::PathBuf;

/// Removes what a passing test built: the `.orch_cache` a build leaves beside the source
/// it compiled, and the probe binary. `ORCH_KEEP_TEST_BUILDS=1` keeps them.
fn remove_build_leftovers(paths: &[PathBuf]) {
    if std::env::var_os("ORCH_KEEP_TEST_BUILDS").is_some_and(|v| v == "1") {
        return;
    }
    for path in paths {
        let _ = std::fs::remove_dir_all(path);
        let _ = std::fs::remove_file(path);
    }
}

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

/// `orchestrate check` must reject the file with the message, before any build.
fn assert_check_fails(file: &str, expected_msg: &str) {
    let output = Command::new(get_orchestrate_bin())
        .arg("check")
        .arg(format!("tests/error_cases/{}", file))
        .output()
        .expect("Failed to run orchestrate process");
    assert!(!output.status.success(), "Expected check of {} to fail, but it passed", file);
    let combined_output = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        combined_output.contains(expected_msg),
        "Expected check of {} to contain '{}', but output was:\n{}",
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
    // A sandbox handler type that cannot cross is the typechecker's error, so `check`
    // reports it and no guest crate is ever built.
    assert_compilation_fails("sandbox_unsupported_type.orch", "does not cross the sandbox boundary");
    assert_compilation_fails("sandbox_unsupported_type.orch", "parameter 'l' has type Label");
    // A guest reaches only what it was granted; the typechecker says so, so `check`
    // catches it in a library program that `run` would refuse for other reasons first.
    assert_check_fails("sandbox_ungranted_host_call.orch", "calls world.reset, which it was not granted");
    assert_check_fails("sandbox_ungranted_host_call.orch", "add `grant call world.reset`");
    // on_crash runs on the host; the guest's state is gone by then.
    assert_compilation_fails("sandbox_crash_uses_state.orch", "on_crash uses 'calls', which is the guest's state");
    remove_build_leftovers(&[PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/error_cases/.orch_cache")]);
}

/// Every wrong program must fail as OrchestrateLang, never as rustc.
///
/// The compiler generates Rust, so any check it does not make itself becomes a rustc error
/// against code the user never wrote — with line numbers into generated source and advice
/// about traits and moves that means nothing in this language. Each file in
/// `tests/error_cases/diagnostics/` is a program that must be rejected. This asserts the
/// rejection is the compiler's own, counts how many are rejected by `orchestrate check`
/// alone, and holds the ones that still reach rustc to the list in `KNOWN_LEAKS.txt`:
/// a leak that is not listed fails the test, and a listed case that no longer leaks fails
/// it too, so the list is always exactly the truth. `benchmarks/diagnostics_coverage.py`
/// prints the same classification as a table.
#[test]
fn diagnostics_never_leak_rustc() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/error_cases/diagnostics");
    let mut cases: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("diagnostics directory")
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|path| path.extension().is_some_and(|e| e == "orch"))
        .collect();
    cases.sort();
    assert!(cases.len() >= 40, "expected the audit corpus, found {} files", cases.len());
    let known_leaks: std::collections::BTreeSet<String> = std::fs::read_to_string(dir.join("KNOWN_LEAKS.txt"))
        .expect("KNOWN_LEAKS.txt")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect();

    let mut by_check = 0usize;
    let mut by_build = 0usize;
    let mut through_cargo = 0usize;
    let mut leaked = std::collections::BTreeSet::new();
    let mut accepted = Vec::new();
    for case in &cases {
        let name = case.file_stem().unwrap().to_string_lossy().to_string();
        let check = Command::new(get_orchestrate_bin())
            .arg("check")
            .arg(case)
            .output()
            .expect("failed to run orchestrate check");
        if !check.status.success() {
            by_check += 1;
            continue;
        }
        let output = Command::new(get_orchestrate_bin())
            .arg("build")
            .arg(case)
            .arg("-o")
            .arg(std::env::temp_dir().join("orch_diagnostics_probe"))
            .output()
            .expect("failed to run orchestrate");
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if output.status.success() {
            accepted.push(name);
        } else if combined.contains("error[E") || combined.contains("rustc --explain") {
            leaked.insert(name);
        } else if combined.contains("Cargo compilation failed") {
            through_cargo += 1;
        } else {
            by_build += 1;
        }
    }
    let total = cases.len();
    println!(
        "diagnostics corpus: {by_check} of {total} invalid programs rejected by `orchestrate check`; \
         {by_build} more by the build before Cargo; {through_cargo} through Cargo without a rustc error code; \
         {} reach rustc (all listed in KNOWN_LEAKS.txt); {} wrongly accepted",
        leaked.len(),
        accepted.len()
    );
    assert!(accepted.is_empty(), "these wrong programs were accepted: {accepted:?}");
    let unlisted: Vec<&String> = leaked.difference(&known_leaks).collect();
    assert!(unlisted.is_empty(), "these reached rustc and are not listed in KNOWN_LEAKS.txt: {unlisted:?}");
    let stale: Vec<&String> = known_leaks.difference(&leaked).collect();
    assert!(stale.is_empty(), "these are listed in KNOWN_LEAKS.txt but no longer leak; remove them: {stale:?}");
    remove_build_leftovers(&[dir.join(".orch_cache"), std::env::temp_dir().join("orch_diagnostics_probe")]);
}
