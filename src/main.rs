use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

mod ast;
mod lexer;
mod parser;
mod codegen;
mod typechecker;
mod prom;

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

fn prepare_cache_dir(input_path: &Path) -> Result<PathBuf, String> {
    let parent = input_path.parent().unwrap_or(Path::new("."));
    let cache_dir = parent.join(".orch_cache");
    fs::create_dir_all(cache_dir.join("src")).map_err(|e| format!("Failed to create cache directory: {}", e))?;

    let cargo_toml_content = r#"[package]
name = "orch_generated"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1.35", features = ["full"] }
"#;

    fs::write(cache_dir.join("Cargo.toml"), cargo_toml_content)
        .map_err(|e| format!("Failed to write Cargo.toml: {}", e))?;

    Ok(cache_dir)
}

fn compile_module(dir_path: &Path) -> Result<Vec<ast::Stmt>, String> {
    let entry_file = dir_path.join("module.orch");
    if !entry_file.exists() {
        return Err(format!("Module entry file not found: {:?}", entry_file));
    }
    
    let source = fs::read_to_string(&entry_file)
        .map_err(|e| format!("Failed to read module file {:?}: {}", entry_file, e))?;
    
    let mut lexer = lexer::Lexer::new(&source);
    let tokens = lexer.tokenize()?;
    let mut parser = parser::Parser::new(tokens);
    let raw_stmts = parser.parse()?;
    
    let mut merged_stmts = Vec::new();
    for stmt in raw_stmts {
        if let ast::Stmt::Load { path } = stmt {
            let nested = resolve_load_recursive(&path, dir_path)?;
            merged_stmts.extend(nested);
        } else {
            merged_stmts.push(stmt);
        }
    }
    
    Ok(merged_stmts)
}

fn resolve_load_recursive(path_str: &str, dir_path: &Path) -> Result<Vec<ast::Stmt>, String> {
    let sub_file = dir_path.join(path_str);
    let sub_source = fs::read_to_string(&sub_file)
        .map_err(|e| format!("Failed to read loaded file {:?}: {}", sub_file, e))?;
    let mut sub_lexer = lexer::Lexer::new(&sub_source);
    let sub_tokens = sub_lexer.tokenize()?;
    let mut sub_parser = parser::Parser::new(sub_tokens);
    let sub_stmts = sub_parser.parse()?;
    
    let mut merged = Vec::new();
    for stmt in sub_stmts {
        if let ast::Stmt::Load { path } = stmt {
            let nested = resolve_load_recursive(&path, dir_path)?;
            merged.extend(nested);
        } else {
            merged.push(stmt);
        }
    }
    Ok(merged)
}

fn compile_main_file_and_modules(input_file: &str, cache_dir: &Path) -> Result<String, String> {
    let input_path = Path::new(input_file);
    let source = fs::read_to_string(input_path)
        .map_err(|e| format!("Failed to read source file '{}': {}", input_file, e))?;

    let mut lexer = lexer::Lexer::new(&source);
    let tokens = lexer.tokenize()?;

    let mut parser = parser::Parser::new(tokens);
    let ast = parser.parse()?;

    // Type checking phase
    let mut type_checker = typechecker::TypeChecker::new();
    type_checker.type_check(&ast).map_err(|e| format!("Type Error: {}", e))?;

    let parent_dir = input_path.parent().unwrap_or(Path::new("."));
    let mut modules_registered = Vec::new();
    let mut all_tasks = std::collections::HashSet::new();
    
    for stmt in &ast {
        if let ast::Stmt::TaskDecl { name, .. } = stmt {
            all_tasks.insert(name.clone());
        }
        if let ast::Stmt::ProcessDecl { name, .. } = stmt {
            all_tasks.insert(name.clone());
        }
        if let ast::Stmt::OrchestratorDecl { name, .. } = stmt {
            all_tasks.insert(name.clone());
        }
    }

    let mut modules_data = Vec::new();
    for stmt in &ast {
        if let ast::Stmt::UseModule { local_name, module_name } = stmt {
            let module_path = if let Some(resolved) = prom::resolve_module(module_name)? {
                resolved
            } else if module_name.contains('/') || module_name.contains('\\') || module_name.starts_with('.') {
                parent_dir.join(module_name)
            } else {
                return Err(format!("Module '{}' is not a path and is not registered in PROM. Run 'orchestrate prom add <name> <path>' to register it, or use a './'-prefixed path.", module_name));
            };
            
            let module_stmts = compile_module(&module_path)?;
            
            for m_stmt in &module_stmts {
                if let ast::Stmt::TaskDecl { name, .. } = m_stmt {
                    all_tasks.insert(format!("{}::{}", local_name, name));
                }
                if let ast::Stmt::ProcessDecl { name, .. } = m_stmt {
                    all_tasks.insert(format!("{}::{}", local_name, name));
                }
                if let ast::Stmt::OrchestratorDecl { name, .. } = m_stmt {
                    all_tasks.insert(format!("{}::{}", local_name, name));
                }
            }
            
            modules_data.push((local_name.clone(), module_stmts, module_path));
            modules_registered.push(local_name.clone());
        }
    }

    for (local_name, module_stmts, module_path) in modules_data {
        let mut generator = codegen::Codegen::new(all_tasks.clone());
        let mut module_rust_code = generator.generate(&module_stmts, false);
        
        let mut foreign_code = String::new();
        for stmt in &module_stmts {
            if let ast::Stmt::LoadForeign { language, path } = stmt {
                if language != "rust" {
                    return Err("load_foreign: only 'rust' is supported currently".to_string());
                }
                let foreign_path = module_path.join(path);
                let code = fs::read_to_string(&foreign_path)
                    .map_err(|e| format!("Failed to read foreign file {:?}: {}", foreign_path, e))?;
                foreign_code.push_str(&code);
                foreign_code.push_str("\n");
            }
        }
        
        if !foreign_code.is_empty() {
            foreign_code.push_str(&module_rust_code);
            module_rust_code = foreign_code;
        }

        let module_out_file = cache_dir.join("src").join(format!("{}.rs", local_name));
        fs::write(&module_out_file, module_rust_code)
            .map_err(|e| format!("Failed to write module Rust code: {}", e))?;
    }

    let mut generator = codegen::Codegen::new(all_tasks);
    let main_rust = generator.generate(&ast, true);

    Ok(main_rust)
}

