use std::{fs, path::Path, time::Duration};

use crate::core::config::config::Config;
use crate::core::openrouter::types::Message;
use crate::core::runtime::agent::AgentState;
use crate::core::runtime::agent_config::{
    ConfigFilesError, AGENTS_FILE_NAME, AGENT_CONFIG_FILE_NAME, SYSTEM_FILE_NAME,
};
use crate::core::test_support::{
    agent_dir, project_dir, project_with_delayed_server, project_with_script, project_with_server,
    wait_until, write_agent_config, write_agent_config_with_tools, write_directive, StubReply,
    TEST_MODEL,
};

use super::runtime::{Runtime, RuntimeConfig, RuntimeError};

fn runtime(dir: &Path) -> Runtime {
    let config = Config::load(dir).expect("load config");
    Runtime::new(RuntimeConfig::new(dir.to_path_buf(), config)).expect("build runtime")
}

/// Runs the agent and waits for the reply its directive asked for.
fn start_and_wait(rt: &Runtime, id: &str) {
    rt.start_agent(id).expect("start the agent");
    wait_until("the agent's reply", || {
        rt.agent(id).expect("agent exists").state() == AgentState::Idle
    });
}

#[test]
fn agents_are_loaded_from_the_project_directories() {
    let dir = project_dir("load");
    agent_dir(&dir, "builder", true);
    agent_dir(&dir, "reviewer", true);

    let rt = runtime(&dir);
    let agents = rt.list_agents();

    assert_eq!(agents.len(), 2);
    // sorted by name, so the list doesn't jump around between reloads
    assert_eq!(agents[0].config().name(), "builder");
    assert_eq!(agents[1].config().name(), "reviewer");
    assert_eq!(agents[0].config().dir(), dir.join("builder"));
    assert_eq!(agents[0].config().model().full_slug(), "openai/gpt-4o");
}

#[test]
fn a_loaded_agent_starts_out_configuring_with_its_files_read() {
    let dir = project_dir("configuring");
    agent_dir(&dir, "builder", true);

    let rt = runtime(&dir);
    let agent = rt.agent("builder").expect("agent exists");

    assert_eq!(agent.state(), AgentState::Configuring);
    let config_files = agent.config_files().expect("config files were read");
    assert!(config_files.complete());
    assert_eq!(
        config_files.system_file().path(),
        dir.join(SYSTEM_FILE_NAME)
    );
}

#[test]
fn a_directory_without_an_agents_file_is_still_loaded_but_incomplete() {
    let dir = project_dir("incomplete");
    agent_dir(&dir, "builder", false);

    let rt = runtime(&dir);
    let agent = rt.agent("builder").expect("agent exists");

    assert!(!agent
        .config_files()
        .expect("config files were read")
        .complete());
}

#[test]
fn hidden_directories_and_target_are_not_agents() {
    let dir = project_dir("skipped");
    agent_dir(&dir, "builder", true);
    agent_dir(&dir, ".git", true);
    agent_dir(&dir, "target", true);
    fs::write(dir.join("notes.md"), "").expect("write file");

    let rt = runtime(&dir);
    let names: Vec<String> = rt
        .list_agents()
        .iter()
        .map(|agent| agent.config().name())
        .collect();

    assert_eq!(names, vec!["builder".to_string()]);
}

#[test]
fn an_agent_cannot_run_until_its_config_files_are_complete() {
    let dir = project_dir("cannot-run");
    agent_dir(&dir, "builder", false);

    let rt = runtime(&dir);
    let err = rt
        .start_agent("builder")
        .expect_err("the agents file is missing");

    assert!(matches!(err, RuntimeError::ConfigFileMissing(name) if name == AGENTS_FILE_NAME));
}

#[test]
fn reloading_picks_up_a_directory_added_after_startup() {
    let dir = project_dir("added");
    agent_dir(&dir, "builder", true);

    let rt = runtime(&dir);
    assert_eq!(rt.list_agents().len(), 1);

    agent_dir(&dir, "reviewer", true);
    rt.load_agents();

    assert_eq!(rt.list_agents().len(), 2);
    assert!(rt.agent("reviewer").is_some());
}

#[test]
fn reloading_drops_an_agent_whose_directory_is_gone() {
    let dir = project_dir("removed");
    agent_dir(&dir, "builder", true);

    let rt = runtime(&dir);
    fs::remove_dir_all(dir.join("builder")).expect("remove agent dir");
    rt.load_agents();

    assert!(rt.list_agents().is_empty());
}

