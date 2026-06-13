use crate::ast;
use crate::typechecker::TypeChecker;

pub fn extract_and_register_rust_functions(code: &str, alias: &str, file_name: &str, tc: &mut TypeChecker) {
    for line in code.lines() {
        let trimmed_line = line.trim();
        if trimmed_line.starts_with("///") || trimmed_line.starts_with("//!") || trimmed_line.starts_with("//") {
            continue;
        }

        if trimmed_line.starts_with("pub fn ") {
            let mut warning = None;
            
            if let Some(open_paren) = trimmed_line.find('(') {
                let name_part = trimmed_line[7..open_paren].trim();
                let name = if let Some(bracket) = name_part.find('<') {
                    warning = Some(format!("uses generics, which are not supported by load_foreign \"rust\""));
                    name_part[..bracket].trim()
                } else {
                    name_part
                };

                if warning.is_none() && trimmed_line.contains(" where ") {
                    warning = Some(format!("has a where clause, which is not supported"));
                }

                if warning.is_none() {
                    if let Some(close_paren) = trimmed_line.rfind(')') {
                        let args_str = trimmed_line[open_paren+1..close_paren].trim();
                        let mut orch_args = Vec::new();
                        
                        if !args_str.is_empty() {
                            for arg in args_str.split(',') {
                                let parts: Vec<&str> = arg.splitn(2, ':').collect();
                                if parts.len() == 2 {
                                    let arg_name = parts[0].trim();
                                    let rust_ty = parts[1].trim();
                                    let orch_ty = match rust_ty {
                                        "i64" => ast::Type::Int,
                                        "f64" => ast::Type::Float,
                                        "bool" => ast::Type::Bool,
                                        "String" | "&str" | "&String" => ast::Type::Str,
                                        _ => {
                                            warning = Some(format!("has unsupported parameter type '{}' for parameter '{}'", rust_ty, arg_name));
                                            ast::Type::Void // dummy
                                        }
                                    };
                                    if warning.is_some() { break; }
                                    orch_args.push(orch_ty);
                                } else {
                                    warning = Some(format!("has malformed parameter '{}'", arg));
                                    break;
                                }
                            }
                        }

                        if warning.is_none() {
                            let ret_str = if let Some(arrow_idx) = trimmed_line.find("->") {
                                let ret_part = trimmed_line[arrow_idx+2..].trim();
                                let ret_part = if let Some(brace_idx) = ret_part.find('{') {
                                    ret_part[..brace_idx].trim()
                                } else {
                                    ret_part
                                };
                                ret_part.trim()
                            } else {
                                "()"
                            };

                            let orch_ret = match ret_str {
                                "i64" => ast::Type::Int,
                                "f64" => ast::Type::Float,
                                "bool" => ast::Type::Bool,
                                "String" | "&str" => ast::Type::Str,
                                "()" | "" => ast::Type::Void,
                                _ => {
                                    warning = Some(format!("has unsupported return type '{}'", ret_str));
                                    ast::Type::Void
                                }
                            };

                            if warning.is_none() {
                                tc.register_foreign_function(alias, name, orch_args, orch_ret);
                                continue;
                            }
                        }
                    } else {
                        warning = Some(format!("is missing a closing parenthesis"));
                    }
                }
                
                if let Some(msg) = warning {
                    eprintln!("[orchestrate] warning: foreign Rust function '{}' in {} has an unsupported signature and was not registered — calls to it may produce confusing type errors. Supported parameter/return types: int, float, bool, string (mapped from i64, f64, bool, String/&str).\n  Reason: {}", name, file_name, msg);
                }
            } else {
                eprintln!("[orchestrate] warning: foreign Rust function line in {} lacks '(' and was not registered.\n  Line: {}", file_name, trimmed_line);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_ffi_warnings() {
        let code = r#"
            /// Here is a doc comment
            /// pub fn example() {}
            pub fn valid_func(a: i64) -> f64 { 0.0 }
            pub fn generic_func<T>(a: T) {}
            pub fn result_func() -> Result<f64, String> {}
            pub fn wrong_param(a: std::path::PathBuf) {}
        "#;
        let mut tc = TypeChecker::new();
        extract_and_register_rust_functions(code, "test", "test.rs", &mut tc);
        // Valid should be present
        assert!(tc.has_function("test::valid_func"));
        // Others should not
        assert!(!tc.has_function("test::generic_func"));
        assert!(!tc.has_function("test::result_func"));
        assert!(!tc.has_function("test::wrong_param"));
    }
}
