use crate::{builtins, config::Config, context, executor, parser, prompt::OxshPrompt, scripting, structured};
use reedline::{Reedline, Signal};
use std::collections::HashMap;
use std::time::Instant;

/// Suggest a correction for a mistyped command using Damerau-Levenshtein distance.
/// Returns the best match if close enough (distance <= 2).
fn suggest_correction(cmd: &str) -> Option<String> {
    // Don't suggest for paths or very short inputs
    if cmd.len() < 2 || cmd.contains('/') {
        return None;
    }

    let mut best: Option<(String, usize)> = None;
    let threshold = if cmd.len() <= 3 { 1 } else { 2 };

    // Check builtins
    for &name in builtins::BUILTIN_NAMES {
        let dist = strsim::damerau_levenshtein(cmd, name);
        if dist > 0 && dist <= threshold {
            if best.as_ref().map_or(true, |(_, d)| dist < *d) {
                best = Some((name.to_string(), dist));
            }
        }
    }

    // Check structured commands
    for &name in structured::STRUCTURED_COMMANDS {
        let dist = strsim::damerau_levenshtein(cmd, name);
        if dist > 0 && dist <= threshold {
            if best.as_ref().map_or(true, |(_, d)| dist < *d) {
                best = Some((name.to_string(), dist));
            }
        }
    }

    // Check PATH executables
    if let Ok(path) = std::env::var("PATH") {
        for dir in path.split(':') {
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    if let Some(name) = entry.file_name().to_str() {
                        let dist = strsim::damerau_levenshtein(cmd, name);
                        if dist > 0 && dist <= threshold {
                            if best.as_ref().map_or(true, |(_, d)| dist < *d) {
                                best = Some((name.to_string(), dist));
                            }
                            if dist == 1 {
                                // Can't do better than 1, stop early
                                return best.map(|(name, _)| name);
                            }
                        }
                    }
                }
            }
        }
    }

    best.map(|(name, _)| name)
}

pub struct Shell {
    pub config: Config,
    line_editor: Reedline,
    aliases: HashMap<String, String>,
    shell_vars: scripting::ShellVars,
    last_exit_code: i32,
    cmd_duration_ms: u128,
    cached_context: context::ShellContext,
    last_cwd: Option<std::path::PathBuf>,
    dir_history: Vec<String>,
    dir_index: usize,
    last_command: String,
}

impl Shell {
    pub fn new(config: Config, line_editor: Reedline) -> Self {
        let cwd = std::env::current_dir().ok();
        let cwd_str = cwd.as_ref().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
        let dir_history = if cwd_str.is_empty() { vec![] } else { vec![cwd_str] };
        Self {
            aliases: config.aliases.clone(),
            config,
            line_editor,
            shell_vars: scripting::ShellVars::new(),
            last_exit_code: 0,
            cmd_duration_ms: 0,
            cached_context: context::ShellContext::detect(),
            last_cwd: cwd,
            dir_history,
            dir_index: 0,
            last_command: String::new(),
        }
    }

