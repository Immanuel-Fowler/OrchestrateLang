use crate::ast::{BinaryOp, Expr, Literal, Stmt, Type};
use std::collections::HashSet;

fn pascal_case(s: &str) -> String {
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
    tasks: HashSet<String>,
    in_parallel: bool,
    modules: HashSet<String>,
    is_main: bool,
    events: std::collections::HashMap<String, Vec<Type>>,
    local_stmts: Vec<Stmt>,
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
        }
    }

    /// Scan the program to find all declared tasks, so we know which calls need .await
    fn scan_tasks(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            match stmt {
                Stmt::TaskDecl { name, .. } => {
                    self.tasks.insert(name.clone());
                }
                Stmt::ProcessDecl { name, .. } => {
                    self.tasks.insert(name.clone());
                }
                Stmt::OrchestratorDecl { name, .. } => {
                    self.tasks.insert(name.clone());
                }
                _ => {}
            }
        }
    }

    fn scan_modules(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            match stmt {
                Stmt::UseModule { local_name, .. } => {
                    self.modules.insert(local_name.clone());
                }
                _ => {}
            }
        }
    }

    fn scan_events(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            self.scan_events_in_stmt(stmt);
        }
    }

    fn scan_events_in_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Let { value, .. } => self.scan_events_in_expr(value),
            Stmt::Expr(expr) => self.scan_events_in_expr(expr),
            Stmt::Return(opt_expr) => {
                if let Some(expr) = opt_expr {
                    self.scan_events_in_expr(expr);
                }
            }
            Stmt::FnDecl { body, .. } => self.scan_events_in_expr(body),
            Stmt::TaskDecl { body, .. } => self.scan_events_in_expr(body),
            Stmt::ProcessDecl { body, .. } => self.scan_events_in_expr(body),
            Stmt::OrchestratorDecl { body, .. } => self.scan_events_in_expr(body),
            Stmt::Parallel(stmts) => {
                for s in stmts {
                    self.scan_events_in_stmt(s);
                }
            }
            Stmt::While { cond, body } => {
                self.scan_events_in_expr(cond);
                self.scan_events_in_expr(body);
            }
            Stmt::Serverlet { state, handlers, .. } => {
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

    fn scan_events_in_expr(&mut self, expr: &Expr) {
        match expr {
            Expr::Block(stmts) => {
                for s in stmts {
                    self.scan_events_in_stmt(s);
                }
            }
            Expr::Binary { lhs, rhs, .. } => {
                self.scan_events_in_expr(lhs);
                self.scan_events_in_expr(rhs);
            }
            Expr::Call { args, .. } => {
                for a in args {
                    self.scan_events_in_expr(a);
                }
            }
            Expr::Pipeline { value, function } => {
                self.scan_events_in_expr(value);
                self.scan_events_in_expr(function);
            }
            Expr::If { cond, then_branch, else_branch } => {
                self.scan_events_in_expr(cond);
                self.scan_events_in_expr(then_branch);
                if let Some(eb) = else_branch {
                    self.scan_events_in_expr(eb);
                }
            }
            Expr::ModuleCall { args, .. } => {
                for a in args {
                    self.scan_events_in_expr(a);
                }
            }
            Expr::StartServerlet { args, .. } => {
                for a in args {
                    self.scan_events_in_expr(a);
                }
            }
            Expr::StartProcess { target } => {
                self.scan_events_in_expr(target);
            }
            Expr::AutomaticBlock { body } => {
                self.scan_events_in_expr(body);
            }
            Expr::TriggeredBlock { event_name, params, body } => {
                let types = params.iter().map(|p| p.ty.clone()).collect();
                self.events.insert(event_name.clone(), types);
                self.scan_events_in_expr(body);
            }
            Expr::ArrayLiteral(elements) => {
                for e in elements {
                    self.scan_events_in_expr(e);
                }
            }
            _ => {}
        }
    }

    pub fn generate(&mut self, stmts: &[Stmt], is_main: bool) -> String {
        self.is_main = is_main;
        self.scan_tasks(stmts);
        self.scan_modules(stmts);
        self.scan_events(stmts);

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

                let var_name = format!("REGISTRY_{}", event_name.to_uppercase());
                let func_name = format!("get_registry_{}", event_name);
                
                code.push_str(&format!(
                    "static {}: std::sync::OnceLock<std::sync::Mutex<Vec<tokio::sync::mpsc::Sender<{}>>>> = std::sync::OnceLock::new();\n",
                    var_name, type_str
                ));
                code.push_str(&format!(
                    "fn {}() -> &'static std::sync::Mutex<Vec<tokio::sync::mpsc::Sender<{}>>> {{\n    {}.get_or_init(|| std::sync::Mutex::new(Vec::new()))\n}}\n\n",
                    func_name, type_str, var_name
                ));
            }
        }

        code.push_str(r#"trait OrchAdd<RHS = Self> {
    type Output;
    fn orch_add(self, rhs: RHS) -> Self::Output;
}

