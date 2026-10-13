//! The virtual machine: one stack, one call-frame list, and 5.0's semantics for calls,
//! metamethods, coercions and error messages.

use std::rc::Rc;

use crate::heap::{FuncObj, Heap, NativeFn, UpvalObj, UserObj};
use crate::number::{fmt_g14, str2d};
use crate::proto::{Arith, Cmp, Op, Proto, Reg, Rk, RK_CONST};
use crate::table::KeyError;
use crate::value::{FuncRef, StrRef, TableRef, Value};

/// An error in flight: the error object, which `pcall` answers.
#[derive(Clone, Copy, Debug)]
pub struct LuaError {
    pub value: Value,
    /// The innermost protected call's handler already ran on it.
    handled: bool,
}

/// 5.0's `LUA_MAXCCALLS`: nested host-level calls.
const MAXCCALLS: u32 = 200;
/// 5.0's `LUA_MAXCALLS`: nested Lua calls.
const MAXCALLS: usize = 4096;
/// 5.0's `MAXTAGLOOP`.
const MAXTAGLOOP: u32 = 100;

#[derive(Clone)]
struct Frame {
    func: FuncRef,
    /// The stack slot of the called function, where its results go.
    func_idx: usize,
    base: usize,
    pc: usize,
    /// Results the caller wants; -1 for all.
    nresults: i32,
    /// The code this frame runs, null for a native. Valid while the frame exists: its closure is
    /// a collector root, and a hot reload retires the code it replaces instead of dropping it
    /// (`Vm::retired`).
    proto: *const Proto,
    /// Tail calls this frame replaced, which 5.0 counts as stack levels.
    tailcalls: u32,
    /// Returning from this frame ends the `execute` that entered it.
    boundary: bool,
}

/// Metamethod and library names, interned once.
#[derive(Clone)]
pub struct Names {
    pub index: StrRef,
    pub newindex: StrRef,
    pub call: StrRef,
    pub add: StrRef,
    pub sub: StrRef,
    pub mul: StrRef,
    pub div: StrRef,
    pub pow: StrRef,
    pub unm: StrRef,
    pub concat: StrRef,
    pub eq: StrRef,
    pub lt: StrRef,
    pub le: StrRef,
    pub mode: StrRef,
    pub metatable: StrRef,
    pub tostring: StrRef,
    pub n: StrRef,
    pub next: StrRef,
    pub gpow: StrRef,
}

#[derive(Clone)]
pub struct Vm {
    pub heap: Heap,
    pub stack: Vec<Value>,
    pub top: usize,
    frames: Vec<Frame>,
    /// Open upvalues, by stack slot, ascending.
    open: Vec<(usize, u32)>,
    pub globals: TableRef,
    pub registry: TableRef,
    pub names: Names,
    c_depth: u32,
    /// The handler of each active protected call, innermost last; `None` for `pcall`.
    handlers: Vec<Option<Value>>,
    /// MSVC's `rand()` state, which `math.random` steps; 1 until `randomseed`.
    pub rand_seed: u32,
    /// Collect at every safe point: a test switch that finds a value the roots miss.
    pub gc_stress: bool,
    /// Code and upvalue lists a hot reload replaced, kept while a frame may still run them.
    pub(crate) retired: Vec<(Rc<Proto>, Box<[u32]>)>,
    /// Bumped by every `setfenv` of a Lua function, so a running frame's cached environment
    /// knows to reread.
    pub(crate) fenv_gen: u64,
}

/// What a call produced, for the dispatch loop.
enum Called {
    /// A Lua frame was pushed; run it.
    Lua,
    /// A native ran; its results are in place.
    Done,
}

impl Default for Vm {
    fn default() -> Self {
        Self::new()
    }
}

impl Vm {
    pub fn new() -> Vm {
        let mut heap = Heap::default();
        let mut s = |b: &[u8]| heap.intern(b);
        let names = Names {
            index: s(b"__index"),
            newindex: s(b"__newindex"),
            call: s(b"__call"),
            add: s(b"__add"),
            sub: s(b"__sub"),
            mul: s(b"__mul"),
            div: s(b"__div"),
            pow: s(b"__pow"),
            unm: s(b"__unm"),
            concat: s(b"__concat"),
            eq: s(b"__eq"),
            lt: s(b"__lt"),
            le: s(b"__le"),
            mode: s(b"__mode"),
            metatable: s(b"__metatable"),
            tostring: s(b"__tostring"),
            n: s(b"n"),
            next: s(b"next"),
            gpow: s(b"__pow"),
        };
        let globals = heap.new_table(0, 0);
        let registry = heap.new_table(0, 0);
        let mut vm = Vm {
            heap,
            stack: vec![Value::Nil; 256],
            top: 0,
            frames: Vec::new(),
            open: Vec::new(),
            globals,
            registry,
            names,
            c_depth: 0,
            handlers: Vec::new(),
            rand_seed: 1,
            gc_stress: false,
            retired: Vec::new(),
            fenv_gen: 0,
        };
        vm.heap.threshold = 2 * vm.heap.bytes.max(32 * 1024);
        vm
    }

    // ── strings and values ───────────────────────────────────────────────────────────────────

    pub fn str(&mut self, s: &[u8]) -> Value {
        self.heap.str_value(s)
    }

    pub fn bytes(&self, v: Value) -> Option<&[u8]> {
        match v {
            Value::Str(s) => Some(self.heap.str(s)),
            _ => None,
        }
    }

    /// `luaV_tonumber`: a number, or a string `str2d` reads.
    pub fn tonumber(&self, v: Value) -> Option<f64> {
        match v {
            Value::Num(n) => Some(n),
            Value::Str(s) => str2d(self.heap.str(s)),
            _ => None,
        }
    }

    /// `luaV_tostring`'s conversion: a string, or a number as `%.14g`.
    pub fn tostr(&mut self, v: Value) -> Option<StrRef> {
        match v {
            Value::Str(s) => Some(s),
            Value::Num(n) => Some(self.heap.intern(fmt_g14(n).as_bytes())),
            _ => None,
        }
    }

    pub fn get_global(&mut self, name: &str) -> Value {
        let k = self.str(name.as_bytes());
        self.heap.table(self.globals).get(k)
    }

    pub fn set_global(&mut self, name: &str, v: Value) {
        let k = self.str(name.as_bytes());
        let g = self.globals;
        self.heap.table_mut(g).set(k, v).unwrap();
    }

    pub fn new_native(&mut self, name: &'static str, f: NativeFn) -> Value {
        let env = self.globals;
        Value::Func(self.heap.new_func(FuncObj::Native {
            f,
            name,
            upvals: Box::new([]),
            env,
        }))
    }

    pub fn register(&mut self, name: &'static str, f: NativeFn) {
        let v = self.new_native(name, f);
        self.set_global(name, v);
    }

    pub fn new_user(&mut self, tag: u64, meta: Option<TableRef>) -> Value {
        Value::User(self.heap.new_user(UserObj { meta, tag }))
    }

    // ── loading ──────────────────────────────────────────────────────────────────────────────

