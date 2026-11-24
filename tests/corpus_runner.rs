//! Versioned behaviour corpus for the lexer/expander (issue #10).
//!
//! Runs every case in `tests/corpus/*.jsonl` with `oxsh -c` in a fresh
//! fixture directory and checks the recorded expectation. No reference shell
//! is needed (bash is never run in CI). See `tests/corpus/README.md` for the
//! schema, and docs/design/expansion.md for where each expectation comes from.
//!
//! - A case with `skip` is not run; skips are counted and printed.
//! - A case with neither `expect` nor `skip` is unresolved and fails.
//! - Knobs: `OXSH_CORPUS_FILTER=<substring of id>`, `OXSH_CORPUS_JOBS=<n>`,
//!   `OXSH_CORPUS_RECORD=<path>` (write observations as JSONL; used to
//!   produce snapshots after review, never in CI).
#![cfg(unix)]

use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(10);
const OUT_CAP: usize = 1 << 20;

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len() / 2)
        .filter_map(|i| s.get(2 * i..2 * i + 2))
        .filter_map(|h| u8::from_str_radix(h, 16).ok())
        .collect()
}

/// `[[unit, count], ...]` -> the units repeated and concatenated (long generated inputs).
fn expand_runs(v: &Value) -> String {
    let mut s = String::new();
    for r in v.as_array().into_iter().flatten() {
        let unit = r[0].as_str().unwrap_or("");
        s.push_str(&unit.repeat(r[1].as_u64().unwrap_or(1) as usize));
    }
    s
}

