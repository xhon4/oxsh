use nu_ansi_term::{Color, Style};
use reedline::{Highlighter, StyledText};
use std::cell::RefCell;
use std::collections::HashMap;

pub struct OxshHighlighter {
    cache: RefCell<HashMap<String, bool>>,
}

impl OxshHighlighter {
    pub fn new() -> Self {
        let mut cache = HashMap::with_capacity(256);
        // Pre-populate with all known builtins + structured commands (instant, no I/O)
        for &b in crate::builtins::BUILTIN_NAMES {
            cache.insert(b.to_string(), true);
        }
        for &s in crate::structured::STRUCTURED_COMMANDS {
            cache.insert(s.to_string(), true);
        }
        Self {
            cache: RefCell::new(cache),
        }
    }

    /// Seed cache with known-good commands (from PATH scan). Eliminates first-keystroke lag.
    pub fn seed_commands(&self, commands: &[String]) {
        let mut cache = self.cache.borrow_mut();
        for cmd in commands {
            cache.entry(cmd.clone()).or_insert(true);
        }
    }

    fn cmd_exists(&self, cmd: &str) -> bool {
        // Fast path: check cache (no I/O)
        if let Some(&exists) = self.cache.borrow().get(cmd) {
            return exists;
        }
        // Slow path: filesystem check, then cache forever
        let exists = which::which(cmd).is_ok();
        self.cache.borrow_mut().insert(cmd.to_string(), exists);
        exists
    }
}

impl Highlighter for OxshHighlighter {
    fn highlight(&self, line: &str, _cursor: usize) -> StyledText {
        let mut styled = StyledText::new();
        if line.is_empty() {
            return styled;
        }

        // Split line into segments by pipes to highlight each command
        self.highlight_pipeline(&mut styled, line);
        styled
    }
}

impl OxshHighlighter {
    fn highlight_pipeline(&self, styled: &mut StyledText, line: &str) {
        let segments = split_pipe_segments(line);
        for (_i, (segment, separator)) in segments.iter().enumerate() {
            // Skip leading whitespace, but preserve it
            let trimmed_start = segment.len() - segment.trim_start().len();
            if trimmed_start > 0 {
                styled.push((Style::new(), segment[..trimmed_start].to_string()));
            }
            let trimmed = segment.trim_start();

            if trimmed.is_empty() {
                if let Some(sep) = separator {
                    styled.push((Style::new().fg(Color::Blue).bold(), sep.clone()));
                }
                continue;
            }

            // Split into command + rest
            let mut parts = trimmed.splitn(2, |c: char| c.is_whitespace());
            if let Some(cmd) = parts.next() {
                let cmd_style = if cmd.contains('/') || cmd.starts_with('.') {
                    if std::path::Path::new(cmd).exists() {
                        Style::new().fg(Color::Green).bold()
                    } else {
                        Style::new().fg(Color::Red)
                    }
                } else if self.cmd_exists(cmd) {
                    Style::new().fg(Color::Green).bold()
                } else {
                    Style::new().fg(Color::Red)
                };
                styled.push((cmd_style, cmd.to_string()));

                if let Some(rest) = parts.next() {
                    let ws_len = trimmed.len() - cmd.len() - rest.len();
                    let ws = &trimmed[cmd.len()..cmd.len() + ws_len];
                    styled.push((Style::new(), ws.to_string()));
                    highlight_args(styled, rest);
                }
            }

            if let Some(sep) = separator {
                styled.push((Style::new().fg(Color::Blue).bold(), sep.clone()));
            }
        }
    }
}

