use crate::expand::{self, ExpandCtx, ExpandError, Field, Interrupted, RedirectTarget};
use crate::syntax::parse::{self as sparse, Arg, ChainOp, Item, RedirKind, Stage, StageItem};
use crate::syntax::{self, OpKind, Token};
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

/// The shell side of word expansion (see `expand`).
struct ShellCtx<'a> {
    vars: &'a scripting::ShellVars,
    glob: bool,
}

impl ExpandCtx for ShellCtx<'_> {
    fn var(&self, name: &str, braced: bool) -> Option<String> {
        if braced {
            // `${name}` looks the name up as written, as main did.
            return self.vars.resolve(name);
        }
        match name {
            "?" => Some(self.vars.resolve("?").unwrap_or_else(|| "0".into())),
            "$" => Some(std::process::id().to_string()),
            "!" => {
                let pid = executor::LAST_BG_PID.load(std::sync::atomic::Ordering::Relaxed);
                Some(if pid > 0 {
                    pid.to_string()
                } else {
                    String::new()
                })
            }
            "#" => Some(self.vars.resolve("#").unwrap_or_else(|| "0".into())),
            "@" | "*" => self.vars.resolve("@"),
            "0" => Some(self.vars.resolve("0").unwrap_or_else(|| "oxsh".into())),
            _ => self.vars.resolve(name),
        }
    }

    fn run_cmdsub(&mut self, src: &str) -> Result<String, Interrupted> {
        run_cmdsub(src)
    }

    fn home(&self) -> Option<String> {
        let h = shellexpand::tilde("~").to_string();
        if h == "~" { None } else { Some(h) }
    }

    fn glob_enabled(&self) -> bool {
        self.glob
    }
}

/// Run a command substitution with `$SHELL -c` (fallback `sh`), as main did,
/// and return its stdout without trailing newlines. A Ctrl-C before or
/// during the child is reported as `Interrupted` (I4, D12).
fn run_cmdsub(src: &str) -> Result<String, Interrupted> {
    if executor::was_interrupted() {
        return Err(Interrupted);
    }
    let cmd = src.trim();
    if cmd.is_empty() {
        return Ok(String::new());
    }
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "sh".to_string());
    let out = std::process::Command::new(&shell)
        .arg("-c")
        .arg(cmd)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .output();
    if executor::was_interrupted() {
        return Err(Interrupted);
    }
    match out {
        Ok(o) => Ok(String::from_utf8_lossy(&o.stdout)
            .trim_end_matches('\n')
            .to_string()),
        Err(e) => {
            eprintln!("oxsh: $({cmd}): {e}");
            Ok(String::new())
        }
    }
}

/// What to do after a chain segment.
enum Flow {
    Next,
    /// `exit`: `last_exit_code` holds the encoded exit signal.
    Exit,
    /// Ctrl-C during expansion: abandon the rest of the line with `$?=130`.
    Abort,
}

/// Expand one argument position. D1(b) cut words and unquoted alias-body
/// words that expand to a single empty field produce no field (main parity,
/// see the empty-argument table in docs/design/expansion.md).
fn expand_arg(a: &Arg, ctx: &mut dyn ExpandCtx) -> Result<Vec<Field>, ExpandError> {
    match a {
        Arg::OpText(k) => Ok(vec![Field::op_text(*k)]),
        Arg::Word { word, from_alias } => {
            let f = expand::expand_word(word, ctx)?;
            let single_empty = f.len() == 1 && f.first().is_some_and(|x| x.as_str().is_empty());
            if single_empty && (word.is_cut() || (*from_alias && !word.has_quoted())) {
                return Ok(Vec::new());
            }
            Ok(f)
        }
    }
}

/// Expand a stage into an executor command, in source order.
fn build_command(st: &Stage, ctx: &mut dyn ExpandCtx) -> Result<parser::Command, ExpandError> {
    let mut cmd = parser::Command {
        args: Vec::new(),
        stdin_redirect: None,
        stdout_redirect: None,
        stderr_redirect: None,
        merge_stderr: false,
        background: st.background,
    };
    for item in &st.items {
        match item {
            StageItem::Arg(a) => cmd
                .args
                .extend(expand_arg(a, ctx)?.into_iter().map(Field::into_string)),
            StageItem::MergeStderr => cmd.merge_stderr = true,
            StageItem::Redirect { kind, target } => {
                let t = RedirectTarget::from_fields(expand_arg(target, ctx)?)?.into_string();
                match kind {
                    RedirKind::In => cmd.stdin_redirect = Some(t),
                    RedirKind::Out => cmd.stdout_redirect = Some(parser::Redirect::Truncate(t)),
                    RedirKind::Append => cmd.stdout_redirect = Some(parser::Redirect::Append(t)),
                    RedirKind::Err => cmd.stderr_redirect = Some(parser::Redirect::Truncate(t)),
                    RedirKind::ErrAppend => cmd.stderr_redirect = Some(parser::Redirect::Append(t)),
                }
            }
        }
    }
    Ok(cmd)
}

