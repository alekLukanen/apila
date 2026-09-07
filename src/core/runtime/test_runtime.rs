use std::{env, fs, path::PathBuf};

use crate::core::config::config::Config;
use crate::core::openrouter::types::Message;
use crate::core::runtime::agent::{
    AgentState, AGENTS_FILE_NAME, AGENT_CONFIG_FILE_NAME, DIRECTIVE_FILE_NAME, SYSTEM_FILE_NAME,
};

use super::runtime::{Runtime, RuntimeConfig, RuntimeError};

/// A project directory holding a config.json and a project level SYSTEM.md.
fn project_dir(name: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!(
        "apila-test-runtime-{}-{}",
        name,
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create project dir");
    fs::write(
        dir.join("config.json"),
        r#"{"openrouter_api_key": "sk-or-test", "default_model": "openai/gpt-4o"}"#,
    )
    .expect("write config");
    fs::write(dir.join(SYSTEM_FILE_NAME), "guidelines").expect("write system file");
    dir
}

/// Creates a fully configured agent directory inside `project`, unless
/// `with_agents_file` says to leave out its AGENTS.md.
fn agent_dir(project: &PathBuf, name: &str, with_agents_file: bool) -> PathBuf {
    let dir = project.join(name);
    fs::create_dir_all(&dir).expect("create agent dir");
    if with_agents_file {
        fs::write(dir.join(AGENTS_FILE_NAME), "purpose").expect("write agents file");
    }
    write_agent_config(&dir, "openai/gpt-4o");
    dir
}

/// Writes an agent's config.json, naming the model it runs on.
fn write_agent_config(dir: &PathBuf, model: &str) {
    fs::write(
        dir.join(AGENT_CONFIG_FILE_NAME),
        format!(r#"{{"model": "{}"}}"#, model),
    )
    .expect("write agent config");
}

fn runtime(dir: &PathBuf) -> Runtime {
    let config = Config::load(dir).expect("load config");
    Runtime::new(RuntimeConfig::new(dir.clone(), config)).expect("build runtime")
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
    let dir = project_dir("running");
    agent_dir(&dir, "builder", true);

    let rt = runtime(&dir);
    rt.seed_session_for_test("builder", "start", "on it", false);
    fs::remove_dir_all(dir.join("builder")).expect("remove agent dir");
    rt.load_agents();

    // its session is still worth something even though the directory is gone
    assert_eq!(
        rt.agent("builder").expect("agent exists").state(),
        AgentState::Idle
    );
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
fn an_agent_without_a_usable_model_falls_back_to_the_project_default() {
    let dir = project_dir("fallback-model");
    let builder = agent_dir(&dir, "builder", true);
    // not an "author/slug" id, so it cannot be used
    write_agent_config(&builder, "gpt-4o");

    let rt = runtime(&dir);

    assert_eq!(
        rt.agent("builder")
            .expect("agent exists")
            .config()
            .model()
            .full_slug(),
        "openai/gpt-4o"
    );
}

#[test]
fn an_agent_cannot_run_without_its_config_file() {
    let dir = project_dir("no-config");
    let builder = agent_dir(&dir, "builder", true);
    fs::remove_file(builder.join(AGENT_CONFIG_FILE_NAME)).expect("remove agent config");

    let rt = runtime(&dir);
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
    let dir = project_dir("directive");
    let builder = agent_dir(&dir, "builder", true);
    fs::write(builder.join(AGENTS_FILE_NAME), "You review code.").expect("write agents file");
    fs::write(builder.join(DIRECTIVE_FILE_NAME), "Review the parser.\n")
        .expect("write directive file");

    let rt = runtime(&dir);
    let request = rt
        .start_request_for_test("builder")
        .expect("build the opening request")
        .expect("the directive gives the agent something to send");

    assert!(request.validate().is_ok());
    assert_eq!(request.messages.len(), 2);

    // SYSTEM.md and AGENTS.md are both the system prompt
    let system = request.messages[0].content().expect("content");
    assert!(matches!(request.messages[0], Message::System { .. }));
    assert!(system.contains("guidelines"));
    assert!(system.contains("You review code."));

    // the directive is the opening user turn, sent as written
    assert!(matches!(request.messages[1], Message::User { .. }));
    assert_eq!(
        request.messages[1].content().expect("content"),
        "Review the parser."
    );
}

#[test]
fn without_a_directive_the_agent_waits_for_the_user() {
    let dir = project_dir("no-directive");
    agent_dir(&dir, "builder", true);

    let rt = runtime(&dir);
    let request = rt
        .start_request_for_test("builder")
        .expect("ready the agent");

    // nothing to send, so nothing is sent
    assert!(request.is_none());

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
