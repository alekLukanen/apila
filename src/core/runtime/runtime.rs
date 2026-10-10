use std::{
    collections::HashSet,
    fmt::Debug,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use thiserror::Error;

use crate::core::config::config::Config;
use crate::core::memory::store::memory_path;
use crate::core::memory::writer::{process_token, AgentJob, MemoryWriter};
use crate::core::openrouter::client::{OpenRouter, OpenRouterConfig, OpenRouterError};
use crate::core::runtime::agent::{Agent, AgentDefinition, AgentError, ClearOutcome};
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

    #[error("`{0}` has not started, so it has no session to clear")]
    AgentNotStarted(String),

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
            // the caller knows which agent it asked about and names it
            AgentError::NotStarted => RuntimeError::AgentNotStarted(String::new()),
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
    /// Learns from every agent's ended sessions, one agent at a time.
    writer: MemoryWriter,
    /// The agents whose leftover sessions this process has already asked the
    /// writer to recover, so a reload does not ask again.
    recovered: Mutex<HashSet<PathBuf>>,
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
        let openrouter = Arc::new(OpenRouter::new(OpenRouterConfig::from_config(
            &config.config(),
        ))?);

        let runtime = Runtime {
            config,
            writer: MemoryWriter::start(Arc::clone(&openrouter), process_token()),
            recovered: Mutex::new(HashSet::new()),
            openrouter,
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

    /// Tools are resolved here, not in the agent loop, so an unknown tool is
    /// reported on the configuration screen before the agent runs.
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

    /// Also called on reload, so a directory added on disk shows up as an
    /// agent. Running agents are left alone; the rest re-read their files.
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
        let mut recover = Vec::new();

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
                    inner.agents.push(Agent::new(
                        name,
                        config,
                        Arc::clone(&self.openrouter),
                        self.writer.handle(),
                    ));
                    inner.agents.len() - 1
                }
            };

            let mut definition = inner.agents[index].definition();
            match loaded {
                Ok((config_files, tools)) => {
                    let config = definition.config().set_from_config_files(&config_files);
                    definition.set_config(config.clone());
                    definition.set_config_files(config_files, tools);
                    recover.push(AgentJob {
                        dir: config.dir(),
                        skills: config.memory().skills,
                        status: definition.memory_status_handle(),
                    });
                }
                Err(err) => {
                    // the settings came out of the file that just failed to load
                    let config = definition.config().clear_settings();
                    definition.set_config(config);
                    definition.set_config_error(err);
                }
            }
        }

        self.recover_sessions(recover);

        inner
            .agents
            .sort_by_key(|agent| agent.agent_definition().config().name());
    }

    /// Asks the writer to tidy up after earlier processes, once per agent per
    /// process. An agent whose config cannot be read is left alone, so a typo
    /// never decides whether its old sessions are analysed.
    fn recover_sessions(&self, jobs: Vec<AgentJob>) {
        let mut recovered = self.recovered.lock().expect("mutex error");
        for job in jobs {
            if !memory_path(&job.dir).is_file() || recovered.contains(&job.dir) {
                continue;
            }
            recovered.insert(job.dir.clone());
            self.writer.handle().enqueue(job);
        }
    }

    /// Refused once the agent has started: its tools and their states were
    /// built from the first reading, and a second would leave them out of step.
    /// A started agent picks up edits on a restart.
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

    /// With a `DIRECTIVE.md` the agent runs on it straight away; without one it
    /// waits for the user's first message.
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

    /// Ends the agent's session and starts a new one. Mid turn the clear is
    /// queued until the turn ends.
    pub fn clear_agent_session(&self, id: &str) -> Result<ClearOutcome, RuntimeError> {
        let inner = self.inner.lock().expect("mutex error");
        inner.agent(id)?.clear_session().map_err(|err| match err {
            AgentError::NotStarted => RuntimeError::AgentNotStarted(id.to_string()),
            err => err.into(),
        })
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
