#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Keywords
    Let,
    Fn,
    Task,
    Parallel,
    If,
    Else,
    While,
    Return,
    True,
    False,
    Use,
    Module,
    Load,
    LoadForeign,
    Serverlet,
    On,
    Start,
    Process,
    Orchestrator,
    Automatic,
    Trigger,
    OnStart,
    OnStop,
    Struct,

    // Literals
    Identifier(String),
    Int(i64),
    Float(f64),
    Str(String),

    // Operators
    Plus,       // +
    Minus,      // -
    Star,       // *
    Slash,      // /
    EqEq,       // ==
    BangEq,     // !=
    LtEq,       // <=
    GtEq,       // >=
    Lt,         // <
    Gt,         // >
    Eq,         // =
    Arrow,      // ->
    Pipe,       // |>

    // Punctuation
    LParen,     // (
    RParen,     // )
    LBrace,     // {
    RBrace,     // }
    Colon,      // :
    Comma,      // ,
    Semicolon,  // ;
    Dot,        // .
    LBracket,   // [
    RBracket,   // ]

    EOF,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub line: usize,
    pub col: usize,
}

pub struct Lexer {
    input: Vec<char>,
    pos: usize,
    line: usize,
    col: usize,
}

impl Lexer {
    pub fn new(input: &str) -> Self {
        Lexer {
            input: input.chars().collect(),
            pos: 0,
            line: 1,
            col: 1,
        }
    }

    fn peek(&self) -> Option<char> {
        if self.pos < self.input.len() {
            Some(self.input[self.pos])
        } else {
            None
        }
    }

    fn peek_next(&self) -> Option<char> {
        if self.pos + 1 < self.input.len() {
            Some(self.input[self.pos + 1])
        } else {
            None
        }
    }

    fn advance(&mut self) -> Option<char> {
        if self.pos < self.input.len() {
            let c = self.input[self.pos];
            self.pos += 1;
            if c == '\n' {
                self.line += 1;
                self.col = 1;
            } else {
                self.col += 1;
            }
            Some(c)
        } else {
            None
        }
    }

