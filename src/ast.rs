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
    Assign,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnaryOp {
    Neg,
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
        }
    }

    pub fn contains_type_param(&self) -> bool {
        match self {
            Type::TypeParam(_) => true,
            Type::Array(inner, _) => inner.contains_type_param(),
            Type::Option(inner) => inner.contains_type_param(),
            Type::Result(ok, err) => ok.contains_type_param() || err.contains_type_param(),
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
    /// What a call that misses its budget returns.
    pub late: LatePolicy,
    /// TypeScript only: `auto`, `scriptc`, or `bun`.
    pub backend: Option<String>,
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
    StartServerlet {
        name: String,
        args: Vec<Expr>,
    },
    AutomaticBlock {
        body: Box<Expr>,
        restart_policy: RestartPolicy,
        crash_handler: Option<(String, Box<Expr>)>,
    },
    TriggeredBlock {
        event_name: String,
        params: Vec<Param>,
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
    },
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
    OnTick { param: String, input: Option<Param>, return_type: Type, body: Expr },
    OnFixedTick { param: String, body: Expr },
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
