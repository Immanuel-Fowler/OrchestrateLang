//! C-ABI sidecars (`.orch_ffi` beside a C, C++, Zig, or Swift file): signatures in
//! OrchestrateLang types, turned into `extern "C"` declarations and safe wrappers.
//!
//! Types cross the boundary as: `int` `i64`, `float` `f64`, `bool` `bool`, `string` as a
//! NUL-terminated `const char *` valid for the call (parameters) or a `malloc`'d
//! `char *` the wrapper copies and frees (returns), and `handle` as an opaque `void *`
//! the sidecar's one `drop <function>(h: handle)` releases when the last owner drops.
use crate::codegen::core::rust_ident;
use crate::ast::Type;
use crate::lexer::{Lexer, TokenKind};

/// One line of a C-ABI sidecar.
#[derive(Clone, Debug, PartialEq)]
pub struct CSignature {
    pub name: String,
    pub params: Vec<(String, Type)>,
    pub ret: Type,
    /// `drop name(h: handle)`: releases this sidecar's handles. Not callable from
    /// OrchestrateLang; the generated handle calls it.
    pub drop: bool,
}

impl CSignature {
    /// Which parameters are handles; codegen passes those by reference.
    pub fn handle_params(&self) -> Vec<bool> {
        self.params.iter().map(|(_, t)| *t == Type::Handle).collect()
    }

    pub fn mentions_handle(&self) -> bool {
        self.ret == Type::Handle || self.params.iter().any(|(_, t)| *t == Type::Handle)
    }
}

pub fn parse_ffi_and_generate_bindings(
    ffi_content: &str,
    language: &str,
    file_name: &str,
) -> Result<String, String> {
    let signatures = parse_ffi(ffi_content, language, file_name)?;
    generate_bindings(&signatures, file_name)
}

pub fn parse_ffi(ffi_content: &str, language: &str, file_name: &str) -> Result<Vec<CSignature>, String> {
    let mut lexer = Lexer::new(ffi_content);
    let tokens = lexer.tokenize().map_err(|e| format!("Error in {} at {}", file_name, e))?;

    let mut signatures = Vec::new();
    let mut pos = 0;

    while pos < tokens.len() && tokens[pos].kind != TokenKind::EOF {
        // `drop name(...)`: a drop keyword followed by a name; a function called `drop`
        // is followed by `(` instead.
        let mut drop = false;
        if matches!(&tokens[pos].kind, TokenKind::Identifier(n) if n == "drop")
            && matches!(tokens.get(pos + 1).map(|t| &t.kind), Some(TokenKind::Identifier(_)))
        {
            drop = true;
            pos += 1;
        }
        let start_tok = &tokens[pos];
        let fn_name = match &start_tok.kind {
            TokenKind::Identifier(n) => n.clone(),
            _ => return Err(format!("Error in {} at line {}: expected function name, found {:?}", file_name, start_tok.line, start_tok.kind)),
        };
        pos += 1;

        let mut tok = tokens.get(pos).unwrap_or_else(|| &tokens[tokens.len()-1]);
        if tok.kind != TokenKind::LParen {
            return Err(format!("Error in {} at line {}: expected '(' after function name '{}', found {:?}", file_name, tok.line, fn_name, tok.kind));
        }
        pos += 1;

        let mut params = Vec::new();
        tok = tokens.get(pos).unwrap_or_else(|| &tokens[tokens.len()-1]);

        while tok.kind != TokenKind::RParen && tok.kind != TokenKind::EOF {
            let arg_name = match &tok.kind {
                TokenKind::Identifier(n) => n.clone(),
                _ => return Err(format!("Error in {} at line {}: expected parameter name, found {:?}", file_name, tok.line, tok.kind)),
            };
            pos += 1;

            tok = tokens.get(pos).unwrap_or_else(|| &tokens[tokens.len()-1]);
            if tok.kind != TokenKind::Colon {
                return Err(format!("Error in {} at line {}: expected ':' after parameter name '{}', found {:?}", file_name, tok.line, arg_name, tok.kind));
            }
            pos += 1;

            tok = tokens.get(pos).unwrap_or_else(|| &tokens[tokens.len()-1]);
            let arg_type_str = match &tok.kind {
                TokenKind::Identifier(n) => n.clone(),
                _ => return Err(format!("Error in {} at line {}: expected parameter type, found {:?}", file_name, tok.line, tok.kind)),
            };
            let line = tok.line;
            pos += 1;
            let arg_ty = c_type_at(&arg_type_str, &tokens, &mut pos, file_name, line)?;
            params.push((arg_name, arg_ty));

            tok = tokens.get(pos).unwrap_or_else(|| &tokens[tokens.len()-1]);
            if tok.kind == TokenKind::Comma {
                pos += 1;
                tok = tokens.get(pos).unwrap_or_else(|| &tokens[tokens.len()-1]);
                if tok.kind == TokenKind::RParen {
                    return Err(format!("Error in {} at line {}: expected parameter name, found {:?}", file_name, tok.line, tok.kind));
                }
            } else if tok.kind != TokenKind::RParen {
                return Err(format!("Error in {} at line {}: expected ',' or ')', found {:?}", file_name, tok.line, tok.kind));
            }
        }

        if tok.kind != TokenKind::RParen {
            return Err(format!("Error in {} at line {}: expected ')', found {:?}", file_name, tok.line, tok.kind));
        }
        pos += 1;

        let mut ret = Type::Void;
        tok = tokens.get(pos).unwrap_or_else(|| &tokens[tokens.len()-1]);
        if tok.kind == TokenKind::Arrow {
            pos += 1;
            tok = tokens.get(pos).unwrap_or_else(|| &tokens[tokens.len()-1]);
            let ret_type_str = match &tok.kind {
                TokenKind::Identifier(n) => n.clone(),
                _ => return Err(format!("Error in {} at line {}: expected return type, found {:?}", file_name, tok.line, tok.kind)),
            };
            let line = tok.line;
            pos += 1;
            ret = c_type_at(&ret_type_str, &tokens, &mut pos, file_name, line)?;
        }

        signatures.push(CSignature { name: fn_name, params, ret, drop });
    }

    let drops = signatures.iter().filter(|s| s.drop).count();
    if drops > 1 {
        return Err(format!("Error in {}: only one `drop` function per sidecar", file_name));
    }
    if let Some(release) = signatures.iter().find(|s| s.drop) {
        let one_handle = release.params.len() == 1 && release.params[0].1 == Type::Handle;
        if !one_handle || release.ret != Type::Void {
            return Err(format!("Error in {}: `drop {}` must take one handle and return nothing", file_name, release.name));
        }
    }
    if drops == 0 && signatures.iter().any(|s| s.ret == Type::Handle) {
        return Err(format!(
            "load_foreign '{}': {} returns a handle but declares no `drop <function>(h: handle)` to release it",
            language, file_name
        ));
    }

    Ok(signatures)
}

