use crate::parser::{Command, Redirect};
use crate::structured;
use crate::value::Value;
use std::fs::{File, OpenOptions};
use std::io::{self, IsTerminal, Read, Write};
use std::process::{Child, Command as StdCommand, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

/// PID of the last process spawned in the background (for `$!`). 0 = none.
pub static LAST_BG_PID: AtomicU32 = AtomicU32::new(0);

#[cfg(unix)]
unsafe extern "C" {
    fn dup2(oldfd: i32, newfd: i32) -> i32;
}

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
    let mut write_threads: Vec<std::thread::JoinHandle<()>> = Vec::new();

    for (i, cmd) in commands.iter().enumerate() {
        if cmd.args.is_empty() {
            // Reap any pending upstream child so an empty stage (e.g. a trailing
            // pipe) does not leak it as a zombie.
            if let PipeState::Child(mut child) =
                std::mem::replace(&mut prev, PipeState::None)
            {
                child.wait().ok();
            }
            continue;
        }

        let is_last = i == last;
        let is_structured = structured::is_structured_command(&cmd.args[0]);

        if is_structured {
            let input = match collect_input(&mut prev, &mut children, &cmd.stdin_redirect) {
                Ok(s) => s,
                Err(e) => {
                    wait_all(&mut children);
                    eprintln!("oxsh: {e}");
                    return 1;
                }
            };
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

            // Build stderr: handle 2>&1 or file redirect.
            // For non-last pipeline stages with 2>&1, the child's fd 1 is the
            // pipe write-end set up by the spawn machinery, NOT the shell's fd 1.
            // We use pre_exec to dup2(1→2) in the child after that setup.
            let stderr = if cmd.merge_stderr && !is_last {
                Ok(Stdio::inherit()) // placeholder; pre_exec below will dup2(1→2)
            } else {
                build_stderr(cmd, &stdout)
            };
            let stderr = match stderr {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("oxsh: {e}");
                    return 1;
                }
            };

            let mut ext_cmd = StdCommand::new(&cmd.args[0]);
            ext_cmd.args(&cmd.args[1..]).stdin(stdin).stdout(stdout).stderr(stderr);

            // After fork, dup2(1, 2) routes stderr into the pipe write-end.
            // dup2 is async-signal-safe per POSIX.1-2008 §2.4.3.
            #[cfg(unix)]
            if cmd.merge_stderr && !is_last {
                use std::os::unix::process::CommandExt;
                unsafe {
                    ext_cmd.pre_exec(|| {
                        if dup2(1, 2) == -1 {
                            return Err(std::io::Error::last_os_error());
                        }
                        Ok(())
                    });
                }
            }

            match ext_cmd.spawn() {
                Ok(mut child) => {
                    if let Some(data) = data_to_write
                        && let Some(child_stdin) = child.stdin.take() {
                            let handle = std::thread::spawn(move || {
                                let mut w = child_stdin;
                                if let Err(e) = w.write_all(data.as_bytes()) {
                                    // BrokenPipe is expected when the consumer closes early (e.g. `head`).
                                    if e.kind() != std::io::ErrorKind::BrokenPipe {
                                        eprintln!("oxsh: pipe write error: {e}");
                                    }
                                }
                            });
                            write_threads.push(handle);
                        }

                    if is_last {
                        if cmd.background {
                            let pid = child.id();
                            LAST_BG_PID.store(pid, Ordering::Relaxed);
                            println!("[bg] pid={pid}");
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
    // Join stdin-writer threads after children have exited (broken pipe is safe here).
    for handle in write_threads {
        handle.join().ok();
    }
    exit_code
}

/// Build the stderr Stdio for a command, handling 2>&1 and file redirects.
/// For non-last pipeline stages with merge_stderr the caller uses pre_exec
/// instead; this function covers the last-stage and single-command paths.
fn build_stderr(cmd: &Command, _stdout: &Stdio) -> Result<Stdio, String> {
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

/// Returns a Stdio that writes to the current stdout destination (for 2>&1).
/// Duplicates the fd/handle so the shell's own fd 1 is never closed on drop.
fn merge_stderr_to_stdout() -> Stdio {
    #[cfg(unix)]
    {
        use std::os::unix::io::{FromRawFd, IntoRawFd};
        // SAFETY: fd 1 is valid for the lifetime of the process. We wrap it
        // temporarily to call try_clone() (= dup(1)), which produces a new fd
        // pointing to the same pipe/file. into_raw_fd() releases ownership of
        // fd 1 so it is never closed; only the duplicate is handed to Stdio.
        let guard = unsafe { File::from_raw_fd(1) };
        let duped = guard.try_clone().expect("dup(stdout) failed");
        let _ = guard.into_raw_fd(); // release — do NOT close fd 1
        unsafe { Stdio::from_raw_fd(duped.into_raw_fd()) }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::{FromRawHandle, RawHandle};
        use windows_sys::Win32::Foundation::{CloseHandle, DUPLICATE_SAME_ACCESS, HANDLE};
        use windows_sys::Win32::System::Console::{GetStdHandle, STD_OUTPUT_HANDLE};
        use windows_sys::Win32::System::Threading::GetCurrentProcess;
        // SAFETY: DuplicateHandle creates a new handle for the child so the
        // original STD_OUTPUT_HANDLE is never invalidated by the Stdio drop.
        unsafe {
            let proc = GetCurrentProcess();
            let src = GetStdHandle(STD_OUTPUT_HANDLE);
            let mut dup: HANDLE = 0;
            let ok = windows_sys::Win32::Foundation::DuplicateHandle(
                proc,
                src,
                proc,
                &mut dup,
                0,
                0,
                DUPLICATE_SAME_ACCESS,
            );
            if ok != 0 {
                Stdio::from_raw_handle(dup as RawHandle)
            } else {
                Stdio::inherit()
            }
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
        if let Err(e) = io::stdin().read_to_string(&mut buf) {
            eprintln!("oxsh: stdin read error: {e}");
        }
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
                let pid = child.id();
                LAST_BG_PID.store(pid, Ordering::Relaxed);
                println!("[bg] pid={pid}");
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
) -> Result<String, String> {
    match std::mem::replace(prev, PipeState::None) {
        PipeState::Data(data) => Ok(data),
        PipeState::Child(mut child) => {
            let mut buf = String::new();
            if let Some(stdout) = child.stdout.as_mut() {
                stdout
                    .read_to_string(&mut buf)
                    .map_err(|e| format!("pipe read error: {e}"))?;
            }
            child.wait().ok();
            Ok(buf)
        }
        PipeState::None => {
            if let Some(path) = stdin_redirect {
                std::fs::read_to_string(path).map_err(|e| format!("{path}: {e}"))
            } else {
                let mut buf = String::new();
                io::stdin()
                    .read_to_string(&mut buf)
                    .map_err(|e| format!("stdin: {e}"))?;
                Ok(buf)
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
                if let Err(e) = f.write_all(output.as_bytes()) {
                    eprintln!("oxsh: write error: {e}");
                }
            }
            Err(e) => {
                eprintln!("oxsh: {e}");
            }
        }
        return;
    }

    if is_structured && io::stdout().is_terminal() {
        let trimmed = output.trim();
        if (trimmed.starts_with('[') || trimmed.starts_with('{'))
            && let Ok(val) = Value::from_json(trimmed) {
                print!("{}", val.format_table());
                return;
            }
    }

    print!("{output}");
}

fn wait_all(children: &mut [Child]) {
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
