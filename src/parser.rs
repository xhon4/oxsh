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

/// Expand ~ and $ENV_VAR in tokens
pub fn expand_vars(tokens: &mut Vec<String>) {
    for token in tokens.iter_mut() {
        if token.starts_with('~') {
            *token = shellexpand::tilde(token).to_string();
        }
        *token = shellexpand::env(token).unwrap_or_else(|_| token.clone().into()).to_string();
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
        let first_expanded = tokenize(expansion).into_iter().next();
        // Break self-reference (e.g. `alias ls='ls --color'`)
        if first_expanded.as_deref() == Some(tokens[0].as_str()) {
            let mut expanded = tokenize(expansion);
            expanded.extend(tokens.drain(1..));
            *tokens = expanded;
            return;
        }
        let mut expanded = tokenize(expansion);
        expanded.extend(tokens.drain(1..));
        *tokens = expanded;
        resolve_alias_depth(tokens, aliases, depth + 1);
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
        .enumerate()
        .map(|(_, mut tokens)| {
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

/// Expand glob patterns without quote awareness.
#[allow(dead_code)]
pub fn expand_globs(tokens: &mut Vec<String>) {
    let flags = vec![false; tokens.len()];
    expand_globs_respecting_quotes(tokens, &flags);
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
pub fn expand_subshells(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut in_single = false;

    while i < bytes.len() {
        match bytes[i] {
            b'\'' => {
                in_single = !in_single;
                result.push('\'');
                i += 1;
            }
            b'$' if !in_single && i + 1 < bytes.len() && bytes[i + 1] == b'(' => {
                i += 2; // skip $(
                let start = i;
                let mut depth = 1;
                let mut sq = false;
                let mut dq = false;
                while i < bytes.len() && depth > 0 {
                    match bytes[i] {
                        b'\'' if !dq => sq = !sq,
                        b'"' if !sq => dq = !dq,
                        b'(' if !sq && !dq => depth += 1,
                        b')' if !sq && !dq => depth -= 1,
                        _ => {}
                    }
                    if depth > 0 {
                        i += 1;
                    }
                }
                let cmd = &input[start..i];
                if i < bytes.len() {
                    i += 1; // skip closing )
                }
                result.push_str(&run_subshell(cmd));
            }
            b'`' if !in_single => {
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != b'`' {
                    i += 1;
                }
                let cmd = &input[start..i];
                if i < bytes.len() {
                    i += 1;
                }
                result.push_str(&run_subshell(cmd));
            }
            b'\\' if !in_single => {
                // Preserve the escape so the tokenizer downstream handles it correctly
                result.push('\\');
                i += 1;
                if i < bytes.len() {
                    result.push(bytes[i] as char);
                    i += 1;
                }
            }
            _ => {
                result.push(bytes[i] as char);
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
        Err(_) => String::new(),
    }
}
