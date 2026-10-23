use std::fmt::Debug;
use std::num::NonZero;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use thiserror::Error;
use turso_core::{
    Connection, Database, LimboError, Numeric, OpenFlags, OpenOptions, PlatformIO, SqliteDialect,
    Statement, StepResult, Value,
};

use crate::core::openrouter::client::OpenRouter;
use crate::core::openrouter::types::{EmbeddingRequest, Message, Spend, Usage};

/// The directory inside an agent's directory that holds what it remembers.
pub const MEMORY_DIR: &str = "memory";
pub const MEMORY_FILE: &str = "memory.db";

/// The most of a skill's body [`MemoryStore::skills`] reads.
pub const LISTED_BODY_CHARS: usize = 16 * 1024;

/// How many times a session is analysed before it is given up on.
pub const MAX_ATTEMPTS: i64 = 3;

/// How long a write waits on another connection's transaction before it is
/// reported as busy and left for the next try.
const BUSY_TIMEOUT: Duration = Duration::from_secs(2);

const SCHEMA_VERSION: &str = "3";

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS sessions (
  id INTEGER PRIMARY KEY,
  owner TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('active','ended','pending','analysing','analysed','failed')),
  started_at INTEGER NOT NULL,
  ended_at INTEGER,
  analysed_at INTEGER,
  attempts INTEGER NOT NULL DEFAULT 0,
  error TEXT
);
CREATE TABLE IF NOT EXISTS session_messages (
  session_id INTEGER NOT NULL REFERENCES sessions(id),
  seq INTEGER NOT NULL,
  message TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (session_id, seq)
);
CREATE TABLE IF NOT EXISTS memories (
  id INTEGER PRIMARY KEY,
  kind TEXT NOT NULL CHECK (kind IN ('skill')),
  name TEXT NOT NULL UNIQUE,
  when_to_use TEXT NOT NULL,
  body TEXT NOT NULL,
  embedding BLOB NOT NULL,
  embedding_model TEXT NOT NULL,
  dimensions INTEGER NOT NULL,
  source_session INTEGER REFERENCES sessions(id),
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS session_costs (
  session_id INTEGER PRIMARY KEY REFERENCES sessions(id),
  tokens INTEGER NOT NULL DEFAULT 0,
  cost_nanos INTEGER NOT NULL DEFAULT 0,
  unpriced INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS memory_jobs (
  id INTEGER PRIMARY KEY,
  session_id INTEGER NOT NULL REFERENCES sessions(id),
  owner TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('running','done','failed','interrupted')),
  skills_saved INTEGER NOT NULL DEFAULT 0,
  skills_updated INTEGER NOT NULL DEFAULT 0,
  tokens INTEGER NOT NULL DEFAULT 0,
  cost_nanos INTEGER NOT NULL DEFAULT 0,
  unpriced INTEGER NOT NULL DEFAULT 0,
  error TEXT,
  started_at INTEGER NOT NULL,
  finished_at INTEGER
);
";

/// Where an agent's memory database lives.
pub fn memory_path(agent_dir: &Path) -> PathBuf {
    agent_dir.join(MEMORY_DIR).join(MEMORY_FILE)
}

#[derive(Debug, Clone, Error)]
pub enum StoreError {
    /// Another connection held the database past the busy timeout. Nothing
    /// was written, and trying again later is expected to work.
    #[error("the memory database is busy")]
    Busy,

    #[error("the database is in use by another apila process")]
    InUse,

    #[error("{0}")]
    Sql(String),

    #[error("{0}")]
    Io(String),

    #[error("a stored value is not what was expected: {0}")]
    Corrupt(String),
}

impl From<LimboError> for StoreError {
    fn from(err: LimboError) -> StoreError {
        match err {
            LimboError::Busy | LimboError::BusySnapshot => StoreError::Busy,
            LimboError::LockingError(_) => StoreError::InUse,
            err => StoreError::Sql(err.to_string()),
        }
    }
}

/// Where a recorded session is on its way to being learned from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    Active,
    /// Over, and not to be analysed.
    Ended,
    Pending,
    Analysing,
    Analysed,
    Failed,
}

impl SessionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionStatus::Active => "active",
            SessionStatus::Ended => "ended",
            SessionStatus::Pending => "pending",
            SessionStatus::Analysing => "analysing",
            SessionStatus::Analysed => "analysed",
            SessionStatus::Failed => "failed",
        }
    }

    fn parse(text: &str) -> Option<SessionStatus> {
        [
            SessionStatus::Active,
            SessionStatus::Ended,
            SessionStatus::Pending,
            SessionStatus::Analysing,
            SessionStatus::Analysed,
            SessionStatus::Failed,
        ]
        .into_iter()
        .find(|status| status.as_str() == text)
    }

    /// What a session that just ended becomes.
    fn ended(analyse: bool) -> SessionStatus {
        if analyse {
            SessionStatus::Pending
        } else {
            SessionStatus::Ended
        }
    }
}

