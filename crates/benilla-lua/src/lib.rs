//! benilla-lua: an experimental interpreter for the 1.12.1 client's Lua, which is Lua 5.0 with the
//! client's own library set. The state is plain Rust data, so it can be cloned, inspected and
//! restored; that is the experiment. Not wired into the client.

pub mod api;
pub mod ast;
pub mod cfmt;
pub mod compile;
pub mod heap;
pub mod lex;
pub mod lib_base;
pub mod lib_math;
pub mod lib_string;
pub mod lib_table;
pub mod number;
pub mod parse;
pub mod proto;
pub mod reload;
pub mod table;
pub mod value;
pub mod vm;

pub use lex::SyntaxError;
pub use value::Value;
pub use vm::{LuaError, Vm};

/// A VM with 1.12's Lua libraries open: base, `string`, `table`, `math` and `bit`, then the
/// bare compatibility globals the client defines over them.
pub fn new_state() -> Vm {
    let mut vm = Vm::new();
    lib_base::open(&mut vm);
    lib_string::open(&mut vm);
    lib_table::open(&mut vm);
    lib_math::open(&mut vm);
    vm.exec(COMPAT, b"=[benilla-lua compat]")
        .expect("the compat chunk runs");
    vm
}

/// The bare globals the client's start-up chunk publishes (its `.rdata` at `0x8722e8`): aliases
/// of the table, math and string libraries, and the trigonometry in degrees. Written here, not
/// copied; Lua closures as in the client, so a bad argument names the inner call.
const COMPAT: &[u8] = br#"
local t, m, s = table, math, string
foreach, foreachi, getn = t.foreach, t.foreachi, t.getn
tinsert, tremove, sort = t.insert, t.remove, t.sort
abs, ceil, floor, deg, rad = m.abs, m.ceil, m.floor, m.deg, m.rad
exp, log, log10, frexp, ldexp = m.exp, m.log, m.log10, m.frexp, m.ldexp
max, min, mod, sqrt, PI = m.max, m.min, m.mod, m.sqrt, m.pi
random, randomseed = m.random, m.randomseed
function sin(x) return m.sin(m.rad(x)) end
function cos(x) return m.cos(m.rad(x)) end
function tan(x) return m.tan(m.rad(x)) end
function asin(x) return m.deg(m.asin(x)) end
function acos(x) return m.deg(m.acos(x)) end
function atan(x) return m.deg(m.atan(x)) end
function atan2(y, x) return m.deg(m.atan2(y, x)) end
strbyte, strchar, strfind, format, gsub = s.byte, s.char, s.find, s.format, s.gsub
strlen, strlower, strrep, strsub, strupper = s.len, s.lower, s.rep, s.sub, s.upper
"#;

/// `luaO_chunkid`: a chunk name as a message shows it, in a buffer of `bufflen` bytes. 5.0 uses
/// 80 (`MAXSRC`) for compile errors and 60 (`LUA_IDSIZE`) for runtime ones. `=name` is verbatim,
/// `@file` keeps the file's tail, and anything else is `[string "first line..."]`.
pub fn chunkid_n(source: &[u8], bufflen: usize) -> Vec<u8> {
    // A C string: the name ends at its first NUL.
    let source = &source[..source.iter().position(|&b| b == 0).unwrap_or(source.len())];
    match source.first() {
        Some(b'=') => {
            let s = &source[1..];
            s[..s.len().min(bufflen - 1)].to_vec()
        }
        Some(b'@') => {
            let s = &source[1..];
            let room = bufflen - b" `...' ".len() - 1;
            if s.len() > room {
                let mut out = b"...".to_vec();
                out.extend_from_slice(&s[s.len() - room..]);
                out
            } else {
                s.to_vec()
            }
        }
        _ => {
            let room = bufflen - b" [string \"...\"] ".len() - 1;
            let line = source
                .iter()
                .position(|&b| b == b'\n')
                .unwrap_or(source.len());
            let len = line.min(room);
            let mut out = b"[string \"".to_vec();
            if len < source.len() {
                out.extend_from_slice(&source[..len]);
                out.extend_from_slice(b"...");
            } else {
                out.extend_from_slice(source);
            }
            out.extend_from_slice(b"\"]");
            out
        }
    }
}

