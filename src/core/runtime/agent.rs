use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;

use crate::core::openrouter::types::{Message, Usage};

/// The agent's own settings. Lives in the agent's directory, alongside the
/// project's `config.json` but scoped to this one agent.
pub const AGENT_CONFIG_FILE_NAME: &str = "config.json";
/// The opening instruction an agent runs on. Optional: without it the agent
/// waits for the user to type the first message. Lives in the agent's directory.
pub const DIRECTIVE_FILE_NAME: &str = "DIRECTIVE.md";
/// Describes what the agent is meant to accomplish. Lives in the agent's directory.
pub const AGENTS_FILE_NAME: &str = "AGENTS.md";
/// The system prompt. It sits one level above `AGENTS.md`, describing the
/// agent's restrictions and general guidelines. Looked up in the agent's
/// directory first, then in the project directory.
pub const SYSTEM_FILE_NAME: &str = "SYSTEM.md";

pub struct Agent {
    definition: AgentDefinition,
}

impl Agent {
    /// `id` is the name of the directory the agent was loaded from, which is
    /// unique among the project directory's children.
    pub fn new(id: String, config: AgentConfig) -> Agent {
        Agent {
            definition: AgentDefinition {
                id,
                config,
                state: AgentState::Configuring,
                config_files: None,
                system_prompt: String::new(),
                messages: Vec::new(),
                opened_with_directive: false,
                usage: None,
            },
        }
    }

    pub fn agent_definition(&self) -> AgentDefinition {
        self.definition.clone()
    }
    pub fn id(&self) -> String {
        self.definition.id.clone()
    }
    pub fn definition_mut(&mut self) -> &mut AgentDefinition {
        &mut self.definition
    }
}

/// A snapshot of everything the ui needs to render an agent.
#[derive(Clone)]
pub struct AgentDefinition {
    id: String,
    config: AgentConfig,

    // state
    state: AgentState,
    config_files: Option<ConfigFiles>,
    system_prompt: String,
    messages: Vec<Message>,
    /// Whether the first message came from `DIRECTIVE.md` rather than the user.
    opened_with_directive: bool,
    usage: Option<Usage>,
}

impl AgentDefinition {
    pub fn id(&self) -> String {
        self.id.clone()
    }
    pub fn config(&self) -> AgentConfig {
        self.config.clone()
    }
    pub fn state(&self) -> AgentState {
        self.state.clone()
    }
    pub fn config_files(&self) -> Option<ConfigFiles> {
        self.config_files.clone()
    }
    pub fn system_prompt(&self) -> String {
        self.system_prompt.clone()
    }
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }
    /// True when the conversation was opened by the agent's `DIRECTIVE.md`,
    /// which makes the first message a directive rather than the user's.
    pub fn opened_with_directive(&self) -> bool {
        self.opened_with_directive
    }
    pub fn usage(&self) -> Option<Usage> {
        self.usage.clone()
    }

    pub fn set_config(&mut self, config: AgentConfig) {
        self.config = config;
    }
    pub fn set_state(&mut self, state: AgentState) {
        self.state = state;
    }
    pub fn set_config_files(&mut self, config_files: ConfigFiles) {
        self.config_files = Some(config_files);
    }
    pub fn set_system_prompt(&mut self, system_prompt: String) {
        self.system_prompt = system_prompt;
    }
    pub fn set_usage(&mut self, usage: Usage) {
        self.usage = Some(usage);
    }
    pub fn push_message(&mut self, message: Message) {
        self.messages.push(message);
    }
    /// Records that `DIRECTIVE.md` opened the conversation. Only meaningful
    /// for the first message.
    pub fn push_directive(&mut self, directive: impl Into<String>) {
        self.opened_with_directive = self.messages.is_empty();
        self.messages.push(Message::user(directive));
    }

    /// The messages sent to the model: the system prompt followed by the
    /// conversation so far.
    pub fn request_messages(&self) -> Vec<Message> {
        let mut messages = Vec::with_capacity(self.messages.len() + 1);
        if self.system_prompt.trim() != "" {
            messages.push(Message::system(self.system_prompt.clone()));
        }
        messages.extend(self.messages.iter().cloned());
        messages
    }
}

/// Where an agent is in its lifecycle. Agents are loaded from the project
/// directory already pointed at a directory, so they start out configuring.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentState {
    /// Loaded but not started; its configuration files are on screen.
    Configuring,
    /// Waiting on a response from the model.
    Working,
    /// Started, waiting on the user's next message.
    Idle,
    Failed(String),
}

impl AgentState {
    pub fn label(&self) -> String {
        match self {
            AgentState::Configuring => "configuring".into(),
            AgentState::Working => "working".into(),
            AgentState::Idle => "idle".into(),
            AgentState::Failed(err) => format!("failed: {}", err),
        }
    }
    /// True once the agent has been started and holds a conversation.
    pub fn started(&self) -> bool {
        matches!(
            self,
            AgentState::Working | AgentState::Idle | AgentState::Failed(_)
        )
    }
}

/// One of the files an agent is configured from.
#[derive(Debug, Clone)]
pub struct ConfigFile {
    name: String,
    path: PathBuf,
    present: bool,
    /// An optional file changes how the agent behaves when it is there, but
    /// its absence never stops the agent from running.
    required: bool,
}

impl ConfigFile {
    pub fn name(&self) -> String {
        self.name.clone()
    }
    pub fn path(&self) -> PathBuf {
        self.path.clone()
    }
    pub fn present(&self) -> bool {
        self.present
    }
    pub fn required(&self) -> bool {
        self.required
    }
    /// The agent cannot run until this file shows up.
    pub fn missing(&self) -> bool {
        self.required && !self.present
    }
    pub fn contents(&self) -> Option<String> {
        fs::read_to_string(&self.path).ok()
    }
}

