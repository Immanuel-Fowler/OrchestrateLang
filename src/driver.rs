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
    let mut raw_stmts = parser.parse()?;
    resolve_landline_sources(&mut raw_stmts, dir_path)?;
    
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
    let mut sub_stmts = sub_parser.parse()?;
    resolve_landline_sources(&mut sub_stmts, sub_file.parent().unwrap_or(dir_path))?;
    
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
    Zig(PathBuf),
    Swift(PathBuf),
    CSharp(PathBuf),
}

/// The external compiler `load_foreign` needs for a language, if any beyond the C/C++ toolchain.
fn foreign_toolchain(language: &str) -> Option<(&'static str, &'static str, &'static str)> {
    match language {
        "zig" => Some(("zig", "version", "install Zig 0.16 or newer (https://ziglang.org/download/)")),
        "swift" => Some(("swiftc", "--version", "install Swift 5.10 or newer (https://www.swift.org/install/)")),
        "csharp" => Some(("dotnet", "--version", "install the .NET SDK 8 or newer (https://dotnet.microsoft.com/download)")),
        _ => None,
    }
}

/// The program a language's toolchain is invoked as. `ORCH_DOTNET` moves the .NET SDK,
/// which is commonly installed outside PATH.
fn foreign_program(language: &str, default: &str) -> String {
    match language {
        "csharp" => std::env::var("ORCH_DOTNET").unwrap_or_else(|_| default.to_string()),
        _ => default.to_string(),
    }
}

/// A static-library name that is unique per foreign source and valid as a Swift module name.
fn foreign_lib_name(language: &str, index: usize, path: &Path) -> String {
    let stem: String = path.file_stem().and_then(|s| s.to_str()).unwrap_or("lib")
        .chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
    format!("orch_{}_{}_{}", language, index, stem)
}

/// Whether any serverlet here is sandboxed, which puts the wasmtime host in the
/// generated crate and makes the build compile a guest for each one.
fn has_sandbox_serverlets(stmts: &[ast::Stmt]) -> bool {
    stmts.iter().any(|stmt| matches!(&stmt.node, ast::StmtNode::Serverlet { sandbox: Some(_), .. }))
}

/// Read a `load_foreign "wasm"` module and its sidecar, and check the one against the
/// other. Shared by `check` and the build, so a name or a signature that does not match
/// the module's export table is reported before anything is compiled.
fn wasm_sidecar(
    module_path: &Path,
    path: &str,
) -> Result<(Vec<ffi_parser::CSignature>, Vec<u8>, String), String> {
    let wasm_path = module_path.join(path);
    let ffi_path = wasm_path.with_extension("orch_ffi");
    if !ffi_path.exists() {
        return Err(format!(
            "load_foreign 'wasm': no sidecar file found at {:?} — create this file to declare the function signatures",
            ffi_path
        ));
    }
    let bytes = fs::read(&wasm_path)
        .map_err(|e| format!("load_foreign 'wasm': failed to read {:?}: {}", wasm_path, e))?;
    let label = wasm_path.file_name().and_then(|s| s.to_str()).unwrap_or("module.wasm").to_string();
    let module = crate::wasm_module::Module::parse(&bytes)
        .map_err(|e| format!("load_foreign 'wasm': {:?}: {}", wasm_path, e))?;
    let content = fs::read_to_string(&ffi_path)
        .map_err(|e| format!("Failed to read FFI file {:?}: {}", ffi_path, e))?;
    let file_name = ffi_path.file_name().and_then(|s| s.to_str()).unwrap_or("unknown.orch_ffi");
    let signatures = ffi_parser::parse_ffi(&content, "wasm", file_name)?;
    crate::wasm_ffi::check(&signatures, &module, file_name, &label)?;
    Ok((signatures, bytes, label))
}

/// Write a sandboxed serverlet's WASM guest crate under `.orch_cache/sandbox_<name>/`,
/// compile it to `wasm32-wasip1`, and put the `.wasm` beside the generated source, where
/// the host embeds it with `include_bytes!`.
fn build_sandbox_guest(cache_dir: &Path, name: &str, lib_src: &str) -> Result<(), String> {
    let crate_name = format!("sandbox_{}", name);
    let crate_dir = cache_dir.join(&crate_name);
    fs::create_dir_all(crate_dir.join("src"))
        .map_err(|e| format!("Failed to create sandbox guest crate dir: {}", e))?;

    let cargo_toml = format!(
        "[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[lib]\ncrate-type = [\"cdylib\"]\n\n[workspace]\n\n[dependencies]\n",
        crate_name
    );
    fs::write(crate_dir.join("Cargo.toml"), cargo_toml)
        .map_err(|e| format!("Failed to write sandbox guest Cargo.toml: {}", e))?;
    fs::write(crate_dir.join("src/lib.rs"), lib_src)
        .map_err(|e| format!("Failed to write sandbox guest lib.rs: {}", e))?;

    println!("[orchestrate] Compiling sandbox guest '{}' to wasm32-wasip1...", name);
    let output = Command::new("cargo")
        .args(["build", "--release", "--target", "wasm32-wasip1", "-q"])
        .current_dir(&crate_dir)
        .output()
        .map_err(|e| format!("Failed to run cargo for sandbox guest '{}': {}", name, e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "Failed to compile sandbox guest '{}' to WASM.\nIs the wasm32-wasip1 target installed? (rustup target add wasm32-wasip1)\n{}",
            name, stderr
        ));
    }

    let wasm_path = crate_dir
        .join("target/wasm32-wasip1/release")
        .join(format!("{}.wasm", crate_name));
    if !wasm_path.exists() {
        return Err(format!("Sandbox guest '{}' compiled but no .wasm artifact was found at {:?}", name, wasm_path));
    }
    fs::copy(&wasm_path, cache_dir.join("src").join(format!("sandbox_{}.wasm", name)))
        .map_err(|e| format!("Failed to stage sandbox guest '{}': {}", name, e))?;
    Ok(())
}

