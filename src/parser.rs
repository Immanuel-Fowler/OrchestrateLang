use crate::ast::{BinaryOp, Expr, ExprNode, Literal, Param, Stmt, StmtNode, Type, Handler, Span, Spanned};
use crate::lexer::{Token, TokenKind};

pub struct ParseResult {
    pub stmts: Vec<Stmt>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, PartialOrd)]
pub enum Precedence {
    Lowest = 0,
    Assign = 1,     // =
    Pipe = 2,       // |>
    Equality = 3,   // ==, !=
    Comparison = 4, // <, >, <=, >=
    Sum = 5,        // +, -
    Product = 6,    // *, /
    Call = 7,       // f(x)
}

pub struct Parser {
    tokens: Vec<Token>,
    pos: usize,
    pub errors: Vec<String>,
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Parser { tokens, pos: 0, errors: Vec::new() }
    }

    fn peek(&self) -> &Token {
        if self.pos < self.tokens.len() {
            &self.tokens[self.pos]
        } else {
            &self.tokens[self.tokens.len() - 1]
        }
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
        if self.peek().kind == kind {
            self.advance();
            true
        } else {
            false
        }
    }

    fn consume(&mut self, kind: TokenKind, msg: &str) -> Result<&Token, String> {
        let tok = self.peek();
        if tok.kind == kind {
            Ok(self.advance())
        } else {
            Err(format!("{} at line {}, col {}. Found: {:?}", msg, tok.line, tok.col, tok.kind))
        }
    }

    fn is_at_end(&self) -> bool {
        self.peek().kind == TokenKind::EOF
    }

    pub fn parse(&mut self) -> Result<Vec<Stmt>, String> {
        let mut statements = Vec::new();
        while !self.is_at_end() {
            match self.parse_statement() {
                Ok(stmt) => statements.push(stmt),
                Err(e) => {
                    self.errors.push(e);
                    self.skip_to_next_statement_boundary();
                }
            }
        }
        if self.errors.is_empty() {
            Ok(statements)
        } else {
            Err(self.errors.join("\n"))
        }
    }

    fn skip_to_next_statement_boundary(&mut self) {
        while !self.is_at_end() {
            match &self.peek().kind {
                TokenKind::Semicolon => { self.advance(); return; }
                TokenKind::Let | TokenKind::Fn | TokenKind::Task | TokenKind::Process |
                TokenKind::Orchestrator | TokenKind::Parallel | TokenKind::While |
                TokenKind::Return | TokenKind::Trigger | TokenKind::Serverlet |
                TokenKind::Load | TokenKind::LoadForeign | TokenKind::Use |
                TokenKind::Struct | TokenKind::OnStart | TokenKind::OnStop => return,
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
        } else {
            self.parse_expr_statement()?
        };

        // Consume optional semicolon
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
            _ => return Err(format!("Expected string literal for language after 'load_foreign' at line {}, col {}", tok_lang.line, tok_lang.col)),
        };

        let tok_path = self.advance().clone();
        let path = match &tok_path.kind {
            TokenKind::Str(s) => s.clone(),
            _ => return Err(format!("Expected string literal for path after 'load_foreign' at line {}, col {}", tok_path.line, tok_path.col)),
        };

        Ok(StmtNode::LoadForeign { language, path })
    }

    fn parse_serverlet_statement(&mut self) -> Result<StmtNode, String> {
        let tok_ident = self.advance().clone();
        let name = match &tok_ident.kind {
            TokenKind::Identifier(n) => n.clone(),
            _ => return Err(format!("Expected identifier for serverlet name at line {}, col {}", tok_ident.line, tok_ident.col)),
        };

        // Optional `secret` modifier (contextual keyword): runs the serverlet
        // out of process via a mirror, keeping its code out of the orchestrator.
        let mut secret = false;
        if let TokenKind::Identifier(kw) = &self.peek().kind {
            if kw == "secret" {
                self.advance();
                secret = true;
            }
        }

        self.consume(TokenKind::LBrace, "Expected '{' to start serverlet body")?;

        let mut state = Vec::new();
        let mut handlers = Vec::new();

        while self.peek().kind != TokenKind::RBrace && self.peek().kind != TokenKind::EOF {
            if self.match_token(TokenKind::Let) {
                let start_tok = self.peek().clone();
                let span = Span::new(start_tok.line, start_tok.col);
                let node = self.parse_let_statement()?;
                state.push(Spanned { node, span });
                let _ = self.match_token(TokenKind::Semicolon);
            } else if self.match_token(TokenKind::On) {
                handlers.push(self.parse_handler()?);
            } else {
                let tok = self.peek();
                return Err(format!("Expected state variable ('let') or handler ('on') in serverlet, found {:?} at line {}, col {}", tok.kind, tok.line, tok.col));
            }
        }

        self.consume(TokenKind::RBrace, "Expected '}' to end serverlet body")?;

        Ok(StmtNode::Serverlet { name: name.to_string(), state, handlers, secret })
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
        if self.match_token(TokenKind::Arrow) {
            return_type = self.parse_type()?;
        }

        let body = self.parse_block()?;
        Ok(Handler {
            name,
            params,
            return_type,
            body,
        })
    }

    fn parse_let_statement(&mut self) -> Result<StmtNode, String> {
        let tok = self.advance().clone();
        let name = match &tok.kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => return Err(format!("Expected identifier after 'let' at line {}, col {}", tok.line, tok.col)),
        };

        let mut ty = None;
        if self.match_token(TokenKind::Colon) {
            ty = Some(self.parse_type()?);
        }

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

    fn parse_parallel_statement(&mut self) -> Result<StmtNode, String> {
        self.consume(TokenKind::LBrace, "Expected '{' after parallel keyword")?;
        let mut stmts = Vec::new();
        while self.peek().kind != TokenKind::RBrace && self.peek().kind != TokenKind::EOF {
            stmts.push(self.parse_statement()?);
        }
        self.consume(TokenKind::RBrace, "Expected '}' to end parallel block")?;
        Ok(StmtNode::Parallel(stmts))
    }

    fn parse_fn_statement(&mut self) -> Result<StmtNode, String> {
        let tok = self.advance().clone();
        let name = match &tok.kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => return Err(format!("Expected identifier after 'fn' at line {}, col {}", tok.line, tok.col)),
        };

        self.consume(TokenKind::LParen, "Expected '(' after function name")?;
        let params = self.parse_params()?;
        self.consume(TokenKind::RParen, "Expected ')' after parameters")?;

        let mut return_type = Type::Void;
        if self.match_token(TokenKind::Arrow) {
            return_type = self.parse_type()?;
        }

        let body = self.parse_block()?;
        Ok(StmtNode::FnDecl {
            name,
            params,
            return_type,
            body,
        })
    }

    fn parse_task_statement(&mut self) -> Result<StmtNode, String> {
        let tok = self.advance().clone();
        let name = match &tok.kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => return Err(format!("Expected identifier after 'task' at line {}, col {}", tok.line, tok.col)),
        };

        self.consume(TokenKind::LParen, "Expected '(' after task name")?;
        let params = self.parse_params()?;
        self.consume(TokenKind::RParen, "Expected ')' after parameters")?;

        let mut return_type = Type::Void;
        if self.match_token(TokenKind::Arrow) {
            return_type = self.parse_type()?;
        }

        let body = self.parse_block()?;
        Ok(StmtNode::TaskDecl {
            name,
            params,
            return_type,
            body,
        })
    }

    fn parse_process_statement(&mut self) -> Result<StmtNode, String> {
        let tok = self.advance().clone();
        let name = match &tok.kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => return Err(format!("Expected identifier after 'process' at line {}, col {}", tok.line, tok.col)),
        };

        self.consume(TokenKind::LParen, "Expected '(' after process name")?;
        let params = self.parse_params()?;
        self.consume(TokenKind::RParen, "Expected ')' after parameters")?;

        let mut return_type = Type::Void;
        if self.match_token(TokenKind::Arrow) {
            return_type = self.parse_type()?;
        }

        let body = self.parse_block()?;
        Ok(StmtNode::ProcessDecl {
            name,
            params,
            return_type,
            body,
        })
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
        if self.match_token(TokenKind::Arrow) {
            return_type = self.parse_type()?;
        }

        let body = self.parse_block()?;
        Ok(StmtNode::OrchestratorDecl {
            name,
            params,
            return_type,
            body,
        })
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
        let body = self.parse_block()?;
        Ok(StmtNode::OnStart(body))
    }

    fn parse_on_stop_statement(&mut self) -> Result<StmtNode, String> {
        let body = self.parse_block()?;
        Ok(StmtNode::OnStop(body))
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

    fn parse_params(&mut self) -> Result<Vec<Param>, String> {
        let mut params = Vec::new();
        if self.peek().kind == TokenKind::RParen {
            return Ok(params);
        }

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
                    if self.match_token(TokenKind::Comma) {
                        continue;
                    }
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
            TokenKind::Identifier(s) => match s.as_str() {
                "int" => Ok(Type::Int),
                "float" => Ok(Type::Float),
                "string" => Ok(Type::Str),
                "bool" => Ok(Type::Bool),
                "void" => Ok(Type::Void),
                _ => Ok(Type::Named(s.clone())),
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

        Ok(lhs)
    }

    fn peek_precedence(&self) -> Precedence {
        match &self.peek().kind {
            TokenKind::Eq => Precedence::Assign,
            TokenKind::Pipe => Precedence::Pipe,
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
        // Current token is '{'. Peek ahead: identifier followed by ':' means struct literal.
        if self.pos + 1 < self.tokens.len() {
            if let TokenKind::Identifier(_) = &self.tokens[self.pos + 1].kind {
                if self.pos + 2 < self.tokens.len() {
                    return self.tokens[self.pos + 2].kind == TokenKind::Colon;
                }
            }
        }
        false
    }

    fn parse_prefix_node(&mut self) -> Result<ExprNode, String> {
        let tok = self.peek().clone();
        match &tok.kind {
            TokenKind::Int(v) => {
                self.advance();
                Ok(ExprNode::Literal(Literal::Int(*v)))
            }
            TokenKind::Float(v) => {
                self.advance();
                Ok(ExprNode::Literal(Literal::Float(*v)))
            }
            TokenKind::Str(v) => {
                self.advance();
                Ok(ExprNode::Literal(Literal::Str(v.clone())))
            }
            TokenKind::True => {
                self.advance();
                Ok(ExprNode::Literal(Literal::Bool(true)))
            }
            TokenKind::False => {
                self.advance();
                Ok(ExprNode::Literal(Literal::Bool(false)))
            }
            TokenKind::Identifier(name) => {
                let name = name.clone();
                self.advance();
                // Check for struct literal: Name { field: val, ... }
                // Disambiguate from a plain block by peeking: { identifier : ...
                if self.peek().kind == TokenKind::LBrace && self.is_struct_literal_start() {
                    self.advance(); // consume '{'
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
                    Ok(ExprNode::StructLiteral { name, fields })
                } else {
                    Ok(ExprNode::Identifier(name))
                }
            }
            TokenKind::LParen => {
                self.advance(); // consume '('
                let expr = self.parse_expression(Precedence::Lowest)?;
                self.consume(TokenKind::RParen, "Expected ')' after grouped expression")?;
                Ok(expr.node)
            }
            TokenKind::If => {
                self.advance(); // consume 'if'
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
            TokenKind::LBrace => {
                Ok(self.parse_block()?.node)
            }
            TokenKind::Start => {
                self.advance(); // consume 'start'
                let expr = self.parse_expression(Precedence::Lowest)?;
                match expr.node {
                    ExprNode::Call { callee, args } => {
                        Ok(ExprNode::StartServerlet { name: callee, args })
                    }
                    ExprNode::ModuleCall { module_local_name, function, args } => {
                        Ok(ExprNode::StartServerlet { name: format!("{}::{}", module_local_name, function), args })
                    }
                    _ => {
                        Ok(ExprNode::StartProcess { target: Box::new(expr) })
                    }
                }
            }
            TokenKind::Automatic => {
                self.advance(); // consume 'automatic'
                let body = self.parse_block()?;
                Ok(ExprNode::AutomaticBlock { body: Box::new(body) })
            }
            TokenKind::On => {
                self.advance(); // consume 'on'
                let tok_evt = self.advance().clone();
                let event_name = match &tok_evt.kind {
                    TokenKind::Identifier(s) => s.clone(),
                    _ => return Err(format!("Expected event name after 'on' at line {}, col {}", tok_evt.line, tok_evt.col)),
                };
                self.consume(TokenKind::LParen, "Expected '(' after event name")?;
                let params = self.parse_params()?;
                self.consume(TokenKind::RParen, "Expected ')' after parameters")?;
                let body = self.parse_block()?;
                Ok(ExprNode::TriggeredBlock {
                    event_name,
                    params,
                    body: Box::new(body),
                })
            }
            TokenKind::LBracket => {
                self.advance(); // consume '['
                let mut elements = Vec::new();
                if self.peek().kind != TokenKind::RBracket {
                    elements.push(self.parse_expression(Precedence::Lowest)?);
                    while self.match_token(TokenKind::Comma) {
                        if self.peek().kind == TokenKind::RBracket {
                            break; // trailing comma allowed
                        }
                        elements.push(self.parse_expression(Precedence::Lowest)?);
                    }
                }
                self.consume(TokenKind::RBracket, "Expected ']' to close array literal")?;
                Ok(ExprNode::ArrayLiteral(elements))
            }
            _ => Err(format!("Expected expression at line {}, col {}, found {:?}", tok.line, tok.col, tok.kind)),
        }
    }

    fn parse_infix_node(&mut self, lhs: Expr) -> Result<ExprNode, String> {
        let tok = self.peek().clone();
        let op_prec = self.peek_precedence();
        match &tok.kind {
            TokenKind::Eq => {
                self.advance(); // consume '='
                let rhs = self.parse_expression(Precedence::Assign)?;
                Ok(ExprNode::Binary {
                    op: BinaryOp::Assign,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                })
            }
            TokenKind::LParen => {
                self.advance(); // consume '('
                let args = self.parse_call_args()?;
                if let ExprNode::Identifier(name) = &lhs.node {
                    Ok(ExprNode::Call { callee: name.to_string(), args })
                } else {
                    Err(format!("Expected identifier before '(', found {:?} at line {}, col {}", lhs, tok.line, tok.col))
                }
            }
            TokenKind::Pipe => {
                self.advance(); // consume '|>'
                let rhs = self.parse_expression(Precedence::Pipe)?;
                Ok(ExprNode::Pipeline {
                    value: Box::new(lhs),
                    function: Box::new(rhs),
                })
            }
            TokenKind::Dot => {
                self.advance(); // consume '.'
                let tok_fn = self.advance().clone();
                let field_or_fn = match &tok_fn.kind {
                    TokenKind::Identifier(name) => name.clone(),
                    _ => return Err(format!("Expected identifier after '.' at line {}, col {}", tok_fn.line, tok_fn.col)),
                };

                if self.peek().kind == TokenKind::LParen {
                    // Module call: identifier.method(args)
                    self.advance(); // consume '('
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
                    // Field access: expr.field
                    Ok(ExprNode::FieldAccess {
                        object: Box::new(lhs),
                        field: field_or_fn,
                    })
                }
            }
            _ => {
                self.advance(); // consume operator
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
                    _ => unreachable!(),
                };
                let rhs = self.parse_expression(op_prec)?;
                Ok(ExprNode::Binary {
                    op,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                })
            }
        }
    }

    fn parse_call_args(&mut self) -> Result<Vec<Expr>, String> {
        let mut args = Vec::new();
        if self.peek().kind == TokenKind::RParen {
            self.advance();
            return Ok(args);
        }
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

    #[test]
    fn test_parser_basic() {
        let mut lexer = Lexer::new("let x = 5 + 10 * 2;");
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let ast = parser.parse().unwrap();

        assert_eq!(ast.len(), 1);
        if let StmtNode::Let { name, ty, value } = &ast[0].node {
            assert_eq!(name, "x");
            assert_eq!(ty, &None);
            
            // Value should be parsed as 5 + (10 * 2) due to precedence
            if let ExprNode::Binary { op, lhs, rhs } = &value.node {
                assert_eq!(op, &BinaryOp::Add);
                assert!(matches!(&lhs.node, ExprNode::Literal(Literal::Int(5))));
                if let ExprNode::Binary { op: op2, lhs: lhs2, rhs: rhs2 } = &rhs.node {
                    assert_eq!(op2, &BinaryOp::Mul);
                    assert!(matches!(&lhs2.node, ExprNode::Literal(Literal::Int(10))));
                    assert!(matches!(&rhs2.node, ExprNode::Literal(Literal::Int(2))));
                } else {
                    panic!("Expected binary right side");
                }
            } else {
                panic!("Expected binary expression");
            }
        } else {
            panic!("Expected Let statement");
        }
    }

    #[test]
    fn test_parser_modules() {
        let mut lexer = Lexer::new("use module frontend: \"react_app\"; let res = frontend.prompt(\"Name\");");
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let ast = parser.parse().unwrap();

        assert_eq!(ast.len(), 2);
        assert_eq!(
            ast[0].node,
            StmtNode::UseModule {
                local_name: "frontend".to_string(),
                module_name: "react_app".to_string(),
            }
        );

        if let StmtNode::Let { name, ty, value } = &ast[1].node {
            assert_eq!(name, "res");
            assert_eq!(ty, &None);
            if let ExprNode::ModuleCall { module_local_name, function, args } = &value.node {
                assert_eq!(module_local_name, "frontend");
                assert_eq!(function, "prompt");
                assert_eq!(args.len(), 1);
                assert!(matches!(&args[0].node, ExprNode::Literal(Literal::Str(s)) if s == "Name"));
            } else {
                panic!("Expected ModuleCall expression");
            }
        } else {
            panic!("Expected Let statement");
        }
    }
}
