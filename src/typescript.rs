//! TypeScript 7 checking and automatic scriptc/Bun executable selection.
use crate::codegen::core::rust_ident;
use crate::ast::{Handler, Param, Stmt, StmtNode, Type};
use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    process::Command,
};

fn tool(source: &Path, variable: &str, default: &str) -> std::ffi::OsString {
    if let Some(value) = std::env::var_os(variable) {
        return value;
    }
    for ancestor in source.parent().unwrap_or(Path::new(".")).ancestors() {
        let candidate = ancestor.join("node_modules/.bin").join(default);
        if candidate.is_file() {
            return candidate.into_os_string();
        }
    }
    default.into()
}
fn ts_type(ty: &Type) -> Result<String, String> {
    Ok(match ty {
        Type::Int => "bigint".into(),
        Type::Float => "number".into(),
        Type::Bool => "boolean".into(),
        Type::Str => "string".into(),
        Type::Void => "void".into(),
        Type::Array(t, _) => format!("{}[]", ts_type(t)?),
        Type::Named(n) => rust_ident(n),
        _ => {
            return Err(format!(
                "Unsupported TypeScript wire type {}",
                ty.display_name()
            ))
        }
    })
}

pub fn sidecar(source: &str) -> Result<Vec<Handler>, String> {
    use crate::lexer::{Lexer, TokenKind as K};
    let tokens = Lexer::new(source).tokenize()?;
    let mut pos = 0;
    let mut handlers = Vec::new();
    while tokens.get(pos).is_some_and(|t| t.kind != K::EOF) {
        let name = match &tokens[pos].kind {
            K::Identifier(s) => s.clone(),
            _ => return Err("Expected TypeScript FFI function name".into()),
        };
        pos += 1;
        if tokens.get(pos).map(|t| &t.kind) != Some(&K::LParen) {
            return Err("Expected '(' in TypeScript sidecar".into());
        }
        pos += 1;
        let mut params = Vec::new();
        while tokens.get(pos).map(|t| &t.kind) != Some(&K::RParen) {
            let name = match tokens.get(pos).map(|t| &t.kind) {
                Some(K::Identifier(s)) => s.clone(),
                _ => return Err("Expected TypeScript FFI parameter".into()),
            };
            pos += 1;
            if tokens.get(pos).map(|t| &t.kind) != Some(&K::Colon) {
                return Err("Expected ':' in TypeScript sidecar".into());
            }
            pos += 1;
            let ty = crate::ffi_rust::parse_sidecar_type(&tokens, &mut pos, "TypeScript sidecar", &[])?;
            ts_type(&ty)?;
            if ty == Type::Void {
                return Err("void parameters are unsupported".into());
            }
            params.push(Param { name, ty });
            if tokens.get(pos).map(|t| &t.kind) == Some(&K::Comma) {
                pos += 1;
            } else {
                break;
            }
        }
        if tokens.get(pos).map(|t| &t.kind) != Some(&K::RParen) {
            return Err("Expected ')' in TypeScript sidecar".into());
        }
        pos += 1;
        let return_type = if tokens.get(pos).map(|t| &t.kind) == Some(&K::Arrow) {
            pos += 1;
            crate::ffi_rust::parse_sidecar_type(&tokens, &mut pos, "TypeScript sidecar", &[])?
        } else {
            Type::Void
        };
        ts_type(&return_type)?;
        if handlers.iter().any(|h: &Handler| h.name == name) {
            return Err(format!("Duplicate TypeScript FFI function {name}"));
        }
        handlers.push(Handler {
            name,
            params,
            return_type,
            body: crate::ast::Spanned {
                node: crate::ast::ExprNode::Block(Vec::new()),
                span: crate::ast::Span::new(1, 1),
            },
        });
    }
    if let Some(reason) = crate::codegen::stmt::wire_unsupported_reason(&handlers, &[]) {
        return Err(reason);
    }
    Ok(handlers)
}

/// A staged adapter directory that has passed the TypeScript 7 check.
struct Staged {
    source: PathBuf,
    work: PathBuf,
}

