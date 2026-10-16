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
