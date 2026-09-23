use super::core::{Codegen, StateRewrite, rust_ident};
use crate::ast::{Expr, ExprNode, MatchPattern, Stmt, StmtNode, StringPart, Type};
use std::collections::HashSet;

/// One field of the program struct: a top-level `let`, in declaration order.
pub(super) struct StateField {
    pub(super) name: String,
    rust_type: String,
    /// A closure has no nameable type, so its field boxes it.
    pub(super) boxed: bool,
    /// No type could be derived; emitted as a compile_error! beside the struct.
    error: Option<String>,
}

impl Codegen {
    pub(super) fn host_type(&self, ty: &Type) -> String {
        match ty {
            Type::Named(name) => format!("crate::{}", rust_ident(name)),
            Type::Array(inner, _) => format!("Vec<{}>", self.host_type(inner)),
            Type::Map(key, value) => format!("crate::OrchMap<{}, {}>", self.host_type(key), self.host_type(value)),
            _ => self.compile_type(ty),
        }
    }

    pub fn host_method(group: &str, handler: &str) -> String {
        format!("{}_{}", group, handler)
    }

    fn tick_types(&self) -> (Option<String>, String) {
        for stmt in &self.local_stmts {
            if let StmtNode::OnTick { input, return_type, .. } = &stmt.node {
                if input.is_some() || *return_type != Type::Void {
                    return (input.as_ref().map(|p| self.compile_type(&p.ty)), self.compile_type(return_type));
                }
            }
        }
        (None, "()".into())
    }

    fn client_type(serverlet: &str) -> String {
        match serverlet.rsplit_once("::") {
            Some((module, name)) => format!("{module}::{name}Client"),
            None => format!("{serverlet}Client"),
        }
    }

    /// Every name an expression uses — identifiers, call targets, and serverlet receivers —
    /// regardless of shadowing, which over-approximates safely.
    fn hook_names(expr: &Expr, names: &mut HashSet<String>) {
        match &expr.node {
            ExprNode::Identifier(name) => { names.insert(name.clone()); }
            ExprNode::Literal(_) | ExprNode::NoneLiteral => {}
            ExprNode::Unary { operand, .. } => Self::hook_names(operand, names),
            ExprNode::Binary { lhs, rhs, .. } => { Self::hook_names(lhs, names); Self::hook_names(rhs, names); }
            ExprNode::Call { callee, args } => {
                names.insert(callee.clone());
                for arg in args { Self::hook_names(arg, names); }
            }
            ExprNode::Pipeline { value, function } => { Self::hook_names(value, names); Self::hook_names(function, names); }
            ExprNode::Block(stmts) => for stmt in stmts { Self::hook_names_stmt(stmt, names); },
            ExprNode::If { cond, then_branch, else_branch } => {
                Self::hook_names(cond, names);
                Self::hook_names(then_branch, names);
                if let Some(branch) = else_branch { Self::hook_names(branch, names); }
            }
            ExprNode::ModuleCall { module_local_name, args, .. } => {
                names.insert(module_local_name.clone());
                for arg in args { Self::hook_names(arg, names); }
            }
            ExprNode::MethodCall { receiver, args, .. } => {
                Self::hook_names(receiver, names);
                for arg in args { Self::hook_names(arg, names); }
            }
            ExprNode::StartServerlet { args, .. } => for arg in args { Self::hook_names(arg, names); },
            ExprNode::AutomaticBlock { body, crash_handler, .. } => {
                Self::hook_names(body, names);
                if let Some((_, handler)) = crash_handler { Self::hook_names(handler, names); }
            }
            ExprNode::TriggeredBlock { body, .. } | ExprNode::Closure { body, .. } => Self::hook_names(body, names),
            ExprNode::StartProcess { target } => Self::hook_names(target, names),
            ExprNode::ArrayLiteral(items) => for item in items { Self::hook_names(item, names); },
            ExprNode::StructLiteral { fields, .. } => for (_, value) in fields { Self::hook_names(value, names); },
            ExprNode::FieldAccess { object, .. } | ExprNode::SomeLiteral(object) | ExprNode::OkLiteral(object)
            | ExprNode::ErrLiteral(object) | ExprNode::Propagate(object) => Self::hook_names(object, names),
            ExprNode::Index { object, index, .. } => { Self::hook_names(object, names); Self::hook_names(index, names); }
            ExprNode::MapLiteral(entries) => for (key, value) in entries { Self::hook_names(key, names); Self::hook_names(value, names); },
            ExprNode::TryCatch { body, handler, .. } => { Self::hook_names(body, names); Self::hook_names(handler, names); }
            ExprNode::EnumVariantLiteral { payload, .. } => if let Some(payload) = payload { Self::hook_names(payload, names); },
            ExprNode::Match { value, arms } => {
                Self::hook_names(value, names);
                for arm in arms {
                    if let MatchPattern::Guard { condition, .. } = &arm.pattern { Self::hook_names(condition, names); }
                    Self::hook_names(&arm.body, names);
                }
            }
            ExprNode::StringInterp { parts } => for part in parts {
                if let StringPart::Expr(expr) = part { Self::hook_names(expr, names); }
            },
        }
    }

