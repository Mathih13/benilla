//! The parser: the script host's `lparser.c`, 5.1's with the 1.12 grammar, producing an [`ast`]
//! with names resolved in the same pass. Messages and the token they quote are 5.1's.

use std::rc::Rc;

use crate::ast::*;
use crate::lex::{Lexer, SyntaxError, Tok};

/// 5.0's `MAXVARS`: active locals per function.
const MAXVARS: usize = 200;
/// 5.0's `MAXUPVALUES`.
const MAXUPVALUES: usize = 32;
/// 5.0's `MAXPARAMS`.
const MAXPARAMS: usize = 100;
/// 5.0's `LUA_MAXPARSERLEVEL`: nested expressions.
const MAXPARSERLEVEL: u32 = 200;

struct BlockScope {
    nactive: usize,
    is_loop: bool,
}

struct FuncState {
    func: Func,
    /// Active locals, innermost last.
    actives: Vec<LocalId>,
    blocks: Vec<BlockScope>,
}

struct Parser<'a> {
    lx: Lexer<'a>,
    funcs: Vec<FuncState>,
    level: u32,
}

type R<T> = Result<T, SyntaxError>;

/// Parse a chunk into its main function. 5.0 marks it vararg but declares no `arg` local (only
/// `code_params` does), so `arg` at chunk level is a global.
pub fn parse(src: &[u8], chunkname: &[u8]) -> R<Func> {
    let mut p = Parser {
        lx: Lexer::new(src, chunkname)?,
        funcs: Vec::new(),
        level: 0,
    };
    p.open_func(Rc::from(chunkname), 0);
    p.chunk()?;
    if p.lx.cur.tok != Tok::Eof {
        return Err(p.lx.error("`<eof>' expected"));
    }
    Ok(p.close_func())
}

fn block_follow(t: Tok) -> bool {
    matches!(
        t,
        Tok::Else | Tok::Elseif | Tok::End | Tok::Until | Tok::Eof
    )
}

impl<'a> Parser<'a> {
    fn fs(&mut self) -> &mut FuncState {
        self.funcs.last_mut().unwrap()
    }

    fn open_func(&mut self, source: Bytes, line: u32) {
        self.funcs.push(FuncState {
            func: Func {
                source,
                line_defined: line,
                nparams: 0,
                vararg: None,
                locals: Vec::new(),
                upvals: Vec::new(),
                body: Block::default(),
            },
            actives: Vec::new(),
            blocks: Vec::new(),
        });
    }

    fn close_func(&mut self) -> Func {
        self.funcs.pop().unwrap().func
    }

    fn tok(&self) -> Tok {
        self.lx.cur.tok
    }

    fn next(&mut self) -> R<()> {
        self.lx.advance()
    }

