//! The lexer: Lua 5.0's `llex.c`, which the 1.12 client embeds. Its `.rdata` carries 5.0's token
//! names (`*name`, `*number`, `*string`) and 5.0-only messages (`invalid control char`,
//! `ambiguous syntax (decimal point x string concatenation)`), and lacks 5.1's
//! `invalid long string delimiter`. So, unlike 5.1: `\n` alone ends a line (`\r` is whitespace and
//! stays inside a literal), long brackets are `[[` only and nest, a numeral stops at its first
//! non-numeric byte, and a control byte outside a literal is an error. Source is bytes.

use crate::chunkid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tok {
    // Reserved words, in `token2string` order.
    And,
    Break,
    Do,
    Else,
    Elseif,
    End,
    False,
    For,
    Function,
    If,
    In,
    Local,
    Nil,
    Not,
    Or,
    Repeat,
    Return,
    Then,
    True,
    Until,
    While,
    Name,
    Concat,
    Dots,
    Eq,
    Ge,
    Le,
    Ne,
    Number,
    String,
    Eof,
    /// A single-character token, the byte itself.
    Char(u8),
}

const RESERVED: [(&[u8], Tok); 21] = [
    (b"and", Tok::And),
    (b"break", Tok::Break),
    (b"do", Tok::Do),
    (b"else", Tok::Else),
    (b"elseif", Tok::Elseif),
    (b"end", Tok::End),
    (b"false", Tok::False),
    (b"for", Tok::For),
    (b"function", Tok::Function),
    (b"if", Tok::If),
    (b"in", Tok::In),
    (b"local", Tok::Local),
    (b"nil", Tok::Nil),
    (b"not", Tok::Not),
    (b"or", Tok::Or),
    (b"repeat", Tok::Repeat),
    (b"return", Tok::Return),
    (b"then", Tok::Then),
    (b"true", Tok::True),
    (b"until", Tok::Until),
    (b"while", Tok::While),
];

impl Tok {
    /// `luaX_token2str`: a token as an error message names it.
    pub fn text(self) -> Vec<u8> {
        let s: &[u8] = match self {
            Tok::Name => b"*name",
            Tok::Concat => b"..",
            Tok::Dots => b"...",
            Tok::Eq => b"==",
            Tok::Ge => b">=",
            Tok::Le => b"<=",
            Tok::Ne => b"~=",
            Tok::Number => b"*number",
            Tok::String => b"*string",
            Tok::Eof => b"<eof>",
            // `%c`: the byte as it is.
            Tok::Char(c) => return vec![c],
            word => RESERVED.iter().find(|r| r.1 == word).unwrap().0,
        };
        s.to_vec()
    }
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    /// A name's or string's bytes; empty otherwise.
    pub bytes: Vec<u8>,
    pub num: f64,
}

/// A compile error, rendered as 5.0's `luaX_errorline` does: `chunkid:line: msg near `tok'`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxError(pub Vec<u8>);

impl std::fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&String::from_utf8_lossy(&self.0))
    }
}

pub struct Lexer<'a> {
    src: &'a [u8],
    pos: usize,
    pub line: u32,
    /// The line of the last token consumed (`lastline`).
    pub lastline: u32,
    chunk: Vec<u8>,
    pub cur: Token,
    ahead: Option<Token>,
    /// 5.0's `buff`: the text of the last string or numeral read, which `luaX_syntaxerror` quotes
    /// for a current string or number token, even when the lookahead has since overwritten it.
    buff: Vec<u8>,
}

const EOZ: i32 = -1;

/// C's `isspace` in the "C" locale.
fn is_space(c: i32) -> bool {
    matches!(c, 0x20 | 0x09..=0x0D)
}

fn is_digit(c: i32) -> bool {
    (b'0' as i32..=b'9' as i32).contains(&c)
}

