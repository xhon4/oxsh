use std::collections::HashMap;
use std::path::Path;

/// Tokenize raw input into arguments, handling quotes and escapes.
pub fn tokenize(input: &str) -> Vec<String> {
    let (tokens, _) = tokenize_with_quote_flags(input);
    tokens
}

/// Tokenize input and also return a parallel bool vector indicating which
/// tokens were entirely enclosed in quotes. Used to suppress glob expansion
/// on tokens like `"*.txt"` where the user intended a literal string.
pub fn tokenize_with_quote_flags(input: &str) -> (Vec<String>, Vec<bool>) {
    let mut tokens = Vec::new();
    let mut quoted_flags = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;
    let mut was_quoted = false;
    let mut has_unquoted_char = false;

    for ch in input.chars() {
        if escape {
            current.push(ch);
            escape = false;
            // Escaped chars are outside of quotes — token is not fully-quoted
            has_unquoted_char = true;
            continue;
        }
        match ch {
            '\\' if !in_single => {
                escape = true;
                has_unquoted_char = true;
            }
            '\'' if !in_double => {
                in_single = !in_single;
                was_quoted = true;
            }
            '"' if !in_single => {
                in_double = !in_double;
                was_quoted = true;
            }
            ' ' | '\t' if !in_single && !in_double => {
                if !current.is_empty() || was_quoted {
                    let fully_quoted = was_quoted && !has_unquoted_char;
                    tokens.push(std::mem::take(&mut current));
                    quoted_flags.push(fully_quoted);
                    was_quoted = false;
                    has_unquoted_char = false;
                }
            }
            _ => {
                if !in_single && !in_double {
                    has_unquoted_char = true;
                }
                current.push(ch);
            }
        }
    }
    if !current.is_empty() || was_quoted {
        let fully_quoted = was_quoted && !has_unquoted_char;
        tokens.push(current);
        quoted_flags.push(fully_quoted);
    }
    (tokens, quoted_flags)
}

/// Expand a leading `~` to the home directory.
///
/// `$VAR`/`${VAR}` (including environment variables, via the env fallback) are
/// already expanded upstream by `scripting::expand_shell_vars`; expanding them
/// again here re-expanded any value that happened to contain a `$`.
pub fn expand_vars(tokens: &mut [String]) {
    for token in tokens.iter_mut() {
        if token.starts_with('~') {
            *token = shellexpand::tilde(token).to_string();
        }
    }
}

/// Resolve aliases with recursion support and a depth limit to prevent hangs
/// from self-referencing aliases like `alias ls='ls --color'`.
pub fn resolve_alias(tokens: &mut Vec<String>, aliases: &HashMap<String, String>) {
    resolve_alias_depth(tokens, aliases, 0);
}

fn resolve_alias_depth(
    tokens: &mut Vec<String>,
    aliases: &HashMap<String, String>,
    depth: usize,
) {
    if tokens.is_empty() || depth > 16 {
        return;
    }
    if let Some(expansion) = aliases.get(&tokens[0]) {
        let mut expanded = tokenize(expansion);
        // Break self-reference (e.g. `alias ls='ls --color'`): expand once, don't recurse.
        let is_self_ref = expanded.first().map(|s| s.as_str()) == Some(tokens[0].as_str());
        expanded.extend(tokens.drain(1..));
        *tokens = expanded;
        if !is_self_ref {
            resolve_alias_depth(tokens, aliases, depth + 1);
        }
    }
}

/// A single command in a pipeline
#[derive(Debug)]
pub struct Command {
    pub args: Vec<String>,
    pub stdin_redirect: Option<String>,
    pub stdout_redirect: Option<Redirect>,
    pub stderr_redirect: Option<Redirect>,
    /// If true, stderr is merged into stdout (2>&1)
    pub merge_stderr: bool,
    pub background: bool,
}

#[derive(Debug)]
pub enum Redirect {
    Truncate(String),
    Append(String),
}

/// Parse a full input line into a pipeline of commands.
/// Takes pre-tokenized args to avoid re-parsing expanded tokens as raw text.
pub fn parse_pipeline_from_tokens(token_groups: Vec<Vec<String>>) -> Vec<Command> {
    token_groups
        .into_iter()
        
        .map(|mut tokens| {
            let background = tokens.last().map(|t| t == "&").unwrap_or(false);
            if background {
                tokens.pop();
            }
            let (stdin_redirect, stdout_redirect, stderr_redirect, merge_stderr) =
                extract_redirects(&mut tokens);
            Command {
                args: tokens,
                stdin_redirect,
                stdout_redirect,
                stderr_redirect,
                merge_stderr,
                background,
            }
        })
        .collect()
}