#[test]
fn reloading_keeps_a_running_agent_whose_directory_is_gone() {
    let (dir, _server) = project_with_server("running", "on it");
    let builder = agent_dir(&dir, "builder", true);
    write_directive(&builder, "Start work.\n");

    let rt = runtime(&dir);
    start_and_wait(&rt, "builder");
    fs::remove_dir_all(dir.join("builder")).expect("remove agent dir");
    rt.load_agents();

    // its session is still worth something even though the directory is gone
    let agent = rt.agent("builder").expect("agent exists");
    assert_eq!(agent.state(), AgentState::Idle);
    assert_eq!(agent.messages().len(), 2);
}

#[test]
fn reloading_picks_up_a_config_file_added_after_startup() {
    let dir = project_dir("reload-files");
    let builder = agent_dir(&dir, "builder", false);

    let rt = runtime(&dir);
    assert!(!rt
        .agent("builder")
        .expect("agent exists")
        .config_files()
        .expect("config files were read")
        .complete());

    fs::write(builder.join(AGENTS_FILE_NAME), "purpose").expect("write agents file");

    assert!(rt
        .reload_config_files("builder")
        .expect("reload")
        .complete());
    rt.load_agents();
    assert!(rt
        .agent("builder")
        .expect("agent exists")
        .config_files()
        .expect("config files were read")
        .complete());
}

#[test]
fn an_agent_runs_on_the_model_its_own_config_file_names() {
    let dir = project_dir("agent-model");
    let builder = agent_dir(&dir, "builder", true);
    write_agent_config(&builder, "anthropic/claude-opus-5");
    agent_dir(&dir, "reviewer", true);

    let rt = runtime(&dir);

    assert_eq!(
        rt.agent("builder")
            .expect("agent exists")
            .config()
            .model()
            .full_slug(),
        "anthropic/claude-opus-5"
    );
    // the other agent still gets what its own config.json names
    assert_eq!(
        rt.agent("reviewer")
            .expect("agent exists")
            .config()
            .model()
            .full_slug(),
        "openai/gpt-4o"
    );
}

#[test]
fn an_agent_without_a_usable_model_is_left_unconfigured() {
    let dir = project_dir("unusable-model");
    let builder = agent_dir(&dir, "builder", true);
    // not an "author/slug" id, and there is no project default to fall back on
    write_agent_config(&builder, "gpt-4o");

    let rt = runtime(&dir);
    let agent = rt.agent("builder").expect("agent exists");

    assert!(agent.config_files().is_none());
    assert!(!agent.config().model().valid());
    assert!(matches!(
        agent.config_error().expect("the config error was kept"),
        ConfigFilesError::InvalidModel(model) if model == "gpt-4o"
    ));

    let err = rt
        .start_agent("builder")
        .expect_err("the model is unusable");
    assert!(matches!(
        err,
        RuntimeError::ConfigFileUnreadable { name, .. } if name == AGENT_CONFIG_FILE_NAME
    ));
}

#[test]
fn an_agent_whose_config_file_appears_later_becomes_configured() {
    let dir = project_dir("config-added");
    let builder = agent_dir(&dir, "builder", true);
    fs::remove_file(builder.join(AGENT_CONFIG_FILE_NAME)).expect("remove agent config");

    let rt = runtime(&dir);
    assert!(rt
        .agent("builder")
        .expect("agent exists")
        .config_error()
        .is_some());

    write_agent_config(&builder, "anthropic/claude-opus-5");
    assert!(rt
        .reload_config_files("builder")
        .expect("reload")
        .complete());

    let agent = rt.agent("builder").expect("agent exists");
    assert!(agent.config_error().is_none());
    assert_eq!(
        agent.config().model().full_slug(),
        "anthropic/claude-opus-5"
    );
}

#[test]
fn an_agent_cannot_run_without_its_config_file() {
    let dir = project_dir("no-config");
    let builder = agent_dir(&dir, "builder", true);
    fs::remove_file(builder.join(AGENT_CONFIG_FILE_NAME)).expect("remove agent config");

    let rt = runtime(&dir);
    // it is still listed, so the user can see what it is waiting on
    let agent = rt.agent("builder").expect("agent exists");
    assert!(agent.config_files().is_none());
    assert!(matches!(
        agent.config_error().expect("the config error was kept"),
        ConfigFilesError::Missing { .. }
    ));

    let err = rt
        .start_agent("builder")
        .expect_err("the agent config file is missing");

    assert!(matches!(err, RuntimeError::ConfigFileMissing(name) if name == AGENT_CONFIG_FILE_NAME));
}

