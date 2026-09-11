use std::any::Any;
use std::cell::Cell;
use std::panic::{self, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::Once;

use thiserror::Error;

/// Something an agent can be given to do beyond talking, and the means of
/// starting one for a particular agent.
///
/// Registered once for the whole runtime and shared across every agent thread,
/// so an implementation holds nothing that belongs to one agent — that lives in
/// the [`ToolState`] it hands out.
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

    /// Starts the tool for one agent: opens whatever it needs open and parses
    /// whatever it needs parsed, once. Called the first time that agent calls
    /// this tool, and never again for as long as the agent lives.
    ///
    /// A failure here is reported to the model like any other failed call, and
    /// is not remembered — a database that was down on one call can be up on
    /// the next, and an agent that could never retry would be stuck.
    fn new_state(&self, context: &ToolContext) -> Result<Box<dyn ToolState>, ToolError>;
}

/// What one tool holds for one agent, for as long as that agent runs.
///
/// It lives on the agent's own thread and is never shared, which is what makes
/// `&mut self` here reasonable: a tool mutates its own state directly instead
/// of locking, and whatever it opened is closed by its `Drop` when the agent's
/// thread ends.
pub trait ToolState: Send {
    /// Runs one call. `arguments` is the model's parsed json, normalised to an
    /// object by the caller, so a tool only looks up the keys it declared.
    ///
    /// Failing here is not the agent failing: the failure is turned into a tool
    /// message the model can read and correct from, which is why the error says
    /// what was wrong with the call.
    fn run(
        &mut self,
        context: &ToolContext,
        arguments: &serde_json::Value,
    ) -> Result<ToolOutput, ToolError>;
}

/// What a tool is told about the agent calling it: where to work, and the
/// tool's own entry from the agent's `tools.configs`. The same context is given
/// to `new_state` and to every `run` after it, so a tool can either read it each
/// call or keep what it needs at the moment it starts.
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

/// What a tool has to say back to the model, and whether it wants the agent's
/// turn to stop here.
///
/// The control signal rides on the output rather than the caller watching for a
/// tool named `end_turn`, so the agent loop never has to know which tools are
/// registered, and a later tool that also finishes a turn needs no change to
/// the loop.
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

/// Why a tool call could not be run, whether it failed to start for this agent
/// or failed on this one call. All of these are the model's mistake or the
/// environment's, and all of them come back to the model as a tool message
/// rather than stopping the agent.
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

    /// The tool was running and something went wrong that the call is not to
    /// blame for.
    #[error("the tool failed while running: {error}")]
    Failed { error: String },
}

// Panics ////////////////////////////
//////////////////////////////////////

thread_local! {
    /// Set while this thread is inside [`catching_panics`].
    static CATCHING_PANICS: Cell<bool> = const { Cell::new(false) };
}

/// True when this thread is running something whose panic is about to be
/// caught and turned into a result.
///
/// The terminal's panic hook reads this: a panic the program is not going to
/// die from must not tear down the alternate screen or print over it.
pub fn panics_are_being_caught() -> bool {
    CATCHING_PANICS.with(|catching| catching.get())
}

/// Runs `body`, turning a panic into an `Err` carrying whatever the panic said.
///
/// A tool is, as far as the agent loop is concerned, someone else's code. One
/// that panics would otherwise take the agent's thread with it, and the agent
/// would sit in `Working` forever with a tool call in its transcript that
/// nothing ever answered — so a panic is reported to the model the same way a
/// refused call is.
///
/// A tool that unwinds may be halfway through whatever it was doing, which is
/// why the caller drops its state rather than calling it again.
pub fn catching_panics<T>(body: impl FnOnce() -> T) -> Result<T, String> {
    quieten_caught_panics();

    CATCHING_PANICS.with(|catching| catching.set(true));
    let result = panic::catch_unwind(AssertUnwindSafe(body));
    CATCHING_PANICS.with(|catching| catching.set(false));
    result.map_err(panic_message)
}

static QUIETEN: Once = Once::new();

/// Stops a panic that is about to be caught from being reported as though the
/// program were going down.
///
/// Whatever hook is already in place is kept and deferred to for every other
/// panic, so this composes with the terminal's hook whichever of the two is
/// installed first. It is done here rather than left to the caller so that a
/// tool panicking is quiet in its own right — a library that prints a backtrace
/// over the thing that already handled the failure is a library that cannot be
/// used from a full screen ui.
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
