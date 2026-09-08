use std::sync::{
    mpsc::{self, Receiver, Sender, TryRecvError},
    Arc, Mutex, MutexGuard,
};
use std::thread;

use thiserror::Error;

use crate::core::openrouter::client::OpenRouter;
use crate::core::openrouter::types::{ChatCompletionRequest, Message, Usage};
use crate::core::runtime::agent_config::{self, AgentConfig, ConfigFiles, ConfigFilesError};
use crate::core::runtime::helpers::{classify, Turn};

/// Why an agent could not be started. Every one of these is a configuration
/// file the agent needs and cannot use.
#[derive(Debug, Clone, Error)]
pub enum AgentError {
    #[error("the agent's directory is missing `{0}`")]
    ConfigFileMissing(String),

    #[error("`{name}` could not be read: {error}")]
    ConfigFileUnreadable { name: String, error: String },
}

impl From<ConfigFilesError> for AgentError {
    fn from(err: ConfigFilesError) -> AgentError {
        match err {
            ConfigFilesError::Missing { name, .. } => AgentError::ConfigFileMissing(name),
            ConfigFilesError::Unreadable { name, error, .. } => {
                AgentError::ConfigFileUnreadable { name, error }
            }
            err @ ConfigFilesError::InvalidModel(_) => AgentError::ConfigFileUnreadable {
                name: agent_config::AGENT_CONFIG_FILE_NAME.into(),
                error: err.to_string(),
            },
        }
    }
}

/// What the agent's thread is asked to do. The state it works from is set by
/// the caller before the command goes out, so the thread only ever has to be
/// told that there is something to send.
enum AgentCommand {
    /// Run the conversation forward until the model has nothing left to say.
    Run,
    Shutdown,
}

/// An agent and the thread it runs on. The thread is spawned with the agent
/// and lives as long as it does, so a conversation in flight is never tied to
/// whoever asked for it; the ui and the runtime talk to it over a channel and
/// read its state through the mutex.
pub struct Agent {
    id: String,
    inner: Arc<AgentInner>,
    commands: Sender<AgentCommand>,
}

/// Everything the agent's thread shares with the rest of the program.
struct AgentInner {
    definition: Mutex<AgentDefinition>,
    openrouter: Arc<OpenRouter>,
}

impl Agent {
    /// `id` is the name of the directory the agent was loaded from, which is
    /// unique among the project directory's children.
    pub fn new(id: String, config: AgentConfig, openrouter: Arc<OpenRouter>) -> Agent {
        let inner = Arc::new(AgentInner {
            definition: Mutex::new(AgentDefinition {
                id: id.clone(),
                config,
                state: AgentState::Configuring,
                config_files: None,
                config_error: None,
                system_prompt: String::new(),
                messages: Vec::new(),
                message_queue: Vec::new(),
                opened_with_directive: false,
                usage: None,
            }),
            openrouter,
        });

        let (commands, receiver) = mpsc::channel();
        let thread_inner = Arc::clone(&inner);
        thread::spawn(move || thread_inner.run(receiver));

        Agent {
            id,
            inner,
            commands,
        }
    }

    pub fn id(&self) -> String {
        self.id.clone()
    }

    /// A snapshot of the agent, for a caller that only means to read it.
    pub fn agent_definition(&self) -> AgentDefinition {
        self.definition().clone()
    }

    /// The agent's state, held for as long as the guard is. The thread takes
    /// the same lock, so hold it only for as long as it takes to read or
    /// write what you came for.
    pub fn definition(&self) -> MutexGuard<'_, AgentDefinition> {
        self.inner.definition.lock().expect("mutex error")
    }

    /// Starts the agent. With a `DIRECTIVE.md` its thread runs straight away
    /// on that instruction; without one it waits for the user to type the
    /// first message. Fails while any required configuration file is still
    /// missing.
    pub fn start(&self) -> Result<(), AgentError> {
        {
            let mut definition = self.definition();

            // without a usable config.json the agent was never configured at
            // all, so that failure is what to report
            if let Some(err) = definition.config_error() {
                return Err(err.into());
            }
            let config_files = definition.config_files().ok_or_else(|| {
                AgentError::ConfigFileMissing(agent_config::AGENT_CONFIG_FILE_NAME.into())
            })?;
            if let Some(file) = config_files.missing_file() {
                return Err(AgentError::ConfigFileMissing(file.name()));
            }

            definition.set_system_prompt(build_system_prompt(&config_files));

            // without a directive there is nothing to say yet, so the agent
            // goes idle and the user's first message opens the conversation
            let Some(directive) = config_files.directive_file().contents() else {
                definition.set_state(AgentState::Idle);
                return Ok(());
            };

            definition.push_directive(directive.trim());
            definition.set_state(AgentState::Working);
        }

        self.run();
        Ok(())
    }

    /// Hands the agent the user's next message. While the agent is working the
    /// message is queued instead, and the thread picks it up on its next time
    /// around the loop rather than interrupting the turn in flight.
    pub fn send_message(&self, content: String) {
        {
            let mut definition = self.definition();
            if definition.state() == AgentState::Working {
                definition.queue_message(Message::user(content));
                return;
            }

            definition.push_message(Message::user(content));
            definition.set_state(AgentState::Working);
        }

        self.run();
    }

    /// Tells the thread there is something to send. The state it works from is
    /// already written, so a dead thread costs the agent its turn and nothing
    /// else — it is marked failed rather than left working forever.
    fn run(&self) {
        if self.commands.send(AgentCommand::Run).is_err() {
            self.definition()
                .set_state(AgentState::Failed("the agent is no longer running".into()));
        }
    }
}

