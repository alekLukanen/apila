use std::collections::HashMap;
use std::fmt::Debug;
use std::path::Path;
use std::sync::Arc;

use thiserror::Error;

use crate::core::memory::store::MemoryStore;
use crate::core::openrouter::client::OpenRouter;
use crate::core::openrouter::types::{Message, ToolCall};
use crate::core::runtime::agent_config::{ToolSettings, SKILL_TOOL_NAMES};
use crate::core::tools::bash::BashTool;
use crate::core::tools::end_turn::EndTurnTool;
use crate::core::tools::skills::{GetSkillTool, SearchSkillsTool};
use crate::core::tools::sqlite::SqliteTool;
use crate::core::tools::tool::{catching_panics, Tool, ToolContext, ToolOutput, ToolState};
use crate::core::tools::webpage::{FetchWebpageTool, ReadWebpageDataTool};

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

/// Registration order is the order the model sees the tools in, so it is a
/// `Vec` rather than a map. Shared by every agent; per agent state lives in
/// [`ToolStates`].
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
            .register(Arc::new(SqliteTool::new()))
            .register(Arc::new(FetchWebpageTool::new()))
            .register(Arc::new(ReadWebpageDataTool::new()))
            .register(Arc::new(SearchSkillsTool::new()))
            .register(Arc::new(GetSkillTool::new()))
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
    /// registered tool that is not already on for everyone or switched on by
    /// `memory.skills`.
    pub fn enableable_names(&self) -> Vec<String> {
        self.tools
            .iter()
            .filter(|tool| !tool.always_enabled())
            .filter(|tool| !SKILL_TOOL_NAMES.contains(&tool.name().as_str()))
            .map(|tool| tool.name())
            .collect()
    }

    /// Called when the agent's files are read, so an unknown name or bad
    /// setting shows on the configuration screen rather than mid turn.
    pub fn resolve(&self, settings: &ToolSettings) -> Result<AgentTools, ToolSettingsError> {
        let mut configs: HashMap<String, serde_json::Value> = HashMap::new();
        for config in &settings.configs {
            // a config naming a tool this runtime has never heard of is a typo,
            // not a setting waiting to be switched on. left alone it does
            // nothing at all, quietly, which is the worst of both
            let tool =
                self.tool(&config.tool)
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

            if configs
                .insert(config.tool.clone(), settings_value)
                .is_some()
            {
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
            tool.validate_config(&config).map_err(|error| {
                ToolSettingsError::InvalidToolConfig {
                    tool: tool.name(),
                    error,
                }
            })?;
            tools.push((tool, config));
        }

        Ok(AgentTools { tools })
    }
}

/// Each tool paired with this agent's settings for it, resolved once so no
/// request re-reads a `config.json` or searches the registry.
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

    /// Every failure comes back as a tool message rather than an error, since
    /// the model is the one that can fix it. A bad call costs an iteration, not
    /// the turn.
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

        let context = ToolContext::new(dir.to_path_buf(), config.clone())
            .set_openrouter(states.openrouter.clone())
            .set_memory(states.memory.clone());
        match states.run(tool, &context, &arguments) {
            Ok(output) => ToolResult::new(call, output),
            Err(err) => ToolResult::error(call, err),
        }
    }
}

/// Calls with no arguments often arrive as `""` or `null`; both read as an
/// empty object rather than an error.
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

/// Owned by the agent's thread, unlike the freely cloned [`AgentTools`], so no
/// state needs a lock. Each is created on the tool's first call and never
/// rebuilt, which is why a started agent's config cannot be reloaded.
pub struct ToolStates {
    states: HashMap<String, Box<dyn ToolState>>,
    /// Handed to every call, for the tools that reach a model or the agent's
    /// memory themselves.
    openrouter: Option<Arc<OpenRouter>>,
    memory: Option<Arc<MemoryStore>>,
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
            openrouter: None,
            memory: None,
        }
    }

    pub fn set_openrouter(&mut self, openrouter: Arc<OpenRouter>) {
        self.openrouter = Some(openrouter);
    }

    pub fn set_memory(&mut self, memory: Arc<MemoryStore>) {
        self.memory = Some(memory);
    }

    pub fn has_memory(&self) -> bool {
        self.memory.is_some()
    }

    /// Errors are text because a panic can be one. A state that failed to
    /// start or panicked is not kept, so the next call starts a fresh one.
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
