use std::{fs, path::Path};

use crate::core::config::config::Config;
use crate::core::openrouter::types::Message;
use crate::core::runtime::agent::AgentState;
use crate::core::runtime::agent_config::{
    ConfigFilesError, AGENTS_FILE_NAME, AGENT_CONFIG_FILE_NAME, SYSTEM_FILE_NAME,
};
use crate::core::test_support::{
    agent_dir, project_dir, project_with_server, wait_until, write_agent_config, write_directive,
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
