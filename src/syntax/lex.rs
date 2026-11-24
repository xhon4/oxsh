//! The PR-A lexer (issue #10): turns one input line into typed tokens.
//!
//! `lex` is pure: it reads only its argument. It never runs a command,
//! never reads a variable and never touches the filesystem (invariant I2).
//! Operators exist only as [`Op`] tokens, and an [`Op`] can only be built in
//! this module, from unquoted source characters. Expansion results are
//! `expand::Field`s, which have no conversion back into tokens, so data can
//! never become syntax (invariant I1). See `docs/design/expansion.md`.

use std::fmt;

/// How a piece of literal text was written in the source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quoting {
    Unquoted,
    Single,
    Double,
    /// A character preceded by an unquoted backslash.
    Escaped,
}

/// Operator kinds: main's operator set (`| & ; && || < > >> 2> 2>> 2>&1`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpKind {
    Pipe,
    Background,
    Semi,
    AndIf,
    OrIf,
    RedirIn,
    RedirOut,
    RedirAppend,
    ErrOut,
    ErrAppend,
    ErrToOut,
}

impl OpKind {
    /// The source spelling of the operator.
    pub fn spelling(self) -> &'static str {
        match self {
            OpKind::Pipe => "|",
            OpKind::Background => "&",
            OpKind::Semi => ";",
            OpKind::AndIf => "&&",
            OpKind::OrIf => "||",
            OpKind::RedirIn => "<",
            OpKind::RedirOut => ">",
            OpKind::RedirAppend => ">>",
            OpKind::ErrOut => "2>",
            OpKind::ErrAppend => "2>>",
            OpKind::ErrToOut => "2>&1",
        }
    }

    /// `;`, `&&` or `||`.
    pub fn is_chain(self) -> bool {
        matches!(self, OpKind::Semi | OpKind::AndIf | OpKind::OrIf)
    }

    /// Unit operators: everything except the chain separators.
    fn from_unit(s: &str) -> Option<OpKind> {
        Some(match s {
            "|" => OpKind::Pipe,
            "&" => OpKind::Background,
            "<" => OpKind::RedirIn,
            ">" => OpKind::RedirOut,
            ">>" => OpKind::RedirAppend,
            "2>" => OpKind::ErrOut,
            "2>>" => OpKind::ErrAppend,
            "2>&1" => OpKind::ErrToOut,
            _ => return None,
        })
    }
}

/// An operator token. The field is private: only the lexer creates `Op`s.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Op {
    kind: OpKind,
    span: (usize, usize),
}

impl Op {
    pub fn kind(&self) -> OpKind {
        self.kind
    }
}

/// `${name}` or `${name:-word}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParamOp {
    /// `$name`, `${name}`.
    Plain,
    /// `${name:-word}`: the operand is used when the value is unset or empty.
    UseDefault,
}

/// One piece of a word.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment {
    /// Literal text. `Single`/`Double` text may be empty (`''`, `""`).
    Lit { text: String, q: Quoting },
    /// A parameter expansion. `q` is `Unquoted` or `Double`.
    Param {
        name: String,
        braced: bool,
        op: ParamOp,
        operand: Option<Word>,
        q: Quoting,
        raw: String,
    },
    /// `$(...)` or a backquoted command; `src` is the command text (never run here).
    CmdSub {
        src: String,
        q: Quoting,
        raw: String,
    },
    /// `$((...))`: arithmetic, never a command.
    Arith {
        src: String,
        q: Quoting,
        raw: String,
    },
}

/// A word: a sequence of segments. Only the lexer creates words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Word {
    segs: Vec<Segment>,
    span: (usize, usize),
    cut: bool,
}

