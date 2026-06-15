/// Minimal OrchestrateLang LSP server.
/// Communicates via JSON-RPC over stdin/stdout using the Language Server Protocol.
/// Supports: initialize, textDocument/didOpen, textDocument/didChange,
///           textDocument/hover, shutdown, exit.

use std::collections::HashMap;
use std::io::{self, BufRead, Write};
use serde_json::{json, Value};

fn read_message(reader: &mut dyn BufRead) -> Option<Value> {
    let mut content_length: usize = 0;
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let line_trimmed = line.trim_end_matches(|c: char| c == '\r' || c == '\n');
        if line_trimmed.is_empty() { break; }
        if let Some(rest) = line_trimmed.strip_prefix("Content-Length: ") {
            content_length = rest.trim().parse().unwrap_or(0);
        }
    }
    if content_length == 0 { return None; }
    let mut buf = vec![0u8; content_length];
    {
        use std::io::Read;
        reader.read_exact(&mut buf).ok()?;
    }
    serde_json::from_slice(&buf).ok()
}

fn send_message(writer: &mut dyn Write, value: Value) {
    let body = value.to_string();
    let _ = write!(writer, "Content-Length: {}\r\n\r\n{}", body.len(), body);
    let _ = writer.flush();
}

fn send_response(writer: &mut dyn Write, id: &Value, result: Value) {
    send_message(writer, json!({ "jsonrpc": "2.0", "id": id, "result": result }));
}

fn send_notification(writer: &mut dyn Write, method: &str, params: Value) {
    send_message(writer, json!({ "jsonrpc": "2.0", "method": method, "params": params }));
}

// Returns (diagnostics, type_map) where type_map maps (1-based line, 1-based col) -> type string
fn type_check_source(_uri: &str, source: &str) -> (Vec<Value>, HashMap<(usize, usize), String>) {
    use orchestrate_lib::{lexer, parser, typechecker};

    let mut lex = lexer::Lexer::new(source);
    let tokens = match lex.tokenize() {
        Ok(t) => t,
        Err(e) => {
            return (vec![make_diagnostic(0, 0, &e)], HashMap::new());
        }
    };

    let mut p = parser::Parser::new(tokens);
    let stmts = match p.parse() {
        Ok(s) => s,
        Err(e) => {
            let (line, col) = extract_line_col(&e);
            return (vec![make_diagnostic(line, col, &e)], HashMap::new());
        }
    };

    let mut tc = typechecker::TypeChecker::new();
    let diags = match tc.type_check(&stmts) {
        Ok(()) => vec![],
        Err(e) => {
            e.lines()
                .map(|line| {
                    let (l, c) = extract_line_col(line);
                    make_diagnostic(l, c, line)
                })
                .collect()
        }
    };

    let type_map: HashMap<(usize, usize), String> = tc.type_map
        .into_iter()
        .map(|(k, v)| (k, v.display_name().to_string()))
        .collect();

    (diags, type_map)
}

fn extract_line_col(msg: &str) -> (u32, u32) {
    // Try "line N, col M" pattern
    if let Some(pos) = msg.find("line ") {
        let rest = &msg[pos + 5..];
        let line_end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        let line: u32 = rest[..line_end].parse().unwrap_or(1);
        let col: u32 = if let Some(col_pos) = rest.find("col ") {
            let col_rest = &rest[col_pos + 4..];
            let col_end = col_rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(col_rest.len());
            col_rest[..col_end].parse().unwrap_or(0)
        } else { 0 };
        return (line.saturating_sub(1), col.saturating_sub(1));
    }
    (0, 0)
}

fn make_diagnostic(line: u32, col: u32, message: &str) -> Value {
    json!({
        "range": {
            "start": { "line": line, "character": col },
            "end":   { "line": line, "character": col + 1 }
        },
        "severity": 1,   // Error
        "source": "orchestrate",
        "message": message
    })
}

