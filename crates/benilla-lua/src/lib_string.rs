//! The string library: 5.0.2's `lstrlib.c`, whose twelve names and messages the client carries
//! (the pattern matcher with `%f` and `%b`, `gfind`, no `gmatch`/`match`/`reverse`). A pattern
//! ends at its first NUL, as 5.0's matcher reads a C string.

use crate::api::R;
use crate::cfmt::{self, Spec};
use crate::value::{Value, V};
use crate::vm::{LuaError, Vm};

pub fn open(vm: &mut Vm) {
    let t = vm.heap.new_table(0, 4);
    let fns: [(&'static str, crate::heap::NativeFn); 12] = [
        ("len", len),
        ("sub", sub),
        ("lower", lower),
        ("upper", upper),
        ("char", char_),
        ("rep", rep),
        ("byte", byte),
        ("format", format),
        ("dump", dump),
        ("find", find),
        ("gfind", gfind),
        ("gsub", gsub),
    ];
    for (name, f) in fns {
        let v = vm.new_native(name, f);
        let k = vm.str(name.as_bytes());
        vm.heap.table_mut(t).set(k, v).unwrap();
    }
    vm.set_global("string", Value::Table(t));
}

/// 5.0's `posrelat`: a negative position counts from the end.
fn posrelat(pos: i64, len: usize) -> i64 {
    if pos >= 0 {
        pos
    } else {
        len as i64 + pos + 1
    }
}

fn len(vm: &mut Vm, nargs: usize) -> R<usize> {
    let s = vm.check_str(nargs, 1)?;
    let n = vm.heap.str(s).len();
    Ok(vm.ret1(Value::Num(n as f64)))
}

fn sub(vm: &mut Vm, nargs: usize) -> R<usize> {
    let s = vm.check_str(nargs, 1)?;
    let l = vm.heap.str(s).len();
    let start = vm.check_int(nargs, 2)? as i64;
    let end = vm.opt_int(nargs, 3, -1)? as i64;
    let mut start = posrelat(start, l);
    let mut end = posrelat(end, l);
    if start < 1 {
        start = 1;
    }
    if end > l as i64 {
        end = l as i64;
    }
    if start <= end {
        let (a, e) = (start as usize - 1, end as usize);
        let r = vm
            .heap
            .intern_with(|h, b| b.extend_from_slice(&h.str(s)[a..e]));
        Ok(vm.ret1(Value::Str(r)))
    } else {
        Ok(vm.ret_str(b""))
    }
}

fn lower(vm: &mut Vm, nargs: usize) -> R<usize> {
    let s = vm.check_str(nargs, 1)?;
    let r = vm.heap.intern_with(|h, b| {
        b.extend_from_slice(h.str(s));
        b.make_ascii_lowercase();
    });
    Ok(vm.ret1(Value::Str(r)))
}

fn upper(vm: &mut Vm, nargs: usize) -> R<usize> {
    let s = vm.check_str(nargs, 1)?;
    let r = vm.heap.intern_with(|h, b| {
        b.extend_from_slice(h.str(s));
        b.make_ascii_uppercase();
    });
    Ok(vm.ret1(Value::Str(r)))
}

fn char_(vm: &mut Vm, nargs: usize) -> R<usize> {
    let mut out = Vec::with_capacity(nargs);
    for i in 1..=nargs {
        let c = vm.check_int(nargs, i)?;
        if !(0..=255).contains(&c) {
            return Err(vm.arg_error(i, "invalid value"));
        }
        out.push(c as u8);
    }
    Ok(vm.ret_str(&out))
}

fn rep(vm: &mut Vm, nargs: usize) -> R<usize> {
    let s = vm.check_str(nargs, 1)?;
    let n = vm.check_int(nargs, 2)?;
    let b = vm.heap.str(s);
    let out = if n > 0 {
        b.repeat(n as usize)
    } else {
        Vec::new()
    };
    Ok(vm.ret_str(&out))
}

/// 5.0's `byte`: one byte, or nothing out of range.
fn byte(vm: &mut Vm, nargs: usize) -> R<usize> {
    let s = vm.check_str(nargs, 1)?;
    let l = vm.heap.str(s).len();
    let pos = posrelat(vm.opt_int(nargs, 2, 1)? as i64, l);
    if pos <= 0 || pos as usize > l {
        return Ok(0);
    }
    let b = vm.heap.str(s)[pos as usize - 1];
    Ok(vm.ret1(Value::Num(b as f64)))
}

fn dump(vm: &mut Vm, nargs: usize) -> R<usize> {
    vm.check_function(nargs, 1)?;
    Err(vm.error_at(1, "unable to dump given function"))
}

// ── string.format ────────────────────────────────────────────────────────────────────────────

fn format(vm: &mut Vm, nargs: usize) -> R<usize> {
    let mut b = vm.take_bufs::<2>();
    let r = format_in(vm, nargs, &mut b);
    vm.give_bufs(b);
    r
}

fn format_in(vm: &mut Vm, nargs: usize, [f, out]: &mut [Vec<u8>; 2]) -> R<usize> {
    let fs = vm.check_str(nargs, 1)?;
    f.extend_from_slice(vm.heap.str(fs));
    let f = &f[..];
    let mut arg = 1;
    let mut i = 0;
    while i < f.len() {
        if f[i] != b'%' {
            out.push(f[i]);
            i += 1;
            continue;
        }
        i += 1;
        if i < f.len() && f[i] == b'%' {
            out.push(b'%');
            i += 1;
            continue;
        }
        arg += 1;
        // `scanformat`: flags, two width digits, a point and two precision digits at most.
        let mut spec = Spec::default();
        let at = |j: usize| f.get(j).copied().unwrap_or(0);
        while b"-+ #0".contains(&at(i)) && at(i) != 0 {
            match at(i) {
                b'-' => spec.minus = true,
                b'+' => spec.plus = true,
                b' ' => spec.space = true,
                b'#' => spec.alt = true,
                _ => spec.zero = true,
            }
            i += 1;
        }
        for _ in 0..2 {
            if at(i).is_ascii_digit() {
                spec.width = spec.width * 10 + (at(i) - b'0') as usize;
                i += 1;
            }
        }
        if at(i) == b'.' {
            i += 1;
            let mut p = 0;
            for _ in 0..2 {
                if at(i).is_ascii_digit() {
                    p = p * 10 + (at(i) - b'0') as usize;
                    i += 1;
                }
            }
            spec.precision = Some(p);
        }
        if at(i).is_ascii_digit() {
            return Err(vm.error_at(1, "invalid format (width or precision too long)"));
        }
        let conv = at(i);
        i += 1;
        match conv {
            b'c' => {
                let c = vm.check_int(nargs, arg)?;
                let item = cfmt::fmt_str(
                    &[c as u8],
                    &Spec {
                        precision: None,
                        ..spec
                    },
                );
                // `luaL_addlstring(b, buff, strlen(buff))`: a `%c` of 0 ends the item.
                out.extend(item.iter().take_while(|&&b| b != 0));
            }
            b'd' | b'i' => {
                let n = vm.check_int(nargs, arg)?;
                if spec.is_plain() {
                    // A bare `%d`: the digits alone.
                    crate::number::push_g14(n as f64, out);
                } else {
                    out.extend_from_slice(cfmt::fmt_int(n, &spec).as_bytes());
                }
            }
            b'o' | b'u' | b'x' | b'X' => {
                let n = vm.check_number(nargs, arg)?;
                out.extend_from_slice(cfmt::fmt_uint(n as i64 as u32, conv, &spec).as_bytes());
            }
            b'e' | b'E' | b'f' | b'g' | b'G' => {
                let n = vm.check_number(nargs, arg)?;
                out.extend_from_slice(cfmt::fmt_float(n, conv, &spec).as_bytes());
            }
            b'q' => {
                let s = vm.check_str(nargs, arg)?;
                out.push(b'"');
                for &c in vm.heap.str(s) {
                    match c {
                        b'"' | b'\\' | b'\n' => {
                            out.push(b'\\');
                            out.push(c);
                        }
                        0 => out.extend_from_slice(b"\\000"),
                        _ => out.push(c),
                    }
                }
                out.push(b'"');
            }
            b's' => {
                let s = vm.check_str(nargs, arg)?;
                let b = vm.heap.str(s);
                if spec.precision.is_none() && b.len() >= 100 {
                    out.extend_from_slice(b);
                } else {
                    // `sprintf` reads a C string.
                    let c = &b[..b.iter().position(|&x| x == 0).unwrap_or(b.len())];
                    if spec.is_plain() {
                        out.extend_from_slice(c);
                    } else {
                        out.extend_from_slice(&cfmt::fmt_str(c, &spec));
                    }
                }
            }
            _ => return Err(vm.error_at(1, "invalid option in `format'")),
        }
    }
    Ok(vm.ret_str(out))
}

// ── the matcher ──────────────────────────────────────────────────────────────────────────────

const MAX_CAPTURES: usize = 32;
const CAP_UNFINISHED: isize = -1;
const CAP_POSITION: isize = -2;
const SPECIALS: &[u8] = b"^$*+?.([%-";

struct MatchState<'a> {
    src: &'a [u8],
    /// The pattern, NUL-terminated as 5.0's C string is.
    pat: &'a [u8],
    level: usize,
    capture: [(usize, isize); MAX_CAPTURES],
}

