use crate::context::ShellContext;
use crate::structured;
use std::env;
use std::path::Path;

/// Sentinel range: any `code <= EXIT_SIGNAL` means "terminate the shell".
/// The actual exit code is encoded as `-(9999 + requested_code)`.
/// `EXIT_SIGNAL` itself (-9999) encodes exit code 0.
pub const EXIT_SIGNAL: i32 = -9999;

/// Returns true when `code` is an exit-signal (produced by `exit`/`quit`).
#[inline]
pub fn is_exit_signal(code: i32) -> bool { code <= EXIT_SIGNAL }

/// Decodes the requested exit code out of an exit-signal value.
/// Saturates to [0, 255] — matches POSIX `exit` behavior.
#[inline]
pub fn exit_code_from_signal(signal: i32) -> i32 { (-signal).saturating_sub(9999).clamp(0, 255) }

/// Encodes `code` into an exit-signal value.
#[inline]
fn make_exit_signal(code: i32) -> i32 { -(9999 + code.clamp(0, 255)) }

/// Execute a builtin command. Returns Some(exit_code) if handled, None if not a builtin.
pub fn try_builtin(args: &[String]) -> Option<i32> {
    if args.is_empty() {
        return Some(0);
    }
    match args[0].as_str() {
        "cd" => Some(builtin_cd(args)),
        "exit" | "quit" => {
            let code = args.get(1).and_then(|s| s.parse::<i32>().ok()).unwrap_or(0);
            Some(make_exit_signal(code))
        }
        "export" => Some(builtin_export(args)),
        "unset" => Some(builtin_unset(args)),
        "pwd" => {
            println!("{}", env::current_dir().unwrap_or_default().display());
            Some(0)
        }
        "which" => Some(builtin_which(args)),
        "echo" => Some(builtin_echo(args)),
        "true" => Some(0),
        "false" => Some(1),
        "help" => Some(builtin_help()),
        "type" => Some(builtin_type(args)),
        "context" => Some(builtin_context()),
        _ => None,
    }
}

/// List of builtin names (for completer and type detection)
pub static BUILTIN_NAMES: &[&str] = &[
    "cd", "exit", "quit", "export", "unset", "pwd", "which",
    "echo", "true", "false", "help", "type", "context",
    "alias", "unalias", "source", ".", "history", "reload",
    "back", "next", "read",
];

fn builtin_cd(args: &[String]) -> i32 {
    let target = if args.len() < 2 {
        dirs::home_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|| "/".into())
    } else if args[1] == "-" {
        env::var("OLDPWD").unwrap_or_else(|_| ".".into())
    } else {
        shellexpand::tilde(&args[1]).to_string()
    };

    let old = env::current_dir().unwrap_or_default();
    let path = Path::new(&target);
    match env::set_current_dir(path) {
        Ok(()) => {
            // SAFETY: called only from the REPL main thread. The PATH scanner
            // thread is joined before the REPL starts; stdin-writer threads are
            // joined after each command. No concurrent env readers.
            unsafe {
                env::set_var("OLDPWD", old);
                env::set_var("PWD", env::current_dir().unwrap_or_default());
            }
            0
        }
        Err(e) => {
            eprintln!("cd: {target}: {e}");
            1
        }
    }
}

fn builtin_export(args: &[String]) -> i32 {
    for arg in &args[1..] {
        if let Some((key, val)) = arg.split_once('=') {
            let expanded = shellexpand::tilde(val).to_string();
            // SAFETY: REPL main thread only; see builtin_cd comment.
            unsafe { env::set_var(key, &expanded); }
        } else {
            // export VAR (no value) — just ensure it's exported, noop in our model
        }
    }
    0
}

fn builtin_unset(args: &[String]) -> i32 {
    for arg in &args[1..] {
        // SAFETY: REPL main thread only; see builtin_cd comment.
        unsafe { env::remove_var(arg); }
    }
    0
}

