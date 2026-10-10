use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use crate::core::memory::store::{memory_path, MemoryStore, StoreError};
use crate::core::openrouter::types::Message;

/// What the ui shows about an agent's memory. Shared by the agent's definition
/// and whatever is working on its memory, so neither has to tell the other.
#[derive(Debug, Clone, Default)]
pub struct MemoryStatus(Arc<Mutex<Option<String>>>);

impl MemoryStatus {
    pub fn get(&self) -> Option<String> {
        self.0.lock().expect("mutex error").clone()
    }
    pub fn set(&self, status: impl Into<String>) {
        *self.0.lock().expect("mutex error") = Some(status.into());
    }
    pub fn clear(&self) {
        *self.0.lock().expect("mutex error") = None;
    }
}

/// Writes one session's messages into the agent's memory database as they
/// happen. Owned by the agent's thread, which is the only one that creates
/// the database.
pub struct SessionRecorder {
    dir: PathBuf,
    owner: String,
    store: Option<Arc<MemoryStore>>,
    session_id: Option<i64>,
    /// How many of the conversation's messages are already stored.
    persisted: usize,
    status: MemoryStatus,
    /// Set while `status` is showing this recorder's failure, so a later
    /// success clears only what it put there.
    failing: bool,
}

impl SessionRecorder {
    /// `dir` is the agent's directory; `owner` names this apila process.
    pub fn new(dir: PathBuf, owner: String, status: MemoryStatus) -> SessionRecorder {
        SessionRecorder {
            dir,
            owner,
            store: None,
            session_id: None,
            persisted: 0,
            status,
            failing: false,
        }
    }

    /// The memory database, opened on first use. Kept once open so the skill
    /// tools on this thread share the one connection.
    pub fn store(&mut self) -> Result<Arc<MemoryStore>, StoreError> {
        if let Some(store) = &self.store {
            return Ok(Arc::clone(store));
        }
        let store = Arc::new(MemoryStore::create(&memory_path(&self.dir))?);
        self.store = Some(Arc::clone(&store));
        Ok(store)
    }

    /// The session's id, once something has been written to it.
    pub fn session_id(&self) -> Option<i64> {
        self.session_id
    }

    /// Stores whatever of `messages` is not stored yet. A failure is shown on
    /// the agent and tried again on the next sync; it never stops the agent.
    pub fn sync(&mut self, messages: &Mutex<impl Messages>) {
        let unsaved: Vec<Message> = {
            let messages = messages.lock().expect("mutex error");
            messages
                .messages()
                .get(self.persisted..)
                .unwrap_or(&[])
                .to_vec()
        };
        if unsaved.is_empty() {
            return;
        }
        match self.write(&unsaved) {
            Ok(()) => {
                self.persisted += unsaved.len();
                if self.failing {
                    self.failing = false;
                    self.status.clear();
                }
            }
            Err(err) => {
                self.failing = true;
                self.status
                    .set(format!("recording the session failed: {}", err));
            }
        }
    }

    fn write(&mut self, messages: &[Message]) -> Result<(), StoreError> {
        let store = self.store()?;
        let session_id = match self.session_id {
            Some(id) => id,
            None => {
                let id = store.begin_session(&self.owner)?;
                self.session_id = Some(id);
                id
            }
        };
        store.append_messages(session_id, self.persisted, messages)
    }

    /// Ends the session, if one was ever written. `analyse` queues it for the
    /// skill writer.
    pub fn end(&mut self, analyse: bool) -> Result<(), StoreError> {
        let Some(session_id) = self.session_id else {
            return Ok(());
        };
        self.store()?.end_session(session_id, analyse)
    }
}

/// Whatever holds the conversation being recorded.
pub trait Messages {
    fn messages(&self) -> &[Message];
}

impl Messages for Vec<Message> {
    fn messages(&self) -> &[Message] {
        self
    }
}
