use std::collections::HashMap;
use std::path::Path;

/// A token retains lexical syntax and the expansion behavior of each fragment.
#[derive(Clone, Debug)]
pub struct Word {
    pub fragments: Vec<Fragment>,
    pub operator: bool,
    pub fully_quoted: bool,
}

#[derive(Clone, Debug)]
pub struct Fragment {
    pub text: String,
    pub expand_vars: bool,
}

impl Word {
    pub fn text(&self) -> String {
        self.fragments.iter().map(|part| part.text.as_str()).collect()
    }

    pub fn literal(text: String) -> Self {
        Self {
            fragments: vec![Fragment {
                text,
                expand_vars: false,
            }],
            operator: false,
            fully_quoted: false,
        }
    }
}

/// Tokenize raw input into arguments, handling quotes, escapes and substitutions.
#[allow(dead_code)]
pub fn tokenize(input: &str) -> Vec<String> {
    tokenize_words(input).into_iter().map(|word| word.text()).collect()
}

/// Tokenize input and also return a parallel bool vector indicating which
/// tokens were entirely enclosed in quotes. Used to suppress glob expansion
/// on tokens like `"*.txt"` where the user intended a literal string.
#[allow(dead_code)]
pub fn tokenize_with_quote_flags(input: &str) -> (Vec<String>, Vec<bool>) {
    let words = tokenize_words(input);
    (
        words.iter().map(Word::text).collect(),
        words.iter().map(|word| word.fully_quoted).collect(),
    )
}

/// Lex input into words whose fragments carry expansion and syntax provenance.
pub fn tokenize_words(input: &str) -> Vec<Word> {
    let chars: Vec<char> = input.chars().collect();
    let mut tokens = Vec::new();
    let mut current = Word {
        fragments: Vec::new(),
        operator: false,
        fully_quoted: false,
    };
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;
    let mut was_quoted = false;
    let mut has_unquoted_char = false;
    let mut operator_eligible = true;
    let mut i = 0;

    while i < chars.len() {
        let ch = chars[i];
        if escape {
            push_fragment(&mut current, ch, ch != '$');
            escape = false;
            operator_eligible = false;
            has_unquoted_char = true;
            i += 1;
            continue;
        }
        if !in_single && ch == '$' && chars.get(i + 1) == Some(&'(') {
            if let Some(end) = find_subshell_end(&chars, i + 2) {
                let command: String = chars[i + 2..end].iter().collect();
                push_text_fragment(&mut current, &run_subshell(&command), false);
                operator_eligible = false;
                if !in_double {
                    has_unquoted_char = true;
                }
                i = end + 1;
                continue;
            }
        }
        if !in_single && ch == '`' {
            let start = i + 1;
            let mut end = start;
            while end < chars.len() && chars[end] != '`' {
                if chars[end] == '\\' {
                    end += 1;
                }
                end += 1;
            }
            if end < chars.len() {
                let command: String = chars[start..end].iter().collect();
                push_text_fragment(&mut current, &run_subshell(&command), false);
                operator_eligible = false;
                if !in_double {
                    has_unquoted_char = true;
                }
                i = end + 1;
                continue;
            }
        }
        match ch {
            '\\' if !in_single => {
                escape = true;
                has_unquoted_char = true;
            }
            '\'' if !in_double => {
                in_single = !in_single;
                was_quoted = true;
                operator_eligible = false;
            }
            '"' if !in_single => {
                in_double = !in_double;
                was_quoted = true;
                operator_eligible = false;
            }
            ' ' | '\t' if !in_single && !in_double => {
                if !current.fragments.is_empty() || was_quoted {
                    finish_word(
                        &mut tokens,
                        &mut current,
                        was_quoted && !has_unquoted_char,
                        operator_eligible,
                    );
                    was_quoted = false;
                    has_unquoted_char = false;
                    operator_eligible = true;
                }
            }
            _ => {
                if !in_single && !in_double {
                    has_unquoted_char = true;
                }
                push_fragment(&mut current, ch, !in_single);
            }
        }
        i += 1;
    }
    if !current.fragments.is_empty() || was_quoted {
        finish_word(
            &mut tokens,
            &mut current,
            was_quoted && !has_unquoted_char,
            operator_eligible,
        );
    }
    tokens
}

fn push_fragment(word: &mut Word, ch: char, expand_vars: bool) {
    if let Some(last) = word
        .fragments
        .last_mut()
        .filter(|last| last.expand_vars == expand_vars)
    {
        last.text.push(ch);
    } else {
        word.fragments.push(Fragment {
            text: ch.to_string(),
            expand_vars,
        });
    }
}

fn push_text_fragment(word: &mut Word, text: &str, expand_vars: bool) {
    if text.is_empty() {
        return;
    }
    if let Some(last) = word
        .fragments
        .last_mut()
        .filter(|last| last.expand_vars == expand_vars)
    {
        last.text.push_str(text);
    } else {
        word.fragments.push(Fragment {
            text: text.to_string(),
            expand_vars,
        });
    }
}

fn finish_word(
    tokens: &mut Vec<Word>,
    word: &mut Word,
    fully_quoted: bool,
    operator_eligible: bool,
) {
    let text = word.text();
    tokens.push(Word {
        fragments: std::mem::take(&mut word.fragments),
        operator: operator_eligible && is_shell_operator(&text),
        fully_quoted,
    });
    word.operator = false;
    word.fully_quoted = false;
}

