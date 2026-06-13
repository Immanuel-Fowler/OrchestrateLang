use crate::lexer::{Lexer, TokenKind};
use crate::ast;
use crate::typechecker::TypeChecker;

/// Reads a `.orch_ffi` sidecar file and registers the declared Rust functions
/// with the type checker. The sidecar uses the same syntax as C/C++ sidecars:
///
///   add(a: int, b: int) -> int
///   greet(name: string) -> void
///
/// Supported types: int, float, bool, void, string.
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

            let (ty_str, line) = match tokens.get(pos) {
                Some(t) => match &t.kind {
                    TokenKind::Identifier(s) => (s.clone(), t.line),
                    _ => return Err(format!("Error in {} at line {}: expected parameter type, found {:?}",
                        sidecar_file_name, t.line, t.kind)),
                },
                None => return Err(format!("Error in {}: unexpected end of file", sidecar_file_name)),
            };
            pos += 1;

            let orch_ty = sidecar_type_to_orch(&ty_str, sidecar_file_name, line)?;
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
            let (ty_str, line) = match tokens.get(pos) {
                Some(t) => match &t.kind {
                    TokenKind::Identifier(s) => (s.clone(), t.line),
                    _ => return Err(format!("Error in {} at line {}: expected return type, found {:?}",
                        sidecar_file_name, t.line, t.kind)),
                },
                None => return Err(format!("Error in {}: unexpected end of file after '->'", sidecar_file_name)),
            };
            pos += 1;
            sidecar_type_to_orch(&ty_str, sidecar_file_name, line)?
        } else {
            ast::Type::Void
        };

        tc.register_foreign_function(alias, &fn_name, param_types, ret_ty);
    }

    Ok(())
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
    fn test_rust_ffi_sidecar_unknown_type() {
        let sidecar = "bad_fn(a: custom_type) -> void";
        let mut tc = TypeChecker::new();
        let err = register_rust_ffi_from_sidecar(sidecar, "lib", "lib.orch_ffi", &mut tc);
        assert!(err.is_err());
        assert!(err.unwrap_err().contains("unknown type"));
    }
}