    /// Compile a chunk into a function over the globals.
    pub fn load(&mut self, src: &[u8], chunkname: &[u8]) -> Result<Value, crate::SyntaxError> {
        let ast = crate::parse::parse(src, chunkname)?;
        let proto = crate::compile::compile(&ast, &mut self.heap)?;
        let env = self.globals;
        Ok(Value::Func(self.heap.new_func(FuncObj::Lua {
            proto,
            upvals: Box::new([]),
            env,
        })))
    }

    /// Load and run a chunk protected, answering its results or its error value.
    pub fn exec(&mut self, src: &[u8], chunkname: &[u8]) -> Result<Vec<Value>, Value> {
        let f = match self.load(src, chunkname) {
            Ok(f) => f,
            Err(e) => return Err(self.str(&e.0)),
        };
        self.pcall_value(f, &[], -1)
    }

    /// Call `f` protected: its results, or its error value with the state unwound.
    pub fn pcall_value(
        &mut self,
        f: Value,
        args: &[Value],
        nres: i32,
    ) -> Result<Vec<Value>, Value> {
        let func_idx = self.top;
        self.ensure(func_idx + args.len() + 1);
        self.stack[func_idx] = f;
        self.stack[func_idx + 1..func_idx + 1 + args.len()].copy_from_slice(args);
        self.top = func_idx + 1 + args.len();
        match self.pcall_at(func_idx, args.len(), nres, None) {
            Ok(()) => {
                let out = self.stack[func_idx..self.top].to_vec();
                self.top = func_idx;
                Ok(out)
            }
            Err(e) => Err(e.value),
        }
    }

    /// A value as a Rust string, for hosts and tests: strings and numbers as `tostring` writes
    /// them, other values by type.
    pub fn display(&mut self, v: Value) -> String {
        match self.tostr(v) {
            Some(s) => String::from_utf8_lossy(self.heap.str(s)).into_owned(),
            None => match v {
                Value::Nil => "nil".into(),
                Value::Bool(b) => b.to_string(),
                other => other.type_name().into(),
            },
        }
    }

    // ── errors ───────────────────────────────────────────────────────────────────────────────

    /// Raise `v`: the innermost protected call's handler runs here, before the stack unwinds, as
    /// 5.0's `luaD_throw` runs `errfunc` at the raise.
    pub fn throw(&mut self, v: Value) -> LuaError {
        let mut e = LuaError {
            value: v,
            handled: false,
        };
        if let Some(Some(h)) = self.handlers.last().copied() {
            // The handler runs unprotected; an error inside it is 5.0's "error in error handling".
            self.handlers.push(None);
            let r = self.call_value(h, &[v], 1);
            self.handlers.pop();
            e.value = match r {
                Ok(vals) => vals.first().copied().unwrap_or(Value::Nil),
                Err(_) => self.str(b"error in error handling"),
            };
            e.handled = true;
        }
        e
    }

    /// `luaG_runerror`: the message with the current Lua function's position.
    pub fn rt_error(&mut self, msg: &str) -> LuaError {
        let mut out = self.where_(0);
        out.extend_from_slice(msg.as_bytes());
        let v = self.str(&out);
        self.throw(v)
    }

    /// `luaL_error`: the message with the position of the Lua function `level` frames up
    /// (`luaL_where(L, 1)` from a native names its caller).
    pub fn error_at(&mut self, level: usize, msg: &str) -> LuaError {
        let mut out = self.where_(level);
        out.extend_from_slice(msg.as_bytes());
        let v = self.str(&out);
        self.throw(v)
    }

    /// `luaL_where`: `chunk:line: ` of the frame `level` up, if it is a Lua function.
    pub fn where_(&self, level: usize) -> Vec<u8> {
        let Some(fi) = self.frame_at_level(level) else {
            return Vec::new();
        };
        let f = &self.frames[fi];
        // SAFETY: a live frame's code is alive (see `Frame::proto`).
        match unsafe { f.proto.as_ref() } {
            Some(p) => {
                let pc = f.pc.saturating_sub(1);
                let line = p.lines.get(pc).copied().unwrap_or(0);
                if line == 0 {
                    return Vec::new();
                }
                let mut out = crate::chunkid_n(&p.source, 60);
                out.extend_from_slice(format!(":{line}: ").as_bytes());
                out
            }
            None => Vec::new(),
        }
    }

    /// The function `level` frames up: `None` past the bottom, `Some(None)` for a level a tail
    /// call replaced.
    pub fn function_at_level(&self, level: usize) -> Option<Option<FuncRef>> {
        let mut lv = level as i64;
        let mut i = self.frames.len();
        while i > 0 {
            i -= 1;
            if lv == 0 {
                return Some(Some(self.frames[i].func));
            }
            lv -= 1;
            if !self.frames[i].proto.is_null() {
                lv -= self.frames[i].tailcalls as i64;
            }
            if lv < 0 {
                return Some(None);
            }
        }
        None
    }

    pub(crate) fn frames_empty(&self) -> bool {
        self.frames.is_empty()
    }

    pub(crate) fn native_base(&self) -> usize {
        self.frames.last().unwrap().base
    }

    pub(crate) fn running(&self) -> FuncRef {
        self.frames.last().unwrap().func
    }

    /// 5.0's `lua_getstack`: the frame `level` up, tail calls counting as levels.
    fn frame_at_level(&self, level: usize) -> Option<usize> {
        let mut level = level as i64;
        let mut i = self.frames.len();
        while i > 0 {
            i -= 1;
            if level == 0 {
                return Some(i);
            }
            level -= 1;
            if !self.frames[i].proto.is_null() {
                level -= self.frames[i].tailcalls as i64;
            }
            if level < 0 {
                return None;
            }
        }
        None
    }

    /// 5.0's `getobjname` for register `reg` of the current Lua frame: `("local", name)` and so
    /// on, from the locals and a symbolic run of the code up to the current instruction.
    fn objname(&self, fi: usize, reg: usize) -> Option<(&'static str, Vec<u8>)> {
        let f = &self.frames[fi];
        // SAFETY: a live frame's code is alive (see `Frame::proto`).
        let p = unsafe { f.proto.as_ref() }?;
        let pc = f.pc.saturating_sub(1);
        objname_in(p, &self.heap, pc, reg)
    }

    /// `luaG_typeerror`: `attempt to <op> <kind> `<name>' (a <type> value)`.
    fn type_error(&mut self, v: Value, reg: Option<usize>, op: &str) -> LuaError {
        let t = v.type_name();
        let fi = self.frames.len() - 1;
        let named = reg.and_then(|r| self.objname(fi, r));
        let msg = match named {
            Some((kind, name)) => format!(
                "attempt to {op} {kind} `{}' (a {t} value)",
                String::from_utf8_lossy(&name)
            ),
            None => format!("attempt to {op} a {t} value"),
        };
        self.rt_error(&msg)
    }

    fn order_error(&mut self, a: Value, b: Value) -> LuaError {
        let (t1, t2) = (a.type_name(), b.type_name());
        // 5.0 compares the third character of the two type names.
        let msg = if t1.as_bytes()[2] == t2.as_bytes()[2] {
            format!("attempt to compare two {t1} values")
        } else {
            format!("attempt to compare {t1} with {t2}")
        };
        self.rt_error(&msg)
    }

    // ── stack ────────────────────────────────────────────────────────────────────────────────

    #[inline]
    pub fn ensure(&mut self, n: usize) {
        if self.stack.len() < n {
            self.stack.resize(n.next_power_of_two(), Value::Nil);
        }
    }