    pub fn run(&mut self) {
        'mainloop: loop {
            // Re-detect context only if CWD changed
            let current_cwd = std::env::current_dir().ok();
            if current_cwd != self.last_cwd {
                self.cached_context = context::ShellContext::detect();
                // Track directory history for back/next
                if let Some(ref cwd) = current_cwd {
                    let cwd_str = cwd.to_string_lossy().to_string();
                    if self.dir_history.get(self.dir_index).map(|s| s.as_str()) != Some(&cwd_str) {
                        self.dir_history.truncate(self.dir_index + 1);
                        self.dir_history.push(cwd_str);
                        self.dir_index = self.dir_history.len() - 1;
                    }
                }
                self.last_cwd = current_cwd;
            }

            let prompt = OxshPrompt::with_context(
                self.last_exit_code,
                self.cmd_duration_ms,
                self.cached_context.clone(),
                &self.config.prompt,
            );

            match self.line_editor.read_line(&prompt) {
                Ok(Signal::Success(line)) => {
                    let trimmed = line.trim();
                    if trimmed.is_empty() {
                        continue;
                    }

                    // !! expansion: replace !! with last command
                    let input = if trimmed.contains("!!") {
                        if self.last_command.is_empty() {
                            eprintln!("oxsh: no previous command");
                            continue;
                        }
                        trimmed.replace("!!", &self.last_command)
                    } else {
                        trimmed.to_string()
                    };
                    self.last_command = input.clone();

                    if self.handle_input(&input) {
                        break 'mainloop;
                    }

                    if let Err(e) = self.line_editor.sync_history() {
                        eprintln!("oxsh: history sync error: {e}");
                    }
                }
                Ok(Signal::CtrlC) => {
                    self.last_exit_code = 130;
                    self.cmd_duration_ms = 0;
                }
                Ok(Signal::CtrlD) => {
                    if let Err(e) = self.line_editor.sync_history() {
                        eprintln!("oxsh: history sync error: {e}");
                    }
                    break 'mainloop;
                }
                Ok(_) => {}
                Err(e) => {
                    eprintln!("oxsh: input error: {e}");
                    break;
                }
            }
        }
    }

    /// Process a single input line. Returns true if the shell should exit.
    fn handle_input(&mut self, input: &str) -> bool {
        // Explain mode: ?? command
        if let Some(rest) = input.strip_prefix("?? ").or_else(|| input.strip_prefix("??")) {
            let cmd = rest.trim().split_whitespace().next().unwrap_or(rest.trim());
            if !cmd.is_empty() {
                explain_command(cmd);
            }
            self.cmd_duration_ms = 0;
            return false;
        }

        // Handle clear
        if input == "clear" {
            crossterm::execute!(
                std::io::stdout(),
                crossterm::terminal::Clear(crossterm::terminal::ClearType::All),
                crossterm::cursor::MoveTo(0, 0)
            )
            .ok();
            if let Some(alias_cmd) = self.config.aliases.get("clear").cloned() {
                let pipeline = parser::parse_pipeline(&alias_cmd);
                self.last_exit_code = executor::execute_pipeline(pipeline);
            }
            self.cmd_duration_ms = 0;
            return false;
        }

        let start = Instant::now();
        let chain_segments = split_chain_ops(input);

        for (segment, op) in chain_segments {
            let segment = segment.trim();
            if segment.is_empty() {
                continue;
            }

            self.shell_vars.set("?", &self.last_exit_code.to_string());

            // For loop
            if let Some(for_loop) = scripting::parse_for_loop(segment) {
                for item in &for_loop.items {
                    self.shell_vars.set(&for_loop.var, item);
                    self.last_exit_code = self.execute_line_inner(&for_loop.body);
                }
                if should_skip(op, self.last_exit_code) { break; }
                continue;
            }

            // If block
            if let Some(if_block) = scripting::parse_if(segment) {
                let cond_code = self.execute_line_inner(&if_block.condition);
                if cond_code == 0 {
                    self.last_exit_code = self.execute_line_inner(&if_block.then_body);
                } else if let Some(ref else_body) = if_block.else_body {
                    self.last_exit_code = self.execute_line_inner(else_body);
                }
                if should_skip(op, self.last_exit_code) { break; }
                continue;
            }

            // Variable assignment
            if let Some((key, val)) = scripting::is_var_assignment(segment) {
                self.shell_vars.set(key, val);
                self.last_exit_code = 0;
                continue;
            }

            // Subshell expansion $(...) and backticks, then tokenize
            let expanded_segment = parser::expand_subshells(segment);
            let mut tokens = parser::tokenize(&expanded_segment);
            scripting::expand_shell_vars(&mut tokens, &self.shell_vars);
            parser::expand_vars(&mut tokens);
            parser::resolve_alias(&mut tokens, &self.aliases);
            parser::expand_globs(&mut tokens);

            // Auto-cd
            if self.config.shell.auto_cd
                && tokens.len() == 1
                && parser::looks_like_directory(&tokens[0])
            {
                let cd_args = vec!["cd".into(), tokens[0].clone()];
                self.last_exit_code = builtins::try_builtin(&cd_args).unwrap_or(0);
                if should_skip(op, self.last_exit_code) { break; }
                continue;
            }

            // Shell-state builtins (need mutable access to shell state)
            if !tokens.is_empty() {
                if let Some(code) = self.handle_stateful_builtin(&tokens) {
                    self.last_exit_code = code;
                    if should_skip(op, self.last_exit_code) { break; }
                    continue;
                }
            }

            // Regular builtins
            if let Some(code) = builtins::try_builtin(&tokens) {
                if code == builtins::EXIT_SIGNAL {
                    let _ = self.line_editor.sync_history();
                    return true;
                }
                self.last_exit_code = code;
                if should_skip(op, self.last_exit_code) { break; }
                continue;
            }

            // External commands
            let reconstructed = tokens.join(" ");
            let pipeline = parser::parse_pipeline(&reconstructed);
            self.last_exit_code = executor::execute_pipeline(pipeline);

            // Typo correction: suggest if command not found
            if self.last_exit_code == 127 && !tokens.is_empty() {
                if let Some(suggestion) = suggest_correction(&tokens[0]) {
                    eprintln!(
                        "\x1b[33moxsh: did you mean \x1b[1m{suggestion}\x1b[22m?\x1b[0m",
                    );
                }
            }

            if should_skip(op, self.last_exit_code) { break; }
        }

        self.cmd_duration_ms = start.elapsed().as_millis();
        false
    }

    /// Handle builtins that need mutable access to shell state.
    fn handle_stateful_builtin(&mut self, tokens: &[String]) -> Option<i32> {
        match tokens[0].as_str() {
            "alias" => {
                if tokens.len() == 1 {
                    let mut sorted: Vec<_> = self.aliases.iter().collect();
                    sorted.sort_by_key(|(k, _)| k.clone());
                    for (k, v) in sorted {
                        println!("alias {k}='{v}'");
                    }
                } else {
                    for arg in &tokens[1..] {
                        if let Some((name, value)) = arg.split_once('=') {
                            self.aliases.insert(name.into(), value.into());
                        } else if let Some(v) = self.aliases.get(arg.as_str()) {
                            println!("alias {arg}='{v}'");
                        } else {
                            eprintln!("alias: {arg}: not found");
                        }
                    }
                }
                Some(0)
            }
            "unalias" => {
                for arg in &tokens[1..] {
                    self.aliases.remove(arg.as_str());
                }
                Some(0)
            }
            "source" | "." | "reload" => {
                let path = if tokens.len() > 1 {
                    std::path::PathBuf::from(shellexpand::tilde(&tokens[1]).to_string())
                } else {
                    crate::config::config_path()
                };
                self.config = Config::load_from(&path);
                self.config.apply_env();
                for (k, v) in &self.config.aliases {
                    self.aliases.entry(k.clone()).or_insert_with(|| v.clone());
                }
                Some(0)
            }
            "history" => {
                let hist_path = shellexpand::tilde(&self.config.history.file).to_string();
                if let Ok(content) = std::fs::read_to_string(&hist_path) {
                    for (i, line) in content.lines().enumerate() {
                        println!("{:5}  {}", i + 1, line);
                    }
                }
                Some(0)
            }
            "back" => {
                if self.dir_index > 0 {
                    self.dir_index -= 1;
                    let target = self.dir_history[self.dir_index].clone();
                    if std::env::set_current_dir(&target).is_err() {
                        eprintln!("back: {target}: directory not found");
                        self.dir_index += 1;
                        return Some(1);
                    }
                } else {
                    eprintln!("back: no previous directory");
                    return Some(1);
                }
                Some(0)
            }
            "next" => {
                if self.dir_index + 1 < self.dir_history.len() {
                    self.dir_index += 1;
                    let target = self.dir_history[self.dir_index].clone();
                    if std::env::set_current_dir(&target).is_err() {
                        eprintln!("next: {target}: directory not found");
                        self.dir_index -= 1;
                        return Some(1);
                    }
                } else {
                    eprintln!("next: no next directory");
                    return Some(1);
                }
                Some(0)
            }
            _ => None,
        }
    }

    fn execute_line_inner(&mut self, input: &str) -> i32 {
        let expanded = parser::expand_subshells(input);
        let mut tokens = parser::tokenize(&expanded);
        if tokens.is_empty() {
            return 0;
        }
        scripting::expand_shell_vars(&mut tokens, &self.shell_vars);
        parser::expand_vars(&mut tokens);
        parser::resolve_alias(&mut tokens, &self.config.aliases);
        parser::expand_globs(&mut tokens);

        if let Some((key, val)) = scripting::is_var_assignment(input) {
            self.shell_vars.set(key, val);
            return 0;
        }

        if let Some(code) = builtins::try_builtin(&tokens) {
            return code;
        }

        let reconstructed = tokens.join(" ");
        let pipeline = parser::parse_pipeline(&reconstructed);
        executor::execute_pipeline(pipeline)
    }
}

