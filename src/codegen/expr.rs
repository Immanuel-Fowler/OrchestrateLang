use crate::ast::{ExprNode, MatchPattern, StmtNode, BinaryOp, Expr, Literal, StringPart, Type};
use std::collections::HashSet;
use super::core::{Codegen, rust_ident};

impl Codegen {
    pub fn compile_expr(&mut self, expr: &Expr) -> String {
        match &expr.node {
            ExprNode::Literal(lit) => match lit {
                Literal::Int(v) => v.to_string(),
                Literal::Float(v) => format!("{:?}", v),
                Literal::Str(v) => format!("String::from({:?})", v),
                Literal::Bool(v) => v.to_string(),
            },
            ExprNode::Identifier(name) => self.read_name(name),
            ExprNode::Unary { op, operand } => {
                let operand_str = self.compile_expr(operand);
                match op {
                    crate::ast::UnaryOp::Neg => format!("(-{})", operand_str),
                    crate::ast::UnaryOp::Not => format!("(!{})", operand_str),
                }
            }
            // The element is read out of the array where it lives; the array itself is not
            // copied first, which for a `shared let` it used to be on every read.
            ExprNode::Index { .. } => format!("{}.clone()", self.compile_place(expr)),
            ExprNode::Binary { op: BinaryOp::Assign, lhs, rhs } => {
                // Assigning needs the place itself: a write to a copy is a write lost. The
                // value assigned is a copy, so `b = a` leaves `a` usable.
                let value = self.compile_value(rhs);
                let place = self.compile_place(lhs);
                format!("{} = {}", place, value)
            }
            ExprNode::Binary { op, lhs, rhs } => {
                // Operands are read where they live: every operator borrows or copies them.
                let lhs_str = self.compile_place(lhs);
                let rhs_str = self.compile_place(rhs);
                if *op == BinaryOp::Add {
                    // Both sides are borrowed: `a + b` must leave `a` and `b` usable.
                    format!("OrchAdd::orch_add(&({}), &({}))", lhs_str, rhs_str)
                } else {
                    let op_str = match op {
                        BinaryOp::Sub => "-",
                        BinaryOp::Mul => "*",
                        BinaryOp::Div => "/",
                        BinaryOp::Mod => "%",
                        BinaryOp::Eq => "==",
                        BinaryOp::Ne => "!=",
                        BinaryOp::Lt => "<",
                        BinaryOp::Gt => ">",
                        BinaryOp::Le => "<=",
                        BinaryOp::Ge => ">=",
                        BinaryOp::And => "&&",
                        BinaryOp::Or => "||",
                        BinaryOp::BitAnd => "&",
                        BinaryOp::BitOr => "|",
                        BinaryOp::BitXor => "^",
                        BinaryOp::Shl => "<<",
                        BinaryOp::Shr => ">>",
                        BinaryOp::Assign | BinaryOp::Add => unreachable!(),
                    };
                    format!("({} {} {})", lhs_str, op_str, rhs_str)
                }
            }
            ExprNode::Call { callee, args } => {
                // `length`, `append`, and `remove` work on the array where it lives, so a
                // change reaches the binding — a `shared let` or instance state included.
                if callee == "length" && args.len() == 1 {
                    // Parenthesised: `n as i64 < m` reads `<` as the start of generic arguments.
                    return format!("({}.len() as i64)", self.compile_place(&args[0]));
                } else if callee == "append" && args.len() == 2 {
                    let value = self.compile_value(&args[1]);
                    return format!("{}.push({})", self.compile_place(&args[0]), value);
                } else if callee == "remove" && args.len() == 2 {
                    // A block, so the call is void as the language types it: Rust's
                    // `remove` returns the element, which as the last statement of an
                    // `if` would have become the `if`'s value.
                    let index = self.compile_expr(&args[1]);
                    return format!("{{ {}.remove(({}) as usize); }}", self.compile_place(&args[0]), index);
                }

                if let Some(math) = self.compile_math(callee, args) {
                    return math;
                }
                if callee == "print" || callee == "to_string" {
                    // Both only display their argument, so it is lent, not copied.
                    let args_str = args.iter().map(|a| self.compile_lent(a)).collect::<Vec<_>>().join(", ");
                    let function = if callee == "print" { "print_val" } else { "to_string" };
                    format!("{}({})", function, args_str)
                } else if callee == "range" && args.len() == 1 {
                    let n = self.compile_expr(&args[0]);
                    format!("(0i64..({}) as i64).collect::<Vec<i64>>()", n)
                } else if callee == "range" && args.len() == 2 {
                    let s = self.compile_expr(&args[0]);
                    let e = self.compile_expr(&args[1]);
                    format!("(({}) as i64..({}) as i64).collect::<Vec<i64>>()", s, e)
                } else if matches!(callee.as_str(), "map" | "filter" | "find" | "any" | "all") && args.len() == 2 {
                    // The function is taken by value; the array is walked where it lives,
                    // one copied element at a time.
                    let xs = self.compile_place(&args[0]);
                    let f = self.compile_value(&args[1]);
                    let walk = match callee.as_str() {
                        "map" => "map(|__x| __f(__x)).collect::<Vec<_>>()",
                        "filter" => "filter(|__x| __f((*__x).clone())).collect::<Vec<_>>()",
                        "find" => "find(|__x| __f((*__x).clone()))",
                        "any" => "any(|__x| __f(__x))",
                        _ => "all(|__x| __f(__x))",
                    };
                    format!("{{ let mut __f = ({}); ({}).clone().into_iter().{} }}", f, xs, walk)
                } else if callee == "reduce" && args.len() == 3 {
                    let xs = self.compile_place(&args[0]);
                    let init = self.compile_value(&args[1]);
                    let f = self.compile_value(&args[2]);
                    format!("{{ let mut __f = ({}); ({}).clone().into_iter().fold({}, |acc, __x| __f(acc, __x)) }}", f, xs, init)
                } else if callee == "to_int" && args.len() == 1 {
                    format!("(({}) as i64)", self.compile_expr(&args[0]))
                } else if callee == "to_float" && args.len() == 1 {
                    format!("(({}) as f64)", self.compile_expr(&args[0]))
                } else if callee == "latest" && args.len() == 1 && !self.user_functions.contains(callee) {
                    // `latest(p.tick)`: the client keeps what the runtime's last call answered.
                    match &args[0].node {
                        ExprNode::FieldAccess { object, .. } => format!("({}).cadence_latest()", self.compile_place(object)),
                        _ => format!("compile_error!({:?})", "latest(...) takes a cadenced serverlet's tick, as in `latest(planner.tick)`"),
                    }
                } else if callee == "parse_int" && args.len() == 1 {
                    format!("({}).parse::<i64>().map_err(|e| e.to_string())", self.compile_place(&args[0]))
                } else if callee == "parse_float" && args.len() == 1 {
                    format!("({}).parse::<f64>().map_err(|e| e.to_string())", self.compile_place(&args[0]))
                } else if callee == "sleep" {
                    let args_str = self.compile_values(args);
                    self.require_async("calls sleep, which suspends until the timer fires");
                    format!("tokio::time::sleep(std::time::Duration::from_millis(({}) as u64)).await", args_str)
                } else if self.tasks.contains(callee) {
                    let args_str = self.compile_values(args);
                    self.require_async(&format!("calls the task '{}', which it would have to wait for", callee));
                    format!("{}({}).await", rust_ident(callee), args_str)
                } else if self.foreign_handle_params.contains_key(callee) {
                    let args = self.compile_args_for(callee, args);
                    format!("{}({})", self.call_name(callee), args)
                } else {
                    let args_str = self.compile_values(args);
                    format!("{}({})", self.call_name(callee), args_str)
                }
            }
            // `x |> f(a)` is `f(x, a)`, compiled as that call, so a pipeline and a call can
            // never mean different things.
            ExprNode::Pipeline { value, function } => {
                let (callee, rest) = match &function.node {
                    ExprNode::Identifier(name) => (name.clone(), Vec::new()),
                    ExprNode::Call { callee, args } => (callee.clone(), args.clone()),
                    _ => panic!("Invalid pipeline function target: {:?}", function),
                };
                let mut args = vec![(**value).clone()];
                args.extend(rest);
                let call = crate::ast::Spanned { span: expr.span.clone(), node: ExprNode::Call { callee, args } };
                self.compile_expr(&call)
            }
            ExprNode::Block(_) => {
                let inner = self.compile_block_inner(expr, false);
                format!("{{\n    {}\n}}", inner.replace("\n", "\n    "))
            }
            ExprNode::If { cond, then_branch, else_branch } => {
                let cond_str = self.compile_expr(cond);
                if let Some(else_b) = else_branch {
                    let then_str = self.compile_expr(then_branch);
                    let else_str = self.compile_expr(else_b);
                    format!("if {} {} else {}", cond_str, then_str, else_str)
                } else {
                    // Without `else` there is no value on the false path, so the branch is
                    // a statement: its last call's result is discarded, not the `if`'s value.
                    format!("if {} {}", cond_str, self.compile_statement_body(then_branch))
                }
            }
            ExprNode::ModuleCall { module_local_name, function, args } => {
                // Every callee here owns its arguments — a host method, a guest's import,
                // a module function, a serverlet's message — so a caller's binding is
                // copied and is still there after the call, on every boundary.
                let is_foreign_with_handles = self.modules.contains(module_local_name)
                    && self.foreign_handle_params.contains_key(&format!("{}::{}", module_local_name, function));
                let args_str = if is_foreign_with_handles { String::new() } else { self.compile_values(args) };

                let is_host_call = self.host_functions.iter().any(|(g, h)| g == module_local_name && h.name == *function);
                if let (Some(granted), true) = (&self.sandbox_guest_grants, is_host_call) {
                    // Inside the guest the host is reachable only through the imports its
                    // grants defined; each one is a stub that carries the call across.
                    let call = format!("{module_local_name}.{function}");
                    if granted.contains(&call) {
                        return format!("__orch_grant_{module_local_name}_{function}({args_str})");
                    }
                    self.errors.push(format!(
                        "sandboxed serverlet handler calls {call}, which the serverlet was not granted; add `grant call {call}` or remove the call"
                    ));
                    return "Default::default()".to_string();
                }
                if self.library && is_host_call {
                    // The trait call and one branch, with the failure out of line; the
                    // parentheses keep the match a statement where its value is dropped.
                    let receiver = if self.context_bound { "__host" } else { "crate::__orch_context().host" };
                    return format!(
                        "(match {receiver}.{}({args_str}) {{ Ok(__value) => __value, Err(__error) => {{ crate::__orch_host_failed({:?}, &__error); Default::default() }} }})",
                        Self::host_method(module_local_name, function),
                        format!("{module_local_name}.{function}")
                    );
                }
                if self.modules.contains(module_local_name) {
                    let full_name = format!("{}::{}", module_local_name, function);
                    if self.tasks.contains(&full_name) {
                        self.require_async(&format!("calls the task '{}', which it would have to wait for", full_name));
                        format!("{}::{}({}).await", rust_ident(module_local_name), rust_ident(function), args_str)
                    } else if self.foreign_handle_params.contains_key(&full_name) {
                        let args = self.compile_args_for(&full_name, args);
                        format!("{}::{}({})", rust_ident(module_local_name), rust_ident(function), args)
                    } else {
                        format!("{}::{}({})", rust_ident(module_local_name), rust_ident(function), args_str)
                    }
                } else {
                    self.require_async(&format!("calls '{}.{}', which waits for the serverlet to reply", module_local_name, function));
                    format!("{}.{}({}).await", self.place_name(module_local_name), rust_ident(function), args_str)
                }
            }
            ExprNode::StartServerlet { name, args } => {
                let start_fn = if name.contains("::") {
                    let parts: Vec<&str> = name.split("::").collect();
                    format!("{}::start_{}", parts[0], parts[1])
                } else {
                    format!("start_{}", name)
                };
                let args_str = self.compile_values(args);
                format!("{}({})", start_fn, args_str)
            }
            ExprNode::AutomaticBlock { body, restart_policy, backoff_ms, every_micros, crash_handler } => {
                let own_name = self.naming.take();
                let worker = own_name.as_ref().map(|name| format!(" '{name}'")).unwrap_or_default();
                let mut free_vars = HashSet::new();
                self.get_free_vars_expr(expr, &mut HashSet::new(), &mut free_vars);
                // In name order: set order changes between runs, and so would the output.
                let mut free_vars: Vec<String> = free_vars.into_iter().collect();
                // A `shared let` is reached through the lock wherever it is used, so it is
                // never captured — capturing it would copy the value and lose the sharing.
                free_vars.retain(|name| !self.is_shared(name));
                free_vars.sort();
                // A worker that names itself — to keep itself in a set it swaps in — reaches
                // its own handle through a slot filled once it exists.
                let (self_slot, self_bind) = self.self_reference(own_name.as_deref(), &mut free_vars);

                let mut capture_code = String::new();
                // The worker's copies are taken once, when it is declared; each start of
                // the worker, and each restart, copies them again, so the closure can be
                // called any number of times and every run gets what the first one got.
                let mut recapture = self_bind;
                for var in &free_vars {
                    capture_code.push_str(&format!("let {} = {}.clone();\n    ", rust_ident(var), self.read_name(var)));
                    recapture.push_str(&format!("let {0} = {0}.clone(); ", rust_ident(var)));
                }
                // Inside the closure every captured name is the clone above, not a program field.
                self.push_scope();
                for var in &free_vars { self.define_local(var); }
                // Each of the worker's tasks binds the instance once, at its top.
                let bound = std::mem::replace(&mut self.context_bound, self.library);
                let deferred = self.enter_deferred_body();
                let bind = if self.library { "let __context = crate::__orch_context(); let __host = &*__context.host;\n            " } else { "" };

                // What a crash does before the policy is asked: the declared `on_crash`,
                // with the error bound, or a report.
                let crash_code = if let Some((err_name, handler)) = crash_handler {
                    self.push_scope();
                    self.define_local(err_name);
                    let handler_str = self.compile_statement_body(handler);
                    self.pop_scope();
                    format!("let {} = format!(\"{{:?}}\", __e);\n                {}", rust_ident(err_name), handler_str)
                } else {
                    format!("eprintln!(\"[orchestrate] automatic block{worker} panicked (after {{}} restarts): {{:?}}\", __restarts, __e);")
                };

                // Generate the body
                let (setup_code, loop_code) = if let ExprNode::Block(stmts) = &body.node {
                    let mut setup = Vec::new();
                    let mut loop_c = Vec::new();
                    for stmt in stmts {
                        let is_serverlet_start = matches!(&stmt.node, StmtNode::Let { value, .. } if matches!(value.node, ExprNode::StartServerlet { .. }));
                        let compiled = self.compile_stmt(stmt);
                        let compiled = if compiled.ends_with(';') || compiled.ends_with('}') { compiled } else { format!("{};", compiled) };
                        if is_serverlet_start { setup.push(compiled); } else { loop_c.push(compiled); }
                    }
                    (setup, loop_c)
                } else {
                    (vec![], vec![self.compile_expr(body)])
                };
                self.pop_scope();
                self.leave_deferred_body(deferred);
                self.context_bound = bound;

                let setup_inner = setup_code.join("\n                ");
                let loop_inner = loop_code.join("\n                    ");

                // The policy is the only thing that restarts a worker: `restart: N` is at
                // most N restarts after crashes, and a worker that has spent them stays
                // stopped, saying so once.
                let (allowed, spent) = match restart_policy {
                    crate::ast::RestartPolicy::Always => ("true".to_string(), String::new()),
                    crate::ast::RestartPolicy::Never => ("false".to_string(), "its policy is restart: never".to_string()),
                    crate::ast::RestartPolicy::MaxAttempts(n) => (format!("__restarts < {n}"), format!("its policy, restart: {n}, allows no more restarts")),
                };
                // A declared backoff is the wait before every restart; the default starts at
                // a second and doubles with each restart, up to thirty.
                let delay = match backoff_ms {
                    Some(ms) => format!("{ms}u64"),
                    None => "(1000u64 * 2u64.saturating_pow((__restarts - 1).min(5))).min(30000)".to_string(),
                };

                // `every: "20ms"`: each pass through the body waits for its turn first, on a
                // schedule fixed to the first pass, so a `continue` waits too and the rate does
                // not drift with the body's own time. A pass that overran skips the turns it
                // missed rather than running them back to back.
                let pace = match every_micros {
                    Some(micros) => format!(
                        "let __period = std::time::Duration::from_micros({micros}u64);\n                    \
                         let mut __due = std::time::Instant::now();\n                    "
                    ),
                    None => String::new(),
                };
                let wait = if every_micros.is_some() {
                    "let __now = std::time::Instant::now();\n                        \
                     if __due > __now { tokio::time::sleep(__due - __now).await; }\n                        \
                     let __now = std::time::Instant::now();\n                        \
                     __due += __period;\n                        \
                     if __due <= __now { __due += __period * ((((__now - __due).as_nanos() / __period.as_nanos()) + 1).min(u32::MAX as u128) as u32); }\n                        "
                } else {
                    ""
                };

                let setup_block = if setup_code.is_empty() {
                    String::new()
                } else {
                    format!("// Serverlet setup — once per process lifetime\n                {}\n                ", setup_inner)
                };

                let worker_code = format!(
                    r#"{{
    {capture}std::sync::Arc::new(move || {{
        {recapture}__orch_spawn_worker({worker_label:?}, async move {{
            {bind}let mut __restarts: u32 = 0;
            loop {{
                {setup}{recapture}let __inner_handle = tokio::spawn(async move {{
                    {bind}{pace}loop {{
                        {wait}{loop_body}
                    }}
                }});
                // Stopping the worker stops its current run with it.
                let __run = __OrchAbortOnDrop(__inner_handle.abort_handle());
                let __e = match __inner_handle.await {{
                    Ok(_) => break,
                    Err(__e) => __e,
                }};
                drop(__run);
                {crash}
                if !({allowed}) {{
                    eprintln!("[orchestrate] automatic block{worker} crashed and stays stopped: {spent}");
                    break;
                }}
                __restarts += 1;
                tokio::time::sleep(std::time::Duration::from_millis({delay})).await;
            }}
        }})
    }}) as ProcessRef
}}"#,
                    capture = capture_code,
                    recapture = recapture,
                    bind = bind,
                    setup = setup_block,
                    loop_body = loop_inner,
                    pace = pace,
                    wait = wait,
                    crash = crash_code,
                    allowed = allowed,
                    worker_label = own_name.as_ref().map(|n| format!("'{n}'")).unwrap_or_else(|| "(unnamed)".to_string()),
                    worker = worker,
                    spent = spent,
                    delay = delay,
                );
                Self::fill_self_slot(worker_code, &self_slot)
            }
            ExprNode::TriggeredBlock { event_name, params, body } => {
                // A library's top-level handler runs inside a tick, one at a time, so it
                // reaches instance state where the tick does rather than holding a copy.
                let reaches_program = self.library && std::mem::take(&mut self.top_level_handler);
                let mut free_vars = HashSet::new();
                self.get_free_vars_expr(expr, &mut HashSet::new(), &mut free_vars);
                let mut free_vars: Vec<String> = free_vars.into_iter().collect();
                // A `shared let` is reached through the lock wherever it is used, so it is
                // never captured — capturing it would copy the value and lose the sharing.
                free_vars.retain(|name| !self.is_shared(name));
                if reaches_program {
                    free_vars.retain(|name| !self.program_fields.contains_key(name));
                }
                free_vars.sort();

                // Copies taken when the handler is declared, and again for each
                // registration and each event, so every run gets what the first one got.
                let mut capture_code = String::new();
                let mut recapture = String::new();
                for var in &free_vars {
                    capture_code.push_str(&format!("let {} = {}.clone();\n    ", rust_ident(var), self.read_name(var)));
                    recapture.push_str(&format!("let {0} = {0}.clone(); ", rust_ident(var)));
                }
                let outer_rewrite = if reaches_program {
                    self.state_rewrite.replace(super::core::StateRewrite {
                        fields: self.program_fields.clone(),
                        scopes: Vec::new(),
                        receiver: "(*__context.program_ptr())",
                    })
                } else {
                    None
                };
                self.push_scope();
                for var in &free_vars { self.define_local(var); }
                for p in params { self.define_local(&p.name); }

                let func_name = format!("get_registry_{}", event_name);
                let types_str = if params.is_empty() {
                    "()".to_string()
                } else if params.len() == 1 {
                    self.compile_type(&params[0].ty).to_string()
                } else {
                    let compiled_tys = params.iter().map(|p| self.compile_type(&p.ty)).collect::<Vec<String>>().join(", ");
                    format!("({})", compiled_tys)
                };
                let arc_type_str = format!("std::sync::Arc<{}>", types_str);

                let bindings = if params.is_empty() {
                    "_".to_string()
                } else if params.len() == 1 {
                    rust_ident(&params[0].name)
                } else {
                    let names = params.iter().map(|p| rust_ident(&p.name)).collect::<Vec<String>>().join(", ");
                    format!("({})", names)
                };

                // A library handler binds the instance once, at its top.
                let handler = self.library && event_name != "update_orchestrator";
                let bound = std::mem::replace(&mut self.context_bound, handler);
                let deferred = self.enter_deferred_body();
                let body_str = self.compile_statement_body(body);
                self.leave_deferred_body(deferred);
                self.context_bound = bound;
                self.pop_scope();
                if reaches_program {
                    self.state_rewrite = outer_rewrite;
                }
                if handler {
                    // Instance state is reached through a pointer the tick sets while it
                    // drains events, and only then: see `OrchContext::program_ptr`.
                    let body_str = if reaches_program { format!("#[allow(unused_unsafe)] unsafe {body_str}") } else { body_str };
                    return format!("{{ {capture_code} std::sync::Arc::new(move || {{ {recapture} crate::{func_name}().lock().unwrap().push(std::sync::Arc::new(move |msg: {arc_type_str}| {{ {recapture} Box::pin(async move {{ let __context = crate::__orch_context(); let __host = &*__context.host; let {bindings} = (*msg).clone(); {body_str}; }}) as crate::OrchEvent }})); }}) }}");
                }

                // A process: starting it registers the handler and spawns its receive loop,
                // whose handle is the process's, so stopping it ends the loop; its sender
                // then reads as closed and a trigger drops it.
                format!(
                    "{{\n    {capture}std::sync::Arc::new(move || {{\n        {recapture}let (tx, mut rx) = tokio::sync::mpsc::channel::<{ty}>(100);\n        {registry}().lock().unwrap().push(tx);\n        tokio::spawn(async move {{\n            while let Some(msg) = rx.recv().await {{\n                let {bindings} = (*msg).clone();\n                {recapture}tokio::spawn(async move {body});\n            }}\n        }})\n    }}) as ProcessRef\n}}",
                    capture = capture_code, recapture = recapture, ty = arc_type_str, registry = func_name, bindings = bindings, body = body_str
                )
            }
            ExprNode::StartProcess { target } => {
                let target_str = self.compile_expr(target);
                if target_str.starts_with("__program.") { format!("({})()", target_str) } else { format!("{}()", target_str) }
            }
            ExprNode::ArrayLiteral(elements) => {
                let elems_str = self.compile_values(elements);
                format!("vec![{}]", elems_str)
            }
            ExprNode::StructLiteral { name, fields } => {
                let fields_str = fields.iter()
                    .map(|(fname, val)| format!("    {}: {},", rust_ident(fname), self.compile_value(val)))
                    .collect::<Vec<_>>()
                    .join("\n");
                format!("{} {{\n{}\n}}", rust_ident(name), fields_str)
            }
            // A field is read where its struct lives, not from a copy of the whole struct;
            // where the value is taken, `compile_value` copies the field itself.
            ExprNode::FieldAccess { .. } => self.compile_place(expr),
            // Error handling expressions
            ExprNode::NoneLiteral => "None".to_string(),
            ExprNode::SomeLiteral(inner) => {
                format!("Some({})", self.compile_value(inner))
            }
            ExprNode::OkLiteral(inner) => {
                format!("Ok({})", self.compile_value(inner))
            }
            ExprNode::ErrLiteral(msg) => {
                format!("Err({})", self.compile_value(msg))
            }
            ExprNode::Propagate(inner) => {
                // In async context, ? works directly on Result/Option
                format!("({})? ", self.compile_value(inner))
            }
            ExprNode::TryCatch { body, err_name, err_type, handler } => {
                let before = self.awaits;
                let body_str = self.compile_expr(body);
                self.push_scope();
                self.define_local(err_name);
                let handler_str = self.compile_expr(handler);
                self.pop_scope();
                let error_ty = err_type.as_ref().map(|t| self.compile_type(t)).unwrap_or_else(|| "String".to_string());
                if self.awaits > before {
                    // Something in here waits, so the block cannot be a synchronous
                    // closure. An async block awaited in place keeps `?` working and lets
                    // the handler wait too.
                    return format!(
                        r#"match async {{ Ok::<_, {error_ty}>({{ {} }}) }}.await {{ Ok(__ok) => __ok, Err({}) => {{ {} }} }}"#,
                        body_str, err_name, handler_str
                    );
                }
                format!(
                    r#"(|| -> Result<_, {error_ty}> {{ Ok({{ {} }}) }})().unwrap_or_else(|{}: {error_ty}| {{ {} }})"#,
                    body_str, err_name, handler_str
                )
            }
            // Enum expressions
            ExprNode::EnumVariantLiteral { enum_name, variant_name, payload } => {
                match payload {
                    Some(p) => format!("{}::{}({})", rust_ident(enum_name), rust_ident(variant_name), self.compile_value(p)),
                    None => format!("{}::{}", rust_ident(enum_name), rust_ident(variant_name)),
                }
            }
            ExprNode::Match { value, arms } => {
                // An arm that binds takes the value it binds, so the subject is a copy; one
                // that only compares reads it where it lives.
                let binds = arms.iter().any(|arm| {
                    let mut names = Vec::new();
                    Self::pattern_bindings(&arm.pattern, &mut names);
                    !names.is_empty()
                });
                let val_str = if binds { self.compile_value(value) } else { self.compile_place(value) };
                // String literal patterns require matching on &str via .as_str()
                let has_str_literal = arms.iter().any(|a| matches!(&a.pattern, MatchPattern::Literal(Literal::Str(_))));
                let match_subject = if has_str_literal { format!("{}.as_str()", val_str) } else { val_str };

                let mut arms_code = Vec::new();
                for arm in arms {
                    self.push_scope();
                    let mut bindings = Vec::new();
                    Self::pattern_bindings(&arm.pattern, &mut bindings);
                    for name in &bindings { self.define_local(name); }
                    // Pre-compile guard conditions (needs &mut self, so do before pattern compilation)
                    let guard_str: Option<String> = if let MatchPattern::Guard { condition, .. } = &arm.pattern {
                        let cond_clone = *condition.clone();
                        Some(self.compile_expr(&cond_clone))
                    } else {
                        None
                    };

                    let pattern_str = Self::compile_match_pattern_str(&arm.pattern, guard_str.as_deref());
                    let body_str = self.compile_expr(&arm.body);
                    self.pop_scope();
                    arms_code.push(format!("        {} => {{ {} }}", pattern_str, body_str));
                }
                format!("match {} {{\n{}\n    }}", match_subject, arms_code.join(",\n"))
            }
            ExprNode::Closure { params, return_type, body } => {
                let params_str = params.iter()
                    .map(|p| format!("{}: {}", rust_ident(&p.name), self.compile_type(&p.ty)))
                    .collect::<Vec<_>>()
                    .join(", ");
                let ret_str = match return_type {
                    Some(rt) if *rt != Type::Void => format!(" -> {}", self.compile_type(rt)),
                    _ => String::new(),
                };
                // A closure captures copies of what it names from outside, taken when it is
                // made, so the originals stay usable and the closure can run any number of
                // times. A `shared let` is reached through the lock instead, as everywhere.
                let mut free = HashSet::new();
                let mut bound_names: HashSet<String> = params.iter().map(|p| p.name.clone()).collect();
                self.get_free_vars_expr(body, &mut bound_names, &mut free);
                let mut free: Vec<String> = free.into_iter().filter(|name| !self.is_shared(name)).collect();
                free.sort();
                let capture = free.iter()
                    .map(|name| format!("let {} = {}.clone(); ", rust_ident(name), self.read_name(name)))
                    .collect::<String>();
                self.push_scope();
                for name in &free { self.define_local(name); }
                for p in params { self.define_local(&p.name); }
                // A closure may outlive the tick, so it cannot borrow the tick's bindings.
                let bound = std::mem::replace(&mut self.context_bound, false);
                let deferred = self.enter_deferred_body();
                self.closure_depth += 1;
                let body_str = if return_type.as_ref().is_some_and(|rt| *rt != Type::Void) {
                    self.compile_expr(body)
                } else {
                    self.compile_statement_body(body)
                };
                self.closure_depth -= 1;
                self.leave_deferred_body(deferred);
                self.context_bound = bound;
                self.pop_scope();
                if capture.is_empty() {
                    format!("move |{}|{} {}", params_str, ret_str, body_str)
                } else {
                    format!("{{ {}move |{}|{} {} }}", capture, params_str, ret_str, body_str)
                }
            }
            ExprNode::StringInterp { parts } => {
                let mut fmt_str = String::new();
                let mut args: Vec<String> = Vec::new();
                for part in parts {
                    match part {
                        StringPart::Literal(s) => {
                            fmt_str.push_str(&s.replace('{', "{{").replace('}', "}}"));
                        }
                        StringPart::Expr(e) => {
                            // `format!` borrows what it prints, so a binding is read in place.
                            fmt_str.push_str("{}");
                            args.push(self.compile_place(e));
                        }
                    }
                }
                if args.is_empty() {
                    format!("String::from({:?})", fmt_str)
                } else {
                    format!("format!({:?}, {})", fmt_str, args.join(", "))
                }
            }
        }
    }

    /// For a process declared as `let name = automatic { ... }` whose body names `name`:
    /// removes it from the captures and returns the code for a slot that holds a weak
    /// handle to the process, filled once the process exists — weak, so the process does
    /// not keep itself alive — and the binding that reads it at the start of each run.
    /// Both are empty for a process that does not name itself.
    fn self_reference(&self, own_name: Option<&str>, free_vars: &mut Vec<String>) -> (bool, String) {
        let Some(name) = own_name else { return (false, String::new()) };
        let Some(position) = free_vars.iter().position(|v| v == name) else { return (false, String::new()) };
        free_vars.remove(position);
        let bind = format!(
            "let {}: ProcessRef = __orch_self.get().and_then(std::sync::Weak::upgrade).expect(\"a process reached its own handle before it existed\"); let __orch_self = __orch_self.clone(); ",
            rust_ident(name)
        );
        (true, bind)
    }

    /// The process `code` builds, with its self slot around it when it has one: made
    /// before the process, captured by it, and filled with the process once it exists.
    fn fill_self_slot(code: String, has_slot: &bool) -> String {
        if !has_slot { return code; }
        format!(
            "{{ let __orch_self: std::sync::Arc<std::sync::OnceLock<std::sync::Weak<dyn Fn() -> tokio::task::JoinHandle<()> + Send + Sync + 'static>>> = std::sync::Arc::new(std::sync::OnceLock::new()); \
             let __orch_self_slot = __orch_self.clone(); \
             let __orch_process: ProcessRef = {code}; \
             let _ = __orch_self_slot.set(std::sync::Arc::downgrade(&__orch_process)); __orch_process }}"
        )
    }

    /// A math builtin as the Rust method it is, or `None` when `callee` is not one — or is
    /// the program's own function of that name, which comes first.
    fn compile_math(&mut self, callee: &str, args: &[Expr]) -> Option<String> {
        if self.user_functions.contains(callee) { return None; }
        let method = match (callee, args.len()) {
            ("sqrt" | "abs" | "floor" | "ceil" | "round" | "sin" | "cos" | "tan" | "exp", 1) => callee,
            ("ln", 1) => "ln",
            ("atan2" | "min" | "max", 2) => callee,
            ("pow", 2) => "pow",
            _ => return None,
        };
        let receiver = self.compile_numeric(&args[0]);
        let other = args.get(1).map(|arg| self.compile_numeric(arg));
        Some(match (callee, other) {
            // Floats only, so named on `f64`, which also settles a bare literal's type.
            ("abs", None) => format!("({receiver}).abs()"),
            (_, None) => format!("f64::{method}({receiver})"),
            ("atan2", Some(other)) => format!("f64::atan2({receiver}, {other})"),
            // `powf` for floats and `pow` for ints, chosen by the arguments' own type.
            ("pow", Some(other)) => format!("__OrchPow::orch_pow({receiver}, {other})"),
            (_, Some(other)) => format!("({receiver}).{method}({other})"),
        })
    }

    /// A number whose type a method call has to know: a bare literal gets its suffix, since
    /// `(-3).abs()` names no type; anything else already has one.
    fn compile_numeric(&mut self, expr: &Expr) -> String {
        match &expr.node {
            ExprNode::Literal(Literal::Int(v)) => format!("{v}i64"),
            ExprNode::Literal(Literal::Float(v)) => format!("{v:?}f64"),
            ExprNode::Unary { op: crate::ast::UnaryOp::Neg, operand } if matches!(operand.node, ExprNode::Literal(Literal::Int(_) | Literal::Float(_))) => {
                format!("(-{})", self.compile_numeric(operand))
            }
            _ => self.compile_expr(expr),
        }
    }

    /// The names a match pattern binds, which shadow program fields in its arm.
    pub(super) fn pattern_bindings(pattern: &MatchPattern, names: &mut Vec<String>) {
        match pattern {
            MatchPattern::EnumVariant { binding: Some(name), .. } | MatchPattern::Binding(name) => names.push(name.clone()),
            MatchPattern::Guard { inner, .. } => Self::pattern_bindings(inner, names),
            _ => {}
        }
    }

    /// Pure helper — compiles a MatchPattern to a Rust pattern string.
    /// `guard_str` is pre-compiled from the condition expr (if any Guard wraps this pattern).
    pub fn compile_match_pattern_str(pattern: &MatchPattern, guard_str: Option<&str>) -> String {
        match pattern {
            MatchPattern::EnumVariant { enum_name, variant_name, binding } => {
                let is_builtin = matches!(enum_name.as_str(), "option" | "result");
                let base = if is_builtin {
                    match binding {
                        Some(b) => format!("{}({})", variant_name, rust_ident(b)),
                        None => variant_name.clone(),
                    }
                } else {
                    match binding {
                        Some(b) => format!("{}::{}({})", rust_ident(enum_name), rust_ident(variant_name), rust_ident(b)),
                        None => format!("{}::{}", rust_ident(enum_name), rust_ident(variant_name)),
                    }
                };
                if let Some(g) = guard_str { format!("{} if {}", base, g) } else { base }
            }
            MatchPattern::Wildcard => {
                if let Some(g) = guard_str { format!("_ if {}", g) } else { "_".to_string() }
            }
            MatchPattern::Literal(lit) => {
                let base = match lit {
                    Literal::Int(n) => format!("{}i64", n),
                    Literal::Float(f) => format!("{}_f64", f),
                    Literal::Bool(b) => b.to_string(),
                    // String literal patterns work when the match subject uses .as_str()
                    Literal::Str(s) => format!("{:?}", s),
                };
                if let Some(g) = guard_str { format!("{} if {}", base, g) } else { base }
            }
            MatchPattern::Binding(name) => {
                if let Some(g) = guard_str { format!("{} if {}", rust_ident(name), g) } else { rust_ident(name) }
            }
            MatchPattern::Guard { inner, .. } => {
                // guard_str is pre-compiled and passed in; inner already has no nested guard
                Self::compile_match_pattern_str(inner, guard_str)
            }
        }
    }
}
