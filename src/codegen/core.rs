use crate::ast::{ExprNode, StmtNode, Expr, Stmt, Type};
use std::collections::HashSet;

/// The shared runtime preamble (operator traits + builtins). When
/// `print_to_stderr` is true, `print_val` writes to stderr — used by secret
/// serverlet child programs whose stdout is the IPC channel.
pub fn runtime_preamble(print_to_stderr: bool) -> String {
    let print_macro = if print_to_stderr { "eprintln!" } else { "println!" };
    format!(r#"trait OrchAdd<RHS = Self> {{
    type Output;
    fn orch_add(self, rhs: RHS) -> Self::Output;
}}

impl OrchAdd for i64 {{
    type Output = i64;
    fn orch_add(self, rhs: i64) -> i64 {{ self + rhs }}
}}

impl OrchAdd for f64 {{
    type Output = f64;
    fn orch_add(self, rhs: f64) -> f64 {{ self + rhs }}
}}

impl OrchAdd<&str> for String {{
    type Output = String;
    fn orch_add(mut self, rhs: &str) -> String {{
        self.push_str(rhs);
        self
    }}
}}

impl OrchAdd<String> for String {{
    type Output = String;
    fn orch_add(mut self, rhs: String) -> String {{
        self.push_str(&rhs);
        self
    }}
}}

impl OrchAdd<&str> for &str {{
    type Output = String;
    fn orch_add(self, rhs: &str) -> String {{
        let mut s = self.to_string();
        s.push_str(rhs);
        s
    }}
}}

impl OrchAdd<String> for &str {{
    type Output = String;
    fn orch_add(self, rhs: String) -> String {{
        let mut s = self.to_string();
        s.push_str(&rhs);
        s
    }}
}}

fn print_val<T: std::fmt::Display>(val: T) {{
    {print_macro}("{{}}", val);
}}

fn to_string<T: std::fmt::Display>(val: T) -> String {{
    val.to_string()
}}

fn stop_orch() {{
    std::process::exit(0);
}}

type ProcessRef = std::sync::Arc<dyn Fn() -> tokio::task::JoinHandle<()> + Send + Sync + 'static>;

"#, print_macro = print_macro)
}

/// Async length-prefixed frame helpers used by a secret serverlet's mirror
/// (orchestrator side) to talk to the child process over its stdio.
pub const SECRET_MIRROR_HELPERS: &str = r#"async fn __secret_write_frame<W: tokio::io::AsyncWriteExt + Unpin>(w: &mut W, fields: &[String]) -> std::io::Result<()> {
    w.write_all(&(fields.len() as u32).to_le_bytes()).await?;
    for f in fields {
        let b = f.as_bytes();
        w.write_all(&(b.len() as u32).to_le_bytes()).await?;
        w.write_all(b).await?;
    }
    w.flush().await?;
    Ok(())
}

async fn __secret_read_frame<R: tokio::io::AsyncReadExt + Unpin>(r: &mut R) -> std::io::Result<Option<Vec<String>>> {
    let mut n_buf = [0u8; 4];
    match r.read_exact(&mut n_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let n = u32::from_le_bytes(n_buf);
    let mut fields = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let mut len_buf = [0u8; 4];
        r.read_exact(&mut len_buf).await?;
        let len = u32::from_le_bytes(len_buf) as usize;
        let mut buf = vec![0u8; len];
        r.read_exact(&mut buf).await?;
        fields.push(String::from_utf8_lossy(&buf).into_owned());
    }
    Ok(Some(fields))
}

"#;