fn is_alpha(c: i32) -> bool {
    (0..256).contains(&c) && (c as u8).is_ascii_alphabetic()
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a [u8], chunkname: &[u8]) -> Result<Self, SyntaxError> {
        let mut lx = Lexer {
            src,
            pos: 0,
            line: 1,
            lastline: 1,
            chunk: chunkid(chunkname),
            cur: Token {
                tok: Tok::Eof,
                bytes: Vec::new(),
                num: 0.0,
            },
            ahead: None,
            buff: Vec::new(),
        };
        lx.advance()?;
        Ok(lx)
    }

    fn ch(&self) -> i32 {
        self.src.get(self.pos).map_or(EOZ, |&c| c as i32)
    }

    pub fn advance(&mut self) -> Result<(), SyntaxError> {
        self.lastline = self.line;
        self.cur = match self.ahead.take() {
            Some(t) => t,
            None => self.lex()?,
        };
        Ok(())
    }

    /// `luaX_lookahead`: the token after the current one, without consuming it.
    pub fn lookahead(&mut self) -> Result<Tok, SyntaxError> {
        if self.ahead.is_none() {
            self.ahead = Some(self.lex()?);
        }
        Ok(self.ahead.as_ref().unwrap().tok)
    }

    fn error_at(&self, msg: &[u8], near: &[u8]) -> SyntaxError {
        let mut out = self.chunk.clone();
        out.extend_from_slice(format!(":{}: ", self.line).as_bytes());
        out.extend_from_slice(msg);
        out.extend_from_slice(b" near `");
        out.extend_from_slice(near);
        out.push(b'\'');
        SyntaxError(out)
    }

    /// `luaX_syntaxerror` at the current token.
    pub fn error(&self, msg: &str) -> SyntaxError {
        let near = match self.cur.tok {
            Tok::Name => self.cur.bytes.clone(),
            Tok::String | Tok::Number => self.buff.clone(),
            t => t.text(),
        };
        self.error_at(msg.as_bytes(), &near)
    }

    /// `luaX_lexerror`: an end of input names `<eof>`, anything else quotes the buffer.
    fn lex_error(&self, msg: &str, eof: bool) -> SyntaxError {
        if eof {
            self.error_at(msg.as_bytes(), b"<eof>")
        } else {
            self.error_at(msg.as_bytes(), &self.buff)
        }
    }

    /// `inclinenumber`: at a `\n`, which alone ends a line in 5.0.
    fn newline(&mut self) -> Result<(), SyntaxError> {
        self.pos += 1;
        self.line += 1;
        // `checklimit(LS, ++LS->linenumber, MAX_INT, "lines in a chunk")` never trips here.
        Ok(())
    }

    fn tok(tok: Tok) -> Token {
        Token {
            tok,
            bytes: Vec::new(),
            num: 0.0,
        }
    }

    fn lex(&mut self) -> Result<Token, SyntaxError> {
        loop {
            let c = self.ch();
            match c {
                EOZ => return Ok(Self::tok(Tok::Eof)),
                0x0A => self.newline()?,
                0x2D /* - */ => {
                    self.pos += 1;
                    if self.ch() != b'-' as i32 {
                        return Ok(Self::tok(Tok::Char(b'-')));
                    }
                    self.pos += 1;
                    // `if (current == '[' && (next(LS), current == '['))`: the first `[` is
                    // consumed either way.
                    if self.ch() == b'[' as i32 && {
                        self.pos += 1;
                        self.ch() == b'[' as i32
                    } {
                        self.long_string(false)?;
                    } else {
                        while self.ch() != b'\n' as i32 && self.ch() != EOZ {
                            self.pos += 1;
                        }
                    }
                }
                0x5B /* [ */ => {
                    self.pos += 1;
                    if self.ch() != b'[' as i32 {
                        return Ok(Self::tok(Tok::Char(b'[')));
                    }
                    let bytes = self.long_string(true)?;
                    let mut t = Self::tok(Tok::String);
                    t.bytes = bytes;
                    return Ok(t);
                }
                0x3D /* = */ => return Ok(self.two(Tok::Eq, b'=')),
                0x3C /* < */ => return Ok(self.two(Tok::Le, b'<')),
                0x3E /* > */ => return Ok(self.two(Tok::Ge, b'>')),
                0x7E /* ~ */ => return Ok(self.two(Tok::Ne, b'~')),
                0x22 | 0x27 => return self.string(c as u8),
                0x2E /* . */ => {
                    self.pos += 1;
                    if self.ch() == b'.' as i32 {
                        self.pos += 1;
                        if self.ch() == b'.' as i32 {
                            self.pos += 1;
                            return Ok(Self::tok(Tok::Dots));
                        }
                        return Ok(Self::tok(Tok::Concat));
                    }
                    if !is_digit(self.ch()) {
                        return Ok(Self::tok(Tok::Char(b'.')));
                    }
                    return self.numeral(true);
                }
                _ => {
                    if is_space(c) {
                        self.pos += 1;
                    } else if is_digit(c) {
                        return self.numeral(false);
                    } else if is_alpha(c) || c == b'_' as i32 {
                        let start = self.pos;
                        while is_alpha(self.ch()) || is_digit(self.ch()) || self.ch() == b'_' as i32
                        {
                            self.pos += 1;
                        }
                        let word = &self.src[start..self.pos];
                        if let Some(r) = RESERVED.iter().find(|r| r.0 == word) {
                            return Ok(Self::tok(r.1));
                        }
                        let mut t = Self::tok(Tok::Name);
                        t.bytes = word.to_vec();
                        return Ok(t);
                    } else {
                        // C's `iscntrl`: 0-31 and 127.
                        if c < 32 || c == 127 {
                            return Err(self.error_at(
                                b"invalid control char",
                                format!("char({c})").as_bytes(),
                            ));
                        }
                        self.pos += 1;
                        return Ok(Self::tok(Tok::Char(c as u8)));
                    }
                }
            }
        }
    }

    fn two(&mut self, tok: Tok, single: u8) -> Token {
        self.pos += 1;
        if self.ch() == b'=' as i32 {
            self.pos += 1;
            Self::tok(tok)
        } else {
            Self::tok(Tok::Char(single))
        }
    }

    /// 5.0's `read_long_string`, at the second `[`. `[[` nests and only the matching `]]`
    /// closes; the buffer keeps both brackets, which an error quotes.
    fn long_string(&mut self, keep: bool) -> Result<Vec<u8>, SyntaxError> {
        let mut cont = 0u32;
        self.buff.clear();
        self.buff.extend_from_slice(b"[[");
        self.pos += 1;
        if self.ch() == b'\n' as i32 {
            self.newline()?;
        }
        loop {
            match self.ch() {
                EOZ => {
                    let msg = if keep {
                        "unfinished long string"
                    } else {
                        "unfinished long comment"
                    };
                    return Err(self.lex_error(msg, true));
                }
                0x5B => {
                    self.buff.push(b'[');
                    self.pos += 1;
                    if self.ch() == b'[' as i32 {
                        cont += 1;
                        self.buff.push(b'[');
                        self.pos += 1;
                    }
                }
                0x5D => {
                    self.buff.push(b']');
                    self.pos += 1;
                    if self.ch() == b']' as i32 {
                        if cont == 0 {
                            break;
                        }
                        cont -= 1;
                        self.buff.push(b']');
                        self.pos += 1;
                    }
                }
                0x0A => {
                    self.buff.push(b'\n');
                    self.newline()?;
                    if !keep {
                        self.buff.clear();
                    }
                }
                c => {
                    self.buff.push(c as u8);
                    self.pos += 1;
                }
            }
        }
        self.buff.push(b']');
        self.pos += 1;
        // `luaZ_buffer + 2, l - 5` with `l` counting the terminator: the text between the brackets.
        // A comment's buffer was reset at each newline and holds no string.
        if !keep {
            return Ok(Vec::new());
        }
        Ok(self.buff[2..self.buff.len() - 2].to_vec())
    }

    /// 5.0's `read_string`: `\n` ends it unfinished, `\r` is kept, `\ddd` is up to three digits.
    fn string(&mut self, delim: u8) -> Result<Token, SyntaxError> {
        self.buff.clear();
        self.buff.push(delim);
        self.pos += 1;
        while self.ch() != delim as i32 {
            match self.ch() {
                EOZ => return Err(self.lex_error("unfinished string", true)),
                0x0A => return Err(self.lex_error("unfinished string", false)),
                0x5C => {
                    self.pos += 1;
                    let e = self.ch();
                    let simple = match e {
                        0x61 => Some(7),
                        0x62 => Some(8),
                        0x66 => Some(12),
                        0x6E => Some(b'\n'),
                        0x72 => Some(b'\r'),
                        0x74 => Some(b'\t'),
                        0x76 => Some(11),
                        _ => None,
                    };
                    if let Some(b) = simple {
                        self.buff.push(b);
                        self.pos += 1;
                    } else if e == 0x0A {
                        self.buff.push(b'\n');
                        self.newline()?;
                    } else if e == EOZ {
                        // Raised as unfinished on the next pass.
                    } else if !is_digit(e) {
                        self.buff.push(e as u8);
                        self.pos += 1;
                    } else {
                        let mut v = 0i32;
                        let mut i = 0;
                        loop {
                            v = 10 * v + (self.ch() - b'0' as i32);
                            self.pos += 1;
                            i += 1;
                            if i >= 3 || !is_digit(self.ch()) {
                                break;
                            }
                        }
                        if v > 255 {
                            return Err(self.lex_error("escape sequence too large", false));
                        }
                        self.buff.push(v as u8);
                    }
                }
                c => {
                    self.buff.push(c as u8);
                    self.pos += 1;
                }
            }
        }
        self.buff.push(delim);
        self.pos += 1;
        let mut t = Self::tok(Tok::String);
        t.bytes = self.buff[1..self.buff.len() - 1].to_vec();
        Ok(t)
    }

    /// 5.0's `read_numeral`: digits, one `.`, digits, an exponent; it stops at anything else, so
    /// `0x10` is the number `0` then the name `x10`.
    fn numeral(&mut self, comma: bool) -> Result<Token, SyntaxError> {
        self.buff.clear();
        if comma {
            self.buff.push(b'.');
        }
        while is_digit(self.ch()) {
            self.buff.push(self.ch() as u8);
            self.pos += 1;
        }
        if self.ch() == b'.' as i32 {
            self.buff.push(b'.');
            self.pos += 1;
            if self.ch() == b'.' as i32 {
                self.buff.push(b'.');
                self.pos += 1;
                return Err(self.lex_error(
                    "ambiguous syntax (decimal point x string concatenation)",
                    false,
                ));
            }
        }
        while is_digit(self.ch()) {
            self.buff.push(self.ch() as u8);
            self.pos += 1;
        }
        if self.ch() == b'e' as i32 || self.ch() == b'E' as i32 {
            self.buff.push(self.ch() as u8);
            self.pos += 1;
            if self.ch() == b'+' as i32 || self.ch() == b'-' as i32 {
                self.buff.push(self.ch() as u8);
                self.pos += 1;
            }
            while is_digit(self.ch()) {
                self.buff.push(self.ch() as u8);
                self.pos += 1;
            }
        }
        match crate::number::str2d(&self.buff) {
            Some(n) => {
                let mut t = Self::tok(Tok::Number);
                t.num = n;
                Ok(t)
            }
            None => Err(self.lex_error("malformed number", false)),
        }
    }
}
