//! Generative robustness tests for the lexer/expander (issue #10; written by the refuter review).
//!
//! Black-box: every case runs `oxsh -c SCRIPT` as a child process (CARGO_BIN_EXE_oxsh), so the
//! tests exercise the real binary. Only `std` is used.
//!
//! SAFETY: the generated scripts are random shell input. Generators avoid command names that
//! write (no letters forming rm/mv/cp/touch); writes can only come from `>` / `>>`, and every
//! "no write" property is only asserted for inputs where that is guaranteed. Each run gets its own
//! fixture directory and HOME under OXSH_FUZZ_TMP. Developers should still run it in a sandbox.
//!
//! The defaults keep the whole file well under 60 s in CI. Knobs (env, defaults in brackets):
//! - OXSH_FUZZ_ITERS [150]: generated scripts per generative test (release checks use 2000).
//! - OXSH_FUZZ_SEED [0x0c5eed]: base seed; each test xors in a hash of its name.
//! - OXSH_FUZZ_MAX_LEN [40]: soup length; OXSH_FUZZ_SHRINK [120]: shrink runs per failure;
//!   OXSH_FUZZ_MAX_FAILS [3]: failures reported per test before stopping.
//! - OXSH_FUZZ_SIZES [1024,16384,122880] and OXSH_FUZZ_DEPTHS [1,4,16,48]: size/nesting ladders.
//! - OXSH_FUZZ_TIMEOUT_S [10]: wall clock per run (timeout(1) when present, then a hard kill).
//! - OXSH_FUZZ_MEM_KB [2097152] (ulimit -v), OXSH_FUZZ_FSIZE [20000] (ulimit -f),
//!   OXSH_FUZZ_CPU_S [20] (ulimit -t); output is capped at 1 MiB per stream.
//! - OXSH_FUZZ_TMP [temp_dir()]: where fixtures are created; OXSH_FUZZ_BIN: test another binary.
//!
//! Unix only (uses /bin/sh, ulimit and coreutils timeout; falls back to an in-process kill without
//! timeout(1)).
#![cfg(unix)]

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

// ------------------------------------------------------------------ config

fn env_u64(k: &str, d: u64) -> u64 {
    std::env::var(k)
        .ok()
        .and_then(|v| {
            let v = v.trim();
            if let Some(h) = v.strip_prefix("0x") {
                u64::from_str_radix(h, 16).ok()
            } else {
                v.parse().ok()
            }
        })
        .unwrap_or(d)
}
fn iters() -> u64 {
    env_u64("OXSH_FUZZ_ITERS", 150)
}
fn seed() -> u64 {
    env_u64("OXSH_FUZZ_SEED", 0x0c5eed)
}
fn timeout_s() -> u64 {
    env_u64("OXSH_FUZZ_TIMEOUT_S", 10)
}

fn oxsh_bin() -> PathBuf {
    if let Ok(p) = std::env::var("OXSH_FUZZ_BIN") {
        return PathBuf::from(p);
    }
    match option_env!("CARGO_BIN_EXE_oxsh") {
        Some(p) => PathBuf::from(p),
        None => panic!("set OXSH_FUZZ_BIN to the oxsh binary"),
    }
}

// ------------------------------------------------------------------ PRNG (xorshift64*)

