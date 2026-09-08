use std::{
    fmt::Debug,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
};

use thiserror::Error;

use crate::core::openrouter::client::{OpenRouter, OpenRouterConfig, OpenRouterError};
use crate::core::openrouter::types::{ChatCompletionRequest, Message};
use crate::core::runtime::agent::{Agent, AgentDefinition, AgentState};
use crate::core::runtime::agent_config::{AgentConfig, Model};
use crate::core::runtime::agent_config::{ConfigFiles, ConfigFilesError};
use crate::core::{config::config::Config, runtime::agent_config};

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
        match err {
            ConfigFilesError::Missing { name, .. } => RuntimeError::ConfigFileMissing(name),
            ConfigFilesError::Unreadable { name, error, .. } => {
                RuntimeError::ConfigFileUnreadable { name, error }
            }
            err @ ConfigFilesError::InvalidModel(_) => RuntimeError::ConfigFileUnreadable {
                name: agent_config::AGENT_CONFIG_FILE_NAME.into(),
                error: err.to_string(),
            },
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
    fn agent_mut(&mut self, id: &str) -> Result<&mut agent::Agent, RuntimeError> {
        self.agents
            .iter_mut()
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
                    inner.agents.push(Agent::new(name, config));
                    inner.agents.len() - 1
                }
            };

            let definition = inner.agents[index].definition_mut();
            match loaded {
                Ok(config_files) => {
                    definition.set_config(definition.config().set_model(config_files.model()));
                    definition.set_config_files(config_files);
                }
                Err(err) => {
                    // the model came out of the file that just failed to load
                    definition.set_config(definition.config().set_model(Model::empty()));
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

        let mut inner = self.inner.lock().expect("mutex error");
        let agent = inner.agent_mut(id)?;
        let definition = agent.definition_mut();

        let config_files = match ConfigFiles::load(&definition.config().dir(), &project_dir) {
            Ok(config_files) => config_files,
            Err(err) => {
                definition.set_config(definition.config().set_model(Model::empty()));
                definition.set_config_error(err.clone());
                return Err(err.into());
            }
        };

        definition.set_config(definition.config().set_model(config_files.model()));
        definition.set_config_files(config_files.clone());

        Ok(config_files)
    }

    /// Starts the agent. With a `DIRECTIVE.md` it runs straight away on that
    /// instruction; without one it waits for the user to type the first
    /// message. Fails while any required configuration file is still missing.
    pub fn start_agent(&self, id: &str) -> Result<(), RuntimeError> {
        if let Some(request) = self.start_request(id)? {
            self.request_response(id, request);
        }

        Ok(())
    }

    /// Readies the agent and builds its opening request out of `DIRECTIVE.md`.
    /// Returns `None` when the agent has no directive, leaving it idle with
    /// nothing sent — a request holding only a system prompt is rejected by
    /// the providers behind openrouter anyway.
    fn start_request(&self, id: &str) -> Result<Option<ChatCompletionRequest>, RuntimeError> {
        let request = {
            let mut inner = self.inner.lock().expect("mutex error");
            let agent = inner.agent_mut(id)?;
            let definition = agent.definition_mut();

            // without a usable config.json the agent was never configured at
            // all, so that failure is what to report
            if let Some(err) = definition.config_error() {
                return Err(err.into());
            }
            let config_files = definition.config_files().ok_or_else(|| {
                RuntimeError::ConfigFileMissing(agent_config::AGENT_CONFIG_FILE_NAME.into())
            })?;
            if let Some(file) = config_files.missing_file() {
                return Err(RuntimeError::ConfigFileMissing(file.name()));
            }

            definition.set_system_prompt(Self::build_system_prompt(&config_files));

            // without a directive there is nothing to say yet, so the agent
            // goes idle and the user's first message opens the conversation
            let Some(directive) = config_files.directive_file().contents() else {
                definition.set_state(AgentState::Idle);
                return Ok(None);
            };

            definition.push_directive(directive.trim());
            definition.set_state(AgentState::Working);

            Some(Self::chat_request(definition))
        };

        Ok(request)
    }

    /// Appends the user's message and asks the model for a response.
    pub fn send_agent_message(&self, id: &str, content: String) -> Result<(), RuntimeError> {
        let request = {
            let mut inner = self.inner.lock().expect("mutex error");
            let agent = inner.agent_mut(id)?;
            let definition = agent.definition_mut();

            definition.push_message(Message::user(content));
            definition.set_state(AgentState::Working);

            Self::chat_request(definition)
        };

        self.request_response(id, request);

        Ok(())
    }

    /// Runs the request on a background thread so the ui keeps drawing while
    /// it waits, recording the reply against the agent once it lands.
    fn request_response(&self, id: &str, request: ChatCompletionRequest) {
        let inner = Arc::clone(&self.inner);
        let openrouter = Arc::clone(&self.openrouter);
        let id = id.to_string();

        thread::spawn(move || {
            let result = openrouter.chat_completion(request);

            let mut inner = inner.lock().expect("mutex error");
            let agent = match inner.agent_mut(&id) {
                Ok(agent) => agent,
                // the agent was removed while the request was in flight
                Err(_) => return,
            };
            let definition = agent.definition_mut();

            match result {
                Ok(resp) => {
                    if let Some(choice) = resp.first_choice() {
                        definition.push_message(choice.message.clone());
                    }
                    if let Some(usage) = resp.usage {
                        definition.set_usage(usage);
                    }
                    definition.set_state(AgentState::Idle);
                }
                Err(err) => definition.set_state(AgentState::Failed(err.to_string())),
            }
        });
    }

    /// The system prompt is `SYSTEM.md` — the restrictions and general
    /// guidelines — followed by `AGENTS.md`, which is scoped to this agent's
    /// purpose and so sits below it.
    fn build_system_prompt(config_files: &ConfigFiles) -> String {
        let system = config_files.system_file().contents().unwrap_or_default();
        let agents = config_files.agents_file().contents().unwrap_or_default();
        format!(
            "{}\n\n# Agent Purpose\n\nThe following describes the purpose of this \
             specific agent. It is scoped by everything above.\n\n{}",
            system.trim(),
            agents.trim()
        )
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

    fn chat_request(definition: &AgentDefinition) -> ChatCompletionRequest {
        ChatCompletionRequest::new(
            definition.config().model().full_slug(),
            definition.request_messages(),
        )
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
