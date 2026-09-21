use std::collections::{HashMap, HashSet};
use crate::ast::{EnumVariant, ExprNode, MatchPattern, StmtNode, BinaryOp, Expr, Literal, Stmt, StringPart, Type};

pub struct TypeChecker {
    env: Vec<HashMap<String, Type>>,
    functions: HashMap<String, (Vec<Type>, Type)>,
    generic_functions: HashMap<String, Vec<String>>, // name → type_params
    exempt_functions: HashSet<String>,
    pub struct_defs: HashMap<String, Vec<(String, Type)>>,
    pub enum_defs: HashMap<String, Vec<EnumVariant>>,
    current_return_type: Option<Type>,
    host_groups: HashSet<String>,
    pub type_map: HashMap<(usize, usize), Type>,  // (line, col) → inferred type
    /// One collector per open `try` block: the error types its `?`s propagate.
    try_errors: Vec<Vec<Type>>,
    /// Each serverlet's state, in declaration order, with the type each binding was given
    /// or inferred. A sandboxed serverlet's state crosses into a WASM guest, where it
    /// becomes struct fields, and a field needs a type the generator can name.
    pub serverlet_state: HashMap<String, Vec<(String, Type)>>,
    /// Serverlets declared in this file, so `start X()` can be checked.
    serverlet_names: HashSet<String>,
    /// A binding to the serverlet it was started from, so `c.add(1)` can be checked
    /// against that serverlet's handlers.
    var_serverlet: HashMap<String, String>,
    /// Events declared by an `on name(...)` block, so `trigger` can be checked.
    declared_events: HashMap<String, Vec<Type>>,
    /// While a sandboxed serverlet's handlers are checked: its name and the host
    /// functions it was granted. A guest reaches nothing else, so a call to an
    /// ungranted host function is an error here rather than a trap at run time.
    sandbox_grants: Option<(String, HashSet<String>)>,
    /// Module aliases whose declarations were registered, so `m.nope()` on a known
    /// module is an error naming what the module does have.
    module_aliases: HashSet<String>,
}

impl TypeChecker {
    pub fn new() -> Self {
        let mut tc = TypeChecker {
            env: vec![HashMap::new()],
            functions: HashMap::new(),
            generic_functions: HashMap::new(),
            exempt_functions: HashSet::new(),
            struct_defs: HashMap::new(),
            enum_defs: HashMap::new(),
            current_return_type: None,
            host_groups: HashSet::new(),
            type_map: HashMap::new(),
            try_errors: Vec::new(),
            serverlet_state: HashMap::new(),
            serverlet_names: HashSet::new(),
            var_serverlet: HashMap::new(),
            declared_events: HashMap::new(),
            sandbox_grants: None,
            module_aliases: HashSet::new(),
        };

        // `print` is generic over anything displayable in the generated code, so its
        // parameter is a type parameter rather than `string`.
        tc.functions.insert("print".to_string(), (vec![Type::TypeParam("T".to_string())], Type::Void));
        tc.functions.insert("sleep".to_string(), (vec![Type::Int], Type::Void));

        let builtins = vec![
            "print", "to_string", "to_int", "to_float", "parse_int", "parse_float",
            "sleep", "stop_orch", "clock_micros",
            "length", "append", "remove",
            "map", "filter", "reduce", "find", "any", "all", "range",
        ];
        for b in builtins {
            tc.exempt_functions.insert(b.to_string());
        }

        tc
    }