fn builtin_which(args: &[String]) -> i32 {
    let mut code = 0;
    for arg in &args[1..] {
        match which::which(arg) {
            Ok(path) => println!("{}", path.display()),
            Err(_) => {
                eprintln!("{arg} not found");
                code = 1;
            }
        }
    }
    code
}

fn builtin_echo(args: &[String]) -> i32 {
    let mut newline = true;
    let mut start = 1;
    if args.len() > 1 && args[1] == "-n" {
        newline = false;
        start = 2;
    }
    let text = args[start..].join(" ");
    if newline {
        println!("{text}");
    } else {
        print!("{text}");
    }
    0
}

fn builtin_help() -> i32 {
    println!("oxsh builtins:");
    println!("  cd [dir]          Change directory");
    println!("  pwd               Print working directory");
    println!("  echo [-n] [args]  Print arguments");
    println!("  export KEY=VAL    Set environment variable");
    println!("  unset KEY         Remove environment variable");
    println!("  alias [name=val]  Show or set aliases");
    println!("  unalias name      Remove alias");
    println!("  type name         Show what a command resolves to");
    println!("  which name        Find command in PATH");
    println!("  history           Show command history");
    println!("  source [file]     Reload config (~/.oxshrc)");
    println!("  back              Go to previous directory");
    println!("  next              Go to next directory");
    println!("  read [-p P] VAR   Read a line from stdin into VAR (default: REPLY)");
    println!("  context           Show detected project context");
    println!("  help              Show this help");
    println!("  exit              Exit the shell");
    println!();
    println!("Special:");
    println!("  !!                Repeat last command");
    println!("  ?? command        Explain a command");
    println!();
    println!("Structured pipeline commands:");
    println!("  from-json         Parse JSON input into structured data");
    println!("  to-json [-p]      Convert to JSON (--pretty for formatted)");
    println!("  to-table          Format structured data as a table");
    println!("  where F OP V      Filter records (== != > < >= <= =~ ^=)");
    println!("  select F1 F2...   Pick fields from records");
    println!("  sort-by F [--desc] Sort by field");
    println!("  get FIELD         Extract a single field");
    println!("  first [N]         Take first N items");
    println!("  last [N]          Take last N items");
    println!("  count             Count items");
    println!("  uniq              Remove duplicates");
    println!("  reverse           Reverse order");
    println!("  flatten           Flatten nested lists");
    println!();
    println!("Explain mode: ?? command  (shows help for a command)");
    0
}

fn builtin_type(args: &[String]) -> i32 {
    let mut code = 0;
    for arg in &args[1..] {
        if BUILTIN_NAMES.contains(&arg.as_str()) {
            println!("{arg} is a shell builtin");
        } else if structured::is_structured_command(arg) {
            println!("{arg} is an oxsh structured pipeline command");
        } else if let Ok(path) = which::which(arg) {
            println!("{arg} is {}", path.display());
        } else {
            eprintln!("type: {arg}: not found");
            code = 1;
        }
    }
    code
}

fn builtin_context() -> i32 {
    let ctx = ShellContext::detect();
    if let Some(ref pt) = ctx.project_type {
        println!("project:  {} {}", pt.icon(), pt.name());
    }
    if ctx.git_repo {
        println!(
            "git:      {}",
            ctx.git_branch.as_deref().unwrap_or("(detached)")
        );
    }
    if let Some(ref venv) = ctx.virtualenv {
        println!("venv:     {venv}");
    }
    if let Some(ref k8s) = ctx.k8s_context {
        println!("k8s:      {k8s}");
    }
    if ctx.in_ssh {
        println!("ssh:      yes");
    }
    if ctx.project_type.is_none()
        && !ctx.git_repo
        && ctx.virtualenv.is_none()
        && ctx.k8s_context.is_none()
    {
        println!("(no context detected)");
    }
    0
}