#[test]
fn reloading_picks_up_a_model_change() {
    let dir = project_dir("model-change");
    let builder = agent_dir(&dir, "builder", true);

    let rt = runtime(&dir);
    write_agent_config(&builder, "anthropic/claude-opus-5");
    rt.load_agents();

    assert_eq!(
        rt.agent("builder")
            .expect("agent exists")
            .config()
            .model()
            .full_slug(),
        "anthropic/claude-opus-5"
    );
}

#[test]
fn an_agent_cannot_run_with_an_unreadable_config_file() {
    let dir = project_dir("bad-config");
    let builder = agent_dir(&dir, "builder", true);
    fs::write(builder.join(AGENT_CONFIG_FILE_NAME), "{ not json").expect("write agent config");

    let rt = runtime(&dir);
    let err = rt
        .start_agent("builder")
        .expect_err("the agent config file cannot be parsed");

    assert!(matches!(
        err,
        RuntimeError::ConfigFileUnreadable { name, .. } if name == AGENT_CONFIG_FILE_NAME
    ));
}

#[test]
fn a_directive_opens_the_conversation_as_a_user_turn() {
    let (dir, server) = project_with_server("directive", "Reading it now.");
    let builder = agent_dir(&dir, "builder", true);
    fs::write(builder.join(AGENTS_FILE_NAME), "You review code.").expect("write agents file");
    write_directive(&builder, "Review the parser.\n");

    let rt = runtime(&dir);
    start_and_wait(&rt, "builder");

    // what went out: a system prompt and the directive as the opening turn
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    let sent: serde_json::Value = serde_json::from_str(&requests[0]).expect("a json request");
    let messages = sent["messages"].as_array().expect("messages were sent");
    assert_eq!(messages.len(), 2);

    // SYSTEM.md and AGENTS.md are both the system prompt
    assert_eq!(messages[0]["role"], "system");
    let system = messages[0]["content"].as_str().expect("content");
    assert!(system.contains("guidelines"));
    assert!(system.contains("You review code."));

    // the directive is the opening user turn, sent as written
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["content"], "Review the parser.");

    // and it is the agent's own first turn, not something the user typed
    let agent = rt.agent("builder").expect("agent exists");
    assert!(agent.opened_with_directive());
    assert!(matches!(agent.messages()[0], Message::User { .. }));
    assert!(matches!(agent.messages()[1], Message::Assistant { .. }));
}

#[test]
fn without_a_directive_the_agent_waits_for_the_user() {
    let (dir, server) = project_with_server("no-directive", "unused");
    agent_dir(&dir, "builder", true);

    let rt = runtime(&dir);
    rt.start_agent("builder").expect("ready the agent");

    // nothing to send, so nothing is sent
    assert!(server.requests().is_empty());

    let agent = rt.agent("builder").expect("agent exists");
    assert_eq!(agent.state(), AgentState::Idle);
    assert!(agent.messages().is_empty());
    // it is still ready to talk: the system prompt is built
    assert!(agent.system_prompt().contains("guidelines"));
}

#[test]
fn a_missing_directive_never_stops_an_agent_running() {
    let dir = project_dir("directive-optional");
    agent_dir(&dir, "builder", true);

    let rt = runtime(&dir);
    let config_files = rt
        .agent("builder")
        .expect("agent exists")
        .config_files()
        .expect("config files were read");

    assert!(!config_files.directive_file().present());
    assert!(!config_files.directive_file().required());
    assert!(config_files.complete());
}

/// The messages of a request body the stub server was sent.
fn sent_messages(request: &str) -> Vec<serde_json::Value> {
    let sent: serde_json::Value = serde_json::from_str(request).expect("a json request");
    sent["messages"]
        .as_array()
        .expect("messages were sent")
        .clone()
}

#[test]
fn a_message_sent_to_an_idle_agent_opens_the_conversation() {
    let (dir, server) = project_with_server("first-message", "on it");
    agent_dir(&dir, "builder", true);

    let rt = runtime(&dir);
    rt.start_agent("builder").expect("ready the agent");
    rt.send_agent_message("builder", "Review the parser.".into())
        .expect("send the message");
    wait_until("the agent's reply", || {
        rt.agent("builder").expect("agent exists").state() == AgentState::Idle
    });

    let messages = sent_messages(&server.requests()[0]);
    assert_eq!(
        messages.last().expect("a user turn")["content"],
        "Review the parser."
    );

    let agent = rt.agent("builder").expect("agent exists");
    assert!(!agent.opened_with_directive());
    assert_eq!(agent.messages().len(), 2);
}

