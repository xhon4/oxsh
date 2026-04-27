use std::collections::HashMap;
use std::env;

/// Shell-local variables (not exported to env unless `export`ed)
pub struct ShellVars {
    vars: HashMap<String, String>,
}

impl ShellVars {
    pub fn new() -> Self {
        Self {
            vars: HashMap::new(),
        }
    }

    pub fn set(&mut self, key: &str, val: &str) {
        self.vars.insert(key.to_string(), val.to_string());
    }

    #[allow(dead_code)]
    pub fn get(&self, key: &str) -> Option<&str> {
        self.vars.get(key).map(|s| s.as_str())
    }

    /// Get from shell vars first, then fall back to env
    pub fn resolve(&self, key: &str) -> Option<String> {
        self.vars
            .get(key)
            .cloned()
            .or_else(|| env::var(key).ok())
    }
}

/// Check if a line is a variable assignment: VAR=value or VAR=$(cmd) etc.
///
/// Returns Some((key, raw_value_str)) where raw_value_str is the raw RHS
/// (not yet expanded — caller is responsible for expansion).
///
/// We only validate the LHS identifier. We do NOT strip quotes here because
/// the caller may need to expand subshells inside the value first.
pub fn is_var_assignment(input: &str) -> Option<(&str, &str)> {
    let bytes = input.as_bytes();
    if bytes.is_empty() || bytes[0] == b'=' {
        return None;
    }

    // Find the first `=` that is not inside quotes or a subshell.
    // We only look at the identifier characters before the `=`.
    let eq_pos = input.find('=')?;
    let key = &input[..eq_pos];

    // Validate key: must be valid shell identifier (letter/_ first, then alnum/_)
    if key.is_empty() {
        return None;
    }
    let mut chars = key.chars();
    let first = chars.next().unwrap();
    if !first.is_ascii_alphabetic() && first != '_' {
        return None;
    }
    if !chars.all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }

    let val = &input[eq_pos + 1..];
    Some((key, val))
}

/// Strip surrounding quotes from an already-expanded value string.
/// Call this after subshell/variable expansion if the value came from a literal.
pub fn strip_quotes(val: &str) -> &str {
    val.strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| val.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
        .unwrap_or(val)
}

/// Expand $VAR, ${VAR}, and special variables in tokens using shell vars + env.
///
/// Special variables supported:
///   $?  last exit code
///   $$  current process PID
///   $0  shell name (oxsh)
///   $#  number of positional parameters (stored in shell var "#")
///   $@  all positional parameters (stored in shell var "@")
///   $*  all positional parameters as single word (same as $@)
///   $!  last background process PID (stored in shell var "!")
///   $1..$9  positional parameters
pub fn expand_shell_vars(tokens: &mut Vec<String>, vars: &ShellVars) {
    for token in tokens.iter_mut() {
        if !token.contains('$') {
            continue;
        }
        let mut result = String::with_capacity(token.len());
        let chars: Vec<char> = token.chars().collect();
        let mut i = 0;

        while i < chars.len() {
            if chars[i] == '$' && i + 1 < chars.len() {
                i += 1;
                match chars[i] {
                    '{' => {
                        // ${VAR} or ${VAR:-default} (default not yet supported — just expand)
                        i += 1;
                        let start = i;
                        while i < chars.len() && chars[i] != '}' {
                            i += 1;
                        }
                        let name: String = chars[start..i].iter().collect();
                        if i < chars.len() {
                            i += 1; // skip }
                        }
                        if let Some(val) = vars.resolve(&name) {
                            result.push_str(&val);
                        }
                    }
                    '?' => {
                        result.push_str(vars.resolve("?").as_deref().unwrap_or("0"));
                        i += 1;
                    }
                    '$' => {
                        // $$ → current process PID
                        result.push_str(&std::process::id().to_string());
                        i += 1;
                    }
                    '!' => {
                        // $! → last background PID
                        result.push_str(vars.resolve("!").as_deref().unwrap_or(""));
                        i += 1;
                    }
                    '#' => {
                        // $# → number of positional parameters
                        result.push_str(vars.resolve("#").as_deref().unwrap_or("0"));
                        i += 1;
                    }
                    '@' | '*' => {
                        // $@ / $* → all positional parameters
                        result.push_str(vars.resolve("@").as_deref().unwrap_or(""));
                        i += 1;
                    }
                    '0' => {
                        // $0 → shell name
                        result.push_str(vars.resolve("0").as_deref().unwrap_or("oxsh"));
                        i += 1;
                    }
                    c if c.is_ascii_digit() => {
                        // $1..$9 → positional parameters
                        let digit = c.to_string();
                        result.push_str(vars.resolve(&digit).as_deref().unwrap_or(""));
                        i += 1;
                    }
                    _ => {
                        // $VAR
                        let start = i;
                        while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                            i += 1;
                        }
                        let name: String = chars[start..i].iter().collect();
                        if name.is_empty() {
                            result.push('$');
                        } else if let Some(val) = vars.resolve(&name) {
                            result.push_str(&val);
                        }
                    }
                }
            } else {
                result.push(chars[i]);
                i += 1;
            }
        }
        *token = result;
    }
}