/// Parse a raw input string into a pipeline of commands.
/// Used for cases where we have a raw string (e.g. -c flag, subshell expansion).
pub fn parse_pipeline(input: &str) -> Vec<Command> {
    let segments = split_on_pipes(input);
    let mut commands = Vec::new();

    for (i, seg) in segments.iter().enumerate() {
        let trimmed = seg.trim();
        let is_last = i == segments.len() - 1;
        let (cmd_str, background) = if is_last && trimmed.ends_with('&') {
            (&trimmed[..trimmed.len() - 1], true)
        } else {
            (trimmed, false)
        };

        let mut tokens = tokenize(cmd_str);
        let (stdin_redirect, stdout_redirect, stderr_redirect, merge_stderr) =
            extract_redirects(&mut tokens);

        commands.push(Command {
            args: tokens,
            stdin_redirect,
            stdout_redirect,
            stderr_redirect,
            merge_stderr,
            background,
        });
    }
    commands
}

/// Split input on unquoted `|` characters (but not `||`)
fn split_on_pipes(input: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;

    for (i, ch) in input.char_indices() {
        if escape {
            escape = false;
            continue;
        }
        match ch {
            '\\' if !in_single => escape = true,
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '|' if !in_single && !in_double => {
                // Check it's not || (chain op — handled by split_chain_ops in shell.rs)
                let next = input.as_bytes().get(i + 1).copied();
                if next == Some(b'|') {
                    // This is ||, skip — split_chain_ops already split on this boundary
                    continue;
                }
                segments.push(&input[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    segments.push(&input[start..]);
    segments
}

/// Extract redirect operators from tokens: <, >, >>, 2>, 2>>, 2>&1
fn extract_redirects(
    tokens: &mut Vec<String>,
) -> (Option<String>, Option<Redirect>, Option<Redirect>, bool) {
    let mut stdin = None;
    let mut stdout = None;
    let mut stderr = None;
    let mut merge_stderr = false;
    let mut i = 0;

    while i < tokens.len() {
        let token = &tokens[i];
        if token == "2>&1" {
            merge_stderr = true;
            tokens.remove(i);
        } else if token == "<" && i + 1 < tokens.len() {
            stdin = Some(tokens[i + 1].clone());
            tokens.drain(i..=i + 1);
        } else if token == ">>" && i + 1 < tokens.len() {
            stdout = Some(Redirect::Append(tokens[i + 1].clone()));
            tokens.drain(i..=i + 1);
        } else if token == ">" && i + 1 < tokens.len() {
            stdout = Some(Redirect::Truncate(tokens[i + 1].clone()));
            tokens.drain(i..=i + 1);
        } else if token == "2>" && i + 1 < tokens.len() {
            stderr = Some(Redirect::Truncate(tokens[i + 1].clone()));
            tokens.drain(i..=i + 1);
        } else if token == "2>>" && i + 1 < tokens.len() {
            stderr = Some(Redirect::Append(tokens[i + 1].clone()));
            tokens.drain(i..=i + 1);
        } else {
            i += 1;
        }
    }
    (stdin, stdout, stderr, merge_stderr)
}

/// Check if input looks like a directory path (for auto_cd)
pub fn looks_like_directory(input: &str) -> bool {
    let expanded = shellexpand::tilde(input).to_string();
    Path::new(&expanded).is_dir()
}

/// Expand glob patterns in tokens (*, ?, **).
/// Quoted tokens (e.g. from `"*.txt"`) are left unexpanded.
/// `quoted` must be the same length as `tokens` (use `tokenize_with_quote_flags`).
pub fn expand_globs_respecting_quotes(tokens: &mut Vec<String>, quoted: &[bool]) {
    let mut expanded = Vec::with_capacity(tokens.len());
    for (token, &is_quoted) in tokens.drain(..).zip(quoted.iter()) {
        if !is_quoted && contains_glob_chars(&token) {
            match glob::glob(&token) {
                Ok(paths) => {
                    let mut matched: Vec<String> = paths
                        .filter_map(|p| p.ok())
                        .map(|p| p.display().to_string())
                        .collect();
                    if matched.is_empty() {
                        expanded.push(token);
                    } else {
                        matched.sort();
                        expanded.extend(matched);
                    }
                }
                Err(_) => expanded.push(token),
            }
        } else {
            expanded.push(token);
        }
    }
    *tokens = expanded;
}

fn contains_glob_chars(s: &str) -> bool {
    s.contains('*') || s.contains('?') || s.contains('[')
}

/// Expand brace expressions in tokens: `{a,b,c}` → three tokens,
/// `{1..5}` → five tokens, `{a..e}` → five tokens.
/// Braces inside single-quoted strings are not expanded.
pub fn expand_braces(tokens: &mut Vec<String>) {
    let mut result = Vec::with_capacity(tokens.len());
    for token in tokens.drain(..) {
        result.extend(expand_brace_token(&token));
    }
    *tokens = result;
}

fn expand_brace_token(token: &str) -> Vec<String> {
    // Find the first unescaped `{`
    let Some(open) = find_brace_open(token) else {
        return vec![token.to_string()];
    };
    let Some(close) = find_brace_close(token, open) else {
        return vec![token.to_string()];
    };

    let prefix = &token[..open];
    let inner = &token[open + 1..close];
    let suffix = &token[close + 1..];

    // Try range expansion: {1..10} or {a..z}
    if let Some(items) = try_range_expand(inner) {
        return items
            .into_iter()
            .flat_map(|s| expand_brace_token(&format!("{prefix}{s}{suffix}")))
            .collect();
    }

    // Try comma expansion: {a,b,c}
    if inner.contains(',') {
        let parts = split_brace_inner(inner);
        return parts
            .into_iter()
            .flat_map(|p| expand_brace_token(&format!("{prefix}{p}{suffix}")))
            .collect();
    }

    // Not a valid brace expression — return as-is
    vec![token.to_string()]
}

fn find_brace_open(s: &str) -> Option<usize> {
    let mut in_single = false;
    for (i, ch) in s.char_indices() {
        match ch {
            '\'' => in_single = !in_single,
            '{' if !in_single => return Some(i),
            _ => {}
        }
    }
    None
}

fn find_brace_close(s: &str, open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (i, ch) in s[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(open + i);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_brace_inner(inner: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0;
    for (i, ch) in inner.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(&inner[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts.push(&inner[start..]);
    parts
}

fn try_range_expand(inner: &str) -> Option<Vec<String>> {
    let (start_str, end_str) = inner.split_once("..")?;
    // Numeric range
    if let (Ok(s), Ok(e)) = (start_str.parse::<i64>(), end_str.parse::<i64>()) {
        let range: Vec<String> = if s <= e {
            (s..=e).map(|n| n.to_string()).collect()
        } else {
            (e..=s).rev().map(|n| n.to_string()).collect()
        };
        return Some(range);
    }
    // Single-character range
    let s_chars: Vec<char> = start_str.chars().collect();
    let e_chars: Vec<char> = end_str.chars().collect();
    if s_chars.len() == 1 && e_chars.len() == 1 {
        let s = s_chars[0] as u32;
        let e = e_chars[0] as u32;
        let range: Vec<String> = if s <= e {
            (s..=e).filter_map(char::from_u32).map(|c| c.to_string()).collect()
        } else {
            (e..=s).rev().filter_map(char::from_u32).map(|c| c.to_string()).collect()
        };
        return Some(range);
    }
    None
}

/// Expand `$(command)` and backtick substitutions in a string.
///
/// Operates on `char`s (UTF-8 safe) and tracks both single- and double-quote
/// context so that an apostrophe inside double quotes does not wrongly disable
/// expansion. Escaped characters inside `$(...)` are skipped, not counted toward
/// the parenthesis depth.
pub fn expand_subshells(input: &str) -> String {
    let chars: Vec<char> = input.chars().collect();
    let mut result = String::with_capacity(input.len());
    let mut i = 0;
    let mut in_single = false;
    let mut in_double = false;

    while i < chars.len() {
        match chars[i] {
            '\'' if !in_double => {
                in_single = !in_single;
                result.push('\'');
                i += 1;
            }
            '"' if !in_single => {
                in_double = !in_double;
                result.push('"');
                i += 1;
            }
            '$' if !in_single && i + 1 < chars.len() && chars[i + 1] == '(' => {
                i += 2; // skip "$("
                let start = i;
                let mut depth = 1;
                let mut sq = false;
                let mut dq = false;
                while i < chars.len() && depth > 0 {
                    match chars[i] {
                        '\\' if !sq => {
                            // Escaped char: skip it and the next, don't count parens.
                            i += 1;
                            if i < chars.len() {
                                i += 1;
                            }
                            continue;
                        }
                        '\'' if !dq => sq = !sq,
                        '"' if !sq => dq = !dq,
                        '(' if !sq && !dq => depth += 1,
                        ')' if !sq && !dq => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => {}
                    }
                    i += 1;
                }
                let cmd: String = chars[start..i].iter().collect();
                if i < chars.len() {
                    i += 1; // skip closing ')'
                }
                result.push_str(&run_subshell(&cmd));
            }
            '`' if !in_single => {
                i += 1;
                let start = i;
                while i < chars.len() && chars[i] != '`' {
                    i += 1;
                }
                let cmd: String = chars[start..i].iter().collect();
                if i < chars.len() {
                    i += 1; // skip closing backtick
                }
                result.push_str(&run_subshell(&cmd));
            }
            '\\' if !in_single => {
                // Preserve the escape so the downstream tokenizer handles it.
                result.push('\\');
                i += 1;
                if i < chars.len() {
                    result.push(chars[i]);
                    i += 1;
                }
            }
            c => {
                result.push(c);
                i += 1;
            }
        }
    }
    result
}

/// Run a command and capture its stdout, trimming trailing newlines.
/// Uses $SHELL if set, falling back to `sh`.
fn run_subshell(cmd: &str) -> String {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return String::new();
    }
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
    match std::process::Command::new(&shell)
        .arg("-c")
        .arg(cmd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .output()
    {
        Ok(output) => {
            let s = String::from_utf8_lossy(&output.stdout);
            s.trim_end_matches('\n').to_string()
        }
        Err(e) => {
            eprintln!("oxsh: $({cmd}): {e}");
            String::new()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(input: &str) -> (Vec<String>, Vec<bool>) {
        tokenize_with_quote_flags(input)
    }

    fn brace(input: &str) -> Vec<String> {
        let mut t = vec![input.to_string()];
        expand_braces(&mut t);
        t
    }

    // ── tokenize_with_quote_flags ──

    #[test]
    fn splits_plain_words() {
        let (t, q) = tok("a b c");
        assert_eq!(t, ["a", "b", "c"]);
        assert_eq!(q, [false, false, false]);
    }

    #[test]
    fn keeps_double_quoted_spaces_and_flags_quoted() {
        let (t, q) = tok("\"a b\" c");
        assert_eq!(t, ["a b", "c"]);
        assert_eq!(q, [true, false]);
    }

    #[test]
    fn single_quotes_are_literal() {
        let (t, q) = tok("'a b'");
        assert_eq!(t, ["a b"]);
        assert_eq!(q, [true]);
    }

    #[test]
    fn concatenates_adjacent_quoted_and_unquoted() {
        let (t, q) = tok("a\"b\"c");
        assert_eq!(t, ["abc"]);
        assert_eq!(q, [false]); // mixed → not fully quoted
    }

    #[test]
    fn empty_quotes_produce_empty_token() {
        let (t, q) = tok("\"\"");
        assert_eq!(t, [""]);
        assert_eq!(q, [true]);
    }

    #[test]
    fn backslash_escapes_space_outside_quotes() {
        let (t, _) = tok("a\\ b");
        assert_eq!(t, ["a b"]);
    }

    #[test]
    fn double_quote_char_is_literal_inside_single_quotes() {
        let (t, _) = tok("'\"'");
        assert_eq!(t, ["\""]);
    }

    #[test]
    #[ignore = "ISSUE #24 (E1): inside double quotes a backslash should be literal \
                unless before $ ` \" \\; currently it escapes the next char and is dropped."]
    fn backslash_literal_in_double_quotes() {
        let (t, _) = tok("\"a\\nb\"");
        assert_eq!(t, ["a\\nb"]); // currently produces "anb"
    }

    // ── expand_braces ──

    #[test]
    fn brace_comma_expansion() {
        assert_eq!(brace("{a,b,c}"), ["a", "b", "c"]);
    }

    #[test]
    fn brace_prefix_suffix_applied() {
        assert_eq!(brace("pre{a,b}post"), ["preapost", "prebpost"]);
    }

    #[test]
    fn brace_numeric_range_both_directions() {
        assert_eq!(brace("{1..4}"), ["1", "2", "3", "4"]);
        assert_eq!(brace("{4..1}"), ["4", "3", "2", "1"]);
    }

    #[test]
    fn brace_char_range() {
        assert_eq!(brace("{a..d}"), ["a", "b", "c", "d"]);
    }

    #[test]
    fn brace_nested() {
        assert_eq!(brace("{a,b{c,d}}"), ["a", "bc", "bd"]);
    }

    #[test]
    fn brace_non_expression_is_literal() {
        assert_eq!(brace("{a}"), ["{a}"]);
    }

    #[test]
    #[ignore = "ISSUE #50 (G4): stepped ranges {1..N..step} are unsupported (returned literal)."]
    fn brace_stepped_range() {
        assert_eq!(brace("{1..10..2}"), ["1", "3", "5", "7", "9"]);
    }

    #[test]
    #[ignore = "ISSUE #50 (G4): zero-padded ranges {01..03} lose their padding."]
    fn brace_zero_padded_range() {
        assert_eq!(brace("{01..03}"), ["01", "02", "03"]);
    }

    // ── extract_redirects (private) ──

    #[test]
    fn extracts_stdout_truncate_and_strips_redirect_tokens() {
        let mut t = vec!["echo".into(), "hi".into(), ">".into(), "f".into()];
        let (stdin, stdout, stderr, merge) = extract_redirects(&mut t);
        assert_eq!(t, ["echo", "hi"]);
        assert!(matches!(stdout, Some(Redirect::Truncate(ref p)) if p == "f"));
        assert!(stdin.is_none() && stderr.is_none() && !merge);
    }

    #[test]
    fn extracts_append_and_merge() {
        let mut t = vec![">>".into(), "out".into()];
        let (_, stdout, _, _) = extract_redirects(&mut t);
        assert!(matches!(stdout, Some(Redirect::Append(_))));

        let mut t2 = vec!["cmd".into(), "2>&1".into()];
        let (_, _, _, merge) = extract_redirects(&mut t2);
        assert!(merge);
        assert_eq!(t2, ["cmd"]);
    }

    // ── resolve_alias ──

    fn aliases(pairs: &[(&str, &str)]) -> std::collections::HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn resolves_simple_alias_with_args() {
        let a = aliases(&[("g", "git")]);
        let mut t = vec!["g".into(), "status".into()];
        resolve_alias(&mut t, &a);
        assert_eq!(t, ["git", "status"]);
    }

    #[test]
    fn self_referencing_alias_does_not_hang() {
        let a = aliases(&[("ls", "ls --color")]);
        let mut t = vec!["ls".into(), "-a".into()];
        resolve_alias(&mut t, &a);
        assert_eq!(t, ["ls", "--color", "-a"]);
    }

    #[test]
    fn mutual_alias_recursion_is_bounded() {
        let a = aliases(&[("x", "y"), ("y", "x")]);
        let mut t = vec!["x".into()];
        resolve_alias(&mut t, &a); // must terminate via depth limit, no hang
        assert!(!t.is_empty());
    }

    // ── expand_subshells ──

    #[test]
    fn subshell_passthrough_preserves_utf8() {
        // No substitution present — input is returned unchanged, including
        // multi-byte UTF-8 (regression for the byte-cast corruption, S2).
        assert_eq!(expand_subshells("café ☃ 你好"), "café ☃ 你好");
    }

    #[test]
    fn subshell_in_single_quotes_is_not_expanded() {
        // Single quotes protect $(...) — and this needs no command execution.
        assert_eq!(expand_subshells("'$(echo x)'"), "'$(echo x)'");
    }

    #[test]
    fn apostrophe_in_double_quotes_does_not_disable_expansion() {
        // Q3: an apostrophe inside double quotes must not flip into single-quote
        // mode and suppress the following $(...). Robust to whatever $SHELL emits:
        // the `$(` must be gone because the substitution was performed.
        let out = expand_subshells("\"a's $(echo X)\"");
        assert!(!out.contains("$("), "subshell not expanded: {out}");
    }
}
