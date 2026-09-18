//! Extra Cargo dependencies for the generated crate: a Rust foreign module declares them
//! in its `.orch_ffi` sidecar under `[dependencies]`, and `build --lib` accepts more from
//! the command line. Declarations are normalized so that the same crate declared twice can
//! be compared, resolved so that a `path` means the same directory from every manifest the
//! compiler writes, and emitted in name order so the manifest never changes on its own.
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    Str(String),
    Bool(bool),
    Int(i64),
    Array(Vec<Value>),
    Table(BTreeMap<String, Value>),
}

/// One `[dependencies]` entry. `name = "1.0"` is stored as `{ version = "1.0" }`, so the
/// two spellings compare equal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dependency {
    pub name: String,
    pub spec: BTreeMap<String, Value>,
}

/// The dependency every generated crate already has.
pub fn builtin_tokio() -> Dependency {
    Dependency {
        name: "tokio".into(),
        spec: BTreeMap::from([
            ("version".to_string(), Value::Str("1.35".into())),
            ("features".to_string(), Value::Array(vec![Value::Str("full".into())])),
        ]),
    }
}

/// Separates a sidecar's `[dependencies]` section from its signatures. The section's lines
/// are blanked rather than removed, so signature diagnostics keep their line numbers.
pub fn split_sidecar(text: &str) -> (String, Option<String>) {
    let mut signatures = String::new();
    let mut section = String::new();
    let mut in_section = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            in_section = true;
        }
        if in_section {
            section.push_str(line);
            section.push('\n');
            signatures.push('\n');
        } else {
            signatures.push_str(line);
            signatures.push('\n');
        }
    }
    (signatures, in_section.then_some(section))
}

/// Parses `name = <spec>` entries, with or without a `[dependencies]` header. Relative
/// `path` values are resolved against `base` and must exist.
pub fn parse_section(text: &str, base: &Path, origin: &str) -> Result<Vec<Dependency>, String> {
    let mut parser = Parser { tokens: tokenize(text, origin)?, pos: 0, origin };
    let mut dependencies = Vec::new();
    while !parser.at_end() {
        if parser.eat(&Token::LBracket) {
            let section = parser.key()?;
            parser.expect(&Token::RBracket)?;
            if section != "dependencies" {
                return Err(format!("{origin}: only a [dependencies] section is supported, found [{section}]"));
            }
            continue;
        }
        let name = parser.key()?;
        parser.expect(&Token::Eq)?;
        let spec = match parser.value()? {
            Value::Str(version) => BTreeMap::from([("version".to_string(), Value::Str(version))]),
            Value::Table(table) => table,
            other => {
                return Err(format!(
                    "{origin}: dependency '{name}' must be a version string or an inline table, found {}",
                    describe(&other)
                ))
            }
        };
        let mut dependency = Dependency { name, spec };
        resolve_path(&mut dependency, base, origin)?;
        dependencies.push(dependency);
    }
    Ok(dependencies)
}

/// Adds one origin's declarations. The same crate declared twice must be identical, and
/// `tokio` may only repeat what the generated crate provides.
pub fn merge(
    into: &mut BTreeMap<String, (Dependency, String)>,
    dependencies: Vec<Dependency>,
    origin: &str,
) -> Result<(), String> {
    for dependency in dependencies {
        if dependency.name == "tokio" {
            if dependency.spec == builtin_tokio().spec {
                continue;
            }
            return Err(format!(
                "{origin}: dependency 'tokio' conflicts with the {} the generated crate provides",
                emit_line(&builtin_tokio()).trim_end()
            ));
        }
        match into.get(&dependency.name) {
            Some((existing, first)) if existing.spec != dependency.spec => {
                return Err(format!(
                    "dependency '{}' is declared as {} in {first} and as {} in {origin}; declarations of the same crate must be identical",
                    dependency.name,
                    emit_value(&Value::Table(existing.spec.clone())),
                    emit_value(&Value::Table(dependency.spec.clone()))
                ))
            }
            Some(_) => {}
            None => {
                into.insert(dependency.name.clone(), (dependency, origin.to_string()));
            }
        }
    }
    Ok(())
}

/// The `[dependencies]` lines for these entries, one per line, as given (callers pass
/// them in name order).
pub fn emit<'a>(dependencies: impl Iterator<Item = &'a Dependency>) -> String {
    dependencies.map(emit_line).collect()
}

fn emit_line(dependency: &Dependency) -> String {
    format!("{} = {}\n", key_text(&dependency.name), emit_value(&Value::Table(dependency.spec.clone())))
}

