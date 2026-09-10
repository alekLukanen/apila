use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use crate::core::openrouter::types::{
    ChatCompletionRequest, FunctionCall, Message, ToolCall,
};
use crate::core::runtime::agent_config::{ToolConfig, ToolSettings};
use crate::core::tools::tool::{Tool, ToolContext, ToolError, ToolOutput, ToolState};

use super::tool_registry::{AgentTools, ToolRegistry, ToolSettingsError, ToolStates};

// Stub tools ////////////////////////
//////////////////////////////////////

/// A tool that hands its arguments straight back, so a test can see what
/// reached it.
struct EchoTool;

impl Tool for EchoTool {
    fn name(&self) -> String {
        "echo".into()
    }
    fn description(&self) -> String {
        "echoes".into()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn new_state(&self, context: &ToolContext) -> Result<Box<dyn ToolState>, ToolError> {
        Ok(Box::new(EchoState {
            config: context.config(),
        }))
    }
}

struct EchoState {
    config: serde_json::Value,
}

impl ToolState for EchoState {
    fn run(
        &mut self,
        _context: &ToolContext,
        arguments: &serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::new(
            serde_json::json!({"arguments": arguments, "config": self.config}).to_string(),
        ))
    }
}

/// A tool that is on for every agent whether it asked or not.
struct AlwaysOnTool;

impl Tool for AlwaysOnTool {
    fn name(&self) -> String {
        "always_on".into()
    }
    fn description(&self) -> String {
        "always on".into()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn always_enabled(&self) -> bool {
        true
    }
    fn new_state(&self, _context: &ToolContext) -> Result<Box<dyn ToolState>, ToolError> {
        Ok(Box::new(SilentState))
    }
}

/// A state that says nothing at all, for the empty output guard.
struct SilentState;

impl ToolState for SilentState {
    fn run(
        &mut self,
        _context: &ToolContext,
        _arguments: &serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::new(""))
    }
}

/// A tool that refuses every call it is given.
struct RefusingTool;

impl Tool for RefusingTool {
    fn name(&self) -> String {
        "refusing".into()
    }
    fn description(&self) -> String {
        "refuses".into()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn new_state(&self, _context: &ToolContext) -> Result<Box<dyn ToolState>, ToolError> {
        Ok(Box::new(RefusingState))
    }
}

struct RefusingState;

impl ToolState for RefusingState {
    fn run(
        &mut self,
        _context: &ToolContext,
        _arguments: &serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        Err(ToolError::MissingArgument {
            argument: "something".into(),
        })
    }
}

/// A tool whose state counts the calls it has been given. This is how a state
/// outliving a single call is actually observed: the count can only climb if
/// the same state came back.
struct CountingTool {
    started: Arc<AtomicUsize>,
}

impl Tool for CountingTool {
    fn name(&self) -> String {
        "counting".into()
    }
    fn description(&self) -> String {
        "counts".into()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn new_state(&self, _context: &ToolContext) -> Result<Box<dyn ToolState>, ToolError> {
        self.started.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(CountingState { calls: 0 }))
    }
}

struct CountingState {
    calls: usize,
}

impl ToolState for CountingState {
    fn run(
        &mut self,
        _context: &ToolContext,
        _arguments: &serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        self.calls += 1;
        Ok(ToolOutput::new(self.calls.to_string()))
    }
}

/// A tool that will not start the first time it is asked, standing in for a
/// connection that was refused once.
struct FlakyTool {
    attempts: Arc<AtomicUsize>,
}

impl Tool for FlakyTool {
    fn name(&self) -> String {
        "flaky".into()
    }
    fn description(&self) -> String {
        "flaky".into()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn new_state(&self, _context: &ToolContext) -> Result<Box<dyn ToolState>, ToolError> {
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err(ToolError::NotStarted {
                error: "the connection was refused".into(),
            });
        }
        Ok(Box::new(SilentState))
    }
}

/// A tool whose state says when it is dropped, standing in for one that has a
/// connection to close.
struct ClosingTool {
    closed: Arc<AtomicBool>,
}

impl Tool for ClosingTool {
    fn name(&self) -> String {
        "closing".into()
    }
    fn description(&self) -> String {
        "closes".into()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn new_state(&self, _context: &ToolContext) -> Result<Box<dyn ToolState>, ToolError> {
        Ok(Box::new(ClosingState {
            closed: Arc::clone(&self.closed),
        }))
    }
}

struct ClosingState {
    closed: Arc<AtomicBool>,
}

impl Drop for ClosingState {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

impl ToolState for ClosingState {
    fn run(
        &mut self,
        _context: &ToolContext,
        _arguments: &serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::new("open"))
    }
}

/// A tool that refuses any settings at all, for the config checks.
struct PickyTool;

impl Tool for PickyTool {
    fn name(&self) -> String {
        "picky".into()
    }
    fn description(&self) -> String {
        "picky".into()
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {}})
    }
    fn validate_config(&self, config: &serde_json::Value) -> Result<(), String> {
        match config.as_object().map(|object| object.is_empty()) {
            Some(true) => Ok(()),
            _ => Err("no settings are understood".into()),
        }
    }
    fn new_state(&self, _context: &ToolContext) -> Result<Box<dyn ToolState>, ToolError> {
        Ok(Box::new(SilentState))
    }
}