impl OrchAdd for i64 {
    type Output = i64;
    fn orch_add(self, rhs: i64) -> i64 { self + rhs }
}

impl OrchAdd for f64 {
    type Output = f64;
    fn orch_add(self, rhs: f64) -> f64 { self + rhs }
}

impl OrchAdd<&str> for String {
    type Output = String;
    fn orch_add(mut self, rhs: &str) -> String {
        self.push_str(rhs);
        self
    }
}

impl OrchAdd<String> for String {
    type Output = String;
    fn orch_add(mut self, rhs: String) -> String {
        self.push_str(&rhs);
        self
    }
}

impl OrchAdd<&str> for &str {
    type Output = String;
    fn orch_add(self, rhs: &str) -> String {
        let mut s = self.to_string();
        s.push_str(rhs);
        s
    }
}

impl OrchAdd<String> for &str {
    type Output = String;
    fn orch_add(self, rhs: String) -> String {
        let mut s = self.to_string();
        s.push_str(&rhs);
        s
    }
}

fn print_val<T: std::fmt::Display>(val: T) {
    println!("{}", val);
}

fn to_string<T: std::fmt::Display>(val: T) -> String {
    val.to_string()
}

fn stop_orch() {
    std::process::exit(0);
}

type ProcessRef = std::sync::Arc<dyn Fn() -> tokio::task::JoinHandle<()> + Send + Sync + 'static>;

