use std::{
    fmt::Debug,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    thread,
};

use thiserror::Error;

use crate::core::config::config::Config;
use crate::core::openrouter::client::{OpenRouter, OpenRouterConfig, OpenRouterError};
use crate::core::openrouter::types::{ChatCompletionRequest, Message};
use crate::core::runtime::agent::{
    Agent, AgentConfig, AgentDefinition, AgentState, ConfigFiles, Model,
};

use super::agent;

/// Model used for new agents when the config file doesn't name one.
const FALLBACK_MODEL_AUTHOR: &str = "openai";
const FALLBACK_MODEL_SLUG: &str = "gpt-5.6-sol";

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
            let config_files = ConfigFiles::load(&dir, &project_dir);
            let existing = inner
                .agents
                .iter_mut()
                .find(|agent| agent.agent_definition().config().dir() == dir);

            let model = self.model_for(&config_files);

            match existing {
                Some(agent) => {
                    let definition = agent.definition_mut();
                    // a running agent already read its files; re-reading them
                    // would say nothing about the session in flight
                    if !definition.state().started() {
                        definition.set_config(definition.config().set_model(model));
                        definition.set_config_files(config_files);
                    }
                }
                None => {
                    let name = Self::dir_name(&dir);
                    let config = AgentConfig::empty()
                        .set_name(name.clone())
                        .set_model(model)
                        .set_dir(dir);
                    let mut agent = Agent::new(name, config);
                    agent.definition_mut().set_config_files(config_files);
                    inner.agents.push(agent);
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

        let config_files = ConfigFiles::load(&definition.config().dir(), &project_dir);
        let model = Self::model_from(&config_files).unwrap_or_else(|| definition.config().model());
        definition.set_config(definition.config().set_model(model));
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

            let config_files = definition.config_files().ok_or_else(|| {
                RuntimeError::ConfigFileMissing(agent::AGENT_CONFIG_FILE_NAME.into())
            })?;
            if let Some(file) = config_files.missing_file() {
                return Err(RuntimeError::ConfigFileMissing(file.name()));
            }
            if let Some(error) = config_files.settings_error() {
                return Err(RuntimeError::ConfigFileUnreadable {
                    name: agent::AGENT_CONFIG_FILE_NAME.into(),
                    error,
                });
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

    fn chat_request(definition: &AgentDefinition) -> ChatCompletionRequest {
        ChatCompletionRequest::new(
            definition.config().model().full_slug(),
            definition.request_messages(),
        )
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

    /// Builds the opening request without sending it, for the tests.
    #[cfg(test)]
    pub fn start_request_for_test(
        &self,
        id: &str,
    ) -> Result<Option<ChatCompletionRequest>, RuntimeError> {
        self.start_request(id)
    }

    /// Puts the agent into a running session with a canned exchange, so the
    /// session view can be rendered without touching the network.
    #[cfg(test)]
    pub fn seed_session_for_test(
        &self,
        id: &str,
        opening: &str,
        assistant: &str,
        from_directive: bool,
    ) {
        let mut inner = self.inner.lock().expect("mutex error");
        let agent = inner.agent_mut(id).expect("agent exists");
        let definition = agent.definition_mut();
        if from_directive {
            definition.push_directive(opening);
        } else {
            definition.push_message(Message::user(opening));
        }
        definition.push_message(Message::assistant(assistant));
        definition.set_state(AgentState::Idle);
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

    fn dir_name(dir: &Path) -> String {
        dir.file_name()
            .map(|name| name.to_string_lossy().to_string())
            .unwrap_or_default()
    }

    /// The model the agent runs on: whatever its own `config.json` names,
    /// falling back to the project's default when it names nothing usable.
    fn model_for(&self, config_files: &ConfigFiles) -> Model {
        Self::model_from(config_files).unwrap_or_else(|| self.default_model())
    }

    /// The model named by an agent's `config.json`, if it parsed and holds a
    /// usable "author/slug" id.
    fn model_from(config_files: &ConfigFiles) -> Option<Model> {
        config_files
            .settings()
            .and_then(|settings| Model::parse(&settings.model))
    }

    /// The model used when an agent's `config.json` names none, taken from the
    /// project's config.json when it names one.
    fn default_model(&self) -> Model {
        let model = self
            .config
            .config()
            .default_model
            .as_deref()
            .and_then(Model::parse);

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
