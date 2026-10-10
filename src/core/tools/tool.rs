use std::any::Any;
use std::cell::Cell;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::Once;

use thiserror::Error;

/// Shared by every agent thread, so it holds nothing that belongs to one
/// agent; that lives in the [`ToolState`] it hands out.
pub trait Tool: Send + Sync {
    /// How the model names the tool when it calls it, and the name the user
    /// writes in `enabled`. Limited to 64 characters of `[A-Za-z0-9_-]` by the
    /// api, which `ChatCompletionRequest::validate` already checks.
    fn name(&self) -> String;

    /// The only documentation the model gets, so it says when to reach for the
    /// tool rather than only what the tool does.
    fn description(&self) -> String;

    /// A json schema object describing the call's arguments.
    fn parameters(&self) -> serde_json::Value;

    /// Whether every agent gets this tool whether or not its `config.json`
    /// asked for it. True only for the tools that are part of how a turn
    /// works, rather than things the agent can go and do.
    fn always_enabled(&self) -> bool {
        false
    }

    /// Checks the tool's own block out of `configs` before any agent runs, so
    /// a typo shows up on the agent's configuration screen instead of halfway
    /// through a turn. `config` is an object, empty when the user wrote none.
    fn validate_config(&self, config: &serde_json::Value) -> Result<(), String> {
        let _ = config;
        Ok(())
    }

    /// Called on an agent's first call to this tool. A failure is reported to
    /// the model and not remembered, so the next call tries again.
    fn new_state(&self, context: &ToolContext) -> Result<Box<dyn ToolState>, ToolError>;
}

/// One tool's state for one agent. It never leaves the agent's thread, so it
/// is mutated without locking and closed by its `Drop` when the thread ends.
pub trait ToolState: Send {
    /// `arguments` is always a json object. An error goes back to the model as
    /// a tool message, so it should say what to change about the call.
    fn run(
        &mut self,
        context: &ToolContext,
        arguments: &serde_json::Value,
    ) -> Result<ToolOutput, ToolError>;
}

/// The same context is given to `new_state` and every `run` after it, so a
/// tool can read it each call or keep what it needs when it starts.
#[derive(Debug, Clone)]
pub struct ToolContext {
    dir: PathBuf,
    config: serde_json::Value,
}

impl ToolContext {
    pub fn new(dir: PathBuf, config: serde_json::Value) -> ToolContext {
        ToolContext { dir, config }
    }
    /// The agent's own directory. Everything a tool touches on disk is relative
    /// to it; an agent never works in another agent's directory.
    pub fn dir(&self) -> PathBuf {
        self.dir.clone()
    }
    /// This tool's block out of the agent's `tools.configs`, an empty object
    /// when the user wrote none. A tool deserializes its own settings struct
    /// out of it rather than reading keys one at a time.
    pub fn config(&self) -> serde_json::Value {
        self.config.clone()
    }
    pub fn set_dir(mut self, dir: PathBuf) -> ToolContext {
        self.dir = dir;
        self
    }
    pub fn set_config(mut self, config: serde_json::Value) -> ToolContext {
        self.config = config;
        self
    }
}

/// Ending the turn rides on the output rather than the loop watching for
/// `end_turn`, so the loop never needs to know which tools exist.
#[derive(Debug, Clone)]
pub struct ToolOutput {
    content: String,
    ends_turn: bool,
}

impl ToolOutput {
    pub fn new(content: impl Into<String>) -> ToolOutput {
        ToolOutput {
            content: content.into(),
            ends_turn: false,
        }
    }
    pub fn content(&self) -> String {
        self.content.clone()
    }
    /// True when the tool has declared the agent's work finished. The loop
    /// still answers every outstanding tool call before it stops.
    pub fn ends_turn(&self) -> bool {
        self.ends_turn
    }
    pub fn set_ends_turn(mut self, ends_turn: bool) -> ToolOutput {
        self.ends_turn = ends_turn;
        self
    }
}

/// Every variant goes back to the model as a tool message; none of them stops
/// the agent.
#[derive(Debug, Clone, Error)]
pub enum ToolError {
    #[error("`{argument}` is required")]
    MissingArgument { argument: String },

    #[error("`{argument}` must be {expected}")]
    InvalidArgument { argument: String, expected: String },

    /// The tool could not start for this agent at all — bad settings, or
    /// something it needed open that would not open.
    #[error("the tool could not be started: {error}")]
    NotStarted { error: String },

    #[error("the tool's config is not usable: {error}")]
    InvalidConfig { error: String },

    /// The tool understood the call and ran it, and what it asked for could not
    /// be done — sql that does not parse, a table that does not exist. The
    /// call is to blame, and the message says what to change.
    #[error("the call was rejected: {error}")]
    Rejected { error: String },

    /// The tool was running and something went wrong that the call is not to
    /// blame for.
    #[error("the tool failed while running: {error}")]
    Failed { error: String },
}

// Arguments /////////////////////////
//////////////////////////////////////

/// A string argument the call must send. A blank one is refused too, since no
/// tool can do anything with it.
pub fn required_string<'a>(
    arguments: &'a serde_json::Value,
    argument: &str,
) -> Result<&'a str, ToolError> {
    let value = optional_string(arguments, argument)?.ok_or_else(|| {
        ToolError::MissingArgument {
            argument: argument.into(),
        }
    })?;
    if value.trim().is_empty() {
        return Err(ToolError::InvalidArgument {
            argument: argument.into(),
            expected: "not empty".into(),
        });
    }
    Ok(value)
}

/// A string argument the call may leave out; `null` counts as left out.
pub fn optional_string<'a>(
    arguments: &'a serde_json::Value,
    argument: &str,
) -> Result<Option<&'a str>, ToolError> {
    match arguments.get(argument) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(Some)
            .ok_or_else(|| ToolError::InvalidArgument {
                argument: argument.into(),
                expected: "a string".into(),
            }),
    }
}

// Panics ////////////////////////////
//////////////////////////////////////

thread_local! {
    /// Set while this thread is inside [`catching_panics`].
    static CATCHING_PANICS: Cell<bool> = const { Cell::new(false) };
}

/// Read by the terminal's panic hook, so a panic that will be caught does not
/// tear down the alternate screen or print over it.
pub fn panics_are_being_caught() -> bool {
    CATCHING_PANICS.with(|catching| catching.get())
}

/// Keeps a panicking tool from taking the agent's thread with it and leaving
/// its call unanswered. The tool may be left half done, so the caller should
/// drop its state.
pub fn catching_panics<T>(body: impl FnOnce() -> T) -> Result<T, String> {
    quieten_caught_panics();

    CATCHING_PANICS.with(|catching| catching.set(true));
    let result = panic::catch_unwind(AssertUnwindSafe(body));
    CATCHING_PANICS.with(|catching| catching.set(false));
    result.map_err(panic_message)
}

static QUIETEN: Once = Once::new();

/// Silences panics that are about to be caught. Other panics go to the hook
/// already in place, so this works whether it or the terminal's hook is
/// installed first.
fn quieten_caught_panics() {
    QUIETEN.call_once(|| {
        let hook = panic::take_hook();
        panic::set_hook(Box::new(move |panic_info| {
            if panics_are_being_caught() {
                return;
            }
            hook(panic_info);
        }));
    });
}

/// Whatever a panic said, when it said anything this can read.
fn panic_message(payload: Box<dyn Any + Send>) -> String {
    if let Some(message) = payload.downcast_ref::<&str>() {
        return (*message).to_string();
    }
    if let Some(message) = payload.downcast_ref::<String>() {
        return message.clone();
    }
    "no message".to_string()
}
