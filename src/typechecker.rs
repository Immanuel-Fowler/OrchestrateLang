use std::collections::{HashMap, HashSet};
use crate::ast::{ExprNode, StmtNode, BinaryOp, Expr, Literal, Stmt, Type};

pub struct TypeChecker {
    env: Vec<HashMap<String, Type>>,
    functions: HashMap<String, (Vec<Type>, Type)>,
    exempt_functions: HashSet<String>,
    pub struct_defs: HashMap<String, Vec<(String, Type)>>,
}

impl TypeChecker {
    pub fn new() -> Self {
        let mut tc = TypeChecker {
            env: vec![HashMap::new()],
            functions: HashMap::new(),
            exempt_functions: HashSet::new(),
            struct_defs: HashMap::new(),
        };

        // Built-ins
        tc.functions.insert("print".to_string(), (vec![Type::Str], Type::Void)); // Allow string for now
        tc.functions.insert("sleep".to_string(), (vec![Type::Int], Type::Void));
        
        // Note: For array functions (length, append, remove), we will handle them as special cases 
        // in `infer_expr` because they are polymorphic over the inner array type.
        
        let builtins = vec!["print", "to_string", "sleep", "stop_orch", "length", "append", "remove"];
        for b in builtins {
            tc.exempt_functions.insert(b.to_string());
        }

        tc
    }

    pub fn type_check(&mut self, stmts: &[Stmt]) -> Result<(), String> {
        // First pass: register all top-level declarations for forward references
        for stmt in stmts {
            match &stmt.node {
                StmtNode::FnDecl { name, params, return_type, .. } |
                StmtNode::TaskDecl { name, params, return_type, .. } |
                StmtNode::ProcessDecl { name, params, return_type, .. } |
                StmtNode::OrchestratorDecl { name, params, return_type, .. } => {
                    let param_types: Vec<Type> = params.iter().map(|p| p.ty.clone()).collect();
                    self.functions.insert(name.clone(), (param_types, return_type.clone()));
                }
                StmtNode::StructDef { name, fields } => {
                    self.struct_defs.insert(name.clone(), fields.clone());
                }
                StmtNode::Serverlet { name, handlers, .. } => {
                    // Register handlers as `Serverlet::handler` so calls through a
                    // client variable (`v.handler(..)`) resolve to the right return
                    // type instead of falling back to void.
                    for h in handlers {
                        let param_types: Vec<Type> = h.params.iter().map(|p| p.ty.clone()).collect();
                        self.functions.insert(format!("{}::{}", name, h.name), (param_types, h.return_type.clone()));
                    }
                }
                _ => {}
            }
        }

        // Second pass: type check bodies, collecting all errors
        let mut errors: Vec<String> = Vec::new();
        for stmt in stmts {
            if let Err(e) = self.check_stmt(stmt) {
                errors.push(e);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("\n"))
        }
    }

    pub fn register_module_functions(&mut self, alias: &str, stmts: &[Stmt]) {
        for stmt in stmts {
            match &stmt.node {
                StmtNode::FnDecl { name, params, return_type, .. } |
                StmtNode::TaskDecl { name, params, return_type, .. } |
                StmtNode::ProcessDecl { name, params, return_type, .. } |
                StmtNode::OrchestratorDecl { name, params, return_type, .. } => {
                    let param_types: Vec<Type> = params.iter().map(|p| p.ty.clone()).collect();
                    let full_name = format!("{}::{}", alias, name);
                    self.functions.insert(full_name, (param_types, return_type.clone()));
                }
                StmtNode::Serverlet { name: serverlet_name, handlers, .. } => {
                    for h in handlers {
                        let param_types: Vec<Type> = h.params.iter().map(|p| p.ty.clone()).collect();
                        let alias_name = format!("{}::{}", alias, h.name);
                        let direct_name = format!("{}::{}", serverlet_name, h.name);
                        self.functions.insert(alias_name, (param_types.clone(), h.return_type.clone()));
                        self.functions.insert(direct_name, (param_types, h.return_type.clone()));
                    }
                }
                _ => {}
            }
        }
    }

    #[allow(dead_code)]
    pub fn has_function(&self, name: &str) -> bool {
        self.functions.contains_key(name)
    }

