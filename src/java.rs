//! A small Java/Kotlin source lexer. It keeps identifiers, punctuation and the
//! contents of string literals, and drops comments, character literals and
//! whitespace, so the Spring scanner can work on a token stream instead of
//! matching regexes against raw text.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Ident,
    Punct,
    /// A string literal; the token covers the text between the quotes.
    Str,
    /// A numeric literal.
    Literal,
}

#[derive(Clone, Copy, Debug)]
pub struct Tok {
    pub kind: Kind,
    pub start: usize,
    pub end: usize,
}

impl Tok {
    pub fn text<'a>(&self, src: &'a str) -> &'a str {
        &src[self.start..self.end]
    }
}

#[derive(Debug, Default)]
pub struct Lexed {
    pub toks: Vec<Tok>,
    /// `/** … */` comments: byte ranges of the whole comment.
    pub docs: Vec<(usize, usize)>,
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b == b'$' || b >= 0x80
}

fn is_ident_part(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
}

/// Tokenizes `src`. Byte offsets index into `src`, so they can be sliced.
pub fn lex(src: &str) -> Lexed {
    let b = src.as_bytes();
    let n = b.len();
    let mut toks: Vec<Tok> = Vec::new();
    let mut docs = Vec::new();
    let mut i = 0usize;
    while i < n {
        let c = b[i];
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        if c == b'/' && i + 1 < n {
            if b[i + 1] == b'/' {
                while i < n && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            if b[i + 1] == b'*' {
                let start = i;
                i += 2;
                // `/***/` and `/**/` are ordinary block comments.
                let doc = i + 1 < n && b[i] == b'*' && b[i + 1] != b'/';
                while i + 1 < n && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(n);
                if doc {
                    docs.push((start, i));
                }
                continue;
            }
        }
        // Text block (Java 15+): """ … """
        if c == b'"' && b.get(i..i + 3) == Some(b"\"\"\"") {
            i += 3;
            let start = i;
            while i < n {
                if b[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if b.get(i..i + 3) == Some(b"\"\"\"") {
                    break;
                }
                i += 1;
            }
            let end = i.min(n);
            toks.push(Tok { kind: Kind::Str, start, end });
            i = (i + 3).min(n);
            continue;
        }
        if c == b'"' {
            i += 1;
            let start = i;
            while i < n {
                if b[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if b[i] == b'"' {
                    break;
                }
                i += 1;
            }
            let end = i.min(n);
            toks.push(Tok { kind: Kind::Str, start, end });
            i = (i + 1).min(n);
            continue;
        }
        if c == b'\'' {
            i += 1;
            while i < n {
                if b[i] == b'\\' && i + 1 < n {
                    i += 2;
                    continue;
                }
                if b[i] == b'\'' {
                    break;
                }
                i += 1;
            }
            i = (i + 1).min(n);
            continue;
        }
        if is_ident_start(c) {
            let start = i;
            i += 1;
            while i < n && is_ident_part(b[i]) {
                i += 1;
            }
            toks.push(Tok { kind: Kind::Ident, start, end: i });
            continue;
        }
        if c.is_ascii_digit() {
            let start = i;
            i += 1;
            while i < n && (is_ident_part(b[i]) || b[i] == b'.') {
                i += 1;
            }
            toks.push(Tok { kind: Kind::Literal, start, end: i });
            continue;
        }
        toks.push(Tok { kind: Kind::Punct, start: i, end: i + 1 });
        i += 1;
    }
    Lexed { toks, docs }
}

/// `true` when token `i` is the one-byte punctuation `c`.
pub fn is(src: &str, lexed: &Lexed, i: usize, c: char) -> bool {
    match lexed.toks.get(i) {
        Some(t) if t.kind == Kind::Punct && t.end == t.start + 1 => src.as_bytes().get(t.start) == Some(&(c as u8)),
        _ => false,
    }
}

/// The identifier `i` names, if it is one.
pub fn ident<'a>(src: &'a str, lexed: &Lexed, i: usize) -> Option<&'a str> {
    match lexed.toks.get(i) {
        Some(t) if t.kind == Kind::Ident => Some(t.text(src)),
        _ => None,
    }
}

/// Index of the token closing the bracket opened at `open`.
pub fn matching(src: &str, lexed: &Lexed, open: usize, open_ch: char, close_ch: char) -> Option<usize> {
    let mut depth = 0usize;
    for i in open..lexed.toks.len() {
        if is(src, lexed, i, open_ch) {
            depth += 1;
        } else if is(src, lexed, i, close_ch) {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(i);
            }
        }
    }
    None
}

/// Splits `src` into (line, UTF-16 column) pairs cheaply: positions asked in
/// increasing byte order cost about one pass over the text in total.
pub struct LineIndex<'a> {
    src: &'a str,
    at: usize,
    line: u32,
    col: u32,
}

impl<'a> LineIndex<'a> {
    pub fn new(src: &'a str) -> Self {
        LineIndex { src, at: 0, line: 0, col: 0 }
    }

    pub fn of(&mut self, byte: usize) -> (u32, u32) {
        let byte = byte.min(self.src.len());
        if byte < self.at {
            *self = LineIndex::new(self.src);
        }
        for ch in self.src[self.at..byte].chars() {
            if ch == '\n' {
                self.line += 1;
                self.col = 0;
            } else {
                self.col += ch.len_utf16() as u32;
            }
        }
        self.at = byte;
        (self.line, self.col)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn idents(src: &str) -> Vec<String> {
        let l = lex(src);
        l.toks.iter().filter(|t| t.kind == Kind::Ident).map(|t| t.text(src).to_owned()).collect()
    }

    #[test]
    fn skips_comments_and_reads_strings() {
        let src = r#"
            // @GetMapping("/hidden")
            /* @PostMapping("/also-hidden") */
            /** Doc for the method. */
            @GetMapping("/visible")
            public String go() { return "x"; }
        "#;
        let l = lex(src);
        assert_eq!(idents(src).first().map(String::as_str), Some("GetMapping"));
        let strs: Vec<&str> = l.toks.iter().filter(|t| t.kind == Kind::Str).map(|t| t.text(src)).collect();
        assert_eq!(strs, vec!["/visible", "x"]);
        assert_eq!(l.docs.len(), 1);
    }

    #[test]
    fn text_blocks_and_escapes() {
        let src = r#"String a = "he said \"hi\""; String b = """
            deep
            """; char c = '"';"#;
        let l = lex(src);
        let strs: Vec<&str> = l.toks.iter().filter(|t| t.kind == Kind::Str).map(|t| t.text(src)).collect();
        assert_eq!(strs.len(), 2);
        assert!(strs[0].contains(r#"\""#));
        assert!(strs[1].contains("deep"));
    }

    #[test]
    fn brackets_and_positions() {
        let src = "f(a, (b), c)";
        let l = lex(src);
        assert_eq!(matching(src, &l, 1, '(', ')'), Some(9));
        let mut ix = LineIndex::new("a\n😀b");
        assert_eq!(ix.of(0), (0, 0));
        assert_eq!(ix.of(2), (1, 0));
        assert_eq!(ix.of(6), (1, 2));
    }
}
