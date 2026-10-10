use std::path::PathBuf;
use std::sync::{
    mpsc::{self, Receiver, Sender, TryRecvError},
    Arc, Mutex, MutexGuard,
};
use std::thread;

use thiserror::Error;

use crate::core::memory::recorder::{MemoryStatus, Messages, SessionRecorder};
use crate::core::memory::writer::{AgentJob, WriterHandle};
use crate::core::openrouter::client::OpenRouter;
use crate::core::openrouter::types::{
    ChatCompletionRequest, Message, Reasoning, ReasoningEffort, ToolChoice, Usage,
};
use crate::core::runtime::agent_config::{self, AgentConfig, ConfigFiles, ConfigFilesError};
use crate::core::runtime::helpers::{classify, Turn};
use crate::core::runtime::tool_registry::{AgentTools, ToolResult, ToolStates};

/// Why an agent could not be started. Every one of these is a configuration
/// file the agent needs and cannot use.
#[derive(Debug, Clone, Error)]
pub enum AgentError {
    #[error("the agent's directory is missing `{0}`")]
    ConfigFileMissing(String),

    #[error("`{name}` could not be read: {error}")]
    ConfigFileUnreadable { name: String, error: String },

    #[error("the agent has not started, so it has no session to clear")]
    NotStarted,
}