    pub fn register_foreign_function(&mut self, alias: &str, name: &str, params: Vec<Type>, ret_ty: Type) {
        let full_name = format!("{}::{}", alias, name);
        self.functions.insert(full_name, (params, ret_ty));
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
        match &stmt.node {
            StmtNode::Let { name, ty, value } => {
                let val_ty = self.infer_expr(value)?;
                if let Some(expected_ty) = ty {
                    if *expected_ty != val_ty && *expected_ty != Type::Void && val_ty != Type::Void {
                        return Err(format!("line {}, col {}: Type mismatch in let statement: expected {:?}, found {:?}", stmt.span.line, stmt.span.col, expected_ty, val_ty));
                    }
                    self.define_var(name.clone(), expected_ty.clone());
                } else {
                    self.define_var(name.clone(), val_ty);
                }
            }
            StmtNode::Expr(expr) => {
                self.infer_expr(expr)?;
            }
            StmtNode::Return(opt_expr) => {
                if let Some(expr) = opt_expr {
                    self.infer_expr(expr)?;
                }
            }
            StmtNode::FnDecl { params, body, .. } |
            StmtNode::TaskDecl { params, body, .. } |
            StmtNode::ProcessDecl { params, body, .. } |
            StmtNode::OrchestratorDecl { params, body, .. } => {
                self.push_env();
                for p in params {
                    self.define_var(p.name.clone(), p.ty.clone());
                }
                self.infer_expr(body)?;
                self.pop_env();
            }
            StmtNode::Trigger { args, .. } => {
                for arg in args {
                    self.infer_expr(arg)?;
                }
            }
            StmtNode::Parallel(stmts) => {
                // NOTE: Intentionally no push_env/pop_env here.
                // Variables bound inside a parallel block (let x = task()) are
                // available after the block — they escape into the enclosing scope.
                // This matches the generated Rust: `let (a, b) = tokio::join!(...)` where
                // `a` and `b` are in scope after the join completes.
                for s in stmts {
                    self.check_stmt(s)?;
                }
            }
            StmtNode::While { cond, body } => {
                let cond_ty = self.infer_expr(cond)?;
                if cond_ty != Type::Bool {
                    return Err(format!("line {}, col {}: While condition must be a boolean", stmt.span.line, stmt.span.col));
                }
                self.infer_expr(body)?;
            }
            StmtNode::UseModule { .. } | StmtNode::Load { .. } | StmtNode::LoadForeign { .. } | StmtNode::StructDef { .. } => {}
            StmtNode::Serverlet { state, handlers, .. } => {
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
            StmtNode::OnStart(expr) | StmtNode::OnStop(expr) => {
                self.infer_expr(expr)?;
            }
        }
        Ok(())
    }

    fn infer_expr(&mut self, expr: &Expr) -> Result<Type, String> {
        match &expr.node {
            ExprNode::Literal(lit) => match lit {
                Literal::Int(_) => Ok(Type::Int),
                Literal::Float(_) => Ok(Type::Float),
                Literal::Str(_) => Ok(Type::Str),
                Literal::Bool(_) => Ok(Type::Bool),
            },
            ExprNode::Identifier(name) => {
                if let Some(ty) = self.lookup_var(name) {
                    Ok(ty)
                } else {
                    Err(format!("line {}, col {}: undefined variable '{}'", expr.span.line, expr.span.col, name))
                }
            }
            ExprNode::Binary { op, lhs, rhs } => {
                let lhs_ty = self.infer_expr(lhs)?;
                let rhs_ty = self.infer_expr(rhs)?;
                
                if *op == BinaryOp::Assign {
                    // Simplified assignment check
                    Ok(Type::Void)
                } else if [BinaryOp::Eq, BinaryOp::Ne, BinaryOp::Lt, BinaryOp::Gt, BinaryOp::Le, BinaryOp::Ge].contains(op) {
                    Ok(Type::Bool)
                } else if [BinaryOp::And, BinaryOp::Or].contains(op) {
                    if lhs_ty != Type::Bool || rhs_ty != Type::Bool {
                        return Err(format!("line {}, col {}: Logical operations require boolean operands, got {:?} and {:?}", expr.span.line, expr.span.col, lhs_ty, rhs_ty));
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
                        Err(format!("line {}, col {}: Mismatched types in binary operation: {:?} and {:?}", expr.span.line, expr.span.col, lhs_ty, rhs_ty))
                    }
                }
            }
            ExprNode::Call { callee, args } => {
                let mut arg_types = Vec::new();
                for a in args {
                    arg_types.push(self.infer_expr(a)?);
                }

                // Special handling for array functions
                if callee == "length" && args.len() == 1 {
                    if let Type::Array(_, _) = arg_types[0] {
                        return Ok(Type::Int);
                    } else {
                        return Err(format!("line {}, col {}: length() expects an array, got {:?}", expr.span.line, expr.span.col, arg_types[0]));
                    }
                }
                if callee == "append" && args.len() == 2 {
                    if let Type::Array(_inner_ty, _) = &arg_types[0] {
                        // Basic type compatibility check could be done here
                        // For now, we trust the types align or fallback to rustc
                        return Ok(Type::Void);
                    } else {
                        return Err(format!("line {}, col {}: append() expects an array as first argument, got {:?}", expr.span.line, expr.span.col, arg_types[0]));
                    }
                }
                if callee == "remove" && args.len() == 2 {
                    if let Type::Array(_, _) = arg_types[0] {
                        if arg_types[1] != Type::Int {
                            return Err(format!("line {}, col {}: remove() expects an integer index, got {:?}", expr.span.line, expr.span.col, arg_types[1]));
                        }
                        return Ok(Type::Void); // Returns value in Rust, but Void for simplicity or inner_ty
                    } else {
                        return Err(format!("line {}, col {}: remove() expects an array as first argument, got {:?}", expr.span.line, expr.span.col, arg_types[0]));
                    }
                }

                if let Some((expected_args, ret_ty)) = self.functions.get(callee) {
                    if expected_args.len() != args.len() {
                        return Err(format!("line {}, col {}: Function {} expected {} arguments, got {}", expr.span.line, expr.span.col, callee, expected_args.len(), args.len()));
                    }
                    Ok(ret_ty.clone())
                } else {
                    if !self.exempt_functions.contains(callee) {
                        eprintln!("[orchestrate] warning: unknown function '{}' — if this is a foreign function, this warning can be ignored", callee);
                    }
                    Ok(Type::Void) // Unknown function
                }
            }
            ExprNode::Pipeline { value, function } => {
                let _val_ty = self.infer_expr(value)?;
                // The piped value is implicitly prepended as the first argument.
                // When the function side is a Call, adjust arity checking accordingly.
                match &function.node {
                    ExprNode::Call { callee, args } => {
                        // Infer types of explicit args
                        for a in args {
                            self.infer_expr(a)?;
                        }
                        // Check arity: pipeline adds 1 implicit arg
                        if let Some((expected_args, ret_ty)) = self.functions.get(callee) {
                            let effective_arg_count = args.len() + 1; // +1 for piped value
                            if expected_args.len() != effective_arg_count {
                                return Err(format!(
                                    "line {}, col {}: Function {} expected {} arguments, got {} (including piped value)",
                                    expr.span.line, expr.span.col,
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
                    ExprNode::Identifier(name) => {
                        // Pipeline to a bare identifier: value |> fn
                        if let Some((expected_args, ret_ty)) = self.functions.get(name) {
                            if expected_args.len() != 1 {
                                return Err(format!(
                                    "line {}, col {}: Function {} expected {} arguments, got 1 (piped value)",
                                    expr.span.line, expr.span.col,
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
            ExprNode::Block(stmts) => {
                self.push_env();
                let mut last_ty = Type::Void;
                for stmt in stmts {
                    self.check_stmt(stmt)?;
                    if let StmtNode::Expr(e) = &stmt.node {
                        last_ty = self.infer_expr(e)?;
                    } else {
                        last_ty = Type::Void;
                    }
                }
                self.pop_env();
                Ok(last_ty)
            }
            ExprNode::If { cond, then_branch, else_branch } => {
                let cond_ty = self.infer_expr(cond)?;
                if cond_ty != Type::Bool {
                    return Err(format!("line {}, col {}: If condition must be a boolean", expr.span.line, expr.span.col));
                }
                let then_ty = self.infer_expr(then_branch)?;
                if let Some(eb) = else_branch {
                    self.infer_expr(eb)?;
                }
                Ok(then_ty)
            }
            ExprNode::ModuleCall { module_local_name, function, args } => {
                let mut arg_types = Vec::new();
                for arg in args {
                    arg_types.push(self.infer_expr(arg)?);
                }
                
                let alias_key = format!("{}::{}", module_local_name, function);
                if let Some((expected_args, ret_ty)) = self.functions.get(&alias_key) {
                    if expected_args.len() != args.len() {
                        return Err(format!("line {}, col {}: Module function {} expected {} arguments, got {}", expr.span.line, expr.span.col, alias_key, expected_args.len(), args.len()));
                    }
                    return Ok(ret_ty.clone());
                }
                
                // Fallback: check if ANY registered function ends with `::function`
                // This handles cases where `module_local_name` is a variable holding a serverlet client
                // e.g. `service.increment(5)` where `increment` is registered as `counter::increment`.
                for (key, (expected_args, ret_ty)) in self.functions.iter() {
                    if key.ends_with(&format!("::{}", function)) {
                        if expected_args.len() != args.len() {
                            return Err(format!("line {}, col {}: Module function {} expected {} arguments, got {}", expr.span.line, expr.span.col, key, expected_args.len(), args.len()));
                        }
                        return Ok(ret_ty.clone());
                    }
                }

                if !self.exempt_functions.contains(function.as_str()) {
                    eprintln!("[orchestrate] warning: unknown function call '.{}()' — if this is a foreign function or serverlet method, this warning can be ignored", function);
                }
                
                Ok(Type::Void)
            }
            ExprNode::StartServerlet { args, .. } => {
                for arg in args {
                    self.infer_expr(arg)?;
                }
                Ok(Type::Process)
            }
            ExprNode::AutomaticBlock { body } => {
                self.infer_expr(body)?;
                Ok(Type::Process)
            }
            ExprNode::TriggeredBlock { params, body, .. } => {
                self.push_env();
                for p in params {
                    self.define_var(p.name.clone(), p.ty.clone());
                }
                self.infer_expr(body)?;
                self.pop_env();
                Ok(Type::Process)
            }
            ExprNode::StartProcess { target } => {
                self.infer_expr(target)?;
                Ok(Type::Process)
            }
            ExprNode::ArrayLiteral(elements) => {
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
            ExprNode::StructLiteral { name, fields } => {
                let def = self.struct_defs.get(name).cloned()
                    .ok_or_else(|| format!("line {}, col {}: unknown struct type '{}'", expr.span.line, expr.span.col, name))?;
                for (fname, fexpr) in fields {
                    let actual_ty = self.infer_expr(fexpr)?;
                    if let Some((_, expected_ty)) = def.iter().find(|(n, _)| n == fname) {
                        if *expected_ty != actual_ty && *expected_ty != Type::Void && actual_ty != Type::Void {
                            return Err(format!("line {}, col {}: field '{}' of struct '{}': expected {:?}, found {:?}",
                                expr.span.line, expr.span.col, fname, name, expected_ty, actual_ty));
                        }
                    } else {
                        return Err(format!("line {}, col {}: struct '{}' has no field '{}'", expr.span.line, expr.span.col, name, fname));
                    }
                }
                Ok(Type::Named(name.clone()))
            }
            ExprNode::FieldAccess { object, field } => {
                let obj_ty = self.infer_expr(object)?;
                if let Type::Named(struct_name) = &obj_ty {
                    let def = self.struct_defs.get(struct_name).cloned()
                        .ok_or_else(|| format!("line {}, col {}: unknown struct '{}'", expr.span.line, expr.span.col, struct_name))?;
                    if let Some((_, fty)) = def.iter().find(|(n, _)| n == field) {
                        Ok(fty.clone())
                    } else {
                        Err(format!("line {}, col {}: struct '{}' has no field '{}'", expr.span.line, expr.span.col, struct_name, field))
                    }
                } else {
                    Err(format!("line {}, col {}: field access '.{}' on non-struct type {:?}", expr.span.line, expr.span.col, field, obj_ty))
                }
            }
        }
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    #[test]
    fn test_typecheck_let() {
        let mut lexer = Lexer::new("let x = 5; let y: int = x;");
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let stmts = parser.parse().unwrap();
        let mut tc = TypeChecker::new();
        assert!(tc.type_check(&stmts).is_ok());
    }

    #[test]
    fn test_typecheck_error() {
        let mut lexer = Lexer::new("let x = 5; let y: string = x;");
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let stmts = parser.parse().unwrap();
        let mut tc = TypeChecker::new();
        assert!(tc.type_check(&stmts).is_err());
    }
}

