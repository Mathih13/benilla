//! Compiled functions: 5.1.5's register machine, with each test fused with the jump after it
//! and the common operand shapes given ops of their own.

/// Byte strings the compiler, the heap and hot reload share: names, sources, function keys.
pub type Bytes = std::rc::Rc<[u8]>;

use crate::value::Value;

pub type Reg = u8;
/// A register below [`RK_CONST`], a constant index at or above it.
pub type Rk = u16;
pub const RK_CONST: u16 = 256;

/// 5.1's `MAXSTACK`: a function uses fewer registers than this.
pub const MAXSTACK: usize = 250;
/// 5.1's `LFIELDS_PER_FLUSH`: list items a constructor stores per `SetList`.
pub const FIELDS_PER_FLUSH: u32 = 50;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmp {
    Eq,
    Lt,
    Le,
}

/// A comparison and the result that jumps, in one byte, so a compare-and-jump fits in an
/// eight-byte instruction.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CmpJ(u8);

impl CmpJ {
    pub fn new(cmp: Cmp, cond: bool) -> CmpJ {
        CmpJ((cmp as u8) << 1 | cond as u8)
    }

    #[inline(always)]
    pub fn cmp(self) -> Cmp {
        match self.0 >> 1 {
            0 => Cmp::Eq,
            1 => Cmp::Lt,
            _ => Cmp::Le,
        }
    }

    #[inline(always)]
    pub fn cond(self) -> bool {
        self.0 & 1 != 0
    }
}

impl std::fmt::Debug for CmpJ {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{:?}={}", self.cmp(), self.cond())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arith {
    Add,
    Sub,
    Mul,
    Div,
    Pow,
}

#[derive(Clone, Copy, Debug)]
pub enum Op {
    Move(Reg, Reg),
    LoadK(Reg, u32),
    /// `a..=b` become nil.
    LoadNil(Reg, Reg),
    LoadBool(Reg, bool),
    GetUpval(Reg, u16),
    SetUpval(Reg, u16),
    GetGlobal(Reg, u32),
    SetGlobal(Reg, u32),
    GetTable(Reg, Reg, Rk),
    SetTable(Reg, Rk, Rk),
    /// `R(a) = R(b)[K(c)]` for a string constant key: `GetTable` without the key's type test.
    GetField(Reg, Reg, u16),
    /// `R(a)[K(b)] = RK(c)` for a string constant key.
    SetField(Reg, u16, Rk),
    /// `R(a) = {}` with an array part and a hash part of `2^c` nodes.
    NewTable(Reg, u32, u8),
    /// `R(a+1) = R(b); R(a) = R(b)[RK(c)]`.
    SelfOp(Reg, Reg, Rk),
    /// `SelfOp` for a string constant name: the method call `o:m()`.
    SelfField(Reg, Reg, u16),
    /// `R(a) = R(b) op R(c)`. A constant operand has its own ops (`AddK`, `AddKR`).
    Add(Reg, Reg, Reg),
    Sub(Reg, Reg, Reg),
    Mul(Reg, Reg, Reg),
    Div(Reg, Reg, Reg),
    Pow(Reg, Rk, Rk),
    /// `R(a) = R(b) op K(c)`: the common `i + 1`, without the register-or-constant test.
    AddK(Reg, Reg, u16),
    SubK(Reg, Reg, u16),
    MulK(Reg, Reg, u16),
    DivK(Reg, Reg, u16),
    /// `R(a) = K(b) op R(c)`: the common `2 * i`, operands in source order.
    AddKR(Reg, u16, Reg),
    SubKR(Reg, u16, Reg),
    MulKR(Reg, u16, Reg),
    DivKR(Reg, u16, Reg),
    Unm(Reg, Reg),
    Not(Reg, Reg),
    /// `R(a) = R(b) .. ... .. R(c)`.
    Concat(Reg, Reg, Reg),
    Jmp(i32),
    /// Jump when `cmp(R(b), R(c)) == cond`.
    JmpCmp(CmpJ, Reg, Reg, i32),
    /// `cmp(R(b), K(c))`: the common `n < 2`, `x == nil`. Constants past the first 256 are
    /// loaded into a register and compared with `JmpCmp`.
    JmpCmpK(CmpJ, Reg, u8, i32),
    /// `cmp(K(b), R(c))`: `n > 2`, swapped into `2 < n`.
    JmpCmpKR(CmpJ, u8, Reg, i32),
    /// 5.1's `TEST` and its `JMP`: jump when the truth of `R(a)` is `cond`.
    JmpIf(Reg, bool, i32),
    /// 5.1's `TESTSET` and its `JMP`: when the truth of `R(b)` is `cond`, `R(a) = R(b)` and jump.
    TestSet(Reg, Reg, bool, i32),
    /// `b` is the argument count + 1 (0: up to the top), `c` the result count + 1 (0: all).
    Call(Reg, u8, u8),
    TailCall(Reg, u8),
    /// Return `R(a)..`, `b - 1` values (0: up to the top).
    Return(Reg, u8),
    /// Return `R(a)` alone.
    Return1(Reg),
    /// 5.1's `FORPREP`: `R(a)`, `R(a+1)`, `R(a+2)` made numbers or the loop refused, the index
    /// primed one step back; then the jump to the loop's `ForLoop`.
    ForPrep(Reg, i32),
    /// 5.1's `FORLOOP`: `R(a) += R(a+2)`; while within `R(a+1)`, `R(a+3) = R(a)` and jump.
    ForLoop(Reg, i32),
    /// `ForLoop` whose step the compiler knows is a positive constant: the test is `<=`.
    ForLoopUp(Reg, i32),
    /// 5.0's `TFORPREP`: a table generator becomes `next` over it; then the jump.
    TForPrep(Reg, i32),
    /// 5.1's `TFORLOOP`: call `R(a)(R(a+1), R(a+2))` into the `b` variables from `R(a+3)`; unless
    /// the first is nil it becomes the control `R(a+2)`, else the next instruction, the jump back
    /// to the body, is skipped.
    TForLoop(Reg, u8),
    /// 5.1's `SETLIST`: `R(a)[base + i] = R(a + i)` for `i` in `1..=n`, the array part grown
    /// first to hold them all.
    SetList(Reg, u32, u16),
    /// The same, items up to the top (the last item was a call).
    SetListTop(Reg, u32),
    /// Close every open upvalue at or above `R(a)`.
    Close(Reg),
    Closure(Reg, u32),
}