impl Word {
    pub fn segments(&self) -> &[Segment] {
        &self.segs
    }
    /// True when this word was cut off a trailing operator by rule D1(b).
    pub fn is_cut(&self) -> bool {
        self.cut
    }
    /// True if any part of the word was quoted or escaped.
    pub fn has_quoted(&self) -> bool {
        self.segs.iter().any(|s| match s {
            Segment::Lit { q, .. }
            | Segment::Param { q, .. }
            | Segment::CmdSub { q, .. }
            | Segment::Arith { q, .. } => *q != Quoting::Unquoted,
        })
    }
    /// The word's text if it is made of literal segments only (quotes removed).
    pub fn literal_text(&self) -> Option<String> {
        let mut out = String::new();
        for s in &self.segs {
            match s {
                Segment::Lit { text, .. } => out.push_str(text),
                _ => return None,
            }
        }
        Some(out)
    }
    /// The word as literal source text: quotes removed from literal
    /// segments, expansions kept verbatim (used by on_startup and `clear`).
    pub fn source_text(&self) -> String {
        let mut out = String::new();
        for s in &self.segs {
            match s {
                Segment::Lit { text, .. } => out.push_str(text),
                Segment::Param { raw, .. }
                | Segment::CmdSub { raw, .. }
                | Segment::Arith { raw, .. } => out.push_str(raw),
            }
        }
        out
    }
    /// If the word starts with an unquoted literal `NAME=`, return the name
    /// and the segments of the value (D11).
    pub fn assignment(&self) -> Option<(String, Vec<Segment>)> {
        let Some(Segment::Lit {
            text,
            q: Quoting::Unquoted,
        }) = self.segs.first()
        else {
            return None;
        };
        let eq = text.find('=')?;
        let name = &text[..eq];
        let mut chars = name.chars();
        let first = chars.next()?;
        if !(first.is_ascii_alphabetic() || first == '_')
            || !chars.all(|c| c.is_alphanumeric() || c == '_')
        {
            return None;
        }
        let mut value = Vec::with_capacity(self.segs.len());
        let rest = &text[eq + 1..];
        if !rest.is_empty() {
            value.push(Segment::Lit {
                text: rest.to_string(),
                q: Quoting::Unquoted,
            });
        }
        value.extend(self.segs.iter().skip(1).cloned());
        Some((name.to_string(), value))
    }
}

/// A lexer token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    Op(Op),
    Word(Word),
}

impl Token {
    pub fn span(&self) -> (usize, usize) {
        match self {
            Token::Op(o) => o.span,
            Token::Word(w) => w.span,
        }
    }
    pub fn op_kind(&self) -> Option<OpKind> {
        match self {
            Token::Op(o) => Some(o.kind),
            Token::Word(_) => None,
        }
    }
}

/// A lexical error: the line is rejected as a whole (rc 2, nothing runs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyntaxError {
    msg: &'static str,
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.msg)
    }
}

fn err<T>(msg: &'static str) -> Result<T, SyntaxError> {
    Err(SyntaxError { msg })
}

/// Maximum nesting of `${...:-...}` operands (I3: bounded recursion).
const MAX_DEPTH: usize = 64;

/// Lex a whole input line.
pub fn lex(input: &str) -> Result<Vec<Token>, SyntaxError> {
    let chars: Vec<char> = input.chars().collect();
    let last_nl = chars.iter().rposition(|c| *c == '\n');
    let mut lx = Lexer {
        c: &chars,
        i: 0,
        depth: 0,
        last_nl,
    };
    lx.line()
}

struct Lexer<'a> {
    c: &'a [char],
    i: usize,
    depth: usize,
    /// Index of the last newline in the line, if any.
    last_nl: Option<usize>,
}

fn is_op_char(c: char) -> bool {
    matches!(c, '|' | '&' | '<' | '>')
}

fn is_name_start(c: char) -> bool {
    (c.is_alphanumeric() || c == '_') && !c.is_ascii_digit()
}

/// Accumulates the segments of one blank-delimited piece.
struct Piece {
    segs: Vec<Segment>,
    start: usize,
}

