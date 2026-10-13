//! The math library (5.0's `lmathlib.c`, radians, with the global `__pow` that `^` calls) and
//! 1.12's `bit` library (`0x7fadc0`).

use crate::api::R;
use crate::value::Value;
use crate::vm::Vm;

pub fn open(vm: &mut Vm) {
    let t = vm.heap.new_table(0, 5);
    let fns: [(&'static str, crate::heap::NativeFn); 24] = [
        ("abs", |vm, n| un(vm, n, f64::abs)),
        ("sin", |vm, n| un(vm, n, f64::sin)),
        ("cos", |vm, n| un(vm, n, f64::cos)),
        ("tan", |vm, n| un(vm, n, f64::tan)),
        ("asin", |vm, n| un(vm, n, f64::asin)),
        ("acos", |vm, n| un(vm, n, f64::acos)),
        ("atan", |vm, n| un(vm, n, f64::atan)),
        ("atan2", |vm, n| bin(vm, n, f64::atan2)),
        ("ceil", |vm, n| un(vm, n, f64::ceil)),
        ("floor", |vm, n| un(vm, n, f64::floor)),
        // C's `fmod`: the sign of the dividend.
        ("mod", |vm, n| bin(vm, n, |a, b| a % b)),
        ("frexp", frexp),
        ("ldexp", ldexp),
        ("sqrt", |vm, n| un(vm, n, f64::sqrt)),
        ("min", min),
        ("max", max),
        ("log", |vm, n| un(vm, n, f64::ln)),
        ("log10", |vm, n| un(vm, n, f64::log10)),
        ("exp", |vm, n| un(vm, n, f64::exp)),
        ("deg", |vm, n| un(vm, n, f64::to_degrees)),
        ("pow", |vm, n| bin(vm, n, f64::powf)),
        ("rad", |vm, n| un(vm, n, f64::to_radians)),
        ("random", random),
        ("randomseed", randomseed),
    ];
    for (name, f) in fns {
        let v = vm.new_native(name, f);
        let k = vm.str(name.as_bytes());
        vm.heap.table_mut(t).set(k, v).unwrap();
    }
    let k = vm.str(b"pi");
    vm.heap
        .table_mut(t)
        .set(k, Value::Num(std::f64::consts::PI))
        .unwrap();
    vm.set_global("math", Value::Table(t));
    // `luaopen_math` registers `math_pow` as the global `__pow`, which the VM calls for `^`.
    let pow = vm.new_native("__pow", |vm, n| bin(vm, n, f64::powf));
    vm.set_global("__pow", pow);
    open_bit(vm);
}

fn un(vm: &mut Vm, nargs: usize, f: fn(f64) -> f64) -> R<usize> {
    let x = vm.check_number(nargs, 1)?;
    Ok(vm.ret1(Value::Num(f(x))))
}

fn bin(vm: &mut Vm, nargs: usize, f: fn(f64, f64) -> f64) -> R<usize> {
    let x = vm.check_number(nargs, 1)?;
    let y = vm.check_number(nargs, 2)?;
    Ok(vm.ret1(Value::Num(f(x, y))))
}

fn frexp(vm: &mut Vm, nargs: usize) -> R<usize> {
    let x = vm.check_number(nargs, 1)?;
    let (m, e) = if x == 0.0 || !x.is_finite() {
        (x, 0)
    } else {
        let bits = x.to_bits();
        let exp = ((bits >> 52) & 0x7ff) as i32;
        if exp == 0 {
            // Subnormal: scale into the normal range first.
            let y = x * 2f64.powi(54);
            let e = ((y.to_bits() >> 52) & 0x7ff) as i32 - 1022 - 54;
            (y / 2f64.powi(e + 54), e)
        } else {
            let e = exp - 1022;
            (f64::from_bits((bits & !(0x7ff << 52)) | (1022 << 52)), e)
        }
    };
    vm.push(Value::Num(m));
    vm.push(Value::Num(e as f64));
    Ok(2)
}

fn ldexp(vm: &mut Vm, nargs: usize) -> R<usize> {
    let x = vm.check_number(nargs, 1)?;
    let e = vm.check_int(nargs, 2)?;
    Ok(vm.ret1(Value::Num(x * 2f64.powi(e))))
}

fn min(vm: &mut Vm, nargs: usize) -> R<usize> {
    let mut m = vm.check_number(nargs, 1)?;
    for i in 2..=nargs {
        let d = vm.check_number(nargs, i)?;
        if d < m {
            m = d;
        }
    }
    Ok(vm.ret1(Value::Num(m)))
}

fn max(vm: &mut Vm, nargs: usize) -> R<usize> {
    let mut m = vm.check_number(nargs, 1)?;
    for i in 2..=nargs {
        let d = vm.check_number(nargs, i)?;
        if d > m {
            m = d;
        }
    }
    Ok(vm.ret1(Value::Num(m)))
}

/// MSVC's `rand()`: the client's generator, seeded 1 until `randomseed`.
fn rand(vm: &mut Vm) -> u32 {
    vm.rand_seed = vm.rand_seed.wrapping_mul(214013).wrapping_add(2531011);
    (vm.rand_seed >> 16) & 0x7fff
}

const RAND_MAX: u32 = 0x7fff;

fn random(vm: &mut Vm, nargs: usize) -> R<usize> {
    let r = (rand(vm) % RAND_MAX) as f64 / RAND_MAX as f64;
    let v = match nargs {
        0 => r,
        1 => {
            let u = vm.check_int(nargs, 1)?;
            if u < 1 {
                return Err(vm.arg_error(1, "interval is empty"));
            }
            ((r * u as f64).floor() as i32 + 1) as f64
        }
        2 => {
            let l = vm.check_int(nargs, 1)?;
            let u = vm.check_int(nargs, 2)?;
            if l > u {
                return Err(vm.arg_error(2, "interval is empty"));
            }
            ((r * (u as f64 - l as f64 + 1.0)).floor() as i32 + l) as f64
        }
        _ => return Err(vm.error_at(1, "wrong number of arguments")),
    };
    Ok(vm.ret1(Value::Num(v)))
}

fn randomseed(vm: &mut Vm, nargs: usize) -> R<usize> {
    let s = vm.check_int(nargs, 1)?;
    vm.rand_seed = s as u32;
    Ok(0)
}

// ── bit ──────────────────────────────────────────────────────────────────────────────────────

/// The client truncates to a 32-bit int and answers it signed.
fn u32_of(v: f64) -> u32 {
    v as i64 as u32
}

fn out(v: u32) -> Value {
    Value::Num(v as i32 as f64)
}

fn open_bit(vm: &mut Vm) {
    let t = vm.heap.new_table(0, 3);
    let fns: [(&'static str, crate::heap::NativeFn); 8] = [
        ("bnot", |vm, n| {
            let a = vm.check_number(n, 1)?;
            Ok(vm.ret1(out(!u32_of(a))))
        }),
        ("band", |vm, n| bit2(vm, n, |a, b| a & b)),
        ("bor", |vm, n| bit2(vm, n, |a, b| a | b)),
        ("bxor", |vm, n| bit2(vm, n, |a, b| a ^ b)),
        // Shift counts mask to 5 bits, as x86 shifts do.
        ("lshift", |vm, n| bit2(vm, n, |a, b| a << (b & 31))),
        ("rshift", |vm, n| bit2(vm, n, |a, b| a >> (b & 31))),
        ("arshift", |vm, n| {
            bit2(vm, n, |a, b| ((a as i32) >> (b & 31)) as u32)
        }),
        ("mod", |vm, n| {
            let a = vm.check_number(n, 1)?;
            let b = u32_of(vm.check_number(n, 2)?) as i32;
            if b == 0 {
                return Err(vm.error_at(1, "bit.mod: division by zero"));
            }
            Ok(vm.ret1(Value::Num(((u32_of(a) as i32).wrapping_rem(b)) as f64)))
        }),
    ];
    for (name, f) in fns {
        let v = vm.new_native(name, f);
        let k = vm.str(name.as_bytes());
        vm.heap.table_mut(t).set(k, v).unwrap();
    }
    vm.set_global("bit", Value::Table(t));
}

fn bit2(vm: &mut Vm, nargs: usize, f: fn(u32, u32) -> u32) -> R<usize> {
    let a = vm.check_number(nargs, 1)?;
    let b = vm.check_number(nargs, 2)?;
    Ok(vm.ret1(out(f(u32_of(a), u32_of(b)))))
}