struct Rng(u64);
impl Rng {
    fn new(s: u64) -> Self {
        Rng(s.max(1))
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
}

// ------------------------------------------------------------------ sandboxed run

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Outcome {
    code: Option<i32>,
    signal: Option<i32>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    elapsed: Duration,
    timed_out: bool,
    before: BTreeMap<String, Vec<u8>>,
    after: BTreeMap<String, Vec<u8>>,
}
impl Outcome {
    fn files_changed(&self) -> bool {
        self.before != self.after
    }
    fn summary(&self) -> String {
        format!(
            "code={:?} signal={:?} timed_out={} {:?} stdout={:?} stderr={:?} changed={:?}",
            self.code,
            self.signal,
            self.timed_out,
            self.elapsed,
            String::from_utf8_lossy(&self.stdout[..self.stdout.len().min(300)]),
            String::from_utf8_lossy(&self.stderr[..self.stderr.len().min(300)]),
            diff_keys(&self.before, &self.after)
        )
    }
}
fn diff_keys(a: &BTreeMap<String, Vec<u8>>, b: &BTreeMap<String, Vec<u8>>) -> Vec<String> {
    let mut v = vec![];
    for (k, x) in b {
        if a.get(k) != Some(x) {
            v.push(k.clone());
        }
    }
    for k in a.keys() {
        if !b.contains_key(k) {
            v.push(format!("-{k}"));
        }
    }
    v
}

fn snapshot(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
            match e.file_type() {
                Ok(t) if t.is_dir() => {
                    out.insert(rel + "/", vec![]);
                    walk(base, &p, out);
                }
                _ => {
                    let mut buf = vec![];
                    if let Ok(f) = std::fs::File::open(&p) {
                        let _ = f.take(4096).read_to_end(&mut buf);
                    }
                    out.insert(rel, buf);
                }
            }
        }
    }
    let mut m = BTreeMap::new();
    walk(root, root, &mut m);
    m
}

fn fixture(t: &Path) {
    let w = t.join("work");
    std::fs::create_dir_all(w.join("sub")).unwrap();
    std::fs::create_dir_all(t.join("home")).unwrap();
    std::fs::write(w.join("a.txt"), "DATA\n").unwrap();
    std::fs::write(w.join("b.txt"), "").unwrap();
    std::fs::write(w.join("b.log"), "x\n").unwrap();
    std::fs::write(w.join(".hidden"), "").unwrap();
}

fn read_capped(mut r: impl Read + Send + 'static) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut out = Vec::new();
        let mut buf = [0u8; 65536];
        loop {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if out.len() < (1 << 20) {
                        out.extend_from_slice(&buf[..n.min((1 << 20) - out.len())]);
                    }
                }
            }
        }
        out
    })
}

