use std::{env, fs, path::PathBuf};

use crate::core::test_support::{
    write_agent_config, write_agent_config_json, write_agent_config_with_tools,
    TEST_MAX_ITERATIONS,
};

use super::agent_config::{
    ConfigFiles, ConfigFilesError, AGENTS_FILE_NAME, AGENT_CONFIG_FILE_NAME, SYSTEM_FILE_NAME,
};

/// Creates a unique temp directory for a single test to work in.
fn temp_dir(name: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!(
        "apila-test-agent-config-{}-{}",
        name,
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn config_files_are_complete_when_both_are_in_the_agent_dir() {
    let project_dir = temp_dir("both-local");
    let agent_dir = project_dir.join("worker");
    fs::create_dir_all(&agent_dir).expect("create agent dir");
    fs::write(agent_dir.join(AGENTS_FILE_NAME), "purpose").expect("write agents file");
    write_agent_config(&agent_dir, "openai/gpt-4o");
    fs::write(agent_dir.join(SYSTEM_FILE_NAME), "guidelines").expect("write system file");

    let config_files = ConfigFiles::load(&agent_dir, &project_dir).expect("load config files");

    assert!(config_files.complete());
    assert_eq!(
        config_files.system_file().path(),
        agent_dir.join(SYSTEM_FILE_NAME)
    );
}

#[test]
fn config_files_fall_back_to_the_project_system_file() {
    let project_dir = temp_dir("system-fallback");
    let agent_dir = project_dir.join("worker");
    fs::create_dir_all(&agent_dir).expect("create agent dir");
    fs::write(agent_dir.join(AGENTS_FILE_NAME), "purpose").expect("write agents file");
    write_agent_config(&agent_dir, "openai/gpt-4o");
    fs::write(project_dir.join(SYSTEM_FILE_NAME), "guidelines").expect("write system file");

    let config_files = ConfigFiles::load(&agent_dir, &project_dir).expect("load config files");

    assert!(config_files.complete());
    assert_eq!(
        config_files.system_file().path(),
        project_dir.join(SYSTEM_FILE_NAME)
    );
    assert_eq!(
        config_files.system_file().contents().as_deref(),
        Some("guidelines")
    );
}

#[test]
fn config_files_are_incomplete_when_the_agents_file_is_missing() {
    let project_dir = temp_dir("missing-agents");
    let agent_dir = project_dir.join("worker");
    fs::create_dir_all(&agent_dir).expect("create agent dir");
    write_agent_config(&agent_dir, "openai/gpt-4o");
    fs::write(project_dir.join(SYSTEM_FILE_NAME), "guidelines").expect("write system file");

    let config_files = ConfigFiles::load(&agent_dir, &project_dir).expect("load config files");

    assert!(!config_files.complete());
    assert!(!config_files.agents_file().present());
    assert!(config_files.system_file().present());
}

#[test]
fn config_files_are_incomplete_when_the_system_file_is_missing_everywhere() {
    let project_dir = temp_dir("missing-system");
    let agent_dir = project_dir.join("worker");
    fs::create_dir_all(&agent_dir).expect("create agent dir");
    fs::write(agent_dir.join(AGENTS_FILE_NAME), "purpose").expect("write agents file");
    write_agent_config(&agent_dir, "openai/gpt-4o");

    let config_files = ConfigFiles::load(&agent_dir, &project_dir).expect("load config files");

    assert!(!config_files.complete());
    assert!(!config_files.system_file().present());
}

#[test]
fn the_settings_are_read_out_of_the_agent_config_file() {
    let project_dir = temp_dir("settings");
    let agent_dir = project_dir.join("worker");
    fs::create_dir_all(&agent_dir).expect("create agent dir");
    fs::write(agent_dir.join(AGENTS_FILE_NAME), "purpose").expect("write agents file");
    fs::write(agent_dir.join(SYSTEM_FILE_NAME), "guidelines").expect("write system file");
    write_agent_config(&agent_dir, "anthropic/claude-opus-5");

    let config_files = ConfigFiles::load(&agent_dir, &project_dir).expect("load config files");

    assert!(config_files.complete());
    assert_eq!(config_files.settings().model, "anthropic/claude-opus-5");
    assert_eq!(config_files.model().full_slug(), "anthropic/claude-opus-5");
}

#[test]
fn nothing_loads_without_the_agent_config_file() {
    let project_dir = temp_dir("missing-config");
    let agent_dir = project_dir.join("worker");
    fs::create_dir_all(&agent_dir).expect("create agent dir");
    fs::write(agent_dir.join(AGENTS_FILE_NAME), "purpose").expect("write agents file");
    fs::write(agent_dir.join(SYSTEM_FILE_NAME), "guidelines").expect("write system file");

    // the other files are all there, and it still fails: config.json is what
    // the rest of the configuration hangs off
    let err = ConfigFiles::load(&agent_dir, &project_dir).expect_err("no agent config file");

    assert!(matches!(
        err,
        ConfigFilesError::Missing { ref path, .. } if *path == agent_dir.join(AGENT_CONFIG_FILE_NAME)
    ));
}

#[test]
fn an_unreadable_agent_config_file_is_reported() {
    let project_dir = temp_dir("bad-config");
    let agent_dir = project_dir.join("worker");
    fs::create_dir_all(&agent_dir).expect("create agent dir");
    fs::write(agent_dir.join(AGENTS_FILE_NAME), "purpose").expect("write agents file");
    fs::write(agent_dir.join(SYSTEM_FILE_NAME), "guidelines").expect("write system file");
    fs::write(agent_dir.join(AGENT_CONFIG_FILE_NAME), "{ not json").expect("write agent config");

    let err = ConfigFiles::load(&agent_dir, &project_dir).expect_err("an unreadable agent config");

    assert!(matches!(err, ConfigFilesError::Unreadable { .. }));
}

#[test]
fn an_agent_config_file_naming_no_usable_model_is_reported() {
    let project_dir = temp_dir("bad-model");
    let agent_dir = project_dir.join("worker");
    fs::create_dir_all(&agent_dir).expect("create agent dir");
    fs::write(agent_dir.join(AGENTS_FILE_NAME), "purpose").expect("write agents file");
    fs::write(agent_dir.join(SYSTEM_FILE_NAME), "guidelines").expect("write system file");
    // not an "author/slug" id, and there is no default to fall back on
    write_agent_config(&agent_dir, "gpt-4o");

    let err = ConfigFiles::load(&agent_dir, &project_dir).expect_err("an unusable model");

    assert!(matches!(err, ConfigFilesError::InvalidModel(model) if model == "gpt-4o"));
}

/// An agent directory with everything but its config.json, which each test
/// below writes itself.
fn agent_dir_without_config(name: &str) -> (PathBuf, PathBuf) {
    let project_dir = temp_dir(name);
    let agent_dir = project_dir.join("worker");
    fs::create_dir_all(&agent_dir).expect("create agent dir");
    fs::write(agent_dir.join(AGENTS_FILE_NAME), "purpose").expect("write agents file");
    fs::write(project_dir.join(SYSTEM_FILE_NAME), "guidelines").expect("write system file");
    (project_dir, agent_dir)
}

#[test]
fn the_maximum_iterations_are_read_out_of_the_agent_config_file() {
    let (project_dir, agent_dir) = agent_dir_without_config("max-iterations");
    write_agent_config_json(
        &agent_dir,
        r#"{"model": "openai/gpt-4o", "agent_max_iterations": 42}"#,
    );

    let config_files = ConfigFiles::load(&agent_dir, &project_dir).expect("load config files");

    assert_eq!(config_files.agent_max_iterations(), 42);
    assert_eq!(config_files.settings().agent_max_iterations, 42);
}

/// The bound has no project wide default to fall back on, so a file that does
/// not name one is a file that cannot be used.
#[test]
fn an_agent_config_file_without_a_maximum_iteration_count_is_unreadable() {
    let (project_dir, agent_dir) = agent_dir_without_config("no-max-iterations");
    write_agent_config_json(&agent_dir, r#"{"model": "openai/gpt-4o"}"#);

    let err = ConfigFiles::load(&agent_dir, &project_dir).expect_err("no bound");

    match err {
        ConfigFilesError::Unreadable { name, error, .. } => {
            assert_eq!(name, AGENT_CONFIG_FILE_NAME);
            assert!(error.contains("agent_max_iterations"), "{}", error);
        }
        other => panic!("expected Unreadable, got {:?}", other),
    }
}

/// A turn allowed no iterations would end before it began.
#[test]
fn a_maximum_iteration_count_of_zero_is_reported() {
    let (project_dir, agent_dir) = agent_dir_without_config("zero-max-iterations");
    write_agent_config_json(
        &agent_dir,
        r#"{"model": "openai/gpt-4o", "agent_max_iterations": 0}"#,
    );

    let err = ConfigFiles::load(&agent_dir, &project_dir).expect_err("zero bound");

    assert!(matches!(err, ConfigFilesError::InvalidMaxIterations));
}

#[test]
fn an_agent_config_file_without_a_tools_block_enables_nothing() {
    let (project_dir, agent_dir) = agent_dir_without_config("no-tools");
    write_agent_config(&agent_dir, "openai/gpt-4o");

    let config_files = ConfigFiles::load(&agent_dir, &project_dir).expect("load config files");

    assert_eq!(config_files.agent_max_iterations(), TEST_MAX_ITERATIONS);
    assert!(config_files.tool_settings().enabled.is_empty());
    assert!(config_files.tool_settings().configs.is_empty());
}

#[test]
fn the_tools_block_is_read_out_of_the_agent_config_file() {
    let (project_dir, agent_dir) = agent_dir_without_config("tools-block");
    write_agent_config_with_tools(
        &agent_dir,
        "openai/gpt-4o",
        5,
        serde_json::json!({
            "enabled": ["bash"],
            "configs": [{"tool": "bash", "bash_timeout": 30}],
        }),
    );

    let config_files = ConfigFiles::load(&agent_dir, &project_dir).expect("load config files");
    let tools = config_files.tool_settings();

    assert_eq!(tools.enabled, vec!["bash".to_string()]);
    assert_eq!(tools.configs.len(), 1);
    assert_eq!(tools.configs[0].tool, "bash");
}

/// Nothing in the config file reads a tool's own settings, which is what lets
/// a tool add one without this file changing.
#[test]
fn a_tool_config_keeps_whatever_settings_it_was_given() {
    let (project_dir, agent_dir) = agent_dir_without_config("tool-settings");
    write_agent_config_with_tools(
        &agent_dir,
        "openai/gpt-4o",
        5,
        serde_json::json!({
            "enabled": ["bash"],
            "configs": [{"tool": "bash", "something_invented_later": {"deep": true}}],
        }),
    );

    let config_files = ConfigFiles::load(&agent_dir, &project_dir).expect("load config files");

    assert_eq!(
        config_files.tool_settings().configs[0].settings_value(),
        serde_json::json!({"something_invented_later": {"deep": true}})
    );
}

#[test]
fn a_misspelled_key_in_the_tools_block_is_reported() {
    let (project_dir, agent_dir) = agent_dir_without_config("tools-typo");
    write_agent_config_json(
        &agent_dir,
        r#"{"model": "openai/gpt-4o", "agent_max_iterations": 5, "tools": {"enable": ["bash"]}}"#,
    );

    let err = ConfigFiles::load(&agent_dir, &project_dir).expect_err("misspelled key");

    assert!(matches!(err, ConfigFilesError::Unreadable { .. }));
}