pub fn generate_bindings(signatures: &[CSignature], file_name: &str) -> Result<String, String> {
    let stem: String = file_name
        .trim_end_matches(".orch_ffi")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let free_symbol = format!("__ffi_free_{}", stem);
    let release = signatures.iter().find(|s| s.drop).map(|s| format!("__ffi_{}", s.name));

    let mut extern_c = String::from("unsafe extern \"C\" {\n");
    let mut wrappers = String::new();
    let mut returns_string = false;

    for signature in signatures {
        // An array is two C parameters, a pointer and a count, for one declared one.
        let mut decl_parts: Vec<String> = Vec::new();
        for (name, ty) in &signature.params {
            match ty {
                Type::Array(inner, _) => {
                    decl_parts.push(format!("{}: *const {}", rust_ident(name), ffi_type(inner, false)));
                    decl_parts.push(format!("{}_count: i64", name));
                }
                _ => decl_parts.push(format!("{}: {}", rust_ident(name), ffi_type(ty, false))),
            }
        }
        // An array return is a pointer plus a count written through an out-parameter,
        // because C returns one value.
        let returns_array = matches!(signature.ret, Type::Array(_, _));
        if returns_array {
            decl_parts.push("__out_count: *mut i64".to_string());
        }
        let decl_params = decl_parts.join(", ");
        let decl_ret = if signature.ret == Type::Void {
            String::new()
        } else if let Type::Array(inner, _) = &signature.ret {
            format!(" -> *mut {}", ffi_type(inner, true))
        } else {
            format!(" -> {}", ffi_type(&signature.ret, true))
        };
        extern_c.push_str(&format!("    #[link_name = \"{0}\"]\n    fn __ffi_{0}({1}){2};\n", signature.name, decl_params, decl_ret));
        if signature.drop {
            continue;
        }

        let wrapper_params = signature.params.iter()
            .map(|(name, ty)| format!("{}: {}", rust_ident(name), wrapper_type(ty, false)))
            .collect::<Vec<_>>()
            .join(", ");
        let wrapper_ret = if signature.ret == Type::Void { String::new() } else { format!(" -> {}", wrapper_type(&signature.ret, true)) };
        let mut prologue = String::new();
        let mut call_parts: Vec<String> = Vec::new();
        for (name, ty) in &signature.params {
            match ty {
                Type::Str => {
                    prologue.push_str(&format!(
                        "    let __{0} = std::ffi::CString::new({1}.replace('\\0', \"\")).expect(\"a string without NUL\");\n",
                        name, rust_ident(name)
                    ));
                    call_parts.push(format!("__{}.as_ptr()", name));
                }
                Type::Handle => call_parts.push(format!("{}.ptr()", rust_ident(name))),
                Type::Array(_, _) => {
                    call_parts.push(format!("{}.as_ptr()", rust_ident(name)));
                    call_parts.push(format!("{}.len() as i64", rust_ident(name)));
                }
                _ => call_parts.push(rust_ident(name)),
            }
        }
        if returns_array {
            prologue.push_str("    let mut __count: i64 = 0;\n");
            call_parts.push("&mut __count".to_string());
        }
        let call_args = call_parts.join(", ");
        let call = format!("unsafe {{ __ffi_{}({}) }}", signature.name, call_args);
        let body = match &signature.ret {
            Type::Str => {
                returns_string = true;
                format!(
                    "    let __result = {call};\n    if __result.is_null() {{ String::new() }} else {{\n        let __text = unsafe {{ std::ffi::CStr::from_ptr(__result) }}.to_string_lossy().into_owned();\n        unsafe {{ {free_symbol}(__result as *mut std::ffi::c_void) }};\n        __text\n    }}"
                )
            }
            Type::Handle => format!("    crate::OrchHandle::new({call}, {})", release.as_deref().unwrap_or("__ffi_drop")),
            Type::Array(inner, _) => {
                returns_string = true;
                let inner = ffi_type(inner, true);
                format!(
                    "    let __result = {call};\n    if __result.is_null() || __count <= 0 {{ Vec::new() }} else {{\n        let __items = unsafe {{ std::slice::from_raw_parts(__result as *const {inner}, __count as usize) }}.to_vec();\n        unsafe {{ {free_symbol}(__result as *mut std::ffi::c_void) }};\n        __items\n    }}"
                )
            }
            _ => format!("    {call}"),
        };
        wrappers.push_str(&format!("pub fn {}({}){} {{\n{}{}\n}}\n", rust_ident(&signature.name), wrapper_params, wrapper_ret, prologue, body));
    }

    if returns_string {
        extern_c.push_str(&format!("    #[link_name = \"free\"]\n    fn {}(p: *mut std::ffi::c_void);\n", free_symbol));
    }
    extern_c.push_str("}\n\n");
    Ok(format!("{}{}", extern_c, wrappers))
}

