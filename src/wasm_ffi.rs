//! `load_foreign "wasm"`: a WebAssembly module's exports called as ordinary functions.
//!
//! The sidecar declares the contract in OrchestrateLang types and the compiler checks it
//! against the module's own export table, so a name or a signature that does not match is
//! a build error rather than a trap at run time. Values cross as numbers — `int` is `i64`,
//! `float` is `f64`, `bool` is an `i32` that is 0 or 1 — and a `string` is a pointer and a
//! length into the module's memory, which means a module carrying strings must export
//! `orch_alloc(i32) -> i32` and `orch_free(i32, i32)` so both sides use one allocator.

use crate::ast::Type;
use crate::ffi_parser::CSignature;
use crate::wasm_module::{Module, ValType};

/// The wasm parameter types one OrchestrateLang parameter becomes.
fn param_types(ty: &Type) -> Vec<ValType> {
    match ty {
        Type::Int => vec![ValType::I64],
        Type::Float => vec![ValType::F64],
        Type::Bool => vec![ValType::I32],
        Type::Str => vec![ValType::I32, ValType::I32],
        _ => Vec::new(),
    }
}

/// The wasm result types one OrchestrateLang return type becomes.
fn result_types(ty: &Type) -> Vec<ValType> {
    match ty {
        Type::Void => Vec::new(),
        Type::Str => vec![ValType::I64],
        other => param_types(other),
    }
}

fn rust_type(val: ValType) -> &'static str {
    match val {
        ValType::I32 => "i32",
        ValType::I64 => "i64",
        ValType::F32 => "f32",
        _ => "f64",
    }
}

fn unsupported(ty: &Type) -> bool {
    !matches!(ty, Type::Int | Type::Float | Type::Bool | Type::Str | Type::Void)
}

/// A tuple type for `get_typed_func`, which takes its parameters as one tuple. A single
/// type keeps its trailing comma, or `(i64)` would parse as `i64` and stop matching the
/// tuple the call passes.
fn tuple(types: &[ValType]) -> String {
    let names = types.iter().map(|t| rust_type(*t)).collect::<Vec<_>>();
    match names.len() {
        1 => format!("({},)", names[0]),
        _ => format!("({})", names.join(", ")),
    }
}

fn describe(types: &[ValType]) -> String {
    if types.is_empty() {
        "nothing".to_string()
    } else {
        types.iter().map(|t| t.name()).collect::<Vec<_>>().join(", ")
    }
}

/// Check every declared signature against the module's export table.
pub fn check(signatures: &[CSignature], module: &Module, file: &str, wasm: &str) -> Result<(), String> {
    let mut carries_strings = false;
    for signature in signatures {
        if signature.drop {
            return Err(format!(
                "{}: `drop` applies to C-ABI handles, which wasm modules do not have",
                file
            ));
        }
        for (name, ty) in &signature.params {
            if unsupported(ty) {
                return Err(format!(
                    "{}: '{}' parameter '{}' has a type a wasm module cannot carry; wasm functions take int, float, bool, and string",
                    file, signature.name, name
                ));
            }
        }
        if unsupported(&signature.ret) {
            return Err(format!(
                "{}: '{}' returns a type a wasm module cannot carry; wasm functions return int, float, bool, string, or nothing",
                file, signature.name
            ));
        }
        carries_strings |= signature.ret == Type::Str
            || signature.params.iter().any(|(_, ty)| *ty == Type::Str);

        let expected_params: Vec<ValType> = signature.params.iter().flat_map(|(_, ty)| param_types(ty)).collect();
        let expected_results = result_types(&signature.ret);
        let Some(actual) = module.export(&signature.name) else {
            let mut exports = module.exported_function_names();
            exports.sort();
            return Err(format!(
                "{}: declares '{}', which {} does not export. It exports: {}",
                file,
                signature.name,
                wasm,
                if exports.is_empty() { "nothing".to_string() } else { exports.join(", ") }
            ));
        };
        if actual.params != expected_params || actual.results != expected_results {
            return Err(format!(
                "{}: '{}' is declared to take ({}) and return {}, but {} exports it taking ({}) and returning {}",
                file,
                signature.name,
                describe(&expected_params),
                describe(&expected_results),
                wasm,
                describe(&actual.params),
                describe(&actual.results)
            ));
        }
    }

    if carries_strings {
        for (required, params, results) in [
            ("orch_alloc", vec![ValType::I32], vec![ValType::I32]),
            ("orch_free", vec![ValType::I32, ValType::I32], Vec::new()),
        ] {
            match module.export(required) {
                Some(found) if found.params == params && found.results == results => {}
                Some(_) => {
                    return Err(format!(
                        "{}: {} exports '{}' with the wrong signature; a module that carries strings needs `orch_alloc(i32) -> i32` and `orch_free(i32, i32)`",
                        file, wasm, required
                    ))
                }
                None => {
                    return Err(format!(
                        "{}: a declared function carries a string, so {} must export '{}'; a module that carries strings needs `orch_alloc(i32) -> i32` and `orch_free(i32, i32)`",
                        file, wasm, required
                    ))
                }
            }
        }
        if !module.exports_memory {
            return Err(format!(
                "{}: a declared function carries a string, so {} must export its memory",
                file, wasm
            ));
        }
    }
    Ok(())
}

