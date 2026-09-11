use std::collections::HashMap;
use std::fmt::Debug;
use std::path::Path;
use std::sync::Arc;

use thiserror::Error;

use crate::core::openrouter::types::{Message, ToolCall};
use crate::core::runtime::agent_config::ToolSettings;
use crate::core::tools::bash::BashTool;
use crate::core::tools::end_turn::EndTurnTool;
use crate::core::tools::tool::{catching_panics, Tool, ToolContext, ToolOutput, ToolState};

/// Why an agent's `tools` block could not be turned into a set of tools it can
/// call. Every one of these stops the agent being configured, the same way a
/// model id that is not an id does.
#[derive(Debug, Clone, Error)]
pub enum ToolSettingsError {
    #[error("`{name}` is not a tool. the tools you can enable are: {available}")]
    UnknownTool { name: String, available: String },

    #[error("more than one config for `{0}`")]
    DuplicateToolConfig(String),

    #[error("there is a config for `{name}`, which is not a tool. the tools are: {available}")]
    UnknownToolConfig { name: String, available: String },

    #[error("the config for `{tool}` is not usable: {error}")]
    InvalidToolConfig { tool: String, error: String },
}

/// Every tool this runtime knows how to run, in the order it was registered —
/// which is the order the model sees them in, so it is kept stable rather than
/// left to a map's iteration order.
///
/// One registry is built by the runtime and shared by every agent behind an
/// `Arc`. It can be shared because a registered tool holds no per agent state:
/// what an agent accumulates lives in the [`ToolStates`] its own thread owns.
pub struct ToolRegistry {
    tools: Vec<Arc<dyn Tool>>,
}

impl Debug for ToolRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ToolRegistry<...>")
    }
}

impl ToolRegistry {
    pub fn new() -> ToolRegistry {
        ToolRegistry { tools: Vec::new() }
    }

    /// The tools every runtime starts with. `end_turn` comes first because it
    /// is the one call that is always there.
    pub fn with_default_tools() -> ToolRegistry {
        ToolRegistry::new()
            .register(Arc::new(EndTurnTool::new()))
            .register(Arc::new(BashTool::new()))
    }

    /// Adds a tool, replacing one already registered under the same name so a
    /// caller can put its own in place of a built in.
    pub fn register(mut self, tool: Arc<dyn Tool>) -> ToolRegistry {
        let name = tool.name();
        self.tools.retain(|existing| existing.name() != name);
        self.tools.push(tool);
        self
    }

    pub fn tool(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools
            .iter()
            .find(|tool| tool.name() == name)
            .map(Arc::clone)
    }

    /// Every registered tool's name.
    pub fn names(&self) -> Vec<String> {
        self.tools.iter().map(|tool| tool.name()).collect()
    }

    /// Every name a `config.json` may put in `enabled`, which is every
    /// registered tool that is not already on for everyone.
    pub fn enableable_names(&self) -> Vec<String> {
        self.tools
            .iter()
            .filter(|tool| !tool.always_enabled())
            .map(|tool| tool.name())
            .collect()
    }