/// The `build.rs` helper that turns one C# file into a native shared library.
///
/// Shared, not static: two NativeAOT **static** archives cannot be linked into one module
/// because each embeds its own runtime, which would cap a program at one C# module and
/// fail as duplicate symbols from the linker rather than as a diagnostic the compiler
/// could write. A shared library per module has no such limit.
const CSHARP_BUILD_HELPER: &str = r#"
/// Publishes a C# file as a NativeAOT shared library and links it.
fn orch_build_csharp(lib: &str, source: &str, out_dir: &str) {
    let target = std::env::var("TARGET").unwrap();
    let host = std::env::var("HOST").unwrap();
    if target != host {
        panic!("load_foreign 'csharp' cannot cross-compile yet (host {}, target {})", host, target);
    }
    // The runtime identifier NativeAOT publishes for, from the target triple.
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let rid = format!(
        "{}-{}",
        match os.as_str() { "macos" => "osx", "windows" => "win", other => other },
        match arch.as_str() { "aarch64" => "arm64", "x86_64" => "x64", other => other },
    );
    let project_dir = format!("{}/{}", out_dir, lib);
    std::fs::create_dir_all(&project_dir).expect("load_foreign 'csharp': cannot create the project directory");

    // A throwaway project around the user's file. EnableDefaultCompileItems is off so
    // only that file is compiled, whatever else sits beside it.
    let project = format!(
        concat!(
            "<Project Sdk=\"Microsoft.NET.Sdk\">\n",
            "  <PropertyGroup>\n",
            "    <TargetFramework>{framework}</TargetFramework>\n",
            "    <AssemblyName>{lib}</AssemblyName>\n",
            "    <RootNamespace>{lib}</RootNamespace>\n",
            "    <Nullable>disable</Nullable>\n",
            "    <AllowUnsafeBlocks>true</AllowUnsafeBlocks>\n",
            "    <PublishAot>true</PublishAot>\n",
            "    <NativeLib>Shared</NativeLib>\n",
            "    <InvariantGlobalization>true</InvariantGlobalization>\n",
            "    <EnableDefaultCompileItems>false</EnableDefaultCompileItems>\n",
            "  </PropertyGroup>\n",
            "  <ItemGroup>\n    <Compile Include=\"{source}\" />\n  </ItemGroup>\n",
            "</Project>\n",
        ),
        framework = orch_csharp_framework(),
        lib = lib,
        source = source,
    );
    let project_file = format!("{}/{}.csproj", project_dir, lib);
    let previous = std::fs::read_to_string(&project_file).unwrap_or_default();
    if previous != project {
        std::fs::write(&project_file, &project).expect("load_foreign 'csharp': cannot write the project file");
    }

    let published = format!("{}/publish", project_dir);
    let output = std::process::Command::new(orch_dotnet())
        .args(["publish", &project_file, "-c", "Release", "-r", &rid, "-o", &published, "--nologo", "-v", "quiet"])
        .output()
        .unwrap_or_else(|e| panic!("load_foreign 'csharp': failed to run `dotnet`: {}", e));
    if !output.status.success() {
        panic!(
            "load_foreign 'csharp': `dotnet publish` failed for {}\n{}{}",
            source,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
    }

    let extension = match os.as_str() { "macos" => "dylib", "windows" => "dll", _ => "so" };
    let library = format!("{}/{}.{}", published, lib, extension);
    if !std::path::Path::new(&library).exists() {
        panic!("load_foreign 'csharp': published {} but no {} was produced", source, library);
    }

    // NativeAOT names the library after the assembly, with no `lib` prefix, and a Unix
    // linker given `-l<name>` looks for `lib<name>`. Copy it under the name the linker
    // expects rather than renaming the assembly, which would rename the symbols' owner.
    let linkable = if os == "windows" {
        published.clone()
    } else {
        let staged = format!("{}/lib{}.{}", out_dir, lib, extension);
        std::fs::copy(&library, &staged)
            .unwrap_or_else(|e| panic!("load_foreign 'csharp': cannot stage {}: {}", library, e));
        // The recorded install name is the path it was built at; point it at the rpath
        // instead so the copy is what gets loaded.
        if os == "macos" {
            let _ = std::process::Command::new("install_name_tool")
                .args(["-id", &format!("@rpath/lib{}.{}", lib, extension), &staged])
                .status();
        }
        out_dir.to_string()
    };

    println!("cargo:rerun-if-changed={}", source);
    println!("cargo:rustc-link-search=native={}", linkable);
    println!("cargo:rustc-link-lib=dylib={}", lib);
    // The library is loaded at run time, so the program has to be able to find it.
    if os != "windows" {
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", linkable);
    }
}

fn orch_dotnet() -> String {
    std::env::var("ORCH_DOTNET").unwrap_or_else(|_| "dotnet".to_string())
}

/// The newest `net<major>.0` the installed SDK can target.
fn orch_csharp_framework() -> String {
    let output = std::process::Command::new(orch_dotnet()).arg("--list-sdks").output();
    let major = output
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| {
            text.lines()
                .filter_map(|line| line.split('.').next()?.trim().parse::<u32>().ok())
                .max()
        })
        .unwrap_or(8);
    format!("net{}.0", major.max(8))
}
"#;

const DEFAULT_LIBRARY_RUST_VERSION: &str = "1.89";

/// The `rust-version` a generated library crate declares: `x.y` or `x.y.z`, never below
/// 1.85, the first release that understands edition 2024.
fn library_rust_version(requested: Option<&str>) -> Result<String, String> {
    let version = requested.unwrap_or(DEFAULT_LIBRARY_RUST_VERSION);
    let parts = version.split('.').collect::<Vec<_>>();
    let invalid = || format!("--rust-version expects x.y or x.y.z, got '{}'", version);
    if !(2..=3).contains(&parts.len()) || parts.iter().any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit())) {
        return Err(invalid());
    }
    let major: u32 = parts[0].parse().map_err(|_| invalid())?;
    let minor: u32 = parts[1].parse().map_err(|_| invalid())?;
    if (major, minor) < (1, 85) {
        return Err(format!("--rust-version {} is below 1.85, the first release with edition 2024", version));
    }
    Ok(version.to_string())
}

pub fn compile_main_file_and_modules(input_file: &str, cache_dir: &Path) -> Result<String, String> {
    compile_with_mode(input_file, cache_dir, None, &[])
}

