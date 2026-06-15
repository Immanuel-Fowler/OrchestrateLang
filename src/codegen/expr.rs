use crate::ast::{ExprNode, MatchPattern, StmtNode, BinaryOp, Expr, Literal, StringPart, Type};
use std::collections::HashSet;
use super::core::Codegen;

impl Codegen {
    pub fn compile_expr(&mut self, expr: &Expr) -> String {
        match &expr.node {
            ExprNode::Literal(lit) => match lit {
                Literal::Int(v) => v.to_string(),
                Literal::Float(v) => format!("{:?}", v),
                Literal::Str(v) => format!("String::from({:?})", v),
                Literal::Bool(v) => v.to_string(),
            },
            ExprNode::Identifier(name) => name.clone(),
            ExprNode::Binary { op, lhs, rhs } => {
                let lhs_str = self.compile_expr(lhs);
                let rhs_str = self.compile_expr(rhs);
                if *op == BinaryOp::Add {
                    format!("OrchAdd::orch_add({}, {})", lhs_str, rhs_str)
                } else if *op == BinaryOp::Assign {
                    format!("{} = {}", lhs_str, rhs_str)
                } else {
                    let op_str = match op {
                        BinaryOp::Sub => "-",
                        BinaryOp::Mul => "*",
                        BinaryOp::Div => "/",
                        BinaryOp::Eq => "==",
                        BinaryOp::Ne => "!=",
                        BinaryOp::Lt => "<",
                        BinaryOp::Gt => ">",
                        BinaryOp::Le => "<=",
                        BinaryOp::Ge => ">=",
                        BinaryOp::And => "&&",
                        BinaryOp::Or => "||",
                        BinaryOp::Assign | BinaryOp::Add => unreachable!(),
                    };
                    format!("({} {} {})", lhs_str, op_str, rhs_str)
                }
            }
            ExprNode::Call { callee, args } => {
                let args_str = args.iter().map(|a| self.compile_expr(a)).collect::<Vec<String>>().join(", ");

                if callee == "print" {
                    format!("print_val({})", args_str)
                } else if callee == "length" && args.len() == 1 {
                    format!("{}.len() as i64", self.compile_expr(&args[0]))
                } else if callee == "append" && args.len() == 2 {
                    format!("{}.push({})", self.compile_expr(&args[0]), self.compile_expr(&args[1]))
                } else if callee == "remove" && args.len() == 2 {
                    format!("{}.remove({} as usize)", self.compile_expr(&args[0]), self.compile_expr(&args[1]))
                } else if callee == "range" && args.len() == 1 {
                    let n = self.compile_expr(&args[0]);
                    format!("(0i64..{} as i64).collect::<Vec<i64>>()", n)
                } else if callee == "range" && args.len() == 2 {
                    let s = self.compile_expr(&args[0]);
                    let e = self.compile_expr(&args[1]);
                    format!("({} as i64..{} as i64).collect::<Vec<i64>>()", s, e)
                } else if callee == "map" && args.len() == 2 {
                    let xs = self.compile_expr(&args[0]);
                    let f = self.compile_expr(&args[1]);
                    format!("{{ let mut __f = ({}); ({}).clone().into_iter().map(|__x| __f(__x)).collect::<Vec<_>>() }}", f, xs)
                } else if callee == "filter" && args.len() == 2 {
                    let xs = self.compile_expr(&args[0]);
                    let f = self.compile_expr(&args[1]);
                    format!("{{ let mut __f = ({}); ({}).clone().into_iter().filter(|__x| __f((*__x).clone())).collect::<Vec<_>>() }}", f, xs)
                } else if callee == "reduce" && args.len() == 3 {
                    let xs = self.compile_expr(&args[0]);
                    let init = self.compile_expr(&args[1]);
                    let f = self.compile_expr(&args[2]);
                    format!("{{ let mut __f = ({}); ({}).clone().into_iter().fold({}, |acc, __x| __f(acc, __x)) }}", f, xs, init)
                } else if callee == "find" && args.len() == 2 {
                    let xs = self.compile_expr(&args[0]);
                    let f = self.compile_expr(&args[1]);
                    format!("{{ let mut __f = ({}); ({}).clone().into_iter().find(|__x| __f((*__x).clone())) }}", f, xs)
                } else if callee == "any" && args.len() == 2 {
                    let xs = self.compile_expr(&args[0]);
                    let f = self.compile_expr(&args[1]);
                    format!("{{ let mut __f = ({}); ({}).clone().into_iter().any(|__x| __f(__x)) }}", f, xs)
                } else if callee == "all" && args.len() == 2 {
                    let xs = self.compile_expr(&args[0]);
                    let f = self.compile_expr(&args[1]);
                    format!("{{ let mut __f = ({}); ({}).clone().into_iter().all(|__x| __f(__x)) }}", f, xs)
                } else if callee == "to_int" && args.len() == 1 {
                    format!("({}) as i64", self.compile_expr(&args[0]))
                } else if callee == "to_float" && args.len() == 1 {
                    format!("({}) as f64", self.compile_expr(&args[0]))
                } else if callee == "parse_int" && args.len() == 1 {
                    format!("({}).parse::<i64>().map_err(|e| e.to_string())", self.compile_expr(&args[0]))
                } else if callee == "parse_float" && args.len() == 1 {
                    format!("({}).parse::<f64>().map_err(|e| e.to_string())", self.compile_expr(&args[0]))
                } else if callee == "sleep" {
                    if self.in_parallel {
                        format!("tokio::time::sleep(std::time::Duration::from_millis({} as u64))", args_str)
                    } else {
                        format!("tokio::time::sleep(std::time::Duration::from_millis({} as u64)).await", args_str)
                    }
                } else if self.tasks.contains(callee) && !self.in_parallel {
                    format!("{}({}).await", callee, args_str)
                } else {
                    format!("{}({})", callee, args_str)
                }
            }
            ExprNode::Pipeline { value, function } => {
                let val_str = self.compile_expr(value);
                match &function.node {
                    ExprNode::Identifier(name) => {
                        if name == "print" {
                            format!("print_val({})", val_str)
                        } else if name == "sleep" {
                            if self.in_parallel {
                                format!("tokio::time::sleep(std::time::Duration::from_millis({} as u64))", val_str)
                            } else {
                                format!("tokio::time::sleep(std::time::Duration::from_millis({} as u64)).await", val_str)
                            }
                        } else if self.tasks.contains(name) && !self.in_parallel {
                            format!("{}({}).await", name, val_str)
                        } else {
                            format!("{}({})", name, val_str)
                        }
                    }
                    ExprNode::Call { callee, args } => {
                        let mut all_args = vec![val_str];
                        for arg in args {
                            all_args.push(self.compile_expr(arg));
                        }
                        let all_args_str = all_args.join(", ");
                        if callee == "print" {
                            format!("print_val({})", all_args_str)
                        } else if callee == "length" && all_args.len() == 1 {
                            format!("{}.len() as i64", all_args[0])
                        } else if callee == "append" && all_args.len() == 2 {
                            format!("{}.push({})", all_args[0], all_args[1])
                        } else if callee == "remove" && all_args.len() == 2 {
                            format!("{}.remove({} as usize)", all_args[0], all_args[1])
                        } else if callee == "sleep" {
                            if self.in_parallel {
                                format!("tokio::time::sleep(std::time::Duration::from_millis({} as u64))", all_args_str)
                            } else {
                                format!("tokio::time::sleep(std::time::Duration::from_millis({} as u64)).await", all_args_str)
                            }
                        } else if self.tasks.contains(callee) && !self.in_parallel {
                            format!("{}({}).await", callee, all_args_str)
                        } else {
                            format!("{}({})", callee, all_args_str)
                        }
                    }
                    _ => panic!("Invalid pipeline function target: {:?}", function),
                }
            }
            ExprNode::Block(_) => {
                let inner = self.compile_block_inner(expr, false);
                format!("{{\n    {}\n}}", inner.replace("\n", "\n    "))
            }
            ExprNode::If { cond, then_branch, else_branch } => {
                let cond_str = self.compile_expr(cond);
                let then_str = self.compile_expr(then_branch);
                if let Some(else_b) = else_branch {
                    let else_str = self.compile_expr(else_b);
                    format!("if {} {} else {}", cond_str, then_str, else_str)
                } else {
                    format!("if {} {}", cond_str, then_str)
                }
            }
            ExprNode::ModuleCall { module_local_name, function, args } => {
                let args_str = args.iter().map(|a| self.compile_expr(a)).collect::<Vec<String>>().join(", ");

                if self.modules.contains(module_local_name) {
                    let full_name = format!("{}::{}", module_local_name, function);
                    if self.tasks.contains(&full_name) && !self.in_parallel {
                        format!("{}::{}({}).await", module_local_name, function, args_str)
                    } else {
                        format!("{}::{}({})", module_local_name, function, args_str)
                    }
                } else if self.in_parallel {
                    format!("{}.{}({})", module_local_name, function, args_str)
                } else {
                    format!("{}.{}({}).await", module_local_name, function, args_str)
                }
            }
            ExprNode::StartServerlet { name, args } => {
                let start_fn = if name.contains("::") {
                    let parts: Vec<&str> = name.split("::").collect();
                    format!("{}::start_{}", parts[0], parts[1])
                } else {
                    format!("start_{}", name)
                };
                let args_str = args.iter().map(|a| self.compile_expr(a)).collect::<Vec<String>>().join(", ");
                format!("{}({})", start_fn, args_str)
            }
            ExprNode::AutomaticBlock { body, restart_policy, crash_handler } => {
                let mut free_vars = HashSet::new();
                self.get_free_vars_expr(expr, &mut HashSet::new(), &mut free_vars);

                let mut capture_code = String::new();
                for var in &free_vars {
                    capture_code.push_str(&format!("let {} = {}.clone();\n    ", var, var));
                }

                let crash_handler_code = if let Some((err_name, handler)) = crash_handler {
                    let handler_str = self.compile_expr(handler);
                    format!(
                        r#"Err(__e) => {{
                    let {} = format!("{{:?}}", __e);
                    {};
                    let __delay_ms = (1000u64 * 2u64.saturating_pow(__restart_count.min(5))).min(30000);
                    tokio::time::sleep(std::time::Duration::from_millis(__delay_ms)).await;
                }}"#,
                        err_name, handler_str
                    )
                } else {
                    r#"Err(__e) => {
                    eprintln!("[orchestrate] automatic block panicked (restart #{}): {:?}", __restart_count, __e);
                    let __delay_ms = (1000u64 * 2u64.saturating_pow(__restart_count.min(5))).min(30000);
                    tokio::time::sleep(std::time::Duration::from_millis(__delay_ms)).await;
                }"#.to_string()
                };

