use super::core::{pascal_case, Codegen};
use crate::ast::{Expr, Handler, Type};

impl Codegen {
    pub(super) fn compile_python_mirror(
        &mut self,
        name: &str,
        handlers: &[Handler],
        crash: &Option<(String, Box<Expr>)>,
    ) -> String {
        if let Some(reason) = super::stmt::wire_unsupported_reason(handlers, &self.struct_defs) {
            return format!("compile_error!({:?});", reason);
        }
        let signatures = handlers
            .iter()
            .map(|h| {
                let params = h
                    .params
                    .iter()
                    .map(|p| p.ty.display_name())
                    .collect::<Vec<_>>()
                    .join(",");
                format!(
                    "{:?}.to_string()",
                    format!("{}({})->{}", h.name, params, h.return_type.display_name())
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let recovery = crash
            .as_ref()
            .map(|(var, body)| {
                format!(
                    "let {} = __error.clone(); {};",
                    var,
                    self.compile_expr(body)
                )
            })
            .unwrap_or_default();
        let arms = handlers.iter().enumerate().map(|(id, h)| {
            let mut bindings = h.params.iter().map(|p| p.name.clone()).collect::<Vec<_>>();
            bindings.push("reply_to".into());
            let encode = h.params.iter().map(|p| format!("{}.wire_encode(&mut __payload);", p.name)).collect::<Vec<_>>().join("\n");
            let decode = if h.return_type == Type::Void {
                "if !f.payload.is_empty() { return Err(\"unexpected void reply payload\".to_string()); } ()".into()
            } else {
                format!("let mut pos = 0; let value = <{} as OrchWire>::wire_decode(&f.payload, &mut pos).ok_or(\"invalid reply value\")?; if pos != f.payload.len() {{ return Err(\"trailing reply data\".to_string()); }} value", self.compile_type(&h.return_type))
            };
            format!(r#"{name}Msg::{variant} {{ {bindings} }} => {{
                __next_call = __next_call.wrapping_add(1);
                let mut __payload = Vec::new();
                {id}i64.wire_encode(&mut __payload);
                {encode}
                let __result: Result<_, String> = async {{
                    __secret_write_frame(&mut __cin, ORCH_KIND_CALL, __next_call, &__payload).await.map_err(|e| e.to_string())?;
                    let f = __secret_read_frame(&mut __cout).await.map_err(|e| e.to_string())?.ok_or("process exited")?;
                    if f.call_id != __next_call {{ return Err("reply call id mismatch".into()); }}
                    if f.kind == ORCH_KIND_ERROR {{
                        let mut pos = 0;
                        let error = String::wire_decode(&f.payload, &mut pos).ok_or("invalid error reply")?;
                        eprintln!("[orchestrate] landline '{name}' handler '{handler}' failed: {{}}", error);
                        return Ok(Default::default());
                    }}
                    if f.kind != ORCH_KIND_REPLY {{ return Err("expected REPLY".into()); }}
                    Ok({{ {decode} }})
                }}.await;
                match __result {{
                    Ok(value) => {{ let _ = reply_to.send(value); }},
                    Err(__error) => {{
                        eprintln!("[orchestrate] landline '{name}' crashed: {{}}", __error);
                        let _ = __child.kill().await;
                        {recovery}
                        let _ = reply_to.send(Default::default());
                        continue 'restart;
                    }}
                }}
            }}"#, name=name, variant=pascal_case(&h.name), bindings=bindings.join(", "), id=id, encode=encode, handler=h.name, decode=decode, recovery=recovery)
        }).collect::<Vec<_>>().join("\n");
        include_str!("python_mirror.rs.txt")
            .replace("@NAME@", name)
            .replace("@SIGNATURES@", &signatures)
            .replace("@ARMS@", &arms)
    }
}