/// The contents of an agent's `config.json`. Fields added here in the future
/// should default, so an older `config.json` keeps loading.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentSettings {
    /// The model the agent runs on. Ex: "openai/gpt-4o".
    pub model: String,
}

/// The files an agent is configured from, and whether each one was found.
#[derive(Debug, Clone)]
pub struct ConfigFiles {
    config_file: ConfigFile,
    agents_file: ConfigFile,
    system_file: ConfigFile,
    directive_file: ConfigFile,

    /// The parsed `config.json`, absent when the file is missing or bad.
    settings: Option<AgentSettings>,
    /// Why `config.json` could not be read, when it is there but unusable.
    settings_error: Option<String>,
}

impl ConfigFiles {
    /// Looks for `config.json`, `AGENTS.md`, `SYSTEM.md` and the optional
    /// `DIRECTIVE.md` in `dir`, reading the settings out of `config.json`. The
    /// system prompt is allowed to live in `project_dir` instead, so a single
    /// one can be shared by every agent in the project.
    pub fn load(dir: &Path, project_dir: &Path) -> ConfigFiles {
        let config_path = dir.join(AGENT_CONFIG_FILE_NAME);
        let config_file = ConfigFile {
            name: AGENT_CONFIG_FILE_NAME.into(),
            present: config_path.is_file(),
            path: config_path,
            required: true,
        };

        let (settings, settings_error) = match config_file.contents() {
            Some(raw) => match serde_json::from_str::<AgentSettings>(&raw) {
                Ok(settings) => (Some(settings), None),
                Err(err) => (None, Some(err.to_string())),
            },
            None => (None, None),
        };

        let agents_path = dir.join(AGENTS_FILE_NAME);
        let agents_file = ConfigFile {
            name: AGENTS_FILE_NAME.into(),
            present: agents_path.is_file(),
            path: agents_path,
            required: true,
        };

        let local_system_path = dir.join(SYSTEM_FILE_NAME);
        let system_path = if local_system_path.is_file() {
            local_system_path
        } else {
            project_dir.join(SYSTEM_FILE_NAME)
        };
        let system_file = ConfigFile {
            name: SYSTEM_FILE_NAME.into(),
            present: system_path.is_file(),
            path: system_path,
            required: true,
        };

        let directive_path = dir.join(DIRECTIVE_FILE_NAME);
        let directive_file = ConfigFile {
            name: DIRECTIVE_FILE_NAME.into(),
            present: directive_path.is_file(),
            path: directive_path,
            required: false,
        };

        ConfigFiles {
            config_file,
            agents_file,
            system_file,
            directive_file,
            settings,
            settings_error,
        }
    }

    pub fn config_file(&self) -> ConfigFile {
        self.config_file.clone()
    }
    pub fn agents_file(&self) -> ConfigFile {
        self.agents_file.clone()
    }
    pub fn system_file(&self) -> ConfigFile {
        self.system_file.clone()
    }
    /// The optional opening instruction. Present means the agent starts on its
    /// own; absent means it waits for the user's first message.
    pub fn directive_file(&self) -> ConfigFile {
        self.directive_file.clone()
    }
    /// The agent's own settings, once `config.json` has been read.
    pub fn settings(&self) -> Option<AgentSettings> {
        self.settings.clone()
    }
    pub fn settings_error(&self) -> Option<String> {
        self.settings_error.clone()
    }
    pub fn files(&self) -> Vec<ConfigFile> {
        vec![
            self.config_file.clone(),
            self.system_file.clone(),
            self.agents_file.clone(),
            self.directive_file.clone(),
        ]
    }
    /// The first required file that isn't there, if any.
    pub fn missing_file(&self) -> Option<ConfigFile> {
        self.files().into_iter().find(|file| file.missing())
    }
    /// Every required file was found and `config.json` parsed, so the agent
    /// can be run.
    pub fn complete(&self) -> bool {
        self.missing_file().is_none() && self.settings.is_some()
    }
}

#[derive(Clone)]
pub struct AgentConfig {
    name: String,
    model: Model,
    dir: PathBuf,
}

impl AgentConfig {
    pub fn empty() -> AgentConfig {
        AgentConfig {
            name: "".into(),
            model: Model {
                author: "".into(),
                slug: "".into(),
            },
            dir: PathBuf::new(),
        }
    }
    pub fn name(&self) -> String {
        self.name.clone()
    }
    pub fn model(&self) -> Model {
        self.model.clone()
    }
    /// The directory the agent works in, chosen by the user at creation time.
    pub fn dir(&self) -> PathBuf {
        self.dir.clone()
    }
    pub fn set_name(mut self, name: String) -> AgentConfig {
        self.name = name;
        self
    }
    pub fn set_model(mut self, model: Model) -> AgentConfig {
        self.model = model;
        self
    }
    pub fn set_dir(mut self, dir: PathBuf) -> AgentConfig {
        self.dir = dir;
        self
    }
}

#[derive(Clone)]
pub struct Model {
    pub author: String, // ex: openai
    pub slug: String,   // ex: gpt-4o
}

impl Model {
    /// Splits an "author/slug" model id, as written in a `config.json`.
    pub fn parse(full_slug: &str) -> Option<Model> {
        match full_slug.split_once('/') {
            Some((author, slug)) if !author.is_empty() && !slug.is_empty() => Some(Model {
                author: author.to_string(),
                slug: slug.to_string(),
            }),
            _ => None,
        }
    }
    pub fn valid(&self) -> bool {
        self.author != "" && self.slug != ""
    }
    pub fn full_slug(&self) -> String {
        format!("{}/{}", self.author, self.slug)
    }
}