/// [`chunkid_n`] at the compiler's 80 bytes.
pub fn chunkid(source: &[u8]) -> Vec<u8> {
    chunkid_n(source, 80)
}

/// Parse a chunk, or answer the client's own syntax error.
pub fn parse(src: &[u8], chunkname: &[u8]) -> Result<ast::Func, SyntaxError> {
    parse::parse(src, chunkname)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn err(src: &str) -> String {
        parse(src.as_bytes(), b"=t").unwrap_err().to_string()
    }

    /// The client's `.rdata` strings: 5.0's lexer and parser, not 5.1's.
    #[test]
    fn syntax_errors_are_5_0_s() {
        assert_eq!(err("x = 1 +"), "t:1: unexpected symbol near `<eof>'");
        assert_eq!(err("local function"), "t:1: <name> expected near `<eof>'");
        assert_eq!(err("x = #t"), "t:1: unexpected symbol near `#'");
        // `x = a` ends; `%` then opens a statement.
        assert_eq!(err("x = a % b"), "t:1: unexpected symbol near `%'");
        assert_eq!(
            err("return function(...) return ... end"),
            "t:1: unexpected symbol near `...'"
        );
        assert_eq!(err("for x do end"), "t:1: `=' or `in' expected near `do'");
        assert_eq!(
            err("x = 1..2"),
            "t:1: ambiguous syntax (decimal point x string concatenation) near `1..'"
        );
        assert_eq!(err("x = \"a\nb\""), "t:1: unfinished string near `\"a'");
        assert_eq!(err("x = [[a"), "t:1: unfinished long string near `<eof>'");
        assert_eq!(err("x = 1\x01"), "t:1: invalid control char near `char(1)'");
        assert_eq!(
            err("f()\n(g)()"),
            "t:2: ambiguous syntax (function call x new statement) near `('"
        );
        assert_eq!(err("break"), "t:1: no loop to break near `<eof>'");
        assert_eq!(err("(a) = 1"), "t:1: syntax error near `='");
        assert_eq!(err("if x then\nend end"), "t:2: <eof> expected near `end'");
        assert_eq!(
            err("do\nx = 1"),
            "t:2: `end' expected (to close `do' at line 1) near `<eof>'"
        );
    }

    /// 5.0's lexer: `\r` is whitespace, `[[` nests, `[=[` does not exist, a numeral stops at its
    /// first non-numeric byte.
    #[test]
    fn the_lexer_is_5_0_s() {
        let one = |src: &[u8]| -> Vec<u8> {
            let f = parse(src, b"=t").unwrap();
            match &f.body.stats[0] {
                ast::Stat::Return { exprs } => match &exprs[0] {
                    ast::Expr::Str(s) => s.to_vec(),
                    e => panic!("{e:?}"),
                },
                s => panic!("{s:?}"),
            }
        };
        assert_eq!(one(b"return [[a\r\nb]]"), b"a\r\nb", "5.0 keeps the \\r");
        assert_eq!(one(b"return [[a [[b]] c]]"), b"a [[b]] c");
        assert_eq!(
            one(b"return 'a\rb'"),
            b"a\rb",
            "\\r does not end a short string"
        );
        assert!(parse(b"return [=[x]=]", b"=t").is_err());
        // `0x10` is `0` then the statement `x10`.
        assert_eq!(err("x = 0x10"), "t:1: `=' expected near `<eof>'");
        // A `\r`-only file is one line: line numbers count `\n` alone.
        assert_eq!(err("x = 1\ry = +"), "t:1: unexpected symbol near `+'");
    }

    #[test]
    fn chunk_names_are_5_0_s() {
        assert_eq!(chunkid(b"=name"), b"name");
        assert_eq!(chunkid(b"@a/b.lua"), b"a/b.lua");
        assert_eq!(chunkid(b"x = 1\ny = 2"), b"[string \"x = 1...\"]");
        assert_eq!(chunkid(b"x = 1"), b"[string \"x = 1\"]");
        let long = [b'a'; 100];
        assert_eq!(chunkid_n(&long, 60).len(), 9 + 43 + 3 + 2);
    }
}