    fn hook_names_stmt(stmt: &Stmt, names: &mut HashSet<String>) {
        match &stmt.node {
            StmtNode::Let { value, .. } | StmtNode::Expr(value) => Self::hook_names(value, names),
            StmtNode::Return(Some(value)) => Self::hook_names(value, names),
            StmtNode::Trigger { args, .. } => for arg in args { Self::hook_names(arg, names); },
            StmtNode::While { cond, body } => { Self::hook_names(cond, names); Self::hook_names(body, names); }
            StmtNode::ForIn { iter, body, .. } => { Self::hook_names(iter, names); Self::hook_names(body, names); }
            StmtNode::Parallel(stmts) => for stmt in stmts { Self::hook_names_stmt(stmt, names); },
            _ => {}
        }
    }

    /// The program struct's fields: the top-level `let`s a hook body can reach. The rest
    /// stay locals of the entry task exactly as before, so a binding consumed during
    /// startup — moved into the process list, say — is unaffected. A name declared twice
    /// keeps its first position and takes the type of its last declaration, which is the
    /// binding that gets packed.
    pub(super) fn state_fields(&self) -> Vec<StateField> {
        let mut referenced = HashSet::new();
        for stmt in self.local_stmts.iter().chain(self.body_hooks.iter()) {
            match &stmt.node {
                StmtNode::OnTick { body, .. } | StmtNode::OnFixedTick { body, .. } | StmtNode::OnStop(body) => {
                    Self::hook_names(body, &mut referenced)
                }
                // A top-level event handler runs inside a tick and reaches state the
                // same way a tick does.
                StmtNode::Expr(Expr { node: ExprNode::TriggeredBlock { body, .. }, .. })
                | StmtNode::Let { value: Expr { node: ExprNode::TriggeredBlock { body, .. }, .. }, .. } => {
                    Self::hook_names(body, &mut referenced)
                }
                _ => {}
            }
        }
        let mut fields: Vec<StateField> = Vec::new();
        for stmt in &self.local_stmts {
            let StmtNode::Let { name, ty, value, shared } = &stmt.node else { continue };
            if *shared || !referenced.contains(name) { continue; }
            let (rust_type, boxed, error) = match &value.node {
                ExprNode::StartServerlet { name: serverlet, .. } => (Self::client_type(serverlet), false, None),
                ExprNode::AutomaticBlock { .. } | ExprNode::TriggeredBlock { .. } => ("ProcessRef".to_string(), false, None),
                ExprNode::Identifier(other) if fields.iter().any(|f| f.name == *other) => {
                    let alias = fields.iter().find(|f| f.name == *other).unwrap();
                    (alias.rust_type.clone(), alias.boxed, None)
                }
                _ => match ty.clone().or_else(|| self.state_types.get(name).cloned()) {
                    Some(Type::Fn(params, ret)) => (
                        format!(
                            "Box<dyn Fn({}) -> {} + Send>",
                            params.iter().map(|t| self.compile_type(t)).collect::<Vec<_>>().join(", "),
                            self.compile_type(&ret)
                        ),
                        true,
                        None,
                    ),
                    Some(Type::TypeParam(_)) | None => (
                        "()".to_string(),
                        false,
                        Some(format!("library state '{name}' has no type the compiler can name; add a type annotation")),
                    ),
                    Some(other) => (self.compile_type(&other), false, None),
                },
            };
            if let Some(existing) = fields.iter_mut().find(|f| f.name == *name) {
                existing.rust_type = rust_type;
                existing.boxed = boxed;
                existing.error = error;
            } else {
                fields.push(StateField { name: name.clone(), rust_type, boxed, error });
            }
        }
        fields
    }

