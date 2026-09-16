use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn check_foreign(file: &Path) -> (bool, String) {
    run_check_foreign(file, &[])
}

fn check_foreign_deep(file: &Path) -> (bool, String) {
    run_check_foreign(file, &["--deep"])
}

fn run_check_foreign(file: &Path, flags: &[&str]) -> (bool, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_orchestrate"))
        .arg("check-foreign")
        .arg(file)
        .args(flags)
        .output()
        .expect("Failed to run orchestrate process");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    (output.status.success(), combined)
}

/// A minimal project whose module loads one C source, written under `target/` so
/// the test can put a deliberate error in it.
fn write_c_project(name: &str, body: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let module = root.join("math");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&module).unwrap();
    fs::write(
        root.join("main.orch"),
        "use module math: \"./math\"\n\nlet worker = automatic {\n    print(to_string(math.twice(2.0)))\n    stop_orch()\n}\n\norchestrator main(procs: process[worker]) { }\n",
    )
    .unwrap();
    fs::write(module.join("module.orch"), "load_foreign \"c\" \"./calc.c\"\n").unwrap();
    fs::write(module.join("calc.orch_ffi"), "twice(value: float) -> float\n").unwrap();
    fs::write(module.join("calc.c"), body).unwrap();
    root
}

#[test]
fn test_reports_a_c_error_and_fails() {
    let root = write_c_project(
        "foreign_check_bad_c",
        "double twice(double value) { return no_such_function(value); }\n",
    );
    let (passed, output) = check_foreign(&root.join("main.orch"));
    assert!(!passed, "Expected a C error to fail the check:\n{}", output);
    assert!(output.contains("FAIL"), "Expected a FAIL row:\n{}", output);
    assert!(
        output.contains("no_such_function"),
        "Expected the C compiler's own diagnostic:\n{}",
        output
    );
}

#[test]
fn test_accepts_valid_c() {
    let root = write_c_project(
        "foreign_check_good_c",
        "double twice(double value) { return value * 2.0; }\n",
    );
    let (passed, output) = check_foreign(&root.join("main.orch"));
    assert!(passed, "Expected valid C to pass:\n{}", output);
    assert!(output.contains("ok"), "Expected an ok row:\n{}", output);
}

#[test]
fn test_reports_a_missing_source() {
    let root = write_c_project("foreign_check_missing_c", "");
    fs::remove_file(root.join("math/calc.c")).unwrap();
    let (passed, output) = check_foreign(&root.join("main.orch"));
    assert!(!passed, "Expected a missing source to fail:\n{}", output);
    assert!(
        output.contains("source file not found"),
        "Expected a clear missing-file error:\n{}",
        output
    );
}

/// Checking TypeScript needs only the type checker, not the Bun backend a build uses.
fn typescript_available() -> bool {
    let tsc = std::env::var_os("ORCH_TSC").unwrap_or_else(|| "tsc".into());
    let ok = Command::new(tsc).arg("--version").output().is_ok_and(|o| {
        o.status.success() && String::from_utf8_lossy(&o.stdout).starts_with("Version 7.")
    });
    if !ok {
        eprintln!("Skipping: TypeScript 7 is needed (install with bun add --dev typescript)");
    }
    ok
}

/// A project whose module loads one TypeScript source declaring `twice(float) -> float`.
fn write_typescript_project(name: &str, body: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let module = root.join("math");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&module).unwrap();
    fs::write(
        root.join("main.orch"),
        "use module math: \"./math\"\n\nlet worker = automatic {\n    print(to_string(math.twice(2.0)))\n    stop_orch()\n}\n\norchestrator main(procs: process[worker]) { }\n",
    )
    .unwrap();
    fs::write(module.join("module.orch"), "load_foreign \"typescript\" \"./calc.ts\"\n").unwrap();
    fs::write(module.join("calc.orch_ffi"), "twice(value: float) -> float\n").unwrap();
    fs::write(module.join("calc.ts"), body).unwrap();
    root
}

#[test]
fn test_accepts_typescript_matching_its_contract() {
    if !typescript_available() {
        return;
    }
    let root = write_typescript_project(
        "foreign_check_good_ts",
        "export function twice(value: number): number { return value * 2; }\n",
    );
    let (passed, output) = check_foreign(&root.join("main.orch"));
    assert!(passed, "Expected valid TypeScript to pass:\n{}", output);
}

#[test]
fn test_reports_a_typescript_type_error() {
    if !typescript_available() {
        return;
    }
    let root = write_typescript_project(
        "foreign_check_bad_ts",
        "export function twice(value: number): number {\n    const s: string = 42;\n    return value * 2;\n}\n",
    );
    let (passed, output) = check_foreign(&root.join("main.orch"));
    assert!(!passed, "Expected a TypeScript type error to fail:\n{}", output);
    assert!(
        output.contains("TS2322"),
        "Expected tsc's own diagnostic:\n{}",
        output
    );
}

/// The sidecar is a contract, so a source that compiles on its own but does not
/// match what the sidecar declares still has to fail.
#[test]
fn test_reports_typescript_that_breaks_its_sidecar_contract() {
    if !typescript_available() {
        return;
    }
    let root = write_typescript_project(
        "foreign_check_ts_contract",
        "export function twice(value: number): string { return String(value * 2); }\n",
    );
    let (passed, output) = check_foreign(&root.join("main.orch"));
    assert!(!passed, "Expected a contract violation to fail:\n{}", output);
    assert!(
        output.contains("Contract"),
        "Expected the mismatch to be reported against the contract:\n{}",
        output
    );
}