    pub fn push(&mut self, v: Value) {
        self.ensure(self.top + 1);
        self.stack[self.top] = v;
        self.top += 1;
    }

    // ── upvalues ─────────────────────────────────────────────────────────────────────────────

    /// `luaF_findupval`: the open upvalue for stack slot `idx`, made if absent.
    fn find_upval(&mut self, idx: usize) -> u32 {
        match self.open.binary_search_by_key(&idx, |&(i, _)| i) {
            Ok(p) => self.open[p].1,
            Err(p) => {
                let u = self.heap.new_upval(UpvalObj::Open(idx as u32));
                self.open.insert(p, (idx, u));
                u
            }
        }
    }

    /// `luaF_close`: every open upvalue at or above `level` takes its value.
    fn close_upvals(&mut self, level: usize) {
        while let Some(&(idx, u)) = self.open.last() {
            if idx < level {
                break;
            }
            *self.heap.upval_mut(u) = UpvalObj::Closed(self.stack[idx]);
            self.open.pop();
        }
    }

    #[inline]
    fn upval_get(&self, u: u32) -> Value {
        match self.heap.upval(u) {
            UpvalObj::Open(i) => self.stack[i as usize],
            UpvalObj::Closed(v) => v,
        }
    }

    #[inline]
    fn upval_set(&mut self, u: u32, v: Value) {
        match self.heap.upval(u) {
            UpvalObj::Open(i) => self.stack[i as usize] = v,
            UpvalObj::Closed(_) => *self.heap.upval_mut(u) = UpvalObj::Closed(v),
        }
    }

    // ── metamethods ──────────────────────────────────────────────────────────────────────────

    /// `luaT_gettmbyobj`: only tables and userdata carry metatables in 1.12.
    fn tm(&self, v: Value, name: StrRef) -> Value {
        match self.heap.metatable(v) {
            Some(m) => self.heap.table(m).get(Value::Str(name)),
            None => Value::Nil,
        }
    }

    /// `callTMres`: `f(a, b)`'s first result.
    fn call_tm_res(&mut self, f: Value, a: Value, b: Value) -> Result<Value, LuaError> {
        let r = self.call_value(f, &[a, b], 1)?;
        Ok(r[0])
    }

    /// `call_binTM`: the first operand's metamethod, else the second's; it must be a function.
    fn bin_tm(&mut self, a: Value, b: Value, name: StrRef) -> Result<Option<Value>, LuaError> {
        let mut tm = self.tm(a, name);
        if tm.is_nil() {
            tm = self.tm(b, name);
        }
        if !matches!(tm, Value::Func(_)) {
            return Ok(None);
        }
        Ok(Some(self.call_tm_res(tm, a, b)?))
    }

    /// `luaV_gettable`. `reg` names the indexed value for an error.
    pub fn index(&mut self, t: Value, key: Value, reg: Option<usize>) -> Result<Value, LuaError> {
        let mut t = t;
        let mut reg = reg;
        for _ in 0..=MAXTAGLOOP {
            let tm = match t {
                Value::Table(tr) => {
                    let tab = self.heap.table(tr);
                    let v = tab.get(key);
                    if !v.is_nil() {
                        return Ok(v);
                    }
                    let Some(m) = tab.meta else {
                        return Ok(Value::Nil);
                    };
                    let tm = self.heap.table(m).get(Value::Str(self.names.index));
                    if tm.is_nil() {
                        return Ok(Value::Nil);
                    }
                    tm
                }
                _ => {
                    let tm = self.tm(t, self.names.index);
                    if tm.is_nil() {
                        return Err(self.type_error(t, reg, "index"));
                    }
                    tm
                }
            };
            if let Value::Func(_) = tm {
                return self.call_tm_res(tm, t, key);
            }
            t = tm;
            reg = None;
        }
        Err(self.rt_error("loop in gettable"))
    }

    /// `luaV_settable`: 5.0 makes the key in a table before it consults `__newindex`.
    pub fn setindex(
        &mut self,
        t: Value,
        key: Value,
        val: Value,
        reg: Option<usize>,
    ) -> Result<(), LuaError> {
        let mut t = t;
        let mut reg = reg;
        for _ in 0..=MAXTAGLOOP {
            let tm = match t {
                Value::Table(tr) => {
                    let slot = match self.heap.table_mut(tr).slot_for(key) {
                        Ok(s) => s,
                        Err(KeyError::Nil) => return Err(self.rt_error("table index is nil")),
                        Err(KeyError::NaN) => return Err(self.rt_error("table index is NaN")),
                    };
                    let tab = self.heap.table(tr);
                    let tm = if tab.slot(slot).is_nil() {
                        match tab.meta {
                            Some(m) => self.heap.table(m).get(Value::Str(self.names.newindex)),
                            None => Value::Nil,
                        }
                    } else {
                        Value::Nil
                    };
                    if tm.is_nil() {
                        *self.heap.table_mut(tr).slot_mut(slot) = val;
                        return Ok(());
                    }
                    tm
                }
                _ => {
                    let tm = self.tm(t, self.names.newindex);
                    if tm.is_nil() {
                        return Err(self.type_error(t, reg, "index"));
                    }
                    tm
                }
            };
            if let Value::Func(_) = tm {
                self.call_value(tm, &[t, key, val], 0)?;
                return Ok(());
            }
            t = tm;
            reg = None;
        }
        Err(self.rt_error("loop in settable"))
    }

    /// 5.0's `Arith`. Numbers (and numeric strings) compute, `^` through the global `__pow`;
    /// anything else tries the operands' metamethods.
    fn arith(
        &mut self,
        op: Arith,
        a: Value,
        b: Value,
        regs: (Option<usize>, Option<usize>),
    ) -> Result<Value, LuaError> {
        if let (Some(x), Some(y)) = (self.tonumber(a), self.tonumber(b)) {
            return Ok(Value::Num(match op {
                Arith::Add => x + y,
                Arith::Sub => x - y,
                Arith::Mul => x * y,
                Arith::Div => x / y,
                Arith::Pow => {
                    let f = self
                        .heap
                        .table(self.globals)
                        .get(Value::Str(self.names.gpow));
                    if !matches!(f, Value::Func(_)) {
                        return Err(self.rt_error("`__pow' (`^' operator) is not a function"));
                    }
                    return self.call_tm_res(f, Value::Num(x), Value::Num(y));
                }
            }));
        }
        let name = match op {
            Arith::Add => self.names.add,
            Arith::Sub => self.names.sub,
            Arith::Mul => self.names.mul,
            Arith::Div => self.names.div,
            Arith::Pow => self.names.pow,
        };
        match self.bin_tm(a, b, name)? {
            Some(v) => Ok(v),
            None => {
                // `luaG_aritherror`: blame the first operand unless it converts.
                let (bad, reg) = if self.tonumber(a).is_none() {
                    (a, regs.0)
                } else {
                    (b, regs.1)
                };
                Err(self.type_error(bad, reg, "perform arithmetic on"))
            }
        }
    }

