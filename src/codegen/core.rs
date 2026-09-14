use crate::ast::{ExprNode, StmtNode, Expr, Stmt, StringPart, Type};
use std::collections::HashSet;

pub fn runtime_preamble(print_to_stderr: bool, include_process_ref: bool) -> String {
    let print_macro = if print_to_stderr { "eprintln!" } else { "println!" };
    let process_ref = if include_process_ref {
        "type ProcessRef = std::sync::Arc<dyn Fn() -> tokio::task::JoinHandle<()> + Send + Sync + 'static>;\n\n"
    } else {
        ""
    };
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

{process_ref}"#, print_macro = print_macro, process_ref = process_ref)
}

pub const SECRET_MIRROR_HELPERS: &str = r#"async fn __secret_write_frame<W: tokio::io::AsyncWriteExt + Unpin>(w: &mut W, kind: u8, call_id: u32, payload: &[u8]) -> std::io::Result<()> {
    w.write_all(&((payload.len() + 5) as u32).to_le_bytes()).await?;
    w.write_all(&[kind]).await?;
    w.write_all(&call_id.to_le_bytes()).await?;
    w.write_all(payload).await?;
    w.flush().await
}

async fn __secret_read_frame<R: tokio::io::AsyncReadExt + Unpin>(r: &mut R) -> std::io::Result<Option<OrchFrame>> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let mut buf = vec![0u8; u32::from_le_bytes(len_buf) as usize];
    r.read_exact(&mut buf).await?;
    OrchFrame::parse(buf).map(Some)
}

"#;

pub const SECRET_CHILD_FRAMES: &str = r#"fn __frame_read<R: std::io::Read>(r: &mut R) -> std::io::Result<Option<OrchFrame>> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let mut buf = vec![0u8; u32::from_le_bytes(len_buf) as usize];
    r.read_exact(&mut buf)?;
    OrchFrame::parse(buf).map(Some)
}

fn __frame_write<W: std::io::Write>(w: &mut W, kind: u8, call_id: u32, payload: &[u8]) -> std::io::Result<()> {
    w.write_all(&((payload.len() + 5) as u32).to_le_bytes())?;
    w.write_all(&[kind])?;
    w.write_all(&call_id.to_le_bytes())?;
    w.write_all(payload)?;
    w.flush()
}

fn __panic_message(e: &Box<dyn std::any::Any + Send>) -> String {
    e.downcast_ref::<&str>().map(|s| s.to_string())
        .or_else(|| e.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "handler panicked".to_string())
}

"#;

/// Serverlet wire protocol v1 (docs/design/landline-serverlets.md §7), shared by both ends.
///
/// Frame: `[u32 length][u8 kind][u32 call_id][payload]`, little-endian; the length covers
/// kind + call_id + payload. Values use the `OrchWire` encoding: int and float as 8 bytes,
/// bool as 1 byte, string as a u32 length + UTF-8, arrays as a u32 count + elements, and
/// structs as their fields in declaration order (impls generated per struct).
pub const WIRE_CODEC: &str = r#"const ORCH_WIRE_VERSION: i64 = 1;
const ORCH_KIND_HELLO: u8 = 1;
const ORCH_KIND_READY: u8 = 2;
const ORCH_KIND_CALL: u8 = 3;
const ORCH_KIND_REPLY: u8 = 4;
const ORCH_KIND_ERROR: u8 = 5;
// 6 = HOST_CALL, 7 = HOST_REPLY, and 9 = TICK are reserved for landline serverlets.
const ORCH_KIND_BYE: u8 = 8;

struct OrchFrame {
    kind: u8,
    call_id: u32,
    payload: Vec<u8>,
}

impl OrchFrame {
    fn parse(mut buf: Vec<u8>) -> std::io::Result<OrchFrame> {
        if buf.len() < 5 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "serverlet frame too short"));
        }
        let payload = buf.split_off(5);
        Ok(OrchFrame { kind: buf[0], call_id: u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]), payload })
    }
}

trait OrchWire: Sized {
    fn wire_encode(&self, out: &mut Vec<u8>);
    fn wire_decode(buf: &[u8], pos: &mut usize) -> Option<Self>;
}

fn __wire_take<'a>(buf: &'a [u8], pos: &mut usize, n: usize) -> Option<&'a [u8]> {
    let end = pos.checked_add(n)?;
    let bytes = buf.get(*pos..end)?;
    *pos = end;
    Some(bytes)
}