/// Commands for `on_startup` lines and the `clear` alias (D-START): lexed by
/// the PR-A lexer, words used as literal source text, no expansion, no
/// aliases, and `;`/`&&`/`||` kept as literal words, as on main.
pub fn literal_commands(line: &str) -> Result<Vec<parser::Command>, syntax::SyntaxError> {
    let items = syntax::lex(line)?
        .into_iter()
        .map(|tok| Item {
            tok,
            from_alias: false,
        })
        .collect();
    let lit = |a: &Arg| match a {
        Arg::OpText(k) => k.spelling().to_string(),
        Arg::Word { word, .. } => word.source_text(),
    };
    Ok(sparse::parse_pipeline(items)
        .iter()
        .map(|st| {
            let mut cmd = parser::Command {
                args: Vec::new(),
                stdin_redirect: None,
                stdout_redirect: None,
                stderr_redirect: None,
                merge_stderr: false,
                background: st.background,
            };
            for item in &st.items {
                match item {
                    StageItem::Arg(a) => cmd.args.push(lit(a)),
                    StageItem::MergeStderr => cmd.merge_stderr = true,
                    StageItem::Redirect { kind, target } => {
                        let t = lit(target);
                        match kind {
                            RedirKind::In => cmd.stdin_redirect = Some(t),
                            RedirKind::Out => {
                                cmd.stdout_redirect = Some(parser::Redirect::Truncate(t))
                            }
                            RedirKind::Append => {
                                cmd.stdout_redirect = Some(parser::Redirect::Append(t))
                            }
                            RedirKind::Err => {
                                cmd.stderr_redirect = Some(parser::Redirect::Truncate(t))
                            }
                            RedirKind::ErrAppend => {
                                cmd.stderr_redirect = Some(parser::Redirect::Append(t))
                            }
                        }
                    }
                }
            }
            cmd
        })
        .collect())
}

/// Why alias resolution failed: the message and the exit code.
type AliasError = (String, i32);

/// Replace an alias command word by its body's tokens (D10). The command
/// word must be literal text; the body is lexed now and expanded later, at
/// use. Self-reference expands once; mutual recursion stops at depth 16.
fn resolve_alias_items(
    mut items: Vec<Item>,
    aliases: &HashMap<String, String>,
) -> Result<Vec<Item>, AliasError> {
    for _ in 0..=16 {
        let Some(Item {
            tok: Token::Word(w),
            ..
        }) = items.first()
        else {
            break;
        };
        let Some(name) = w.literal_text() else { break };
        let Some(body) = aliases.get(&name) else {
            break;
        };
        let body_toks =
            syntax::lex(body).map_err(|e| (format!("oxsh: alias {name}: syntax error: {e}"), 2))?;
        if body_toks
            .iter()
            .any(|t| t.op_kind().is_some_and(OpKind::is_chain))
        {
            return Err((
                format!("oxsh: alias {name}: command lists in aliases are not supported yet"),
                1,
            ));
        }
        let self_ref = matches!(body_toks.first(), Some(Token::Word(b)) if b.literal_text().as_deref() == Some(name.as_str()));
        let mut new: Vec<Item> = body_toks
            .into_iter()
            .map(|tok| Item {
                tok,
                from_alias: true,
            })
            .collect();
        new.extend(items.into_iter().skip(1));
        items = new;
        if self_ref {
            break;
        }
    }
    Ok(items)
}

/// Returns true if `input` contains `!!` or `!$` outside of single or double quotes.
fn has_unquoted_bang(input: &str) -> bool {
    let mut in_single = false;
    let mut in_double = false;
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' if !in_double => in_single = !in_single,
            b'"' if !in_single => in_double = !in_double,
            b'\\' if !in_single => i += 1, // skip next byte
            b'!' if !in_single && !in_double
                && i + 1 < bytes.len()
                && (bytes[i + 1] == b'!' || bytes[i + 1] == b'$') =>
            {
                return true;
            }
            _ => {}
        }
        i += 1;
    }
    false
}