/// A matcher error: the message `luaL_error` raises.
struct PatErr(&'static str);

type M<T> = Result<T, PatErr>;

fn class_match(c: u8, cl: u8) -> bool {
    let res = match cl.to_ascii_lowercase() {
        b'a' => c.is_ascii_alphabetic(),
        b'c' => c < 32 || c == 127,
        b'd' => c.is_ascii_digit(),
        b'l' => c.is_ascii_lowercase(),
        b'p' => c.is_ascii_punctuation(),
        b's' => matches!(c, 9..=13 | 32),
        b'u' => c.is_ascii_uppercase(),
        b'w' => c.is_ascii_alphanumeric(),
        b'x' => c.is_ascii_hexdigit(),
        b'z' => c == 0,
        _ => return cl == c,
    };
    if cl.is_ascii_lowercase() {
        res
    } else {
        !res
    }
}

impl MatchState<'_> {
    fn p(&self, i: usize) -> u8 {
        self.pat.get(i).copied().unwrap_or(0)
    }

    fn class_end(&self, p: usize) -> M<usize> {
        let mut p = p;
        let c = self.p(p);
        p += 1;
        match c {
            b'%' => {
                if self.p(p) == 0 {
                    return Err(PatErr("malformed pattern (ends with `%')"));
                }
                Ok(p + 1)
            }
            b'[' => {
                if self.p(p) == b'^' {
                    p += 1;
                }
                loop {
                    if self.p(p) == 0 {
                        return Err(PatErr("malformed pattern (missing `]')"));
                    }
                    let c = self.p(p);
                    p += 1;
                    if c == b'%' && self.p(p) != 0 {
                        p += 1;
                    }
                    if self.p(p) == b']' {
                        break;
                    }
                }
                Ok(p + 1)
            }
            _ => Ok(p),
        }
    }

    /// `matchbracketclass`: `p` at the `[`, `ec` at the `]`.
    fn bracket(&self, c: u8, p: usize, ec: usize) -> bool {
        let mut sig = true;
        let mut p = p;
        if self.p(p + 1) == b'^' {
            sig = false;
            p += 1;
        }
        loop {
            p += 1;
            if p >= ec {
                break;
            }
            if self.p(p) == b'%' {
                p += 1;
                if class_match(c, self.p(p)) {
                    return sig;
                }
            } else if self.p(p + 1) == b'-' && p + 2 < ec {
                p += 2;
                if self.p(p - 2) <= c && c <= self.p(p) {
                    return sig;
                }
            } else if self.p(p) == c {
                return sig;
            }
        }
        !sig
    }

    fn single(&self, c: u8, p: usize, ep: usize) -> bool {
        match self.p(p) {
            b'.' => true,
            b'%' => class_match(c, self.p(p + 1)),
            b'[' => self.bracket(c, p, ep - 1),
            pc => pc == c,
        }
    }

    fn do_match(&mut self, s: usize, p: usize) -> M<Option<usize>> {
        let (mut s, mut p) = (s, p);
        loop {
            match self.p(p) {
                b'(' => {
                    return if self.p(p + 1) == b')' {
                        self.start_capture(s, p + 2, CAP_POSITION)
                    } else {
                        self.start_capture(s, p + 1, CAP_UNFINISHED)
                    };
                }
                b')' => return self.end_capture(s, p + 1),
                0 => return Ok(Some(s)),
                b'$' if self.p(p + 1) == 0 => {
                    return Ok(if s == self.src.len() { Some(s) } else { None });
                }
                b'%' if self.p(p + 1) == b'b' => match self.balance(s, p + 2)? {
                    Some(e) => {
                        s = e;
                        p += 4;
                        continue;
                    }
                    None => return Ok(None),
                },
                b'%' if self.p(p + 1) == b'f' => {
                    p += 2;
                    if self.p(p) != b'[' {
                        return Err(PatErr("missing `[' after `%f' in pattern"));
                    }
                    let ep = self.class_end(p)?;
                    let prev = if s == 0 { 0 } else { self.src[s - 1] };
                    let cur = self.src.get(s).copied().unwrap_or(0);
                    if !self.bracket(prev, p, ep - 1) && self.bracket(cur, p, ep - 1) {
                        p = ep;
                        continue;
                    }
                    return Ok(None);
                }
                b'%' if self.p(p + 1).is_ascii_digit() => {
                    match self.match_capture(s, self.p(p + 1))? {
                        Some(e) => {
                            s = e;
                            p += 2;
                            continue;
                        }
                        None => return Ok(None),
                    }
                }
                _ => {
                    let ep = self.class_end(p)?;
                    let m = s < self.src.len() && self.single(self.src[s], p, ep);
                    match self.p(ep) {
                        b'?' => {
                            if m {
                                if let Some(r) = self.do_match(s + 1, ep + 1)? {
                                    return Ok(Some(r));
                                }
                            }
                            p = ep + 1;
                        }
                        b'*' => return self.max_expand(s, p, ep),
                        b'+' => {
                            return if m {
                                self.max_expand(s + 1, p, ep)
                            } else {
                                Ok(None)
                            }
                        }
                        b'-' => return self.min_expand(s, p, ep),
                        _ => {
                            if !m {
                                return Ok(None);
                            }
                            s += 1;
                            p = ep;
                        }
                    }
                }
            }
        }
    }

    fn balance(&self, s: usize, p: usize) -> M<Option<usize>> {
        if self.p(p) == 0 || self.p(p + 1) == 0 {
            return Err(PatErr("unbalanced pattern"));
        }
        if s >= self.src.len() || self.src[s] != self.p(p) {
            return Ok(None);
        }
        let (b, e) = (self.p(p), self.p(p + 1));
        let mut cont = 1;
        let mut s = s + 1;
        while s < self.src.len() {
            let c = self.src[s];
            if c == e {
                cont -= 1;
                if cont == 0 {
                    return Ok(Some(s + 1));
                }
            } else if c == b {
                cont += 1;
            }
            s += 1;
        }
        Ok(None)
    }

    // Inlined into `do_match`: one frame per pattern level, so a deep match stays within the
    // CPU's return-address predictor.
    #[inline(always)]
    fn max_expand(&mut self, s: usize, p: usize, ep: usize) -> M<Option<usize>> {
        let mut i = 0;
        while s + i < self.src.len() && self.single(self.src[s + i], p, ep) {
            i += 1;
        }
        loop {
            if let Some(r) = self.do_match(s + i, ep + 1)? {
                return Ok(Some(r));
            }
            if i == 0 {
                return Ok(None);
            }
            i -= 1;
        }
    }

    // Inlined into `do_match`: one frame per pattern level, so a deep match stays within the
    // CPU's return-address predictor.
    #[inline(always)]
    fn min_expand(&mut self, s: usize, p: usize, ep: usize) -> M<Option<usize>> {
        let mut s = s;
        loop {
            if let Some(r) = self.do_match(s, ep + 1)? {
                return Ok(Some(r));
            }
            if s < self.src.len() && self.single(self.src[s], p, ep) {
                s += 1;
            } else {
                return Ok(None);
            }
        }
    }

    // Inlined into `do_match`: one frame per pattern level, so a deep match stays within the
    // CPU's return-address predictor.
    #[inline(always)]
    fn start_capture(&mut self, s: usize, p: usize, what: isize) -> M<Option<usize>> {
        if self.level >= MAX_CAPTURES {
            return Err(PatErr("too many captures"));
        }
        self.capture[self.level] = (s, what);
        self.level += 1;
        let r = self.do_match(s, p)?;
        if r.is_none() {
            self.level -= 1;
        }
        Ok(r)
    }

    // Inlined into `do_match`: one frame per pattern level, so a deep match stays within the
    // CPU's return-address predictor.
    #[inline(always)]
    fn end_capture(&mut self, s: usize, p: usize) -> M<Option<usize>> {
        let l = self.capture_to_close()?;
        self.capture[l].1 = (s - self.capture[l].0) as isize;
        let r = self.do_match(s, p)?;
        if r.is_none() {
            self.capture[l].1 = CAP_UNFINISHED;
        }
        Ok(r)
    }

    fn capture_to_close(&self) -> M<usize> {
        (0..self.level)
            .rev()
            .find(|&l| self.capture[l].1 == CAP_UNFINISHED)
            .ok_or(PatErr("invalid pattern capture"))
    }

    fn check_capture(&self, l: u8) -> M<usize> {
        let l = l as isize - b'1' as isize;
        if l < 0 || l as usize >= self.level || self.capture[l as usize].1 == CAP_UNFINISHED {
            return Err(PatErr("invalid capture index"));
        }
        Ok(l as usize)
    }

    fn match_capture(&self, s: usize, l: u8) -> M<Option<usize>> {
        let l = self.check_capture(l)?;
        let (init, len) = self.capture[l];
        let len = len as usize;
        if self.src.len() - s >= len && self.src[init..init + len] == self.src[s..s + len] {
            Ok(Some(s + len))
        } else {
            Ok(None)
        }
    }

    /// `push_onecapture`.
    fn capture_value(&self, vm: &mut Vm, i: usize) -> M<Value> {
        let (init, len) = self.capture[i];
        if len == CAP_UNFINISHED {
            return Err(PatErr("unfinished capture"));
        }
        Ok(if len == CAP_POSITION {
            Value::Num((init + 1) as f64)
        } else {
            vm.str(&self.src[init..init + len as usize])
        })
    }

    /// `push_captures`: the captures, or the whole match `s..e` when there are none.
    fn captures(&self, vm: &mut Vm, whole: Option<(usize, usize)>) -> M<Vec<Value>> {
        if self.level == 0 {
            if let Some((s, e)) = whole {
                return Ok(vec![vm.str(&self.src[s..e])]);
            }
        }
        (0..self.level).map(|i| self.capture_value(vm, i)).collect()
    }
}

