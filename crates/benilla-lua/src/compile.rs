//! AST to [`Proto`]: registers allocated as 5.0's code generator allocates them (locals in
//! declaration order, temporaries above), and 5.0's observable choices kept: a constructor's
//! size hints and 32-item `SETLIST` batches (which fix the table's layout, and so `next`'s
//! order), right-to-left multiple assignment, tail calls, and loop variables shared across
//! iterations.

use std::collections::HashMap;
use std::rc::Rc;

use crate::ast::*;
use crate::heap::Heap;
use crate::lex::SyntaxError;
use crate::proto::*;
use crate::value::Value;

type R<T> = Result<T, SyntaxError>;

/// Compile a parsed main function.
pub fn compile(f: &Func, heap: &mut Heap) -> R<Rc<Proto>> {
    let fc = Fc::new(f, heap, &[]);
    fc.run()
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum KKey {
    Nil,
    Bool(bool),
    Num(u64),
    Str(u32),
}

struct Scope {
    nactive: u8,
    captured: bool,
    is_loop: bool,
    breaks: Vec<usize>,
}

struct Fc<'a> {
    heap: &'a mut Heap,
    f: &'a Func,
    code: Vec<Op>,
    lines: Vec<u32>,
    consts: Vec<Value>,
    kmap: HashMap<KKey, u32>,
    protos: Vec<Rc<Proto>>,
    reg_of: Vec<u8>,
    nactive: u8,
    freereg: u8,
    maxstack: u8,
    scopes: Vec<Scope>,
    line: u32,
    locvars: Vec<LocVar>,
    /// `locvars` index of each active local, innermost last.
    active_vars: Vec<usize>,
    upvals: Vec<(bool, u16)>,
}

/// `luaO_int2fb`: 5.0's "floating point byte" for a constructor's array hint.
fn int2fb(mut x: u32) -> u32 {
    let mut m = 0;
    while x >= 8 {
        x = (x + 1) >> 1;
        m += 1;
    }
    (m << 3) | x
}

fn fb2int(x: u32) -> u32 {
    (x & 7) << (x >> 3)
}

fn log2(x: u32) -> i32 {
    if x == 0 {
        -1
    } else {
        31 - x.leading_zeros() as i32
    }
}

/// An expression that writes its destination before it has read all its operands, so it may
/// not target a local it reads.
fn writes_early(e: &Expr) -> bool {
    match e {
        Expr::Table(_) | Expr::Bin(BinOp::And | BinOp::Or, ..) => true,
        Expr::Paren(e) => writes_early(e),
        _ => false,
    }
}

enum Target {
    Local(u8),
    Upval(u16),
    Global(u32),
    Index(u8, Rk),
}

