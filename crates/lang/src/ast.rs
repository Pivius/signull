//! Abstract syntax tree for sinu source.

use crate::diag::Span;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub items: Vec<Item>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Item {
    Func(Func),
    Global(Global),
    Struct(StructDecl),
    Enum(EnumDecl),
    TypeAlias(TypeAlias),
    /// `entry name;` — selects the program entry point.
    Entry(EntryDecl),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Func {
    pub name: String,
    pub params: Vec<Param>,
    pub ret_ty: Type,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    pub name: String,
    pub ty: Type,
}

/// A top-level object. `explicit_static` records a written `static` keyword;
/// top-level storage is static either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Global {
    pub name: String,
    pub ty: Type,
    pub value: Option<Expr>,
    pub explicit_static: bool,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeAlias {
    pub name: String,
    pub ty: Type,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryDecl {
    pub name: String,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructDecl {
    pub name: String,
    pub packed: bool,
    pub fields: Vec<Field>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub ty: Type,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumDecl {
    pub name: String,
    /// Underlying integer type; parsed as a primitive, defaults to u32.
    pub underlying: Type,
    pub variants: Vec<Variant>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variant {
    pub name: String,
    pub value: Option<i64>,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storage {
    /// Ordinary automatic storage.
    Auto,
    /// Explicit `stack` storage specifier.
    Stack,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Stmt {
    /// `[static|stack] Type name = expr;` / `Type name;` declaration.
    Decl {
        name: String,
        ty: Type,
        value: Option<Expr>,
        storage: Storage,
        span: Span,
    },
    Assign {
        target: Expr,
        value: Expr,
        span: Span,
    },
    Expr(Expr),
    If {
        cond: Expr,
        then_branch: Block,
        else_branch: Option<Block>,
        span: Span,
    },
    While {
        cond: Expr,
        body: Block,
        span: Span,
    },
    /// `for (init; cond; step) body`. `step` is an expression or assignment
    /// without a trailing semicolon.
    For {
        init: Option<Box<Stmt>>,
        cond: Option<Expr>,
        step: Option<Box<Stmt>>,
        body: Block,
        span: Span,
    },
    Break(Span),
    Continue(Span),
    Return {
        value: Option<Expr>,
        span: Span,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    BitNot,
    AddrOf,
    Deref,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    And,
    Or,
    Xor,
    Shl,
    Shr,
    AndAnd,
    OrOr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Expr {
    Int(i64, Span),
    Bool(bool, Span),
    Str(Vec<u8>, Span),
    /// `null` — the null pointer constant.
    Null(Span),
    Ident(String, Span),
    Array(Vec<Expr>, Span),
    Unary {
        op: UnOp,
        expr: Box<Expr>,
        span: Span,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        span: Span,
    },
    Call {
        callee: Box<Expr>,
        args: Vec<Expr>,
        span: Span,
    },
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
        span: Span,
    },
    Field {
        base: Box<Expr>,
        name: String,
        span: Span,
    },
    /// `{ field = expr, ... }` aggregate initializer.
    AggregateLit {
        fields: Vec<(String, Expr, Span)>,
        span: Span,
    },
    /// `cast<T>(expr)` explicit conversion.
    Cast {
        expr: Box<Expr>,
        ty: Type,
        span: Span,
    },
    /// `load<T>(address)` read a typed value from memory.
    Load {
        ty: Type,
        addr: Box<Expr>,
        span: Span,
    },
    /// `store<T>(address, value)` write a typed value to memory.
    Store {
        ty: Type,
        addr: Box<Expr>,
        value: Box<Expr>,
        span: Span,
    },
    /// `sizeof(T)` / `alignof(T)` constant-folded byte counts.
    SizeOf {
        ty: Type,
        align: bool,
        span: Span,
    },
    /// `offsetof(T, field)` constant-folded field offset.
    OffsetOf {
        ty: Type,
        field: String,
        span: Span,
    },
}

impl Stmt {
    pub fn span(&self) -> Span {
        match self {
            Stmt::Decl { span: s, .. }
            | Stmt::Assign { span: s, .. }
            | Stmt::If { span: s, .. }
            | Stmt::While { span: s, .. }
            | Stmt::For { span: s, .. }
            | Stmt::Return { span: s, .. } => *s,
            Stmt::Break(s) | Stmt::Continue(s) => *s,
            Stmt::Expr(expr) => expr.span(),
        }
    }
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Int(_, s)
            | Expr::Bool(_, s)
            | Expr::Str(_, s)
            | Expr::Null(s)
            | Expr::Ident(_, s)
            | Expr::Array(_, s)
            | Expr::AggregateLit { span: s, .. }
            | Expr::Cast { span: s, .. }
            | Expr::Load { span: s, .. }
            | Expr::Store { span: s, .. }
            | Expr::SizeOf { span: s, .. }
            | Expr::OffsetOf { span: s, .. }
            | Expr::Unary { span: s, .. }
            | Expr::Binary { span: s, .. }
            | Expr::Call { span: s, .. }
            | Expr::Index { span: s, .. }
            | Expr::Field { span: s, .. } => *s,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    Bool,
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    /// Unsigned integer matching the target's native word width (32-bit).
    Word,
    /// Signed integer matching the target's native word width (32-bit).
    Sword,
    /// Absence of a return value.
    Void,
    /// Fixed-size array `T[N]` of `elem` with `len` elements.
    Array {
        elem: Box<Type>,
        len: usize,
    },
    /// User-defined struct type.
    Struct(String),
    /// User-defined enum type.
    Enum(String),
    /// `type Name = ...` alias; resolved to its target during compilation.
    Alias(String),
    /// Raw pointer `*T`.
    Ptr(Box<Type>),
}
