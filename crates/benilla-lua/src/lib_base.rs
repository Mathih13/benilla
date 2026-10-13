//! The base library as 1.12 opens it: 5.0's `lbaselib.c` without `print`, `_VERSION`,
//! `loadfile`, `dofile`, `require` or coroutines, with 5.0's `assert`, `gcinfo` and
//! `collectgarbage` shapes.

use crate::api::{c_int, R};
use crate::heap::FuncObj;
use crate::number::fmt_g14;
use crate::value::Value;
use crate::vm::Vm;

pub fn open(vm: &mut Vm) {
    vm.register("error", error);
    vm.register("getmetatable", getmetatable);
    vm.register("setmetatable", setmetatable);
    vm.register("getfenv", getfenv);
    vm.register("setfenv", setfenv);
    vm.register("next", next);
    vm.register("ipairs", ipairs);
    vm.register("pairs", pairs);
    vm.register("tonumber", tonumber);
    vm.register("tostring", tostring);
    vm.register("type", type_);
    vm.register("assert", assert);
    vm.register("unpack", unpack);
    vm.register("rawequal", rawequal);
    vm.register("rawget", rawget);
    vm.register("rawset", rawset);
    vm.register("pcall", pcall);
    vm.register("xpcall", xpcall);
    vm.register("collectgarbage", collectgarbage);
    vm.register("gcinfo", gcinfo);
    vm.register("loadstring", loadstring);
    let g = Value::Table(vm.globals);
    vm.set_global("_G", g);
}

/// `luaB_error`: a string message gets the position `level` up (default 1).
fn error(vm: &mut Vm, nargs: usize) -> R<usize> {
    let level = vm.opt_int(nargs, 2, 1)?;
    let msg = vm.check_any(nargs, 1)?;
    if vm.is_string(msg) && level > 0 {
        let mut out = vm.where_(level as usize);
        let s = vm.tostr(msg).unwrap();
        out.extend_from_slice(vm.heap.str(s));
        let v = vm.str(&out);
        return Err(vm.throw(v));
    }
    Err(vm.throw(msg))
}

fn getmetatable(vm: &mut Vm, nargs: usize) -> R<usize> {
    let v = vm.check_any(nargs, 1)?;
    let Some(m) = vm.heap.metatable(v) else {
        return Ok(vm.ret1(Value::Nil));
    };
    let protected = vm.metafield(v, b"__metatable");
    Ok(vm.ret1(if protected.is_nil() {
        Value::Table(m)
    } else {
        protected
    }))
}

fn setmetatable(vm: &mut Vm, nargs: usize) -> R<usize> {
    let mt = vm.arg(nargs, 1);
    let t = vm.check_table(nargs, 1)?;
    let mt = match mt {
        Value::Nil => None,
        Value::Table(m) if nargs >= 2 => Some(m),
        _ => return Err(vm.arg_error(2, "nil or table expected")),
    };
    if !vm.metafield(Value::Table(t), b"__metatable").is_nil() {
        return Err(vm.error_at(1, "cannot change a protected metatable"));
    }
    vm.heap.table_mut(t).meta = mt;
    Ok(vm.ret1(Value::Table(t)))
}

/// `getfunc`: argument 1 as a function, or the function at that stack level.
fn getfunc(vm: &mut Vm, nargs: usize) -> R<Value> {
    let a = vm.arg(nargs, 0);
    if let Value::Func(_) = a {
        return Ok(a);
    }
    let level = vm.opt_int(nargs, 1, 1)?;
    if level < 0 {
        return Err(vm.arg_error(1, "level must be non-negative"));
    }
    match vm.function_at_level(level as usize) {
        Some(Some(f)) => Ok(Value::Func(f)),
        Some(None) => Err(vm.error_at(
            1,
            &format!("no function environment for tail call at level {level}"),
        )),
        None => Err(vm.arg_error(1, "invalid level")),
    }
}

fn fenv(vm: &Vm, f: Value) -> Value {
    match f {
        Value::Func(f) => match vm.heap.func(f) {
            FuncObj::Lua { env, .. } | FuncObj::Native { env, .. } => Value::Table(*env),
        },
        _ => Value::Table(vm.globals),
    }
}

fn getfenv(vm: &mut Vm, nargs: usize) -> R<usize> {
    let f = getfunc(vm, nargs)?;
    let env = fenv(vm, f);
    // `aux_getfenv`: a `__fenv` field answers in the environment's place.
    let k = vm.str(b"__fenv");
    let fe = match env {
        Value::Table(t) => vm.heap.table(t).get(k),
        _ => Value::Nil,
    };
    Ok(vm.ret1(if fe.is_nil() { env } else { fe }))
}