    /// Works out which tools an agent gets, and checks each one's settings.
    ///
    /// Called once, when the agent's files are read: an unknown name or a bad
    /// setting shows up on the configuration screen rather than mid turn, and
    /// the agent loop is handed the answer rather than working it out again on
    /// every request.
    pub fn resolve(&self, settings: &ToolSettings) -> Result<AgentTools, ToolSettingsError> {
        let mut configs: HashMap<String, serde_json::Value> = HashMap::new();
        for config in &settings.configs {
            // a config naming a tool this runtime has never heard of is a typo,
            // not a setting waiting to be switched on. left alone it does
            // nothing at all, quietly, which is the worst of both
            let tool = self
                .tool(&config.tool)
                .ok_or_else(|| ToolSettingsError::UnknownToolConfig {
                    name: config.tool.clone(),
                    available: self.names().join(", "),
                })?;

            let settings_value = config.settings_value();
            // checked whether or not the tool is enabled, so switching one on
            // later cannot turn up settings that were wrong all along
            tool.validate_config(&settings_value).map_err(|error| {
                ToolSettingsError::InvalidToolConfig {
                    tool: config.tool.clone(),
                    error,
                }
            })?;

            if configs.insert(config.tool.clone(), settings_value).is_some() {
                return Err(ToolSettingsError::DuplicateToolConfig(config.tool.clone()));
            }
        }

        // the tools that are part of how a turn works come first and come
        // whether or not the agent asked for them
        let mut chosen: Vec<Arc<dyn Tool>> = self
            .tools
            .iter()
            .filter(|tool| tool.always_enabled())
            .map(Arc::clone)
            .collect();

        for name in &settings.enabled {
            // naming one of the always on tools is harmless rather than an
            // error, and must not offer it to the model twice
            if chosen.iter().any(|tool| tool.name() == *name) {
                continue;
            }
            let tool = self
                .tool(name)
                .ok_or_else(|| ToolSettingsError::UnknownTool {
                    name: name.clone(),
                    available: self.enableable_names().join(", "),
                })?;
            chosen.push(tool);
        }

        let mut tools = Vec::with_capacity(chosen.len());
        for tool in chosen {
            // a tool the agent enabled without settings of its own still has to
            // be happy with having none
            let config = configs
                .get(&tool.name())
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            tool.validate_config(&config)
                .map_err(|error| ToolSettingsError::InvalidToolConfig {
                    tool: tool.name(),
                    error,
                })?;
            tools.push((tool, config));
        }

        Ok(AgentTools { tools })
    }
}

/// The tools one agent may call, already looked up and already paired with that
/// agent's settings for each. Worked out once when the agent's files are read
/// and held from then on, so nothing on the hot path re-reads a `config.json`
/// or searches the registry. Cheap to clone: an `Arc` and a small json value
/// per tool.
#[derive(Clone)]
pub struct AgentTools {
    tools: Vec<(Arc<dyn Tool>, serde_json::Value)>,
}

impl Debug for AgentTools {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("AgentTools<...>")
    }
}

impl AgentTools {
    /// An agent with nothing to call, which is what an agent whose files have
    /// not been read yet has.
    pub fn empty() -> AgentTools {
        AgentTools { tools: Vec::new() }
    }

    pub fn names(&self) -> Vec<String> {
        self.tools.iter().map(|(tool, _)| tool.name()).collect()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// The tools as the api describes them, for the request's `tools` field.
    pub fn definitions(&self) -> Vec<crate::core::openrouter::types::Tool> {
        self.tools
            .iter()
            .map(|(tool, _)| {
                crate::core::openrouter::types::Tool::function(
                    tool.name(),
                    tool.description(),
                    tool.parameters(),
                )
            })
            .collect()
    }

    /// Runs one call the model asked for, in `dir`, against the state this
    /// agent keeps for the tool — starting that state if this is the first time
    /// the agent has reached for it.
    ///
    /// Every way this can go wrong — a tool that isn't enabled, arguments that
    /// aren't json, a tool that refuses the call — comes back as a tool message
    /// rather than as an error, because the model is the one that can fix any
    /// of them and it can only read what it is sent. A bad call costs an
    /// iteration, not the turn.
    pub fn dispatch(&self, states: &mut ToolStates, dir: &Path, call: &ToolCall) -> ToolResult {
        let Some((tool, config)) = self
            .tools
            .iter()
            .find(|(tool, _)| tool.name() == call.function.name)
        else {
            // naming the tools that do exist is what lets the model recover on
            // the next iteration rather than guessing again
            return ToolResult::error(
                call,
                format!(
                    "there is no tool named `{}`. the tools you can call are: {}",
                    call.function.name,
                    self.names().join(", ")
                ),
            );
        };

        let arguments = match parse_arguments(&call.function.arguments) {
            Ok(arguments) => arguments,
            Err(err) => return ToolResult::error(call, err),
        };

        let context = ToolContext::new(dir.to_path_buf(), config.clone());
        match states.run(tool, &context, &arguments) {
            Ok(output) => ToolResult::new(call, output),
            Err(err) => ToolResult::error(call, err),
        }
    }
}

/// Reads the arguments the model sent, which arrive as a json document encoded
/// in a string rather than as json.
///
/// A call that takes no arguments regularly arrives as `""` or as `null`, and
/// neither is an error worth making the model recover from, so both read as an
/// empty object.
fn parse_arguments(raw: &str) -> Result<serde_json::Value, String> {
    if raw.trim() == "" {
        return Ok(serde_json::json!({}));
    }
    let value: serde_json::Value = serde_json::from_str(raw)
        .map_err(|err| format!("arguments were not valid json: {}", err))?;
    match value {
        serde_json::Value::Null => Ok(serde_json::json!({})),
        value @ serde_json::Value::Object(_) => Ok(value),
        _ => Err("arguments must be a json object".to_string()),
    }
}

/// What each tool holds for one agent.
///
/// Kept apart from [`AgentTools`] on purpose: `AgentTools` is immutable and
/// cloned freely, while this is a single mutable thing owned by the agent's own
/// thread and never shared — so a state needs no lock, and closes whatever it
/// opened when that thread ends.
///
/// A state is created the first time the agent calls its tool rather than when
/// the agent starts, so a tool that opens a connection does not open one for an
/// agent that never gets round to using it.
///
/// Nothing rebuilds a state once it exists, which means whatever it read out of
/// its config when it started is what it runs on for the rest of the agent's
/// life. That is not a staleness problem: an agent's `config.json` is read once,
/// when its files are loaded, and `Runtime::reload_config_files` refuses a
/// started agent for exactly this reason.
pub struct ToolStates {
    states: HashMap<String, Box<dyn ToolState>>,
}

impl Debug for ToolStates {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ToolStates<...>")
    }
}