fn pat_err(vm: &mut Vm, e: PatErr) -> LuaError {
    vm.error_at(1, e.0)
}

/// The pattern as 5.0 reads it: up to its first NUL, then a NUL.
/// The pattern as 5.0's matcher reads it, a C string: up to its first NUL, then one.
fn c_pattern(p: &[u8], out: &mut Vec<u8>) {
    out.clear();
    out.extend_from_slice(&p[..p.iter().position(|&b| b == 0).unwrap_or(p.len())]);
    out.push(0);
}

fn new_state<'a>(src: &'a [u8], pat: &'a [u8]) -> MatchState<'a> {
    MatchState {
        src,
        pat,
        level: 0,
        capture: [(0, 0); MAX_CAPTURES],
    }
}

// The pattern functions copy the subject and pattern out of the heap (a match calls back into
// the VM); the copies go into buffers the VM keeps, not fresh allocations.
fn find(vm: &mut Vm, nargs: usize) -> R<usize> {
    let mut b = vm.take_bufs::<3>();
    let r = find_in(vm, nargs, &mut b);
    vm.give_bufs(b);
    r
}

fn find_in(vm: &mut Vm, nargs: usize, [src, pat, cpat]: &mut [Vec<u8>; 3]) -> R<usize> {
    let s = vm.check_str(nargs, 1)?;
    let p = vm.check_str(nargs, 2)?;
    let l1 = vm.heap.str(s).len();
    let mut init = posrelat(vm.opt_int(nargs, 3, 1)? as i64, l1) - 1;
    if init < 0 {
        init = 0;
    } else if init as usize > l1 {
        init = l1 as i64;
    }
    let init = init as usize;
    let plain = vm.arg(nargs, 3).truthy();
    src.extend_from_slice(vm.heap.str(s));
    pat.extend_from_slice(vm.heap.str(p));
    c_pattern(pat, cpat);
    let (src, pat, cpat) = (&src[..], &pat[..], &cpat[..]);
    if plain || !cpat[..cpat.len() - 1].iter().any(|c| SPECIALS.contains(c)) {
        // `lmemfind` over the whole pattern, NULs included.
        let hay = &src[init..];
        let found = if pat.is_empty() {
            Some(0)
        } else if pat.len() > hay.len() {
            None
        } else {
            hay.windows(pat.len()).position(|w| w == pat)
        };
        if let Some(i) = found {
            vm.push(Value::Num((init + i + 1) as f64));
            vm.push(Value::Num((init + i + pat.len()) as f64));
            return Ok(2);
        }
        return Ok(vm.ret1(Value::Nil));
    }
    let (anchor, start) = if cpat[0] == b'^' {
        (true, 1)
    } else {
        (false, 0)
    };
    let mut ms = new_state(src, cpat);
    let mut s1 = init;
    loop {
        ms.level = 0;
        match ms.do_match(s1, start) {
            Err(e) => return Err(pat_err(vm, e)),
            Ok(Some(e)) => {
                let caps = match ms.captures(vm, None) {
                    Ok(c) => c,
                    Err(e) => return Err(pat_err(vm, e)),
                };
                vm.push(Value::Num((s1 + 1) as f64));
                vm.push(Value::Num(e as f64));
                for c in &caps {
                    vm.push(*c);
                }
                return Ok(2 + caps.len());
            }
            Ok(None) => {}
        }
        s1 += 1;
        if s1 > src.len() || anchor {
            break;
        }
    }
    Ok(vm.ret1(Value::Nil))
}