/// How one analysis of a session went.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatus {
    Running,
    Done,
    Failed,
    /// Its process stopped before it finished.
    Interrupted,
}

impl JobStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            JobStatus::Running => "running",
            JobStatus::Done => "done",
            JobStatus::Failed => "failed",
            JobStatus::Interrupted => "interrupted",
        }
    }

    fn parse(text: &str) -> Option<JobStatus> {
        [
            JobStatus::Running,
            JobStatus::Done,
            JobStatus::Failed,
            JobStatus::Interrupted,
        ]
        .into_iter()
        .find(|status| status.as_str() == text)
    }
}

/// The skills one analysis wrote: new names, and names already stored.
/// `added` is kept in the `skills_saved` column, its name before updates
/// were counted apart.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SkillsSaved {
    pub added: usize,
    pub updated: usize,
}

impl SkillsSaved {
    pub fn total(&self) -> usize {
        self.added + self.updated
    }
}

/// One analysis of a session, and what it cost.
#[derive(Debug, Clone, PartialEq)]
pub struct JobRecord {
    pub id: i64,
    pub session_id: i64,
    pub owner: String,
    pub status: JobStatus,
    pub skills: SkillsSaved,
    pub spend: Spend,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionRecord {
    pub id: i64,
    pub owner: String,
    pub status: SessionStatus,
    pub attempts: i64,
    pub error: Option<String>,
}

/// A skill as the writer hands it over to be stored.
#[derive(Debug, Clone)]
pub struct NewSkill {
    pub name: String,
    pub when_to_use: String,
    pub body: String,
    pub embedding: Vec<f32>,
    pub embedding_model: String,
    pub source_session: Option<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Skill {
    pub id: i64,
    pub name: String,
    pub when_to_use: String,
    pub body: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SkillSummary {
    pub id: i64,
    pub name: String,
    pub when_to_use: String,
}

/// A skill as the ui lists it, apart from [`SkillSummary`], which is what the
/// writer is prompted with.
#[derive(Debug, Clone, PartialEq)]
pub struct SkillListing {
    pub id: i64,
    pub name: String,
    pub when_to_use: String,
    /// Cut to [`LISTED_BODY_CHARS`], so one runaway skill can't stall the ui.
    pub body: String,
    /// Unix seconds.
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SkillMatch {
    pub id: i64,
    pub name: String,
    pub when_to_use: String,
    pub similarity: f64,
}

/// One connection to an agent's memory database. Every call runs in a
/// transaction of its own, so a call that fails leaves nothing behind.
pub struct MemoryStore {
    path: PathBuf,
    _database: Arc<Database>,
    connection: Arc<Connection>,
}

impl Debug for MemoryStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryStore")
            .field("path", &self.path)
            .finish()
    }
}

impl MemoryStore {
    /// Opens the database at `path`, creating it and its directory when they
    /// are not there yet. Only an agent's own thread does this.
    pub fn create(path: &Path) -> Result<MemoryStore, StoreError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| StoreError::Io(err.to_string()))?;
        }
        MemoryStore::open(path, OpenFlags::Create)
    }

    /// Opens the database at `path` only if it is already there.
    pub fn open_existing(path: &Path) -> Result<Option<MemoryStore>, StoreError> {
        if !path.is_file() {
            return Ok(None);
        }
        MemoryStore::open(path, OpenFlags::None).map(Some)
    }

    fn open(path: &Path, flags: OpenFlags) -> Result<MemoryStore, StoreError> {
        let path_text = path
            .to_str()
            .ok_or_else(|| StoreError::Io(format!("{} is not valid utf-8", path.display())))?;
        let io = Arc::new(PlatformIO::new()?);
        let database = Database::open(
            io,
            path_text,
            OpenOptions::new(Arc::new(SqliteDialect)).flags(flags),
        )?;
        let connection = database.connect()?;
        connection.set_busy_timeout(BUSY_TIMEOUT);

        let store = MemoryStore {
            path: path.to_path_buf(),
            _database: database,
            connection,
        };
        store.write(|store| {
            store.script(SCHEMA)?;
            store.add_missing_columns()?;
            store.execute(
                "INSERT OR IGNORE INTO meta (key, value) VALUES ('schema_version', ?1)",
                vec![text(SCHEMA_VERSION)],
            )?;
            // only ever raised, so a db a newer binary wrote keeps its label
            store.execute(
                "UPDATE meta SET value = ?1 \
                 WHERE key = 'schema_version' AND CAST(value AS INTEGER) < ?2",
                vec![text(SCHEMA_VERSION), integer(schema_number())],
            )?;
            Ok(())
        })?;
        Ok(store)
    }

    /// Adds the columns a table made by an older release lacks, which
    /// `CREATE TABLE IF NOT EXISTS` leaves alone.
    fn add_missing_columns(&self) -> Result<(), StoreError> {
        let columns = self
            .rows("PRAGMA table_info(memory_jobs)", Vec::new())?
            .iter()
            .map(|row| text_at(row, 1))
            .collect::<Result<Vec<_>, _>>()?;
        if !columns.iter().any(|column| column == "skills_updated") {
            self.execute(
                "ALTER TABLE memory_jobs \
                 ADD COLUMN skills_updated INTEGER NOT NULL DEFAULT 0",
                Vec::new(),
            )?;
        }
        Ok(())
    }

    pub fn schema_version(&self) -> Result<Option<String>, StoreError> {
        let rows = self.rows(
            "SELECT value FROM meta WHERE key = 'schema_version'",
            Vec::new(),
        )?;
        rows.first().map(|row| text_at(row, 0)).transpose()
    }

    // Sessions //////////////////////////
    //////////////////////////////////////

    /// Opens a session recorded by the apila process `owner`.
    pub fn begin_session(&self, owner: &str) -> Result<i64, StoreError> {
        self.write(|store| {
            store.execute(
                "INSERT INTO sessions (owner, status, started_at) VALUES (?1, 'active', ?2)",
                vec![text(owner), integer(now())],
            )?;
            Ok(store.connection.last_insert_rowid())
        })
    }

    /// Stores `messages` as the session's messages from `from_seq` on. A
    /// message already stored at a position is replaced, so a retry after an
    /// unclear failure cannot duplicate one.
    pub fn append_messages(
        &self,
        session_id: i64,
        from_seq: usize,
        messages: &[Message],
    ) -> Result<(), StoreError> {
        let encoded = encode_messages(messages)?;
        self.write(|store| store.insert_messages(session_id, from_seq, encoded))
    }

    /// Stores `messages` as `append_messages` does, and sets what the session
    /// has cost so far to `spend`, in one write.
    pub fn record(
        &self,
        session_id: i64,
        from_seq: usize,
        messages: &[Message],
        spend: &Spend,
    ) -> Result<(), StoreError> {
        let encoded = encode_messages(messages)?;
        self.write(|store| {
            store.insert_messages(session_id, from_seq, encoded)?;
            store.execute(
                "INSERT INTO session_costs (session_id, tokens, cost_nanos, unpriced) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(session_id) DO UPDATE SET tokens = excluded.tokens, \
                 cost_nanos = excluded.cost_nanos, unpriced = excluded.unpriced",
                vec![
                    integer(session_id),
                    integer(clamp_u64(spend.tokens)),
                    integer(spend.cost_nanos),
                    integer(i64::from(spend.unpriced)),
                ],
            )?;
            Ok(())
        })
    }

    fn insert_messages(
        &self,
        session_id: i64,
        from_seq: usize,
        encoded: Vec<String>,
    ) -> Result<(), StoreError> {
        let created_at = now();
        for (offset, message) in encoded.into_iter().enumerate() {
            self.execute(
                "INSERT OR REPLACE INTO session_messages (session_id, seq, message, created_at) \
                 VALUES (?1, ?2, ?3, ?4)",
                vec![
                    integer(session_id),
                    integer((from_seq + offset) as i64),
                    text(message),
                    integer(created_at),
                ],
            )?;
        }
        Ok(())
    }

    /// What one session's chat has cost, as last recorded.
    pub fn session_spend(&self, session_id: i64) -> Result<Option<Spend>, StoreError> {
        let rows = self.rows(
            "SELECT tokens, cost_nanos, unpriced FROM session_costs WHERE session_id = ?1",
            vec![integer(session_id)],
        )?;
        rows.first().map(|row| spend_at(row, 0)).transpose()
    }

    /// What every session's chat but those in `exclude` has cost together.
    pub fn session_totals(&self, exclude: &[i64]) -> Result<Spend, StoreError> {
        let rows = self.rows(
            "SELECT session_id, tokens, cost_nanos, unpriced FROM session_costs",
            Vec::new(),
        )?;
        let mut total = Spend::default();
        for row in &rows {
            if !exclude.contains(&int_at(row, 0)?) {
                total.merge(&spend_at(row, 1)?);
            }
        }
        Ok(total)
    }

    /// Ends an active session: `pending` to be analysed, or `ended` when it
    /// never will be.
    pub fn end_session(&self, session_id: i64, analyse: bool) -> Result<(), StoreError> {
        self.write(|store| {
            store.execute(
                "UPDATE sessions SET status = ?1, ended_at = ?2 WHERE id = ?3 AND status = 'active'",
                vec![
                    text(SessionStatus::ended(analyse).as_str()),
                    integer(now()),
                    integer(session_id),
                ],
            )?;
            Ok(())
        })
    }

    /// Ends the sessions an earlier apila process left open or half analysed,
    /// which every process but `owner` is. Returns how many were touched.
    pub fn recover(&self, owner: &str, analyse: bool) -> Result<usize, StoreError> {
        self.write(|store| {
            let failed = store.execute(
                "UPDATE sessions SET status = 'failed', \
                 error = coalesce(error, 'stopped while being analysed too many times') \
                 WHERE owner != ?1 AND status = 'analysing' AND attempts >= ?2",
                vec![text(owner), integer(MAX_ATTEMPTS)],
            )?;
            let ended = store.execute(
                "UPDATE sessions SET status = ?1, ended_at = coalesce(ended_at, ?2) \
                 WHERE owner != ?3 AND status IN ('active', 'analysing')",
                vec![
                    text(SessionStatus::ended(analyse).as_str()),
                    integer(now()),
                    text(owner),
                ],
            )?;
            store.execute(
                "UPDATE memory_jobs SET status = 'interrupted', finished_at = ?1 \
                 WHERE owner != ?2 AND status = 'running'",
                vec![integer(now()), text(owner)],
            )?;
            Ok((failed + ended) as usize)
        })
    }

    /// Takes the oldest pending session for `owner` to analyse, counting the
    /// attempt and opening a job for it. Done in one write so two writers
    /// never take the same one. Returns the session's id and the job's.
    pub fn claim_pending(&self, owner: &str) -> Result<Option<(i64, i64)>, StoreError> {
        self.write(|store| {
            let rows = store.rows(
                "SELECT id FROM sessions WHERE status = 'pending' ORDER BY id LIMIT 1",
                Vec::new(),
            )?;
            let Some(id) = rows.first().map(|row| int_at(row, 0)).transpose()? else {
                return Ok(None);
            };
            store.execute(
                "UPDATE sessions SET status = 'analysing', owner = ?1, attempts = attempts + 1 \
                 WHERE id = ?2",
                vec![text(owner), integer(id)],
            )?;
            store.execute(
                "INSERT INTO memory_jobs (session_id, owner, status, started_at) \
                 VALUES (?1, ?2, 'running', ?3)",
                vec![integer(id), text(owner), integer(now())],
            )?;
            Ok(Some((id, store.connection.last_insert_rowid())))
        })
    }

    pub fn pending_count(&self) -> Result<usize, StoreError> {
        let rows = self.rows(
            "SELECT count(*) FROM sessions WHERE status = 'pending'",
            Vec::new(),
        )?;
        Ok(rows
            .first()
            .map(|row| int_at(row, 0))
            .transpose()?
            .unwrap_or(0) as usize)
    }

    /// Sets what a running job has cost so far.
    pub fn set_job_spend(&self, job_id: i64, spend: &Spend) -> Result<(), StoreError> {
        self.write(|store| {
            store.execute(
                "UPDATE memory_jobs SET tokens = ?1, cost_nanos = ?2, unpriced = ?3 WHERE id = ?4",
                vec![
                    integer(clamp_u64(spend.tokens)),
                    integer(spend.cost_nanos),
                    integer(i64::from(spend.unpriced)),
                    integer(job_id),
                ],
            )?;
            Ok(())
        })
    }

    /// Records how analysing a claimed session went: `result` holds the
    /// skills it saved. A failure goes back to `pending` until it has used
    /// up its attempts.
    pub fn finish_session(
        &self,
        session_id: i64,
        job_id: i64,
        result: Result<SkillsSaved, String>,
        spend: &Spend,
    ) -> Result<SessionStatus, StoreError> {
        self.write(|store| {
            let (status, job_status, saved, error) = match &result {
                Ok(saved) => {
                    store.execute(
                        "UPDATE sessions SET status = 'analysed', analysed_at = ?1, error = NULL \
                         WHERE id = ?2",
                        vec![integer(now()), integer(session_id)],
                    )?;
                    (SessionStatus::Analysed, JobStatus::Done, *saved, None)
                }
                Err(error) => {
                    let attempts = store
                        .rows(
                            "SELECT attempts FROM sessions WHERE id = ?1",
                            vec![integer(session_id)],
                        )?
                        .first()
                        .map(|row| int_at(row, 0))
                        .transpose()?
                        .unwrap_or(MAX_ATTEMPTS);
                    let status = if attempts >= MAX_ATTEMPTS {
                        SessionStatus::Failed
                    } else {
                        SessionStatus::Pending
                    };
                    store.execute(
                        "UPDATE sessions SET status = ?1, error = ?2 WHERE id = ?3",
                        vec![
                            text(status.as_str()),
                            text(error.clone()),
                            integer(session_id),
                        ],
                    )?;
                    (
                        status,
                        JobStatus::Failed,
                        SkillsSaved::default(),
                        Some(error.clone()),
                    )
                }
            };
            store.execute(
                "UPDATE memory_jobs SET status = ?1, skills_saved = ?2, skills_updated = ?3, \
                 tokens = ?4, cost_nanos = ?5, unpriced = ?6, error = ?7, finished_at = ?8 \
                 WHERE id = ?9",
                vec![
                    text(job_status.as_str()),
                    integer(saved.added as i64),
                    integer(saved.updated as i64),
                    integer(clamp_u64(spend.tokens)),
                    integer(spend.cost_nanos),
                    integer(i64::from(spend.unpriced)),
                    error.map(text).unwrap_or(Value::Null),
                    integer(now()),
                    integer(job_id),
                ],
            )?;
            Ok(status)
        })
    }

    pub fn job(&self, job_id: i64) -> Result<Option<JobRecord>, StoreError> {
        let rows = self.rows(
            "SELECT id, session_id, owner, status, skills_saved, skills_updated, tokens, \
             cost_nanos, unpriced, error FROM memory_jobs WHERE id = ?1",
            vec![integer(job_id)],
        )?;
        rows.first().map(|row| job_record(row)).transpose()
    }

    /// The newest job, whatever became of it.
    pub fn last_job(&self) -> Result<Option<JobRecord>, StoreError> {
        let rows = self.rows(
            "SELECT id, session_id, owner, status, skills_saved, skills_updated, tokens, \
             cost_nanos, unpriced, error FROM memory_jobs ORDER BY id DESC LIMIT 1",
            Vec::new(),
        )?;
        rows.first().map(|row| job_record(row)).transpose()
    }

    /// What every job but those run by `exclude_owner` has cost together.
    pub fn memory_totals(&self, exclude_owner: &str) -> Result<Spend, StoreError> {
        let rows = self.rows(
            "SELECT tokens, cost_nanos, unpriced FROM memory_jobs WHERE owner != ?1",
            vec![text(exclude_owner)],
        )?;
        sum_spend(&rows)
    }

    pub fn session(&self, session_id: i64) -> Result<Option<SessionRecord>, StoreError> {
        let rows = self.rows(
            "SELECT id, owner, status, attempts, error FROM sessions WHERE id = ?1",
            vec![integer(session_id)],
        )?;
        rows.first().map(|row| session_record(row)).transpose()
    }

    /// Every session, oldest first.
    pub fn sessions(&self) -> Result<Vec<SessionRecord>, StoreError> {
        let rows = self.rows(
            "SELECT id, owner, status, attempts, error FROM sessions ORDER BY id",
            Vec::new(),
        )?;
        rows.iter().map(|row| session_record(row)).collect()
    }

    /// A session's messages, in the order they were recorded.
    pub fn session_messages(&self, session_id: i64) -> Result<Vec<Message>, StoreError> {
        let rows = self.rows(
            "SELECT message FROM session_messages WHERE session_id = ?1 ORDER BY seq",
            vec![integer(session_id)],
        )?;
        rows.iter()
            .map(|row| {
                let json = text_at(row, 0)?;
                serde_json::from_str(&json).map_err(|err| StoreError::Corrupt(err.to_string()))
            })
            .collect()
    }

    // Skills ////////////////////////////
    //////////////////////////////////////

    /// Stores a skill, replacing the one already under its name while keeping
    /// that one's id. Returns the id and whether the skill is new.
    pub fn upsert_skill(&self, skill: &NewSkill) -> Result<(i64, bool), StoreError> {
        let created_at = now();
        self.write(|store| {
            let existing = store.rows(
                "SELECT id FROM memories WHERE name = ?1",
                vec![text(skill.name.clone())],
            )?;
            store.execute(
                "INSERT INTO memories (kind, name, when_to_use, body, embedding, embedding_model, \
                 dimensions, source_session, created_at, updated_at) \
                 VALUES ('skill', ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8) \
                 ON CONFLICT(name) DO UPDATE SET when_to_use = excluded.when_to_use, \
                 body = excluded.body, embedding = excluded.embedding, \
                 embedding_model = excluded.embedding_model, dimensions = excluded.dimensions, \
                 source_session = excluded.source_session, updated_at = excluded.updated_at",
                vec![
                    text(skill.name.clone()),
                    text(skill.when_to_use.clone()),
                    text(skill.body.clone()),
                    Value::Blob(encode_vector(&skill.embedding)),
                    text(skill.embedding_model.clone()),
                    integer(skill.embedding.len() as i64),
                    skill.source_session.map(integer).unwrap_or(Value::Null),
                    integer(created_at),
                ],
            )?;
            let rows = store.rows(
                "SELECT id FROM memories WHERE name = ?1",
                vec![text(skill.name.clone())],
            )?;
            let id = rows
                .first()
                .map(|row| int_at(row, 0))
                .transpose()?
                .ok_or_else(|| StoreError::Corrupt("the skill was not stored".into()))?;
            Ok((id, existing.is_empty()))
        })
    }

    /// The skills embedded by `model` closest to `query`, most similar first,
    /// leaving out any below `min_similarity`.
    pub fn search(
        &self,
        query: &[f32],
        model: &str,
        min_similarity: f64,
        limit: usize,
    ) -> Result<Vec<SkillMatch>, StoreError> {
        let rows = self.rows(
            "SELECT id, name, when_to_use, embedding FROM memories \
             WHERE kind = 'skill' AND embedding_model = ?1 AND dimensions = ?2",
            vec![text(model), integer(query.len() as i64)],
        )?;
        let mut matches = Vec::new();
        for row in &rows {
            let embedding = decode_vector(blob_at(row, 3)?)?;
            let Some(similarity) = cosine_similarity(query, &embedding) else {
                continue;
            };
            if similarity < min_similarity {
                continue;
            }
            matches.push(SkillMatch {
                id: int_at(row, 0)?,
                name: text_at(row, 1)?,
                when_to_use: text_at(row, 2)?,
                similarity,
            });
        }
        matches.sort_by(|a, b| b.similarity.total_cmp(&a.similarity));
        matches.truncate(limit);
        Ok(matches)
    }

    pub fn count_skills(&self) -> Result<usize, StoreError> {
        let rows = self.rows(
            "SELECT count(*) FROM memories WHERE kind = 'skill'",
            Vec::new(),
        )?;
        Ok(rows
            .first()
            .map(|row| int_at(row, 0))
            .transpose()?
            .unwrap_or(0) as usize)
    }

    /// How many skills were embedded some other way than `model` makes
    /// vectors of `dimensions`, which a search with it can never find.
    pub fn count_unsearchable(&self, model: &str, dimensions: usize) -> Result<usize, StoreError> {
        let rows = self.rows(
            "SELECT count(*) FROM memories \
             WHERE kind = 'skill' AND (embedding_model != ?1 OR dimensions != ?2)",
            vec![text(model), integer(dimensions as i64)],
        )?;
        Ok(rows
            .first()
            .map(|row| int_at(row, 0))
            .transpose()?
            .unwrap_or(0) as usize)
    }

    pub fn get(&self, id: i64) -> Result<Option<Skill>, StoreError> {
        let rows = self.rows(
            "SELECT id, name, when_to_use, body FROM memories WHERE kind = 'skill' AND id = ?1",
            vec![integer(id)],
        )?;
        rows.first()
            .map(|row| {
                Ok(Skill {
                    id: int_at(row, 0)?,
                    name: text_at(row, 1)?,
                    when_to_use: text_at(row, 2)?,
                    body: text_at(row, 3)?,
                })
            })
            .transpose()
    }

    /// Up to `limit` skills, the most recently updated first.
    pub fn list_skills(&self, limit: usize) -> Result<Vec<SkillSummary>, StoreError> {
        let rows = self.rows(
            "SELECT id, name, when_to_use FROM memories WHERE kind = 'skill' \
             ORDER BY updated_at DESC, id DESC LIMIT ?1",
            vec![integer(limit as i64)],
        )?;
        rows.iter()
            .map(|row| {
                Ok(SkillSummary {
                    id: int_at(row, 0)?,
                    name: text_at(row, 1)?,
                    when_to_use: text_at(row, 2)?,
                })
            })
            .collect()
    }

    /// Every skill with its body, the most recently updated first.
    pub fn skills(&self) -> Result<Vec<SkillListing>, StoreError> {
        let rows = self.rows(
            "SELECT id, name, when_to_use, substr(body, 1, ?1), updated_at FROM memories \
             WHERE kind = 'skill' ORDER BY updated_at DESC, id DESC",
            vec![integer(LISTED_BODY_CHARS as i64)],
        )?;
        rows.iter()
            .map(|row| {
                Ok(SkillListing {
                    id: int_at(row, 0)?,
                    name: text_at(row, 1)?,
                    when_to_use: text_at(row, 2)?,
                    body: text_at(row, 3)?,
                    updated_at: int_at(row, 4)?,
                })
            })
            .collect()
    }

    /// Permanently removes a skill. False when there was none with `id`.
    pub fn delete_skill(&self, id: i64) -> Result<bool, StoreError> {
        self.write(|store| {
            let deleted = store.execute(
                "DELETE FROM memories WHERE kind = 'skill' AND id = ?1",
                vec![integer(id)],
            )?;
            Ok(deleted > 0)
        })
    }

    // Running sql ///////////////////////
    //////////////////////////////////////

    /// Runs `body` in a write transaction, taken up front so two connections
    /// never both read and then both try to write.
    fn write<T>(
        &self,
        body: impl FnOnce(&MemoryStore) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.execute("BEGIN IMMEDIATE", Vec::new())?;
        let result = body(self).and_then(|value| {
            self.execute("COMMIT", Vec::new())?;
            Ok(value)
        });
        if result.is_err() && !self.connection.get_auto_commit() {
            // the error being reported says more than a failed rollback would
            let _ = self.execute("ROLLBACK", Vec::new());
        }
        result
    }

    /// Runs one statement to the end and returns how many rows it changed.
    fn execute(&self, sql: &str, params: Vec<Value>) -> Result<u64, StoreError> {
        let mut statement = self.prepare(sql, params)?;
        while step(&mut statement)? {}
        Ok(statement.n_change() as u64)
    }

    fn rows(&self, sql: &str, params: Vec<Value>) -> Result<Vec<Vec<Value>>, StoreError> {
        let mut statement = self.prepare(sql, params)?;
        let mut rows = Vec::new();
        while step(&mut statement)? {
            let row = statement.row().expect("a row after stepping to one");
            rows.push(row.get_values().cloned().collect());
        }
        Ok(rows)
    }

    fn script(&self, sql: &str) -> Result<(), StoreError> {
        let mut rest = sql;
        while let Some((mut statement, end)) = self.connection.consume_stmt(rest)? {
            while step(&mut statement)? {}
            rest = &rest[end..];
        }
        Ok(())
    }

    fn prepare(&self, sql: &str, params: Vec<Value>) -> Result<Statement, StoreError> {
        let mut statement = self.connection.prepare(sql)?;
        for (index, value) in params.into_iter().enumerate() {
            statement.bind_at(NonZero::new(index + 1).expect("one or more"), value)?;
        }
        Ok(statement)
    }
}

