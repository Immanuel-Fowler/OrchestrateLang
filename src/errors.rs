use std::path::Path;
use std::fs;
use std::env;

pub fn print_friendly_errors(stderr: &str, cache_dir: &Path) {
    let mut matched = false;
    if stderr.contains("error[E0425]: cannot find value") {
        eprintln!("[orchestrate] error: undefined variable — check your variable names");
        matched = true;
    } else if stderr.contains("error[E0308]: mismatched types") {
        eprintln!("[orchestrate] error: type mismatch — check that your function return types and variable assignments match");
        matched = true;
    } else if stderr.contains("error[E0061]: this function takes") {
        eprintln!("[orchestrate] error: wrong number of arguments to a function call");
        matched = true;
    } else if stderr.contains("error: linking with") {
        eprintln!("[orchestrate] error: linker failed — if using load_foreign 'c' or 'cpp', check that your C/C++ source compiles correctly");
        matched = true;
    } else if stderr.contains("error[E0412]: cannot find type") {
        eprintln!("[orchestrate] error: unknown type — this may be a compiler bug, please report it");
        matched = true;
    }

    if !matched {
        eprintln!("[orchestrate] Rust compilation error (this may indicate a bug in the Orchestrate compiler or a type mismatch in a foreign function):\n{}", stderr);
    }

    eprintln!("[orchestrate] tip: run with ORCH_SHOW_GENERATED=1 to see the generated Rust code");

    if env::var("ORCH_SHOW_GENERATED").unwrap_or_else(|_| "0".to_string()) == "1" {
        let main_rs_path = cache_dir.join("src/main.rs");
        if let Ok(code) = fs::read_to_string(&main_rs_path) {
            eprintln!("\n--- Generated Rust Code ({:?}) ---\n{}\n--- End Generated Code ---", main_rs_path, code);
        }
    }
}