/// Split line into (segment, separator) pairs where separator is |, &&, ||, or ;
fn split_pipe_segments(input: &str) -> Vec<(String, Option<String>)> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut in_single = false;
    let mut in_double = false;
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'\'' if !in_double => {
                in_single = !in_single;
                current.push('\'');
                i += 1;
            }
            b'"' if !in_single => {
                in_double = !in_double;
                current.push('"');
                i += 1;
            }
            b'\\' if !in_single => {
                current.push('\\');
                i += 1;
                if i < bytes.len() {
                    current.push(bytes[i] as char);
                    i += 1;
                }
            }
            b'|' if !in_single && !in_double => {
                if i + 1 < bytes.len() && bytes[i + 1] == b'|' {
                    segments.push((std::mem::take(&mut current), Some("||".to_string())));
                    i += 2;
                } else {
                    segments.push((std::mem::take(&mut current), Some("|".to_string())));
                    i += 1;
                }
            }
            b'&' if !in_single && !in_double && i + 1 < bytes.len() && bytes[i + 1] == b'&' => {
                segments.push((std::mem::take(&mut current), Some("&&".to_string())));
                i += 2;
            }
            b';' if !in_single && !in_double => {
                segments.push((std::mem::take(&mut current), Some(";".to_string())));
                i += 1;
            }
            _ => {
                current.push(bytes[i] as char);
                i += 1;
            }
        }
    }
    segments.push((current, None));
    segments
}

fn highlight_args(styled: &mut StyledText, args: &str) {
    let flag_style = Style::new().fg(Color::Yellow);
    let string_style = Style::new().fg(Color::Cyan);
    let var_style = Style::new().fg(Color::Magenta);
    let number_style = Style::new().fg(Color::Cyan);
    let redirect_style = Style::new().fg(Color::Blue).bold();
    let default_style = Style::new().fg(Color::White);

    let bytes = args.as_bytes();
    let mut i = 0;
    let mut prev_ws = true; // track if previous char was whitespace (for flags/numbers)

    while i < bytes.len() {
        let ch = bytes[i];
        match ch {
            // Quoted strings
            b'"' | b'\'' => {
                let quote = ch;
                let start = i;
                i += 1;
                while i < bytes.len() && bytes[i] != quote {
                    if bytes[i] == b'\\' {
                        i += 1;
                    }
                    i += 1;
                }
                if i < bytes.len() {
                    i += 1;
                }
                styled.push((string_style, args[start..i].to_string()));
                prev_ws = false;
            }
            // Redirects only (pipes/chains handled by pipeline splitter)
            b'>' | b'<' => {
                let start = i;
                if i + 1 < bytes.len() && bytes[i + 1] == ch {
                    i += 2;
                } else {
                    i += 1;
                }
                styled.push((redirect_style, args[start..i].to_string()));
                prev_ws = false;
            }
            // Variables $VAR or ${VAR}
            b'$' => {
                let start = i;
                i += 1;
                if i < bytes.len() && bytes[i] == b'{' {
                    while i < bytes.len() && bytes[i] != b'}' {
                        i += 1;
                    }
                    if i < bytes.len() {
                        i += 1;
                    }
                } else {
                    while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'?') {
                        i += 1;
                    }
                }
                styled.push((var_style, args[start..i].to_string()));
                prev_ws = false;
            }
            // Flags: -x, --flag
            b'-' if prev_ws => {
                let start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                styled.push((flag_style, args[start..i].to_string()));
                prev_ws = false;
            }
            // Whitespace
            c if c.is_ascii_whitespace() => {
                styled.push((default_style, (c as char).to_string()));
                i += 1;
                prev_ws = true;
            }
            // Numbers (standalone)
            c if c.is_ascii_digit() && prev_ws => {
                let start = i;
                while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                    i += 1;
                }
                if i >= bytes.len() || bytes[i].is_ascii_whitespace() {
                    styled.push((number_style, args[start..i].to_string()));
                } else {
                    while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                        i += 1;
                    }
                    styled.push((default_style, args[start..i].to_string()));
                }
                prev_ws = false;
            }
            // Default word
            _ => {
                let start = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                styled.push((default_style, args[start..i].to_string()));
                prev_ws = false;
            }
        }
    }
}