// ── Utility functions ──

fn explain_command(cmd: &str) {
    if builtins::BUILTIN_NAMES.contains(&cmd) {
        println!("\x1b[1m{cmd}\x1b[0m is an oxsh builtin. Run 'help' for details.");
        return;
    }
    if structured::is_structured_command(cmd) {
        println!("\x1b[1m{cmd}\x1b[0m is an oxsh structured pipeline command.");
        let args: Vec<String> = Vec::new();
        let (usage, _, _) = structured::run_structured(cmd, &args, "");
        print!("{usage}");
        return;
    }
    let help = std::process::Command::new(cmd)
        .arg("--help")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output();

    match help {
        Ok(output) => {
            let text = if !output.stdout.is_empty() {
                String::from_utf8_lossy(&output.stdout).to_string()
            } else {
                String::from_utf8_lossy(&output.stderr).to_string()
            };
            let lines: Vec<&str> = text.lines().take(30).collect();
            println!("\x1b[1m{cmd} --help\x1b[0m");
            for line in &lines {
                println!("{line}");
            }
            if text.lines().count() > 30 {
                println!("\x1b[2m... (truncated, run '{cmd} --help' for full output)\x1b[0m");
            }
        }
        Err(_) => {
            eprintln!("oxsh: {cmd}: command not found");
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ChainOp {
    And,
    Or,
    Semicolon,
    None,
}

fn should_skip(op: ChainOp, exit_code: i32) -> bool {
    match op {
        ChainOp::And => exit_code != 0,
        ChainOp::Or => exit_code == 0,
        ChainOp::Semicolon | ChainOp::None => false,
    }
}

fn split_chain_ops(input: &str) -> Vec<(&str, ChainOp)> {
    let mut segments: Vec<(&str, ChainOp)> = Vec::new();
    let mut start = 0;
    let mut in_single = false;
    let mut in_double = false;
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        match bytes[i] {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'&' if !in_single && !in_double && i + 1 < bytes.len() && bytes[i + 1] == b'&' => {
                segments.push((&input[start..i], ChainOp::And));
                i += 2;
                start = i;
                continue;
            }
            b'|' if !in_single && !in_double && i + 1 < bytes.len() && bytes[i + 1] == b'|' => {
                segments.push((&input[start..i], ChainOp::Or));
                i += 2;
                start = i;
                continue;
            }
            b';' if !in_single && !in_double => {
                segments.push((&input[start..i], ChainOp::Semicolon));
                i += 1;
                start = i;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
    segments.push((&input[start..], ChainOp::None));
    segments
}