                // Generate the body
                let (setup_code, loop_code) = if let ExprNode::Block(stmts) = &body.node {
                    let mut setup = Vec::new();
                    let mut loop_c = Vec::new();
                    for stmt in stmts {
                        let is_serverlet_start = matches!(&stmt.node, StmtNode::Let { value, .. } if matches!(value.node, ExprNode::StartServerlet { .. }));
                        let compiled = self.compile_stmt(stmt);
                        let compiled = if compiled.ends_with(';') || compiled.ends_with('}') { compiled } else { format!("{};", compiled) };
                        if is_serverlet_start { setup.push(compiled); } else { loop_c.push(compiled); }
                    }
                    (setup, loop_c)
                } else {
                    (vec![], vec![self.compile_expr(body)])
                };

                let setup_inner = setup_code.join("\n                ");
                let loop_inner = loop_code.join("\n                    ");

                let restart_policy_str = match restart_policy {
                    crate::ast::RestartPolicy::Always => "true".to_string(),
                    crate::ast::RestartPolicy::Never => "false".to_string(),
                    crate::ast::RestartPolicy::MaxAttempts(n) => format!("__restart_count < {}", n),
                };

                let setup_block = if setup_code.is_empty() {
                    String::new()
                } else {
                    format!("// Serverlet setup — once per process lifetime\n                {}\n                ", setup_inner)
                };

