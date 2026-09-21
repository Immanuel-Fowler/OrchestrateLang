use crate::ast::{Expr, ExprNode, StmtNode, Stmt, Type, Handler};
use super::core::{Codegen, StateRewrite, pascal_case, rust_ident, runtime_preamble, SECRET_CHILD_FRAMES};

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
            StmtNode::Let { name, ty, value, shared } => {
                if *shared {
                    // The binding lives in the shared struct, initialised there; nothing
                    // is declared at the point it was written.
                    return String::new();
                }
                let before = self.shared_touches.get();
                let awaits_before = self.awaits;
                let outer = std::mem::replace(&mut self.guard_held, true);
                let val_str = self.compile_expr(value);
                self.guard_held = outer;
                if self.shared_touches.get() != before && self.awaits != awaits_before {
                    self.errors.push(
                        "a statement cannot both wait and touch shared state: the lock would be held across the wait, blocking every other reader. Split it into two statements.".to_string()
                    );
                }
                self.define_local(name);
                // A `let` that reads shared state takes the lock around its initialiser —
                // only the initialiser, as a block expression, so the binding itself stays
                // in the enclosing scope for the statements after it.
                let val_str = if self.shared_touches.get() != before && !outer {
                    self.wrap_shared(val_str)
                } else {
                    val_str
                };
                if let Some(t) = ty {
                    // Closure types can't be annotated directly — let Rust infer
                    if matches!(t, Type::Fn(_, _)) {
                        format!("let mut {} = {};", rust_ident(name), val_str)
                    } else {
                        format!("let mut {}: {} = {};", rust_ident(name), self.compile_type(t), val_str)
                    }
                } else {
                    format!("let mut {} = {};", rust_ident(name), val_str)
                }
            }
            StmtNode::Break => "break".to_string(),
            StmtNode::Continue => "continue".to_string(),
            StmtNode::Expr(expr) => {
                let before = self.shared_touches.get();
                let awaits_before = self.awaits;
                let outer = std::mem::replace(&mut self.guard_held, true);
                let compiled = self.compile_expr(expr);
                self.guard_held = outer;
                if self.shared_touches.get() != before && self.awaits != awaits_before {
                    self.errors.push(
                        "a statement cannot both wait and touch shared state: the lock would be held across the wait, blocking every other reader. Split it into two statements.".to_string()
                    );
                }
                if self.shared_touches.get() != before && !outer {
                    return self.wrap_shared(format!("{};", compiled));
                }
                compiled
            }
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
                    let before = self.shared_touches.get();
                    let awaits_before = self.awaits;
                    let outer = std::mem::replace(&mut self.guard_held, true);
                    let compiled = format!("{}{}", self.compile_expr(expr), self.state_copy_suffix(expr));
                    self.guard_held = outer;
                    if self.shared_touches.get() != before && self.awaits != awaits_before {
                        self.errors.push(
                            "a statement cannot both wait and touch shared state: the lock would be held across the wait, blocking every other reader. Split it into two statements.".to_string()
                        );
                    }
                    if self.shared_touches.get() != before && !outer {
                        // The value is read out before the guard drops, so the lock is not
                        // held across the return.
                        return self.wrap_shared(format!("let __returned = {compiled}; return __returned;"));
                    }
                    format!("return {}", compiled)
                } else {
                    "return".to_string()
                }
            }
            StmtNode::FnDecl { name, params, return_type, body, type_params } => {
                let generics = type_params_str(type_params);
                let params_str = params.iter()
                    .map(|p| format!("{}: {}", rust_ident(&p.name), self.compile_type(&p.ty)))
                    .collect::<Vec<String>>()
                    .join(", ");
                let ret_str = if *return_type == Type::Void {
                    "".to_string()
                } else {
                    format!(" -> {}", self.compile_type(return_type))
                };
                let outer = std::mem::replace(&mut self.sync_fn, Some(name.clone()));
                let body_str = if let ExprNode::Block(_) = &body.node {
                    let force_semi = *return_type == Type::Void;
                    let inner = self.compile_block_inner(body, force_semi);
                    format!("{{\n    {}\n}}", inner.replace("\n", "\n    "))
                } else {
                    self.compile_expr(body)
                };
                self.sync_fn = outer;
                let vis = if self.is_main { "" } else { "pub " };
                format!("{}fn {}{}({}){} {}", vis, rust_ident(name), generics, params_str, ret_str, body_str)
            }
            StmtNode::TaskDecl { name, params, return_type, body, type_params } => {
                let generics = type_params_str(type_params);
                let params_str = params.iter()
                    .map(|p| format!("{}: {}", rust_ident(&p.name), self.compile_type(&p.ty)))
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
                format!("{}async fn {}{}({}){} {}", vis, rust_ident(name), generics, params_str, ret_str, body_str)
            }
            StmtNode::ProcessDecl { name, params, return_type, body, type_params } => {
                let generics = type_params_str(type_params);
                let params_str = params.iter()
                    .map(|p| format!("{}: {}", rust_ident(&p.name), self.compile_type(&p.ty)))
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
                format!("{}async fn {}{}({}){} {}", vis, rust_ident(name), generics, params_str, ret_str, body_str)
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
                        .map(|p| format!("{}: {}", rust_ident(&p.name), self.compile_type(&p.ty)))
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
                        .map(|p| format!("{}: {}", rust_ident(&p.name), self.compile_type(&p.ty)))
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
                    format!("async fn {}({}){} {}", rust_ident(name), params_str, ret_str, body_str)
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
                    if self.context_bound {
                        return format!("{{ let value = std::sync::Arc::new({payload}); let handlers = __context.event_{event_name}.lock().unwrap().clone(); for handler in handlers {{ __context.queue_event(handler(value.clone())); }} }}");
                    }
                    return format!("let context = crate::__orch_context(); let value = std::sync::Arc::new({payload}); let handlers = {func_name}().lock().unwrap().clone(); for handler in handlers {{ context.queue_event(handler(value.clone())); }}");
                }
                format!(
                    "let payload_eval = std::sync::Arc::new({});\nif let Ok(handlers) = {}().lock() {{\n    for tx in handlers.iter() {{\n        if tx.try_send(std::sync::Arc::clone(&payload_eval)).is_err() {{\n            eprintln!(\"[orchestrate] warning: dropped event '{}' — subscriber channel full\");\n        }}\n    }}\n}}",
                    payload, func_name, event_name
                )
            }
            StmtNode::Parallel(stmts) => {
                self.require_async("uses a parallel block, which waits for its branches");
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
                        rust_ident(var), iter_str, rust_ident(idx), inner
                    )
                } else {
                    let body_str = self.compile_expr(body);
                    format!("for {} in {} {}", rust_ident(var), iter_str, body_str)
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
                format!("mod {};", rust_ident(local_name))
            }
            StmtNode::Load { .. } | StmtNode::LoadForeign { .. } => "".to_string(),
            StmtNode::Serverlet { name, state, handlers, secret, crash_handler, sandbox, landline, grants } => {
                // Sandboxed serverlet: stash a WASM guest crate (compiled by the
                // driver). The orchestrator still runs the serverlet in-process for
                // now — host integration is step 3 — and the driver warns about it.
                if sandbox.is_some() {
                    // A grant is a hole punched in the wall on purpose: each one becomes
                    // exactly one import the guest can reach, defined in the linker by
                    // the host loop below and called through a stub in the guest.
                    let guest = self.compile_sandbox_guest(name, state, handlers, grants);
                    self.sandbox_programs.push((name.clone(), guest));
                }
                let mut enum_variants = Vec::new();
                for h in handlers {
                    let variant_name = pascal_case(&h.name);
                    let mut fields = h.params.iter()
                        .map(|p| format!("{}: {}", rust_ident(&p.name), self.compile_type(&p.ty)))
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
                        .map(|p| format!("{}: {}", rust_ident(&p.name), self.compile_type(&p.ty)))
                        .collect::<Vec<String>>()
                        .join(", ");
                    let self_params = if method_params.is_empty() { "&self" } else { "&self, " };
                    let ret_ty = self.compile_type(&h.return_type);
                    let variant_name = pascal_case(&h.name);
                    let mut send_fields = h.params.iter().map(|p| rust_ident(&p.name)).collect::<Vec<String>>();
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
                                format!("self.latest.{}.lock().unwrap().clone().unwrap_or_default()", rust_ident(&h.name))
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
                        rust_ident(&h.name), self_params, method_params, ret_ty, body
                    ));
                }

                // Landline clients share each handler's most recent result with their actor,
                // for the `late: "latest"` budget policy.
                let (latest_struct, latest_field) = if landline.is_some() {
                    let fields = handlers.iter()
                        .filter(|h| h.return_type != Type::Void)
                        .map(|h| format!("    {}: std::sync::Mutex<Option<{}>>,", rust_ident(&h.name), self.compile_type(&h.return_type)))
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
                self.handler_state = state_names(state);
                for h in handlers {
                    let variant_name = pascal_case(&h.name);
                    let mut bindings = h.params.iter().map(|p| rust_ident(&p.name)).collect::<Vec<String>>();
                    bindings.push("reply_to".to_string());
                    let bindings_str = bindings.join(", ");
                    let body_str = self.compile_expr(&h.body);

                    let handler_name_str = &h.name;

                    let crash_recovery = if let Some((err_name, crash_body)) = crash_handler {
                        let crash_body_str = self.compile_expr(crash_body);
                        format!(
                            "Err(__panic_err) => {{\n                    let {err_name} = format!(\"{{:?}}\", __panic_err);\n                    eprintln!(\"[orchestrate] serverlet '{name}' handler '{handler_name_str}' panicked: {{}}\", {err_name});\n                    {crash_body_str};\n                    let _ = reply_to.send(Default::default());\n                }}",
                            err_name = rust_ident(err_name),
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

                self.handler_state.clear();

                let mut state_vars = Vec::new();
                for s in state {
                    if let StmtNode::Let { name: vname, ty, value, .. } = &s.node {
                        let val_str = self.compile_expr(value);
                        if let Some(t) = ty {
                            state_vars.push(format!("            let mut {}: {} = {};", rust_ident(vname), self.compile_type(t), val_str));
                        } else {
                            state_vars.push(format!("            let mut {} = {};", rust_ident(vname), val_str));
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

                if let Some(config) = sandbox {
                    // The handlers run in the guest, so the host loop marshals rather
                    // than executing anything the serverlet declared.
                    let start_fn = self.compile_sandbox_host(name, state, handlers, config, crash_handler, grants);
                    let start_fn = if self.library {
                        start_fn.replace("tokio::spawn(", "__ORCH_LINE_SPAWN(")
                            .replace("rx.recv().await", "crate::__orch_recv(&mut rx).await")
                    } else {
                        start_fn
                    };
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
                    .map(|(fname, fty)| format!("    pub {}: {},", rust_ident(fname), self.compile_type(fty)))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("#[derive(Clone, Debug, Default)]\n#[repr(C)]\npub struct {} {{\n{}\n}}", name, fields_str)
            }
            StmtNode::EnumDef { name, variants } => {
                let variants_str = variants.iter().map(|v| {
                    match &v.payload {
                        Some(ty) => format!("    {}({}),", rust_ident(&v.name), self.compile_type(ty)),
                        None => format!("    {},", rust_ident(&v.name)),
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
            let param_names: Vec<String> = h.params.iter().map(|p| rust_ident(&p.name)).collect();
            let binding = if param_names.is_empty() {
                "reply_to".to_string()
            } else {
                format!("{}, reply_to", param_names.join(", "))
            };

            // CALL payload: the handler id, then each argument in the wire encoding.
            let encode_args = h.params.iter()
                .map(|p| format!("                    {}.wire_encode(&mut __payload);\n", rust_ident(&p.name)))
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
            if let StmtNode::Let { name: vname, ty, value, .. } = &s.node {
                let val_str = self.compile_expr(value);
                if let Some(t) = ty {
                    state_vars.push(format!("    let mut {}: {} = {};", rust_ident(vname), self.compile_type(t), val_str));
                } else {
                    state_vars.push(format!("    let mut {} = {};", rust_ident(vname), val_str));
                }
            }
        }

        let mut arms = Vec::new();
        self.handler_state = state_names(state);
        for (k, h) in handlers.iter().enumerate() {
            let mut arg_lets = Vec::new();
            for p in &h.params {
                let ty = self.compile_type(&p.ty);
                arg_lets.push(format!(
                    "                        let {}: {} = OrchWire::wire_decode(&__frame.payload, &mut __pos).ok_or_else(|| \"invalid arguments\".to_string())?;",
                    rust_ident(&p.name), ty
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

        self.handler_state.clear();

        // The child is its own program, so it needs the struct definitions (and wire impls)
        // its handlers can use.
        let structs = self.struct_defs.iter()
            .filter(|(sname, _)| wire_supported(&Type::Named(sname.clone()), &self.struct_defs, 0))
            .map(|(sname, fields)| {
                let fields_str = fields.iter()
                    .map(|(fname, fty)| format!("    pub {}: {},", rust_ident(fname), self.compile_type(fty)))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("#[derive(Clone, Debug, Default)]\n#[repr(C)]\npub struct {} {{\n{}\n}}\n", sname, fields_str)
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
    /// The actor loop for a sandboxed serverlet. It looks like any other serverlet's from
    /// the outside — same `XMsg`, same `XClient` — but each call crosses into a wasmtime
    /// guest instead of running inline, under the declared memory cap and timeout. A call
    /// that traps is reported and answered with the return type's default, so one hostile
    /// or broken call cannot take the program with it.
    ///
    /// Values cross under the C ABI's rules (0.14.0): a number goes as itself; a string or
    /// an array is a pointer and a length or count into the guest's memory; a struct is
    /// its `#[repr(C)]` bytes behind a pointer. What the host writes for a call it frees
    /// after the call, and what the guest returns the host copies and frees.
    fn compile_sandbox_host(
        &mut self,
        name: &str,
        state: &[Stmt],
        handlers: &[Handler],
        config: &crate::ast::SandboxConfig,
        crash_handler: &Option<(String, Box<Expr>)>,
        grants: &[String],
    ) -> String {
        // `on_crash` runs on the host, after the trap is reported and before the guest is
        // replaced, with the trap's message bound. The guest's state is not reachable
        // from here: it is inside the instance being thrown away.
        let crash = match crash_handler {
            Some((error_name, body)) => {
                let mut free = std::collections::HashSet::new();
                self.get_free_vars_expr(body, &mut std::collections::HashSet::new(), &mut free);
                let mut reached: Vec<&String> = free.iter().filter(|v| state_names(state).contains(*v)).collect();
                reached.sort();
                if let Some(reached) = reached.first() {
                    self.errors.push(format!(
                        "sandboxed serverlet '{name}': on_crash uses '{reached}', which is the guest's state. on_crash runs on the host after the guest trapped, so the state is gone; report the error and let the fresh guest start over"
                    ));
                }
                format!(" {{ let {} = __error.clone(); {}; }}", rust_ident(error_name), self.compile_expr(body))
            }
            None => String::new(),
        };
        let bail = format!("{{ eprintln!(\"[orchestrate] {{}}\", __error); __guest.reset();{crash} let _ = reply_to.send(Default::default()); continue; }}");
        let bail = bail.as_str();
        let mut arms = Vec::new();
        for h in handlers {
            let variant = pascal_case(&h.name);
            let bindings = h
                .params
                .iter()
                .map(|p| rust_ident(&p.name))
                .chain(std::iter::once("reply_to".to_string()))
                .collect::<Vec<_>>()
                .join(", ");

            // Strings, arrays, and structs are copied into guest memory first and freed
            // once the call is back; numbers go as they are.
            let mut setup = String::new();
            let mut cleanup = String::new();
            let mut arguments = Vec::new();
            let mut wasm_params = Vec::new();
            for p in &h.params {
                match &p.ty {
                    Type::Str => {
                        setup.push_str(&format!(
                            "                    let ({0}_pointer, {0}_length) = match __guest.write_string(&{1}) {{ Ok(__written) => __written, Err(__error) => {bail} }};\n",
                            p.name, rust_ident(&p.name)
                        ));
                        cleanup.push_str(&format!("                    __guest.free({0}_pointer, {0}_length);\n", p.name));
                        arguments.push(format!("{}_pointer", p.name));
                        arguments.push(format!("{}_length", p.name));
                        wasm_params.push("i32".to_string());
                        wasm_params.push("i32".to_string());
                    }
                    Type::Array(_, _) => {
                        setup.push_str(&format!(
                            "                    let ({0}_pointer, {0}_count, {0}_length) = match __guest.write_array(&{1}) {{ Ok(__written) => __written, Err(__error) => {bail} }};\n",
                            p.name, rust_ident(&p.name)
                        ));
                        cleanup.push_str(&format!("                    __guest.free({0}_pointer, {0}_length);\n", p.name));
                        arguments.push(format!("{}_pointer", p.name));
                        arguments.push(format!("{}_count", p.name));
                        wasm_params.push("i32".to_string());
                        wasm_params.push("i32".to_string());
                    }
                    Type::Named(sname) => {
                        setup.push_str(&format!(
                            "                    let ({0}_pointer, {0}_length) = match __guest.write_struct(std::mem::size_of::<{1}>(), |__out| __orch_sandbox_write_{1}(&{2}, __out)) {{ Ok(__written) => __written, Err(__error) => {bail} }};\n",
                            p.name, sname, rust_ident(&p.name)
                        ));
                        cleanup.push_str(&format!("                    __guest.free({0}_pointer, {0}_length);\n", p.name));
                        arguments.push(format!("{}_pointer", p.name));
                        wasm_params.push("i32".to_string());
                    }
                    Type::Bool => {
                        arguments.push(format!("{} as i32", rust_ident(&p.name)));
                        wasm_params.push("i32".to_string());
                    }
                    Type::Float => {
                        arguments.push(rust_ident(&p.name));
                        wasm_params.push("f64".to_string());
                    }
                    _ => {
                        arguments.push(rust_ident(&p.name));
                        wasm_params.push("i64".to_string());
                    }
                }
            }
            let results = match h.return_type {
                Type::Void => "()",
                Type::Float => "f64",
                Type::Bool => "i32",
                _ => "i64",
            };
            // A one-element tuple keeps its trailing comma, or it stops being a tuple.
            let tuple_type = match wasm_params.len() {
                1 => format!("({},)", wasm_params[0]),
                _ => format!("({})", wasm_params.join(", ")),
            };
            let tuple_args = match arguments.len() {
                1 => format!("{},", arguments[0]),
                _ => arguments.join(", "),
            };
            let convert = match &h.return_type {
                Type::Bool => "                    let __value = __value != 0;\n".to_string(),
                Type::Str => format!("                    let __value = match __guest.read_string(__value) {{ Ok(__text) => __text, Err(__error) => {bail} }};\n"),
                Type::Array(inner, _) => format!(
                    "                    let __value = match __guest.read_array::<{}>(__value) {{ Ok(__items) => __items, Err(__error) => {bail} }};\n",
                    self.compile_type(inner)
                ),
                Type::Named(sname) => format!(
                    "                    let __value = match __guest.read_struct(__value, std::mem::size_of::<{0}>(), __orch_sandbox_read_{0}) {{ Ok(__struct) => __struct, Err(__error) => {bail} }};\n",
                    sname
                ),
                _ => String::new(),
            };

            arms.push(format!(
                "                {name}Msg::{variant} {{ {bindings} }} => {{\n                    \
                     __guest.begin();\n{setup}                    \
                     let __outcome = match __guest.instance.get_typed_func::<{tuple_type}, {results}>(&mut __guest.store, {handler:?}) {{\n                        \
                         Ok(__function) => {{\n                            \
                             let __called = __function.call(&mut __guest.store, ({tuple_args}));\n                            \
                             __called.map_err(|__error| __guest.failed({handler:?}, &__error))\n                        \
                         }}\n                        \
                         Err(__error) => Err(format!(\"sandboxed serverlet '{name}' exports no '{handler}': {{}}\", __error)),\n                    \
                     }};\n                    \
                     let __value = match __outcome {{\n                        \
                         Ok(__value) => __value,\n                        \
                         Err(__error) => {bail}\n                    \
                     }};\n{cleanup}{convert}                    \
                     let _ = reply_to.send(__value);\n                \
                 }}",
                name = name,
                variant = variant,
                bindings = bindings,
                setup = setup,
                tuple_type = tuple_type,
                results = results,
                handler = h.name,
                tuple_args = tuple_args,
                cleanup = cleanup,
                convert = convert,
                bail = bail,
            ));
        }

        // The struct codecs the arms above call, once per file.
        let codecs = if self.sandbox_codecs_emitted {
            String::new()
        } else {
            self.sandbox_codecs_emitted = true;
            sandbox_struct_codecs(&self.struct_defs)
        };

        // Each grant is one linker definition: the host method behind one import, and
        // nothing for anything ungranted, which therefore traps if the guest names it.
        let (grants_fn, grants_arg) = if grants.is_empty() {
            (String::new(), "None".to_string())
        } else {
            let mut definitions = Vec::new();
            for grant in grants {
                let Some((group, handler)) = self
                    .host_functions
                    .iter()
                    .find(|(group, h)| format!("{}.{}", group, h.name) == *grant)
                    .cloned()
                else {
                    self.errors.push(format!("Unknown host grant '{}'", grant));
                    continue;
                };
                let decode = handler.params.iter().map(|p| format!(
                    "            let {}: {} = OrchWire::wire_decode(__payload, &mut __pos).ok_or(\"invalid host arguments\")?;\n",
                    rust_ident(&p.name), self.host_type(&p.ty)
                )).collect::<String>();
                let args = handler.params.iter().map(|p| rust_ident(&p.name)).collect::<Vec<_>>().join(", ");
                let encode = if handler.return_type == Type::Void { "Vec::new()" } else { "__wire_to_bytes(&__value)" };
                definitions.push(format!(
                    "    linker.func_wrap(\"orch_host\", {import:?}, |mut __caller: wasmtime::Caller<'_, crate::__OrchGuestLimits>, __pointer: i32, __length: i32| -> i64 {{\n        \
                         crate::__orch_grant_call(&mut __caller, __pointer, __length, |__payload| -> Result<Vec<u8>, String> {{\n            \
                             let mut __pos = 0;\n{decode}            \
                             if __pos != __payload.len() {{ return Err(\"trailing host arguments\".into()); }}\n            \
                             let __value = crate::__orch_context().host.{method}({args})?;\n            \
                             Ok({encode})\n        \
                         }})\n    \
                     }})?;",
                    import = format!("{}_{}", group, handler.name),
                    decode = decode,
                    method = Self::host_method(&group, &handler.name),
                    args = args,
                    encode = encode,
                ));
            }
            (
                format!(
                    "/// The host functions serverlet '{name}' was granted, one import each.\n\
                     #[allow(non_snake_case)]\nfn __orch_grants_{name}(linker: &mut wasmtime::Linker<crate::__OrchGuestLimits>) -> Result<(), wasmtime::Error> {{\n{}\n    Ok(())\n}}\n",
                    definitions.join("\n")
                ),
                format!("Some(__orch_grants_{name})"),
            )
        };

        format!(
            "{codecs}{grants_fn}#[allow(non_snake_case)]\npub fn start_{name}() -> {name}Client {{\n    \
                 let (tx, mut rx) = tokio::sync::mpsc::channel::<{name}Msg>(100);\n    \
                 tokio::spawn(async move {{\n        \
                     let mut __guest = match crate::__OrchGuest::new({name:?}, include_bytes!(\"sandbox_{name}.wasm\"), Some({memory}), Some({timeout}), {grants_arg}) {{\n            \
                         Ok(__guest) => __guest,\n            \
                         Err(__error) => {{ eprintln!(\"[orchestrate] {{}}\", __error); return; }}\n        \
                     }};\n        \
                     while let Some(msg) = rx.recv().await {{\n            \
                         match msg {{\n{arms}\n            }}\n        }}\n    \
                 }});\n    \
                 {name}Client {{ tx }}\n}}",
            codecs = codecs,
            grants_fn = grants_fn,
            grants_arg = grants_arg,
            name = name,
            memory = config.memory_bytes().unwrap_or(64 * 1024 * 1024),
            timeout = config.timeout_ms().unwrap_or(5_000),
            arms = arms.join("\n"),
        )
    }

    /// The WASM guest crate `lib.rs` for a sandboxed serverlet: the serverlet's state as a
    /// struct that lives inside the guest between calls, the file's struct definitions,
    /// and one exported `extern "C"` function per handler. Numbers cross as themselves; a
    /// string or an array crosses as a pointer and a length or count into the guest's own
    /// memory, and a struct as its `#[repr(C)]` bytes, all allocated by the allocator
    /// both sides share.
    fn compile_sandbox_guest(&mut self, name: &str, state: &[Stmt], handlers: &[Handler], grants: &[String]) -> String {
        // The entry file's serverlets were checked by the typechecker; a module's were
        // not, so the gate is here too, reported by the driver before anything builds.
        if let Some(reason) = sandbox_unsupported_reason(name, handlers, &self.struct_defs) {
            self.errors.push(reason);
            return String::new();
        }

        // Each grant is one import from the host and one stub that carries a call
        // across: arguments encoded with the wire codec into guest memory, the reply
        // decoded from it. The guest has these imports and no others.
        let mut imports = Vec::new();
        let mut stubs = Vec::new();
        for grant in grants {
            let Some((group, handler)) = self
                .host_functions
                .iter()
                .find(|(group, h)| format!("{}.{}", group, h.name) == *grant)
                .cloned()
            else {
                self.errors.push(format!("Unknown host grant '{}'", grant));
                continue;
            };
            let import = format!("{}_{}", group, handler.name);
            imports.push(format!(
                "    #[link_name = {import:?}]\n    fn __orch_import_{import}(pointer: i32, length: i32) -> i64;"
            ));
            let params = handler.params.iter()
                .map(|p| format!("{}: {}", rust_ident(&p.name), self.compile_type(&p.ty)))
                .collect::<Vec<_>>()
                .join(", ");
            let encodes = handler.params.iter()
                .map(|p| format!("    {}.wire_encode(&mut __payload);\n", p.name))
                .collect::<String>();
            let (returns, decode) = if handler.return_type == Type::Void {
                (String::new(), "()".to_string())
            } else {
                let ty = self.compile_type(&handler.return_type);
                (format!(" -> {ty}"), format!("<{ty} as OrchWire>::wire_decode(&__reply, &mut __pos).unwrap_or_default()"))
            };
            stubs.push(format!(
                "fn __orch_grant_{import}({params}){returns} {{\n    \
                     let mut __payload = Vec::new();\n{encodes}    \
                     let __reply = __orch_host_call(__orch_import_{import}, &__payload);\n    \
                     let mut __pos = 0;\n    \
                     match bool::wire_decode(&__reply, &mut __pos) {{\n        \
                         Some(true) => {decode},\n        \
                         Some(false) => {{\n            \
                             let __error = String::wire_decode(&__reply, &mut __pos).unwrap_or_default();\n            \
                             eprintln!(\"[orchestrate] host call {grant} failed: {{}}\", __error);\n            \
                             Default::default()\n        \
                         }}\n        \
                         None => {{ eprintln!(\"[orchestrate] host call {grant}: no reply from the host\"); Default::default() }}\n    \
                     }}\n}}",
                import = import, params = params, returns = returns, encodes = encodes, decode = decode, grant = grant
            ));
        }
        let grants_code = if imports.is_empty() {
            String::new()
        } else {
            format!(
                "#[link(wasm_import_module = \"orch_host\")]\nunsafe extern \"C\" {{\n{}\n}}\n\n{}\n\n{}\n",
                imports.join("\n"),
                stubs.join("\n\n"),
                super::core::WIRE_CODEC.to_string() + &wire_struct_impls(&self.struct_defs)
            )
        };
        self.sandbox_guest_grants = Some(grants.to_vec());

        // The state's types come from the typechecker, since a struct field cannot be
        // written as `let mut x = ...` and inferred.
        let declared = self.serverlet_state_types.get(name).cloned().unwrap_or_default();
        let mut fields = Vec::new();
        let mut initializers = Vec::new();
        let mut names = Vec::new();
        for s in state {
            let StmtNode::Let { name: field, ty, value, .. } = &s.node else { continue };
            let resolved = ty
                .clone()
                .or_else(|| declared.iter().find(|(n, _)| n == field).map(|(_, t)| t.clone()))
                .or_else(|| literal_type(value));
            let Some(resolved) = resolved else {
                self.errors.push(format!(
                    "sandboxed serverlet '{name}': state '{field}' has no type the compiler can name; add a type annotation"
                ));
                return String::new();
            };
            let value = self.compile_expr(value);
            initializers.push(format!("        let mut {}: {} = {};", rust_ident(field), self.compile_type(&resolved), value));
            if !names.contains(field) {
                fields.push(format!("    {}: {},", rust_ident(field), self.compile_type(&resolved)));
                names.push(field.clone());
            }
        }
        let rewrite_fields: std::collections::BTreeMap<String, bool> =
            names.iter().map(|field| (field.clone(), false)).collect();

        let mut exports = Vec::new();
        self.handler_state = state_names(state);
        for h in handlers {
            // Handler parameters shadow state of the same name, as they do in-process.
            let mut scope = std::collections::HashSet::new();
            for p in &h.params {
                scope.insert(p.name.clone());
            }
            self.state_rewrite = Some(StateRewrite {
                fields: rewrite_fields.clone(),
                scopes: vec![scope],
                receiver: "__state",
            });
            let awaits_before = self.awaits;
            let body = self.compile_expr(&h.body);
            self.state_rewrite = None;
            // A guest handler is an exported function that runs to completion inside
            // the guest: there is no runtime in there to wait on.
            if self.awaits != awaits_before {
                self.errors.push(format!(
                    "sandboxed serverlet '{name}': handler '{}' waits — it calls a serverlet, a task, or sleep — and a guest handler cannot wait; it runs to completion inside the guest, which has no runtime and reaches nothing but its grants",
                    h.name
                ));
            }

            let mut params = Vec::new();
            let mut unpack = String::new();
            for p in &h.params {
                match &p.ty {
                    Type::Str => {
                        params.push(format!("{}_pointer: i32", p.name));
                        params.push(format!("{}_length: i32", p.name));
                        unpack.push_str(&format!(
                            "    let {} = __orch_unpack({1}_pointer, {1}_length);\n",
                            rust_ident(&p.name), p.name
                        ));
                    }
                    Type::Array(inner, _) => {
                        params.push(format!("{}_pointer: i32", p.name));
                        params.push(format!("{}_count: i32", p.name));
                        unpack.push_str(&format!(
                            "    let {}: Vec<{2}> = __orch_unpack_array({1}_pointer, {1}_count);\n",
                            rust_ident(&p.name),
                            p.name,
                            self.compile_type(inner)
                        ));
                    }
                    Type::Named(sname) => {
                        params.push(format!("{}_pointer: i32", p.name));
                        unpack.push_str(&format!(
                            "    let {} = __orch_sandbox_read_{2}(__orch_bytes({1}_pointer, std::mem::size_of::<{2}>() as i32));\n",
                            rust_ident(&p.name), p.name, sname
                        ));
                    }
                    Type::Bool => {
                        params.push(format!("{}_flag: i32", p.name));
                        unpack.push_str(&format!("    let {} = {1}_flag != 0;\n", rust_ident(&p.name), p.name));
                    }
                    _ => params.push(format!("{}: {}", rust_ident(&p.name), self.compile_type(&p.ty))),
                }
            }
            let (returns, open, close) = match &h.return_type {
                Type::Void => (String::new(), String::new(), String::new()),
                Type::Str => (" -> i64".to_string(), "__orch_pack(".to_string(), ")".to_string()),
                Type::Array(_, _) => (" -> i64".to_string(), "__orch_pack_array(".to_string(), ")".to_string()),
                Type::Named(sname) => (
                    " -> i64".to_string(),
                    "{ let __value = ".to_string(),
                    format!("; __orch_pack_struct(std::mem::size_of::<{0}>(), |__out| __orch_sandbox_write_{0}(&__value, __out)) }}", sname),
                ),
                Type::Bool => (" -> i32".to_string(), "(".to_string(), ") as i32".to_string()),
                other => (format!(" -> {}", self.compile_type(other)), String::new(), String::new()),
            };
            exports.push(format!(
                "#[unsafe(export_name = {export:?})]\npub extern \"C\" fn {hname}({params}){returns} {{\n{unpack}    {open}__orch_state(|__state| {{ {body} }}){close}\n}}",
                export = h.name,
                hname = rust_ident(&h.name),
                params = params.join(", "),
                returns = returns,
                unpack = unpack,
                open = open,
                body = body,
                close = close,
            ));
        }

        self.handler_state.clear();
        self.sandbox_guest_grants = None;

        // Every struct in the file, so a handler body or the state can use any of them;
        // the codecs, for the ones that can cross.
        let structs = self.struct_defs.iter()
            .map(|(sname, sfields)| {
                let fields_str = sfields.iter()
                    .map(|(fname, fty)| format!("    pub {}: {},", rust_ident(fname), self.compile_type(fty)))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("#[derive(Clone, Debug, Default)]\n#[repr(C)]\npub struct {} {{\n{}\n}}\n", sname, fields_str)
            })
            .collect::<String>() + &sandbox_struct_codecs(&self.struct_defs);

        format!(
"// Generated by Orchestrate Compiler — sandbox guest for serverlet '{name}'\n\
#![allow(unused_variables)]\n#![allow(dead_code)]\n#![allow(unused_imports)]\n#![allow(unused_parens)]\n#![allow(unused_mut)]\n#![allow(improper_ctypes_definitions)]\n\n\
{preamble}\n\
{pod}\
{structs}\n\
/// The serverlet's state. It lives here, inside the guest, for as long as the instance\n\
/// does, so one call sees what the call before it left.\n\
struct __OrchState {{\n{fields}\n}}\n\
impl __OrchState {{\n    fn new() -> __OrchState {{\n{initializers}\n        __OrchState {{ {names} }}\n    }}\n}}\n\
thread_local! {{ static __ORCH_STATE: std::cell::RefCell<__OrchState> = std::cell::RefCell::new(__OrchState::new()); }}\n\
fn __orch_state<R>(body: impl FnOnce(&mut __OrchState) -> R) -> R {{\n    __ORCH_STATE.with(|cell| body(&mut cell.borrow_mut()))\n}}\n\n\
{marshal}\n{grants_code}{exports}\n",
            name = name,
            preamble = runtime_preamble(true, false),
            pod = super::core::WASM_POD,
            structs = structs,
            fields = fields.join("\n"),
            initializers = initializers.join("\n"),
            names = names.iter().map(|n| rust_ident(n)).collect::<Vec<_>>().join(", "),
            marshal = SANDBOX_GUEST_MARSHAL,
            grants_code = grants_code,
            exports = exports.join("\n\n")
        )
    }
}

/// The guest's side of the boundary: the allocator both sides share, and the packing of
/// a string, an array, or a struct for the host. Eight-byte aligned, so the guest can
/// read numbers in place.
const SANDBOX_GUEST_MARSHAL: &str = r#"/// The allocator the host shares, so a value is allocated and released on one side.
#[unsafe(no_mangle)]
pub extern "C" fn orch_alloc(len: i32) -> i32 {
    if len <= 0 { return 0; }
    let layout = std::alloc::Layout::from_size_align(len as usize, 8).expect("layout");
    unsafe { std::alloc::alloc(layout) as i32 }
}
#[unsafe(no_mangle)]
pub extern "C" fn orch_free(ptr: i32, len: i32) {
    if ptr == 0 || len <= 0 { return; }
    let layout = std::alloc::Layout::from_size_align(len as usize, 8).expect("layout");
    unsafe { std::alloc::dealloc(ptr as *mut u8, layout) }
}
/// Bytes the host wrote into guest memory for this call. The host owns that allocation
/// and frees it once the call returns, so the guest copies what it keeps.
fn __orch_bytes<'a>(ptr: i32, len: i32) -> &'a [u8] {
    if ptr == 0 || len <= 0 { return &[]; }
    unsafe { std::slice::from_raw_parts(ptr as *const u8, len as usize) }
}
fn __orch_unpack(ptr: i32, len: i32) -> String {
    String::from_utf8_lossy(__orch_bytes(ptr, len)).into_owned()
}
fn __orch_unpack_array<T: __OrchWasmPod>(ptr: i32, count: i32) -> Vec<T> {
    let bytes = __orch_bytes(ptr, count.saturating_mul(T::SIZE as i32));
    bytes.chunks_exact(T::SIZE).map(T::from_bytes).collect()
}
/// A value for the host, as (pointer << 32) | length or count. The host reads it and
/// hands the allocation back through `orch_free`.
fn __orch_pack_bytes(bytes: &[u8], low: i32) -> i64 {
    if bytes.is_empty() { return 0; }
    let pointer = orch_alloc(bytes.len() as i32);
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer as *mut u8, bytes.len()) };
    ((pointer as i64) << 32) | (low as i64 & 0xffff_ffff)
}
fn __orch_pack(text: String) -> i64 {
    let bytes = text.into_bytes();
    __orch_pack_bytes(&bytes, bytes.len() as i32)
}
fn __orch_pack_array<T: __OrchWasmPod>(items: Vec<T>) -> i64 {
    let mut bytes = Vec::with_capacity(items.len() * T::SIZE);
    for item in &items { item.to_bytes(&mut bytes); }
    __orch_pack_bytes(&bytes, items.len() as i32)
}
fn __orch_pack_struct(size: usize, encode: impl FnOnce(&mut [u8])) -> i64 {
    let mut bytes = vec![0u8; size];
    encode(&mut bytes);
    __orch_pack_bytes(&bytes, size as i32)
}
/// One granted host call: the arguments go to the host as bytes in this memory, freed
/// once the host has read them; the reply the host allocated here is copied and freed.
/// A reply of 0 is a call the host could not make, which decodes as no reply.
fn __orch_host_call(import: unsafe extern "C" fn(i32, i32) -> i64, payload: &[u8]) -> Vec<u8> {
    let length = payload.len() as i32;
    let pointer = orch_alloc(length);
    if pointer != 0 {
        unsafe { std::ptr::copy_nonoverlapping(payload.as_ptr(), pointer as *mut u8, payload.len()) };
    }
    let packed = unsafe { import(pointer, length) };
    orch_free(pointer, length);
    let (reply_pointer, reply_length) = ((packed >> 32) as i32, (packed & 0xffff_ffff) as i32);
    let reply = __orch_bytes(reply_pointer, reply_length).to_vec();
    orch_free(reply_pointer, reply_length);
    reply
}
"#;

/// The names a serverlet's state declares.
fn state_names(state: &[Stmt]) -> std::collections::HashSet<String> {
    state.iter().filter_map(|s| match &s.node {
        StmtNode::Let { name, .. } => Some(name.clone()),
        _ => None,
    }).collect()
}

/// The type of a literal initializer, for state whose type nothing else supplies — a
/// serverlet inside an imported module, whose body the typechecker does not walk.
pub(super) fn literal_type(value: &crate::ast::Expr) -> Option<Type> {
    match &value.node {
        ExprNode::Literal(crate::ast::Literal::Int(_)) => Some(Type::Int),
        ExprNode::Literal(crate::ast::Literal::Float(_)) => Some(Type::Float),
        ExprNode::Literal(crate::ast::Literal::Str(_)) => Some(Type::Str),
        ExprNode::Literal(crate::ast::Literal::Bool(_)) => Some(Type::Bool),
        ExprNode::StringInterp { .. } => Some(Type::Str),
        ExprNode::Unary { operand, .. } => literal_type(operand),
        ExprNode::ArrayLiteral(items) => {
            let inner = literal_type(items.first()?)?;
            Some(Type::Array(Box::new(inner), Vec::new()))
        }
        _ => None,
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
            let enc = fields.iter().map(|(f, _)| format!("self.{}.wire_encode(out);", rust_ident(f))).collect::<Vec<_>>().join(" ");
            let dec = fields.iter().map(|(f, _)| format!("{}: OrchWire::wire_decode(buf, pos)?", rust_ident(f))).collect::<Vec<_>>().join(", ");
            format!(
                "impl OrchWire for {name} {{\n    fn wire_encode(&self, out: &mut Vec<u8>) {{ {enc} }}\n    fn wire_decode(buf: &[u8], pos: &mut usize) -> Option<Self> {{ Some({name} {{ {dec} }}) }}\n}}\n",
                name = name, enc = enc, dec = dec
            )
        })
        .collect()
}

/// What crosses a sandbox boundary, in the words the diagnostics use.
const SANDBOX_TYPES: &str = "a sandboxed serverlet carries int, float, bool, string, int[], float[], bool[], and structs whose fields are those numbers, booleans, or such structs";

/// Returns Some(reason) if a handler uses a type the sandbox boundary cannot carry: the
/// C ABI's set (0.14.0), so the fastest boundary and the contained one carry the same
/// values under the same ownership rule. Strings cross too, as they do there.
pub(crate) fn sandbox_unsupported_reason(name: &str, handlers: &[Handler], structs: &StructDefs) -> Option<String> {
    for h in handlers {
        for p in &h.params {
            if !sandbox_type_supported(&p.ty, structs) {
                return Some(format!(
                    "sandboxed serverlet '{}': handler '{}' parameter '{}' has type {}, which does not cross the sandbox boundary; {}",
                    name, h.name, p.name, p.ty.display_name(), SANDBOX_TYPES
                ));
            }
        }
        if h.return_type != Type::Void && !sandbox_type_supported(&h.return_type, structs) {
            return Some(format!(
                "sandboxed serverlet '{}': handler '{}' returns {}, which does not cross the sandbox boundary; {}",
                name, h.name, h.return_type.display_name(), SANDBOX_TYPES
            ));
        }
    }
    None
}

fn sandbox_type_supported(ty: &Type, structs: &StructDefs) -> bool {
    match ty {
        Type::Int | Type::Float | Type::Bool | Type::Str => true,
        Type::Array(inner, _) => matches!(**inner, Type::Int | Type::Float | Type::Bool),
        Type::Named(name) => sandbox_struct_supported(name, structs, 0),
        _ => false,
    }
}

/// A struct crosses as its `#[repr(C)]` bytes, so its fields must be numbers, booleans,
/// or structs that cross the same way — the C ABI's rule for a struct by value.
fn sandbox_struct_supported(name: &str, structs: &StructDefs, depth: usize) -> bool {
    if depth > 32 {
        return false;
    }
    structs.iter()
        .find(|(n, _)| n == name)
        .map_or(false, |(_, fields)| fields.iter().all(|(_, fty)| match fty {
            Type::Int | Type::Float | Type::Bool => true,
            Type::Named(inner) => sandbox_struct_supported(inner, structs, depth + 1),
            _ => false,
        }))
}

/// Field-by-field codecs for the structs that can cross a sandbox boundary: each field
/// at its own `#[repr(C)]` offset, little-endian, padding zero. Emitted on both sides,
/// so neither side reads a struct's padding or depends on the other's endianness.
pub(crate) fn sandbox_struct_codecs(structs: &StructDefs) -> String {
    structs.iter()
        .filter(|(name, _)| sandbox_struct_supported(name, structs, 0))
        .map(|(name, fields)| {
            let writes = fields.iter().map(|(f, ty)| { let f = rust_ident(f); match ty {
                Type::Int | Type::Float => format!("    out[std::mem::offset_of!({name}, {f})..][..8].copy_from_slice(&value.{f}.to_le_bytes());"),
                Type::Bool => format!("    out[std::mem::offset_of!({name}, {f})] = value.{f} as u8;"),
                Type::Named(inner) => format!("    __orch_sandbox_write_{inner}(&value.{f}, &mut out[std::mem::offset_of!({name}, {f})..][..std::mem::size_of::<{inner}>()]);"),
                _ => String::new(),
            } }).collect::<Vec<_>>().join("\n");
            let reads = fields.iter().map(|(f, ty)| { let f = rust_ident(f); match ty {
                Type::Int => format!("        {f}: i64::from_le_bytes(bytes[std::mem::offset_of!({name}, {f})..][..8].try_into().unwrap()),"),
                Type::Float => format!("        {f}: f64::from_le_bytes(bytes[std::mem::offset_of!({name}, {f})..][..8].try_into().unwrap()),"),
                Type::Bool => format!("        {f}: bytes[std::mem::offset_of!({name}, {f})] != 0,"),
                Type::Named(inner) => format!("        {f}: __orch_sandbox_read_{inner}(&bytes[std::mem::offset_of!({name}, {f})..][..std::mem::size_of::<{inner}>()]),"),
                _ => String::new(),
            } }).collect::<Vec<_>>().join("\n");
            format!(
                "#[allow(non_snake_case)]\nfn __orch_sandbox_write_{name}(value: &{name}, out: &mut [u8]) {{\n{writes}\n}}\n#[allow(non_snake_case)]\nfn __orch_sandbox_read_{name}(bytes: &[u8]) -> {name} {{\n    {name} {{\n{reads}\n    }}\n}}\n"
            )
        })
        .collect()
}