// Every instruction is eight bytes: the dispatch loads one word and decodes only its own fields.
const _: () = assert!(std::mem::size_of::<Op>() == 8);

/// A local's name and the registers' lifetime, for error messages.
#[derive(Clone, Debug)]
pub struct LocVar {
    pub name: Bytes,
    pub reg: Reg,
    pub startpc: u32,
    pub endpc: u32,
}

#[derive(Clone, Debug)]
pub struct Proto {
    pub code: Vec<Op>,
    pub lines: Vec<u32>,
    pub consts: Vec<Value>,
    pub protos: Vec<std::rc::Rc<Proto>>,
    /// Each upvalue: `(true, register)` in the enclosing frame, or `(false, index)` of its
    /// upvalues.
    pub upvals: Vec<(bool, u16)>,
    pub upval_names: Vec<Bytes>,
    /// The declaration each upvalue captures, for hot reload: the declaring function's key, the
    /// name, and which declaration of that name it is (`MyAddon|x#2`).
    pub upval_decls: Vec<Bytes>,
    /// Parameters including `self`.
    pub nparams: u8,
    /// `...`: the extras become the `arg` table in register `nparams`.
    pub vararg: bool,
    pub maxstack: u16,
    pub source: Bytes,
    pub line_defined: u32,
    pub locvars: Vec<LocVar>,
    /// The function's identity within its chunk, stable across edits that leave its name and
    /// nesting alone. A function is named as written (`Frame:OnEvent`, `Frame.Init`,
    /// `local helper`), or after the single target it is assigned to (`H.OnShow = function`,
    /// `local f = function`, a constructor's `name = function`), or `anon`; a name used twice
    /// is numbered (`Init/anon#2`). Hot reload matches on it.
    pub key: Bytes,
    /// For each part of `key`, how many siblings share its name: a numbered key matches the
    /// same function across an edit only while these counts hold.
    pub key_counts: Box<[u32]>,
    /// Every key of the chunk this function was compiled in.
    pub chunk_keys: std::rc::Rc<[Bytes]>,
    /// `source`'s id in the heap's chunk list, for profiles and memory censuses.
    pub source_id: u32,
    /// The collection that last marked this prototype's constants (`Heap::collect`).
    pub(crate) gc_mark: std::cell::Cell<u32>,
}