                format!(
                    r#"{{
    {capture}std::sync::Arc::new(move || {{
        tokio::spawn(async move {{
            let mut __restart_count: u32 = 0;
            loop {{
                {setup}let __inner_handle = tokio::spawn(async move {{
                    loop {{
                        {loop_body}
                    }}
                }});
                match __inner_handle.await {{
                    Ok(_) => break,
                    {crash_handler}
                }}
                __restart_count += 1;
                if !({restart_check}) {{
                    eprintln!("[orchestrate] automatic block stopped after {{}} restarts", __restart_count);
                    break;
                }}
            }}
        }})
    }}) as ProcessRef
}}"#,
                    capture = capture_code,
                    setup = setup_block,
                    loop_body = loop_inner,
                    crash_handler = crash_handler_code,
                    restart_check = restart_policy_str,
                )
            }
            ExprNode::TriggeredBlock { event_name, params, body } => {
                let mut free_vars = HashSet::new();
                self.get_free_vars_expr(expr, &mut HashSet::new(), &mut free_vars);

                let mut capture_code = String::new();
                for var in &free_vars {
                    capture_code.push_str(&format!("let {} = {}.clone();\n    ", var, var));
                }

                let func_name = format!("get_registry_{}", event_name);
                let types_str = if params.is_empty() {
                    "()".to_string()
                } else if params.len() == 1 {
                    self.compile_type(&params[0].ty).to_string()
                } else {
                    let compiled_tys = params.iter().map(|p| self.compile_type(&p.ty)).collect::<Vec<String>>().join(", ");
                    format!("({})", compiled_tys)
                };
                let arc_type_str = format!("std::sync::Arc<{}>", types_str);

                let bindings = if params.is_empty() {
                    "_".to_string()
                } else if params.len() == 1 {
                    params[0].name.clone()
                } else {
                    let names = params.iter().map(|p| p.name.clone()).collect::<Vec<String>>().join(", ");
                    format!("({})", names)
                };

                let body_str = self.compile_expr(body);

                format!(
                    "{{\n    {}std::sync::Arc::new(move || {{\n        let (tx, mut rx) = tokio::sync::mpsc::channel::<{}>(100);\n        {}().lock().unwrap().push(tx);\n        tokio::spawn(async move {{\n            while let Some(msg) = rx.recv().await {{\n                let {} = (*msg).clone();\n                tokio::spawn(async move {{\n                    {}\n                }});\n            }}\n        }});\n    }})\n}}",
                    capture_code, arc_type_str, func_name, bindings, body_str
                )
            }
            ExprNode::StartProcess { target } => {
                let target_str = self.compile_expr(target);
                format!("{}()", target_str)
            }
            ExprNode::ArrayLiteral(elements) => {
                let elems_str = elements.iter().map(|e| self.compile_expr(e)).collect::<Vec<String>>().join(", ");
                format!("vec![{}]", elems_str)
            }
            ExprNode::StructLiteral { name, fields } => {
                let fields_str = fields.iter()
                    .map(|(fname, val)| format!("    {}: {},", fname, self.compile_expr(val)))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("{} {{\n{}\n}}", name, fields_str)
            }
            ExprNode::FieldAccess { object, field } => {
                format!("{}.{}", self.compile_expr(object), field)
            }
            // Error handling expressions
            ExprNode::NoneLiteral => "None".to_string(),
            ExprNode::SomeLiteral(inner) => {
                format!("Some({})", self.compile_expr(inner))
            }
            ExprNode::OkLiteral(inner) => {
                format!("Ok({})", self.compile_expr(inner))
            }
            ExprNode::ErrLiteral(msg) => {
                format!("Err({})", self.compile_expr(msg))
            }
            ExprNode::Propagate(inner) => {
                // In async context, ? works directly on Result/Option
                format!("({})? ", self.compile_expr(inner))
            }
            ExprNode::TryCatch { body, err_name, handler } => {
                let body_str = self.compile_expr(body);
                let handler_str = self.compile_expr(handler);
                format!(
                    r#"(|| -> Result<_, String> {{ Ok({{ {} }}) }})().unwrap_or_else(|{}: String| {{ {} }})"#,
                    body_str, err_name, handler_str
                )
            }
            // Enum expressions
            ExprNode::EnumVariantLiteral { enum_name, variant_name, payload } => {
                match payload {
                    Some(p) => format!("{}::{}({})", enum_name, variant_name, self.compile_expr(p)),
                    None => format!("{}::{}", enum_name, variant_name),
                }
            }
            ExprNode::Match { value, arms } => {
                let val_str = self.compile_expr(value);
                // String literal patterns require matching on &str via .as_str()
                let has_str_literal = arms.iter().any(|a| matches!(&a.pattern, MatchPattern::Literal(Literal::Str(_))));
                let match_subject = if has_str_literal { format!("{}.as_str()", val_str) } else { val_str };

                let mut arms_code = Vec::new();
                for arm in arms {
                    // Pre-compile guard conditions (needs &mut self, so do before pattern compilation)
                    let guard_str: Option<String> = if let MatchPattern::Guard { condition, .. } = &arm.pattern {
                        let cond_clone = *condition.clone();
                        Some(self.compile_expr(&cond_clone))
                    } else {
                        None
                    };

                    let pattern_str = Self::compile_match_pattern_str(&arm.pattern, guard_str.as_deref());
                    let body_str = self.compile_expr(&arm.body);
                    arms_code.push(format!("        {} => {{ {} }}", pattern_str, body_str));
                }
                format!("match {} {{\n{}\n    }}", match_subject, arms_code.join(",\n"))
            }
            ExprNode::Closure { params, return_type, body } => {
                let params_str = params.iter()
                    .map(|p| format!("{}: {}", p.name, self.compile_type(&p.ty)))
                    .collect::<Vec<_>>()
                    .join(", ");
                let ret_str = match return_type {
                    Some(rt) if *rt != Type::Void => format!(" -> {}", self.compile_type(rt)),
                    _ => String::new(),
                };
                let body_str = self.compile_expr(body);
                format!("move |{}|{} {}", params_str, ret_str, body_str)
            }
            ExprNode::StringInterp { parts } => {
                let mut fmt_str = String::new();
                let mut args: Vec<String> = Vec::new();
                for part in parts {
                    match part {
                        StringPart::Literal(s) => {
                            fmt_str.push_str(&s.replace('{', "{{").replace('}', "}}"));
                        }
                        StringPart::Expr(e) => {
                            fmt_str.push_str("{}");
                            args.push(self.compile_expr(e));
                        }
                    }
                }
                if args.is_empty() {
                    format!("String::from({:?})", fmt_str)
                } else {
                    format!("format!({:?}, {})", fmt_str, args.join(", "))
                }
            }
        }
    }

    /// Pure helper — compiles a MatchPattern to a Rust pattern string.
    /// `guard_str` is pre-compiled from the condition expr (if any Guard wraps this pattern).
    pub fn compile_match_pattern_str(pattern: &MatchPattern, guard_str: Option<&str>) -> String {
        match pattern {
            MatchPattern::EnumVariant { enum_name, variant_name, binding } => {
                let is_builtin = matches!(enum_name.as_str(), "option" | "result");
                let base = if is_builtin {
                    match binding {
                        Some(b) => format!("{}({})", variant_name, b),
                        None => variant_name.clone(),
                    }
                } else {
                    match binding {
                        Some(b) => format!("{}::{}({})", enum_name, variant_name, b),
                        None => format!("{}::{}", enum_name, variant_name),
                    }
                };
                if let Some(g) = guard_str { format!("{} if {}", base, g) } else { base }
            }
            MatchPattern::Wildcard => {
                if let Some(g) = guard_str { format!("_ if {}", g) } else { "_".to_string() }
            }
            MatchPattern::Literal(lit) => {
                let base = match lit {
                    Literal::Int(n) => format!("{}i64", n),
                    Literal::Float(f) => format!("{}_f64", f),
                    Literal::Bool(b) => b.to_string(),
                    // String literal patterns work when the match subject uses .as_str()
                    Literal::Str(s) => format!("{:?}", s),
                };
                if let Some(g) = guard_str { format!("{} if {}", base, g) } else { base }
            }
            MatchPattern::Binding(name) => {
                if let Some(g) = guard_str { format!("{} if {}", name, g) } else { name.clone() }
            }
            MatchPattern::Guard { inner, .. } => {
                // guard_str is pre-compiled and passed in; inner already has no nested guard
                Self::compile_match_pattern_str(inner, guard_str)
            }
        }
    }
}
