use crate::ast::{ExprNode, StmtNode, Stmt, Type, Handler};
use super::core::{Codegen, pascal_case, runtime_preamble, SECRET_CHILD_FRAMES};

impl Codegen {
    pub fn compile_stmt(&mut self, stmt: &Stmt) -> String {
        match &stmt.node {
            StmtNode::Let { name, ty, value } => {
                let val_str = self.compile_expr(value);
                if let Some(t) = ty {
                    format!("let mut {}: {} = {};", name, self.compile_type(t), val_str)
                } else {
                    format!("let mut {} = {};", name, val_str)
                }
            }
            StmtNode::Expr(expr) => self.compile_expr(expr),
            StmtNode::OnStart(expr) => {
                let inner = self.compile_expr(expr);
                format!("// OnStart\n{}", inner)
            }
            StmtNode::OnStop(expr) => {
                let inner = self.compile_expr(expr);
                format!("// OnStop\n{{\n    tokio::spawn(async move {{\n        tokio::signal::ctrl_c().await.unwrap();\n        {}\n        std::process::exit(0);\n    }});\n}}", inner.replace("\n", "\n        "))
            }
            StmtNode::Return(val) => {
                if let Some(expr) = val {
                    format!("return {}", self.compile_expr(expr))
                } else {
                    "return".to_string()
                }
            }
            StmtNode::FnDecl {
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
                
                let body_str = if let ExprNode::Block(_) = &body.node {
                    let force_semi = *return_type == Type::Void;
                    let inner = self.compile_block_inner(body, force_semi);
                    format!("{{\n    {}\n}}", inner.replace("\n", "\n    "))
                } else {
                    self.compile_expr(body)
                };

                let vis = if self.is_main { "" } else { "pub " };
                format!("{}fn {}({}){} {}", vis, name, params_str, ret_str, body_str)
            }
            StmtNode::TaskDecl {
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
                
                let body_str = if let ExprNode::Block(_) = &body.node {
                    let force_semi = *return_type == Type::Void;
                    let inner = self.compile_block_inner(body, force_semi);
                    format!("{{\n    {}\n}}", inner.replace("\n", "\n    "))
                } else {
                    self.compile_expr(body)
                };
                
                let vis = if self.is_main { "" } else { "pub " };
                format!("{}async fn {}({}){} {}", vis, name, params_str, ret_str, body_str)
            }
            StmtNode::ProcessDecl {
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
                
                let body_str = if let ExprNode::Block(_) = &body.node {
                    let force_semi = *return_type == Type::Void;
                    let inner = self.compile_block_inner(body, force_semi);
                    format!("{{\n    {}\n}}", inner.replace("\n", "\n    "))
                } else {
                    self.compile_expr(body)
                };
                
                let vis = if self.is_main { "" } else { "pub " };
                format!("{}async fn {}({}){} {}", vis, name, params_str, ret_str, body_str)
            }
            StmtNode::OrchestratorDecl {
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
                        if let StmtNode::Let { name, value, .. } = &s.node {
                            if let ExprNode::TriggeredBlock { .. } = &value.node {
                                is_auto_let = true;
                                auto_name = name.clone();
                            }
                        }

                        if let StmtNode::Expr(crate::ast::Spanned { node: ExprNode::TriggeredBlock { .. }, .. }) = &s.node {
                            compiled = format!("({});", compiled);
                        } else if !compiled.ends_with(';') && !compiled.ends_with('}') {
                            compiled.push(';');
                        }

                        match &s.node {
                            StmtNode::Let { .. } => {
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
                            if *inner.as_ref() == Type::Process {
                                has_process_array = true;
                                param_name = params[0].name.clone();
                                initial_procs = init_vals.clone();
                            }
                        }
                    }

                    let body_str = if let ExprNode::Block(_) = &body.node {
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

    let (tx, mut rx) = tokio::sync::mpsc::channel::<std::sync::Arc<Vec<ProcessRef>>>(100);
    get_registry_update_orchestrator().lock().unwrap().push(tx);
    let state_clone = state.clone();
    tokio::spawn(async move {{
        while let Some(msg) = rx.recv().await {{
            let new_procs = (*msg).clone();
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
                    
                    let body_str = if let ExprNode::Block(_) = &body.node {
                        let force_semi = *return_type == Type::Void;
                        let inner = self.compile_block_inner(body, force_semi);
                        format!("{{\n    {}\n}}", inner.replace("\n", "\n    "))
                    } else {
                        self.compile_expr(body)
                    };

                    format!("async fn {}({}){} {}", name, params_str, ret_str, body_str)
                }
            }
            StmtNode::Trigger { event_name, args } => {
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
                    "let payload_eval = std::sync::Arc::new({});\nif let Ok(handlers) = {}().lock() {{\n    for tx in handlers.iter() {{\n        if tx.try_send(std::sync::Arc::clone(&payload_eval)).is_err() {{\n            eprintln!(\"[orchestrate] warning: dropped event '{}' — subscriber channel full\");\n        }}\n    }}\n}}",
                    payload, func_name, event_name
                )
            }
            StmtNode::Parallel(stmts) => {
                let mut binds = Vec::new();
                let mut futures = Vec::new();

                let old_parallel = self.in_parallel;
                self.in_parallel = true;

                for s in stmts {
                    match &s.node {
                        StmtNode::Let { name, value, .. } => {
                            binds.push(name.clone());
                            futures.push(self.compile_expr(value));
                        }
                        StmtNode::Expr(expr) => {
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
            StmtNode::While { cond, body } => {
                let cond_str = self.compile_expr(cond);
                let body_str = self.compile_expr(body);
                format!("while {} {}", cond_str, body_str)
            }
            StmtNode::UseModule { local_name, .. } => {
                format!("mod {};", local_name)
            }
            StmtNode::Load { .. } | StmtNode::LoadForeign { .. } => "".to_string(),
            StmtNode::Serverlet { name, state, handlers, secret } => {
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
                    if let StmtNode::Let { name, ty, value } = &s.node {
                        let val_str = self.compile_expr(value);
                        if let Some(t) = ty {
                            state_vars.push(format!("            let mut {}: {} = {};", name, self.compile_type(t), val_str));
                        } else {
                            state_vars.push(format!("            let mut {} = {};", name, val_str));
                        }
                    }
                }

                if *secret {
                    // Secret serverlet: the handler logic is emitted as a separate
                    // program (pushed to self.secret_programs); the orchestrator
                    // only gets a mirror that relays messages over IPC.
                    let start_fn = self.compile_secret_mirror(name, handlers);
                    let child_program = self.compile_secret_program(name, state, handlers);
                    self.secret_programs.push((format!("secret_{}", name), child_program));
                    return format!("{}\n\n{}\n\n{}", msg_enum, client_struct, start_fn);
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
            StmtNode::StructDef { name, fields } => {
                let fields_str = fields.iter()
                    .map(|(fname, fty)| format!("    pub {}: {},", fname, self.compile_type(fty)))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("#[derive(Clone, Debug)]\npub struct {} {{\n{}\n}}", name, fields_str)
            }
        }
    }

    /// Orchestrator-side mirror for a secret serverlet: same XClient/XMsg surface,
    /// but `start_X` spawns the separate `secret_X` process and relays each call
    /// over its stdio instead of running the handler bodies inline.
    fn compile_secret_mirror(&mut self, name: &str, handlers: &[Handler]) -> String {
        if let Some(reason) = secret_unsupported_reason(handlers) {
            return format!("compile_error!(\"secret serverlet '{}': {}\");\n", name, reason);
        }

        let mut arms = Vec::new();
        for (k, h) in handlers.iter().enumerate() {
            let variant = pascal_case(&h.name);
            let param_names: Vec<String> = h.params.iter().map(|p| p.name.clone()).collect();
            let binding = if param_names.is_empty() {
                "reply_to".to_string()
            } else {
                format!("{}, reply_to", param_names.join(", "))
            };

            let mut req_fields = vec![format!("\"{}\".to_string()", k)];
            for p in &h.params {
                let enc = match p.ty {
                    Type::Str => p.name.clone(),
                    Type::Float => format!("format!(\"{{:?}}\", {})", p.name),
                    _ => format!("{}.to_string()", p.name),
                };
                req_fields.push(enc);
            }

            let decode = if h.return_type == Type::Void {
                "()".to_string()
            } else {
                match h.return_type {
                    Type::Str => "__f.get(0).cloned().unwrap_or_default()".to_string(),
                    Type::Float => "__f.get(0).and_then(|s| s.parse::<f64>().ok()).unwrap_or_default()".to_string(),
                    Type::Bool => "__f.get(0).and_then(|s| s.parse::<bool>().ok()).unwrap_or_default()".to_string(),
                    _ => "__f.get(0).and_then(|s| s.parse::<i64>().ok()).unwrap_or_default()".to_string(),
                }
            };

            arms.push(format!(
"                {n}Msg::{v} {{ {b} }} => {{\n                    let __req: Vec<String> = vec![{req}];\n                    if __secret_write_frame(&mut __cin, &__req).await.is_err() {{\n                        eprintln!(\"[orchestrate] secret serverlet '{n}' is not reachable\");\n                        let _ = reply_to.send(Default::default());\n                        continue;\n                    }}\n                    match __secret_read_frame(&mut __cout).await {{\n                        Ok(Some(__f)) => {{ let _ = reply_to.send({dec}); }}\n                        _ => {{ eprintln!(\"[orchestrate] secret serverlet '{n}' exited unexpectedly\"); let _ = reply_to.send(Default::default()); }}\n                    }}\n                }}",
                n = name, v = variant, b = binding, req = req_fields.join(", "), dec = decode
            ));
        }

        format!(
"#[allow(non_snake_case)]\npub fn start_{n}() -> {n}Client {{\n    let (tx, mut rx) = tokio::sync::mpsc::channel::<{n}Msg>(100);\n    tokio::spawn(async move {{\n        use tokio::io::{{AsyncReadExt, AsyncWriteExt}};\n        let __exe = std::env::current_exe().expect(\"current_exe\");\n        let __dir = __exe.parent().expect(\"exe dir\").to_path_buf();\n        let __bin = if cfg!(target_os = \"windows\") {{ \"secret_{n}.exe\" }} else {{ \"secret_{n}\" }};\n        let mut __child = match tokio::process::Command::new(__dir.join(__bin))\n            .stdin(std::process::Stdio::piped())\n            .stdout(std::process::Stdio::piped())\n            .spawn() {{\n            Ok(c) => c,\n            Err(e) => {{ eprintln!(\"[orchestrate] failed to spawn secret serverlet '{n}': {{}}\", e); return; }}\n        }};\n        let mut __cin = __child.stdin.take().expect(\"child stdin\");\n        let mut __cout = __child.stdout.take().expect(\"child stdout\");\n        let _ = __secret_read_frame(&mut __cout).await;\n        while let Some(msg) = rx.recv().await {{\n            match msg {{\n{arms}\n            }}\n        }}\n        drop(__cin);\n        let _ = __child.wait().await;\n    }});\n    {n}Client {{ tx }}\n}}",
            n = name, arms = arms.join("\n")
        )
    }

    /// The standalone child program for a secret serverlet: owns the state, reads
    /// framed requests from stdin, dispatches by handler index, writes framed
    /// replies to stdout. `print` is routed to stderr (stdout is the IPC channel).
    fn compile_secret_program(&mut self, name: &str, state: &[Stmt], handlers: &[Handler]) -> String {
        if let Some(reason) = secret_unsupported_reason(handlers) {
            return format!("compile_error!(\"secret serverlet '{}': {}\");\n", name, reason);
        }

        let mut state_vars = Vec::new();
        for s in state {
            if let StmtNode::Let { name: vname, ty, value } = &s.node {
                let val_str = self.compile_expr(value);
                if let Some(t) = ty {
                    state_vars.push(format!("    let mut {}: {} = {};", vname, self.compile_type(t), val_str));
                } else {
                    state_vars.push(format!("    let mut {} = {};", vname, val_str));
                }
            }
        }

        let mut arms = Vec::new();
        for (k, h) in handlers.iter().enumerate() {
            let mut arg_lets = Vec::new();
            for (j, p) in h.params.iter().enumerate() {
                let idx = j + 1;
                let decode = match p.ty {
                    Type::Str => format!("__fields[{}].clone()", idx),
                    Type::Int => format!("__fields[{}].parse::<i64>().unwrap_or_default()", idx),
                    Type::Float => format!("__fields[{}].parse::<f64>().unwrap_or_default()", idx),
                    Type::Bool => format!("__fields[{}].parse::<bool>().unwrap_or_default()", idx),
                    _ => "Default::default()".to_string(),
                };
                arg_lets.push(format!("                        let {}: {} = {};", p.name, self.compile_type(&p.ty), decode));
            }
            let body = self.compile_expr(&h.body);
            let finish = if h.return_type == Type::Void {
                "                        let _ = __handler();\n                        let _ = __frame_write(&mut __writer, &[]);".to_string()
            } else {
                let enc = match h.return_type {
                    Type::Float => "format!(\"{:?}\", __r)".to_string(),
                    Type::Str => "__r".to_string(),
                    _ => "__r.to_string()".to_string(),
                };
                format!("                        let __r = __handler();\n                        let _ = __frame_write(&mut __writer, &[{}]);", enc)
            };
            arms.push(format!(
"                    {k}usize => {{\n{args}\n                        #[allow(unused_mut)]\n                        let mut __handler = || {{ {body} }};\n{finish}\n                    }}",
                k = k, args = arg_lets.join("\n"), body = body, finish = finish
            ));
        }

        format!(
"// Generated by Orchestrate Compiler — secret serverlet '{name}'\n#![allow(unused_variables)]\n#![allow(dead_code)]\n#![allow(unused_imports)]\n#![allow(unused_parens)]\n#![allow(unused_mut)]\n\n{preamble}\n{frames}\nfn main() {{\n    let __stdin = std::io::stdin();\n    let mut __reader = std::io::BufReader::new(__stdin.lock());\n    let __stdout = std::io::stdout();\n    let mut __writer = std::io::BufWriter::new(__stdout.lock());\n\n{state}\n\n    let _ = __frame_write(&mut __writer, &[\"ready\".to_string()]);\n\n    loop {{\n        match __frame_read(&mut __reader) {{\n            Ok(Some(__fields)) => {{\n                if __fields.is_empty() {{ continue; }}\n                let __idx: usize = __fields[0].parse().unwrap_or(usize::MAX);\n                match __idx {{\n{arms}\n                    _ => {{}}\n                }}\n            }}\n            Ok(None) => break,\n            Err(_) => break,\n        }}\n    }}\n}}\n",
            name = name,
            preamble = runtime_preamble(true),
            frames = SECRET_CHILD_FRAMES,
            state = state_vars.join("\n"),
            arms = arms.join("\n")
        )
    }


}

/// Returns Some(reason) if any handler uses a type the secret-serverlet IPC layer
/// can't marshal in v1 (only int, float, bool, string params; those plus void as
/// returns). Kept free-standing so both the mirror and the child program guard
/// identically.
fn secret_unsupported_reason(handlers: &[Handler]) -> Option<String> {
    for h in handlers {
        for p in &h.params {
            if !matches!(p.ty, Type::Int | Type::Float | Type::Str | Type::Bool) {
                return Some(format!(
                    "handler '{}' parameter '{}' uses an unsupported type; secret serverlets support only int, float, bool, string in v1",
                    h.name, p.name
                ));
            }
        }
        if !matches!(h.return_type, Type::Int | Type::Float | Type::Str | Type::Bool | Type::Void) {
            return Some(format!(
                "handler '{}' uses an unsupported return type; secret serverlets support int, float, bool, string, void in v1",
                h.name
            ));
        }
    }
    None
}