    /// `luaV_equalObj`: tables and userdata consult a shared `__eq`.
    pub fn equals(&mut self, a: Value, b: Value) -> Result<bool, LuaError> {
        match (a, b) {
            (Value::Table(x), Value::Table(y)) if x != y => {}
            (Value::User(x), Value::User(y)) if x != y => {}
            _ => return Ok(a.raw_eq(b)),
        }
        let tm = self.comp_tm(a, b, self.names.eq);
        match tm {
            Some(f) => Ok(self.call_tm_res(f, a, b)?.truthy()),
            None => Ok(false),
        }
    }

    /// `get_compTM`: both operands' metatables must give the same metamethod.
    fn comp_tm(&self, a: Value, b: Value, name: StrRef) -> Option<Value> {
        let m1 = self.heap.metatable(a)?;
        let tm1 = self.heap.table(m1).get(Value::Str(name));
        if tm1.is_nil() {
            return None;
        }
        let m2 = self.heap.metatable(b)?;
        if m1 == m2 {
            return Some(tm1);
        }
        let tm2 = self.heap.table(m2).get(Value::Str(name));
        if tm1.raw_eq(tm2) {
            Some(tm1)
        } else {
            None
        }
    }

    /// `call_orderTM`: `Some(result)`, or `None` with no shared metamethod.
    fn order_tm(&mut self, a: Value, b: Value, name: StrRef) -> Result<Option<bool>, LuaError> {
        let tm1 = self.tm(a, name);
        if tm1.is_nil() {
            return Ok(None);
        }
        let tm2 = self.tm(b, name);
        if !tm1.raw_eq(tm2) {
            return Ok(None);
        }
        Ok(Some(self.call_tm_res(tm1, a, b)?.truthy()))
    }

    /// `l_strcmp` under the "C" locale: bytewise, embedded zeros included.
    fn str_lt(&self, a: StrRef, b: StrRef) -> std::cmp::Ordering {
        self.heap.str(a).cmp(self.heap.str(b))
    }

    pub fn less_than(&mut self, a: Value, b: Value) -> Result<bool, LuaError> {
        match (a, b) {
            (Value::Num(x), Value::Num(y)) => Ok(x < y),
            (Value::Str(x), Value::Str(y)) => Ok(self.str_lt(x, y).is_lt()),
            _ if a.type_name() == b.type_name() => match self.order_tm(a, b, self.names.lt)? {
                Some(r) => Ok(r),
                None => Err(self.order_error(a, b)),
            },
            _ => Err(self.order_error(a, b)),
        }
    }

    fn less_equal(&mut self, a: Value, b: Value) -> Result<bool, LuaError> {
        match (a, b) {
            (Value::Num(x), Value::Num(y)) => Ok(x <= y),
            (Value::Str(x), Value::Str(y)) => Ok(self.str_lt(x, y).is_le()),
            _ if a.type_name() == b.type_name() => {
                if let Some(r) = self.order_tm(a, b, self.names.le)? {
                    return Ok(r);
                }
                // 5.0: `a <= b` as `not (b < a)`.
                match self.order_tm(b, a, self.names.lt)? {
                    Some(r) => Ok(!r),
                    None => Err(self.order_error(a, b)),
                }
            }
            _ => Err(self.order_error(a, b)),
        }
    }

    /// 5.0's `luaV_concat` over `stack[first..=last]`, collapsing from the right; the result is
    /// left in `stack[first]`.
    fn concat(&mut self, first: usize, last: usize) -> Result<(), LuaError> {
        let mut total = last - first + 1;
        let mut last = last;
        while total > 1 {
            let top = last + 1;
            let mut n = 2;
            let a = self.stack[top - 2];
            let b = self.stack[top - 1];
            let (sa, sb) = (self.tostr(a), self.tostr(b));
            match (sa, sb) {
                (Some(sa), Some(sb)) => {
                    self.stack[top - 2] = Value::Str(sa);
                    self.stack[top - 1] = Value::Str(sb);
                    if !self.heap.str(sb).is_empty() {
                        while n < total {
                            let v = self.stack[top - n - 1];
                            match self.tostr(v) {
                                Some(s) => {
                                    self.stack[top - n - 1] = Value::Str(s);
                                    n += 1;
                                }
                                None => break,
                            }
                        }
                        let mut buf = Vec::new();
                        for i in (1..=n).rev() {
                            if let Value::Str(s) = self.stack[top - i] {
                                buf.extend_from_slice(self.heap.str(s));
                            }
                        }
                        self.stack[top - n] = self.str(&buf);
                    }
                }
                _ => match self.bin_tm(a, b, self.names.concat)? {
                    Some(v) => self.stack[top - 2] = v,
                    None => {
                        // `luaG_concaterror`: blame the first operand unless it is a string.
                        let (bad, reg) = if matches!(a, Value::Str(_) | Value::Num(_)) {
                            (b, top - 1)
                        } else {
                            (a, top - 2)
                        };
                        let base = self.frames.last().unwrap().base;
                        return Err(self.type_error(bad, Some(reg - base), "concatenate"));
                    }
                },
            }
            total -= n - 1;
            last -= n - 1;
        }
        Ok(())
    }

    // ── calls ────────────────────────────────────────────────────────────────────────────────

    /// Call `f` with `args` from the host or a native, answering `nres` results (-1: all).
    pub fn call_value(
        &mut self,
        f: Value,
        args: &[Value],
        nres: i32,
    ) -> Result<Vec<Value>, LuaError> {
        let func_idx = self.top;
        self.ensure(func_idx + 1 + args.len() + 1);
        self.stack[func_idx] = f;
        self.stack[func_idx + 1..func_idx + 1 + args.len()].copy_from_slice(args);
        self.top = func_idx + 1 + args.len();
        self.call_at(func_idx, args.len(), nres)?;
        let out = self.stack[func_idx..self.top].to_vec();
        self.top = func_idx;
        Ok(out)
    }

    /// Call the function at `func_idx` with the `nargs` values above it; its results replace
    /// them, `top` just past the last. Runs a Lua function to completion.
    pub fn call_at(&mut self, func_idx: usize, nargs: usize, nres: i32) -> Result<(), LuaError> {
        self.c_depth += 1;
        if self.c_depth >= MAXCCALLS {
            self.c_depth -= 1;
            return Err(self.rt_error("C stack overflow"));
        }
        let r = match self.precall(func_idx, nargs, nres) {
            Ok(Called::Lua) => {
                self.frames.last_mut().unwrap().boundary = true;
                self.execute()
            }
            Ok(Called::Done) => Ok(()),
            Err(e) => Err(e),
        };
        self.c_depth -= 1;
        r
    }

    /// A protected call: on error the stack and frames unwind to here, open upvalues above
    /// close, and the error is the result.
    pub fn pcall_at(
        &mut self,
        func_idx: usize,
        nargs: usize,
        nres: i32,
        handler: Option<Value>,
    ) -> Result<(), LuaError> {
        let frames = self.frames.len();
        let c_depth = self.c_depth;
        self.handlers.push(handler);
        let r = self.call_at(func_idx, nargs, nres);
        self.handlers.pop();
        if let Err(mut e) = r {
            self.close_upvals(func_idx);
            self.frames.truncate(frames);
            self.c_depth = c_depth;
            self.top = func_idx;
            e.handled = false;
            return Err(e);
        }
        Ok(())
    }

