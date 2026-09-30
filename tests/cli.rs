//! Integration smoke tests — validate that the binary builds and the
//! `assert_cmd` harness works end-to-end. Behavioral integration tests
//! (chains, pipelines, redirects) build on top of this in later phases.

use assert_cmd::Command;
use predicates::prelude::*;

fn oxsh() -> Command {
    Command::cargo_bin("oxsh").expect("binary `oxsh` should build")
}

#[test]
fn issue_10_preserves_quoted_and_escaped_variables() {
    oxsh()
        .args(["-c", "echo '$OXSH_ISSUE10_VALUE' \"$OXSH_ISSUE10_VALUE\""])
        .env("OXSH_ISSUE10_VALUE", "secret")
        .assert()
        .success()
        .stdout(predicate::str::contains("$OXSH_ISSUE10_VALUE secret"));

    oxsh()
        .args(["-c", "echo pre'$OXSH_ISSUE10_VALUE'-\"$OXSH_ISSUE10_VALUE\""])
        .env("OXSH_ISSUE10_VALUE", "secret")
        .assert()
        .success()
        .stdout(predicate::str::contains("pre$OXSH_ISSUE10_VALUE-secret"));

    oxsh()
        .args(["-c", "echo \\$OXSH_ISSUE10_VALUE"])
        .env("OXSH_ISSUE10_VALUE", "secret")
        .assert()
        .success()
        .stdout(predicate::str::contains("$OXSH_ISSUE10_VALUE"));
}

#[test]
fn issue_10_preserves_quoted_and_expansion_produced_operators() {
    oxsh()
        .args(["-c", "echo '>' \\> $OXSH_ISSUE10_OPERATOR"])
        .env("OXSH_ISSUE10_OPERATOR", ">")
        .assert()
        .success()
        .stdout(predicate::str::contains("> > >"));
}

#[test]
fn issue_10_redirects_only_on_genuine_lexical_operator() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let victim = dir.path().join("victim");
    std::fs::write(&victim, "preserved").expect("seed victim file");
    let victim_arg = victim.to_str().unwrap();

    oxsh()
        .current_dir(dir.path())
        .args(["-c", &format!("printf \"%s\" '>' {victim_arg}")])
        .assert()
        .success();
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "preserved");

    oxsh()
        .current_dir(dir.path())
        .args(["-c", &format!("printf '%s' \\> {victim_arg}")])
        .assert()
        .success();
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "preserved");

    oxsh()
        .current_dir(dir.path())
        .args(["-c", &format!("printf \"%s\" $OXSH_ISSUE10_OPERATOR {victim_arg}")])
        .env("OXSH_ISSUE10_OPERATOR", ">")
        .assert()
        .success();
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "preserved");

    oxsh()
        .current_dir(dir.path())
        .args(["-c", "printf '%s' redirected > actual"])
        .assert()
        .success();
    assert_eq!(std::fs::read_to_string(dir.path().join("actual")).unwrap(), "redirected");
}

#[test]
fn issue_10_private_use_characters_are_not_internal_markers() {
    let private_use = '\u{e000}'.to_string();
    oxsh()
        .args(["-c", &format!("/usr/bin/printf '%s' '{private_use}'")])
        .assert()
        .success()
        .stdout(predicate::eq(private_use.clone()));

    oxsh()
        .args(["-c", "/usr/bin/printf '%s' \"$OXSH_ISSUE10_PRIVATE\""])
        .env("OXSH_ISSUE10_PRIVATE", &private_use)
        .assert()
        .success()
        .stdout(predicate::eq(private_use));
}

#[test]
fn issue_10_alias_expansion_preserves_original_pipeline_syntax() {
    oxsh()
        .args(["-c", "alias p='/usr/bin/printf %s'; p hello | /usr/bin/cat"])
        .assert()
        .success()
        .stdout(predicate::eq("hello"));
}

#[test]
fn issue_10_brace_expansion_preserves_following_pipeline_syntax() {
    oxsh()
        .args(["-c", "/usr/bin/printf '%s' {a,b} | /usr/bin/cat"])
        .assert()
        .success()
        .stdout(predicate::eq("ab"));
}

