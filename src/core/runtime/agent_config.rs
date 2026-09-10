use std::{
    fs,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use thiserror::Error;

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

/// The iteration bound of an agent that has not read its `config.json` yet.
/// Every started agent overwrites it from its own settings, which are required
/// to name one; this exists so an unconfigured config is never a trap.
pub const DEFAULT_MAX_ITERATIONS: u32 = 10;

#[derive(Clone)]
pub struct AgentConfig {
    name: String,
    model: Model,
    dir: PathBuf,
    max_iterations: u32,
}

impl AgentConfig {
    pub fn empty() -> AgentConfig {
        AgentConfig {
            name: "".into(),
            model: Model::empty(),
            dir: PathBuf::new(),
            max_iterations: DEFAULT_MAX_ITERATIONS,
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
    /// The most requests the agent may make to the model in a single turn.
    pub fn max_iterations(&self) -> u32 {
        self.max_iterations
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
    pub fn set_max_iterations(mut self, max_iterations: u32) -> AgentConfig {
        self.max_iterations = max_iterations;
        self
    }

    /// Everything the agent's `config.json` decides, moved onto the config the
    /// runtime holds, so a setting added to the file is wired through in one
    /// place rather than at every call site that reloads an agent.
    pub fn set_from_config_files(self, config_files: &ConfigFiles) -> AgentConfig {
        self.set_model(config_files.model())
            .set_max_iterations(config_files.agent_max_iterations())
    }

    /// What the agent runs with when its `config.json` could not be read. The
    /// file that named the model and the bound is the one that just failed, so
    /// neither is kept.
    pub fn clear_settings(self) -> AgentConfig {
        self.set_model(Model::empty())
            .set_max_iterations(DEFAULT_MAX_ITERATIONS)
    }
}

#[derive(Debug, Clone)]
pub struct Model {
    pub author: String, // ex: openai
    pub slug: String,   // ex: gpt-4o
}

impl Model {
    /// The model of an agent that has none yet, either because it has not
    /// been configured or because its `config.json` could not be read.
    pub fn empty() -> Model {
        Model {
            author: "".into(),
            slug: "".into(),
        }
    }
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
    /// How the model reads on screen. An agent whose `config.json` failed to
    /// load has no model at all, which says more than an empty "/".
    pub fn label(&self) -> String {
        if self.valid() {
            self.full_slug()
        } else {
            "(unset)".to_string()
        }
    }
    pub fn full_slug(&self) -> String {
        format!("{}/{}", self.author, self.slug)
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

/// The contents of an agent's `config.json`.
///
/// `model` and `agent_max_iterations` are required: neither has a project wide
/// default, and an agent that loops without a bound is worse than one that
/// refuses to start. `tools` defaults to nothing, so an agent has to ask before
/// it can run commands.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentSettings {
    /// The model the agent runs on. Ex: "openai/gpt-4o".
    pub model: String,

    /// The most requests the agent may make to the model in a single turn
    /// before it gives up. Every tool call the model makes costs one, so this
    /// is the ceiling on what one turn can cost.
    pub agent_max_iterations: u32,

    #[serde(default)]
    pub tools: ToolSettings,
}

/// Which tools an agent may call and how each one is set up.
///
/// Kept as its own block, and each tool's settings kept as its own json, so
/// registering a new tool never means adding a field here.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolSettings {
    /// The tools the agent may call, named as the tool names itself. Tools that
    /// are part of how a turn works — `end_turn` — are on regardless and need
    /// not be listed.
    #[serde(default)]
    pub enabled: Vec<String>,

    /// Per tool settings. Each entry names its tool and carries whatever that
    /// tool understands; nothing here reads the settings themselves, which is
    /// what lets a tool add one without this file changing.
    #[serde(default)]
    pub configs: Vec<ToolConfig>,
}

/// One tool's settings, out of an agent's `tools.configs`.
#[derive(Debug, Clone, Deserialize)]
pub struct ToolConfig {
    /// The tool these settings belong to.
    pub tool: String,

    /// Everything written alongside it, handed to that tool untouched.
    #[serde(flatten)]
    pub settings: serde_json::Map<String, serde_json::Value>,
}

impl ToolConfig {
    /// The settings as the tool reads them: a json object, never anything else.
    pub fn settings_value(&self) -> serde_json::Value {
        serde_json::Value::Object(self.settings.clone())
    }
}

/// Why an agent's `config.json` could not be turned into settings. Every one
/// of these stops the agent being configured at all, since the file says
/// which model it runs on.
#[derive(Debug, Clone, Error)]
pub enum ConfigFilesError {
    #[error("`{name}` not found: {path}")]
    Missing { name: String, path: PathBuf },

    #[error("`{name}` could not be read: {error}")]
    Unreadable {
        name: String,
        path: PathBuf,
        error: String,
    },

    #[error("`model` is not an \"author/slug\" id: {0}")]
    InvalidModel(String),

    #[error("`agent_max_iterations` must be at least 1")]
    InvalidMaxIterations,

    /// Why the agent's `tools` block could not be turned into tools it can
    /// call. Carried as text because working it out needs the runtime's
    /// registry, which this file knows nothing about.
    #[error("`tools` is not usable: {0}")]
    InvalidTools(String),
}

/// The files an agent is configured from, and whether each one was found.
/// `config.json` is not among the maybes: it is read before the rest, and
/// without it there are no `ConfigFiles` at all.
#[derive(Debug, Clone)]
pub struct ConfigFiles {
    config_file: ConfigFile,
    agents_file: ConfigFile,
    system_file: ConfigFile,
    directive_file: ConfigFile,

    /// The parsed `config.json`.
    settings: AgentSettings,
    /// The model named by `settings`, already split into author and slug.
    model: Model,
}

impl ConfigFiles {
    /// Reads `config.json` out of `dir`, then looks for `AGENTS.md`,
    /// `SYSTEM.md` and the optional `DIRECTIVE.md` alongside it. The system
    /// prompt is allowed to live in `project_dir` instead, so a single one can
    /// be shared by every agent in the project.
    ///
    /// `config.json` is required: it names the model the agent runs on, and
    /// there is no project wide default to fall back on, so a missing,
    /// unparseable or modelless one is an error rather than a gap the user
    /// can be shown alongside the other files.
    pub fn load(dir: &Path, project_dir: &Path) -> Result<ConfigFiles, ConfigFilesError> {
        let config_path = dir.join(AGENT_CONFIG_FILE_NAME);
        let config_file = ConfigFile {
            name: AGENT_CONFIG_FILE_NAME.into(),
            present: config_path.is_file(),
            path: config_path.clone(),
            required: true,
        };

        let raw = config_file
            .contents()
            .ok_or_else(|| ConfigFilesError::Missing {
                name: AGENT_CONFIG_FILE_NAME.into(),
                path: config_path.clone(),
            })?;
        let settings: AgentSettings =
            serde_json::from_str(&raw).map_err(|err| ConfigFilesError::Unreadable {
                name: AGENT_CONFIG_FILE_NAME.into(),
                path: config_path,
                error: err.to_string(),
            })?;
        let model = Model::parse(&settings.model)
            .ok_or_else(|| ConfigFilesError::InvalidModel(settings.model.clone()))?;
        // a turn allowed no iterations would end before it began
        if settings.agent_max_iterations == 0 {
            return Err(ConfigFilesError::InvalidMaxIterations);
        }

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

        Ok(ConfigFiles {
            config_file,
            agents_file,
            system_file,
            directive_file,
            settings,
            model,
        })
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
    /// The agent's own settings, read out of `config.json`.
    pub fn settings(&self) -> AgentSettings {
        self.settings.clone()
    }
    /// The model the agent runs on, as named by its `config.json`.
    pub fn model(&self) -> Model {
        self.model.clone()
    }
    /// The most requests the agent may make to the model in a single turn.
    pub fn agent_max_iterations(&self) -> u32 {
        self.settings.agent_max_iterations
    }
    /// The agent's `tools` block, as written. Working out which tools it comes
    /// to is the runtime's job, since only it holds the registry.
    pub fn tool_settings(&self) -> ToolSettings {
        self.settings.tools.clone()
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
    /// Every required file was found, so the agent can be run. `config.json`
    /// is already accounted for: these only exist because it loaded.
    pub fn complete(&self) -> bool {
        self.missing_file().is_none()
    }
}