/// Dropping the agent stops its thread. The turn already in flight finishes
/// first — the thread owns everything it is working with, so it is left to
/// land rather than joined, which would block the ui on a request — but the
/// thread stops there rather than working through what is still queued.
impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.commands.send(AgentCommand::Shutdown);
    }
}

impl AgentInner {
    /// The agent loop. It sits on the channel until there is something to
    /// send, and goes back to waiting once the model has finished answering.
    fn run(self: Arc<Self>, commands: Receiver<AgentCommand>) {
        while let Ok(command) = commands.recv() {
            match command {
                AgentCommand::Run => {
                    let shutting_down = self.run_turns(&commands);
                    if shutting_down {
                        return;
                    }
                }
                AgentCommand::Shutdown => return,
            }
        }
    }

    /// Sends the conversation and records the reply, going around again while
    /// there is more to say — a message the user queued while the agent was
    /// working, or a turn the model has not finished. Returns true when the
    /// agent went away while it was working, in which case the thread is done.
    fn run_turns(&self, commands: &Receiver<AgentCommand>) -> bool {
        loop {
            // the agent can be dropped while a turn is in flight, and what is
            // queued behind that turn then belongs to a conversation nobody
            // can read, so it is not worth another request
            if shutting_down(commands) {
                return true;
            }

            let request = {
                let mut definition = self.definition.lock().expect("mutex error");
                // anything the user typed mid turn joins the conversation here,
                // at the top of the loop, so it is sent with the next request
                definition.take_queued_messages();
                definition.set_state(AgentState::Working);
                chat_request(&definition)
            };

            let result = self.openrouter.chat_completion(request);

            let mut definition = self.definition.lock().expect("mutex error");
            let turn = match result {
                Err(err) => {
                    definition.set_state(AgentState::Failed(err.to_string()));
                    return false;
                }
                Ok(response) => {
                    if let Some(usage) = response.usage.clone() {
                        definition.set_usage(usage);
                    }
                    match response.first_choice() {
                        Some(choice) => {
                            definition.push_message(choice.message.clone());
                            classify(choice)
                        }
                        None => Turn::Failed("the model answered with no choices".into()),
                    }
                }
            };

            match turn {
                Turn::Failed(err) => {
                    definition.set_state(AgentState::Failed(err));
                    return false;
                }
                // the model asked for tool calls, and nothing runs them yet;
                // sending the same conversation back would only ask again
                Turn::Continue => {
                    definition.set_state(AgentState::Failed("tool calls are not supported".into()));
                    return false;
                }
                // the queue is checked under the same lock that idles the
                // agent, so a message sent right now is either queued and
                // answered below or sent as a turn of its own
                Turn::Done => {
                    if definition.queued_messages().is_empty() {
                        definition.set_state(AgentState::Idle);
                        return false;
                    }
                }
            }
        }
    }
}

/// Whether the agent has gone away, leaving its thread nothing to run for.
/// Reads whatever it has sent since the thread last looked, without waiting
/// on it. A `Run` is dropped: the state it refers to is written before the
/// command goes out, so the loop already has it in hand.
fn shutting_down(commands: &Receiver<AgentCommand>) -> bool {
    loop {
        match commands.try_recv() {
            Ok(AgentCommand::Run) => continue,
            // the agent was dropped: either it said so, or it took its end of
            // the channel with it
            Ok(AgentCommand::Shutdown) | Err(TryRecvError::Disconnected) => return true,
            Err(TryRecvError::Empty) => return false,
        }
    }
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

fn chat_request(definition: &AgentDefinition) -> ChatCompletionRequest {
    ChatCompletionRequest::new(
        definition.config().model().full_slug(),
        definition.request_messages(),
    )
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
    /// Messages the user sent while the agent was working. They join
    /// `messages` at the top of the agent loop's next iteration.
    message_queue: Vec<Message>,
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
    /// What the user has sent that the agent has not taken up yet.
    pub fn queued_messages(&self) -> &[Message] {
        &self.message_queue
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
    /// Holds a message back until the agent has finished the turn it is on.
    pub fn queue_message(&mut self, message: Message) {
        self.message_queue.push(message);
    }
    /// Moves everything queued into the conversation, in the order it was
    /// sent, so the next request carries it.
    pub fn take_queued_messages(&mut self) {
        self.messages.append(&mut self.message_queue);
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