    pub fn tokenize(&mut self) -> Result<Vec<Token>, String> {
        let mut tokens = Vec::new();

        while let Some(c) = self.peek() {
            let start_line = self.line;
            let start_col = self.col;

            if c.is_whitespace() {
                self.advance();
                continue;
            }

            // Inline comment '//'
            if c == '/' && self.peek_next() == Some('/') {
                self.advance(); // first '/'
                self.advance(); // second '/'
                while let Some(nc) = self.peek() {
                    if nc == '\n' {
                        break;
                    }
                    self.advance();
                }
                continue;
            }

            // Numbers: integer or float
            if c.is_ascii_digit() {
                tokens.push(self.read_number(start_line, start_col)?);
                continue;
            }

            // Identifiers / Keywords
            if c.is_alphabetic() || c == '_' {
                tokens.push(self.read_identifier_or_keyword(start_line, start_col));
                continue;
            }

            // String literals
            if c == '"' {
                tokens.push(self.read_string(start_line, start_col)?);
                continue;
            }

            // Operators & Punctuation
            self.advance();
            let kind = match c {
                '(' => TokenKind::LParen,
                ')' => TokenKind::RParen,
                '{' => TokenKind::LBrace,
                '}' => TokenKind::RBrace,
                ':' => TokenKind::Colon,
                ',' => TokenKind::Comma,
                ';' => TokenKind::Semicolon,
                '.' => TokenKind::Dot,
                '[' => TokenKind::LBracket,
                ']' => TokenKind::RBracket,
                '+' => TokenKind::Plus,
                '*' => TokenKind::Star,
                '/' => TokenKind::Slash,
                '-' => {
                    if self.peek() == Some('>') {
                        self.advance();
                        TokenKind::Arrow
                    } else {
                        TokenKind::Minus
                    }
                }
                '=' => {
                    if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::EqEq
                    } else {
                        TokenKind::Eq
                    }
                }
                '!' => {
                    if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::BangEq
                    } else {
                        return Err(format!("Unexpected character '!' at line {}, col {}", start_line, start_col));
                    }
                }
                '<' => {
                    if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::LtEq
                    } else {
                        TokenKind::Lt
                    }
                }
                '>' => {
                    if self.peek() == Some('=') {
                        self.advance();
                        TokenKind::GtEq
                    } else {
                        TokenKind::Gt
                    }
                }
                '|' => {
                    if self.peek() == Some('>') {
                        self.advance();
                        TokenKind::Pipe
                    } else {
                        return Err(format!("Unexpected character '|' at line {}, col {}", start_line, start_col));
                    }
                }
                _ => return Err(format!("Unexpected character '{}' at line {}, col {}", c, start_line, start_col)),
            };

            tokens.push(Token {
                kind,
                line: start_line,
                col: start_col,
            });
        }

        tokens.push(Token {
            kind: TokenKind::EOF,
            line: self.line,
            col: self.col,
        });

        Ok(tokens)
    }

    fn read_number(&mut self, start_line: usize, start_col: usize) -> Result<Token, String> {
        let mut num_str = String::new();
        let mut is_float = false;

        while let Some(c) = self.peek() {
            if c.is_ascii_digit() {
                num_str.push(c);
                self.advance();
            } else if c == '.' {
                if is_float {
                    return Err(format!("Invalid float literal with multiple decimal points at line {}, col {}", start_line, start_col));
                }
                if let Some(next_c) = self.peek_next() {
                    if next_c.is_ascii_digit() {
                        is_float = true;
                        num_str.push(c);
                        self.advance();
                    } else {
                        // Dot followed by non-digit is not part of the number (e.g. member access or syntax error later)
                        break;
                    }
                } else {
                    break;
                }
            } else {
                break;
            }
        }

        let kind = if is_float {
            let val = num_str.parse::<f64>().map_err(|_| format!("Failed to parse float '{}'", num_str))?;
            TokenKind::Float(val)
        } else {
            let val = num_str.parse::<i64>().map_err(|_| format!("Failed to parse integer '{}'", num_str))?;
            TokenKind::Int(val)
        };

        Ok(Token {
            kind,
            line: start_line,
            col: start_col,
        })
    }

    fn read_string(&mut self, start_line: usize, start_col: usize) -> Result<Token, String> {
        self.advance(); // Consume the opening quote
        let mut string = String::new();

        while let Some(c) = self.peek() {
            if c == '"' {
                self.advance(); // Consume closing quote
                return Ok(Token {
                    kind: TokenKind::Str(string),
                    line: start_line,
                    col: start_col,
                });
            } else if c == '\\' {
                self.advance(); // consume '\'
                if let Some(escaped) = self.advance() {
                    match escaped {
                        'n' => string.push('\n'),
                        'r' => string.push('\r'),
                        't' => string.push('\t'),
                        '\\' => string.push('\\'),
                        '"' => string.push('"'),
                        _ => return Err(format!("Invalid escape sequence '\\{}' at line {}, col {}", escaped, self.line, self.col - 1)),
                    }
                } else {
                    return Err(format!("Unterminated string literal starting at line {}, col {}", start_line, start_col));
                }
            } else {
                string.push(c);
                self.advance();
            }
        }

        Err(format!("Unterminated string literal starting at line {}, col {}", start_line, start_col))
    }

    fn read_identifier_or_keyword(&mut self, start_line: usize, start_col: usize) -> Token {
        let mut s = String::new();
        while let Some(c) = self.peek() {
            if c.is_alphanumeric() || c == '_' {
                s.push(c);
                self.advance();
            } else {
                break;
            }
        }

        let kind = match s.as_str() {
            "let" => TokenKind::Let,
            "fn" => TokenKind::Fn,
            "task" => TokenKind::Task,
            "parallel" => TokenKind::Parallel,
            "if" => TokenKind::If,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "return" => TokenKind::Return,
            "true" => TokenKind::True,
            "false" => TokenKind::False,
            "use" => TokenKind::Use,
            "module" => TokenKind::Module,
            "load" => TokenKind::Load,
            "load_foreign" => TokenKind::LoadForeign,
            "serverlet" => TokenKind::Serverlet,
            "on" => TokenKind::On,
            "start" => TokenKind::Start,
            "process" => TokenKind::Process,
            "orchestrator" => TokenKind::Orchestrator,
            "automatic" => TokenKind::Automatic,
            "trigger" => TokenKind::Trigger,
            "on_start" => TokenKind::OnStart,
            "on_stop" => TokenKind::OnStop,
            "struct" => TokenKind::Struct,
            _ => TokenKind::Identifier(s),
        };

        Token {
            kind,
            line: start_line,
            col: start_col,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lexer_basic() {
        let mut lexer = Lexer::new("let x = 10; fn main() {}");
        let tokens = lexer.tokenize().unwrap();
        
        let kinds: Vec<TokenKind> = tokens.into_iter().map(|t| t.kind).collect();
        assert_eq!(
            kinds,
            vec![
                TokenKind::Let,
                TokenKind::Identifier("x".to_string()),
                TokenKind::Eq,
                TokenKind::Int(10),
                TokenKind::Semicolon,
                TokenKind::Fn,
                TokenKind::Identifier("main".to_string()),
                TokenKind::LParen,
                TokenKind::RParen,
                TokenKind::LBrace,
                TokenKind::RBrace,
                TokenKind::EOF,
            ]
        );
    }
}
