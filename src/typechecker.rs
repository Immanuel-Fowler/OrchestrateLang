use std::collections::HashMap;
use crate::ast::{BinaryOp, Expr, Literal, Stmt, Type};

pub struct TypeChecker {
    env: Vec<HashMap<String, Type>>,
    functions: HashMap<String, (Vec<Type>, Type)>,
}

impl TypeChecker {
    pub fn new() -> Self {
        let mut tc = TypeChecker {
            env: vec![HashMap::new()],
            functions: HashMap::new(),
        };

        // Built-ins
        tc.functions.insert("print".to_string(), (vec![Type::Str], Type::Void)); // Allow string for now
        tc.functions.insert("sleep".to_string(), (vec![Type::Int], Type::Void));
        
        // Note: For array functions (length, append, remove), we will handle them as special cases 
        // in `infer_expr` because they are polymorphic over the inner array type.

        tc
    }

    pub fn type_check(&mut self, stmts: &[Stmt]) -> Result<(), String> {
        // First pass: register all functions and tasks to allow forward references
        for stmt in stmts {
            match stmt {
                Stmt::FnDecl { name, params, return_type, .. } |
                Stmt::TaskDecl { name, params, return_type, .. } |
                Stmt::ProcessDecl { name, params, return_type, .. } |
                Stmt::OrchestratorDecl { name, params, return_type, .. } => {
                    let param_types: Vec<Type> = params.iter().map(|p| p.ty.clone()).collect();
                    self.functions.insert(name.clone(), (param_types, return_type.clone()));
                }
                _ => {}
            }
        }

        // Second pass: type check the bodies
        for stmt in stmts {
            self.check_stmt(stmt)?;
        }
        Ok(())
    }

    fn push_env(&mut self) {
        self.env.push(HashMap::new());
    }

    fn pop_env(&mut self) {
        self.env.pop();
    }

    fn define_var(&mut self, name: String, ty: Type) {
        if let Some(scope) = self.env.last_mut() {
            scope.insert(name, ty);
        }
    }

    fn lookup_var(&self, name: &str) -> Option<Type> {
        for scope in self.env.iter().rev() {
            if let Some(ty) = scope.get(name) {
                return Some(ty.clone());
            }
        }
        None
    }

    fn check_stmt(&mut self, stmt: &Stmt) -> Result<(), String> {
        match stmt {
            Stmt::Let { name, ty, value } => {
                let val_ty = self.infer_expr(value)?;
                if let Some(expected_ty) = ty {
                    if *expected_ty != val_ty && *expected_ty != Type::Void && val_ty != Type::Void {
                        return Err(format!("Type mismatch in let statement: expected {:?}, found {:?}", expected_ty, val_ty));
                    }
                    self.define_var(name.clone(), expected_ty.clone());
                } else {
                    self.define_var(name.clone(), val_ty);
                }
            }
            Stmt::Expr(expr) => {
                self.infer_expr(expr)?;
            }
            Stmt::Return(opt_expr) => {
                if let Some(expr) = opt_expr {
                    self.infer_expr(expr)?;
                }
            }
            Stmt::FnDecl { params, body, .. } |
            Stmt::TaskDecl { params, body, .. } |
            Stmt::ProcessDecl { params, body, .. } |
            Stmt::OrchestratorDecl { params, body, .. } => {
                self.push_env();
                for p in params {
                    self.define_var(p.name.clone(), p.ty.clone());
                }
                self.infer_expr(body)?;
                self.pop_env();
            }
            Stmt::Trigger { args, .. } => {
                for arg in args {
                    self.infer_expr(arg)?;
                }
            }
            Stmt::Parallel(stmts) => {
                self.push_env();
                for s in stmts {
                    self.check_stmt(s)?;
                }
                self.pop_env();
            }
            Stmt::While { cond, body } => {
                let cond_ty = self.infer_expr(cond)?;
                if cond_ty != Type::Bool {
                    return Err("While condition must be a boolean".to_string());
                }
                self.infer_expr(body)?;
            }
            Stmt::UseModule { .. } | Stmt::Load { .. } => {}
            Stmt::Serverlet { state, handlers, .. } => {
                self.push_env();
                for s in state {
                    self.check_stmt(s)?;
                }
                for h in handlers {
                    self.push_env();
                    for p in &h.params {
                        self.define_var(p.name.clone(), p.ty.clone());
                    }
                    self.infer_expr(&h.body)?;
                    self.pop_env();
                }
                self.pop_env();
            }
            Stmt::OnStart(expr) | Stmt::OnStop(expr) => {
                self.infer_expr(expr)?;
            }
        }
        Ok(())
    }

