//! Word expansion (issue #10): parameters, command substitution, field
//! splitting, tilde, brace and glob, on lexer [`Word`]s.
//!
//! The output type is [`Field`]. A `Field` is data only: it has no
//! conversion into a lexer token, so an expansion result can never become an
//! operator or other syntax (invariant I1). Each character of a field keeps
//! its provenance, so quoted characters are never globbed, brace-expanded or
//! tilde-expanded (D8a). See `docs/design/expansion.md`.
// Lexing and expansion must not panic on any input (I3); tests may.
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic
    )
)]

use crate::syntax::lex::{OpKind, ParamOp, Quoting, Segment, Word};

/// Where a character of a field came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prov {
    /// Unquoted literal source text.
    Literal,
    /// Quoted or escaped source text, or a quoted expansion result.
    Quoted,
    /// The result of an unquoted expansion.
    Expanded,
}

impl Prov {
    /// Unquoted literal text and unquoted expansion results take part in
    /// tilde, brace and glob expansion (D8b keeps main for results).
    fn active(self) -> bool {
        !matches!(self, Prov::Quoted)
    }
}

/// One expanded field (an argument). Fields are never re-lexed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    text: String,
}

impl Field {
    fn new(text: String) -> Self {
        Field { text }
    }
    /// An operator kept as a literal argument (its own source spelling).
    pub fn op_text(kind: OpKind) -> Self {
        Field {
            text: kind.spelling().to_string(),
        }
    }
    pub fn as_str(&self) -> &str {
        &self.text
    }
    pub fn into_string(self) -> String {
        self.text
    }
}

/// Raised when a command substitution was interrupted by Ctrl-C (I4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Interrupted;

/// Why an expansion failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpandError {
    /// Ctrl-C during a command substitution: abandon the line (D12).
    Interrupted,
    /// A redirect target did not expand to exactly one field (D4).
    Ambiguous,
    /// `$((...))` is not supported yet (D-AR).
    ArithUnsupported,
}

impl From<Interrupted> for ExpandError {
    fn from(_: Interrupted) -> Self {
        ExpandError::Interrupted
    }
}

/// What the expander needs from the shell.
pub trait ExpandCtx {
    /// The value of a parameter. `braced` is true for `${name}`.
    fn var(&self, name: &str, braced: bool) -> Option<String>;
    /// Run a command substitution and return its output with trailing
    /// newlines removed.
    fn run_cmdsub(&mut self, src: &str) -> Result<String, Interrupted>;
    /// The home directory for `~`.
    fn home(&self) -> Option<String>;
    /// Whether globbing is enabled (config `shell.glob`).
    fn glob_enabled(&self) -> bool;
}

/// A redirect target: exactly one field (D4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedirectTarget(String);

impl RedirectTarget {
    pub fn from_fields(mut fields: Vec<Field>) -> Result<Self, ExpandError> {
        if fields.len() == 1
            && let Some(f) = fields.pop()
        {
            return Ok(RedirectTarget(f.into_string()));
        }
        Err(ExpandError::Ambiguous)
    }
    pub fn into_string(self) -> String {
        self.0
    }
}

#[derive(Clone, Copy)]
struct Ch {
    c: char,
    prov: Prov,
}

/// Collects characters and splits them into raw fields.
struct Builder {
    cur: Vec<Ch>,
    /// The current field must be produced even if empty (D3).
    keep: bool,
    out: Vec<Vec<Ch>>,
}

impl Builder {
    fn push_str(&mut self, s: &str, prov: Prov, split: bool) {
        for c in s.chars() {
            if split && matches!(c, ' ' | '\t' | '\n') {
                if !self.cur.is_empty() || self.keep {
                    self.out.push(std::mem::take(&mut self.cur));
                }
                self.keep = false;
            } else {
                self.cur.push(Ch { c, prov });
            }
        }
    }
    fn finish(mut self) -> Vec<Vec<Ch>> {
        if !self.cur.is_empty() || self.keep {
            self.out.push(self.cur);
        }
        self.out
    }
}

