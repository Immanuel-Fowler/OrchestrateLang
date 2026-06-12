use crate::ast::{BinaryOp, Expr, Literal, Param, Stmt, Type, Handler};
use crate::lexer::{Token, TokenKind};

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
}

impl Parser {
    pub fn new(tokens: Vec<Token>) -> Self {
        Parser { tokens, pos: 0 }
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
            statements.push(self.parse_statement()?);
        }
        Ok(statements)
    }

    fn parse_statement(&mut self) -> Result<Stmt, String> {
        let stmt = if self.match_token(TokenKind::Use) {
            self.parse_use_statement()?
        } else if self.match_token(TokenKind::Load) {
            self.parse_load_statement()?
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
        } else {
            self.parse_expr_statement()?
        };

        // Consume optional semicolon
        let _ = self.match_token(TokenKind::Semicolon);

        Ok(stmt)
    }

    fn parse_use_statement(&mut self) -> Result<Stmt, String> {
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

        Ok(Stmt::UseModule { local_name, module_name })
    }

    fn parse_load_statement(&mut self) -> Result<Stmt, String> {
        let tok = self.advance().clone();
        let path = match &tok.kind {
            TokenKind::Str(s) => s.clone(),
            _ => return Err(format!("Expected string literal path after 'load' at line {}, col {}", tok.line, tok.col)),
        };
        Ok(Stmt::Load { path })
    }

    fn parse_serverlet_statement(&mut self) -> Result<Stmt, String> {
        let tok_ident = self.advance().clone();
        let name = match &tok_ident.kind {
            TokenKind::Identifier(n) => n.clone(),
            _ => return Err(format!("Expected identifier for serverlet name at line {}, col {}", tok_ident.line, tok_ident.col)),
        };

        self.consume(TokenKind::LBrace, "Expected '{' to start serverlet body")?;

        let mut state = Vec::new();
        let mut handlers = Vec::new();

        while self.peek().kind != TokenKind::RBrace && self.peek().kind != TokenKind::EOF {
            if self.match_token(TokenKind::Let) {
                state.push(self.parse_let_statement()?);
                let _ = self.match_token(TokenKind::Semicolon);
            } else if self.match_token(TokenKind::On) {
                handlers.push(self.parse_handler()?);
            } else {
                let tok = self.peek();
                return Err(format!("Expected state variable ('let') or handler ('on') in serverlet, found {:?} at line {}, col {}", tok.kind, tok.line, tok.col));
            }
        }

        self.consume(TokenKind::RBrace, "Expected '}' to end serverlet body")?;

        Ok(Stmt::Serverlet { name, state, handlers })
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

    fn parse_let_statement(&mut self) -> Result<Stmt, String> {
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
        Ok(Stmt::Let { name, ty, value })
    }

    fn parse_return_statement(&mut self) -> Result<Stmt, String> {
        let next_kind = &self.peek().kind;
        if next_kind == &TokenKind::Semicolon || next_kind == &TokenKind::RBrace || next_kind == &TokenKind::EOF {
            Ok(Stmt::Return(None))
        } else {
            let value = self.parse_expression(Precedence::Lowest)?;
            Ok(Stmt::Return(Some(value)))
        }
    }

    fn parse_while_statement(&mut self) -> Result<Stmt, String> {
        let cond = self.parse_expression(Precedence::Lowest)?;
        let body = self.parse_block()?;
        Ok(Stmt::While { cond, body })
    }

    fn parse_parallel_statement(&mut self) -> Result<Stmt, String> {
        self.consume(TokenKind::LBrace, "Expected '{' after parallel keyword")?;
        let mut stmts = Vec::new();
        while self.peek().kind != TokenKind::RBrace && self.peek().kind != TokenKind::EOF {
            stmts.push(self.parse_statement()?);
        }
        self.consume(TokenKind::RBrace, "Expected '}' to end parallel block")?;
        Ok(Stmt::Parallel(stmts))
    }

    fn parse_fn_statement(&mut self) -> Result<Stmt, String> {
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
        Ok(Stmt::FnDecl {
            name,
            params,
            return_type,
            body,
        })
    }

    fn parse_task_statement(&mut self) -> Result<Stmt, String> {
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
        Ok(Stmt::TaskDecl {
            name,
            params,
            return_type,
            body,
        })
    }

    fn parse_process_statement(&mut self) -> Result<Stmt, String> {
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
        Ok(Stmt::ProcessDecl {
            name,
            params,
            return_type,
            body,
        })
    }

    fn parse_orchestrator_statement(&mut self) -> Result<Stmt, String> {
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
        Ok(Stmt::OrchestratorDecl {
            name,
            params,
            return_type,
            body,
        })
    }

    fn parse_trigger_statement(&mut self) -> Result<Stmt, String> {
        let tok = self.advance().clone();
        let event_name = match &tok.kind {
            TokenKind::Identifier(s) => s.clone(),
            _ => return Err(format!("Expected event name after 'trigger' at line {}, col {}", tok.line, tok.col)),
        };
        self.consume(TokenKind::LParen, "Expected '(' after event name")?;
        let args = self.parse_call_args()?;
        Ok(Stmt::Trigger { event_name, args })
    }

    fn parse_on_start_statement(&mut self) -> Result<Stmt, String> {
        let body = self.parse_block()?;
        Ok(Stmt::OnStart(body))
    }

    fn parse_on_stop_statement(&mut self) -> Result<Stmt, String> {
        let body = self.parse_block()?;
        Ok(Stmt::OnStop(body))
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
                _ => Err(format!("Unknown type '{}' at line {}, col {}", s, tok.line, tok.col)),
            },
            _ => Err(format!("Expected type name, found {:?} at line {}, col {}", tok.kind, tok.line, tok.col)),
        }
    }

    fn parse_expr_statement(&mut self) -> Result<Stmt, String> {
        let expr = self.parse_expression(Precedence::Lowest)?;
        Ok(Stmt::Expr(expr))
    }

    fn parse_block(&mut self) -> Result<Expr, String> {
        self.consume(TokenKind::LBrace, "Expected '{' to start block")?;
        let mut stmts = Vec::new();
        while self.peek().kind != TokenKind::RBrace && self.peek().kind != TokenKind::EOF {
            stmts.push(self.parse_statement()?);
        }
        self.consume(TokenKind::RBrace, "Expected '}' to end block")?;
        Ok(Expr::Block(stmts))
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
        let tok = self.peek().clone();
        match &tok.kind {
            TokenKind::Int(v) => {
                self.advance();
                Ok(Expr::Literal(Literal::Int(*v)))
            }
            TokenKind::Float(v) => {
                self.advance();
                Ok(Expr::Literal(Literal::Float(*v)))
            }
            TokenKind::Str(v) => {
                self.advance();
                Ok(Expr::Literal(Literal::Str(v.clone())))
            }
            TokenKind::True => {
                self.advance();
                Ok(Expr::Literal(Literal::Bool(true)))
            }
            TokenKind::False => {
                self.advance();
                Ok(Expr::Literal(Literal::Bool(false)))
            }
            TokenKind::Identifier(name) => {
                self.advance();
                Ok(Expr::Identifier(name.clone()))
            }
            TokenKind::LParen => {
                self.advance(); // consume '('
                let expr = self.parse_expression(Precedence::Lowest)?;
                self.consume(TokenKind::RParen, "Expected ')' after grouped expression")?;
                Ok(expr)
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
                Ok(Expr::If {
                    cond: Box::new(cond),
                    then_branch: Box::new(then_branch),
                    else_branch: else_branch.map(Box::new),
                })
            }
            TokenKind::LBrace => {
                self.parse_block()
            }
            TokenKind::Start => {
                self.advance(); // consume 'start'
                let expr = self.parse_expression(Precedence::Lowest)?;
                match expr {
                    Expr::Call { callee, args } => {
                        Ok(Expr::StartServerlet { name: callee, args })
                    }
                    Expr::ModuleCall { module_local_name, function, args } => {
                        Ok(Expr::StartServerlet { name: format!("{}::{}", module_local_name, function), args })
                    }
                    _ => {
                        Ok(Expr::StartProcess { target: Box::new(expr) })
                    }
                }
            }
            TokenKind::Automatic => {
                self.advance(); // consume 'automatic'
                let body = self.parse_block()?;
                Ok(Expr::AutomaticBlock { body: Box::new(body) })
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
                Ok(Expr::TriggeredBlock {
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
                Ok(Expr::ArrayLiteral(elements))
            }
            _ => Err(format!("Expected expression at line {}, col {}, found {:?}", tok.line, tok.col, tok.kind)),
        }
    }

    fn parse_infix(&mut self, lhs: Expr) -> Result<Expr, String> {
        let tok = self.peek().clone();
        let op_prec = self.peek_precedence();
        match &tok.kind {
            TokenKind::Eq => {
                self.advance(); // consume '='
                let rhs = self.parse_expression(Precedence::Assign)?;
                Ok(Expr::Binary {
                    op: BinaryOp::Assign,
                    lhs: Box::new(lhs),
                    rhs: Box::new(rhs),
                })
            }
            TokenKind::LParen => {
                self.advance(); // consume '('
                let args = self.parse_call_args()?;
                if let Expr::Identifier(name) = lhs {
                    Ok(Expr::Call { callee: name, args })
                } else {
                    Err(format!("Expected identifier before '(', found {:?} at line {}, col {}", lhs, tok.line, tok.col))
                }
            }
            TokenKind::Pipe => {
                self.advance(); // consume '|>'
                let rhs = self.parse_expression(Precedence::Pipe)?;
                Ok(Expr::Pipeline {
                    value: Box::new(lhs),
                    function: Box::new(rhs),
                })
            }
            TokenKind::Dot => {
                self.advance(); // consume '.'
                let tok_fn = self.advance().clone();
                let function = match &tok_fn.kind {
                    TokenKind::Identifier(name) => name.clone(),
                    _ => return Err(format!("Expected function name after '.' at line {}, col {}", tok_fn.line, tok_fn.col)),
                };

                self.consume(TokenKind::LParen, "Expected '(' after module function name")?;
                let args = self.parse_call_args()?;

                if let Expr::Identifier(module_local_name) = lhs {
                    Ok(Expr::ModuleCall {
                        module_local_name,
                        function,
                        args,
                    })
                } else {
                    Err(format!("Expected identifier before '.', found {:?} at line {}, col {}", lhs, tok.line, tok.col))
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
                Ok(Expr::Binary {
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
        if let Stmt::Let { name, ty, value } = &ast[0] {
            assert_eq!(name, "x");
            assert_eq!(ty, &None);
            
            // Value should be parsed as 5 + (10 * 2) due to precedence
            if let Expr::Binary { op, lhs, rhs } = value {
                assert_eq!(op, &BinaryOp::Add);
                assert!(matches!(**lhs, Expr::Literal(Literal::Int(5))));
                if let Expr::Binary { op: op2, lhs: lhs2, rhs: rhs2 } = &**rhs {
                    assert_eq!(op2, &BinaryOp::Mul);
                    assert!(matches!(**lhs2, Expr::Literal(Literal::Int(10))));
                    assert!(matches!(**rhs2, Expr::Literal(Literal::Int(2))));
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
            ast[0],
            Stmt::UseModule {
                local_name: "frontend".to_string(),
                module_name: "react_app".to_string(),
            }
        );

        if let Stmt::Let { name, ty, value } = &ast[1] {
            assert_eq!(name, "res");
            assert_eq!(ty, &None);
            if let Expr::ModuleCall { module_local_name, function, args } = value {
                assert_eq!(module_local_name, "frontend");
                assert_eq!(function, "prompt");
                assert_eq!(args.len(), 1);
                assert!(matches!(&args[0], Expr::Literal(Literal::Str(s)) if s == "Name"));
            } else {
                panic!("Expected ModuleCall expression");
            }
        } else {
            panic!("Expected Let statement");
        }
    }
}
