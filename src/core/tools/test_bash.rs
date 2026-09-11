use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};
use std::{env, fs, thread};

use super::bash::{BashTool, MAX_STREAM_BYTES};
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
    // the kept bytes, the headings and the truncation note, and nothing else
    assert!(
        content.len() < MAX_STREAM_BYTES + 512,
        "the output was not bounded: {} bytes",
        content.len()
    );
    assert!(
        content.contains(&format!("{} more bytes", 200_000 - MAX_STREAM_BYTES)),
        "the dropped byte count is wrong: {}",
        &content[content.len().saturating_sub(120)..]
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

/// Observed rather than asserted about the constant: a command that outlives
/// any plausible small timeout still finishes, which it only can if the fallback
/// is the generous default.
#[test]
fn the_timeout_falls_back_to_the_default_when_the_config_leaves_it_out() {
    let dir = temp_dir("default-timeout");

    let content = run(&dir, serde_json::json!({}), "sleep 2; echo outlasted");

    assert!(content.contains("outlasted"), "{}", content);
    assert!(content.contains("exit code: 0"), "{}", content);
    assert!(!content.contains("timed out"), "{}", content);
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

/// Whether any process on this machine was started from a command line holding
/// `marker`. Used to watch what a command left behind.
fn any_process_matching(marker: &str) -> bool {
    Command::new("pgrep")
        .arg("-f")
        .arg(marker)
        .output()
        .map(|output| !output.stdout.is_empty())
        .unwrap_or(false)
}

fn wait_until_gone(marker: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if !any_process_matching(marker) {
            return true;
        }
        thread::sleep(Duration::from_millis(20));
    }
    false
}

/// A command can exit while something it started keeps the pipe open and keeps
/// writing. The reader has to stop rather than read and discard for the life of
/// the process, and dropping its end of the pipe is what finishes off the
/// orphan — so the orphan going away is the proof that the reader stopped.
#[test]
fn a_command_that_leaves_something_writing_does_not_read_forever() {
    let dir = temp_dir("orphan-writer");
    let marker = format!("apila-probe-writer-{}", std::process::id());

    let content = run(
        &dir,
        serde_json::json!({"bash_timeout": 30}),
        &format!("yes {} &", marker),
    );

    assert!(content.contains("exit code: 0"), "{}", content);
    // the reader never reached the end of the pipe, so the output it captured is
    // what had arrived rather than all there was, and it says so
    assert!(
        content.contains("still writing"),
        "the output claimed to be complete: {}",
        &content[..content.len().min(200)]
    );
    assert!(
        wait_until_gone(&marker),
        "something is still reading the orphan's output, so it never got a broken pipe"
    );
}

/// The message the model reads has to be true: work that is still running is
/// work it must not assume was undone.
#[test]
fn work_a_timed_out_command_started_is_killed_with_it() {
    let dir = temp_dir("timeout-group");
    let marker = dir.join("survived.txt");

    let content = run(
        &dir,
        serde_json::json!({"bash_timeout": 1}),
        // the shell exits as soon as it is killed, but the work it forked would
        // carry on writing into the agent's directory
        "(sleep 3; echo survived > survived.txt) & wait",
    );

    assert!(
        content.contains("everything it started were killed"),
        "{}",
        content
    );

    // long enough that the work would have finished had it lived
    thread::sleep(Duration::from_secs(4));
    assert!(
        !marker.exists(),
        "the command was reported killed but its work carried on"
    );
}

/// A command that deliberately detaches is left alone when it exits normally:
/// an agent starting a long running server is doing that on purpose. Only a
/// timeout takes the whole group.
#[test]
fn a_command_that_exits_normally_does_not_have_its_group_killed() {
    let dir = temp_dir("no-group-kill");
    let marker = dir.join("finished.txt");

    let content = run(
        &dir,
        serde_json::json!({"bash_timeout": 30}),
        // redirected away from the pipe, so nothing depends on the reader
        "(sleep 1; echo finished > finished.txt) > /dev/null 2>&1 & echo started",
    );

    assert!(content.contains("exit code: 0"), "{}", content);
    thread::sleep(Duration::from_secs(2));
    assert!(marker.exists(), "detached work was killed: {}", content);
}