/// Push the expansion of `segs` into `b`. `in_operand` marks the operand of
/// an unquoted `${name:-word}`, whose unquoted literal text is an expansion
/// result (D8b), and whose command substitutions are not split (D2).
fn push_segments(
    segs: &[Segment],
    ctx: &mut dyn ExpandCtx,
    b: &mut Builder,
    in_operand: bool,
) -> Result<(), ExpandError> {
    for seg in segs {
        match seg {
            Segment::Lit { text, q } => {
                let prov = match q {
                    Quoting::Unquoted if in_operand => Prov::Expanded,
                    Quoting::Unquoted => Prov::Literal,
                    _ => Prov::Quoted,
                };
                if *q != Quoting::Unquoted {
                    b.keep = true;
                }
                b.push_str(text, prov, false);
                if !text.is_empty() {
                    b.keep = true;
                }
            }
            Segment::Param {
                name,
                braced,
                op,
                operand,
                q,
                ..
            } => {
                b.keep = true;
                let prov = if *q == Quoting::Unquoted {
                    Prov::Expanded
                } else {
                    Prov::Quoted
                };
                let val = ctx.var(name, *braced);
                match (op, operand) {
                    (ParamOp::UseDefault, Some(w)) if val.as_deref().is_none_or(str::is_empty) => {
                        if *q == Quoting::Unquoted {
                            push_segments(w.segments(), ctx, b, true)?;
                        } else {
                            // Inside double quotes the whole operand is quoted data.
                            let mut inner = Builder {
                                cur: Vec::new(),
                                keep: false,
                                out: Vec::new(),
                            };
                            push_segments(w.segments(), ctx, &mut inner, true)?;
                            for f in inner.finish() {
                                for ch in f {
                                    b.cur.push(Ch {
                                        c: ch.c,
                                        prov: Prov::Quoted,
                                    });
                                }
                            }
                        }
                    }
                    _ => b.push_str(val.as_deref().unwrap_or(""), prov, false),
                }
            }
            Segment::CmdSub { src, q, .. } => {
                let out = ctx.run_cmdsub(src)?;
                if *q == Quoting::Unquoted {
                    b.push_str(&out, Prov::Expanded, !in_operand);
                } else {
                    b.keep = true;
                    b.push_str(&out, Prov::Quoted, false);
                }
            }
            Segment::Arith { .. } => return Err(ExpandError::ArithUnsupported),
        }
    }
    Ok(())
}

/// Expand a word in argument position: parameters and substitutions, then
/// field splitting of unquoted substitution output, then tilde, brace and
/// glob on unquoted characters.
pub fn expand_word(w: &Word, ctx: &mut dyn ExpandCtx) -> Result<Vec<Field>, ExpandError> {
    let mut b = Builder {
        cur: Vec::new(),
        keep: false,
        out: Vec::new(),
    };
    push_segments(w.segments(), ctx, &mut b, false)?;
    let home = ctx.home();
    let glob_on = ctx.glob_enabled();
    let mut fields = Vec::new();
    for raw in b.finish() {
        let raw = tilde(raw, home.as_deref());
        for f in brace(&raw) {
            glob_field(&f, glob_on, &mut fields);
        }
    }
    Ok(fields)
}

/// Expand an assignment value (D11): no field splitting, no brace or glob;
/// a leading unquoted `~` is expanded as on main.
pub fn expand_assignment_value(
    segs: &[Segment],
    ctx: &mut dyn ExpandCtx,
) -> Result<String, ExpandError> {
    let mut b = Builder {
        cur: Vec::new(),
        keep: false,
        out: Vec::new(),
    };
    push_segments(segs, ctx, &mut b, true)?;
    let mut chars: Vec<Ch> = Vec::new();
    for f in b.finish() {
        chars.extend(f);
    }
    // The operand-style push above marks unquoted literal text as Expanded;
    // restore Literal for the leading-tilde check on plain literal values.
    if let Some(Segment::Lit {
        q: Quoting::Unquoted,
        ..
    }) = segs.first()
        && let Some(first) = chars.first_mut()
    {
        first.prov = Prov::Literal;
    }
    let home = ctx.home();
    Ok(tilde(chars, home.as_deref()).iter().map(|c| c.c).collect())
}

/// `~` or `~/...` at the start of a field, from an active character.
fn tilde(raw: Vec<Ch>, home: Option<&str>) -> Vec<Ch> {
    let Some(first) = raw.first() else { return raw };
    if first.c != '~' || !first.prov.active() {
        return raw;
    }
    if raw.get(1).is_some_and(|c| c.c != '/') {
        return raw;
    }
    let Some(home) = home else { return raw };
    let mut out: Vec<Ch> = home
        .chars()
        .map(|c| Ch {
            c,
            prov: Prov::Expanded,
        })
        .collect();
    out.extend(raw.into_iter().skip(1));
    out
}

