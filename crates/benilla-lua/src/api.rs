//! What a native function sees: 5.0's `lauxlib` checks and results, over [`Vm`].

use crate::heap::{FuncObj, NativeFn};
use crate::value::{StrRef, TableRef, Value, V};
use crate::vm::{LuaError, Vm};

pub type R<T> = Result<T, LuaError>;

/// C's `(int)` cast of a double on the client: x87 truncation through a 64-bit integer, low 32
/// bits kept.
pub fn c_int(n: f64) -> i32 {
    n as i64 as i32
}

impl Vm {
    /// `luaL_checkany`.
    pub fn check_any(&mut self, nargs: usize, n: usize) -> R<Value> {
        if n > nargs {
            return Err(self.arg_error(n, "value expected"));
        }
        Ok(self.arg(nargs, n - 1))
    }

    /// `luaL_checktype(L, n, LUA_TTABLE)`.
    pub fn check_table(&mut self, nargs: usize, n: usize) -> R<TableRef> {
        match self.arg(nargs, n - 1).as_table() {
            Some(t) if n <= nargs => Ok(t),
            _ => Err(self.type_arg_error(nargs, n, "table")),
        }
    }

    /// `luaL_checktype(L, n, LUA_TFUNCTION)`.
    pub fn check_function(&mut self, nargs: usize, n: usize) -> R<Value> {
        let v = self.arg(nargs, n - 1);
        match v.as_func() {
            Some(_) if n <= nargs => Ok(v),
            _ => Err(self.type_arg_error(nargs, n, "function")),
        }
    }

    /// `luaL_checknumber`: a number or a numeric string.
    pub fn check_number(&mut self, nargs: usize, n: usize) -> R<f64> {
        let v = self.arg(nargs, n - 1);
        match self.tonumber(v) {
            Some(x) if n <= nargs => Ok(x),
            _ => Err(self.type_arg_error(nargs, n, "number")),
        }
    }

    /// `luaL_optnumber`: absent or nil is the default.
    pub fn opt_number(&mut self, nargs: usize, n: usize, def: f64) -> R<f64> {
        if self.arg(nargs, n - 1).is_nil() {
            Ok(def)
        } else {
            self.check_number(nargs, n)
        }
    }

    pub fn check_int(&mut self, nargs: usize, n: usize) -> R<i32> {
        Ok(c_int(self.check_number(nargs, n)?))
    }

    pub fn opt_int(&mut self, nargs: usize, n: usize, def: i32) -> R<i32> {
        if self.arg(nargs, n - 1).is_nil() {
            Ok(def)
        } else {
            self.check_int(nargs, n)
        }
    }

    /// `luaL_checklstring`: a string, or a number converted in its stack slot as
    /// `lua_tolstring` converts it.
    pub fn check_str(&mut self, nargs: usize, n: usize) -> R<StrRef> {
        let v = self.arg(nargs, n - 1);
        if n <= nargs {
            if let Some(s) = self.tostr(v) {
                self.set_arg(n - 1, Value::Str(s));
                return Ok(s);
            }
        }
        Err(self.type_arg_error(nargs, n, "string"))
    }

    /// `luaL_optlstring`.
    pub fn opt_str(&mut self, nargs: usize, n: usize, def: &[u8]) -> R<StrRef> {
        if self.arg(nargs, n - 1).is_nil() {
            Ok(self.heap.intern(def))
        } else {
            self.check_str(nargs, n)
        }
    }

    /// Replace argument `i` (0-based) of the running native.
    pub fn set_arg(&mut self, i: usize, v: Value) {
        let b = self.native_base();
        self.stack[b + i] = v;
    }

    /// Push one result and answer the count, for `return Ok(vm.ret1(v))`.
    pub fn ret1(&mut self, v: Value) -> usize {
        self.push(v);
        1
    }

    pub fn ret_str(&mut self, s: &[u8]) -> usize {
        let v = self.str(s);
        self.ret1(v)
    }

    /// A native closure with upvalues, which [`Vm::upvalue`] reads.
    pub fn new_closure(&mut self, name: &'static str, f: NativeFn, upvals: Vec<Value>) -> Value {
        let env = self.globals;
        Value::Func(self.heap.new_func(FuncObj::Native {
            f,
            name,
            upvals: upvals.into_boxed_slice(),
            env,
        }))
    }

    /// The running native closure's upvalue `i`.
    pub fn upvalue(&self, i: usize) -> Value {
        match self.heap.func(self.running()) {
            FuncObj::Native { upvals, .. } => upvals[i],
            _ => Value::Nil,
        }
    }

    pub fn set_upvalue(&mut self, i: usize, v: Value) {
        let f = self.running();
        if let FuncObj::Native { upvals, .. } = self.heap.func_mut(f) {
            upvals[i] = v;
        }
    }

    /// `luaL_getmetafield`: a field of `v`'s metatable, raw.
    pub fn metafield(&mut self, v: Value, name: &[u8]) -> Value {
        match self.heap.metatable(v) {
            Some(m) => {
                let k = self.str(name);
                self.heap.table(m).get(k)
            }
            None => Value::Nil,
        }
    }

    /// `lua_isstring`: a string or a number.
    pub fn is_string(&self, v: Value) -> bool {
        matches!(v.unpack(), V::Str(_) | V::Num(_))
    }
}