// ── Control flow structures ──

/// Parse and evaluate `for VAR in ITEMS; do BODY; done` (single-line)
pub fn parse_for_loop(input: &str) -> Option<ForLoop> {
    let trimmed = input.trim();
    if !trimmed.starts_with("for ") {
        return None;
    }

    let rest = &trimmed[4..];
    let in_pos = rest.find(" in ")?;
    let var = rest[..in_pos].trim().to_string();
    let after_in = &rest[in_pos + 4..];

    let do_pos = after_in.find("; do ").or_else(|| after_in.find(" do "))?;
    let items_str = after_in[..do_pos].trim();
    let do_offset = if after_in[do_pos..].starts_with("; do ") { 5 } else { 4 };
    let body_and_done = &after_in[do_pos + do_offset..];

    let done_str = body_and_done.trim_end();
    let body = done_str
        .strip_suffix("; done")
        .or_else(|| done_str.strip_suffix(";done"))
        .or_else(|| done_str.strip_suffix(" done"))
        .or_else(|| done_str.strip_suffix("done"))?;

    let items: Vec<String> = items_str
        .split_whitespace()
        .map(|s| s.to_string())
        .collect();

    Some(ForLoop {
        var,
        items,
        body: body.trim().to_string(),
    })
}

pub struct ForLoop {
    pub var: String,
    pub items: Vec<String>,
    pub body: String,
}

/// Parse `while COND; do BODY; done` (single-line)
pub fn parse_while_loop(input: &str) -> Option<WhileLoop> {
    let trimmed = input.trim();
    if !trimmed.starts_with("while ") {
        return None;
    }

    let rest = &trimmed[6..];

    // Find "; do " or " do "
    let do_pos = rest.find("; do ").or_else(|| rest.find(" do "))?;
    let condition = rest[..do_pos].trim().to_string();
    let do_offset = if rest[do_pos..].starts_with("; do ") { 5 } else { 4 };
    let body_and_done = &rest[do_pos + do_offset..];

    let done_str = body_and_done.trim_end();
    let body = done_str
        .strip_suffix("; done")
        .or_else(|| done_str.strip_suffix(";done"))
        .or_else(|| done_str.strip_suffix(" done"))
        .or_else(|| done_str.strip_suffix("done"))?;

    Some(WhileLoop {
        condition,
        body: body.trim().to_string(),
    })
}

pub struct WhileLoop {
    pub condition: String,
    pub body: String,
}

/// Parse `if COND; then BODY; fi` or `if COND; then BODY; else ELSE; fi`
pub fn parse_if(input: &str) -> Option<IfBlock> {
    let trimmed = input.trim();
    if !trimmed.starts_with("if ") {
        return None;
    }
    let rest = &trimmed[3..];

    let then_pos = rest.find("; then ")?;
    let condition = rest[..then_pos].trim().to_string();
    let after_then = &rest[then_pos + 7..];

    let fi_trimmed = after_then.trim_end();
    let body_section = fi_trimmed
        .strip_suffix("; fi")
        .or_else(|| fi_trimmed.strip_suffix(";fi"))
        .or_else(|| fi_trimmed.strip_suffix(" fi"))?;

    if let Some(else_pos) = body_section.find("; else ") {
        let then_body = body_section[..else_pos].trim().to_string();
        let else_body = body_section[else_pos + 7..].trim().to_string();
        Some(IfBlock {
            condition,
            then_body,
            else_body: Some(else_body),
        })
    } else {
        Some(IfBlock {
            condition,
            then_body: body_section.trim().to_string(),
            else_body: None,
        })
    }
}

pub struct IfBlock {
    pub condition: String,
    pub then_body: String,
    pub else_body: Option<String>,
}