// Returns (word, word_start_col) for the identifier at `character` on `text_line`
fn extract_word(text_line: &str, character: usize) -> (String, usize) {
    let chars: Vec<char> = text_line.chars().collect();
    let col = character.min(chars.len());
    let start = (0..col)
        .rev()
        .take_while(|&i| chars.get(i).map(|c| c.is_alphanumeric() || *c == '_').unwrap_or(false))
        .last()
        .unwrap_or(col);
    let end = (col..chars.len())
        .take_while(|&i| chars[i].is_alphanumeric() || chars[i] == '_')
        .last()
        .map(|i| i + 1)
        .unwrap_or(col);
    let word: String = chars[start..end].iter().collect();
    (word, start)
}

fn hover_info(
    source: &str,
    line: u32,
    character: u32,
    type_map: Option<&HashMap<(usize, usize), String>>,
) -> Option<String> {
    let lines: Vec<&str> = source.lines().collect();
    let text_line = lines.get(line as usize)?;
    let (word, word_start) = extract_word(text_line, character as usize);
    if word.is_empty() {
        return None;
    }

    // Try inferred type from type_map first (1-based line/col)
    if let Some(tm) = type_map {
        let key = (line as usize + 1, word_start + 1);
        if let Some(ty_str) = tm.get(&key) {
            return Some(format!("**{}**: `{}`", word, ty_str));
        }
    }

    // Fall back to keyword documentation
    let doc = match word.as_str() {
        "fn" => Some("Declares a synchronous function.\n\n```\nfn name(param: type) -> type { body }\n```"),
        "task" => Some("Declares an async task (runs concurrently).\n\n```\ntask name(param: type) -> type { body }\n```"),
        "process" => Some("Declares a long-running async process.\n\n```\nprocess name() { loop { ... } }\n```"),
        "orchestrator" => Some("Entry point or named async coordinator.\n\n```\norchestrator main() { ... }\n```"),
        "serverlet" => Some("Declares an actor-style message-passing server.\n\n```\nserverlet Counter { let count = 0; on increment() { count = count + 1 } }\n```"),
        "let" => Some("Declares a mutable variable.\n\n```\nlet x: int = 42\nlet name = \"Alice\"\n```"),
        "match" => Some("Pattern-matches on enum values.\n\n```\nmatch value {\n  Enum::Variant(x) => expr,\n  _ => fallback\n}\n```"),
        "for" => Some("Iterates over an array or range.\n\n```\nfor item in items { ... }\nfor i, item in items { ... }\nfor n in range(10) { ... }\n```"),
        "while" => Some("Loops while a boolean condition is true.\n\n```\nwhile condition { body }\n```"),
        "if" => Some("Conditional branch.\n\n```\nif cond { then } else { otherwise }\n```"),
        "break" => Some("Exits the nearest enclosing `for` or `while` loop."),
        "continue" => Some("Skips to the next iteration of the nearest enclosing loop."),
        "parallel" => Some("Runs enclosed tasks concurrently and joins results.\n\n```\nparallel {\n  let a = task1()\n  let b = task2()\n}\n```"),
        "automatic" => Some("Runs a block as a supervised, auto-restarting process.\n\n```\nautomatic(restart: always) { ... }\n```"),
        "try" => Some("Executes body; on error binds the message to a variable in `catch`.\n\n```\ntry { risky() } catch err { handle(err) }\n```"),
        "some" => Some("Wraps a value in `option<T>`.\n\n```\nlet x: option<int> = some(42)\n```"),
        "none" => Some("The absent value for `option<T>`.\n\n```\nlet x: option<int> = none\n```"),
        "ok" => Some("Wraps a success value in `result<T>`.\n\n```\nok(value)\n```"),
        "err" => Some("Wraps an error string in `result<T>`.\n\n```\nerr(\"something went wrong\")\n```"),
        "range" => Some("Generates an integer array from 0..n or start..end.\n\n```\nrange(10)           // [0,1,...,9]\nrange(3, 8)         // [3,4,...,7]\n```"),
        "map" => Some("Transforms each element of an array with a function.\n\n```\nmap(xs, fn(x: int) -> int { x * 2 })\n```"),
        "filter" => Some("Keeps array elements matching a predicate.\n\n```\nfilter(xs, fn(x: int) -> bool { x > 0 })\n```"),
        "reduce" => Some("Folds an array into a single value.\n\n```\nreduce(xs, 0, fn(acc: int, x: int) -> int { acc + x })\n```"),
        "to_int" => Some("Casts a numeric value to `int`.\n\n```\nto_int(3.7)   // 3\n```"),
        "to_float" => Some("Casts a numeric value to `float`.\n\n```\nto_float(5)   // 5.0\n```"),
        "parse_int" => Some("Parses a string to `result<int>`.\n\n```\nparse_int(\"42\")   // ok(42)\n```"),
        "parse_float" => Some("Parses a string to `result<float>`.\n\n```\nparse_float(\"3.14\")   // ok(3.14)\n```"),
        "int" => Some("64-bit signed integer type."),
        "float" => Some("64-bit floating point type."),
        "string" => Some("UTF-8 string type."),
        "bool" => Some("Boolean type: `true` or `false`."),
        "void" => Some("The unit type, returned by procedures with no value."),
        _ => None,
    };

    doc.map(|d| format!("**{}** — {}", word, d))
}

