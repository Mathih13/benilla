//! Compiled functions: a register machine close to 5.0's, with compare-and-jump fused.

use crate::ast::Bytes;
use crate::value::Value;

pub type Reg = u8;
/// A register below [`RK_CONST`], a constant index at or above it.
pub type Rk = u16;
pub const RK_CONST: u16 = 256;

/// 5.0's `MAXSTACK`: registers per function.
pub const MAXSTACK: usize = 250;
/// 5.0's `LFIELDS_PER_FLUSH`.
pub const FIELDS_PER_FLUSH: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmp {
    Eq,
    Lt,
    Le,
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
    /// `R(a) = {}` with an array part and a hash part of `2^c` nodes.
    NewTable(Reg, u32, u8),
    /// `R(a+1) = R(b); R(a) = R(b)[RK(c)]`.
    SelfOp(Reg, Reg, Rk),
    Arith(Arith, Reg, Rk, Rk),
    Unm(Reg, Reg),
    Not(Reg, Reg),
    /// `R(a) = R(b) .. ... .. R(c)`.
    Concat(Reg, Reg, Reg),
    Jmp(i32),
    /// Jump when `cmp(RK(b), RK(c)) == cond`.
    JmpCmp(Cmp, bool, Rk, Rk, i32),
    /// Jump when the truth of `R(a)` is `cond`.
    JmpIf(Reg, bool, i32),
    /// `b` is the argument count + 1 (0: up to the top), `c` the result count + 1 (0: all).
    Call(Reg, u8, u8),
    TailCall(Reg, u8),
    /// Return `R(a)..`, `b - 1` values (0: up to the top).
    Return(Reg, u8),
    /// 5.0's numeric loop step: `R(a) += R(a+2)`, and while within `R(a+1)`, jump.
    ForLoop(Reg, i32),
    /// 5.0's `TFORPREP`: a table generator becomes `next` over it; then the jump.
    TForPrep(Reg, i32),
    /// 5.0's `TFORLOOP`: call `R(a)(R(a+1), R(a+2))` into the `b` variables; on nil, skip the
    /// next instruction, a jump back to the body.
    TForLoop(Reg, u8),
    /// `R(a)[base + i] = R(a + i)` for `i` in `1..=n`: the pending list items.
    SetList(Reg, u32, u16),
    /// The same, items up to the top (the last item was a call).
    SetListTop(Reg, u32),
    /// Close every open upvalue at or above `R(a)`.
    Close(Reg),
    Closure(Reg, u32),
}

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
    /// Parameters including `self`.
    pub nparams: u8,
    /// `...`: the extras become the `arg` table in register `nparams`.
    pub vararg: bool,
    pub maxstack: u16,
    pub source: Bytes,
    pub line_defined: u32,
    pub locvars: Vec<LocVar>,
    /// The function's identity within its chunk, stable across edits that leave its name and
    /// nesting alone: `Frame:OnEvent`, `local helper`, `Init/anon#2`. Hot reload matches on it.
    pub key: Bytes,
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
