use std::path::PathBuf;

pub struct Agent {
    agent: AgentDefinition,

    // state
    messages: Vec<AgentMessage>,
}

impl Agent {
    pub fn agent_definition(&self) -> AgentDefinition {
        self.agent.clone()
    }
}

impl AgentDefinition {
    pub fn id(&self) -> String {
        self.id.clone()
    }
    pub fn config(&self) -> AgentConfig {
        self.config.clone()
    }
}

#[derive(Clone)]
pub struct AgentDefinition {
    id: String,
    config: AgentConfig,

    // state
    memory_file: PathBuf,
}

#[derive(Clone)]
pub struct AgentConfig {
    name: String,
    model: Model,
    initial_directive: String,
}

impl AgentConfig {
    pub fn empty() -> AgentConfig {
        AgentConfig {
            name: "".into(),
            model: Model {
                author: "".into(),
                slug: "".into(),
            },
            initial_directive: "".into(),
        }
    }
    pub fn name(&self) -> String {
        self.name.clone()
    }
    pub fn model(&self) -> Model {
        self.model.clone()
    }
    pub fn initial_directive(&self) -> String {
        self.initial_directive.clone()
    }
    pub fn set_name(mut self, name: String) -> AgentConfig {
        self.name = name;
        self
    }
    pub fn set_model(mut self, model: Model) -> AgentConfig {
        self.model = model;
        self
    }
    pub fn set_initial_directive(mut self, directive: String) -> AgentConfig {
        self.initial_directive = directive;
        self
    }
}

pub enum AgentMessageRole {
    User,
}

pub struct AgentMessage {
    role: AgentMessageRole,
    content: String,
}

#[derive(Clone)]
pub struct Model {
    pub author: String, // ex: openai
    pub slug: String,   // ex: gpt-4o
}

impl Model {
    pub fn valid(&self) -> bool {
        self.author != "" && self.slug != ""
    }
    pub fn full_slug(&self) -> String {
        format!("{}/{}", self.author, self.slug)
    }
}

impl Agent {
    pub fn new(idx: u64, config: AgentConfig, memory_file: PathBuf) -> Agent {
        Agent {
            agent: AgentDefinition {
                id: format!("agent-{}", idx),
                config,
                memory_file,
            },
            messages: Vec::new(),
        }
    }
}