/// A sidecar type, with any `[]` suffixes that follow it consumed from the token stream.
fn c_type_at(
    name: &str,
    tokens: &[crate::lexer::Token],
    pos: &mut usize,
    file_name: &str,
    line: usize,
) -> Result<Type, String> {
    let mut ty = c_type(name, file_name, line)?;
    while matches!(tokens.get(*pos).map(|t| &t.kind), Some(TokenKind::LBracket)) {
        match tokens.get(*pos + 1).map(|t| &t.kind) {
            Some(TokenKind::RBracket) => {}
            _ => return Err(format!("Error in {} at line {}: expected ']' after '[' in a type", file_name, line)),
        }
        *pos += 2;
        if !matches!(ty, Type::Int | Type::Float | Type::Bool) {
            return Err(format!(
                "Error in {} at line {}: an array across the C ABI carries int, float, or bool; '{}' does not cross as an array",
                file_name, line, ty.display_name()
            ));
        }
        ty = Type::Array(Box::new(ty), Vec::new());
    }
    Ok(ty)
}

fn c_type(name: &str, file_name: &str, line: usize) -> Result<Type, String> {
    match name {
        "int" => Ok(Type::Int),
        "float" => Ok(Type::Float),
        "bool" => Ok(Type::Bool),
        "void" => Ok(Type::Void),
        "string" => Ok(Type::Str),
        "handle" => Ok(Type::Handle),
        // A capitalised name is a struct declared in the program; the generated struct is
        // `#[repr(C)]`, so it has the layout the foreign side sees.
        other if other.starts_with(|c: char| c.is_ascii_uppercase()) => Ok(Type::Named(other.to_string())),
        _ => Err(format!("Error in {} at line {}: unknown type '{}'", file_name, line, name)),
    }
}

/// The type in the `extern "C"` declaration.
fn ffi_type(ty: &Type, returning: bool) -> String {
    match ty {
        Type::Int => "i64".into(),
        Type::Float => "f64".into(),
        Type::Bool => "bool".into(),
        Type::Void => "()".into(),
        Type::Str => if returning { "*mut std::os::raw::c_char".into() } else { "*const std::os::raw::c_char".into() },
        Type::Handle => "*mut std::ffi::c_void".into(),
        // A struct is declared in the program's entry file, so a module's bindings reach
        // it through the crate root, the way handles do.
        Type::Named(name) => format!("crate::{}", rust_ident(name)),
        other => other.display_name(),
    }
}