/// `library` carries the `rust-version` the generated crate declares; `None` builds a binary.
fn compile_with_mode(input_file: &str, cache_dir: &Path, library: Option<&str>, cli_dependencies: &[crate::dependencies::Dependency]) -> Result<String, String> {
    let input_path = Path::new(input_file);
    let source = fs::read_to_string(input_path)
        .map_err(|e| format!("Failed to read source file '{}': {}", input_file, e))?;

    // Clear any stale secret-serverlet child binaries from a previous compile.
    let _ = fs::remove_dir_all(cache_dir.join("src/bin"));

    let mut lexer = lexer::Lexer::new(&source);
    let tokens = lexer.tokenize()?;

    let mut parser = parser::Parser::new(tokens);
    let mut ast = parser.parse()?;
    resolve_landline_sources(&mut ast, input_path.parent().unwrap_or(Path::new(".")))?;

    let mut uses_sandbox = has_sandbox_serverlets(&ast);

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
            uses_sandbox |= has_sandbox_serverlets(&module_stmts);

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

    let host_functions = ast.iter().flat_map(|s| match &s.node {
        ast::StmtNode::Host { name, functions } => functions.iter().map(|h| (name.clone(), h.clone())).collect(),
        _ => Vec::new(),
    }).collect::<Vec<_>>();
    let module_stmts: Vec<&[ast::Stmt]> = modules_data.iter().map(|(_, stmts, _)| stmts.as_slice()).collect();
    library_rules(&ast, &module_stmts, library.is_some())?;
    let mut all_foreign_sources = Vec::new();
    // Crates the generated crate must depend on, by name, with where each was declared.
    let mut extra_dependencies: std::collections::BTreeMap<String, (crate::dependencies::Dependency, String)> = std::collections::BTreeMap::new();
    crate::dependencies::merge(&mut extra_dependencies, cli_dependencies.to_vec(), "the command line")?;
    // C-ABI functions with a handle parameter, as `module::name`, and whether any sidecar
    // uses handles at all; codegen passes handles by reference and defines the type once.
    let mut handle_params: std::collections::HashMap<String, Vec<bool>> = std::collections::HashMap::new();
    let mut uses_handles = false;
    // Any wasm module or sandboxed serverlet puts the wasmtime host in the generated crate.
    let mut uses_wasm = uses_sandbox;

    for (local_name, module_stmts, module_path) in &modules_data {
        type_checker.register_module_functions(local_name, module_stmts);

        for stmt in module_stmts {
            if let ast::StmtNode::LoadForeign { language, path, .. } = &stmt.node {
                if language == "rust" || language == "typescript" {
                    let foreign_path = module_path.join(path);
                    let mut sidecar_path = foreign_path.clone();
                    sidecar_path.set_extension("orch_ffi");
                    if !sidecar_path.exists() {
                        return Err(format!("load_foreign 'rust': no sidecar file found at {:?} — create this file to declare the function signatures", sidecar_path));
                    }
                    let sidecar_content = fs::read_to_string(&sidecar_path)
                        .map_err(|e| format!("Failed to read Rust FFI sidecar {:?}: {}", sidecar_path, e))?;
                    let sidecar_name = sidecar_path.file_name().and_then(|s| s.to_str()).unwrap_or("unknown.orch_ffi");
                    let (signatures, dependencies) = crate::dependencies::split_sidecar(&sidecar_content);
                    register_rust_ffi_from_sidecar(&signatures, local_name, sidecar_name, &mut type_checker)
                        .map_err(|e| format!("Rust FFI sidecar error: {}", e))?;
                    if let Some(section) = dependencies {
                        if language != "rust" {
                            return Err(format!("{}: [dependencies] applies to load_foreign \"rust\" sidecars", sidecar_path.display()));
                        }
                        let origin = sidecar_path.display().to_string();
                        let base = sidecar_path.parent().unwrap_or(module_path);
                        let declared = crate::dependencies::parse_section(&section, base, &origin)?;
                        crate::dependencies::merge(&mut extra_dependencies, declared, &origin)?;
                    }
                } else if language == "wasm" {
                    // A missing or mismatched sidecar is reported by codegen below, which
                    // reads the module itself; here the signatures are registered so calls
                    // to the module's exports are typed.
                    let sidecar_path = module_path.join(path).with_extension("orch_ffi");
                    if let Ok(content) = fs::read_to_string(&sidecar_path) {
                        let file_name = sidecar_path.file_name().and_then(|s| s.to_str()).unwrap_or("unknown.orch_ffi").to_string();
                        for signature in ffi_parser::parse_ffi(&content, language, &file_name)? {
                            if signature.drop { continue; }
                            let params = signature.params.iter().map(|(_, ty)| ty.clone()).collect();
                            type_checker.register_foreign_function(local_name, &signature.name, params, signature.ret.clone());
                        }
                    }
                } else if matches!(language.as_str(), "c" | "cpp" | "zig" | "swift" | "csharp") {
                    // A missing sidecar is reported by codegen below; here the signatures
                    // are registered so calls are typed and handles are known.
                    let sidecar_path = module_path.join(path).with_extension("orch_ffi");
                    if let Ok(content) = fs::read_to_string(&sidecar_path) {
                        let file_name = sidecar_path.file_name().and_then(|s| s.to_str()).unwrap_or("unknown.orch_ffi").to_string();
                        for signature in ffi_parser::parse_ffi(&content, language, &file_name)? {
                            if signature.drop { continue; }
                            if signature.mentions_handle() { uses_handles = true; }
                            if signature.handle_params().iter().any(|is_handle| *is_handle) {
                                handle_params.insert(format!("{}::{}", local_name, signature.name), signature.handle_params());
                            }
                            let params = signature.params.iter().map(|(_, ty)| ty.clone()).collect();
                            type_checker.register_foreign_function(local_name, &signature.name, params, signature.ret.clone());
                        }
                    }
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

    let bundle_dir = cache_dir.join("landlines");
    if bundle_dir.exists() { fs::remove_dir_all(&bundle_dir).map_err(|e| e.to_string())?; }
    stage_landlines(&ast, &bundle_dir)?;
    for (_, stmts, _) in &modules_data {
        stage_landlines(stmts, &bundle_dir)?;
    }
    let mut all_secret_programs: Vec<(String, String)> = Vec::new();
    let mut all_sandbox_programs: Vec<(String, String)> = Vec::new();

    let module_event_stmts = modules_data.iter().flat_map(|(_, stmts, _)| stmts.clone()).collect::<Vec<_>>();
    for (local_name, module_stmts, module_path) in modules_data {
        let mut generator = codegen::Codegen::new(all_tasks.clone());
        generator.library = library.is_some();
        generator.serverlet_state_types = type_checker.serverlet_state.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        generator.host_functions = host_functions.clone();
        // The module's own code calls its foreign functions by bare name.
        let prefix = format!("{}::", local_name);
        generator.foreign_handle_params = handle_params.iter()
            .filter_map(|(key, flags)| key.strip_prefix(&prefix).map(|name| (name.to_string(), flags.clone())))
            .collect();
        let mut module_rust_code = generator.generate(&module_stmts, false);
        if let Some(error) = generator.errors.first() {
            return Err(format!("{}: {}", module_path.display(), error));
        }
        all_secret_programs.append(&mut generator.secret_programs);
        all_sandbox_programs.append(&mut generator.sandbox_programs);
        
        let mut foreign_code = String::new();
        for stmt in &module_stmts {
            if let ast::StmtNode::LoadForeign { language, path, backend } = &stmt.node {
                let foreign_path = module_path.join(path);
                if language == "rust" {
                    let code = fs::read_to_string(&foreign_path)
                        .map_err(|e| format!("Failed to read foreign file {:?}: {}", foreign_path, e))?;
                    foreign_code.push_str(&code);
                    foreign_code.push_str("\n");
                } else if language == "typescript" {
                    let sidecar = fs::read_to_string(foreign_path.with_extension("orch_ffi")).map_err(|e| format!("TypeScript FFI sidecar: {e}"))?;
                    let handlers = crate::typescript::sidecar(&sidecar)?;
                    let asset = format!("ffi_{}_{}", local_name, foreign_code.len());
                    crate::typescript::build(&foreign_path, &bundle_dir.join(&asset), &handlers, &module_stmts, true, backend.as_deref())?;
                    foreign_code.push_str(&crate::typescript::ffi_bindings(&asset, &handlers, library.is_some())?);
                } else if language == "wasm" {
                    // The module is already compiled, so there is no toolchain to run:
                    // check the sidecar against its export table, then embed it.
                    let (signatures, wasm_bytes, label) = wasm_sidecar(&module_path, path)?;
                    let asset = crate::wasm_ffi::asset_name(&local_name, &foreign_path);
                    fs::write(cache_dir.join("src").join(format!("{}.wasm", asset)), &wasm_bytes)
                        .map_err(|e| format!("Failed to stage wasm module: {}", e))?;
                    foreign_code.push_str(&crate::wasm_ffi::bindings(&signatures, &asset, &label));
                    foreign_code.push('\n');
                    uses_wasm = true;
                } else if matches!(language.as_str(), "c" | "cpp" | "zig" | "swift" | "csharp") {
                    if let Some((program, version_arg, install_hint)) = foreign_toolchain(language) {
                        // The same override the generated build.rs honours, so a toolchain
                        // installed outside PATH is found by both.
                        let program = foreign_program(language, program);
                        if Command::new(&program).arg(version_arg).output().is_err() {
                            return Err(format!("load_foreign '{}': `{}` was not found on PATH — {}", language, program, install_hint));
                        }
                    }
                    let abs_path = fs::canonicalize(&foreign_path)
                        .unwrap_or_else(|_| foreign_path.clone());

                    let mut abs_path_str = abs_path.to_string_lossy().to_string();
                    if abs_path_str.starts_with(r"\\?\") {
                        abs_path_str = abs_path_str[4..].to_string();
                    }
                    let abs_path = PathBuf::from(abs_path_str);

                    all_foreign_sources.push(match language.as_str() {
                        "c" => ForeignSource::C(abs_path),
                        "cpp" => ForeignSource::Cpp(abs_path),
                        "zig" => ForeignSource::Zig(abs_path),
                        "csharp" => ForeignSource::CSharp(abs_path),
                        _ => ForeignSource::Swift(abs_path),
                    });
                    
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

    // Landlines and secret serverlets start children through tokio::process, which needs
    // the host runtime's IO driver. The whole graph decides, since a host reading the entry
    // file cannot see a declaration inside an imported module.
    let mut io_driver_users = ast.iter().chain(module_event_stmts.iter()).filter_map(|stmt| match &stmt.node {
        ast::StmtNode::Serverlet { name, landline: Some(config), .. } => Some(format!("{} landline serverlet '{}'", config.runtime, name)),
        ast::StmtNode::Serverlet { name, secret: true, .. } => Some(format!("secret serverlet '{}'", name)),
        _ => None,
    }).collect::<Vec<_>>();
    io_driver_users.sort();
    if library.is_some() && !io_driver_users.is_empty() {
        println!("[orchestrate] host runtime needs Tokio's IO driver: {}", io_driver_users.join(", "));
    }
    let mut generator = codegen::Codegen::new(all_tasks);
    generator.library = library.is_some();
    generator.serverlet_state_types = type_checker.serverlet_state.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    generator.host_functions = host_functions;
    generator.io_driver_users = io_driver_users;
    generator.foreign_handle_params = handle_params;
    generator.emit_handle_type = uses_handles;
    generator.needs_wasm = uses_wasm;
    generator.state_types = ast.iter().filter_map(|stmt| match &stmt.node {
        ast::StmtNode::Let { name, .. } => type_checker.global_var_type(name).map(|ty| (name.clone(), ty)),
        _ => None,
    }).collect();
    if library.is_some() { generator.scan_events(&module_event_stmts); }
    let main_rust = generator.generate(&ast, true);
    if let Some(error) = generator.errors.first() {
        return Err(format!("{}: {}", input_file, error));
    }
    all_secret_programs.append(&mut generator.secret_programs);
    all_sandbox_programs.append(&mut generator.sandbox_programs);

    // Build each sandboxed serverlet's WASM guest crate (step 2: produce the
    // .wasm artifact; host integration via wasmtime is step 3).
    for (sb_name, lib_src) in &all_sandbox_programs {
        build_sandbox_guest(cache_dir, sb_name, lib_src)?;
    }

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

    if uses_wasm {
        crate::dependencies::merge(
            &mut extra_dependencies,
            vec![crate::dependencies::builtin_wasmtime()],
            "a wasm module or sandboxed serverlet",
        )?;
    }
    let mut cargo_toml_content = format!(
        "[package]\nname = \"orch_generated\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[dependencies]\ntokio = {{ version = \"1.35\", features = [\"full\"] }}\n{}",
        crate::dependencies::emit(extra_dependencies.values().map(|(dependency, _)| dependency))
    );

    if !all_foreign_sources.is_empty() {
        let mut build_rs = String::from("fn main() {\n");
        let mut helpers = String::new();
        let mut has_c = false;
        let mut has_cpp = false;

        let mut c_files = Vec::new();
        let mut cpp_files = Vec::new();
        let mut zig_files = Vec::new();
        let mut swift_files = Vec::new();
        let mut csharp_files = Vec::new();

        for (index, source) in all_foreign_sources.into_iter().enumerate() {
            match source {
                ForeignSource::C(p) => { c_files.push(p); has_c = true; },
                ForeignSource::Cpp(p) => { cpp_files.push(p); has_cpp = true; },
                ForeignSource::Zig(p) => zig_files.push((foreign_lib_name("zig", index, &p), p)),
                ForeignSource::Swift(p) => swift_files.push((foreign_lib_name("swift", index, &p), p)),
                ForeignSource::CSharp(p) => csharp_files.push((foreign_lib_name("csharp", index, &p), p)),
            }
        }
        if has_c || has_cpp {
            cargo_toml_content.push_str("\n[build-dependencies]\ncc = \"1.0\"\n");
        }

        if !zig_files.is_empty() || !swift_files.is_empty() || !csharp_files.is_empty() {
            // Zig and Swift compile to one static library each in OUT_DIR, using the
            // language's own compiler for the host target.
            build_rs.push_str("    let out_dir = std::env::var(\"OUT_DIR\").unwrap();\n");
            build_rs.push_str("    println!(\"cargo:rustc-link-search=native={}\", out_dir);\n");
            helpers.push_str(r#"
fn orch_compile_foreign(language: &str, program: &str, args: &[&str]) {
    let target = std::env::var("TARGET").unwrap();
    let host = std::env::var("HOST").unwrap();
    if target != host {
        panic!("load_foreign '{}' cannot cross-compile yet (host {}, target {})", language, host, target);
    }
    let status = std::process::Command::new(program).args(args).status()
        .unwrap_or_else(|e| panic!("load_foreign '{}': failed to run `{}`: {}", language, program, e));
    if !status.success() {
        panic!("load_foreign '{}': `{} {}` failed", language, program, args.join(" "));
    }
}

/// Packs one object file into a static archive with the platform's own archiver.
///
/// Zig compiles to an object rather than using `zig build-lib`: Zig 0.16's archive
/// writer does not 8-byte-align Mach-O members, and Xcode 26's toolchain then drops
/// the member's symbols (the link fails with "Undefined symbols").
fn orch_archive_object(language: &str, archive: &str, object: &str) {
    let _ = std::fs::remove_file(archive);
    let (program, args): (&str, Vec<&str>) = if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        ("xcrun", vec!["libtool", "-static", "-o", archive, object])
    } else {
        ("ar", vec!["crs", archive, object])
    };
    let status = std::process::Command::new(program).args(&args).status()
        .unwrap_or_else(|e| panic!("load_foreign '{}': failed to run `{}`: {}", language, program, e));
    if !status.success() {
        panic!("load_foreign '{}': `{} {}` failed", language, program, args.join(" "));
    }
}
"#);
        }
        for (lib, p) in &zig_files {
            build_rs.push_str(&format!(
                "    orch_compile_foreign(\"zig\", \"zig\", &[\"build-obj\", \"-O\", \"ReleaseFast\", \"-fPIC\", \"--cache-dir\", &format!(\"{{}}/zig-cache\", out_dir), &format!(\"-femit-bin={{}}/{lib}.o\", out_dir), {:?}]);\n    orch_archive_object(\"zig\", &format!(\"{{}}/lib{lib}.a\", out_dir), &format!(\"{{}}/{lib}.o\", out_dir));\n    println!(\"cargo:rustc-link-lib=static={lib}\");\n",
                p.to_string_lossy()
            ));
        }
        for (lib, p) in &swift_files {
            build_rs.push_str(&format!(
                "    orch_compile_foreign(\"swift\", \"swiftc\", &[\"-emit-library\", \"-static\", \"-parse-as-library\", \"-O\", \"-module-name\", \"{lib}\", \"-o\", &format!(\"{{}}/lib{lib}.a\", out_dir), {:?}]);\n    println!(\"cargo:rustc-link-lib=static={lib}\");\n",
                p.to_string_lossy()
            ));
        }
        for (lib, p) in &csharp_files {
            build_rs.push_str(&format!(
                "    orch_build_csharp(\"{lib}\", {:?}, &out_dir);\n",
                p.to_string_lossy()
            ));
        }
        if !csharp_files.is_empty() {
            helpers.push_str(CSHARP_BUILD_HELPER);
        }

        if !swift_files.is_empty() {
            build_rs.push_str("    orch_link_swift_runtime();\n\n");
            helpers.push_str(r#"
/// Links the Swift runtime that Swift static libraries depend on.
fn orch_link_swift_runtime() {
    let macos = std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos");
    let info = std::process::Command::new("swiftc").arg("-print-target-info").output()
        .expect("load_foreign 'swift': failed to run `swiftc -print-target-info`");
    let info = String::from_utf8_lossy(&info.stdout);
    let mut dirs: Vec<String> = Vec::new();
    if let Some(start) = info.find("\"runtimeLibraryPaths\"") {
        let rest = &info[start..];
        if let (Some(open), Some(close)) = (rest.find('['), rest.find(']')) {
            dirs.extend(rest[open + 1..close].split('"').skip(1).step_by(2).map(String::from));
        }
    }
    if macos {
        if let Ok(sdk) = std::process::Command::new("xcrun").arg("--show-sdk-path").output() {
            let sdk = String::from_utf8_lossy(&sdk.stdout).trim().to_string();
            if !sdk.is_empty() { dirs.push(format!("{}/usr/lib/swift", sdk)); }
        }
    }
    for dir in &dirs {
        println!("cargo:rustc-link-search=native={}", dir);
        if !macos { println!("cargo:rustc-link-arg=-Wl,-rpath,{}", dir); }
    }
    println!("cargo:rustc-link-lib=dylib=swiftCore");
}
"#);
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
        build_rs.push_str(&helpers);
        fs::write(cache_dir.join("build.rs"), build_rs)
            .map_err(|e| format!("Failed to write build.rs: {}", e))?;
    } else {
        let _ = fs::remove_file(cache_dir.join("build.rs"));
    }
    
    cargo_toml_content.push_str("\n[workspace]\n");
    if let Some(rust_version) = library {
        cargo_toml_content = cargo_toml_content.replace("edition = \"2021\"", &format!("edition = \"2024\"\nrust-version = \"{}\"", rust_version));
    }
    fs::write(cache_dir.join("Cargo.toml"), cargo_toml_content)
        .map_err(|e| format!("Failed to write Cargo.toml: {}", e))?;

    Ok(main_rust)
}

pub fn run_build(input_file: &str, output_binary: Option<&str>) -> Result<(), String> {
    println!("[orchestrate] Parsing and compiling '{}'...", input_file);
    let cache_dir = prepare_cache_dir(Path::new(input_file))?;
    let rust_code = compile_main_file_and_modules(input_file, &cache_dir)?;
    
    fs::write(cache_dir.join("src/main.rs"), rust_code)
        .map_err(|e| format!("Failed to write generated Rust file: {}", e))?;

    println!("[orchestrate] Building release binary with Cargo...");

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

    copy_landlines(&cache_dir.join("landlines"), &dest_dir)?;
    println!("[orchestrate] Successfully built binary: {}", exe_name);
    Ok(())
}

pub fn run_run(input_file: &str) -> Result<(), String> {
    println!("[orchestrate] Parsing and compiling '{}'...", input_file);
    let cache_dir = prepare_cache_dir(Path::new(input_file))?;
    let rust_code = compile_main_file_and_modules(input_file, &cache_dir)?;

    fs::write(cache_dir.join("src/main.rs"), rust_code)
        .map_err(|e| format!("Failed to write generated Rust file: {}", e))?;

    println!("[orchestrate] Building Rust binary (this may take a few seconds on first run)...");

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

    println!("[orchestrate] Running program...");

    let mut target_exe = "orch_generated".to_string();
    if cfg!(target_os = "windows") {
        target_exe.push_str(".exe");
    }
    let exe_path = cache_dir.join("target/debug").join(target_exe);

    copy_landlines(&cache_dir.join("landlines"), &cache_dir.join("target/debug"))?;
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
/// The rules a build applies before it generates anything that depend only on the
/// declarations and on whether it builds a library. `build` and `check` both call this,
/// so `check` refuses what `build` would for them, and `check --lib` means what
/// `build --lib` does.
fn library_rules(ast: &[ast::Stmt], modules: &[&[ast::Stmt]], library: bool) -> Result<(), String> {
    let host_functions = ast.iter().flat_map(|s| match &s.node {
        ast::StmtNode::Host { name, functions } => functions.iter().map(|h| (name.clone(), h.clone())).collect(),
        _ => Vec::new(),
    }).collect::<Vec<_>>();
    let mut host_names = std::collections::HashSet::new();
    for (group, handler) in &host_functions {
        if group.starts_with('_') || handler.name.starts_with('_') || !host_names.insert(format!("{}_{}", group, handler.name)) {
            return Err("Host names must not start with underscore or generate duplicate Rust method names".into());
        }
    }
    for stmt in ast.iter().chain(modules.iter().flat_map(|stmts| stmts.iter())) {
        if !library && matches!(&stmt.node, ast::StmtNode::Host { .. } | ast::StmtNode::OnTick { .. } | ast::StmtNode::OnFixedTick { .. }) {
            return Err("host and on_tick require build --lib; check such a program with check --lib".into());
        }
        if let ast::StmtNode::Serverlet { grants, .. } = &stmt.node {
            let mut seen = std::collections::HashSet::new();
            for grant in grants {
                if !library || !seen.insert(grant) || !host_functions.iter().any(|(g,h)| format!("{}.{}", g, h.name) == *grant) {
                    return Err(format!("Unknown or duplicate host grant '{}' (host grants require build --lib, and check --lib)", grant));
                }
            }
        }
    }
    if library {
        let ticks = ast.iter().filter(|s| matches!(&s.node, ast::StmtNode::OnTick { .. })).collect::<Vec<_>>();
        if ticks.len() > 1 && ticks.iter().any(|s| matches!(&s.node, ast::StmtNode::OnTick { input: Some(_), .. }) || matches!(&s.node, ast::StmtNode::OnTick { return_type, .. } if *return_type != ast::Type::Void)) {
            return Err("A typed on_tick must be the only on_tick declaration".into());
        }
        for stmts in modules {
            if stmts.iter().any(|s| matches!(&s.node, ast::StmtNode::Host { .. } | ast::StmtNode::OnTick { .. } | ast::StmtNode::OnFixedTick { .. } | ast::StmtNode::OnStart(_) | ast::StmtNode::OnStop(_))) {
                return Err("Library host declarations and lifecycle hooks must be in the entry file".into());
            }
        }
        for stmt in ast {
            if let ast::StmtNode::OrchestratorDecl { name, params, .. } = &stmt.node {
                if name == "main" && !(params.is_empty() || (params.len() == 1 && matches!(&params[0].ty, ast::Type::Array(inner, _) if **inner == ast::Type::Process))) {
                    return Err("Library main accepts no parameters or one process[] parameter".into());
                }
            }
        }
    }
    Ok(())
}

pub fn run_check(input_file: &str, library: bool) -> Result<(), String> {
    let input_path = Path::new(input_file);
    let source = fs::read_to_string(input_path)
        .map_err(|e| format!("Failed to read source file '{}': {}", input_file, e))?;

    let mut lexer = lexer::Lexer::new(&source);
    let tokens = lexer.tokenize()?;

    let mut parser = parser::Parser::new(tokens);
    let mut ast = parser.parse()?;
    resolve_landline_sources(&mut ast, input_path.parent().unwrap_or(Path::new(".")))?;

    let mut type_checker = typechecker::TypeChecker::new();

    let parent_dir = input_path.parent().unwrap_or(Path::new("."));

    // Register modules for type-checking (no codegen)
    let mut modules: Vec<(PathBuf, Vec<ast::Stmt>)> = Vec::new();
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
            register_module_sidecars(&mut type_checker, local_name, &module_path, &module_stmts)?;
            modules.push((module_path, module_stmts));
        }
    }

    type_checker.type_check(&ast).map_err(|e| type_error_in(input_file, &e))?;

    // A module's own declarations, checked in the module's own scope as the entry file's
    // are in its: a serverlet declared in a module used to be looked at first by codegen,
    // during a build. Its foreign functions are callable by their bare names inside it.
    for (module_path, module_stmts) in &modules {
        let mut module_checker = typechecker::TypeChecker::new();
        register_module_sidecars(&mut module_checker, MODULE_SELF, module_path, module_stmts)?;
        module_checker.expose_module_functions(MODULE_SELF);
        let file = module_path.join("module.orch");
        module_checker
            .type_check(module_stmts)
            .map_err(|e| type_error_in(&file.to_string_lossy(), &e))?;
    }

    let module_stmts: Vec<&[ast::Stmt]> = modules.iter().map(|(_, stmts)| stmts.as_slice()).collect();
    library_rules(&ast, &module_stmts, library)?;

    println!("[orchestrate] {} — no type errors found", input_file);
    Ok(())
}

/// The alias a module's own foreign functions are registered under while the module is
/// checked by itself; `expose_module_functions` makes them callable by bare name.
const MODULE_SELF: &str = "__module";

/// A type error with the file it is in, one line per error, as `check` prints it.
fn type_error_in(file: &str, e: &str) -> String {
    let lines: Vec<String> = e.lines()
        .map(|line| {
            if line.trim_start().starts_with("line ") || line.contains(':') {
                format!("{}:{}", file, line)
            } else {
                format!("{}:1: {}", file, line)
            }
        })
        .collect();
    format!("Type Error: {}", lines.join("\n"))
}

/// Registers the functions a module's `load_foreign` sidecars declare, under `local_name`,
/// as the build does; a language the build does not support is the build's error here.
fn register_module_sidecars(type_checker: &mut typechecker::TypeChecker, local_name: &str, module_path: &Path, module_stmts: &[ast::Stmt]) -> Result<(), String> {
    for stmt in module_stmts {
        if let ast::StmtNode::LoadForeign { language, path, .. } = &stmt.node {
            if language == "rust" {
                // As the build does, so a call into a Rust foreign module — the
                // standard library included — is typed here rather than unknown.
                let sidecar_path = module_path.join(path).with_extension("orch_ffi");
                if let Ok(content) = fs::read_to_string(&sidecar_path) {
                    let sidecar_name = sidecar_path.file_name().and_then(|s| s.to_str()).unwrap_or("unknown.orch_ffi");
                    let (signatures, _) = crate::dependencies::split_sidecar(&content);
                    register_rust_ffi_from_sidecar(&signatures, local_name, sidecar_name, type_checker)
                        .map_err(|e| format!("Rust FFI sidecar error: {}", e))?;
                }
            } else if language == "typescript" {
                let content = fs::read_to_string(module_path.join(path).with_extension("orch_ffi")).map_err(|e| e.to_string())?;
                for h in crate::typescript::sidecar(&content)? {
                    type_checker.register_foreign_function(local_name, &h.name, h.params.into_iter().map(|p| p.ty).collect(), h.return_type);
                }
            } else if language == "wasm" {
                // The module itself is the authority on what it exports, so this
                // is the same check the build runs.
                let (signatures, _, _) = wasm_sidecar(&module_path, path)?;
                for signature in signatures {
                    let params = signature.params.iter().map(|(_, ty)| ty.clone()).collect();
                    type_checker.register_foreign_function(local_name, &signature.name, params, signature.ret.clone());
                }
            } else if matches!(language.as_str(), "c" | "cpp" | "zig" | "swift" | "csharp") {
                let sidecar_path = module_path.join(path).with_extension("orch_ffi");
                if let Ok(content) = fs::read_to_string(&sidecar_path) {
                    let file_name = sidecar_path.file_name().and_then(|s| s.to_str()).unwrap_or("unknown.orch_ffi").to_string();
                    for signature in ffi_parser::parse_ffi(&content, language, &file_name)? {
                        if signature.drop { continue; }
                        let params = signature.params.iter().map(|(_, ty)| ty.clone()).collect();
                        type_checker.register_foreign_function(local_name, &signature.name, params, signature.ret.clone());
                    }
                }
            } else {
                return Err(format!("load_foreign: language '{}' is not supported currently", language));
            }
        }
    }
    Ok(())
}


struct ForeignSource2 {
    language: String,
    path: PathBuf,
    /// TypeScript is checked against a generated adapter, so it needs the contract
    /// its handlers declare, the declarations in scope, and whether it is FFI.
    typescript: Option<(Vec<ast::Handler>, Vec<ast::Stmt>, bool)>,
}

/// Collect every foreign source a file declares: `load_foreign` paths, relative to
/// the declaring directory, and landline sources, already absolute.
fn collect_foreign_sources(
    stmts: &[ast::Stmt],
    dir: &Path,
    found: &mut Vec<ForeignSource2>,
) -> Result<(), String> {
    for stmt in stmts {
        match &stmt.node {
            ast::StmtNode::LoadForeign { language, path, .. } => {
                let path = dir.join(path);
                let typescript = if language == "typescript" {
                    let sidecar = fs::read_to_string(path.with_extension("orch_ffi"))
                        .map_err(|e| format!("TypeScript FFI sidecar: {e}"))?;
                    Some((crate::typescript::sidecar(&sidecar)?, stmts.to_vec(), true))
                } else {
                    None
                };
                found.push(ForeignSource2 { language: language.clone(), path, typescript });
            }
            ast::StmtNode::Serverlet { handlers, landline: Some(config), .. } => {
                let typescript = (config.runtime == "typescript")
                    .then(|| (handlers.clone(), stmts.to_vec(), false));
                found.push(ForeignSource2 {
                    language: config.runtime.clone(),
                    path: PathBuf::from(&config.source),
                    typescript,
                });
            }
            _ => {}
        }
    }
    Ok(())
}

/// Generate the program's Rust and run `cargo check` on it. This is how a Rust
/// `load_foreign` file gets checked honestly: alongside the module code it is
/// concatenated with, exactly as a build would compile it, minus the final link.
fn check_generated_rust(input_file: &str) -> Result<(), String> {
    let cache_dir = prepare_cache_dir(Path::new(input_file))?;
    let rust_code = compile_main_file_and_modules(input_file, &cache_dir)?;
    fs::write(cache_dir.join("src/main.rs"), rust_code)
        .map_err(|e| format!("Failed to write generated Rust file: {}", e))?;

    let output = Command::new("cargo")
        .args(["check", "-q"])
        .current_dir(&cache_dir)
        .output()
        .map_err(|e| format!("Failed to run cargo check: {}", e))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    std::env::set_var("ORCH_SOURCE_FILE", input_file);
    print_friendly_errors(&stderr, &cache_dir);
    Err("cargo check reported errors in the generated Rust".to_string())
}

/// Check every foreign source with its own language's checker. Without `deep` this
/// runs no codegen and no Cargo, so it stays usable in an edit loop; `deep` adds
/// mypy for Python and a `cargo check` pass that covers Rust.
pub fn run_check_foreign(input_file: &str, deep: bool) -> Result<(), String> {
    let input_path = Path::new(input_file);
    let source = fs::read_to_string(input_path)
        .map_err(|e| format!("Failed to read source file '{}': {}", input_file, e))?;

    let mut lexer = lexer::Lexer::new(&source);
    let tokens = lexer.tokenize()?;
    let mut parser = parser::Parser::new(tokens);
    let mut ast = parser.parse()?;
    let parent_dir = input_path.parent().unwrap_or(Path::new("."));
    resolve_landline_sources(&mut ast, parent_dir)?;

    let mut sources = Vec::new();
    collect_foreign_sources(&ast, parent_dir, &mut sources)?;

    for stmt in &ast {
        if let ast::StmtNode::UseModule { module_name, .. } = &stmt.node {
            let module_path = if let Some(resolved) = prom::resolve_module(module_name)? {
                resolved
            } else if module_name.contains('/') || module_name.contains('\\') || module_name.starts_with('.') {
                parent_dir.join(module_name)
            } else {
                return Err(format!("Module '{}' not found in PROM registry", module_name));
            };
            let module_stmts = compile_module(&module_path)?;
            collect_foreign_sources(&module_stmts, &module_path, &mut sources)?;
        }
    }

    // Canonicalize so the report reads cleanly and two spellings of one path
    // are checked once. A path that does not resolve is reported by the checker.
    for source in sources.iter_mut() {
        if let Ok(resolved) = source.path.canonicalize() {
            source.path = resolved;
        }
    }
    sources.sort_by(|a, b| (&a.language, &a.path).cmp(&(&b.language, &b.path)));
    sources.dedup_by(|a, b| (&a.language, &a.path) == (&b.language, &b.path));

    if sources.is_empty() {
        println!("[orchestrate] {} — no foreign sources to check", input_file);
        return Ok(());
    }

    let scratch = prepare_cache_dir(input_path)?.join("check");
    fs::create_dir_all(&scratch)
        .map_err(|e| format!("Failed to create check directory: {}", e))?;

    println!("[orchestrate] Checking {} foreign source(s)...", sources.len());
    let mut failures = Vec::new();
    let mut deferred = Vec::new();
    for (index, source) in sources.iter().enumerate() {
        let (language, path) = (&source.language, &source.path);
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("?");
        let outcome = match &source.typescript {
            Some((handlers, declarations, ffi)) => crate::typescript::check(
                path,
                &scratch.join(format!("typescript_{}", index)),
                handlers,
                declarations,
                *ffi,
            )
            .map(|_| crate::foreign_check::Outcome::Checked),
            None => crate::foreign_check::check_file(language, path, &scratch, deep),
        };
        match outcome {
            Ok(crate::foreign_check::Outcome::Checked) => {
                println!("  ok    {:<10} {}", language, name)
            }
            Ok(crate::foreign_check::Outcome::CheckedWith(note)) => {
                println!("  ok    {:<10} {}  ({})", language, name, note)
            }
            Ok(crate::foreign_check::Outcome::Deferred) if deep => {
                deferred.push(name.to_string())
            }
            Ok(crate::foreign_check::Outcome::Deferred) => println!(
                "  skip  {:<10} {}  (checked by cargo during build; --deep checks it here)",
                language, name
            ),
            Err(diagnostics) => {
                println!("  FAIL  {:<10} {}", language, name);
                failures.push((path.clone(), diagnostics));
            }
        }
    }

    if deep {
        println!("[orchestrate] Deep check: generating Rust and running cargo check...");
        match check_generated_rust(input_file) {
            Ok(()) => {
                for name in &deferred {
                    println!("  ok    {:<10} {}  (cargo check)", "rust", name);
                }
                if deferred.is_empty() {
                    println!("  ok    generated Rust");
                }
            }
            Err(e) => {
                for name in &deferred {
                    println!("  FAIL  {:<10} {}  (cargo check)", "rust", name);
                }
                if deferred.is_empty() {
                    println!("  FAIL  generated Rust");
                }
                // print_friendly_errors has already reported the diagnostics.
                failures.push((PathBuf::from(input_file), e));
            }
        }
    }

    if failures.is_empty() {
        println!("[orchestrate] {} — no foreign errors found", input_file);
        return Ok(());
    }
    for (path, diagnostics) in &failures {
        eprintln!("\n--- {} ---\n{}", path.display(), diagnostics);
    }
    Err(format!("{} check(s) failed", failures.len()))
}

/// Resolve relative to the declaring file, including declarations loaded by modules.
fn resolve_landline_sources(stmts: &mut [ast::Stmt], directory: &Path) -> Result<(), String> {
    for stmt in stmts {
        if let ast::StmtNode::Serverlet { landline: Some(config), .. } = &mut stmt.node {
            let path = directory.join(&config.source).canonicalize()
                .map_err(|e| format!("Cannot resolve Landline source '{}': {}", config.source, e))?;
            if !path.is_file() { return Err(format!("Landline source is not a file: {}", path.display())); }
            config.source = path.to_str().ok_or("Landline source path must be UTF-8")?.to_string();
        }
    }
    Ok(())
}

fn stage_landlines(stmts: &[ast::Stmt], destination: &Path) -> Result<(), String> {
    for stmt in stmts {
        if let ast::StmtNode::Serverlet { name, handlers, landline: Some(config), .. } = &stmt.node {
            let directory = destination.join(format!("landline_{}", name));
            if directory.exists() { return Err(format!("Landline serverlet names must be unique: '{}'", name)); }
            if config.runtime == "typescript" {
                crate::typescript::build(Path::new(&config.source), &directory, handlers, stmts, false, config.backend.as_deref())?;
                continue;
            }
            let sdk = directory.join("orchestratelang");
            fs::create_dir_all(&sdk).map_err(|e| e.to_string())?;
            fs::copy(&config.source, directory.join("implementation.py")).map_err(|e| e.to_string())?;
            fs::write(directory.join("main.py"), "import os, runpy, sys\nsys.stdout = sys.stderr\nrunpy.run_path(os.path.join(os.path.dirname(__file__), 'implementation.py'), run_name='__main__')\n").map_err(|e| e.to_string())?;
            fs::write(sdk.join("__init__.py"), include_str!("../sdk/python/orchestratelang/__init__.py")).map_err(|e| e.to_string())?;
            fs::write(sdk.join("landline.py"), include_str!("../sdk/python/orchestratelang/landline.py")).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

fn copy_landlines(source: &Path, destination: &Path) -> Result<(), String> {
    if !source.exists() { return Ok(()); }
    fs::create_dir_all(destination).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(source).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let target = destination.join(entry.file_name());
        if entry.file_type().map_err(|e| e.to_string())?.is_dir() {
            copy_landlines(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// Generate a standalone Cargo library crate; do not overwrite unrelated directories.
pub fn run_build_library(input: &str, output: Option<&str>) -> Result<(), String> {
    run_build_library_for_target(input, output, None, None, &[])
}
pub fn run_build_library_for_target(input: &str, output: Option<&str>, target: Option<&str>, rust_version: Option<&str>, dependencies: &[crate::dependencies::Dependency]) -> Result<(), String> {
    let rust_version = library_rust_version(rust_version)?;
    let destination = PathBuf::from(output.ok_or("build --lib requires -o <crate directory>")?);
    let marker = destination.join(".orchestrate-library");
    if destination.exists() && !marker.is_file() {
        return Err("Library output directory already exists and was not generated by OrchestrateLang".into());
    }
    let cache = prepare_cache_dir(Path::new(input))?.join("library");
    fs::create_dir_all(cache.join("src")).map_err(|e| e.to_string())?;
    let source = compile_with_mode(input, &cache, Some(&rust_version), dependencies)?;
    if let Some(target) = target {
        let has_typescript = fs::read_dir(cache.join("landlines")).ok().into_iter().flatten().filter_map(Result::ok).any(|entry| entry.path().join("backend.txt").exists());
        if has_typescript {
            let rustc = Command::new("rustc").arg("-vV").output().map_err(|e| e.to_string())?;
            let version = String::from_utf8_lossy(&rustc.stdout);
            let host = version.lines().find_map(|l| l.strip_prefix("host: ")).ok_or("Cannot determine Rust host target")?;
            if target != host { return Err(format!("TypeScript FFI/landline executables currently require the host target {host}; requested {target}")); }
        }
    }
    fs::write(cache.join("src/lib.rs"), source).map_err(|e| e.to_string())?;
    // Generate placeholder assets while compiling sidecar binaries, then embed them.
    fs::write(cache.join("src/assets.rs"), "const ORCH_ASSETS: &[(&str, &[u8])] = &[];\n").map_err(|e| e.to_string())?;
    let assets = cache.join("assets");
    if assets.exists() { fs::remove_dir_all(&assets).map_err(|e| e.to_string())?; }
    fs::create_dir_all(&assets).map_err(|e| e.to_string())?;
    copy_landlines(&cache.join("landlines"), &assets)?;
    if cache.join("src/bin").exists() {
        let mut command = Command::new("cargo");
        command.args(["build", "--bins", "--quiet"]);
        if let Some(target) = target { command.args(["--target", target]); }
        let result = command.current_dir(&cache).output().map_err(|e| e.to_string())?;
        if !result.status.success() { return Err(String::from_utf8_lossy(&result.stderr).into_owned()); }
        for entry in fs::read_dir(cache.join("src/bin")).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            let suffix = if target.map_or(cfg!(windows), |t| t.contains("windows")) { ".exe" } else { "" };
            let name = format!("{}{}", path.file_stem().unwrap().to_string_lossy(), suffix);
            let binaries = match target { Some(t) => cache.join("target").join(t).join("debug"), None => cache.join("target/debug") };
            fs::copy(binaries.join(&name), assets.join(&name)).map_err(|e| e.to_string())?;
        }
    }
    fn asset_entries(directory: &Path, root: &Path, entries: &mut Vec<String>) -> Result<(), String> {
        for entry in fs::read_dir(directory).map_err(|e| e.to_string())? {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.is_dir() { asset_entries(&path, root, entries)?; }
            else {
                let relative = path.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/");
                entries.push(format!("({:?}, include_bytes!({:?}))", relative, format!("../assets/{}", relative)));
            }
        }
        Ok(())
    }
    let mut entries = Vec::new();
    asset_entries(&assets, &assets, &mut entries)?;
    entries.sort();
    fs::write(cache.join("src/assets.rs"), format!("const ORCH_ASSETS: &[(&str, &[u8])] = &[{}];\n", entries.join(",\n"))).map_err(|e| e.to_string())?;
    let manifest = fs::read_to_string(cache.join("Cargo.toml")).map_err(|e| e.to_string())?;
    let name = destination.file_name().and_then(|n| n.to_str()).ok_or("Output must name a crate directory")?.replace('-', "_");
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err("Output directory must be a valid Rust crate name".into());
    }
    let manifest = manifest.replace("name = \"orch_generated\"", &format!("name = {:?}\nautobins = false", name));
    fs::write(cache.join("Cargo.toml"), &manifest).map_err(|e| e.to_string())?;
    let mut command = Command::new("cargo");
    command.args(["check", "--lib", "--quiet"]);
    if let Some(target) = target { command.args(["--target", target]); }
    let result = command.current_dir(&cache).output().map_err(|e| e.to_string())?;
    if !result.status.success() { return Err(String::from_utf8_lossy(&result.stderr).into_owned()); }
    fs::create_dir_all(&destination).map_err(|e| e.to_string())?;
    copy_landlines(&cache.join("src"), &destination.join("src"))?;
    copy_landlines(&assets, &destination.join("assets"))?;
    fs::write(destination.join("Cargo.toml"), manifest.replace("\n[workspace]\n", "\n")).map_err(|e| e.to_string())?;
    if cache.join("build.rs").exists() { fs::copy(cache.join("build.rs"), destination.join("build.rs")).map_err(|e| e.to_string())?; }
    fs::write(marker, "Generated by OrchestrateLang\n").map_err(|e| e.to_string())?;
    println!("[orchestrate] Generated library crate: {}", destination.display());
    Ok(())
}
