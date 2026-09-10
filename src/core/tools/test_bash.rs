use std::path::PathBuf;
use std::time::Instant;
use std::{env, fs};

use super::bash::{BashTool, DEFAULT_BASH_TIMEOUT, MAX_STREAM_BYTES};
use super::tool::{Tool, ToolContext, ToolError, ToolState};

/// Creates a unique temp directory for a single test to work in.
fn temp_dir(name: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("apila-test-bash-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

/// A started bash tool working in `dir`, configured with `config`.
fn state(dir: &PathBuf, config: serde_json::Value) -> (ToolContext, Box<dyn ToolState>) {
    let context = ToolContext::new(dir.clone(), config);
    let state = BashTool::new().new_state(&context).expect("start the tool");
    (context, state)
}

fn run(dir: &PathBuf, config: serde_json::Value, command: &str) -> String {
    let (context, mut state) = state(dir, config);
    state
        .run(&context, &serde_json::json!({ "command": command }))
        .expect("run the command")
        .content()
}

#[test]
fn a_command_runs_in_the_agents_own_directory() {
    let dir = temp_dir("working-dir");
    fs::write(dir.join("marker.txt"), "found me").expect("write marker");

    // a marker file rather than `pwd`, which canonicalises a symlinked temp
    // directory and would let the test pass for the wrong reason
    let content = run(&dir, serde_json::json!({}), "cat marker.txt");

    assert!(content.contains("found me"), "{}", content);
    assert!(content.contains("exit code: 0"), "{}", content);
}

#[test]
fn the_exit_code_and_the_error_output_come_back() {
    let dir = temp_dir("exit-code");

    let content = run(&dir, serde_json::json!({}), "echo boom >&2; exit 3");

    assert!(content.contains("exit code: 3"), "{}", content);
    assert!(content.contains("boom"), "{}", content);
    assert!(content.contains("stdout:\n(none)"), "{}", content);
}

/// A command that fails is not the tool failing: the model is the one that has
/// to read what went wrong and try something else.
#[test]
fn a_failing_command_is_output_rather_than_an_error() {
    let dir = temp_dir("failing");
    let (context, mut state) = state(&dir, serde_json::json!({}));

    let output = state.run(&context, &serde_json::json!({"command": "exit 1"}));

    assert!(output.is_ok());
}

#[test]
fn a_command_that_runs_too_long_is_killed() {
    let dir = temp_dir("timeout");
    let started = Instant::now();

    let content = run(&dir, serde_json::json!({"bash_timeout": 1}), "sleep 30");

    assert!(content.contains("timed out after 1s"), "{}", content);
    assert!(started.elapsed().as_secs() < 10, "the command was not killed");
}

/// The readers are threads on purpose: a pipe nobody drains fills at about
/// 64KB and stops the command dead, which the poll loop would then sit through
/// until the timeout. This is the test that fails if that ever changes.
#[test]
fn a_command_that_prints_more_than_a_pipe_holds_still_finishes() {
    let dir = temp_dir("big-output");
    let started = Instant::now();

    let content = run(
        &dir,
        serde_json::json!({"bash_timeout": 30}),
        "head -c 400000 /dev/zero | tr '\\0' 'a'",
    );

    assert!(content.contains("exit code: 0"), "{}", content);
    assert!(started.elapsed().as_secs() < 20, "the command blocked");
}

#[test]
fn output_longer_than_the_limit_is_cut_short() {
    let dir = temp_dir("truncated");

    let content = run(
        &dir,
        serde_json::json!({"bash_timeout": 30}),
        "head -c 200000 /dev/zero | tr '\\0' 'a'",
    );

    assert!(content.contains("… truncated,"), "cut short");
    assert!(
        content.len() < MAX_STREAM_BYTES * 3,
        "the output was not bounded: {} bytes",
        content.len()
    );
}

#[test]
fn a_call_without_a_command_is_an_error() {
    let dir = temp_dir("no-command");
    let (context, mut state) = state(&dir, serde_json::json!({}));

    let err = state
        .run(&context, &serde_json::json!({}))
        .expect_err("no command");

    assert!(matches!(err, ToolError::MissingArgument { argument } if argument == "command"));
}

#[test]
fn a_command_that_is_not_a_string_is_an_error() {
    let dir = temp_dir("bad-command");
    let (context, mut state) = state(&dir, serde_json::json!({}));

    let err = state
        .run(&context, &serde_json::json!({"command": 7}))
        .expect_err("not a string");

    assert!(matches!(err, ToolError::InvalidArgument { argument, .. } if argument == "command"));
}

#[test]
fn an_empty_command_is_an_error() {
    let dir = temp_dir("empty-command");
    let (context, mut state) = state(&dir, serde_json::json!({}));

    let err = state
        .run(&context, &serde_json::json!({"command": "   "}))
        .expect_err("empty command");

    assert!(matches!(err, ToolError::InvalidArgument { .. }));
}

#[test]
fn a_command_that_cannot_be_started_is_reported() {
    let dir = temp_dir("missing-dir").join("gone");
    let (context, mut state) = state(&dir, serde_json::json!({}));

    let err = state
        .run(&context, &serde_json::json!({"command": "echo hi"}))
        .expect_err("no such directory");

    assert!(matches!(err, ToolError::NotStarted { .. }));
}

#[test]
fn the_timeout_falls_back_to_the_default_when_the_config_leaves_it_out() {
    let config = serde_json::json!({});

    assert!(BashTool::new().validate_config(&config).is_ok());
    // the default is long enough that a quick command is nowhere near it
    assert!(DEFAULT_BASH_TIMEOUT >= 30);
}

/// A misspelled setting does nothing at all if it is quietly ignored, so it is
/// refused when the agent's files are read instead.
#[test]
fn a_misspelled_setting_is_rejected_when_the_config_is_checked() {
    let err = BashTool::new()
        .validate_config(&serde_json::json!({"bash_timout": 30}))
        .expect_err("unknown field");

    assert!(err.contains("bash_timout"), "{}", err);
}

#[test]
fn the_settings_are_read_once_when_the_tool_starts() {
    let context = ToolContext::new(PathBuf::from("."), serde_json::json!({"bash_timout": 30}));

    let err = BashTool::new()
        .new_state(&context)
        .err()
        .expect("bad settings");

    assert!(matches!(err, ToolError::InvalidConfig { .. }));
}