/// The agent answers on its own thread, so a message sent mid turn cannot go
/// out with the request already in flight. It waits instead of being dropped.
#[test]
fn a_message_sent_while_the_agent_is_working_is_queued_for_the_next_turn() {
    let (dir, server) =
        project_with_delayed_server("queued-message", "on it", Duration::from_millis(200));
    let builder = agent_dir(&dir, "builder", true);
    write_directive(&builder, "Review the parser.\n");

    let rt = runtime(&dir);
    rt.start_agent("builder").expect("start the agent");
    wait_until("the directive to go out", || server.requests().len() == 1);

    rt.send_agent_message("builder", "Check the lexer too.".into())
        .expect("send the message");

    // the turn in flight is left alone and the message waits its turn
    let agent = rt.agent("builder").expect("agent exists");
    assert_eq!(agent.state(), AgentState::Working);
    assert_eq!(agent.queued_messages().len(), 1);

    wait_until("the agent's replies", || {
        rt.agent("builder").expect("agent exists").state() == AgentState::Idle
    });

    // the directive and its reply, then the queued message and its reply
    let agent = rt.agent("builder").expect("agent exists");
    assert!(agent.queued_messages().is_empty());
    assert_eq!(agent.messages().len(), 4);
    assert!(matches!(agent.messages()[2], Message::User { .. }));

    // and it went out as a turn of its own, on top of the conversation so far
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let messages = sent_messages(&requests[1]);
    assert_eq!(messages.len(), 4);
    assert_eq!(
        messages.last().expect("a user turn")["content"],
        "Check the lexer too."
    );
}

/// The thread belongs to the agent rather than to a single request, so the
/// conversation carries on across turns.
#[test]
fn an_agent_keeps_answering_across_turns() {
    let (dir, _server) = project_with_server("many-turns", "on it");
    agent_dir(&dir, "builder", true);

    let rt = runtime(&dir);
    rt.start_agent("builder").expect("ready the agent");

    for turn in 0..3 {
        rt.send_agent_message("builder", format!("message {}", turn))
            .expect("send the message");
        wait_until("the agent's reply", || {
            rt.agent("builder").expect("agent exists").state() == AgentState::Idle
        });
    }

    assert_eq!(
        rt.agent("builder").expect("agent exists").messages().len(),
        6
    );
}

// Tools /////////////////////////////
//////////////////////////////////////

/// A project whose one agent may run commands, answering from `replies`.
fn project_with_bash(
    name: &str,
    max_iterations: u32,
    replies: Vec<StubReply>,
) -> (std::path::PathBuf, crate::core::test_support::StubOpenRouter) {
    let (dir, server) = project_with_script(name, replies);
    let builder = agent_dir(&dir, "builder", true);
    write_agent_config_with_tools(
        &builder,
        TEST_MODEL,
        max_iterations,
        serde_json::json!({"enabled": ["bash"]}),
    );
    (dir, server)
}

/// The tools of a request body the stub server was sent.
fn sent_tool_names(request: &str) -> Vec<String> {
    let sent: serde_json::Value = serde_json::from_str(request).expect("a json request");
    sent["tools"]
        .as_array()
        .expect("tools were sent")
        .iter()
        .map(|tool| tool["function"]["name"].as_str().expect("a name").to_string())
        .collect()
}

/// Waits for the agent to stop working, whether it went idle or failed.
fn wait_until_settled(rt: &Runtime, id: &str) {
    wait_until("the agent to settle", || {
        rt.agent(id).expect("agent exists").state() != AgentState::Working
    });
}

#[test]
fn the_tools_an_agent_may_call_go_out_with_every_request() {
    let (dir, server) = project_with_bash(
        "tools-sent",
        5,
        vec![
            StubReply::tool_call("call_1", "bash", serde_json::json!({"command": "true"})),
            StubReply::text("done"),
        ],
    );
    let rt = runtime(&dir);

    rt.start_agent("builder").expect("start the agent");
    rt.send_agent_message("builder", "hello".into())
        .expect("send a message");
    wait_until_settled(&rt, "builder");

    let requests = server.requests();
    assert!(requests.len() >= 2, "a tool loop ran more than one request");
    for request in &requests {
        assert_eq!(
            sent_tool_names(request),
            vec!["end_turn".to_string(), "bash".to_string()],
            "an iteration went out without its tools"
        );
        let sent: serde_json::Value = serde_json::from_str(request).expect("a json request");
        assert_eq!(sent["tool_choice"], serde_json::json!("auto"));
    }
}