fn setfenv(vm: &mut Vm, nargs: usize) -> R<usize> {
    let t = vm.check_table(nargs, 2)?;
    let first = vm.arg(nargs, 0);
    if vm.tonumber(first) == Some(0.0) {
        // The thread's own globals.
        vm.globals = t;
        return Ok(0);
    }
    let f = getfunc(vm, nargs)?;
    let env = fenv(vm, f);
    let k = vm.str(b"__fenv");
    if let Value::Table(e) = env {
        if !vm.heap.table(e).get(k).is_nil() {
            return Err(vm.error_at(1, "`setfenv' cannot change a protected environment"));
        }
    }
    match f {
        Value::Func(fr) => match vm.heap.func_mut(fr) {
            FuncObj::Lua { env, .. } => *env = t,
            FuncObj::Native { .. } => {
                return Err(vm.error_at(1, "`setfenv' cannot change environment of given function"))
            }
        },
        _ => unreachable!(),
    }
    Ok(0)
}

fn next(vm: &mut Vm, nargs: usize) -> R<usize> {
    let t = vm.check_table(nargs, 1)?;
    let k = vm.arg(nargs, 1);
    match vm.heap.table(t).next(k) {
        Ok(Some((k, v))) => {
            vm.push(k);
            vm.push(v);
            Ok(2)
        }
        Ok(None) => Ok(vm.ret1(Value::Nil)),
        Err(crate::table::InvalidKey) => Err(vm.rt_error("invalid key for `next'")),
    }
}

/// 5.0's `pairs`: the global `next`, read raw at the call.
fn pairs(vm: &mut Vm, nargs: usize) -> R<usize> {
    let t = vm.check_table(nargs, 1)?;
    let next = vm.heap.table(vm.globals).get(Value::Str(vm.names.next));
    vm.push(next);
    vm.push(Value::Table(t));
    vm.push(Value::Nil);
    Ok(3)
}

/// 5.0's `ipairs` is its own iterator: called with one argument it starts, with two it steps.
fn ipairs(vm: &mut Vm, nargs: usize) -> R<usize> {
    let i = vm.tonumber(vm.arg(nargs, 1)).unwrap_or(0.0);
    let t = vm.check_table(nargs, 1)?;
    if i == 0.0 && nargs < 2 {
        let k = vm.str(b"ipairs");
        let g = vm.heap.table(vm.globals).get(k);
        vm.push(g);
        vm.push(Value::Table(t));
        vm.push(Value::Num(0.0));
        return Ok(3);
    }
    let i = i + 1.0;
    let v = vm.heap.table(t).get_int(i as i32);
    if v.is_nil() {
        return Ok(0);
    }
    vm.push(Value::Num(i));
    vm.push(v);
    Ok(2)
}

fn tonumber(vm: &mut Vm, nargs: usize) -> R<usize> {
    let base = vm.opt_int(nargs, 2, 10)?;
    if base == 10 {
        let v = vm.check_any(nargs, 1)?;
        if let Some(n) = vm.tonumber(v) {
            return Ok(vm.ret1(Value::Num(n)));
        }
    } else {
        let s = vm.check_str(nargs, 1)?;
        if !(2..=36).contains(&base) {
            return Err(vm.arg_error(2, "base out of range"));
        }
        if let Some(n) = strtoul(vm.heap.str(s), base as u32) {
            return Ok(vm.ret1(Value::Num(n as f64)));
        }
    }
    Ok(vm.ret1(Value::Nil))
}

/// C's `strtoul` then 5.0's trailing-space check: `None` unless the whole string is read.
fn strtoul(s: &[u8], base: u32) -> Option<u32> {
    let s = &s[..s.iter().position(|&b| b == 0).unwrap_or(s.len())];
    let mut i = 0;
    while i < s.len() && matches!(s[i], 0x20 | 0x09..=0x0D) {
        i += 1;
    }
    let mut neg = false;
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        neg = s[i] == b'-';
        i += 1;
    }
    if base == 16 && s[i..].len() >= 2 && s[i] == b'0' && (s[i + 1] | 0x20) == b'x' {
        i += 2;
    }
    let start = i;
    let mut n: u64 = 0;
    let mut overflow = false;
    while i < s.len() {
        let d = match s[i] {
            c @ b'0'..=b'9' => (c - b'0') as u32,
            c @ b'a'..=b'z' => (c - b'a') as u32 + 10,
            c @ b'A'..=b'Z' => (c - b'A') as u32 + 10,
            _ => break,
        };
        if d >= base {
            break;
        }
        n = n * base as u64 + d as u64;
        if n > u32::MAX as u64 {
            overflow = true;
        }
        i += 1;
    }
    if i == start {
        return None;
    }
    while i < s.len() && matches!(s[i], 0x20 | 0x09..=0x0D) {
        i += 1;
    }
    if i != s.len() {
        return None;
    }
    let v = if overflow { u32::MAX } else { n as u32 };
    Some(if neg { v.wrapping_neg() } else { v })
}

