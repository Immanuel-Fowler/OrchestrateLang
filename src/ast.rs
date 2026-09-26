#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub line: usize,
    pub col: usize,
}

impl Span {
    pub fn new(line: usize, col: usize) -> Self {
        Span { line, col }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Spanned<T> {
    pub node: T,
    pub span: Span,
}

pub type Expr = Spanned<ExprNode>;
pub type Stmt = Spanned<StmtNode>;

#[derive(Debug, Clone, PartialEq)]
pub enum Literal {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum BinaryOp {
    Add, Sub, Mul, Div, Mod,
    Eq, Ne, Lt, Gt, Le, Ge,
    And, Or,
    /// Bitwise and shifts on `int`: `&`, `|`, `^`, `<<`, `>>`.
    BitAnd, BitOr, BitXor, Shl, Shr,
    Assign,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
    /// `!` on a `bool`.
    Not,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    Int,
    Float,
    Str,
    Bool,
    Void,
    Process,
    /// An opaque native object owned by a C-ABI foreign module.
    Handle,
    Array(Box<Type>, Vec<String>),
    Named(String),
    Option(Box<Type>),
    /// `result<T, E>`; `result<T>` is `result<T, string>`.
    Result(Box<Type>, Box<Type>),
    Fn(Vec<Type>, Box<Type>),       // fn(T1, T2) -> T3
    TypeParam(String),               // generic T, U, K, V
    /// `S.client`: the address of a running instance of serverlet `S` — what `start S()`
    /// returns. A module's serverlet is `alias::S`, written `alias.S.client`.
    Client(String),
    /// `map<K, V>`, ordered by key; a key is an `int`, a `string`, or a `bool`.
    Map(Box<Type>, Box<Type>),
}

impl Type {
    pub fn display_name(&self) -> String {
        match self {
            Type::Int => "int".to_string(),
            Type::Float => "float".to_string(),
            Type::Str => "string".to_string(),
            Type::Bool => "bool".to_string(),
            Type::Void => "void".to_string(),
            Type::Process => "process".to_string(),
            Type::Handle => "handle".to_string(),
            Type::Array(inner, _) => format!("{}[]", inner.display_name()),
            Type::Named(name) => name.clone(),
            Type::Option(inner) => format!("option<{}>", inner.display_name()),
            Type::Result(ok, err) if **err == Type::Str => format!("result<{}>", ok.display_name()),
            Type::Result(ok, err) => format!("result<{}, {}>", ok.display_name(), err.display_name()),
            Type::Fn(params, ret) => format!(
                "fn({}) -> {}",
                params.iter().map(|t| t.display_name()).collect::<Vec<_>>().join(", "),
                ret.display_name()
            ),
            Type::TypeParam(name) => name.clone(),
            Type::Client(serverlet) => format!("{}.client", serverlet.replace("::", ".")),
            Type::Map(key, value) => format!("map<{}, {}>", key.display_name(), value.display_name()),
        }
    }

    pub fn contains_type_param(&self) -> bool {
        match self {
            Type::TypeParam(_) => true,
            Type::Array(inner, _) => inner.contains_type_param(),
            Type::Option(inner) => inner.contains_type_param(),
            Type::Result(ok, err) | Type::Map(ok, err) => ok.contains_type_param() || err.contains_type_param(),
            Type::Fn(params, ret) => params.iter().any(|t| t.contains_type_param()) || ret.contains_type_param(),
            _ => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SandboxConfig {
    pub memory_limit: String,
    pub timeout: String,
    /// `every: "100ms"`: how often, in host time, the runtime calls the serverlet's `tick`.
    pub every_micros: Option<u64>,
}

/// How often a cadenced tick hook runs: a period of host time, or every so many ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cadence {
    Micros(u64),
    Ticks(u64),
}

/// A duration as a program writes it: a number and `us`, `ms`, or `s` — "500us", "2ms",
/// "0.5s" — in whole microseconds, at least one.
pub fn duration_micros(text: &str) -> Option<u64> {
    let text = text.trim();
    let (number, scale) = if let Some(n) = text.strip_suffix("us") {
        (n, 1.0)
    } else if let Some(n) = text.strip_suffix("ms") {
        (n, 1_000.0)
    } else if let Some(n) = text.strip_suffix('s') {
        (n, 1_000_000.0)
    } else {
        return None;
    };
    let micros = number.trim().parse::<f64>().ok()? * scale;
    (micros.is_finite() && micros >= 1.0 && micros <= u64::MAX as f64).then(|| micros.round() as u64)
}

impl SandboxConfig {
    /// The memory cap in bytes. `1kb` is 1024 bytes, as the units are binary.
    pub fn memory_bytes(&self) -> Option<u64> {
        let text = self.memory_limit.trim().to_ascii_lowercase();
        let (number, scale) = if let Some(n) = text.strip_suffix("gb") {
            (n, 1024 * 1024 * 1024)
        } else if let Some(n) = text.strip_suffix("mb") {
            (n, 1024 * 1024)
        } else if let Some(n) = text.strip_suffix("kb") {
            (n, 1024)
        } else if let Some(n) = text.strip_suffix('b') {
            (n, 1)
        } else {
            (text.as_str(), 1)
        };
        let bytes = number.trim().parse::<f64>().ok()? * scale as f64;
        (bytes.is_finite() && bytes >= 1.0 && bytes <= u64::MAX as f64).then(|| bytes.round() as u64)
    }

    /// How long one call may run, in milliseconds.
    pub fn timeout_ms(&self) -> Option<u64> {
        let text = self.timeout.trim().to_ascii_lowercase();
        let (number, scale) = if let Some(n) = text.strip_suffix("ms") {
            (n, 1.0)
        } else if let Some(n) = text.strip_suffix('m') {
            (n, 60_000.0)
        } else if let Some(n) = text.strip_suffix('s') {
            (n, 1_000.0)
        } else {
            return None;
        };
        let millis = number.trim().parse::<f64>().ok()? * scale;
        (millis.is_finite() && millis >= 1.0).then(|| millis.round() as u64)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct LandlineConfig {
    pub runtime: String,
    pub source: String,
    /// Per-call deadline in microseconds. A call with no reply by then returns early.
    pub budget_micros: Option<u64>,
    /// `every: "100ms"`: how often, in host time, the runtime calls the serverlet's `tick`.
    pub every_micros: Option<u64>,
    /// What a call that misses its budget returns.
    pub late: LatePolicy,
    /// TypeScript only: `auto`, `scriptc`, or `bun`.
    pub backend: Option<String>,
    /// `files: ["codec.py", "data/"]`: files and folders staged beside the source, besides
    /// the local modules a Python source imports, which are found and staged without it.
    pub files: Vec<String>,
}

/// A duration in microseconds as a program would have written it: `5ms`, `250us`, `2s`.
pub fn display_micros(micros: u64) -> String {
    if micros % 1_000_000 == 0 {
        format!("{}s", micros / 1_000_000)
    } else if micros % 1_000 == 0 {
        format!("{}ms", micros / 1_000)
    } else {
        format!("{}us", micros)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LatePolicy {
    /// Return the default value and discard the reply when it arrives.
    Drop,
    /// Return the handler's most recent completed result; a late reply becomes that result.
    Latest,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Handler {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Type,
    /// `unmeasured`: left out of what `measure serverlets` records.
    pub unmeasured: bool,
    pub body: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EnumVariant {
    pub name: String,
    pub payload: Option<Type>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub pattern: MatchPattern,
    pub body: Box<Expr>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MatchPattern {
    EnumVariant {
        enum_name: String,
        variant_name: String,
        binding: Option<String>,
    },
    Wildcard,
    Literal(Literal),
    Binding(String),
    Guard {
        inner: Box<MatchPattern>,
        condition: Box<Expr>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum RestartPolicy {
    Always,
    Never,
    MaxAttempts(u32),
}

/// A segment in a string interpolation: either literal text or an embedded expression.
#[derive(Debug, Clone, PartialEq)]
pub enum StringPart {
    Literal(String),
    Expr(Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExprNode {
    Literal(Literal),
    Identifier(String),
    Unary {
        op: UnaryOp,
        operand: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Call {
        callee: String,
        args: Vec<Expr>,
    },
    Pipeline {
        value: Box<Expr>,
        function: Box<Expr>,
    },
    Block(Vec<Stmt>),
    If {
        cond: Box<Expr>,
        then_branch: Box<Expr>,
        else_branch: Option<Box<Expr>>,
    },
    ModuleCall {
        module_local_name: String,
        function: String,
        args: Vec<Expr>,
    },
    /// `books[0].place(1)`: a handler called on a client that is not a bare name.
    MethodCall {
        receiver: Box<Expr>,
        method: String,
        args: Vec<Expr>,
    },
    StartServerlet {
        name: String,
        args: Vec<Expr>,
    },
    AutomaticBlock {
        body: Box<Expr>,
        restart_policy: RestartPolicy,
        /// `backoff: "50ms"`: the fixed wait before each restart. Absent, the wait starts
        /// at a second and doubles with each restart, up to thirty.
        backoff_ms: Option<u64>,
        /// `every: "20ms"`: the body runs once per period, on wall time, rather than in a
        /// loop as fast as it goes.
        every_micros: Option<u64>,
        /// `unmeasured`: left out of what `measure workers` records.
        unmeasured: bool,
        crash_handler: Option<(String, Box<Expr>)>,
    },
    TriggeredBlock {
        event_name: String,
        params: Vec<Param>,
        /// `unmeasured`: left out of what `measure hooks` records.
        unmeasured: bool,
        body: Box<Expr>,
    },
    StartProcess {
        target: Box<Expr>,
    },
    ArrayLiteral(Vec<Expr>),
    StructLiteral {
        name: String,
        fields: Vec<(String, Box<Expr>)>,
    },
    FieldAccess {
        object: Box<Expr>,
        field: String,
    },
    Index {
        object: Box<Expr>,
        index: Box<Expr>,
        /// Set by the typechecker when `object` is a map, which codegen indexes by key.
        on_map: std::cell::Cell<bool>,
    },
    /// `map { key: value, ... }`
    MapLiteral(Vec<(Expr, Expr)>),
    // Error handling
    NoneLiteral,
    SomeLiteral(Box<Expr>),
    OkLiteral(Box<Expr>),
    ErrLiteral(Box<Expr>),
    Propagate(Box<Expr>),
    TryCatch {
        body: Box<Expr>,
        err_name: String,
        /// `catch e: Failure`; absent means the error is a `string`.
        err_type: Option<Type>,
        handler: Box<Expr>,
    },
    // Enum support
    EnumVariantLiteral {
        enum_name: String,
        variant_name: String,
        payload: Option<Box<Expr>>,
    },
    Match {
        value: Box<Expr>,
        arms: Vec<MatchArm>,
    },
    // Closures
    Closure {
        params: Vec<Param>,
        return_type: Option<Type>,
        body: Box<Expr>,
    },
    // String interpolation: "hello {name}, you have {count} items"
    StringInterp {
        parts: Vec<StringPart>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum StmtNode {
    /// `shared queue jobs: Job (capacity: 256, full: "reject")`: a queue any task may
    /// push to and take from, each operation atomic, with a declared capacity and what a
    /// push to a full one does.
    SharedQueue {
        name: String,
        ty: Type,
        capacity: u64,
        /// `full: "drop_oldest"`; otherwise `"reject"`.
        drop_oldest: bool,
    },
    Let {
        name: String,
        ty: Option<Type>,
        value: Expr,
        /// `shared let`: the binding lives behind the program's one shared-state mutex,
        /// so any task or function can reach it, not only hooks.
        shared: bool,
    },
    Expr(Expr),
    Return(Option<Expr>),
    FnDecl {
        name: String,
        type_params: Vec<String>,
        params: Vec<Param>,
        return_type: Type,
        body: Expr,
    },
    TaskDecl {
        name: String,
        type_params: Vec<String>,
        params: Vec<Param>,
        return_type: Type,
        body: Expr,
    },
    ProcessDecl {
        name: String,
        type_params: Vec<String>,
        params: Vec<Param>,
        return_type: Type,
        body: Expr,
    },
    OrchestratorDecl {
        name: String,
        params: Vec<Param>,
        return_type: Type,
        body: Expr,
    },
    Trigger {
        event_name: String,
        args: Vec<Expr>,
    },
    Parallel(Vec<Stmt>),
    While {
        cond: Expr,
        body: Expr,
    },
    ForIn {
        var: String,
        index_var: Option<String>,
        iter: Expr,
        body: Expr,
    },
    UseModule {
        local_name: String,
        module_name: String,
    },
    Load {
        path: String,
    },
    LoadForeign {
        language: String,
        path: String,
        /// TypeScript only: `auto`, `scriptc`, or `bun`.
        backend: Option<String>,
    },
    Serverlet {
        name: String,
        state: Vec<Stmt>,
        handlers: Vec<Handler>,
        secret: bool,
        crash_handler: Option<(String, Box<Expr>)>,
        sandbox: Option<SandboxConfig>,
        landline: Option<LandlineConfig>,
        grants: Vec<String>,
    },
    Host { name: String, functions: Vec<Handler> },
    /// `every` makes it a cadenced hook: it runs when its period of host time has passed,
    /// and its parameter is the host time since it last ran. `unmeasured` leaves it out of
    /// what `measure hooks` records.
    OnTick { param: String, input: Option<Param>, return_type: Type, every: Option<Cadence>, unmeasured: bool, body: Expr },
    OnFixedTick { param: String, every: Option<Cadence>, unmeasured: bool, body: Expr },
    /// `measure serverlets, hooks, workers`: what the runtime keeps counts and timings of,
    /// for `stats()` and `Scripts::stats()`.
    Measure { serverlets: bool, hooks: bool, workers: bool },
    OnStart(Expr),
    OnStop(Expr),
    Break,
    Continue,
    StructDef {
        name: String,
        fields: Vec<(String, Type)>,
    },
    EnumDef {
        name: String,
        variants: Vec<EnumVariant>,
    },
}

/// A node of the tree, as a walk visits it.
#[derive(Clone, Copy)]
pub enum Node<'a> {
    Expr(&'a Expr),
    Stmt(&'a Stmt),
}

/// Visits `expr` and everything inside it — statements, closure and handler bodies,
/// serverlet handlers — parent before children.
pub fn walk_expr<'a>(expr: &'a Expr, visit: &mut dyn FnMut(Node<'a>)) {
    walk_expr_inner(expr, visit, true);
}

/// `walk_expr`, without entering closure, `automatic`, or event-handler bodies, which run
/// later and on their own.
pub fn walk_expr_within<'a>(expr: &'a Expr, visit: &mut dyn FnMut(Node<'a>)) {
    walk_expr_inner(expr, visit, false);
}

/// `walk_expr` for a statement.
pub fn walk_stmt<'a>(stmt: &'a Stmt, visit: &mut dyn FnMut(Node<'a>)) {
    walk_stmt_inner(stmt, visit, true);
}

fn walk_expr_inner<'a>(expr: &'a Expr, visit: &mut dyn FnMut(Node<'a>), deferred: bool) {
    visit(Node::Expr(expr));
    let go = |e: &'a Expr, visit: &mut dyn FnMut(Node<'a>)| walk_expr_inner(e, visit, deferred);
    match &expr.node {
        ExprNode::Literal(_) | ExprNode::Identifier(_) | ExprNode::NoneLiteral => {}
        ExprNode::Unary { operand, .. } => go(operand, visit),
        ExprNode::Binary { lhs, rhs, .. } => { go(lhs, visit); go(rhs, visit); }
        ExprNode::Call { args, .. } | ExprNode::ModuleCall { args, .. } | ExprNode::StartServerlet { args, .. }
        | ExprNode::ArrayLiteral(args) => for arg in args { go(arg, visit); },
        ExprNode::MethodCall { receiver, args, .. } => {
            go(receiver, visit);
            for arg in args { go(arg, visit); }
        }
        ExprNode::Pipeline { value, function } => { go(value, visit); go(function, visit); }
        ExprNode::Block(stmts) => for stmt in stmts { walk_stmt_inner(stmt, visit, deferred); },
        ExprNode::If { cond, then_branch, else_branch } => {
            go(cond, visit);
            go(then_branch, visit);
            if let Some(branch) = else_branch { go(branch, visit); }
        }
        ExprNode::AutomaticBlock { body, crash_handler, .. } => if deferred {
            go(body, visit);
            if let Some((_, handler)) = crash_handler { go(handler, visit); }
        },
        ExprNode::TriggeredBlock { body, .. } | ExprNode::Closure { body, .. } => if deferred { go(body, visit); },
        ExprNode::StartProcess { target } => go(target, visit),
        ExprNode::StructLiteral { fields, .. } => for (_, value) in fields { go(value, visit); },
        ExprNode::FieldAccess { object, .. } => go(object, visit),
        ExprNode::Index { object, index, .. } => { go(object, visit); go(index, visit); }
        ExprNode::MapLiteral(entries) => for (key, value) in entries { go(key, visit); go(value, visit); },
        ExprNode::SomeLiteral(inner) | ExprNode::OkLiteral(inner) | ExprNode::ErrLiteral(inner) | ExprNode::Propagate(inner) => {
            go(inner, visit)
        }
        ExprNode::TryCatch { body, handler, .. } => { go(body, visit); go(handler, visit); }
        ExprNode::EnumVariantLiteral { payload, .. } => if let Some(payload) = payload { go(payload, visit); },
        ExprNode::Match { value, arms } => {
            go(value, visit);
            for arm in arms {
                if let MatchPattern::Guard { condition, .. } = &arm.pattern { go(condition, visit); }
                go(&arm.body, visit);
            }
        }
        ExprNode::StringInterp { parts } => for part in parts {
            if let StringPart::Expr(inner) = part { go(inner, visit); }
        },
    }
}

fn walk_stmt_inner<'a>(stmt: &'a Stmt, visit: &mut dyn FnMut(Node<'a>), deferred: bool) {
    visit(Node::Stmt(stmt));
    let go = |e: &'a Expr, visit: &mut dyn FnMut(Node<'a>)| walk_expr_inner(e, visit, deferred);
    match &stmt.node {
        StmtNode::Let { value, .. } | StmtNode::Expr(value) | StmtNode::Return(Some(value)) => go(value, visit),
        StmtNode::SharedQueue { .. } | StmtNode::Measure { .. } => {}
        StmtNode::FnDecl { body, .. } | StmtNode::TaskDecl { body, .. } | StmtNode::ProcessDecl { body, .. }
        | StmtNode::OrchestratorDecl { body, .. } | StmtNode::OnTick { body, .. } | StmtNode::OnFixedTick { body, .. }
        | StmtNode::OnStart(body) | StmtNode::OnStop(body) => go(body, visit),
        StmtNode::Trigger { args, .. } => for arg in args { go(arg, visit); },
        StmtNode::Parallel(stmts) => for inner in stmts { walk_stmt_inner(inner, visit, deferred); },
        StmtNode::While { cond, body } | StmtNode::ForIn { iter: cond, body, .. } => { go(cond, visit); go(body, visit); }
        StmtNode::Serverlet { state, handlers, crash_handler, .. } => {
            for s in state { walk_stmt_inner(s, visit, deferred); }
            for h in handlers { go(&h.body, visit); }
            if let Some((_, handler)) = crash_handler { go(handler, visit); }
        }
        StmtNode::Return(None) | StmtNode::UseModule { .. } | StmtNode::Load { .. } | StmtNode::LoadForeign { .. }
        | StmtNode::Host { .. } | StmtNode::Break | StmtNode::Continue | StmtNode::StructDef { .. } | StmtNode::EnumDef { .. } => {}
    }
}