impl ToolStates {
    pub fn new() -> ToolStates {
        ToolStates {
            states: HashMap::new(),
        }
    }

    /// Runs one call against the state this agent keeps for `tool`, starting
    /// that state if this is the first time the agent has reached for it.
    ///
    /// Returns the reason as text rather than a `ToolError`, because a panic is
    /// one of the things that can come back and it is not one.
    ///
    /// A failure to start is not remembered: the next call tries again, so a
    /// connection that was refused once does not leave the tool broken for the
    /// rest of the agent's life. Neither is a state that panicked kept — it may
    /// be halfway through whatever it was doing, so it is dropped and the next
    /// call starts a fresh one.
    fn run(
        &mut self,
        tool: &Arc<dyn Tool>,
        context: &ToolContext,
        arguments: &serde_json::Value,
    ) -> Result<ToolOutput, String> {
        let name = tool.name();

        if !self.states.contains_key(&name) {
            let started = catching_panics(|| tool.new_state(context))
                .map_err(|panic| format!("the tool panicked while starting: {}", panic))?;
            let state = started.map_err(|err| err.to_string())?;
            self.states.insert(name.clone(), state);
        }

        let state = self.states.get_mut(&name).expect("just inserted");
        match catching_panics(|| state.run(context, arguments)) {
            Ok(Ok(output)) => Ok(output),
            Ok(Err(err)) => Err(err.to_string()),
            Err(panic) => {
                self.states.remove(&name);
                Err(format!("the tool panicked: {}", panic))
            }
        }
    }

    /// The tools this agent has actually reached for so far.
    pub fn started(&self) -> Vec<String> {
        let mut names: Vec<String> = self.states.keys().cloned().collect();
        names.sort();
        names
    }
}

/// What running one tool call came to: the message that answers it, and whether
/// the tool asked for the turn to end.
#[derive(Debug, Clone)]
pub struct ToolResult {
    message: Message,
    ends_turn: bool,
}

impl ToolResult {
    fn new(call: &ToolCall, output: crate::core::tools::tool::ToolOutput) -> ToolResult {
        ToolResult {
            message: Message::tool(call.id.clone(), content_or_placeholder(output.content())),
            ends_turn: output.ends_turn(),
        }
    }

    /// A call that could not be run, said in a way the model can act on. It
    /// never ends the turn: the agent is meant to try again.
    fn error(call: &ToolCall, error: impl std::fmt::Display) -> ToolResult {
        ToolResult {
            message: Message::tool(call.id.clone(), format!("error: {}", error)),
            ends_turn: false,
        }
    }

    pub fn message(&self) -> Message {
        self.message.clone()
    }

    pub fn ends_turn(&self) -> bool {
        self.ends_turn
    }
}

/// Some providers reject a tool message with nothing in it, so a tool that
/// said nothing says so.
fn content_or_placeholder(content: String) -> String {
    if content.trim() == "" {
        "(no output)".to_string()
    } else {
        content
    }
}