/// The type OrchestrateLang code sees on the safe wrapper.
fn wrapper_type(ty: &Type, returning: bool) -> String {
    match ty {
        Type::Str => "String".into(),
        Type::Handle => if returning { "crate::OrchHandle".into() } else { "&crate::OrchHandle".into() },
        Type::Array(inner, _) => format!("Vec<{}>", ffi_type(inner, returning)),
        other => ffi_type(other, returning),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_signatures() {
        let ffi = "
            circle_area(radius: float) -> float
            hypotenuse(a: int, b: int) -> int
            do_nothing()
        ";
        let res = parse_ffi_and_generate_bindings(ffi, "c", "test.orch_ffi").unwrap();
        assert!(res.contains("fn __ffi_circle_area(radius: f64) -> f64;"));
        assert!(res.contains("fn __ffi_hypotenuse(a: i64, b: i64) -> i64;"));
        assert!(res.contains("fn __ffi_do_nothing();"));

        assert!(res.contains("pub fn circle_area(radius: f64) -> f64 {"));
        assert!(res.contains("unsafe { __ffi_circle_area(radius) }"));
    }

    #[test]
    fn test_missing_colon() {
        let ffi = "circle_area(radius float) -> float";
        let err = parse_ffi_and_generate_bindings(ffi, "c", "test.orch_ffi").unwrap_err();
        assert_eq!(err, "Error in test.orch_ffi at line 1: expected ':' after parameter name 'radius', found Identifier(\"float\")");
    }

    #[test]
    fn test_string_signatures_marshal_c_strings() {
        let res = parse_ffi_and_generate_bindings("greet(name: string) -> string\nlength(s: string) -> int", "c", "text.orch_ffi").unwrap();
        assert!(res.contains("fn __ffi_greet(name: *const std::os::raw::c_char) -> *mut std::os::raw::c_char;"));
        assert!(res.contains("pub fn greet(name: String) -> String {"));
        assert!(res.contains("std::ffi::CString::new(name.replace('\\0', \"\"))"));
        assert!(res.contains("std::ffi::CStr::from_ptr(__result)"));
        assert!(res.contains("__ffi_free_text(__result as *mut std::ffi::c_void)"));
        assert!(res.contains("#[link_name = \"free\"]"));
        assert!(res.contains("pub fn length(s: String) -> i64 {"));
    }

    #[test]
    fn test_handle_signatures_release_through_drop() {
        // `start` is a keyword, so a parameter cannot be called that; `initial` will do.
        let ffi = "make(initial: int) -> handle\nbump(c: handle) -> int\ndrop release(c: handle)\n";
        let signatures = parse_ffi(ffi, "zig", "counter.orch_ffi").unwrap();
        assert!(signatures[2].drop);
        assert_eq!(signatures[1].handle_params(), vec![true]);
        assert!(signatures[0].mentions_handle());
        let res = generate_bindings(&signatures, "counter.orch_ffi").unwrap();
        assert!(res.contains("fn __ffi_make(initial: i64) -> *mut std::ffi::c_void;"));
        assert!(res.contains("pub fn make(initial: i64) -> crate::OrchHandle {"));
        assert!(res.contains("crate::OrchHandle::new(unsafe { __ffi_make(initial) }, __ffi_release)"));
        assert!(res.contains("pub fn bump(c: &crate::OrchHandle) -> i64 {"));
        assert!(res.contains("unsafe { __ffi_bump(c.ptr()) }"));
        assert!(!res.contains("pub fn release("), "drop functions are not callable");
        let err = parse_ffi("make() -> handle\n", "c", "counter.orch_ffi").unwrap_err();
        assert!(err.contains("declares no `drop"), "{err}");
        let err = parse_ffi("drop release(c: int)\n", "c", "counter.orch_ffi").unwrap_err();
        assert!(err.contains("must take one handle"), "{err}");
        let err = parse_ffi("drop a(c: handle)\ndrop b(c: handle)\n", "c", "counter.orch_ffi").unwrap_err();
        assert!(err.contains("only one `drop`"), "{err}");
    }

    #[test]
    fn test_trailing_comma() {
        let ffi = "func(a: int,) -> void";
        let err = parse_ffi_and_generate_bindings(ffi, "c", "test.orch_ffi").unwrap_err();
        assert_eq!(err, "Error in test.orch_ffi at line 1: expected parameter name, found RParen");
    }

    #[test]
    fn test_unknown_type() {
        let ffi = "func(a: custom) -> void";
        let err = parse_ffi_and_generate_bindings(ffi, "c", "test.orch_ffi").unwrap_err();
        assert_eq!(err, "Error in test.orch_ffi at line 1: unknown type 'custom'");
    }

    #[test]
    fn test_missing_paren() {
        let ffi = "func(a: int -> void";
        let err = parse_ffi_and_generate_bindings(ffi, "c", "test.orch_ffi").unwrap_err();
        assert_eq!(err, "Error in test.orch_ffi at line 1: expected ',' or ')', found Arrow");
    }
}