impl Piece {
    fn push_char(&mut self, ch: char, q: Quoting) {
        if let Some(Segment::Lit { text, q: lq }) = self.segs.last_mut()
            && *lq == q
            && q != Quoting::Single
        {
            text.push(ch);
            return;
        }
        self.segs.push(Segment::Lit {
            text: ch.to_string(),
            q,
        });
    }
}

impl Lexer<'_> {
    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }
    fn peek_at(&self, k: usize) -> Option<char> {
        self.c.get(self.i + k).copied()
    }
    fn slice(&self, a: usize, b: usize) -> String {
        self.c
            .get(a..b)
            .map(|s| s.iter().collect())
            .unwrap_or_default()
    }

    fn line(&mut self) -> Result<Vec<Token>, SyntaxError> {
        let mut out = Vec::new();
        let mut piece = Piece {
            segs: Vec::new(),
            start: 0,
        };
        loop {
            let Some(ch) = self.peek() else {
                finish(&mut piece, self.i, &mut out);
                break;
            };
            match ch {
                ' ' | '\t' => {
                    finish(&mut piece, self.i, &mut out);
                    self.i += 1;
                }
                ';' => {
                    self.chain_op(&mut piece, &mut out, OpKind::Semi, 1);
                }
                '&' if self.peek_at(1) == Some('&') => {
                    self.chain_op(&mut piece, &mut out, OpKind::AndIf, 2);
                }
                '|' if self.peek_at(1) == Some('|') => {
                    self.chain_op(&mut piece, &mut out, OpKind::OrIf, 2);
                }
                // A here-document body (an unquoted `<<` and a later newline)
                // would otherwise be lexed as words and its substitutions run.
                // Without a newline, `<<` stays a literal word as on main.
                '<' if self.peek_at(1) == Some('<') && self.last_nl.is_some_and(|n| n > self.i) => {
                    return err("here-documents are not supported yet");
                }
                _ => {
                    if piece.segs.is_empty() {
                        piece.start = self.i;
                    }
                    self.word_char(&mut piece, Quoting::Unquoted)?;
                }
            }
        }
        Ok(out)
    }

    fn chain_op(&mut self, piece: &mut Piece, out: &mut Vec<Token>, kind: OpKind, len: usize) {
        finish(piece, self.i, out);
        out.push(Token::Op(Op {
            kind,
            span: (self.i, self.i + len),
        }));
        self.i += len;
    }

    /// Consume one unit of word text at the top level (`q` = Unquoted) or in
    /// an unquoted `${}` operand.
    fn word_char(&mut self, piece: &mut Piece, ctx: Quoting) -> Result<(), SyntaxError> {
        let Some(ch) = self.peek() else { return Ok(()) };
        match ch {
            '\'' => {
                let start = self.i + 1;
                let mut j = start;
                while j < self.c.len() && self.c.get(j) != Some(&'\'') {
                    j += 1;
                }
                if j >= self.c.len() {
                    return err("unterminated single quote");
                }
                piece.segs.push(Segment::Lit {
                    text: self.slice(start, j),
                    q: Quoting::Single,
                });
                self.i = j + 1;
            }
            '"' => {
                self.i += 1;
                self.double_quoted(piece)?;
            }
            '\\' => {
                // A trailing lone backslash is dropped, as on main.
                if let Some(n) = self.peek_at(1) {
                    piece.push_char(n, Quoting::Escaped);
                }
                self.i += 2.min(self.c.len() - self.i);
            }
            '$' => self.dollar(piece, ctx)?,
            '`' => self.backtick(piece, ctx)?,
            c => {
                piece.push_char(c, ctx);
                self.i += 1;
            }
        }
        Ok(())
    }

    /// After the opening `"`: read up to the closing `"`.
    fn double_quoted(&mut self, piece: &mut Piece) -> Result<(), SyntaxError> {
        let before = piece.segs.len();
        loop {
            let Some(ch) = self.peek() else {
                return err("unterminated double quote");
            };
            match ch {
                '"' => {
                    self.i += 1;
                    break;
                }
                '\\' => {
                    let Some(n) = self.peek_at(1) else {
                        return err("unterminated double quote");
                    };
                    if matches!(n, '$' | '`' | '"' | '\\' | '\n') {
                        piece.push_char(n, Quoting::Double);
                    } else {
                        piece.push_char('\\', Quoting::Double);
                        piece.push_char(n, Quoting::Double);
                    }
                    self.i += 2;
                }
                '$' => self.dollar(piece, Quoting::Double)?,
                '`' => self.backtick(piece, Quoting::Double)?,
                c => {
                    piece.push_char(c, Quoting::Double);
                    self.i += 1;
                }
            }
        }
        if piece.segs.len() == before {
            piece.segs.push(Segment::Lit {
                text: String::new(),
                q: Quoting::Double,
            });
        }
        Ok(())
    }

    /// At a `$`. `q` is the quoting context (`Unquoted` or `Double`).
    fn dollar(&mut self, piece: &mut Piece, q: Quoting) -> Result<(), SyntaxError> {
        let start = self.i;
        match self.peek_at(1) {
            Some('(') if self.peek_at(2) == Some('(') => {
                let end = self.arith_end(self.i + 3)?;
                let src = self.slice(self.i + 3, end);
                let raw = self.slice(start, end + 2);
                piece.segs.push(Segment::Arith { src, q, raw });
                self.i = end + 2;
            }
            Some('(') => {
                let end = self.cmdsub_end(self.i + 2)?;
                let src = self.slice(self.i + 2, end);
                let raw = self.slice(start, end + 1);
                piece.segs.push(Segment::CmdSub { src, q, raw });
                self.i = end + 1;
            }
            Some('{') => self.brace_param(piece, q)?,
            Some(c) if matches!(c, '?' | '$' | '!' | '#' | '@' | '*') || c.is_ascii_digit() => {
                piece.segs.push(Segment::Param {
                    name: c.to_string(),
                    braced: false,
                    op: ParamOp::Plain,
                    operand: None,
                    q,
                    raw: self.slice(start, start + 2),
                });
                self.i += 2;
            }
            Some(c) if is_name_start(c) => {
                let mut j = self.i + 1;
                while self
                    .c
                    .get(j)
                    .is_some_and(|c| c.is_alphanumeric() || *c == '_')
                {
                    j += 1;
                }
                piece.segs.push(Segment::Param {
                    name: self.slice(self.i + 1, j),
                    braced: false,
                    op: ParamOp::Plain,
                    operand: None,
                    q,
                    raw: self.slice(start, j),
                });
                self.i = j;
            }
            _ => {
                // `$` before anything else (including `$'`) is a literal `$`.
                piece.push_char('$', q);
                self.i += 1;
            }
        }
        Ok(())
    }

    /// `${name}` / `${name:-word}`; at the `$`.
    fn brace_param(&mut self, piece: &mut Piece, q: Quoting) -> Result<(), SyntaxError> {
        let start = self.i;
        let name_start = self.i + 2;
        let mut j = name_start;
        loop {
            match self.c.get(j) {
                None => return err("unterminated ${"),
                Some('}') => {
                    piece.segs.push(Segment::Param {
                        name: self.slice(name_start, j),
                        braced: true,
                        op: ParamOp::Plain,
                        operand: None,
                        q,
                        raw: self.slice(start, j + 1),
                    });
                    self.i = j + 1;
                    return Ok(());
                }
                Some(':') if self.c.get(j + 1) == Some(&'-') => break,
                Some(_) => j += 1,
            }
        }
        let name = self.slice(name_start, j);
        self.i = j + 2;
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return err("${...} nested too deeply");
        }
        let op_start = self.i;
        let mut operand = Piece {
            segs: Vec::new(),
            start: op_start,
        };
        loop {
            let Some(ch) = self.peek() else {
                return err("unterminated ${");
            };
            if ch == '}' {
                break;
            }
            if q == Quoting::Double {
                match ch {
                    '"' => {
                        self.i += 1;
                        self.double_quoted(&mut operand)?;
                    }
                    '\\' => {
                        let Some(n) = self.peek_at(1) else {
                            return err("unterminated ${");
                        };
                        if matches!(n, '$' | '`' | '"' | '\\' | '\n' | '}') {
                            operand.push_char(n, Quoting::Double);
                        } else {
                            operand.push_char('\\', Quoting::Double);
                            operand.push_char(n, Quoting::Double);
                        }
                        self.i += 2;
                    }
                    '$' => self.dollar(&mut operand, Quoting::Double)?,
                    '`' => self.backtick(&mut operand, Quoting::Double)?,
                    c => {
                        operand.push_char(c, Quoting::Double);
                        self.i += 1;
                    }
                }
            } else {
                self.word_char(&mut operand, Quoting::Unquoted)?;
            }
        }
        self.depth -= 1;
        let end = self.i; // at '}'
        piece.segs.push(Segment::Param {
            name,
            braced: true,
            op: ParamOp::UseDefault,
            operand: Some(Word {
                segs: operand.segs,
                span: (op_start, end),
                cut: false,
            }),
            q,
            raw: self.slice(start, end + 1),
        });
        self.i = end + 1;
        Ok(())
    }

    /// Backquoted command substitution; at the opening backquote.
    fn backtick(&mut self, piece: &mut Piece, q: Quoting) -> Result<(), SyntaxError> {
        let start = self.i;
        let mut j = self.i + 1;
        let mut src = String::new();
        loop {
            match self.c.get(j) {
                None => return err("unterminated backquote"),
                Some('`') => break,
                Some('\\') => match self.c.get(j + 1) {
                    None => return err("unterminated backquote"),
                    Some(&n) => {
                        if matches!(n, '$' | '`' | '\\') || (q == Quoting::Double && n == '"') {
                            src.push(n);
                        } else {
                            src.push('\\');
                            src.push(n);
                        }
                        j += 2;
                    }
                },
                Some(&c) => {
                    src.push(c);
                    j += 1;
                }
            }
        }
        piece.segs.push(Segment::CmdSub {
            src,
            q,
            raw: self.slice(start, j + 1),
        });
        self.i = j + 1;
        Ok(())
    }

    /// Index of the `)` closing a `$(` whose body starts at `from`.
    /// Iterative (bounded stack), linear in the input.
    fn cmdsub_end(&self, from: usize) -> Result<usize, SyntaxError> {
        #[derive(Clone, Copy, PartialEq)]
        enum Ctx {
            Paren,
            Brace,
            Dq,
            Bq,
        }
        let mut stack = vec![Ctx::Paren];
        let mut j = from;
        while let Some(&c) = self.c.get(j) {
            let next = self.c.get(j + 1).copied();
            let top = stack.last().copied().unwrap_or(Ctx::Paren);
            match top {
                Ctx::Paren | Ctx::Brace => match c {
                    '\\' => {
                        j += 2;
                        continue;
                    }
                    '\'' => {
                        j += 1;
                        while self.c.get(j).is_some_and(|c| *c != '\'') {
                            j += 1;
                        }
                        if j >= self.c.len() {
                            return err("unterminated $(");
                        }
                    }
                    '"' => stack.push(Ctx::Dq),
                    '`' => stack.push(Ctx::Bq),
                    '$' if next == Some('(') => {
                        stack.push(Ctx::Paren);
                        j += 2;
                        continue;
                    }
                    '$' if next == Some('{') => {
                        stack.push(Ctx::Brace);
                        j += 2;
                        continue;
                    }
                    '(' if top == Ctx::Paren => stack.push(Ctx::Paren),
                    ')' if top == Ctx::Paren => {
                        stack.pop();
                        if stack.is_empty() {
                            return Ok(j);
                        }
                    }
                    '}' if top == Ctx::Brace => {
                        stack.pop();
                    }
                    _ => {}
                },
                Ctx::Dq => match c {
                    '\\' => {
                        j += 2;
                        continue;
                    }
                    '"' => {
                        stack.pop();
                    }
                    '`' => stack.push(Ctx::Bq),
                    '$' if next == Some('(') => {
                        stack.push(Ctx::Paren);
                        j += 2;
                        continue;
                    }
                    '$' if next == Some('{') => {
                        stack.push(Ctx::Brace);
                        j += 2;
                        continue;
                    }
                    _ => {}
                },
                Ctx::Bq => match c {
                    '\\' => {
                        j += 2;
                        continue;
                    }
                    '`' => {
                        stack.pop();
                    }
                    _ => {}
                },
            }
            j += 1;
        }
        err("unterminated $(")
    }

    /// Index of the first `)` of the `))` closing a `$((` whose body starts at `from`.
    fn arith_end(&self, from: usize) -> Result<usize, SyntaxError> {
        let mut depth = 0usize;
        let mut j = from;
        while let Some(&c) = self.c.get(j) {
            match c {
                '(' => depth += 1,
                ')' if depth == 0 => {
                    if self.c.get(j + 1) == Some(&')') {
                        return Ok(j);
                    }
                    return err("unterminated $((");
                }
                ')' => depth -= 1,
                _ => {}
            }
            j += 1;
        }
        err("unterminated $((")
    }
}