/// Stage the whole adapter and type-check it. Checking the generated adapter rather
/// than the user's source alone is what verifies the implementation against the
/// contract the sidecar or the serverlet's handlers declare.
fn stage_and_check(
    source: &Path,
    destination: &Path,
    handlers: &[Handler],
    stmts: &[Stmt],
    ffi: bool,
) -> Result<Staged, String> {
    let source = source.canonicalize().map_err(|e| e.to_string())?;
    let mut hasher = DefaultHasher::new();
    destination.hash(&mut hasher);
    let work = source
        .parent()
        .unwrap()
        .join(".orch_cache")
        .join(format!("typescript_{:x}", hasher.finish()));
    fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    fs::create_dir_all(destination).map_err(|e| e.to_string())?;
    let schema = stmts
        .iter()
        .filter_map(|s| match &s.node {
            StmtNode::StructDef { name, fields } => Some((
                name.clone(),
                fields
                    .iter()
                    .map(|p| serde_json::json!({"name":p.0,"type":p.1.display_name()}))
                    .collect::<Vec<_>>(),
            )),
            _ => None,
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut declarations = String::new();
    for s in stmts {
        if let StmtNode::StructDef { name, fields } = &s.node {
            declarations.push_str(&format!(
                "interface {name} {{ {} }}\n",
                fields
                    .iter()
                    .map(|p| Ok(format!("{}: {};", p.0, ts_type(&p.1)?)))
                    .collect::<Result<Vec<_>, String>>()?
                    .join(" ")
            ));
        }
    }
    let contract = handlers
        .iter()
        .map(|h| {
            Ok(format!(
                "{}({}): {} | Promise<{}>;",
                h.name,
                h.params
                    .iter()
                    .map(|p| Ok(format!("{}: {}", p.name, ts_type(&p.ty)?)))
                    .collect::<Result<Vec<_>, String>>()?
                    .join(","),
                ts_type(&h.return_type)?,
                ts_type(&h.return_type)?
            ))
        })
        .collect::<Result<Vec<_>, String>>()?
        .join("\n");
    let methods = handlers.iter().map(|h| serde_json::json!({"name": h.name, "args": h.params.iter().map(|p| p.ty.display_name()).collect::<Vec<_>>(), "result":h.return_type.display_name()})).collect::<Vec<_>>();
    fs::write(
        work.join("landline.ts"),
        include_str!("../sdk/typescript/landline.ts"),
    )
    .map_err(|e| e.to_string())?;
    // Separate bootstrap evaluates before implementation imports, preserving stdout framing.
    fs::write(work.join("bootstrap.ts"), "console.log = console.error; console.info = console.error; console.debug = console.error;\n").map_err(|e| e.to_string())?;
    let import = serde_json::to_string(&format!(
        "../../{}",
        source
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or("TypeScript path must be UTF-8")?
    ))
    .unwrap();
    let factory = if ffi {
        "implementation"
    } else {
        "new Constructor(context)"
    };
    let constructor = if ffi {
        ""
    } else {
        "const Constructor: new(context: Context) => Contract = Implementation;"
    };
    let import_name = if ffi {
        "* as implementation"
    } else {
        "Implementation"
    };
    let adapter = format!("import './bootstrap.ts';\nimport {{serve, type Context}} from './landline.ts';\nimport {import_name} from {import};\n{declarations}\ninterface Contract {{ {contract} }}\n{constructor}\nserve((context: Context): Contract => {factory}, {}, {}).catch(error => {{ console.error(error); process.exitCode = 1; }});\n", serde_json::to_string(&methods).unwrap(), serde_json::to_string(&schema).unwrap());
    fs::write(work.join("main.ts"), adapter).map_err(|e| e.to_string())?;
    fs::write(work.join("platform.d.ts"), "declare module 'node:fs' { export function read(fd: number, buffer: Uint8Array, offset: number, length: number, position: null, callback: (error: Error | null, count: number) => void): void; export function readSync(fd: number, buffer: Uint8Array, offset: number, length: number, position: null): number; export function writeSync(fd: number, buffer: Uint8Array, offset: number, length: number): number; }\ndeclare var process: { exitCode: number };\n").map_err(|e| e.to_string())?;
    let mut config = serde_json::json!({"compilerOptions":{"target":"ES2022","module":"ESNext","moduleResolution":"bundler","strict":true,"noEmit":true,"allowImportingTsExtensions":true,"types":[],"skipLibCheck":true},"files":["main.ts","platform.d.ts"]});
    for ancestor in source.parent().unwrap().ancestors() {
        let project = ancestor.join("tsconfig.json");
        if project.is_file() {
            config["extends"] = serde_json::json!(project);
            config["compilerOptions"]
                .as_object_mut()
                .unwrap()
                .remove("types");
            config["files"] = serde_json::json!(["main.ts"]);
            break;
        }
        let types_root = ancestor.join("node_modules/@types");
        let types = ["bun", "node"]
            .into_iter()
            .map(|name| types_root.join(name))
            .filter(|p| p.is_dir())
            .collect::<Vec<_>>();
        if !types.is_empty() {
            config["compilerOptions"]["types"] = serde_json::json!(types);
            config["files"] = serde_json::json!(["main.ts"]);
            break;
        }
    }
    fs::write(
        work.join("tsconfig.json"),
        serde_json::to_vec(&config).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    let checker = tool(&source, "ORCH_TSC", "tsc");
    let version = Command::new(&checker).arg("--version").output().map_err(|e| format!("TypeScript 7 checker unavailable: {e}. Install with `bun add --dev typescript`, then put its bin directory on PATH or set ORCH_TSC."))?;
    let version_text = String::from_utf8_lossy(&version.stdout);
    let major = version_text
        .trim()
        .strip_prefix("Version ")
        .and_then(|s| s.split('.').next())
        .and_then(|s| s.parse::<u32>().ok());
    if !version.status.success() || major != Some(7) {
        return Err(format!(
            "TypeScript 7 is required; checker reported {}",
            version_text.trim()
        ));
    }
    let checked = Command::new(&checker)
        .arg("--project")
        .arg(work.join("tsconfig.json"))
        .output()
        .map_err(|e| e.to_string())?;
    if !checked.status.success() {
        return Err(format!(
            "TypeScript 7 check failed:\n{}{}",
            String::from_utf8_lossy(&checked.stdout),
            String::from_utf8_lossy(&checked.stderr)
        ));
    }
    Ok(Staged { source, work })
}

/// Type-check a TypeScript source against its contract, without building an executable.
pub fn check(
    source: &Path,
    destination: &Path,
    handlers: &[Handler],
    stmts: &[Stmt],
    ffi: bool,
) -> Result<(), String> {
    stage_and_check(source, destination, handlers, stmts, ffi)?;
    Ok(())
}

/// Build the entire adapter, not just the user's source: native coverage must include transport.
pub fn build(
    source: &Path,
    destination: &Path,
    handlers: &[Handler],
    stmts: &[Stmt],
    ffi: bool,
    backend: Option<&str>,
) -> Result<(), String> {
    let Staged { source, work } = stage_and_check(source, destination, handlers, stmts, ffi)?;
    // The declaration in source wins; the environment is only a project-wide default.
    let selection = match backend {
        Some(backend) => backend.to_string(),
        None => std::env::var("ORCH_TS_BACKEND").unwrap_or_else(|_| "auto".into()),
    };
    if !matches!(selection.as_str(), "auto" | "scriptc" | "bun") {
        return Err("ORCH_TS_BACKEND must be auto, scriptc, or bun".into());
    }
    let output = destination.join(if cfg!(windows) {
        "serverlet.exe"
    } else {
        "serverlet"
    });
    let mut native_error = String::new();
    if selection != "bun" {
        match Command::new(tool(&source, "ORCH_SCRIPTC", "scriptc"))
            .arg("build")
            .arg(work.join("main.ts"))
            .arg("-o")
            .arg(&output)
            .output()
        {
            Ok(result) if result.status.success() && output.is_file() => {
                fs::write(destination.join("backend.txt"), "scriptc\n")
                    .map_err(|e| e.to_string())?;
                println!(
                    "[orchestrate] TypeScript {}: scriptc native",
                    source.display()
                );
                return Ok(());
            }
            Ok(result) => {
                native_error = format!(
                    "{}{}",
                    String::from_utf8_lossy(&result.stdout),
                    String::from_utf8_lossy(&result.stderr)
                )
            }
            Err(e) => native_error.push_str(&e.to_string()),
        }
    }
    if selection == "scriptc" {
        return Err(format!(
            "scriptc could not compile the TypeScript adapter:\n{native_error}"
        ));
    }
    let result = Command::new(tool(&source, "ORCH_BUN", "bun"))
        .arg("build")
        .arg(work.join("main.ts"))
        .arg("--compile")
        .arg("--outfile")
        .arg(&output)
        .output()
        .map_err(|e| format!("Bun unavailable ({e}); scriptc: {native_error}"))?;
    if !result.status.success() {
        return Err(format!(
            "Bun TypeScript build failed:\n{}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        ));
    }
    fs::write(destination.join("backend.txt"), "bun\n").map_err(|e| e.to_string())?;
    println!(
        "[orchestrate] TypeScript {}: Bun compiled executable{}",
        source.display(),
        if selection == "auto" {
            " (scriptc unavailable or unsupported; see native-diagnostic.txt)"
        } else {
            ""
        }
    );
    fs::write(destination.join("native-diagnostic.txt"), native_error)
        .map_err(|e| e.to_string())?;
    Ok(())
}

pub fn ffi_bindings(
    asset: &str,
    handlers: &[Handler],
    library: bool,
) -> Result<String, String> {
    fn rust_type(t: &Type) -> Result<String, String> {
        Ok(match t {
            Type::Int => "i64".into(),
            Type::Float => "f64".into(),
            Type::Bool => "bool".into(),
            Type::Str => "String".into(),
            Type::Void => "()".into(),
            Type::Array(t, _) => format!("Vec<{}>", rust_type(t)?),
            _ => return Err("Unsupported TypeScript FFI type".into()),
        })
    }
    let module = format!("__orch_ts_{asset}");
    let base = if library {
        "crate::__orch_context().assets.clone()"
    } else {
        "std::env::current_exe().expect(\"current_exe\").parent().unwrap().to_path_buf()"
    };
    let mut code = format!("mod {module} {{\n{}\n", crate::codegen::core::WIRE_CODEC);
    code.push_str(include_str!("typescript_ffi.rs.txt"));
    let signatures = handlers
        .iter()
        .map(|h| {
            format!(
                "{:?}.to_string()",
                format!(
                    "{}({})->{}",
                    h.name,
                    h.params
                        .iter()
                        .map(|p| p.ty.display_name())
                        .collect::<Vec<_>>()
                        .join(","),
                    h.return_type.display_name()
                )
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    for (id, h) in handlers.iter().enumerate() {
        let args = h
            .params
            .iter()
            .map(|p| Ok(format!("{}: {}", rust_ident(&p.name), rust_type(&p.ty)?)))
            .collect::<Result<Vec<_>, String>>()?
            .join(",");
        let encode = h
            .params
            .iter()
            .map(|p| format!("{}.wire_encode(&mut payload);", rust_ident(&p.name)))
            .collect::<Vec<_>>()
            .join("\n");
        let decode = if h.return_type == Type::Void {
            "assert!(reply.is_empty(), \"unexpected void reply\"); ()".into()
        } else {
            format!("let mut pos = 0; let value = <{} as OrchWire>::wire_decode(&reply, &mut pos).expect(\"invalid TypeScript FFI reply\"); assert_eq!(pos, reply.len(), \"trailing TypeScript reply\"); value", rust_type(&h.return_type)?)
        };
        code.push_str(&format!("pub fn {}({args}) -> {} {{ let mut payload = Vec::new(); {id}i64.wire_encode(&mut payload); {encode} let reply = call({base}.join({asset:?}).join(if cfg!(windows) {{\"serverlet.exe\"}} else {{\"serverlet\"}}), payload, vec![{signatures}]); {decode} }}\n", rust_ident(&h.name), rust_type(&h.return_type)?));
    }
    code.push_str("}\n");
    for h in handlers {
        code.push_str(&format!("pub use {module}::{};\n", rust_ident(&h.name)));
    }
    Ok(code)
}
