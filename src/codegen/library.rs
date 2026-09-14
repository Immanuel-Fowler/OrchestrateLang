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
            fields.push_str(&format!("    event_{}: std::sync::Arc<std::sync::Mutex<Vec<tokio::sync::mpsc::Sender<std::sync::Arc<{}>>>>>,\n", name, ty));
            init.push_str(&format!("    event_{}: Default::default(),\n", name));
        }
        let mut runtime = include_str!("library_runtime.rs.txt")
            .replace("@HOST_METHODS@", &methods)
            .replace("@EVENT_FIELDS@", &fields)
            .replace("@EVENT_INIT@", &init);
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
        for stmt in self.local_stmts.clone() {
            match stmt.node {
                StmtNode::OnStart(body) => {
                    start.push_str(&format!("{};\n", self.compile_expr(&body)))
                }
                StmtNode::OnStop(body) => {
                    stop.push_str(&format!("{};\n", self.compile_expr(&body)))
                }
                StmtNode::OnTick { param, body } => tick.push_str(&format!(
                    "{{ let {} = __dt; {}; }}\n",
                    param,
                    self.compile_expr(&body)
                )),
                _ => {}
            }
        }
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
    let __main = tokio::spawn(async move {{ orchestrator_main({args}).await; }});
    let _ = ready.send(());
    while !*__shutdown.borrow() {{
        let command = tokio::select! {{ biased; _ = __shutdown.changed() => break, command = commands.recv() => command }};
        let Some(command) = command else {{ break; }};
        match command {{
            OrchCommand::Tick(__dt, reply) => {{
                tokio::select! {{ biased; _ = __shutdown.changed() => break, _ = async {{ {tick} }} => {{ let _ = reply.send(()); }} }}
            }},
            OrchCommand::Shutdown => break,
        }}
    }}
    __main.abort();
    let _ = __main.await;
    {stop}
}}
"#
        )
    }

    pub(super) fn library_adjust(&self, code: String) -> String {
        code.replace("tokio::spawn(", "crate::__orch_spawn(")
            .replace("__ORCH_LINE_SPAWN(", "crate::__orch_spawn_line(")
            .replace(
                "fn stop_orch() {\n    std::process::exit(0);\n}",
                "use crate::stop_orch;",
            )
    }
}
