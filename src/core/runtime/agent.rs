use crate::core::{
    openrouter::types::{Message, Usage},
    runtime::agent_config::{AgentConfig, ConfigFiles, ConfigFilesError},
};

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
                config_error: None,
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
    /// Why the agent's `config.json` could not be loaded, when it could not.
    /// Set instead of `config_files`, since without that file there is no
    /// configuration to speak of.
    config_error: Option<ConfigFilesError>,
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
    /// Why the configuration could not be read. Present exactly when
    /// `config_files` is absent, once the agent has been loaded.
    pub fn config_error(&self) -> Option<ConfigFilesError> {
        self.config_error.clone()
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
        self.config_error = None;
    }
    /// Records that the agent has no usable configuration, dropping whatever
    /// was read before it: the files on disk no longer say what it holds.
    pub fn set_config_error(&mut self, error: ConfigFilesError) {
        self.config_files = None;
        self.config_error = Some(error);
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