fn find_subshell_end(chars: &[char], start: usize) -> Option<usize> {
    let mut depth = 1usize;
    let mut single = false;
    let mut double = false;
    let mut i = start;
    while i < chars.len() {
        match chars[i] {
            '\\' if !single => i += 1,
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            '(' if !single && !double => depth += 1,
            ')' if !single && !double => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

fn is_shell_operator(token: &str) -> bool {
    matches!(token, "|" | "&" | "<" | ">" | ">>" | "2>" | "2>>" | "2>&1")
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
#[allow(dead_code)]
pub fn resolve_alias(tokens: &mut Vec<String>, aliases: &HashMap<String, String>) {
    resolve_alias_depth(tokens, aliases, 0);
}

/// Resolve aliases without discarding syntax or fragment metadata.
pub fn resolve_word_aliases(words: &mut Vec<Word>, aliases: &HashMap<String, String>) {
    resolve_word_aliases_depth(words, aliases, 0);
}

fn resolve_word_aliases_depth(
    words: &mut Vec<Word>,
    aliases: &HashMap<String, String>,
    depth: usize,
) {
    if words.is_empty() || depth > 16 {
        return;
    }
    let name = words[0].text();
    if let Some(expansion) = aliases.get(&name) {
        let mut expanded = tokenize_words(expansion);
        let self_ref = expanded.first().is_some_and(|word| word.text() == name);
        expanded.extend(words.drain(1..));
        *words = expanded;
        if !self_ref {
            resolve_word_aliases_depth(words, aliases, depth + 1);
        }
    }
}

#[allow(dead_code)]
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

/// Parse commands while retaining each word's original lexical metadata.
pub fn parse_pipeline_from_words(token_groups: Vec<Vec<Word>>) -> Vec<Command> {
    token_groups
        .into_iter()
        .map(|mut words| {
            let background = words.last().is_some_and(|word| word.operator && word.text() == "&");
            if background {
                words.pop();
            }
            let (stdin_redirect, stdout_redirect, stderr_redirect, merge_stderr) =
                extract_word_redirects(&mut words);
            Command {
                args: words.into_iter().map(|word| word.text()).collect(),
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

        let mut tokens = tokenize_words(cmd_str);
        let (stdin_redirect, stdout_redirect, stderr_redirect, merge_stderr) =
            extract_word_redirects(&mut tokens);

        commands.push(Command {
            args: tokens.into_iter().map(|word| word.text()).collect(),
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
#[cfg(test)]
fn extract_redirects(
    tokens: &mut Vec<String>,
) -> (Option<String>, Option<Redirect>, Option<Redirect>, bool) {
    let mut words: Vec<Word> = tokens
        .drain(..)
        .map(|text| {
            let operator = is_shell_operator(&text);
            Word {
                fragments: vec![Fragment {
                    text,
                    expand_vars: false,
                }],
                operator,
                fully_quoted: false,
            }
        })
        .collect();
    let result = extract_word_redirects(&mut words);
    *tokens = words.into_iter().map(|word| word.text()).collect();
    result
}

fn extract_word_redirects(
    words: &mut Vec<Word>,
) -> (Option<String>, Option<Redirect>, Option<Redirect>, bool) {
    let mut stdin = None;
    let mut stdout = None;
    let mut stderr = None;
    let mut merge_stderr = false;
    let mut i = 0;

    while i < words.len() {
        let token = words[i].text();
        let is_operator = words[i].operator;
        let remove = if is_operator && token == "2>&1" {
            merge_stderr = true;
            1
        } else if is_operator && token == "<" && i + 1 < words.len() {
            stdin = Some(words[i + 1].text());
            2
        } else if is_operator && token == ">>" && i + 1 < words.len() {
            stdout = Some(Redirect::Append(words[i + 1].text()));
            2
        } else if is_operator && token == ">" && i + 1 < words.len() {
            stdout = Some(Redirect::Truncate(words[i + 1].text()));
            2
        } else if is_operator && token == "2>" && i + 1 < words.len() {
            stderr = Some(Redirect::Truncate(words[i + 1].text()));
            2
        } else if is_operator && token == "2>>" && i + 1 < words.len() {
            stderr = Some(Redirect::Append(words[i + 1].text()));
            2
        } else {
            0
        };
        if remove > 0 {
            words.drain(i..i + remove);
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
#[allow(dead_code)]
pub fn expand_braces(tokens: &mut Vec<String>) {
    let mut result = Vec::with_capacity(tokens.len());
    for token in tokens.drain(..) {
        result.extend(expand_brace_token(&token));
    }
    *tokens = result;
}

/// Expand brace words while marking generated words as data, not syntax.
pub fn expand_brace_words(words: &mut Vec<Word>) {
    let mut result = Vec::with_capacity(words.len());
    for word in words.drain(..) {
        let text = word.text();
        let expanded = expand_brace_token(&text);
        if expanded.len() == 1 && expanded[0] == text {
            result.push(word);
        } else {
            result.extend(expanded.into_iter().map(Word::literal));
        }
    }
    *words = result;
}

/// Expand globs without promoting generated paths to shell syntax.
pub fn expand_glob_words(words: &mut Vec<Word>) {
    let mut result = Vec::with_capacity(words.len());
    for word in words.drain(..) {
        if word.fully_quoted { result.push(word); continue; }
        let text = word.text();
        let mut expanded = vec![text.clone()];
        expand_globs_respecting_quotes(&mut expanded, &[false]);
        if expanded.len() == 1 && expanded[0] == text {
            result.push(word);
        } else {
            result.extend(expanded.into_iter().map(Word::literal));
        }
    }
    *words = result;
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
#[allow(dead_code)]
pub fn expand_subshells(input: &str) -> String {
    // Fast-path: avoid Vec<char> allocation when no expansion markers exist (P4).
    if !input.contains('$') && !input.contains('`') {
        return input.to_string();
    }
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