impl Proto {
    /// The local in register `reg` at `pc`, if one is active there.
    pub fn local_name(&self, reg: Reg, pc: usize) -> Option<&Bytes> {
        self.locvars
            .iter()
            .rev()
            .find(|l| l.reg == reg && (l.startpc as usize) <= pc && pc < l.endpc as usize)
            .map(|l| &l.name)
    }
}

impl Proto {
    /// Check, once per compiled function, every fact the interpreter's unchecked reads take on
    /// trust: each register an instruction touches is below `maxstack`, each constant, upvalue
    /// and child index exists, each jump lands inside the code, and the code ends in a return,
    /// so execution never runs off it. A failure is a code generator bug, and it stops here
    /// instead of becoming a read outside the frame.
    pub(crate) fn verify(&self) {
        self.verify_in(None);
    }

    /// `parent`: the enclosing function's register count and upvalue count, which this
    /// function's upvalue descriptions index.
    fn verify_in(&self, parent: Option<(u16, usize)>) {
        let fail = |pc: usize, what: &str| -> ! {
            panic!(
                "benilla-lua compiled bad code: {what} at instruction {pc} of `{}` in {}",
                String::from_utf8_lossy(&self.key),
                String::from_utf8_lossy(&self.source),
            )
        };
        let n = self.code.len();
        let (nk, nup, nproto) = (self.consts.len(), self.upval_names.len(), self.protos.len());
        if !matches!(self.code.last(), Some(Op::Return(..) | Op::Return1(_))) {
            fail(n, "no final return");
        }
        if self.lines.len() != n {
            fail(n, "a line per instruction");
        }
        if (self.nparams as usize + self.vararg as usize) > self.maxstack as usize
            || self.maxstack as usize > MAXSTACK + 5
        {
            fail(0, "the register count");
        }
        if self.upvals.len() != nup {
            fail(0, "the upvalue count");
        }
        if let Some((pmax, pups)) = parent {
            for &(in_stack, i) in &self.upvals {
                if (in_stack && i >= pmax) || (!in_stack && i as usize >= pups) {
                    fail(0, "an upvalue the enclosing function lacks");
                }
            }
        }
        for (pc, &op) in self.code.iter().enumerate() {
            let reg = |r: usize| {
                if r >= self.maxstack as usize {
                    fail(pc, "a register past maxstack");
                }
            };
            let k = |i: usize| {
                if i >= nk {
                    fail(pc, "a constant past the table");
                }
            };
            let rk = |x: Rk| {
                if x >= RK_CONST {
                    k((x - RK_CONST) as usize)
                } else {
                    reg(x as usize)
                }
            };
            let to = |skip: usize, off: i32| {
                let t = pc as i64 + 1 + skip as i64 + off as i64;
                if t < 0 || t >= n as i64 {
                    fail(pc, "a jump outside the code");
                }
            };
            let r = |x: Reg| reg(x as usize);
            let ku = |i: u16| k(i as usize);
            match op {
                Op::Move(x, y) | Op::Unm(x, y) | Op::Not(x, y) => {
                    r(x);
                    r(y);
                }
                Op::LoadK(x, i) | Op::GetGlobal(x, i) | Op::SetGlobal(x, i) => {
                    r(x);
                    k(i as usize);
                }
                Op::LoadNil(x, y) => {
                    if y < x {
                        fail(pc, "an empty nil range");
                    }
                    r(y);
                }
                Op::LoadBool(x, _) | Op::Close(x) | Op::NewTable(x, ..) | Op::Return1(x) => r(x),
                Op::SetListTop(x, _) => r(x),
                Op::GetUpval(x, u) | Op::SetUpval(x, u) => {
                    if u as usize >= nup {
                        fail(pc, "an upvalue past the list");
                    }
                    r(x);
                }
                Op::GetTable(x, y, z) => {
                    r(x);
                    r(y);
                    rk(z);
                }
                Op::SetTable(x, y, z) | Op::Pow(x, y, z) => {
                    r(x);
                    rk(y);
                    rk(z);
                }
                Op::GetField(x, y, i)
                | Op::AddK(x, y, i)
                | Op::SubK(x, y, i)
                | Op::MulK(x, y, i)
                | Op::DivK(x, y, i) => {
                    r(x);
                    r(y);
                    ku(i);
                }
                Op::SetField(x, i, z) => {
                    r(x);
                    ku(i);
                    rk(z);
                }
                Op::SelfOp(x, y, z) => {
                    reg(x as usize + 1);
                    r(y);
                    rk(z);
                }
                Op::SelfField(x, y, i) => {
                    reg(x as usize + 1);
                    r(y);
                    ku(i);
                }
                Op::Add(x, y, z) | Op::Sub(x, y, z) | Op::Mul(x, y, z) | Op::Div(x, y, z) => {
                    r(x);
                    r(y);
                    r(z);
                }
                Op::AddKR(x, i, z)
                | Op::SubKR(x, i, z)
                | Op::MulKR(x, i, z)
                | Op::DivKR(x, i, z) => {
                    r(x);
                    ku(i);
                    r(z);
                }
                Op::Concat(x, y, z) => {
                    if z <= y {
                        fail(pc, "a concat of fewer than two values");
                    }
                    r(x);
                    r(z);
                }
                Op::Jmp(off) => to(0, off),
                Op::JmpCmp(_, y, z, off) => {
                    r(y);
                    r(z);
                    to(0, off);
                }
                Op::JmpCmpK(_, y, i, off) => {
                    r(y);
                    k(i as usize);
                    to(0, off);
                }
                Op::JmpCmpKR(_, i, z, off) => {
                    k(i as usize);
                    r(z);
                    to(0, off);
                }
                Op::JmpIf(x, _, off) => {
                    r(x);
                    to(0, off);
                }
                Op::TestSet(x, y, _, off) => {
                    r(x);
                    r(y);
                    to(0, off);
                }
                // Arguments `a+1..a+b-1`, results `a..a+c-2`; a count of zero is up to the top,
                // which the stack covers at run time.
                Op::Call(x, nb, nc) => {
                    let x = x as usize;
                    reg(x + (nb as usize).saturating_sub(1));
                    reg(x + (nc as usize).saturating_sub(2));
                }
                Op::TailCall(x, nb) => reg(x as usize + (nb as usize).saturating_sub(1)),
                // `Return(a, 1)` returns nothing, and its `a` may be the first free register.
                Op::Return(x, nb) => {
                    if nb > 1 {
                        reg(x as usize + nb as usize - 2);
                    } else if x as usize > self.maxstack as usize {
                        fail(pc, "a return past maxstack");
                    }
                }
                Op::ForPrep(x, off) | Op::ForLoop(x, off) | Op::ForLoopUp(x, off) => {
                    reg(x as usize + 3);
                    to(0, off);
                }
                Op::TForPrep(x, off) => {
                    reg(x as usize + 2);
                    to(0, off);
                }
                // The loop variables `a+3..a+2+b`; past a nil, the jump after it is skipped.
                Op::TForLoop(x, nb) => {
                    reg(x as usize + 2 + (nb as usize).max(1));
                    to(1, 0);
                }
                Op::SetList(x, _, cnt) => reg(x as usize + cnt as usize),
                Op::Closure(x, i) => {
                    if i as usize >= nproto {
                        fail(pc, "a child function past the list");
                    }
                    r(x);
                }
            }
        }
        for p in &self.protos {
            p.verify_in(Some((self.maxstack, nup)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compiled(src: &str) -> Proto {
        let mut heap = crate::heap::Heap::default();
        let p = crate::compile::compile(src.as_bytes(), b"=t", &mut heap, 0).unwrap();
        (*p).clone()
    }

    fn rejects(p: Proto) -> String {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| p.verify()));
        let e = r.expect_err("the verifier refuses it");
        e.downcast_ref::<String>().cloned().unwrap_or_default()
    }

    #[test]
    fn the_verifier_refuses_code_the_interpreter_would_read_past() {
        let mut p = compiled("local a = 1 return a");
        p.code[0] = Op::Move(p.maxstack as Reg, 0);
        assert!(rejects(p).contains("a register past maxstack"));

        let mut p = compiled("local a = 1 return a");
        p.code[0] = Op::LoadK(0, p.consts.len() as u32);
        assert!(rejects(p).contains("a constant past the table"));

        let mut p = compiled("local a = 1 return a");
        p.code[0] = Op::Jmp(p.code.len() as i32);
        assert!(rejects(p).contains("a jump outside the code"));

        let mut p = compiled("local a = 1 return a");
        *p.code.last_mut().unwrap() = Op::Move(0, 0);
        assert!(rejects(p).contains("no final return"));

        let mut p = compiled("local u = 1 F = function() return u end");
        let mut child = (*p.protos[0]).clone();
        child.code[0] = Op::GetUpval(0, 5);
        p.protos[0] = std::rc::Rc::new(child);
        assert!(rejects(p).contains("an upvalue past the list"));
    }
}