    fn testnext(&mut self, t: Tok) -> R<bool> {
        if self.tok() == t {
            self.next()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn error_expected(&self, t: Tok) -> SyntaxError {
        self.lx.error(&format!(
            "`{}' expected",
            String::from_utf8_lossy(&t.text())
        ))
    }

    fn check(&mut self, t: Tok) -> R<()> {
        if !self.testnext(t)? {
            return Err(self.error_expected(t));
        }
        Ok(())
    }

    fn check_match(&mut self, what: Tok, who: Tok, line: u32) -> R<()> {
        if !self.testnext(what)? {
            if line == self.lx.line {
                return Err(self.error_expected(what));
            }
            return Err(self.lx.error(&format!(
                "`{}' expected (to close `{}' at line {})",
                String::from_utf8_lossy(&what.text()),
                String::from_utf8_lossy(&who.text()),
                line
            )));
        }
        Ok(())
    }

    fn checkname(&mut self) -> R<Bytes> {
        if self.tok() != Tok::Name {
            return Err(self.lx.error("`<name>' expected"));
        }
        let name: Bytes = Rc::from(&self.lx.cur.bytes[..]);
        self.next()?;
        Ok(name)
    }

    fn limit_error(&self, what: &str, limit: usize) -> SyntaxError {
        self.lx.error(&format!("too many {what} (limit={limit})"))
    }

    // ── scopes ───────────────────────────────────────────────────────────────────────────────

    /// `new_localvar`: declared, not yet in scope. `pending` counts this declaration's earlier
    /// names, which 5.0's limit check includes.
    fn declare_n(&mut self, name: &[u8], pending: usize) -> R<LocalId> {
        if self.fs().actives.len() + pending + 1 > MAXVARS {
            return Err(self.limit_error("local variables", MAXVARS));
        }
        let fs = self.fs();
        fs.func.locals.push(LocalDesc {
            name: Rc::from(name),
            captured: false,
        });
        Ok((fs.func.locals.len() - 1) as LocalId)
    }

    fn declare(&mut self, name: &[u8]) -> R<LocalId> {
        self.declare_n(name, 0)
    }

    /// `adjustlocalvars`: the last `n` declared locals come into scope.
    fn activate(&mut self, n: usize) {
        let fs = self.fs();
        let total = fs.func.locals.len();
        for id in total - n..total {
            fs.actives.push(id as LocalId);
        }
    }

    fn enter_block(&mut self, is_loop: bool) {
        let fs = self.fs();
        let nactive = fs.actives.len();
        fs.blocks.push(BlockScope { nactive, is_loop });
    }

    fn leave_block(&mut self) {
        let fs = self.fs();
        let b = fs.blocks.pop().unwrap();
        fs.actives.truncate(b.nactive);
    }

    /// `singlevar`: the name just read, resolved through the enclosing functions.
    fn singlevar(&mut self, name: Bytes) -> R<Expr> {
        let top = self.funcs.len() - 1;
        match self.resolve(top, &name, true)? {
            Some((true, id)) => Ok(Expr::Local(id)),
            Some((false, idx)) => Ok(Expr::Upval(idx)),
            None => Ok(Expr::Global(name)),
        }
    }

    /// `singlevaraux`: `(true, local)` or `(false, upvalue)` at function `level`, or a global.
    fn resolve(&mut self, level: usize, name: &Bytes, base: bool) -> R<Option<(bool, u16)>> {
        let fs = &mut self.funcs[level];
        if let Some(&id) = fs
            .actives
            .iter()
            .rev()
            .find(|&&id| fs.func.locals[id as usize].name == *name)
        {
            if !base {
                fs.func.locals[id as usize].captured = true;
            }
            return Ok(Some((true, id)));
        }
        if level == 0 {
            return Ok(None);
        }
        let Some((from_local, index)) = self.resolve(level - 1, name, false)? else {
            return Ok(None);
        };
        // `indexupvalue`: an existing upvalue for the same variable is reused.
        let fs = &self.funcs[level];
        if let Some(i) = fs
            .func
            .upvals
            .iter()
            .position(|u| u.from_local == from_local && u.index == index)
        {
            return Ok(Some((false, i as u16)));
        }
        if fs.func.upvals.len() + 1 > MAXUPVALUES {
            return Err(self.limit_error("upvalues", MAXUPVALUES));
        }
        let fs = &mut self.funcs[level];
        fs.func.upvals.push(UpvalDesc {
            name: name.clone(),
            from_local,
            index,
        });
        Ok(Some((false, (fs.func.upvals.len() - 1) as u16)))
    }

    // ── statements ───────────────────────────────────────────────────────────────────────────

    /// `chunk`: statements until a block follows; `return` and `break` end it.
    fn chunk(&mut self) -> R<()> {
        let mut stats = Vec::new();
        self.chunk_into(&mut stats)?;
        self.fs().func.body = Block { stats };
        Ok(())
    }

    fn chunk_into(&mut self, stats: &mut Vec<Stat>) -> R<()> {
        let mut last = false;
        while !last && !block_follow(self.tok()) {
            self.enter_level()?;
            last = self.statement(stats)?;
            self.level -= 1;
            self.testnext(Tok::Char(b';'))?;
        }
        Ok(())
    }

    fn enter_level(&mut self) -> R<()> {
        self.level += 1;
        if self.level > MAXPARSERLEVEL {
            return Err(self.lx.error("too many syntax levels"));
        }
        Ok(())
    }

    /// `block`: a chunk in its own scope.
    fn block(&mut self) -> R<Block> {
        self.enter_block(false);
        let mut stats = Vec::new();
        self.chunk_into(&mut stats)?;
        self.leave_block();
        Ok(Block { stats })
    }

    fn statement(&mut self, out: &mut Vec<Stat>) -> R<bool> {
        let line = self.lx.line;
        match self.tok() {
            Tok::If => out.push(self.ifstat(line)?),
            Tok::While => {
                self.next()?;
                let cond = self.expr()?;
                self.check(Tok::Do)?;
                self.enter_block(true);
                let body = self.block()?;
                self.leave_block();
                self.check_match(Tok::End, Tok::While, line)?;
                out.push(Stat::While { cond, body });
            }
            Tok::Do => {
                self.next()?;
                let b = self.block()?;
                self.check_match(Tok::End, Tok::Do, line)?;
                out.push(Stat::Do(b));
            }
            Tok::For => out.push(self.forstat(line)?),
            Tok::Repeat => {
                // 5.1's `repeatstat`: a loop block, and inside it the scope the condition shares
                // with the body.
                self.enter_block(true);
                self.enter_block(false);
                self.next()?;
                let mut stats = Vec::new();
                self.chunk_into(&mut stats)?;
                self.check_match(Tok::Until, Tok::Repeat, line)?;
                let cond = self.expr()?;
                self.leave_block();
                self.leave_block();
                out.push(Stat::Repeat {
                    body: Block { stats },
                    cond,
                });
            }
            Tok::Function => {
                self.next()?;
                let (target, needself) = self.funcname()?;
                let func = self.body(needself, line)?;
                out.push(Stat::Function {
                    target,
                    func: Box::new(func),
                    line,
                });
            }
            Tok::Local => {
                self.next()?;
                if self.testnext(Tok::Function)? {
                    let name = self.checkname()?;
                    let var = self.declare(&name)?;
                    self.activate(1);
                    let fline = self.lx.line;
                    let func = self.body(false, fline)?;
                    out.push(Stat::LocalFunction {
                        var,
                        func: Box::new(func),
                    });
                } else {
                    out.push(self.localstat()?);
                }
            }
            Tok::Return => {
                self.next()?;
                let exprs = if block_follow(self.tok()) || self.tok() == Tok::Char(b';') {
                    Vec::new()
                } else {
                    self.explist()?
                };
                out.push(Stat::Return { exprs });
                return Ok(true);
            }
            Tok::Break => {
                self.next()?;
                if !self.fs().blocks.iter().any(|b| b.is_loop) {
                    return Err(self.lx.error("no loop to break"));
                }
                out.push(Stat::Break);
                return Ok(true);
            }
            _ => out.push(self.exprstat()?),
        }
        Ok(false)
    }

    fn ifstat(&mut self, line: u32) -> R<Stat> {
        let mut arms = Vec::new();
        // `test_then_block`, for `if` and each `elseif`.
        loop {
            self.next()?;
            let cond = self.expr()?;
            self.check(Tok::Then)?;
            let b = self.block()?;
            arms.push((cond, b));
            if self.tok() != Tok::Elseif {
                break;
            }
        }
        let orelse = if self.testnext(Tok::Else)? {
            Some(self.block()?)
        } else {
            None
        };
        self.check_match(Tok::End, Tok::If, line)?;
        Ok(Stat::If { arms, orelse })
    }

    fn forstat(&mut self, line: u32) -> R<Stat> {
        // The loop's block, which `break` leaves, holds the control variables.
        self.enter_block(true);
        self.next()?;
        let name = self.checkname()?;
        let s = match self.tok() {
            Tok::Char(b'=') => self.fornum(name, line)?,
            Tok::Char(b',') | Tok::In => self.forlist(name, line)?,
            _ => return Err(self.lx.error("`=' or `in' expected")),
        };
        self.check_match(Tok::End, Tok::For, line)?;
        self.leave_block();
        Ok(s)
    }

    fn fornum(&mut self, name: Bytes, line: u32) -> R<Stat> {
        let index = self.declare_n(b"(for index)", 0)?;
        let limit = self.declare_n(b"(for limit)", 1)?;
        let step = self.declare_n(b"(for step)", 2)?;
        let var = self.declare_n(&name, 3)?;
        self.check(Tok::Char(b'='))?;
        let start_e = self.expr()?;
        self.check(Tok::Char(b','))?;
        let limit_e = self.expr()?;
        let step_e = if self.testnext(Tok::Char(b','))? {
            Some(self.expr()?)
        } else {
            None
        };
        let body = self.forbody(1)?;
        Ok(Stat::NumFor {
            index,
            limit,
            step,
            var,
            start_e,
            limit_e,
            step_e,
            body,
            line,
        })
    }

    fn forlist(&mut self, first: Bytes, _line: u32) -> R<Stat> {
        let generator = self.declare_n(b"(for generator)", 0)?;
        let state = self.declare_n(b"(for state)", 1)?;
        let control = self.declare_n(b"(for control)", 2)?;
        let mut vars = vec![self.declare_n(&first, 3)?];
        while self.testnext(Tok::Char(b','))? {
            let n = self.checkname()?;
            vars.push(self.declare_n(&n, vars.len() + 3)?);
        }
        self.check(Tok::In)?;
        let line = self.lx.line;
        let exprs = self.explist()?;
        let body = self.forbody(vars.len())?;
        Ok(Stat::GenFor {
            generator,
            state,
            control,
            vars,
            exprs,
            body,
            line,
        })
    }

    /// 5.1's `forbody`: the three control variables, then the declared ones in a block of their
    /// own, entered fresh each iteration.
    fn forbody(&mut self, nvars: usize) -> R<Block> {
        let total = self.fs().func.locals.len();
        // `adjustlocalvars(ls, 3)`: the control variables, declared before the declared ones.
        let fs = self.fs();
        for id in total - nvars - 3..total - nvars {
            fs.actives.push(id as LocalId);
        }
        self.check(Tok::Do)?;
        self.enter_block(false);
        self.activate(nvars);
        let b = self.block()?;
        self.leave_block();
        Ok(b)
    }

    /// `funcname`: `NAME {'.' NAME} [':' NAME]`, and whether the body takes `self`.
    fn funcname(&mut self) -> R<(Expr, bool)> {
        let name = self.checkname()?;
        let mut e = self.singlevar(name)?;
        let mut needself = false;
        while self.tok() == Tok::Char(b'.') {
            e = self.field(e)?;
        }
        if self.tok() == Tok::Char(b':') {
            needself = true;
            e = self.field(e)?;
        }
        Ok((e, needself))
    }

    /// `luaY_field`: `.name` or `:name`, the separator current.
    fn field(&mut self, obj: Expr) -> R<Expr> {
        self.next()?;
        let key = self.checkname()?;
        let line = self.lx.lastline;
        Ok(Expr::Index(Box::new(obj), Box::new(Expr::Str(key)), line))
    }

    fn localstat(&mut self) -> R<Stat> {
        let mut vars = Vec::new();
        loop {
            let name = self.checkname()?;
            vars.push(self.declare_n(&name, vars.len())?);
            if !self.testnext(Tok::Char(b','))? {
                break;
            }
        }
        let exprs = if self.testnext(Tok::Char(b'='))? {
            self.explist()?
        } else {
            Vec::new()
        };
        self.activate(vars.len());
        Ok(Stat::Local { vars, exprs })
    }

    fn exprstat(&mut self) -> R<Stat> {
        let e = self.primaryexp()?;
        if e.is_multi() {
            return Ok(Stat::Call(e));
        }
        let mut targets = vec![e];
        loop {
            // `assignment`'s check: only a variable can be assigned.
            if !matches!(
                targets.last().unwrap(),
                Expr::Local(_) | Expr::Upval(_) | Expr::Global(_) | Expr::Index(..)
            ) {
                return Err(self.lx.error("syntax error"));
            }
            if !self.testnext(Tok::Char(b','))? {
                break;
            }
            targets.push(self.primaryexp()?);
        }
        self.check(Tok::Char(b'='))?;
        let exprs = self.explist()?;
        let line = self.lx.lastline;
        Ok(Stat::Assign {
            targets,
            exprs,
            line,
        })
    }

    // ── functions ────────────────────────────────────────────────────────────────────────────

    /// `body`: `(` parlist `)` chunk `end`.
    fn body(&mut self, needself: bool, line: u32) -> R<Func> {
        let source = self.fs().func.source.clone();
        self.open_func(source, line);
        self.check(Tok::Char(b'('))?;
        if needself {
            self.declare(b"self")?;
            self.activate(1);
        }
        // `parlist`
        let mut nparams = 0;
        let mut dots = false;
        if self.tok() != Tok::Char(b')') {
            loop {
                match self.tok() {
                    Tok::Dots => {
                        dots = true;
                        self.next()?;
                    }
                    Tok::Name => {
                        let n = self.checkname()?;
                        self.declare_n(&n, nparams)?;
                        nparams += 1;
                    }
                    _ => return Err(self.lx.error("<name> or `...' expected")),
                }
                if dots || !self.testnext(Tok::Char(b','))? {
                    break;
                }
            }
        }
        // `code_params`
        self.activate(nparams);
        if self.fs().actives.len() > MAXPARAMS {
            return Err(self.limit_error("parameters", MAXPARAMS));
        }
        let n = self.fs().actives.len() as u8;
        self.fs().func.nparams = n;
        if dots {
            let arg = self.declare(b"arg")?;
            self.activate(1);
            self.fs().func.vararg = Some(arg);
        }
        self.check(Tok::Char(b')'))?;
        self.chunk()?;
        self.check_match(Tok::End, Tok::Function, line)?;
        Ok(self.close_func())
    }

    // ── expressions ──────────────────────────────────────────────────────────────────────────

    fn explist(&mut self) -> R<Vec<Expr>> {
        let mut v = vec![self.expr()?];
        while self.testnext(Tok::Char(b','))? {
            v.push(self.expr()?);
        }
        Ok(v)
    }

    fn expr(&mut self) -> R<Expr> {
        Ok(self.subexpr(0)?.0)
    }

    /// `subexpr`: operators binding tighter than `limit`; answers the first untreated operator.
    fn subexpr(&mut self, limit: u8) -> R<(Expr, Option<BinOp>)> {
        self.enter_level()?;
        let mut e = match self.tok() {
            Tok::Not | Tok::Char(b'-') => {
                let op = if self.tok() == Tok::Not {
                    UnOp::Not
                } else {
                    UnOp::Neg
                };
                self.next()?;
                let (operand, _) = self.subexpr(UNARY_PRIORITY)?;
                let line = self.lx.lastline;
                Expr::Un(op, Box::new(operand), line)
            }
            _ => self.simpleexp()?,
        };
        let mut op = binop(self.tok());
        while let Some(o) = op {
            let (left, right) = priority(o);
            if left <= limit {
                break;
            }
            self.next()?;
            let (rhs, nextop) = self.subexpr(right)?;
            let line = self.lx.lastline;
            e = Expr::Bin(o, Box::new(e), Box::new(rhs), line);
            op = nextop;
        }
        self.level -= 1;
        Ok((e, op))
    }

    fn simpleexp(&mut self) -> R<Expr> {
        let e = match self.tok() {
            Tok::Number => Expr::Num(self.lx.cur.num),
            Tok::String => Expr::Str(Rc::from(&self.lx.cur.bytes[..])),
            Tok::Nil => Expr::Nil,
            Tok::True => Expr::True,
            Tok::False => Expr::False,
            Tok::Char(b'{') => return self.constructor(),
            Tok::Function => {
                self.next()?;
                let line = self.lx.line;
                return Ok(Expr::Function(Box::new(self.body(false, line)?)));
            }
            _ => return self.primaryexp(),
        };
        self.next()?;
        Ok(e)
    }

    /// `prefixexp`: a name or a parenthesised expression.
    fn prefixexp(&mut self) -> R<Expr> {
        match self.tok() {
            Tok::Char(b'(') => {
                let line = self.lx.line;
                self.next()?;
                let e = self.expr()?;
                self.check_match(Tok::Char(b')'), Tok::Char(b'('), line)?;
                Ok(Expr::Paren(Box::new(e)))
            }
            Tok::Name => {
                let name = self.checkname()?;
                self.singlevar(name)
            }
            _ => Err(self.lx.error("unexpected symbol")),
        }
    }

    fn primaryexp(&mut self) -> R<Expr> {
        let mut e = self.prefixexp()?;
        loop {
            match self.tok() {
                Tok::Char(b'.') => e = self.field(e)?,
                Tok::Char(b'[') => {
                    let key = self.index()?;
                    let line = self.lx.lastline;
                    e = Expr::Index(Box::new(e), Box::new(key), line);
                }
                Tok::Char(b':') => {
                    self.next()?;
                    let name = self.checkname()?;
                    let line = self.lx.line;
                    let args = self.funcargs()?;
                    e = Expr::Method(Box::new(e), name, args, line);
                }
                Tok::Char(b'(') | Tok::String | Tok::Char(b'{') => {
                    let line = self.lx.line;
                    let args = self.funcargs()?;
                    e = Expr::Call(Box::new(e), args, line);
                }
                _ => return Ok(e),
            }
        }
    }

    /// `luaY_index`: `[` expr `]`.
    fn index(&mut self) -> R<Expr> {
        self.next()?;
        let e = self.expr()?;
        self.check(Tok::Char(b']'))?;
        Ok(e)
    }

    fn funcargs(&mut self) -> R<Vec<Expr>> {
        let line = self.lx.line;
        match self.tok() {
            Tok::Char(b'(') => {
                if line != self.lx.lastline {
                    return Err(self
                        .lx
                        .error("ambiguous syntax (function call x new statement)"));
                }
                self.next()?;
                let args = if self.tok() == Tok::Char(b')') {
                    Vec::new()
                } else {
                    self.explist()?
                };
                self.check_match(Tok::Char(b')'), Tok::Char(b'('), line)?;
                Ok(args)
            }
            Tok::Char(b'{') => Ok(vec![self.constructor()?]),
            Tok::String => {
                let s = Expr::Str(Rc::from(&self.lx.cur.bytes[..]));
                self.next()?;
                Ok(vec![s])
            }
            _ => Err(self.lx.error("function arguments expected")),
        }
    }

    /// `constructor`, with 5.0's compat-semicolon at the head of each item.
    fn constructor(&mut self) -> R<Expr> {
        let line = self.lx.line;
        self.check(Tok::Char(b'{'))?;
        let mut fields = Vec::new();
        loop {
            self.testnext(Tok::Char(b';'))?;
            if self.tok() == Tok::Char(b'}') {
                break;
            }
            match self.tok() {
                Tok::Name => {
                    if self.lx.lookahead()? != Tok::Char(b'=') {
                        fields.push(Field::Item(self.expr()?));
                    } else {
                        let k = self.checkname()?;
                        self.check(Tok::Char(b'='))?;
                        fields.push(Field::Named(k, self.expr()?));
                    }
                }
                Tok::Char(b'[') => {
                    let k = self.index()?;
                    self.check(Tok::Char(b'='))?;
                    fields.push(Field::Pair(k, self.expr()?));
                }
                _ => fields.push(Field::Item(self.expr()?)),
            }
            if !(self.testnext(Tok::Char(b','))? || self.testnext(Tok::Char(b';'))?) {
                break;
            }
        }
        self.check_match(Tok::Char(b'}'), Tok::Char(b'{'), line)?;
        Ok(Expr::Table(fields))
    }
}

const UNARY_PRIORITY: u8 = 8;

fn binop(t: Tok) -> Option<BinOp> {
    Some(match t {
        Tok::Char(b'+') => BinOp::Add,
        Tok::Char(b'-') => BinOp::Sub,
        Tok::Char(b'*') => BinOp::Mul,
        Tok::Char(b'/') => BinOp::Div,
        Tok::Char(b'^') => BinOp::Pow,
        Tok::Concat => BinOp::Concat,
        Tok::Ne => BinOp::Ne,
        Tok::Eq => BinOp::Eq,
        Tok::Char(b'<') => BinOp::Lt,
        Tok::Le => BinOp::Le,
        Tok::Char(b'>') => BinOp::Gt,
        Tok::Ge => BinOp::Ge,
        Tok::And => BinOp::And,
        Tok::Or => BinOp::Or,
        _ => return None,
    })
}

/// 5.0's `priority`: (left, right); `^` and `..` are right associative.
fn priority(op: BinOp) -> (u8, u8) {
    match op {
        BinOp::Add | BinOp::Sub => (6, 6),
        BinOp::Mul | BinOp::Div => (7, 7),
        BinOp::Pow => (10, 9),
        BinOp::Concat => (5, 4),
        BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => (3, 3),
        BinOp::And => (2, 2),
        BinOp::Or => (1, 1),
    }
}
