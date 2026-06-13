use std::env;

mod ast;
mod lexer;
mod parser;
mod codegen;
mod typechecker;
mod prom;
mod ffi_parser;
mod errors;
mod ffi_rust;
mod driver;

fn print_usage() {
    println!("Orchestrate Language Compiler");
    println!("Usage:");
    println!("  orchestrate run <file.orch>            Compile and run the program immediately");
    println!("  orchestrate build <file.orch>          Compile the program to a standalone binary");
    println!("  orchestrate build <file.orch> -o <out> Specify the output binary name");
    println!("  orchestrate prom add <name> <path>     Register a module path under a short name");
    println!("  orchestrate prom remove <name>         Remove a registered module");
    println!("  orchestrate prom list                  List all registered modules");
}

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() < 2 {
        print_usage();
        std::process::exit(1);
    }

    match args[1].as_str() {
        "run" => {
            if args.len() < 3 {
                print_usage();
                std::process::exit(1);
            }
            if let Err(e) = driver::run_run(&args[2]) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        "build" => {
            if args.len() < 3 {
                print_usage();
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
        "prom" => {
            if args.len() < 3 {
                print_usage();
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
                    eprintln!("Unknown prom subcommand: {}", args[2]);
                    print_usage();
                    std::process::exit(1);
                }
            }
        }
        _ => {
            print_usage();
            std::process::exit(1);
        }
    }
}