/// FNV-1a 64-bit, for long outputs stored as length + hash.
fn fnv64(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h = (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn fixture(work: &Path, kind: &str) {
    std::fs::create_dir_all(work.join("sub")).unwrap();
    for (f, c) in [
        ("a.txt", "DATA\n"),
        ("b.txt", ""),
        ("b.log", "x\n"),
        (".hidden", ""),
    ] {
        std::fs::write(work.join(f), c).unwrap();
    }
    match kind {
        "basic-v1" => {}
        "basic-v1+zz" => {
            use std::os::unix::fs::PermissionsExt;
            std::fs::write(work.join("zz"), "#!/bin/sh\nsleep 3\n").unwrap();
            std::fs::set_permissions(work.join("zz"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        "adv-v1" => {
            std::fs::write(
                work.join("data.json"),
                "[{\"name\":\"a b\",\"age\":30},{\"name\":\"c\",\"age\":10}]\n",
            )
            .unwrap();
            std::fs::write(work.join("my file.txt"), "SPACE\n").unwrap();
        }
        other => panic!("unknown fixture {other}"),
    }
}

/// Files under `root`: path -> first 200 bytes (lossy UTF-8); empty dirs as `dir/` -> null.
fn snap(root: &Path, t: &str) -> BTreeMap<String, Value> {
    fn walk(dir: &Path, root: &Path, t: &str, out: &mut BTreeMap<String, Value>) {
        let Ok(rd) = std::fs::read_dir(dir) else {
            return;
        };
        for e in rd.flatten() {
            let p = e.path();
            let rel = p.strip_prefix(root).unwrap().to_string_lossy().into_owned();
            let Ok(md) = std::fs::symlink_metadata(&p) else {
                continue;
            };
            if md.is_dir() {
                if std::fs::read_dir(&p)
                    .map(|mut r| r.next().is_none())
                    .unwrap_or(false)
                {
                    out.insert(format!("{rel}/"), Value::Null);
                } else {
                    walk(&p, root, t, out);
                }
            } else {
                let mut buf = Vec::new();
                let v = match std::fs::File::open(&p) {
                    Ok(f) => {
                        let _ = f.take(200).read_to_end(&mut buf);
                        Value::String(norm(&String::from_utf8_lossy(&buf), t))
                    }
                    Err(e) => Value::String(format!("<unreadable {:?}>", e.kind())),
                };
                out.insert(rel, v);
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(root, root, t, &mut out);
    out
}

/// Replace the temp dir with `<T>` and background pids with `N`.
fn norm(s: &str, t: &str) -> String {
    let s = s.replace(t, "<T>");
    let mut out = String::with_capacity(s.len());
    let mut rest = s.as_str();
    while let Some(i) = rest.find("pid=") {
        out.push_str(&rest[..i + 4]);
        rest = &rest[i + 4..];
        let digits = rest.chars().take_while(char::is_ascii_digit).count();
        if digits > 0 {
            out.push('N');
            rest = &rest[digits..];
        }
    }
    out.push_str(rest);
    out
}

struct Obs {
    rc: Value,
    stdout: String,
    stderr: String,
    ms: u128,
    work: BTreeMap<String, Value>,
    home: BTreeMap<String, Value>,
    work0: BTreeMap<String, Value>,
    home0: BTreeMap<String, Value>,
}

fn run_case(bin: &str, c: &Value) -> Obs {
    let tmp = tempfile::tempdir().unwrap();
    let t = tmp.path().to_string_lossy().into_owned();
    let (work, home) = (tmp.path().join("work"), tmp.path().join("home"));
    std::fs::create_dir_all(&home).unwrap();
    fixture(&work, c["fixture"].as_str().unwrap_or("basic-v1"));
    if let Some(files) = c["home_files"].as_object() {
        for (k, v) in files {
            std::fs::write(home.join(k), v.as_str().unwrap_or("")).unwrap();
        }
    }
    let (work0, home0) = (snap(&work, &t), snap(&home, &t));
    // `"script": null` means `oxsh -c` with no script argument at all.
    let script: Option<OsString> = if let Some(h) = c["script_hex"].as_str() {
        Some(OsString::from_vec(unhex(h)))
    } else if let Some(s) = c["script"].as_str() {
        Some(s.into())
    } else if c["script_runs"].is_array() {
        Some(expand_runs(&c["script_runs"]).into())
    } else if c.get("script").is_some_and(Value::is_null) {
        None
    } else {
        panic!("{}: no script", c["id"])
    };
    let mut cmd = Command::new(bin);
    cmd.arg("-c").args(script);
    if let Some(h) = c["args_hex"].as_array() {
        cmd.args(
            h.iter()
                .map(|a| OsString::from_vec(unhex(a.as_str().unwrap_or("")))),
        );
    } else if let Some(a) = c["args_runs"].as_array() {
        cmd.args(a.iter().map(expand_runs));
    } else if let Some(a) = c["args"].as_array() {
        cmd.args(a.iter().map(|a| a.as_str().unwrap_or("")));
    }
    cmd.env_clear()
        .env("PATH", "/usr/local/bin:/usr/bin:/bin")
        .env("HOME", &home)
        .env("SHELL", "/bin/sh")
        .env("LANG", "C.UTF-8")
        .env("TERM", "dumb");
    if let Some(e) = c["env"].as_object() {
        for (k, v) in e {
            let v = v.as_str().unwrap_or("");
            match k.strip_suffix("_HEX") {
                Some(k) => cmd.env(k, OsString::from_vec(unhex(v))),
                None => cmd.env(k, v),
            };
        }
    }
    cmd.current_dir(&work)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let start = Instant::now();
    let mut child = cmd.spawn().expect("spawn oxsh");
    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let ro = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = (&mut so).take(OUT_CAP as u64).read_to_end(&mut b);
        let _ = std::io::copy(&mut so, &mut std::io::sink());
        b
    });
    let re = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = (&mut se).take(OUT_CAP as u64).read_to_end(&mut b);
        let _ = std::io::copy(&mut se, &mut std::io::sink());
        b
    });
    let rc = loop {
        match child.try_wait().unwrap() {
            Some(st) => {
                break st
                    .code()
                    .map(|c| json!(c))
                    .unwrap_or_else(|| json!(-st.signal().unwrap_or(0)));
            }
            None if start.elapsed() > TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                break json!("timeout");
            }
            None => std::thread::sleep(Duration::from_millis(2)),
        }
    };
    let ms = start.elapsed().as_millis();
    // Background children may keep the pipes open; do not wait for them forever.
    let join = |h: std::thread::JoinHandle<Vec<u8>>| {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !h.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        if h.is_finished() {
            h.join().unwrap_or_default()
        } else {
            Vec::new()
        }
    };
    let (out, err) = (join(ro), join(re));
    Obs {
        rc,
        stdout: norm(&String::from_utf8_lossy(&out), &t),
        stderr: norm(&String::from_utf8_lossy(&err), &t),
        ms,
        work: snap(&work, &t),
        home: snap(&home, &t),
        work0,
        home0,
    }
}

fn as_map(v: &Value) -> BTreeMap<String, Value> {
    v.as_object()
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
        .unwrap_or_default()
}

fn strs(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// A file in the snapshot; `~/name` is under HOME, anything else under the work dir.
fn lookup<'a>(o: &'a Obs, path: &str) -> Option<&'a Value> {
    match path.strip_prefix("~/") {
        Some(p) => o.home.get(p),
        None => o.work.get(path).or_else(|| o.work.get(&format!("{path}/"))),
    }
}

/// Every mismatch between the observation and the expectation.
fn check(o: &Obs, e: &Map<String, Value>) -> Vec<String> {
    let mut v = Vec::new();
    // Always: no panic, no death by signal, no hang (I3).
    // `allow_panic` (with the debt it belongs to) pins a known panic of main.
    if (o.rc == json!(101) || o.stderr.contains("panicked")) && e.get("allow_panic").is_none() {
        v.push(format!("panic: {:?}", o.stderr));
    }
    if o.rc.as_i64().is_some_and(|rc| rc < 0) || o.rc == json!("timeout") {
        v.push(format!("abnormal exit {}", o.rc));
    }
    if let Some(rc) = e.get("rc")
        && *rc != o.rc
    {
        v.push(format!("rc {} != {}", o.rc, rc));
    }
    if let Some(bad) = e.get("rc_not").and_then(Value::as_array)
        && bad.contains(&o.rc)
    {
        v.push(format!("rc {} is one of {bad:?}", o.rc));
    }
    if let Some(s) = e.get("stdout").and_then(Value::as_str)
        && s != o.stdout
    {
        v.push(format!("stdout {:?} != {:?}", o.stdout, s));
    }
    if let Some(want) = e.get("stdout_lines").map(strs) {
        let mut got: Vec<String> = o.stdout.split_inclusive('\n').map(String::from).collect();
        got.sort();
        if got != want {
            v.push(format!("stdout lines {got:?} != {want:?}"));
        }
    }
    if let Some(n) = e.get("stdout_len").and_then(Value::as_u64)
        && (n != o.stdout.chars().count() as u64
            || e.get("stdout_fnv64").and_then(Value::as_str) != Some(fnv64(&o.stdout).as_str()))
    {
        v.push(format!(
            "long stdout differs (len {})",
            o.stdout.chars().count()
        ));
    }
    if let Some(b) = e.get("stderr_nonempty").and_then(Value::as_bool)
        && b != !o.stderr.is_empty()
    {
        v.push(format!("stderr {:?} (expected non-empty: {b})", o.stderr));
    }
    if let Some(w) = e.get("work")
        && as_map(w) != o.work
    {
        v.push(format!("work {:?} != {:?}", o.work, as_map(w)));
    }
    if let Some(h) = e.get("home")
        && as_map(h) != o.home
    {
        v.push(format!("home {:?} != {:?}", o.home, as_map(h)));
    }
    for s in strs(e.get("stdout_contains").unwrap_or(&Value::Null)) {
        if !o.stdout.contains(&s) {
            v.push(format!("stdout lacks {s:?}"));
        }
    }
    for s in strs(e.get("stdout_absent").unwrap_or(&Value::Null)) {
        if o.stdout.contains(&s) {
            v.push(format!("stdout has {s:?}"));
        }
    }
    for s in strs(e.get("stderr_absent").unwrap_or(&Value::Null)) {
        if o.stderr.contains(&s) {
            v.push(format!("stderr has {s:?}"));
        }
    }
    for f in strs(e.get("files_absent").unwrap_or(&Value::Null)) {
        if lookup(o, &f).is_some() {
            v.push(format!("unexpected file {f}"));
        }
    }
    for f in strs(e.get("files_present").unwrap_or(&Value::Null)) {
        if lookup(o, &f).is_none() {
            v.push(format!("missing file {f}"));
        }
    }
    for (f, want) in as_map(e.get("file_equals").unwrap_or(&Value::Null)) {
        if lookup(o, &f) != Some(&want) {
            v.push(format!("{f}={:?} != {want:?}", lookup(o, &f)));
        }
    }
    for (f, n) in as_map(e.get("file_max_lines").unwrap_or(&Value::Null)) {
        let lines = lookup(o, &f)
            .and_then(Value::as_str)
            .map_or(0, |t| t.matches('\n').count());
        if n.as_u64().is_some_and(|n| lines as u64 > n) {
            v.push(format!("{f} has {lines} lines (max {n})"));
        }
    }
    if e.get("no_new_files").and_then(Value::as_bool) == Some(true)
        && (o.work != o.work0 || o.home != o.home0)
    {
        v.push("files changed".into());
    }
    if e.get("home_unchanged").and_then(Value::as_bool) == Some(true) && o.home != o.home0 {
        v.push("home changed".into());
    }
    if let Some(ms) = e.get("max_ms").and_then(Value::as_u64)
        && o.ms > u128::from(ms)
    {
        v.push(format!("slow {}ms > {ms}ms", o.ms));
    }
    v
}

/// (case index, status, problems, recorded observation)
type CaseResult = (usize, &'static str, Vec<String>, Option<Value>);

#[test]
fn corpus() {
    let bin = env!("CARGO_BIN_EXE_oxsh");
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let filter = std::env::var("OXSH_CORPUS_FILTER").ok();
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .collect();
    files.sort();
    let mut cases: Vec<(String, Value)> = Vec::new();
    for f in &files {
        let name = f.file_stem().unwrap().to_string_lossy().into_owned();
        for (n, line) in std::fs::read_to_string(f).unwrap().lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let c: Value =
                serde_json::from_str(line).unwrap_or_else(|e| panic!("{name}:{}: {e}", n + 1));
            if filter
                .as_deref()
                .is_some_and(|flt| !c["id"].as_str().unwrap_or("").contains(flt))
            {
                continue;
            }
            cases.push((name.clone(), c));
        }
    }
    let jobs: usize = std::env::var("OXSH_CORPUS_JOBS")
        .ok()
        .and_then(|j| j.parse().ok())
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(2)
                .min(8)
        });
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<CaseResult>> = Mutex::new(Vec::new());
    let record = std::env::var("OXSH_CORPUS_RECORD").ok();
    std::thread::scope(|s| {
        for _ in 0..jobs {
            s.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::SeqCst);
                let Some((_, c)) = cases.get(i) else { break };
                let (status, problems, rec) = if let Some(reason) = c["skip"].as_str() {
                    ("skip", vec![reason.to_string()], None)
                } else {
                    let o = run_case(bin, c);
                    let rec = record.as_ref().map(|_| {
                        json!({"id": c["id"], "rc": o.rc, "stdout": o.stdout, "stderr": o.stderr, "ms": o.ms as u64,
                               "work": o.work, "home": o.home})
                    });
                    match c["expect"].as_object() {
                        None => ("unresolved", vec!["no expect and no skip".into()], rec),
                        Some(e) => {
                            let p = check(&o, e);
                            (if p.is_empty() { "pass" } else { "fail" }, p, rec)
                        }
                    }
                };
                results.lock().unwrap().push((i, status, problems, rec));
            });
        }
    });
    let mut results = results.into_inner().unwrap();
    results.sort_by_key(|r| r.0);
    if let Some(path) = record {
        let lines: Vec<String> = results
            .iter()
            .filter_map(|r| r.3.as_ref().map(Value::to_string))
            .collect();
        std::fs::write(path, lines.join("\n") + "\n").unwrap();
    }
    let mut table: BTreeMap<(String, String, &str), usize> = BTreeMap::new();
    let mut bad = Vec::new();
    for (i, status, problems, _) in &results {
        let (file, c) = &cases[*i];
        let class = c["class"].as_str().unwrap_or("?").to_string();
        *table.entry((file.clone(), class, *status)).or_default() += 1;
        if matches!(*status, "fail" | "unresolved") {
            bad.push(format!(
                "{file}/{} [{}]: {}\n    script: {:?}",
                c["id"].as_str().unwrap_or("?"),
                status,
                problems.join("; "),
                c["script"]
            ));
        }
    }
    eprintln!("corpus summary (file, class, status: count):");
    for ((f, cl, st), n) in &table {
        eprintln!("  {f:<12} {cl:<7} {st:<10} {n}");
    }
    let skipped = results.iter().filter(|r| r.1 == "skip").count();
    eprintln!("  total {} cases, {} skipped", results.len(), skipped);
    assert!(
        bad.is_empty(),
        "{} corpus case(s) failed:\n{}",
        bad.len(),
        bad.join("\n")
    );
}