/// `gfind`: an iterator closure over `(s, p, position)`.
fn gfind(vm: &mut Vm, nargs: usize) -> R<usize> {
    let s = vm.check_str(nargs, 1)?;
    let p = vm.check_str(nargs, 2)?;
    let f = vm.new_closure(
        "gfind_aux",
        gfind_aux,
        vec![Value::Str(s), Value::Str(p), Value::Num(0.0)],
    );
    Ok(vm.ret1(f))
}

fn gfind_aux(vm: &mut Vm, nargs: usize) -> R<usize> {
    let mut b = vm.take_bufs::<2>();
    let r = gfind_in(vm, nargs, &mut b);
    vm.give_bufs(b);
    r
}

fn gfind_in(vm: &mut Vm, _nargs: usize, [src, cpat]: &mut [Vec<u8>; 2]) -> R<usize> {
    let (V::Str(s), V::Str(p)) = ((vm.upvalue(0)).unpack(), (vm.upvalue(1)).unpack()) else {
        return Ok(0);
    };
    let pos = vm.tonumber(vm.upvalue(2)).unwrap_or(0.0) as usize;
    src.extend_from_slice(vm.heap.str(s));
    c_pattern(vm.heap.str(p), cpat);
    let (src, cpat) = (&src[..], &cpat[..]);
    let mut ms = new_state(src, cpat);
    let mut at = pos;
    while at <= src.len() {
        ms.level = 0;
        match ms.do_match(at, 0) {
            Err(e) => return Err(pat_err(vm, e)),
            Ok(Some(e)) => {
                let newstart = if e == at { e + 1 } else { e };
                vm.set_upvalue(2, Value::Num(newstart as f64));
                let caps = match ms.captures(vm, Some((at, e))) {
                    Ok(c) => c,
                    Err(e) => return Err(pat_err(vm, e)),
                };
                for c in &caps {
                    vm.push(*c);
                }
                return Ok(caps.len());
            }
            Ok(None) => at += 1,
        }
    }
    Ok(0)
}