/// `end_turn` is how a turn ends rather than something the user grants, so an
/// agent that enabled nothing still gets it.
#[test]
fn an_agent_without_a_tools_block_is_only_offered_end_turn() {
    let (dir, server) = project_with_script("no-tools-block", vec![StubReply::text("done")]);
    agent_dir(&dir, "builder", true);
    let rt = runtime(&dir);

    rt.start_agent("builder").expect("start the agent");
    rt.send_agent_message("builder", "hello".into())
        .expect("send a message");
    wait_until_settled(&rt, "builder");

    assert_eq!(
        sent_tool_names(&server.requests()[0]),
        vec!["end_turn".to_string()]
    );
}

#[test]
fn an_agent_runs_the_tool_the_model_asks_for_and_answers_with_the_result() {
    let (dir, server) = project_with_bash(
        "runs-tools",
        5,
        vec![
            StubReply::tool_call("call_1", "bash", serde_json::json!({"command": "echo hi"})),
            StubReply::text("it said hi"),
        ],
    );
    let rt = runtime(&dir);

    rt.start_agent("builder").expect("start the agent");
    rt.send_agent_message("builder", "what does it say?".into())
        .expect("send a message");
    wait_until_settled(&rt, "builder");

    let definition = rt.agent("builder").expect("agent exists");
    assert_eq!(definition.state(), AgentState::Idle);
    // the user's message, the tool call, its result, and the answer
    assert_eq!(definition.messages().len(), 4);

    // the result of the command went back to the model on the next request
    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    let messages = sent_messages(&requests[1]);
    // the assistant's tool calls go back out with the conversation: a provider
    // rejects a tool result that answers a call it cannot see
    let assistant = messages
        .iter()
        .find(|message| message["role"] == "assistant")
        .expect("the assistant message was sent");
    assert_eq!(assistant["tool_calls"][0]["id"], "call_1");
    assert_eq!(assistant["tool_calls"][0]["function"]["name"], "bash");

    let tool_message = messages
        .iter()
        .find(|message| message["role"] == "tool")
        .expect("a tool message was sent");
    assert_eq!(tool_message["tool_call_id"], "call_1");
    let content = tool_message["content"].as_str().expect("content");
    assert!(content.contains("hi"), "{}", content);
    assert!(content.contains("exit code: 0"), "{}", content);
}

#[test]
fn calling_end_turn_finishes_the_turn() {
    let (dir, server) = project_with_bash(
        "end-turn",
        5,
        vec![StubReply::tool_call(
            "call_1",
            "end_turn",
            serde_json::json!({"summary": "nothing to do"}),
        )],
    );
    let rt = runtime(&dir);

    rt.start_agent("builder").expect("start the agent");
    rt.send_agent_message("builder", "anything to do?".into())
        .expect("send a message");
    wait_until_settled(&rt, "builder");

    let definition = rt.agent("builder").expect("agent exists");
    assert_eq!(definition.state(), AgentState::Idle);
    // the turn stopped there rather than asking the model again
    assert_eq!(server.requests().len(), 1);
    assert!(matches!(
        definition.messages().last(),
        Some(Message::Tool { .. })
    ));
}

/// The behaviour that was there before tools, pinned so it cannot regress.
#[test]
fn a_plain_reply_finishes_the_turn() {
    let (dir, server) = project_with_bash("plain-reply", 5, vec![StubReply::text("all done")]);
    let rt = runtime(&dir);

    rt.start_agent("builder").expect("start the agent");
    rt.send_agent_message("builder", "hello".into())
        .expect("send a message");
    wait_until_settled(&rt, "builder");

    assert_eq!(
        rt.agent("builder").expect("agent exists").state(),
        AgentState::Idle
    );
    assert_eq!(server.requests().len(), 1);
}

