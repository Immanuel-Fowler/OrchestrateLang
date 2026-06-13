use crate::lexer::{Lexer, TokenKind};

pub fn parse_ffi_and_generate_bindings(
    ffi_content: &str,
    language: &str,
    file_name: &str,
) -> Result<String, String> {
    let mut lexer = Lexer::new(ffi_content);
    let tokens = lexer.tokenize().map_err(|e| format!("Error in {} at {}", file_name, e))?;

    let mut extern_c = String::from("extern \"C\" {\n");
    let mut wrappers = String::new();

    let mut pos = 0;

    while pos < tokens.len() && tokens[pos].kind != TokenKind::EOF {
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

        let mut args = Vec::new();
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
            
            if arg_type_str == "string" {
                return Err(format!("load_foreign '{}': string type is not supported in C/C++ FFI signatures", language));
            }
            
            let rust_type = orch_type_to_rust_ffi(&arg_type_str, file_name, tok.line)?;
            args.push((arg_name, rust_type));
            pos += 1;

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

        let mut rust_ret = "()".to_string();
        tok = tokens.get(pos).unwrap_or_else(|| &tokens[tokens.len()-1]);
        if tok.kind == TokenKind::Arrow {
            pos += 1;
            tok = tokens.get(pos).unwrap_or_else(|| &tokens[tokens.len()-1]);
            let ret_type_str = match &tok.kind {
                TokenKind::Identifier(n) => n.clone(),
                _ => return Err(format!("Error in {} at line {}: expected return type, found {:?}", file_name, tok.line, tok.kind)),
            };
            
            if ret_type_str == "string" {
                return Err(format!("load_foreign '{}': string type is not supported in C/C++ FFI signatures", language));
            }
            
            rust_ret = orch_type_to_rust_ffi(&ret_type_str, file_name, tok.line)?;
            pos += 1;
        }

        let mut rust_args_decl = Vec::new();
        let mut call_args = Vec::new();
        for (a_name, a_type) in args {
            rust_args_decl.push(format!("{}: {}", a_name, a_type));
            call_args.push(a_name);
        }

        let args_decl_str = rust_args_decl.join(", ");
        let call_decl_str = call_args.join(", ");
        let ret_decl_str = if rust_ret == "()" { String::new() } else { format!(" -> {}", rust_ret) };

        extern_c.push_str(&format!("    #[link_name = \"{}\"]\n    fn __ffi_{}({}){};\n", fn_name, fn_name, args_decl_str, ret_decl_str));
        wrappers.push_str(&format!("pub fn {}({}){} {{\n    unsafe {{ __ffi_{}({}) }}\n}}\n", fn_name, args_decl_str, ret_decl_str, fn_name, call_decl_str));
    }

    extern_c.push_str("}\n\n");
    Ok(format!("{}{}", extern_c, wrappers))
}

fn orch_type_to_rust_ffi(orch_type: &str, file_name: &str, line: usize) -> Result<String, String> {
    match orch_type {
        "int" => Ok("i64".to_string()),
        "float" => Ok("f64".to_string()),
        "bool" => Ok("bool".to_string()),
        "void" => Ok("()".to_string()),
        _ => Err(format!("Error in {} at line {}: unknown type '{}'", file_name, line, orch_type)),
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
    fn test_string_type_rejected() {
        let ffi = "greet(name: string) -> void";
        let err = parse_ffi_and_generate_bindings(ffi, "c", "test.orch_ffi").unwrap_err();
        assert_eq!(err, "load_foreign 'c': string type is not supported in C/C++ FFI signatures");
        
        let ffi2 = "get_name() -> string";
        let err2 = parse_ffi_and_generate_bindings(ffi2, "c", "test.orch_ffi").unwrap_err();
        assert_eq!(err2, "load_foreign 'c': string type is not supported in C/C++ FFI signatures");
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