/// 5.0's `gsub`: a string or function replacement; a function answering neither string nor
/// number deletes the match.
fn gsub(vm: &mut Vm, nargs: usize) -> R<usize> {
    let mut b = vm.take_bufs::<4>();
    let r = gsub_in(vm, nargs, &mut b);
    vm.give_bufs(b);
    r
}

fn gsub_in(vm: &mut Vm, nargs: usize, [src, cpat, out, repl_buf]: &mut [Vec<u8>; 4]) -> R<usize> {
    let s = vm.check_str(nargs, 1)?;
    let p = vm.check_str(nargs, 2)?;
    let srcl = vm.heap.str(s).len();
    let max_s = vm.opt_int(nargs, 4, srcl as i32 + 1)?;
    let repl = vm.arg(nargs, 2);
    if nargs < 3 || !(vm.is_string(repl) || matches!(repl.unpack(), V::Func(_))) {
        return Err(vm.arg_error(3, "string or function expected"));
    }
    let repl_bytes = match repl.unpack() {
        V::Func(_) => None,
        _ => {
            let r = vm.tostr(repl).unwrap();
            repl_buf.extend_from_slice(vm.heap.str(r));
            Some(&repl_buf[..])
        }
    };
    src.extend_from_slice(vm.heap.str(s));
    c_pattern(vm.heap.str(p), cpat);
    let (src, cpat) = (&src[..], &cpat[..]);
    let (anchor, pstart) = if cpat[0] == b'^' {
        (true, 1)
    } else {
        (false, 0)
    };
    let mut ms = new_state(src, cpat);
    let mut at = 0usize;
    let mut n = 0;
    while n < max_s {
        ms.level = 0;
        let e = match ms.do_match(at, pstart) {
            Ok(e) => e,
            Err(e) => return Err(pat_err(vm, e)),
        };
        if let Some(e) = e {
            n += 1;
            // `add_s`
            match &repl_bytes {
                Some(r) => {
                    let mut i = 0;
                    while i < r.len() {
                        if r[i] != b'%' {
                            out.push(r[i]);
                        } else {
                            i += 1;
                            let c = r.get(i).copied().unwrap_or(0);
                            if !c.is_ascii_digit() {
                                out.push(c);
                            } else {
                                let v = ms.check_capture(c).and_then(|l| ms.capture_value(vm, l));
                                match v {
                                    Ok(v) => {
                                        let sv = vm.tostr(v).unwrap();
                                        out.extend_from_slice(vm.heap.str(sv));
                                    }
                                    Err(e) => return Err(pat_err(vm, e)),
                                }
                            }
                        }
                        i += 1;
                    }
                }
                None => {
                    let caps = match ms.captures(vm, Some((at, e))) {
                        Ok(c) => c,
                        Err(e) => return Err(pat_err(vm, e)),
                    };
                    let r = vm.call_value(repl, &caps, 1)?;
                    if let Some(sv) = match r[0].unpack() {
                        V::Str(_) | V::Num(_) => vm.tostr(r[0]),
                        _ => None,
                    } {
                        out.extend_from_slice(vm.heap.str(sv));
                    }
                }
            }
        }
        match e {
            Some(e) if e > at => at = e,
            _ if at < src.len() => {
                out.push(src[at]);
                at += 1;
            }
            _ => break,
        }
        if anchor {
            break;
        }
    }
    out.extend_from_slice(&src[at..]);
    let r = vm.str(out);
    vm.push(r);
    vm.push(Value::Num(n as f64));
    Ok(2)
}
