use std::{env, fs, path::PathBuf};

use super::config::{Config, ConfigError};

/// Creates a unique temp directory for a single test to work in.
fn temp_dir(name: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("apila-test-config-{}-{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

#[test]
fn load_reads_a_valid_config() {
    let dir = temp_dir("valid");
    fs::write(
        dir.join("config.json"),
        r#"{
            "openrouter_api_key": "sk-or-test",
            "default_model": "openai/gpt-4o",
            "x_openrouter_title": "apila"
        }"#,
    )
    .expect("write config");

    let config = Config::load(&dir).expect("load config");

    assert_eq!(config.openrouter_api_key, "sk-or-test");
    assert_eq!(config.default_model.as_deref(), Some("openai/gpt-4o"));
    assert_eq!(config.x_openrouter_title.as_deref(), Some("apila"));
    assert_eq!(config.openrouter_base_url, None);
    assert_eq!(config.http_referer, None);

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn load_fails_when_the_file_is_missing() {
    let dir = temp_dir("missing");

    match Config::load(&dir) {
        Err(ConfigError::NotFound(path)) => assert_eq!(path, dir.join("config.json")),
        other => panic!("expected NotFound, got {:?}", other.map(|_| ())),
    }

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn load_fails_on_malformed_json() {
    let dir = temp_dir("malformed");
    fs::write(dir.join("config.json"), "{ not json").expect("write config");

    match Config::load(&dir) {
        Err(ConfigError::Parse(_)) => {}
        other => panic!("expected Parse, got {:?}", other.map(|_| ())),
    }

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn load_fails_on_an_empty_api_key() {
    let dir = temp_dir("empty-key");
    fs::write(dir.join("config.json"), r#"{"openrouter_api_key": "  "}"#).expect("write config");

    match Config::load(&dir) {
        Err(ConfigError::MissingApiKey) => {}
        other => panic!("expected MissingApiKey, got {:?}", other.map(|_| ())),
    }

    fs::remove_dir_all(&dir).expect("cleanup");
}

#[test]
fn debug_does_not_leak_the_api_key() {
    let dir = temp_dir("redacted");
    fs::write(
        dir.join("config.json"),
        r#"{"openrouter_api_key": "sk-or-super-secret"}"#,
    )
    .expect("write config");

    let config = Config::load(&dir).expect("load config");
    let debug = format!("{:?}", config);
    assert!(!debug.contains("sk-or-super-secret"), "debug: {}", debug);

    fs::remove_dir_all(&dir).expect("cleanup");
}