// Helpers ///////////////////////////
//////////////////////////////////////

fn call(name: &str, arguments: &str) -> ToolCall {
    ToolCall {
        id: format!("call_{}", name),
        call_type: "function".into(),
        function: FunctionCall {
            name: name.into(),
            arguments: arguments.into(),
        },
    }
}

fn settings(enabled: &[&str], configs: Vec<ToolConfig>) -> ToolSettings {
    ToolSettings {
        enabled: enabled.iter().map(|name| name.to_string()).collect(),
        configs,
    }
}

fn tool_config(tool: &str, settings: serde_json::Value) -> ToolConfig {
    ToolConfig {
        tool: tool.into(),
        settings: settings
            .as_object()
            .expect("an object")
            .clone(),
    }
}

fn dir() -> PathBuf {
    PathBuf::from(".")
}

/// The content of the tool message a dispatch answered with.
fn content(tools: &AgentTools, states: &mut ToolStates, call: &ToolCall) -> String {
    let result = tools.dispatch(states, &dir(), call);
    result
        .message()
        .content()
        .expect("a tool message has content")
        .to_string()
}

fn registry() -> ToolRegistry {
    ToolRegistry::new()
        .register(Arc::new(AlwaysOnTool))
        .register(Arc::new(EchoTool))
        .register(Arc::new(RefusingTool))
}

// Registration //////////////////////
//////////////////////////////////////

#[test]
fn a_tool_is_looked_up_by_the_name_it_registers_under() {
    let registry = registry();

    assert!(registry.tool("echo").is_some());
    assert!(registry.tool("nope").is_none());
}

/// The always on tools are never named in `enabled`, so they are not offered
/// as something to enable either.
#[test]
fn only_the_tools_an_agent_can_ask_for_are_offered_as_names() {
    assert_eq!(
        registry().enableable_names(),
        vec!["echo".to_string(), "refusing".to_string()]
    );
}

#[test]
fn registering_a_tool_twice_keeps_the_last_one() {
    let registry = registry().register(Arc::new(CountingTool {
        started: Arc::new(AtomicUsize::new(0)),
    }));
    let registry = registry.register(Arc::new(CountingTool {
        started: Arc::new(AtomicUsize::new(0)),
    }));

    let tools = registry
        .resolve(&settings(&["counting"], Vec::new()))
        .expect("resolve");

    assert_eq!(tools.names().iter().filter(|n| *n == "counting").count(), 1);
}

// Resolving /////////////////////////
//////////////////////////////////////

