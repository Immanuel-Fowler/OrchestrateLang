use crate::lexer::{Lexer, TokenKind};
use crate::ast;
use crate::typechecker::TypeChecker;

/// Reads a `.orch_ffi` sidecar file and registers the declared Rust functions
/// with the type checker. The sidecar uses the same syntax as C/C++ sidecars:
///
///   add(a: int, b: int) -> int
///   greet(name: string) -> void
///   reverse<T>(items: T[]) -> T[]
///
/// Supported types: int, float, bool, void, string, option, result, arrays, and the
/// function's own type parameters. A generic signature registers as a generic function,
/// so a call infers `T` from its arguments and the Rust implementation infers it too.
/// The corresponding Rust file is included verbatim in the generated module (handled by driver.rs).
pub fn register_rust_ffi_from_sidecar(
    sidecar_content: &str,
    alias: &str,
    sidecar_file_name: &str,
    tc: &mut TypeChecker,
) -> Result<(), String> {
    let mut lexer = Lexer::new(sidecar_content);
    let tokens = lexer.tokenize()
        .map_err(|e| format!("Error in {}: {}", sidecar_file_name, e))?;

    let mut pos = 0;
    while pos < tokens.len() && tokens[pos].kind != TokenKind::EOF {
        let fn_name = match &tokens[pos].kind {
            TokenKind::Identifier(n) => n.clone(),
            _ => return Err(format!("Error in {} at line {}: expected function name, found {:?}",
                sidecar_file_name, tokens[pos].line, tokens[pos].kind)),
        };
        pos += 1;

        let mut type_params: Vec<String> = Vec::new();
        if tokens.get(pos).map(|t| &t.kind) == Some(&TokenKind::Lt) {
            pos += 1;
            loop {
                match tokens.get(pos).map(|t| &t.kind) {
                    Some(TokenKind::Identifier(n)) => { type_params.push(n.clone()); pos += 1; }
                    _ => return Err(format!("Error in {} at line {}: expected a type parameter name after '<'",
                        sidecar_file_name, tokens.get(pos).map(|t| t.line).unwrap_or(0))),
                }
                match tokens.get(pos).map(|t| &t.kind) {
                    Some(TokenKind::Comma) => pos += 1,
                    Some(TokenKind::Gt) => { pos += 1; break; }
                    _ => return Err(format!("Error in {} at line {}: expected ',' or '>' after type parameter",
                        sidecar_file_name, tokens.get(pos).map(|t| t.line).unwrap_or(0))),
                }
            }
        }

        if tokens.get(pos).map(|t| &t.kind) != Some(&TokenKind::LParen) {
            let line = tokens.get(pos).map(|t| t.line).unwrap_or(0);
            return Err(format!("Error in {} at line {}: expected '(' after '{}'",
                sidecar_file_name, line, fn_name));
        }
        pos += 1;

        let mut param_types: Vec<ast::Type> = Vec::new();
        while tokens.get(pos).map(|t| &t.kind) != Some(&TokenKind::RParen)
            && tokens.get(pos).map(|t| &t.kind) != Some(&TokenKind::EOF)
        {
            // param name
            let _param_name = match tokens.get(pos).map(|t| &t.kind) {
                Some(TokenKind::Identifier(n)) => n.clone(),
                _ => return Err(format!("Error in {} at line {}: expected parameter name",
                    sidecar_file_name, tokens.get(pos).map(|t| t.line).unwrap_or(0))),
            };
            pos += 1;

            if tokens.get(pos).map(|t| &t.kind) != Some(&TokenKind::Colon) {
                return Err(format!("Error in {} at line {}: expected ':' after parameter name",
                    sidecar_file_name, tokens.get(pos).map(|t| t.line).unwrap_or(0)));
            }
            pos += 1;

            let orch_ty = parse_sidecar_type(&tokens, &mut pos, sidecar_file_name, &type_params)?;
            param_types.push(orch_ty);

            match tokens.get(pos).map(|t| &t.kind) {
                Some(TokenKind::Comma) => { pos += 1; }
                Some(TokenKind::RParen) => {}
                _ => return Err(format!("Error in {} at line {}: expected ',' or ')'",
                    sidecar_file_name, tokens.get(pos).map(|t| t.line).unwrap_or(0))),
            }
        }

        // consume ')'
        pos += 1;

        let ret_ty = if tokens.get(pos).map(|t| &t.kind) == Some(&TokenKind::Arrow) {
            pos += 1;
            parse_sidecar_type(&tokens, &mut pos, sidecar_file_name, &type_params)?
        } else {
            ast::Type::Void
        };

        if type_params.is_empty() {
            tc.register_foreign_function(alias, &fn_name, param_types, ret_ty);
        } else {
            tc.register_generic_foreign_function(alias, &fn_name, type_params, param_types, ret_ty);
        }
    }

    Ok(())
}