/// A synthetic address for `tostring`: stable per object, in MSVC's `%p` shape.
fn address(v: Value) -> u32 {
    let (kind, id) = match v {
        Value::Table(r) => (1u32, r.0),
        Value::Func(r) => (2, r.0),
        Value::User(r) => (3, r.0),
        Value::Light(p) => return p as u32,
        _ => (0, 0),
    };
    0x1000_0000u32
        .wrapping_add(kind << 24)
        .wrapping_add(id.wrapping_mul(0x20))
}

pub fn tostring_value(vm: &mut Vm, v: Value) -> R<Value> {
    let tm = vm.metafield(v, b"__tostring");
    if !tm.is_nil() {
        let r = vm.call_value(tm, &[v], 1)?;
        return Ok(r[0]);
    }
    Ok(match v {
        Value::Num(n) => vm.str(fmt_g14(n).as_bytes()),
        Value::Str(_) => v,
        Value::Bool(b) => vm.str(if b { b"true" } else { b"false" }),
        Value::Nil => vm.str(b"nil"),
        _ => {
            let s = format!("{}: {:08X}", v.type_name(), address(v));
            vm.str(s.as_bytes())
        }
    })
}

fn tostring(vm: &mut Vm, nargs: usize) -> R<usize> {
    let v = vm.check_any(nargs, 1)?;
    let s = tostring_value(vm, v)?;
    Ok(vm.ret1(s))
}

fn type_(vm: &mut Vm, nargs: usize) -> R<usize> {
    let v = vm.check_any(nargs, 1)?;
    Ok(vm.ret_str(v.type_name().as_bytes()))
}

/// 1.12's `assert` (`0x7031a0`): one result, the first argument.
fn assert(vm: &mut Vm, nargs: usize) -> R<usize> {
    let v = vm.check_any(nargs, 1)?;
    if !v.truthy() {
        let msg = vm.opt_str(nargs, 2, b"assertion failed!")?;
        let m = String::from_utf8_lossy(vm.heap.str(msg)).into_owned();
        return Err(vm.error_at(1, &m));
    }
    Ok(vm.ret1(v))
}

/// `luaL_getn`: `t.n` if numeric, else the size `setn` remembered, else a count to the first nil.
pub fn getn(vm: &mut Vm, t: crate::value::TableRef) -> usize {
    let n = vm.heap.table(t).get(Value::Str(vm.names.n));
    if let Value::Num(x) = n {
        let i = c_int(x);
        if i >= 0 {
            return i as usize;
        }
    }
    if let Some(n) = vm.heap.table(t).setn {
        return n;
    }
    vm.heap.table(t).border()
}

/// `luaL_setn`: into `t.n` when the table has a numeric one, else the remembered size.
pub fn setn(vm: &mut Vm, t: crate::value::TableRef, n: usize) {
    let key = Value::Str(vm.names.n);
    let has_n = matches!(vm.heap.table(t).get(key), Value::Num(x) if c_int(x) >= 0);
    if has_n {
        vm.heap.table_mut(t).set(key, Value::Num(n as f64)).unwrap();
    } else {
        vm.heap.table_mut(t).setn = Some(n);
    }
}

fn unpack(vm: &mut Vm, nargs: usize) -> R<usize> {
    let t = vm.check_table(nargs, 1)?;
    let n = getn(vm, t);
    // `luaL_checkstack` against 5.0's `LUA_MAXCSTACK` of 2048.
    if n > 2048 {
        return Err(vm.error_at(1, "stack overflow (table too big to unpack)"));
    }
    for i in 1..=n {
        let v = vm.heap.table(t).get_int(i as i32);
        vm.push(v);
    }
    Ok(n)
}

fn rawequal(vm: &mut Vm, nargs: usize) -> R<usize> {
    let a = vm.check_any(nargs, 1)?;
    let b = vm.check_any(nargs, 2)?;
    Ok(vm.ret1(Value::Bool(a.raw_eq(b))))
}

