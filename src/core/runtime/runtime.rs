use std::{
    fmt::Debug,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use thiserror::Error;

use crate::core::config::config::Config;
use crate::core::openrouter::client::{OpenRouter, OpenRouterConfig, OpenRouterError};
use crate::core::runtime::agent::{AgentDefinition, Model};

use super::agent;

/// Model used for new agents when the config file doesn't name one.
const FALLBACK_MODEL_AUTHOR: &str = "openai";
const FALLBACK_MODEL_SLUG: &str = "gpt-5.6-sol";

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("create agent failed")]
    CreateAgentFailed,

    #[error("openrouter error: {0}")]
    OpenRouter(#[from] OpenRouterError),
}

pub struct Runtime {
    config: RuntimeConfig,
    openrouter: OpenRouter,
    inner: Arc<Mutex<RuntimeInner>>,
}

impl Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Runtime<...>")
    }
}

struct RuntimeInner {
    agent_idx: u64,
    agents: Vec<agent::Agent>,
}

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    project_dir: PathBuf,
    config: Config,
}

impl RuntimeConfig {
    pub fn new(project_dir: PathBuf, config: Config) -> RuntimeConfig {
        RuntimeConfig {
            project_dir: project_dir,
            config: config,
        }
    }
    pub fn project_dir(&self) -> PathBuf {
        self.project_dir.clone()
    }
    /// The contents of the project's config.json.
    pub fn config(&self) -> Config {
        self.config.clone()
    }
    // add setters here barrow then pass back -> RuntimeConfig
}

impl Runtime {
    pub fn new(config: RuntimeConfig) -> Result<Runtime, RuntimeError> {
        let openrouter = OpenRouter::new(OpenRouterConfig::from_config(&config.config()))?;

        Ok(Runtime {
            config,
            openrouter,
            inner: Arc::new(Mutex::new(RuntimeInner {
                agent_idx: 0,
                agents: Vec::new(),
            })),
        })
    }

    pub fn config(&self) -> RuntimeConfig {
        self.config.clone()
    }

    /// The interface used to talk to OpenRouter. Held outside the mutex since
    /// it is immutable and safe to share.
    pub fn openrouter(&self) -> &OpenRouter {
        &self.openrouter
    }

    // Agent operations //////////////////
    //////////////////////////////////////
    pub fn create_agent(&self, mut config: agent::AgentConfig) -> Result<u64, RuntimeError> {
        let mut inner = self.inner.lock().expect("mutex error");
        inner.agent_idx += 1;
        let idx = inner.agent_idx;

        // handle defaults as needed
        if config.name() == "" {
            config = config.set_name(format!("agent-{}", idx));
        }
        if !config.model().valid() {
            config = config.set_model(self.default_model());
        }

        let agent = agent::Agent::new(idx, config, PathBuf::new());
        inner.agents.push(agent);

        Ok(idx)
    }

    /// The model new agents use, taken from config.json when it names one.
    fn default_model(&self) -> Model {
        let model = self
            .config
            .config()
            .default_model
            .and_then(|slug| match slug.split_once('/') {
                Some((author, slug)) if !author.is_empty() && !slug.is_empty() => Some(Model {
                    author: author.to_string(),
                    slug: slug.to_string(),
                }),
                _ => None,
            });

        model.unwrap_or(Model {
            author: FALLBACK_MODEL_AUTHOR.into(),
            slug: FALLBACK_MODEL_SLUG.into(),
        })
    }

    pub fn list_agents(&self) -> Vec<AgentDefinition> {
        self.inner
            .lock()
            .expect("mutex error")
            .agents
            .iter()
            .map(|agent| agent.agent_definition())
            .collect()
    }
}
