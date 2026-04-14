use std::collections::HashMap;
use std::path::Path;

/// Tokenize raw input into arguments, handling quotes and escapes.
pub fn tokenize(input: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;
    let mut was_quoted = false;

    for ch in input.chars() {
        if escape {
            current.push(ch);
            escape = false;
            continue;
        }
        match ch {
            '\\' if !in_single => escape = true,
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
                    tokens.push(std::mem::take(&mut current));
                    was_quoted = false;
                }
            }
            _ => current.push(ch),
        }
    }
    if !current.is_empty() || was_quoted {
        tokens.push(current);
    }
    tokens
}

/// Expand ~ and $ENV_VAR in tokens
pub fn expand_vars(tokens: &mut Vec<String>) {
    for token in tokens.iter_mut() {
        // Tilde expansion
        if token.starts_with('~') {
            *token = shellexpand::tilde(token).to_string();
        }
        // $VAR expansion
        *token = shellexpand::env(token).unwrap_or_else(|_| token.clone().into()).to_string();
    }
}

/// Resolve aliases (non-recursive, single pass)
pub fn resolve_alias(tokens: &mut Vec<String>, aliases: &HashMap<String, String>) {
    if tokens.is_empty() {
        return;
    }
    if let Some(expansion) = aliases.get(&tokens[0]) {
        let mut expanded = tokenize(expansion);
        expanded.extend(tokens.drain(1..));
        *tokens = expanded;
    }
}

/// A single command in a pipeline
#[derive(Debug)]
pub struct Command {
    pub args: Vec<String>,
    pub stdin_redirect: Option<String>,
    pub stdout_redirect: Option<Redirect>,
    pub stderr_redirect: Option<Redirect>,
    pub background: bool,
}

#[derive(Debug)]
pub enum Redirect {
    Truncate(String),
    Append(String),
}

/// Parse a full input line into a pipeline of commands
pub fn parse_pipeline(input: &str) -> Vec<Command> {
    let segments: Vec<&str> = split_on_pipes(input);
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
        let (stdin_redirect, stdout_redirect, stderr_redirect) = extract_redirects(&mut tokens);

        commands.push(Command {
            args: tokens,
            stdin_redirect,
            stdout_redirect,
            stderr_redirect,
            background,
        });
    }
    commands
}

/// Split input on unquoted `|` characters
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
            '\\' => escape = true,
            '\'' if !in_double => in_single = !in_single,
            '"' if !in_single => in_double = !in_double,
            '|' if !in_single && !in_double => {
                segments.push(&input[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    segments.push(&input[start..]);
    segments
}

/// Extract redirect operators from tokens: <, >, >>
fn extract_redirects(
    tokens: &mut Vec<String>,
) -> (Option<String>, Option<Redirect>, Option<Redirect>) {
    let mut stdin = None;
    let mut stdout = None;
    let mut stderr = None;
    let mut i = 0;

    while i < tokens.len() {
        let token = &tokens[i];
        if token == "<" && i + 1 < tokens.len() {
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
    (stdin, stdout, stderr)
}

/// Check if input looks like a directory path (for auto_cd)
pub fn looks_like_directory(input: &str) -> bool {
    let expanded = shellexpand::tilde(input).to_string();
    Path::new(&expanded).is_dir()
}

/// Expand glob patterns in tokens (*, ?, **)
pub fn expand_globs(tokens: &mut Vec<String>) {
    let mut expanded = Vec::with_capacity(tokens.len());
    for token in tokens.drain(..) {
        if contains_glob_chars(&token) {
            match glob::glob(&token) {
                Ok(paths) => {
                    let mut matched: Vec<String> = paths
                        .filter_map(|p| p.ok())
                        .map(|p| p.display().to_string())
                        .collect();
                    if matched.is_empty() {
                        // No matches: keep the literal pattern (like bash NOMATCH)
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

/// Expand `$(command)` and backtick `` `command` `` substitutions in a string.
/// Runs each captured command via the system shell and replaces with its stdout.
pub fn expand_subshells(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let bytes = input.as_bytes();
    let mut i = 0;
    let mut in_single = false;

    while i < bytes.len() {
        match bytes[i] {
            // Single-quoted strings: no expansion
            b'\'' => {
                in_single = !in_single;
                result.push('\'');
                i += 1;
            }
            // $( ... ) substitution
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
            // Backtick substitution
            b'`' if !in_single => {
                i += 1;
                let start = i;
                while i < bytes.len() && bytes[i] != b'`' {
                    i += 1;
                }
                let cmd = &input[start..i];
                if i < bytes.len() {
                    i += 1; // skip closing `
                }
                result.push_str(&run_subshell(cmd));
            }
            b'\\' if !in_single => {
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
fn run_subshell(cmd: &str) -> String {
    let cmd = cmd.trim();
    if cmd.is_empty() {
        return String::new();
    }
    match std::process::Command::new("sh")
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