impl From<ConfigFilesError> for AgentError {
    fn from(err: ConfigFilesError) -> AgentError {
        match err {
            ConfigFilesError::Missing { name, .. } => AgentError::ConfigFileMissing(name),
            ConfigFilesError::Unreadable { name, error, .. } => {
                AgentError::ConfigFileUnreadable { name, error }
            }
            // every way a setting can be wrong reads the same to the user: the
            // file is there and cannot be used, and the message says why
            err @ (ConfigFilesError::InvalidModel(_)
            | ConfigFilesError::InvalidMaxIterations
            | ConfigFilesError::InvalidTools(_)
            | ConfigFilesError::InvalidMemory(_)) => AgentError::ConfigFileUnreadable {
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

/// What became of a request to clear the agent's session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClearOutcome {
    /// The agent was not working, so the new session starts straight away.
    Cleared,
    /// The agent is mid turn; the session ends once the turn does.
    Queued,
}

/// The thread lives as long as the agent, so a conversation in flight is never
/// tied to whoever asked for it. Commands go over a channel; state is read
/// through the mutex.
pub struct Agent {
    id: String,
    inner: Arc<AgentInner>,
    commands: Sender<AgentCommand>,
}

/// Everything the agent's thread shares with the rest of the program.
struct AgentInner {
    definition: Mutex<AgentDefinition>,
    openrouter: Arc<OpenRouter>,
    writer: WriterHandle,
}

impl Agent {
    /// `id` is the name of the agent's directory, unique in the project.
    /// `writer` takes its ended sessions and names the recording process.
    pub fn new(
        id: String,
        config: AgentConfig,
        openrouter: Arc<OpenRouter>,
        writer: WriterHandle,
    ) -> Agent {
        let inner = Arc::new(AgentInner {
            definition: Mutex::new(AgentDefinition {
                id: id.clone(),
                config,
                state: AgentState::Configuring,
                config_files: None,
                tools: AgentTools::empty(),
                config_error: None,
                system_prompt: String::new(),
                messages: Vec::new(),
                message_queue: Vec::new(),
                opened_with_directive: false,
                usage: None,
                clear_pending: false,
                next_session_messages: Vec::new(),
                memory_status: MemoryStatus::default(),
            }),
            openrouter,
            writer,
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

    /// With a `DIRECTIVE.md` the agent runs on it straight away; without one it
    /// waits for the user's first message.
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

            // without a directive there is nothing to say yet, so the agent
            // goes idle and the user's first message opens the conversation
            if !begin_session(&mut definition, &config_files) {
                definition.set_state(AgentState::Idle);
                return Ok(());
            }
            definition.set_state(AgentState::Working);
        }

        self.run();
        Ok(())
    }

    /// While the agent works the message is queued and joins at the next
    /// iteration, which lets the user steer mid turn without ever landing
    /// between tool calls and their results.
    pub fn send_message(&self, content: String) {
        {
            let mut definition = self.definition();
            // a clear is on its way, so this opens the session after it
            if definition.clear_pending {
                definition
                    .next_session_messages
                    .push(Message::user(content));
            } else if definition.state() == AgentState::Working {
                definition.queue_message(Message::user(content));
            } else {
                definition.push_message(Message::user(content));
                definition.set_state(AgentState::Working);
            }
        }

        // sent even for a queued message, so a dead thread is noticed here
        // rather than leaving the agent working forever
        self.run();
    }

    /// Ends the session and starts a new one, the way starting the agent does.
    /// Mid turn it waits for the turn to end; a second request while one is
    /// waiting changes nothing.
    pub fn clear_session(&self) -> Result<ClearOutcome, AgentError> {
        let outcome = {
            let mut definition = self.definition();
            if !definition.state().started() {
                return Err(AgentError::NotStarted);
            }
            definition.clear_pending = true;
            if definition.state() == AgentState::Working {
                ClearOutcome::Queued
            } else {
                ClearOutcome::Cleared
            }
        };
        self.run();
        Ok(outcome)
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

/// The thread is not joined, which would block the ui on a request. It
/// finishes the turn in flight and then stops, leaving the queue unworked.
impl Drop for Agent {
    fn drop(&mut self) {
        let _ = self.commands.send(AgentCommand::Shutdown);
    }
}

impl AgentInner {
    /// The agent loop. It sits on the channel until there is something to
    /// send, and goes back to waiting once the model has finished answering.
    fn run(self: Arc<Self>, commands: Receiver<AgentCommand>) {
        // made on the first command, once the runtime has settled the agent's
        // directory
        let mut session: Option<Session> = None;

        while let Ok(command) = commands.recv() {
            match command {
                AgentCommand::Run => {
                    let session = session.get_or_insert_with(|| self.new_session());
                    if self.on_run(&commands, session) {
                        break;
                    }
                }
                AgentCommand::Shutdown => break,
            }
        }

        // whatever landed since the last sync is kept; the session itself is
        // left open and ended by the next process
        if let Some(session) = &mut session {
            session.recorder.sync(&self.definition);
        }
    }

    /// Nothing else can reach a session's tool states, so tools mutate them
    /// without locking, and whatever a tool opened is closed when it drops.
    fn new_session(&self) -> Session {
        let (dir, status) = {
            let definition = self.definition.lock().expect("mutex error");
            (definition.config().dir(), definition.memory_status.clone())
        };
        let mut states = ToolStates::new();
        states.set_openrouter(Arc::clone(&self.openrouter));
        Session {
            states,
            recorder: SessionRecorder::new(dir, self.writer.owner(), status),
        }
    }

    /// Runs whatever the state says is waiting, then any clear asked for while
    /// it ran. A `Run` with nothing waiting is stale and dropped. True when
    /// the agent went away and the thread is done.
    fn on_run(&self, commands: &Receiver<AgentCommand>, session: &mut Session) -> bool {
        let mut run = {
            let definition = self.definition.lock().expect("mutex error");
            match definition.state() {
                // a clear asked for before this turn began was told it waits
                // for the turn, so the turn runs first
                AgentState::Working => true,
                // a message the failed turn never took up is retried, unless
                // a clear carries it into the next session instead
                AgentState::Failed(_) => {
                    !definition.clear_pending && !definition.message_queue.is_empty()
                }
                AgentState::Idle | AgentState::Configuring => false,
            }
        };

        loop {
            if run && self.run_turns(commands, session) {
                return true;
            }
            if !self.definition.lock().expect("mutex error").clear_pending {
                return false;
            }
            run = self.clear(session);
        }
    }

    /// Ends the session and starts the next. True when the new session opens
    /// with something to answer.
    fn clear(&self, session: &mut Session) -> bool {
        session.recorder.sync(&self.definition);

        let (config, config_files, status) = {
            let definition = self.definition.lock().expect("mutex error");
            (
                definition.config(),
                definition.config_files(),
                definition.memory_status.clone(),
            )
        };
        let skills = config.memory().skills;
        if let Err(err) = session.recorder.end(skills.is_some()) {
            status.set(format!("ending the session failed: {}", err));
        }
        if skills.is_some() {
            self.writer.enqueue(AgentJob {
                dir: config.dir(),
                skills,
                status,
            });
        }
        *session = self.new_session();

        // reset and reopened under one lock, so the ui never draws the gap
        let mut definition = self.definition.lock().expect("mutex error");
        let mut carried = std::mem::take(&mut definition.message_queue);
        carried.append(&mut definition.next_session_messages);
        definition.messages.clear();
        definition.usage = None;
        definition.opened_with_directive = false;
        definition.clear_pending = false;

        let mut working = match &config_files {
            Some(config_files) => begin_session(&mut definition, config_files),
            None => false,
        };
        for message in carried {
            definition.push_message(message);
            working = true;
        }
        definition.set_state(if working {
            AgentState::Working
        } else {
            AgentState::Idle
        });
        working
    }

    /// Loops while there are tool calls or queued messages to answer, up to
    /// `agent_max_iterations` requests. True when the agent went away mid turn
    /// and the thread is done.
    fn run_turns(&self, commands: &Receiver<AgentCommand>, session: &mut Session) -> bool {
        let settings = {
            let definition = self.definition.lock().expect("mutex error");
            run_settings(&definition)
        };

        let mut iteration: u32 = 0;

        loop {
            // the agent can be dropped while a turn is in flight, and what is
            // queued behind that turn then belongs to a conversation nobody
            // can read, so it is not worth another request
            if shutting_down(commands) {
                return true;
            }

            let request = {
                let mut definition = self.definition.lock().expect("mutex error");

                // checked before the queue is taken, so a message is not shown
                // as sent when this turn will never send it; the next turn
                // picks it up
                if iteration >= settings.max_iterations {
                    definition.set_state(AgentState::Failed(format!(
                        "the agent did not finish within {} iterations",
                        settings.max_iterations
                    )));
                    return false;
                }

                // must stay at the top of the loop: later, a queued message
                // could land between tool calls and their results, which
                // providers reject
                definition.take_queued_messages();
                definition.set_state(AgentState::Working);

                chat_request(&definition, &settings)
            };
            iteration += 1;
            session.recorder.sync(&self.definition);

            let result = self.openrouter.chat_completion(request);

            // the tool calls are cloned out from under the lock so the commands
            // they ask for run without the ui waiting on them
            let (turn, tool_calls) = {
                let mut definition = self.definition.lock().expect("mutex error");
                match result {
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
                                (classify(choice), choice.message.tool_calls().to_vec())
                            }
                            None => (
                                Turn::Failed("the model answered with no choices".into()),
                                Vec::new(),
                            ),
                        }
                    }
                }
            };

            session.recorder.sync(&self.definition);

            match turn {
                Turn::Failed(err) => {
                    self.definition
                        .lock()
                        .expect("mutex error")
                        .set_state(AgentState::Failed(err));
                    return false;
                }
                Turn::Done => {
                    if self.finish_turn() {
                        return false;
                    }
                    iteration = 0;
                }
                Turn::Continue => {
                    if !session.states.has_memory() {
                        if let Ok(store) = session.recorder.store() {
                            session.states.set_memory(store);
                        }
                    }
                    // no lock is held here: a tool call is a command running on
                    // this thread and can take as long as the command does,
                    // while the ui reads the definition every tick
                    let results: Vec<ToolResult> = tool_calls
                        .iter()
                        .map(|call| {
                            settings
                                .tools
                                .dispatch(&mut session.states, &settings.dir, call)
                        })
                        .collect();

                    // every call the model made is answered, in the order it
                    // made them, even when one of them ended the turn
                    let ends_turn = {
                        let mut definition = self.definition.lock().expect("mutex error");
                        for result in &results {
                            definition.push_message(result.message());
                        }
                        results.iter().any(|result| result.ends_turn())
                    };
                    session.recorder.sync(&self.definition);

                    if ends_turn {
                        if self.finish_turn() {
                            return false;
                        }
                        iteration = 0;
                    }
                }
            }
        }
    }

    /// The queue is checked under the lock that idles the agent, so no message
    /// sent now is lost. True when the agent went idle.
    fn finish_turn(&self) -> bool {
        let mut definition = self.definition.lock().expect("mutex error");
        if definition.queued_messages().is_empty() {
            definition.set_state(AgentState::Idle);
            return true;
        }
        false
    }
}

/// Doesn't block. A `Run` is dropped, since the state it refers to was written
/// before it was sent and the loop already has it.
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

/// What belongs to one session on the agent's thread: the tools' states and
/// the recorder writing it down. Replaced whole when the session is cleared.
struct Session {
    states: ToolStates,
    recorder: SessionRecorder,
}

/// Opens a session the way starting the agent does: the system prompt, then
/// `DIRECTIVE.md` when there is one. True when the directive was pushed.
fn begin_session(definition: &mut AgentDefinition, config_files: &ConfigFiles) -> bool {
    definition.set_system_prompt(build_system_prompt(config_files));
    match config_files.directive_file().contents() {
        Some(directive) => {
            definition.push_directive(directive.trim());
            true
        }
        None => false,
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

/// Read once before the loop starts, so a reload mid turn cannot leave one
/// request mixing old and new settings.
struct RunSettings {
    model: String,
    dir: PathBuf,
    max_iterations: u32,
    reasoning_effort: Option<ReasoningEffort>,
    tools: AgentTools,
}

fn run_settings(definition: &AgentDefinition) -> RunSettings {
    let config = definition.config();
    RunSettings {
        model: config.model().full_slug(),
        dir: config.dir(),
        max_iterations: config.max_iterations(),
        reasoning_effort: config.reasoning_effort(),
        tools: definition.tools(),
    }
}

/// The request for one iteration: the conversation so far, plus the tools the
/// agent may call. An agent with no tools at all leaves `tools` off the request
/// entirely — an empty `tools: []` is rejected by some providers.
fn chat_request(definition: &AgentDefinition, settings: &RunSettings) -> ChatCompletionRequest {
    let mut request =
        ChatCompletionRequest::new(settings.model.clone(), definition.request_messages());
    if let Some(effort) = settings.reasoning_effort {
        request = request.set_reasoning(Reasoning {
            effort: Some(effort),
        });
    }
    if settings.tools.is_empty() {
        return request;
    }
    request
        .set_tools(settings.tools.definitions())
        .set_tool_choice(ToolChoice::auto())
}

/// A snapshot of everything the ui needs to render an agent.
#[derive(Clone)]
pub struct AgentDefinition {
    id: String,
    config: AgentConfig,

    // state
    state: AgentState,
    config_files: Option<ConfigFiles>,
    /// Set and cleared with `config_files`, since the same file decides both.
    tools: AgentTools,
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
    /// Set by `/clear` until the agent's thread has started the new session.
    clear_pending: bool,
    /// What the user sent while a clear was pending; it opens the new session.
    next_session_messages: Vec<Message>,
    /// Shared with whatever works on the agent's memory, which updates it.
    memory_status: MemoryStatus,
}

impl Messages for AgentDefinition {
    fn messages(&self) -> &[Message] {
        &self.messages
    }
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
    /// The tools the agent may call. Empty until its files have been read.
    pub fn tools(&self) -> AgentTools {
        self.tools.clone()
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
    /// True while a `/clear` waits for the turn in flight to end.
    pub fn clear_pending(&self) -> bool {
        self.clear_pending
    }
    /// What will open the session after the pending clear.
    pub fn next_session_messages(&self) -> &[Message] {
        &self.next_session_messages
    }
    /// What is happening to the agent's memory, when there is anything to say.
    pub fn memory_status(&self) -> Option<String> {
        self.memory_status.get()
    }
    /// The handle the memory status is written through.
    pub fn memory_status_handle(&self) -> MemoryStatus {
        self.memory_status.clone()
    }

    pub fn set_config(&mut self, config: AgentConfig) {
        self.config = config;
    }
    pub fn set_state(&mut self, state: AgentState) {
        self.state = state;
    }
    /// The files the agent was configured from, and the tools they came to.
    /// They are set together because they are read together; nothing should be
    /// able to leave one describing a `config.json` the other does not.
    pub fn set_config_files(&mut self, config_files: ConfigFiles, tools: AgentTools) {
        self.config_files = Some(config_files);
        self.tools = tools;
        self.config_error = None;
    }
    /// Records that the agent has no usable configuration, dropping whatever
    /// was read before it: the files on disk no longer say what it holds.
    pub fn set_config_error(&mut self, error: ConfigFilesError) {
        self.config_files = None;
        self.tools = AgentTools::empty();
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
