//! Numbers to and from text as the 1.12 client's C runtime does it: 5.0's `luaO_str2d` over
//! MSVC's `strtod`, and `lua_number2str`'s `%.14g` through MSVC's `printf`.

/// 5.0's `luaO_str2d`: `strtod` must read something, and only whitespace may follow. MSVC 7.1's
/// `strtod` reads `[ws][sign]digits[.digits][(e|E|d|D)[sign]digits]`; no hex, `inf` or `nan`,
/// which a C99 runtime would accept.
pub fn str2d(s: &[u8]) -> Option<f64> {
    let (v, end) = strtod(s)?;
    if s[end..].iter().all(|&b| matches!(b, 0x20 | 0x09..=0x0D)) {
        Some(v)
    } else {
        None
    }
}

/// MSVC's `strtod`: the value and the index after the last byte read, or `None` when nothing
/// numeric was read (`endptr == s`).
pub fn strtod(s: &[u8]) -> Option<(f64, usize)> {
    let mut i = 0;
    while i < s.len() && matches!(s[i], 0x20 | 0x09..=0x0D) {
        i += 1;
    }
    let mut text = String::new();
    if i < s.len() && (s[i] == b'+' || s[i] == b'-') {
        text.push(s[i] as char);
        i += 1;
    }
    let mut digits = 0;
    while i < s.len() && s[i].is_ascii_digit() {
        text.push(s[i] as char);
        i += 1;
        digits += 1;
    }
    if i < s.len() && s[i] == b'.' {
        text.push('.');
        i += 1;
        while i < s.len() && s[i].is_ascii_digit() {
            text.push(s[i] as char);
            i += 1;
            digits += 1;
        }
    }
    if digits == 0 {
        return None;
    }
    // An exponent counts only with at least one digit; otherwise `endptr` stays before it.
    if i < s.len() && matches!(s[i], b'e' | b'E' | b'd' | b'D') {
        let mut j = i + 1;
        let mut exp = String::from("e");
        if j < s.len() && (s[j] == b'+' || s[j] == b'-') {
            exp.push(s[j] as char);
            j += 1;
        }
        let at = j;
        while j < s.len() && s[j].is_ascii_digit() {
            exp.push(s[j] as char);
            j += 1;
        }
        if j > at {
            text.push_str(&exp);
            i = j;
        }
    }
    // `1.` and `.5` both parse in Rust once the dangling dot is given a digit.
    if text.ends_with('.') {
        text.push('0');
    }
    let v: f64 = text.parse().ok()?;
    Some((v, i))
}

/// `%.14g` as MSVC's `printf` writes it: a three-digit exponent (`1e+020`), and `1.#INF`,
/// `-1.#INF` and `-1.#IND` for the non-finite values.
pub fn fmt_g14(n: f64) -> String {
    fmt_g(n, 14)
}

/// `%.<prec>g` in MSVC's spelling.
pub fn fmt_g(n: f64, prec: usize) -> String {
    if n.is_nan() {
        // x87's default NaN has the sign bit set and prints as the indefinite.
        return if n.is_sign_negative() {
            "-1.#IND".into()
        } else {
            "1.#QNAN".into()
        };
    }
    if n.is_infinite() {
        return if n < 0.0 {
            "-1.#INF".into()
        } else {
            "1.#INF".into()
        };
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
        format!("{mant}e{sign}{:03}", x.abs())
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
    fn g14_matches_msvc() {
        assert_eq!(fmt_g14(1.0), "1");
        assert_eq!(fmt_g14(0.1), "0.1");
        assert_eq!(fmt_g14(1e20), "1e+020");
        assert_eq!(fmt_g14(1e-5), "1e-005");
        assert_eq!(fmt_g14(123456789012345.0), "1.2345678901234e+014");
        assert_eq!(fmt_g14(12345678901234.0), "12345678901234");
        assert_eq!(fmt_g14(-2.5), "-2.5");
        assert_eq!(fmt_g14(1.0 / 3.0), "0.33333333333333");
        assert_eq!(fmt_g14(f64::INFINITY), "1.#INF");
        assert_eq!(fmt_g14(0.0001), "0.0001");
    }

    #[test]
    fn str2d_is_msvc_strtod() {
        assert_eq!(str2d(b"10"), Some(10.0));
        assert_eq!(str2d(b"  1.5e3  "), Some(1500.0));
        assert_eq!(str2d(b".5"), Some(0.5));
        assert_eq!(str2d(b"5."), Some(5.0));
        assert_eq!(str2d(b"0x10"), None, "MSVC 7.1 reads no hex");
        assert_eq!(str2d(b"inf"), None);
        assert_eq!(str2d(b"1e"), None, "`e` without digits is trailing text");
        assert_eq!(str2d(b""), None);
        assert_eq!(str2d(b"-"), None);
    }
}