impl<'a> Fc<'a> {
    fn new(f: &'a Func, heap: &'a mut Heap, parent_regs: &[u8]) -> Fc<'a> {
        let upvals = f
            .upvals
            .iter()
            .map(|u| {
                if u.from_local {
                    (true, parent_regs[u.index as usize] as u16)
                } else {
                    (false, u.index)
                }
            })
            .collect();
        Fc {
            heap,
            f,
            code: Vec::new(),
            lines: Vec::new(),
            consts: Vec::new(),
            kmap: HashMap::new(),
            protos: Vec::new(),
            reg_of: vec![u8::MAX; f.locals.len()],
            nactive: 0,
            freereg: 0,
            maxstack: 2,
            scopes: Vec::new(),
            line: f.line_defined.max(1),
            locvars: Vec::new(),
            active_vars: Vec::new(),
            upvals,
        }
    }

    fn run(mut self) -> R<Rc<Proto>> {
        let f = self.f;
        // Parameters, then `arg`, are the first registers.
        let nparams = f.nparams as usize + f.vararg.is_some() as usize;
        for id in 0..nparams {
            self.activate(id as LocalId)?;
        }
        self.enter_scope(false);
        self.stats(&f.body.stats)?;
        self.leave_scope();
        self.emit(Op::Return(0, 1));
        let end = self.code.len() as u32;
        for &i in &self.active_vars {
            self.locvars[i].endpc = end;
        }
        Ok(Rc::new(Proto {
            code: self.code,
            lines: self.lines,
            consts: self.consts,
            protos: self.protos,
            upvals: self.upvals,
            upval_names: f.upvals.iter().map(|u| u.name.clone()).collect(),
            nparams: f.nparams,
            vararg: f.vararg.is_some(),
            maxstack: self.maxstack as u16 + 1,
            source: f.source.clone(),
            line_defined: f.line_defined,
            locvars: self.locvars,
        }))
    }

    // ── emission ─────────────────────────────────────────────────────────────────────────────

    fn emit(&mut self, op: Op) -> usize {
        self.code.push(op);
        self.lines.push(self.line);
        self.code.len() - 1
    }

    fn here(&self) -> usize {
        self.code.len()
    }

    fn jump(&mut self) -> usize {
        self.emit(Op::Jmp(0))
    }

    /// Point the jumps at `pos` to `target`.
    fn patch_to(&mut self, jumps: &[usize], target: usize) {
        for &pos in jumps {
            let off = target as i32 - (pos as i32 + 1);
            match &mut self.code[pos] {
                Op::Jmp(o)
                | Op::JmpCmp(_, _, _, _, o)
                | Op::JmpIf(_, _, o)
                | Op::ForLoop(_, o)
                | Op::TForPrep(_, o) => *o = off,
                op => unreachable!("not a jump: {op:?}"),
            }
        }
    }

    fn patch_here(&mut self, jumps: &[usize]) {
        let h = self.here();
        self.patch_to(jumps, h);
    }

    fn too_complex(&self) -> SyntaxError {
        let mut m = crate::chunkid(&self.f.source);
        m.extend_from_slice(
            format!(":{}: function or expression too complex", self.line).as_bytes(),
        );
        SyntaxError(m)
    }

    fn reserve(&mut self, n: u8) -> R<u8> {
        let r = self.freereg;
        let top = self.freereg as usize + n as usize;
        if top > MAXSTACK {
            return Err(self.too_complex());
        }
        self.freereg = top as u8;
        self.maxstack = self.maxstack.max(self.freereg);
        Ok(r)
    }

    /// The stack must hold `top` registers (a generic for's call frame).
    fn need(&mut self, top: usize) -> R<()> {
        if top > MAXSTACK {
            return Err(self.too_complex());
        }
        self.maxstack = self.maxstack.max(top as u8);
        Ok(())
    }

    fn konst(&mut self, k: KKey, v: Value) -> u32 {
        if let Some(&i) = self.kmap.get(&k) {
            return i;
        }
        self.consts.push(v);
        let i = (self.consts.len() - 1) as u32;
        self.kmap.insert(k, i);
        i
    }

    fn kstr(&mut self, s: &[u8]) -> u32 {
        let r = self.heap.intern(s);
        self.konst(KKey::Str(r.id), Value::Str(r))
    }

    fn knum(&mut self, n: f64) -> u32 {
        self.konst(KKey::Num(n.to_bits()), Value::Num(n))
    }

    /// A constant as an RK operand, if its index fits.
    fn rk_const(&mut self, e: &Expr) -> Option<Rk> {
        let k = match e {
            Expr::Nil => self.konst(KKey::Nil, Value::Nil),
            Expr::True => self.konst(KKey::Bool(true), Value::Bool(true)),
            Expr::False => self.konst(KKey::Bool(false), Value::Bool(false)),
            Expr::Num(n) => self.knum(*n),
            Expr::Str(s) => self.kstr(s),
            _ => return None,
        };
        if k + (RK_CONST as u32) < u16::MAX as u32 {
            Some(k as u16 + RK_CONST)
        } else {
            None
        }
    }

    // ── scopes ───────────────────────────────────────────────────────────────────────────────

    fn activate(&mut self, id: LocalId) -> R<()> {
        let reg = self.nactive;
        self.reg_of[id as usize] = reg;
        self.nactive += 1;
        if self.freereg < self.nactive {
            self.reserve(self.nactive - self.freereg)?;
        }
        self.locvars.push(LocVar {
            name: self.f.locals[id as usize].name.clone(),
            reg,
            startpc: self.here() as u32,
            endpc: 0,
        });
        self.active_vars.push(self.locvars.len() - 1);
        if self.f.locals[id as usize].captured {
            if let Some(s) = self.scopes.last_mut() {
                s.captured = true;
            }
        }
        Ok(())
    }

    fn enter_scope(&mut self, is_loop: bool) {
        self.scopes.push(Scope {
            nactive: self.nactive,
            captured: false,
            is_loop,
            breaks: Vec::new(),
        });
    }

    /// 5.0's `leaveblock`: the scope's locals end, its open upvalues close; answers its breaks.
    fn leave_scope(&mut self) -> Vec<usize> {
        let s = self.scopes.pop().unwrap();
        let end = self.here() as u32;
        while self.active_vars.len() > s.nactive as usize {
            let i = self.active_vars.pop().unwrap();
            self.locvars[i].endpc = end;
        }
        if s.captured {
            self.emit(Op::Close(s.nactive));
        }
        self.nactive = s.nactive;
        self.freereg = s.nactive;
        s.breaks
    }

    /// `block`: statements in a scope of their own.
    fn block(&mut self, b: &Block) -> R<()> {
        self.enter_scope(false);
        self.stats(&b.stats)?;
        self.leave_scope();
        Ok(())
    }

    // ── statements ───────────────────────────────────────────────────────────────────────────

    fn stats(&mut self, stats: &[Stat]) -> R<()> {
        for s in stats {
            self.stat(s)?;
            debug_assert_eq!(self.freereg, self.nactive);
        }
        Ok(())
    }

    fn stat(&mut self, s: &Stat) -> R<()> {
        match s {
            Stat::Call(e) => {
                self.call(e, 0)?;
                self.freereg = self.nactive;
            }
            Stat::Local { vars, exprs } => {
                self.explist_adjust(exprs, vars.len())?;
                for &v in vars {
                    self.activate(v)?;
                }
                self.freereg = self.nactive;
            }
            Stat::LocalFunction { var, func } => {
                self.activate(*var)?;
                let reg = self.reg_of[*var as usize];
                self.closure(func, reg)?;
            }
            Stat::Assign {
                targets,
                exprs,
                line,
            } => self.assign(targets, exprs, *line)?,
            Stat::Do(b) => self.block(b)?,
            Stat::While { cond, body } => {
                let start = self.here();
                let exits = self.cond(cond, false)?;
                self.enter_scope(true);
                self.block(body)?;
                let j = self.jump();
                self.patch_to(&[j], start);
                let breaks = self.leave_scope();
                self.patch_here(&exits);
                self.patch_here(&breaks);
            }
            Stat::Repeat { body, cond } => {
                let start = self.here();
                self.enter_scope(true);
                self.block(body)?;
                let back = self.cond(cond, false)?;
                self.patch_to(&back, start);
                let breaks = self.leave_scope();
                self.patch_here(&breaks);
            }
            Stat::If { arms, orelse } => {
                let mut ends = Vec::new();
                for (i, (c, b)) in arms.iter().enumerate() {
                    let skip = self.cond(c, false)?;
                    self.block(b)?;
                    if i + 1 < arms.len() || orelse.is_some() {
                        ends.push(self.jump());
                    }
                    self.patch_here(&skip);
                }
                if let Some(b) = orelse {
                    self.block(b)?;
                }
                self.patch_here(&ends);
            }
            Stat::NumFor {
                var,
                limit,
                step,
                start_e,
                limit_e,
                step_e,
                body,
                line,
            } => {
                self.enter_scope(false);
                let base = self.freereg;
                let r = self.reserve(1)?;
                self.exp2reg(start_e, r)?;
                let r = self.reserve(1)?;
                self.exp2reg(limit_e, r)?;
                let r = self.reserve(1)?;
                match step_e {
                    Some(e) => self.exp2reg(e, r)?,
                    None => {
                        let k = self.knum(1.0);
                        self.emit(Op::LoadK(r, k));
                    }
                }
                self.line = *line;
                // 5.0 primes the counter one step back, then enters at the loop test; the
                // variables come into scope after, so an error here names no local.
                self.emit(Op::Arith(Arith::Sub, base, base as Rk, base as Rk + 2));
                let prep = self.jump();
                self.activate(*var)?;
                self.activate(*limit)?;
                self.activate(*step)?;
                self.enter_scope(true);
                let body_start = self.here();
                self.block(body)?;
                self.patch_here(&[prep]);
                self.line = *line;
                let l = self.emit(Op::ForLoop(base, 0));
                self.patch_to(&[l], body_start);
                let breaks = self.leave_scope();
                self.patch_here(&breaks);
                self.leave_scope();
            }
            Stat::GenFor {
                generator,
                state,
                vars,
                exprs,
                body,
                line,
            } => {
                self.enter_scope(false);
                let base = self.freereg;
                let nvars = vars.len() + 2;
                self.line = *line;
                self.explist_adjust(exprs, nvars)?;
                self.activate(*generator)?;
                self.activate(*state)?;
                for &v in vars {
                    self.activate(v)?;
                }
                // The call frame above the variables: generator, state, control.
                self.need(base as usize + nvars + 3)?;
                let prep = self.emit(Op::TForPrep(base, 0));
                self.enter_scope(true);
                let body_start = self.here();
                self.block(body)?;
                self.patch_here(&[prep]);
                self.line = *line;
                self.emit(Op::TForLoop(base, vars.len() as u8));
                let back = self.jump();
                self.patch_to(&[back], body_start);
                let breaks = self.leave_scope();
                self.patch_here(&breaks);
                self.leave_scope();
            }
            Stat::Function { target, func, line } => {
                self.line = *line;
                match target {
                    Expr::Local(id) => {
                        let reg = self.reg_of[*id as usize];
                        self.closure(func, reg)?;
                    }
                    Expr::Upval(i) => {
                        let r = self.reserve(1)?;
                        self.closure(func, r)?;
                        self.emit(Op::SetUpval(r, *i));
                    }
                    Expr::Global(n) => {
                        let r = self.reserve(1)?;
                        self.closure(func, r)?;
                        let k = self.kstr(n);
                        self.emit(Op::SetGlobal(r, k));
                    }
                    Expr::Index(o, k, _) => {
                        let ro = self.exp2anyreg(o)?;
                        let rk = self.exp2rk(k)?;
                        let r = self.reserve(1)?;
                        self.closure(func, r)?;
                        self.line = *line;
                        self.emit(Op::SetTable(ro, rk, r as Rk));
                    }
                    _ => unreachable!("funcname is a variable"),
                }
                self.freereg = self.nactive;
            }
            Stat::Return { exprs } => {
                self.ret(exprs)?;
                self.freereg = self.nactive;
            }
            Stat::Break => {
                // 5.0's `breakstat`: close every scope's upvalues up to the loop's.
                let mut upval = false;
                let mut loop_idx = None;
                for (i, s) in self.scopes.iter().enumerate().rev() {
                    upval |= s.captured;
                    if s.is_loop {
                        loop_idx = Some(i);
                        break;
                    }
                }
                let i = loop_idx.expect("the parser checked for a loop");
                if upval {
                    let n = self.scopes[i].nactive;
                    self.emit(Op::Close(n));
                }
                let j = self.jump();
                self.scopes[i].breaks.push(j);
            }
        }
        Ok(())
    }

    fn ret(&mut self, exprs: &[Expr]) -> R<()> {
        if exprs.len() == 1 && exprs[0].is_multi() {
            // `return f(x)`: a tail call.
            let base = self.call(&exprs[0], -1)?;
            let last = self.code.len() - 1;
            if let Op::Call(a, b, _) = self.code[last] {
                self.code[last] = Op::TailCall(a, b);
            }
            self.emit(Op::Return(base, 0));
            return Ok(());
        }
        if exprs.len() == 1 {
            if let Expr::Local(id) = &exprs[0] {
                let r = self.reg_of[*id as usize];
                self.emit(Op::Return(r, 2));
                return Ok(());
            }
        }
        let base = self.freereg;
        let multi = self.explist(exprs)?;
        let b = if multi { 0 } else { exprs.len() as u8 + 1 };
        self.emit(Op::Return(base, b));
        Ok(())
    }

    fn assign(&mut self, targets: &[Expr], exprs: &[Expr], line: u32) -> R<()> {
        if targets.len() == 1 && exprs.len() == 1 {
            let e = &exprs[0];
            match &targets[0] {
                Expr::Local(id) => {
                    let reg = self.reg_of[*id as usize];
                    if writes_early(e) {
                        let r = self.reserve(1)?;
                        self.exp2reg(e, r)?;
                        self.emit(Op::Move(reg, r));
                    } else {
                        self.exp2reg(e, reg)?;
                    }
                }
                Expr::Upval(i) => {
                    let r = self.exp2anyreg(e)?;
                    self.emit(Op::SetUpval(r, *i));
                }
                Expr::Global(n) => {
                    let r = self.exp2anyreg(e)?;
                    let k = self.kstr(n);
                    self.emit(Op::SetGlobal(r, k));
                }
                Expr::Index(o, k, _) => {
                    let ro = self.exp2anyreg(o)?;
                    let rk = self.exp2rk(k)?;
                    let rv = self.exp2rk(e)?;
                    self.line = line;
                    self.emit(Op::SetTable(ro, rk, rv));
                }
                _ => unreachable!("the parser checked the target"),
            }
            self.freereg = self.nactive;
            return Ok(());
        }
        // 5.0's `check_conflict`: a local assigned here and read by an indexed target's table
        // or key is copied first, since the stores run right to left.
        let assigned: Vec<LocalId> = targets
            .iter()
            .filter_map(|t| match t {
                Expr::Local(id) => Some(*id),
                _ => None,
            })
            .collect();
        let mut slots = Vec::new();
        for t in targets {
            slots.push(match t {
                Expr::Local(id) => Target::Local(self.reg_of[*id as usize]),
                Expr::Upval(i) => Target::Upval(*i),
                Expr::Global(n) => Target::Global(self.kstr(n)),
                Expr::Index(o, k, _) => {
                    let conflict = |e: &Expr| matches!(e, Expr::Local(id) if assigned.contains(id));
                    let ro = if conflict(o) {
                        let r = self.reserve(1)?;
                        self.exp2reg(o, r)?;
                        r
                    } else {
                        self.exp2anyreg(o)?
                    };
                    let rk = if conflict(k) {
                        let r = self.reserve(1)?;
                        self.exp2reg(k, r)?;
                        r as Rk
                    } else {
                        self.exp2rk(k)?
                    };
                    Target::Index(ro, rk)
                }
                _ => unreachable!("the parser checked the target"),
            });
        }
        let base = self.freereg;
        self.explist_adjust(exprs, targets.len())?;
        self.line = line;
        for (i, t) in slots.iter().enumerate().rev() {
            let r = base + i as u8;
            match *t {
                Target::Local(reg) => {
                    self.emit(Op::Move(reg, r));
                }
                Target::Upval(u) => {
                    self.emit(Op::SetUpval(r, u));
                }
                Target::Global(k) => {
                    self.emit(Op::SetGlobal(r, k));
                }
                Target::Index(ro, rk) => {
                    self.emit(Op::SetTable(ro, rk, r as Rk));
                }
            }
        }
        self.freereg = self.nactive;
        Ok(())
    }

    // ── expression lists ─────────────────────────────────────────────────────────────────────

    /// Every expression into consecutive registers from `freereg`, a trailing call keeping all
    /// its values. Answers whether it did (the count is then "up to the top").
    fn explist(&mut self, exprs: &[Expr]) -> R<bool> {
        for (i, e) in exprs.iter().enumerate() {
            if i + 1 == exprs.len() && e.is_multi() {
                self.call(e, -1)?;
                return Ok(true);
            }
            let r = self.reserve(1)?;
            self.exp2reg(e, r)?;
        }
        Ok(false)
    }

    /// 5.0's `adjust_assign`: exactly `n` values from `freereg` on, extras evaluated and dropped,
    /// missing ones nil.
    fn explist_adjust(&mut self, exprs: &[Expr], n: usize) -> R<()> {
        let base = self.freereg;
        for (i, e) in exprs.iter().enumerate() {
            if i + 1 == exprs.len() && e.is_multi() {
                let want = (n as i32 - i as i32).max(0);
                self.call(e, want)?;
                self.freereg = base + i as u8 + want as u8;
            } else {
                let r = self.reserve(1)?;
                self.exp2reg(e, r)?;
            }
        }
        let have = self.freereg as usize - base as usize;
        if have < n {
            let first = self.reserve((n - have) as u8)?;
            self.emit(Op::LoadNil(first, base + n as u8 - 1));
        }
        self.freereg = base + n as u8;
        self.maxstack = self.maxstack.max(self.freereg);
        Ok(())
    }

    // ── calls ────────────────────────────────────────────────────────────────────────────────

    /// A call with its function at `freereg`, keeping `nres` results there (-1: all, up to the
    /// top). Answers the base register; `freereg` ends just past the kept results.
    fn call(&mut self, e: &Expr, nres: i32) -> R<u8> {
        let base = self.freereg;
        let (args, line, nfixed) = match e {
            Expr::Call(f, args, line) => {
                let r = self.reserve(1)?;
                self.exp2reg(f, r)?;
                (args, *line, 1)
            }
            Expr::Method(obj, name, args, line) => {
                let o = self.exp2anyreg(obj)?;
                self.freereg = base;
                self.reserve(2)?;
                let k = self.kstr(name);
                let rk = if (k as usize) + (RK_CONST as usize) < u16::MAX as usize {
                    k as u16 + RK_CONST
                } else {
                    let r = self.reserve(1)?;
                    self.emit(Op::LoadK(r, k));
                    self.freereg -= 1;
                    r as Rk
                };
                self.line = *line;
                self.emit(Op::SelfOp(base, o, rk));
                (args, *line, 2)
            }
            _ => unreachable!("a call"),
        };
        let multi = self.explist(args)?;
        let b = if multi {
            0
        } else {
            (args.len() + nfixed) as u8
        };
        self.line = line;
        self.emit(Op::Call(base, b, (nres + 1) as u8));
        self.freereg = base;
        if nres > 0 {
            self.reserve(nres as u8)?;
        }
        Ok(base)
    }

    // ── expressions ──────────────────────────────────────────────────────────────────────────

    fn exp2anyreg(&mut self, e: &Expr) -> R<u8> {
        match e {
            Expr::Local(id) => Ok(self.reg_of[*id as usize]),
            Expr::Paren(inner) if matches!(**inner, Expr::Local(_)) => self.exp2anyreg(inner),
            _ => {
                let r = self.reserve(1)?;
                self.exp2reg(e, r)?;
                Ok(r)
            }
        }
    }

    fn exp2rk(&mut self, e: &Expr) -> R<Rk> {
        if let Some(k) = self.rk_const(e) {
            return Ok(k);
        }
        Ok(self.exp2anyreg(e)? as Rk)
    }

    /// `e`'s value into `dst`; temporaries above `freereg` are freed again.
    fn exp2reg(&mut self, e: &Expr, dst: u8) -> R<()> {
        let save = self.freereg;
        match e {
            Expr::Nil => {
                self.emit(Op::LoadNil(dst, dst));
            }
            Expr::True => {
                self.emit(Op::LoadBool(dst, true));
            }
            Expr::False => {
                self.emit(Op::LoadBool(dst, false));
            }
            Expr::Num(n) => {
                let k = self.knum(*n);
                self.emit(Op::LoadK(dst, k));
            }
            Expr::Str(s) => {
                let k = self.kstr(s);
                self.emit(Op::LoadK(dst, k));
            }
            Expr::Local(id) => {
                let r = self.reg_of[*id as usize];
                if r != dst {
                    self.emit(Op::Move(dst, r));
                }
            }
            Expr::Upval(i) => {
                self.emit(Op::GetUpval(dst, *i));
            }
            Expr::Global(n) => {
                let k = self.kstr(n);
                self.emit(Op::GetGlobal(dst, k));
            }
            Expr::Index(o, k, line) => {
                let ro = self.exp2anyreg(o)?;
                let rk = self.exp2rk(k)?;
                self.line = *line;
                self.emit(Op::GetTable(dst, ro, rk));
            }
            Expr::Call(..) | Expr::Method(..) => {
                // The top temporary is the call's own base: no move.
                if dst + 1 == self.freereg && dst >= self.nactive {
                    self.freereg = dst;
                    self.call(e, 1)?;
                } else {
                    let base = self.call(e, 1)?;
                    self.emit(Op::Move(dst, base));
                }
            }
            Expr::Function(f) => self.closure(f, dst)?,
            Expr::Table(fields) => self.constructor(fields, dst)?,
            Expr::Paren(inner) => self.exp2reg(inner, dst)?,
            Expr::Un(UnOp::Neg, inner, line) => {
                if let Expr::Num(n) = **inner {
                    // 5.0's `luaK_prefix` folds a negated numeral.
                    let k = self.knum(-n);
                    self.emit(Op::LoadK(dst, k));
                } else {
                    let r = self.exp2anyreg(inner)?;
                    self.line = *line;
                    self.emit(Op::Unm(dst, r));
                }
            }
            Expr::Un(UnOp::Not, inner, line) => {
                let r = self.exp2anyreg(inner)?;
                self.line = *line;
                self.emit(Op::Not(dst, r));
            }
            Expr::Bin(op, l, r, line) => match op {
                BinOp::And | BinOp::Or => {
                    self.exp2reg(l, dst)?;
                    let j = self.emit(Op::JmpIf(dst, *op == BinOp::Or, 0));
                    self.exp2reg(r, dst)?;
                    self.patch_here(&[j]);
                }
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    let t = self.cond(e, true)?;
                    self.emit(Op::LoadBool(dst, false));
                    let j = self.jump();
                    self.patch_here(&t);
                    self.emit(Op::LoadBool(dst, true));
                    self.patch_here(&[j]);
                }
                BinOp::Concat => {
                    let mut parts = vec![&**l];
                    let mut rest = &**r;
                    while let Expr::Bin(BinOp::Concat, a, b, _) = rest {
                        parts.push(a);
                        rest = b;
                    }
                    parts.push(rest);
                    let first = self.freereg;
                    for p in parts {
                        let reg = self.reserve(1)?;
                        self.exp2reg(p, reg)?;
                    }
                    self.line = *line;
                    self.emit(Op::Concat(dst, first, self.freereg - 1));
                }
                _ => {
                    let a = match op {
                        BinOp::Add => Arith::Add,
                        BinOp::Sub => Arith::Sub,
                        BinOp::Mul => Arith::Mul,
                        BinOp::Div => Arith::Div,
                        _ => Arith::Pow,
                    };
                    let rb = self.exp2rk(l)?;
                    let rc = self.exp2rk(r)?;
                    self.line = *line;
                    self.emit(Op::Arith(a, dst, rb, rc));
                }
            },
        }
        self.freereg = save;
        Ok(())
    }

    /// Code that jumps when `e`'s truth is `jump_when` and falls through otherwise; answers the
    /// jumps to patch.
    fn cond(&mut self, e: &Expr, jump_when: bool) -> R<Vec<usize>> {
        match e {
            Expr::Nil | Expr::False => Ok(if jump_when { vec![] } else { vec![self.jump()] }),
            Expr::True | Expr::Num(_) | Expr::Str(_) => {
                Ok(if jump_when { vec![self.jump()] } else { vec![] })
            }
            Expr::Un(UnOp::Not, inner, _) => self.cond(inner, !jump_when),
            Expr::Paren(inner) => self.cond(inner, jump_when),
            Expr::Bin(BinOp::And, a, b, _) => {
                if jump_when {
                    let skip = self.cond(a, false)?;
                    let j = self.cond(b, true)?;
                    self.patch_here(&skip);
                    Ok(j)
                } else {
                    let mut j = self.cond(a, false)?;
                    j.extend(self.cond(b, false)?);
                    Ok(j)
                }
            }
            Expr::Bin(BinOp::Or, a, b, _) => {
                if jump_when {
                    let mut j = self.cond(a, true)?;
                    j.extend(self.cond(b, true)?);
                    Ok(j)
                } else {
                    let skip = self.cond(a, true)?;
                    let j = self.cond(b, false)?;
                    self.patch_here(&skip);
                    Ok(j)
                }
            }
            Expr::Bin(
                op @ (BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge),
                l,
                r,
                line,
            ) => {
                let save = self.freereg;
                let rb = self.exp2rk(l)?;
                let rc = self.exp2rk(r)?;
                let (cmp, swap, neg) = match op {
                    BinOp::Eq => (Cmp::Eq, false, false),
                    BinOp::Ne => (Cmp::Eq, false, true),
                    BinOp::Lt => (Cmp::Lt, false, false),
                    BinOp::Le => (Cmp::Le, false, false),
                    BinOp::Gt => (Cmp::Lt, true, false),
                    _ => (Cmp::Le, true, false),
                };
                let (b, c) = if swap { (rc, rb) } else { (rb, rc) };
                self.line = *line;
                let j = self.emit(Op::JmpCmp(cmp, jump_when != neg, b, c, 0));
                self.freereg = save;
                Ok(vec![j])
            }
            _ => {
                let save = self.freereg;
                let r = self.exp2anyreg(e)?;
                let j = self.emit(Op::JmpIf(r, jump_when, 0));
                self.freereg = save;
                Ok(vec![j])
            }
        }
    }

    fn closure(&mut self, f: &Func, dst: u8) -> R<()> {
        let proto = Fc::new(f, self.heap, &self.reg_of).run()?;
        self.protos.push(proto);
        self.emit(Op::Closure(dst, (self.protos.len() - 1) as u32));
        Ok(())
    }

    /// 5.0's `constructor`: `NEWTABLE` sized by the hints, named fields stored as they come,
    /// list items flushed every 32, in place, by `SETLIST`.
    fn constructor(&mut self, fields: &[Field], dst: u8) -> R<()> {
        // The table must sit below its pending items.
        if dst >= self.freereg {
            self.freereg = dst + 1;
            self.maxstack = self.maxstack.max(self.freereg);
        }
        let na = fields
            .iter()
            .filter(|f| matches!(f, Field::Item(_)))
            .count() as u32;
        let nh = fields
            .iter()
            .filter(|f| matches!(f, Field::Named(..)))
            .count() as u32;
        let pc = self.emit(Op::NewTable(dst, 0, 0));
        let items_base = self.freereg;
        let mut pending = 0u32;
        let mut stored = 0u32;
        for (i, f) in fields.iter().enumerate() {
            if pending as usize == FIELDS_PER_FLUSH {
                self.emit(Op::SetList(dst, stored, pending as u16));
                stored += pending;
                pending = 0;
                self.freereg = items_base;
            }
            match f {
                Field::Item(e) => {
                    if i + 1 == fields.len() && e.is_multi() {
                        self.call(e, -1)?;
                        self.emit(Op::SetListTop(dst, stored));
                        self.freereg = items_base;
                        pending = 0;
                        continue;
                    }
                    let r = self.reserve(1)?;
                    self.exp2reg(e, r)?;
                    pending += 1;
                }
                Field::Named(k, v) => {
                    let save = self.freereg;
                    let kk = self.kstr(k);
                    let rk = if (kk as usize) + (RK_CONST as usize) < u16::MAX as usize {
                        kk as u16 + RK_CONST
                    } else {
                        self.exp2anyreg(&Expr::Str(k.clone()))? as Rk
                    };
                    let rv = self.exp2rk(v)?;
                    self.emit(Op::SetTable(dst, rk, rv));
                    self.freereg = save;
                }
                Field::Pair(k, v) => {
                    let save = self.freereg;
                    let rk = self.exp2rk(k)?;
                    let rv = self.exp2rk(v)?;
                    self.emit(Op::SetTable(dst, rk, rv));
                    self.freereg = save;
                }
            }
        }
        if pending > 0 {
            self.emit(Op::SetList(dst, stored, pending as u16));
        }
        self.freereg = items_base;
        let lnh = (log2(nh) + 1) as u8;
        self.code[pc] = Op::NewTable(dst, fb2int(int2fb(na)), lnh);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constructor_hints_are_5_0_s() {
        assert_eq!(fb2int(int2fb(9)), 10);
        assert_eq!(fb2int(int2fb(7)), 7);
        assert_eq!(fb2int(int2fb(33)), 40);
        assert_eq!(log2(0) + 1, 0);
        assert_eq!(log2(3) + 1, 2);
    }
}
