use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::ast;
use crate::lexer;
use crate::parser;
use crate::codegen;
use crate::typechecker;
use crate::prom;
use crate::ffi_parser;
use crate::ffi_rust::register_rust_ffi_from_sidecar;
use crate::errors::print_friendly_errors;

pub fn prepare_cache_dir(input_path: &Path) -> Result<PathBuf, String> {
    let parent = input_path.parent().unwrap_or(Path::new("."));
    let cache_dir = parent.join(".orch_cache");
    fs::create_dir_all(cache_dir.join("src")).map_err(|e| format!("Failed to create cache directory: {}", e))?;
    Ok(cache_dir)
}

pub fn compile_module(dir_path: &Path) -> Result<Vec<ast::Stmt>, String> {
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
        if let ast::StmtNode::Load { path } = &stmt.node {
            let nested = resolve_load_recursive(&path, dir_path)?;
            merged_stmts.extend(nested);
        } else {
            merged_stmts.push(stmt);
        }
    }
    
    Ok(merged_stmts)
}

pub fn resolve_load_recursive(path_str: &str, dir_path: &Path) -> Result<Vec<ast::Stmt>, String> {
    let sub_file = dir_path.join(path_str);
    let sub_source = fs::read_to_string(&sub_file)
        .map_err(|e| format!("Failed to read loaded file {:?}: {}", sub_file, e))?;
    let mut sub_lexer = lexer::Lexer::new(&sub_source);
    let sub_tokens = sub_lexer.tokenize()?;
    let mut sub_parser = parser::Parser::new(sub_tokens);
    let sub_stmts = sub_parser.parse()?;
    
    let mut merged = Vec::new();
    for stmt in sub_stmts {
        if let ast::StmtNode::Load { path } = &stmt.node {
            let nested = resolve_load_recursive(&path, dir_path)?;
            merged.extend(nested);
        } else {
            merged.push(stmt);
        }
    }
    Ok(merged)
}

pub enum ForeignSource {
    C(PathBuf),
    Cpp(PathBuf),
}