    /// 5.0's `luaD_precall`.
    fn precall(&mut self, func_idx: usize, nargs: usize, nres: i32) -> Result<Called, LuaError> {
        let mut nargs = nargs;
        let fref = loop {
            match self.stack[func_idx] {
                Value::Func(f) => break f,
                other => {
                    // `tryfuncTM`: `__call` goes below the arguments.
                    let tm = self.tm(other, self.names.call);
                    if !matches!(tm, Value::Func(_)) {
                        let reg = self
                            .frames
                            .last()
                            .filter(|f| !f.proto.is_null())
                            .map(|f| func_idx - f.base);
                        return Err(self.type_error(other, reg, "call"));
                    }
                    self.ensure(func_idx + nargs + 2);
                    self.stack
                        .copy_within(func_idx..func_idx + nargs + 1, func_idx + 1);
                    self.stack[func_idx] = tm;
                    nargs += 1;
                    self.top = func_idx + nargs + 1;
                }
            }
        };
        if self.frames.len() >= MAXCALLS {
            return Err(self.rt_error("stack overflow"));
        }
        match self.heap.func(fref) {
            FuncObj::Lua { proto, .. } => {
                let pp = Rc::as_ptr(proto);
                // SAFETY: the closure being called holds this code, and the closure sits in the
                // stack slot `func_idx` for the whole call.
                let proto = unsafe { &*pp };
                let base = func_idx + 1;
                let np = proto.nparams as usize;
                let frame_top = base + proto.maxstack as usize;
                self.ensure(frame_top + 1);
                let mut fill_from = base + nargs;
                if proto.vararg {
                    // 5.0's `adjust_varargs`: the extras into `arg`, with `n`.
                    let extra = nargs.saturating_sub(np);
                    let t = self.heap.new_table(extra, 1);
                    for i in 0..extra {
                        let v = self.stack[base + np + i];
                        self.heap.table_mut(t).set_int(i as i32 + 1, v);
                    }
                    let n = Value::Str(self.names.n);
                    self.heap
                        .table_mut(t)
                        .set(n, Value::Num(extra as f64))
                        .unwrap();
                    for i in nargs..np {
                        self.stack[base + i] = Value::Nil;
                    }
                    self.stack[base + np] = Value::Table(t);
                    fill_from = base + np + 1;
                } else if nargs > np {
                    fill_from = base + np;
                }
                // Missing parameters are nil. The registers above are not cleared, as C Lua
                // clears them: the compiler writes each before reading it, and the collector
                // skips a stale handle (`Heap::collect`).
                let params_end = base + np;
                if fill_from < params_end {
                    for v in &mut self.stack[fill_from..params_end] {
                        *v = Value::Nil;
                    }
                }
                self.top = frame_top;
                self.frames.push(Frame {
                    func: fref,
                    func_idx,
                    base,
                    pc: 0,
                    nresults: nres,
                    proto: pp,
                    tailcalls: 0,
                    boundary: false,
                });
                Ok(Called::Lua)
            }
            FuncObj::Native { f, .. } => {
                let f = *f;
                let base = func_idx + 1;
                self.top = base + nargs;
                self.frames.push(Frame {
                    func: fref,
                    func_idx,
                    base,
                    pc: 0,
                    nresults: nres,
                    proto: std::ptr::null(),
                    tailcalls: 0,
                    boundary: false,
                });
                let n = f(self, nargs)?;
                self.frames.pop();
                let first = self.top - n;
                self.poscall(func_idx, first, n, nres);
                Ok(Called::Done)
            }
        }
    }

    /// 5.0's `luaD_poscall`: `n` results from `first` move to `dest`, adjusted to `wanted`.
    fn poscall(&mut self, dest: usize, first: usize, n: usize, wanted: i32) {
        // One result wanted, at least one given: the common call in an expression.
        if wanted == 1 && n >= 1 {
            self.stack[dest] = self.stack[first];
            self.top = dest + 1;
            return;
        }
        if wanted < 0 {
            self.stack.copy_within(first..first + n, dest);
            self.top = dest + n;
        } else {
            let wanted = wanted as usize;
            self.ensure(dest + wanted + 1);
            let k = n.min(wanted);
            self.stack.copy_within(first..first + k, dest);
            for v in &mut self.stack[dest + k..dest + wanted] {
                *v = Value::Nil;
            }
            self.top = dest + wanted;
        }
    }

    // ── natives' view ────────────────────────────────────────────────────────────────────────

    /// The `i`th argument (0-based) of the running native, or nil.
    pub fn arg(&self, nargs: usize, i: usize) -> Value {
        if i < nargs {
            self.stack[self.frames.last().unwrap().base + i]
        } else {
            Value::Nil
        }
    }

    /// The running native's arguments.
    pub fn args(&self, nargs: usize) -> &[Value] {
        let b = self.frames.last().unwrap().base;
        &self.stack[b..b + nargs]
    }

    /// The running function's environment table (`getfenv(0)`'s answer for a native).
    pub fn current_env(&self) -> TableRef {
        let f = self.frames.last().unwrap().func;
        match self.heap.func(f) {
            FuncObj::Lua { env, .. } | FuncObj::Native { env, .. } => *env,
        }
    }

    /// The name a native was called by, as 5.0's `getfuncname` finds it from the calling
    /// instruction: `("global", "strfind")`, `("method", "SetText")`.
    pub fn called_name(&self) -> Option<(&'static str, Vec<u8>)> {
        let n = self.frames.len();
        if n < 2 {
            return None;
        }
        let me = &self.frames[n - 1];
        let caller = &self.frames[n - 2];
        if me.tailcalls > 0 {
            return None;
        }
        // SAFETY: a live frame's code is alive (see `Frame::proto`).
        let p = unsafe { caller.proto.as_ref() }?;
        let pc = caller.pc.checked_sub(1)?;
        match p.code.get(pc)? {
            Op::Call(a, ..) | Op::TailCall(a, ..) => objname_in(p, &self.heap, pc, *a as usize),
            _ => None,
        }
    }

    /// `luaL_argerror`: `bad argument #n to 'name' (msg)`, with a method's `self` not counted.
    pub fn arg_error(&mut self, narg: usize, msg: &str) -> LuaError {
        let named = self.called_name();
        let mut narg = narg;
        let name = match &named {
            Some((kind, n)) => {
                if *kind == "method" {
                    narg -= 1;
                    if narg == 0 {
                        let m = format!(
                            "calling `{}' on bad self ({msg})",
                            String::from_utf8_lossy(n)
                        );
                        return self.error_at(1, &m);
                    }
                }
                String::from_utf8_lossy(n).into_owned()
            }
            None => "?".to_string(),
        };
        self.error_at(1, &format!("bad argument #{narg} to `{name}' ({msg})"))
    }

    /// `luaL_typerror`: `<expected> expected, got <type>`; an absent argument is "no value".
    pub fn type_arg_error(&mut self, nargs: usize, narg: usize, expected: &str) -> LuaError {
        let got = if narg > nargs {
            "no value"
        } else {
            self.arg(nargs, narg - 1).type_name()
        };
        self.arg_error(narg, &format!("{expected} expected, got {got}"))
    }

    // ── collection ───────────────────────────────────────────────────────────────────────────

    /// 5.0's `luaC_checkGC`, at a safe point: every live value is on the stack or reachable.
    #[inline]
    pub fn check_gc(&mut self) {
        if self.heap.bytes >= self.heap.threshold || self.gc_stress {
            self.full_gc();
        }
    }

