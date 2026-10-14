//! Numbers to and from text as benilla's Lua 5.1 build read and wrote them over glibc: 5.1's
//! `luaO_str2d` over `strtod`, `tonumber`'s `strtoul` with a 64-bit `unsigned long`, and
//! `lua_number2str`'s `%.14g`.

/// C's `isspace`.
fn is_space(b: u8) -> bool {
    matches!(b, 0x20 | 0x09..=0x0D)
}

/// 5.1's `luaO_str2d`: `strtod` must read something; a stop at an `x` is retried as hex through
/// `strtoul`; only whitespace may follow.
pub fn str2d(s: &[u8]) -> Option<f64> {
    let (mut v, mut end) = strtod(s)?;
    if matches!(s.get(end), Some(b'x' | b'X')) {
        let (n, e) = strtoul(s, 16);
        v = n as f64;
        end = e;
    }
    s[end..].iter().all(|&b| is_space(b)).then_some(v)
}

fn starts_ci(s: &[u8], word: &[u8]) -> bool {
    s.len() >= word.len() && s[..word.len()].eq_ignore_ascii_case(word)
}

/// glibc's `strtod`: the value and the index after the last byte read, or `None` when nothing
/// numeric was read (`endptr == s`). Reads `inf`, `infinity`, `nan[(...)]`, hex with an optional
/// binary exponent, and decimal with an exponent that counts only with a digit.
pub fn strtod(s: &[u8]) -> Option<(f64, usize)> {
    let mut i = 0;
    while i < s.len() && is_space(s[i]) {
        i += 1;
    }
    let neg = matches!(s.get(i), Some(b'-'));
    if matches!(s.get(i), Some(b'+' | b'-')) {
        i += 1;
    }
    let sign = |v: f64| if neg { -v } else { v };
    let rest = &s[i..];
    if starts_ci(rest, b"infinity") {
        return Some((sign(f64::INFINITY), i + 8));
    }
    if starts_ci(rest, b"inf") {
        return Some((sign(f64::INFINITY), i + 3));
    }
    if starts_ci(rest, b"nan") {
        let mut j = i + 3;
        if s.get(j) == Some(&b'(') {
            let mut k = j + 1;
            while k < s.len() && (s[k].is_ascii_alphanumeric() || s[k] == b'_') {
                k += 1;
            }
            if s.get(k) == Some(&b')') {
                j = k + 1;
            }
        }
        return Some((sign(f64::NAN), j));
    }
    let hexd = |b: Option<&u8>| b.is_some_and(|b| b.is_ascii_hexdigit());
    if rest.first() == Some(&b'0')
        && matches!(rest.get(1), Some(b'x' | b'X'))
        && (hexd(rest.get(2)) || (rest.get(2) == Some(&b'.') && hexd(rest.get(3))))
    {
        let (v, n) = hex_float(&rest[2..]);
        return Some((sign(v), i + 2 + n));
    }
    let start = i;
    let mut j = i;
    let mut digits = 0;
    while j < s.len() && s[j].is_ascii_digit() {
        j += 1;
        digits += 1;
    }
    if j < s.len() && s[j] == b'.' {
        j += 1;
        while j < s.len() && s[j].is_ascii_digit() {
            j += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return None;
    }
    let mut end = j;
    if matches!(s.get(j), Some(b'e' | b'E')) {
        let mut k = j + 1;
        if matches!(s.get(k), Some(b'+' | b'-')) {
            k += 1;
        }
        let at = k;
        while k < s.len() && s[k].is_ascii_digit() {
            k += 1;
        }
        if k > at {
            end = k;
        }
    }
    let mut text = String::from_utf8_lossy(&s[start..end]).into_owned();
    // `1.` and `.5` parse in Rust once the dangling dot has a digit on each side.
    if text.starts_with('.') {
        text.insert(0, '0');
    }
    text = text.replacen(".e", ".0e", 1).replacen(".E", ".0E", 1);
    if text.ends_with('.') {
        text.push('0');
    }
    let v: f64 = text.parse().ok()?;
    Some((sign(v), end))
}

/// The digits after `0x`: `h[.h][p[+-]d]`, the value and the bytes read.
fn hex_float(s: &[u8]) -> (f64, usize) {
    let mut m: u128 = 0;
    let mut exp: i64 = 0;
    let mut sticky = false;
    let mut i = 0;
    let mut seen_point = false;
    while i < s.len() {
        let c = s[i];
        if c == b'.' && !seen_point {
            seen_point = true;
        } else if let Some(d) = (c as char).to_digit(16) {
            if m >> 120 == 0 {
                m = m << 4 | d as u128;
                if seen_point {
                    exp -= 4;
                }
            } else {
                // Past 120 bits a digit only rounds: kept as a sticky bit.
                sticky |= d != 0;
                if !seen_point {
                    exp += 4;
                }
            }
        } else {
            break;
        }
        i += 1;
    }
    if matches!(s.get(i), Some(b'p' | b'P')) {
        let mut k = i + 1;
        let neg = s.get(k) == Some(&b'-');
        if matches!(s.get(k), Some(b'+' | b'-')) {
            k += 1;
        }
        let at = k;
        let mut e: i64 = 0;
        while k < s.len() && s[k].is_ascii_digit() {
            e = (e * 10 + (s[k] - b'0') as i64).min(1 << 20);
            k += 1;
        }
        if k > at {
            exp += if neg { -e } else { e };
            i = k;
        }
    }
    let m = m | sticky as u128;
    let v = m as f64 * 2f64.powi(exp.clamp(-2200, 2200) as i32);
    (v, i)
}

/// glibc's `strtoul` with a 64-bit `unsigned long`: the value and the index after the last digit,
/// that index `0` when no digit was read. A `-` negates modulo 2^64; an overflow is `u64::MAX`.
pub fn strtoul(s: &[u8], base: u32) -> (u64, usize) {
    let mut i = 0;
    while i < s.len() && is_space(s[i]) {
        i += 1;
    }
    let neg = matches!(s.get(i), Some(b'-'));
    if matches!(s.get(i), Some(b'+' | b'-')) {
        i += 1;
    }
    if base == 16
        && s.get(i) == Some(&b'0')
        && matches!(s.get(i + 1), Some(b'x' | b'X'))
        && s.get(i + 2).is_some_and(|b| b.is_ascii_hexdigit())
    {
        i += 2;
    }
    let at = i;
    let mut n: u64 = 0;
    let mut over = false;
    while let Some(d) = s.get(i).and_then(|&c| (c as char).to_digit(base)) {
        match n
            .checked_mul(base as u64)
            .and_then(|x| x.checked_add(d as u64))
        {
            Some(x) => n = x,
            None => over = true,
        }
        i += 1;
    }
    if i == at {
        return (0, 0);
    }
    let n = if over {
        u64::MAX
    } else if neg {
        n.wrapping_neg()
    } else {
        n
    };
    (n, i)
}

/// `%.14g` as glibc's `printf` writes it: a three-digit exponent (`1e+020`), and `1.#INF`,
/// `-1.#INF` and `-1.#IND` for the non-finite values.
pub fn fmt_g14(n: f64) -> String {
    fmt_g(n, 14)
}

/// `10^p`, exact for the precisions `%g` is asked for.
fn pow10(p: usize) -> f64 {
    const P: [f64; 18] = [
        1e0, 1e1, 1e2, 1e3, 1e4, 1e5, 1e6, 1e7, 1e8, 1e9, 1e10, 1e11, 1e12, 1e13, 1e14, 1e15, 1e16,
        1e17,
    ];
    P.get(p).copied().unwrap_or_else(|| 10f64.powi(p as i32))
}

/// `fmt_g14(n)` appended to `out`; an integer is written without a temporary string.
pub fn push_g14(n: f64, out: &mut Vec<u8>) {
    if n.fract() == 0.0 && n.abs() < 1e14 && n != 0.0 {
        let mut v = n as i64;
        if v < 0 {
            out.push(b'-');
            v = -v;
        }
        let mut digits = [0u8; 20];
        let mut i = digits.len();
        while v > 0 {
            i -= 1;
            digits[i] = b'0' + (v % 10) as u8;
            v /= 10;
        }
        out.extend_from_slice(&digits[i..]);
    } else {
        out.extend_from_slice(fmt_g14(n).as_bytes());
    }
}

/// `%.<prec>g` in glibc's spelling: a two-digit exponent at least, `inf` and `nan`.
pub fn fmt_g(n: f64, prec: usize) -> String {
    // An integer below `10^prec` prints as its digits: the common case, without float
    // formatting's exact-decimal arithmetic.
    if n.fract() == 0.0 && n.abs() < pow10(prec) && n != 0.0 {
        return (n as i64).to_string();
    }
    if n.is_nan() {
        // The sign bit prints: x86's default NaN, from `0/0`, has it set.
        return if n.is_sign_negative() {
            "-nan".into()
        } else {
            "nan".into()
        };
    }
    if n.is_infinite() {
        return if n < 0.0 { "-inf".into() } else { "inf".into() };
    }
    let p = prec.max(1);
    if n == 0.0 {
        return if n.is_sign_negative() {
            "-0".into()
        } else {
            "0".into()
        };
    }
    // The exponent after rounding to `p` significant digits decides the style.
    let sci = format!("{:.*e}", p - 1, n);
    let (mant, exp) = sci.split_once('e').unwrap();
    let x: i32 = exp.parse().unwrap();
    if x < -4 || x >= p as i32 {
        let mant = strip_zeros(mant);
        let sign = if x < 0 { '-' } else { '+' };
        format!("{mant}e{sign}{:02}", x.abs())
    } else {
        let decimals = (p as i32 - 1 - x).max(0) as usize;
        strip_zeros(&format!("{:.*}", decimals, n)).to_string()
    }
}

fn strip_zeros(s: &str) -> &str {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.')
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn g14_matches_glibc() {
        assert_eq!(fmt_g14(1.0), "1");
        assert_eq!(fmt_g14(0.1), "0.1");
        assert_eq!(fmt_g14(1e20), "1e+20");
        assert_eq!(fmt_g14(1e-5), "1e-05");
        assert_eq!(fmt_g14(1e100), "1e+100");
        assert_eq!(fmt_g14(123456789012345.0), "1.2345678901234e+14");
        assert_eq!(fmt_g14(12345678901234.0), "12345678901234");
        assert_eq!(fmt_g14(-2.5), "-2.5");
        for n in [
            1.0,
            -7.0,
            42.0,
            99999999999999.0,
            -12345678901234.0,
            0.0,
            -0.0,
            0.5,
            1e14,
            1e20,
        ] {
            let mut out = Vec::new();
            push_g14(n, &mut out);
            assert_eq!(out, fmt_g14(n).into_bytes(), "{n}");
        }
        assert_eq!(fmt_g14(1.0 / 3.0), "0.33333333333333");
        assert_eq!(fmt_g14(f64::INFINITY), "inf");
        assert_eq!(fmt_g14(f64::NEG_INFINITY), "-inf");
        assert_eq!(fmt_g14(-f64::NAN), "-nan");
        assert_eq!(fmt_g14(0.0001), "0.0001");
    }

    #[test]
    fn str2d_is_glibc_strtod() {
        assert_eq!(str2d(b"10"), Some(10.0));
        assert_eq!(str2d(b"  1.5e3  "), Some(1500.0));
        assert_eq!(str2d(b".5"), Some(0.5));
        assert_eq!(str2d(b"5."), Some(5.0));
        assert_eq!(str2d(b"0x10"), Some(16.0));
        assert_eq!(str2d(b" 0x1A "), Some(26.0));
        assert_eq!(str2d(b"0x1p4"), Some(16.0));
        assert_eq!(str2d(b"0x.8"), Some(0.5));
        assert_eq!(str2d(b"inf"), Some(f64::INFINITY));
        assert_eq!(str2d(b"-Infinity"), Some(f64::NEG_INFINITY));
        assert!(str2d(b"nan").unwrap().is_nan());
        assert_eq!(
            str2d(b"1e"),
            None,
            "an exponent needs a digit, and then `e` trails"
        );
        assert_eq!(str2d(b"0x"), None);
        assert_eq!(str2d(b"1d5"), None, "glibc reads no `d` exponent");
        assert_eq!(str2d(b"- 1"), None);
        assert_eq!(str2d(b""), None);
        assert_eq!(str2d(b"1e400"), Some(f64::INFINITY));
    }

    #[test]
    fn strtoul_is_glibc_64() {
        assert_eq!(strtoul(b"ff", 16), (255, 2));
        assert_eq!(strtoul(b"0x10", 16), (16, 4));
        assert_eq!(strtoul(b"-1", 16), (u64::MAX, 2));
        assert_eq!(strtoul(b"ffffffffffffffffffff", 16).0, u64::MAX);
        assert_eq!(strtoul(b"8", 8), (0, 0));
        assert_eq!(strtoul(b"  +7", 8), (7, 4));
    }
}
