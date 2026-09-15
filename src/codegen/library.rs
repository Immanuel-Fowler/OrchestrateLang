use super::core::Codegen;
use crate::ast::{StmtNode, Type};

impl Codegen {
    pub(super) fn host_type(&self, ty: &Type) -> String {
        match ty {
            Type::Named(name) => format!("crate::{}", name),
            Type::Array(inner, _) => format!("Vec<{}>", self.host_type(inner)),
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

    pub(super) fn library_runtime(&self) -> String {
        let methods = self
            .host_functions
            .iter()
            .map(|(group, handler)| {
                let params = handler
                    .params
                    .iter()
                    .map(|p| format!("{}: {}", p.name, self.compile_type(&p.ty)))
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
                triggers.push_str(&format!("pub fn trigger_{name}(&self, {params}) -> Result<(), String> {{ if *self.context.shutdown.borrow() {{ return Err(\"library is stopped\".into()); }} let context = self.context.clone(); let future: OrchEvent = Box::pin(async move {{ let handlers = context.event_{name}.lock().unwrap().clone(); let value = std::sync::Arc::new({value}); for handler in handlers {{ context.batch.lock().unwrap().push_back(handler(value.clone())); }} }}); self.context.events.lock().unwrap().push_back(future); Ok(()) }}\n"));
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
        let mut runtime = include_str!("library_runtime.rs.txt")
            .replace("@HOST_METHODS@", &methods)
            .replace("@EVENT_FIELDS@", &fields)
            .replace("@EVENT_INIT@", &init)
            .replace("@TICK_PARAM@", &input_param).replace("@TICK_ARG@", input_arg)
            .replace("@TICK_FIELD@", &input_field).replace("@TICK_OUTPUT@", &output);
        runtime.push_str(&format!("\nimpl Scripts {{ {} }}\n", triggers));
        if self.host_functions.is_empty() {
            runtime.push_str("\nimpl Host for () {}\n");
        }
        runtime
    }

    pub(super) fn library_entry(
        &mut self,
        helper: &str,
        declarations: &str,
        execution: &str,
        args: &str,
    ) -> String {
        let mut start = String::new();
        let mut stop = String::new();
        let mut tick = String::new();
        let mut fixed = String::new();
        for stmt in self.local_stmts.clone() {
            match stmt.node {
                StmtNode::OnStart(body) => {
                    start.push_str(&format!("{};\n", self.compile_expr(&body)))
                }
                StmtNode::OnStop(body) => {
                    stop.push_str(&format!("{};\n", self.compile_expr(&body)))
                }
                StmtNode::OnFixedTick { param, body } => fixed.push_str(&format!("{{ let {} = __dt; {}; }}\n", param, self.compile_expr(&body))),
                StmtNode::OnTick { param, input, return_type, body } => {
                    let bind = input.as_ref().map(|p| format!("let {} = __input;", p.name)).unwrap_or_default();
                    if input.is_some() || return_type != Type::Void {
                        tick = format!("let {} = __dt; {} {}", param, bind, self.compile_expr(&body));
                    } else { tick.push_str(&format!("{{ let {} = __dt; {}; }}\n", param, self.compile_expr(&body))); }
                },
                _ => {}
            }
        }
        let input_binding = if self.tick_types().0.is_some() { ", __input" } else { "" };
        format!(
            r#"{helper}
async fn __orch_entry(mut commands: tokio::sync::mpsc::UnboundedReceiver<OrchCommand>, ready: tokio::sync::oneshot::Sender<()>) {{
    {declarations}
    let __context = crate::__orch_context();
    let mut __shutdown = __context.shutdown.subscribe();
    if !*__shutdown.borrow() {{
        tokio::select! {{ biased; _ = __shutdown.changed() => {{}}, _ = async {{ {start} }} => {{}} }}
    }}
    {execution}
    let __main = if __context.options.deterministic {{ orchestrator_main({args}).await; None }} else {{ Some(tokio::spawn(async move {{ orchestrator_main({args}).await; }})) }};
    let _ = ready.send(());
    while !*__shutdown.borrow() {{
        let command = tokio::select! {{ biased; _ = __shutdown.changed() => break, command = commands.recv() => command }};
        let Some(command) = command else {{ break; }};
        match command {{
            OrchCommand::Tick(__dt{input_binding}, reply) => {{
                __context.frame.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                __context.dt_bits.store(__dt.to_bits(), std::sync::atomic::Ordering::Relaxed);
                tokio::select! {{ biased; _ = __shutdown.changed() => break, __output = async {{ __context.advance_time(__dt); __context.drain_events().await; let output = async {{ {tick} }}.await; __context.drain_events().await; output }} => {{ let _ = reply.send(__output); }} }}
            }},
            OrchCommand::FixedTick(__dt, reply) => {{
                tokio::select! {{ biased; _ = __shutdown.changed() => break, _ = async {{ __context.drain_events().await; {fixed} __context.drain_events().await; }} => {{ let _ = reply.send(()); }} }}
            }},
            OrchCommand::Shutdown => break,
        }}
    }}
    if let Some(__main) = __main {{ __main.abort(); let _ = __main.await; }}
    {stop}
}}
"#
        )
    }

    pub(super) fn library_adjust(&self, code: String) -> String {
        escape_edition_identifiers(&code).replace("tokio::spawn(", "crate::__orch_spawn(")
            .replace("tokio::time::sleep(", "crate::__orch_sleep(")
            .replace("__ORCH_LINE_SPAWN(", "crate::__orch_spawn_line(")
            .replace(
                "fn stop_orch() {\n    std::process::exit(0);\n}",
                "use crate::stop_orch;",
            )
    }
}


// Generated Rust uses quoted strings for language literals; preserve their contents
// while escaping the identifier newly reserved by edition 2024.
fn escape_edition_identifiers(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut output = String::new();
    let mut start = 0;
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'"' {
            index += 1;
            while index < bytes.len() {
                if bytes[index] == b'\\' { index += 2; }
                else if bytes[index] == b'"' { index += 1; break; }
                else { index += 1; }
            }
        } else if bytes[index..].starts_with(b"//") {
            while index < bytes.len() && bytes[index] != b'\n' { index += 1; }
        } else if bytes[index].is_ascii_alphabetic() || bytes[index] == b'_' {
            let begin = index;
            while index < bytes.len() && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_') { index += 1; }
            if &source[begin..index] == "gen" && !(begin >= 2 && &bytes[begin-2..begin] == b"r#") {
                output.push_str(&source[start..begin]);
                output.push_str("r#gen");
                start = index;
            }
        } else { index += 1; }
    }
    output.push_str(&source[start..]);
    output
}