/// Upper bound on the fields one brace expression may produce (I3).
const MAX_BRACE_FIELDS: usize = 1 << 16;

/// Brace expansion over active characters only (main's algorithm otherwise).
fn brace(raw: &[Ch]) -> Vec<Vec<Ch>> {
    let mut out = Vec::new();
    brace_into(raw.to_vec(), &mut out);
    out
}

fn brace_into(tok: Vec<Ch>, out: &mut Vec<Vec<Ch>>) {
    if out.len() >= MAX_BRACE_FIELDS {
        out.push(tok);
        return;
    }
    let Some(open) = find_brace_open(&tok) else {
        out.push(tok);
        return;
    };
    let Some(close) = find_brace_close(&tok, open) else {
        out.push(tok);
        return;
    };
    let (prefix, rest) = tok.split_at(open);
    let inner = rest.get(1..close - open).unwrap_or(&[]);
    let suffix = rest.get(close - open + 1..).unwrap_or(&[]);
    let join = |mid: &[Ch]| -> Vec<Ch> {
        let mut v = prefix.to_vec();
        v.extend_from_slice(mid);
        v.extend_from_slice(suffix);
        v
    };
    if inner.iter().all(|c| c.prov.active()) {
        let s: String = inner.iter().map(|c| c.c).collect();
        if let Some(items) = try_range_expand(&s) {
            for it in items {
                let mid: Vec<Ch> = it
                    .chars()
                    .map(|c| Ch {
                        c,
                        prov: Prov::Expanded,
                    })
                    .collect();
                brace_into(join(&mid), out);
            }
            return;
        }
    }
    if inner.iter().any(|c| c.c == ',' && c.prov.active()) {
        for part in split_brace_inner(inner) {
            brace_into(join(part), out);
        }
        return;
    }
    out.push(tok);
}

fn find_brace_open(s: &[Ch]) -> Option<usize> {
    let mut in_single = false;
    for (i, ch) in s.iter().enumerate() {
        if !ch.prov.active() {
            continue;
        }
        match ch.c {
            '\'' => in_single = !in_single,
            '{' if !in_single => return Some(i),
            _ => {}
        }
    }
    None
}

