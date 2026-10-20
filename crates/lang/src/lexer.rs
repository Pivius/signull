//! Tokenizer for sinu source.

use crate::diag::{CompileError, Span};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kw {
    If,
    Else,
    While,
    For,
    Break,
    Continue,
    Return,
    True,
    False,
    Static,
    Stack,
    Entry,
    Null,
    Type,
    Struct,
    Enum,
    Void,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tok {
    Int(i64),
    Str(Vec<u8>),
    Ident(String),
    Kw(Kw),

    LParen,
    RParen,
    LBrace,
    RBrace,
    LBracket,
    RBracket,
    Comma,
    Semi,
    Colon,
    Dot,
    Arrow,

    Assign,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    AndAnd,
    OrOr,
    Amp,
    Pipe,
    Caret,
    Tilde,
    Bang,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,

    Eof,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: Tok,
    pub span: Span,
    pub line: usize,
    pub col: usize,
}

pub fn lex(source: &str) -> Result<Vec<Token>, CompileError> {
    let mut lexer = Lexer {
        chars: source.char_indices().collect(),
        idx: 0,
        line: 1,
        col: 1,
        start_line: 1,
        start_col: 1,
        tokens: Vec::new(),
    };
    lexer.run()?;

    let end = source.len();
    lexer.tokens.push(Token {
        kind: Tok::Eof,
        span: Span::new(end, end),
        line: lexer.line,
        col: lexer.col,
    });
    Ok(lexer.tokens)
}

struct Lexer {
    chars: Vec<(usize, char)>,
    idx: usize,
    line: usize,
    col: usize,
    start_line: usize,
    start_col: usize,
    tokens: Vec<Token>,
}

impl Lexer {
    fn peek(&self) -> Option<(usize, char)> {
        self.chars.get(self.idx).copied()
    }

    fn peek2(&self) -> Option<(usize, char)> {
        self.chars.get(self.idx + 1).copied()
    }

    fn advance(&mut self) -> Option<(usize, char)> {
        let next = self.chars.get(self.idx)?;
        self.idx += 1;
        if next.1 == '\n' {
            self.line += 1;
            self.col = 1;
        } else {
            self.col += 1;
        }
        Some(*next)
    }

    fn error(&self, message: impl Into<String>) -> CompileError {
        CompileError::new(message, self.line, self.col)
    }

    fn push(&mut self, kind: Tok, span: Span) {
        self.tokens.push(Token {
            kind,
            span,
            line: self.start_line,
            col: self.start_col,
        });
    }

    fn here(&self) -> (usize, usize) {
        (self.line, self.col)
    }

    fn run(&mut self) -> Result<(), CompileError> {
        while let Some((off, ch)) = self.peek() {
            self.start_line = self.line;
            self.start_col = self.col;
            match ch {
                ' ' | '\t' | '\r' => {
                    self.advance();
                }
                '\n' => {
                    self.advance();
                }
                '/' => match self.peek2().map(|(_, c)| c) {
                    Some('/') => self.skip_line_comment()?,
                    Some('*') => self.skip_block_comment()?,
                    _ => {
                        self.advance();
                        self.push(Tok::Slash, Span::new(off, off + 1));
                    }
                },
                '0'..='9' => self.number()?,
                '"' => self.string()?,
                '\'' => self.char_literal()?,
                c if is_ident_start(c) => self.ident(),
                '(' => self.sym(Tok::LParen, 1),
                ')' => self.sym(Tok::RParen, 1),
                '{' => self.sym(Tok::LBrace, 1),
                '}' => self.sym(Tok::RBrace, 1),
                '[' => self.sym(Tok::LBracket, 1),
                ']' => self.sym(Tok::RBracket, 1),
                ',' => self.sym(Tok::Comma, 1),
                ';' => self.sym(Tok::Semi, 1),
                ':' => self.sym(Tok::Colon, 1),
                '.' => self.sym(Tok::Dot, 1),
                '+' => self.sym(Tok::Plus, 1),
                '*' => self.sym(Tok::Star, 1),
                '%' => self.sym(Tok::Percent, 1),
                '^' => self.sym(Tok::Caret, 1),
                '~' => self.sym(Tok::Tilde, 1),
                '&' => self.double_or_single(Tok::AndAnd, b'&', Tok::Amp),
                '|' => self.double_or_single(Tok::OrOr, b'|', Tok::Pipe),
                '=' => self.double_or_single(Tok::Eq, b'=', Tok::Assign),
                '!' => self.double_or_single(Tok::Ne, b'=', Tok::Bang),
                '<' => self.compare_or_eq(Tok::Shl, '<', Tok::Le, Tok::Lt),
                '>' => self.compare_or_eq(Tok::Shr, '>', Tok::Ge, Tok::Gt),
                '-' => {
                    let (off, _) = self.peek().unwrap();
                    let two = self.peek2().map(|(_, c)| c);
                    if two == Some('>') {
                        self.advance();
                        self.advance();
                        self.push(Tok::Arrow, Span::new(off, off + 2));
                    } else {
                        self.advance();
                        self.push(Tok::Minus, Span::new(off, off + 1));
                    }
                }
                other => return Err(self.error(format!("unexpected character `{other}`"))),
            }
        }
        Ok(())
    }

    fn sym(&mut self, kind: Tok, width: usize) {
        let (start, _) = self.peek().unwrap();
        for _ in 0..width {
            self.advance();
        }
        self.push(kind, Span::new(start, start + width));
    }

    fn double_or_single(&mut self, double: Tok, second: u8, single: Tok) {
        let (start, _) = self.peek().unwrap();
        let is_double = self.peek2().map(|(_, c)| c as u8) == Some(second);
        if is_double {
            self.advance();
            self.advance();
            self.push(double, Span::new(start, start + 2));
        } else {
            self.advance();
            self.push(single, Span::new(start, start + 1));
        }
    }

    fn compare_or_eq(&mut self, double: Tok, double_ch: char, eq: Tok, single: Tok) {
        let (start, _) = self.peek().unwrap();
        let next = self.peek2().map(|(_, c)| c);
        self.advance();
        if next == Some(double_ch) {
            self.advance();
            self.push(double, Span::new(start, start + 2));
        } else if next == Some('=') {
            self.advance();
            self.push(eq, Span::new(start, start + 2));
        } else {
            self.push(single, Span::new(start, start + 1));
        }
    }

    fn skip_line_comment(&mut self) -> Result<(), CompileError> {
        while let Some((_, c)) = self.peek() {
            if c == '\n' {
                break;
            }
            self.advance();
        }
        Ok(())
    }

    fn skip_block_comment(&mut self) -> Result<(), CompileError> {
        let (start_line, _) = self.here();
        self.advance();
        self.advance();
        loop {
            match (self.peek(), self.peek2()) {
                (Some((_, '*')), Some((_, '/'))) => {
                    self.advance();
                    self.advance();
                    return Ok(());
                }
                (Some(_), _) => {
                    self.advance();
                }
                _ => {
                    return Err(CompileError::new(
                        "unterminated block comment",
                        start_line,
                        1,
                    ));
                }
            }
        }
    }

    fn number(&mut self) -> Result<(), CompileError> {
        let (start, _) = self.peek().unwrap();
        let mut raw = String::new();
        while let Some((_, c)) = self.peek() {
            if c.is_ascii_alphanumeric() || c == '_' {
                raw.push(c);
                self.advance();
            } else {
                break;
            }
        }

        let (line, col) = self.here();
        let value = parse_int(&raw, line, col)?;
        self.push(Tok::Int(value), Span::new(start, start + raw.len()));
        Ok(())
    }

    fn ident(&mut self) {
        let (start, _) = self.peek().unwrap();
        let mut name = String::new();
        while let Some((_, c)) = self.peek() {
            if is_ident_continue(c) {
                name.push(c);
                self.advance();
            } else {
                break;
            }
        }

        let len = name.len();
        let kind = match name.as_str() {
            "if" => Tok::Kw(Kw::If),
            "else" => Tok::Kw(Kw::Else),
            "while" => Tok::Kw(Kw::While),
            "for" => Tok::Kw(Kw::For),
            "break" => Tok::Kw(Kw::Break),
            "continue" => Tok::Kw(Kw::Continue),
            "return" => Tok::Kw(Kw::Return),
            "true" => Tok::Kw(Kw::True),
            "false" => Tok::Kw(Kw::False),
            "static" => Tok::Kw(Kw::Static),
            "stack" => Tok::Kw(Kw::Stack),
            "entry" => Tok::Kw(Kw::Entry),
            "null" => Tok::Kw(Kw::Null),
            "type" => Tok::Kw(Kw::Type),
            "struct" => Tok::Kw(Kw::Struct),
            "enum" => Tok::Kw(Kw::Enum),
            "void" => Tok::Kw(Kw::Void),
            _ => Tok::Ident(name),
        };
        self.push(kind, Span::new(start, start + len));
    }

    fn string(&mut self) -> Result<(), CompileError> {
        let (start, _) = self.peek().unwrap();
        let (line, col) = self.here();
        self.advance();
        let mut bytes = Vec::new();

        loop {
            let Some((_, c)) = self.peek() else {
                return Err(CompileError::new("unterminated string", line, col));
            };
            match c {
                '"' => {
                    self.advance();
                    break;
                }
                '\\' => {
                    self.advance();
                    let Some((_, esc)) = self.peek() else {
                        return Err(CompileError::new("unterminated string", line, col));
                    };
                    self.advance();
                    match esc {
                        'n' => bytes.push(b'\n'),
                        't' => bytes.push(b'\t'),
                        'r' => bytes.push(b'\r'),
                        '0' => bytes.push(0),
                        '\\' => bytes.push(b'\\'),
                        '"' => bytes.push(b'"'),
                        '\'' => bytes.push(b'\''),
                        'x' => {
                            let hi = self.hex_digit(line, col)?;
                            let lo = self.hex_digit(line, col)?;
                            bytes.push((hi << 4) | lo);
                        }
                        other => {
                            return Err(CompileError::new(
                                format!("unknown escape `\\{other}`"),
                                line,
                                col,
                            ));
                        }
                    }
                }
                c => {
                    let mut buf = [0u8; 4];
                    bytes.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
                    self.advance();
                }
            }
        }

        self.push(Tok::Str(bytes), Span::new(start, self.pos()));
        Ok(())
    }

    /// `'a'`, `'\n'`, `'\x1b'` — a single-byte character as an integer.
    fn char_literal(&mut self) -> Result<(), CompileError> {
        let (start, _) = self.peek().unwrap();
        let (line, col) = self.here();
        self.advance(); // opening '
        let Some((_, c)) = self.peek() else {
            return Err(CompileError::new(
                "unterminated character literal",
                line,
                col,
            ));
        };
        let value = if c == '\\' {
            self.advance();
            let Some((_, esc)) = self.peek() else {
                return Err(CompileError::new(
                    "unterminated character literal",
                    line,
                    col,
                ));
            };
            self.advance();
            match esc {
                'n' => b'\n' as i64,
                't' => b'\t' as i64,
                'r' => b'\r' as i64,
                '0' => 0,
                '\\' => b'\\' as i64,
                '\'' => b'\'' as i64,
                '"' => b'"' as i64,
                'x' => {
                    let hi = self.hex_digit(line, col)?;
                    let lo = self.hex_digit(line, col)?;
                    i64::from((hi << 4) | lo)
                }
                other => {
                    return Err(CompileError::new(
                        format!("unknown escape `\\{other}`"),
                        line,
                        col,
                    ));
                }
            }
        } else {
            self.advance();
            let mut buf = [0u8; 4];
            let encoded = c.encode_utf8(&mut buf);
            if encoded.len() != 1 {
                return Err(CompileError::new(
                    "character literals must be a single byte",
                    line,
                    col,
                ));
            }
            i64::from(c as u8)
        };
        if !self.peek().is_some_and(|(_, c)| c == '\'') {
            return Err(CompileError::new(
                "expected `'` to close character literal",
                line,
                col,
            ));
        }
        self.advance();
        self.push(Tok::Int(value), Span::new(start, self.pos()));
        Ok(())
    }

    fn hex_digit(&mut self, line: usize, col: usize) -> Result<u8, CompileError> {
        let Some((_, c)) = self.peek() else {
            return Err(CompileError::new(
                "expected two hex digits in `\\x..` escape",
                line,
                col,
            ));
        };
        let digit = c
            .to_digit(16)
            .ok_or_else(|| CompileError::new(format!("invalid hex digit `{c}`"), line, col))?;
        self.advance();
        Ok(digit as u8)
    }

    fn pos(&self) -> usize {
        self.chars
            .get(self.idx - 1)
            .map(|(off, c)| off + c.len_utf8())
            .unwrap_or(0)
    }
}

fn is_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_'
}

