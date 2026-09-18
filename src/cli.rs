use crate::{driver, prom};

fn print_help(invocation: &str) {
    println!("OrchestrateLang compiler v{}", env!("CARGO_PKG_VERSION"));
    println!();
    println!("USAGE:");
    println!("  {} <command> [options]", invocation);
    println!();
    println!("COMMANDS:");
    println!("  run <file.orch>              Compile and run a program immediately");
    println!("  build <file.orch>            Compile to a standalone binary");
    println!("  build <file.orch> -o <out>   Specify the output binary name");
    println!("  build --lib <file.orch> -o <dir>   Generate a host-linkable Rust crate");
    println!("  build --lib --rust-version <x.y>   Set the generated crate's rust-version");
    println!("  build --lib --dependency '<name> = <spec>'   Add a Cargo dependency to the generated crate (repeatable)");
    println!("  build --lib --dependencies <file.toml>       Add the dependencies a TOML fragment declares");
    println!("  check <file.orch>            Type-check only — no compilation (fast)");
    println!("  check-foreign <file.orch>    Check foreign sources with their own language's checker");
    println!("  check-foreign --deep         Also run mypy on Python and cargo check on Rust (slower)");
    println!();
    println!("  prom add <name> <path>       Register a module path under a short name");
    println!("  prom remove <name>           Remove a registered module");
    println!("  prom list                    List all registered modules");
    println!();
    println!("FLAGS:");
    println!("  --help, -h                   Show this help message");
    println!();
    println!("ENVIRONMENT:");
    println!("  ORCH_SHOW_GENERATED=1        Print the generated Rust code before compiling");
    println!();
    println!("EXAMPLES:");
    println!("  {} run hello.orch", invocation);
    println!("  {} build main.orch -o myapp", invocation);
}

/// `--dependency 'name = spec'` and `--dependencies <file>` entries, with relative paths
/// resolved against the working directory the command runs in.
fn parse_dependency_args(args: &[(&str, &str)]) -> Result<Vec<crate::dependencies::Dependency>, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let mut dependencies = Vec::new();
    for (flag, value) in args {
        let (text, origin) = if *flag == "--dependency" {
            (value.to_string(), "--dependency".to_string())
        } else {
            (std::fs::read_to_string(value).map_err(|e| format!("cannot read --dependencies {}: {}", value, e))?, value.to_string())
        };
        dependencies.extend(crate::dependencies::parse_section(&text, &cwd, &origin)?);
    }
    Ok(dependencies)
}

fn print_short_usage(invocation: &str) {
    println!("OrchestrateLang compiler");
    println!("Run '{} --help' for usage.", invocation);
}

