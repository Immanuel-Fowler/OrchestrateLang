use crate::ast::{ExprNode, StmtNode, Stmt, Type, Handler};
use super::core::{Codegen, pascal_case, runtime_preamble, SECRET_CHILD_FRAMES};

fn type_params_str(type_params: &[String]) -> String {
    if type_params.is_empty() {
        String::new()
    } else {
        let bounds = type_params.iter()
            .map(|t| format!("{}: Clone + std::fmt::Debug + Send + Sync + 'static", t))
            .collect::<Vec<_>>()
            .join(", ");
        format!("<{}>", bounds)
    }
}

impl Codegen {
    pub fn compile_stmt(&mut self, stmt: &Stmt) -> String {
        match &stmt.node {
            StmtNode::Let { name, ty, value } => {
                let val_str = self.compile_expr(value);
                self.define_local(name);
                if let Some(t) = ty {
                    // Closure types can't be annotated directly — let Rust infer
                    if matches!(t, Type::Fn(_, _)) {
                        format!("let mut {} = {};", name, val_str)
                    } else {
                        format!("let mut {}: {} = {};", name, self.compile_type(t), val_str)
                    }
                } else {
                    format!("let mut {} = {};", name, val_str)
                }
            }
            StmtNode::Break => "break".to_string(),
            StmtNode::Continue => "continue".to_string(),
            StmtNode::Expr(expr) => self.compile_expr(expr),
            StmtNode::Host { .. } | StmtNode::OnTick { .. } | StmtNode::OnFixedTick { .. } => String::new(),
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
            StmtNode::FnDecl { name, params, return_type, body, type_params } => {
                let generics = type_params_str(type_params);
                let params_str = params.iter()
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
                format!("{}fn {}{}({}){} {}", vis, name, generics, params_str, ret_str, body_str)
            }
            StmtNode::TaskDecl { name, params, return_type, body, type_params } => {
                let generics = type_params_str(type_params);
                let params_str = params.iter()
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
                format!("{}async fn {}{}({}){} {}", vis, name, generics, params_str, ret_str, body_str)
            }
            StmtNode::ProcessDecl { name, params, return_type, body, type_params } => {
                let generics = type_params_str(type_params);
                let params_str = params.iter()
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
                format!("{}async fn {}{}({}){} {}", vis, name, generics, params_str, ret_str, body_str)
            }
            StmtNode::OrchestratorDecl { name, params, return_type, body } => {
                if name == "main" {
                    let mut decl_code = Vec::new();
                    let mut exec_code = Vec::new();
                    let local_stmts = self.local_stmts.clone();
                    for s in &local_stmts {
                        if self.library && matches!(&s.node, StmtNode::OnStart(_) | StmtNode::OnStop(_) | StmtNode::OnTick { .. } | StmtNode::OnFixedTick { .. }) { continue; }
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
                            compiled = if self.library { format!("({})();", compiled) } else { format!("({});", compiled) };
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
                            _ => { exec_code.push(compiled); }
                        }
                    }
                    let decl_body_str = decl_code.join("\n");
                    let exec_body_str = exec_code.join("\n");

                    let params_str = params.iter()
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
                        self.compile_block_inner(body, true)
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
        procs: {param}.clone(),
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

    // Liveness monitor: restart processes that exit unexpectedly
    let state_monitor = state.clone();
    tokio::spawn(async move {{
        loop {{
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let mut locked = state_monitor.lock().unwrap();
            let mut to_restart = Vec::new();
            locked.handles.retain(|(p, h)| {{
                if h.is_finished() {{
                    to_restart.push(p.clone());
                    false
                }} else {{
                    true
                }}
            }});
            for p in to_restart {{
                eprintln!("[orchestrate] process exited unexpectedly — restarting");
                let handle = p();
                locked.handles.push((p, handle));
            }}
        }}
    }});

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

    {body}"#,
                            param = param_name,
                            body = body_str
                        )
                    } else {
                        body_str
                    };

                    let helper_fn = format!(
                        "async fn orchestrator_main({}) {{\n    {}\n}}",
                        params_str,
                        helper_fn_body.replace("\n", "\n    ")
                    );

                    let mut args_str = params.iter().map(|p| p.name.clone()).collect::<Vec<String>>().join(", ");
                    if has_process_array {
                        if initial_procs.is_empty() {
                            args_str = "vec![]".to_string();
                        } else {
                            let procs_list = initial_procs.join(", ");
                            args_str = format!("vec![{}]", procs_list);
                        }
                    }

                    if self.library {
                        return self.library_entry(&helper_fn, &decl_body_str, &exec_body_str, &args_str);
                    }
                    let mut main_body = String::new();
                    if !decl_body_str.is_empty() {
                        main_body.push_str(&decl_body_str);
                        main_body.push('\n');
                    }
                    main_body.push_str(&format!("orchestrator_main({}).await;\n", args_str));
                    if !exec_body_str.is_empty() {
                        main_body.push_str(&exec_body_str);
                        main_body.push('\n');
                    }
                    main_body.push_str("loop {\n    tokio::time::sleep(std::time::Duration::from_secs(3600)).await;\n}");

                    format!(
                        "{}\n\n#[tokio::main]\nasync fn main() -> Result<(), Box<dyn std::error::Error>> {{\n    {}\n}}",
                        helper_fn,
                        main_body.replace("\n", "\n    ")
                    )
                } else {
                    let params_str = params.iter()
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
                    let compiled_args = args.iter().map(|a| self.compile_expr(a)).collect::<Vec<String>>().join(", ");
                    format!("({})", compiled_args)
                };
                if self.library && event_name != "update_orchestrator" {
                    return format!("let context = crate::__orch_context(); let value = std::sync::Arc::new({payload}); let handlers = {func_name}().lock().unwrap().clone(); for handler in handlers {{ context.queue_event(handler(value.clone())); }}");
                }
                format!(
                    "let payload_eval = std::sync::Arc::new({});\nif let Ok(handlers) = {}().lock() {{\n    for tx in handlers.iter() {{\n        if tx.try_send(std::sync::Arc::clone(&payload_eval)).is_err() {{\n            eprintln!(\"[orchestrate] warning: dropped event '{}' — subscriber channel full\");\n        }}\n    }}\n}}",
                    payload, func_name, event_name
                )
            }
            StmtNode::Parallel(stmts) => {
                let mut binds = Vec::new();
                let mut futures = Vec::new();

                // Each branch is its own future, so a synchronous call such as a
                // foreign function joins alongside an awaited task.
                for s in stmts {
                    match &s.node {
                        StmtNode::Let { name, value, .. } => {
                            binds.push(name.clone());
                            futures.push(format!("async {{ {} }}", self.compile_expr(value)));
                        }
                        StmtNode::Expr(expr) => {
                            binds.push("_".to_string());
                            futures.push(format!("async {{ {} }}", self.compile_expr(expr)));
                        }
                        _ => {
                            binds.push("_".to_string());
                            futures.push(format!("async {{ {} }}", self.compile_stmt(s)));
                        }
                    }
                }
                for bind in &binds {
                    if bind != "_" { self.define_local(bind); }
                }

                if futures.is_empty() {
                    "()".to_string()
                } else if futures.len() == 1 {
                    format!("let {} = ({}).await;", binds[0], futures[0])
                } else {
                    format!("let ({}) = tokio::join!({});", binds.join(", "), futures.join(", "))
                }
            }
            StmtNode::ForIn { var, index_var, iter, body } => {
                // Build iterator expression — detect range() for efficient codegen
                let iter_str = if let ExprNode::Call { callee, args } = &iter.node {
                    if callee == "range" {
                        if args.len() == 1 {
                            format!("(0i64..{} as i64)", self.compile_expr(&args[0]))
                        } else if args.len() == 2 {
                            format!("({} as i64..{} as i64)", self.compile_expr(&args[0]), self.compile_expr(&args[1]))
                        } else {
                            format!("({}).clone().into_iter()", self.compile_expr(iter))
                        }
                    } else {
                        format!("({}).clone().into_iter()", self.compile_expr(iter))
                    }
                } else {
                    format!("({}).clone().into_iter()", self.compile_expr(iter))
                };

                self.push_scope();
                self.define_local(var);
                if let Some(idx) = index_var { self.define_local(idx); }
                let compiled = if let Some(idx) = index_var {
                    let inner = self.compile_block_inner(body, true);
                    format!(
                        "for (__orch_enum_i, {}) in ({}).enumerate() {{\n    let {} = __orch_enum_i as i64;\n    {}\n}}",
                        var, iter_str, idx, inner
                    )
                } else {
                    let body_str = self.compile_expr(body);
                    format!("for {} in {} {}", var, iter_str, body_str)
                };
                self.pop_scope();
                compiled
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
            StmtNode::Serverlet { name, state, handlers, secret, crash_handler, sandbox, landline, grants } => {
                // Sandboxed serverlet: stash a WASM guest crate (compiled by the
                // driver). The orchestrator still runs the serverlet in-process for
                // now — host integration is step 3 — and the driver warns about it.
                if sandbox.is_some() {
                    let guest = self.compile_sandbox_guest(name, state, handlers);
                    self.sandbox_programs.push((name.clone(), guest));
                }
                let mut enum_variants = Vec::new();
                for h in handlers {
                    let variant_name = pascal_case(&h.name);
                    let mut fields = h.params.iter()
                        .map(|p| format!("{}: {}", p.name, self.compile_type(&p.ty)))
                        .collect::<Vec<String>>();
                    let ret_ty = self.compile_type(&h.return_type);
                    fields.push(format!("reply_to: tokio::sync::oneshot::Sender<{}>", ret_ty));
                    enum_variants.push(format!("    {} {{ {} }},", variant_name, fields.join(", ")));
                }

                let msg_enum = format!(
                    "#[derive(Debug)]\npub enum {}Msg {{\n{}\n}}",
                    name, enum_variants.join("\n")
                );

                let mut client_methods = Vec::new();
                for h in handlers {
                    let method_params = h.params.iter()
                        .map(|p| format!("{}: {}", p.name, self.compile_type(&p.ty)))
                        .collect::<Vec<String>>()
                        .join(", ");
                    let self_params = if method_params.is_empty() { "&self" } else { "&self, " };
                    let ret_ty = self.compile_type(&h.return_type);
                    let variant_name = pascal_case(&h.name);
                    let mut send_fields = h.params.iter().map(|p| p.name.clone()).collect::<Vec<String>>();
                    send_fields.push("reply_to: reply_tx".to_string());

                    // Use ? on reply_rx.await so channel errors propagate cleanly
                    let await_expr = if h.return_type == Type::Void {
                        "let _ = reply_rx.await;".to_string()
                    } else {
                        format!("reply_rx.await.unwrap_or_default()")
                    };

                    let budget = landline.as_ref().and_then(|config| config.budget_micros);
                    let body = if let Some(micros) = budget {
                        // A budgeted call returns early instead of waiting past its deadline.
                        let fallback = match landline.as_ref().map(|config| config.late) {
                            Some(crate::ast::LatePolicy::Latest) if h.return_type != Type::Void => {
                                format!("self.latest.{}.lock().unwrap().clone().unwrap_or_default()", h.name)
                            }
                            _ => "Default::default()".to_string(),
                        };
                        format!(
                            "        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();\n        let call = async {{\n            let _ = self.tx.send({}Msg::{} {{ {} }}).await;\n            reply_rx.await.ok()\n        }};\n        match tokio::time::timeout(std::time::Duration::from_micros({}), call).await {{\n            Ok(Some(value)) => value,\n            _ => {},\n        }}",
                            name, variant_name, send_fields.join(", "), micros, fallback
                        )
                    } else {
                        format!(
                            "        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();\n        let _ = self.tx.send({}Msg::{} {{ {} }}).await;\n        {}",
                            name, variant_name, send_fields.join(", "), await_expr
                        )
                    };

                    client_methods.push(format!(
                        "    pub async fn {}({}{}) -> {} {{\n{}\n    }}",
                        h.name, self_params, method_params, ret_ty, body
                    ));
                }

                // Landline clients share each handler's most recent result with their actor,
                // for the `late: "latest"` budget policy.
                let (latest_struct, latest_field) = if landline.is_some() {
                    let fields = handlers.iter()
                        .filter(|h| h.return_type != Type::Void)
                        .map(|h| format!("    {}: std::sync::Mutex<Option<{}>>,", h.name, self.compile_type(&h.return_type)))
                        .collect::<Vec<_>>()
                        .join("\n");
                    (
                        format!("#[derive(Default, Debug)]\npub struct {}Latest {{\n{}\n}}\n\n", name, fields),
                        format!("    latest: std::sync::Arc<{}Latest>,\n", name),
                    )
                } else {
                    (String::new(), String::new())
                };

                let client_struct = format!(
                    "{}#[derive(Clone, Debug)]\npub struct {}Client {{\n    tx: tokio::sync::mpsc::Sender<{}Msg>,\n{}}}\n\nimpl {}Client {{\n{}\n}}",
                    latest_struct, name, name, latest_field, name, client_methods.join("\n\n")
                );

                if let Some(config) = landline {
                    let start_fn = self.compile_python_mirror(name, handlers, crash_handler, grants, config);
                    return format!("{}\n\n{}\n\n{}", msg_enum, client_struct, start_fn);
                }

                // Build match arms with catch_unwind for panic safety
                let mut match_arms = Vec::new();
                for h in handlers {
                    let variant_name = pascal_case(&h.name);
                    let mut bindings = h.params.iter().map(|p| p.name.clone()).collect::<Vec<String>>();
                    bindings.push("reply_to".to_string());
                    let bindings_str = bindings.join(", ");
                    let body_str = self.compile_expr(&h.body);

                    let handler_name_str = &h.name;

                    let crash_recovery = if let Some((err_name, crash_body)) = crash_handler {
                        let crash_body_str = self.compile_expr(crash_body);
                        format!(
                            "Err(__panic_err) => {{\n                    let {err_name} = format!(\"{{:?}}\", __panic_err);\n                    eprintln!(\"[orchestrate] serverlet '{name}' handler '{handler_name_str}' panicked: {{}}\", {err_name});\n                    {crash_body_str};\n                    let _ = reply_to.send(Default::default());\n                }}",
                            err_name = err_name,
                            name = name,
                            handler_name_str = handler_name_str,
                            crash_body_str = crash_body_str,
                        )
                    } else {
                        format!(
                            "Err(__panic_err) => {{\n                    eprintln!(\"[orchestrate] serverlet '{name}' handler '{handler_name_str}' panicked: {{:?}}\", __panic_err);\n                    let _ = reply_to.send(Default::default());\n                }}",
                            name = name,
                            handler_name_str = handler_name_str,
                        )
                    };

                    match_arms.push(format!(
                        "                {}Msg::{} {{ {} }} => {{\n                    #[allow(unused_mut)]\n                    let mut __handler = std::panic::AssertUnwindSafe(|| {{ {} }});\n                    match std::panic::catch_unwind(__handler) {{\n                        Ok(res) => {{ let _ = reply_to.send(res); }}\n                        {}\n                    }}\n                }}",
                        name, variant_name, bindings_str, body_str, crash_recovery
                    ));
                }

                let mut state_vars = Vec::new();
                for s in state {
                    if let StmtNode::Let { name: vname, ty, value } = &s.node {
                        let val_str = self.compile_expr(value);
                        if let Some(t) = ty {
                            state_vars.push(format!("            let mut {}: {} = {};", vname, self.compile_type(t), val_str));
                        } else {
                            state_vars.push(format!("            let mut {} = {};", vname, val_str));
                        }
                    }
                }

                if *secret {
                    let mut start_fn = self.compile_secret_mirror(name, handlers);
                    if self.library {
                        start_fn = start_fn.replace(".stdout(std::process::Stdio::piped())", ".stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())");
                        start_fn = start_fn.replace("let mut __cout = __child.stdout.take().expect(\"child stdout\");", "let mut __cout = __child.stdout.take().expect(\"child stdout\"); let stderr = __child.stderr.take().expect(\"child stderr\"); crate::__orch_spawn(async move { use tokio::io::AsyncBufReadExt; let mut lines = tokio::io::BufReader::new(stderr).lines(); while let Ok(Some(line)) = lines.next_line().await { crate::__orch_log(crate::LogLevel::Info, line); } });");
                        start_fn = start_fn.replace("tokio::spawn(", "__ORCH_LINE_SPAWN(")
                            .replace("let __dir = __exe.parent().expect(\"exe dir\").to_path_buf();", "let __dir = crate::__orch_context().assets.clone();")
                            .replace("__secret_read_frame(", "crate::__orch_read_frame(")
                            .replace("rx.recv().await", "crate::__orch_recv(&mut rx).await");
                    }
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
                format!("#[derive(Clone, Debug, Default)]\npub struct {} {{\n{}\n}}", name, fields_str)
            }
            StmtNode::EnumDef { name, variants } => {
                let variants_str = variants.iter().map(|v| {
                    match &v.payload {
                        Some(ty) => format!("    {}({}),", v.name, self.compile_type(ty)),
                        None => format!("    {},", v.name),
                    }
                }).collect::<Vec<_>>().join("\n");
                format!("#[derive(Clone, Debug)]\npub enum {} {{\n{}\n}}", name, variants_str)
            }
        }
    }

    fn compile_secret_mirror(&mut self, name: &str, handlers: &[Handler]) -> String {
        if let Some(reason) = wire_unsupported_reason(handlers, &self.struct_defs) {
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

            // CALL payload: the handler id, then each argument in the wire encoding.
            let encode_args = h.params.iter()
                .map(|p| format!("                    {}.wire_encode(&mut __payload);\n", p.name))
                .collect::<String>();

            let decode = if h.return_type == Type::Void {
                "()".to_string()
            } else {
                format!("{{ let mut __p = 0usize; <{} as OrchWire>::wire_decode(&__f.payload, &mut __p).unwrap_or_default() }}", self.compile_type(&h.return_type))
            };

            arms.push(format!(
"                {n}Msg::{v} {{ {b} }} => {{\n                    __next_call = __next_call.wrapping_add(1);\n                    let mut __payload = Vec::new();\n                    {k}i64.wire_encode(&mut __payload);\n{args}                    if __secret_write_frame(&mut __cin, ORCH_KIND_CALL, __next_call, &__payload).await.is_err() {{\n                        eprintln!(\"[orchestrate] secret serverlet '{n}' is not reachable\");\n                        let _ = reply_to.send(Default::default());\n                        continue;\n                    }}\n                    match __secret_read_frame(&mut __cout).await {{\n                        Ok(Some(__f)) if __f.kind == ORCH_KIND_REPLY && __f.call_id == __next_call => {{ let _ = reply_to.send({dec}); }}\n                        Ok(Some(__f)) if __f.kind == ORCH_KIND_ERROR && __f.call_id == __next_call => {{\n                            let mut __p = 0usize;\n                            let __msg: String = OrchWire::wire_decode(&__f.payload, &mut __p).unwrap_or_default();\n                            eprintln!(\"[orchestrate] secret serverlet '{n}' handler '{h}' failed: {{}}\", __msg);\n                            let _ = reply_to.send(Default::default());\n                        }}\n                        _ => {{ eprintln!(\"[orchestrate] secret serverlet '{n}' exited unexpectedly\"); let _ = reply_to.send(Default::default()); }}\n                    }}\n                }}",
                n = name, v = variant, b = binding, k = k, h = h.name, args = encode_args, dec = decode
            ));
        }

        format!(
"#[allow(non_snake_case)]\npub fn start_{n}() -> {n}Client {{\n    let (tx, mut rx) = tokio::sync::mpsc::channel::<{n}Msg>(100);\n    tokio::spawn(async move {{\n        use tokio::io::{{AsyncReadExt, AsyncWriteExt}};\n        let __exe = std::env::current_exe().expect(\"current_exe\");\n        let __dir = __exe.parent().expect(\"exe dir\").to_path_buf();\n        let __bin = if cfg!(target_os = \"windows\") {{ \"secret_{n}.exe\" }} else {{ \"secret_{n}\" }};\n        let mut __child = match tokio::process::Command::new(__dir.join(__bin))\n            .kill_on_drop(true)\n            .stdin(std::process::Stdio::piped())\n            .stdout(std::process::Stdio::piped())\n            .spawn() {{\n            Ok(c) => c,\n            Err(e) => {{ eprintln!(\"[orchestrate] failed to spawn secret serverlet '{n}': {{}}\", e); return; }}\n        }};\n        let mut __cin = __child.stdin.take().expect(\"child stdin\");\n        let mut __cout = __child.stdout.take().expect(\"child stdout\");\n        // Handshake: the child sends HELLO with its protocol version and handler signatures.\n        let __hello = match __secret_read_frame(&mut __cout).await {{\n            Ok(Some(f)) if f.kind == ORCH_KIND_HELLO => f,\n            _ => {{ eprintln!(\"[orchestrate] secret serverlet '{n}' did not complete the handshake\"); return; }}\n        }};\n        let mut __pos = 0usize;\n        let __version = i64::wire_decode(&__hello.payload, &mut __pos).unwrap_or(0);\n        let __got: Vec<String> = OrchWire::wire_decode(&__hello.payload, &mut __pos).unwrap_or_default();\n        let __expected: Vec<String> = vec![{sigs}];\n        if __version != ORCH_WIRE_VERSION || __got != __expected {{\n            eprintln!(\"[orchestrate] secret serverlet '{n}' interface mismatch: expected protocol {{}} {{:?}}, got protocol {{}} {{:?}}\", ORCH_WIRE_VERSION, __expected, __version, __got);\n            let _ = __child.kill().await;\n            return;\n        }}\n        if __secret_write_frame(&mut __cin, ORCH_KIND_READY, 0, &[]).await.is_err() {{ return; }}\n        let mut __next_call: u32 = 0;\n        while let Some(msg) = rx.recv().await {{\n            match msg {{\n{arms}\n            }}\n        }}\n        let _ = __secret_write_frame(&mut __cin, ORCH_KIND_BYE, 0, &[]).await;\n        drop(__cin);\n        let _ = __child.wait().await;\n    }});\n    {n}Client {{ tx }}\n}}",
            n = name, arms = arms.join("\n"), sigs = Self::handler_signatures(handlers)
        )
    }

    /// Handler signatures checked during the protocol handshake, as Rust string literals,
    /// e.g. `"shift(Point,int)->Point".to_string()`.
    fn handler_signatures(handlers: &[Handler]) -> String {
        handlers.iter()
            .map(|h| {
                let params = h.params.iter().map(|p| p.ty.display_name()).collect::<Vec<_>>().join(",");
                format!("{:?}.to_string()", format!("{}({})->{}", h.name, params, h.return_type.display_name()))
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn compile_secret_program(&mut self, name: &str, state: &[Stmt], handlers: &[Handler]) -> String {
        if let Some(reason) = wire_unsupported_reason(handlers, &self.struct_defs) {
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
            for p in &h.params {
                let ty = self.compile_type(&p.ty);
                arg_lets.push(format!(
                    "                        let {}: {} = OrchWire::wire_decode(&__frame.payload, &mut __pos).ok_or_else(|| \"invalid arguments\".to_string())?;",
                    p.name, ty
                ));
            }
            let body = self.compile_expr(&h.body);
            let finish = if h.return_type == Type::Void {
                "                        __handler();\n                        Ok(Vec::new())".to_string()
            } else {
                "                        let __r = __handler();\n                        Ok(__wire_to_bytes(&__r))".to_string()
            };
            arms.push(format!(
"                    {k}i64 => {{\n{args}\n                        if __pos != __frame.payload.len() {{ return Err(\"trailing arguments\".to_string()); }}\n                        #[allow(unused_mut)]\n                        let mut __handler = || {{ {body} }};\n{finish}\n                    }}",
                k = k, args = arg_lets.join("\n"), body = body, finish = finish
            ));
        }

        // The child is its own program, so it needs the struct definitions (and wire impls)
        // its handlers can use.
        let structs = self.struct_defs.iter()
            .filter(|(sname, _)| wire_supported(&Type::Named(sname.clone()), &self.struct_defs, 0))
            .map(|(sname, fields)| {
                let fields_str = fields.iter()
                    .map(|(fname, fty)| format!("    pub {}: {},", fname, self.compile_type(fty)))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("#[derive(Clone, Debug, Default)]\npub struct {} {{\n{}\n}}\n", sname, fields_str)
            })
            .collect::<String>() + &wire_struct_impls(&self.struct_defs);

        format!(
"// Generated by Orchestrate Compiler — secret serverlet '{name}'\n#![allow(unused_variables)]\n#![allow(dead_code)]\n#![allow(unused_imports)]\n#![allow(unused_parens)]\n#![allow(unused_mut)]\n\n{preamble}\n{frames}\n{wire}\n{structs}\nfn main() {{\n    let __stdin = std::io::stdin();\n    let mut __reader = std::io::BufReader::new(__stdin.lock());\n    let __stdout = std::io::stdout();\n    let mut __writer = std::io::BufWriter::new(__stdout.lock());\n\n{state}\n\n    let mut __hello = Vec::new();\n    ORCH_WIRE_VERSION.wire_encode(&mut __hello);\n    let __signatures: Vec<String> = vec![{sigs}];\n    __signatures.wire_encode(&mut __hello);\n    if __frame_write(&mut __writer, ORCH_KIND_HELLO, 0, &__hello).is_err() {{ return; }}\n    match __frame_read(&mut __reader) {{\n        Ok(Some(f)) if f.kind == ORCH_KIND_READY && f.call_id == 0 && f.payload.is_empty() => {{}},\n        _ => return,\n    }}\n    while let Ok(Some(__frame)) = __frame_read(&mut __reader) {{\n        if __frame.kind == ORCH_KIND_BYE {{ break; }}\n        let __result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Vec<u8>, String> {{\n            if __frame.kind != ORCH_KIND_CALL {{ return Err(\"expected CALL\".to_string()); }}\n            let mut __pos = 0usize;\n            let __idx = i64::wire_decode(&__frame.payload, &mut __pos).ok_or_else(|| \"missing handler id\".to_string())?;\n            match __idx {{\n{arms}\n                _ => Err(\"unknown handler id\".to_string()),\n            }}\n        }}));\n        let (__kind, __payload) = match __result {{\n            Ok(Ok(value)) => (ORCH_KIND_REPLY, value),\n            Ok(Err(message)) => (ORCH_KIND_ERROR, __wire_to_bytes(&message)),\n            Err(e) => (ORCH_KIND_ERROR, __wire_to_bytes(&__panic_message(&e))),\n        }};\n        if __frame_write(&mut __writer, __kind, __frame.call_id, &__payload).is_err() {{ break; }}\n    }}\n}}\n",
            name = name,
            preamble = runtime_preamble(true, true),
            frames = SECRET_CHILD_FRAMES,
            wire = super::core::WIRE_CODEC,
            structs = structs,
            sigs = Self::handler_signatures(handlers),
            state = state_vars.join("\n"),
            arms = arms.join("\n")
        )
    }
}

impl Codegen {
    /// The WASM guest crate `lib.rs` for a sandboxed serverlet: one exported
    /// `extern "C"` function per handler. STEP 2 SCAFFOLD — state is re-initialized
    /// per call; cross-call persistence and host integration land in step 3. The
    /// orchestrator still runs the real serverlet in-process for now.
    fn compile_sandbox_guest(&mut self, name: &str, state: &[Stmt], handlers: &[Handler]) -> String {
        if let Some(reason) = secret_unsupported_reason(handlers) {
            return format!("compile_error!(\"sandbox serverlet '{}': {}\");\n", name, reason);
        }

        // State bindings, re-created per call for now (step 3 makes them persist).
        let mut state_lets = Vec::new();
        for s in state {
            if let StmtNode::Let { name: vname, ty, value } = &s.node {
                let val_str = self.compile_expr(value);
                if let Some(t) = ty {
                    state_lets.push(format!("    let mut {}: {} = {};", vname, self.compile_type(t), val_str));
                } else {
                    state_lets.push(format!("    let mut {} = {};", vname, val_str));
                }
            }
        }
        let state_block = state_lets.join("\n");

        let mut exports = Vec::new();
        for h in handlers {
            let params = h.params.iter()
                .map(|p| format!("{}: {}", p.name, self.compile_type(&p.ty)))
                .collect::<Vec<_>>()
                .join(", ");
            let ret = if h.return_type == Type::Void {
                String::new()
            } else {
                format!(" -> {}", self.compile_type(&h.return_type))
            };
            let body = self.compile_expr(&h.body);
            exports.push(format!(
                "#[unsafe(no_mangle)]\npub extern \"C\" fn {hname}({params}){ret} {{\n{state}\n    {{ {body} }}\n}}",
                hname = h.name, params = params, ret = ret, state = state_block, body = body
            ));
        }

        format!(
"// Generated by Orchestrate Compiler — sandbox guest for serverlet '{name}'\n#![allow(unused_variables)]\n#![allow(dead_code)]\n#![allow(unused_imports)]\n#![allow(unused_parens)]\n#![allow(unused_mut)]\n#![allow(improper_ctypes_definitions)]\n\n{preamble}\n{exports}\n",
            name = name,
            preamble = runtime_preamble(true, false),
            exports = exports.join("\n\n")
        )
    }
}

type StructDefs = [(String, Vec<(String, Type)>)];

/// Returns Some(reason) if a handler uses a type the serverlet wire codec can't carry:
/// int, float, bool, string, arrays of carryable types, and structs declared in the same
/// file whose fields are carryable. Returns may also be void.
pub(crate) fn wire_unsupported_reason(handlers: &[Handler], structs: &StructDefs) -> Option<String> {
    for h in handlers {
        for p in &h.params {
            if !wire_supported(&p.ty, structs, 0) {
                return Some(format!(
                    "handler '{}' parameter '{}' uses an unsupported type; secret serverlets support int, float, bool, string, arrays, and structs declared in the same file",
                    h.name, p.name
                ));
            }
        }
        if h.return_type != Type::Void && !wire_supported(&h.return_type, structs, 0) {
            return Some(format!(
                "handler '{}' uses an unsupported return type; secret serverlets support int, float, bool, string, arrays, structs declared in the same file, and void",
                h.name
            ));
        }
    }
    None
}

fn wire_supported(ty: &Type, structs: &StructDefs, depth: usize) -> bool {
    if depth > 32 {
        return false; // recursive struct definitions can't be encoded
    }
    match ty {
        Type::Int | Type::Float | Type::Str | Type::Bool => true,
        Type::Array(inner, _) => wire_supported(inner, structs, depth + 1),
        Type::Named(name) => structs.iter()
            .find(|(n, _)| n == name)
            .map_or(false, |(_, fields)| fields.iter().all(|(_, fty)| wire_supported(fty, structs, depth + 1))),
        _ => false,
    }
}

/// `OrchWire` impls for every struct in the file that can cross a serverlet wire.
pub(crate) fn wire_struct_impls(structs: &StructDefs) -> String {
    structs.iter()
        .filter(|(name, _)| wire_supported(&Type::Named(name.clone()), structs, 0))
        .map(|(name, fields)| {
            let enc = fields.iter().map(|(f, _)| format!("self.{}.wire_encode(out);", f)).collect::<Vec<_>>().join(" ");
            let dec = fields.iter().map(|(f, _)| format!("{}: OrchWire::wire_decode(buf, pos)?", f)).collect::<Vec<_>>().join(", ");
            format!(
                "impl OrchWire for {name} {{\n    fn wire_encode(&self, out: &mut Vec<u8>) {{ {enc} }}\n    fn wire_decode(buf: &[u8], pos: &mut usize) -> Option<Self> {{ Some({name} {{ {dec} }}) }}\n}}\n",
                name = name, enc = enc, dec = dec
            )
        })
        .collect()
}

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
