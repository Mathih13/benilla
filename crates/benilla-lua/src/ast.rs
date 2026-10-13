//! The parsed chunk, with every name already resolved to a local, an upvalue or a global, as
//! 5.0's one-pass parser resolves it.

use std::rc::Rc;

pub type Bytes = Rc<[u8]>;

/// An index into [`Func::locals`].
pub type LocalId = u16;

#[derive(Debug)]
pub struct LocalDesc {
    pub name: Bytes,
    /// Some nested function closes over it, so its register needs closing at scope exit.
    pub captured: bool,
}

#[derive(Debug, Clone)]
pub struct UpvalDesc {
    pub name: Bytes,
    /// A local of the enclosing function, or one of its upvalues.
    pub from_local: bool,
    /// The enclosing function's [`LocalId`] or upvalue index.
    pub index: u16,
}

#[derive(Debug)]
pub struct Func {
    pub source: Bytes,
    pub line_defined: u32,
    /// Parameters, then `self`'s or `arg`'s slot: the first `nparams` locals.
    pub nparams: u8,
    /// `...` in the parameter list: 5.0 collects the extras into the local `arg`.
    pub vararg: Option<LocalId>,
    pub locals: Vec<LocalDesc>,
    pub upvals: Vec<UpvalDesc>,
    pub body: Block,
}

#[derive(Debug, Default)]
pub struct Block {
    pub stats: Vec<Stat>,
}

#[derive(Debug)]
pub enum Stat {
    /// A call statement; its results are dropped.
    Call(Expr),
    Local {
        vars: Vec<LocalId>,
        exprs: Vec<Expr>,
    },
    LocalFunction {
        var: LocalId,
        func: Box<Func>,
    },
    Assign {
        targets: Vec<Expr>,
        exprs: Vec<Expr>,
        line: u32,
    },
    Do(Block),
    While {
        cond: Expr,
        body: Block,
    },
    /// 5.0's `until` sees none of the body's locals.
    Repeat {
        body: Block,
        cond: Expr,
    },
    If {
        arms: Vec<(Expr, Block)>,
        orelse: Option<Block>,
    },
    /// 5.0's numeric for: the variable is the counter itself, beside its two hidden locals.
    NumFor {
        var: LocalId,
        limit: LocalId,
        step: LocalId,
        start_e: Expr,
        limit_e: Expr,
        step_e: Option<Expr>,
        body: Block,
        line: u32,
    },
    /// 5.0's generic for: `(for generator)`, `(for state)`, and the first user variable is the
    /// control.
    GenFor {
        generator: LocalId,
        state: LocalId,
        vars: Vec<LocalId>,
        exprs: Vec<Expr>,
        body: Block,
        line: u32,
    },
    Function {
        target: Expr,
        func: Box<Func>,
        line: u32,
    },
    Return {
        exprs: Vec<Expr>,
    },
    Break,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
    Concat,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Debug)]
pub enum Field {
    /// A positional item.
    Item(Expr),
    /// `name = v`, which counts toward 5.0's hash size hint.
    Named(Bytes, Expr),
    /// `[k] = v`, which counts toward neither hint.
    Pair(Expr, Expr),
}

#[derive(Debug)]
pub enum Expr {
    Nil,
    True,
    False,
    Num(f64),
    Str(Bytes),
    Local(LocalId),
    Upval(u16),
    Global(Bytes),
    Index(Box<Expr>, Box<Expr>, u32),
    Call(Box<Expr>, Vec<Expr>, u32),
    Method(Box<Expr>, Bytes, Vec<Expr>, u32),
    Function(Box<Func>),
    Table(Vec<Field>),
    Bin(BinOp, Box<Expr>, Box<Expr>, u32),
    Un(UnOp, Box<Expr>, u32),
    /// A parenthesised expression: one value, and no longer assignable.
    Paren(Box<Expr>),
}

impl Expr {
    /// A call, whose value count follows its context.
    pub fn is_multi(&self) -> bool {
        matches!(self, Expr::Call(..) | Expr::Method(..))
    }
}
