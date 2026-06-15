use std::env;
use orchestrate_lib::{driver, prom};

fn print_help() {
    println!("Orchestrate Language Compiler v{}", env!("CARGO_PKG_VERSION"));
    println!();
    println!("USAGE:");
    println!("  orchestrate <command> [options]");
    println!();
    println!("COMMANDS:");
    println!("  run <file.orch>              Compile and run a program immediately");
    println!("  build <file.orch>            Compile to a standalone binary");
    println!("  build <file.orch> -o <out>   Specify the output binary name");
    println!("  check <file.orch>            Type-check only — no compilation (fast)");
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
    println!("  orchestrate run hello.orch");
    println!("  orchestrate build main.orch -o myapp");
}

fn print_short_usage() {
    println!("Orchestrate Language Compiler");
    println!("Run 'orchestrate --help' for usage.");
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print_short_usage();
        std::process::exit(1);
    }

    match args[1].as_str() {
        "run" => {
            if args.len() < 3 {
                eprintln!("Usage: orchestrate run <file.orch>");
                std::process::exit(1);
            }
            if let Err(e) = driver::run_run(&args[2]) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        "build" => {
            if args.len() < 3 {
                eprintln!("Usage: orchestrate build <file.orch> [-o <output>]");
                std::process::exit(1);
            }
            let input = &args[2];
            let mut out = None;
            if args.len() >= 5 && args[3] == "-o" {
                out = Some(args[4].as_str());
            }
            if let Err(e) = driver::run_build(input, out) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        "check" => {
            if args.len() < 3 {
                eprintln!("Usage: orchestrate check <file.orch>");
                std::process::exit(1);
            }
            if let Err(e) = driver::run_check(&args[2]) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        "prom" => {
            if args.len() < 3 {
                eprintln!("Usage: orchestrate prom <add|remove|list> [args]");
                std::process::exit(1);
            }
            match args[2].as_str() {
                "add" => {
                    if args.len() < 5 {
                        eprintln!("Usage: orchestrate prom add <name> <path>");
                        std::process::exit(1);
                    }
                    if let Err(e) = prom::prom_add(&args[3], &args[4]) {
                        eprintln!("{}", e);
                        std::process::exit(1);
                    }
                }
                "remove" => {
                    if args.len() < 4 {
                        eprintln!("Usage: orchestrate prom remove <name>");
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
            print_help();
        }
        _ => {
            eprintln!("Unknown command: '{}'. Run 'orchestrate --help' for usage.", args[1]);
            std::process::exit(1);
        }
    }
}