/// Blocking length-prefixed frame helpers used by a secret serverlet's child
/// program (stdin/stdout side).
pub const SECRET_CHILD_FRAMES: &str = r#"fn __frame_read<R: std::io::Read>(r: &mut R) -> std::io::Result<Option<Vec<String>>> {
    let mut n_buf = [0u8; 4];
    match r.read_exact(&mut n_buf) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let n = u32::from_le_bytes(n_buf);
    let mut fields = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let mut len_buf = [0u8; 4];
        r.read_exact(&mut len_buf)?;
        let len = u32::from_le_bytes(len_buf) as usize;
        let mut buf = vec![0u8; len];
        r.read_exact(&mut buf)?;
        fields.push(String::from_utf8_lossy(&buf).into_owned());
    }
    Ok(Some(fields))
}

fn __frame_write<W: std::io::Write>(w: &mut W, fields: &[String]) -> std::io::Result<()> {
    w.write_all(&(fields.len() as u32).to_le_bytes())?;
    for f in fields {
        let b = f.as_bytes();
        w.write_all(&(b.len() as u32).to_le_bytes())?;
        w.write_all(b)?;
    }
    w.flush()?;
    Ok(())
}

"#;

pub fn pascal_case(s: &str) -> String {
    let mut res = String::new();
    let mut capitalize = true;
    for c in s.chars() {
        if c == '_' {
            capitalize = true;
        } else if capitalize {
            res.extend(c.to_uppercase());
            capitalize = false;
        } else {
            res.push(c);
        }
    }
    res
}

pub struct Codegen {
    pub tasks: HashSet<String>,
    pub in_parallel: bool,
    pub modules: HashSet<String>,
    pub is_main: bool,
    pub events: std::collections::HashMap<String, Vec<Type>>,
    pub local_stmts: Vec<Stmt>,
    /// Standalone programs for secret serverlets: (binary_name, rust_source).
    /// The driver writes each to `.orch_cache/src/bin/<binary_name>.rs`.
    pub secret_programs: Vec<(String, String)>,
    /// True if any secret serverlet was emitted, so the main/module file pulls in
    /// the async IPC frame helpers.
    pub has_secret: bool,
}

impl Codegen {
    pub fn new(mut tasks: HashSet<String>) -> Self {
        tasks.insert("sleep".to_string());
        Codegen {
            tasks,
            in_parallel: false,
            modules: HashSet::new(),
            is_main: false,
            events: std::collections::HashMap::new(),
            local_stmts: Vec::new(),
            secret_programs: Vec::new(),
            has_secret: false,
        }
    }