/// Close the current piece and classify it as operator(s) and/or a word (D1).
fn finish(piece: &mut Piece, end: usize, out: &mut Vec<Token>) {
    let segs = std::mem::take(&mut piece.segs);
    let start = piece.start;
    if segs.is_empty() {
        return;
    }
    // Split off the last segment if it is unquoted literal text.
    let (prefix, last_text): (&[Segment], Option<&str>) = match segs.split_last() {
        Some((
            Segment::Lit {
                text,
                q: Quoting::Unquoted,
            },
            rest,
        )) => (rest, Some(text.as_str())),
        _ => (&segs[..], None),
    };
    if let Some(tail) = last_text {
        // (a) A unit that is exactly an operator spelling.
        if prefix.is_empty()
            && let Some(kind) = OpKind::from_unit(tail)
        {
            out.push(Token::Op(Op {
                kind,
                span: (start, end),
            }));
            return;
        }
    }
    // (a') Main parity: literal pieces whose operator characters are all
    // unquoted and whose only quoted parts are empty quotes or a leading
    // quoted `2` (`"">>`, `>''`, `"2">`) were operators on main, because main
    // removed quotes before matching operator text. The operator characters
    // are unquoted source text, so this is still syntax from source (I1).
    if let Some(kind) = quoted_glue_op(&segs) {
        out.push(Token::Op(Op {
            kind,
            span: (start, end),
        }));
        return;
    }
    if let Some(tail) = last_text {
        // (b) An expansion prefix followed by a trailing operator run.
        let has_exp = prefix.iter().any(|s| !matches!(s, Segment::Lit { .. }));
        if has_exp {
            let run_start = tail
                .char_indices()
                .rev()
                .take_while(|(_, c)| is_op_char(*c))
                .last()
                .map(|(i, _)| i);
            if let Some(rs) = run_start
                && let Some(kind) = OpKind::from_unit(&tail[rs..])
            {
                let op_len = tail[rs..].chars().count();
                let mut wsegs = prefix.to_vec();
                if rs > 0 {
                    wsegs.push(Segment::Lit {
                        text: tail[..rs].to_string(),
                        q: Quoting::Unquoted,
                    });
                }
                out.push(Token::Word(Word {
                    segs: wsegs,
                    span: (start, end - op_len),
                    cut: true,
                }));
                out.push(Token::Op(Op {
                    kind,
                    span: (end - op_len, end),
                }));
                return;
            }
        }
    }
    out.push(Token::Word(Word {
        segs,
        span: (start, end),
        cut: false,
    }));
}