fn is_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn parse_int(raw: &str, line: usize, col: usize) -> Result<i64, CompileError> {
    let (radix, body) = if raw.len() >= 2 && raw.starts_with('0') {
        match raw.as_bytes()[1] {
            b'x' => (16, &raw[2..]),
            b'b' => (2, &raw[2..]),
            b'o' => (8, &raw[2..]),
            _ => (10, raw),
        }
    } else {
        (10, raw)
    };

    let digits: String = body.chars().filter(|c| *c != '_').collect();
    if digits.is_empty() {
        return Err(CompileError::new("malformed integer literal", line, col));
    }

    i64::from_str_radix(&digits, radix)
        .map_err(|_| CompileError::new("integer literal out of range", line, col))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<Tok> {
        lex(src).unwrap().into_iter().map(|t| t.kind).collect()
    }

    #[test]
    fn keywords_and_identifiers() {
        assert_eq!(
            kinds("int main() { return 0; }"),
            vec![
                Tok::Ident("int".into()),
                Tok::Ident("main".into()),
                Tok::LParen,
                Tok::RParen,
                Tok::LBrace,
                Tok::Kw(Kw::Return),
                Tok::Int(0),
                Tok::Semi,
                Tok::RBrace,
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn integer_literals_in_bases() {
        assert_eq!(
            kinds("0x2a 0b1010 0o17 1_000_000"),
            vec![
                Tok::Int(42),
                Tok::Int(10),
                Tok::Int(15),
                Tok::Int(1000000),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn operators() {
        assert_eq!(
            kinds("&& || << >> == != <= >= & | ^ ~ ! -> < >"),
            vec![
                Tok::AndAnd,
                Tok::OrOr,
                Tok::Shl,
                Tok::Shr,
                Tok::Eq,
                Tok::Ne,
                Tok::Le,
                Tok::Ge,
                Tok::Amp,
                Tok::Pipe,
                Tok::Caret,
                Tok::Tilde,
                Tok::Bang,
                Tok::Arrow,
                Tok::Lt,
                Tok::Gt,
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn keywords_include_control_flow_and_storage() {
        assert_eq!(
            kinds("struct enum void for break continue static stack entry null type"),
            vec![
                Tok::Kw(Kw::Struct),
                Tok::Kw(Kw::Enum),
                Tok::Kw(Kw::Void),
                Tok::Kw(Kw::For),
                Tok::Kw(Kw::Break),
                Tok::Kw(Kw::Continue),
                Tok::Kw(Kw::Static),
                Tok::Kw(Kw::Stack),
                Tok::Kw(Kw::Entry),
                Tok::Kw(Kw::Null),
                Tok::Kw(Kw::Type),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn string_escapes() {
        assert_eq!(
            kinds(r#""a\n\t\x41\0\"""#),
            vec![Tok::Str(b"a\n\tA\0\"".to_vec()), Tok::Eof]
        );
    }

    #[test]
    fn character_literals() {
        assert_eq!(
            kinds(r"'a' '\n' '\x1b' '\''"),
            vec![
                Tok::Int(97),
                Tok::Int(10),
                Tok::Int(27),
                Tok::Int(39),
                Tok::Eof
            ]
        );
    }

    #[test]
    fn unterminated_char_is_an_error() {
        assert!(lex("'a").is_err());
        assert!(lex("'\\n").is_err());
    }

    #[test]
    fn comments_are_skipped() {
        assert_eq!(
            kinds("1 // line\n2 /* block\ncomment */ 3"),
            vec![Tok::Int(1), Tok::Int(2), Tok::Int(3), Tok::Eof]
        );
    }

    #[test]
    fn unterminated_string_is_an_error() {
        assert!(lex("\"oops").is_err());
    }

    #[test]
    fn invalid_number_is_an_error() {
        assert!(lex("0x").is_err());
        assert!(lex("1234").is_ok());
    }

    #[test]
    fn tokens_carry_line_and_col() {
        let tokens = lex("1 +\n 2").unwrap();
        let plus = &tokens[1];
        assert_eq!((plus.line, plus.col), (1, 3));
        let two = &tokens[2];
        assert_eq!((two.line, two.col), (2, 2));
    }
}
