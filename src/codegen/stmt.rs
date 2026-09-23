use crate::ast::{Expr, ExprNode, StmtNode, Stmt, Type, Handler};
use super::core::{Codegen, StateRewrite, pascal_case, rust_ident, runtime_preamble, SECRET_CHILD_FRAMES};

/// A generic function's type parameters with their bounds: what every one needs, and what
/// its body needs of it — to be shown, compared, or ordered — as the check found.
fn type_params_str(type_params: &[String], needs: Option<&std::collections::BTreeMap<String, std::collections::BTreeSet<&'static str>>>) -> String {
    if type_params.is_empty() {
        String::new()
    } else {
        let bounds = type_params.iter()
            .map(|t| {
                let extra = needs.and_then(|needs| needs.get(t)).map(|bounds| bounds.iter().map(|bound| match *bound {
                    "Display" => " + std::fmt::Display",
                    "PartialEq" => " + PartialEq",
                    _ => " + PartialOrd",
                }).collect::<String>()).unwrap_or_default();
                format!("{}: Clone + std::fmt::Debug + Send + Sync + 'static{}", t, extra)
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!("<{}>", bounds)
    }
}

impl Codegen {
    pub fn compile_stmt(&mut self, stmt: &Stmt) -> String {
        match &stmt.node {
            // Declared with the other queues, where they live; nothing to run here.
            StmtNode::SharedQueue { .. } => String::new(),
            // Read before any code is generated; nothing to run here.
            StmtNode::Measure { .. } => String::new(),
            StmtNode::Let { name, ty, value, shared } => {
                if *shared {
                    // The binding lives in the shared struct, initialised there; nothing
                    // is declared at the point it was written.
                    return String::new();
                }
                let before = self.shared_touches.get();
                let awaits_before = self.awaits;
                let outer = std::mem::replace(&mut self.guard_held, true);
                if matches!(value.node, ExprNode::AutomaticBlock { .. }) { self.naming = Some(name.clone()); }
                let val_str = self.compile_value(value);
                self.naming = None;
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
            // An `if` that waits somewhere is not one unit: its condition takes the lock by
            // itself, and each statement in its branches takes it or waits on its own. One
            // that does not wait stays one unit, so `if n < 10 { n = n + 1 }` is atomic.
            StmtNode::Expr(expr) if !self.guard_held && matches!(expr.node, ExprNode::If { .. }) && self.may_wait(expr) => {
                self.compile_split_if(expr)
            }
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
                    let compiled = self.compile_leaving_value(expr);
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
                let generics = type_params_str(type_params, self.generic_bounds.get(name.as_str()));
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
                    self.tail_moves = !force_semi;
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
                let generics = type_params_str(type_params, self.generic_bounds.get(name.as_str()));
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
                    self.tail_moves = !force_semi;
                    let inner = self.compile_block_inner(body, force_semi);
                    format!("{{\n    {}\n}}", inner.replace("\n", "\n    "))
                } else {
                    self.compile_expr(body)
                };
                let vis = if self.is_main { "" } else { "pub " };
                format!("{}async fn {}{}({}){} {}", vis, rust_ident(name), generics, params_str, ret_str, body_str)
            }
            StmtNode::ProcessDecl { name, params, return_type, body, type_params } => {
                let generics = type_params_str(type_params, self.generic_bounds.get(name.as_str()));
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
                    self.tail_moves = !force_semi;
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
                    return self.compile_main_orchestrator(params, body);
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
                // The event carries copies: the trigger's bindings are still there after it.
                let payload = if args.is_empty() {
                    "()".to_string()
                } else if args.len() == 1 {
                    self.compile_value(&args[0])
                } else {
                    format!("({})", self.compile_values(args))
                };
                if self.library && event_name != "update_orchestrator" {
                    if self.context_bound {
                        return format!("{{ let value = std::sync::Arc::new({payload}); let handlers = __context.event_{event_name}.lock().unwrap().clone(); for handler in handlers {{ __context.queue_event(handler(value.clone())); }} }}");
                    }
                    return format!("let context = crate::__orch_context(); let value = std::sync::Arc::new({payload}); let handlers = {func_name}().lock().unwrap().clone(); for handler in handlers {{ context.queue_event(handler(value.clone())); }}");
                }
                // A measured binary's events carry when they were triggered.
                let payload = if self.measure.hooks && event_name != "update_orchestrator" { format!("(std::time::Instant::now(), {payload})") } else { payload };
                // A subscriber that has stopped — a handler taken out of the active set —
                // has a closed channel, and is dropped here rather than warned about.
                format!(
                    "{{\n    let payload_eval = std::sync::Arc::new({});\n    if let Ok(mut handlers) = {}().lock() {{\n        handlers.retain(|tx| !tx.is_closed());\n        for tx in handlers.iter() {{\n            if let Err(tokio::sync::mpsc::error::TrySendError::Full(_)) = tx.try_send(std::sync::Arc::clone(&payload_eval)) {{\n                eprintln!(\"[orchestrate] warning: dropped event '{}' — subscriber channel full\");\n            }}\n        }}\n    }}\n}}",
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
                            futures.push(format!("async {{ {} }}", self.compile_value(value)));
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
                // The array is walked where it lives, one copied element at a time. A
                // `shared let` is copied out under the lock, which is released before the
                // body runs; the body's statements take it for themselves.
                let before = self.shared_touches.get();
                let outer = std::mem::replace(&mut self.guard_held, true);
                let iter_str = match &iter.node {
                    ExprNode::Call { callee, args } if callee == "range" && args.len() == 1 => {
                        format!("(0i64..({}) as i64)", self.compile_expr(&args[0]))
                    }
                    ExprNode::Call { callee, args } if callee == "range" && args.len() == 2 => {
                        format!("(({}) as i64..({}) as i64)", self.compile_expr(&args[0]), self.compile_expr(&args[1]))
                    }
                    _ => format!("({}).clone().into_iter()", self.compile_place(iter)),
                };
                self.guard_held = outer;
                let iter_str = if self.shared_touches.get() != before && !outer { self.wrap_shared(iter_str) } else { iter_str };

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
                    let body_str = self.compile_statement_body(body);
                    format!("for {} in {} {}", rust_ident(var), iter_str, body_str)
                };
                self.pop_scope();
                compiled
            }
            StmtNode::While { cond, body } => {
                // The condition reads shared state under the lock, released before the body.
                let before = self.shared_touches.get();
                let outer = std::mem::replace(&mut self.guard_held, true);
                let cond_str = self.compile_expr(cond);
                self.guard_held = outer;
                let cond_str = if self.shared_touches.get() != before && !outer { self.wrap_shared(cond_str) } else { cond_str };
                let body_str = self.compile_statement_body(body);
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
                // `every:`: the runtime calls `tick` on a cadence of host time, and keeps its
                // last answer for `latest(...)`, which says when there is none — so on the host
                // side that call answers `ok` or `err` whatever `tick` declares. A program's
                // own call to `tick` still gets the declared type: the method unwraps it.
                let every = landline.as_ref().and_then(|c| c.every_micros)
                    .or(sandbox.as_ref().and_then(|c| c.every_micros))
                    .filter(|_| self.library);
                let tick_answer = every.and_then(|_| handlers.iter().find(|h| h.name == "tick"))
                    .filter(|h| h.return_type != Type::Void)
                    .map(|h| answered_type(&h.return_type).clone());
                let unwrapped_tick = every.is_some() && handlers.iter().any(|h| h.name == "tick" && h.return_type != Type::Void && !answers_failure(h));
                let host_handlers: Vec<Handler> = handlers.iter().map(|h| {
                    let mut h = h.clone();
                    if unwrapped_tick && h.name == "tick" {
                        h.return_type = Type::Result(Box::new(h.return_type.clone()), Box::new(Type::Str));
                    }
                    h
                }).collect();
                let handlers = &host_handlers;
                // `measure serverlets`: each handler not declared `unmeasured` has a record,
                // its slot in the serverlet's `stats` its place in this list.
                self.measured_handlers = if self.measure.serverlets {
                    handlers.iter().filter(|h| !h.unmeasured).map(|h| h.name.clone()).collect()
                } else {
                    Vec::new()
                };
                let mut enum_variants = Vec::new();
                for h in handlers {
                    let variant_name = pascal_case(&h.name);
                    let mut fields = h.params.iter()
                        .map(|p| format!("{}: {}", rust_ident(&p.name), self.compile_type(&p.ty)))
                        .collect::<Vec<String>>();
                    let ret_ty = self.compile_type(&h.return_type);
                    fields.push(format!("reply_to: tokio::sync::oneshot::Sender<{}>", ret_ty));
                    // When the call was sent, for how late the handler started on it.
                    if self.measured_slot(h).is_some() { fields.push("__sent: std::time::Instant".to_string()); }
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
                    let slot = self.measured_slot(h);
                    if slot.is_some() { send_fields.push("__sent: __begun".to_string()); }

                    // The reply is awaited through the actor, which in a deterministic run is
                    // what runs the actor.
                    let await_expr = if h.return_type == Type::Void {
                        "let _ = self.actor.reply(reply_rx).await;".to_string()
                    } else if answers_failure(h) {
                        format!(
                            "self.actor.reply(reply_rx).await.unwrap_or_else(|_| Err({:?}.to_string()))",
                            format!("serverlet '{}' stopped before it answered '{}'", name, h.name)
                        )
                    } else {
                        format!("self.actor.reply(reply_rx).await.unwrap_or_default()")
                    };

                    let budget = landline.as_ref().and_then(|config| config.budget_micros);
                    let body = if let Some(micros) = budget {
                        // A budgeted call returns early instead of waiting past its deadline.
                        let missed = format!(
                            "{:?}.to_string()",
                            format!("landline '{}' handler '{}' missed its budget of {}", name, h.name, crate::ast::display_micros(micros))
                        );
                        let fallback = match landline.as_ref().map(|config| config.late) {
                            Some(crate::ast::LatePolicy::Latest) if answers_failure(h) => {
                                format!("self.latest.{}.lock().unwrap().clone().unwrap_or_else(|| Err({}))", rust_ident(&h.name), missed)
                            }
                            Some(crate::ast::LatePolicy::Latest) if h.return_type != Type::Void => {
                                format!("self.latest.{}.lock().unwrap().clone().unwrap_or_default()", rust_ident(&h.name))
                            }
                            _ => self.failure_reply(h, &missed),
                        };
                        // A deterministic run ignores the budget, since a result that depends
                        // on wall time cannot replay; a host can also lift budgets itself.
                        format!(
                            "        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();\n        let call = async {{\n            let _ = self.tx.send({}Msg::{} {{ {} }}).await;\n            self.actor.reply(reply_rx).await.ok()\n        }};\n        let answered = if __orch_budgets_apply() {{ tokio::time::timeout(std::time::Duration::from_micros({}), call).await.ok().flatten() }} else {{ call.await }};\n        match answered {{\n            Some(value) => value,\n            None => {},\n        }}",
                            name, variant_name, send_fields.join(", "), micros, fallback
                        )
                    } else {
                        format!(
                            "        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();\n        let _ = self.tx.send({}Msg::{} {{ {} }}).await;\n        {}",
                            name, variant_name, send_fields.join(", "), await_expr
                        )
                    };

                    // A measured call is timed as its caller sees it, answer or failure.
                    let body = match slot {
                        Some(slot) => format!(
                            "        let __begun = std::time::Instant::now();\n        let __orch_stats = &self.stats;\n        let __answer = async {{\n{body}\n        }}.await;\n        __orch_stats[{slot}].called(__begun.elapsed());\n        __answer"
                        ),
                        None => body,
                    };
                    if unwrapped_tick && h.name == "tick" {
                        // The runtime's call, and the program's, which answers as declared.
                        let declared = self.compile_type(answered_type(&h.return_type));
                        client_methods.push(format!(
                            "    pub async fn __cadence_tick({}{}) -> {} {{\n{}\n    }}\n\n    \
                             pub async fn tick({}{}) -> {} {{\n        \
                                 match self.__cadence_tick({}).await {{\n            \
                                     Ok(__value) => __value,\n            \
                                     Err(__why) => {{ eprintln!(\"[orchestrate] {{}}\", __why); Default::default() }}\n        \
                                 }}\n    }}",
                            self_params, method_params, ret_ty, body,
                            self_params, method_params, declared,
                            h.params.iter().map(|p| rust_ident(&p.name)).collect::<Vec<_>>().join(", ")
                        ));
                        continue;
                    }
                    client_methods.push(format!(
                        "    pub async fn {}({}{}) -> {} {{\n{}\n    }}",
                        rust_ident(&h.name), self_params, method_params, ret_ty, body
                    ));
                }
                // What `latest(p.tick)` reads: the runtime's last call to `tick`, answered.
                let (cadence_field, cadence_method) = match &tick_answer {
                    Some(answer) => {
                        let ty = format!("Result<{}, String>", self.compile_type(answer));
                        (
                            format!("    cadence: std::sync::Arc<std::sync::Mutex<{ty}>>,\n"),
                            format!("\n\n    pub fn cadence_latest(&self) -> {ty} {{\n        self.cadence.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()\n    }}"),
                        )
                    }
                    None => (String::new(), String::new()),
                };

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

                // The records of its measured handlers, shared by every copy of the client and
                // its actor.
                let stats_field = if self.measured_handlers.is_empty() {
                    String::new()
                } else {
                    format!("    stats: std::sync::Arc<[std::sync::Arc<crate::__OrchUnit>; {}]>,\n", self.measured_handlers.len())
                };
                let client_struct = format!(
                    "{}#[derive(Clone, Debug)]\npub struct {}Client {{\n    tx: tokio::sync::mpsc::Sender<{}Msg>,\n    actor: __OrchActor,\n{}{}{}}}\n\nimpl {}Client {{\n{}{}\n}}",
                    latest_struct, name, name, latest_field, cadence_field, stats_field, name, client_methods.join("\n\n"), cadence_method
                );
                let measured = !self.measured_handlers.is_empty();

                if let Some(config) = landline {
                    let start_fn = self.compile_python_mirror(name, handlers, crash_handler, grants, config);
                    let start_fn = self.measure_start(start_fn, name, &["latest", "actor"]);
                    let fields: &[&str] = if measured { &["latest", "actor", "stats"] } else { &["latest", "actor"] };
                    let start_fn = match every {
                        Some(micros) => Self::register_cadence(start_fn, name, micros, fields, tick_answer.is_some(), unwrapped_tick),
                        None => start_fn,
                    };
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
                    let body_str = self.compile_handler_body(h);

                    let handler_name_str = &h.name;

                    // The panic's own message, which `on_crash` binds and a `result` handler's
                    // caller gets as its `err`.
                    let failed = self.failure_reply(h, &format!("format!(\"serverlet '{name}' handler '{handler_name_str}' panicked: {{}}\", __panic_text)"));
                    let crash_recovery = if let Some((err_name, crash_body)) = crash_handler {
                        let crash_body_str = self.compile_expr(crash_body);
                        format!(
                            "Err(__panic_err) => {{\n                    let __panic_text = __orch_panic_text(&*__panic_err);\n                    let {err_name} = __panic_text.clone();\n                    eprintln!(\"[orchestrate] serverlet '{name}' handler '{handler_name_str}' panicked: {{}}\", {err_name});\n                    {crash_body_str};\n                    let _ = reply_to.send({failed});\n                }}",
                            err_name = rust_ident(err_name),
                            name = name,
                            handler_name_str = handler_name_str,
                            crash_body_str = crash_body_str,
                        )
                    } else {
                        format!(
                            "Err(__panic_err) => {{\n                    let __panic_text = __orch_panic_text(&*__panic_err);\n                    eprintln!(\"[orchestrate] serverlet '{name}' handler '{handler_name_str}' panicked: {{}}\", __panic_text);\n                    let _ = reply_to.send({failed});\n                }}",
                            name = name,
                            handler_name_str = handler_name_str,
                        )
                    };

                    // A measured handler: how late it started on the call, and the CPU time it
                    // used, which is exact here, since it runs to the end without waiting.
                    let (bindings_str, before, after) = match self.measured_slot(h) {
                        Some(slot) => (
                            format!("{bindings_str}, __sent"),
                            format!("__orch_stats[{slot}].started_late(__sent.elapsed());\n                    let __cpu = crate::__orch_thread_cpu();\n                    "),
                            format!("\n                    __orch_stats[{slot}].used(__cpu);"),
                        ),
                        None => (bindings_str, String::new(), String::new()),
                    };
                    match_arms.push(format!(
                        "                {}Msg::{} {{ {} }} => {{\n                    {}#[allow(unused_mut)]\n                    let mut __handler = std::panic::AssertUnwindSafe(||{} {{ {} }});\n                    match std::panic::catch_unwind(__handler) {{\n                        Ok(res) => {{ let _ = reply_to.send(res); }}\n                        {}\n                    }}{}\n                }}",
                        name, variant_name, bindings_str, before, self.handler_return_annotation(h), body_str, crash_recovery, after
                    ));
                }

                self.handler_state.clear();

                let mut state_vars = Vec::new();
                for s in state {
                    if let StmtNode::Let { name: vname, ty, value, .. } = &s.node {
                        let val_str = self.compile_value(value);
                        if let Some(t) = ty {
                            state_vars.push(format!("            let mut {}: {} = {};", rust_ident(vname), self.compile_type(t), val_str));
                        } else {
                            state_vars.push(format!("            let mut {} = {};", rust_ident(vname), val_str));
                        }
                    }
                }

                if *secret {
                    let mut start_fn = self.compile_secret_mirror(name, handlers, crash_handler, state);
                    if self.library {
                        start_fn = start_fn.replace(".stdout(std::process::Stdio::piped())", ".stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped())");
                        start_fn = start_fn.replace("let mut __cout = __child.stdout.take().expect(\"child stdout\");", "let mut __cout = __child.stdout.take().expect(\"child stdout\"); let stderr = __child.stderr.take().expect(\"child stderr\"); crate::__orch_spawn(async move { use tokio::io::AsyncBufReadExt; let mut lines = tokio::io::BufReader::new(stderr).lines(); while let Ok(Some(line)) = lines.next_line().await { crate::__orch_log(crate::LogLevel::Info, line); } });");
                        start_fn = start_fn.replace("tokio::spawn(", "__ORCH_LINE_SPAWN(")
                            .replace("let __dir = crate::__orch_children_dir();", "let __dir = crate::__orch_context().assets.clone();")
                            .replace("__secret_read_frame(", "crate::__orch_read_frame(")
                            .replace("rx.recv().await", "crate::__orch_recv(&mut rx).await");
                    }
                    let start_fn = self.measure_start(start_fn, name, &["actor"]);
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
                    let start_fn = self.measure_start(start_fn, name, &["actor"]);
                    let fields: &[&str] = if measured { &["actor", "stats"] } else { &["actor"] };
                    let start_fn = match every {
                        Some(micros) => Self::register_cadence(start_fn, name, micros, fields, tick_answer.is_some(), unwrapped_tick),
                        None => start_fn,
                    };
                    return format!("{}\n\n{}\n\n{}", msg_enum, client_struct, start_fn);
                }

                let start_fn = format!(
                    "#[allow(non_snake_case)]\npub fn start_{}() -> {}Client {{\n    let (tx, mut rx) = tokio::sync::mpsc::channel::<{}Msg>(100);\n    let actor = __OrchActor::spawn(false, async move {{\n{}\n        while let Some(msg) = rx.recv().await {{\n            match msg {{\n{}\n            }}\n        }}\n    }});\n    {}Client {{ tx, actor }}\n}}",
                    name, name, name,
                    state_vars.join("\n"),
                    match_arms.join("\n\n"),
                    name
                );
                let start_fn = self.measure_start(start_fn, name, &["actor"]);

                format!("{}\n\n{}\n\n{}", msg_enum, client_struct, start_fn)
            }
            // A module's type is the crate root's, where the entry file defines it.
            StmtNode::StructDef { name, .. } | StmtNode::EnumDef { name, .. } if !self.is_main => {
                format!("pub use crate::{};", rust_ident(name))
            }
            StmtNode::StructDef { name, fields } => {
                let fields_str = fields.iter()
                    .map(|(fname, fty)| format!("    pub {}: {},", rust_ident(fname), self.compile_type(fty)))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("#[derive({})]\n#[repr(C)]\npub struct {} {{\n{}\n}}", struct_derives(name, &self.struct_defs, &self.enum_defs), rust_ident(name), fields_str)
            }
            StmtNode::EnumDef { name, variants } => self.enum_definition(name, variants),
        }
    }

    /// `orchestrator main`: the program's entry. Its body runs where the top-level bindings
    /// are in scope, so it reaches a client, a worker, or state declared beside it. In a
    /// standalone program that is `main` itself, which runs the body in place; in a library
    /// the body runs beside the ticks, as a future that owns copies of what it names.
    ///
    /// Event handlers are registered before the body runs, so an event the body — or an
    /// `on_start` — triggers is handled.
    fn compile_main_orchestrator(&mut self, params: &[crate::ast::Param], body: &Expr) -> String {
        let process_names = Self::process_names(&self.local_stmts, params, body);

        // The hooks written inside the body: `on_start` runs first, before the workers
        // start; `on_stop` is armed for Ctrl+C. In a library both are the library's hooks.
        let (hooks, rest): (Vec<Stmt>, Vec<Stmt>) = match &body.node {
            ExprNode::Block(stmts) => stmts.iter().cloned().partition(|s| matches!(s.node, StmtNode::OnStart(_) | StmtNode::OnStop(_))),
            _ => (Vec::new(), Vec::new()),
        };
        let body = match &body.node {
            ExprNode::Block(_) => crate::ast::Spanned { span: body.span, node: ExprNode::Block(rest) },
            _ => body.clone(),
        };
        if self.library {
            self.body_hooks = hooks.clone();
            // Instance state is known before any handler is compiled, since a handler
            // reaches it through the tick rather than capturing a copy.
            self.program_fields = self.state_fields().iter().map(|f| (f.name.clone(), f.boxed)).collect();
        }

        let mut decl_code = Vec::new();
        let mut exec_code = Vec::new();
        let local_stmts = self.local_stmts.clone();
        for s in &local_stmts {
            if self.library && matches!(&s.node, StmtNode::OnStart(_) | StmtNode::OnStop(_) | StmtNode::OnTick { .. } | StmtNode::OnFixedTick { .. }) { continue; }
            let is_handler = matches!(&s.node, StmtNode::Expr(crate::ast::Spanned { node: ExprNode::TriggeredBlock { .. }, .. }))
                || matches!(&s.node, StmtNode::Let { value: crate::ast::Spanned { node: ExprNode::TriggeredBlock { .. }, .. }, .. });
            self.top_level_handler = is_handler;
            let compiled = self.compile_stmt(s);
            self.top_level_handler = false;
            match &s.node {
                // A handler bound to a name is a process. Named in a process set, it runs
                // while it is in the active set; named in none, it is registered at start
                // for the program's lifetime, like one that has no name.
                StmtNode::Let { name, value, .. } if matches!(value.node, ExprNode::TriggeredBlock { .. }) => {
                    decl_code.push(compiled);
                    if self.library || !process_names.contains(name) {
                        decl_code.push(format!("{}();", rust_ident(name)));
                    }
                }
                StmtNode::Expr(crate::ast::Spanned { node: ExprNode::TriggeredBlock { .. }, .. }) => {
                    decl_code.push(format!("({})();", compiled));
                }
                StmtNode::Let { .. } => decl_code.push(Self::terminated(compiled)),
                _ => exec_code.push(Self::terminated(compiled)),
            }
        }

        let mut start_hooks = String::new();
        if !self.library {
            for hook in &hooks {
                start_hooks.push_str(&Self::terminated(self.compile_stmt(hook)));
                start_hooks.push('\n');
            }
        }

        let process_param = match params {
            [param] => match &param.ty {
                Type::Array(inner, initial) if **inner == Type::Process => Some((param.name.clone(), initial.clone())),
                _ => None,
            },
            _ => None,
        };
        // The parameters are bound from the top-level declarations of the same names: a
        // process set from the workers it lists.
        let mut bind_params = String::new();
        for p in params {
            match &process_param {
                Some((name, initial)) if *name == p.name => {
                    let procs = initial.iter().map(|w| format!("{}.clone()", rust_ident(w))).collect::<Vec<_>>().join(", ");
                    bind_params.push_str(&format!("let {}: Vec<ProcessRef> = vec![{}];\n", rust_ident(name), procs));
                }
                _ => bind_params.push_str(&format!("let {0} = {0}.clone();\n", rust_ident(&p.name))),
            }
        }

        let body_str = self.compile_block_inner(&body, true);
        let run = match &process_param {
            Some((name, _)) => format!(
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

    // A worker's restarts are its own policy's to decide; nothing here restarts it.

    let (tx, mut rx) = tokio::sync::mpsc::channel::<std::sync::Arc<Vec<ProcessRef>>>(100);
    get_registry_update_orchestrator().lock().unwrap().push(tx);
    let state_clone = state.clone();
    tokio::spawn(async move {{
        // One set at a time, in the order they were triggered.
        while let Some(msg) = rx.recv().await {{
            let new_procs = (*msg).clone();
            {{
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
                    // A worker that has stopped and is named again starts afresh.
                    if found && !handle.is_finished() {{
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
            }}
        }}
    }});

    {body}"#,
                param = rust_ident(name),
                body = body_str
            ),
            None => body_str,
        };
        let run = format!("{start_hooks}{run}");

        if self.library {
            // The body runs beside the ticks, so it owns copies of what it names, taken
            // before the top-level state is handed to the hooks.
            let mut free = std::collections::HashSet::new();
            let mut bound: std::collections::HashSet<String> = params.iter().map(|p| p.name.clone()).collect();
            self.get_free_vars_expr(&body, &mut bound, &mut free);
            let mut free: Vec<String> = free.into_iter().filter(|n| !self.is_shared(n) && !params.iter().any(|p| p.name == *n)).collect();
            free.sort();
            let captures = free.iter().map(|n| format!("let {0} = {0}.clone();\n", rust_ident(n))).collect::<String>();
            let future = format!("{{\n{bind_params}{captures}async move {{\n    {}\n}}\n}}", run.replace("\n", "\n    "));
            return self.library_entry(&future, &decl_code.join("\n"), &exec_code.join("\n"));
        }

        let mut main_body = String::new();
        if !decl_code.is_empty() {
            main_body.push_str(&decl_code.join("\n"));
            main_body.push('\n');
        }
        // In place, borrowing `main`'s bindings; an async block so a `return` in the body
        // ends the body, not the program.
        let inline = format!("{bind_params}async {{\n    {}\n}}.await;", run.replace("\n", "\n    "));
        main_body.push_str(&format!("{{\n    {}\n}}\n", inline.replace("\n", "\n    ")));
        if !exec_code.is_empty() {
            main_body.push_str(&exec_code.join("\n"));
            main_body.push('\n');
        }
        main_body.push_str("loop {\n    tokio::time::sleep(std::time::Duration::from_secs(3600)).await;\n}");
        format!(
            "#[tokio::main]\nasync fn main() -> Result<(), Box<dyn std::error::Error>> {{\n    {}\n}}",
            main_body.replace("\n", "\n    ")
        )
    }

    /// An `if` whose branches wait: see `compile_stmt`.
    fn compile_split_if(&mut self, expr: &Expr) -> String {
        let ExprNode::If { cond, then_branch, else_branch } = &expr.node else { unreachable!() };
        let before = self.shared_touches.get();
        let awaits_before = self.awaits;
        let outer = std::mem::replace(&mut self.guard_held, true);
        let cond_str = self.compile_expr(cond);
        self.guard_held = outer;
        if self.shared_touches.get() != before && self.awaits != awaits_before {
            self.errors.push(
                "a statement cannot both wait and touch shared state: the lock would be held across the wait, blocking every other reader. Split it into two statements.".to_string()
            );
        }
        let cond_str = if self.shared_touches.get() != before { self.wrap_shared(cond_str) } else { cond_str };
        let then_str = self.compile_statement_body(then_branch);
        match else_branch {
            Some(branch) if matches!(branch.node, ExprNode::If { .. }) => {
                let else_str = self.compile_split_if(branch);
                format!("if {cond_str} {then_str} else {else_str}")
            }
            Some(branch) => format!("if {cond_str} {then_str} else {}", self.compile_statement_body(branch)),
            None => format!("if {cond_str} {then_str}"),
        }
    }

    /// Whether `expr` waits — calls a task, `sleep`, or a serverlet, or has a `parallel`
    /// block — outside any closure, worker, or handler inside it, which wait on their own.
    /// Codegen's own rule, from `require_async`, read before anything is compiled.
    pub(super) fn may_wait(&self, expr: &Expr) -> bool {
        let mut waits = false;
        crate::ast::walk_expr_within(expr, &mut |node| {
            waits |= match node {
                crate::ast::Node::Stmt(stmt) => matches!(stmt.node, StmtNode::Parallel(_)),
                crate::ast::Node::Expr(e) => match &e.node {
                    ExprNode::Call { callee, .. } => callee == "sleep" || self.tasks.contains(callee) || (callee == "take" && !self.user_functions.contains(callee)),
                    ExprNode::ModuleCall { module_local_name, function, .. } => {
                        if self.modules.contains(module_local_name) {
                            self.tasks.contains(&format!("{}::{}", module_local_name, function))
                        } else {
                            self.is_binding_receiver(module_local_name)
                        }
                    }
                    ExprNode::MethodCall { .. } => true,
                    _ => false,
                },
            };
        });
        waits
    }

    /// A compiled statement with its terminating semicolon.
    fn terminated(compiled: String) -> String {
        if compiled.ends_with(';') || compiled.ends_with('}') { compiled } else { format!("{compiled};") }
    }

    /// Every name the program uses as a process: the workers the main orchestrator's
    /// process set lists, and those in any `update_orchestrator` set.
    fn process_names(stmts: &[Stmt], params: &[crate::ast::Param], body: &Expr) -> std::collections::HashSet<String> {
        let mut names = std::collections::HashSet::new();
        for p in params {
            if let Type::Array(inner, initial) = &p.ty {
                if **inner == Type::Process { names.extend(initial.iter().cloned()); }
            }
        }
        let mut visit = |node: crate::ast::Node| {
            let crate::ast::Node::Stmt(Stmt { node: StmtNode::Trigger { event_name, args }, .. }) = node else { return };
            if event_name != "update_orchestrator" { return; }
            for arg in args {
                if let ExprNode::ArrayLiteral(items) = &arg.node {
                    names.extend(items.iter().filter_map(|item| match &item.node {
                        ExprNode::Identifier(name) => Some(name.clone()),
                        _ => None,
                    }));
                }
            }
        };
        crate::ast::walk_expr(body, &mut visit);
        for stmt in stmts { crate::ast::walk_stmt(stmt, &mut visit); }
        names
    }

    /// The host side of a secret serverlet: an actor that forwards each call to the child
    /// process and its reply back. The child is started for the first call and again for
    /// the first call after it dies — killed, crashed, or exited — with `on_crash`, if
    /// declared, run on the host each time it is lost. Its state starts over when it does.
    fn compile_secret_mirror(&mut self, name: &str, handlers: &[Handler], crash_handler: &Option<(String, Box<Expr>)>, state: &[Stmt]) -> String {
        if let Some(reason) = handler_wire_unsupported_reason(handlers, &self.struct_defs) {
            return format!("compile_error!(\"secret serverlet '{}': {}\");\n", name, reason);
        }
        // `on_crash` runs on the host, after the child is gone, with the reason bound; the
        // state lived in the child, so it is not reachable here.
        let crash = match crash_handler {
            Some((error_name, body)) => {
                let mut free = std::collections::HashSet::new();
                self.get_free_vars_expr(body, &mut std::collections::HashSet::new(), &mut free);
                let mut reached: Vec<&String> = free.iter().filter(|v| state_names(state).contains(*v)).collect();
                reached.sort();
                if let Some(reached) = reached.first() {
                    self.errors.push(format!(
                        "secret serverlet '{name}': on_crash uses '{reached}', which is the child's state. on_crash runs on the host after the child is gone, so the state is gone with it; report the error and let the next child start over"
                    ));
                }
                format!("{{ let {} = __error.clone(); {}; }}", rust_ident(error_name), self.compile_statement_body(body))
            }
            None => String::new(),
        };

        let mut arms = Vec::new();
        let mut default_arms = Vec::new();
        for (k, h) in handlers.iter().enumerate() {
            let variant = pascal_case(&h.name);
            let param_names: Vec<String> = h.params.iter().map(|p| rust_ident(&p.name)).collect();
            let binding = if param_names.is_empty() {
                "reply_to".to_string()
            } else {
                format!("{}, reply_to", param_names.join(", "))
            };
            // A measured handler: how late its call reached the child's turn.
            let (binding, started) = match self.measured_slot(h) {
                Some(slot) => (format!("{binding}, __sent"), format!("__orch_stats[{slot}].started_late(__sent.elapsed());\n                    ")),
                None => (binding, String::new()),
            };

            // CALL payload: the handler id, then each argument in the wire encoding.
            let encode_args = h.params.iter()
                .map(|p| format!("                    {}.wire_encode(&mut __payload);\n", rust_ident(&p.name)))
                .collect::<String>();

            // A reply the host cannot read is a failed call, not an answer.
            let unreadable = self.failure_reply(h, &format!("{:?}.to_string()", format!("secret serverlet '{}' handler '{}' answered with a reply that could not be read", name, h.name)));
            let decode = if h.return_type == Type::Void {
                "()".to_string()
            } else if answers_failure(h) {
                format!("{{ let mut __p = 0usize; <{} as OrchWire>::wire_decode(&__f.payload, &mut __p).unwrap_or_else(|| {}) }}", self.compile_type(&h.return_type), unreadable)
            } else {
                format!("{{ let mut __p = 0usize; <{} as OrchWire>::wire_decode(&__f.payload, &mut __p).unwrap_or_default() }}", self.compile_type(&h.return_type))
            };
            // The child reports a handler that panicked, or a call it could not make, with
            // an error frame; the child itself is fine.
            let failed = self.failure_reply(h, &format!("format!(\"secret serverlet '{}' handler '{}' failed: {{}}\", __msg)", name, h.name));
            let lost = self.failure_reply(h, "__why.clone()");
            let not_running = self.failure_reply(h, &format!("format!(\"secret serverlet '{}' is not running: it could not start ({{}})\", __start_error)", name));

            arms.push(format!(
"                {n}Msg::{v} {{ {b} }} => {{
                    {started}__next_call = __next_call.wrapping_add(1);
                    let mut __payload = Vec::new();
                    {k}i64.wire_encode(&mut __payload);
{args}                    let __call: Result<_, String> = async {{
                        __secret_write_frame(&mut *__cin, ORCH_KIND_CALL, __next_call, &__payload).await.map_err(|e| format!(\"it is not reachable: {{}}\", e))?;
                        match __secret_read_frame(&mut *__cout).await {{
                            Ok(Some(__f)) if __f.kind == ORCH_KIND_REPLY && __f.call_id == __next_call => Ok({dec}),
                            Ok(Some(__f)) if __f.kind == ORCH_KIND_ERROR && __f.call_id == __next_call => {{
                                let mut __p = 0usize;
                                let __msg: String = OrchWire::wire_decode(&__f.payload, &mut __p).unwrap_or_default();
                                eprintln!(\"[orchestrate] secret serverlet '{n}' handler '{h}' failed: {{}}\", __msg);
                                Ok({failed})
                            }}
                            Ok(None) => Err(\"it exited during the call\".to_string()),
                            Ok(Some(_)) => Err(\"it answered out of turn\".to_string()),
                            Err(e) => Err(format!(\"its reply could not be read: {{}}\", e)),
                        }}
                    }}.await;
                    match __call {{
                        Ok(__value) => {{ let _ = reply_to.send(__value); }}
                        Err(__detail) => {{
                            let __why = format!(\"secret serverlet '{n}' was lost during a call to '{h}': {{}}\", __detail);
                            let _ = reply_to.send({lost});
                            __lost = Some(__why);
                        }}
                    }}
                }}",
                n = name, v = variant, b = binding, k = k, h = h.name, args = encode_args, dec = decode, failed = failed, lost = lost, started = started
            ));
            default_arms.push(format!("                    {name}Msg::{variant} {{ reply_to, .. }} => {{ let _ = reply_to.send({not_running}); }}"));
        }

        include_str!("secret_mirror.rs.txt")
            .replace("@NAME@", name)
            .replace("@SIGNATURES@", &Self::handler_signatures(handlers))
            .replace("@DEFAULT_ARMS@", &default_arms.join("\n"))
            .replace("@ARMS@", &arms.join("\n"))
            .replace("@CRASH@", &crash)
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
        if let Some(reason) = handler_wire_unsupported_reason(handlers, &self.struct_defs) {
            return format!("compile_error!(\"secret serverlet '{}': {}\");\n", name, reason);
        }

        let mut state_vars = Vec::new();
        for s in state {
            if let StmtNode::Let { name: vname, ty, value, .. } = &s.node {
                let val_str = self.compile_value(value);
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
            let body = self.compile_handler_body(h);
            // A `result<T>` handler's answer crosses whole, `ok` or `err`: an error frame
            // is left to mean the call itself failed.
            let finish = if h.return_type == Type::Void {
                "                        __handler();\n                        Ok(Vec::new())".to_string()
            } else {
                "                        let __r = __handler();\n                        Ok(__wire_to_bytes(&__r))".to_string()
            };
            arms.push(format!(
"                    {k}i64 => {{\n{args}\n                        if __pos != __frame.payload.len() {{ return Err(\"trailing arguments\".to_string()); }}\n                        #[allow(unused_mut)]\n                        let mut __handler = ||{returns} {{ {body} }};\n{finish}\n                    }}",
                k = k, args = arg_lets.join("\n"), returns = self.handler_return_annotation(h), body = body, finish = finish
            ));
        }

        self.handler_state.clear();

        // The child is its own program, so it needs the struct and enum definitions its
        // handlers can use, and the wire impls for the ones that cross.
        let structs = self.standalone_type_definitions() + &wire_struct_impls(&self.struct_defs);

        format!(
"// Generated by Orchestrate Compiler — secret serverlet '{name}'\n#![allow(unused_variables)]\n#![allow(dead_code)]\n#![allow(unused_imports)]\n#![allow(unused_parens)]\n#![allow(unused_mut)]\n\n{preamble}\n{frames}\n{wire}\n{structs}\nfn main() {{\n    let __stdin = std::io::stdin();\n    let mut __reader = std::io::BufReader::new(__stdin.lock());\n    let __stdout = std::io::stdout();\n    let mut __writer = std::io::BufWriter::new(__stdout.lock());\n\n{state}\n\n    let mut __hello = Vec::new();\n    ORCH_WIRE_VERSION.wire_encode(&mut __hello);\n    let __signatures: Vec<String> = vec![{sigs}];\n    __signatures.wire_encode(&mut __hello);\n    if __frame_write(&mut __writer, ORCH_KIND_HELLO, 0, &__hello).is_err() {{ return; }}\n    match __frame_read(&mut __reader) {{\n        Ok(Some(f)) if f.kind == ORCH_KIND_READY && f.call_id == 0 && f.payload.is_empty() => {{}},\n        _ => return,\n    }}\n    while let Ok(Some(__frame)) = __frame_read(&mut __reader) {{\n        if __frame.kind == ORCH_KIND_BYE {{ break; }}\n        let __result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<Vec<u8>, String> {{\n            if __frame.kind != ORCH_KIND_CALL {{ return Err(\"expected CALL\".to_string()); }}\n            let mut __pos = 0usize;\n            let __idx = i64::wire_decode(&__frame.payload, &mut __pos).ok_or_else(|| \"missing handler id\".to_string())?;\n            match __idx {{\n{arms}\n                _ => Err(\"unknown handler id\".to_string()),\n            }}\n        }}));\n        let (__kind, __payload) = match __result {{\n            Ok(Ok(value)) => (ORCH_KIND_REPLY, value),\n            Ok(Err(message)) => (ORCH_KIND_ERROR, __wire_to_bytes(&message)),\n            Err(e) => (ORCH_KIND_ERROR, __wire_to_bytes(&__panic_message(&e))),\n        }};\n        if __frame_write(&mut __writer, __kind, __frame.call_id, &__payload).is_err() {{ break; }}\n    }}\n}}\n",
            name = name,
            preamble = runtime_preamble(true, true, false),
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
    fn enum_definition(&self, name: &str, variants: &[crate::ast::EnumVariant]) -> String {
        let variants_str = variants.iter().map(|v| {
            match &v.payload {
                Some(ty) => format!("    {}({}),", rust_ident(&v.name), self.compile_type(ty)),
                None => format!("    {},", rust_ident(&v.name)),
            }
        }).collect::<Vec<_>>().join("\n");
        // An enum compares when every payload does, so `side == Side::Buy` works.
        let derives = if has_equality(&Type::Named(name.to_string()), &self.struct_defs, &self.enum_defs) { "Clone, Debug, PartialEq" } else { "Clone, Debug" };
        format!("#[derive({})]\npub enum {} {{\n{}\n}}\n", derives, rust_ident(name), variants_str)
    }

    /// The file's structs and enums for a crate of its own — a sandbox guest, a secret
    /// child — which a handler body may use: each one that stands on its own there. One
    /// holding a client, a process, a function, or a handle names something that exists
    /// only in the program, so it is left out; a handler cannot hold one anyway.
    fn standalone_type_definitions(&self) -> String {
        let mut code = String::new();
        for (name, fields) in &self.struct_defs {
            if !stands_alone(&Type::Named(name.clone()), &self.struct_defs, &self.enum_defs) { continue; }
            let fields_str = fields.iter()
                .map(|(fname, fty)| format!("    pub {}: {},", rust_ident(fname), self.compile_type(fty)))
                .collect::<Vec<_>>()
                .join("\n");
            code.push_str(&format!("#[derive({})]\n#[repr(C)]\npub struct {} {{\n{}\n}}\n", struct_derives(name, &self.struct_defs, &self.enum_defs), rust_ident(name), fields_str));
        }
        for (name, variants) in &self.enum_variants {
            if stands_alone(&Type::Named(name.clone()), &self.struct_defs, &self.enum_defs) {
                code.push_str(&self.enum_definition(name, variants));
            }
        }
        code
    }

    /// A cadenced serverlet's `start_X()`: the client it returns is also registered with the
    /// runtime, which calls its `tick` when due and keeps the answer. The registry holds the
    /// client's sender weakly, so the instance still stops when the program's last client
    /// goes; the entry is dropped at its next due tick.
    fn register_cadence(start_fn: String, name: &str, micros: u64, fields: &[&str], answers: bool, unwrapped: bool) -> String {
        let built = format!("{}Client {{ tx, {} }}", name, fields.join(", "));
        assert!(start_fn.contains(&built), "cadenced serverlet '{name}': client construction not found");
        let call = if unwrapped { "__cadence_tick" } else { "tick" };
        let (initial, keep, store) = if answers {
            (
                format!(", cadence: std::sync::Arc::new(std::sync::Mutex::new(Err({:?}.to_string())))", format!("serverlet '{name}' has not answered a tick yet")),
                ", __client.cadence.clone()".to_string(),
                "*__client.cadence.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = __answer;".to_string(),
            )
        } else {
            (String::new(), String::new(), "let _ = __answer;".to_string())
        };
        let rebuild = fields.iter().enumerate()
            .map(|(i, field)| format!("{field}: __parts.{i}.clone()"))
            .chain(answers.then(|| format!("cadence: __parts.{}.clone()", fields.len())))
            .collect::<Vec<_>>()
            .join(", ");
        let parts = fields.iter().map(|field| format!("__client.{field}.clone()")).collect::<Vec<_>>().join(", ");
        let registered = format!(
            "{{\n    let __client = {name}Client {{ tx, {fields}{initial} }};\n    \
                 let __weak = __client.tx.downgrade();\n    \
                 let __parts = ({parts}{keep},);\n    \
                 crate::__orch_context().register_cadence({nanos}u64, Box::new(move |__dt: f64| {{\n        \
                     let tx = __weak.upgrade()?;\n        \
                     let __client = {name}Client {{ tx, {rebuild} }};\n        \
                     Some(Box::pin(async move {{\n            \
                         let __answer = __client.{call}(__dt).await;\n            \
                         {store}\n        \
                     }}))\n    \
                 }}));\n    \
                 __client\n}}",
            fields = fields.join(", "),
            nanos = micros.saturating_mul(1_000),
        );
        start_fn.replacen(&built, &registered, 1)
    }

    /// A measured serverlet's `start_X()`: its records, found or added by name, made before
    /// its actor — which records how late each call started and what failed — and handed to
    /// the client, which records how long each call took. `fields` are the client's other
    /// fields after `tx`, as the start function builds it.
    fn measure_start(&self, start_fn: String, name: &str, fields: &[&str]) -> String {
        if self.measured_handlers.is_empty() {
            return start_fn;
        }
        let channel = format!("let (tx, mut rx) = tokio::sync::mpsc::channel::<{}Msg>(100);", name);
        assert!(start_fn.contains(&channel), "serverlet '{name}': channel not found for its records");
        let units = self.measured_handlers.iter()
            .map(|handler| format!("crate::__orch_unit({:?}, \"handler\")", format!("{}.{}", name.replace("::", "."), handler)))
            .collect::<Vec<_>>()
            .join(", ");
        let built = format!("{}Client {{ tx, {} }}", name, fields.join(", "));
        assert!(start_fn.contains(&built), "serverlet '{name}': client construction not found for its records");
        start_fn
            .replacen(&channel, &format!("{channel}\n    let stats = std::sync::Arc::new([{units}]);\n    let __orch_stats = stats.clone();"), 1)
            .replacen(&built, &format!("{}Client {{ tx, {}, stats }}", name, fields.join(", ")), 1)
    }

    /// `plain_failure_reply`, counted as a failure when the handler is measured: every
    /// boundary builds its failed call's answer here, so every failure is counted once.
    /// `__orch_stats` is the serverlet's records, in scope in its client and its actor.
    pub(super) fn failure_reply(&self, h: &Handler, why: &str) -> String {
        let reply = plain_failure_reply(h, why);
        match self.measured_slot(h) {
            Some(slot) => format!("{{ __orch_stats[{slot}].failed(); {reply} }}"),
            None => reply,
        }
    }

    /// Where a measured handler's record is in its serverlet's `stats`, or `None`.
    pub(super) fn measured_slot(&self, h: &Handler) -> Option<usize> {
        self.measured_handlers.iter().position(|name| *name == h.name)
    }

    /// The return type a handler's closure is declared with, when inference needs it: a
    /// `result` handler's body can use `?`, which cannot infer the type it returns from.
    fn handler_return_annotation(&self, h: &Handler) -> String {
        if answers_failure(h) { format!(" -> {}", self.compile_type(&h.return_type)) } else { String::new() }
    }

    /// A serverlet handler's body: its final value is the reply, or, for a handler that
    /// returns nothing, discarded.
    fn compile_handler_body(&mut self, h: &Handler) -> String {
        if h.return_type == Type::Void {
            self.compile_statement_body(&h.body)
        } else {
            self.tail_moves = true;
            let body = self.compile_expr(&h.body);
            self.tail_moves = false;
            body
        }
    }

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
        let mut arms = Vec::new();
        for h in handlers {
            let bail = format!("{{ eprintln!(\"[orchestrate] {{}}\", __error); __guest.reset();{crash} let _ = reply_to.send({}); continue; }}", self.failure_reply(h, "__error"));
            let bail = bail.as_str();
            let variant = pascal_case(&h.name);
            let bindings = h
                .params
                .iter()
                .map(|p| rust_ident(&p.name))
                .chain(std::iter::once("reply_to".to_string()))
                .collect::<Vec<_>>()
                .join(", ");
            // A measured handler: how late its call reached the guest's turn.
            let (bindings, started) = match self.measured_slot(h) {
                Some(slot) => (format!("{bindings}, __sent"), format!("__orch_stats[{slot}].started_late(__sent.elapsed());\n                    ")),
                None => (bindings, String::new()),
            };

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
                            "                    let ({0}_pointer, {0}_length) = match __guest.write_struct(std::mem::size_of::<{3}>(), |__out| __orch_sandbox_write_{1}(&{2}, __out)) {{ Ok(__written) => __written, Err(__error) => {bail} }};\n",
                            p.name, sname, rust_ident(&p.name), rust_ident(sname)
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
            let answered = answered_type(&h.return_type);
            let results = match answered {
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
            let mut convert = match answered {
                Type::Bool => "                    let __value = __value != 0;\n".to_string(),
                Type::Str => format!("                    let __value = match __guest.read_string(__value) {{ Ok(__text) => __text, Err(__error) => {bail} }};\n"),
                Type::Array(inner, _) => format!(
                    "                    let __value = match __guest.read_array::<{}>(__value) {{ Ok(__items) => __items, Err(__error) => {bail} }};\n",
                    self.compile_type(inner)
                ),
                Type::Named(sname) => format!(
                    "                    let __value = match __guest.read_struct(__value, std::mem::size_of::<{1}>(), __orch_sandbox_read_{0}) {{ Ok(__struct) => __struct, Err(__error) => {bail} }};\n",
                    sname, rust_ident(sname)
                ),
                _ => String::new(),
            };
            // A `result` handler that answered `err` said so before returning.
            if answers_failure(h) {
                convert.push_str("                    let __value = match __guest.store.data_mut().answered_err.take() { Some(__why) => Err(__why), None => Ok(__value) };\n");
            }

            arms.push(format!(
                "                {name}Msg::{variant} {{ {bindings} }} => {{\n                    \
                     {started}{setup}                    __guest.begin();\n                    \
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
                started = started,
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
                 let actor = __OrchActor::spawn(true, async move {{\n        \
                     // A deterministic run does not enforce the timeout: it measures wall time.\n        \
                     let mut __guest = match crate::__OrchGuest::new({name:?}, include_bytes!(\"sandbox_{name}.wasm\"), Some({memory}), Some({timeout}).filter(|_| !__orch_deterministic()), {grants_arg}) {{\n            \
                         Ok(__guest) => __guest,\n            \
                         Err(__error) => {{ eprintln!(\"[orchestrate] {{}}\", __error); return; }}\n        \
                     }};\n        \
                     while let Some(msg) = rx.recv().await {{\n            \
                         match msg {{\n{arms}\n            }}\n        }}\n    \
                 }});\n    \
                 {name}Client {{ tx, actor }}\n}}",
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
            let value = self.compile_value(value);
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
            let body = self.compile_handler_body(h);
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
                            "    let {} = __orch_sandbox_read_{2}(__orch_bytes({1}_pointer, std::mem::size_of::<{3}>() as i32));\n",
                            rust_ident(&p.name), p.name, sname, rust_ident(sname)
                        ));
                    }
                    Type::Bool => {
                        params.push(format!("{}_flag: i32", p.name));
                        unpack.push_str(&format!("    let {} = {1}_flag != 0;\n", rust_ident(&p.name), p.name));
                    }
                    _ => params.push(format!("{}: {}", rust_ident(&p.name), self.compile_type(&p.ty))),
                }
            }
            let answered = answered_type(&h.return_type);
            let (returns, open, close) = match answered {
                Type::Void => (String::new(), String::new(), String::new()),
                Type::Str => (" -> i64".to_string(), "__orch_pack(".to_string(), ")".to_string()),
                Type::Array(_, _) => (" -> i64".to_string(), "__orch_pack_array(".to_string(), ")".to_string()),
                Type::Named(sname) => (
                    " -> i64".to_string(),
                    "{ let __value = ".to_string(),
                    format!("; __orch_pack_struct(std::mem::size_of::<{1}>(), |__out| __orch_sandbox_write_{0}(&__value, __out)) }}", sname, rust_ident(sname)),
                ),
                Type::Bool => (" -> i32".to_string(), "(".to_string(), ") as i32".to_string()),
                other => (format!(" -> {}", self.compile_type(other)), String::new(), String::new()),
            };
            // A `result<T>` handler returns its `T` as any handler would; an `err` is handed
            // to the host first, and a default stands in for the value it did not have.
            let run = if answers_failure(h) {
                format!(
                    "match __orch_state(|__state|{} {{ {} }}) {{ Ok(__answer) => __answer, Err(__why) => {{ __orch_answer_err(&__why); <{} as Default>::default() }} }}",
                    self.handler_return_annotation(h), body, self.compile_type(answered)
                )
            } else {
                format!("__orch_state(|__state| {{ {} }})", body)
            };
            exports.push(format!(
                "#[unsafe(export_name = {export:?})]\npub extern \"C\" fn {hname}({params}){returns} {{\n{unpack}    {open}{run}{close}\n}}",
                export = h.name,
                hname = rust_ident(&h.name),
                params = params.join(", "),
                returns = returns,
                unpack = unpack,
                open = open,
                run = run,
                close = close,
            ));
        }

        self.handler_state.clear();
        self.sandbox_guest_grants = None;
        // The one import a `result` handler needs, and not a capability: it tells the host
        // about the call it is answering, and reaches nothing.
        let answer_err = if handlers.iter().any(answers_failure) {
            "#[link(wasm_import_module = \"orch_guest\")]\nunsafe extern \"C\" {\n    #[link_name = \"answer_err\"]\n    fn __orch_import_answer_err(pointer: i32, length: i32);\n}\n\
             fn __orch_answer_err(why: &str) {\n    unsafe { __orch_import_answer_err(why.as_ptr() as i32, why.len() as i32) }\n}\n\n"
        } else {
            ""
        };

        // The file's structs and enums, so a handler body or the state can use them; the
        // codecs, for the ones that can cross.
        let structs = self.standalone_type_definitions() + &sandbox_struct_codecs(&self.struct_defs);

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
{marshal}\n{grants_code}{answer_err}{exports}\n",
            name = name,
            preamble = runtime_preamble(true, false, false),
            pod = super::core::WASM_POD,
            structs = structs,
            fields = fields.join("\n"),
            initializers = initializers.join("\n"),
            names = names.iter().map(|n| rust_ident(n)).collect::<Vec<_>>().join(", "),
            marshal = SANDBOX_GUEST_MARSHAL,
            grants_code = grants_code,
            answer_err = answer_err,
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

/// Returns Some(reason) if a function uses a type the wire codec can't carry: int, float,
/// bool, string, arrays of carryable types, and structs declared in the same file whose
/// fields are carryable. Returns may also be void. A host function or an FFI function is
/// held to this; a serverlet handler to `handler_wire_unsupported_reason`.
pub(crate) fn wire_unsupported_reason(handlers: &[Handler], structs: &StructDefs) -> Option<String> {
    if let Some(h) = handlers.iter().find(|h| answers_failure(h)) {
        return Some(format!(
            "'{}' returns {}, which does not cross this boundary: only a serverlet handler can answer with a result, since only its caller is told when the call fails",
            h.name, h.return_type.display_name()
        ));
    }
    handler_wire_unsupported_reason(handlers, structs)
}

/// `wire_unsupported_reason` for a serverlet's handlers, which may also return `result<T>`
/// of a carryable `T`.
pub(crate) fn handler_wire_unsupported_reason(handlers: &[Handler], structs: &StructDefs) -> Option<String> {
    for h in handlers {
        for p in &h.params {
            if let Type::Client(_) = p.ty {
                return Some(format!(
                    "handler '{}' parameter '{}' is a serverlet client, which is the address of an instance in this process and means nothing in another; pass what the handler needs from it instead",
                    h.name, p.name
                ));
            }
            if !wire_supported(&p.ty, structs, 0) {
                return Some(format!(
                    "handler '{}' parameter '{}' uses an unsupported type; secret serverlets support int, float, bool, string, arrays, maps, and structs declared in the same file",
                    h.name, p.name
                ));
            }
        }
        if h.return_type != Type::Void && !wire_supported(answered_type(&h.return_type), structs, 0) {
            return Some(format!(
                "handler '{}' uses an unsupported return type; secret serverlets support int, float, bool, string, arrays, maps, structs declared in the same file, result<T> of any of those, and void",
                h.name
            ));
        }
    }
    None
}

/// What a handler answers with when it answers: `T` for a `result<T>` handler, whose
/// failure crosses as an error rather than as a value, and the return type otherwise.
pub(crate) fn answered_type(ty: &Type) -> &Type {
    match ty {
        Type::Result(ok, error) if **error == Type::Str => ok,
        other => other,
    }
}

/// Whether a type has a default value: what a failed call to a handler that cannot answer
/// `err` answers with, and what a struct needs of every field to have one itself. An enum
/// has none — no variant is a neutral answer — and neither has a `result` or a function.
pub(crate) fn has_default(ty: &Type, structs: &StructDefs) -> bool {
    fn go(ty: &Type, structs: &StructDefs, depth: usize) -> bool {
        match ty {
            Type::Int | Type::Float | Type::Str | Type::Bool | Type::Void | Type::Array(_, _) | Type::Option(_) | Type::Map(_, _) => true,
            Type::Named(name) if depth <= 32 => structs.iter()
                .find(|(n, _)| n == name)
                .is_some_and(|(_, fields)| fields.iter().all(|(_, field)| go(field, structs, depth + 1))),
            _ => false,
        }
    }
    go(ty, structs, 0)
}

/// Whether `==` can compare two values of a type: numbers, strings, and booleans, and
/// anything built only of them — arrays, options, results, structs, and enums, which then
/// derive it. A client, a process, a function, or a handle has no equality: two of them
/// are the same only if they are the same thing, which the language has no way to ask.
/// `enums` lists each enum's payload types, as `structs` lists each struct's fields.
pub(crate) fn has_equality(ty: &Type, structs: &StructDefs, enums: &StructDefs) -> bool {
    fn go(ty: &Type, structs: &StructDefs, enums: &StructDefs, depth: usize) -> bool {
        match ty {
            Type::Int | Type::Float | Type::Str | Type::Bool | Type::Void | Type::TypeParam(_) => true,
            Type::Array(inner, _) | Type::Option(inner) => go(inner, structs, enums, depth + 1),
            Type::Result(ok, error) | Type::Map(ok, error) => go(ok, structs, enums, depth + 1) && go(error, structs, enums, depth + 1),
            Type::Named(name) if depth <= 32 => structs.iter().chain(enums.iter())
                .find(|(n, _)| n == name)
                .is_some_and(|(_, fields)| fields.iter().all(|(_, field)| go(field, structs, enums, depth + 1))),
            _ => false,
        }
    }
    go(ty, structs, enums, 0)
}

/// Whether `<` and its kin can order two values of a type: numbers, strings, booleans, and
/// arrays, options, and results of them. A struct or an enum has no order of its own.
pub(crate) fn has_order(ty: &Type) -> bool {
    match ty {
        Type::Int | Type::Float | Type::Str | Type::Bool | Type::TypeParam(_) => true,
        Type::Array(inner, _) | Type::Option(inner) => has_order(inner),
        Type::Result(ok, error) => has_order(ok) && has_order(error),
        _ => false,
    }
}

/// What a `shared let` may hold, in the words its diagnostic uses.
pub(crate) const SHAREABLE: &str = "A shared binding holds data — int, float, bool, string, an array, a map, an option, or a struct — or serverlet clients";

/// Whether a `shared let` can hold a type: data, and clients — a statement copies a client
/// out under the lock, and its call waits outside it. A process or a closure cannot. A
/// handle can: every statement that uses it holds the lock, so its native object is used by
/// one statement at a time. The one rule `check` and the build both apply.
pub(crate) fn shareable(ty: &Type) -> bool {
    match ty {
        Type::Int | Type::Float | Type::Bool | Type::Str | Type::Named(_) | Type::Handle | Type::Client(_) => true,
        Type::Array(inner, _) | Type::Option(inner) => shareable(inner),
        Type::Map(_, value) => shareable(value),
        _ => false,
    }
}

/// Whether `print`, `to_string`, and `{...}` in a string can show a value of a type.
pub(crate) fn has_display(ty: &Type) -> bool {
    matches!(ty, Type::Int | Type::Float | Type::Str | Type::Bool | Type::TypeParam(_))
}

/// Whether a type means the same in a crate of its own as in the program: data, not a
/// client, a process, a function, or a handle.
fn stands_alone(ty: &Type, structs: &StructDefs, enums: &StructDefs) -> bool {
    fn go(ty: &Type, structs: &StructDefs, enums: &StructDefs, depth: usize) -> bool {
        match ty {
            Type::Int | Type::Float | Type::Str | Type::Bool | Type::Void => true,
            Type::Array(inner, _) | Type::Option(inner) => go(inner, structs, enums, depth + 1),
            Type::Result(a, b) | Type::Map(a, b) => go(a, structs, enums, depth + 1) && go(b, structs, enums, depth + 1),
            Type::Named(name) if depth <= 32 => structs.iter().chain(enums.iter())
                .find(|(n, _)| n == name)
                .is_some_and(|(_, fields)| fields.iter().all(|(_, field)| go(field, structs, enums, depth + 1))),
            _ => false,
        }
    }
    go(ty, structs, enums, 0)
}

/// A struct's derives: `Default` when every field has a default, and `PartialEq` when
/// every field can be compared.
fn struct_derives(name: &str, structs: &StructDefs, enums: &StructDefs) -> String {
    let ty = Type::Named(name.to_string());
    let mut derives = vec!["Clone", "Debug"];
    if has_default(&ty, structs) { derives.push("Default"); }
    if has_equality(&ty, structs, enums) { derives.push("PartialEq"); }
    derives.join(", ")
}

/// Whether a failed call to this handler answers `err` rather than the type's default.
pub(crate) fn answers_failure(h: &Handler) -> bool {
    matches!(&h.return_type, Type::Result(_, error) if **error == Type::Str)
}

/// The reply a handler gets when its call failed: `err(why)` for a `result<T>` handler,
/// and the return type's default for any other. `why` is a Rust `String` expression.
pub(crate) fn plain_failure_reply(h: &Handler, why: &str) -> String {
    if answers_failure(h) { format!("Err({})", why) } else { "Default::default()".to_string() }
}

fn wire_supported(ty: &Type, structs: &StructDefs, depth: usize) -> bool {
    if depth > 32 {
        return false; // recursive struct definitions can't be encoded
    }
    match ty {
        Type::Int | Type::Float | Type::Str | Type::Bool => true,
        Type::Array(inner, _) => wire_supported(inner, structs, depth + 1),
        Type::Map(key, value) => matches!(**key, Type::Int | Type::Str | Type::Bool) && wire_supported(value, structs, depth + 1),
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
                name = rust_ident(name), enc = enc, dec = dec
            )
        })
        .collect()
}

/// What crosses a sandbox boundary, in the words the diagnostics use.
const SANDBOX_TYPES: &str = "a sandboxed serverlet carries int, float, bool, string, int[], float[], bool[], and structs whose fields are those numbers, booleans, or such structs, and a handler may return result<T> of any of them";

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
        if h.return_type != Type::Void && !sandbox_type_supported(answered_type(&h.return_type), structs) {
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
            // The type in type position is a Rust identifier and may need escaping; the
            // codec functions' names are built from the plain name, which is always valid.
            let ty = rust_ident(name);
            let writes = fields.iter().map(|(f, field_ty)| { let f = rust_ident(f); match field_ty {
                Type::Int | Type::Float => format!("    out[std::mem::offset_of!({ty}, {f})..][..8].copy_from_slice(&value.{f}.to_le_bytes());"),
                Type::Bool => format!("    out[std::mem::offset_of!({ty}, {f})] = value.{f} as u8;"),
                Type::Named(inner) => format!("    __orch_sandbox_write_{inner}(&value.{f}, &mut out[std::mem::offset_of!({ty}, {f})..][..std::mem::size_of::<{}>()]);", rust_ident(inner)),
                _ => String::new(),
            } }).collect::<Vec<_>>().join("\n");
            let reads = fields.iter().map(|(f, field_ty)| { let f = rust_ident(f); match field_ty {
                Type::Int => format!("        {f}: i64::from_le_bytes(bytes[std::mem::offset_of!({ty}, {f})..][..8].try_into().unwrap()),"),
                Type::Float => format!("        {f}: f64::from_le_bytes(bytes[std::mem::offset_of!({ty}, {f})..][..8].try_into().unwrap()),"),
                Type::Bool => format!("        {f}: bytes[std::mem::offset_of!({ty}, {f})] != 0,"),
                Type::Named(inner) => format!("        {f}: __orch_sandbox_read_{inner}(&bytes[std::mem::offset_of!({ty}, {f})..][..std::mem::size_of::<{}>()]),", rust_ident(inner)),
                _ => String::new(),
            } }).collect::<Vec<_>>().join("\n");
            format!(
                "#[allow(non_snake_case)]\nfn __orch_sandbox_write_{name}(value: &{ty}, out: &mut [u8]) {{\n{writes}\n}}\n#[allow(non_snake_case)]\nfn __orch_sandbox_read_{name}(bytes: &[u8]) -> {ty} {{\n    {ty} {{\n{reads}\n    }}\n}}\n"
            )
        })
        .collect()
}
