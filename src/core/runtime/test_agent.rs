use std::path::PathBuf;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::core::openrouter::client::{OpenRouter, OpenRouterConfig};
use crate::core::openrouter::types::Message;
use crate::core::runtime::agent_config::{AgentConfig, ConfigFiles, ConfigFilesError, Model};
use crate::core::runtime::tool_registry::{AgentTools, ToolRegistry};
use crate::core::test_support::{
    agent_dir, project_dir, wait_until, write_directive, StubOpenRouter,
};

use super::agent::{Agent, AgentDefinition, AgentState};

/// An agent the way the runtime makes one: named after its directory, with
/// nothing read off disk yet. Its thread is spawned along with it and has
/// nothing to do until the agent is started, so nothing here reaches the
/// client it was handed.
fn agent(name: &str) -> Agent {
    let config = AgentConfig::empty()
        .set_name(name.to_string())
        .set_model(Model::empty())
        .set_dir(PathBuf::from(name));
    let openrouter =
        OpenRouter::new(OpenRouterConfig::new("sk-or-test".into())).expect("build client");
    Agent::new(name.to_string(), config, Arc::new(openrouter))
}

/// A `ConfigFilesError` to hand to an agent, standing in for whatever went
/// wrong with its `config.json`.
fn config_error() -> ConfigFilesError {
    ConfigFilesError::InvalidModel("gpt-4o".into())
}

#[test]
fn a_new_agent_starts_out_configuring_with_nothing_read() {
    let agent = agent("builder");
    let definition = agent.agent_definition();

    assert_eq!(agent.id(), "builder");
    assert_eq!(definition.state(), AgentState::Configuring);
    assert!(definition.config_files().is_none());
    assert!(definition.config_error().is_none());
    assert!(definition.messages().is_empty());
}

#[test]
fn config_files_and_a_config_error_replace_one_another() {
    let dir = project_dir("agent-config-files");
    let builder = agent_dir(&dir, "builder", true);
    let config_files = ConfigFiles::load(&builder, &dir).expect("load config files");

    let mut agent = agent("builder");
    let mut definition = agent.definition();

    let tools = ToolRegistry::with_default_tools()
        .resolve(&config_files.tool_settings())
        .expect("resolve tools");

    definition.set_config_files(config_files, tools);
    assert!(definition.config_files().is_some());
    assert!(definition.config_error().is_none());
    // end_turn is on for every agent, so a configured agent always has one
    assert_eq!(definition.tools().names(), vec!["end_turn".to_string()]);

    // the files no longer say what the agent holds, so they go with the error,
    // and so do the tools they were read into
    definition.set_config_error(config_error());
    assert!(definition.config_files().is_none());
    assert!(definition.config_error().is_some());
    assert!(definition.tools().is_empty());
}

#[test]
fn the_system_prompt_opens_the_request_messages() {
    let mut agent = agent("builder");
    let mut definition = agent.definition();
    definition.set_system_prompt("guidelines".into());
    definition.push_message(Message::user("hello"));

    let messages = definition.request_messages();

    assert_eq!(messages.len(), 2);
    assert!(matches!(messages[0], Message::System { .. }));
    assert!(matches!(messages[1], Message::User { .. }));
}

#[test]
fn an_empty_system_prompt_is_left_out_of_the_request_messages() {
    let mut agent = agent("builder");
    let mut definition = agent.definition();
    definition.set_system_prompt("   ".into());
    definition.push_message(Message::user("hello"));

    let messages = definition.request_messages();

    assert_eq!(messages.len(), 1);
    assert!(matches!(messages[0], Message::User { .. }));
}

#[test]
fn a_directive_opens_the_conversation_as_a_user_turn() {
    let mut agent = agent("builder");
    let mut definition = agent.definition();

    definition.push_directive("Review the parser.");

    assert!(definition.opened_with_directive());
    assert_eq!(definition.messages().len(), 1);
    assert!(matches!(definition.messages()[0], Message::User { .. }));
}

#[test]
fn a_conversation_the_user_opened_is_not_a_directive() {
    let mut agent = agent("builder");
    let mut definition = agent.definition();

    definition.push_message(Message::user("hello"));
    definition.push_directive("Review the parser.");

    assert!(!definition.opened_with_directive());
}

#[test]
fn only_a_started_agent_holds_a_conversation() {
    assert!(!AgentState::Configuring.started());
    assert!(AgentState::Working.started());
    assert!(AgentState::Idle.started());
    assert!(AgentState::Failed("boom".into()).started());
}

#[test]
fn a_failed_state_says_what_went_wrong() {
    assert_eq!(AgentState::Configuring.label(), "configuring");
    assert_eq!(AgentState::Failed("boom".into()).label(), "failed: boom");
}

/// The ui reads an agent through this snapshot, so it has to carry the state
/// the definition was in when it was taken.
#[test]
fn the_definition_is_a_snapshot_of_the_agent() {
    let agent = agent("builder");
    agent.definition().set_state(AgentState::Idle);

    let definition: AgentDefinition = agent.agent_definition();
    agent.definition().set_state(AgentState::Working);

    assert_eq!(definition.state(), AgentState::Idle);
    assert_eq!(agent.agent_definition().state(), AgentState::Working);
}

#[test]
fn a_queued_message_joins_the_conversation_when_it_is_taken_up() {
    let agent = agent("builder");
    let mut definition = agent.definition();
    definition.push_message(Message::user("hello"));

    definition.queue_message(Message::user("and one more thing"));
    // it waits its turn rather than joining the conversation on the spot
    assert_eq!(definition.messages().len(), 1);
    assert_eq!(definition.queued_messages().len(), 1);

    definition.take_queued_messages();

    assert_eq!(definition.messages().len(), 2);
    assert!(definition.queued_messages().is_empty());
}

/// The thread belongs to the agent, so it goes when the agent does. What was
/// queued behind the turn in flight is part of a conversation nobody can read
/// any more, and is not worth the request it would take.
#[test]
fn a_dropped_agent_stops_rather_than_working_through_its_queue() {
    let project = project_dir("agent-dropped");
    let dir = agent_dir(&project, "builder", true);
    write_directive(&dir, "Review the parser.\n");
    let server = StubOpenRouter::start_delayed("on it", Duration::from_millis(100));

    let config_files = ConfigFiles::load(&dir, &project).expect("load config files");
    let openrouter =
        OpenRouter::new(OpenRouterConfig::new("sk-or-test".into()).set_base_url(server.base_url()))
            .expect("build client");
    let config = AgentConfig::empty()
        .set_name("builder".into())
        .set_model(config_files.model())
        .set_dir(dir);

    let agent = Agent::new("builder".into(), config, Arc::new(openrouter));
    agent
        .definition()
        .set_config_files(config_files, AgentTools::empty());
    agent.start().expect("start the agent");

    wait_until("the directive to go out", || server.requests().len() == 1);
    agent.send_message("Check the lexer too.".into());
    drop(agent);

    // the turn in flight lands, and nothing follows it
    thread::sleep(Duration::from_millis(300));
    assert_eq!(server.requests().len(), 1);
}
