# Behaviour corpus

Versioned shell-behaviour cases for the lexer/expander (issue #10), run by
`tests/corpus_runner.rs` as part of `cargo test`. Design and decisions:
`docs/design/expansion.md` (§9, §10).

| File | Cases | Source |
|---|---|---|
| `maintainer.jsonl` | 1,039 | maintainer corpus: #28 review rounds, historical regressions, curated findings |
| `validator.jsonl` | 155 | adversarial validator, written before the implementation |
| `advocate.jsonl` | 215 | adversarial advocate (main-compatibility objections) |
| `refuter.jsonl` | 298 | adversarial refuter (invariant violations, I1–I6) |

## Running

```
cargo test --test corpus_runner                     # all cases
OXSH_CORPUS_FILTER=rf-02 cargo test --test corpus_runner -- --nocapture
```

The summary (file, class, pass/fail/skip counts) is printed to stderr. Knobs:
`OXSH_CORPUS_FILTER` (substring of the case id), `OXSH_CORPUS_JOBS` (parallel runs,
default min(cores, 8)), `OXSH_CORPUS_RECORD=path` (write observations as JSONL).

The scripts are adversarial shell input (they create, truncate and delete files in
their fixture directory). Each case runs in a fresh temporary directory with a clean
environment, but developers should run the corpus inside a sandbox.

## Case schema

One JSON object per line:

- `id`, `class` (E, R, K, B-fix, B-err, B-debt; see the design doc), `decision`, `note`.
- Input: `script` (or `script_hex` for non-UTF-8 bytes, `script_runs` for long
  generated input as `[[unit, count], ...]`; `"script": null` means `oxsh -c` with no
  script), optional `args` / `args_hex` / `args_runs`, `env` (a `NAME_HEX` key sets
  `NAME` to raw bytes), `home_files` (written into HOME, e.g. `.oxshrc`).
- `fixture`: `basic-v1` = `a.txt` ("DATA\n"), `b.txt` (empty), `b.log` ("x\n"),
  `.hidden` (empty), `sub/`; `basic-v1+zz` adds an executable `zz` that sleeps 3 s;
  `adv-v1` adds `data.json` and `my file.txt`.
- Environment: `PATH=/usr/local/bin:/usr/bin:/bin`, `HOME=<T>/home`, `SHELL=/bin/sh`,
  `LANG=C.UTF-8`, `TERM=dumb`, stdin `/dev/null`, cwd `<T>/work`, 10 s limit.
- `expect`: any of `rc`, `rc_not`, `stdout` (or `stdout_lines`, sorted lines, when a
  background job's `[bg] pid=N` notice races with output; or `stdout_len` +
  `stdout_fnv64` for long output), `stdout_contains`, `stdout_absent`, `stderr_absent`,
  `stderr_nonempty`, `work` and `home` (every file: first 200 bytes, lossy UTF-8; an
  empty directory is `"dir/": null`), `files_present`, `files_absent`, `file_equals`,
  `file_max_lines` (`~/name` means HOME), `no_new_files`, `home_unchanged`, `max_ms`.
  The temp dir is written `<T>` and background pids `pid=N`.
- Every run must also not panic, die by a signal or time out, unless `expect` has
  `allow_panic` naming the debt it belongs to.
- `expect_source`: `main` (equal to main's recorded run), `reference` (equal to bash),
  `case` (the reviewer's own constraints hold), or `spec: ...` / `pinned: ...` with
  the decision or debt that defines the expectation.
- `skip`: the reason a case is not run (PTY-only cases, a nondeterministic case, cases
  that hang until killed as on main). Skips are counted in the summary.

A case with neither `expect` nor `skip` is unresolved and fails the test.

## Changing an expectation

Behaviour changes are reviewed, not re-recorded wholesale. Record the new behaviour
with `OXSH_CORPUS_RECORD`, compare it with the old expectation, and update only the
cases a recorded decision covers, with that decision in `expect_source`. Reference
(bash) outputs were recorded once; bash is never run by the tests.