/// A name the model made up is something it can read and correct, so it costs
/// an iteration rather than the agent.
#[test]
fn an_unknown_tool_is_answered_rather_than_failing_the_agent() {
    let (dir, server) = project_with_bash(
        "unknown-tool",
        5,
        vec![
            StubReply::tool_call("call_1", "teleport", serde_json::json!({})),
            StubReply::text("sorry about that"),
        ],
    );
    let rt = runtime(&dir);

    rt.start_agent("builder").expect("start the agent");
    rt.send_agent_message("builder", "go".into())
        .expect("send a message");
    wait_until_settled(&rt, "builder");

    let definition = rt.agent("builder").expect("agent exists");
    assert_eq!(definition.state(), AgentState::Idle);

    let messages = sent_messages(&server.requests()[1]);
    let tool_message = messages
        .iter()
        .find(|message| message["role"] == "tool")
        .expect("a tool message was sent");
    let content = tool_message["content"].as_str().expect("content");
    assert!(content.starts_with("error: "), "{}", content);
    assert!(content.contains("bash"), "{}", content);
}

#[test]
fn an_agent_that_never_finishes_gives_up_after_its_maximum_iterations() {
    let (dir, server) = project_with_bash(
        "runaway",
        3,
        // the last reply repeats, so this agent asks for a command forever
        vec![StubReply::tool_call(
            "call_1",
            "bash",
            serde_json::json!({"command": "true"}),
        )],
    );
    let rt = runtime(&dir);

    rt.start_agent("builder").expect("start the agent");
    rt.send_agent_message("builder", "go".into())
        .expect("send a message");
    wait_until_settled(&rt, "builder");

    match rt.agent("builder").expect("agent exists").state() {
        AgentState::Failed(err) => assert!(err.contains("3 iterations"), "{}", err),
        other => panic!("expected Failed, got {:?}", other.label()),
    }
    assert_eq!(server.requests().len(), 3);
}

/// A tool name the user made up is reported the way a bad model id is: on the
/// configuration screen, before the agent runs.
#[test]
fn an_agent_that_enables_a_tool_that_does_not_exist_is_left_unconfigured() {
    let dir = project_dir("unknown-tool-enabled");
    let builder = agent_dir(&dir, "builder", true);
    write_agent_config_with_tools(
        &builder,
        TEST_MODEL,
        5,
        serde_json::json!({"enabled": ["teleport"]}),
    );

    let rt = runtime(&dir);
    let definition = rt.agent("builder").expect("agent exists");

    assert!(definition.config_files().is_none());
    assert!(matches!(
        definition.config_error(),
        Some(ConfigFilesError::InvalidTools(err)) if err.contains("teleport")
    ));

    let err = rt.start_agent("builder").expect_err("cannot start");
    assert!(matches!(
        err,
        RuntimeError::ConfigFileUnreadable { name, .. } if name == AGENT_CONFIG_FILE_NAME
    ));
}

/// A message the user sends mid turn is sent with the next request, and never
/// between an assistant's tool calls and their answers — providers reject that
/// ordering outright.
#[test]
fn a_message_queued_during_a_tool_loop_never_splits_a_tool_call() {
    let (dir, server) = project_with_bash(
        "queued-mid-tool",
        5,
        vec![
            StubReply::tool_call("call_1", "bash", serde_json::json!({"command": "sleep 0.2"})),
            StubReply::text("done"),
        ],
    );
    let rt = runtime(&dir);

    rt.start_agent("builder").expect("start the agent");
    rt.send_agent_message("builder", "go".into())
        .expect("send a message");
    wait_until("the first request", || server.requests().len() == 1);
    rt.send_agent_message("builder", "and this too".into())
        .expect("send a message");
    wait_until_settled(&rt, "builder");

    let requests = server.requests();
    let messages = sent_messages(&requests[requests.len() - 1]);
    for pair in messages.windows(2) {
        let tool_calls = pair[0]["tool_calls"].is_array();
        assert!(
            !(tool_calls && pair[1]["role"] == "user"),
            "a user message split a tool call: {:?}",
            pair
        );
    }
    // the queued message did reach the model rather than being dropped
    assert!(messages
        .iter()
        .any(|message| message["content"] == "and this too"));
}

