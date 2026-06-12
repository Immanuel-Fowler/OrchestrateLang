use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

mod ast;
mod lexer;
mod parser;
mod codegen;
mod typechecker;

fn print_usage() {
    println!("Orchestrate Language Compiler");
    println!("Usage:");
    println!("  orchestrate run <file.orch>            Compile and run the program immediately");
    println!("  orchestrate build <file.orch>          Compile the program to a standalone binary");
    println!("  orchestrate build <file.orch> -o <out> Specify the output binary name");
}

fn prepare_cache_dir() -> Result<PathBuf, String> {
    let cache_dir = PathBuf::from(".orch_cache");
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
            let module_path = parent_dir.join(module_name);
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
            
            modules_data.push((local_name.clone(), module_stmts));
            modules_registered.push(local_name.clone());
        }
    }

    for (local_name, module_stmts) in modules_data {
        let mut generator = codegen::Codegen::new(all_tasks.clone());
        let module_rust_code = generator.generate(&module_stmts, false);
        
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
    let cache_dir = prepare_cache_dir()?;
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
    let cache_dir = prepare_cache_dir()?;
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
    if args.len() < 3 {
        print_usage();
        std::process::exit(1);
    }

    let command = &args[1];
    let file = &args[2];

    match command.as_str() {
        "run" => {
            if let Err(e) = run_run(file) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        "build" => {
            let mut out = None;
            if args.len() >= 5 && args[3] == "-o" {
                out = Some(args[4].as_str());
            }
            if let Err(e) = run_build(file, out) {
                eprintln!("Error: {}", e);
                std::process::exit(1);
            }
        }
        _ => {
            print_usage();
            std::process::exit(1);
        }
    }
}
