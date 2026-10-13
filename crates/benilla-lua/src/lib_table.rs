//! The table library: 5.0's `ltablib.c`, `n`-based end to end through `luaL_getn`/`luaL_setn`,
//! and 5.0's own quicksort, so an inconsistent comparator does what it does in the client.

use crate::api::R;
use crate::lib_base::{getn, setn};
use crate::value::{TableRef, Value, V};
use crate::vm::Vm;

pub fn open(vm: &mut Vm) {
    let t = vm.heap.new_table(0, 3);
    let fns: [(&'static str, crate::heap::NativeFn); 8] = [
        ("concat", concat),
        ("foreach", foreach),
        ("foreachi", foreachi),
        ("getn", getn_),
        ("setn", setn_),
        ("sort", sort),
        ("insert", insert),
        ("remove", remove),
    ];
    for (name, f) in fns {
        let v = vm.new_native(name, f);
        let k = vm.str(name.as_bytes());
        vm.heap.table_mut(t).set(k, v).unwrap();
    }
    vm.set_global("table", Value::Table(t));
}

fn getn_(vm: &mut Vm, nargs: usize) -> R<usize> {
    let t = vm.check_table(nargs, 1)?;
    let n = getn(vm, t);
    Ok(vm.ret1(Value::Num(n as f64)))
}

fn setn_(vm: &mut Vm, nargs: usize) -> R<usize> {
    let t = vm.check_table(nargs, 1)?;
    let n = vm.check_int(nargs, 2)?;
    setn(vm, t, n.max(0) as usize);
    Ok(0)
}

fn rawgeti(vm: &Vm, t: TableRef, i: i64) -> Value {
    vm.heap.table(t).get_int(i as i32)
}

fn rawseti(vm: &mut Vm, t: TableRef, i: i64, v: Value) {
    vm.heap.table_mut(t).set_int(i as i32, v);
}

fn foreachi(vm: &mut Vm, nargs: usize) -> R<usize> {
    let t = vm.check_table(nargs, 1)?;
    let f = vm.check_function(nargs, 2)?;
    let n = getn(vm, t);
    for i in 1..=n {
        let v = rawgeti(vm, t, i as i64);
        let r = vm.call_value(f, &[Value::Num(i as f64), v], 1)?;
        if !r[0].is_nil() {
            return Ok(vm.ret1(r[0]));
        }
    }
    Ok(0)
}

fn foreach(vm: &mut Vm, nargs: usize) -> R<usize> {
    let t = vm.check_table(nargs, 1)?;
    let f = vm.check_function(nargs, 2)?;
    let mut k = Value::Nil;
    loop {
        let next = match vm.heap.table(t).next(k) {
            Ok(n) => n,
            Err(crate::table::InvalidKey) => return Err(vm.rt_error("invalid key for `next'")),
        };
        let Some((nk, v)) = next else { return Ok(0) };
        // The key stays on the stack across the call, as 5.0's loop keeps it.
        vm.push(nk);
        let r = vm.call_value(f, &[nk, v], 1);
        vm.top -= 1;
        let r = r?;
        if !r[0].is_nil() {
            return Ok(vm.ret1(r[0]));
        }
        k = nk;
    }
}

/// 5.0's `tinsert`: two arguments append; three insert at the position, growing the size to it.
fn insert(vm: &mut Vm, nargs: usize) -> R<usize> {
    let t = vm.check_table(nargs, 1)?;
    let mut n = getn(vm, t) as i64 + 1;
    let (pos, v) = if nargs == 2 {
        (n, vm.arg(nargs, 1))
    } else {
        let pos = vm.check_int(nargs, 2)? as i64;
        if pos > n {
            n = pos;
        }
        (pos, vm.arg(nargs, 2))
    };
    setn(vm, t, n.max(0) as usize);
    n -= 1;
    while n >= pos {
        let x = rawgeti(vm, t, n);
        rawseti(vm, t, n + 1, x);
        n -= 1;
    }
    rawseti(vm, t, pos, v);
    Ok(0)
}

fn remove(vm: &mut Vm, nargs: usize) -> R<usize> {
    let t = vm.check_table(nargs, 1)?;
    let n = getn(vm, t) as i64;
    let mut pos = vm.opt_int(nargs, 2, n as i32)? as i64;
    if n <= 0 {
        return Ok(0);
    }
    setn(vm, t, (n - 1) as usize);
    let out = rawgeti(vm, t, pos);
    while pos < n {
        let x = rawgeti(vm, t, pos + 1);
        rawseti(vm, t, pos, x);
        pos += 1;
    }
    rawseti(vm, t, n, Value::Nil);
    Ok(vm.ret1(out))
}

fn concat(vm: &mut Vm, nargs: usize) -> R<usize> {
    let sep = vm.opt_str(nargs, 2, b"")?;
    let mut i = vm.opt_int(nargs, 3, 1)? as i64;
    let mut n = vm.opt_int(nargs, 4, 0)? as i64;
    let t = vm.check_table(nargs, 1)?;
    if n == 0 {
        n = getn(vm, t) as i64;
    }
    let sep = vm.heap.str(sep).to_vec();
    let mut out = Vec::new();
    while i <= n {
        let v = rawgeti(vm, t, i);
        let Some(s) = (match v.unpack() {
            V::Str(_) | V::Num(_) => vm.tostr(v),
            _ => None,
        }) else {
            return Err(vm.arg_error(1, "table contains non-strings"));
        };
        out.extend_from_slice(vm.heap.str(s));
        if i != n {
            out.extend_from_slice(&sep);
        }
        i += 1;
    }
    Ok(vm.ret_str(&out))
}

/// `sort_comp`: the comparator, or `lua_lessthan`.
fn comp(vm: &mut Vm, f: Value, a: Value, b: Value) -> R<bool> {
    if f.is_nil() {
        vm.less_than(a, b)
    } else {
        Ok(vm.call_value(f, &[a, b], 1)?[0].truthy())
    }
}

/// 5.0's `auxsort`, step for step, so a comparator's call order is the client's.
fn auxsort(vm: &mut Vm, t: TableRef, f: Value, mut l: i64, mut u: i64) -> R<()> {
    while l < u {
        let al = rawgeti(vm, t, l);
        let au = rawgeti(vm, t, u);
        if comp(vm, f, au, al)? {
            rawseti(vm, t, l, au);
            rawseti(vm, t, u, al);
        }
        if u - l == 1 {
            break;
        }
        let mut i = (l + u) / 2;
        let ai = rawgeti(vm, t, i);
        let al = rawgeti(vm, t, l);
        if comp(vm, f, ai, al)? {
            rawseti(vm, t, i, al);
            rawseti(vm, t, l, ai);
        } else {
            let au = rawgeti(vm, t, u);
            if comp(vm, f, au, ai)? {
                rawseti(vm, t, i, au);
                rawseti(vm, t, u, ai);
            }
        }
        if u - l == 2 {
            break;
        }
        let p = rawgeti(vm, t, i);
        let au1 = rawgeti(vm, t, u - 1);
        rawseti(vm, t, i, au1);
        rawseti(vm, t, u - 1, p);
        i = l;
        let mut j = u - 1;
        loop {
            loop {
                i += 1;
                let ai = rawgeti(vm, t, i);
                if !comp(vm, f, ai, p)? {
                    break;
                }
                if i > u {
                    return Err(vm.error_at(1, "invalid order function for sorting"));
                }
            }
            loop {
                j -= 1;
                let aj = rawgeti(vm, t, j);
                if !comp(vm, f, p, aj)? {
                    break;
                }
                if j < l {
                    return Err(vm.error_at(1, "invalid order function for sorting"));
                }
            }
            if j < i {
                break;
            }
            let ai = rawgeti(vm, t, i);
            let aj = rawgeti(vm, t, j);
            rawseti(vm, t, i, aj);
            rawseti(vm, t, j, ai);
        }
        let au1 = rawgeti(vm, t, u - 1);
        let ai = rawgeti(vm, t, i);
        rawseti(vm, t, u - 1, ai);
        rawseti(vm, t, i, au1);
        let (lo, hi);
        if i - l < u - i {
            lo = l;
            hi = i - 1;
            l = i + 1;
        } else {
            lo = i + 1;
            hi = u;
            u = i - 1;
        }
        auxsort(vm, t, f, lo, hi)?;
    }
    Ok(())
}

fn sort(vm: &mut Vm, nargs: usize) -> R<usize> {
    let t = vm.check_table(nargs, 1)?;
    let n = getn(vm, t) as i64;
    let f = vm.arg(nargs, 1);
    if !f.is_nil() {
        vm.check_function(nargs, 2)?;
    }
    auxsort(vm, t, f, 1, n)?;
    Ok(0)
}