fn run_build(input_file: &str, output_binary: Option<&str>) -> Result<(), String> {
    println!("[Orchestrate] Parsing and compiling '{}'...", input_file);
    let cache_dir = prepare_cache_dir(Path::new(input_file))?;
    let rust_code = compile_main_file_and_modules(input_file, &cache_dir)?;
    
    fs::write(cache_dir.join("src/main.rs"), rust_code)
        .map_err(|e| format!("Failed to write generated Rust file: {}", e))?;

    println!("[Orchestrate] Building release binary with Cargo...");

    let status = Command::new("cargo")
        .arg("build")
        .arg("--release")
        .arg("-q")
        .current_dir(&cache_dir)
        .status()
        .map_err(|e| format!("Failed to execute cargo build: {}", e))?;

    if !status.success() {
        return Err("Cargo compilation failed".to_string());
    }

    let input_path = Path::new(input_file);
    let default_output = input_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("app");

    let out_name = output_binary.unwrap_or(default_output);
    let mut exe_name = out_name.to_string();
    let mut target_exe = "orch_generated".to_string();
    if cfg!(target_os = "windows") {
        exe_name.push_str(".exe");
        target_exe.push_str(".exe");
    }

    let src_exe_path = cache_dir.join("target/release").join(target_exe);
    let dest_exe_path = PathBuf::from(exe_name.clone());

    fs::copy(&src_exe_path, &dest_exe_path)
        .map_err(|e| format!("Failed to copy compiled binary: {}", e))?;

    println!("[Orchestrate] Successfully built binary: {}", exe_name);
    Ok(())
}

fn run_run(input_file: &str) -> Result<(), String> {
    println!("[Orchestrate] Parsing and compiling '{}'...", input_file);
    let cache_dir = prepare_cache_dir(Path::new(input_file))?;
    let rust_code = compile_main_file_and_modules(input_file, &cache_dir)?;

    fs::write(cache_dir.join("src/main.rs"), rust_code)
        .map_err(|e| format!("Failed to write generated Rust file: {}", e))?;

    println!("[Orchestrate] Building Rust binary (this may take a few seconds on first run)...");

    let status = Command::new("cargo")
        .arg("build")
        .arg("-q")
        .current_dir(&cache_dir)
        .status()
        .map_err(|e| format!("Failed to compile generated Rust file via cargo build: {}", e))?;

    if !status.success() {
        return Err("Cargo compilation failed".to_string());
    }

    println!("[Orchestrate] Running program...");

    let mut target_exe = "orch_generated".to_string();
    if cfg!(target_os = "windows") {
        target_exe.push_str(".exe");
    }
    let exe_path = cache_dir.join("target/debug").join(target_exe);

    let mut child = Command::new(exe_path)
        .spawn()
        .map_err(|e| format!("Failed to execute generated binary: {}", e))?;

    let status = child.wait().map_err(|e| format!("Failed to wait for program to finish: {}", e))?;

    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }

    Ok(())
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
            if let Err(e) = run_run(&args[2]) {
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
            if let Err(e) = run_build(input, out) {
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