/// The Rust bindings for one wasm module: a lazily loaded instance and one function per
/// declared export.
pub fn bindings(signatures: &[CSignature], asset: &str, label: &str) -> String {
    let upper = asset.to_uppercase();
    let mut code = format!(
        "static {upper}: std::sync::OnceLock<crate::__OrchWasmModule> = std::sync::OnceLock::new();\n\
         fn {asset}() -> &'static crate::__OrchWasmModule {{\n    \
             {upper}.get_or_init(|| crate::__OrchWasmModule::load({label:?}, include_bytes!({file:?})))\n\
         }}\n",
        upper = upper,
        asset = asset,
        label = label,
        file = format!("{}.wasm", asset),
    );

    for signature in signatures {
        let name = &signature.name;
        let params = signature
            .params
            .iter()
            .map(|(param, ty)| format!("{}: {}", param, declared_type(ty)))
            .collect::<Vec<_>>()
            .join(", ");
        let returns = match signature.ret {
            Type::Void => String::new(),
            _ => format!(" -> {}", declared_type(&signature.ret)),
        };

        // Strings are copied into the module's memory first; the rest are already numbers.
        let mut setup = String::new();
        let mut arguments = Vec::new();
        for (param, ty) in &signature.params {
            match ty {
                Type::Str => {
                    setup.push_str(&format!(
                        "        let ({param}_pointer, {param}_length) = __guest.write_string(&{param})?;\n"
                    ));
                    arguments.push(format!("{param}_pointer"));
                    arguments.push(format!("{param}_length"));
                }
                Type::Bool => arguments.push(format!("{param} as i32")),
                _ => arguments.push(param.clone()),
            }
        }

        let wasm_params: Vec<ValType> = signature.params.iter().flat_map(|(_, ty)| param_types(ty)).collect();
        let wasm_results = result_types(&signature.ret);
        let results = match wasm_results.len() {
            0 => "()".to_string(),
            _ => rust_type(wasm_results[0]).to_string(),
        };
        let convert = match signature.ret {
            Type::Void => "        Ok(())".to_string(),
            Type::Bool => "        Ok(__value != 0)".to_string(),
            Type::Str => "        __guest.read_string(__value)".to_string(),
            _ => "        Ok(__value)".to_string(),
        };

        code.push_str(&format!(
            "pub fn {name}({params}){returns} {{\n    \
                 {asset}().call({name:?}, |__guest| {{\n\
             {setup}        let __function = __guest.instance.get_typed_func::<{tuple}, {results}>(&mut __guest.store, {name:?}).map_err(|e| format!(\"wasm call '{{}}': {{}}\", {name:?}, e))?;\n        \
                 let __called = __function.call(&mut __guest.store, ({arguments}));\n        \
                 let __value = __called.map_err(|e| __guest.failed({name:?}, &e))?;\n\
             {convert}\n    \
                 }})\n\
             }}\n",
            name = name,
            params = params,
            returns = returns,
            asset = asset,
            setup = setup,
            tuple = tuple(&wasm_params),
            results = results,
            // A one-element tuple needs its trailing comma to stay a tuple.
            arguments = if arguments.len() == 1 { format!("{},", arguments[0]) } else { arguments.join(", ") },
            convert = convert,
        ));
    }
    code
}

