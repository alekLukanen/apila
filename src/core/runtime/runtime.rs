use std::{
    fmt::Debug,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use thiserror::Error;

use crate::core::runtime::agent::{AgentDefinition, Model};

use super::agent;

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("create agent failed")]
    CreateAgentFailed,
}

pub struct Runtime {
    config: RuntimeConfig,
    inner: Arc<Mutex<RuntimeInner>>,
}

impl Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Runtime<...>")
    }
}

struct RuntimeInner {
    agent_idx: u64,
    agents: Vec<agent::Agent>,
}

#[derive(Clone)]
pub struct RuntimeConfig {
    project_dir: PathBuf,
}

impl RuntimeConfig {
    pub fn new(project_dir: PathBuf) -> RuntimeConfig {
        RuntimeConfig {
            project_dir: project_dir,
        }
    }
    pub fn project_dir(&self) -> PathBuf {
        self.project_dir.clone()
    }
    // add setters here barrow then pass back -> RuntimeConfig
}

impl Runtime {
    pub fn new(config: RuntimeConfig) -> Runtime {
        Runtime {
            config,
            inner: Arc::new(Mutex::new(RuntimeInner {
                agent_idx: 0,
                agents: Vec::new(),
            })),
        }
    }

    pub fn config(&self) -> RuntimeConfig {
        self.config.clone()
    }

    // Agent operations //////////////////
    //////////////////////////////////////
    pub fn create_agent(&self, mut config: agent::AgentConfig) -> Result<u64, RuntimeError> {
        let mut inner = self.inner.lock().expect("mutex error");
        inner.agent_idx += 1;
        let idx = inner.agent_idx;

        // handle defaults as needed
        if config.name() == "" {
            config = config.set_name(format!("agent-{}", idx));
        }
        if !config.model().valid() {
            // set default model here
            config = config.set_model(Model {
                author: "openai".into(),
                slug: "gpt-5.6-sol".into(),
            });
        }

        let agent = agent::Agent::new(idx, config, PathBuf::new());
        inner.agents.push(agent);

        Ok(idx)
    }

    pub fn list_agents(&self) -> Vec<AgentDefinition> {
        self.inner
            .lock()
            .expect("mutex error")
            .agents
            .iter()
            .map(|agent| agent.agent_definition())
            .collect()
    }
}