    /// Scan the program to find all declared tasks, so we know which calls need .await
    pub fn scan_tasks(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            match &stmt.node {
                StmtNode::TaskDecl { name, .. } => {
                    self.tasks.insert(name.clone());
                }
                StmtNode::ProcessDecl { name, .. } => {
                    self.tasks.insert(name.clone());
                }
                StmtNode::OrchestratorDecl { name, .. } => {
                    self.tasks.insert(name.clone());
                }
                _ => {}
            }
        }
    }

    pub fn scan_modules(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            match &stmt.node {
                StmtNode::UseModule { local_name, .. } => {
                    self.modules.insert(local_name.clone());
                }
                _ => {}
            }
        }
    }

    pub fn scan_events(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            self.scan_events_in_stmt(stmt);
        }
    }

    pub fn scan_events_in_stmt(&mut self, stmt: &Stmt) {
        match &stmt.node {
            StmtNode::Let { value, .. } => self.scan_events_in_expr(value),
            StmtNode::Expr(expr) => self.scan_events_in_expr(expr),
            StmtNode::Return(opt_expr) => {
                if let Some(expr) = opt_expr {
                    self.scan_events_in_expr(expr);
                }
            }
            StmtNode::FnDecl { body, .. } => self.scan_events_in_expr(body),
            StmtNode::TaskDecl { body, .. } => self.scan_events_in_expr(body),
            StmtNode::ProcessDecl { body, .. } => self.scan_events_in_expr(body),
            StmtNode::OrchestratorDecl { body, .. } => self.scan_events_in_expr(body),
            StmtNode::Parallel(stmts) => {
                for s in stmts {
                    self.scan_events_in_stmt(s);
                }
            }
            StmtNode::While { cond, body } => {
                self.scan_events_in_expr(cond);
                self.scan_events_in_expr(body);
            }
            StmtNode::Serverlet { state, handlers, .. } => {
                for s in state {
                    self.scan_events_in_stmt(s);
                }
                for h in handlers {
                    self.scan_events_in_expr(&h.body);
                }
            }
            _ => {}
        }
    }

    pub fn scan_events_in_expr(&mut self, expr: &Expr) {
        match &expr.node {
            ExprNode::Block(stmts) => {
                for s in stmts {
                    self.scan_events_in_stmt(s);
                }
            }
            ExprNode::Binary { lhs, rhs, .. } => {
                self.scan_events_in_expr(lhs);
                self.scan_events_in_expr(rhs);
            }
            ExprNode::Call { args, .. } => {
                for a in args {
                    self.scan_events_in_expr(a);
                }
            }
            ExprNode::Pipeline { value, function } => {
                self.scan_events_in_expr(value);
                self.scan_events_in_expr(function);
            }
            ExprNode::If { cond, then_branch, else_branch } => {
                self.scan_events_in_expr(cond);
                self.scan_events_in_expr(then_branch);
                if let Some(eb) = else_branch {
                    self.scan_events_in_expr(eb);
                }
            }
            ExprNode::ModuleCall { args, .. } => {
                for a in args {
                    self.scan_events_in_expr(a);
                }
            }
            ExprNode::StartServerlet { args, .. } => {
                for a in args {
                    self.scan_events_in_expr(a);
                }
            }
            ExprNode::StartProcess { target } => {
                self.scan_events_in_expr(target);
            }
            ExprNode::AutomaticBlock { body } => {
                self.scan_events_in_expr(body);
            }
            ExprNode::TriggeredBlock { event_name, params, body } => {
                let types = params.iter().map(|p| p.ty.clone()).collect();
                self.events.insert(event_name.clone(), types);
                self.scan_events_in_expr(body);
            }
            ExprNode::ArrayLiteral(elements) => {
                for e in elements {
                    self.scan_events_in_expr(e);
                }
            }
            _ => {}
        }
    }

    pub fn get_free_vars_expr(&self, expr: &Expr, local_env: &mut HashSet<String>, free_vars: &mut HashSet<String>) {
        match &expr.node {
            ExprNode::Identifier(name) => {
                if !local_env.contains(name) && !self.functions_and_tasks_contain(name) {
                    free_vars.insert(name.clone());
                }
            }
            ExprNode::Binary { lhs, rhs, .. } => {
                self.get_free_vars_expr(lhs, local_env, free_vars);
                self.get_free_vars_expr(rhs, local_env, free_vars);
            }
            ExprNode::Call { args, .. } => {
                for a in args {
                    self.get_free_vars_expr(a, local_env, free_vars);
                }
            }
            ExprNode::Pipeline { value, function } => {
                self.get_free_vars_expr(value, local_env, free_vars);
                self.get_free_vars_expr(function, local_env, free_vars);
            }
            ExprNode::Block(stmts) => {
                let mut inner_env = local_env.clone();
                for s in stmts {
                    self.get_free_vars_stmt(s, &mut inner_env, free_vars);
                }
            }
            ExprNode::If { cond, then_branch, else_branch } => {
                self.get_free_vars_expr(cond, local_env, free_vars);
                self.get_free_vars_expr(then_branch, local_env, free_vars);
                if let Some(eb) = else_branch {
                    self.get_free_vars_expr(eb, local_env, free_vars);
                }
            }
            ExprNode::ModuleCall { args, .. } => {
                for a in args {
                    self.get_free_vars_expr(a, local_env, free_vars);
                }
            }
            ExprNode::StartServerlet { args, .. } => {
                for a in args {
                    self.get_free_vars_expr(a, local_env, free_vars);
                }
            }
            ExprNode::StartProcess { target } => {
                self.get_free_vars_expr(target, local_env, free_vars);
            }
            ExprNode::AutomaticBlock { body } => {
                self.get_free_vars_expr(body, local_env, free_vars);
            }
            ExprNode::TriggeredBlock { params, body, .. } => {
                let mut inner_env = local_env.clone();
                for p in params {
                    inner_env.insert(p.name.clone());
                }
                self.get_free_vars_expr(body, &mut inner_env, free_vars);
            }
            ExprNode::ArrayLiteral(elements) => {
                for e in elements {
                    self.get_free_vars_expr(e, local_env, free_vars);
                }
            }
            _ => {}
        }
    }

    pub fn get_free_vars_stmt(&self, stmt: &Stmt, local_env: &mut HashSet<String>, free_vars: &mut HashSet<String>) {
        match &stmt.node {
            StmtNode::Let { name, value, .. } => {
                self.get_free_vars_expr(value, local_env, free_vars);
                local_env.insert(name.clone());
            }
            StmtNode::Expr(expr) => self.get_free_vars_expr(expr, local_env, free_vars),
            StmtNode::Return(opt_expr) => {
                if let Some(expr) = opt_expr {
                    self.get_free_vars_expr(expr, local_env, free_vars);
                }
            }
            StmtNode::Trigger { args, .. } => {
                for a in args {
                    self.get_free_vars_expr(a, local_env, free_vars);
                }
            }
            StmtNode::While { cond, body } => {
                self.get_free_vars_expr(cond, local_env, free_vars);
                self.get_free_vars_expr(body, local_env, free_vars);
            }
            StmtNode::Parallel(stmts) => {
                for s in stmts {
                    self.get_free_vars_stmt(s, local_env, free_vars);
                }
            }
            StmtNode::OnStart(expr) | StmtNode::OnStop(expr) => {
                self.get_free_vars_expr(expr, local_env, free_vars);
            }
            _ => {}
        }
    }

    pub fn functions_and_tasks_contain(&self, name: &str) -> bool {
        self.tasks.contains(name) || name == "print" || name == "to_string" || name == "length" || name == "append" || name == "remove" || name == "sleep" || name == "stop_orch"
    }

    pub fn generate(&mut self, stmts: &[Stmt], is_main: bool) -> String {
        self.is_main = is_main;
        self.scan_tasks(stmts);
        self.scan_modules(stmts);
        self.scan_events(stmts);

        // Detect secret serverlets up front so the preamble can include the IPC
        // frame helpers before any mirror code is emitted.
        for stmt in stmts {
            if let StmtNode::Serverlet { secret: true, .. } = &stmt.node {
                self.has_secret = true;
            }
        }

        let mut code = String::new();

        // Preamble
        code.push_str("// Generated by Orchestrate Compiler\n");
        if is_main {
            self.events.entry("update_orchestrator".to_string())
                .or_insert_with(|| vec![Type::Array(Box::new(Type::Process), vec![])]);

            code.push_str("#![allow(unused_variables)]\n");
            code.push_str("#![allow(dead_code)]\n");
            code.push_str("#![allow(unused_imports)]\n");
            code.push_str("#![allow(unused_parens)]\n");
            code.push_str("#![allow(unused_mut)]\n\n");

            // Output dynamic event registries
            for (event_name, types) in &self.events {
                let type_str = if types.is_empty() {
                    "()".to_string()
                } else if types.len() == 1 {
                    self.compile_type(&types[0]).to_string()
                } else {
                    let compiled_tys = types
                        .iter()
                        .map(|t| self.compile_type(t))
                        .collect::<Vec<String>>()
                        .join(", ");
                    format!("({})", compiled_tys)
                };

                let arc_type_str = format!("std::sync::Arc<{}>", type_str);

                let var_name = format!("REGISTRY_{}", event_name.to_uppercase());
                let func_name = format!("get_registry_{}", event_name);
                
                code.push_str(&format!(
                    "static {}: std::sync::OnceLock<std::sync::Mutex<Vec<tokio::sync::mpsc::Sender<{}>>>> = std::sync::OnceLock::new();\n",
                    var_name, arc_type_str
                ));
                code.push_str(&format!(
                    "fn {}() -> &'static std::sync::Mutex<Vec<tokio::sync::mpsc::Sender<{}>>> {{\n    {}.get_or_init(|| std::sync::Mutex::new(Vec::new()))\n}}\n\n",
                    func_name, arc_type_str, var_name
                ));
            }
        }

        code.push_str(&runtime_preamble(false));

        // Async IPC frame helpers, emitted only when a secret serverlet's mirror
        // is present in this file (main or module).
        if self.has_secret {
            code.push_str(SECRET_MIRROR_HELPERS);
        }

        let mut global_stmts = Vec::new();
        let mut local_stmts = Vec::new();
        let mut main_decl = None;

        if is_main {
            for stmt in stmts {
                match &stmt.node {
                    StmtNode::UseModule { .. } |
                    StmtNode::Load { .. } |
                    StmtNode::LoadForeign { .. } |
                    StmtNode::FnDecl { .. } |
                    StmtNode::TaskDecl { .. } |
                    StmtNode::ProcessDecl { .. } |
                    StmtNode::Serverlet { .. } |
                    StmtNode::StructDef { .. } => {
                        global_stmts.push(stmt.clone());
                    }
                    StmtNode::OrchestratorDecl { name, .. } => {
                        if name == "main" {
                            main_decl = Some(stmt.clone());
                        } else {
                            global_stmts.push(stmt.clone());
                        }
                    }
                    _ => {
                        local_stmts.push(stmt.clone());
                    }
                }
            }
            if main_decl.is_none() {
                main_decl = Some(crate::ast::Spanned {
                    span: crate::ast::Span::new(0, 0),
                    node: StmtNode::OrchestratorDecl {
                        name: "main".to_string(),
                        params: Vec::new(),
                        return_type: Type::Void,
                        body: crate::ast::Spanned {
                            span: crate::ast::Span::new(0, 0),
                            node: crate::ast::ExprNode::Block(Vec::new())
                        },
                    }
                });
            }
            global_stmts.push(main_decl.unwrap());
        } else {
            global_stmts = stmts.to_vec();
        }

        self.local_stmts = local_stmts;

        for stmt in &global_stmts {
            code.push_str(&self.compile_stmt(stmt));
            code.push_str("\n\n");
        }

        code
    }

    