fn run(script: &[u8], env: &[(&str, &[u8])]) -> Outcome {
    let base = std::env::var_os("OXSH_FUZZ_TMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let t = base.join(format!(
        "oxsh-rf-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&t);
    fixture(&t);
    let before = snapshot(&t);
    let to = timeout_s();
    let lim = format!(
        "ulimit -v {}; ulimit -f {}; ulimit -t {}; exec \"$@\"",
        env_u64("OXSH_FUZZ_MEM_KB", 2_097_152),
        env_u64("OXSH_FUZZ_FSIZE", 20_000),
        env_u64("OXSH_FUZZ_CPU_S", 20)
    );
    let has_timeout = Path::new("/usr/bin/timeout").exists();
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c").arg(&lim).arg("sh");
    if has_timeout {
        cmd.args(["/usr/bin/timeout", "-k", "2", &to.to_string()]);
    }
    cmd.arg(oxsh_bin()).arg("-c").arg(OsStr::from_bytes(script));
    cmd.env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", t.join("home"))
        .env("SHELL", "/bin/sh")
        .env("LANG", "C.UTF-8")
        .env("TERM", "dumb");
    for (k, v) in env {
        cmd.env(k, OsStr::from_bytes(v));
    }
    cmd.current_dir(t.join("work"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let start = Instant::now();
    let mut child = cmd.spawn().expect("spawn /bin/sh");
    let (o, e) = (
        read_capped(child.stdout.take().unwrap()),
        read_capped(child.stderr.take().unwrap()),
    );
    let hard = Duration::from_secs(to + 5);
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if start.elapsed() > hard {
            let _ = child.kill();
            break child.wait().unwrap();
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    let elapsed = start.elapsed();
    let code = status.code();
    // timeout(1) exits 124 on TERM, 137 after KILL
    let timed_out =
        elapsed >= Duration::from_secs(to) && matches!(code, Some(124) | Some(137) | None);
    let out = Outcome {
        code,
        signal: status.signal(),
        stdout: o.join().unwrap(),
        stderr: e.join().unwrap(),
        elapsed,
        timed_out,
        before,
        after: snapshot(&t),
    };
    let _ = std::fs::remove_dir_all(&t);
    out
}

// ------------------------------------------------------------------ properties

/// I3: no panic, no crash signal, no hang, no abort.
fn robust(o: &Outcome) -> Result<(), String> {
    if o.timed_out {
        return Err("hang (timeout)".into());
    }
    if o.code == Some(101) || o.stderr.windows(8).any(|w| w == b"panicked") {
        return Err("panic".into());
    }
    if let Some(s) = o.signal {
        return Err(format!("killed by signal {s}"));
    }
    if matches!(o.code, Some(134) | Some(139) | Some(136)) {
        return Err(format!("abnormal exit {:?}", o.code));
    }
    Ok(())
}

fn esc(b: &[u8]) -> String {
    b.iter()
        .flat_map(|&c| std::ascii::escape_default(c))
        .map(char::from)
        .collect()
}

/// Delta-debugging style shrink: remove chunks while `still_fails` holds (bounded number of runs).
fn shrink(mut s: Vec<u8>, still_fails: &dyn Fn(&[u8]) -> bool) -> Vec<u8> {
    let mut budget = env_u64("OXSH_FUZZ_SHRINK", 120) as i64;
    let mut chunk = s.len() / 2;
    while chunk >= 1 && budget > 0 {
        let mut i = 0;
        let mut progressed = false;
        while i + chunk <= s.len() && budget > 0 {
            let mut cand = s.clone();
            cand.drain(i..i + chunk);
            budget -= 1;
            if !cand.is_empty() && still_fails(&cand) {
                s = cand;
                progressed = true;
            } else {
                i += chunk;
            }
        }
        if !progressed {
            chunk /= 2;
        }
    }
    s
}

/// A property every run of a generated script must satisfy.
type Property = dyn Fn(&[u8], &Outcome) -> Result<(), String>;

/// Run `iters` generated scripts; on the first failure shrink it and panic with a reproducer.
fn check_gen(
    name: &str,
    generate: &dyn Fn(&mut Rng) -> Vec<u8>,
    env: &[(&str, &[u8])],
    prop: &Property,
) {
    let s0 = seed() ^ fxhash(name);
    let mut rng = Rng::new(s0);
    let mut failures = vec![];
    for i in 0..iters() {
        let script = generate(&mut rng);
        let o = run(&script, env);
        if let Err(why) = robust(&o).and_then(|_| prop(&script, &o)) {
            let kind = failure_kind(&why);
            let min = shrink(script.clone(), &|c| {
                let o = run(c, env);
                matches!(robust(&o).and_then(|_| prop(c, &o)), Err(w) if failure_kind(&w) == kind)
            });
            let om = run(&min, env);
            failures.push(format!("[{name}] seed={s0:#x} iter={i}: {why}\n  script : b\"{}\"\n  shrunk : b\"{}\"\n  outcome: {}",
                esc(&script), esc(&min), om.summary()));
            if failures.len() >= env_u64("OXSH_FUZZ_MAX_FAILS", 3) as usize {
                break;
            }
        }
    }
    if !failures.is_empty() {
        panic!("{} failure(s):\n{}", failures.len(), failures.join("\n"));
    }
}
/// Failure class used to keep the shrinker on the same bug (text up to the first ':' or digit).
fn failure_kind(why: &str) -> String {
    why.chars()
        .take_while(|c| *c != ':' && !c.is_ascii_digit())
        .collect()
}
fn fxhash(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325u64, |h, b| {
        (h ^ b as u64).wrapping_mul(0x100000001b3)
    })
}

fn no_write_unless_gt(script: &[u8], o: &Outcome) -> Result<(), String> {
    if !script.contains(&b'>') && o.files_changed() {
        return Err(format!(
            "wrote files without any '>': {:?}",
            diff_keys(&o.before, &o.after)
        ));
    }
    Ok(())
}

// ------------------------------------------------------------------ generators

const PREFIXES: &[&str] = &[
    "/usr/bin/printf [%s] ",
    "/bin/echo ",
    "",
    "$U ",
    "true ",
    "$U $U /usr/bin/printf [%s] ",
];
const SOUP: &[&str] = &[
    "${",
    "${U:-",
    "${X:-",
    ":-",
    "}",
    "}",
    "a",
    "b",
    " ",
    " ",
    "\t",
    "'",
    "\"",
    "\\",
    "$",
    "(",
    ")",
    "`",
    ">",
    ">>",
    "<",
    "|",
    "||",
    "&",
    "&&",
    ";",
    "{",
    ",",
    "*",
    "?",
    "[",
    "]",
    "~",
    "2",
    "1",
    ".",
    "$(",
    "$((",
    "))",
    "$X",
    "${X}",
    "$U",
    "${U}",
    "\"\"",
    "''",
    "$(true)",
    "$(printf %s x)",
    "\n",
    "#",
    "!",
    "=",
    "-",
    "<<",
    "<<<",
    "é",
    "\\\n",
    "$1",
    "$@",
    "$?",
];
fn gen_soup(r: &mut Rng) -> Vec<u8> {
    let mut s = String::from(*r.pick(PREFIXES));
    let n = 1 + r.below(env_u64("OXSH_FUZZ_MAX_LEN", 40) as usize);
    for _ in 0..n {
        s.push_str(r.pick::<&str>(SOUP));
    }
    s.into_bytes()
}

const PLAIN: &[&str] = &["a", "b", "x", " ", "$U", "${U}", "1", ".", "-", "é"];
fn plain(r: &mut Rng, n: usize) -> String {
    (0..n).map(|_| *r.pick(PLAIN)).collect()
}

/// Unterminated quote whose (unterminated) body contains a redirect: the body is quoted data under any
/// fallback, so nothing may be written.
fn gen_unterminated_quote(r: &mut Rng) -> Vec<u8> {
    let q = *r.pick(&["'", "\""]);
    let inner: &[&str] = if q == "'" {
        &[
            "a", " ", "> F", ">> b.log", "| b", "; b", "&& b", "$(true)", "`b`", "${U}", "\\",
            "\"", "<", "&", "2>&1",
        ]
    } else {
        &[
            "a", " ", "> F", ">> b.log", "| b", "; b", "&& b", "${U}", "$U", "'", "<", "&", "2>&1",
            "\\\\",
        ]
    };
    let pre = *r.pick(PREFIXES);
    let k = 1 + r.below(4);
    let mut s = format!("{}{} {}", pre, plain(r, k), q);
    for _ in 0..1 + r.below(8) {
        s.push_str(r.pick::<&str>(inner));
    }
    s.into_bytes()
}
fn prop_no_write(_: &[u8], o: &Outcome) -> Result<(), String> {
    if o.files_changed() {
        Err(format!("wrote: {:?}", diff_keys(&o.before, &o.after)))
    } else {
        Ok(())
    }
}

/// A single side-effecting substitution, wrapped in random (possibly unterminated) constructs:
/// it appears once in the source, so it may run at most once (I2: lexing never executes; no double run).
fn gen_once(r: &mut Rng) -> Vec<u8> {
    let open = *r.pick(&[
        "$(",
        "${U:-$(",
        "${U:-`",
        "\"${U:-$(",
        "${U:-\"$(",
        "`",
        "\"$(",
        "\"`",
        "$($(",
        "${U:-${U:-$(",
    ]);
    let close = *r.pick(&["", ")", ")}", ")\"", "`", "`}", ")}\"", "))", "}", ")\"}"]);
    let pre = *r.pick(PREFIXES);
    let mut s = format!("{}{}/bin/echo x >> S1{}", pre, open, close);
    for _ in 0..r.below(4) {
        s.push_str(r.pick::<&str>(&[
            " a", " }", " )", " ${U", " $(", " \"", " '", " `", ";", " && true",
        ]));
    }
    s.into_bytes()
}
fn prop_once(_: &[u8], o: &Outcome) -> Result<(), String> {
    // The single `/bin/echo x >> S1...` may run at most once in total. Under a literal fallback for an
    // unterminated construct the target can be spelled `S1}`, `S1)`, ... so all `work/S1*` files count.
    let s1 = |k: &String| k.starts_with("work/S1");
    let n: usize = o
        .after
        .iter()
        .filter(|(k, _)| s1(k))
        .map(|(_, c)| c.iter().filter(|&&b| b == b'\n').count())
        .sum();
    if n > 1 {
        return Err(format!("substitution ran {n} times"));
    }
    let mut a = o.after.clone();
    a.retain(|k, _| !s1(k));
    if a != o.before {
        return Err(format!("unexpected writes: {:?}", diff_keys(&o.before, &a)));
    }
    Ok(())
}

const DATA: &[u8] = b"ab ;|&<>'\"\\$`(){}[]*?~#=!-\t\nx%^:,.";
fn gen_data(r: &mut Rng) -> Vec<u8> {
    let n = r.below(40);
    let mut v: Vec<u8> = (0..n).map(|_| DATA[r.below(DATA.len())]).collect();
    if r.below(4) == 0 {
        v.extend_from_slice(r.pick::<&[u8]>(&[
            &b"$HOME"[..],
            b"$(touch P)",
            b"`touch P`",
            b"${HOME}",
            b"> F",
            b"| tee F",
            b"; touch P",
        ]));
    }
    v
}

// ------------------------------------------------------------------ tests

#[test]
fn soup_is_robust_and_writes_only_with_gt() {
    check_gen("soup", &gen_soup, &[], &no_write_unless_gt);
}

#[test]
fn unterminated_quote_never_writes() {
    check_gen(
        "unterminated-quote",
        &gen_unterminated_quote,
        &[],
        &prop_no_write,
    );
}

#[test]
fn substitution_runs_at_most_once() {
    check_gen("once", &gen_once, &[], &prop_once);
}

/// I1: data from the environment, a quoted expansion, or quoted substitution output is passed through
/// byte for byte and never becomes syntax (no redirect, pipe, command or re-expansion).
#[test]
fn quoted_expansion_data_roundtrips() {
    let mut rng = Rng::new(seed() ^ fxhash("data"));
    let quoted: [&[u8]; 3] = [
        b"/usr/bin/printf '[%s]' \"$FZ\"",
        b"/usr/bin/printf '[%s]' \"${FZ}\"",
        b"/usr/bin/printf '[%s]' \"$(/usr/bin/printf '%s' \"$FZ\")\"",
    ];
    let unquoted: [&[u8]; 3] = [
        b"/usr/bin/printf '[%s]' $FZ",
        b"/usr/bin/printf '[%s]' $(/usr/bin/printf '%s' \"$FZ\")",
        b"/usr/bin/printf '[%s]' ${U:-$FZ}",
    ];
    let mut stats: BTreeMap<String, (u64, Vec<String>)> = BTreeMap::new();
    let mut note = |script: &[u8], i: u64, d: &[u8], e: String| {
        let ent = stats
            .entry(String::from_utf8_lossy(script).into_owned())
            .or_default();
        ent.0 += 1;
        if ent.1.len() < 2 {
            ent.1.push(format!("iter={i} FZ=b\"{}\": {e}", esc(d)));
        }
    };
    for i in 0..iters() {
        let d = gen_data(&mut rng);
        let env: [(&str, &[u8]); 1] = [("FZ", &d)];
        for (k, script) in quoted.iter().enumerate() {
            let mut want = d.clone();
            if k == 2 {
                while want.last() == Some(&b'\n') {
                    want.pop();
                }
            } // $(...) strips trailing newlines
            let want = [b"[", &want[..], b"]"].concat();
            let o = run(script, &env);
            let r = robust(&o)
                .and_then(|_| prop_no_write(script, &o))
                .and_then(|_| {
                    if o.stdout == want {
                        Ok(())
                    } else {
                        Err(format!(
                            "stdout b\"{}\" != b\"{}\"",
                            esc(&o.stdout),
                            esc(&want)
                        ))
                    }
                });
            if let Err(e) = r {
                note(script, i, &d, e);
            }
        }
        // unquoted: splitting/globbing may differ by design, but nothing may be written or executed
        for script in unquoted.iter() {
            let o = run(script, &env);
            if let Err(e) = robust(&o).and_then(|_| prop_no_write(script, &o)) {
                note(script, i, &d, e);
            }
        }
    }
    if !stats.is_empty() {
        let rep: Vec<String> = stats
            .iter()
            .map(|(s, (n, ex))| format!("{n}/{} failed: {s}\n    {}", iters(), ex.join("\n    ")))
            .collect();
        panic!("data round-trip failures:\n{}", rep.join("\n"));
    }
}

/// I3: large inputs finish in bounded time and memory.
#[test]
fn size_ladder_is_bounded() {
    let pats: &[&str] = &[
        "a", "$X", "\"", "'", "$(", "${", "`", "\\", " ", ";", "|", "&&", "$U", "*", "${U:-",
        "\"$U\" ", "x ",
    ];
    let sizes: Vec<usize> = std::env::var("OXSH_FUZZ_SIZES")
        .ok()
        .map(|v| v.split(',').filter_map(|x| x.parse().ok()).collect())
        .unwrap_or(vec![1024, 16 * 1024, 120 * 1024]);
    let mut fails = vec![];
    for p in pats {
        for &n in &sizes {
            let mut s = b"/bin/echo ".to_vec();
            while s.len() < n {
                s.extend_from_slice(p.as_bytes());
            }
            s.truncate(n.min(128 * 1024 - 1)); // MAX_ARG_STRLEN
            let o = run(&s, &[]);
            if let Err(e) = robust(&o).and_then(|_| no_write_unless_gt(&s, &o)) {
                fails.push(format!("pattern {p:?} x {n} bytes: {e} ({:?})", o.elapsed));
            }
        }
    }
    if !fails.is_empty() {
        panic!("{} failure(s):\n{}", fails.len(), fails.join("\n"));
    }
}

/// I3 + correctness: nesting depth.
#[test]
fn nesting_ladder() {
    let depths: Vec<usize> = std::env::var("OXSH_FUZZ_DEPTHS")
        .ok()
        .map(|v| v.split(',').filter_map(|x| x.parse().ok()).collect())
        .unwrap_or(vec![1, 4, 16, 48]);
    let mut fails = vec![];
    for &d in &depths {
        let forms: Vec<(String, Option<&str>)> = vec![
            (
                format!("/bin/echo {}x{}", "$(/bin/echo ".repeat(d), ")".repeat(d)),
                Some("x\n"),
            ),
            (
                format!(
                    "/bin/echo \"{}x{}\"",
                    "$(/bin/echo \"".repeat(d),
                    "\")".repeat(d)
                ),
                Some("x\n"),
            ),
            (
                format!("/bin/echo {}x{}", "${U:-".repeat(d), "}".repeat(d)),
                Some("x\n"),
            ),
            (
                format!("/bin/echo {}x{}", "(".repeat(d), ")".repeat(d)),
                None,
            ),
            (format!("/bin/echo {}", "\"'".repeat(d)), None),
            (format!("/bin/echo {}", "$(".repeat(d)), None),
        ];
        for (s, want) in forms {
            let o = run(s.as_bytes(), &[]);
            let r = robust(&o)
                .and_then(|_| no_write_unless_gt(s.as_bytes(), &o))
                .and_then(|_| match want {
                    Some(w) if o.stdout != w.as_bytes() => {
                        Err(format!("stdout {:?}", String::from_utf8_lossy(&o.stdout)))
                    }
                    _ => Ok(()),
                });
            if let Err(e) = r {
                fails.push(format!("depth {d}: {}: {e}", &s[..s.len().min(80)]));
            }
        }
    }
    if !fails.is_empty() {
        panic!("{} failure(s):\n{}", fails.len(), fails.join("\n"));
    }
}

/// I3 debt (kept by PR-A): main reads argv with std::env::args(), which panics on non-UTF-8.
#[test]
#[ignore = "debt B-UTF8 (docs/design/expansion.md, Debt): non-UTF-8 argv panics in std::env::args(), as on main 1dbfa00; enable when fixed"]
fn non_utf8_is_robust() {
    check_gen(
        "non-utf8",
        &move |r: &mut Rng| {
            let mut s = gen_soup(r);
            let pos = r.below(s.len() + 1);
            let bad: &[&[u8]] = &[
                b"\xff",
                b"\xc3",
                b"\xed\xa0\x80",
                b"\xc0\xaf",
                b"\xf4\x90\x80\x80",
            ];
            s.splice(pos..pos, r.pick(bad).iter().copied());
            s
        },
        &[],
        &no_write_unless_gt,
    );
}
