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

/// Check if a line is a variable assignment: VAR=value
pub fn is_var_assignment(input: &str) -> Option<(&str, &str)> {
    // Must not start with = and must contain =
    // Must start with letter or _, followed by alphanumeric/_
    let bytes = input.as_bytes();
    if bytes.is_empty() || bytes[0] == b'=' {
        return None;
    }

    let eq_pos = input.find('=')?;
    let key = &input[..eq_pos];

    // Validate key: must be valid identifier
    if !key
        .chars()
        .all(|c| c.is_alphanumeric() || c == '_')
        || key.chars().next().map_or(true, |c| c.is_ascii_digit())
    {
        return None;
    }

    let val = &input[eq_pos + 1..];
    // Strip surrounding quotes if present
    let val = val
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .or_else(|| val.strip_prefix('\'').and_then(|v| v.strip_suffix('\'')))
        .unwrap_or(val);

    Some((key, val))
}

/// Expand $VAR and ${VAR} references using shell vars + env
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
                if chars[i] == '{' {
                    // ${VAR}
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
                } else if chars[i] == '?' {
                    // $? → last exit code (handled elsewhere via env)
                    if let Some(val) = vars.resolve("?") {
                        result.push_str(&val);
                    }
                    i += 1;
                } else {
                    // $VAR
                    let start = i;
                    while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                        i += 1;
                    }
                    let name: String = chars[start..i].iter().collect();
                    if let Some(val) = vars.resolve(&name) {
                        result.push_str(&val);
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

/// Parse and evaluate `for VAR in ITEMS; do BODY; done` (single-line)
/// Returns Some(list of body lines to execute) or None if not a for loop.
pub fn parse_for_loop(input: &str) -> Option<ForLoop> {
    let trimmed = input.trim();
    if !trimmed.starts_with("for ") {
        return None;
    }

    // for VAR in ITEM1 ITEM2 ...; do BODY; done
    let rest = &trimmed[4..];
    let in_pos = rest.find(" in ")?;
    let var = rest[..in_pos].trim().to_string();
    let after_in = &rest[in_pos + 4..];

    // Find "; do " or " do "
    let do_pos = after_in.find("; do ").or_else(|| after_in.find(" do "))?;
    let items_str = &after_in[..do_pos].trim();
    let do_offset = if after_in[do_pos..].starts_with("; do ") { 5 } else { 4 };
    let body_and_done = &after_in[do_pos + do_offset..];

    // Find "; done" or " done"
    let done_str = body_and_done.trim_end();
    let body = done_str.strip_suffix("; done")
        .or_else(|| done_str.strip_suffix(";done"))
        .or_else(|| done_str.strip_suffix(" done"))
        .or_else(|| done_str.strip_suffix("done"))?;

    let items: Vec<String> = items_str.split_whitespace().map(|s| s.to_string()).collect();

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
    let body_section = fi_trimmed.strip_suffix("; fi")
        .or_else(|| fi_trimmed.strip_suffix(";fi"))
        .or_else(|| fi_trimmed.strip_suffix(" fi"))?;

    // Check for else
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