/// Steps to the next row: true if there is one, false once done. A wait the
/// busy handler asks for is slept, never spun.
fn step(statement: &mut Statement) -> Result<bool, StoreError> {
    loop {
        match statement.step()? {
            StepResult::Row => return Ok(true),
            StepResult::Done => return Ok(false),
            StepResult::IO | StepResult::Yield => statement._io().step()?,
            StepResult::Sleep { duration } => {
                thread::sleep(duration);
                statement._io().step()?;
            }
            StepResult::Busy => return Err(StoreError::Busy),
            StepResult::Interrupt => {
                return Err(StoreError::Sql("the statement was interrupted".into()))
            }
        }
    }
}

// Values ////////////////////////////
//////////////////////////////////////

fn text(value: impl Into<String>) -> Value {
    Value::build_text(value.into())
}

fn integer(value: i64) -> Value {
    Value::Numeric(Numeric::Integer(value))
}

fn int_at(row: &[Value], index: usize) -> Result<i64, StoreError> {
    row.get(index)
        .and_then(|value| value.as_int())
        .ok_or_else(|| StoreError::Corrupt(format!("column {} is not an integer", index)))
}

fn text_at(row: &[Value], index: usize) -> Result<String, StoreError> {
    row.get(index)
        .and_then(|value| value.to_text())
        .map(|value| value.to_string())
        .ok_or_else(|| StoreError::Corrupt(format!("column {} is not text", index)))
}

