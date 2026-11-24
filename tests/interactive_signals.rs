//! Interactive Ctrl-C regression tests, driven through a pseudo-terminal.
//!
//! The terminal's line discipline turns a `^C` byte into SIGINT for the whole
//! foreground process group, which includes the shell itself. These tests make
//! the PTY slave oxsh's controlling terminal (setsid + TIOCSCTTY) so that path
//! is exercised for real. Linux-only: the ioctl request number is platform
//! specific and CI for other platforms does not exist yet (#27).
#![cfg(target_os = "linux")]

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

unsafe extern "C" {
    fn posix_openpt(flags: i32) -> i32;
    fn grantpt(fd: i32) -> i32;
    fn unlockpt(fd: i32) -> i32;
    fn ptsname_r(fd: i32, buf: *mut u8, len: usize) -> i32;
    fn setsid() -> i32;
    fn ioctl(fd: i32, request: u64, ...) -> i32;
}

const O_RDWR: i32 = 0o2;
const O_NOCTTY: i32 = 0o400;
const TIOCSCTTY: u64 = 0x540E;

struct Session {
    child: Child,
    master: File,
    output: Arc<Mutex<Vec<u8>>>,
    _home: tempfile::TempDir,
}

impl Session {
    fn start() -> Session {
        let home = tempfile::tempdir().expect("temp HOME");
        // SAFETY: plain libc PTY calls; every return value is checked.
        let (master_fd, slave_path) = unsafe {
            let fd = posix_openpt(O_RDWR | O_NOCTTY);
            assert!(fd >= 0, "posix_openpt failed");
            assert_eq!(grantpt(fd), 0, "grantpt failed");
            assert_eq!(unlockpt(fd), 0, "unlockpt failed");
            let mut buf = [0u8; 128];
            assert_eq!(
                ptsname_r(fd, buf.as_mut_ptr(), buf.len()),
                0,
                "ptsname_r failed"
            );
            let len = buf.iter().position(|&b| b == 0).unwrap();
            (fd, String::from_utf8(buf[..len].to_vec()).unwrap())
        };
        let slave = File::options()
            .read(true)
            .write(true)
            .open(&slave_path)
            .expect("open pty slave");

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_oxsh"));
        cmd.env_clear()
            .env("HOME", home.path())
            .env("PATH", "/usr/local/bin:/usr/bin:/bin")
            .env("TERM", "xterm")
            .env("SHELL", "/bin/sh")
            .current_dir(home.path())
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave.try_clone().unwrap()))
            .stderr(Stdio::from(slave));
        // SAFETY: setsid and ioctl are async-signal-safe.
        unsafe {
            cmd.pre_exec(|| {
                if setsid() < 0 || ioctl(0, TIOCSCTTY, 0) < 0 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let child = cmd.spawn().expect("spawn oxsh");

        // SAFETY: we own master_fd from here on.
        let master = unsafe { File::from_raw_fd(master_fd as RawFd) };
        let output = Arc::new(Mutex::new(Vec::new()));
        let (mut reader, mut responder) =
            (master.try_clone().unwrap(), master.try_clone().unwrap());
        let sink = Arc::clone(&output);
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                // Behave like a terminal: answer cursor-position queries, which
                // reedline issues before drawing every prompt.
                let queries = buf[..n].windows(4).filter(|w| w == b"\x1b[6n").count();
                for _ in 0..queries {
                    let _ = responder.write_all(b"\x1b[1;1R");
                }
                sink.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });
        Session {
            child,
            master,
            output,
            _home: home,
        }
    }

    fn send(&mut self, bytes: &[u8]) {
        self.master.write_all(bytes).expect("write to pty");
    }

    /// Wait until `needle` appears in the output; return how long it took.
    fn wait_for(&mut self, needle: &str, timeout: Duration) -> Duration {
        let start = Instant::now();
        loop {
            if String::from_utf8_lossy(&self.output.lock().unwrap()).contains(needle) {
                return start.elapsed();
            }
            if let Ok(Some(status)) = self.child.try_wait() {
                panic!("oxsh exited ({status}) while waiting for {needle:?}");
            }
            if start.elapsed() >= timeout {
                let out = String::from_utf8_lossy(&self.output.lock().unwrap()).to_string();
                let tail: String = out
                    .chars()
                    .rev()
                    .take(600)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect();
                panic!("timed out waiting for {needle:?}; output tail: {tail:?}");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Run a command whose output contains `marker` while the typed (echoed)
    /// line does not, because the marker is assembled from quoted parts.
    fn run_marked(&mut self, cmd: &str, marker: &str) -> Duration {
        self.send(format!("{cmd}\r").as_bytes());
        self.wait_for(marker, Duration::from_secs(10))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn ctrl_c_stops_foreground_command_but_not_the_shell() {
    let mut sh = Session::start();
    sh.run_marked("echo READY\"_\"1", "READY_1");

    sh.send(b"sleep 5\r");
    std::thread::sleep(Duration::from_millis(500));
    sh.send(b"\x03");

    // The shell must survive, `sleep` must be gone well before its 5 s, and
    // the status of a SIGINT-killed command is 128 + 2.
    let took = sh.run_marked("echo STATUS\"_\"$?", "STATUS_130");
    assert!(
        took < Duration::from_secs(3),
        "sleep was not interrupted ({took:?})"
    );
    assert!(
        sh.child.try_wait().unwrap().is_none(),
        "oxsh died on Ctrl-C"
    );
}

#[test]
fn ctrl_c_at_empty_prompt_keeps_the_shell() {
    let mut sh = Session::start();
    sh.run_marked("echo READY\"_\"1", "READY_1");
    sh.send(b"\x03");
    // Give reedline time to redraw the prompt: input typed while it is between
    // two read_line calls can be swallowed (seen on main too, unrelated to signals).
    std::thread::sleep(Duration::from_millis(500));
    sh.run_marked("echo ALIVE\"_\"1", "ALIVE_1");
}

#[test]
fn ctrl_c_abandons_loops_and_the_rest_of_the_line() {
    let mut sh = Session::start();
    sh.run_marked("echo READY\"_\"1", "READY_1");

    sh.send(b"while true; do sleep 1; done\r");
    std::thread::sleep(Duration::from_millis(500));
    sh.send(b"\x03");
    let took = sh.run_marked("echo LOOP\"_\"$?", "LOOP_130");
    assert!(
        took < Duration::from_secs(3),
        "while loop kept running ({took:?})"
    );

    sh.send(b"sleep 5; echo SHOULD\"_\"NOT\"_\"RUN\r");
    std::thread::sleep(Duration::from_millis(500));
    sh.send(b"\x03");
    sh.run_marked("echo AFTER\"_\"1", "AFTER_1");
    let out = String::from_utf8_lossy(&sh.output.lock().unwrap()).to_string();
    assert!(
        !out.contains("SHOULD_NOT_RUN"),
        "commands after ^C on the same line still ran"
    );
}