    fn infer_expr(&mut self, expr: &Expr) -> Result<Type, String> {
        match expr {
            Expr::Literal(lit) => match lit {
                Literal::Int(_) => Ok(Type::Int),
                Literal::Float(_) => Ok(Type::Float),
                Literal::Str(_) => Ok(Type::Str),
                Literal::Bool(_) => Ok(Type::Bool),
            },
            Expr::Identifier(name) => {
                if let Some(ty) = self.lookup_var(name) {
                    Ok(ty)
                } else {
                    Err(format!("undefined variable '{}'", name))
                }
            }
            Expr::Binary { op, lhs, rhs } => {
                let lhs_ty = self.infer_expr(lhs)?;
                let rhs_ty = self.infer_expr(rhs)?;
                
                if *op == BinaryOp::Assign {
                    // Simplified assignment check
                    Ok(Type::Void)
                } else if [BinaryOp::Eq, BinaryOp::Ne, BinaryOp::Lt, BinaryOp::Gt, BinaryOp::Le, BinaryOp::Ge].contains(op) {
                    Ok(Type::Bool)
                } else if [BinaryOp::And, BinaryOp::Or].contains(op) {
                    if lhs_ty != Type::Bool || rhs_ty != Type::Bool {
                        return Err(format!("Logical operations require boolean operands, got {:?} and {:?}", lhs_ty, rhs_ty));
                    }
                    Ok(Type::Bool)
                } else {
                    if lhs_ty == Type::Int && rhs_ty == Type::Int {
                        Ok(Type::Int)
                    } else if lhs_ty == Type::Float && rhs_ty == Type::Float {
                        Ok(Type::Float)
                    } else if *op == BinaryOp::Add && (lhs_ty == Type::Str || rhs_ty == Type::Str) {
                        Ok(Type::Str) // String concatenation
                    } else {
                        Err(format!("Mismatched types in binary operation: {:?} and {:?}", lhs_ty, rhs_ty))
                    }
                }
            }
            Expr::Call { callee, args } => {
                let mut arg_types = Vec::new();
                for a in args {
                    arg_types.push(self.infer_expr(a)?);
                }

                // Special handling for array functions
                if callee == "length" && args.len() == 1 {
                    if let Type::Array(_, _) = arg_types[0] {
                        return Ok(Type::Int);
                    } else {
                        return Err(format!("length() expects an array, got {:?}", arg_types[0]));
                    }
                }
                if callee == "append" && args.len() == 2 {
                    if let Type::Array(_inner_ty, _) = &arg_types[0] {
                        // Basic type compatibility check could be done here
                        // For now, we trust the types align or fallback to rustc
                        return Ok(Type::Void);
                    } else {
                        return Err(format!("append() expects an array as first argument, got {:?}", arg_types[0]));
                    }
                }
                if callee == "remove" && args.len() == 2 {
                    if let Type::Array(_, _) = arg_types[0] {
                        if arg_types[1] != Type::Int {
                            return Err(format!("remove() expects an integer index, got {:?}", arg_types[1]));
                        }
                        return Ok(Type::Void); // Returns value in Rust, but Void for simplicity or inner_ty
                    } else {
                        return Err(format!("remove() expects an array as first argument, got {:?}", arg_types[0]));
                    }
                }

                if let Some((expected_args, ret_ty)) = self.functions.get(callee) {
                    if expected_args.len() != args.len() {
                        return Err(format!("Function {} expected {} arguments, got {}", callee, expected_args.len(), args.len()));
                    }
                    Ok(ret_ty.clone())
                } else {
                    Ok(Type::Void) // Unknown function
                }
            }
            Expr::Pipeline { value, function } => {
                let _val_ty = self.infer_expr(value)?;
                // The piped value is implicitly prepended as the first argument.
                // When the function side is a Call, adjust arity checking accordingly.
                match function.as_ref() {
                    Expr::Call { callee, args } => {
                        // Infer types of explicit args
                        for a in args {
                            self.infer_expr(a)?;
                        }
                        // Check arity: pipeline adds 1 implicit arg
                        if let Some((expected_args, ret_ty)) = self.functions.get(callee) {
                            let effective_arg_count = args.len() + 1; // +1 for piped value
                            if expected_args.len() != effective_arg_count {
                                return Err(format!(
                                    "Function {} expected {} arguments, got {} (including piped value)",
                                    callee,
                                    expected_args.len(),
                                    effective_arg_count
                                ));
                            }
                            Ok(ret_ty.clone())
                        } else {
                            Ok(Type::Void) // Unknown function
                        }
                    }
                    Expr::Identifier(name) => {
                        // Pipeline to a bare identifier: value |> fn
                        if let Some((expected_args, ret_ty)) = self.functions.get(name) {
                            if expected_args.len() != 1 {
                                return Err(format!(
                                    "Function {} expected {} arguments, got 1 (piped value)",
                                    name,
                                    expected_args.len()
                                ));
                            }
                            Ok(ret_ty.clone())
                        } else {
                            Ok(Type::Void) // Unknown function
                        }
                    }
                    _ => self.infer_expr(function),
                }
            }
            Expr::Block(stmts) => {
                self.push_env();
                let mut last_ty = Type::Void;
                for stmt in stmts {
                    self.check_stmt(stmt)?;
                    if let Stmt::Expr(e) = stmt {
                        last_ty = self.infer_expr(e)?;
                    } else {
                        last_ty = Type::Void;
                    }
                }
                self.pop_env();
                Ok(last_ty)
            }
            Expr::If { cond, then_branch, else_branch } => {
                let cond_ty = self.infer_expr(cond)?;
                if cond_ty != Type::Bool {
                    return Err("If condition must be a boolean".to_string());
                }
                let then_ty = self.infer_expr(then_branch)?;
                if let Some(eb) = else_branch {
                    self.infer_expr(eb)?;
                }
                Ok(then_ty)
            }
            Expr::ModuleCall { args, .. } => {
                for arg in args {
                    self.infer_expr(arg)?;
                }
                Ok(Type::Void)
            }
            Expr::StartServerlet { args, .. } => {
                for arg in args {
                    self.infer_expr(arg)?;
                }
                Ok(Type::Process)
            }
            Expr::AutomaticBlock { body } => {
                self.infer_expr(body)?;
                Ok(Type::Process)
            }
            Expr::TriggeredBlock { params, body, .. } => {
                self.push_env();
                for p in params {
                    self.define_var(p.name.clone(), p.ty.clone());
                }
                self.infer_expr(body)?;
                self.pop_env();
                Ok(Type::Process)
            }
            Expr::StartProcess { target } => {
                self.infer_expr(target)?;
                Ok(Type::Process)
            }
            Expr::ArrayLiteral(elements) => {
                let mut inner_ty = Type::Void;
                for e in elements {
                    let ty = self.infer_expr(e)?;
                    if inner_ty == Type::Void {
                        inner_ty = ty;
                    }
                }
                if inner_ty == Type::Void {
                    inner_ty = Type::Int; // Default fallback for empty arrays
                }
                Ok(Type::Array(Box::new(inner_ty), vec![]))
            }
        }
    }
}
