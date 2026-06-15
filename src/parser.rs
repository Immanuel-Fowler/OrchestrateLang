use crate::ast::{
    BinaryOp, EnumVariant, Expr, ExprNode, Handler, Literal, MatchArm, MatchPattern, Param,
    RestartPolicy, Span, Spanned, Stmt, StmtNode, StringPart, Type,
};
use crate::lexer::{Token, TokenKind};

pub struct ParseResult {
    pub stmts: Vec<Stmt>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub enum Precedence {
    Lowest = 0,
    Assign = 1,
    Pipe = 2,
    Or = 3,
    And = 4,
    Equality = 5,
    Comparison = 6,
    Sum = 7,
    Product = 8,
    Call = 9,
}

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    pub errors: Vec<String>,
    type_param_ctx: std::collections::HashSet<String>,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Parser { tokens, pos: 0, errors: Vec::new(), type_param_ctx: std::collections::HashSet::new() }
    }

    fn peek(&self) -> &Token {
        if self.pos < self.tokens.len() { &self.tokens[self.pos] } else { &self.tokens[self.tokens.len() - 1] }
    }

    fn peek_at(&self, offset: usize) -> &Token {
        let idx = self.pos + offset;
        if idx < self.tokens.len() { &self.tokens[idx] } else { &self.tokens[self.tokens.len() - 1] }
    }

    fn advance(&mut self) -> &Token {
        if self.pos < self.tokens.len() {
            let tok = &self.tokens[self.pos];
            self.pos += 1;
            tok
        } else {
            &self.tokens[self.tokens.len() - 1]
        }
    }

    fn match_token(&mut self, kind: TokenKind) -> bool {
        if self.peek().kind == kind { self.advance(); true } else { false }
    }

    fn consume(&mut self, kind: TokenKind, msg: &str) -> Result<&Token, String> {
        let tok = self.peek();
        if tok.kind == kind { Ok(self.advance()) } else {
            Err(format!("{} at line {}, col {}. Found: {:?}", msg, tok.line, tok.col, tok.kind))
        }
    }

    fn is_at_end(&self) -> bool { self.peek().kind == TokenKind::EOF }

    pub fn parse(&mut self) -> Result<Vec<Stmt>, String> {
        let mut statements = Vec::new();
        while !self.is_at_end() {
            match self.parse_statement() {
                Ok(stmt) => statements.push(stmt),
                Err(e) => { self.errors.push(e); self.skip_to_next_statement_boundary(); }
            }
        }
        if self.errors.is_empty() { Ok(statements) } else { Err(self.errors.join("\n")) }
    }

    fn skip_to_next_statement_boundary(&mut self) {
        while !self.is_at_end() {
            match &self.peek().kind {
                TokenKind::Semicolon => { self.advance(); return; }
                TokenKind::Let | TokenKind::Fn | TokenKind::Task | TokenKind::Process |
                TokenKind::Orchestrator | TokenKind::Parallel | TokenKind::While | TokenKind::For |
                TokenKind::Return | TokenKind::Break | TokenKind::Continue |
                TokenKind::Trigger | TokenKind::Serverlet |
                TokenKind::Load | TokenKind::LoadForeign | TokenKind::Use |
                TokenKind::Struct | TokenKind::Enum | TokenKind::OnStart | TokenKind::OnStop => return,
                TokenKind::EOF => return,
                _ => { self.advance(); }
            }
        }
    }

    fn parse_statement(&mut self) -> Result<Stmt, String> {
        let start_tok = self.peek().clone();
        let span = Span::new(start_tok.line, start_tok.col);
        let node = if self.match_token(TokenKind::Use) {
            self.parse_use_statement()?
        } else if self.match_token(TokenKind::Load) {
            self.parse_load_statement()?
        } else if self.match_token(TokenKind::LoadForeign) {
            self.parse_load_foreign_statement()?
        } else if self.match_token(TokenKind::Serverlet) {
            self.parse_serverlet_statement()?
        } else if self.match_token(TokenKind::Let) {
            self.parse_let_statement()?
        } else if self.match_token(TokenKind::Return) {
            self.parse_return_statement()?
        } else if self.match_token(TokenKind::While) {
            self.parse_while_statement()?
        } else if self.match_token(TokenKind::For) {
            self.parse_for_statement()?
        } else if self.match_token(TokenKind::Parallel) {
            self.parse_parallel_statement()?
        } else if self.match_token(TokenKind::Fn) {
            self.parse_fn_statement()?
        } else if self.match_token(TokenKind::Task) {
            self.parse_task_statement()?
        } else if self.match_token(TokenKind::Process) {
            self.parse_process_statement()?
        } else if self.match_token(TokenKind::Orchestrator) {
            self.parse_orchestrator_statement()?
        } else if self.match_token(TokenKind::Trigger) {
            self.parse_trigger_statement()?
        } else if self.match_token(TokenKind::OnStart) {
            self.parse_on_start_statement()?
        } else if self.match_token(TokenKind::OnStop) {
            self.parse_on_stop_statement()?
        } else if self.match_token(TokenKind::Struct) {
            self.parse_struct_decl()?
        } else if self.match_token(TokenKind::Enum) {
            self.parse_enum_decl()?
        } else if self.match_token(TokenKind::Break) {
            StmtNode::Break
        } else if self.match_token(TokenKind::Continue) {
            StmtNode::Continue
        } else {
            self.parse_expr_statement()?
        };
        let _ = self.match_token(TokenKind::Semicolon);
        Ok(Spanned { node, span })
    }

    fn parse_use_statement(&mut self) -> Result<StmtNode, String> {
        self.consume(TokenKind::Module, "Expected 'module' after 'use'")?;
        let tok_ident = self.advance().clone();
        let local_name = match &tok_ident.kind {
            TokenKind::Identifier(name) => name.clone(),
            _ => return Err(format!("Expected identifier for local module name at line {}, col {}", tok_ident.line, tok_ident.col)),
        };
        self.consume(TokenKind::Colon, "Expected ':' after local module name")?;
        let tok_str = self.advance().clone();
        let module_name = match &tok_str.kind {
            TokenKind::Str(name) => name.clone(),
            _ => return Err(format!("Expected string literal for module name at line {}, col {}", tok_str.line, tok_str.col)),
        };
        Ok(StmtNode::UseModule { local_name, module_name })
    }

    fn parse_load_statement(&mut self) -> Result<StmtNode, String> {
        let tok = self.advance().clone();
        let path = match &tok.kind {
            TokenKind::Str(s) => s.clone(),
            _ => return Err(format!("Expected string literal path after 'load' at line {}, col {}", tok.line, tok.col)),
        };
        Ok(StmtNode::Load { path })
    }

    fn parse_load_foreign_statement(&mut self) -> Result<StmtNode, String> {
        let tok_lang = self.advance().clone();
        let language = match &tok_lang.kind {
            TokenKind::Str(s) => s.clone(),
            _ => return Err(format!("Expected string literal for language at line {}, col {}", tok_lang.line, tok_lang.col)),
        };
        let tok_path = self.advance().clone();
        let path = match &tok_path.kind {
            TokenKind::Str(s) => s.clone(),
            _ => return Err(format!("Expected string literal for path at line {}, col {}", tok_path.line, tok_path.col)),
        };
        Ok(StmtNode::LoadForeign { language, path })
    }

    fn parse_serverlet_statement(&mut self) -> Result<StmtNode, String> {
        let tok_ident = self.advance().clone();
        let name = match &tok_ident.kind {
            TokenKind::Identifier(n) => n.clone(),
            _ => return Err(format!("Expected identifier for serverlet name at line {}, col {}", tok_ident.line, tok_ident.col)),
        };
        let mut secret = false;
        if let TokenKind::Identifier(kw) = &self.peek().kind {
            if kw == "secret" { self.advance(); secret = true; }
        }
        self.consume(TokenKind::LBrace, "Expected '{' to start serverlet body")?;
        let mut state = Vec::new();
        let mut handlers = Vec::new();
        let mut crash_handler: Option<(String, Box<Expr>)> = None;
        while self.peek().kind != TokenKind::RBrace && self.peek().kind != TokenKind::EOF {
            if self.match_token(TokenKind::Let) {
                let start_tok = self.peek().clone();
                let span = Span::new(start_tok.line, start_tok.col);
                let node = self.parse_let_statement()?;
                state.push(Spanned { node, span });
                let _ = self.match_token(TokenKind::Semicolon);
            } else if self.match_token(TokenKind::On) {
                handlers.push(self.parse_handler()?);
            } else if self.match_token(TokenKind::OnCrash) {
                let err_tok = self.advance().clone();
                let err_name = match &err_tok.kind {
                    TokenKind::Identifier(s) => s.clone(),
                    _ => return Err(format!("Expected identifier after 'on_crash' at line {}, col {}", err_tok.line, err_tok.col)),
                };
                let handler_body = self.parse_block()?;
                crash_handler = Some((err_name, Box::new(handler_body)));
            } else {
                let tok = self.peek();
                return Err(format!("Expected 'let', 'on', or 'on_crash' in serverlet at line {}, col {}", tok.line, tok.col));
            }
        }
        self.consume(TokenKind::RBrace, "Expected '}' to end serverlet body")?;
        Ok(StmtNode::Serverlet { name, state, handlers, secret, crash_handler })
    }

    fn parse_handler(&mut self) -> Result<Handler, String> {
        let tok = self.advance().clone();
        let name = match &tok.kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => return Err(format!("Expected identifier after 'on' at line {}, col {}", tok.line, tok.col)),
        };
        self.consume(TokenKind::LParen, "Expected '(' after handler name")?;
        let params = self.parse_params()?;
        self.consume(TokenKind::RParen, "Expected ')' after parameters")?;
        let mut return_type = Type::Void;
        if self.match_token(TokenKind::Arrow) { return_type = self.parse_type()?; }
        let body = self.parse_block()?;
        Ok(Handler { name, params, return_type, body })
    }

    fn parse_let_statement(&mut self) -> Result<StmtNode, String> {
        let tok = self.advance().clone();
        let name = match &tok.kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => return Err(format!("Expected identifier after 'let' at line {}, col {}", tok.line, tok.col)),
        };
        let mut ty = None;
        if self.match_token(TokenKind::Colon) { ty = Some(self.parse_type()?); }
        self.consume(TokenKind::Eq, "Expected '=' in variable declaration")?;
        let value = self.parse_expression(Precedence::Lowest)?;
        Ok(StmtNode::Let { name, ty, value })
    }

    fn parse_return_statement(&mut self) -> Result<StmtNode, String> {
        let next_kind = &self.peek().kind;
        if next_kind == &TokenKind::Semicolon || next_kind == &TokenKind::RBrace || next_kind == &TokenKind::EOF {
            Ok(StmtNode::Return(None))
        } else {
            let value = self.parse_expression(Precedence::Lowest)?;
            Ok(StmtNode::Return(Some(value)))
        }
    }

    fn parse_while_statement(&mut self) -> Result<StmtNode, String> {
        let cond = self.parse_expression(Precedence::Lowest)?;
        let body = self.parse_block()?;
        Ok(StmtNode::While { cond, body })
    }

    fn parse_for_statement(&mut self) -> Result<StmtNode, String> {
        // for var in iter { body }
        // for i, var in iter { body }
        let first_tok = self.advance().clone();
        let first_name = match &first_tok.kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => return Err(format!("Expected identifier after 'for' at line {}, col {}", first_tok.line, first_tok.col)),
        };
        let (var, index_var) = if self.match_token(TokenKind::Comma) {
            // for i, item in ...
            let var_tok = self.advance().clone();
            let var_name = match &var_tok.kind {
                TokenKind::Identifier(s) => s.clone(),
                _ => return Err(format!("Expected variable name after ',' in for loop at line {}, col {}", var_tok.line, var_tok.col)),
            };
            (var_name, Some(first_name))
        } else {
            (first_name, None)
        };
        let in_tok = self.peek().clone();
        if !matches!(&in_tok.kind, TokenKind::In) {
            return Err(format!("Expected 'in' in for loop at line {}, col {}", in_tok.line, in_tok.col));
        }
        self.advance(); // consume 'in'
        let iter = self.parse_expression(Precedence::Lowest)?;
        let body = self.parse_block()?;
        Ok(StmtNode::ForIn { var, index_var, iter, body })
    }

    fn parse_parallel_statement(&mut self) -> Result<StmtNode, String> {
        self.consume(TokenKind::LBrace, "Expected '{' after parallel keyword")?;
        let mut stmts = Vec::new();
        while self.peek().kind != TokenKind::RBrace && self.peek().kind != TokenKind::EOF {
            stmts.push(self.parse_statement()?);
        }
        self.consume(TokenKind::RBrace, "Expected '}' to end parallel block")?;
        Ok(StmtNode::Parallel(stmts))
    }

    /// Parse optional generic type parameters: <T, U, V>
    fn parse_type_params(&mut self) -> Result<Vec<String>, String> {
        self.type_param_ctx.clear();
        if self.peek().kind != TokenKind::Lt {
            return Ok(Vec::new());
        }
        self.advance(); // consume <
        let mut params = Vec::new();
        loop {
            let tok = self.advance().clone();
            match &tok.kind {
                TokenKind::Identifier(s) => params.push(s.clone()),
                _ => return Err(format!("Expected type parameter name at line {}, col {}", tok.line, tok.col)),
            }
            if !self.match_token(TokenKind::Comma) { break; }
        }
        self.consume(TokenKind::Gt, "Expected '>' after type parameters")?;
        self.type_param_ctx = params.iter().cloned().collect();
        Ok(params)
    }

    fn parse_fn_statement(&mut self) -> Result<StmtNode, String> {
        // If next token is '(' it's a closure expression at statement level
        if self.peek().kind == TokenKind::LParen {
            let span = Span::new(self.peek().line, self.peek().col);
            let closure = self.parse_closure_expr()?;
            return Ok(StmtNode::Expr(Spanned { node: closure, span }));
        }
        let tok = self.advance().clone();
        let name = match &tok.kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => return Err(format!("Expected identifier after 'fn' at line {}, col {}", tok.line, tok.col)),
        };
        let type_params = self.parse_type_params()?;
        self.consume(TokenKind::LParen, "Expected '(' after function name")?;
        let params = self.parse_params()?;
        self.consume(TokenKind::RParen, "Expected ')' after parameters")?;
        let mut return_type = Type::Void;
        if self.match_token(TokenKind::Arrow) { return_type = self.parse_type()?; }
        let body = self.parse_block()?;
        Ok(StmtNode::FnDecl { name, type_params, params, return_type, body })
    }

    fn parse_task_statement(&mut self) -> Result<StmtNode, String> {
        let tok = self.advance().clone();
        let name = match &tok.kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => return Err(format!("Expected identifier after 'task' at line {}, col {}", tok.line, tok.col)),
        };
        let type_params = self.parse_type_params()?;
        self.consume(TokenKind::LParen, "Expected '(' after task name")?;
        let params = self.parse_params()?;
        self.consume(TokenKind::RParen, "Expected ')' after parameters")?;
        let mut return_type = Type::Void;
        if self.match_token(TokenKind::Arrow) { return_type = self.parse_type()?; }
        let body = self.parse_block()?;
        Ok(StmtNode::TaskDecl { name, type_params, params, return_type, body })
    }

    fn parse_process_statement(&mut self) -> Result<StmtNode, String> {
        let tok = self.advance().clone();
        let name = match &tok.kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => return Err(format!("Expected identifier after 'process' at line {}, col {}", tok.line, tok.col)),
        };
        let type_params = self.parse_type_params()?;
        self.consume(TokenKind::LParen, "Expected '(' after process name")?;
        let params = self.parse_params()?;
        self.consume(TokenKind::RParen, "Expected ')' after parameters")?;
        let mut return_type = Type::Void;
        if self.match_token(TokenKind::Arrow) { return_type = self.parse_type()?; }
        let body = self.parse_block()?;
        Ok(StmtNode::ProcessDecl { name, type_params, params, return_type, body })
    }

    fn parse_orchestrator_statement(&mut self) -> Result<StmtNode, String> {
        let tok = self.advance().clone();
        let name = match &tok.kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => return Err(format!("Expected identifier after 'orchestrator' at line {}, col {}", tok.line, tok.col)),
        };
        self.consume(TokenKind::LParen, "Expected '(' after orchestrator name")?;
        let params = self.parse_params()?;
        self.consume(TokenKind::RParen, "Expected ')' after parameters")?;
        let mut return_type = Type::Void;
        if self.match_token(TokenKind::Arrow) { return_type = self.parse_type()?; }
        let body = self.parse_block()?;
        Ok(StmtNode::OrchestratorDecl { name, params, return_type, body })
    }

    fn parse_trigger_statement(&mut self) -> Result<StmtNode, String> {
        let tok = self.advance().clone();
        let event_name = match &tok.kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => return Err(format!("Expected event name after 'trigger' at line {}, col {}", tok.line, tok.col)),
        };
        self.consume(TokenKind::LParen, "Expected '(' after event name")?;
        let args = self.parse_call_args()?;
        Ok(StmtNode::Trigger { event_name, args })
    }

    fn parse_on_start_statement(&mut self) -> Result<StmtNode, String> {
        Ok(StmtNode::OnStart(self.parse_block()?))
    }

    fn parse_on_stop_statement(&mut self) -> Result<StmtNode, String> {
        Ok(StmtNode::OnStop(self.parse_block()?))
    }

    fn parse_struct_decl(&mut self) -> Result<StmtNode, String> {
        let tok = self.advance().clone();
        let name = match &tok.kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => return Err(format!("Expected struct name at line {}, col {}", tok.line, tok.col)),
        };
        self.consume(TokenKind::LBrace, "Expected '{' after struct name")?;
        let mut fields = Vec::new();
        while self.peek().kind != TokenKind::RBrace && self.peek().kind != TokenKind::EOF {
            let ftok = self.advance().clone();
            let fname = match &ftok.kind {
                TokenKind::Identifier(s) => s.clone(),
                _ => return Err(format!("Expected field name at line {}, col {}", ftok.line, ftok.col)),
            };
            self.consume(TokenKind::Colon, "Expected ':' after field name")?;
            let fty = self.parse_type()?;
            fields.push((fname, fty));
            let _ = self.match_token(TokenKind::Comma);
        }
        self.consume(TokenKind::RBrace, "Expected '}' to end struct definition")?;
        Ok(StmtNode::StructDef { name, fields })
    }

    fn parse_enum_decl(&mut self) -> Result<StmtNode, String> {
        let tok = self.advance().clone();
        let name = match &tok.kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => return Err(format!("Expected enum name at line {}, col {}", tok.line, tok.col)),
        };
        self.consume(TokenKind::LBrace, "Expected '{' after enum name")?;
        let mut variants = Vec::new();
        while self.peek().kind != TokenKind::RBrace && self.peek().kind != TokenKind::EOF {
            let vtok = self.advance().clone();
            let vname = match &vtok.kind {
                TokenKind::Identifier(s) => s.clone(),
                _ => return Err(format!("Expected variant name in enum at line {}, col {}", vtok.line, vtok.col)),
            };
            let payload = if self.match_token(TokenKind::LParen) {
                let ty = self.parse_type()?;
                self.consume(TokenKind::RParen, "Expected ')' after enum variant payload type")?;
                Some(ty)
            } else { None };
            variants.push(EnumVariant { name: vname, payload });
            let _ = self.match_token(TokenKind::Comma);
        }
        self.consume(TokenKind::RBrace, "Expected '}' to end enum definition")?;
        Ok(StmtNode::EnumDef { name, variants })
    }

    fn parse_params(&mut self) -> Result<Vec<Param>, String> {
        let mut params = Vec::new();
        if self.peek().kind == TokenKind::RParen { return Ok(params); }
        let tok = self.advance().clone();
        let name = match &tok.kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => return Err(format!("Expected parameter name at line {}, col {}", tok.line, tok.col)),
        };
        self.consume(TokenKind::Colon, "Expected ':' after parameter name")?;
        let ty = self.parse_type()?;
        params.push(Param { name, ty });
        while self.match_token(TokenKind::Comma) {
            let tok = self.advance().clone();
            let name = match &tok.kind {
                TokenKind::Identifier(s) => s.clone(),
                _ => return Err(format!("Expected parameter name at line {}, col {}", tok.line, tok.col)),
            };
            self.consume(TokenKind::Colon, "Expected ':' after parameter name")?;
            let ty = self.parse_type()?;
            params.push(Param { name, ty });
        }
        Ok(params)
    }

    fn parse_type(&mut self) -> Result<Type, String> {
        let mut ty = self.parse_base_type()?;
        while self.match_token(TokenKind::LBracket) {
            let mut init_vals = Vec::new();
            if self.peek().kind != TokenKind::RBracket {
                loop {
                    let tok = self.advance();
                    let val = match &tok.kind {
                        TokenKind::Identifier(s) => s.clone(),
                        _ => return Err(format!("Expected identifier inside brackets at line {}, col {}", tok.line, tok.col)),
                    };
                    init_vals.push(val);
                    if self.match_token(TokenKind::Comma) { continue; }
                    break;
                }
            }
            self.consume(TokenKind::RBracket, "Expected ']' after '['")?;
            ty = Type::Array(Box::new(ty), init_vals);
        }
        Ok(ty)
    }

    fn parse_base_type(&mut self) -> Result<Type, String> {
        let tok = self.advance().clone();
        match &tok.kind {
            TokenKind::Process => Ok(Type::Process),
            TokenKind::Fn => {
                // fn(T1, T2) -> R
                self.consume(TokenKind::LParen, "Expected '(' after 'fn' in type")?;
                let mut param_types = Vec::new();
                if self.peek().kind != TokenKind::RParen {
                    param_types.push(self.parse_type()?);
                    while self.match_token(TokenKind::Comma) {
                        param_types.push(self.parse_type()?);
                    }
                }
                self.consume(TokenKind::RParen, "Expected ')' in fn type")?;
                let ret_ty = if self.match_token(TokenKind::Arrow) { self.parse_type()? } else { Type::Void };
                Ok(Type::Fn(param_types, Box::new(ret_ty)))
            }
            TokenKind::Identifier(s) => match s.as_str() {
                "int" => Ok(Type::Int),
                "float" => Ok(Type::Float),
                "string" => Ok(Type::Str),
                "bool" => Ok(Type::Bool),
                "void" => Ok(Type::Void),
                "option" => {
                    self.consume(TokenKind::Lt, "Expected '<' after 'option'")?;
                    let inner = self.parse_type()?;
                    self.consume(TokenKind::Gt, "Expected '>' after option inner type")?;
                    Ok(Type::Option(Box::new(inner)))
                }
                "result" => {
                    self.consume(TokenKind::Lt, "Expected '<' after 'result'")?;
                    let inner = self.parse_type()?;
                    self.consume(TokenKind::Gt, "Expected '>' after result inner type")?;
                    Ok(Type::Result(Box::new(inner)))
                }
                s => {
                    // Check the active generic declaration context first, then fall back to
                    // single-uppercase heuristic for backward compatibility
                    if self.type_param_ctx.contains(s) {
                        Ok(Type::TypeParam(s.to_string()))
                    } else if s.len() == 1 && s.chars().next().map(|c| c.is_uppercase()).unwrap_or(false) {
                        Ok(Type::TypeParam(s.to_string()))
                    } else {
                        Ok(Type::Named(s.to_string()))
                    }
                }
            },
            _ => Err(format!("Expected type name, found {:?} at line {}, col {}", tok.kind, tok.line, tok.col)),
        }
    }

    fn parse_expr_statement(&mut self) -> Result<StmtNode, String> {
        let expr = self.parse_expression(Precedence::Lowest)?;
        Ok(StmtNode::Expr(expr))
    }

    fn parse_block(&mut self) -> Result<Expr, String> {
        let start_tok = self.peek().clone();
        let span = Span::new(start_tok.line, start_tok.col);
        self.consume(TokenKind::LBrace, "Expected '{' to start block")?;
        let mut stmts = Vec::new();
        while self.peek().kind != TokenKind::RBrace && self.peek().kind != TokenKind::EOF {
            stmts.push(self.parse_statement()?);
        }
        self.consume(TokenKind::RBrace, "Expected '}' to end block")?;
        Ok(Spanned { node: ExprNode::Block(stmts), span })
    }

    fn parse_expression(&mut self, prec: Precedence) -> Result<Expr, String> {
        let mut lhs = self.parse_prefix()?;
        while prec < self.peek_precedence() {
            lhs = self.parse_infix(lhs)?;
        }
        // ? operator as postfix
        if self.peek().kind == TokenKind::Question {
            let span = Span::new(self.peek().line, self.peek().col);
            self.advance();
            lhs = Spanned { node: ExprNode::Propagate(Box::new(lhs)), span };
        }
        Ok(lhs)
    }

    fn peek_precedence(&self) -> Precedence {
        match &self.peek().kind {
            TokenKind::Eq => Precedence::Assign,
            TokenKind::Pipe => Precedence::Pipe,
            TokenKind::OrOr => Precedence::Or,
            TokenKind::AndAnd => Precedence::And,
            TokenKind::EqEq | TokenKind::BangEq => Precedence::Equality,
            TokenKind::Lt | TokenKind::Gt | TokenKind::LtEq | TokenKind::GtEq => Precedence::Comparison,
            TokenKind::Plus | TokenKind::Minus => Precedence::Sum,
            TokenKind::Star | TokenKind::Slash => Precedence::Product,
            TokenKind::LParen | TokenKind::Dot => Precedence::Call,
            _ => Precedence::Lowest,
        }
    }

    fn parse_prefix(&mut self) -> Result<Expr, String> {
        let start_tok = self.peek().clone();
        let span = Span::new(start_tok.line, start_tok.col);
        let node = self.parse_prefix_node()?;
        Ok(Spanned { node, span })
    }

    fn parse_infix(&mut self, lhs: Expr) -> Result<Expr, String> {
        let start_tok = self.peek().clone();
        let span = Span::new(start_tok.line, start_tok.col);
        let node = self.parse_infix_node(lhs)?;
        Ok(Spanned { node, span })
    }

    fn is_struct_literal_start(&self) -> bool {
        if self.pos + 1 < self.tokens.len() {
            if let TokenKind::Identifier(_) = &self.tokens[self.pos + 1].kind {
                if self.pos + 2 < self.tokens.len() {
                    return self.tokens[self.pos + 2].kind == TokenKind::Colon;
                }
            }
        }
        false
    }

    fn parse_match_expr(&mut self) -> Result<ExprNode, String> {
        let value = self.parse_expression(Precedence::Lowest)?;
        self.consume(TokenKind::LBrace, "Expected '{' after match value")?;
        let mut arms = Vec::new();
        while self.peek().kind != TokenKind::RBrace && self.peek().kind != TokenKind::EOF {
            let pattern = self.parse_match_pattern()?;
            self.consume(TokenKind::FatArrow, "Expected '=>' after match pattern")?;
            let body = self.parse_expression(Precedence::Lowest)?;
            let _ = self.match_token(TokenKind::Comma);
            arms.push(MatchArm { pattern, body: Box::new(body) });
        }
        self.consume(TokenKind::RBrace, "Expected '}' to close match expression")?;
        Ok(ExprNode::Match { value: Box::new(value), arms })
    }

    fn parse_match_pattern(&mut self) -> Result<MatchPattern, String> {
        let inner = self.parse_match_pattern_inner()?;
        // Check for guard: `pattern if condition`
        if self.peek().kind == TokenKind::If {
            self.advance(); // consume 'if'
            let cond = self.parse_expression(Precedence::Lowest)?;
            Ok(MatchPattern::Guard { inner: Box::new(inner), condition: Box::new(cond) })
        } else {
            Ok(inner)
        }
    }

    fn parse_match_pattern_inner(&mut self) -> Result<MatchPattern, String> {
        let tok = self.peek().clone();
        match &tok.kind {
            // Wildcard
            TokenKind::Identifier(s) if s == "_" => {
                self.advance();
                Ok(MatchPattern::Wildcard)
            }
            // Integer literal
            TokenKind::Int(n) => {
                let n = *n;
                self.advance();
                Ok(MatchPattern::Literal(Literal::Int(n)))
            }
            // Negative integer: -N
            TokenKind::Minus => {
                self.advance();
                let num_tok = self.peek().clone();
                match &num_tok.kind {
                    TokenKind::Int(n) => { let n = *n; self.advance(); Ok(MatchPattern::Literal(Literal::Int(-n))) }
                    TokenKind::Float(f) => { let f = *f; self.advance(); Ok(MatchPattern::Literal(Literal::Float(-f))) }
                    _ => Err(format!("Expected number after '-' in match pattern at line {}, col {}", tok.line, tok.col)),
                }
            }
            // Float literal
            TokenKind::Float(f) => {
                let f = *f;
                self.advance();
                Ok(MatchPattern::Literal(Literal::Float(f)))
            }
            // String literal
            TokenKind::Str(s) => {
                let s = s.clone();
                self.advance();
                Ok(MatchPattern::Literal(Literal::Str(s)))
            }
            // Bool literals
            TokenKind::True => { self.advance(); Ok(MatchPattern::Literal(Literal::Bool(true))) }
            TokenKind::False => { self.advance(); Ok(MatchPattern::Literal(Literal::Bool(false))) }
            // Identifier: either `Name::Variant` enum pattern or bare binding
            TokenKind::Identifier(_) => {
                let name_tok = self.advance().clone();
                let name = match &name_tok.kind {
                    TokenKind::Identifier(s) => s.clone(),
                    _ => unreachable!(),
                };
                if self.peek().kind == TokenKind::ColonColon {
                    // EnumName::Variant or EnumName::Variant(binding)
                    self.advance(); // consume '::'
                    let variant_tok = self.advance().clone();
                    let variant_name = match &variant_tok.kind {
                        TokenKind::Identifier(s) => s.clone(),
                        _ => return Err(format!("Expected variant name in match pattern at line {}, col {}", variant_tok.line, variant_tok.col)),
                    };
                    let binding = if self.match_token(TokenKind::LParen) {
                        let b_tok = self.advance().clone();
                        let b_name = match &b_tok.kind {
                            TokenKind::Identifier(s) => s.clone(),
                            _ => return Err(format!("Expected binding name in match pattern at line {}, col {}", b_tok.line, b_tok.col)),
                        };
                        self.consume(TokenKind::RParen, "Expected ')' after binding name")?;
                        Some(b_name)
                    } else { None };
                    Ok(MatchPattern::EnumVariant { enum_name: name, variant_name, binding })
                } else {
                    // Bare identifier: variable binding
                    Ok(MatchPattern::Binding(name))
                }
            }
            _ => {
                let tok = self.advance().clone();
                Err(format!("Expected match pattern at line {}, col {}. Found: {:?}", tok.line, tok.col, tok.kind))
            }
        }
    }

    fn parse_closure_expr(&mut self) -> Result<ExprNode, String> {
        self.consume(TokenKind::LParen, "Expected '(' in closure")?;
        let params = self.parse_params()?;
        self.consume(TokenKind::RParen, "Expected ')' after closure parameters")?;
        let return_type = if self.match_token(TokenKind::Arrow) { Some(self.parse_type()?) } else { None };
        let body = self.parse_block()?;
        Ok(ExprNode::Closure { params, return_type, body: Box::new(body) })
    }

    /// Parse string interpolation: "hello {name}, you have {count} items"
    /// Returns StringInterp node if any `{...}` found, otherwise Literal::Str
    fn parse_string_with_interp(&self, raw: &str, base_span: Span) -> Result<ExprNode, String> {
        if !raw.contains('{') {
            return Ok(ExprNode::Literal(Literal::Str(raw.to_string())));
        }
        let mut parts: Vec<StringPart> = Vec::new();
        let mut current_literal = String::new();
        let chars: Vec<char> = raw.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            if chars[i] == '{' && i + 1 < chars.len() && chars[i + 1] == '{' {
                current_literal.push('{'); i += 2;
            } else if chars[i] == '}' && i + 1 < chars.len() && chars[i + 1] == '}' {
                current_literal.push('}'); i += 2;
            } else if chars[i] == '{' {
                if !current_literal.is_empty() {
                    parts.push(StringPart::Literal(current_literal.clone()));
                    current_literal.clear();
                }
                i += 1;
                let mut expr_src = String::new();
                let mut depth = 1usize;
                while i < chars.len() {
                    match chars[i] {
                        '{' => { depth += 1; expr_src.push('{'); }
                        '}' => {
                            depth -= 1;
                            if depth == 0 { break; }
                            expr_src.push('}');
                        }
                        c => expr_src.push(c),
                    }
                    i += 1;
                }
                if i >= chars.len() {
                    return Err(format!("Unterminated '{{' in string interpolation at line {}, col {}", base_span.line, base_span.col));
                }
                i += 1; // consume closing }
                // Re-lex and re-parse the inner expression
                let mut inner_lexer = crate::lexer::Lexer::new(&expr_src);
                let inner_tokens = inner_lexer.tokenize()
                    .map_err(|e| format!("Error in string interpolation at line {}: {}", base_span.line, e))?;
                let mut inner_parser = Parser::new(inner_tokens);
                let inner_expr = inner_parser.parse_expression(Precedence::Lowest)
                    .map_err(|e| format!("Error in string interpolation at line {}: {}", base_span.line, e))?;
                parts.push(StringPart::Expr(Box::new(inner_expr)));
            } else {
                current_literal.push(chars[i]);
                i += 1;
            }
        }
        if !current_literal.is_empty() { parts.push(StringPart::Literal(current_literal)); }
        if parts.is_empty() { return Ok(ExprNode::Literal(Literal::Str(String::new()))); }
        // Single literal → plain string
        if parts.len() == 1 {
            if let StringPart::Literal(s) = &parts[0] {
                return Ok(ExprNode::Literal(Literal::Str(s.clone())));
            }
        }
        Ok(ExprNode::StringInterp { parts })
    }

    fn parse_prefix_node(&mut self) -> Result<ExprNode, String> {
        let tok = self.peek().clone();
        match &tok.kind {
            TokenKind::Int(v) => { self.advance(); Ok(ExprNode::Literal(Literal::Int(*v))) }
            TokenKind::Float(v) => { self.advance(); Ok(ExprNode::Literal(Literal::Float(*v))) }
            TokenKind::Str(v) => {
                let v = v.clone();
                let span = Span::new(tok.line, tok.col);
                self.advance();
                self.parse_string_with_interp(&v, span)
            }
            TokenKind::True => { self.advance(); Ok(ExprNode::Literal(Literal::Bool(true))) }
            TokenKind::False => { self.advance(); Ok(ExprNode::Literal(Literal::Bool(false))) }
            TokenKind::Fn => {
                // Closure expression: fn(params) -> type { body }
                self.advance();
                self.parse_closure_expr()
            }
            TokenKind::Identifier(name) => {
                let name = name.clone();
                self.advance();
                // none literal
                if name == "none" && self.peek().kind != TokenKind::LParen {
                    return Ok(ExprNode::NoneLiteral);
                }
                // some(x), ok(x), err(x)
                if (name == "some" || name == "ok" || name == "err") && self.peek().kind == TokenKind::LParen {
                    self.advance();
                    let inner = self.parse_expression(Precedence::Lowest)?;
                    self.consume(TokenKind::RParen, &format!("Expected ')' after '{}(' argument", name))?;
                    return match name.as_str() {
                        "some" => Ok(ExprNode::SomeLiteral(Box::new(inner))),
                        "ok" => Ok(ExprNode::OkLiteral(Box::new(inner))),
                        "err" => Ok(ExprNode::ErrLiteral(Box::new(inner))),
                        _ => unreachable!(),
                    };
                }
                // EnumName::Variant
                if self.peek().kind == TokenKind::ColonColon {
                    self.advance();
                    let variant_tok = self.advance().clone();
                    let variant_name = match &variant_tok.kind {
                        TokenKind::Identifier(s) => s.clone(),
                        _ => return Err(format!("Expected variant name after '::' at line {}, col {}", variant_tok.line, variant_tok.col)),
                    };
                    let payload = if self.peek().kind == TokenKind::LParen {
                        self.advance();
                        let expr = self.parse_expression(Precedence::Lowest)?;
                        self.consume(TokenKind::RParen, "Expected ')' after enum variant payload")?;
                        Some(Box::new(expr))
                    } else { None };
                    return Ok(ExprNode::EnumVariantLiteral { enum_name: name, variant_name, payload });
                }
                // Struct literal
                if self.peek().kind == TokenKind::LBrace && self.is_struct_literal_start() {
                    self.advance();
                    let mut fields = Vec::new();
                    while self.peek().kind != TokenKind::RBrace && self.peek().kind != TokenKind::EOF {
                        let ftok = self.advance().clone();
                        let fname = match &ftok.kind {
                            TokenKind::Identifier(s) => s.clone(),
                            _ => return Err(format!("Expected field name at line {}, col {}", ftok.line, ftok.col)),
                        };
                        self.consume(TokenKind::Colon, "Expected ':' after field name in struct literal")?;
                        let val = self.parse_expression(Precedence::Lowest)?;
                        fields.push((fname, Box::new(val)));
                        let _ = self.match_token(TokenKind::Comma);
                    }
                    self.consume(TokenKind::RBrace, "Expected '}' to end struct literal")?;
                    return Ok(ExprNode::StructLiteral { name, fields });
                }
                Ok(ExprNode::Identifier(name))
            }
            TokenKind::LParen => {
                self.advance();
                let expr = self.parse_expression(Precedence::Lowest)?;
                self.consume(TokenKind::RParen, "Expected ')' after grouped expression")?;
                Ok(expr.node)
            }
            TokenKind::If => {
                self.advance();
                let cond = self.parse_expression(Precedence::Lowest)?;
                let then_branch = self.parse_block()?;
                let mut else_branch = None;
                if self.match_token(TokenKind::Else) {
                    let next_tok = self.peek().clone();
                    if next_tok.kind == TokenKind::If {
                        else_branch = Some(self.parse_prefix()?);
                    } else if next_tok.kind == TokenKind::LBrace {
                        else_branch = Some(self.parse_block()?);
                    } else {
                        return Err(format!("Expected 'if' or '{{' after 'else' at line {}, col {}", next_tok.line, next_tok.col));
                    }
                }
                Ok(ExprNode::If {
                    cond: Box::new(cond),
                    then_branch: Box::new(then_branch),
                    else_branch: else_branch.map(Box::new),
                })
            }
            TokenKind::LBrace => Ok(self.parse_block()?.node),
            TokenKind::Start => {
                self.advance();
                let expr = self.parse_expression(Precedence::Lowest)?;
                match expr.node {
                    ExprNode::Call { callee, args } => Ok(ExprNode::StartServerlet { name: callee, args }),
                    ExprNode::ModuleCall { module_local_name, function, args } =>
                        Ok(ExprNode::StartServerlet { name: format!("{}::{}", module_local_name, function), args }),
                    _ => Ok(ExprNode::StartProcess { target: Box::new(expr) }),
                }
            }
            TokenKind::Automatic => {
                self.advance();
                let restart_policy = if self.peek().kind == TokenKind::LParen {
                    self.advance();
                    let kw_tok = self.advance().clone();
                    let kw = match &kw_tok.kind {
                        TokenKind::Identifier(s) => s.clone(),
                        _ => return Err(format!("Expected 'restart' inside automatic(...) at line {}, col {}", kw_tok.line, kw_tok.col)),
                    };
                    if kw != "restart" {
                        return Err(format!("Expected 'restart' inside automatic(...), got '{}' at line {}", kw, kw_tok.line));
                    }
                    self.consume(TokenKind::Colon, "Expected ':' after 'restart'")?;
                    let policy_tok = self.advance().clone();
                    let policy = match &policy_tok.kind {
                        TokenKind::Identifier(s) => match s.as_str() {
                            "always" => RestartPolicy::Always,
                            "never" => RestartPolicy::Never,
                            _ => return Err(format!("Unknown restart policy '{}' at line {}", s, policy_tok.line)),
                        },
                        TokenKind::Int(n) => RestartPolicy::MaxAttempts(*n as u32),
                        _ => return Err(format!("Expected restart policy at line {}", policy_tok.line)),
                    };
                    self.consume(TokenKind::RParen, "Expected ')' after automatic restart policy")?;
                    policy
                } else { RestartPolicy::Always };
                let body = self.parse_block()?;
                let crash_handler = if self.peek().kind == TokenKind::OnCrash {
                    self.advance();
                    let err_tok = self.advance().clone();
                    let err_name = match &err_tok.kind {
                        TokenKind::Identifier(s) => s.clone(),
                        _ => return Err(format!("Expected identifier after 'on_crash' at line {}, col {}", err_tok.line, err_tok.col)),
                    };
                    let handler_body = self.parse_block()?;
                    Some((err_name, Box::new(handler_body)))
                } else { None };
                Ok(ExprNode::AutomaticBlock { body: Box::new(body), restart_policy, crash_handler })
            }
            TokenKind::On => {
                self.advance();
                let tok_evt = self.advance().clone();
                let event_name = match &tok_evt.kind {
                    TokenKind::Identifier(s) => s.clone(),
                    _ => return Err(format!("Expected event name after 'on' at line {}, col {}", tok_evt.line, tok_evt.col)),
                };
                self.consume(TokenKind::LParen, "Expected '(' after event name")?;
                let params = self.parse_params()?;
                self.consume(TokenKind::RParen, "Expected ')' after parameters")?;
                let body = self.parse_block()?;
                Ok(ExprNode::TriggeredBlock { event_name, params, body: Box::new(body) })
            }
            TokenKind::LBracket => {
                self.advance();
                let mut elements = Vec::new();
                if self.peek().kind != TokenKind::RBracket {
                    elements.push(self.parse_expression(Precedence::Lowest)?);
                    while self.match_token(TokenKind::Comma) {
                        if self.peek().kind == TokenKind::RBracket { break; }
                        elements.push(self.parse_expression(Precedence::Lowest)?);
                    }
                }
                self.consume(TokenKind::RBracket, "Expected ']' to close array literal")?;
                Ok(ExprNode::ArrayLiteral(elements))
            }
            TokenKind::Try => {
                self.advance();
                let body = self.parse_block()?;
                self.consume(TokenKind::Catch, "Expected 'catch' after try block")?;
                let err_tok = self.advance().clone();
                let err_name = match &err_tok.kind {
                    TokenKind::Identifier(s) => s.clone(),
                    _ => return Err(format!("Expected error binding name after 'catch' at line {}, col {}", err_tok.line, err_tok.col)),
                };
                let handler = self.parse_block()?;
                Ok(ExprNode::TryCatch { body: Box::new(body), err_name, handler: Box::new(handler) })
            }
            TokenKind::Match => {
                self.advance();
                self.parse_match_expr()
            }
            _ => Err(format!("Expected expression at line {}, col {}, found {:?}", tok.line, tok.col, tok.kind)),
        }
    }

    fn parse_infix_node(&mut self, lhs: Expr) -> Result<ExprNode, String> {
        let tok = self.peek().clone();
        let op_prec = self.peek_precedence();
        match &tok.kind {
            TokenKind::Eq => {
                self.advance();
                let rhs = self.parse_expression(Precedence::Assign)?;
                Ok(ExprNode::Binary { op: BinaryOp::Assign, lhs: Box::new(lhs), rhs: Box::new(rhs) })
            }
            TokenKind::LParen => {
                self.advance();
                let args = self.parse_call_args()?;
                if let ExprNode::Identifier(name) = &lhs.node {
                    Ok(ExprNode::Call { callee: name.to_string(), args })
                } else {
                    Err(format!("Expected identifier before '(', found {:?} at line {}, col {}", lhs, tok.line, tok.col))
                }
            }
            TokenKind::Pipe => {
                self.advance();
                let rhs = self.parse_expression(Precedence::Pipe)?;
                Ok(ExprNode::Pipeline { value: Box::new(lhs), function: Box::new(rhs) })
            }
            TokenKind::Dot => {
                self.advance();
                let tok_fn = self.advance().clone();
                let field_or_fn = match &tok_fn.kind {
                    TokenKind::Identifier(name) => name.clone(),
                    _ => return Err(format!("Expected identifier after '.' at line {}, col {}", tok_fn.line, tok_fn.col)),
                };
                if self.peek().kind == TokenKind::LParen {
                    self.advance();
                    let args = self.parse_call_args()?;
                    if let ExprNode::Identifier(module_local_name) = &lhs.node {
                        Ok(ExprNode::ModuleCall {
                            module_local_name: module_local_name.to_string(),
                            function: field_or_fn,
                            args,
                        })
                    } else {
                        Err(format!("Expected identifier before '.', found {:?} at line {}, col {}", lhs, tok.line, tok.col))
                    }
                } else {
                    Ok(ExprNode::FieldAccess { object: Box::new(lhs), field: field_or_fn })
                }
            }
            _ => {
                self.advance();
                let op = match &tok.kind {
                    TokenKind::Plus => BinaryOp::Add,
                    TokenKind::Minus => BinaryOp::Sub,
                    TokenKind::Star => BinaryOp::Mul,
                    TokenKind::Slash => BinaryOp::Div,
                    TokenKind::EqEq => BinaryOp::Eq,
                    TokenKind::BangEq => BinaryOp::Ne,
                    TokenKind::Lt => BinaryOp::Lt,
                    TokenKind::Gt => BinaryOp::Gt,
                    TokenKind::LtEq => BinaryOp::Le,
                    TokenKind::GtEq => BinaryOp::Ge,
                    TokenKind::AndAnd => BinaryOp::And,
                    TokenKind::OrOr => BinaryOp::Or,
                    _ => unreachable!(),
                };
                let rhs = self.parse_expression(op_prec)?;
                Ok(ExprNode::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) })
            }
        }
    }

    fn parse_call_args(&mut self) -> Result<Vec<Expr>, String> {
        let mut args = Vec::new();
        if self.peek().kind == TokenKind::RParen { self.advance(); return Ok(args); }
        args.push(self.parse_expression(Precedence::Lowest)?);
        while self.match_token(TokenKind::Comma) {
            args.push(self.parse_expression(Precedence::Lowest)?);
        }
        self.consume(TokenKind::RParen, "Expected ')' after call arguments")?;
        Ok(args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Lexer;

    fn parse(src: &str) -> Vec<Stmt> {
        let mut lexer = Lexer::new(src);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        parser.parse().unwrap()
    }

    #[test]
    fn test_parser_basic() {
        let ast = parse("let x = 5 + 10 * 2;");
        assert_eq!(ast.len(), 1);
        if let StmtNode::Let { name, .. } = &ast[0].node { assert_eq!(name, "x"); }
        else { panic!("Expected Let statement"); }
    }

    #[test]
    fn test_parser_modules() {
        let ast = parse("use module frontend: \"react_app\"; let res = frontend.prompt(\"Name\");");
        assert_eq!(ast.len(), 2);
        assert_eq!(ast[0].node, StmtNode::UseModule { local_name: "frontend".to_string(), module_name: "react_app".to_string() });
    }

    #[test]
    fn test_parser_enum() {
        let ast = parse("enum Status { Ok, Failed(string), Pending(int) }");
        if let StmtNode::EnumDef { name, variants } = &ast[0].node {
            assert_eq!(name, "Status");
            assert_eq!(variants.len(), 3);
            assert_eq!(variants[1].name, "Failed");
        } else { panic!("Expected EnumDef"); }
    }

    #[test]
    fn test_parser_for_loop() {
        let ast = parse("for x in items { print(x) }");
        assert!(matches!(&ast[0].node, StmtNode::ForIn { var, index_var: None, .. } if var == "x"));
    }

    #[test]
    fn test_parser_for_indexed() {
        let ast = parse("for i, x in items { print(x) }");
        if let StmtNode::ForIn { var, index_var, .. } = &ast[0].node {
            assert_eq!(var, "x");
            assert_eq!(index_var.as_deref(), Some("i"));
        } else { panic!("Expected ForIn"); }
    }

    #[test]
    fn test_parser_closure() {
        let ast = parse("let f = fn(x: int) -> int { x * 2 }");
        if let StmtNode::Let { value, .. } = &ast[0].node {
            assert!(matches!(&value.node, ExprNode::Closure { .. }));
        } else { panic!("Expected Let with closure"); }
    }

    #[test]
    fn test_parser_string_interp() {
        let ast = parse("let s = \"hello {name}\";");
        if let StmtNode::Let { value, .. } = &ast[0].node {
            assert!(matches!(&value.node, ExprNode::StringInterp { .. }));
        } else { panic!("Expected Let with StringInterp"); }
    }

    #[test]
    fn test_parser_generic_fn() {
        let ast = parse("fn identity<T>(x: T) -> T { x }");
        if let StmtNode::FnDecl { type_params, .. } = &ast[0].node {
            assert_eq!(type_params, &vec!["T".to_string()]);
        } else { panic!("Expected FnDecl with type_params"); }
    }

    #[test]
    fn test_parser_option_type() {
        let ast = parse("fn maybe() -> option<int> { none }");
        if let StmtNode::FnDecl { return_type, .. } = &ast[0].node {
            assert_eq!(return_type, &Type::Option(Box::new(Type::Int)));
        } else { panic!(); }
    }

    #[test]
    fn test_parser_result_type() {
        let ast = parse("fn fallible() -> result<string> { ok(\"yes\") }");
        if let StmtNode::FnDecl { return_type, .. } = &ast[0].node {
            assert_eq!(return_type, &Type::Result(Box::new(Type::Str)));
        } else { panic!(); }
    }

    #[test]
    fn test_parser_logical_ops() {
        let ast = parse("let r = true && false || true;");
        assert!(matches!(&ast[0].node, StmtNode::Let { .. }));
    }

    #[test]
    fn test_parser_fn_type() {
        let ast = parse("let f: fn(int) -> int = fn(x: int) -> int { x };");
        if let StmtNode::Let { ty: Some(ty), .. } = &ast[0].node {
            assert!(matches!(ty, Type::Fn(_, _)));
        } else { panic!("Expected typed let with Fn type"); }
    }
}