    pub(super) fn library_runtime(&self) -> String {
        let methods = self
            .host_functions
            .iter()
            .map(|(group, handler)| {
                let params = handler
                    .params
                    .iter()
                    .map(|p| format!("{}: {}", rust_ident(&p.name), self.compile_type(&p.ty)))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!(
                    "    fn {}(&self{}{}) -> Result<{}, String>;",
                    Self::host_method(group, &handler.name),
                    if params.is_empty() { "" } else { ", " },
                    params,
                    self.compile_type(&handler.return_type)
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let mut events = self.events.iter().collect::<Vec<_>>();
        events.sort_by_key(|(name, _)| *name);
        let mut fields = String::new();
        let mut init = String::new();
        let mut triggers = String::new();
        for (name, types) in events {
            let ty = if types.is_empty() {
                "()".into()
            } else if types.len() == 1 {
                self.compile_type(&types[0])
            } else {
                format!(
                    "({})",
                    types
                        .iter()
                        .map(|t| self.compile_type(t))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            };
            if name != "update_orchestrator" {
                let params = types.iter().enumerate().map(|(i,t)| format!("arg{}: {}", i, self.compile_type(t))).collect::<Vec<_>>().join(", ");
                let args = (0..types.len()).map(|i| format!("arg{}", i)).collect::<Vec<_>>().join(", ");
                let value = if types.len() == 1 { args } else { format!("({})", args) };
                triggers.push_str(&format!("pub fn trigger_{name}(&self, {params}) -> Result<(), String> {{ if *self.context.shutdown.borrow() {{ return Err(\"library is stopped\".into()); }} let context = self.context.clone(); let future: OrchEvent = Box::pin(async move {{ let handlers = context.event_{name}.lock().unwrap().clone(); let value = std::sync::Arc::new({value}); for handler in handlers {{ context.batch.lock().unwrap().push_back(handler(value.clone())); }} }}); self.context.queue_event(future); Ok(()) }}\n"));
                fields.push_str(&format!("    event_{}: std::sync::Arc<std::sync::Mutex<Vec<OrchEventHandler<{}>>>>,\n", name, ty));
            } else {
                fields.push_str(&format!("    event_{}: std::sync::Arc<std::sync::Mutex<Vec<tokio::sync::mpsc::Sender<std::sync::Arc<{}>>>>>,\n", name, ty));
            }
            init.push_str(&format!("    event_{}: Default::default(),\n", name));
        }
        let (input, output) = self.tick_types();
        let input_param = input.as_ref().map(|ty| format!(", input: {}", ty)).unwrap_or_default();
        let input_arg = if input.is_some() { ", input" } else { "" };
        let input_field = input.as_ref().map(|ty| format!(", {}", ty)).unwrap_or_default();
        // Shared state is per instance, so it lives on the context rather than in a
        // static: two libraries started in one process do not share it.
        let (mut shared_field, mut shared_method, mut shared_init) = if self.shared_fields.is_empty() {
            (String::new(), String::new(), String::new())
        } else {
            (
                "    shared: std::sync::OnceLock<std::sync::Mutex<__OrchShared>>,".to_string(),
                "    /// This instance's `shared let` bindings, created the first time they\n    /// are reached.\n    fn shared_state(&self) -> &std::sync::Mutex<__OrchShared> {\n        self.shared.get_or_init(|| std::sync::Mutex::new(__OrchShared::new()))\n    }".to_string(),
                "        shared: std::sync::OnceLock::new(),".to_string(),
            )
        };
        // Queues are per instance too, each with its own lock.
        if !self.queues.is_empty() {
            shared_field.push_str("\n    queues: std::sync::OnceLock<std::sync::Arc<__OrchQueues>>,");
            shared_method.push_str("\n    /// This instance's `shared queue`s, created the first time one is reached.\n    fn queues(&self) -> std::sync::Arc<__OrchQueues> {\n        self.queues.get_or_init(|| std::sync::Arc::new(__OrchQueues::new())).clone()\n    }");
            shared_init.push_str("\n        queues: std::sync::OnceLock::new(),");
        }
        let mut runtime = include_str!("library_runtime.rs.txt")
            .replace("@SHARED_FIELD@", &shared_field)
            .replace("@SHARED_METHOD@", &shared_method)
            .replace("@SHARED_INIT@", &shared_init)
            .replace("@NEEDS_IO_DRIVER@", if self.io_driver_users.is_empty() { "false" } else { "true" })
            .replace("@IO_DRIVER_USERS@", &format!("{:?}", self.io_driver_users.join(", ")))
            .replace("@HOST_METHODS@", &methods)
            .replace("@EVENT_FIELDS@", &fields)
            .replace("@EVENT_INIT@", &init)
            .replace("@TICK_PARAM@", &input_param).replace("@TICK_ARG@", input_arg)
            .replace("@TICK_FIELD@", &input_field).replace("@TICK_OUTPUT@", &output);
        runtime.push_str(&format!("\nimpl Scripts {{ {} }}\n", triggers));
        runtime.push_str(super::core::MEASURE_RUNTIME);
        if self.host_functions.is_empty() {
            runtime.push_str("\nimpl Host for () {}\n");
        }
        runtime
    }

    /// `main_future` is the orchestrator body as a future that owns what it names; it is
    /// awaited in place in deterministic mode and spawned beside the ticks otherwise.
    pub(super) fn library_entry(
        &mut self,
        main_future: &str,
        declarations: &str,
        execution: &str,
    ) -> String {
        let fields = self.state_fields();
        let rewrite_fields: std::collections::BTreeMap<String, bool> =
            fields.iter().map(|f| (f.name.clone(), f.boxed)).collect();
        let mut start = String::new();
        let mut stop = String::new();
        let mut tick = String::new();
        let mut fixed = String::new();
        // Every hook body runs with `__context` and `__host` bound once, in scope.
        self.context_bound = true;
        let hooks = self.local_stmts.iter().chain(self.body_hooks.iter()).cloned().collect::<Vec<_>>();
        // Each cadenced hook's own count of host time, kept with the instance's state.
        let mut cadences = 0usize;
        // The measured hooks' records, by name, kept with the instance's state too.
        let mut hook_units: Vec<String> = Vec::new();
        let (mut tick_measured, mut fixed_measured) = (false, false);
        // `measure hooks`: a hook's record, unless it is `unmeasured` — how long the tick ran
        // before it started, how long it took, and its CPU time if it runs without waiting.
        let measured = |units: &mut Vec<String>, name: String, waits: bool, inner: String| -> String {
            let slot = units.len();
            units.push(name);
            let unit = format!("__program.__hooks[{slot}]");
            let (cpu, used) = if waits { (String::new(), String::new()) } else {
                ("let __cpu = crate::__orch_thread_cpu(); ".to_string(), format!(" {unit}.used(__cpu);"))
            };
            format!("{{ let __hook = std::time::Instant::now(); {unit}.started_late(__hook.saturating_duration_since(__tick_began)); {cpu}{inner} {unit}.called(__hook.elapsed());{used} }}\n")
        };
        let mut cadenced = |every: Option<crate::ast::Cadence>, compiled: String, param: &str, measure: Option<(&mut Vec<String>, String, bool)>| -> String {
            let (inner, binding) = match every {
                None => (None, "__dt".to_string()),
                Some(every) => {
                    let index = cadences;
                    cadences += 1;
                    let due = match every {
                        crate::ast::Cadence::Micros(micros) => format!("every_nanos(__dt, {}u64)", micros.saturating_mul(1_000)),
                        crate::ast::Cadence::Ticks(count) => format!("every_ticks(__dt, {}u64)", count),
                    };
                    (Some(format!("__program.__cadence[{index}].{due}")), "__since".to_string())
                }
            };
            let body = format!("{{ let {} = {}; {}; }}", rust_ident(param), binding, compiled);
            let body = match measure {
                Some((units, name, waits)) => measured(units, name, waits, body),
                None => format!("{body}\n"),
            };
            match inner {
                Some(due) => format!("if let Some(__since) = {due} {body}"),
                None => body,
            }
        };
        // A hook's name in the records: `on_tick`, `on_tick every 100ms`, and `#2` on from
        // the second of the same name.
        let mut names_used: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut hook_name = |base: &str, every: Option<crate::ast::Cadence>| -> String {
            let name = match every {
                None => base.to_string(),
                Some(crate::ast::Cadence::Micros(micros)) => format!("{base} every {}", crate::ast::display_micros(micros)),
                Some(crate::ast::Cadence::Ticks(count)) => format!("{base} every {count} ticks"),
            };
            let seen = names_used.entry(name.clone()).or_insert(0);
            *seen += 1;
            if *seen == 1 { name } else { format!("{name} #{seen}") }
        };
        for stmt in hooks {
            match stmt.node {
                StmtNode::OnStart(body) => {
                    start.push_str(&format!("{};\n", self.compile_expr(&body)))
                }
                StmtNode::OnStop(body) => {
                    stop.push_str(&format!("{};\n", self.compile_expr(&body)))
                }
                StmtNode::OnFixedTick { param, every, unmeasured, body } => {
                    // Hook bodies run after the bindings are packed, so they read the struct.
                    let mut scope = std::collections::HashSet::new();
                    scope.insert(param.clone());
                    self.state_rewrite = Some(StateRewrite { fields: rewrite_fields.clone(), scopes: vec![scope], receiver: "__program" });
                    let compiled = self.compile_expr(&body);
                    self.state_rewrite = None;
                    let measure = (self.measure.hooks && !unmeasured).then(|| {
                        fixed_measured = true;
                        (&mut hook_units, hook_name("on_fixed_tick", every), self.may_wait(&body))
                    });
                    fixed.push_str(&cadenced(every, compiled, &param, measure));
                }
                StmtNode::OnTick { param, input, return_type, every, unmeasured, body } => {
                    let bind = input.as_ref().map(|p| format!("let {} = __input;", rust_ident(&p.name))).unwrap_or_default();
                    let mut scope = std::collections::HashSet::new();
                    scope.insert(param.clone());
                    if let Some(p) = &input { scope.insert(p.name.clone()); }
                    self.state_rewrite = Some(StateRewrite { fields: rewrite_fields.clone(), scopes: vec![scope], receiver: "__program" });
                    let compiled = self.compile_expr(&body);
                    self.state_rewrite = None;
                    let waits = self.may_wait(&body);
                    let measured_here = self.measure.hooks && !unmeasured;
                    tick_measured |= measured_here;
                    if input.is_some() || return_type != Type::Void {
                        // The one typed hook: its value is the tick's output, kept across the
                        // record when it is measured.
                        let body = format!("let {} = __dt; {} {}", rust_ident(&param), bind, compiled);
                        tick = if measured_here {
                            // In a block of its own, so a `return` in it — the tick's output — is
                            // recorded too.
                            let record = measured(&mut hook_units, hook_name("on_tick", None), waits, "let __out = async { BODY }.await;".to_string());
                            let record = record.trim_end();
                            format!("{} __out }}", record[..record.len() - 1].replacen("BODY", &body, 1))
                        } else { body };
                    } else {
                        let measure = measured_here.then(|| (&mut hook_units, hook_name("on_tick", every), waits));
                        tick.push_str(&cadenced(every, compiled, &param, measure));
                    }
                },
                _ => {}
            }
        }
        self.context_bound = false;
        let (input, output) = self.tick_types();
        let input_param = input.as_ref().map(|ty| format!(", __input: {ty}")).unwrap_or_default();
        let input_arg = if input.is_some() { ", __input" } else { "" };
        let errors = fields.iter().filter_map(|f| f.error.as_ref().map(|e| format!("compile_error!({e:?});\n"))).collect::<String>();
        // The cadenced hooks' counters ride with the bindings, so an instance has its own.
        let cadence_field = (cadences > 0).then(|| format!("__cadence: [__OrchCadence; {cadences}]"));
        let hooks_field = (!hook_units.is_empty()).then(|| format!("__hooks: [std::sync::Arc<crate::__OrchUnit>; {}]", hook_units.len()));
        let members = fields.iter().map(|f| format!("    {}: {},", rust_ident(&f.name), f.rust_type))
            .chain(cadence_field.iter().chain(hooks_field.iter()).map(|c| format!("    {c},")))
            .collect::<Vec<_>>();
        let program_struct = if members.is_empty() {
            "struct __OrchProgram {}".to_string()
        } else {
            format!("struct __OrchProgram {{\n{}\n}}", members.join("\n"))
        };
        let literal = |items: Vec<String>| if items.is_empty() { "__OrchProgram {}".to_string() } else { format!("__OrchProgram {{ {} }}", items.join(", ")) };
        let pack = literal(
            fields.iter().map(|f| if f.boxed { format!("{0}: Box::new({0})", rust_ident(&f.name)) } else { rust_ident(&f.name) })
                .chain((cadences > 0).then(|| format!("__cadence: [__OrchCadence::NEW; {cadences}]")))
                .chain((!hook_units.is_empty()).then(|| format!(
                    "__hooks: [{}]",
                    hook_units.iter().map(|name| format!("crate::__orch_unit({name:?}, \"hook\")")).collect::<Vec<_>>().join(", ")
                )))
                .collect()
        );
        let unpack = literal(
            fields.iter().map(|f| format!("mut {}", rust_ident(&f.name)))
                .chain((cadences > 0).then(|| "__cadence: _".to_string()))
                .chain((!hook_units.is_empty()).then(|| "__hooks: _".to_string()))
                .collect()
        );
        let cadenced = if self.has_cadenced { "__context.run_cadenced(__dt).await;\n    " } else { "" };
        // When the tick started, for how late each measured hook started within it.
        let tick_clock = if tick_measured { "let __tick_began = std::time::Instant::now();\n    " } else { "" };
        let fixed_clock = if fixed_measured { "let __tick_began = std::time::Instant::now();\n    " } else { "" };
        format!(
            r#"{errors}/// The top-level bindings the hooks reach, owned by the instance rather than by a task.
{program_struct}
async fn __orch_run_tick(__context: &OrchContext, __program: &mut __OrchProgram, __dt: f64{input_param}) -> {output} {{
    {tick_clock}__context.advance_time(__dt);
    {cadenced}if __context.events_pending() {{ __context.drain_events_with(__program).await; }}
    let __host = &*__context.host;
    let __output = async {{ {tick} }}.await;
    if __context.events_pending() {{ __context.drain_events_with(__program).await; }}
    __output
}}
async fn __orch_run_fixed_tick(__context: &OrchContext, __program: &mut __OrchProgram, __dt: f64) {{
    {fixed_clock}if __context.events_pending() {{ __context.drain_events_with(__program).await; }}
    let __host = &*__context.host;
    {fixed}
    if __context.events_pending() {{ __context.drain_events_with(__program).await; }}
}}
async fn __orch_entry(mut commands: tokio::sync::mpsc::UnboundedReceiver<OrchCommand>, ready: tokio::sync::oneshot::Sender<()>) {{
    {declarations}
    let __context = crate::__orch_context();
    let __host = &*__context.host;
    let mut __shutdown = __context.shutdown.subscribe();
    if !*__shutdown.borrow() {{
        tokio::select! {{ biased; _ = __shutdown.changed() => {{}}, _ = async {{ {start} }} => {{}} }}
    }}
    {execution}
    let __main_future = {main_future};
    let __main = if __context.options.deterministic {{ __main_future.await; None }} else {{ Some(tokio::spawn(__main_future)) }};
    *__context.program.enter().await = Some({pack});
    let _ = ready.send(());
    while !*__shutdown.borrow() {{
        let command = tokio::select! {{ biased; _ = __shutdown.changed() => break, command = commands.recv() => command }};
        let Some(command) = command else {{ break; }};
        match command {{
            OrchCommand::Tick(__dt{input_arg}, reply) => {{
                let mut __state = __context.program.enter().await;
                let __program = __state.as_mut().expect("program state");
                __context.frame.store(__context.frame.load(std::sync::atomic::Ordering::Relaxed).wrapping_add(1), std::sync::atomic::Ordering::Relaxed);
                __context.dt_bits.store(__dt.to_bits(), std::sync::atomic::Ordering::Relaxed);
                tokio::select! {{ biased; _ = __shutdown.changed() => break, __output = __orch_run_tick(&__context, __program, __dt{input_arg}) => {{ let _ = reply.send(__output); }} }}
            }},
            OrchCommand::FixedTick(__dt, reply) => {{
                let mut __state = __context.program.enter().await;
                let __program = __state.as_mut().expect("program state");
                tokio::select! {{ biased; _ = __shutdown.changed() => break, _ = __orch_run_fixed_tick(&__context, __program, __dt) => {{ let _ = reply.send(()); }} }}
            }},
            OrchCommand::Events(reply) => {{
                let mut __state = __context.program.enter().await;
                let __program = __state.as_mut().expect("program state");
                tokio::select! {{ biased; _ = __shutdown.changed() => break, _ = async {{ if __context.events_pending() {{ __context.drain_events_with(__program).await; }} }} => {{ let _ = reply.send(()); }} }}
            }},
            OrchCommand::Shutdown => break,
        }}
    }}
    if let Some(__main) = __main {{ __main.abort(); let _ = __main.await; }}
    let {unpack} = __context.program.enter().await.take().expect("program state");
    {stop}
}}
"#
        )
    }

    pub(super) fn library_adjust(&self, code: String) -> String {
        code.replace("tokio::spawn(", "crate::__orch_spawn(")
            .replace("tokio::time::sleep(", "crate::__orch_sleep(")
            .replace("tokio::time::timeout(", "crate::__orch_timeout(")
            .replace("__ORCH_LINE_SPAWN(", "crate::__orch_spawn_line(")
            .replace(
                "fn stop_orch() {\n    std::process::exit(0);\n}",
                "use crate::stop_orch;",
            )
    }
}