#[test]
fn test_reports_typescript_missing_a_declared_function() {
    if !typescript_available() {
        return;
    }
    let root = write_typescript_project(
        "foreign_check_ts_missing",
        "export function somethingElse(value: number): number { return value * 2; }\n",
    );
    let (passed, output) = check_foreign(&root.join("main.orch"));
    assert!(!passed, "Expected a missing function to fail:\n{}", output);
    assert!(
        output.contains("twice"),
        "Expected the missing function to be named:\n{}",
        output
    );
}

#[test]
fn test_skips_languages_the_build_already_checks() {
    let (passed, output) = check_foreign(Path::new("examples/foreign_rust_math.orch"));
    assert!(passed, "Expected the Rust example to pass:\n{}", output);
    assert!(
        output.contains("skip") && output.contains("cargo"),
        "Expected Rust to be reported as checked by cargo:\n{}",
        output
    );
}

fn mypy_available() -> bool {
    let direct = Command::new("mypy").arg("--version").output().is_ok_and(|o| o.status.success());
    let module = Command::new("python3")
        .args(["-m", "mypy", "--version"])
        .output()
        .is_ok_and(|o| o.status.success());
    if !direct && !module {
        eprintln!("Skipping: mypy is needed for the deep Python check (pip install mypy)");
    }
    direct || module
}

/// A project with one Python landline whose handler is declared `-> int`.
fn write_python_project(name: &str, body: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name);
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("main.orch"),
        "serverlet Counter via python(source: \"counter.py\") {\n    on add(amount: int) -> int\n}\n\norchestrator main() {\n    let counter = start Counter()\n    print(counter.add(10))\n    stop_orch()\n}\n",
    )
    .unwrap();
    fs::write(root.join("counter.py"), body).unwrap();
    root
}

/// `py_compile` only parses, so a type error has to pass the shallow check and
/// fail the deep one. This is the difference `--deep` exists to make.
#[test]
fn test_deep_catches_a_python_type_error_the_shallow_check_misses() {
    if !mypy_available() {
        return;
    }
    let root = write_python_project(
        "foreign_check_python_types",
        "from orchestratelang import landline\n\n\nclass Counter(landline.Serverlet):\n    def add(self, amount: int) -> int:\n        return \"not an int\"\n\n\nlandline.serve(Counter)\n",
    );
    let (shallow, shallow_output) = check_foreign(&root.join("main.orch"));
    assert!(
        shallow,
        "Expected the shallow check to pass a type error:\n{}",
        shallow_output
    );

    let (deep, deep_output) = check_foreign_deep(&root.join("main.orch"));
    assert!(!deep, "Expected --deep to catch the type error:\n{}", deep_output);
    assert!(
        deep_output.contains("Incompatible return value type"),
        "Expected mypy's own diagnostic:\n{}",
        deep_output
    );
}

#[test]
fn test_deep_accepts_valid_python() {
    if !mypy_available() {
        return;
    }
    let root = write_python_project(
        "foreign_check_python_good",
        "from orchestratelang import landline\n\n\nclass Counter(landline.Serverlet):\n    def __init__(self) -> None:\n        self.total = 0\n\n    def add(self, amount: int) -> int:\n        self.total += amount\n        return self.total\n\n\nlandline.serve(Counter)\n",
    );
    let (passed, output) = check_foreign_deep(&root.join("main.orch"));
    assert!(passed, "Expected valid Python to pass --deep:\n{}", output);
}

/// The reason the deep pass generates code instead of checking the Rust file on
/// its own: a `load_foreign` file may call functions its module defines, which a
/// standalone `rustc` would report as undefined.
#[test]
fn test_deep_accepts_rust_calling_a_module_function() {
    // Kept between runs, unlike the other fixtures: this is the one test that runs
    // cargo, and a cold `.orch_cache` would rebuild every dependency each time.
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("foreign_check_rust_context");
    let module = root.join("math");
    fs::create_dir_all(&module).unwrap();
    fs::write(
        root.join("main.orch"),
        "use module math: \"./math\"\n\nlet worker = automatic {\n    print(to_string(math.twice(2.0)))\n    stop_orch()\n}\n\norchestrator main(procs: process[worker]) { }\n",
    )
    .unwrap();
    fs::write(
        module.join("module.orch"),
        "load_foreign \"rust\" \"./calc.rs\"\nfn helper(n: int) -> int { return n * 2 }\n",
    )
    .unwrap();
    fs::write(module.join("calc.orch_ffi"), "twice(value: float) -> float\n").unwrap();
    fs::write(
        module.join("calc.rs"),
        "pub fn twice(value: f64) -> f64 {\n    let _bonus = helper(3);\n    value * 2.0\n}\n",
    )
    .unwrap();

    let (passed, output) = check_foreign_deep(&root.join("main.orch"));
    assert!(
        passed,
        "Expected Rust calling a module function to pass --deep:\n{}",
        output
    );
}

#[test]
fn test_reports_a_program_with_no_foreign_sources() {
    let (passed, output) = check_foreign(Path::new("examples/hello.orch"));
    assert!(passed, "Expected a program with no foreign code to pass:\n{}", output);
    assert!(
        output.contains("no foreign sources"),
        "Expected the empty case to say so:\n{}",
        output
    );
}
