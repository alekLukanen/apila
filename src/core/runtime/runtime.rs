use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use thiserror::Error;

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

struct RuntimeInner {
    agent_idx: u64,
    agents: Vec<agent::Agent>,
}

pub(crate) struct RuntimeConfig {
    project_dir: PathBuf,
}

impl Runtime {
    fn new(config: RuntimeConfig) -> Runtime {
        Runtime {
            config,
            inner: Arc::new(Mutex::new(RuntimeInner {
                agent_idx: 0,
                agents: Vec::new(),
            })),
        }
    }

    fn create_agent(&self, config: agent::AgentConfig) -> Result<u64, RuntimeError> {
        let mut inner = self.inner.lock().expect("mutex error");
        inner.agent_idx += 1;
        let idx = inner.agent_idx;

        let agent = agent::Agent::new(idx, config, PathBuf::new());
        inner.agents.push(agent);

        Ok(idx)
    }
}
