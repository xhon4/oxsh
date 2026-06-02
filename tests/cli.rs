//! Integration smoke tests — validate that the binary builds and the
//! `assert_cmd` harness works end-to-end. Behavioral integration tests
//! (chains, pipelines, redirects) build on top of this in later phases.

use assert_cmd::Command;
use predicates::prelude::*;

fn oxsh() -> Command {
    Command::cargo_bin("oxsh").expect("binary `oxsh` should build")
}

#[test]
fn prints_version() {
    oxsh()
        .arg("--version")
        .assert()
        .success()
        .stdout(predicate::str::contains("oxsh"));
}

#[test]
fn dash_c_runs_a_command() {
    oxsh()
        .args(["-c", "echo hello"])
        .assert()
        .success()
        .stdout(predicate::str::contains("hello"));
}

#[test]
fn dash_c_reports_exit_code_of_false_builtin() {
    oxsh().args(["-c", "false"]).assert().code(1);
}

// ── Wave 1 regressions ──

#[test]
fn empty_while_condition_errors_instead_of_looping() {
    // `while  do ... done` (no condition) must fail fast, not spin to the cap.
    oxsh()
        .args(["-c", "while  do echo x done"])
        .env("OXSH_MAX_ITERATIONS", "5") // safety net if the guard regressed
        .assert()
        .failure()
        .stdout(predicate::str::contains("x").not())
        .stderr(predicate::str::contains("empty condition"));
}

#[test]
fn structured_stage_reports_missing_redirect_file() {
    // A structured stage reading a missing file must error, not silently feed
    // empty input downstream (#15).
    oxsh()
        .args(["-c", "to-table < /nonexistent/oxsh-test-file | cat"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("/nonexistent/oxsh-test-file"));
}

#[test]
fn for_loop_without_semicolons_works() {
    // Documents the currently-working control-flow syntax.
    oxsh()
        .args(["-c", "for i in 1 2 3 do echo $i done"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1").and(predicate::str::contains("3")));
}

#[test]
#[ignore = "ISSUE (new, relates to #19): semicolon control-flow syntax is broken — \
            split_chain_ops splits on the ';' in '; do'/'; then'/'; done' before the \
            control-flow parser sees it, so `for i in 1 2 3; do echo $i; done` errors \
            with 'for: No such file or directory'. Fix when unifying control-flow execution."]
fn semicolon_for_loop_syntax_works() {
    oxsh()
        .args(["-c", "for i in 1 2 3; do echo $i; done"])
        .assert()
        .success()
        .stdout(predicate::str::contains("1"));
}