fn emit_value(value: &Value) -> String {
    match value {
        Value::Str(s) => quote(s),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Array(items) => format!("[{}]", items.iter().map(emit_value).collect::<Vec<_>>().join(", ")),
        Value::Table(table) if table.is_empty() => "{}".into(),
        Value::Table(table) => format!(
            "{{ {} }}",
            table.iter().map(|(k, v)| format!("{} = {}", key_text(k), emit_value(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

fn key_text(key: &str) -> String {
    if !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        key.to_string()
    } else {
        quote(key)
    }
}

fn quote(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04X}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn describe(value: &Value) -> &'static str {
    match value {
        Value::Str(_) => "a string",
        Value::Bool(_) => "a boolean",
        Value::Int(_) => "an integer",
        Value::Array(_) => "an array",
        Value::Table(_) => "a table",
    }
}

fn resolve_path(dependency: &mut Dependency, base: &Path, origin: &str) -> Result<(), String> {
    let Some(Value::Str(path)) = dependency.spec.get("path") else { return Ok(()) };
    let joined = if Path::new(path).is_absolute() { Path::new(path).to_path_buf() } else { base.join(path) };
    let resolved = std::fs::canonicalize(&joined).map_err(|e| {
        format!("{origin}: dependency '{}' path {:?} ({}): {e}", dependency.name, path, joined.display())
    })?;
    let mut resolved = resolved.to_string_lossy().into_owned();
    if let Some(stripped) = resolved.strip_prefix(r"\\?\") {
        resolved = stripped.to_string();
    }
    dependency.spec.insert("path".into(), Value::Str(resolved));
    Ok(())
}

#[derive(Debug, PartialEq)]
enum Token {
    Key(String),
    Str(String),
    Eq,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
}

fn tokenize(text: &str, origin: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = text.chars().collect();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            ' ' | '\t' | '\n' | '\r' => i += 1,
            '#' => while i < chars.len() && chars[i] != '\n' { i += 1 },
            '=' => { tokens.push(Token::Eq); i += 1; }
            '{' => { tokens.push(Token::LBrace); i += 1; }
            '}' => { tokens.push(Token::RBrace); i += 1; }
            '[' => { tokens.push(Token::LBracket); i += 1; }
            ']' => { tokens.push(Token::RBracket); i += 1; }
            ',' => { tokens.push(Token::Comma); i += 1; }
            '"' => {
                i += 1;
                let mut s = String::new();
                loop {
                    let Some(&c) = chars.get(i) else { return Err(format!("{origin}: unterminated string")) };
                    i += 1;
                    match c {
                        '"' => break,
                        '\\' => {
                            let Some(&e) = chars.get(i) else { return Err(format!("{origin}: unterminated string")) };
                            i += 1;
                            match e {
                                'n' => s.push('\n'),
                                't' => s.push('\t'),
                                'r' => s.push('\r'),
                                '"' => s.push('"'),
                                '\\' => s.push('\\'),
                                'u' | 'U' => {
                                    let width = if e == 'u' { 4 } else { 8 };
                                    let digits: String = chars.get(i..i + width).map(|d| d.iter().collect()).unwrap_or_default();
                                    let code = u32::from_str_radix(&digits, 16).ok().and_then(char::from_u32)
                                        .ok_or_else(|| format!("{origin}: invalid unicode escape \\{e}{digits}"))?;
                                    s.push(code);
                                    i += width;
                                }
                                other => return Err(format!("{origin}: unsupported escape \\{other}")),
                            }
                        }
                        c => s.push(c),
                    }
                }
                tokens.push(Token::Str(s));
            }
            '\'' => {
                i += 1;
                let start = i;
                while i < chars.len() && chars[i] != '\'' { i += 1; }
                if i >= chars.len() { return Err(format!("{origin}: unterminated string")); }
                tokens.push(Token::Str(chars[start..i].iter().collect()));
                i += 1;
            }
            c if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' => {
                let start = i;
                while i < chars.len() && (chars[i].is_ascii_alphanumeric() || matches!(chars[i], '_' | '-' | '.')) { i += 1; }
                tokens.push(Token::Key(chars[start..i].iter().collect()));
            }
            other => return Err(format!("{origin}: unexpected character {other:?}")),
        }
    }
    Ok(tokens)
}

struct Parser<'a> {
    tokens: Vec<Token>,
    pos: usize,
    origin: &'a str,
}

