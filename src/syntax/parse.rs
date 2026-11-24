//! Token-level parsing: chain splitting and pipeline structure.
//!
//! Every operator decision here is made on [`Token::Op`] values produced by
//! the lexer, never on expanded text (invariant I1).

use super::lex::{OpKind, Token, Word};

/// The chain operator that follows a segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChainOp {
    And,
    Or,
    Semicolon,
    None,
}

/// Split a token list on `;`, `&&` and `||` tokens (D6).
pub fn split_chain(tokens: Vec<Token>) -> Vec<(Vec<Token>, ChainOp)> {
    let mut out = Vec::new();
    let mut cur = Vec::new();
    for t in tokens {
        let op = match t.op_kind() {
            Some(OpKind::Semi) => ChainOp::Semicolon,
            Some(OpKind::AndIf) => ChainOp::And,
            Some(OpKind::OrIf) => ChainOp::Or,
            _ => {
                cur.push(t);
                continue;
            }
        };
        out.push((std::mem::take(&mut cur), op));
    }
    if !cur.is_empty() {
        out.push((cur, ChainOp::None));
    }
    out
}

/// A token plus where it came from.
#[derive(Clone, Debug)]
pub struct Item {
    pub tok: Token,
    /// The token came from an alias body (D10 mechanics).
    pub from_alias: bool,
}

/// Redirect kinds that take a target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedirKind {
    In,
    Out,
    Append,
    Err,
    ErrAppend,
}

/// An argument position in a stage.
#[derive(Clone, Debug)]
pub enum Arg {
    Word {
        word: Word,
        from_alias: bool,
    },
    /// An operator kept as a literal argument, as main did (a redirect with
    /// no target, or a `&` that is not at the end of a stage). The text is
    /// the operator's own source spelling, never expanded data.
    OpText(OpKind),
}

/// One element of a stage, in source order (expansion runs in this order).
#[derive(Clone, Debug)]
pub enum StageItem {
    Arg(Arg),
    Redirect { kind: RedirKind, target: Arg },
    MergeStderr,
}

/// One pipeline stage before expansion.
#[derive(Clone, Debug, Default)]
pub struct Stage {
    pub items: Vec<StageItem>,
    pub background: bool,
}

fn redir_kind(k: OpKind) -> Option<RedirKind> {
    Some(match k {
        OpKind::RedirIn => RedirKind::In,
        OpKind::RedirOut => RedirKind::Out,
        OpKind::RedirAppend => RedirKind::Append,
        OpKind::ErrOut => RedirKind::Err,
        OpKind::ErrAppend => RedirKind::ErrAppend,
        _ => return None,
    })
}

/// Parse one chain segment into pipeline stages (main's rules, on tokens):
/// `|` separates stages; a `&` that ends a stage means background, any
/// other `&` is a literal argument; a redirect takes the next token as its
/// target, and a redirect with nothing after it stays a literal argument.
pub fn parse_pipeline(items: Vec<Item>) -> Vec<Stage> {
    let mut stages = Vec::new();
    let mut cur: Vec<Item> = Vec::new();
    for it in items {
        if it.tok.op_kind() == Some(OpKind::Pipe) {
            stages.push(build_stage(std::mem::take(&mut cur)));
        } else {
            cur.push(it);
        }
    }
    stages.push(build_stage(cur));
    stages
}

fn to_arg(it: Item) -> Arg {
    match it.tok {
        Token::Word(word) => Arg::Word {
            word,
            from_alias: it.from_alias,
        },
        Token::Op(op) => Arg::OpText(op.kind()),
    }
}

fn build_stage(mut items: Vec<Item>) -> Stage {
    let mut stage = Stage::default();
    if items.last().and_then(|i| i.tok.op_kind()) == Some(OpKind::Background) {
        items.pop();
        stage.background = true;
    }
    let mut it = items.into_iter().peekable();
    while let Some(item) = it.next() {
        match item.tok.op_kind() {
            Some(OpKind::ErrToOut) => stage.items.push(StageItem::MergeStderr),
            Some(k) if redir_kind(k).is_some() => match (redir_kind(k), it.next()) {
                (Some(kind), Some(target)) => stage.items.push(StageItem::Redirect {
                    kind,
                    target: to_arg(target),
                }),
                _ => stage.items.push(StageItem::Arg(Arg::OpText(k))),
            },
            _ => stage.items.push(StageItem::Arg(to_arg(item))),
        }
    }
    stage
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::lex;

    fn items(s: &str) -> Vec<Item> {
        lex(s)
            .unwrap()
            .into_iter()
            .map(|tok| Item {
                tok,
                from_alias: false,
            })
            .collect()
    }

    #[test]
    fn chain_split_on_tokens_only() {
        let c = split_chain(lex("a; b && c || 'd;e'").unwrap());
        assert_eq!(c.len(), 4);
        assert_eq!(c[0].1, ChainOp::Semicolon);
        assert_eq!(c[1].1, ChainOp::And);
        assert_eq!(c[2].1, ChainOp::Or);
        assert_eq!(c[3].1, ChainOp::None);
    }

    // Ported from parser.rs extract_redirects tests.
    #[test]
    fn extracts_stdout_truncate_and_strips_redirect_tokens() {
        let st = parse_pipeline(items("echo hi > f"));
        assert_eq!(st.len(), 1);
        assert!(matches!(
            st[0].items[2],
            StageItem::Redirect {
                kind: RedirKind::Out,
                ..
            }
        ));
        assert_eq!(st[0].items.len(), 3);
    }

    #[test]
    fn extracts_append_and_merge() {
        let st = parse_pipeline(items(">> out"));
        assert!(matches!(
            st[0].items[0],
            StageItem::Redirect {
                kind: RedirKind::Append,
                ..
            }
        ));
        let st = parse_pipeline(items("cmd 2>&1"));
        assert!(matches!(st[0].items[1], StageItem::MergeStderr));
    }

    #[test]
    fn dangling_redirect_and_mid_background_are_literal() {
        let st = parse_pipeline(items("a & b >"));
        assert!(matches!(
            st[0].items[1],
            StageItem::Arg(Arg::OpText(OpKind::Background))
        ));
        assert!(matches!(
            st[0].items[3],
            StageItem::Arg(Arg::OpText(OpKind::RedirOut))
        ));
        assert!(!st[0].background);
        let st = parse_pipeline(items("a | b &"));
        assert_eq!(st.len(), 2);
        assert!(st[1].background);
    }
}
