use std::{
    fmt::Debug,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use thiserror::Error;

use crate::core::config::config::Config;
use crate::core::openrouter::client::{OpenRouter, OpenRouterConfig, OpenRouterError};
use crate::core::runtime::agent::{Agent, AgentDefinition, AgentError};
use crate::core::runtime::agent_config::{AgentConfig, Model};
use crate::core::runtime::agent_config::{ConfigFiles, ConfigFilesError};

use super::agent;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("no agent with id `{0}`")]
    UnknownAgent(String),

    #[error("the agent's directory is missing `{0}`")]
    ConfigFileMissing(String),

    #[error("`{name}` could not be read: {error}")]
    ConfigFileUnreadable { name: String, error: String },

    #[error("openrouter error: {0}")]
    OpenRouter(#[from] OpenRouterError),
}

/// An agent's `config.json` is required, so every way of failing to read it
/// keeps the shape the ui already reports: the file is missing, or it is
/// there and cannot be used.
impl From<ConfigFilesError> for RuntimeError {
    fn from(err: ConfigFilesError) -> RuntimeError {
        AgentError::from(err).into()
    }
}

/// The agent reports the same configuration failures the runtime does, since
/// they are the ones that stop it starting.
impl From<AgentError> for RuntimeError {
    fn from(err: AgentError) -> RuntimeError {
        match err {
            AgentError::ConfigFileMissing(name) => RuntimeError::ConfigFileMissing(name),
            AgentError::ConfigFileUnreadable { name, error } => {
                RuntimeError::ConfigFileUnreadable { name, error }
            }
        }
    }
}

pub struct Runtime {
    config: RuntimeConfig,
    openrouter: Arc<OpenRouter>,
    inner: Arc<Mutex<RuntimeInner>>,
}

impl Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Runtime<...>")
    }
}

struct RuntimeInner {
    agents: Vec<agent::Agent>,
}

impl RuntimeInner {
    /// Agents carry their own state behind their own lock, so a shared
    /// reference is enough to talk to one.
    fn agent(&self, id: &str) -> Result<&agent::Agent, RuntimeError> {
        self.agents
            .iter()
            .find(|agent| agent.id() == id)
            .ok_or_else(|| RuntimeError::UnknownAgent(id.to_string()))
    }
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

        let runtime = Runtime {
            config,
            openrouter: Arc::new(openrouter),
            inner: Arc::new(Mutex::new(RuntimeInner { agents: Vec::new() })),
        };
        runtime.load_agents();

        Ok(runtime)
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

    /// Loads an agent for every directory in the project directory. Called at
    /// startup and again whenever the user reloads, so a directory added on
    /// disk shows up in the list without the user setting anything up.
    ///
    /// Agents that are already running are left alone; the rest have their
    /// configuration files re-read.
    pub fn load_agents(&self) {
        let project_dir = self.config.project_dir();
        let dirs = self.agent_dirs();

        let mut inner = self.inner.lock().expect("mutex error");

        // an agent whose directory is gone is dropped, unless it is already
        // running, in which case its session is kept
        inner.agents.retain(|agent| {
            let definition = agent.agent_definition();
            definition.state().started() || dirs.contains(&definition.config().dir())
        });

        for dir in dirs {
            // an unusable config.json leaves the agent listed but unconfigured,
            // so the user can see what to fix and reload
            let loaded = ConfigFiles::load(&dir, &project_dir);
            let existing = inner
                .agents
                .iter()
                .position(|agent| agent.agent_definition().config().dir() == dir);

            let index = match existing {
                Some(index) => {
                    // a running agent already read its files; re-reading them
                    // would say nothing about the session in flight
                    if inner.agents[index].agent_definition().state().started() {
                        continue;
                    }
                    index
                }
                None => {
                    let name = Self::dir_name(&dir);
                    let config = AgentConfig::empty().set_name(name.clone()).set_dir(dir);
                    inner
                        .agents
                        .push(Agent::new(name, config, Arc::clone(&self.openrouter)));
                    inner.agents.len() - 1
                }
            };

            let mut definition = inner.agents[index].definition();
            match loaded {
                Ok(config_files) => {
                    let config = definition.config().set_model(config_files.model());
                    definition.set_config(config);
                    definition.set_config_files(config_files);
                }
                Err(err) => {
                    // the model came out of the file that just failed to load
                    let config = definition.config().set_model(Model::empty());
                    definition.set_config(config);
                    definition.set_config_error(err);
                }
            }
        }

        inner
            .agents
            .sort_by_key(|agent| agent.agent_definition().config().name());
    }

    /// Re-reads the agent's configuration files, picking up ones the user
    /// added since it was loaded.
    pub fn reload_config_files(&self, id: &str) -> Result<ConfigFiles, RuntimeError> {
        let project_dir = self.config.project_dir();

        let inner = self.inner.lock().expect("mutex error");
        let mut definition = inner.agent(id)?.definition();

        let config_files = match ConfigFiles::load(&definition.config().dir(), &project_dir) {
            Ok(config_files) => config_files,
            Err(err) => {
                let config = definition.config().set_model(Model::empty());
                definition.set_config(config);
                definition.set_config_error(err.clone());
                return Err(err.into());
            }
        };

        let config = definition.config().set_model(config_files.model());
        definition.set_config(config);
        definition.set_config_files(config_files.clone());

        Ok(config_files)
    }

    /// Starts the agent, which from here on runs on its own thread. With a
    /// `DIRECTIVE.md` it begins working straight away; without one it waits
    /// for the user to type the first message. Fails while any required
    /// configuration file is still missing.
    pub fn start_agent(&self, id: &str) -> Result<(), RuntimeError> {
        let inner = self.inner.lock().expect("mutex error");
        inner.agent(id)?.start()?;
        Ok(())
    }

    /// Hands the user's message to the agent. It answers on its own thread,
    /// so this returns as soon as the agent has taken the message; one sent
    /// while the agent is working is queued until it comes back around.
    pub fn send_agent_message(&self, id: &str, content: String) -> Result<(), RuntimeError> {
        let inner = self.inner.lock().expect("mutex error");
        inner.agent(id)?.send_message(content);
        Ok(())
    }

    /// The directories in the project directory an agent is loaded from.
    /// Hidden directories and `target` are skipped since agents never live
    /// there.
    pub fn agent_dirs(&self) -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = match std::fs::read_dir(self.config.project_dir()) {
            Ok(entries) => entries
                .flatten()
                .map(|entry| entry.path())
                .filter(|path| path.is_dir())
                .filter(|path| {
                    let name = Self::dir_name(path);
                    !name.starts_with('.') && name != "target"
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        dirs.sort();
        dirs
    }

    fn dir_name(dir: &Path) -> String {
        dir.file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default()
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

    pub fn agent(&self, id: &str) -> Option<AgentDefinition> {
        self.inner
            .lock()
            .expect("mutex error")
            .agents
            .iter()
            .find(|agent| agent.id() == id)
            .map(|agent| agent.agent_definition())
    }
}