fn quoted_glue_op(segs: &[Segment]) -> Option<OpKind> {
    let mut text = String::new();
    let mut quoted = false;
    for (i, seg) in segs.iter().enumerate() {
        let Segment::Lit { text: t, q } = seg else {
            return None;
        };
        if *q != Quoting::Unquoted {
            quoted = true;
            if !(t.is_empty() || (i == 0 && t == "2")) {
                return None;
            }
        }
        text.push_str(t);
    }
    if !quoted {
        return None;
    }
    OpKind::from_unit(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(s: &str) -> Vec<String> {
        lex(s)
            .unwrap()
            .iter()
            .map(|t| match t {
                Token::Op(o) => format!("<{}>", o.kind().spelling()),
                Token::Word(w) => w.source_text(),
            })
            .collect()
    }

    #[test]
    fn plain_words_and_ops() {
        assert_eq!(kinds("a b | c > f"), ["a", "b", "<|>", "c", "<>>", "f"]);
        assert_eq!(
            kinds("a;b&&c||d"),
            ["a", "<;>", "b", "<&&>", "c", "<||>", "d"]
        );
    }

    #[test]
    fn quoted_ops_are_words() {
        assert_eq!(
            kinds("echo '>' \">>\" \\| x"),
            ["echo", ">", ">>", "|", "x"]
        );
        assert_eq!(kinds("'a;b' \"c&&d\" e\\;f"), ["a;b", "c&&d", "e;f"]);
    }

    #[test]
    fn glued_literal_ops_stay_words() {
        assert_eq!(
            kinds("cmd>file x.bak> log 2>/dev/null >f a|b"),
            ["cmd>file", "x.bak>", "log", "2>/dev/null", ">f", "a|b"]
        );
    }

    #[test]
    fn expansion_prefix_splits_trailing_op() {
        assert_eq!(
            kinds("$X> f $(true)>> g ${U}2> h"),
            [
                "$X", "<>>", "f", "$(true)", "<>>>", "g", "${U}2", "<>>", "h"
            ]
        );
        let t = lex("$X>").unwrap();
        assert!(matches!(&t[0], Token::Word(w) if w.is_cut()));
    }

    #[test]
    fn empty_or_two_quoted_prefix_is_main_op() {
        assert_eq!(
            kinds("a \"\">> f \"2\"> g 'x'> h >'' i"),
            ["a", "<>>>", "f", "<2>>", "g", "x>", "h", "<>>", "i"]
        );
        assert_eq!(kinds("cat <<EOF"), ["cat", "<<EOF"]);
    }

    #[test]
    fn separators_inside_substitutions_are_data() {
        assert_eq!(
            kinds("echo $(a; b) ${U:-x;y} `c && d`"),
            ["echo", "$(a; b)", "${U:-x;y}", "`c && d`"]
        );
    }

    #[test]
    fn unterminated_constructs_are_errors() {
        for s in [
            "echo 'a",
            "echo \"a",
            "echo $(a",
            "echo ${A",
            "echo `a",
            "echo $((1",
            "echo ${U:-$(x}",
            "cat <<EOF\nx\nEOF",
        ] {
            assert!(lex(s).is_err(), "{s}");
        }
    }

    #[test]
    fn double_quote_backslash_rule() {
        let t = lex("\"a\\qb\\$c\"").unwrap();
        let Token::Word(w) = &t[0] else { panic!() };
        assert_eq!(w.literal_text().unwrap(), "a\\qb$c");
    }

    #[test]
    fn brace_operand_uses_same_rules() {
        let t = lex("\"${U:-a\\}b}\"").unwrap();
        let Token::Word(w) = &t[0] else { panic!() };
        match &w.segments()[0] {
            Segment::Param {
                operand: Some(op), ..
            } => assert_eq!(op.literal_text().unwrap(), "a}b"),
            s => panic!("{s:?}"),
        }
    }

    #[test]
    fn ansi_c_dollar_is_literal() {
        let t = lex("$'a\\tb'").unwrap();
        let Token::Word(w) = &t[0] else { panic!() };
        assert_eq!(w.literal_text().unwrap(), "$a\\tb");
    }

    #[test]
    fn arithmetic_is_not_a_command() {
        let t = lex("$((1+2))").unwrap();
        let Token::Word(w) = &t[0] else { panic!() };
        assert!(matches!(w.segments()[0], Segment::Arith { .. }));
    }

    #[test]
    fn deep_nesting_is_bounded() {
        let s = format!("{}x{}", "${U:-".repeat(10_000), "}".repeat(10_000));
        assert!(lex(&s).is_err());
        let s = format!("{}x{}", "$(".repeat(10_000), ")".repeat(10_000));
        assert!(lex(&s).is_ok());
    }

    // Ported from parser.rs (tokenize_with_quote_flags / expand_subshells tests).

    #[test]
    fn subshell_in_single_quotes_is_not_expanded() {
        let t = lex("'$(echo x)'").unwrap();
        let Token::Word(w) = &t[0] else { panic!() };
        assert_eq!(w.literal_text().unwrap(), "$(echo x)");
    }

    #[test]
    fn apostrophe_in_double_quotes_does_not_disable_expansion() {
        let t = lex("\"a's $(echo X)\"").unwrap();
        let Token::Word(w) = &t[0] else { panic!() };
        assert!(
            w.segments()
                .iter()
                .any(|s| matches!(s, Segment::CmdSub { .. }))
        );
    }

    #[test]
    fn passthrough_preserves_utf8() {
        assert_eq!(kinds("café ☃ 你好"), ["café", "☃", "你好"]);
    }

    // Ported from scripting.rs (is_var_assignment / parse_env_prefix /
    // strip_quotes): assignment detection now works on tokens (D11).
    fn assign(s: &str) -> Option<(String, String)> {
        let t = lex(s).unwrap();
        let Some(Token::Word(w)) = t.first() else {
            return None;
        };
        let (name, segs) = w.assignment()?;
        let v = Word {
            segs,
            span: (0, 0),
            cut: false,
        };
        Some((name, v.source_text()))
    }

    #[test]
    fn detects_assignments() {
        let cases: [(&str, Option<(&str, &str)>); 8] = [
            ("a=b", Some(("a", "b"))),
            ("a=", Some(("a", ""))),
            ("_x=1", Some(("_x", "1"))),
            ("a==b", Some(("a", "=b"))),
            ("=b", None),
            ("1a=b", None),
            ("a.b=c", None),
            ("echo a=b", None),
        ];
        for (input, expected) in cases {
            assert_eq!(
                assign(input),
                expected.map(|(a, b)| (a.to_string(), b.to_string())),
                "{input}"
            );
        }
    }

    #[test]
    fn assignment_prefix_is_one_word() {
        // `a=b cmd`: the assignment word is `a=b`; `cmd` is a separate word.
        assert_eq!(assign("a=b cmd"), Some(("a".into(), "b".into())));
        assert_eq!(lex("FOO=bar cmd").unwrap().len(), 2);
        assert_eq!(lex("FOO=bar ").unwrap().len(), 1);
    }

    #[test]
    fn env_prefix_handles_quoted_value() {
        assert_eq!(
            assign("FOO=\"bar baz\" cmd"),
            Some(("FOO".into(), "bar baz".into()))
        );
        assert_eq!(lex("FOO=\"bar baz\"").unwrap().len(), 1);
    }

    #[test]
    fn quotes_are_removed_from_values() {
        assert_eq!(assign("A=\"abc\""), Some(("A".into(), "abc".into())));
        assert_eq!(assign("A='abc'"), Some(("A".into(), "abc".into())));
        assert_eq!(assign("A=\"\""), Some(("A".into(), "".into())));
        assert!(lex("A=\"abc").is_err()); // unbalanced: syntax error (D7)
    }
}