fn blob_at(row: &[Value], index: usize) -> Result<&[u8], StoreError> {
    row.get(index)
        .and_then(|value| value.to_blob())
        .ok_or_else(|| StoreError::Corrupt(format!("column {} is not a blob", index)))
}

fn session_record(row: &[Value]) -> Result<SessionRecord, StoreError> {
    let status = text_at(row, 2)?;
    Ok(SessionRecord {
        id: int_at(row, 0)?,
        owner: text_at(row, 1)?,
        status: SessionStatus::parse(&status)
            .ok_or_else(|| StoreError::Corrupt(format!("unknown status `{}`", status)))?,
        attempts: int_at(row, 3)?,
        error: row
            .get(4)
            .and_then(|value| value.to_text())
            .map(String::from),
    })
}

fn job_record(row: &[Value]) -> Result<JobRecord, StoreError> {
    let status = text_at(row, 3)?;
    Ok(JobRecord {
        id: int_at(row, 0)?,
        session_id: int_at(row, 1)?,
        owner: text_at(row, 2)?,
        status: JobStatus::parse(&status)
            .ok_or_else(|| StoreError::Corrupt(format!("unknown job status `{}`", status)))?,
        skills: SkillsSaved {
            added: int_at(row, 4)?.max(0) as usize,
            updated: int_at(row, 5)?.max(0) as usize,
        },
        spend: spend_at(row, 6)?,
        error: row
            .get(9)
            .and_then(|value| value.to_text())
            .map(String::from),
    })
}

