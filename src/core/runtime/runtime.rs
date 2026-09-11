use std::{
    fmt::Debug,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use thiserror::Error;

use crate::core::config::config::Config;
use crate::core::openrouter::client::{OpenRouter, OpenRouterConfig, OpenRouterError};
use crate::core::runtime::agent::{Agent, AgentDefinition, AgentError};
use crate::core::runtime::agent_config::AgentConfig;
use crate::core::runtime::agent_config::{ConfigFiles, ConfigFilesError};
use crate::core::runtime::tool_registry::{AgentTools, ToolRegistry};

use super::agent;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("no agent with id `{0}`")]
    UnknownAgent(String),

    #[error("`{0}` has already started, so its configuration is settled")]
    AgentAlreadyStarted(String),

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
    /// Every tool the agents in this runtime can be given. Held here rather
    /// than on the agents: an agent is handed the tools its config came to,
    /// never the means of working them out.
    tools: Arc<ToolRegistry>,
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
        Runtime::new_with_tools(config, ToolRegistry::with_default_tools())
    }

    /// The same runtime with a registry of the caller's choosing, so a test can
    /// stand one up around a tool of its own.
    pub fn new_with_tools(
        config: RuntimeConfig,
        tools: ToolRegistry,
    ) -> Result<Runtime, RuntimeError> {
        let openrouter = OpenRouter::new(OpenRouterConfig::from_config(&config.config()))?;

        let runtime = Runtime {
            config,
            openrouter: Arc::new(openrouter),
            tools: Arc::new(tools),
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

    /// The tools this runtime can give an agent.
    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }

    /// Reads the agent's files and works its `tools` block out against what
    /// this runtime can actually run.
    ///
    /// Resolving here rather than in the agent loop means a tool name the user
    /// made up is reported the way a bad model id is — on the configuration
    /// screen, before the agent runs — and the loop is handed the tools rather
    /// than looking them up on every request.
    fn load_config_files(&self, dir: &Path) -> Result<(ConfigFiles, AgentTools), ConfigFilesError> {
        let config_files = ConfigFiles::load(dir, &self.config.project_dir())?;
        let tools = self
            .tools
            .resolve(&config_files.tool_settings())
            .map_err(|err| ConfigFilesError::InvalidTools(err.to_string()))?;
        Ok((config_files, tools))
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
        let dirs = self.agent_dirs();

        // which directories still need reading is a question about the agents,
        // so it is answered under the lock — and then let go of again
        let to_read: Vec<PathBuf> = {
            let mut inner = self.inner.lock().expect("mutex error");

            // an agent whose directory is gone is dropped, unless it is already
            // running, in which case its session is kept
            inner.agents.retain(|agent| {
                let definition = agent.agent_definition();
                definition.state().started() || dirs.contains(&definition.config().dir())
            });

            // a running agent read its files once and keeps what it read, so
            // there is nothing to be learned by reading them again
            let settled: Vec<PathBuf> = inner
                .agents
                .iter()
                .map(|agent| agent.agent_definition())
                .filter(|definition| definition.state().started())
                .map(|definition| definition.config().dir())
                .collect();

            dirs.iter()
                .filter(|dir| !settled.contains(dir))
                .cloned()
                .collect()
        };

        // reading the files and working out the tools they come to happen with
        // no lock held. the ui reads the agent list on every tick, and a project
        // directory on a slow disk would otherwise stop it drawing
        let loaded: Vec<(PathBuf, Result<(ConfigFiles, AgentTools), ConfigFilesError>)> = to_read
            .into_iter()
            .map(|dir| {
                let result = self.load_config_files(&dir);
                (dir, result)
            })
            .collect();

        let mut inner = self.inner.lock().expect("mutex error");

        for (dir, loaded) in loaded {
            // an unusable config.json leaves the agent listed but unconfigured,
            // so the user can see what to fix and reload
            let existing = inner
                .agents
                .iter()
                .position(|agent| agent.agent_definition().config().dir() == dir);

            let index = match existing {
                Some(index) => {
                    // it may have been started while its files were being read,
                    // and a started agent keeps the configuration it started on
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
                Ok((config_files, tools)) => {
                    let config = definition.config().set_from_config_files(&config_files);
                    definition.set_config(config);
                    definition.set_config_files(config_files, tools);
                }
                Err(err) => {
                    // the settings came out of the file that just failed to load
                    let config = definition.config().clear_settings();
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
    ///
    /// Refused once the agent has started. Its settings are read once, when its
    /// files are loaded, and everything downstream is built from that reading:
    /// the tools it resolved to, the model its requests name, the settings its
    /// tools parsed when they started. Reading the file again would leave those
    /// describing different generations of it — at best a turn offering old
    /// tools to a new model, at worst one naming a model that had just been
    /// emptied. An agent picks up an edited `config.json` the way it picked up
    /// the first one: on a restart.
    pub fn reload_config_files(&self, id: &str) -> Result<ConfigFiles, RuntimeError> {
        let inner = self.inner.lock().expect("mutex error");
        let mut definition = inner.agent(id)?.definition();

        if definition.state().started() {
            return Err(RuntimeError::AgentAlreadyStarted(id.to_string()));
        }

        let (config_files, tools) = match self.load_config_files(&definition.config().dir()) {
            Ok(loaded) => loaded,
            Err(err) => {
                let config = definition.config().clear_settings();
                definition.set_config(config);
                definition.set_config_error(err.clone());
                return Err(err.into());
            }
        };

        let config = definition.config().set_from_config_files(&config_files);
        definition.set_config(config);
        definition.set_config_files(config_files.clone(), tools);

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