fn __wire_len(buf: &[u8], pos: &mut usize) -> Option<usize> {
    Some(u32::from_le_bytes(__wire_take(buf, pos, 4)?.try_into().ok()?) as usize)
}

impl OrchWire for i64 {
    fn wire_encode(&self, out: &mut Vec<u8>) { out.extend_from_slice(&self.to_le_bytes()); }
    fn wire_decode(buf: &[u8], pos: &mut usize) -> Option<Self> { Some(i64::from_le_bytes(__wire_take(buf, pos, 8)?.try_into().ok()?)) }
}

impl OrchWire for f64 {
    fn wire_encode(&self, out: &mut Vec<u8>) { out.extend_from_slice(&self.to_le_bytes()); }
    fn wire_decode(buf: &[u8], pos: &mut usize) -> Option<Self> { Some(f64::from_le_bytes(__wire_take(buf, pos, 8)?.try_into().ok()?)) }
}

impl OrchWire for bool {
    fn wire_encode(&self, out: &mut Vec<u8>) { out.push(*self as u8); }
    fn wire_decode(buf: &[u8], pos: &mut usize) -> Option<Self> { Some(__wire_take(buf, pos, 1)?[0] != 0) }
}

impl OrchWire for String {
    fn wire_encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&(self.len() as u32).to_le_bytes());
        out.extend_from_slice(self.as_bytes());
    }
    fn wire_decode(buf: &[u8], pos: &mut usize) -> Option<Self> {
        let len = __wire_len(buf, pos)?;
        String::from_utf8(__wire_take(buf, pos, len)?.to_vec()).ok()
    }
}

impl<T: OrchWire> OrchWire for Vec<T> {
    fn wire_encode(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&(self.len() as u32).to_le_bytes());
        for item in self { item.wire_encode(out); }
    }
    fn wire_decode(buf: &[u8], pos: &mut usize) -> Option<Self> {
        let count = __wire_len(buf, pos)?;
        let mut items = Vec::with_capacity(count.min(4096));
        for _ in 0..count { items.push(T::wire_decode(buf, pos)?); }
        Some(items)
    }
}

fn __wire_to_bytes<T: OrchWire>(value: &T) -> Vec<u8> {
    let mut out = Vec::new();
    value.wire_encode(&mut out);
    out
}