fn rawget(vm: &mut Vm, nargs: usize) -> R<usize> {
    let t = vm.check_table(nargs, 1)?;
    let k = vm.check_any(nargs, 2)?;
    let v = vm.heap.table(t).get(k);
    Ok(vm.ret1(v))
}

fn rawset(vm: &mut Vm, nargs: usize) -> R<usize> {
    let t = vm.check_table(nargs, 1)?;
    let k = vm.check_any(nargs, 2)?;
    let v = vm.check_any(nargs, 3)?;
    if let Err(e) = vm.heap.table_mut(t).set(k, v) {
        let msg = match e {
            crate::table::KeyError::Nil => "table index is nil",
            crate::table::KeyError::NaN => "table index is NaN",
        };
        return Err(vm.rt_error(msg));
    }
    Ok(vm.ret1(Value::Table(t)))
}

fn pcall(vm: &mut Vm, nargs: usize) -> R<usize> {
    vm.check_any(nargs, 1)?;
    let func_idx = vm.top - nargs;
    match vm.pcall_at(func_idx, nargs - 1, -1, None) {
        Ok(()) => {
            let n = vm.top - func_idx;
            vm.ensure(vm.top + 1);
            vm.stack.copy_within(func_idx..vm.top, func_idx + 1);
            vm.stack[func_idx] = Value::Bool(true);
            vm.top += 1;
            Ok(n + 1)
        }
        Err(e) => {
            vm.push(Value::Bool(false));
            vm.push(e.value);
            Ok(2)
        }
    }
}

fn xpcall(vm: &mut Vm, nargs: usize) -> R<usize> {
    let f = vm.arg(nargs, 0);
    let h = vm.arg(nargs, 1);
    let func_idx = vm.top - nargs;
    vm.top = func_idx;
    vm.push(f);
    match vm.pcall_at(func_idx, 0, -1, Some(h)) {
        Ok(()) => {
            let n = vm.top - func_idx;
            vm.ensure(vm.top + 1);
            vm.stack.copy_within(func_idx..vm.top, func_idx + 1);
            vm.stack[func_idx] = Value::Bool(true);
            vm.top += 1;
            Ok(n + 1)
        }
        Err(e) => {
            vm.push(Value::Bool(false));
            vm.push(e.value);
            Ok(2)
        }
    }
}

/// 1.12's `collectgarbage([kb])` (`0x703250`): sets 5.0's threshold, answers nothing.
fn collectgarbage(vm: &mut Vm, nargs: usize) -> R<usize> {
    let n = vm.opt_number(nargs, 1, 0.0)?;
    let kb = n.trunc();
    vm.heap.threshold = if !(0.0..=0x3f_ffff as f64).contains(&kb) {
        u32::MAX as usize
    } else {
        kb as usize * 1024
    };
    vm.check_gc();
    Ok(0)
}

/// 1.12's `gcinfo()` (`0x703200`): live and threshold kilobytes.
fn gcinfo(vm: &mut Vm, _nargs: usize) -> R<usize> {
    vm.push(Value::Num((vm.heap.bytes >> 10) as f64));
    vm.push(Value::Num((vm.heap.threshold >> 10) as f64));
    Ok(2)
}

/// `luaB_loadstring`: the chunk named by its own source unless named; a failure answers nil and
/// the message. The client's `luaL_loadbuffer` strips a UTF-8 BOM, and 5.0's lexer skips a
/// leading `#` line.
fn loadstring(vm: &mut Vm, nargs: usize) -> R<usize> {
    let s = vm.check_str(nargs, 1)?;
    let src = vm.heap.str(s).to_vec();
    let name = match vm.arg(nargs, 1) {
        Value::Nil => src.clone(),
        _ => {
            let n = vm.check_str(nargs, 2)?;
            vm.heap.str(n).to_vec()
        }
    };
    let body = src.strip_prefix(&[0xEF, 0xBB, 0xBF][..]).unwrap_or(&src);
    let body: &[u8] = match body.first() {
        Some(b'#') => match body.iter().position(|&b| b == b'\n') {
            Some(nl) => &body[nl..],
            None => &[],
        },
        _ => body,
    };
    match vm.load(body, &name) {
        Ok(f) => Ok(vm.ret1(f)),
        Err(e) => {
            vm.push(Value::Nil);
            let m = vm.str(&e.0);
            vm.push(m);
            Ok(2)
        }
    }
}