#[test]
fn issue_10_single_quoted_assignment_values_stay_literal() {
    oxsh()
        .args(["-c", "V='$OXSH_ISSUE10_VALUE'; /usr/bin/printf '%s' \"$V\""])
        .env("OXSH_ISSUE10_VALUE", "secret")
        .assert()
        .success()
        .stdout(predicate::eq("$OXSH_ISSUE10_VALUE"));

    oxsh()
        .args(["-c", "V='$OXSH_ISSUE10_VALUE' /usr/bin/printenv V"])
        .env("OXSH_ISSUE10_VALUE", "secret")
        .assert()
        .success()
        .stdout(predicate::eq("$OXSH_ISSUE10_VALUE\n"));
}

#[test]
fn issue_10_substitution_operators_do_not_redirect() {
    let dir = tempfile::tempdir().expect("temporary directory");
    let victim = dir.path().join("victim");
    std::fs::write(&victim, "preserved").expect("seed victim file");
    let victim_arg = victim.to_str().unwrap();

    oxsh()
        .current_dir(dir.path())
        .args(["-c", &format!("/usr/bin/printf '%s' $(/usr/bin/printf '>') {victim_arg}")])
        .assert()
        .success();
    assert_eq!(std::fs::read_to_string(&victim).unwrap(), "preserved");
}

#[test]
fn issue_10_substitution_output_is_not_variable_expanded_again() {
    oxsh()
        .args(["-c", "/usr/bin/printf '%s' \"$(/usr/bin/printf '$OXSH_ISSUE10_VALUE')\""])
        .env("OXSH_ISSUE10_VALUE", "secret")
        .assert()
        .success()
        .stdout(predicate::eq("$OXSH_ISSUE10_VALUE"));
}

#[test]
fn issue_10_quoted_command_substitution_does_not_glob() {
    let dir = tempfile::tempdir().expect("temporary directory");
    std::fs::write(dir.path().join("a.txt"), "a").unwrap();
    std::fs::write(dir.path().join("b.txt"), "b").unwrap();

    oxsh()
        .current_dir(dir.path())
        .args(["-c", "/usr/bin/printf '<%s>' \"$(/usr/bin/printf '*.txt')\""])
        .assert()
        .success()
        .stdout(predicate::eq("<*.txt>"));
}

