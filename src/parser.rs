//! Executor-facing command types. Lexing lives in `syntax`, expansion in
//! `expand` (issue #10); this module keeps the types the executor consumes.

use std::path::Path;

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

/// Check if input looks like a directory path (for auto_cd)
pub fn looks_like_directory(input: &str) -> bool {
    let expanded = shellexpand::tilde(input).to_string();
    Path::new(&expanded).is_dir()
}