/// Reads `tokens, cost_nanos, unpriced` starting at column `start`.
fn spend_at(row: &[Value], start: usize) -> Result<Spend, StoreError> {
    Ok(Spend::new(
        int_at(row, start)?.max(0) as u64,
        int_at(row, start + 1)?,
        int_at(row, start + 2)?.clamp(0, i64::from(u32::MAX)) as u32,
    ))
}

/// Summed here rather than in sql, where an overflowing sum is an error.
fn sum_spend(rows: &[Vec<Value>]) -> Result<Spend, StoreError> {
    let mut total = Spend::default();
    for row in rows {
        total.merge(&spend_at(row, 0)?);
    }
    Ok(total)
}

fn clamp_u64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn schema_number() -> i64 {
    SCHEMA_VERSION
        .parse()
        .expect("the schema version is a number")
}

fn encode_messages(messages: &[Message]) -> Result<Vec<String>, StoreError> {
    messages
        .iter()
        .map(|message| {
            serde_json::to_string(message).map_err(|err| StoreError::Corrupt(err.to_string()))
        })
        .collect()
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs() as i64)
        .unwrap_or(0)
}

// Vectors ///////////////////////////
//////////////////////////////////////

/// Little-endian f32s, back to back, which is also how turso reads a vector
/// out of a blob.
pub fn encode_vector(vector: &[f32]) -> Vec<u8> {
    vector
        .iter()
        .flat_map(|value| value.to_le_bytes())
        .collect()
}