fn find_brace_close(s: &[Ch], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, ch) in s.iter().enumerate().skip(open) {
        if !ch.prov.active() {
            continue;
        }
        match ch.c {
            '{' => depth += 1,
            '}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_brace_inner(inner: &[Ch]) -> Vec<&[Ch]> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, ch) in inner.iter().enumerate() {
        if !ch.prov.active() {
            continue;
        }
        match ch.c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(inner.get(start..i).unwrap_or(&[]));
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(inner.get(start..).unwrap_or(&[]));
    parts
}

fn try_range_expand(inner: &str) -> Option<Vec<String>> {
    let (start_str, end_str) = inner.split_once("..")?;
    if let (Ok(s), Ok(e)) = (start_str.parse::<i64>(), end_str.parse::<i64>()) {
        if s.abs_diff(e) >= MAX_BRACE_FIELDS as u64 {
            return None;
        }
        return Some(if s <= e {
            (s..=e).map(|n| n.to_string()).collect()
        } else {
            (e..=s).rev().map(|n| n.to_string()).collect()
        });
    }
    let mut sc = start_str.chars();
    let mut ec = end_str.chars();
    if let (Some(s), None, Some(e), None) = (sc.next(), sc.next(), ec.next(), ec.next()) {
        let (s, e) = (s as u32, e as u32);
        return Some(if s <= e {
            (s..=e)
                .filter_map(char::from_u32)
                .map(|c| c.to_string())
                .collect()
        } else {
            (e..=s)
                .rev()
                .filter_map(char::from_u32)
                .map(|c| c.to_string())
                .collect()
        });
    }
    None
}

/// Glob one field if it has an active `*`, `?` or `[`; quoted glob
/// characters are escaped in the pattern (D8a). No match keeps the text.
fn glob_field(f: &[Ch], glob_on: bool, out: &mut Vec<Field>) {
    let text: String = f.iter().map(|c| c.c).collect();
    let has_glob = f
        .iter()
        .any(|c| c.prov.active() && matches!(c.c, '*' | '?' | '['));
    if !glob_on || !has_glob {
        out.push(Field::new(text));
        return;
    }
    let mut pat = String::with_capacity(text.len() + 8);
    for c in f {
        if !c.prov.active() && matches!(c.c, '*' | '?' | '[' | ']') {
            pat.push('[');
            pat.push(c.c);
            pat.push(']');
        } else {
            pat.push(c.c);
        }
    }
    match glob::glob(&pat) {
        Ok(paths) => {
            let mut matched: Vec<String> = paths
                .filter_map(|p| p.ok())
                .map(|p| p.display().to_string())
                .collect();
            if matched.is_empty() {
                out.push(Field::new(text));
            } else {
                matched.sort();
                out.extend(matched.into_iter().map(Field::new));
            }
        }
        Err(_) => out.push(Field::new(text)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::lex::{Token, lex};
    use std::collections::HashMap;

    struct T {
        vars: HashMap<String, String>,
        ran: Vec<String>,
    }
    impl ExpandCtx for T {
        fn var(&self, name: &str, _braced: bool) -> Option<String> {
            self.vars.get(name).cloned()
        }
        fn run_cmdsub(&mut self, src: &str) -> Result<String, Interrupted> {
            self.ran.push(src.to_string());
            if src == "INT" {
                return Err(Interrupted);
            }
            Ok(src.strip_prefix("echo ").unwrap_or("").to_string())
        }
        fn home(&self) -> Option<String> {
            Some("/h".into())
        }
        fn glob_enabled(&self) -> bool {
            false
        }
    }

    fn ctx(pairs: &[(&str, &str)]) -> T {
        T {
            vars: pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            ran: vec![],
        }
    }

    fn ex(s: &str, c: &mut T) -> Vec<String> {
        let mut out = vec![];
        for t in lex(s).unwrap() {
            if let Token::Word(w) = t {
                out.extend(
                    expand_word(&w, c)
                        .unwrap()
                        .into_iter()
                        .map(Field::into_string),
                );
            }
        }
        out
    }

    #[test]
    fn quoted_data_round_trips() {
        let mut c = ctx(&[("X", "~/{a,b} * ; > $(x)")]);
        assert_eq!(ex("\"$X\"", &mut c), ["~/{a,b} * ; > $(x)"]);
        // Unquoted results keep main (D8b): tilde and brace apply.
        assert_eq!(ex("$X", &mut c), ["/h/a * ; > $(x)", "/h/b * ; > $(x)"]);
    }

    #[test]
    fn empties_follow_main() {
        let mut c = ctx(&[]);
        assert_eq!(ex("$U", &mut c), [""]);
        assert_eq!(ex("$(echo)", &mut c), Vec::<String>::new());
        assert_eq!(ex("\"$(echo)\"", &mut c), [""]);
        assert_eq!(ex("${U:-''}", &mut c), [""]);
        assert_eq!(ex("''", &mut c), [""]);
    }

    #[test]
    fn only_unquoted_cmdsub_output_is_split() {
        let mut c = ctx(&[("X", "a b")]);
        assert_eq!(ex("$X", &mut c), ["a b"]);
        assert_eq!(ex("$(echo a b)", &mut c), ["a", "b"]);
        assert_eq!(ex("\"$(echo a b)\"", &mut c), ["a b"]);
        assert_eq!(ex("x$(echo  a)", &mut c), ["x", "a"]);
        assert_eq!(ex("${U:-a b}", &mut c), ["a b"]);
    }

    #[test]
    fn default_operand_is_lazy() {
        let mut c = ctx(&[("S", "set")]);
        assert_eq!(ex("${S:-$(echo no)}", &mut c), ["set"]);
        assert!(c.ran.is_empty());
        assert_eq!(ex("${U:-$(echo yes)}", &mut c), ["yes"]);
        assert_eq!(c.ran, ["echo yes"]);
    }

    #[test]
    fn interrupt_stops_later_substitutions() {
        let mut c = ctx(&[]);
        let w = match lex("$(INT)$(echo later)").unwrap().remove(0) {
            Token::Word(w) => w,
            _ => panic!(),
        };
        assert_eq!(expand_word(&w, &mut c), Err(ExpandError::Interrupted));
        assert_eq!(c.ran, ["INT"]);
    }

    #[test]
    fn tilde_only_unquoted() {
        let mut c = ctx(&[]);
        assert_eq!(
            ex("~ ~/x \"~\" \\~ ~x", &mut c),
            ["/h", "/h/x", "~", "~", "~x"]
        );
    }

    #[test]
    fn brace_only_unquoted() {
        let mut c = ctx(&[]);
        assert_eq!(
            ex("{a,b} \"{a,b}\" '{'a,b} x{1..3}", &mut c),
            ["a", "b", "{a,b}", "{a,b}", "x1", "x2", "x3"]
        );
    }

    #[test]
    fn arithmetic_is_unsupported() {
        let mut c = ctx(&[]);
        let w = match lex("$((1+2))").unwrap().remove(0) {
            Token::Word(w) => w,
            _ => panic!(),
        };
        assert_eq!(expand_word(&w, &mut c), Err(ExpandError::ArithUnsupported));
        assert!(c.ran.is_empty());
    }

    #[test]
    fn redirect_target_needs_one_field() {
        assert!(RedirectTarget::from_fields(vec![]).is_err());
        assert!(
            RedirectTarget::from_fields(vec![Field::new("a".into()), Field::new("b".into())])
                .is_err()
        );
        assert_eq!(
            RedirectTarget::from_fields(vec![Field::new("a".into())])
                .unwrap()
                .into_string(),
            "a"
        );
    }

    // Ported from parser.rs (expand_braces) tests.
    fn brace_s(s: &str) -> Vec<String> {
        let raw: Vec<Ch> = s
            .chars()
            .map(|c| Ch {
                c,
                prov: Prov::Literal,
            })
            .collect();
        brace(&raw)
            .into_iter()
            .map(|f| f.iter().map(|c| c.c).collect())
            .collect()
    }

    #[test]
    fn brace_comma_expansion() {
        assert_eq!(brace_s("{a,b,c}"), ["a", "b", "c"]);
    }
    #[test]
    fn brace_prefix_suffix_applied() {
        assert_eq!(brace_s("pre{a,b}post"), ["preapost", "prebpost"]);
    }
    #[test]
    fn brace_numeric_range_both_directions() {
        assert_eq!(brace_s("{1..4}"), ["1", "2", "3", "4"]);
        assert_eq!(brace_s("{4..1}"), ["4", "3", "2", "1"]);
    }
    #[test]
    fn brace_char_range() {
        assert_eq!(brace_s("{a..d}"), ["a", "b", "c", "d"]);
    }
    #[test]
    fn brace_nested() {
        assert_eq!(brace_s("{a,b{c,d}}"), ["a", "bc", "bd"]);
    }
    #[test]
    fn brace_non_expression_is_literal() {
        assert_eq!(brace_s("{a}"), ["{a}"]);
    }
    #[test]
    #[ignore = "Known limitation (G4): stepped ranges {1..N..step} are unsupported (returned literal)."]
    fn brace_stepped_range() {
        assert_eq!(brace_s("{1..10..2}"), ["1", "3", "5", "7", "9"]);
    }
    #[test]
    #[ignore = "Known limitation (G4): zero-padded ranges {01..03} lose their padding."]
    fn brace_zero_padded_range() {
        assert_eq!(brace_s("{01..03}"), ["01", "02", "03"]);
    }

    // Ported from parser.rs tokenize_with_quote_flags tests (now lex + expand).
    #[test]
    fn splits_plain_words() {
        assert_eq!(ex("a b c", &mut ctx(&[])), ["a", "b", "c"]);
    }
    #[test]
    fn keeps_double_quoted_spaces() {
        assert_eq!(ex("\"a b\" c", &mut ctx(&[])), ["a b", "c"]);
    }
    #[test]
    fn single_quotes_are_literal() {
        assert_eq!(ex("'a b' '$X'", &mut ctx(&[("X", "v")])), ["a b", "$X"]);
    }
    #[test]
    fn concatenates_adjacent_quoted_and_unquoted() {
        assert_eq!(ex("a\"b\"c", &mut ctx(&[])), ["abc"]);
    }
    #[test]
    fn empty_quotes_produce_empty_token() {
        assert_eq!(ex("\"\"", &mut ctx(&[])), [""]);
    }
    #[test]
    fn backslash_escapes_space_outside_quotes() {
        assert_eq!(ex("a\\ b", &mut ctx(&[])), ["a b"]);
    }
    #[test]
    fn double_quote_char_is_literal_inside_single_quotes() {
        assert_eq!(ex("'\"'", &mut ctx(&[])), ["\""]);
    }
    /// Was `#[ignore]` (known limitation E1) on main; fixed by D-ESC.
    #[test]
    fn backslash_literal_in_double_quotes() {
        assert_eq!(ex("\"a\\nb\"", &mut ctx(&[])), ["a\\nb"]);
    }
}