/// A tool's state belongs to the agent, not to a turn, so a second turn talks
/// to the same one. Proved here by a tool that counts its calls.
#[test]
fn a_tools_state_survives_across_turns() {
    use std::sync::Arc;

    use crate::core::runtime::tool_registry::ToolRegistry;
    use crate::core::tools::end_turn::EndTurnTool;
    use crate::core::tools::tool::{Tool, ToolContext, ToolError, ToolOutput, ToolState};

    struct CountingTool;

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
            Ok(ToolOutput::new(format!("call {}", self.calls)))
        }
    }

    // a tool call and an answer, twice: one turn each, so the second turn has
    // to reach the tool again for the count to say anything
    let (dir, server) = project_with_script(
        "state-across-turns",
        vec![
            StubReply::tool_call("call_1", "counting", serde_json::json!({})),
            StubReply::text("done"),
            StubReply::tool_call("call_2", "counting", serde_json::json!({})),
            StubReply::text("done again"),
        ],
    );
    let builder = agent_dir(&dir, "builder", true);
    write_agent_config_with_tools(
        &builder,
        TEST_MODEL,
        5,
        serde_json::json!({"enabled": ["counting"]}),
    );

    let config = Config::load(&dir).expect("load config");
    let registry = ToolRegistry::new()
        .register(Arc::new(EndTurnTool::new()))
        .register(Arc::new(CountingTool));
    let rt = Runtime::new_with_tools(RuntimeConfig::new(dir.clone(), config), registry)
        .expect("build runtime");

    rt.start_agent("builder").expect("start the agent");
    rt.send_agent_message("builder", "first".into())
        .expect("send a message");
    wait_until_settled(&rt, "builder");

    assert_eq!(server.requests().len(), 2);

    rt.send_agent_message("builder", "second".into())
        .expect("send a message");
    wait_until("the second turn", || server.requests().len() == 4);
    wait_until_settled(&rt, "builder");

    let tool_outputs: Vec<String> = rt
        .agent("builder")
        .expect("agent exists")
        .messages()
        .iter()
        .filter_map(|message| match message {
            Message::Tool { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect();

    // the second turn talked to the state the first one started, rather than
    // to a fresh one that would have counted from 1 again
    assert_eq!(
        tool_outputs,
        vec!["call 1".to_string(), "call 2".to_string()]
    );
}

/// A tool that panics takes the agent's thread with it unless the loop catches
/// it. The agent must come back to the user rather than sitting in `working`
/// forever with a tool call nothing answered.
#[test]
fn a_tool_that_panics_does_not_leave_the_agent_working() {
    use std::sync::Arc;

    use crate::core::runtime::tool_registry::ToolRegistry;
    use crate::core::tools::end_turn::EndTurnTool;
    use crate::core::tools::tool::{Tool, ToolContext, ToolError, ToolOutput, ToolState};

    struct BustedTool;

    impl Tool for BustedTool {
        fn name(&self) -> String {
            "busted".into()
        }
        fn description(&self) -> String {
            "panics".into()
        }
        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({"type": "object", "properties": {}})
        }
        fn new_state(&self, _context: &ToolContext) -> Result<Box<dyn ToolState>, ToolError> {
            Ok(Box::new(BustedState))
        }
    }

    struct BustedState;

    impl ToolState for BustedState {
        fn run(
            &mut self,
            _context: &ToolContext,
            _arguments: &serde_json::Value,
        ) -> Result<ToolOutput, ToolError> {
            panic!("the tool went bang")
        }
    }

    let (dir, server) = project_with_script(
        "panicking-tool",
        vec![
            StubReply::tool_call("call_1", "busted", serde_json::json!({})),
            StubReply::text("that did not work"),
        ],
    );
    let builder = agent_dir(&dir, "builder", true);
    write_agent_config_with_tools(
        &builder,
        TEST_MODEL,
        5,
        serde_json::json!({"enabled": ["busted"]}),
    );

    let config = Config::load(&dir).expect("load config");
    let registry = ToolRegistry::new()
        .register(Arc::new(EndTurnTool::new()))
        .register(Arc::new(BustedTool));
    let rt = Runtime::new_with_tools(RuntimeConfig::new(dir.clone(), config), registry)
        .expect("build runtime");

    rt.start_agent("builder").expect("start the agent");
    rt.send_agent_message("builder", "go".into())
        .expect("send a message");
    wait_until_settled(&rt, "builder");

    let definition = rt.agent("builder").expect("agent exists");
    assert_eq!(definition.state(), AgentState::Idle);

    // the call the model made was answered, so the conversation is one a
    // provider would still accept
    let tool_messages: Vec<String> = definition
        .messages()
        .iter()
        .filter_map(|message| match message {
            Message::Tool { content, .. } => Some(content.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(tool_messages.len(), 1);
    assert!(tool_messages[0].contains("went bang"), "{}", tool_messages[0]);
    assert_eq!(server.requests().len(), 2);
}

/// A message the user sent is either sent to the model or still visibly queued.
/// It must never be moved into the conversation by a turn that then gives up,
/// which would show it as delivered on screen having never been asked about.
#[test]
fn a_message_is_never_shown_as_delivered_without_reaching_the_model() {
    let (dir, server) = project_with_bash(
        "queued-at-the-limit",
        1,
        vec![StubReply::tool_call(
            "call_1",
            "bash",
            serde_json::json!({"command": "sleep 0.3"}),
        )],
    );
    let rt = runtime(&dir);

    rt.start_agent("builder").expect("start the agent");
    rt.send_agent_message("builder", "go".into())
        .expect("send a message");
    wait_until("the first request", || server.requests().len() == 1);
    rt.send_agent_message("builder", "also do this".into())
        .expect("send a message");
    wait_until_settled(&rt, "builder");

    // the bound was one iteration, so the turn gave up after the first request
    match rt.agent("builder").expect("agent exists").state() {
        AgentState::Failed(err) => assert!(err.contains("1 iterations"), "{}", err),
        other => panic!("expected Failed, got {:?}", other.label()),
    }

    let definition = rt.agent("builder").expect("agent exists");
    let sent = server.requests().join("\n");
    for message in definition.messages() {
        if let Message::User { content } = message {
            assert!(
                sent.contains(content.as_str()),
                "`{}` is in the conversation but never reached the model",
                content
            );
        }
    }
}

/// An agent's settings are read once. Rereading them part way through a session
/// would leave the model, the tools and the settings its tools already parsed
/// describing different generations of the same file.
#[test]
fn reloading_the_config_of_a_started_agent_is_refused() {
    let (dir, _server) = project_with_bash("reload-started", 5, vec![StubReply::text("done")]);
    let rt = runtime(&dir);

    // unstarted, the reload is the ordinary one
    rt.reload_config_files("builder")
        .expect("reload before starting");

    rt.start_agent("builder").expect("start the agent");

    let err = rt
        .reload_config_files("builder")
        .expect_err("cannot reload a started agent");
    assert!(matches!(
        err,
        RuntimeError::AgentAlreadyStarted(id) if id == "builder"
    ));
}

/// A started agent keeps the configuration it started on, so reloading the
/// project does not even read its files. Observed through the tool settings
/// check, which only runs when an agent's `tools` block is worked out: the
/// outcome alone would look the same either way, since the agent is skipped when
/// the results are applied.
#[test]
fn reloading_does_not_read_the_files_of_a_started_agent() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    use crate::core::runtime::tool_registry::ToolRegistry;
    use crate::core::tools::tool::{Tool, ToolContext, ToolError, ToolOutput, ToolState};

    /// Counts how many times its settings have been checked, which is once per
    /// time the agent's configuration is worked out.
    struct CountingTool {
        checked: Arc<AtomicUsize>,
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
        fn always_enabled(&self) -> bool {
            true
        }
        fn validate_config(&self, _config: &serde_json::Value) -> Result<(), String> {
            self.checked.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }
        fn new_state(&self, _context: &ToolContext) -> Result<Box<dyn ToolState>, ToolError> {
            Ok(Box::new(CountingState))
        }
    }

    struct CountingState;

    impl ToolState for CountingState {
        fn run(
            &mut self,
            _context: &ToolContext,
            _arguments: &serde_json::Value,
        ) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::new("counted"))
        }
    }

    let (dir, _server) = project_with_script("reload-not-read", vec![StubReply::text("done")]);
    agent_dir(&dir, "builder", true);

    let checked = Arc::new(AtomicUsize::new(0));
    let config = Config::load(&dir).expect("load config");
    let registry = ToolRegistry::new().register(Arc::new(CountingTool {
        checked: Arc::clone(&checked),
    }));
    let rt = Runtime::new_with_tools(RuntimeConfig::new(dir.clone(), config), registry)
        .expect("build runtime");

    // building the runtime loaded the agent once
    assert_eq!(checked.load(Ordering::SeqCst), 1);
    rt.start_agent("builder").expect("start the agent");

    let before = rt.agent("builder").expect("agent exists").tools().names();

    // the file is left perfectly good, so anything that did read it would get
    // as far as checking the settings and be counted
    rt.load_agents();
    assert_eq!(
        checked.load(Ordering::SeqCst),
        1,
        "the files of a started agent were read again"
    );

    // and a file that has since become unusable cannot take the conversation
    // down with it either
    fs::write(
        dir.join("builder").join(AGENT_CONFIG_FILE_NAME),
        "{ not json",
    )
    .expect("write agent config");
    rt.load_agents();

    let definition = rt.agent("builder").expect("agent exists");
    assert_eq!(definition.state(), AgentState::Idle);
    assert!(definition.config_error().is_none());
    assert_eq!(definition.tools().names(), before);
}