#[test]
fn an_agent_gets_the_tools_its_settings_enable() {
    let tools = registry()
        .resolve(&settings(&["echo"], Vec::new()))
        .expect("resolve");

    assert_eq!(
        tools.names(),
        vec!["always_on".to_string(), "echo".to_string()]
    );
}

#[test]
fn an_agent_gets_the_always_enabled_tools_without_asking() {
    let tools = registry()
        .resolve(&ToolSettings::default())
        .expect("resolve");

    assert_eq!(tools.names(), vec!["always_on".to_string()]);
}

#[test]
fn enabling_an_always_enabled_tool_does_not_offer_it_twice() {
    let tools = registry()
        .resolve(&settings(&["always_on", "echo"], Vec::new()))
        .expect("resolve");

    assert_eq!(
        tools.names(),
        vec!["always_on".to_string(), "echo".to_string()]
    );
}

#[test]
fn enabling_a_tool_that_does_not_exist_is_rejected() {
    let err = registry()
        .resolve(&settings(&["nope"], Vec::new()))
        .expect_err("no such tool");

    assert!(matches!(err, ToolSettingsError::UnknownTool { name, .. } if name == "nope"));
}

#[test]
fn two_configs_for_one_tool_are_rejected() {
    let err = registry()
        .resolve(&settings(
            &["echo"],
            vec![
                tool_config("echo", serde_json::json!({"a": 1})),
                tool_config("echo", serde_json::json!({"a": 2})),
            ],
        ))
        .expect_err("two configs");

    assert!(matches!(err, ToolSettingsError::DuplicateToolConfig(tool) if tool == "echo"));
}

#[test]
fn a_config_a_tool_refuses_is_rejected() {
    let err = registry()
        .register(Arc::new(PickyTool))
        .resolve(&settings(
            &["picky"],
            vec![tool_config("picky", serde_json::json!({"nope": 1}))],
        ))
        .expect_err("bad config");

    assert!(matches!(err, ToolSettingsError::InvalidToolConfig { tool, .. } if tool == "picky"));
}

/// A config for a tool the agent has not switched on is a setting waiting to
/// be used, not a mistake to report.
#[test]
fn a_config_for_a_tool_that_is_not_enabled_is_ignored() {
    let tools = registry()
        .register(Arc::new(PickyTool))
        .resolve(&settings(
            &["echo"],
            vec![tool_config("picky", serde_json::json!({"nope": 1}))],
        ))
        .expect("resolve");

    assert!(!tools.names().contains(&"picky".to_string()));
}

