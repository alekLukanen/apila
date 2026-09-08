use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use thiserror::Error;

/// Name of the config file Apila expects to find in the project directory.
pub const CONFIG_FILE_NAME: &str = "config.json";

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("config file not found: {0}")]
    NotFound(PathBuf),

    #[error("failed to read config file: {0}")]
    Io(#[from] std::io::Error),

    #[error("failed to parse config file: {0}")]
    Parse(#[from] serde_json::Error),

    #[error("`openrouter_api_key` is missing or empty in the config file")]
    MissingApiKey,
}

/// The contents of `<project_dir>/config.json`.
#[derive(Clone, Deserialize)]
pub struct Config {
    pub openrouter_api_key: String,

    /// Overrides the default OpenRouter base url.
    #[serde(default)]
    pub openrouter_base_url: Option<String>,

    /// OpenRouter attribution headers.
    #[serde(default)]
    pub http_referer: Option<String>,
    #[serde(default)]
    pub x_openrouter_title: Option<String>,
}

/// Hand written so the api key can never leak into a log or a panic message.
impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("openrouter_api_key", &"<redacted>")
            .field("openrouter_base_url", &self.openrouter_base_url)
            .field("http_referer", &self.http_referer)
            .field("x_openrouter_title", &self.x_openrouter_title)
            .finish()
    }
}

impl Config {
    /// Loads and validates `<project_dir>/config.json`.
    pub fn load(project_dir: &Path) -> Result<Config, ConfigError> {
        let path = project_dir.join(CONFIG_FILE_NAME);
        if !path.is_file() {
            return Err(ConfigError::NotFound(path));
        }

        let raw = fs::read_to_string(&path)?;
        let config: Config = serde_json::from_str(&raw)?;

        if config.openrouter_api_key.trim() == "" {
            return Err(ConfigError::MissingApiKey);
        }

        Ok(config)
    }
}
