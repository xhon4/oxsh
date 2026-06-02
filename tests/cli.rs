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
