use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use crate::core::memory::store::{memory_path, MemoryStore, SkillsSaved, StoreError};
use crate::core::openrouter::types::{Message, Spend};

/// Where the newest analysis of a session is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobState {
    Running,
    Done {
        skills: SkillsSaved,
    },
    Failed(String),
    /// Its process stopped before it finished.
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryJob {
    pub session_id: i64,
    pub state: JobState,
    pub spend: Spend,
}

/// Everything the ui shows about an agent's memory, read in one go so the
/// figures on screen agree with each other.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemoryView {
    /// The latest error, if the last attempt at something failed.
    pub message: Option<String>,
    pub skills: Option<usize>,
    /// Sessions waiting to be analysed.
    pub pending: Option<usize>,
    pub last_job: Option<MemoryJob>,
    /// What jobs run by earlier processes cost.
    pub jobs_base: Spend,
    /// What jobs run by this process cost.
    pub jobs_live: Spend,
    /// Whether the totals kept in the memory database have been read.
    pub loaded: bool,
    /// Set when a session is handed to the writer, until it starts a job.
    pub queued: bool,
}

impl MemoryView {
    /// What every job, earlier processes' and this one's, has cost.
    pub fn memory_spend(&self) -> Spend {
        let mut total = self.jobs_base;
        total.merge(&self.jobs_live);
        total
    }

    pub fn job_running(&self) -> bool {
        self.last_job
            .as_ref()
            .is_some_and(|job| job.state == JobState::Running)
    }

    /// True when the newest job failed and nothing is queued behind it, so
    /// the pending sessions wait for the next job to be queued.
    pub fn stalled(&self) -> bool {
        !self.queued
            && self
                .last_job
                .as_ref()
                .is_some_and(|job| matches!(job.state, JobState::Failed(_)))
    }
}

/// What the ui shows about an agent's memory. Shared by the agent's definition
/// and whatever is working on its memory, so neither has to tell the other.
/// Taken after the definition's lock when both are held, never before it.
#[derive(Debug, Clone, Default)]
pub struct MemoryStatus(Arc<Mutex<MemoryView>>);

impl MemoryStatus {
    fn lock(&self) -> MutexGuard<'_, MemoryView> {
        self.0.lock().expect("mutex error")
    }

    /// The latest error, if any.
    pub fn get(&self) -> Option<String> {
        self.lock().message.clone()
    }
    pub fn set(&self, status: impl Into<String>) {
        self.lock().message = Some(status.into());
    }
    pub fn clear(&self) {
        self.lock().message = None;
    }
    /// Clears the error only if it is one `prefix` starts, so a success never
    /// hides another part's failure.
    pub fn clear_prefixed(&self, prefix: &str) {
        let mut view = self.lock();
        if view
            .message
            .as_ref()
            .is_some_and(|message| message.starts_with(prefix))
        {
            view.message = None;
        }
    }

    pub fn snapshot(&self) -> MemoryView {
        self.lock().clone()
    }

    /// Takes what the memory database holds from earlier processes. Whatever
    /// this process has already set is newer, so it is kept.
    pub fn load(
        &self,
        jobs_base: Spend,
        skills: usize,
        last_job: Option<MemoryJob>,
        pending: Option<usize>,
    ) {
        let mut view = self.lock();
        view.jobs_base = jobs_base;
        view.skills.get_or_insert(skills);
        if view.last_job.is_none() {
            view.last_job = last_job;
        }
        if view.pending.is_none() {
            view.pending = pending;
        }
        view.loaded = true;
    }

    pub fn loaded(&self) -> bool {
        self.lock().loaded
    }

    pub fn set_skills(&self, skills: usize) {
        self.lock().skills = Some(skills);
    }
    pub fn set_pending(&self, pending: usize) {
        self.lock().pending = Some(pending);
    }

    /// Counts a session just handed to the writer, until it counts for itself.
    pub fn session_queued(&self) {
        let mut view = self.lock();
        view.pending = Some(view.pending.unwrap_or(0).saturating_add(1));
        view.queued = true;
    }

    /// A pending session was taken up for analysis.
    pub fn job_started(&self, session_id: i64) {
        let mut view = self.lock();
        view.pending = view.pending.map(|pending| pending.saturating_sub(1));
        view.queued = false;
        view.last_job = Some(MemoryJob {
            session_id,
            state: JobState::Running,
            spend: Spend::default(),
        });
    }

    /// Adds what the running job just spent.
    pub fn job_spent(&self, delta: &Spend) {
        let mut view = self.lock();
        view.jobs_live.merge(delta);
        if let Some(job) = &mut view.last_job {
            job.spend.merge(delta);
        }
    }

    pub fn job_finished(&self, state: JobState) {
        if let Some(job) = &mut self.lock().last_job {
            job.state = state;
        }
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
    /// What the session had cost when it was last stored.
    persisted_spend: Spend,
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
            persisted_spend: Spend::default(),
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

    /// Stores whatever of `messages` is not stored yet, and what the session
    /// has cost. A failure is shown on the agent and tried again on the next
    /// sync; it never stops the agent.
    pub fn sync(&mut self, messages: &Mutex<impl Messages>) {
        let (unsaved, spend): (Vec<Message>, Spend) = {
            let messages = messages.lock().expect("mutex error");
            let unsaved = messages
                .messages()
                .get(self.persisted..)
                .unwrap_or(&[])
                .to_vec();
            (unsaved, messages.spend())
        };
        // a cost alone never opens a session; it is stored with the next message
        if unsaved.is_empty() && (spend == self.persisted_spend || self.session_id.is_none()) {
            return;
        }
        match self.write(&unsaved, &spend) {
            Ok(()) => {
                self.persisted += unsaved.len();
                self.persisted_spend = spend;
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

    fn write(&mut self, messages: &[Message], spend: &Spend) -> Result<(), StoreError> {
        let store = self.store()?;
        let session_id = match self.session_id {
            Some(id) => id,
            None => {
                let id = store.begin_session(&self.owner)?;
                self.session_id = Some(id);
                id
            }
        };
        store.record(session_id, self.persisted, messages, spend)
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

    /// What the conversation has cost so far.
    fn spend(&self) -> Spend {
        Spend::default()
    }
}

impl Messages for Vec<Message> {
    fn messages(&self) -> &[Message] {
        self
    }
}