fn declared_type(ty: &Type) -> &'static str {
    match ty {
        Type::Int => "i64",
        Type::Float => "f64",
        Type::Bool => "bool",
        Type::Str => "String",
        _ => "()",
    }
}

/// A deterministic, unique name for a module's embedded wasm and its accessor.
pub fn asset_name(module: &str, path: &std::path::Path) -> String {
    let stem: String = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("module")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect();
    format!("__orch_wasm_{}_{}", module.to_lowercase(), stem)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signature(name: &str, params: Vec<(&str, Type)>, ret: Type) -> CSignature {
        CSignature {
            name: name.to_string(),
            params: params.into_iter().map(|(n, t)| (n.to_string(), t)).collect(),
            ret,
            drop: false,
        }
    }

    /// A module exporting `add(i64, i64) -> i64` and nothing else.
    fn module(params: &[u8], results: &[u8], name: &str) -> Module {
        let mut types = vec![1, 0x60, params.len() as u8];
        types.extend(params);
        types.push(results.len() as u8);
        types.extend(results);
        let mut exports = vec![1, name.len() as u8];
        exports.extend(name.as_bytes());
        exports.extend([0x00, 0]);
        let mut bytes = Vec::from(*b"\0asm");
        bytes.extend([1, 0, 0, 0]);
        for (id, content) in [(1u8, types), (3, vec![1, 0]), (7, exports)] {
            bytes.push(id);
            bytes.push(content.len() as u8);
            bytes.extend(content);
        }
        Module::parse(&bytes).unwrap()
    }

    #[test]
    fn accepts_a_matching_signature() {
        let found = module(&[0x7e, 0x7e], &[0x7e], "add");
        let declared = [signature("add", vec![("a", Type::Int), ("b", Type::Int)], Type::Int)];
        assert!(check(&declared, &found, "math.orch_ffi", "math.wasm").is_ok());
    }

    #[test]
    fn a_wrong_type_names_both_sides() {
        let found = module(&[0x7e, 0x7e], &[0x7e], "add");
        let declared = [signature("add", vec![("a", Type::Float), ("b", Type::Int)], Type::Int)];
        let error = check(&declared, &found, "math.orch_ffi", "math.wasm").unwrap_err();
        assert!(error.contains("declared to take (f64, i64)"), "{error}");
        assert!(error.contains("exports it taking (i64, i64)"), "{error}");
    }

    #[test]
    fn a_missing_export_lists_what_there_is() {
        let found = module(&[0x7e], &[0x7e], "double");
        let declared = [signature("add", vec![("a", Type::Int)], Type::Int)];
        let error = check(&declared, &found, "math.orch_ffi", "math.wasm").unwrap_err();
        assert!(error.contains("does not export"), "{error}");
        assert!(error.contains("double"), "{error}");
    }

    #[test]
    fn strings_require_the_shared_allocator() {
        let found = module(&[0x7f, 0x7f], &[0x7e], "shout");
        let declared = [signature("shout", vec![("text", Type::Str)], Type::Str)];
        let error = check(&declared, &found, "text.orch_ffi", "text.wasm").unwrap_err();
        assert!(error.contains("orch_alloc"), "{error}");
    }

    #[test]
    fn a_void_return_expects_no_results() {
        let found = module(&[0x7e], &[], "record");
        let declared = [signature("record", vec![("n", Type::Int)], Type::Void)];
        assert!(check(&declared, &found, "log.orch_ffi", "log.wasm").is_ok());
    }

    #[test]
    fn one_argument_stays_a_tuple() {
        let declared = [signature("double", vec![("n", Type::Int)], Type::Int)];
        let code = bindings(&declared, "__orch_wasm_math_lib", "math/lib.wasm");
        assert!(code.contains("(n,)"), "{code}");
        assert!(code.contains("get_typed_func::<(i64,), i64>"), "{code}");
    }
}
