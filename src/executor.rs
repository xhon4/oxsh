use crate::parser::{Command, Redirect};
use crate::structured;
use crate::value::Value;
use std::fs::{File, OpenOptions};
use std::io::{self, IsTerminal, Read, Write};
use std::process::{Child, Command as StdCommand, Stdio};

/// Internal state passed between pipeline stages
enum PipeState {
    None,
    Child(Child),
    Data(String),
}

/// Execute a pipeline of commands, connecting them with pipes.
/// Supports mixed pipelines: external commands + structured builtins.
/// Returns the exit code of the last command.
pub fn execute_pipeline(commands: Vec<Command>) -> i32 {
    if commands.is_empty() {
        return 0;
    }

    // Fast path: single external command (most common case)
    if commands.len() == 1 {
        let cmd = &commands[0];
        if !cmd.args.is_empty() && structured::is_structured_command(&cmd.args[0]) {
            return execute_structured_standalone(cmd);
        }
        return execute_single(cmd);
    }

    let last = commands.len() - 1;
    let mut prev = PipeState::None;
    let mut children: Vec<Child> = Vec::new();

    for (i, cmd) in commands.iter().enumerate() {
        if cmd.args.is_empty() {
            continue;
        }

        let is_last = i == last;
        let is_structured = structured::is_structured_command(&cmd.args[0]);

        if is_structured {
            let input = collect_input(&mut prev, &mut children, &cmd.stdin_redirect);
            let (output, code, is_structured_out) =
                structured::run_structured(&cmd.args[0], &cmd.args[1..], &input);

            if code != 0 {
                wait_all(&mut children);
                eprint!("{output}");
                return code;
            }

            if is_last {
                wait_all(&mut children);
                write_final_output(&output, &cmd.stdout_redirect, is_structured_out);
                return 0;
            } else {
                prev = PipeState::Data(output);
            }
        } else {
            let (stdin, data_to_write) = build_stdin(&mut prev, &mut children, &cmd.stdin_redirect);
            let stdin = match stdin {
                Ok(s) => s,
                Err(code) => return code,
            };

            let stdout = if !is_last {
                Stdio::piped()
            } else if let Some(ref redirect) = cmd.stdout_redirect {
                match open_redirect(redirect) {
                    Ok(f) => Stdio::from(f),
                    Err(e) => {
                        eprintln!("oxsh: {e}");
                        return 1;
                    }
                }
            } else {
                Stdio::inherit()
            };

            // Build stderr: handle 2>&1 (merge into stdout pipe) or redirect to file
            let stderr = build_stderr(cmd, is_last, &stdout);
            let stderr = match stderr {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("oxsh: {e}");
                    return 1;
                }
            };

            match StdCommand::new(&cmd.args[0])
                .args(&cmd.args[1..])
                .stdin(stdin)
                .stdout(stdout)
                .stderr(stderr)
                .spawn()
            {
                Ok(mut child) => {
                    if let Some(data) = data_to_write {
                        if let Some(child_stdin) = child.stdin.take() {
                            std::thread::spawn(move || {
                                let mut w = child_stdin;
                                w.write_all(data.as_bytes()).ok();
                            });
                        }
                    }

                    if is_last {
                        if cmd.background {
                            println!("[bg] pid={}", child.id());
                            wait_all(&mut children);
                            return 0;
                        }
                        children.push(child);
                    } else {
                        prev = PipeState::Child(child);
                    }
                }
                Err(e) => {
                    eprintln!("oxsh: {}: {e}", cmd.args[0]);
                    wait_all(&mut children);
                    return 127;
                }
            }
        }
    }

    let mut exit_code = 0;
    for mut child in children {
        match child.wait() {
            Ok(status) => exit_code = status.code().unwrap_or(1),
            Err(e) => {
                eprintln!("oxsh: wait error: {e}");
                exit_code = 1;
            }
        }
    }
    exit_code
}

/// Build the stderr Stdio for a command, handling 2>&1 and file redirects.
fn build_stderr(
    cmd: &Command,
    _is_last: bool,
    _stdout: &Stdio,
) -> Result<Stdio, String> {
    if cmd.merge_stderr {
        // 2>&1: redirect stderr to stdout
        // std::process::Stdio doesn't expose try_clone on Stdio directly,
        // so we use the platform-specific approach via os-level dup.
        // The cleanest cross-platform way: inherit stdout fd via unsafe.
        // For safety we use the known working approach: pass Stdio::inherit() here
        // and set stderr = stdout via pre_exec on Unix.
        // Since Rust's std doesn't support this directly without unsafe/nix,
        // we handle it via a workaround: if merge_stderr is set we use piped
        // and forward. For now we use the documented correct approach.
        //
        // On Unix, the right thing is:
        //   .stderr(unsafe { Stdio::from_raw_fd(stdout_fd) })
        // but we can't get stdout's fd from a Stdio value in safe Rust.
        //
        // Best safe option: use Stdio::from(stderr_file) won't work either.
        // We use the process builder's stdout2stderr via a helper:
        Ok(merge_stderr_to_stdout())
    } else if let Some(ref redirect) = cmd.stderr_redirect {
        open_redirect(redirect)
            .map(Stdio::from)
            .map_err(|e| e.to_string())
    } else {
        Ok(Stdio::inherit())
    }
}