    /// A full collection; 5.0 then sets the threshold to twice what lives.
    pub fn full_gc(&mut self) {
        let mut roots: Vec<Value> = self.stack[..self.top].to_vec();
        roots.push(Value::Table(self.globals));
        roots.push(Value::Table(self.registry));
        for f in &self.frames {
            roots.push(Value::Func(f.func));
        }
        roots.extend(self.handlers.iter().flatten().copied());
        let open: Vec<u32> = self.open.iter().map(|&(_, u)| u).collect();
        // Names live forever.
        let n = &self.names;
        for s in [
            n.index,
            n.newindex,
            n.call,
            n.add,
            n.sub,
            n.mul,
            n.div,
            n.pow,
            n.unm,
            n.concat,
            n.eq,
            n.lt,
            n.le,
            n.mode,
            n.metatable,
            n.tostring,
            n.n,
            n.next,
            n.gpow,
        ] {
            roots.push(Value::Str(s));
        }
        let live = self.heap.collect(&roots, &open, self.names.mode);
        self.heap.threshold = 2 * live;
    }

    // ── the interpreter ──────────────────────────────────────────────────────────────────────

    /// Run the top Lua frame until the frame that entered with `boundary` returns.
    ///
    /// The register file is addressed through `regs`, a raw pointer to `stack[base]`, as C Lua
    /// keeps `base` in a register. Anything that can grow the stack (a call, a metamethod, an
    /// upvalue that touches the stack) is followed by `refresh!`, which recomputes it.
    fn execute(&mut self) -> Result<(), LuaError> {
        'frame: loop {
            let fi = self.frames.len() - 1;
            // SAFETY: the top frame is a Lua frame, and its code outlives it (`Frame::proto`).
            let proto: &Proto = unsafe { &*self.frames[fi].proto };
            let base = self.frames[fi].base;
            let mut pc = self.frames[fi].pc;
            let fref = self.frames[fi].func;
            // The closure's upvalue list and environment, read once per entry. The list lives in
            // its own allocation, which a hot reload retires rather than frees.
            let (upvals, mut env): (*const [u32], TableRef) = match self.heap.func(fref) {
                FuncObj::Lua { upvals, env, .. } => (&**upvals, *env),
                _ => unreachable!("a Lua frame"),
            };
            // SAFETY: as above.
            let upvals: &[u32] = unsafe { &*upvals };
            let mut env_gen = self.fenv_gen;
            let code = &proto.code[..];
            let k = &proto.consts[..];
            let maxstack = proto.maxstack as usize;
            // SAFETY: `precall` grew the stack to cover `base + maxstack`, and the stack never
            // shrinks.
            let mut regs: *mut Value = unsafe { self.stack.as_mut_ptr().add(base) };
            macro_rules! refresh {
                () => {
                    // SAFETY: as above.
                    regs = unsafe { self.stack.as_mut_ptr().add(base) }
                };
            }
            // SAFETY for `r!`: `regs` is current (see above), and the compiler addresses no
            // register at or past `maxstack` (`Fc::reserve`, `Fc::need`).
            macro_rules! r {
                ($x:expr) => {
                    *{
                        let i = $x as usize;
                        debug_assert!(i < maxstack && base + i < self.stack.len());
                        unsafe { &mut *regs.add(i) }
                    }
                };
            }
            macro_rules! rk {
                ($x:expr) => {{
                    let x: Rk = $x;
                    if x >= RK_CONST {
                        // SAFETY: the compiler only encodes constants this function has.
                        let i = (x - RK_CONST) as usize;
                        debug_assert!(i < k.len());
                        unsafe { *k.get_unchecked(i) }
                    } else {
                        r!(x)
                    }
                }};
            }
            // `rk!` by reference: a fast path reads the tag and payload in place instead of
            // copying the value out.
            macro_rules! rkref {
                ($x:expr) => {{
                    let x: Rk = $x;
                    if x >= RK_CONST {
                        let i = (x - RK_CONST) as usize;
                        debug_assert!(i < k.len());
                        // SAFETY: as `rk!`.
                        unsafe { k.get_unchecked(i) }
                    } else {
                        let i = x as usize;
                        debug_assert!(i < maxstack && base + i < self.stack.len());
                        // SAFETY: as `r!`.
                        unsafe { &*regs.add(i) }
                    }
                }};
            }
            macro_rules! rkreg {
                ($x:expr) => {{
                    let x: Rk = $x;
                    if x >= RK_CONST {
                        None
                    } else {
                        Some(x as usize)
                    }
                }};
            }
            macro_rules! env {
                () => {{
                    if env_gen != self.fenv_gen {
                        env_gen = self.fenv_gen;
                        env = match self.heap.func(fref) {
                            FuncObj::Lua { env, .. } => *env,
                            _ => unreachable!(),
                        };
                    }
                    env
                }};
            }
            macro_rules! save {
                () => {
                    self.frames[fi].pc = pc
                };
            }
            /// A call that may run Lua: the pc saved first, the registers refreshed after.
            macro_rules! slow {
                ($e:expr) => {{
                    save!();
                    let v = $e;
                    refresh!();
                    v
                }};
            }
            macro_rules! arith {
                ($a:expr, $b:expr, $c:expr, $op:tt, $kind:expr) => {{
                    if let (&Value::Num(x), &Value::Num(y)) = (rkref!($b), rkref!($c)) {
                        r!($a) = Value::Num(x $op y);
                    } else {
                        let (x, y) = (rk!($b), rk!($c));
                        let v = slow!(self.arith($kind, x, y, (rkreg!($b), rkreg!($c))))?;
                        r!($a) = v;
                    }
                }};
            }
            macro_rules! jump {
                ($off:expr) => {
                    pc = (pc as isize + $off as isize) as usize
                };
            }
            loop {
                debug_assert!(pc < code.len());
                // SAFETY: every function ends in `Return`, and every jump lands inside it.
                let op = unsafe { *code.get_unchecked(pc) };
                pc += 1;
                match op {
                    Op::Move(a, b) => r!(a) = r!(b),
                    Op::LoadK(a, kx) => r!(a) = k[kx as usize],
                    Op::LoadNil(a, b) => {
                        for i in a..=b {
                            r!(i) = Value::Nil;
                        }
                    }
                    Op::LoadBool(a, b) => r!(a) = Value::Bool(b),
                    Op::GetUpval(a, u) => {
                        let v = self.upval_get(upvals[u as usize]);
                        r!(a) = v;
                    }
                    Op::SetUpval(a, u) => {
                        let v = r!(a);
                        self.upval_set(upvals[u as usize], v);
                        refresh!();
                    }
                    Op::GetGlobal(a, kx) => {
                        let env = env!();
                        let key = k[kx as usize];
                        let tab = self.heap.table(env);
                        let v = tab.get(key);
                        if !v.is_nil() || tab.meta.is_none() {
                            r!(a) = v;
                        } else {
                            let v = slow!(self.index(Value::Table(env), key, None))?;
                            r!(a) = v;
                        }
                    }
                    Op::SetGlobal(a, kx) => {
                        let env = env!();
                        let v = r!(a);
                        let key = k[kx as usize];
                        let tab = self.heap.table_mut(env);
                        if tab.meta.is_none() && tab.set(key, v).is_ok() {
                            continue;
                        }
                        slow!(self.setindex(Value::Table(env), key, v, None))?;
                    }
                    Op::GetTable(a, b, c) => {
                        if let &Value::Table(tr) = &r!(b) {
                            let tab = self.heap.table(tr);
                            let v = tab.get_ref(rkref!(c));
                            if !v.is_nil() || tab.meta.is_none() {
                                r!(a) = v;
                                continue;
                            }
                        }
                        let (t, key) = (r!(b), rk!(c));
                        let v = slow!(self.index(t, key, Some(b as usize)))?;
                        r!(a) = v;
                    }
                    Op::SetTable(a, b, c) => {
                        let key = rk!(b);
                        let v = rk!(c);
                        if let &Value::Table(tr) = &r!(a) {
                            let tab = self.heap.table_mut(tr);
                            if tab.meta.is_none() {
                                if tab.set(key, v).is_ok() {
                                    continue;
                                }
                            } else if let Some(s) = tab.find(key) {
                                // An existing value is overwritten raw: `__newindex` is only
                                // consulted for a nil one.
                                if !tab.slot(s).is_nil() {
                                    *tab.slot_mut(s) = v;
                                    continue;
                                }
                            }
                        }
                        let t = r!(a);
                        slow!(self.setindex(t, key, v, Some(a as usize)))?;
                    }
                    Op::NewTable(a, b, c) => {
                        let t = self.heap.new_table(b as usize, c);
                        r!(a) = Value::Table(t);
                        save!();
                        self.top = base + maxstack;
                        self.check_gc();
                    }
                    Op::SelfOp(a, b, c) => {
                        let obj = r!(b);
                        r!(a + 1) = obj;
                        if let &Value::Table(tr) = &r!(b) {
                            let tab = self.heap.table(tr);
                            let v = tab.get_ref(rkref!(c));
                            if !v.is_nil() || tab.meta.is_none() {
                                r!(a) = v;
                                continue;
                            }
                        }
                        let key = rk!(c);
                        let v = slow!(self.index(obj, key, Some(b as usize)))?;
                        r!(a) = v;
                    }
                    Op::Add(a, b, c) => arith!(a, b, c, +, Arith::Add),
                    Op::Sub(a, b, c) => arith!(a, b, c, -, Arith::Sub),
                    Op::Mul(a, b, c) => arith!(a, b, c, *, Arith::Mul),
                    Op::Div(a, b, c) => arith!(a, b, c, /, Arith::Div),
                    Op::Pow(a, b, c) => {
                        // 5.0's `^` always goes through the global `__pow`.
                        let (x, y) = (rk!(b), rk!(c));
                        let v = slow!(self.arith(Arith::Pow, x, y, (rkreg!(b), rkreg!(c))))?;
                        r!(a) = v;
                    }
                    Op::Unm(a, b) => {
                        let x = r!(b);
                        match self.tonumber(x) {
                            Some(n) => r!(a) = Value::Num(-n),
                            None => {
                                let unm = self.names.unm;
                                match slow!(self.bin_tm(x, Value::Nil, unm))? {
                                    Some(v) => r!(a) = v,
                                    None => {
                                        return Err(self.type_error(
                                            x,
                                            Some(b as usize),
                                            "perform arithmetic on",
                                        ))
                                    }
                                }
                            }
                        }
                    }
                    Op::Not(a, b) => r!(a) = Value::Bool(!r!(b).truthy_ref()),
                    Op::Concat(a, b, c) => {
                        self.top = base + maxstack;
                        slow!(self.concat(base + b as usize, base + c as usize))?;
                        r!(a) = r!(b);
                        self.check_gc();
                    }
                    Op::Jmp(off) => jump!(off),
                    Op::JmpCmp(cmp, cond, b, c, off) => {
                        let res = match (cmp, rkref!(b), rkref!(c)) {
                            (Cmp::Lt, &Value::Num(p), &Value::Num(q)) => p < q,
                            (Cmp::Le, &Value::Num(p), &Value::Num(q)) => p <= q,
                            (Cmp::Eq, &Value::Num(p), &Value::Num(q)) => p == q,
                            _ => {
                                let (x, y) = (rk!(b), rk!(c));
                                match cmp {
                                    Cmp::Eq => x.raw_eq(y) || slow!(self.equals(x, y))?,
                                    Cmp::Lt => slow!(self.less_than(x, y))?,
                                    Cmp::Le => slow!(self.less_equal(x, y))?,
                                }
                            }
                        };
                        if res == cond {
                            jump!(off);
                        }
                    }
                    Op::JmpIf(a, cond, off) => {
                        if r!(a).truthy_ref() == cond {
                            jump!(off);
                        }
                    }
                    Op::Call(a, b, c) => {
                        let func_idx = base + a as usize;
                        let nargs = if b == 0 {
                            self.top - func_idx - 1
                        } else {
                            b as usize - 1
                        };
                        save!();
                        match self.precall(func_idx, nargs, c as i32 - 1)? {
                            Called::Lua => continue 'frame,
                            Called::Done => {
                                refresh!();
                                if c != 0 {
                                    self.top = base + maxstack;
                                }
                                self.check_gc();
                            }
                        }
                    }
                    Op::TailCall(a, b) => {
                        let func_idx = base + a as usize;
                        let nargs = if b == 0 {
                            self.top - func_idx - 1
                        } else {
                            b as usize - 1
                        };
                        save!();
                        // A Lua callee replaces this frame; a native runs and its results
                        // return through the `Return` that follows.
                        let is_lua = matches!(
                            r!(a),
                            Value::Func(f) if matches!(self.heap.func(f), FuncObj::Lua { .. })
                        );
                        if !is_lua {
                            match self.precall(func_idx, nargs, -1)? {
                                Called::Lua => continue 'frame,
                                Called::Done => {
                                    refresh!();
                                    continue;
                                }
                            }
                        }
                        self.close_upvals(base);
                        let me = self.frames.pop().unwrap();
                        let dest = me.func_idx;
                        self.stack.copy_within(func_idx..func_idx + nargs + 1, dest);
                        self.top = dest + nargs + 1;
                        match self.precall(dest, nargs, me.nresults) {
                            Ok(Called::Lua) => {
                                let f = self.frames.last_mut().unwrap();
                                f.tailcalls = me.tailcalls + 1;
                                f.boundary = me.boundary;
                                continue 'frame;
                            }
                            Ok(Called::Done) => unreachable!("a Lua callee"),
                            Err(e) => {
                                self.frames.push(me);
                                return Err(e);
                            }
                        }
                    }
                    Op::Return(a, b) => {
                        let first = base + a as usize;
                        let n = if b == 0 {
                            self.top - first
                        } else {
                            b as usize - 1
                        };
                        if !self.open.is_empty() {
                            self.close_upvals(base);
                        }
                        let me = self.frames.pop().unwrap();
                        self.poscall(me.func_idx, first, n, me.nresults);
                        if me.boundary {
                            return Ok(());
                        }
                        // Back in the calling Lua frame: a fixed result count restores its top.
                        let caller = self.frames.last().unwrap();
                        if me.nresults >= 0 {
                            // SAFETY: the caller is a live Lua frame (`Frame::proto`).
                            let cp = unsafe { &*caller.proto };
                            self.top = caller.base + cp.maxstack as usize;
                        }
                        continue 'frame;
                    }
                    Op::ForLoop(a, off) => {
                        // Three numbers already: the step alone, nothing to convert back.
                        if let (&Value::Num(idx), &Value::Num(limit), &Value::Num(step)) =
                            (&r!(a), &r!(a + 1), &r!(a + 2))
                        {
                            let idx = idx + step;
                            if if step > 0.0 {
                                idx <= limit
                            } else {
                                idx >= limit
                            } {
                                jump!(off);
                                r!(a) = Value::Num(idx);
                            }
                            continue;
                        }
                        save!();
                        if !matches!(r!(a), Value::Num(_)) {
                            return Err(self.rt_error("`for' initial value must be a number"));
                        }
                        let Some(limit) = self.tonumber(r!(a + 1)) else {
                            return Err(self.rt_error("`for' limit must be a number"));
                        };
                        let Some(step) = self.tonumber(r!(a + 2)) else {
                            return Err(self.rt_error("`for' step must be a number"));
                        };
                        r!(a + 1) = Value::Num(limit);
                        r!(a + 2) = Value::Num(step);
                        let Value::Num(idx) = r!(a) else {
                            unreachable!()
                        };
                        let idx = idx + step;
                        if if step > 0.0 {
                            idx <= limit
                        } else {
                            idx >= limit
                        } {
                            jump!(off);
                            r!(a) = Value::Num(idx);
                        }
                    }
                    Op::TForPrep(a, off) => {
                        if let Value::Table(_) = r!(a) {
                            r!(a + 1) = r!(a);
                            let g = self.globals;
                            r!(a) = self.heap.table(g).get(Value::Str(self.names.next));
                        }
                        jump!(off);
                    }
                    Op::TForLoop(a, nvar) => {
                        let nv = nvar as usize;
                        let cb = a as usize + nv + 2;
                        r!(cb) = r!(a);
                        r!(cb + 1) = r!(a + 1);
                        r!(cb + 2) = r!(a + 2);
                        self.top = base + cb + 3;
                        slow!(self.call_at(base + cb, 2, nv as i32))?;
                        self.top = base + maxstack;
                        for i in 0..nv {
                            r!(a as usize + 2 + i) = r!(cb + i);
                        }
                        if r!(a + 2).is_nil() {
                            pc += 1;
                        } else {
                            let Op::Jmp(back) = code[pc] else {
                                unreachable!()
                            };
                            jump!(1 + back);
                        }
                    }
                    Op::SetList(a, first, n) => {
                        let Value::Table(t) = r!(a) else {
                            unreachable!()
                        };
                        // 5.0 stores the batch from its last item down.
                        for i in (1..=n as usize).rev() {
                            let v = r!(a as usize + i);
                            self.heap
                                .table_mut(t)
                                .set_int((first as usize + i) as i32, v);
                        }
                    }
                    Op::SetListTop(a, first) => {
                        let Value::Table(t) = r!(a) else {
                            unreachable!()
                        };
                        let n = self.top - (base + a as usize) - 1;
                        for i in (1..=n).rev() {
                            let v = self.stack[base + a as usize + i];
                            self.heap
                                .table_mut(t)
                                .set_int((first as usize + i) as i32, v);
                        }
                        self.top = base + maxstack;
                    }
                    Op::Close(a) => {
                        self.close_upvals(base + a as usize);
                        refresh!();
                    }
                    Op::Closure(a, px) => {
                        let p = proto.protos[px as usize].clone();
                        let env = env!();
                        let mut ups = Vec::with_capacity(p.upvals.len());
                        for &(in_stack, idx) in &p.upvals {
                            ups.push(if in_stack {
                                self.find_upval(base + idx as usize)
                            } else {
                                upvals[idx as usize]
                            });
                        }
                        let f = self.heap.new_func(FuncObj::Lua {
                            proto: p,
                            upvals: ups.into_boxed_slice(),
                            env,
                        });
                        r!(a) = Value::Func(f);
                        save!();
                        self.top = base + maxstack;
                        self.check_gc();
                    }
                }
            }
        }
    }
}