/// `type_params` are the enclosing signature's own parameters, which read as type
/// variables rather than as unknown type names.
pub(crate) fn parse_sidecar_type(tokens: &[crate::lexer::Token], pos: &mut usize, file: &str, type_params: &[String]) -> Result<ast::Type, String> {
    let token = tokens.get(*pos).ok_or_else(|| format!("Error in {}: expected type", file))?;
    let name = match &token.kind { TokenKind::Identifier(name) => name, _ => return Err(format!("Error in {}: expected type", file)) };
    *pos += 1;
    let mut ty = if name == "option" || name == "result" {
        if tokens.get(*pos).map(|t| &t.kind) != Some(&TokenKind::Lt) { return Err(format!("Error in {}: expected '<'", file)); }
        *pos += 1;
        let inner = parse_sidecar_type(tokens, pos, file, type_params)?;
        let error = if name == "result" && tokens.get(*pos).map(|t| &t.kind) == Some(&TokenKind::Comma) {
            *pos += 1;
            parse_sidecar_type(tokens, pos, file, type_params)?
        } else {
            ast::Type::Str
        };
        if tokens.get(*pos).map(|t| &t.kind) != Some(&TokenKind::Gt) { return Err(format!("Error in {}: expected '>'", file)); }
        *pos += 1;
        if name == "option" { ast::Type::Option(Box::new(inner)) } else { ast::Type::Result(Box::new(inner), Box::new(error)) }
    } else if type_params.iter().any(|p| p == name) {
        ast::Type::TypeParam(name.clone())
    } else { sidecar_type_to_orch(name, file, token.line)? };
    while tokens.get(*pos).map(|t| &t.kind) == Some(&TokenKind::LBracket) {
        *pos += 1;
        if tokens.get(*pos).map(|t| &t.kind) != Some(&TokenKind::RBracket) { return Err(format!("Error in {}: expected ']'", file)); }
        *pos += 1;
        ty = ast::Type::Array(Box::new(ty), Vec::new());
    }
    Ok(ty)
}

fn sidecar_type_to_orch(ty: &str, file_name: &str, line: usize) -> Result<ast::Type, String> {
    match ty {
        "int" => Ok(ast::Type::Int),
        "float" => Ok(ast::Type::Float),
        "bool" => Ok(ast::Type::Bool),
        "void" => Ok(ast::Type::Void),
        "string" => Ok(ast::Type::Str),
        _ => Err(format!("Error in {} at line {}: unknown type '{}'", file_name, line, ty)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_ffi_sidecar_registers_functions() {
        let sidecar = "
            add(a: int, b: int) -> int
            greet(name: string) -> void
            scale(x: float) -> float
        ";
        let mut tc = TypeChecker::new();
        register_rust_ffi_from_sidecar(sidecar, "mylib", "mylib.orch_ffi", &mut tc).unwrap();
        assert!(tc.has_function("mylib::add"));
        assert!(tc.has_function("mylib::greet"));
        assert!(tc.has_function("mylib::scale"));
    }

    #[test]
    fn test_rust_ffi_sidecar_generic_signature_infers_at_the_call() {
        use crate::{lexer::Lexer, parser::Parser};
        let sidecar = "reverse<T>(items: T[]) -> T[]\nhead<T>(items: T[]) -> option<T>\n";
        let mut tc = TypeChecker::new();
        register_rust_ffi_from_sidecar(sidecar, "lists", "impl.orch_ffi", &mut tc).unwrap();
        assert!(tc.has_function("lists::reverse"));
        let check = |src: &str, tc: &mut TypeChecker| {
            let ast = Parser::new(Lexer::new(src).tokenize().unwrap()).parse().unwrap();
            tc.type_check(&ast)
        };
        assert!(check("let names: string[] = lists.reverse([\"b\", \"a\"])", &mut tc).is_ok());
        assert!(check("let first: option<float> = lists.head([1.5])", &mut tc).is_ok());
        let error = check("let wrong: int[] = lists.reverse([\"b\"])", &mut tc).unwrap_err();
        assert!(error.contains("type mismatch"), "{error}");
        let error = register_rust_ffi_from_sidecar("bad<T(x: T) -> T", "l", "l.orch_ffi", &mut tc).unwrap_err();
        assert!(error.contains("expected ',' or '>'"), "{error}");
    }

    #[test]
    fn test_rust_ffi_sidecar_unknown_type() {
        let sidecar = "bad_fn(a: custom_type) -> void";
        let mut tc = TypeChecker::new();
        let err = register_rust_ffi_from_sidecar(sidecar, "lib", "lib.orch_ffi", &mut tc);
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("unknown type"));
    }
}
