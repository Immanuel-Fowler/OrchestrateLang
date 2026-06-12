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

enum ForeignSource {
    C(PathBuf),
    Cpp(PathBuf),
}

fn orch_type_to_rust_ffi(orch_type: &str) -> Result<&str, String> {
    match orch_type {
        "int" => Ok("i64"),
        "float" => Ok("f64"),
        "bool" => Ok("bool"),
        "void" => Ok("()"),
        _ => Err(format!("Unsupported FFI type: {}", orch_type)),
    }
}

fn parse_ffi_and_generate_bindings(ffi_content: &str, language: &str) -> Result<String, String> {
    let mut extern_c = String::from("extern \"C\" {\n");
    let mut wrappers = String::new();
    
    for (line_num, line) in ffi_content.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with("//") {
            continue;
        }
        
        let open_paren = line.find('(').ok_or_else(|| format!("FFI parse error on line {}: missing '('", line_num + 1))?;
        let close_paren = line.rfind(')').ok_or_else(|| format!("FFI parse error on line {}: missing ')'", line_num + 1))?;
        
        let name = line[..open_paren].trim();
        let args_str = line[open_paren+1..close_paren].trim();
        let ret_str = if let Some(arrow_idx) = line.find("->") {
            line[arrow_idx+2..].trim()
        } else {
            "void"
        };
        
        if ret_str == "string" {
            return Err(format!("load_foreign '{}': string type is not supported in C/C++ FFI signatures", language));
        }
        
        let rust_ret = orch_type_to_rust_ffi(ret_str)?;
        let mut rust_args = Vec::new();
        let mut call_args = Vec::new();
        
        if !args_str.is_empty() {
            for arg in args_str.split(',') {
                let parts: Vec<&str> = arg.split(':').collect();
                if parts.len() != 2 {
                    return Err(format!("FFI parse error on line {}: invalid argument '{}'", line_num + 1, arg));
                }
                let arg_name = parts[0].trim();
                let arg_type = parts[1].trim();
                if arg_type == "string" {
                    return Err(format!("load_foreign '{}': string type is not supported in C/C++ FFI signatures", language));
                }
                let rust_type = orch_type_to_rust_ffi(arg_type)?;
                rust_args.push(format!("{}: {}", arg_name, rust_type));
                call_args.push(arg_name.to_string());
            }
        }
        
        let args_decl = rust_args.join(", ");
        let call_decl = call_args.join(", ");
        let ret_decl = if rust_ret == "()" { String::new() } else { format!(" -> {}", rust_ret) };
        
        extern_c.push_str(&format!("    #[link_name = \"{}\"]\n    fn __ffi_{}({}){};\n", name, name, args_decl, ret_decl));
        
        wrappers.push_str(&format!("pub fn {}({}){} {{\n    unsafe {{ __ffi_{}({}) }}\n}}\n", name, args_decl, ret_decl, name, call_decl));
    }
    
    extern_c.push_str("}\n\n");
    
    Ok(format!("{}{}", extern_c, wrappers))
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

    let mut all_foreign_sources = Vec::new();

    for (local_name, module_stmts, module_path) in modules_data {
        let mut generator = codegen::Codegen::new(all_tasks.clone());
        let mut module_rust_code = generator.generate(&module_stmts, false);
        
        let mut foreign_code = String::new();
        for stmt in &module_stmts {
            if let ast::Stmt::LoadForeign { language, path } = stmt {
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
                    
                    let ffi_bindings = parse_ffi_and_generate_bindings(&ffi_content, language)?;
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
    }
    
    fs::write(cache_dir.join("Cargo.toml"), cargo_toml_content)
        .map_err(|e| format!("Failed to write Cargo.toml: {}", e))?;

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
