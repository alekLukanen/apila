use std::path::PathBuf;

use super::end_turn::EndTurnTool;
use super::tool::{Tool, ToolContext};

/// The context an agent with nothing configured would hand a tool.
fn context() -> ToolContext {
    ToolContext::new(PathBuf::from("."), serde_json::json!({}))
}

#[test]
fn the_end_turn_tool_is_on_for_every_agent() {
    assert!(EndTurnTool::new().always_enabled());
}

#[test]
fn ending_the_turn_reports_that_the_turn_is_over() {
    let tool = EndTurnTool::new();
    let mut state = tool.new_state(&context()).expect("start the tool");

    let output = state
        .run(&context(), &serde_json::json!({"summary": "read the parser"}))
        .expect("end the turn");

    assert!(output.ends_turn());
    assert_eq!(output.content(), "turn ended: read the parser");
}

#[test]
fn ending_the_turn_without_a_summary_still_ends_it() {
    let tool = EndTurnTool::new();
    let mut state = tool.new_state(&context()).expect("start the tool");

    let output = state
        .run(&context(), &serde_json::json!({}))
        .expect("end the turn");

    assert!(output.ends_turn());
    assert_eq!(output.content(), "turn ended");
}

/// A model that sends `{"summary": "  "}` meant to send nothing, and refusing
/// the call over it would leave the agent running.
#[test]
fn a_blank_summary_reads_as_no_summary() {
    let tool = EndTurnTool::new();
    let mut state = tool.new_state(&context()).expect("start the tool");

    let output = state
        .run(&context(), &serde_json::json!({"summary": "   "}))
        .expect("end the turn");

    assert_eq!(output.content(), "turn ended");
}