"#);

        let mut global_stmts = Vec::new();
        let mut local_stmts = Vec::new();
        let mut main_decl = None;

        if is_main {
            for stmt in stmts {
                match stmt {
                    Stmt::UseModule { .. } |
                    Stmt::Load { .. } |
                    Stmt::FnDecl { .. } |
                    Stmt::TaskDecl { .. } |
                    Stmt::ProcessDecl { .. } |
                    Stmt::Serverlet { .. } => {
                        global_stmts.push(stmt.clone());
                    }
                    Stmt::OrchestratorDecl { name, .. } => {
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
                main_decl = Some(Stmt::OrchestratorDecl {
                    name: "main".to_string(),
                    params: Vec::new(),
                    return_type: Type::Void,
                    body: crate::ast::Expr::Block(Vec::new()),
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

    fn compile_stmt(&mut self, stmt: &Stmt) -> String {
        match stmt {
            Stmt::Let { name, ty, value } => {
                let val_str = self.compile_expr(value);
                if let Some(t) = ty {
                    format!("let mut {}: {} = {};", name, self.compile_type(t), val_str)
                } else {
                    format!("let mut {} = {};", name, val_str)
                }
            }
            Stmt::Expr(expr) => self.compile_expr(expr),
            Stmt::OnStart(expr) => {
                let inner = self.compile_expr(expr);
                format!("// OnStart\n{}", inner)
            }
            Stmt::OnStop(expr) => {
                let inner = self.compile_expr(expr);
                format!("// OnStop\n{{\n    tokio::spawn(async move {{\n        tokio::signal::ctrl_c().await.unwrap();\n        {}\n        std::process::exit(0);\n    }});\n}}", inner.replace("\n", "\n        "))
            }
            Stmt::Return(val) => {
                if let Some(expr) = val {
                    format!("return {}", self.compile_expr(expr))
                } else {
                    "return".to_string()
                }
            }
            Stmt::FnDecl {
                name,
                params,
                return_type,
                body,
            } => {
                let params_str = params
                    .iter()
                    .map(|p| format!("{}: {}", p.name, self.compile_type(&p.ty)))
                    .collect::<Vec<String>>()
                    .join(", ");
                let ret_str = if *return_type == Type::Void {
                    "".to_string()
                } else {
                    format!(" -> {}", self.compile_type(return_type))
                };
                
                let body_str = if let Expr::Block(_) = body {
                    let force_semi = *return_type == Type::Void;
                    let inner = self.compile_block_inner(body, force_semi);
                    format!("{{\n    {}\n}}", inner.replace("\n", "\n    "))
                } else {
                    self.compile_expr(body)
                };

                let vis = if self.is_main { "" } else { "pub " };
                format!("{}fn {}({}){} {}", vis, name, params_str, ret_str, body_str)
            }
            Stmt::TaskDecl {
                name,
                params,
                return_type,
                body,
            } => {
                let params_str = params
                    .iter()
                    .map(|p| format!("{}: {}", p.name, self.compile_type(&p.ty)))
                    .collect::<Vec<String>>()
                    .join(", ");
                let ret_str = if *return_type == Type::Void {
                    "".to_string()
                } else {
                    format!(" -> {}", self.compile_type(return_type))
                };
                
                let body_str = if let Expr::Block(_) = body {
                    let force_semi = *return_type == Type::Void;
                    let inner = self.compile_block_inner(body, force_semi);
                    format!("{{\n    {}\n}}", inner.replace("\n", "\n    "))
                } else {
                    self.compile_expr(body)
                };
                
                let vis = if self.is_main { "" } else { "pub " };
                format!("{}async fn {}({}){} {}", vis, name, params_str, ret_str, body_str)
            }
            Stmt::ProcessDecl {
                name,
                params,
                return_type,
                body,
            } => {
                let params_str = params
                    .iter()
                    .map(|p| format!("{}: {}", p.name, self.compile_type(&p.ty)))
                    .collect::<Vec<String>>()
                    .join(", ");
                let ret_str = if *return_type == Type::Void {
                    "".to_string()
                } else {
                    format!(" -> {}", self.compile_type(return_type))
                };
                
                let body_str = if let Expr::Block(_) = body {
                    let force_semi = *return_type == Type::Void;
                    let inner = self.compile_block_inner(body, force_semi);
                    format!("{{\n    {}\n}}", inner.replace("\n", "\n    "))
                } else {
                    self.compile_expr(body)
                };
                
                let vis = if self.is_main { "" } else { "pub " };
                format!("{}async fn {}({}){} {}", vis, name, params_str, ret_str, body_str)
            }
            Stmt::OrchestratorDecl {
                name,
                params,
                return_type,
                body,
            } => {
                if name == "main" {
                    let mut decl_code = Vec::new();
                    let mut exec_code = Vec::new();
                    let local_stmts = self.local_stmts.clone();
                    for s in &local_stmts {
                        let mut compiled = self.compile_stmt(s);
                        
                        let mut is_auto_let = false;
                        let mut auto_name = String::new();
                        if let Stmt::Let { name, value, .. } = s {
                            if let Expr::TriggeredBlock { .. } = value {
                                is_auto_let = true;
                                auto_name = name.clone();
                            }
                        }

                        if let Stmt::Expr(Expr::TriggeredBlock { .. }) = s {
                            compiled = format!("({});", compiled);
                        } else if !compiled.ends_with(';') && !compiled.ends_with('}') {
                            compiled.push(';');
                        }

                        match s {
                            Stmt::Let { .. } => {
                                decl_code.push(compiled);
                                if is_auto_let {
                                    decl_code.push(format!("{}();", auto_name));
                                }
                            }
                            _ => {
                                exec_code.push(compiled);
                            }
                        }
                    }
                    let decl_body_str = decl_code.join("\n");
                    let exec_body_str = exec_code.join("\n");

                    // Compile orchestrator_main helper function
                    let params_str = params
                        .iter()
                        .map(|p| format!("{}: {}", p.name, self.compile_type(&p.ty)))
                        .collect::<Vec<String>>()
                        .join(", ");
                    
                    let mut has_process_array = false;
                    let mut param_name = String::new();
                    let mut initial_procs = Vec::new();
                    if params.len() == 1 {
                        if let Type::Array(inner, init_vals) = &params[0].ty {
                            if **inner == Type::Process {
                                has_process_array = true;
                                param_name = params[0].name.clone();
                                initial_procs = init_vals.clone();
                            }
                        }
                    }

                    let body_str = if let Expr::Block(_) = body {
                        let force_semi = true;
                        self.compile_block_inner(body, force_semi)
                    } else {
                        self.compile_expr(body)
                    };

                    let helper_fn_body = if has_process_array {
                        format!(
                            r#"struct ActiveState {{
        procs: Vec<ProcessRef>,
        handles: Vec<(ProcessRef, tokio::task::JoinHandle<()>)>,
    }}
    let state = std::sync::Arc::new(std::sync::Mutex::new(ActiveState {{
        procs: {}.clone(),
        handles: Vec::new(),
    }}));
    {{
        let init_procs = {{
            let locked = state.lock().unwrap();
            locked.procs.clone()
        }};
        let mut handles = Vec::new();
        for p in &init_procs {{
            let handle = p();
            handles.push((p.clone(), handle));
        }}
        let mut locked = state.lock().unwrap();
        locked.handles = handles;
    }}

    let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<ProcessRef>>(100);
    get_registry_update_orchestrator().lock().unwrap().push(tx);
    let state_clone = state.clone();
    tokio::spawn(async move {{
        while let Some(new_procs) = rx.recv().await {{
            let state_clone = state_clone.clone();
            tokio::spawn(async move {{
                let mut locked = state_clone.lock().unwrap();
                let mut to_keep = Vec::new();
                for (p, handle) in locked.handles.drain(..) {{
                    let mut found = false;
                    for np in &new_procs {{
                        if std::sync::Arc::ptr_eq(&p, np) {{
                            found = true;
                            break;
                        }}
                    }}
                    if found {{
                        to_keep.push((p, handle));
                    }} else {{
                        handle.abort();
                    }}
                }}
                locked.handles = to_keep;
                for np in &new_procs {{
                    let mut already_running = false;
                    for (p, _) in &locked.handles {{
                        if std::sync::Arc::ptr_eq(p, np) {{
                            already_running = true;
                            break;
                        }}
                    }}
                    if !already_running {{
                        let handle = np();
                        locked.handles.push((np.clone(), handle));
                    }}
                }}
                locked.procs = new_procs;
            }});
        }}
    }});

    {}"#,
                            param_name,
                            body_str
                        )
                    } else {
                        body_str
                    };

                    let helper_fn = format!(
                        "async fn orchestrator_main({}) {{\n    {}\n}}",
                        params_str,
                        helper_fn_body.replace("\n", "\n    ")
                    );

                    // Compile main entry point call
                    let mut args_str = params
                        .iter()
                        .map(|p| p.name.clone())
                        .collect::<Vec<String>>()
                        .join(", ");

                    if has_process_array {
                        if initial_procs.is_empty() {
                            args_str = "vec![]".to_string();
                        } else {
                            let procs_list = initial_procs
                                .iter()
                                .map(|name| name.clone())
                                .collect::<Vec<String>>()
                                .join(", ");
                            args_str = format!("vec![{}]", procs_list);
                        }
                    }

                    let mut main_body = String::new();
                    if !decl_body_str.is_empty() {
                        main_body.push_str(&decl_body_str);
                        main_body.push_str("\n");
                    }
                    main_body.push_str(&format!("orchestrator_main({}).await;\n", args_str));
                    if !exec_body_str.is_empty() {
                        main_body.push_str(&exec_body_str);
                        main_body.push_str("\n");
                    }
                    main_body.push_str("loop {\n    tokio::time::sleep(std::time::Duration::from_secs(3600)).await;\n}");

                    format!(
                        "{}\n\n#[tokio::main]\nasync fn main() -> Result<(), Box<dyn std::error::Error>> {{\n    {}\n}}",
                        helper_fn,
                        main_body.replace("\n", "\n    ")
                    )
                } else {
                    let params_str = params
                        .iter()
                        .map(|p| format!("{}: {}", p.name, self.compile_type(&p.ty)))
                        .collect::<Vec<String>>()
                        .join(", ");
                    let ret_str = if *return_type == Type::Void {
                        "".to_string()
                    } else {
                        format!(" -> {}", self.compile_type(return_type))
                    };
                    
                    let body_str = if let Expr::Block(_) = body {
                        let force_semi = *return_type == Type::Void;
                        let inner = self.compile_block_inner(body, force_semi);
                        format!("{{\n    {}\n}}", inner.replace("\n", "\n    "))
                    } else {
                        self.compile_expr(body)
                    };

                    format!("async fn {}({}){} {}", name, params_str, ret_str, body_str)
                }
            }
            Stmt::Trigger { event_name, args } => {
                let prefix = if self.is_main { "" } else { "crate::" };
                let func_name = format!("{}get_registry_{}", prefix, event_name);
                let payload = if args.is_empty() {
                    "()".to_string()
                } else if args.len() == 1 {
                    self.compile_expr(&args[0])
                } else {
                    let compiled_args = args
                        .iter()
                        .map(|a| self.compile_expr(a))
                        .collect::<Vec<String>>()
                        .join(", ");
                    format!("({})", compiled_args)
                };
                
                format!(
                    "let payload_eval = {};\nif let Ok(handlers) = {}().lock() {{\n    for tx in handlers.iter() {{\n        let _ = tx.try_send(payload_eval.clone());\n    }}\n}}",
                    payload, func_name
                )
            }
            Stmt::Parallel(stmts) => {
                let mut binds = Vec::new();
                let mut futures = Vec::new();

                let old_parallel = self.in_parallel;
                self.in_parallel = true;

                for s in stmts {
                    match s {
                        Stmt::Let { name, value, .. } => {
                            binds.push(name.clone());
                            futures.push(self.compile_expr(value));
                        }
                        Stmt::Expr(expr) => {
                            binds.push("_".to_string());
                            futures.push(self.compile_expr(expr));
                        }
                        _ => {
                            // Non-expressions inside parallel are wrapped in async block
                            binds.push("_".to_string());
                            futures.push(format!("async move {{ {} }}", self.compile_stmt(s)));
                        }
                    }
                }

                self.in_parallel = old_parallel;

                if futures.is_empty() {
                    "()".to_string()
                } else if futures.len() == 1 {
                    format!("let {} = {};", binds[0], futures[0])
                } else {
                    format!(
                        "let ({}) = tokio::join!({});",
                        binds.join(", "),
                        futures.join(", ")
                    )
                }
            }
            Stmt::While { cond, body } => {
                let cond_str = self.compile_expr(cond);
                let body_str = self.compile_expr(body);
                format!("while {} {}", cond_str, body_str)
            }
            Stmt::UseModule { local_name, .. } => {
                format!("mod {};", local_name)
            }
            Stmt::Load { .. } => "".to_string(),
            Stmt::Serverlet { name, state, handlers } => {
                let mut enum_variants = Vec::new();
                for h in handlers {
                    let variant_name = pascal_case(&h.name);
                    let mut fields = h.params
                        .iter()
                        .map(|p| format!("{}: {}", p.name, self.compile_type(&p.ty)))
                        .collect::<Vec<String>>();
                    let ret_ty = self.compile_type(&h.return_type);
                    fields.push(format!("reply_to: tokio::sync::oneshot::Sender<{}>", ret_ty));
                    enum_variants.push(format!("    {} {{ {} }},", variant_name, fields.join(", ")));
                }

                let msg_enum = format!(
                    "#[derive(Debug)]\npub enum {}Msg {{\n{}\n}}",
                    name,
                    enum_variants.join("\n")
                );

                let mut client_methods = Vec::new();
                for h in handlers {
                    let method_params = h.params
                        .iter()
                        .map(|p| format!("{}: {}", p.name, self.compile_type(&p.ty)))
                        .collect::<Vec<String>>()
                        .join(", ");
                    let self_params = if method_params.is_empty() {
                        "&self"
                    } else {
                        "&self, "
                    };
                    let ret_ty = self.compile_type(&h.return_type);
                    
                    let variant_name = pascal_case(&h.name);
                    let mut send_fields = h.params
                        .iter()
                        .map(|p| p.name.clone())
                        .collect::<Vec<String>>();
                    send_fields.push("reply_to: reply_tx".to_string());

                    client_methods.push(format!(
                        "    pub async fn {}({}{}) -> {} {{\n        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();\n        let _ = self.tx.send({}Msg::{} {{ {} }}).await;\n        reply_rx.await.unwrap()\n    }}",
                        h.name,
                        self_params,
                        method_params,
                        ret_ty,
                        name,
                        variant_name,
                        send_fields.join(", ")
                    ));
                }

                let client_struct = format!(
                    "#[derive(Clone, Debug)]\npub struct {}Client {{\n    tx: tokio::sync::mpsc::Sender<{}Msg>,\n}}\n\nimpl {}Client {{\n{}\n}}",
                    name, name, name, client_methods.join("\n\n")
                );

                let mut match_arms = Vec::new();
                for h in handlers {
                    let variant_name = pascal_case(&h.name);
                    let mut bindings = h.params
                        .iter()
                        .map(|p| p.name.clone())
                        .collect::<Vec<String>>();
                    bindings.push("reply_to".to_string());
                    let bindings_str = bindings.join(", ");

                    let body_str = self.compile_expr(&h.body);

                    match_arms.push(format!(
                        "                {}Msg::{} {{ {} }} => {{\n                    #[allow(unused_mut)]\n                    let mut handler = || {{ {} }};\n                    let res = handler();\n                    let _ = reply_to.send(res);\n                }}",
                        name,
                        variant_name,
                        bindings_str,
                        body_str
                    ));
                }

                let mut state_vars = Vec::new();
                for s in state {
                    if let Stmt::Let { name, ty, value } = s {
                        let val_str = self.compile_expr(value);
                        if let Some(t) = ty {
                            state_vars.push(format!("            let mut {}: {} = {};", name, self.compile_type(t), val_str));
                        } else {
                            state_vars.push(format!("            let mut {} = {};", name, val_str));
                        }
                    }
                }

                let start_fn = format!(
                    "#[allow(non_snake_case)]\npub fn start_{}() -> {}Client {{\n    let (tx, mut rx) = tokio::sync::mpsc::channel::<{}Msg>(100);\n    tokio::spawn(async move {{\n{}\n        while let Some(msg) = rx.recv().await {{\n            match msg {{\n{}\n            }}\n        }}\n    }});\n    {}Client {{ tx }}\n}}",
                    name, name, name,
                    state_vars.join("\n"),
                    match_arms.join("\n\n"),
                    name
                );

                format!("{}\n\n{}\n\n{}", msg_enum, client_struct, start_fn)
            }
        }
    }

    fn compile_block_inner(&mut self, body: &Expr, force_semicolons: bool) -> String {
        if let Expr::Block(stmts) = body {
            let mut parts = Vec::new();
            for (i, s) in stmts.iter().enumerate() {
                let is_last = i == stmts.len() - 1;
                match s {
                    Stmt::Expr(expr) if is_last && !force_semicolons => {
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

    fn compile_expr(&mut self, expr: &Expr) -> String {
        match expr {
            Expr::Literal(lit) => match lit {
                Literal::Int(v) => v.to_string(),
                Literal::Float(v) => format!("{:?}", v),
                Literal::Str(v) => format!("String::from({:?})", v),
                Literal::Bool(v) => v.to_string(),
            },
            Expr::Identifier(name) => name.clone(),
            Expr::Binary { op, lhs, rhs } => {
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
            Expr::Call { callee, args } => {
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
            Expr::Pipeline { value, function } => {
                // Compile value
                let val_str = self.compile_expr(value);

                // Compile pipeline function
                match &**function {
                    Expr::Identifier(name) => {
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
                    Expr::Call { callee, args } => {
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
            Expr::Block(_) => {
                let inner = self.compile_block_inner(expr, false);
                format!("{{\n    {}\n}}", inner.replace("\n", "\n    "))
            }
            Expr::If {
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
            Expr::ModuleCall {
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
            Expr::StartServerlet { name, args } => {
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
            Expr::AutomaticBlock { body } => {
                // Split the body into two phases:
                //   Setup  — `let x = start Service()` statements, hoisted before the loop.
                //             Serverlets are created once per process-block lifetime.
                //   Loop   — everything else, executed on every iteration.
                if let Expr::Block(stmts) = body.as_ref() {
                    let mut setup_code: Vec<String> = Vec::new();
                    let mut loop_code: Vec<String> = Vec::new();

                    for stmt in stmts {
                        let is_serverlet_start = match stmt {
                            Stmt::Let { value, .. } => {
                                matches!(value, Expr::StartServerlet { .. })
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
                            "std::sync::Arc::new(move || {{\n    tokio::spawn(async move {{\n        loop {{\n            {}\n        }}\n    }})\n}}) as ProcessRef",
                            loop_inner
                        )
                    } else {
                        // Two-phase: setup runs once, then loop repeats
                        let setup_inner = setup_code.join("\n        ");
                        let loop_inner = loop_code.join("\n            ");
                        format!(
                            "std::sync::Arc::new(move || {{\n    tokio::spawn(async move {{\n        // Serverlet setup — runs once per process-block lifetime\n        {}\n        loop {{\n            {}\n        }}\n    }})\n}}) as ProcessRef",
                            setup_inner,
                            loop_inner
                        )
                    }
                } else {
                    // Non-block body (edge case) — original behavior
                    let body_str = self.compile_expr(body);
                    format!(
                        "std::sync::Arc::new(move || {{\n    tokio::spawn(async move {{\n        loop {{\n            {}\n        }}\n    }})\n}}) as ProcessRef",
                        body_str.replace("\n", "\n        ")
                    )
                }
            }
            Expr::TriggeredBlock { event_name, params, body } => {
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
                    "std::sync::Arc::new(move || {{\n    let (tx, mut rx) = tokio::sync::mpsc::channel::<{}>(100);\n    {}().lock().unwrap().push(tx);\n    tokio::spawn(async move {{\n        while let Some(msg) = rx.recv().await {{\n            let {} = msg;\n            tokio::spawn(async move {{\n                {}\n            }});\n        }}\n    }});\n}})",
                    types_str, func_name, bindings, body_str
                )
            }
            Expr::StartProcess { target } => {
                let target_str = self.compile_expr(target);
                format!("{}()", target_str)
            }
            Expr::ArrayLiteral(elements) => {
                let elems_str = elements
                    .iter()
                    .map(|e| self.compile_expr(e))
                    .collect::<Vec<String>>()
                    .join(", ");
                format!("vec![{}]", elems_str)
            }
        }
    }

    fn compile_type(&self, ty: &Type) -> String {
        match ty {
            Type::Int => "i64".to_string(),
            Type::Float => "f64".to_string(),
            Type::Str => "String".to_string(),
            Type::Bool => "bool".to_string(),
            Type::Void => "()".to_string(),
            Type::Process => "ProcessRef".to_string(),
            Type::Array(inner, _init_vals) => format!("Vec<{}>", self.compile_type(inner)),
        }
    }
}
