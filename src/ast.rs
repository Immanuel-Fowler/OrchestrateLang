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
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
    Assign,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    Int,
    Float,
    Str,
    Bool,
    Void,
    Process,
    Array(Box<Type>, Vec<String>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Handler {
    pub name: String,
    pub params: Vec<Param>,
    pub return_type: Type,
    pub body: Expr,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Literal(Literal),
    Identifier(String),
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
        function: Box<Expr>, // Should be a Call or Identifier
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
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    Let {
        name: String,
        ty: Option<Type>,
        value: Expr,
    },
    Expr(Expr),
    Return(Option<Expr>),
    FnDecl {
        name: String,
        params: Vec<Param>,
        return_type: Type,
        body: Expr, // Often a Expr::Block
    },
    TaskDecl {
        name: String,
        params: Vec<Param>,
        return_type: Type,
        body: Expr, // Often a Expr::Block
    },
    ProcessDecl {
        name: String,
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
        body: Expr, // Often a Expr::Block
    },
    UseModule {
        local_name: String,
        module_name: String,
    },
    Load {
        path: String,
    },
    Serverlet {
        name: String,
        state: Vec<Stmt>,
        handlers: Vec<Handler>,
    },
    OnStart(Expr), // Typically Expr::Block
    OnStop(Expr),  // Typically Expr::Block
}
