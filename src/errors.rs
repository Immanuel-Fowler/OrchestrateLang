use std::path::Path;
use std::fs;
use std::env;
use std::collections::BTreeMap;

/// Parse `// orch:LINE:COL` comments from generated Rust source and build
/// a map from Rust line number → (orch_line, orch_col).
fn build_source_map(generated: &str) -> BTreeMap<usize, (usize, usize)> {
    let mut map = BTreeMap::new();
    let mut pending: Option<(usize, usize)> = None;
    for (rust_line_idx, line) in generated.lines().enumerate() {
        let rust_line = rust_line_idx + 1;
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("// orch:") {
            let parts: Vec<&str> = rest.splitn(2, ':').collect();
            if parts.len() == 2 {
                if let (Ok(ol), Ok(oc)) = (parts[0].parse::<usize>(), parts[1].parse::<usize>()) {
                    pending = Some((ol, oc));
                }
            }
        } else if let Some(loc) = pending.take() {
            map.insert(rust_line, loc);
        }
    }
    map
}

/// Try to extract `src/main.rs:LINE:` from a Rust compiler error line.
fn extract_rust_line(error_line: &str) -> Option<usize> {
    // Look for patterns like "src/main.rs:42:5" or "--> src/main.rs:42:5"
    for part in error_line.split_whitespace() {
        let part = part.trim_start_matches("-->");
        if let Some(rest) = part.strip_prefix("src/main.rs:") {
            let col_idx = rest.find(':').unwrap_or(rest.len());
            if let Ok(n) = rest[..col_idx].parse::<usize>() {
                return Some(n);
            }
        }
    }
    // Also handle absolute paths ending in main.rs
    if let Some(idx) = error_line.find("main.rs:") {
        let after = &error_line[idx + "main.rs:".len()..];
        let col_idx = after.find(':').unwrap_or(after.len());
        if let Ok(n) = after[..col_idx].parse::<usize>() {
            return Some(n);
        }
    }
    None
}

/// Attempt to remap a Rust compiler error message to .orch source locations
/// using the `// orch:LINE:COL` source-map comments.
fn remap_error(stderr: &str, source_map: &BTreeMap<usize, (usize, usize)>, input_file: &str) -> Option<String> {
    if source_map.is_empty() {
        return None;
    }

    let mut output_lines = Vec::new();
    let mut any_remapped = false;

    for line in stderr.lines() {
        if let Some(rust_line) = extract_rust_line(line) {
            // Find the closest orch location at or before this rust line
            let loc = source_map.range(..=rust_line).next_back().map(|(_, v)| *v);
            if let Some((orch_line, orch_col)) = loc {
                let remapped = format!("{}:{}:{}: (in generated code)", input_file, orch_line, orch_col);
                output_lines.push(remapped);
                any_remapped = true;
                continue;
            }
        }
        output_lines.push(line.to_string());
    }

    if any_remapped {
        Some(output_lines.join("\n"))
    } else {
        None
    }
}

pub fn print_friendly_errors(stderr: &str, cache_dir: &Path) {
    // Attempt source-map remapping
    let input_file_hint = env::var("ORCH_SOURCE_FILE").unwrap_or_else(|_| "<source>.orch".to_string());
    let main_rs_path = cache_dir.join("src/main.rs");
    let source_map = if let Ok(code) = fs::read_to_string(&main_rs_path) {
        build_source_map(&code)
    } else {
        BTreeMap::new()
    };

    // Print remapped errors if we can translate them
    if let Some(remapped) = remap_error(stderr, &source_map, &input_file_hint) {
        eprintln!("[orchestrate] compilation error — locations remapped to .orch source:");
        eprintln!("{}", remapped);
    } else {
        // Fall back to pattern-based friendly messages
        let friendly = get_friendly_message(stderr);
        if let Some(msg) = friendly {
            eprintln!("[orchestrate] error: {}", msg);
        } else {
            eprintln!("[orchestrate] Rust compilation error (this may indicate a compiler bug or a type mismatch in a foreign function):\n{}", stderr);
        }
    }

    eprintln!("[orchestrate] tip: run with ORCH_SHOW_GENERATED=1 to see the generated Rust code");

    if env::var("ORCH_SHOW_GENERATED").unwrap_or_else(|_| "0".to_string()) == "1" {
        if let Ok(code) = fs::read_to_string(&main_rs_path) {
            eprintln!("\n--- Generated Rust Code ({:?}) ---\n{}\n--- End Generated Code ---", main_rs_path, code);
        }
    }
}

fn get_friendly_message(stderr: &str) -> Option<&'static str> {
    // Check from most-specific to least-specific
    if stderr.contains("error[E0425]: cannot find value") {
        Some("undefined variable — check your variable names and that all variables are declared with `let` before use")
    } else if stderr.contains("error[E0308]: mismatched types") {
        Some("type mismatch — check that your function return types, variable assignments, and if-else branches use the same type")
    } else if stderr.contains("error[E0061]: this function takes") {
        Some("wrong number of arguments to a function call — check the function signature")
    } else if stderr.contains("error[E0507]: cannot move out of") {
        Some("value used after move — try calling .clone() on the value you want to keep using")
    } else if stderr.contains("error[E0502]: cannot borrow") || stderr.contains("error[E0499]: cannot borrow") {
        Some("borrow conflict in generated code — this may be a compiler bug, please report it")
    } else if stderr.contains("error[E0277]: the trait bound") {
        Some("type does not implement a required trait — ensure struct fields used with `+` implement the right types, or add `#[derive(Clone)]`")
    } else if stderr.contains("error[E0369]: binary operation") {
        Some("operator not supported for this type — check that both sides of an operator have the same numeric type")
    } else if stderr.contains("error[E0596]: cannot borrow") {
        Some("cannot borrow as mutable — this may indicate a compiler bug in variable capture, please report it")
    } else if stderr.contains("error[E0004]: non-exhaustive patterns") {
        Some("match expression does not cover all cases — add a wildcard arm `_ => { }` to handle remaining variants")
    } else if stderr.contains("error[E0412]: cannot find type") {
        Some("unknown type referenced — this is likely a compiler bug, please report it")
    } else if stderr.contains("error[E0428]: the name") && stderr.contains("is defined multiple times") {
        Some("duplicate definition — you have two functions, serverlets, or enums with the same name")
    } else if stderr.contains("error[E0433]: failed to resolve") {
        Some("unresolved path — check your module names and `use` statements")
    } else if stderr.contains("error[E0599]: no method named") {
        Some("unknown method — this may be a compiler bug if using a built-in operation, or the type does not support this operation")
    } else if stderr.contains("error[E0382]: borrow of moved value") || stderr.contains("error[E0382]: use of moved value") {
        Some("value used after it was moved — add .clone() where you need to use a value in multiple places")
    } else if stderr.contains("error: linking with") {
        Some("linker failed — if using load_foreign 'c' or 'cpp', check that your C/C++ source compiles and exports the expected symbols")
    } else if stderr.contains("error[E0080]") {
        Some("compile-time evaluation error — a constant expression could not be evaluated")
    } else {
        None
    }
}
