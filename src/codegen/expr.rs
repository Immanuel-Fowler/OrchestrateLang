use crate::ast::{ExprNode, StmtNode, BinaryOp, Expr, Literal};
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
                        BinaryOp::Assign => unreachable!(),
                        BinaryOp::Add => unreachable!(),
                    };
                    format!("({} {} {})", lhs_str, op_str, rhs_str)
                }
            }
            ExprNode::Call { callee, args } => {
                let args_str = args
                    .iter()
                    .map(|a| self.compile_expr(a))
                    .collect::<Vec<String>>()
                    .join(", ");

                if callee == "print" {
                    format!("print_val({})", args_str)
                } else if callee == "length" && args.len() == 1 {
                    format!("{}.len() as i64", self.compile_expr(&args[0]))
                } else if callee == "append" && args.len() == 2 {
                    format!("{}.push({})", self.compile_expr(&args[0]), self.compile_expr(&args[1]))
                } else if callee == "remove" && args.len() == 2 {
                    format!("{}.remove({} as usize)", self.compile_expr(&args[0]), self.compile_expr(&args[1]))
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
                // Compile value
                let val_str = self.compile_expr(value);

                // Compile pipeline function
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
                    _ => {
                        panic!("Invalid pipeline function target: {:?}", function);
                    }
                }
            }
            ExprNode::Block(_) => {
                let inner = self.compile_block_inner(expr, false);
                format!("{{\n    {}\n}}", inner.replace("\n", "\n    "))
            }
            ExprNode::If {
                cond,
                then_branch,
                else_branch,
            } => {
                let cond_str = self.compile_expr(cond);
                let then_str = self.compile_expr(then_branch);
                if let Some(else_b) = else_branch {
                    let else_str = self.compile_expr(else_b);
                    format!("if {} {} else {}", cond_str, then_str, else_str)
                } else {
                    format!("if {} {}", cond_str, then_str)
                }
            }
            ExprNode::ModuleCall {
                module_local_name,
                function,
                args,
            } => {
                let args_str = args
                    .iter()
                    .map(|a| self.compile_expr(a))
                    .collect::<Vec<String>>()
                    .join(", ");

                if self.modules.contains(module_local_name) {
                    let full_name = format!("{}::{}", module_local_name, function);
                    if self.tasks.contains(&full_name) && !self.in_parallel {
                        format!("{}::{}({}).await", module_local_name, function, args_str)
                    } else {
                        format!("{}::{}({})", module_local_name, function, args_str)
                    }
                } else {
                    if self.in_parallel {
                        format!("{}.{}({})", module_local_name, function, args_str)
                    } else {
                        format!("{}.{}({}).await", module_local_name, function, args_str)
                    }
                }
            }
            ExprNode::StartServerlet { name, args } => {
                let start_fn = if name.contains("::") {
                    let parts: Vec<&str> = name.split("::").collect();
                    format!("{}::start_{}", parts[0], parts[1])
                } else {
                    format!("start_{}", name)
                };
                let args_str = args
                    .iter()
                    .map(|a| self.compile_expr(a))
                    .collect::<Vec<String>>()
                    .join(", ");
                format!("{}({})", start_fn, args_str)
            }
            ExprNode::AutomaticBlock { body } => {
                let mut free_vars = HashSet::new();
                self.get_free_vars_expr(expr, &mut HashSet::new(), &mut free_vars);
                
                let mut capture_code = String::new();
                for var in free_vars {
                    capture_code.push_str(&format!("let {} = {}.clone();\n    ", var, var));
                }

                // Split the body into two phases:
                //   Setup  — `let x = start Service()` statements, hoisted before the loop.
                //             Serverlets are created once per process-block lifetime.
                //   Loop   — everything else, executed on every iteration.
                if let ExprNode::Block(stmts) = &body.node {
                    let mut setup_code: Vec<String> = Vec::new();
                    let mut loop_code: Vec<String> = Vec::new();

                    for stmt in stmts {
                        let is_serverlet_start = match &stmt.node {
                            StmtNode::Let { value, .. } => {
                                matches!(value.node, ExprNode::StartServerlet { .. })
                            }
                            _ => false,
                        };

                        let compiled = self.compile_stmt(stmt);
                        let compiled = if compiled.ends_with(';') || compiled.ends_with('}') {
                            compiled
                        } else {
                            format!("{};", compiled)
                        };

                        if is_serverlet_start {
                            setup_code.push(compiled);
                        } else {
                            loop_code.push(compiled);
                        }
                    }

                    if setup_code.is_empty() {
                        // No serverlet starts — original single-phase loop
                        let loop_inner = loop_code.join("\n            ");
                        format!(
                            "{{\n    {}std::sync::Arc::new(move || {{\n        tokio::spawn(async move {{\n            loop {{\n                {}\n            }}\n        }})\n    }}) as ProcessRef\n}}",
                            capture_code,
                            loop_inner
                        )
                    } else {
                        // Two-phase: setup runs once, then loop repeats
                        let setup_inner = setup_code.join("\n        ");
                        let loop_inner = loop_code.join("\n            ");
                        format!(
                            "{{\n    {}std::sync::Arc::new(move || {{\n        tokio::spawn(async move {{\n            // Serverlet setup — runs once per process-block lifetime\n            {}\n            loop {{\n                {}\n            }}\n        }})\n    }}) as ProcessRef\n}}",
                            capture_code,
                            setup_inner,
                            loop_inner
                        )
                    }
                } else {
                    // Non-block body (edge case) — original behavior
                    let body_str = self.compile_expr(body);
                    format!(
                        "{{\n    {}std::sync::Arc::new(move || {{\n        tokio::spawn(async move {{\n            loop {{\n                {}\n            }}\n        }})\n    }}) as ProcessRef\n}}",
                        capture_code,
                        body_str.replace("\n", "\n        ")
                    )
                }
            }
            ExprNode::TriggeredBlock { event_name, params, body } => {
                let mut free_vars = HashSet::new();
                self.get_free_vars_expr(expr, &mut HashSet::new(), &mut free_vars);
                
                let mut capture_code = String::new();
                for var in free_vars {
                    capture_code.push_str(&format!("let {} = {}.clone();\n    ", var, var));
                }

                let func_name = format!("get_registry_{}", event_name);
                let types_str = if params.is_empty() {
                    "()".to_string()
                } else if params.len() == 1 {
                    self.compile_type(&params[0].ty).to_string()
                } else {
                    let compiled_tys = params
                        .iter()
                        .map(|p| self.compile_type(&p.ty))
                        .collect::<Vec<String>>()
                        .join(", ");
                    format!("({})", compiled_tys)
                };
                let arc_type_str = format!("std::sync::Arc<{}>", types_str);

                let bindings = if params.is_empty() {
                    "_".to_string()
                } else if params.len() == 1 {
                    params[0].name.clone()
                } else {
                    let names = params
                        .iter()
                        .map(|p| p.name.clone())
                        .collect::<Vec<String>>()
                        .join(", ");
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
                let elems_str = elements
                    .iter()
                    .map(|e| self.compile_expr(e))
                    .collect::<Vec<String>>()
                    .join(", ");
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
        }
    }


}