pub fn compile_main_file_and_modules(input_file: &str, cache_dir: &Path) -> Result<String, String> {
    let input_path = Path::new(input_file);
    let source = fs::read_to_string(input_path)
        .map_err(|e| format!("Failed to read source file '{}': {}", input_file, e))?;

    // Clear any stale secret-serverlet child binaries from a previous compile.
    let _ = fs::remove_dir_all(cache_dir.join("src/bin"));

    let mut lexer = lexer::Lexer::new(&source);
    let tokens = lexer.tokenize()?;

    let mut parser = parser::Parser::new(tokens);
    let ast = parser.parse()?;

    let mut type_checker = typechecker::TypeChecker::new();

    let parent_dir = input_path.parent().unwrap_or(Path::new("."));
    let mut modules_registered = Vec::new();
    let mut all_tasks = std::collections::HashSet::new();
    
    for stmt in &ast {
        if let ast::StmtNode::TaskDecl { name, .. } = &stmt.node {
            all_tasks.insert(name.clone());
        }
        if let ast::StmtNode::ProcessDecl { name, .. } = &stmt.node {
            all_tasks.insert(name.clone());
        }
        if let ast::StmtNode::OrchestratorDecl { name, .. } = &stmt.node {
            all_tasks.insert(name.clone());
        }
    }

    let mut modules_data = Vec::new();
    for stmt in &ast {
        if let ast::StmtNode::UseModule { local_name, module_name } = &stmt.node {
            let module_path = if let Some(resolved) = prom::resolve_module(module_name)? {
                resolved
            } else if module_name.contains('/') || module_name.contains('\\') || module_name.starts_with('.') {
                parent_dir.join(module_name)
            } else {
                return Err(format!("Module '{}' is not a path and is not registered in PROM. Run 'orchestrate prom add <name> <path>' to register it, or use a './'-prefixed path.", module_name));
            };
            
            let module_stmts = compile_module(&module_path)?;
            
            for m_stmt in &module_stmts {
                if let ast::StmtNode::TaskDecl { name, .. } = &m_stmt.node {
                    all_tasks.insert(format!("{}::{}", local_name, name));
                }
                if let ast::StmtNode::ProcessDecl { name, .. } = &m_stmt.node {
                    all_tasks.insert(format!("{}::{}", local_name, name));
                }
                if let ast::StmtNode::OrchestratorDecl { name, .. } = &m_stmt.node {
                    all_tasks.insert(format!("{}::{}", local_name, name));
                }
            }
            
            modules_data.push((local_name.clone(), module_stmts, module_path));
            modules_registered.push(local_name.clone());
        }
    }

    let mut all_foreign_sources = Vec::new();

    for (local_name, module_stmts, module_path) in &modules_data {
        type_checker.register_module_functions(local_name, module_stmts);
        
        for stmt in module_stmts {
            if let ast::StmtNode::LoadForeign { language, path } = &stmt.node {
                if language == "rust" {
                    let foreign_path = module_path.join(path);
                    let mut sidecar_path = foreign_path.clone();
                    sidecar_path.set_extension("orch_ffi");
                    if !sidecar_path.exists() {
                        return Err(format!("load_foreign 'rust': no sidecar file found at {:?} — create this file to declare the function signatures", sidecar_path));
                    }
                    let sidecar_content = fs::read_to_string(&sidecar_path)
                        .map_err(|e| format!("Failed to read Rust FFI sidecar {:?}: {}", sidecar_path, e))?;
                    let sidecar_name = sidecar_path.file_name().and_then(|s| s.to_str()).unwrap_or("unknown.orch_ffi");
                    register_rust_ffi_from_sidecar(&sidecar_content, local_name, sidecar_name, &mut type_checker)
                        .map_err(|e| format!("Rust FFI sidecar error: {}", e))?;
                }
            }
        }
    }

    // Type checking phase (after modules are parsed and registered)
    type_checker.type_check(&ast).map_err(|e| {
        // Prefix each error line with "file:line:" so editors can make them clickable
        let lines: Vec<String> = e.lines()
            .map(|line| {
                // If the line already contains a line number hint like "line N", keep it
                if line.trim_start().starts_with("line ") || line.contains(':') {
                    format!("{}:{}", input_file, line)
                } else {
                    format!("{}:1: {}", input_file, line)
                }
            })
            .collect();
        format!("Type Error: {}", lines.join("\n"))
    })?;

    let mut all_secret_programs: Vec<(String, String)> = Vec::new();

    for (local_name, module_stmts, module_path) in modules_data {
        let mut generator = codegen::Codegen::new(all_tasks.clone());
        let mut module_rust_code = generator.generate(&module_stmts, false);
        all_secret_programs.append(&mut generator.secret_programs);
        
        let mut foreign_code = String::new();
        for stmt in &module_stmts {
            if let ast::StmtNode::LoadForeign { language, path } = &stmt.node {
                let foreign_path = module_path.join(path);
                if language == "rust" {
                    let code = fs::read_to_string(&foreign_path)
                        .map_err(|e| format!("Failed to read foreign file {:?}: {}", foreign_path, e))?;
                    foreign_code.push_str(&code);
                    foreign_code.push_str("\n");
                } else if language == "c" || language == "cpp" {
                    let abs_path = fs::canonicalize(&foreign_path)
                        .unwrap_or_else(|_| foreign_path.clone());
                    
                    let mut abs_path_str = abs_path.to_string_lossy().to_string();
                    if abs_path_str.starts_with(r"\\?\") {
                        abs_path_str = abs_path_str[4..].to_string();
                    }
                        
                    if language == "c" {
                        all_foreign_sources.push(ForeignSource::C(PathBuf::from(abs_path_str)));
                    } else {
                        all_foreign_sources.push(ForeignSource::Cpp(PathBuf::from(abs_path_str)));
                    }
                    
                    let mut ffi_path = foreign_path.clone();
                    ffi_path.set_extension("orch_ffi");
                    
                    if !ffi_path.exists() {
                        return Err(format!("load_foreign '{}': no sidecar file found at {:?} — create this file to declare the function signatures", language, ffi_path));
                    }
                    
                    let ffi_content = fs::read_to_string(&ffi_path)
                        .map_err(|e| format!("Failed to read FFI file {:?}: {}", ffi_path, e))?;
                    
                    let ffi_file_name = ffi_path.file_name().and_then(|s| s.to_str()).unwrap_or("unknown.orch_ffi");
                    let ffi_bindings = ffi_parser::parse_ffi_and_generate_bindings(&ffi_content, language, ffi_file_name)?;
                    foreign_code.push_str(&ffi_bindings);
                    foreign_code.push_str("\n");
                } else {
                    return Err(format!("load_foreign: language '{}' is not supported currently", language));
                }
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
    all_secret_programs.append(&mut generator.secret_programs);

    // Write each secret serverlet's standalone program as its own cargo binary.
    if !all_secret_programs.is_empty() {
        let bin_dir = cache_dir.join("src/bin");
        fs::create_dir_all(&bin_dir)
            .map_err(|e| format!("Failed to create src/bin directory: {}", e))?;
        for (bin_name, program_src) in &all_secret_programs {
            let bin_file = bin_dir.join(format!("{}.rs", bin_name));
            fs::write(&bin_file, program_src)
                .map_err(|e| format!("Failed to write secret serverlet program {:?}: {}", bin_file, e))?;
        }
    }

    let mut cargo_toml_content = r#"[package]
name = "orch_generated"
version = "0.1.0"
edition = "2021"

[dependencies]
tokio = { version = "1.35", features = ["full"] }
"#.to_string();

    if !all_foreign_sources.is_empty() {
        cargo_toml_content.push_str("\n[build-dependencies]\ncc = \"1.0\"\n");
        
        let mut build_rs = String::from("fn main() {\n");
        let mut has_c = false;
        let mut has_cpp = false;
        
        let mut c_files = Vec::new();
        let mut cpp_files = Vec::new();
        
        for source in all_foreign_sources {
            match source {
                ForeignSource::C(p) => { c_files.push(p); has_c = true; },
                ForeignSource::Cpp(p) => { cpp_files.push(p); has_cpp = true; },
            }
        }
        
        if has_c {
            build_rs.push_str("    cc::Build::new()\n");
            for p in c_files {
                let p_str = p.to_string_lossy().replace("\\", "\\\\");
                build_rs.push_str(&format!("        .file(\"{}\")\n", p_str));
            }
            build_rs.push_str("        .compile(\"foreign_c\");\n\n");
        }
        
        if has_cpp {
            build_rs.push_str("    cc::Build::new()\n        .cpp(true)\n");
            for p in cpp_files {
                let p_str = p.to_string_lossy().replace("\\", "\\\\");
                build_rs.push_str(&format!("        .file(\"{}\")\n", p_str));
            }
            build_rs.push_str("        .compile(\"foreign_cpp\");\n\n");
        }
        build_rs.push_str("}\n");
        fs::write(cache_dir.join("build.rs"), build_rs)
            .map_err(|e| format!("Failed to write build.rs: {}", e))?;
    } else {
        let _ = fs::remove_file(cache_dir.join("build.rs"));
    }
    
    fs::write(cache_dir.join("Cargo.toml"), cargo_toml_content)
        .map_err(|e| format!("Failed to write Cargo.toml: {}", e))?;

    Ok(main_rust)
}

pub fn run_build(input_file: &str, output_binary: Option<&str>) -> Result<(), String> {
    println!("[Orchestrate] Parsing and compiling '{}'...", input_file);
    let cache_dir = prepare_cache_dir(Path::new(input_file))?;
    let rust_code = compile_main_file_and_modules(input_file, &cache_dir)?;
    
    fs::write(cache_dir.join("src/main.rs"), rust_code)
        .map_err(|e| format!("Failed to write generated Rust file: {}", e))?;

    println!("[Orchestrate] Building release binary with Cargo...");

    let output = Command::new("cargo")
        .arg("build")
        .arg("--release")
        .arg("-q")
        .current_dir(&cache_dir)
        .output()
        .map_err(|e| format!("Failed to execute cargo build: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        std::env::set_var("ORCH_SOURCE_FILE", input_file);
        print_friendly_errors(&stderr, &cache_dir);
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

    // Copy any secret serverlet child binaries next to the output binary, since
    // the orchestrator locates them relative to its own executable at runtime.
    let release_dir = cache_dir.join("target/release");
    let dest_dir = dest_exe_path.parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    if let Ok(entries) = fs::read_dir(&release_dir) {
        for entry in entries.flatten() {
            let fname = entry.file_name();
            let fname_str = fname.to_string_lossy().to_string();
            if fname_str.starts_with("secret_") && !fname_str.ends_with(".d") && entry.path().is_file() {
                let _ = fs::copy(entry.path(), dest_dir.join(&fname_str));
            }
        }
    }

    println!("[Orchestrate] Successfully built binary: {}", exe_name);
    Ok(())
}

pub fn run_run(input_file: &str) -> Result<(), String> {
    println!("[Orchestrate] Parsing and compiling '{}'...", input_file);
    let cache_dir = prepare_cache_dir(Path::new(input_file))?;
    let rust_code = compile_main_file_and_modules(input_file, &cache_dir)?;

    fs::write(cache_dir.join("src/main.rs"), rust_code)
        .map_err(|e| format!("Failed to write generated Rust file: {}", e))?;

    println!("[Orchestrate] Building Rust binary (this may take a few seconds on first run)...");

    let output = Command::new("cargo")
        .arg("build")
        .arg("-q")
        .current_dir(&cache_dir)
        .output()
        .map_err(|e| format!("Failed to compile generated Rust file via cargo build: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        std::env::set_var("ORCH_SOURCE_FILE", input_file);
        print_friendly_errors(&stderr, &cache_dir);
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

/// Type-check only (no codegen, no Cargo invocation). Designed to be fast (<100ms).
pub fn run_check(input_file: &str) -> Result<(), String> {
    let input_path = Path::new(input_file);
    let source = fs::read_to_string(input_path)
        .map_err(|e| format!("Failed to read source file '{}': {}", input_file, e))?;

    let mut lexer = lexer::Lexer::new(&source);
    let tokens = lexer.tokenize()?;

    let mut parser = parser::Parser::new(tokens);
    let ast = parser.parse()?;

    let mut type_checker = typechecker::TypeChecker::new();

    let parent_dir = input_path.parent().unwrap_or(Path::new("."));

    // Register modules for type-checking (no codegen)
    for stmt in &ast {
        if let ast::StmtNode::UseModule { local_name, module_name } = &stmt.node {
            let module_path = if let Some(resolved) = prom::resolve_module(module_name)? {
                resolved
            } else if module_name.contains('/') || module_name.contains('\\') || module_name.starts_with('.') {
                parent_dir.join(module_name)
            } else {
                return Err(format!("Module '{}' not found in PROM registry", module_name));
            };
            let module_stmts = compile_module(&module_path)?;
            type_checker.register_module_functions(local_name, &module_stmts);
        }
    }

    type_checker.type_check(&ast).map_err(|e| {
        let lines: Vec<String> = e.lines()
            .map(|line| {
                if line.trim_start().starts_with("line ") || line.contains(':') {
                    format!("{}:{}", input_file, line)
                } else {
                    format!("{}:1: {}", input_file, line)
                }
            })
            .collect();
        format!("Type Error: {}", lines.join("\n"))
    })?;

    println!("[Orchestrate] {} — no type errors found", input_file);
    Ok(())
}
