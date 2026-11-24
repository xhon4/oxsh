//! Exit status of commands terminated by a signal (shell convention 128+N).
//! Kept in its own file so it does not collide with other work on tests/cli.rs.
#![cfg(unix)]

use assert_cmd::Command;
use predicates::prelude::*;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

#[test]
fn signal_killed_child_reports_128_plus_signal() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let script = dir.path().join("self_term.sh");
    // A script file, so oxsh never sees (and expands) the `$$`.
    std::fs::write(&script, "#!/bin/sh\nkill -TERM $$\n").expect("write script");
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = script.to_str().unwrap();

    Command::cargo_bin("oxsh")
        .unwrap()
        .args(["-c", &format!("{path}; echo status=$?")])
        .timeout(Duration::from_secs(10))
        .assert()
        .success()
        .stdout(predicate::str::contains("status=143"));

    // The pipeline path reports the last stage's signal too.
    Command::cargo_bin("oxsh")
        .unwrap()
        .args(["-c", &format!("/usr/bin/printf x | {path}")])
        .timeout(Duration::from_secs(10))
        .assert()
        .code(143);
}
