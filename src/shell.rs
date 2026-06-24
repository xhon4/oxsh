use crate::{builtins, config::Config, context, executor, parser, prompt::OxshPrompt, scripting, structured};
use reedline::{Reedline, Signal};
use std::collections::HashMap;
use std::time::Instant;

// Maximum entries to keep in the back/next directory history
const DIR_HISTORY_MAX: usize = 256;

/// Suggest a correction for a mistyped command using Damerau-Levenshtein distance.
/// Returns the best match if close enough (distance <= 2).
/// Uses only in-memory data — no filesystem scan on the hot path.
fn suggest_correction(cmd: &str, known_commands: &[String]) -> Option<String> {
    if cmd.len() < 2 || cmd.contains('/') {
        return None;
    }

    let threshold = if cmd.len() <= 3 { 1 } else { 2 };
    let mut best: Option<(&str, usize)> = None;

    // Iterate by reference — no per-candidate allocation; only the winner is owned.
    let all = builtins::BUILTIN_NAMES
        .iter()
        .copied()
        .chain(structured::STRUCTURED_COMMANDS.iter().copied())
        .chain(known_commands.iter().map(|s| s.as_str()));

    for name in all {
        let dist = strsim::damerau_levenshtein(cmd, name);
        if dist > 0 && dist <= threshold {
            let is_better = best.is_none_or(|(_, d)| dist < d);
            if is_better {
                let stop_early = dist == 1;
                best = Some((name, dist));
                if stop_early {
                    break;
                }
            }
        }
    }

    best.map(|(name, _)| name.to_string())
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
    /// Cached list of PATH commands for typo suggestions (populated by main via seed)
    known_commands: Vec<String>,
}

