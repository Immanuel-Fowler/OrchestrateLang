//! Check foreign sources with each language's own checker, without compiling or linking.

use std::path::Path;
use std::process::Command;

pub enum Outcome {
    Checked,
    /// Checked, with a note naming the tool used or the limit of the check.
    CheckedWith(&'static str),
    /// Rust, which is only checked by the deep pass or by a build.
    Deferred,
}

/// Write the Python landline SDK where mypy can resolve `import orchestratelang`,
/// so a landline's use of the SDK is type-checked rather than ignored.
fn stage_python_sdk(scratch: &Path) -> Result<std::path::PathBuf, String> {
    let root = scratch.join("pythonpath");
    let package = root.join("orchestratelang");
    std::fs::create_dir_all(&package).map_err(|e| e.to_string())?;
    std::fs::write(
        package.join("__init__.py"),
        include_str!("../sdk/python/orchestratelang/__init__.py"),
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(
        package.join("landline.py"),
        include_str!("../sdk/python/orchestratelang/landline.py"),
    )
    .map_err(|e| e.to_string())?;
    Ok(root)
}

/// Type-check a Python source with mypy. Only the deep pass runs this: mypy is not
/// in the standard library, so a missing mypy is an error rather than a silent skip.
fn check_python_types(path: &str, scratch: &Path) -> Result<Outcome, String> {
    let sdk = stage_python_sdk(scratch)?;
    let program = ["mypy", "python3", "python"]
        .into_iter()
        .find(|p| Command::new(p).arg("--version").output().is_ok())
        .ok_or("neither mypy nor python was found on PATH")?;
    let mut command = Command::new(program);
    if program != "mypy" {
        command.args(["-m", "mypy"]);
    }
    let output = command
        .args(["--ignore-missing-imports", "--no-error-summary"])
        .arg("--cache-dir")
        .arg(scratch.join("mypy_cache"))
        .arg(path)
        .current_dir(scratch)
        .env("MYPYPATH", &sdk)
        .output()
        .map_err(|e| format!("failed to run mypy: {}", e))?;
    let report = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if output.status.success() {
        return Ok(Outcome::CheckedWith("mypy"));
    }
    if report.contains("No module named mypy") || report.contains("no module named mypy") {
        return Err(
            "mypy is not installed — install it with `pip install mypy`, or drop --deep to check syntax only"
                .into(),
        );
    }
    Err(report.trim().to_string())
}

/// Candidate programs to run, the check-only arguments that precede the source
/// path, and what to tell someone who has none of the candidates installed.
fn checker(language: &str) -> Option<(&'static [&'static str], &'static [&'static str], &'static str)> {
    match language {
        "c" => Some((
            &["cc", "clang", "gcc"],
            &["-fsyntax-only"],
            "install a C compiler (Xcode Command Line Tools on macOS, build-essential on Linux)",
        )),
        "cpp" => Some((
            &["c++", "clang++", "g++"],
            &["-fsyntax-only"],
            "install a C++ compiler (Xcode Command Line Tools on macOS, build-essential on Linux)",
        )),
        "zig" => Some((
            &["zig"],
            &["build-obj", "-fno-emit-bin"],
            "install Zig 0.16 or newer (https://ziglang.org/download/)",
        )),
        "swift" => Some((
            &["swiftc"],
            &["-typecheck"],
            "install Swift 5.10 or newer (https://www.swift.org/install/)",
        )),
        "python" => Some((
            &["python3", "python"],
            &["-m", "py_compile"],
            "install Python 3.10 or newer (https://www.python.org/downloads/)",
        )),
        _ => None,
    }
}

/// Check one foreign source. `scratch` is where checkers that write caches put
/// them, so a check never leaves artifacts beside the source.
pub fn check_file(
    language: &str,
    path: &Path,
    scratch: &Path,
    deep: bool,
) -> Result<Outcome, String> {
    // A Rust `load_foreign` file is concatenated with its module's generated code
    // and may call into it, so checking it alone would report calls to module
    // functions as undefined. Only the deep pass, which generates that code, can
    // check it without reporting those calls as errors.
    if language == "rust" {
        return Ok(Outcome::Deferred);
    }
    let (candidates, args, hint) = checker(language)
        .ok_or_else(|| format!("'{}' is not a supported foreign language", language))?;
    if !path.is_file() {
        return Err(format!("source file not found: {}", path.display()));
    }
    // Checkers run in the scratch directory so their caches land there, so the
    // source has to be named absolutely.
    let absolute = path.canonicalize().map_err(|e| e.to_string())?;
    let absolute = absolute.to_string_lossy();
    let absolute = absolute.strip_prefix(r"\\?\").unwrap_or(&absolute);
    let program = candidates
        .iter()
        .find(|p| Command::new(p).arg("--version").output().is_ok())
        .ok_or_else(|| {
            format!(
                "no checker found on PATH (tried {}) — {}",
                candidates.join(", "),
                hint
            )
        })?;
    let output = Command::new(program)
        .args(args)
        .arg(absolute)
        .current_dir(scratch)
        .env("PYTHONPYCACHEPREFIX", scratch)
        .output()
        .map_err(|e| format!("failed to run `{}`: {}", program, e))?;
    if output.status.success() {
        // py_compile only parses, so Python is the one language whose types are
        // left to the deep pass.
        if language == "python" {
            return if deep {
                check_python_types(absolute, scratch)
            } else {
                Ok(Outcome::CheckedWith("syntax only; --deep adds mypy"))
            };
        }
        return Ok(Outcome::Checked);
    }
    let mut diagnostics = String::from_utf8_lossy(&output.stderr).into_owned();
    diagnostics.push_str(&String::from_utf8_lossy(&output.stdout));
    let diagnostics = diagnostics.trim();
    if diagnostics.is_empty() {
        return Err(format!("`{}` reported a failure with no output", program));
    }
    Err(diagnostics.to_string())
}