    pub fn type_check(&mut self, stmts: &[Stmt]) -> Result<(), String> {
        for stmt in stmts {
            if let StmtNode::Host { name, functions } = &stmt.node {
                if !self.host_groups.insert(name.clone()) { return Err(format!("Duplicate host group '{}'", name)); }
                for f in functions {
                    let key = format!("{}::{}", name, f.name);
                    if self.functions.insert(key, (f.params.iter().map(|p| p.ty.clone()).collect(), f.return_type.clone())).is_some() {
                        return Err(format!("Duplicate host function '{}.{}'", name, f.name));
                    }
                }
            }
        }
        // Events come from `on name(...)` blocks, wherever they are written.
        for stmt in stmts {
            let declaration = match &stmt.node {
                StmtNode::Expr(value) => Some(value),
                StmtNode::Let { value, .. } => Some(value),
                _ => None,
            };
            if let Some(crate::ast::Spanned { node: ExprNode::TriggeredBlock { event_name, params, .. }, .. }) = declaration {
                self.declared_events
                    .insert(event_name.clone(), params.iter().map(|p| p.ty.clone()).collect());
            }
        }
        // First pass: register all top-level declarations for forward references
        let mut orchestrators = HashSet::new();
        for stmt in stmts {
            match &stmt.node {
                StmtNode::OrchestratorDecl { name, .. } => {
                    if !orchestrators.insert(name.clone()) {
                        return Err(format!("orchestrator '{}' is declared more than once", name));
                    }
                }
                StmtNode::FnDecl { name, params, return_type, type_params, .. } => {
                    let param_types: Vec<Type> = params.iter().map(|p| p.ty.clone()).collect();
                    if self.functions.insert(name.clone(), (param_types, return_type.clone())).is_some() {
                        return Err(format!("'{}' is declared more than once", name));
                    }
                    if !type_params.is_empty() {
                        self.generic_functions.insert(name.clone(), type_params.clone());
                    }
                }
                StmtNode::TaskDecl { name, params, return_type, type_params, .. } => {
                    let param_types: Vec<Type> = params.iter().map(|p| p.ty.clone()).collect();
                    if self.functions.insert(name.clone(), (param_types, return_type.clone())).is_some() {
                        return Err(format!("'{}' is declared more than once", name));
                    }
                    if !type_params.is_empty() {
                        self.generic_functions.insert(name.clone(), type_params.clone());
                    }
                }
                StmtNode::ProcessDecl { name, params, return_type, type_params, .. } => {
                    let param_types: Vec<Type> = params.iter().map(|p| p.ty.clone()).collect();
                    if self.functions.insert(name.clone(), (param_types, return_type.clone())).is_some() {
                        return Err(format!("'{}' is declared more than once", name));
                    }
                    if !type_params.is_empty() {
                        self.generic_functions.insert(name.clone(), type_params.clone());
                    }
                }
                StmtNode::OrchestratorDecl { name, params, return_type, .. } => {
                    let param_types: Vec<Type> = params.iter().map(|p| p.ty.clone()).collect();
                    self.functions.insert(name.clone(), (param_types, return_type.clone()));
                }
                StmtNode::StructDef { name, fields } => {
                    self.struct_defs.insert(name.clone(), fields.clone());
                }
                StmtNode::EnumDef { name, variants } => {
                    let mut seen = HashSet::new();
                    for v in variants {
                        if !seen.insert(v.name.clone()) {
                            return Err(format!("enum '{}' declares variant '{}' more than once", name, v.name));
                        }
                    }
                    self.enum_defs.insert(name.clone(), variants.clone());
                }
                StmtNode::Serverlet { name, handlers, .. } if { self.serverlet_names.insert(name.clone()); false } => { let _ = handlers; }
                StmtNode::Serverlet { name, handlers, .. } => {
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
        self.module_aliases.insert(alias.to_string());
        for stmt in stmts {
            match &stmt.node {
                StmtNode::FnDecl { name, params, return_type, type_params, .. } => {
                    let param_types: Vec<Type> = params.iter().map(|p| p.ty.clone()).collect();
                    let full_name = format!("{}::{}", alias, name);
                    self.functions.insert(full_name.clone(), (param_types, return_type.clone()));
                    if !type_params.is_empty() {
                        self.generic_functions.insert(full_name, type_params.clone());
                    }
                }
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
                StmtNode::EnumDef { name, variants } => {
                    self.enum_defs.insert(name.clone(), variants.clone());
                }
                StmtNode::StructDef { name, fields } => {
                    self.struct_defs.insert(name.clone(), fields.clone());
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

    /// A foreign function with its own type parameters: calls infer them from the
    /// arguments, as calls to a generic `fn` do.
    pub fn register_generic_foreign_function(&mut self, alias: &str, name: &str, type_params: Vec<String>, params: Vec<Type>, ret_ty: Type) {
        let full_name = format!("{}::{}", alias, name);
        self.functions.insert(full_name.clone(), (params, ret_ty));
        self.generic_functions.insert(full_name, type_params);
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

    /// The type a top-level `let` was given or inferred, once `type_check` has run.
    pub fn global_var_type(&self, name: &str) -> Option<Type> {
        self.env.first()?.get(name).cloned()
    }

    fn substitute_type_params(&self, ty: &Type, subst: &HashMap<String, Type>) -> Type {
        match ty {
            Type::TypeParam(name) => subst.get(name).cloned().unwrap_or_else(|| ty.clone()),
            Type::Array(inner, dims) => Type::Array(Box::new(self.substitute_type_params(inner, subst)), dims.clone()),
            Type::Option(inner) => Type::Option(Box::new(self.substitute_type_params(inner, subst))),
            Type::Result(ok, err) => Type::Result(
                Box::new(self.substitute_type_params(ok, subst)),
                Box::new(self.substitute_type_params(err, subst)),
            ),
            Type::Fn(params, ret) => Type::Fn(
                params.iter().map(|t| self.substitute_type_params(t, subst)).collect(),
                Box::new(self.substitute_type_params(ret, subst)),
            ),
            other => other.clone(),
        }
    }

    fn unify_type_param(&self, param_ty: &Type, arg_ty: &Type, type_params: &[String], subst: &mut HashMap<String, Type>) {
        match param_ty {
            Type::TypeParam(name) if type_params.contains(name) => {
                subst.entry(name.clone()).or_insert_with(|| arg_ty.clone());
            }
            Type::Array(inner, _) => {
                if let Type::Array(arg_inner, _) = arg_ty {
                    self.unify_type_param(inner, arg_inner, type_params, subst);
                }
            }
            Type::Option(inner) => {
                if let Type::Option(arg_inner) = arg_ty {
                    self.unify_type_param(inner, arg_inner, type_params, subst);
                }
            }
            Type::Result(ok, err) => {
                if let Type::Result(arg_ok, arg_err) = arg_ty {
                    self.unify_type_param(ok, arg_ok, type_params, subst);
                    self.unify_type_param(err, arg_err, type_params, subst);
                }
            }
            Type::Fn(params, _) => {
                if let Type::Fn(arg_params, _) = arg_ty {
                    for (p, a) in params.iter().zip(arg_params.iter()) {
                        self.unify_type_param(p, a, type_params, subst);
                    }
                }
            }
            _ => {}
        }
    }

    fn check_stmt(&mut self, stmt: &Stmt) -> Result<(), String> {
        match &stmt.node {
            StmtNode::Let { name, ty, value, .. } => {
                if let ExprNode::StartServerlet { name: serverlet, .. } = &value.node {
                    self.var_serverlet.insert(name.clone(), serverlet.clone());
                }
                let val_ty = self.infer_expr(value)?;
                if let Some(expected_ty) = ty {
                    if !self.types_compatible(expected_ty, &val_ty) {
                        return Err(format!(
                            "line {}, col {}: type mismatch in let '{}': expected {}, found {}",
                            stmt.span.line, stmt.span.col, name,
                            expected_ty.display_name(), val_ty.display_name()
                        ));
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
                    let ret_ty = self.infer_expr(expr)?;
                    if let Some(Type::Void) = &self.current_return_type {
                        if ret_ty != Type::Void {
                            return Err(format!(
                                "line {}, col {}: this function returns nothing, so it cannot return {}",
                                stmt.span.line, stmt.span.col, ret_ty.display_name()
                            ));
                        }
                    }
                    if let Some(expected) = &self.current_return_type.clone() {
                        if !self.types_compatible(expected, &ret_ty) {
                            return Err(format!(
                                "line {}, col {}: return type mismatch: function returns {}, got {}",
                                stmt.span.line, stmt.span.col,
                                expected.display_name(), ret_ty.display_name()
                            ));
                        }
                    }
                }
            }
            StmtNode::FnDecl { params, body, return_type, type_params, .. } => {
                let prev_return = self.current_return_type.clone();
                self.current_return_type = Some(return_type.clone());
                self.push_env();
                for tp in type_params {
                    self.define_var(tp.clone(), Type::TypeParam(tp.clone()));
                }
                for p in params {
                    self.define_var(p.name.clone(), p.ty.clone());
                }
                self.infer_expr(body)?;
                self.pop_env();
                self.current_return_type = prev_return;
            }
            StmtNode::TaskDecl { params, body, return_type, type_params, .. } |
            StmtNode::ProcessDecl { params, body, return_type, type_params, .. } => {
                let prev_return = self.current_return_type.clone();
                self.current_return_type = Some(return_type.clone());
                self.push_env();
                for tp in type_params {
                    self.define_var(tp.clone(), Type::TypeParam(tp.clone()));
                }
                for p in params {
                    self.define_var(p.name.clone(), p.ty.clone());
                }
                self.infer_expr(body)?;
                self.pop_env();
                self.current_return_type = prev_return;
            }
            StmtNode::OrchestratorDecl { params, body, return_type, .. } => {
                let prev_return = self.current_return_type.clone();
                self.current_return_type = Some(return_type.clone());
                self.push_env();
                for p in params {
                    if let Type::Array(inner, initial) = &p.ty {
                        if **inner == Type::Process {
                            for worker in initial {
                                match self.lookup_var(worker) {
                                    Some(Type::Process) => {}
                                    Some(other) => return Err(format!(
                                        "line {}, col {}: process[...] lists the workers to start, but '{}' is {}, not a process. A process comes from an `automatic` or `on` block.",
                                        stmt.span.line, stmt.span.col, worker, other.display_name()
                                    )),
                                    None => return Err(format!(
                                        "line {}, col {}: process[...] names '{}', which is not declared",
                                        stmt.span.line, stmt.span.col, worker
                                    )),
                                }
                            }
                        }
                    }
                    self.define_var(p.name.clone(), p.ty.clone());
                }
                self.infer_expr(body)?;
                self.pop_env();
                self.current_return_type = prev_return;
            }
            StmtNode::ForIn { var, index_var, iter, body } => {
                let iter_ty = self.infer_expr(iter)?;
                let elem_ty = match &iter_ty {
                    Type::Array(inner, _) => *inner.clone(),
                    Type::Int | Type::Float | Type::Bool | Type::Str => {
                        return Err(format!(
                            "line {}, col {}: for loops over an array, got {}; use range(n) to count",
                            stmt.span.line, stmt.span.col, iter_ty.display_name()
                        ));
                    }
                    _ => {
                        // Allow unknown types (e.g. from range()) to produce Int
                        Type::Int
                    }
                };
                self.push_env();
                if let Some(idx) = index_var {
                    self.define_var(idx.clone(), Type::Int);
                }
                self.define_var(var.clone(), elem_ty);
                self.infer_expr(body)?;
                self.pop_env();
            }
            StmtNode::Trigger { event_name, args } => {
                let mut arg_types = Vec::new();
                for arg in args {
                    arg_types.push(self.infer_expr(arg)?);
                }
                if let Some(expected) = self.declared_events.get(event_name).cloned() {
                    if expected.len() != arg_types.len() {
                        return Err(format!(
                            "line {}, col {}: event '{}' takes {} argument{}, got {}",
                            stmt.span.line, stmt.span.col, event_name, expected.len(),
                            if expected.len() == 1 { "" } else { "s" }, arg_types.len()
                        ));
                    }
                    for (index, (want, got)) in expected.iter().zip(&arg_types).enumerate() {
                        if !self.types_compatible(want, got) {
                            return Err(format!(
                                "line {}, col {}: event '{}' argument {} expects {}, got {}",
                                stmt.span.line, stmt.span.col, event_name, index + 1,
                                want.display_name(), got.display_name()
                            ));
                        }
                    }
                } else if event_name != "update_orchestrator" {
                    let mut known: Vec<&String> = self.declared_events.keys().collect();
                    known.sort();
                    return Err(format!(
                        "line {}, col {}: no event '{}' is declared{}",
                        stmt.span.line, stmt.span.col, event_name,
                        if known.is_empty() { String::new() } else { format!(". Declared: {}.", known.iter().map(|k| k.as_str()).collect::<Vec<_>>().join(", ")) }
                    ));
                }
            }
            StmtNode::Parallel(stmts) => {
                for s in stmts {
                    self.check_stmt(s)?;
                }
            }
            StmtNode::While { cond, body } => {
                let cond_ty = self.infer_expr(cond)?;
                if cond_ty != Type::Bool {
                    return Err(format!("line {}, col {}: while condition must be bool, got {}", stmt.span.line, stmt.span.col, cond_ty.display_name()));
                }
                self.infer_expr(body)?;
            }
            StmtNode::Break | StmtNode::Continue => {}
            StmtNode::UseModule { .. } | StmtNode::Load { .. } | StmtNode::LoadForeign { .. } |
            StmtNode::StructDef { .. } | StmtNode::EnumDef { .. } => {}
            StmtNode::Serverlet { name: serverlet, state, handlers, crash_handler, landline, grants, sandbox, secret } => {
                let mut seen = HashSet::new();
                for grant in grants {
                    if !seen.insert(grant) || !self.functions.contains_key(&grant.replace(".", "::")) || !self.host_groups.contains(grant.split('.').next().unwrap()) {
                        return Err(format!("Unknown or duplicate host grant '{}'", grant));
                    }
                }
                // Two handlers with one name would be two variants of one enum in the
                // generated actor, which rustc would refuse in code the user never wrote.
                let mut names = HashSet::new();
                for h in handlers {
                    if !names.insert(&h.name) {
                        return Err(format!("serverlet '{}' declares handler '{}' more than once", serverlet, h.name));
                    }
                }
                if *secret {
                    let structs = self.struct_defs.iter().map(|(n, f)| (n.clone(), f.clone())).collect::<Vec<_>>();
                    if let Some(reason) = crate::codegen::stmt::wire_unsupported_reason(handlers, &structs) {
                        return Err(format!("secret serverlet '{}': {}", serverlet, reason));
                    }
                }
                if sandbox.is_some() {
                    // Checked here rather than in the guest's build, so `check` rejects
                    // it and the report is the compiler's own.
                    let structs = self.struct_defs.iter().map(|(n, f)| (n.clone(), f.clone())).collect::<Vec<_>>();
                    if let Some(reason) = crate::codegen::stmt::sandbox_unsupported_reason(serverlet, handlers, &structs) {
                        return Err(reason);
                    }
                }
                if landline.is_some() {
                    let structs = self.struct_defs.iter().map(|(n, f)| (n.clone(), f.clone())).collect::<Vec<_>>();
                    if let Some(reason) = crate::codegen::stmt::wire_unsupported_reason(handlers, &structs) {
                        return Err(reason.replace("secret serverlets", "landline serverlets"));
                    }
                }
                self.push_env();
                for s in state {
                    self.check_stmt(s)?;
                }
                // Recorded before the handlers run, so a handler's locals cannot be
                // mistaken for state. A name declared twice keeps its last type.
                let mut recorded: Vec<(String, Type)> = Vec::new();
                for s in state {
                    if let StmtNode::Let { name, .. } = &s.node {
                        if let Some(ty) = self.lookup_var(name) {
                            match recorded.iter_mut().find(|(existing, _)| existing == name) {
                                Some(entry) => entry.1 = ty,
                                None => recorded.push((name.clone(), ty)),
                            }
                        }
                    }
                }
                self.serverlet_state.insert(serverlet.clone(), recorded);
                if sandbox.is_some() {
                    self.sandbox_grants = Some((serverlet.clone(), grants.iter().cloned().collect()));
                }
                for h in handlers {
                    let prev_return = self.current_return_type.clone();
                    self.current_return_type = Some(h.return_type.clone());
                    self.push_env();
                    for p in &h.params {
                        self.define_var(p.name.clone(), p.ty.clone());
                    }
                    let checked = self.infer_expr(&h.body);
                    self.pop_env();
                    self.current_return_type = prev_return;
                    if let Err(error) = checked {
                        self.sandbox_grants = None;
                        return Err(error);
                    }
                }
                self.sandbox_grants = None;
                if let Some((err_name, handler_body)) = crash_handler {
                    self.push_env();
                    self.define_var(err_name.clone(), Type::Str);
                    self.infer_expr(handler_body)?;
                    self.pop_env();
                }
                self.pop_env();
            }
            StmtNode::Host { functions, .. } => {
                let structs = self.struct_defs.iter().map(|(n,f)| (n.clone(),f.clone())).collect::<Vec<_>>();
                if let Some(reason) = crate::codegen::stmt::wire_unsupported_reason(functions, &structs) { return Err(reason); }
            }
            StmtNode::OnTick { param, input, return_type, body } => {
                self.push_env();
                self.define_var(param.clone(), Type::Float);
                if let Some(input) = input { self.define_var(input.name.clone(), input.ty.clone()); }
                let old_return = self.current_return_type.replace(return_type.clone());
                let result = self.infer_expr(body);
                self.current_return_type = old_return;
                self.pop_env();
                let inferred = result?;
                if *return_type != Type::Void && inferred != Type::Void && !self.types_compatible(return_type, &inferred) { return Err("on_tick return type mismatch".into()); }
            }
            StmtNode::OnFixedTick { param, body } => {
                self.push_env();
                self.define_var(param.clone(), Type::Float);
                self.infer_expr(body)?;
                self.pop_env();
            }
            StmtNode::OnStart(expr) | StmtNode::OnStop(expr) => {
                self.infer_expr(expr)?;
            }
        }
        Ok(())
    }

    fn types_compatible(&self, expected: &Type, actual: &Type) -> bool {
        if expected == actual {
            return true;
        }
        // TypeParam is compatible with anything (generic bounds checked by Rust)
        if matches!(expected, Type::TypeParam(_)) || matches!(actual, Type::TypeParam(_)) {
            return true;
        }
        // none literal: Option<Void> is compatible with any option<T>
        if let (Type::Option(_), Type::Option(inner)) = (expected, actual) {
            if **inner == Type::Void { return true; }
        }
        // ok(x) leaves the error side unknown and err(e) the value side; an unknown side
        // (Void) matches anything, a known side must be compatible.
        if let (Type::Result(exp_ok, exp_err), Type::Result(act_ok, act_err)) = (expected, actual) {
            let ok = **act_ok == Type::Void || self.types_compatible(exp_ok, act_ok);
            let err = **act_err == Type::Void || self.types_compatible(exp_err, act_err);
            return ok && err;
        }
        if let (Type::Option(exp_inner), Type::Option(act_inner)) = (expected, actual) {
            return self.types_compatible(exp_inner, act_inner);
        }
        // Array covariance
        if let (Type::Array(exp_inner, _), Type::Array(act_inner, _)) = (expected, actual) {
            return self.types_compatible(exp_inner, act_inner);
        }
        // Void in if-else / try-catch: allow Void on either side when the other is also Void
        // (already handled by expected == actual check above)
        // But allow Void where Void is expected regardless of actual
        if *expected == Type::Void {
            return true;
        }
        false
    }

    fn infer_expr(&mut self, expr: &Expr) -> Result<Type, String> {
        let ty = self.infer_expr_inner(expr)?;
        self.type_map.insert((expr.span.line, expr.span.col), ty.clone());
        Ok(ty)
    }

    fn infer_expr_inner(&mut self, expr: &Expr) -> Result<Type, String> {
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
            ExprNode::Unary { op, operand } => {
                let ty = self.infer_expr(operand)?;
                match op {
                    crate::ast::UnaryOp::Neg if ty == Type::Int || ty == Type::Float => Ok(ty),
                    crate::ast::UnaryOp::Neg => Err(format!(
                        "line {}, col {}: cannot negate a value of type {}",
                        expr.span.line, expr.span.col, ty.display_name()
                    )),
                }
            }
            ExprNode::Index { object, index } => {
                let obj_ty = self.infer_expr(object)?;
                let idx_ty = self.infer_expr(index)?;
                if !self.types_compatible(&Type::Int, &idx_ty) {
                    return Err(format!(
                        "line {}, col {}: array index must be int, got {}",
                        expr.span.line, expr.span.col, idx_ty.display_name()
                    ));
                }
                match obj_ty {
                    Type::Array(inner, _) => Ok(*inner),
                    other => Err(format!(
                        "line {}, col {}: cannot index a value of type {}",
                        expr.span.line, expr.span.col, other.display_name()
                    )),
                }
            }
            ExprNode::Binary { op, lhs, rhs } => {
                let lhs_ty = self.infer_expr(lhs)?;
                let rhs_ty = self.infer_expr(rhs)?;

                if *op == BinaryOp::Assign {
                    if let ExprNode::Identifier(name) = &lhs.node {
                        if let Some(lhs_declared_ty) = self.lookup_var(name) {
                            if !self.types_compatible(&lhs_declared_ty, &rhs_ty) {
                                return Err(format!(
                                    "line {}, col {}: cannot assign {} to '{}' which has type {}",
                                    expr.span.line, expr.span.col,
                                    rhs_ty.display_name(), name, lhs_declared_ty.display_name()
                                ));
                            }
                        }
                    } else if let ExprNode::Index { .. } = &lhs.node {
                        if !self.types_compatible(&lhs_ty, &rhs_ty) {
                            return Err(format!(
                                "line {}, col {}: cannot assign {} to an array element of type {}",
                                expr.span.line, expr.span.col, rhs_ty.display_name(), lhs_ty.display_name()
                            ));
                        }
                    }
                    return Ok(Type::Void);
                }

                if [BinaryOp::Eq, BinaryOp::Ne, BinaryOp::Lt, BinaryOp::Gt, BinaryOp::Le, BinaryOp::Ge].contains(op) {
                    // Both sides have to be the same kind of thing, or rustc reports it as
                    // a missing PartialOrd impl on generated code.
                    if !self.types_compatible(&lhs_ty, &rhs_ty) && !self.types_compatible(&rhs_ty, &lhs_ty) {
                        return Err(format!(
                            "line {}, col {}: cannot compare {} with {}",
                            expr.span.line, expr.span.col,
                            lhs_ty.display_name(), rhs_ty.display_name()
                        ));
                    }
                    return Ok(Type::Bool);
                }
                if [BinaryOp::And, BinaryOp::Or].contains(op) {
                    if lhs_ty != Type::Bool || rhs_ty != Type::Bool {
                        return Err(format!(
                            "line {}, col {}: logical operators require bool operands, got {} and {}",
                            expr.span.line, expr.span.col, lhs_ty.display_name(), rhs_ty.display_name()
                        ));
                    }
                    return Ok(Type::Bool);
                }
                if lhs_ty == Type::Int && rhs_ty == Type::Int {
                    Ok(Type::Int)
                } else if lhs_ty == Type::Float && rhs_ty == Type::Float {
                    Ok(Type::Float)
                } else if *op == BinaryOp::Add && (lhs_ty == Type::Str || rhs_ty == Type::Str) {
                    Ok(Type::Str)
                } else {
                    Err(format!(
                        "line {}, col {}: type mismatch in binary operation: {} and {}",
                        expr.span.line, expr.span.col, lhs_ty.display_name(), rhs_ty.display_name()
                    ))
                }
            }
            ExprNode::Call { callee, args } => {
                let mut arg_types = Vec::new();
                for a in args {
                    arg_types.push(self.infer_expr(a)?);
                }

                // Check if callee is a closure variable
                if let Some(var_ty) = self.lookup_var(callee) {
                    if let Type::Fn(param_types, ret_ty) = &var_ty {
                        if param_types.len() != args.len() {
                            return Err(format!(
                                "line {}, col {}: closure '{}' expects {} arguments, got {}",
                                expr.span.line, expr.span.col, callee, param_types.len(), args.len()
                            ));
                        }
                        return Ok(*ret_ty.clone());
                    }
                }

                // Higher-order builtins
                match callee.as_str() {
                    "range" => {
                        return Ok(Type::Array(Box::new(Type::Int), vec![]));
                    }
                    "to_string" => {
                        return Ok(Type::Str);
                    }
                    "clock_micros" if args.is_empty() => {
                        return Ok(Type::Int);
                    }
                    "to_int" if args.len() == 1 => {
                        return Ok(Type::Int);
                    }
                    "to_float" if args.len() == 1 => {
                        return Ok(Type::Float);
                    }
                    "parse_int" if args.len() == 1 => {
                        return Ok(Type::Result(Box::new(Type::Int), Box::new(Type::Str)));
                    }
                    "parse_float" if args.len() == 1 => {
                        return Ok(Type::Result(Box::new(Type::Float), Box::new(Type::Str)));
                    }
                    "map" if args.len() == 2 => {
                        // map(xs: T[], f: fn(T) -> U) -> U[]
                        let f_ty = &arg_types[1];
                        let elem_ret = if let Type::Fn(_, ret) = f_ty { *ret.clone() } else { Type::Void };
                        return Ok(Type::Array(Box::new(elem_ret), vec![]));
                    }
                    "filter" if args.len() == 2 => {
                        // filter(xs: T[], f: fn(T) -> bool) -> T[]
                        if let Type::Array(inner, dims) = &arg_types[0] {
                            return Ok(Type::Array(inner.clone(), dims.clone()));
                        }
                        return Ok(Type::Array(Box::new(Type::Void), vec![]));
                    }
                    "reduce" if args.len() == 3 => {
                        // reduce(xs, init, f) -> type of init
                        return Ok(arg_types[1].clone());
                    }
                    "find" if args.len() == 2 => {
                        // find(xs: T[], f) -> option<T>
                        if let Type::Array(inner, _) = &arg_types[0] {
                            return Ok(Type::Option(inner.clone()));
                        }
                        return Ok(Type::Option(Box::new(Type::Void)));
                    }
                    "any" | "all" if args.len() == 2 => {
                        return Ok(Type::Bool);
                    }
                    "length" if args.len() == 1 => {
                        if let Type::Array(_, _) = &arg_types[0] {
                            return Ok(Type::Int);
                        }
                        return Err(format!("line {}, col {}: length() expects an array, got {}", expr.span.line, expr.span.col, arg_types[0].display_name()));
                    }
                    "append" if args.len() == 2 => {
                        if let Type::Array(inner_ty, _) = &arg_types[0] {
                            let inner_ty = inner_ty.clone();
                            if !self.types_compatible(&inner_ty, &arg_types[1]) {
                                return Err(format!(
                                    "line {}, col {}: append() element type {} does not match array element type {}",
                                    expr.span.line, expr.span.col, arg_types[1].display_name(), inner_ty.display_name()
                                ));
                            }
                            return Ok(Type::Void);
                        }
                        return Err(format!("line {}, col {}: append() expects an array as first argument", expr.span.line, expr.span.col));
                    }
                    "remove" if args.len() == 2 => {
                        if let Type::Array(_, _) = &arg_types[0] {
                            if arg_types[1] != Type::Int {
                                return Err(format!("line {}, col {}: remove() expects integer index, got {}", expr.span.line, expr.span.col, arg_types[1].display_name()));
                            }
                            return Ok(Type::Void);
                        }
                        return Err(format!("line {}, col {}: remove() expects an array as first argument", expr.span.line, expr.span.col));
                    }
                    _ => {}
                }

                // Generic function substitution
                if let Some(type_params) = self.generic_functions.get(callee).cloned() {
                    if let Some((param_types, ret_ty)) = self.functions.get(callee).cloned() {
                        let mut subst = HashMap::new();
                        for (param_ty, arg_ty) in param_types.iter().zip(arg_types.iter()) {
                            self.unify_type_param(param_ty, arg_ty, &type_params, &mut subst);
                        }
                        let concrete_ret = self.substitute_type_params(&ret_ty, &subst);
                        return Ok(concrete_ret);
                    }
                }

                if let Some((expected_args, ret_ty)) = self.functions.get(callee).cloned() {
                    if expected_args.len() != args.len() {
                        return Err(format!("line {}, col {}: {} expects {} arguments, got {}", expr.span.line, expr.span.col, callee, expected_args.len(), args.len()));
                    }
                    for (index, (expected, actual)) in expected_args.iter().zip(&arg_types).enumerate() {
                        if !self.types_compatible(expected, actual) {
                            return Err(format!(
                                "line {}, col {}: {} argument {} expects {}, got {}",
                                expr.span.line, expr.span.col, callee, index + 1,
                                expected.display_name(), actual.display_name()
                            ));
                        }
                    }
                    Ok(ret_ty)
                } else if self.exempt_functions.contains(callee) {
                    Ok(Type::Void)
                } else {
                    Err(format!(
                        "line {}, col {}: unknown function '{}'",
                        expr.span.line, expr.span.col, callee
                    ))
                }
            }
            ExprNode::Closure { params, return_type, body } => {
                let param_types: Vec<Type> = params.iter().map(|p| p.ty.clone()).collect();
                let declared_ret = return_type.clone().unwrap_or(Type::Void);

                self.push_env();
                for p in params {
                    self.define_var(p.name.clone(), p.ty.clone());
                }
                self.infer_expr(body)?;
                self.pop_env();

                Ok(Type::Fn(param_types, Box::new(declared_ret)))
            }
            ExprNode::StringInterp { parts } => {
                for part in parts {
                    if let StringPart::Expr(e) = part {
                        self.infer_expr(e)?;
                    }
                }
                Ok(Type::Str)
            }
            ExprNode::Pipeline { value, function } => {
                let val_ty = self.infer_expr(value)?;
                match &function.node {
                    ExprNode::Call { callee, args } => {
                        for a in args {
                            self.infer_expr(a)?;
                        }
                        if let Some((expected_args, ret_ty)) = self.functions.get(callee).cloned() {
                            let effective_arg_count = args.len() + 1;
                            if expected_args.len() != effective_arg_count {
                                return Err(format!(
                                    "line {}, col {}: {} expects {} arguments, got {} (including piped value)",
                                    expr.span.line, expr.span.col, callee, expected_args.len(), effective_arg_count
                                ));
                            }
                            if !self.generic_functions.contains_key(callee) && !self.types_compatible(&expected_args[0], &val_ty) {
                                return Err(format!(
                                    "line {}, col {}: {} expects {} as its piped first argument, got {}",
                                    expr.span.line, expr.span.col, callee, expected_args[0].display_name(), val_ty.display_name()
                                ));
                            }
                            Ok(ret_ty)
                        } else {
                            Ok(Type::Void)
                        }
                    }
                    ExprNode::Identifier(name) => {
                        if let Some((expected_args, ret_ty)) = self.functions.get(name).cloned() {
                            if expected_args.len() != 1 {
                                return Err(format!(
                                    "line {}, col {}: {} expects {} arguments, got 1 (piped value)",
                                    expr.span.line, expr.span.col, name, expected_args.len()
                                ));
                            }
                            if !self.generic_functions.contains_key(name) && !self.types_compatible(&expected_args[0], &val_ty) {
                                return Err(format!(
                                    "line {}, col {}: {} expects {} as its piped argument, got {}",
                                    expr.span.line, expr.span.col, name, expected_args[0].display_name(), val_ty.display_name()
                                ));
                            }
                            Ok(ret_ty)
                        } else {
                            Ok(Type::Void)
                        }
                    }
                    _ => self.infer_expr(function),
                }
            }
            ExprNode::Block(stmts) => {
                self.push_env();
                let mut last_ty = Type::Void;
                for (i, stmt) in stmts.iter().enumerate() {
                    self.check_stmt(stmt)?;
                    if i == stmts.len() - 1 {
                        if let StmtNode::Expr(e) = &stmt.node {
                            last_ty = self.infer_expr(e)?;
                        }
                    }
                }
                self.pop_env();
                Ok(last_ty)
            }
            ExprNode::If { cond, then_branch, else_branch } => {
                let cond_ty = self.infer_expr(cond)?;
                if cond_ty != Type::Bool {
                    return Err(format!("line {}, col {}: if condition must be bool, got {}", expr.span.line, expr.span.col, cond_ty.display_name()));
                }
                let then_ty = self.infer_expr(then_branch)?;
                if let Some(eb) = else_branch {
                    let else_ty = self.infer_expr(eb)?;
                    // Both branches must be compatible; ignore if either is Void (statement context)
                    if then_ty != Type::Void && else_ty != Type::Void && !self.types_compatible(&then_ty, &else_ty) {
                        return Err(format!(
                            "line {}, col {}: if-else branches return different types: then={}, else={}",
                            expr.span.line, expr.span.col, then_ty.display_name(), else_ty.display_name()
                        ));
                    }
                    if then_ty != Type::Void { return Ok(then_ty); }
                    return Ok(else_ty);
                }
                Ok(then_ty)
            }
            ExprNode::ModuleCall { module_local_name, function, args } => {
                let mut arg_types = Vec::new();
                for arg in args {
                    arg_types.push(self.infer_expr(arg)?);
                }

                // `c.add(1)` where `c` came from `start Counter()`: the handler has to exist.
                if let Some(serverlet) = self.var_serverlet.get(module_local_name).cloned() {
                    let mut candidates = vec![format!("{}::{}", serverlet, function)];
                    if let Some((alias, bare)) = serverlet.rsplit_once("::") {
                        candidates.push(format!("{}::{}", alias, function));
                        candidates.push(format!("{}::{}", bare, function));
                    }
                    let Some(key) = candidates.iter().find(|key| self.functions.contains_key(*key)).cloned() else {
                        let prefix = format!("{}::", serverlet.rsplit_once("::").map_or(serverlet.as_str(), |(_, bare)| bare));
                        let mut handlers: Vec<String> = self
                            .functions
                            .keys()
                            .filter_map(|key| key.strip_prefix(&prefix).map(str::to_string))
                            .collect();
                        handlers.sort();
                        return Err(format!(
                            "line {}, col {}: serverlet '{}' has no handler '{}'{}",
                            expr.span.line, expr.span.col, serverlet, function,
                            if handlers.is_empty() { String::new() } else { format!(". It has: {}.", handlers.join(", ")) }
                        ));
                    };
                    let (expected_args, ret_ty) = self.functions.get(&key).cloned().unwrap();
                    if expected_args.len() != args.len() {
                        return Err(format!(
                            "line {}, col {}: {}.{}() expects {} arguments, got {}",
                            expr.span.line, expr.span.col, module_local_name, function, expected_args.len(), args.len()
                        ));
                    }
                    for (i, (expected, actual)) in expected_args.iter().zip(&arg_types).enumerate() {
                        if !self.types_compatible(expected, actual) {
                            return Err(format!(
                                "line {}, col {}: {}.{}() argument {} expects {}, got {}",
                                expr.span.line, expr.span.col, module_local_name, function, i + 1,
                                expected.display_name(), actual.display_name()
                            ));
                        }
                    }
                    return Ok(ret_ty);
                }

                let alias_key = format!("{}::{}", module_local_name, function);
                if let Some((expected_args, ret_ty)) = self.functions.get(&alias_key).cloned() {
                    if expected_args.len() != args.len() {
                        return Err(format!(
                            "line {}, col {}: {}.{}() expects {} arguments, got {}",
                            expr.span.line, expr.span.col, module_local_name, function, expected_args.len(), args.len()
                        ));
                    }
                    for (i, (expected, actual)) in expected_args.iter().zip(&arg_types).enumerate() {
                        if !self.types_compatible(expected, actual) {
                            if self.host_groups.contains(module_local_name) {
                                return Err(format!("Host argument type mismatch for {}.{}", module_local_name, function));
                            }
                            return Err(format!(
                                "line {}, col {}: {}.{}() argument {} expects {}, got {}",
                                expr.span.line, expr.span.col, module_local_name, function, i + 1,
                                expected.display_name(), actual.display_name()
                            ));
                        }
                    }
                    if self.host_groups.contains(module_local_name) {
                        if let Some((serverlet, granted)) = &self.sandbox_grants {
                            let call = format!("{}.{}", module_local_name, function);
                            if !granted.contains(&call) {
                                return Err(format!(
                                    "line {}, col {}: sandboxed serverlet '{}' calls {}, which it was not granted; a sandboxed guest reaches only the host functions its declaration grants — add `grant call {}` to the serverlet or remove the call",
                                    expr.span.line, expr.span.col, serverlet, call, call
                                ));
                            }
                        }
                    }
                    if let Some(type_params) = self.generic_functions.get(&alias_key).cloned() {
                        let mut subst = HashMap::new();
                        for (param_ty, arg_ty) in expected_args.iter().zip(arg_types.iter()) {
                            self.unify_type_param(param_ty, arg_ty, &type_params, &mut subst);
                        }
                        return Ok(self.substitute_type_params(&ret_ty, &subst));
                    }
                    return Ok(ret_ty);
                }
                if self.host_groups.contains(module_local_name) { return Err(format!("Unknown host function {}.{}", module_local_name, function)); }
                if self.module_aliases.contains(module_local_name) {
                    let prefix = format!("{}::", module_local_name);
                    let mut known: Vec<String> = self.functions.keys().filter_map(|key| key.strip_prefix(&prefix).map(str::to_string)).collect();
                    known.sort();
                    return Err(format!(
                        "line {}, col {}: module '{}' has no function '{}'{}",
                        expr.span.line, expr.span.col, module_local_name, function,
                        if known.is_empty() { String::new() } else { format!(". It has: {}.", known.join(", ")) }
                    ));
                }

                let suffix = format!("::{}", function);
                let mut found_ret = None;
                for (key, (expected_args, ret_ty)) in self.functions.iter() {
                    if key.ends_with(&suffix) && expected_args.len() == args.len() {
                        found_ret = Some(ret_ty.clone());
                        break;
                    }
                }
                if let Some(ret_ty) = found_ret {
                    return Ok(ret_ty);
                }

                if !self.exempt_functions.contains(function.as_str()) {
                    eprintln!("[orchestrate] warning: unknown method call '.{}()' on '{}' — if this is a serverlet client method or foreign function, this warning can be ignored", function, module_local_name);
                }
                Ok(Type::Void)
            }
            ExprNode::StartServerlet { name, args } => {
                for arg in args {
                    self.infer_expr(arg)?;
                }
                if !args.is_empty() {
                    return Err(format!(
                        "line {}, col {}: start {}() takes no arguments; a serverlet's state is declared inside it, not passed in",
                        expr.span.line, expr.span.col, name
                    ));
                }
                // A name with `::` came from a module, whose declarations are registered
                // under an alias rather than here.
                if !name.contains("::") && !self.serverlet_names.contains(name) {
                    let mut known: Vec<&String> = self.serverlet_names.iter().collect();
                    known.sort();
                    return Err(format!(
                        "line {}, col {}: unknown serverlet '{}'{}",
                        expr.span.line, expr.span.col, name,
                        if known.is_empty() { String::new() } else { format!(". This file declares: {}.", known.iter().map(|n| n.as_str()).collect::<Vec<_>>().join(", ")) }
                    ));
                }
                Ok(Type::Process)
            }
            ExprNode::AutomaticBlock { body, crash_handler, .. } => {
                self.infer_expr(body)?;
                if let Some((err_name, handler)) = crash_handler {
                    self.push_env();
                    self.define_var(err_name.clone(), Type::Str);
                    self.infer_expr(handler)?;
                    self.pop_env();
                }
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
                let target_ty = self.infer_expr(target)?;
                if !matches!(target_ty, Type::Process | Type::TypeParam(_) | Type::Void) {
                    return Err(format!(
                        "line {}, col {}: start expects a process, got {}",
                        expr.span.line, expr.span.col, target_ty.display_name()
                    ));
                }
                Ok(Type::Process)
            }
            ExprNode::ArrayLiteral(elements) => {
                let mut inner_ty: Option<Type> = None;
                for (i, e) in elements.iter().enumerate() {
                    let ty = self.infer_expr(e)?;
                    if let Some(ref prev) = inner_ty.clone() {
                        if !self.types_compatible(prev, &ty) {
                            return Err(format!(
                                "line {}, col {}: array element {} has type {}, expected {}",
                                expr.span.line, expr.span.col, i, ty.display_name(), prev.display_name()
                            ));
                        }
                    } else {
                        inner_ty = Some(ty);
                    }
                }
                Ok(Type::Array(Box::new(inner_ty.unwrap_or(Type::Int)), vec![]))
            }
            ExprNode::StructLiteral { name, fields } => {
                if let Some(def) = self.struct_defs.get(name).cloned() {
                    let missing: Vec<String> = def
                        .iter()
                        .filter(|(field, _)| !fields.iter().any(|(given, _)| given == field))
                        .map(|(field, _)| field.clone())
                        .collect();
                    if !missing.is_empty() {
                        return Err(format!(
                            "line {}, col {}: struct '{}' is missing field{} {}",
                            expr.span.line, expr.span.col, name,
                            if missing.len() == 1 { "" } else { "s" },
                            missing.join(", ")
                        ));
                    }
                }
                let def = self.struct_defs.get(name).cloned()
                    .ok_or_else(|| format!("line {}, col {}: unknown struct '{}'", expr.span.line, expr.span.col, name))?;
                for (fname, fexpr) in fields {
                    let actual_ty = self.infer_expr(fexpr)?;
                    if let Some((_, expected_ty)) = def.iter().find(|(n, _)| n == fname) {
                        if !self.types_compatible(expected_ty, &actual_ty) {
                            return Err(format!(
                                "line {}, col {}: field '{}' of struct '{}': expected {}, found {}",
                                expr.span.line, expr.span.col, fname, name, expected_ty.display_name(), actual_ty.display_name()
                            ));
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
                    Err(format!("line {}, col {}: field access '.{}' on non-struct type {}", expr.span.line, expr.span.col, field, obj_ty.display_name()))
                }
            }
            ExprNode::NoneLiteral => Ok(Type::Option(Box::new(Type::Void))),
            ExprNode::SomeLiteral(inner) => {
                let inner_ty = self.infer_expr(inner)?;
                Ok(Type::Option(Box::new(inner_ty)))
            }
            ExprNode::OkLiteral(inner) => {
                let inner_ty = self.infer_expr(inner)?;
                Ok(Type::Result(Box::new(inner_ty), Box::new(Type::Void)))
            }
            ExprNode::ErrLiteral(value) => {
                let err_ty = self.infer_expr(value)?;
                Ok(Type::Result(Box::new(Type::Void), Box::new(err_ty)))
            }
            ExprNode::Propagate(inner) => {
                let inner_ty = self.infer_expr(inner)?;
                if self.try_errors.is_empty() {
                    if let Some(returns) = &self.current_return_type {
                        if !matches!(returns, Type::Result(_, _) | Type::Option(_) | Type::TypeParam(_)) {
                            return Err(format!(
                                "line {}, col {}: '?' needs a function that returns result or option, or a try block; this one returns {}",
                                expr.span.line, expr.span.col, returns.display_name()
                            ));
                        }
                    }
                }
                match &inner_ty {
                    Type::Result(ok_ty, err_ty) => {
                        if let Some(collector) = self.try_errors.last_mut() {
                            // Inside a try block the error goes to catch, not to the caller.
                            if **err_ty != Type::Void && !collector.contains(err_ty) { collector.push((**err_ty).clone()); }
                        } else if let Some(Type::Result(_, returned)) = self.current_return_type.clone() {
                            let known = **err_ty != Type::Void && *returned != Type::Void
                                && !matches!(*returned, Type::TypeParam(_)) && !matches!(**err_ty, Type::TypeParam(_));
                            if known && !self.types_compatible(&returned, err_ty) {
                                return Err(format!(
                                    "line {}, col {}: '?' propagates a {} error, but the function returns {}",
                                    expr.span.line, expr.span.col, err_ty.display_name(),
                                    self.current_return_type.as_ref().map(|t| t.display_name()).unwrap_or_default()
                                ));
                            }
                        }
                        Ok(*ok_ty.clone())
                    }
                    Type::Option(inner_opt) => Ok(*inner_opt.clone()),
                    _ => Err(format!(
                        "line {}, col {}: '?' operator can only be used on result<T> or option<T>, got {}",
                        expr.span.line, expr.span.col, inner_ty.display_name()
                    )),
                }
            }
            ExprNode::TryCatch { body, err_name, err_type, handler } => {
                self.try_errors.push(Vec::new());
                let body_result = self.infer_expr(body);
                let propagated = self.try_errors.pop().unwrap_or_default();
                let body_ty = body_result?;
                let unwrapped_ty = match &body_ty {
                    Type::Result(inner, _) => *inner.clone(),
                    Type::Option(inner) => *inner.clone(),
                    other => other.clone(),
                };
                let error_ty = match err_type {
                    Some(declared) => {
                        if let Some(other) = propagated.iter().find(|t| !self.types_compatible(declared, t)) {
                            return Err(format!(
                                "line {}, col {}: catch {}: {} but the block propagates a {} error",
                                expr.span.line, expr.span.col, err_name, declared.display_name(), other.display_name()
                            ));
                        }
                        declared.clone()
                    }
                    None => match propagated.as_slice() {
                        [] => Type::Str,
                        [only] if *only == Type::Str => Type::Str,
                        [only] => return Err(format!(
                            "line {}, col {}: the try block propagates a {} error; write `catch {}: {}`",
                            expr.span.line, expr.span.col, only.display_name(), err_name, only.display_name()
                        )),
                        _ => return Err(format!(
                            "line {}, col {}: the try block propagates more than one error type ({}); a try block handles one",
                            expr.span.line, expr.span.col,
                            propagated.iter().map(|t| t.display_name()).collect::<Vec<_>>().join(", ")
                        )),
                    },
                };
                self.push_env();
                self.define_var(err_name.clone(), error_ty);
                let handler_ty = self.infer_expr(handler)?;
                self.pop_env();
                if unwrapped_ty != Type::Void && handler_ty != Type::Void
                    && !self.types_compatible(&unwrapped_ty, &handler_ty) {
                    return Err(format!(
                        "line {}, col {}: try and catch branches return different types: {} vs {}",
                        expr.span.line, expr.span.col, unwrapped_ty.display_name(), handler_ty.display_name()
                    ));
                }
                Ok(unwrapped_ty)
            }
            ExprNode::EnumVariantLiteral { enum_name, variant_name, payload } => {
                let variants = self.enum_defs.get(enum_name).cloned()
                    .ok_or_else(|| format!("line {}, col {}: unknown enum '{}'", expr.span.line, expr.span.col, enum_name))?;
                let variant = variants.iter().find(|v| v.name == *variant_name)
                    .ok_or_else(|| format!("line {}, col {}: enum '{}' has no variant '{}'", expr.span.line, expr.span.col, enum_name, variant_name))?;
                match (&variant.payload.clone(), payload) {
                    (Some(expected_ty), Some(payload_expr)) => {
                        let actual_ty = self.infer_expr(payload_expr)?;
                        if !self.types_compatible(&expected_ty, &actual_ty) {
                            return Err(format!(
                                "line {}, col {}: variant '{}::{}' payload expected {}, got {}",
                                expr.span.line, expr.span.col, enum_name, variant_name, expected_ty.display_name(), actual_ty.display_name()
                            ));
                        }
                    }
                    (None, Some(_)) => {
                        return Err(format!("line {}, col {}: variant '{}::{}' does not take a payload", expr.span.line, expr.span.col, enum_name, variant_name));
                    }
                    (Some(expected_ty), None) => {
                        return Err(format!("line {}, col {}: variant '{}::{}' requires a payload of type {}", expr.span.line, expr.span.col, enum_name, variant_name, expected_ty.display_name()));
                    }
                    (None, None) => {}
                }
                Ok(Type::Named(enum_name.clone()))
            }
            ExprNode::Match { value, arms } => {
                let value_ty = self.infer_expr(value)?;

                // Collect enum name for exhaustiveness checking
                let enum_name_for_check = if let Type::Named(n) = &value_ty {
                    if self.enum_defs.contains_key(n) { Some(n.clone()) } else { None }
                } else { None };

                let mut covered_variants: HashSet<String> = HashSet::new();
                let mut has_wildcard = false;
                let mut result_ty: Option<Type> = None;

                for arm in arms {
                    self.push_env();
                    match &arm.pattern {
                        MatchPattern::EnumVariant { enum_name, variant_name, binding } => {
                            covered_variants.insert(variant_name.clone());
                            // The variant has to exist, and its shape has to match the
                            // pattern, before anything reaches rustc.
                            if let Some(variants) = self.enum_defs.get(enum_name) {
                                match variants.iter().find(|v| v.name == *variant_name) {
                                    None => return Err(format!(
                                        "enum '{}' has no variant '{}'. It has: {}.",
                                        enum_name,
                                        variant_name,
                                        variants.iter().map(|v| v.name.clone()).collect::<Vec<_>>().join(", ")
                                    )),
                                    Some(v) if v.payload.is_some() && binding.is_none() => return Err(format!(
                                        "enum variant '{}::{}' carries a value, so the pattern must bind it, as '{}::{}(value)'.",
                                        enum_name, variant_name, enum_name, variant_name
                                    )),
                                    Some(_) => {}
                                }
                            }
                            if let Some(binding_name) = binding {
                                let payload_ty = match (enum_name.as_str(), variant_name.as_str()) {
                                    ("option", "Some") => {
                                        if let Type::Option(inner) = &value_ty { Some(*inner.clone()) }
                                        else { Some(Type::Void) }
                                    }
                                    ("result", "Ok") => {
                                        if let Type::Result(ok, _) = &value_ty { Some(*ok.clone()) }
                                        else { Some(Type::Void) }
                                    }
                                    ("result", "Err") => {
                                        if let Type::Result(_, err) = &value_ty { Some(*err.clone()) }
                                        else { Some(Type::Str) }
                                    }
                                    _ => {
                                        if let Some(variants) = self.enum_defs.get(enum_name).cloned() {
                                            match variants.iter().find(|v| v.name == *variant_name) {
                                                Some(v) if v.payload.is_some() => v.payload.clone(),
                                                // Binding a variant that carries nothing used to reach
                                                // rustc as "expected tuple variant, found unit variant".
                                                Some(_) => return Err(format!(
                                                    "enum variant '{}::{}' carries no value, so the pattern cannot bind '{}'. Match it as '{}::{}' instead.",
                                                    enum_name, variant_name, binding_name, enum_name, variant_name
                                                )),
                                                None => return Err(format!(
                                                    "enum '{}' has no variant '{}'",
                                                    enum_name, variant_name
                                                )),
                                            }
                                        } else { None }
                                    }
                                };
                                if let Some(ty) = payload_ty {
                                    self.define_var(binding_name.clone(), ty);
                                }
                            }
                        }
                        MatchPattern::Wildcard => { has_wildcard = true; }
                        MatchPattern::Literal(_) => {}
                        MatchPattern::Binding(name) => {
                            self.define_var(name.clone(), value_ty.clone());
                        }
                        MatchPattern::Guard { inner, condition } => {
                            // Apply the inner pattern's bindings first
                            match inner.as_ref() {
                                MatchPattern::Binding(name) => {
                                    self.define_var(name.clone(), value_ty.clone());
                                }
                                MatchPattern::Wildcard => { has_wildcard = true; }
                                _ => {}
                            }
                            // Check the guard condition is bool
                            let cond_ty = self.infer_expr(condition)?;
                            if cond_ty != Type::Bool && cond_ty != Type::Void {
                                return Err(format!(
                                    "line {}, col {}: match guard condition must be bool, got {}",
                                    expr.span.line, expr.span.col, cond_ty.display_name()
                                ));
                            }
                        }
                    }
                    let arm_ty = self.infer_expr(&arm.body)?;
                    self.pop_env();

                    if let Some(prev_ty) = &result_ty {
                        if arm_ty != Type::Void && !self.types_compatible(prev_ty, &arm_ty) {
                            return Err(format!(
                                "line {}, col {}: match arms return different types: {} vs {}",
                                expr.span.line, expr.span.col, prev_ty.display_name(), arm_ty.display_name()
                            ));
                        }
                    } else {
                        result_ty = Some(arm_ty);
                    }
                }

                // Exhaustiveness check
                if !has_wildcard {
                    if let Some(enum_name) = enum_name_for_check {
                        if let Some(variants) = self.enum_defs.get(&enum_name).cloned() {
                            let missing: Vec<String> = variants.iter()
                                .filter(|v| !covered_variants.contains(&v.name))
                                .map(|v| v.name.clone())
                                .collect();
                            if !missing.is_empty() {
                                return Err(format!(
                                    "line {}, col {}: non-exhaustive match on '{}': missing variants: {}",
                                    expr.span.line, expr.span.col, enum_name, missing.join(", ")
                                ));
                            }
                        }
                    }
                }

                Ok(result_ty.unwrap_or(Type::Void))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;
    use crate::parser::Parser;

    fn check(src: &str) -> Result<(), String> {
        let mut lexer = Lexer::new(src);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let stmts = parser.parse().unwrap();
        let mut tc = TypeChecker::new();
        tc.type_check(&stmts)
    }

    #[test]
    fn test_typecheck_let() {
        assert!(check("let x = 5; let y: int = x;").is_ok());
    }

    #[test]
    fn test_typecheck_error() {
        assert!(check("let x = 5; let y: string = x;").is_err());
    }

    #[test]
    fn test_assign_type_mismatch_caught() {
        assert!(check("let x: int = 5; x = \"hello\";").is_err());
    }

    #[test]
    fn test_assign_same_type_ok() {
        assert!(check("let x: int = 5; x = 10;").is_ok());
    }

    #[test]
    fn test_if_else_branch_mismatch_caught() {
        assert!(check("let x = if true { 1 } else { \"a\" };").is_err());
    }

    #[test]
    fn test_if_else_same_type_ok() {
        assert!(check("let x = if true { 1 } else { 2 };").is_ok());
    }

    #[test]
    fn test_array_mixed_types_caught() {
        assert!(check("let a = [1, \"hello\"];").is_err());
    }

    #[test]
    fn test_array_consistent_types_ok() {
        assert!(check("let a = [1, 2, 3];").is_ok());
    }

    #[test]
    fn test_logical_ops_ok() {
        assert!(check("let x = true && false;").is_ok());
        assert!(check("let y = true || false;").is_ok());
    }

    #[test]
    fn test_logical_ops_wrong_type() {
        assert!(check("let x = 1 && 2;").is_err());
    }

    #[test]
    fn test_option_some_infers_ok() {
        assert!(check("let x: option<int> = some(5);").is_ok());
    }

    #[test]
    fn test_none_literal_ok() {
        assert!(check("let x: option<int> = none;").is_ok());
    }

    #[test]
    fn test_result_ok_infers() {
        assert!(check("fn f() -> result<int> { ok(1) }").is_ok());
    }

    #[test]
    fn test_result_err_infers() {
        assert!(check("fn f() -> result<int> { err(\"bad\") }").is_ok());
    }

    // A single uppercase letter reads as a type parameter, so these enums have real names,
    // and the checks go through `return`, the one place a body is compared to its type.
    #[test]
    fn test_result_error_type_defaults_to_string_and_may_be_declared() {
        assert!(check("enum Failure { Bad } fn f() -> result<int, Failure> { return err(Failure::Bad) }").is_ok());
        assert!(check("enum Failure { Bad } fn f() -> result<int, Failure> { return err(\"bad\") }").is_err());
        assert!(check("fn f() -> result<int> { return err(\"bad\") } fn g() -> result<int, string> { return f() }").is_ok());
        assert!(check("fn f() -> result<int> { return err(1) }").is_err());
    }

    #[test]
    fn test_propagate_requires_matching_error_type() {
        assert!(check("enum Failure { Bad } fn g() -> result<int, Failure> { return err(Failure::Bad) } fn f() -> result<int, Failure> { let x = g()? return ok(x) }").is_ok());
        let error = check("enum Failure { Bad } fn g() -> result<int, Failure> { return err(Failure::Bad) } fn f() -> result<int> { let x = g()? return ok(x) }").unwrap_err();
        assert!(error.contains("propagates a Failure error"), "{error}");
    }

    #[test]
    fn test_try_catch_binds_the_propagated_error_type() {
        let g = "enum Failure { Bad } fn g() -> result<int, Failure> { return err(Failure::Bad) } ";
        assert!(check(&format!("{g}fn f() -> int {{ return try {{ g()? }} catch e: Failure {{ 0 }} }}")).is_ok());
        let error = check(&format!("{g}fn f() -> int {{ return try {{ g()? }} catch e {{ 0 }} }}")).unwrap_err();
        assert!(error.contains("write `catch e: Failure`"), "{error}");
        let error = check(&format!("{g}fn f() -> int {{ return try {{ g()? }} catch e: string {{ 0 }} }}")).unwrap_err();
        assert!(error.contains("propagates a Failure error"), "{error}");
        assert!(check("fn f() -> int { return try { parse_int(\"1\")? } catch e { print(\"x\" + e) 0 } }").is_ok());
        assert!(check(&format!("{g}fn f() -> int {{ return match g() {{ result::Ok(v) => v result::Err(e) => match e {{ Failure::Bad => 0 }} }} }}")).is_ok());
    }

    #[test]
    fn test_enum_def_and_variant() {
        assert!(check("enum Status { Ok, Failed(string) } let s = Status::Ok;").is_ok());
    }

    #[test]
    fn test_enum_variant_with_payload() {
        assert!(check("enum Status { Failed(string) } let s = Status::Failed(\"oops\");").is_ok());
    }

    #[test]
    fn test_enum_variant_wrong_payload() {
        assert!(check("enum Status { Failed(string) } let s = Status::Failed(42);").is_err());
    }

    #[test]
    fn test_for_loop_ok() {
        assert!(check("let items = [1, 2, 3]; for x in items { print(to_string(x)) }").is_ok());
    }

    #[test]
    fn test_for_range_ok() {
        assert!(check("for i in range(10) { print(to_string(i)) }").is_ok());
    }

    #[test]
    fn test_closure_infers_fn_type() {
        assert!(check("let f: fn(int) -> int = fn(x: int) -> int { x };").is_ok());
    }

    #[test]
    fn test_string_interp_ok() {
        assert!(check("let name = \"world\"; let s = \"hello {name}\";").is_ok());
    }

    #[test]
    fn test_generic_fn_ok() {
        assert!(check("fn identity<T>(x: T) -> T { x } let y = identity(42);").is_ok());
    }

    #[test]
    fn test_match_exhaustiveness_ok() {
        assert!(check("enum Color { Red, Green, Blue } let c = Color::Red; match c { Color::Red => 1, Color::Green => 2, Color::Blue => 3 }").is_ok());
    }

    #[test]
    fn test_match_non_exhaustive_fails() {
        assert!(check("enum Color { Red, Green, Blue } let c = Color::Red; match c { Color::Red => 1, Color::Green => 2 }").is_err());
    }

    #[test]
    fn test_match_wildcard_exhaustive() {
        assert!(check("enum Color { Red, Green, Blue } let c = Color::Red; match c { Color::Red => 1, _ => 0 }").is_ok());
    }

    #[test]
    fn test_void_not_compatible_with_int() {
        // A function returning void shouldn't match int
        assert!(check("fn get_void() -> void { } let x: int = 5; x = get_void();").is_err());
    }

    #[test]
    fn test_map_builtin() {
        assert!(check("let xs = [1, 2, 3]; let doubled = map(xs, fn(x: int) -> int { x * 2 });").is_ok());
    }

    #[test]
    fn test_filter_builtin() {
        assert!(check("let xs = [1, 2, 3]; let evens = filter(xs, fn(x: int) -> bool { x == 2 });").is_ok());
    }
}
