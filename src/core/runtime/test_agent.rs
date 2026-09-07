use std::{env, fs, path::PathBuf};

use super::agent::{ConfigFiles, AGENTS_FILE_NAME, AGENT_CONFIG_FILE_NAME, SYSTEM_FILE_NAME};

/// Writes the settings an agent directory needs to be complete.
fn write_agent_config(dir: &PathBuf, model: &str) {
    fs::write(
        dir.join(AGENT_CONFIG_FILE_NAME),
        format!(r#"{{"model": "{}"}}"#, model),
    )
    .expect("write agent config");
}

/// Creates a unique temp directory for a single test to work in.
fn temp_dir(name: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("apila-test-agent-{}-{}", name, std::process::id()));
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

    let config_files = ConfigFiles::load(&agent_dir, &project_dir);

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

    let config_files = ConfigFiles::load(&agent_dir, &project_dir);

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
    fs::write(project_dir.join(SYSTEM_FILE_NAME), "guidelines").expect("write system file");

    let config_files = ConfigFiles::load(&agent_dir, &project_dir);

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

    let config_files = ConfigFiles::load(&agent_dir, &project_dir);

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

    let config_files = ConfigFiles::load(&agent_dir, &project_dir);

    assert!(config_files.complete());
    assert_eq!(
        config_files.settings().expect("settings were read").model,
        "anthropic/claude-opus-5"
    );
    assert!(config_files.settings_error().is_none());
}

#[test]
fn config_files_are_incomplete_when_the_agent_config_file_is_missing() {
    let project_dir = temp_dir("missing-config");
    let agent_dir = project_dir.join("worker");
    fs::create_dir_all(&agent_dir).expect("create agent dir");
    fs::write(agent_dir.join(AGENTS_FILE_NAME), "purpose").expect("write agents file");
    fs::write(agent_dir.join(SYSTEM_FILE_NAME), "guidelines").expect("write system file");

    let config_files = ConfigFiles::load(&agent_dir, &project_dir);

    assert!(!config_files.complete());
    assert!(!config_files.config_file().present());
    assert!(config_files.settings().is_none());
    // nothing to report: the file simply isn't there
    assert!(config_files.settings_error().is_none());
}

#[test]
fn an_unreadable_agent_config_file_is_reported() {
    let project_dir = temp_dir("bad-config");
    let agent_dir = project_dir.join("worker");
    fs::create_dir_all(&agent_dir).expect("create agent dir");
    fs::write(agent_dir.join(AGENTS_FILE_NAME), "purpose").expect("write agents file");
    fs::write(agent_dir.join(SYSTEM_FILE_NAME), "guidelines").expect("write system file");
    fs::write(agent_dir.join(AGENT_CONFIG_FILE_NAME), "{ not json").expect("write agent config");

    let config_files = ConfigFiles::load(&agent_dir, &project_dir);

    // the file is there, so it shows as present, but it cannot be used
    assert!(config_files.config_file().present());
    assert!(!config_files.complete());
    assert!(config_files.settings_error().is_some());
}
