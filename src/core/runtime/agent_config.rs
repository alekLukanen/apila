use std::{
    fs::{self, File},
    io::Read,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use thiserror::Error;

use crate::core::openrouter::types::{Reasoning, ReasoningEffort};

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
/// Tells the skill memory writer what to save and how to write it. Optional,
/// read only when skills are enabled. Looked up in the agent's directory first,
/// then in the project directory.
pub const SKILL_MEMORY_FILE_NAME: &str = "SKILL_MEMORY.md";

/// The largest `SKILL_MEMORY.md` an agent may have, since all of it goes into
/// every request the writer makes.
pub const MAX_SKILL_MEMORY_BYTES: usize = 16 * 1024;

/// The iteration bound of an agent that has not read its `config.json` yet.
/// Every started agent overwrites it from its own settings, which are required
/// to name one; this exists so an unconfigured config is never a trap.
pub const DEFAULT_MAX_ITERATIONS: u32 = 10;

/// How close a skill has to be to a request before `search_skills` returns it,
/// unless `memory.skills.min_similarity` says otherwise.
pub const DEFAULT_MIN_SIMILARITY: f64 = 0.25;

/// The tools that come with `memory.skills` rather than with `tools.enabled`.
pub const SKILL_TOOL_NAMES: [&str; 2] = ["search_skills", "get_skill"];

#[derive(Clone)]
pub struct AgentConfig {
    name: String,
    model: Model,
    dir: PathBuf,
    max_iterations: u32,
    reasoning_effort: Option<ReasoningEffort>,
    memory: MemorySettings,
}

impl AgentConfig {
    pub fn empty() -> AgentConfig {
        AgentConfig {
            name: "".into(),
            model: Model::empty(),
            dir: PathBuf::new(),
            max_iterations: DEFAULT_MAX_ITERATIONS,
            reasoning_effort: None,
            memory: MemorySettings::default(),
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
    /// Unset leaves the choice to the provider.
    pub fn reasoning_effort(&self) -> Option<ReasoningEffort> {
        self.reasoning_effort
    }
    pub fn memory(&self) -> MemorySettings {
        self.memory.clone()
    }
    /// The model with its effort, if any. A model that failed to load shows
    /// as unset alone, since the effort came from the same failed file.
    pub fn model_label(&self) -> String {
        match self.reasoning_effort {
            Some(effort) if self.model.valid() => {
                format!("{} ({})", self.model.label(), effort.as_str())
            }
            _ => self.model.label(),
        }
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
    pub fn set_reasoning_effort(mut self, effort: Option<ReasoningEffort>) -> AgentConfig {
        self.reasoning_effort = effort;
        self
    }
    pub fn set_memory(mut self, memory: MemorySettings) -> AgentConfig {
        self.memory = memory;
        self
    }

    /// Everything the agent's `config.json` decides, moved onto the config the
    /// runtime holds, so a setting added to the file is wired through in one
    /// place rather than at every call site that reloads an agent.
    pub fn set_from_config_files(self, config_files: &ConfigFiles) -> AgentConfig {
        self.set_model(config_files.model())
            .set_max_iterations(config_files.agent_max_iterations())
            .set_reasoning_effort(config_files.reasoning_effort())
            .set_memory(config_files.memory())
    }

    /// What the agent runs with when its `config.json` could not be read. The
    /// file that named the model and the bound is the one that just failed, so
    /// neither is kept.
    pub fn clear_settings(self) -> AgentConfig {
        self.set_model(Model::empty())
            .set_max_iterations(DEFAULT_MAX_ITERATIONS)
            .set_reasoning_effort(None)
            .set_memory(MemorySettings::default())
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

/// `agent_max_iterations` is required because an agent that loops without a
/// bound is worse than one that refuses to start. `tools` defaults to none.
#[derive(Debug, Clone, Deserialize)]
pub struct AgentSettings {
    /// The model the agent runs on. Ex: "openai/gpt-4o".
    pub model: String,

    /// The most requests the agent may make to the model in a single turn
    /// before it gives up. Every tool call the model makes costs one, so this
    /// is the ceiling on what one turn can cost.
    pub agent_max_iterations: u32,

    #[serde(default)]
    pub reasoning: Option<Reasoning>,

    #[serde(default)]
    pub tools: ToolSettings,

    #[serde(default)]
    pub memory: Option<MemoryBlock>,
}

/// The agent's `memory` block, as written. One field per kind of memory so a
/// new kind is a new block rather than new keys beside the old ones.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemoryBlock {
    #[serde(default)]
    pub skills: Option<SkillsBlock>,
}

/// `memory.skills` as written. Everything but `enabled` is optional here and
/// checked in [`MemorySettings::resolve`], since what is required depends on it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SkillsBlock {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub embedding_model: Option<String>,
    #[serde(default)]
    pub min_similarity: Option<f64>,
    #[serde(default)]
    pub writer_model: Option<String>,
    /// Signed so a negative count is reported as a bad setting rather than
    /// as json that does not parse.
    #[serde(default)]
    pub writer_max_iterations: Option<i64>,
}

/// The agent's `memory` block with every default filled in and every value
/// checked, worked out once when the files are read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MemorySettings {
    /// Present only when skills are enabled.
    pub skills: Option<SkillsSettings>,
}

/// What an agent that learns skills from its past sessions runs with.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillsSettings {
    pub embedding_model: String,
    pub min_similarity: f64,
    /// An "author/slug" id; the agent's own model unless one is named.
    pub writer_model: String,
    pub writer_max_iterations: u32,
    /// The text of `SKILL_MEMORY.md`, trimmed; unset when there is none, which
    /// leaves the writer on its own guidance.
    pub instructions: Option<String>,
}

impl MemorySettings {
    fn resolve(block: Option<&MemoryBlock>, model: &Model) -> Result<MemorySettings, String> {
        let Some(skills) = block.and_then(|block| block.skills.as_ref()) else {
            return Ok(MemorySettings::default());
        };
        if !skills.enabled {
            return Ok(MemorySettings::default());
        }

        let embedding_model = match &skills.embedding_model {
            Some(model) if model.trim() != "" => model.trim().to_string(),
            _ => return Err("`skills.embedding_model` is required when skills are enabled".into()),
        };

        let min_similarity = skills.min_similarity.unwrap_or(DEFAULT_MIN_SIMILARITY);
        if !(-1.0..=1.0).contains(&min_similarity) {
            return Err("`skills.min_similarity` must be between -1 and 1".into());
        }

        let writer_model = match &skills.writer_model {
            Some(writer_model) => Model::parse(writer_model)
                .ok_or_else(|| {
                    format!(
                        "`skills.writer_model` is not an \"author/slug\" id: {}",
                        writer_model
                    )
                })?
                .full_slug(),
            None => model.full_slug(),
        };

        let writer_max_iterations = match skills.writer_max_iterations {
            Some(count) if count >= 1 => u32::try_from(count)
                .map_err(|_| "`skills.writer_max_iterations` is too large".to_string())?,
            Some(_) => return Err("`skills.writer_max_iterations` must be at least 1".into()),
            None => {
                return Err(
                    "`skills.writer_max_iterations` is required when skills are enabled".into(),
                )
            }
        };

        Ok(MemorySettings {
            skills: Some(SkillsSettings {
                embedding_model,
                min_similarity,
                writer_model,
                writer_max_iterations,
                instructions: None,
            }),
        })
    }

    pub fn skills_enabled(&self) -> bool {
        self.skills.is_some()
    }
}

/// Each tool's settings stay raw json so registering a new tool never means
/// adding a field here.
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

    #[error("`memory` is not usable: {0}")]
    InvalidMemory(String),
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
    skill_memory_file: ConfigFile,

    /// The parsed `config.json`.
    settings: AgentSettings,
    /// The model named by `settings`, already split into author and slug.
    model: Model,
    memory: MemorySettings,
}

impl ConfigFiles {
    /// `SYSTEM.md` and `SKILL_MEMORY.md` may live in `project_dir` so agents can
    /// share them. A bad `config.json` or a `SKILL_MEMORY.md` that cannot be used
    /// is an error, not a gap shown beside the other files.
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
        let mut memory = MemorySettings::resolve(settings.memory.as_ref(), &model)
            .map_err(ConfigFilesError::InvalidMemory)?;
        reject_skill_tools(&settings.tools).map_err(ConfigFilesError::InvalidMemory)?;

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

        let local_skill_memory_path = dir.join(SKILL_MEMORY_FILE_NAME);
        let skill_memory_path = if local_skill_memory_path.is_file() {
            local_skill_memory_path
        } else {
            project_dir.join(SKILL_MEMORY_FILE_NAME)
        };
        let skill_memory_file = ConfigFile {
            name: SKILL_MEMORY_FILE_NAME.into(),
            present: skill_memory_path.is_file(),
            path: skill_memory_path,
            required: false,
        };
        if let Some(skills) = memory.skills.as_mut() {
            skills.instructions =
                read_skill_memory(&skill_memory_file).map_err(ConfigFilesError::InvalidMemory)?;
        }

        Ok(ConfigFiles {
            config_file,
            agents_file,
            system_file,
            directive_file,
            skill_memory_file,
            settings,
            model,
            memory,
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
    pub fn reasoning_effort(&self) -> Option<ReasoningEffort> {
        self.settings.reasoning.as_ref().and_then(|r| r.effort)
    }
    pub fn memory(&self) -> MemorySettings {
        self.memory.clone()
    }
    /// The agent's `tools` block, plus the skill tools when `memory.skills`
    /// turns them on. Working out which tools it comes to is the runtime's job,
    /// since only it holds the registry.
    pub fn tool_settings(&self) -> ToolSettings {
        let mut tools = self.settings.tools.clone();
        if let Some(skills) = &self.memory.skills {
            tools
                .enabled
                .extend(SKILL_TOOL_NAMES.iter().map(|name| name.to_string()));
            let mut settings = serde_json::Map::new();
            settings.insert(
                "embedding_model".into(),
                serde_json::Value::String(skills.embedding_model.clone()),
            );
            settings.insert(
                "min_similarity".into(),
                serde_json::json!(skills.min_similarity),
            );
            tools.configs.push(ToolConfig {
                tool: SKILL_TOOL_NAMES[0].into(),
                settings,
            });
        }
        tools
    }
    /// `SKILL_MEMORY.md` is listed only when skills are enabled, since it does
    /// nothing otherwise.
    pub fn files(&self) -> Vec<ConfigFile> {
        let mut files = vec![
            self.config_file.clone(),
            self.system_file.clone(),
            self.agents_file.clone(),
            self.directive_file.clone(),
        ];
        if self.memory.skills_enabled() {
            files.push(self.skill_memory_file.clone());
        }
        files
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

/// The skill tools are switched on and configured by `memory.skills` alone, so
/// a `tools` block naming them could only disagree with it.
fn reject_skill_tools(tools: &ToolSettings) -> Result<(), String> {
    let named = tools
        .enabled
        .iter()
        .chain(tools.configs.iter().map(|config| &config.tool))
        .find(|name| SKILL_TOOL_NAMES.contains(&name.as_str()));
    match named {
        Some(name) => Err(format!(
            "`{}` cannot be named in `tools`; skills are configured with `memory.skills`",
            name
        )),
        None => Ok(()),
    }
}

/// The writer's instructions out of `SKILL_MEMORY.md`. A file that is absent or
/// blank leaves none; one that cannot be read or is too large is an error.
fn read_skill_memory(file: &ConfigFile) -> Result<Option<String>, String> {
    if !file.present() {
        return Ok(None);
    }
    let unreadable = |err: String| {
        format!(
            "`{}` could not be read ({}): {}",
            SKILL_MEMORY_FILE_NAME,
            file.path().display(),
            err
        )
    };

    // one byte past the limit is enough to tell the file is too large
    let mut buf = Vec::new();
    File::open(file.path())
        .and_then(|f| {
            f.take(MAX_SKILL_MEMORY_BYTES as u64 + 1)
                .read_to_end(&mut buf)
        })
        .map_err(|err| unreadable(err.to_string()))?;
    if buf.len() > MAX_SKILL_MEMORY_BYTES {
        return Err(format!(
            "`{}` ({}) is larger than {} bytes",
            SKILL_MEMORY_FILE_NAME,
            file.path().display(),
            MAX_SKILL_MEMORY_BYTES
        ));
    }
    let text = String::from_utf8(buf).map_err(|err| unreadable(err.to_string()))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text).trim();
    if text.is_empty() {
        Ok(None)
    } else {
        Ok(Some(text.to_string()))
    }
}