#[test]
fn issue_10_quoted_backtick_substitution_does_not_glob() {
    let dir = tempfile::tempdir().expect("temporary directory");
    std::fs::write(dir.path().join("a.txt"), "a").unwrap();
    std::fs::write(dir.path().join("b.txt"), "b").unwrap();

    oxsh()
        .current_dir(dir.path())
        .args(["-c", r#"/usr/bin/printf '<%s>' "`/usr/bin/printf '*.txt'`""#])
        .assert()
        .success()
        .stdout(predicate::eq("<*.txt>"));
}

#[test]
fn issue_10_mixed_fully_quoted_substitution_fragment_does_not_glob() {
    let dir = tempfile::tempdir().expect("temporary directory");
    std::fs::write(dir.path().join("xone.txt"), "x").unwrap();

    oxsh()
        .current_dir(dir.path())
        .args(["-c", "/usr/bin/printf '<%s>' \"$(/usr/bin/printf x)\"'*.txt'"])
        .assert()
        .success()
        .stdout(predicate::eq("<x*.txt>"));
}

#[test]
fn issue_10_tilde_expansion_keeps_quoted_glob_provenance() {
    let dir = tempfile::tempdir().expect("temporary directory");
    std::fs::write(dir.path().join("a.txt"), "a").unwrap();
    std::fs::write(dir.path().join("b.txt"), "b").unwrap();
    let expected = format!("{}/*.txt", dir.path().display());

    oxsh()
        .current_dir(dir.path())
        .args(["-c", "/usr/bin/printf '<%s>' \"$VAR\""])
        .env("HOME", dir.path())
        .env("VAR", "~/*.txt")
        .assert()
        .success()
        .stdout(predicate::eq(format!("<{expected}>")));
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

// ── Semicolon control-flow syntax (relates to #19) ──
// split_chain_ops must not shred the ';' inside `; do`/`; then`/`; done`.

#[test]
fn semicolon_for_loop_syntax_works() {
    oxsh()
        .args(["-c", "for i in 1 2 3; do echo $i; done"])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("1")
                .and(predicate::str::contains("2"))
                .and(predicate::str::contains("3")),
        );
}

#[test]
fn semicolon_if_then_runs_then_branch() {
    oxsh()
        .args(["-c", "if true; then echo yes; fi"])
        .assert()
        .success()
        .stdout(predicate::str::contains("yes"));
}

#[test]
fn semicolon_if_else_runs_else_branch() {
    oxsh()
        .args(["-c", "if false; then echo a; else echo b; fi"])
        .assert()
        .success()
        .stdout(predicate::str::contains("b").and(predicate::str::contains("a").not()));
}

#[test]
#[ignore = "Known limitation (#19): a control-flow statement followed by a chain \
            operator (e.g. `for ... done && echo ok`) is treated as a single statement, \
            so the trailing chain is not honored. Needs control-flow-aware segmentation."]
fn control_flow_followed_by_chain_operator() {
    oxsh()
        .args(["-c", "for i in 1 do echo $i done && echo ok"])
        .assert()
        .success()
        .stdout(predicate::str::contains("ok"));
}

// ── Wave 3 regressions ──

#[test]
fn explain_does_not_execute_external_commands() {
    // `?? cmd` must resolve the command, not run it.
    oxsh()
        .args(["-c", "?? ls"])
        .assert()
        .success()
        .stdout(predicate::str::contains("for usage"));
}

#[test]
fn explain_identifies_builtins() {
    oxsh()
        .args(["-c", "?? echo"])
        .assert()
        .success()
        .stdout(predicate::str::contains("builtin"));
}

#[test]
fn trailing_pipe_exits_cleanly() {
    // Empty trailing stage must not hang or leave the upstream child unwaited.
    oxsh().args(["-c", "echo hi |"]).assert().success();
}

#[test]
fn variable_values_are_not_expanded_twice() {
    // $FOO resolves to the literal "$BAR"; that result must NOT be expanded again.
    oxsh()
        .args(["-c", "echo $FOO"])
        .env("FOO", "$BAR")
        .env("BAR", "should-not-appear")
        .assert()
        .success()
        .stdout(
            predicate::str::contains("$BAR")
                .and(predicate::str::contains("should-not-appear").not()),
        );
}

// ── Issue #6 integration coverage ──

#[test]
fn stderr_merge_survives_pipeline() {
    // `2>&1` must redirect stderr into the pipe write-end so the downstream
    // stage receives it on stdin (and hence stdout). The pipeline exit code is
    // that of the last stage (cat → 0), so the overall command succeeds.
    oxsh()
        .args(["-c", "ls /no-such-dir-oxsh-2and1-test 2>&1 | cat"])
        .assert()
        .success()
        .stdout(predicate::str::contains("no-such-dir-oxsh-2and1-test"));
}

#[test]
fn env_prefix_sets_var_for_child_only() {
    // `VAR=val cmd` must inject VAR into cmd's environment.
    oxsh()
        .args(["-c", "OXSH_TESTVAR=hello123 printenv OXSH_TESTVAR"])
        .assert()
        .success()
        .stdout(predicate::str::contains("hello123"));
}

#[test]
fn exit_propagates_from_if_block() {
    oxsh()
        .args(["-c", "if true; then exit 42; fi"])
        .assert()
        .code(42);
}

#[test]
fn exit_propagates_from_for_block() {
    oxsh()
        .args(["-c", "for i in 1 2; do exit 7; done"])
        .assert()
        .code(7);
}

#[test]
fn exit_propagates_from_while_block() {
    oxsh()
        .args(["-c", "while true; do exit 3; done"])
        .assert()
        .code(3);
}

#[test]
fn partial_pipeline_consumer_does_not_hang() {
    // `head -1` closes the read-end of the pipe after one line; the upstream
    // producer must handle SIGPIPE / write errors without hanging.
    oxsh()
        .args(["-c", "printf 'line1\\nline2\\nline3\\n' | head -1"])
        .timeout(std::time::Duration::from_secs(5))
        .assert()
        .success()
        .stdout(predicate::str::contains("line1").and(predicate::str::contains("line2").not()));
}