/// Expand `!!` and `!$` in `input`, but only at positions that are outside quotes.
fn expand_bang_history(input: &str, last_cmd: &str, last_arg: &str) -> String {
    let mut result = String::with_capacity(input.len() + last_cmd.len());
    let mut in_single = false;
    let mut in_double = false;
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' if !in_double => {
                in_single = !in_single;
                result.push('\'');
            }
            b'"' if !in_single => {
                in_double = !in_double;
                result.push('"');
            }
            b'\\' if !in_single => {
                result.push('\\');
                i += 1;
                if i < bytes.len() {
                    result.push(bytes[i] as char);
                }
            }
            b'!' if !in_single && !in_double && i + 1 < bytes.len() => {
                if bytes[i + 1] == b'!' {
                    result.push_str(last_cmd);
                    i += 2;
                    continue;
                } else if bytes[i + 1] == b'$' {
                    result.push_str(last_arg);
                    i += 2;
                    continue;
                } else {
                    result.push('!');
                }
            }
            c => result.push(c as char),
        }
        i += 1;
    }
    result
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
    /// Cached value of OXSH_MAX_ITERATIONS (P8: avoid env::var on every while-loop tick)
    max_iterations: usize,
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
        let max_iterations = std::env::var("OXSH_MAX_ITERATIONS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1_000_000);
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
            max_iterations,
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

                    // History expansion: !! and !$ — only outside quotes (L3).
                    let input = if has_unquoted_bang(trimmed) {
                        if self.last_command.is_empty() {
                            eprintln!("oxsh: no previous command");
                            continue;
                        }
                        let last_arg = self.last_command
                            .split_whitespace()
                            .last()
                            .unwrap_or("")
                            .to_string();
                        expand_bang_history(trimmed, &self.last_command, &last_arg)
                    } else {
                        trimmed.to_string()
                    };
                    self.last_command = input.clone();

                    executor::clear_interrupted();
                    if self.handle_input(&input) {
                        break 'mainloop;
                    }
                    // A Ctrl-C anywhere on the line (even one whose only effect was
                    // killing a `$(...)`) leaves `$?` = 130.
                    if executor::was_interrupted() {
                        self.last_exit_code = 130;
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
                match literal_commands(&alias_cmd) {
                    Ok(pipeline) => self.last_exit_code = executor::execute_pipeline(pipeline),
                    Err(e) => {
                        eprintln!("oxsh: clear alias: syntax error: {e}");
                        self.last_exit_code = 2;
                    }
                }
            }
            self.cmd_duration_ms = 0;
            return false;
        }

        let start = Instant::now();
        let exit = self.run_line(input);
        if exit {
            let _ = self.line_editor.sync_history();
            return true;
        }
        self.cmd_duration_ms = start.elapsed().as_millis();
        false
    }

    /// Lex one line, split it into chain segments on operator tokens and run
    /// them. Returns true when the shell should exit (`last_exit_code` then
    /// holds the encoded exit signal).
    fn run_line(&mut self, input: &str) -> bool {
        // Control-flow statements use ';' as syntax (`; do`/`; then`/`; done`);
        // a line that starts with one keeps main's string parsers (debt).
        if is_control_flow_start(input) {
            self.shell_vars.set("?", &self.last_exit_code.to_string());
            if let Some(exit) = self.run_control_flow(input) {
                return exit;
            }
            // Main ran an unparsable statement as one command and still
            // expanded it; PR-A rejects it and runs nothing (I3, debt B12).
            eprintln!(
                "oxsh: syntax error: unsupported {} form (the statement must be the whole line)",
                control_keyword(input)
            );
            self.last_exit_code = 2;
            return false;
        }
        let tokens = match syntax::lex(input) {
            Ok(t) => t,
            Err(e) => {
                // D7: the whole line is rejected; nothing runs (I2/I3).
                eprintln!("oxsh: syntax error: {e}");
                self.last_exit_code = 2;
                return false;
            }
        };
        let chars: Vec<char> = input.chars().collect();
        for (seg, op) in sparse::split_chain(tokens) {
            // Ctrl-C abandons the rest of the command line, like other shells.
            if executor::was_interrupted() {
                self.last_exit_code = 130;
                break;
            }
            if seg.is_empty() {
                continue;
            }
            self.shell_vars.set("?", &self.last_exit_code.to_string());

            // A segment that starts with a control-flow keyword goes to main's
            // control-flow parsers with its source text, as on main.
            if let Some(Token::Word(w)) = seg.first()
                && matches!(w.literal_text().as_deref(), Some("for" | "while" | "if"))
            {
                let a = seg.first().map_or(0, |t| t.span().0);
                let b = seg.last().map_or(0, |t| t.span().1);
                let raw: String = chars
                    .get(a..b)
                    .map(|c| c.iter().collect())
                    .unwrap_or_default();
                if let Some(exit) = self.run_control_flow(&raw) {
                    if exit {
                        return true;
                    }
                    if should_skip(op, self.last_exit_code) {
                        break;
                    }
                    continue;
                }
                // Not parsable mid-line: like main, it runs as an ordinary
                // command (`for: not found`) and the chain continues.
            }

            let items = seg
                .into_iter()
                .map(|tok| Item {
                    tok,
                    from_alias: false,
                })
                .collect();
            match self.run_segment(items) {
                Flow::Exit => return true,
                Flow::Abort => {
                    self.last_exit_code = 130;
                    break;
                }
                Flow::Next => {}
            }
            if should_skip(op, self.last_exit_code) {
                break;
            }
        }
        false
    }

    /// Main's for/while/if handling. Returns `None` if `segment` is not a
    /// control-flow statement, `Some(true)` when the shell should exit.
    fn run_control_flow(&mut self, segment: &str) -> Option<bool> {
        if let Some(for_loop) = scripting::parse_for_loop(segment) {
            for item in &for_loop.items {
                self.shell_vars.set(&for_loop.var, item);
                self.last_exit_code = self.execute_line_inner(&for_loop.body);
                if builtins::is_exit_signal(self.last_exit_code) {
                    return Some(true);
                }
                if executor::was_interrupted() {
                    self.last_exit_code = 130;
                    break;
                }
            }
            return Some(false);
        }

        if let Some(while_loop) = scripting::parse_while_loop(segment) {
            // An empty condition would loop until the iteration cap; reject it.
            if while_loop.condition.trim().is_empty() {
                eprintln!("oxsh: while: empty condition");
                self.last_exit_code = 2;
                return Some(false);
            }
            let max_iter = self.max_iterations;
            let mut iterations = 0usize;
            loop {
                let cond_code = self.execute_line_inner(&while_loop.condition);
                if builtins::is_exit_signal(cond_code) {
                    return Some(true);
                }
                if executor::was_interrupted() {
                    self.last_exit_code = 130;
                    break;
                }
                if cond_code != 0 {
                    break;
                }
                self.last_exit_code = self.execute_line_inner(&while_loop.body);
                if builtins::is_exit_signal(self.last_exit_code) {
                    return Some(true);
                }
                if executor::was_interrupted() {
                    self.last_exit_code = 130;
                    break;
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
            return Some(false);
        }

        if let Some(if_block) = scripting::parse_if(segment) {
            let cond_code = self.execute_line_inner(&if_block.condition);
            if builtins::is_exit_signal(cond_code) {
                return Some(true);
            }
            if executor::was_interrupted() {
                self.last_exit_code = 130;
                return Some(false);
            }
            if cond_code == 0 {
                self.last_exit_code = self.execute_line_inner(&if_block.then_body);
            } else if let Some(ref else_body) = if_block.else_body {
                self.last_exit_code = self.execute_line_inner(else_body);
            }
            return Some(builtins::is_exit_signal(self.last_exit_code));
        }
        None
    }

    /// Run one chain segment: assignments and env-prefix (D11), aliases
    /// (D10), expansion, then builtin or pipeline dispatch.
    fn run_segment(&mut self, items: Vec<Item>) -> Flow {
        if let Some(Item {
            tok: Token::Word(w),
            ..
        }) = items.first()
            && let Some((name, value)) = w.assignment()
        {
            let mut ctx = ShellCtx {
                vars: &self.shell_vars,
                glob: self.config.shell.glob,
            };
            let val = match expand::expand_assignment_value(&value, &mut ctx) {
                Ok(v) => v,
                Err(e) => return self.expand_failed(e),
            };
            if items.len() == 1 {
                // Pure assignment. An interrupted value never gets here (D12).
                self.shell_vars.set(&name, &val);
                self.last_exit_code = 0;
                return Flow::Next;
            }
            // `NAME=value cmd`: run the rest with NAME in its environment only.
            // SAFETY: the REPL runs on the main thread; the PATH scanner thread
            // was joined before the loop, and stdin-writer threads are joined
            // per command (M8), so nothing reads the environment concurrently.
            let prev = std::env::var(&name).ok();
            unsafe { std::env::set_var(&name, &val) };
            let flow = self.run_segment(items.into_iter().skip(1).collect());
            match prev {
                Some(v) => unsafe { std::env::set_var(&name, v) },
                None => unsafe { std::env::remove_var(&name) },
            }
            return flow;
        }

        let items = match resolve_alias_items(items, &self.aliases) {
            Ok(i) => i,
            Err((msg, code)) => {
                eprintln!("{msg}");
                self.last_exit_code = code;
                return Flow::Next;
            }
        };
        // Any pipe or redirect operator token, even one left as a literal
        // argument because it has no target (D5 is lexical).
        let has_ops = items.iter().any(|i| {
            i.tok
                .op_kind()
                .is_some_and(|k| !k.is_chain() && k != OpKind::Background)
        });
        let stages = sparse::parse_pipeline(items);

        let mut cmds: Vec<parser::Command> = Vec::with_capacity(stages.len());
        {
            let mut ctx = ShellCtx {
                vars: &self.shell_vars,
                glob: self.config.shell.glob,
            };
            for st in &stages {
                match build_command(st, &mut ctx) {
                    Ok(c) => cmds.push(c),
                    Err(e) => return self.expand_failed(e),
                }
            }
        }
        if !has_ops && cmds.iter().all(|c| c.args.is_empty()) {
            return Flow::Next;
        }

        let first = cmds
            .first()
            .and_then(|c| c.args.first())
            .cloned()
            .unwrap_or_default();
        // D5: builtins cannot honour redirects or pipes yet (#11).
        if has_ops && builtins::BUILTIN_NAMES.contains(&first.as_str()) {
            eprintln!(
                "oxsh: {first}: redirections and pipes are not supported for builtins yet (#11)"
            );
            self.last_exit_code = 1;
            return Flow::Next;
        }

        if !has_ops && cmds.len() == 1 {
            let Some(cmd) = cmds.first() else {
                return Flow::Next;
            };
            // Auto-cd
            if self.config.shell.auto_cd
                && !cmd.background
                && cmd.args.len() == 1
                && parser::looks_like_directory(&first)
            {
                let cd_args = vec!["cd".into(), first.clone()];
                self.last_exit_code = builtins::try_builtin(&cd_args, &self.shell_vars).unwrap_or(0);
                return Flow::Next;
            }
            // Builtins see a trailing `&` as an argument, as on main.
            let mut tokens = cmd.args.clone();
            if cmd.background {
                tokens.push("&".into());
            }
            if !tokens.is_empty() && builtins::BUILTIN_NAMES.contains(&first.as_str()) {
                if let Some(code) = self.handle_stateful_builtin(&tokens) {
                    self.last_exit_code = code;
                    return Flow::Next;
                }
                if let Some(code) = builtins::try_builtin(&tokens, &self.shell_vars) {
                    self.last_exit_code = code;
                    if builtins::is_exit_signal(code) {
                        return Flow::Exit;
                    }
                    return Flow::Next;
                }
            }
        }

        self.last_exit_code = executor::execute_pipeline(cmds);

        // Typo correction: suggest if command not found
        if self.last_exit_code == 127
            && let Some(suggestion) = suggest_correction(&first, &self.known_commands)
        {
            let safe = context::sanitize_label(&suggestion);
            eprintln!("\x1b[33moxsh: did you mean \x1b[1m{safe}\x1b[22m?\x1b[0m");
        }
        Flow::Next
    }

    /// Report an expansion failure for the current segment.
    fn expand_failed(&mut self, e: ExpandError) -> Flow {
        match e {
            ExpandError::Interrupted => Flow::Abort,
            ExpandError::Ambiguous => {
                eprintln!("oxsh: ambiguous redirect");
                self.last_exit_code = 1;
                Flow::Next
            }
            ExpandError::ArithUnsupported => {
                eprintln!("oxsh: arithmetic expansion not supported yet");
                self.last_exit_code = 1;
                Flow::Next
            }
        }
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
        if builtins::is_exit_signal(self.last_exit_code) {
            builtins::exit_code_from_signal(self.last_exit_code)
        } else {
            self.last_exit_code
        }
    }

    /// Set a positional parameter ($1, $2, ...) used by `-c` script mode.
    pub fn set_positional(&mut self, n: usize, value: &str) {
        self.shell_vars.set(&n.to_string(), value);
    }

    /// Execute a line from a for/while/if body or condition through the
    /// same lexer, chain and expansion path as a top-level line (decision 8).
    /// Like main, the caller decides what to do with the returned code;
    /// `last_exit_code` is left as it was (only `$?` inside the line sees
    /// the inner codes).
    fn execute_line_inner(&mut self, input: &str) -> i32 {
        let saved = self.last_exit_code;
        self.run_line(input);
        let code = self.last_exit_code;
        self.last_exit_code = saved;
        code
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

/// The control-flow keyword a line starts with, for error messages.
fn control_keyword(input: &str) -> &str {
    input.split_whitespace().next().unwrap_or("")
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

    // Ported from scripting.rs (expand_shell_vars) tests: variable expansion
    // now goes through the lexer and `expand::expand_word` with ShellCtx.
    fn expand_with(input: &str, vars: &scripting::ShellVars) -> String {
        let mut ctx = ShellCtx { vars, glob: false };
        let toks = syntax::lex(input).unwrap();
        let Some(Token::Word(w)) = toks.first() else {
            panic!("no word")
        };
        expand::expand_word(w, &mut ctx)
            .unwrap()
            .into_iter()
            .map(Field::into_string)
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn expands_named_and_braced_vars() {
        let mut v = scripting::ShellVars::new();
        v.set("FOO", "bar");
        assert_eq!(expand_with("$FOO", &v), "bar");
        assert_eq!(expand_with("${FOO}", &v), "bar");
        assert_eq!(expand_with("pre$FOO", &v), "prebar");
    }

    #[test]
    fn unset_var_expands_to_empty() {
        let v = scripting::ShellVars::new();
        assert_eq!(expand_with("$NOPE_OXSH_TEST", &v), "");
    }

    #[test]
    fn expands_exit_code_special() {
        let mut v = scripting::ShellVars::new();
        v.set("?", "2");
        assert_eq!(expand_with("$?", &v), "2");
    }

    #[test]
    fn lone_dollar_before_non_name_is_literal() {
        let v = scripting::ShellVars::new();
        assert_eq!(expand_with("$.", &v), "$.");
    }

    #[test]
    #[ignore = "Known limitation (IN3): $# and ${#} must agree; currently $# -> \"0\" but ${#} -> \"\"."]
    fn braced_and_unbraced_specials_agree() {
        let v = scripting::ShellVars::new();
        assert_eq!(expand_with("$#", &v), expand_with("${#}", &v));
    }

    #[test]
    fn default_value_parameter_expansion() {
        let mut v = scripting::ShellVars::new();
        assert_eq!(expand_with("${NOPE_OXSH_TEST:-fallback}", &v), "fallback");
        v.set("NOPE_OXSH_TEST", "real");
        assert_eq!(expand_with("${NOPE_OXSH_TEST:-fallback}", &v), "real");
        v.set("EMPTY", "");
        assert_eq!(expand_with("${EMPTY:-fallback}", &v), "fallback");
    }

    // Ported from parser.rs (resolve_alias) tests.
    fn aliases(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn resolve(line: &str, a: &HashMap<String, String>) -> Vec<String> {
        let items = syntax::lex(line)
            .unwrap()
            .into_iter()
            .map(|tok| Item {
                tok,
                from_alias: false,
            })
            .collect();
        resolve_alias_items(items, a)
            .unwrap()
            .into_iter()
            .map(|i| match i.tok {
                Token::Word(w) => w.source_text(),
                Token::Op(o) => o.kind().spelling().to_string(),
            })
            .collect()
    }

    #[test]
    fn resolves_simple_alias_with_args() {
        assert_eq!(
            resolve("g status", &aliases(&[("g", "git")])),
            ["git", "status"]
        );
    }

    #[test]
    fn self_referencing_alias_does_not_hang() {
        assert_eq!(
            resolve("ls -a", &aliases(&[("ls", "ls --color")])),
            ["ls", "--color", "-a"]
        );
    }

    #[test]
    fn mutual_alias_recursion_is_bounded() {
        assert!(!resolve("x", &aliases(&[("x", "y"), ("y", "x")])).is_empty());
    }

    #[test]
    fn alias_body_with_command_list_is_rejected() {
        let items = syntax::lex("g")
            .unwrap()
            .into_iter()
            .map(|tok| Item {
                tok,
                from_alias: false,
            })
            .collect();
        assert!(resolve_alias_items(items, &aliases(&[("g", "a; b")])).is_err());
    }

    #[test]
    fn expansion_words_are_not_alias_names() {
        assert_eq!(resolve("$X y", &aliases(&[("$X", "boom")])), ["$X", "y"]);
    }
}
