use std::path::PathBuf;

pub(crate) struct Agent {
    id: String,
    config: AgentConfig,

    // state
    messages: Vec<AgentMessage>,
    memory_file: PathBuf,
}

pub(crate) struct AgentConfig {
    name: String,
    model: Model,
    initial_directive: String,
}

pub(crate) enum AgentMessageRole {
    User,
}

pub(crate) struct AgentMessage {
    role: AgentMessageRole,
    content: String,
}

pub(crate) struct Model {
    author: String, // ex: openai
    slug: String,   // ex: gpt-4o
}

impl Agent {
    pub(crate) fn new(idx: u64, config: AgentConfig, memory_file: PathBuf) -> Agent {
        Agent {
            id: format!("agent-{}", idx),
            config,
            messages: Vec::new(),
            memory_file,
        }
    }
}