/// 5.0's `getobjname` over a prototype: a local's name, else what a symbolic run from the start
/// says last wrote `reg` before `pc`.
fn objname_in(p: &Proto, heap: &Heap, pc: usize, reg: usize) -> Option<(&'static str, Vec<u8>)> {
    if let Some(n) = p.local_name(reg as Reg, pc) {
        return Some(("local", n.to_vec()));
    }
    let last = symbexec(p, pc, reg)?;
    let kname = |c: Rk| -> Vec<u8> {
        if c >= RK_CONST {
            if let Value::Str(s) = p.consts[(c - RK_CONST) as usize] {
                return heap.str(s).to_vec();
            }
        }
        b"?".to_vec()
    };
    match p.code[last] {
        Op::GetGlobal(_, kx) => match p.consts[kx as usize] {
            Value::Str(s) => Some(("global", heap.str(s).to_vec())),
            _ => None,
        },
        Op::Move(a, b) if (b as usize) < (a as usize) => objname_in(p, heap, last, b as usize),
        Op::GetTable(_, _, c) => Some(("field", kname(c))),
        Op::SelfOp(_, _, c) => Some(("method", kname(c))),
        _ => None,
    }
}

/// The instruction that last wrote `reg` before `lastpc`, running the code from the start and
/// following forward jumps that land at or before `lastpc`, as 5.0's `luaG_symbexec` does.
fn symbexec(p: &Proto, lastpc: usize, reg: usize) -> Option<usize> {
    let mut last = None;
    let mut pc = 0;
    while pc < lastpc {
        let op = p.code[pc];
        let writes = |a: Reg, n: usize| (a as usize..a as usize + n).contains(&reg);
        let hit = match op {
            Op::Move(a, _)
            | Op::LoadK(a, _)
            | Op::LoadBool(a, _)
            | Op::GetUpval(a, _)
            | Op::GetGlobal(a, _)
            | Op::GetTable(a, ..)
            | Op::NewTable(a, ..)
            | Op::Add(a, ..)
            | Op::Sub(a, ..)
            | Op::Mul(a, ..)
            | Op::Div(a, ..)
            | Op::Pow(a, ..)
            | Op::Unm(a, _)
            | Op::Not(a, _)
            | Op::Concat(a, ..)
            | Op::Closure(a, _) => writes(a, 1),
            Op::LoadNil(a, b) => (a as usize..=b as usize).contains(&reg),
            Op::SelfOp(a, ..) => writes(a, 2),
            Op::Call(a, ..) | Op::TailCall(a, ..) => reg >= a as usize,
            Op::ForLoop(a, _) => writes(a, 1),
            Op::TForLoop(a, n) => reg >= a as usize + 2 && reg < a as usize + 2 + n as usize,
            _ => false,
        };
        if hit {
            last = Some(pc);
        }
        let jump = match op {
            Op::Jmp(off) | Op::TForPrep(_, off) => Some(off),
            _ => None,
        };
        if let Some(off) = jump {
            let dest = (pc as i32 + 1 + off) as usize;
            if pc < dest && dest <= lastpc {
                pc = dest;
                continue;
            }
        }
        pc += 1;
    }
    last
}