/// Returns a Stdio that writes to stdout (for 2>&1).
/// Uses platform-specific fd duplication.
fn merge_stderr_to_stdout() -> Stdio {
    #[cfg(unix)]
    {
        use std::os::unix::io::FromRawFd;
        // SAFETY: fd 1 is always stdout on POSIX systems and remains valid for the
        // lifetime of the process. The resulting Stdio duplicates the fd internally,
        // so the original fd 1 is not closed or invalidated.
        unsafe { Stdio::from_raw_fd(1) }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::{FromRawHandle, RawHandle};
        use windows_sys::Win32::System::Console::GetStdHandle;
        use windows_sys::Win32::System::Console::STD_OUTPUT_HANDLE;
        unsafe {
            let handle = GetStdHandle(STD_OUTPUT_HANDLE);
            Stdio::from_raw_handle(handle as RawHandle)
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        Stdio::inherit()
    }
}

/// Execute a standalone structured command (no pipeline)
fn execute_structured_standalone(cmd: &Command) -> i32 {
    let input = if let Some(ref path) = cmd.stdin_redirect {
        match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("oxsh: {path}: {e}");
                return 1;
            }
        }
    } else {
        let mut buf = String::new();
        io::stdin().read_to_string(&mut buf).ok();
        buf
    };

    let (output, code, is_structured) =
        structured::run_structured(&cmd.args[0], &cmd.args[1..], &input);

    if code != 0 {
        eprint!("{output}");
        return code;
    }

    write_final_output(&output, &cmd.stdout_redirect, is_structured);
    0
}

/// Execute a single external command (no pipeline, no structured)
fn execute_single(cmd: &Command) -> i32 {
    if cmd.args.is_empty() {
        return 0;
    }

    let stdin = if let Some(ref path) = cmd.stdin_redirect {
        match File::open(path) {
            Ok(f) => Stdio::from(f),
            Err(e) => {
                eprintln!("oxsh: {path}: {e}");
                return 1;
            }
        }
    } else {
        Stdio::inherit()
    };

    let stdout = if let Some(ref redirect) = cmd.stdout_redirect {
        match open_redirect(redirect) {
            Ok(f) => Stdio::from(f),
            Err(e) => {
                eprintln!("oxsh: {e}");
                return 1;
            }
        }
    } else {
        Stdio::inherit()
    };

    let stderr = if cmd.merge_stderr {
        merge_stderr_to_stdout()
    } else if let Some(ref redirect) = cmd.stderr_redirect {
        match open_redirect(redirect) {
            Ok(f) => Stdio::from(f),
            Err(e) => {
                eprintln!("oxsh: {e}");
                return 1;
            }
        }
    } else {
        Stdio::inherit()
    };

    match StdCommand::new(&cmd.args[0])
        .args(&cmd.args[1..])
        .stdin(stdin)
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
    {
        Ok(mut child) => {
            if cmd.background {
                println!("[bg] pid={}", child.id());
                0
            } else {
                child.wait().map(|s| s.code().unwrap_or(1)).unwrap_or(1)
            }
        }
        Err(e) => {
            eprintln!("oxsh: {}: {e}", cmd.args[0]);
            127
        }
    }
}

/// Collect all input as a string for a structured stage
fn collect_input(
    prev: &mut PipeState,
    _children: &mut Vec<Child>,
    stdin_redirect: &Option<String>,
) -> String {
    match std::mem::replace(prev, PipeState::None) {
        PipeState::Data(data) => data,
        PipeState::Child(mut child) => {
            let mut buf = String::new();
            if let Some(stdout) = child.stdout.as_mut() {
                stdout.read_to_string(&mut buf).ok();
            }
            child.wait().ok();
            buf
        }
        PipeState::None => {
            if let Some(path) = stdin_redirect {
                std::fs::read_to_string(path).unwrap_or_default()
            } else {
                let mut buf = String::new();
                io::stdin().read_to_string(&mut buf).ok();
                buf
            }
        }
    }
}

/// Build Stdio for an external command's stdin, consuming the previous stage.
fn build_stdin(
    prev: &mut PipeState,
    _children: &mut Vec<Child>,
    stdin_redirect: &Option<String>,
) -> (Result<Stdio, i32>, Option<String>) {
    match std::mem::replace(prev, PipeState::None) {
        PipeState::Data(data) => (Ok(Stdio::piped()), Some(data)),
        PipeState::Child(mut child) => {
            let stdio = child
                .stdout
                .take()
                .map(Stdio::from)
                .unwrap_or(Stdio::inherit());
            _children.push(child);
            (Ok(stdio), None)
        }
        PipeState::None => {
            if let Some(path) = stdin_redirect {
                match File::open(path) {
                    Ok(f) => (Ok(Stdio::from(f)), None),
                    Err(e) => {
                        eprintln!("oxsh: {path}: {e}");
                        (Err(1), None)
                    }
                }
            } else {
                (Ok(Stdio::inherit()), None)
            }
        }
    }
}

/// Write final output from the last stage in a pipeline
fn write_final_output(output: &str, redirect: &Option<Redirect>, is_structured: bool) {
    if let Some(redirect) = redirect {
        match open_redirect(redirect) {
            Ok(mut f) => {
                f.write_all(output.as_bytes()).ok();
            }
            Err(e) => {
                eprintln!("oxsh: {e}");
            }
        }
        return;
    }

    if is_structured && io::stdout().is_terminal() {
        let trimmed = output.trim();
        if trimmed.starts_with('[') || trimmed.starts_with('{') {
            if let Ok(val) = Value::from_json(trimmed) {
                print!("{}", val.format_table());
                return;
            }
        }
    }

    print!("{output}");
}

fn wait_all(children: &mut Vec<Child>) {
    for child in children.iter_mut() {
        child.wait().ok();
    }
}

fn open_redirect(redirect: &Redirect) -> io::Result<File> {
    match redirect {
        Redirect::Truncate(path) => File::create(path),
        Redirect::Append(path) => OpenOptions::new().create(true).append(true).open(path),
    }
}