impl Parser<'_> {
    fn at_end(&self) -> bool {
        self.pos >= self.tokens.len()
    }

    fn eat(&mut self, token: &Token) -> bool {
        if self.tokens.get(self.pos) == Some(token) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    fn next(&mut self) -> Result<Token, String> {
        let token = self.tokens.get(self.pos).ok_or_else(|| format!("{}: unexpected end of dependencies", self.origin))?;
        self.pos += 1;
        Ok(match token {
            Token::Key(k) => Token::Key(k.clone()),
            Token::Str(s) => Token::Str(s.clone()),
            Token::Eq => Token::Eq,
            Token::LBrace => Token::LBrace,
            Token::RBrace => Token::RBrace,
            Token::LBracket => Token::LBracket,
            Token::RBracket => Token::RBracket,
            Token::Comma => Token::Comma,
        })
    }

    fn expect(&mut self, token: &Token) -> Result<(), String> {
        if self.eat(token) { Ok(()) } else { Err(format!("{}: expected {token:?} in dependencies", self.origin)) }
    }

    fn key(&mut self) -> Result<String, String> {
        match self.next()? {
            Token::Key(k) | Token::Str(k) => Ok(k),
            other => Err(format!("{}: expected a dependency name, found {other:?}", self.origin)),
        }
    }

    fn value(&mut self) -> Result<Value, String> {
        match self.next()? {
            Token::Str(s) => Ok(Value::Str(s)),
            Token::Key(k) if k == "true" => Ok(Value::Bool(true)),
            Token::Key(k) if k == "false" => Ok(Value::Bool(false)),
            Token::Key(k) => k.parse::<i64>().map(Value::Int)
                .map_err(|_| format!("{}: unquoted value '{k}'; strings need quotes", self.origin)),
            Token::LBrace => {
                let mut table = BTreeMap::new();
                loop {
                    if self.eat(&Token::RBrace) { break; }
                    let key = self.key()?;
                    self.expect(&Token::Eq)?;
                    let value = self.value()?;
                    if table.insert(key.clone(), value).is_some() {
                        return Err(format!("{}: key '{key}' is given twice", self.origin));
                    }
                    if !self.eat(&Token::Comma) {
                        self.expect(&Token::RBrace)?;
                        break;
                    }
                }
                Ok(Value::Table(table))
            }
            Token::LBracket => {
                let mut items = Vec::new();
                loop {
                    if self.eat(&Token::RBracket) { break; }
                    items.push(self.value()?);
                    if !self.eat(&Token::Comma) {
                        self.expect(&Token::RBracket)?;
                        break;
                    }
                }
                Ok(Value::Array(items))
            }
            other => Err(format!("{}: unexpected {other:?} in dependencies", self.origin)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("orch_dependencies_{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sdk")).unwrap();
        dir
    }

    #[test]
    fn version_string_and_inline_table_normalize_and_emit_in_name_order() {
        let deps = parse_section(
            "[dependencies]\nzeta = \"1.0\"\nalpha = { version = \"0.8\", features = [\"std\", \"rc\"], default-features = false }\n",
            &base(), "t",
        ).unwrap();
        assert_eq!(deps[0].spec, BTreeMap::from([("version".to_string(), Value::Str("1.0".into()))]));
        let mut merged = BTreeMap::new();
        merge(&mut merged, deps, "t").unwrap();
        assert_eq!(
            emit(merged.values().map(|(d, _)| d)),
            "alpha = { default-features = false, features = [\"std\", \"rc\"], version = \"0.8\" }\nzeta = { version = \"1.0\" }\n"
        );
    }

    #[test]
    fn same_crate_must_be_identical_across_origins() {
        let mut merged = BTreeMap::new();
        merge(&mut merged, parse_section("sdk = \"1.0\"", &base(), "a.orch_ffi").unwrap(), "a.orch_ffi").unwrap();
        merge(&mut merged, parse_section("sdk = { version = \"1.0\" }", &base(), "b.orch_ffi").unwrap(), "b.orch_ffi").unwrap();
        assert_eq!(merged.len(), 1);
        let error = merge(&mut merged, parse_section("sdk = \"2.0\"", &base(), "c.orch_ffi").unwrap(), "c.orch_ffi").unwrap_err();
        assert!(error.contains("dependency 'sdk'") && error.contains("a.orch_ffi") && error.contains("c.orch_ffi"), "{error}");
    }

    #[test]
    fn tokio_may_only_repeat_the_builtin() {
        let mut merged = BTreeMap::new();
        merge(&mut merged, parse_section("tokio = { version = \"1.35\", features = [\"full\"] }", &base(), "t").unwrap(), "t").unwrap();
        assert!(merged.is_empty());
        let error = merge(&mut merged, parse_section("tokio = \"1.40\"", &base(), "t").unwrap(), "t").unwrap_err();
        assert!(error.contains("tokio"), "{error}");
    }

    #[test]
    fn relative_paths_resolve_against_the_base_and_must_exist() {
        let base = base();
        let deps = parse_section("sdk = { path = \"sdk\" }", &base, "t").unwrap();
        let expected = std::fs::canonicalize(base.join("sdk")).unwrap().to_string_lossy().into_owned();
        assert_eq!(deps[0].spec.get("path"), Some(&Value::Str(expected)));
        let error = parse_section("gone = { path = \"missing\" }", &base, "t").unwrap_err();
        assert!(error.contains("gone") && error.contains("missing"), "{error}");
    }

    #[test]
    fn split_keeps_signature_line_numbers() {
        let (signatures, section) = split_sidecar("add(a: int) -> int\n\n[dependencies]\nsdk = \"1\"\n");
        assert_eq!(signatures, "add(a: int) -> int\n\n\n\n");
        assert_eq!(section.unwrap(), "[dependencies]\nsdk = \"1\"\n");
        assert_eq!(split_sidecar("only(x: int)").1, None);
    }

    #[test]
    fn rejects_other_sections_and_unquoted_strings() {
        assert!(parse_section("[dev-dependencies]\nx = \"1\"", &base(), "t").unwrap_err().contains("[dev-dependencies]"));
        assert!(parse_section("x = latest", &base(), "t").unwrap_err().contains("quotes"));
        assert!(parse_section("x = [\"1\"]", &base(), "t").unwrap_err().contains("must be a version string or an inline table"));
    }
}