/// Dispatch one CLI invocation. `args` starts with the program name, as `env::args` yields it.
/// `invocation` is how the user spelled the command, so help and errors echo it back.
pub fn run(invocation: &str, args: &[String]) {
    if args.len() < 2 {
        print_short_usage(invocation);
        std::process::exit(1);
    }

    match args[1].as_str() {
        "run" => {
            if args.len() < 3 {
                eprintln!("Usage: {} run <file.orch>", invocation);
                std::process::exit(1);
            }
            if let Err(e) = driver::run_run(&args[2]) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        "build" => {
            if args.len() < 3 {
                eprintln!("Usage: {} build <file.orch> [-o <output>]", invocation);
                std::process::exit(1);
            }
            let mut input = None;
            let mut out = None;
            let mut library = false;
            let mut target = None;
            let mut rust_version = None;
            let mut dependency_args: Vec<(&str, &str)> = Vec::new();
            let mut options = args[2..].iter();
            while let Some(arg) = options.next() {
                match arg.as_str() {
                    "--lib" => library = true,
                    "--target" => target = options.next().map(String::as_str),
                    "--rust-version" => rust_version = options.next().map(String::as_str),
                    "--dependency" | "--dependencies" => match options.next() {
                        Some(value) => dependency_args.push((arg.as_str(), value.as_str())),
                        None => { eprintln!("{} needs a value", arg); std::process::exit(1); }
                    },
                    "-o" => out = options.next().map(String::as_str),
                    value if !value.starts_with('-') && input.is_none() => input = Some(value),
                    _ => { eprintln!("Unknown build argument: {}", arg); std::process::exit(1); }
                }
            }
            let result = match input {
                Some(input) if library => parse_dependency_args(&dependency_args)
                    .and_then(|dependencies| driver::run_build_library_for_target(input, out, target, rust_version, &dependencies)),
                Some(_) if rust_version.is_some() => Err("--rust-version applies to build --lib".into()),
                Some(_) if !dependency_args.is_empty() => Err("--dependency and --dependencies apply to build --lib".into()),
                Some(input) => driver::run_build(input, out),
                None => Err("build requires an input file".into()),
            };
            if let Err(e) = result { eprintln!("Error: {}", e); std::process::exit(1); }
        }

        "check" => {
            if args.len() < 3 {
                eprintln!("Usage: {} check <file.orch>", invocation);
                std::process::exit(1);
            }
            if let Err(e) = driver::run_check(&args[2]) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        "check-foreign" => {
            let mut input = None;
            let mut deep = false;
            for arg in &args[2..] {
                match arg.as_str() {
                    "--deep" => deep = true,
                    value if !value.starts_with('-') && input.is_none() => input = Some(value),
                    _ => {
                        eprintln!("Unknown check-foreign argument: {}", arg);
                        std::process::exit(1);
                    }
                }
            }
            let Some(input) = input else {
                eprintln!("Usage: {} check-foreign <file.orch> [--deep]", invocation);
                std::process::exit(1);
            };
            if let Err(e) = driver::run_check_foreign(input, deep) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        "prom" => {
            if args.len() < 3 {
                eprintln!("Usage: {} prom <add|remove|list> [args]", invocation);
                std::process::exit(1);
            }
            match args[2].as_str() {
                "add" => {
                    if args.len() < 5 {
                        eprintln!("Usage: {} prom add <name> <path>", invocation);
                        std::process::exit(1);
                    }
                    if let Err(e) = prom::prom_add(&args[3], &args[4]) {
                        eprintln!("{}", e);
                        std::process::exit(1);
                    }
                }
                "remove" => {
                    if args.len() < 4 {
                        eprintln!("Usage: {} prom remove <name>", invocation);
                        std::process::exit(1);
                    }
                    if let Err(e) = prom::prom_remove(&args[3]) {
                        eprintln!("{}", e);
                        std::process::exit(1);
                    }
                }
                "list" => {
                    if let Err(e) = prom::prom_list() {
                        eprintln!("{}", e);
                        std::process::exit(1);
                    }
                }
                _ => {
                    eprintln!("Unknown prom subcommand: '{}'. Expected add, remove, or list.", args[2]);
                    std::process::exit(1);
                }
            }
        }
        "--help" | "-h" | "help" => {
            print_help(invocation);
        }
        _ => {
            eprintln!("Unknown command: '{}'. Run '{} --help' for usage.", args[1], invocation);
            std::process::exit(1);
        }
    }
}

/// Drop the subcommand name cargo inserts. `cargo orch run x.orch` runs
/// `cargo-orch orch run x.orch`, but the binary also runs directly, where there is
/// nothing to strip.
pub fn strip_cargo_subcommand(subcommand: &str, args: &[String]) -> Vec<String> {
    if args.len() >= 2 && args[1] == subcommand {
        let mut stripped = vec![args[0].clone()];
        stripped.extend_from_slice(&args[2..]);
        stripped
    } else {
        args.to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::strip_cargo_subcommand;

    fn args(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn test_strips_cargo_inserted_subcommand() {
        let given = args(&["cargo-orch", "orch", "run", "main.orch"]);
        assert_eq!(strip_cargo_subcommand("orch", &given), args(&["cargo-orch", "run", "main.orch"]));
    }

    #[test]
    fn test_keeps_args_when_invoked_directly() {
        let given = args(&["cargo-orch", "run", "main.orch"]);
        assert_eq!(strip_cargo_subcommand("orch", &given), given);
    }

    #[test]
    fn test_keeps_a_later_arg_that_matches_the_subcommand() {
        let given = args(&["cargo-orch", "run", "orch"]);
        assert_eq!(strip_cargo_subcommand("orch", &given), given);
    }

    #[test]
    fn test_bare_subcommand_leaves_no_command() {
        let given = args(&["cargo-orch", "orch"]);
        assert_eq!(strip_cargo_subcommand("orch", &given), args(&["cargo-orch"]));
    }
}