#[test]
fn a_tool_is_given_its_own_config_and_nothing_else() {
    let tools = registry()
        .resolve(&settings(
            &["echo"],
            vec![
                tool_config("echo", serde_json::json!({"mine": true})),
                tool_config("refusing", serde_json::json!({"theirs": true})),
            ],
        ))
        .expect("resolve");
    let mut states = ToolStates::new();

    let content = content(&tools, &mut states, &call("echo", "{}"));

    assert!(content.contains(r#""config":{"mine":true}"#), "{}", content);
    assert!(!content.contains("theirs"), "{}", content);
}

/// A tool name the api would reject costs a whole request to find out about,
/// so the check that already exists is pointed at the registry.
#[test]
fn every_definition_is_accepted_by_the_request_validator() {
    let tools = ToolRegistry::with_default_tools()
        .resolve(&settings(&["bash"], Vec::new()))
        .expect("resolve");

    let request = ChatCompletionRequest::new("openai/gpt-4o", vec![Message::user("hi")])
        .set_tools(tools.definitions());

    assert!(request.validate().is_ok());
}

#[test]
fn the_default_tools_are_end_turn_and_bash() {
    let tools = ToolRegistry::with_default_tools()
        .resolve(&settings(&["bash"], Vec::new()))
        .expect("resolve");

    assert_eq!(
        tools.names(),
        vec!["end_turn".to_string(), "bash".to_string()]
    );
}

// Dispatch //////////////////////////
//////////////////////////////////////

#[test]
fn an_unknown_tool_answers_the_model_instead_of_failing_the_agent() {
    let tools = registry()
        .resolve(&settings(&["echo"], Vec::new()))
        .expect("resolve");
    let mut states = ToolStates::new();

    let content = content(&tools, &mut states, &call("nope", "{}"));

    assert!(content.starts_with("error: "), "{}", content);
    // naming what does exist is what lets the model recover
    assert!(content.contains("echo"), "{}", content);
}

#[test]
fn arguments_that_are_not_json_answer_the_model_with_an_error() {
    let tools = registry()
        .resolve(&settings(&["echo"], Vec::new()))
        .expect("resolve");
    let mut states = ToolStates::new();

    let content = content(&tools, &mut states, &call("echo", "{ not json"));

    assert!(content.contains("not valid json"), "{}", content);
}

#[test]
fn empty_arguments_are_read_as_no_arguments() {
    let tools = registry()
        .resolve(&settings(&["echo"], Vec::new()))
        .expect("resolve");
    let mut states = ToolStates::new();

    let content = content(&tools, &mut states, &call("echo", ""));

    assert!(content.contains(r#""arguments":{}"#), "{}", content);
}

#[test]
fn null_arguments_are_read_as_no_arguments() {
    let tools = registry()
        .resolve(&settings(&["echo"], Vec::new()))
        .expect("resolve");
    let mut states = ToolStates::new();

    let content = content(&tools, &mut states, &call("echo", "null"));

    assert!(content.contains(r#""arguments":{}"#), "{}", content);
}

#[test]
fn arguments_that_are_not_an_object_answer_the_model_with_an_error() {
    let tools = registry()
        .resolve(&settings(&["echo"], Vec::new()))
        .expect("resolve");
    let mut states = ToolStates::new();

    let content = content(&tools, &mut states, &call("echo", "[1, 2]"));

    assert!(content.contains("must be a json object"), "{}", content);
}

#[test]
fn a_tool_that_refuses_the_call_answers_the_model_with_an_error() {
    let tools = registry()
        .resolve(&settings(&["refusing"], Vec::new()))
        .expect("resolve");
    let mut states = ToolStates::new();

    let content = content(&tools, &mut states, &call("refusing", "{}"));

    assert!(content.starts_with("error: "), "{}", content);
}

#[test]
fn a_result_carries_back_the_tool_call_id_it_answers() {
    let tools = registry()
        .resolve(&settings(&["echo"], Vec::new()))
        .expect("resolve");
    let mut states = ToolStates::new();

    let result = tools.dispatch(&mut states, &dir(), &call("echo", "{}"));

    match result.message() {
        Message::Tool { tool_call_id, .. } => assert_eq!(tool_call_id, "call_echo"),
        other => panic!("expected a tool message, got {:?}", other),
    }
}

/// Some providers reject a tool message with nothing in it, so a tool that
/// said nothing has to say so.
#[test]
fn a_tool_that_says_nothing_still_answers_the_call() {
    let tools = registry()
        .resolve(&ToolSettings::default())
        .expect("resolve");
    let mut states = ToolStates::new();

    let content = content(&tools, &mut states, &call("always_on", "{}"));

    assert_eq!(content, "(no output)");
}

#[test]
fn ending_the_turn_is_carried_back_on_the_result() {
    let tools = ToolRegistry::with_default_tools()
        .resolve(&ToolSettings::default())
        .expect("resolve");
    let mut states = ToolStates::new();

    let ended = tools.dispatch(&mut states, &dir(), &call("end_turn", "{}"));
    let carried_on = tools.dispatch(&mut states, &dir(), &call("nope", "{}"));

    assert!(ended.ends_turn());
    assert!(!carried_on.ends_turn());
}

// Tool state ////////////////////////
//////////////////////////////////////

#[test]
fn a_tools_state_is_started_once_and_kept() {
    let started = Arc::new(AtomicUsize::new(0));
    let tools = registry()
        .register(Arc::new(CountingTool {
            started: Arc::clone(&started),
        }))
        .resolve(&settings(&["counting"], Vec::new()))
        .expect("resolve");
    let mut states = ToolStates::new();

    let counts: Vec<String> = (0..3)
        .map(|_| content(&tools, &mut states, &call("counting", "{}")))
        .collect();

    assert_eq!(counts, vec!["1", "2", "3"]);
    assert_eq!(started.load(Ordering::SeqCst), 1);
}

/// A tool that opens a connection must not open one for an agent that never
/// gets round to using it.
#[test]
fn a_tool_is_not_started_until_the_agent_calls_it() {
    let started = Arc::new(AtomicUsize::new(0));
    let tools = registry()
        .register(Arc::new(CountingTool {
            started: Arc::clone(&started),
        }))
        .resolve(&settings(&["counting", "echo"], Vec::new()))
        .expect("resolve");
    let mut states = ToolStates::new();

    assert!(states.started().is_empty());
    assert_eq!(started.load(Ordering::SeqCst), 0);

    content(&tools, &mut states, &call("echo", "{}"));

    assert_eq!(states.started(), vec!["echo".to_string()]);
    assert_eq!(started.load(Ordering::SeqCst), 0);
}

#[test]
fn each_agent_gets_its_own_state() {
    let tools = registry()
        .register(Arc::new(CountingTool {
            started: Arc::new(AtomicUsize::new(0)),
        }))
        .resolve(&settings(&["counting"], Vec::new()))
        .expect("resolve");

    let mut one = ToolStates::new();
    let mut other = ToolStates::new();

    content(&tools, &mut one, &call("counting", "{}"));
    content(&tools, &mut one, &call("counting", "{}"));
    let theirs = content(&tools, &mut other, &call("counting", "{}"));

    assert_eq!(theirs, "1");
}

/// A connection refused once must not leave the tool broken for the rest of
/// the agent's life.
#[test]
fn a_state_that_fails_to_start_is_tried_again_on_the_next_call() {
    let tools = registry()
        .register(Arc::new(FlakyTool {
            attempts: Arc::new(AtomicUsize::new(0)),
        }))
        .resolve(&settings(&["flaky"], Vec::new()))
        .expect("resolve");
    let mut states = ToolStates::new();

    let first = content(&tools, &mut states, &call("flaky", "{}"));
    let second = content(&tools, &mut states, &call("flaky", "{}"));

    assert!(first.contains("the connection was refused"), "{}", first);
    assert_eq!(second, "(no output)");
}

/// Whatever a tool opened is closed by its `Drop`, which is what happens when
/// the agent's thread ends.
#[test]
fn a_state_is_closed_when_it_goes_out_of_scope() {
    let closed = Arc::new(AtomicBool::new(false));
    let tools = registry()
        .register(Arc::new(ClosingTool {
            closed: Arc::clone(&closed),
        }))
        .resolve(&settings(&["closing"], Vec::new()))
        .expect("resolve");

    {
        let mut states = ToolStates::new();
        content(&tools, &mut states, &call("closing", "{}"));
        assert!(!closed.load(Ordering::SeqCst));
    }

    assert!(closed.load(Ordering::SeqCst));
}

/// The tools are handed a directory rather than finding one, so an agent can
/// only ever work in its own.
#[test]
fn a_tool_is_run_in_the_directory_it_is_given() {
    let tools = ToolRegistry::with_default_tools()
        .resolve(&settings(&["bash"], Vec::new()))
        .expect("resolve");
    let mut states = ToolStates::new();

    let result = tools.dispatch(
        &mut states,
        Path::new("/"),
        &call("bash", r#"{"command": "ls -d /tmp"}"#),
    );
    let content = result
        .message()
        .content()
        .expect("a tool message has content")
        .to_string();

    assert!(content.contains("exit code: 0"), "{}", content);
}