pub fn decode_vector(bytes: &[u8]) -> Result<Vec<f32>, StoreError> {
    if !bytes.len().is_multiple_of(4) {
        return Err(StoreError::Corrupt(
            "an embedding is not a whole number of f32s".into(),
        ));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

/// `None` when the two cannot be compared: different lengths, or a vector
/// with no direction.
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> Option<f64> {
    if a.len() != b.len() || a.is_empty() {
        return None;
    }
    let (mut dot, mut norm_a, mut norm_b) = (0.0f64, 0.0f64, 0.0f64);
    for (x, y) in a.iter().zip(b) {
        let (x, y) = (*x as f64, *y as f64);
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }
    let similarity = dot / (norm_a.sqrt() * norm_b.sqrt());
    if similarity.is_nan() || similarity.is_infinite() {
        return None;
    }
    Some(similarity)
}

/// The vector `model` gives `text`.
pub fn embed(openrouter: &OpenRouter, model: &str, text: &str) -> Result<Vec<f32>, String> {
    embed_with_usage(openrouter, model, text).0
}

/// The same vector, with what the request used. Usage comes back even when
/// the response held no vector.
pub fn embed_with_usage(
    openrouter: &OpenRouter,
    model: &str,
    text: &str,
) -> (Result<Vec<f32>, String>, Option<Usage>) {
    match openrouter.embeddings(EmbeddingRequest::new(model, vec![text.to_string()])) {
        Err(err) => (Err(err.to_string()), None),
        Ok(response) => {
            let embedding = response
                .data
                .into_iter()
                .next()
                .map(|data| data.embedding)
                .ok_or_else(|| "no embedding came back".to_string());
            (embedding, response.usage)
        }
    }
}