pub fn compile_block_inner(&mut self, body: &Expr, force_semicolons: bool) -> String {
        if let ExprNode::Block(stmts) = &body.node {
            let mut parts = Vec::new();
            for (i, s) in stmts.iter().enumerate() {
                let is_last = i == stmts.len() - 1;
                match &s.node {
                    StmtNode::Expr(expr) if is_last && !force_semicolons => {
                        parts.push(self.compile_expr(expr));
                    }
                    _ => {
                        let compiled = self.compile_stmt(s);
                        if !compiled.ends_with(';') && !compiled.ends_with('}') {
                            parts.push(format!("{};", compiled));
                        } else {
                            parts.push(compiled);
                        }
                    }
                }
            }
            parts.join("\n    ")
        } else {
            self.compile_expr(body)
        }
    }

    
pub fn compile_type(&self, ty: &Type) -> String {
        match ty {
            Type::Int => "i64".to_string(),
            Type::Float => "f64".to_string(),
            Type::Str => "String".to_string(),
            Type::Bool => "bool".to_string(),
            Type::Void => "()".to_string(),
            Type::Process => "ProcessRef".to_string(),
            Type::Array(inner, _init_vals) => format!("Vec<{}>", self.compile_type(inner)),
            Type::Named(name) => name.clone(),
        }
    }
}