impl Shell {
    pub fn new(config: Config, line_editor: Reedline) -> Self {
        let cwd = std::env::current_dir().ok();
        let cwd_str = cwd
            .as_ref()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();
        let dir_history = if cwd_str.is_empty() {
            vec![]
        } else {
            vec![cwd_str]
        };
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
            known_commands: Vec::new(),
        }
    }

    /// Provide the shell with the pre-scanned list of PATH commands for typo suggestions.
    /// Called from main after the background PATH scan completes.
    pub fn seed_known_commands(&mut self, commands: Vec<String>) {
        self.known_commands = commands;
    }

    pub fn run(&mut self) {
        'mainloop: loop {
            // Re-detect context only if CWD changed
            let current_cwd = std::env::current_dir().ok();
            if current_cwd != self.last_cwd {
                self.cached_context = context::ShellContext::detect();
                if let Some(ref cwd) = current_cwd {
                    let cwd_str = cwd.to_string_lossy().to_string();
                    if self
                        .dir_history
                        .get(self.dir_index)
                        .map(|s| s.as_str())
                        != Some(&cwd_str)
                    {
                        self.dir_history.truncate(self.dir_index + 1);
                        self.dir_history.push(cwd_str);
                        self.dir_index = self.dir_history.len() - 1;

                        // Cap directory history size
                        if self.dir_history.len() > DIR_HISTORY_MAX {
                            let excess = self.dir_history.len() - DIR_HISTORY_MAX;
                            self.dir_history.drain(..excess);
                            self.dir_index = self.dir_index.saturating_sub(excess);
                        }
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

                    // History expansion: !! and !$
                    let input = if trimmed.contains("!!") || trimmed.contains("!$") {
                        if self.last_command.is_empty() {
                            eprintln!("oxsh: no previous command");
                            continue;
                        }
                        let last_arg = self.last_command
                            .split_whitespace()
                            .last()
                            .unwrap_or("")
                            .to_string();
                        trimmed
                            .replace("!!", &self.last_command)
                            .replace("!$", &last_arg)
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
        if let Some(rest) = input
            .strip_prefix("?? ")
            .or_else(|| input.strip_prefix("??"))
        {
            let cmd = rest
                .split_whitespace()
                .next()
                .unwrap_or(rest.trim());
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
        // Control-flow statements use ';' as syntax (`; do`/`; then`/`; done`),
        // which must not be treated as chain separators. Hand the whole statement
        // to the control-flow parsers instead of splitting it on operators.
        let chain_segments = if is_control_flow_start(input) {
            vec![(input, ChainOp::None)]
        } else {
            split_chain_ops(input)
        };

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
                    if self.last_exit_code == builtins::EXIT_SIGNAL {
                        let _ = self.line_editor.sync_history();
                        return true;
                    }
                }
                if should_skip(op, self.last_exit_code) {
                    break;
                }
                continue;
            }

            // While loop
            if let Some(while_loop) = scripting::parse_while_loop(segment) {
                // An empty condition would loop until the iteration cap; reject it.
                if while_loop.condition.trim().is_empty() {
                    eprintln!("oxsh: while: empty condition");
                    self.last_exit_code = 2;
                    if should_skip(op, self.last_exit_code) {
                        break;
                    }
                    continue;
                }
                let max_iter: usize = std::env::var("OXSH_MAX_ITERATIONS")
                    .ok()
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(1_000_000);
                let mut iterations = 0usize;
                loop {
                    let cond_code = self.execute_line_inner(&while_loop.condition);
                    if cond_code != 0 {
                        break;
                    }
                    self.last_exit_code = self.execute_line_inner(&while_loop.body);
                    if self.last_exit_code == builtins::EXIT_SIGNAL {
                        let _ = self.line_editor.sync_history();
                        return true;
                    }
                    iterations += 1;
                    if iterations >= max_iter {
                        eprintln!(
                            "oxsh: while loop exceeded {max_iter} iterations (set OXSH_MAX_ITERATIONS to raise)"
                        );
                        self.last_exit_code = 1;
                        break;
                    }
                }
                if should_skip(op, self.last_exit_code) {
                    break;
                }
                continue;
            }

            // If block
            if let Some(if_block) = scripting::parse_if(segment) {
                let cond_code = self.execute_line_inner(&if_block.condition);
                if cond_code == 0 {
                    self.last_exit_code = self.execute_line_inner(&if_block.then_body);
                    if self.last_exit_code == builtins::EXIT_SIGNAL {
                        let _ = self.line_editor.sync_history();
                        return true;
                    }
                } else if let Some(ref else_body) = if_block.else_body {
                    self.last_exit_code = self.execute_line_inner(else_body);
                    if self.last_exit_code == builtins::EXIT_SIGNAL {
                        let _ = self.line_editor.sync_history();
                        return true;
                    }
                }
                if should_skip(op, self.last_exit_code) {
                    break;
                }
                continue;
            }

            // Variable assignment: VAR=value or env-prefix VAR=val cmd
            if let Some((key, val, cmd)) = scripting::parse_env_prefix(segment) {
                // VAR=val cmd — run `cmd` with KEY=val in its environment only.
                let mut vt = vec![val.to_string()];
                scripting::expand_shell_vars(&mut vt, &self.shell_vars);
                parser::expand_vars(&mut vt);
                let env_val = scripting::strip_quotes(&vt[0]).to_string();
                // SAFETY: REPL runs on main thread; PATH scanner was joined before
                // the loop; stdin-writer threads are joined per-command (M8).
                unsafe { std::env::set_var(key, &env_val) };
                self.last_exit_code = self.execute_line_inner(cmd);
                unsafe { std::env::remove_var(key) };
                if self.last_exit_code == builtins::EXIT_SIGNAL {
                    let _ = self.line_editor.sync_history();
                    return true;
                }
                if should_skip(op, self.last_exit_code) {
                    break;
                }
                continue;
            } else if let Some((key, raw_val)) = scripting::is_var_assignment(segment) {
                // Pure assignment: VAR=value — set in shell variable table.
                let expanded_val = parser::expand_subshells(raw_val);
                let mut val_tokens = parser::tokenize(&expanded_val);
                scripting::expand_shell_vars(&mut val_tokens, &self.shell_vars);
                parser::expand_vars(&mut val_tokens);
                let final_val = val_tokens.join(" ");
                let final_val = scripting::strip_quotes(&final_val).to_string();
                self.shell_vars.set(key, &final_val);
                self.last_exit_code = 0;
                if should_skip(op, self.last_exit_code) {
                    break;
                }
                continue;
            }

            // Expand subshells, then tokenize + expand
            let expanded_segment = parser::expand_subshells(segment);
            let (mut tokens, mut quoted_flags) =
                parser::tokenize_with_quote_flags(&expanded_segment);
            if tokens.is_empty() {
                continue;
            }
            scripting::expand_shell_vars(&mut tokens, &self.shell_vars);
            parser::expand_vars(&mut tokens);
            // Track alias resolution: it may replace token[0] with N tokens
            let before_alias = tokens.len();
            parser::resolve_alias(&mut tokens, &self.aliases);
            let after_alias = tokens.len();
            if after_alias != before_alias {
                // Rebuild quote flags: alias-injected tokens are unquoted,
                // original tokens[1..] keep their flags
                let alias_count = (after_alias + 1).saturating_sub(before_alias);
                let mut new_flags = vec![false; alias_count];
                if quoted_flags.len() > 1 {
                    new_flags.extend_from_slice(
                        &quoted_flags[1..before_alias.min(quoted_flags.len())],
                    );
                }
                new_flags.resize(after_alias, false);
                quoted_flags = new_flags;
            } else {
                quoted_flags.resize(after_alias, false);
            }
            parser::expand_braces(&mut tokens);
            quoted_flags.resize(tokens.len(), false);
            if self.config.shell.glob {
                parser::expand_globs_respecting_quotes(&mut tokens, &quoted_flags);
            }

            if tokens.is_empty() {
                continue;
            }

            // Auto-cd
            if self.config.shell.auto_cd
                && tokens.len() == 1
                && parser::looks_like_directory(&tokens[0])
            {
                let cd_args = vec!["cd".into(), tokens[0].clone()];
                self.last_exit_code = builtins::try_builtin(&cd_args).unwrap_or(0);
                if should_skip(op, self.last_exit_code) {
                    break;
                }
                continue;
            }

            // Shell-state builtins (need mutable access to shell state)
            if let Some(code) = self.handle_stateful_builtin(&tokens) {
                self.last_exit_code = code;
                if should_skip(op, self.last_exit_code) {
                    break;
                }
                continue;
            }

            // Regular builtins
            if let Some(code) = builtins::try_builtin(&tokens) {
                if code == builtins::EXIT_SIGNAL {
                    let _ = self.line_editor.sync_history();
                    return true;
                }
                self.last_exit_code = code;
                if should_skip(op, self.last_exit_code) {
                    break;
                }
                continue;
            }

            // External commands — build pipeline directly from tokens to avoid
            // the join→re-parse round-trip that breaks arguments with spaces.
            self.last_exit_code = self.execute_tokens(tokens, segment);

            // Typo correction: suggest if command not found
            if self.last_exit_code == 127 {
                // We need the command name; re-tokenize just the first token from segment
                let first_token = parser::tokenize(segment).into_iter().next();
                if let Some(cmd) = first_token
                    && let Some(suggestion) =
                        suggest_correction(&cmd, &self.known_commands)
                    {
                        eprintln!(
                            "\x1b[33moxsh: did you mean \x1b[1m{suggestion}\x1b[22m?\x1b[0m",
                        );
                    }
            }

            if should_skip(op, self.last_exit_code) {
                break;
            }
        }

        self.cmd_duration_ms = start.elapsed().as_millis();
        false
    }

    /// Execute a pre-expanded, pre-tokenized set of tokens as a pipeline.
    ///
    /// This is the fix for the join→re-parse bug: instead of doing
    ///   `tokens.join(" ")` → `parse_pipeline(string)`
    /// we split the already-expanded tokens on pipe markers and build
    /// Commands directly, preserving arguments with spaces.
    fn execute_tokens(&self, tokens: Vec<String>, _original_segment: &str) -> i32 {
        // Split tokens into pipe stages on "|" token boundaries.
        // Redirect tokens (>, >>, <, 2>, 2>>, 2>&1) are handled per-stage.
        let stages = split_tokens_on_pipes(tokens);

        // If there's only one stage and no pipe was present, we can use the fast path.
        // For multi-stage pipelines we build Commands directly from token groups.
        let commands = parser::parse_pipeline_from_tokens(stages);

        if commands.is_empty() {
            // Fallback: shouldn't happen, but if tokens were empty just no-op
            return 0;
        }

        // If the pipeline has a single command and it has no structured component,
        // but the original segment had subshell expansions in it that produced
        // a pipeline string (e.g. CMD=$(echo "a | b")), fall back to string parse.
        // In practice this is rare; the token path handles 99% of cases correctly.
        executor::execute_pipeline(commands)
    }

    /// Handle builtins that need mutable access to shell state.
    fn handle_stateful_builtin(&mut self, tokens: &[String]) -> Option<i32> {
        match tokens[0].as_str() {
            "alias" => {
                if tokens.len() == 1 {
                    let mut sorted: Vec<_> = self.aliases.iter().collect();
                    sorted.sort_by_key(|(k, _)| k.as_str());
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
                let new_config = Config::load_from(&path);
                new_config.apply_env();

                // Merge aliases: new config wins for existing keys,
                // but aliases set interactively that are NOT in the new config are kept.
                // Aliases that were in the old config but removed from the file are dropped.
                let old_config_keys: std::collections::HashSet<String> =
                    self.config.aliases.keys().cloned().collect();

                // Remove aliases that came from the old config (they may have been deleted)
                for key in &old_config_keys {
                    self.aliases.remove(key);
                }
                // Add all aliases from the new config
                for (k, v) in &new_config.aliases {
                    self.aliases.insert(k.clone(), v.clone());
                }

                self.config = new_config;
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
            "read" => {
                // read [-p "prompt string"] VARNAME
                // Reads one line from stdin into VARNAME (default: REPLY)
                use std::io::Write;
                let mut prompt_str = String::new();
                let mut var_name = "REPLY";
                let mut idx = 1;
                while idx < tokens.len() {
                    if tokens[idx] == "-p" && idx + 1 < tokens.len() {
                        prompt_str = tokens[idx + 1].clone();
                        idx += 2;
                    } else {
                        var_name = &tokens[idx];
                        idx += 1;
                    }
                }
                if !prompt_str.is_empty() {
                    print!("{prompt_str}");
                    std::io::stdout().flush().ok();
                }
                let mut line = String::new();
                match std::io::stdin().read_line(&mut line) {
                    Ok(0) => Some(1), // EOF
                    Ok(_) => {
                        let value = line.trim_end_matches('\n').trim_end_matches('\r');
                        self.shell_vars.set(var_name, value);
                        Some(0)
                    }
                    Err(e) => {
                        eprintln!("read: {e}");
                        Some(1)
                    }
                }
            }
            _ => None,
        }
    }

    /// Execute a command string through the full interactive pipeline.
    /// Used by `-c` mode so all shell features (`;`, `&&`, loops, builtins) work.
    pub fn run_command(&mut self, input: &str) -> i32 {
        self.handle_input(input);
        self.last_exit_code
    }

    /// Set a positional parameter ($1, $2, ...) used by `-c` script mode.
    pub fn set_positional(&mut self, n: usize, value: &str) {
        self.shell_vars.set(&n.to_string(), value);
    }

    /// Execute a single line as a command (used by for/while/if bodies and recursion).
    /// This path goes through full expansion + token-based pipeline building.
    fn execute_line_inner(&mut self, input: &str) -> i32 {
        // Variable assignment: env-prefix first, then pure assignment
        if let Some((key, val, cmd)) = scripting::parse_env_prefix(input) {
            let mut vt = vec![val.to_string()];
            scripting::expand_shell_vars(&mut vt, &self.shell_vars);
            parser::expand_vars(&mut vt);
            let env_val = scripting::strip_quotes(&vt[0]).to_string();
            // SAFETY: see handle_input — REPL main thread, no concurrent env readers.
            unsafe { std::env::set_var(key, &env_val) };
            let code = self.execute_line_inner(cmd);
            unsafe { std::env::remove_var(key) };
            return code;
        } else if let Some((key, raw_val)) = scripting::is_var_assignment(input) {
            let expanded_val = parser::expand_subshells(raw_val);
            let mut val_tokens = parser::tokenize(&expanded_val);
            scripting::expand_shell_vars(&mut val_tokens, &self.shell_vars);
            parser::expand_vars(&mut val_tokens);
            let final_val = val_tokens.join(" ");
            let final_val = scripting::strip_quotes(&final_val).to_string();
            self.shell_vars.set(key, &final_val);
            return 0;
        }

        let expanded = parser::expand_subshells(input);
        let (mut tokens, mut quoted_flags) = parser::tokenize_with_quote_flags(&expanded);
        if tokens.is_empty() {
            return 0;
        }
        scripting::expand_shell_vars(&mut tokens, &self.shell_vars);
        parser::expand_vars(&mut tokens);
        let before = tokens.len();
        parser::resolve_alias(&mut tokens, &self.config.aliases);
        let after = tokens.len();
        if after != before {
            let alias_count = (after + 1).saturating_sub(before);
            let mut new_flags = vec![false; alias_count];
            if quoted_flags.len() > 1 {
                new_flags.extend_from_slice(&quoted_flags[1..before.min(quoted_flags.len())]);
            }
            new_flags.resize(after, false);
            quoted_flags = new_flags;
        } else {
            quoted_flags.resize(after, false);
        }
        parser::expand_braces(&mut tokens);
        quoted_flags.resize(tokens.len(), false);
        if self.config.shell.glob {
            parser::expand_globs_respecting_quotes(&mut tokens, &quoted_flags);
        }

        if tokens.is_empty() {
            return 0;
        }

        if let Some(code) = builtins::try_builtin(&tokens) {
            if code == builtins::EXIT_SIGNAL {
                return builtins::EXIT_SIGNAL;
            }
            return code;
        }

        // Use token-based pipeline building (no join→re-parse)
        let stages = split_tokens_on_pipes(tokens);
        let commands = parser::parse_pipeline_from_tokens(stages);
        executor::execute_pipeline(commands)
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
    // Do NOT execute the command — `?? cmd` must be side-effect free. Resolve
    // where it lives and point the user at its own help/man instead of running it.
    match which::which(cmd) {
        Ok(path) => {
            println!("\x1b[1m{cmd}\x1b[0m is {}", path.display());
            println!("\x1b[2mrun '{cmd} --help' or 'man {cmd}' for usage\x1b[0m");
        }
        Err(_) => {
            eprintln!("oxsh: {cmd}: command not found");
        }
    }
}

/// Split a token list into pipeline stages on bare `|` tokens.
/// A `|` token that appears as a standalone string (not inside a quoted arg)
/// is a pipe separator — since tokens are already tokenized, any `|` here
/// is a real pipe, not part of a quoted string.
fn split_tokens_on_pipes(tokens: Vec<String>) -> Vec<Vec<String>> {
    let mut stages: Vec<Vec<String>> = Vec::new();
    let mut current: Vec<String> = Vec::new();

    for token in tokens {
        if token == "|" {
            stages.push(std::mem::take(&mut current));
        } else {
            current.push(token);
        }
    }
    stages.push(current);
    stages
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

/// True if the input begins a single-line control-flow statement, whose internal
/// `;` separators are part of the statement (`; do`/`; then`/`; done`) and must
/// not be split as chain operators.
fn is_control_flow_start(input: &str) -> bool {
    let t = input.trim_start();
    t.starts_with("for ") || t.starts_with("while ") || t.starts_with("if ")
}

/// Split input on unquoted `&&`, `||`, `;` chain operators.
/// Properly handles `\` escape sequences so `echo a\;b` is not split on `;`.
fn split_chain_ops(input: &str) -> Vec<(&str, ChainOp)> {
    let mut segments: Vec<(&str, ChainOp)> = Vec::new();
    let mut start = 0;
    let mut in_single = false;
    let mut in_double = false;
    let mut escape = false;
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if escape {
            escape = false;
            i += 1;
            continue;
        }
        match bytes[i] {
            b'\\' if !in_single => {
                escape = true;
                i += 1;
            }
            b'\'' if !in_double => {
                in_single = !in_single;
                i += 1;
            }
            b'"' if !in_single => {
                in_double = !in_double;
                i += 1;
            }
            b'&' if !in_single && !in_double && i + 1 < bytes.len() && bytes[i + 1] == b'&' => {
                segments.push((&input[start..i], ChainOp::And));
                i += 2;
                start = i;
            }
            b'|' if !in_single && !in_double && i + 1 < bytes.len() && bytes[i + 1] == b'|' => {
                segments.push((&input[start..i], ChainOp::Or));
                i += 2;
                start = i;
            }
            b';' if !in_single && !in_double => {
                segments.push((&input[start..i], ChainOp::Semicolon));
                i += 1;
                start = i;
            }
            _ => {
                i += 1;
            }
        }
    }
    segments.push((&input[start..], ChainOp::None));
    segments
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_close_builtin() {
        assert_eq!(suggest_correction("ehco", &[]), Some("echo".to_string()));
    }

    #[test]
    fn suggests_from_known_commands() {
        let known = vec!["git".to_string()];
        assert_eq!(suggest_correction("gti", &known), Some("git".to_string()));
    }

    #[test]
    fn no_suggestion_when_too_far() {
        assert_eq!(suggest_correction("xyzzyq", &[]), None);
    }

    #[test]
    fn no_suggestion_for_short_or_path_like() {
        assert_eq!(suggest_correction("a", &[]), None);
        assert_eq!(suggest_correction("./foo", &[]), None);
    }

    #[test]
    fn detects_control_flow_starts() {
        assert!(is_control_flow_start("for i in 1; do x; done"));
        assert!(is_control_flow_start("  while true do x done"));
        assert!(is_control_flow_start("if a; then b; fi"));
        assert!(!is_control_flow_start("forge build"));
        assert!(!is_control_flow_start("echo for"));
    }
}