fn main() {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut reader = io::BufReader::new(stdin.lock());
    let mut writer = io::BufWriter::new(stdout.lock());

    // In-memory document store: uri → source text
    let mut documents: HashMap<String, String> = HashMap::new();
    // Per-document type maps: uri → (1-based line, 1-based col) → type string
    let mut type_maps: HashMap<String, HashMap<(usize, usize), String>> = HashMap::new();

    loop {
        let msg = match read_message(&mut reader) {
            Some(m) => m,
            None => break,
        };

        let method = msg["method"].as_str().unwrap_or("").to_string();
        let id = msg["id"].clone();
        let params = msg["params"].clone();

        match method.as_str() {
            "initialize" => {
                send_response(&mut writer, &id, json!({
                    "capabilities": {
                        "textDocumentSync": 1,   // Full sync
                        "hoverProvider": true,
                    },
                    "serverInfo": {
                        "name": "orchestrate-lsp",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }));
            }
            "initialized" => { /* no-op */ }
            "shutdown" => {
                send_response(&mut writer, &id, Value::Null);
            }
            "exit" => {
                break;
            }
            "textDocument/didOpen" => {
                if let Some(uri) = params["textDocument"]["uri"].as_str() {
                    let text = params["textDocument"]["text"].as_str().unwrap_or("").to_string();
                    let (diagnostics, tm) = type_check_source(uri, &text);
                    documents.insert(uri.to_string(), text);
                    type_maps.insert(uri.to_string(), tm);
                    send_notification(&mut writer, "textDocument/publishDiagnostics", json!({
                        "uri": uri,
                        "diagnostics": diagnostics
                    }));
                }
            }
            "textDocument/didChange" => {
                if let Some(uri) = params["textDocument"]["uri"].as_str() {
                    if let Some(changes) = params["contentChanges"].as_array() {
                        if let Some(last) = changes.last() {
                            let text = last["text"].as_str().unwrap_or("").to_string();
                            let (diagnostics, tm) = type_check_source(uri, &text);
                            documents.insert(uri.to_string(), text);
                            type_maps.insert(uri.to_string(), tm);
                            send_notification(&mut writer, "textDocument/publishDiagnostics", json!({
                                "uri": uri,
                                "diagnostics": diagnostics
                            }));
                        }
                    }
                }
            }
            "textDocument/hover" => {
                if let Some(uri) = params["textDocument"]["uri"].as_str() {
                    let line = params["position"]["line"].as_u64().unwrap_or(0) as u32;
                    let character = params["position"]["character"].as_u64().unwrap_or(0) as u32;
                    let result = if let Some(source) = documents.get(uri) {
                        let tm = type_maps.get(uri);
                        if let Some(info) = hover_info(source, line, character, tm) {
                            json!({ "contents": { "kind": "markdown", "value": info } })
                        } else {
                            Value::Null
                        }
                    } else {
                        Value::Null
                    };
                    send_response(&mut writer, &id, result);
                }
            }
            _ => {
                // Unhandled method — send null response if has id (prevents client hangs)
                if !id.is_null() {
                    send_response(&mut writer, &id, Value::Null);
                }
            }
        }
    }
}