fn __wire_from_bytes<T: OrchWire + Default>(bytes: &[u8]) -> T {
    let mut pos = 0;
    T::wire_decode(bytes, &mut pos).unwrap_or_default()
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
    pub secret_programs: Vec<(String, String)>,
    /// WASM guest crates for sandboxed serverlets: (serverlet_name, lib_rs_source).
    /// The driver writes each as a sub-crate and compiles it to wasm32-wasip1.
    pub sandbox_programs: Vec<(String, String)>,
    pub has_secret: bool,
    /// Struct definitions in the file being generated, used by the serverlet wire codec.
    pub struct_defs: Vec<(String, Vec<(String, Type)>)>,
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
            sandbox_programs: Vec::new(),
            struct_defs: Vec::new(),
        }
    }

    pub fn scan_tasks(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            match &stmt.node {
                StmtNode::TaskDecl { name, .. } => { self.tasks.insert(name.clone()); }
                StmtNode::ProcessDecl { name, .. } => { self.tasks.insert(name.clone()); }
                StmtNode::OrchestratorDecl { name, .. } => { self.tasks.insert(name.clone()); }
                _ => {}
            }
        }
    }

    pub fn scan_modules(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            if let StmtNode::UseModule { local_name, .. } = &stmt.node {
                self.modules.insert(local_name.clone());
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
                if let Some(expr) = opt_expr { self.scan_events_in_expr(expr); }
            }
            StmtNode::FnDecl { body, .. } => self.scan_events_in_expr(body),
            StmtNode::TaskDecl { body, .. } => self.scan_events_in_expr(body),
            StmtNode::ProcessDecl { body, .. } => self.scan_events_in_expr(body),
            StmtNode::OrchestratorDecl { body, .. } => self.scan_events_in_expr(body),
            StmtNode::Parallel(stmts) => {
                for s in stmts { self.scan_events_in_stmt(s); }
            }
            StmtNode::While { cond, body } => {
                self.scan_events_in_expr(cond);
                self.scan_events_in_expr(body);
            }
            StmtNode::ForIn { iter, body, .. } => {
                self.scan_events_in_expr(iter);
                self.scan_events_in_expr(body);
            }
            StmtNode::Serverlet { state, handlers, crash_handler, .. } => {
                for s in state { self.scan_events_in_stmt(s); }
                for h in handlers { self.scan_events_in_expr(&h.body); }
                if let Some((_, handler)) = crash_handler { self.scan_events_in_expr(handler); }
            }
            StmtNode::Break | StmtNode::Continue => {}
            _ => {}
        }
    }

    pub fn scan_events_in_expr(&mut self, expr: &Expr) {
        match &expr.node {
            ExprNode::Block(stmts) => {
                for s in stmts { self.scan_events_in_stmt(s); }
            }
            ExprNode::Unary { operand, .. } => {
                self.scan_events_in_expr(operand);
            }
            ExprNode::Index { object, index } => {
                self.scan_events_in_expr(object);
                self.scan_events_in_expr(index);
            }
            ExprNode::Binary { lhs, rhs, .. } => {
                self.scan_events_in_expr(lhs);
                self.scan_events_in_expr(rhs);
            }
            ExprNode::Call { args, .. } => {
                for a in args { self.scan_events_in_expr(a); }
            }
            ExprNode::Pipeline { value, function } => {
                self.scan_events_in_expr(value);
                self.scan_events_in_expr(function);
            }
            ExprNode::If { cond, then_branch, else_branch } => {
                self.scan_events_in_expr(cond);
                self.scan_events_in_expr(then_branch);
                if let Some(eb) = else_branch { self.scan_events_in_expr(eb); }
            }
            ExprNode::ModuleCall { args, .. } => {
                for a in args { self.scan_events_in_expr(a); }
            }
            ExprNode::StartServerlet { args, .. } => {
                for a in args { self.scan_events_in_expr(a); }
            }
            ExprNode::StartProcess { target } => self.scan_events_in_expr(target),
            ExprNode::AutomaticBlock { body, crash_handler, .. } => {
                self.scan_events_in_expr(body);
                if let Some((_, handler)) = crash_handler { self.scan_events_in_expr(handler); }
            }
            ExprNode::TriggeredBlock { event_name, params, body } => {
                let types = params.iter().map(|p| p.ty.clone()).collect();
                self.events.insert(event_name.clone(), types);
                self.scan_events_in_expr(body);
            }
            ExprNode::ArrayLiteral(elements) => {
                for e in elements { self.scan_events_in_expr(e); }
            }
            ExprNode::TryCatch { body, handler, .. } => {
                self.scan_events_in_expr(body);
                self.scan_events_in_expr(handler);
            }
            ExprNode::Match { value, arms } => {
                self.scan_events_in_expr(value);
                for arm in arms { self.scan_events_in_expr(&arm.body); }
            }
            ExprNode::SomeLiteral(inner) | ExprNode::OkLiteral(inner) |
            ExprNode::ErrLiteral(inner) | ExprNode::Propagate(inner) => {
                self.scan_events_in_expr(inner);
            }
            ExprNode::EnumVariantLiteral { payload, .. } => {
                if let Some(p) = payload { self.scan_events_in_expr(p); }
            }
            ExprNode::Closure { body, .. } => {
                self.scan_events_in_expr(body);
            }
            ExprNode::StringInterp { parts } => {
                for part in parts {
                    if let StringPart::Expr(e) = part { self.scan_events_in_expr(e); }
                }
            }
            ExprNode::StructLiteral { fields, .. } => {
                for (_, v) in fields { self.scan_events_in_expr(v); }
            }
            ExprNode::FieldAccess { object, .. } => {
                self.scan_events_in_expr(object);
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
            ExprNode::Unary { operand, .. } => {
                self.get_free_vars_expr(operand, local_env, free_vars);
            }
            ExprNode::Index { object, index } => {
                self.get_free_vars_expr(object, local_env, free_vars);
                self.get_free_vars_expr(index, local_env, free_vars);
            }
            ExprNode::Binary { lhs, rhs, .. } => {
                self.get_free_vars_expr(lhs, local_env, free_vars);
                self.get_free_vars_expr(rhs, local_env, free_vars);
            }
            ExprNode::Call { args, .. } => {
                for a in args { self.get_free_vars_expr(a, local_env, free_vars); }
            }
            ExprNode::Pipeline { value, function } => {
                self.get_free_vars_expr(value, local_env, free_vars);
                self.get_free_vars_expr(function, local_env, free_vars);
            }
            ExprNode::Block(stmts) => {
                let mut inner_env = local_env.clone();
                for s in stmts { self.get_free_vars_stmt(s, &mut inner_env, free_vars); }
            }
            ExprNode::If { cond, then_branch, else_branch } => {
                self.get_free_vars_expr(cond, local_env, free_vars);
                self.get_free_vars_expr(then_branch, local_env, free_vars);
                if let Some(eb) = else_branch { self.get_free_vars_expr(eb, local_env, free_vars); }
            }
            ExprNode::ModuleCall { args, .. } => {
                for a in args { self.get_free_vars_expr(a, local_env, free_vars); }
            }
            ExprNode::StartServerlet { args, .. } => {
                for a in args { self.get_free_vars_expr(a, local_env, free_vars); }
            }
            ExprNode::StartProcess { target } => self.get_free_vars_expr(target, local_env, free_vars),
            ExprNode::AutomaticBlock { body, crash_handler, .. } => {
                self.get_free_vars_expr(body, local_env, free_vars);
                if let Some((err_name, handler)) = crash_handler {
                    let mut handler_env = local_env.clone();
                    handler_env.insert(err_name.clone());
                    self.get_free_vars_expr(handler, &mut handler_env, free_vars);
                }
            }
            ExprNode::TriggeredBlock { params, body, .. } => {
                let mut inner_env = local_env.clone();
                for p in params { inner_env.insert(p.name.clone()); }
                self.get_free_vars_expr(body, &mut inner_env, free_vars);
            }
            ExprNode::ArrayLiteral(elements) => {
                for e in elements { self.get_free_vars_expr(e, local_env, free_vars); }
            }
            ExprNode::SomeLiteral(inner) | ExprNode::OkLiteral(inner) |
            ExprNode::ErrLiteral(inner) | ExprNode::Propagate(inner) => {
                self.get_free_vars_expr(inner, local_env, free_vars);
            }
            ExprNode::TryCatch { body, err_name, handler } => {
                self.get_free_vars_expr(body, local_env, free_vars);
                let mut handler_env = local_env.clone();
                handler_env.insert(err_name.clone());
                self.get_free_vars_expr(handler, &mut handler_env, free_vars);
            }
            ExprNode::Match { value, arms } => {
                self.get_free_vars_expr(value, local_env, free_vars);
                for arm in arms { self.get_free_vars_expr(&arm.body, local_env, free_vars); }
            }
            ExprNode::EnumVariantLiteral { payload, .. } => {
                if let Some(p) = payload { self.get_free_vars_expr(p, local_env, free_vars); }
            }
            ExprNode::Closure { params, body, .. } => {
                let mut inner_env = local_env.clone();
                for p in params { inner_env.insert(p.name.clone()); }
                self.get_free_vars_expr(body, &mut inner_env, free_vars);
            }
            ExprNode::StringInterp { parts } => {
                for part in parts {
                    if let StringPart::Expr(e) = part {
                        self.get_free_vars_expr(e, local_env, free_vars);
                    }
                }
            }
            ExprNode::StructLiteral { fields, .. } => {
                for (_, v) in fields { self.get_free_vars_expr(v, local_env, free_vars); }
            }
            ExprNode::FieldAccess { object, .. } => {
                self.get_free_vars_expr(object, local_env, free_vars);
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
                if let Some(expr) = opt_expr { self.get_free_vars_expr(expr, local_env, free_vars); }
            }
            StmtNode::Trigger { args, .. } => {
                for a in args { self.get_free_vars_expr(a, local_env, free_vars); }
            }
            StmtNode::While { cond, body } => {
                self.get_free_vars_expr(cond, local_env, free_vars);
                self.get_free_vars_expr(body, local_env, free_vars);
            }
            StmtNode::ForIn { var, index_var, iter, body } => {
                self.get_free_vars_expr(iter, local_env, free_vars);
                let mut inner_env = local_env.clone();
                inner_env.insert(var.clone());
                if let Some(idx) = index_var { inner_env.insert(idx.clone()); }
                self.get_free_vars_expr(body, &mut inner_env, free_vars);
            }
            StmtNode::Parallel(stmts) => {
                for s in stmts { self.get_free_vars_stmt(s, local_env, free_vars); }
            }
            StmtNode::OnStart(expr) | StmtNode::OnStop(expr) => {
                self.get_free_vars_expr(expr, local_env, free_vars);
            }
            _ => {}
        }
    }

    pub fn functions_and_tasks_contain(&self, name: &str) -> bool {
        self.tasks.contains(name)
            || matches!(name, "print" | "to_string" | "to_int" | "to_float" | "parse_int" | "parse_float"
                            | "length" | "append" | "remove" | "sleep" | "stop_orch"
                            | "range" | "map" | "filter" | "reduce" | "find" | "any" | "all")
    }

    pub fn generate(&mut self, stmts: &[Stmt], is_main: bool) -> String {
        self.is_main = is_main;
        self.scan_tasks(stmts);
        self.scan_modules(stmts);
        self.scan_events(stmts);

        for stmt in stmts {
            if let StmtNode::Serverlet { secret: true, .. } = &stmt.node {
                self.has_secret = true;
            }
        }

        let mut code = String::new();

        code.push_str("// Generated by Orchestrate Compiler\n");
        if is_main {
            self.events.entry("update_orchestrator".to_string())
                .or_insert_with(|| vec![Type::Array(Box::new(Type::Process), vec![])]);

            code.push_str("#![allow(unused_variables)]\n");
            code.push_str("#![allow(dead_code)]\n");
            code.push_str("#![allow(unused_imports)]\n");
            code.push_str("#![allow(unused_parens)]\n");
            code.push_str("#![allow(unused_mut)]\n");
            code.push_str("#![allow(unreachable_code)]\n\n");

            for (event_name, types) in &self.events {
                let type_str = if types.is_empty() {
                    "()".to_string()
                } else if types.len() == 1 {
                    self.compile_type(&types[0]).to_string()
                } else {
                    let compiled_tys = types.iter().map(|t| self.compile_type(t)).collect::<Vec<String>>().join(", ");
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

        code.push_str(&runtime_preamble(false, true));

        if self.has_secret {
            code.push_str(SECRET_MIRROR_HELPERS);
            code.push_str(WIRE_CODEC);
            self.struct_defs = stmts.iter().filter_map(|s| match &s.node {
                StmtNode::StructDef { name, fields } => Some((name.clone(), fields.clone())),
                _ => None,
            }).collect();
            code.push_str(&super::stmt::wire_struct_impls(&self.struct_defs));
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
                    StmtNode::StructDef { .. } |
                    StmtNode::EnumDef { .. } => {
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
                            node: crate::ast::ExprNode::Block(Vec::new()),
                        },
                    },
                });
            }
            global_stmts.push(main_decl.unwrap());
        } else {
            global_stmts = stmts.to_vec();
        }

        self.local_stmts = local_stmts;

        for stmt in &global_stmts {
            // Emit source-map comment before each top-level declaration
            if stmt.span.line > 0 {
                code.push_str(&format!("// orch:{}:{}\n", stmt.span.line, stmt.span.col));
            }
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
                // Source map comment for each statement
                let src_comment = if s.span.line > 0 {
                    format!("// orch:{}:{}\n    ", s.span.line, s.span.col)
                } else {
                    String::new()
                };
                match &s.node {
                    StmtNode::Expr(expr) if is_last && !force_semicolons => {
                        parts.push(format!("{}{}", src_comment, self.compile_expr(expr)));
                    }
                    _ => {
                        let compiled = self.compile_stmt(s);
                        if !compiled.ends_with(';') && !compiled.ends_with('}') {
                            parts.push(format!("{}{};", src_comment, compiled));
                        } else {
                            parts.push(format!("{}{}", src_comment, compiled));
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
            Type::Option(inner) => format!("Option<{}>", self.compile_type(inner)),
            Type::Result(inner) => format!("Result<{}, String>", self.compile_type(inner)),
            Type::Fn(params, ret) => {
                let params_str = params.iter().map(|t| self.compile_type(t)).collect::<Vec<_>>().join(", ");
                format!("impl Fn({}) -> {}", params_str, self.compile_type(ret))
            }
            Type::TypeParam(name) => name.clone(),
        }
    }
}
