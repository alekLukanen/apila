use std::path::Path;
use std::sync::Arc;
use std::thread;

use turso_core::{Database, OpenFlags, OpenOptions, PlatformIO, SqliteDialect, StepResult};

use crate::core::openrouter::types::{Message, Spend};
use crate::core::test_support::scratch_dir;

use super::store::{
    cosine_similarity, decode_vector, encode_vector, memory_path, JobStatus, MemoryStore, NewSkill,
    SessionStatus, SkillsSaved, MAX_ATTEMPTS,
};

const MODEL: &str = "openai/text-embedding-3-small";

fn store() -> MemoryStore {
    MemoryStore::create(&memory_path(&scratch_dir())).expect("create the store")
}

fn skill(name: &str, embedding: Vec<f32>) -> NewSkill {
    NewSkill {
        name: name.into(),
        when_to_use: format!("when {} is needed", name),
        body: format!("## Steps\n{}", name),
        embedding,
        embedding_model: MODEL.into(),
        source_session: None,
    }
}

fn status(store: &MemoryStore, id: i64) -> SessionStatus {
    store
        .session(id)
        .expect("read the session")
        .expect("the session exists")
        .status
}

#[test]
fn opening_twice_keeps_what_was_stored() {
    let path = memory_path(&scratch_dir());
    {
        let store = MemoryStore::create(&path).expect("create the store");
        store.begin_session("one").expect("begin a session");
    }

    let store = MemoryStore::create(&path).expect("open the store again");
    assert_eq!(store.sessions().expect("list sessions").len(), 1);
}

#[test]
fn a_missing_database_is_not_created_by_open_existing() {
    let path = memory_path(&scratch_dir());

    assert!(MemoryStore::open_existing(&path)
        .expect("look for the store")
        .is_none());
    assert!(!path.exists());
}

#[test]
fn a_vector_survives_being_encoded() {
    let vector = vec![0.5, -1.25, 3.0e-8, f32::MAX];
    let bytes = encode_vector(&vector);

    assert_eq!(bytes.len(), 16);
    assert_eq!(decode_vector(&bytes).expect("decode"), vector);
    assert!(decode_vector(&bytes[..3]).is_err());
}

#[test]
fn similarity_of_vectors_with_no_direction_is_not_measured() {
    assert_eq!(cosine_similarity(&[1.0, 0.0], &[2.0, 0.0]), Some(1.0));
    assert_eq!(cosine_similarity(&[0.0, 0.0], &[1.0, 0.0]), None);
    assert_eq!(cosine_similarity(&[1.0], &[1.0, 0.0]), None);
}

#[test]
fn messages_come_back_in_the_order_they_were_recorded() {
    let store = store();
    let id = store.begin_session("owner").expect("begin");
    store
        .append_messages(id, 0, &[Message::user("one"), Message::assistant("two")])
        .expect("append");
    store
        .append_messages(id, 2, &[Message::tool("call-1", "three")])
        .expect("append");

    let messages = store.session_messages(id).expect("read");
    let contents: Vec<_> = messages.iter().map(|m| m.content().unwrap_or("")).collect();
    assert_eq!(contents, vec!["one", "two", "three"]);
    assert!(matches!(messages[2], Message::Tool { .. }));
}

#[test]
fn search_orders_by_similarity_and_leaves_out_what_is_too_far() {
    let store = store();
    store
        .upsert_skill(&skill("exact", vec![1.0, 0.0, 0.0]))
        .expect("save");
    store
        .upsert_skill(&skill("close", vec![0.9, 0.1, 0.0]))
        .expect("save");
    store
        .upsert_skill(&skill("far", vec![0.0, 0.0, 1.0]))
        .expect("save");
    store
        .upsert_skill(&skill("opposite", vec![-1.0, 0.0, 0.0]))
        .expect("save");

    let found = store
        .search(&[1.0, 0.0, 0.0], MODEL, 0.25, 10)
        .expect("search");
    let names: Vec<_> = found.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, vec!["exact", "close"]);
    assert!((found[0].similarity - 1.0).abs() < 1e-6);

    let limited = store
        .search(&[1.0, 0.0, 0.0], MODEL, -1.0, 3)
        .expect("search");
    assert_eq!(limited.len(), 3);
    assert_eq!(limited[2].name, "far");
}

#[test]
fn search_ignores_skills_from_another_model_or_size() {
    let store = store();
    store
        .upsert_skill(&skill("same", vec![1.0, 0.0]))
        .expect("save");
    let mut other_model = skill("other-model", vec![1.0, 0.0]);
    other_model.embedding_model = "someone/else".into();
    store.upsert_skill(&other_model).expect("save");
    store
        .upsert_skill(&skill("other-size", vec![1.0, 0.0, 0.0]))
        .expect("save");

    let found = store.search(&[1.0, 0.0], MODEL, 0.0, 10).expect("search");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].name, "same");
    assert_eq!(store.count_unsearchable(MODEL, 2).expect("count"), 2);
}

#[test]
fn saving_a_skill_under_its_name_again_updates_it_in_place() {
    let store = store();
    let (id, created) = store
        .upsert_skill(&skill("fetch", vec![1.0, 0.0]))
        .expect("save");
    assert!(created);

    let mut update = skill("fetch", vec![0.0, 1.0]);
    update.body = "## Steps\nnew".into();
    let (again, created) = store.upsert_skill(&update).expect("save again");

    assert_eq!(again, id);
    assert!(!created);
    let stored = store.get(id).expect("get").expect("the skill exists");
    assert_eq!(stored.body, "## Steps\nnew");
    assert_eq!(store.list_skills(10).expect("list").len(), 1);
    assert!(store.get(id + 100).expect("get").is_none());
}

#[test]
fn an_ended_session_is_pending_only_when_it_is_to_be_analysed() {
    let store = store();
    let kept = store.begin_session("owner").expect("begin");
    let analysed = store.begin_session("owner").expect("begin");

    store.end_session(kept, false).expect("end");
    store.end_session(analysed, true).expect("end");

    assert_eq!(status(&store, kept), SessionStatus::Ended);
    assert_eq!(status(&store, analysed), SessionStatus::Pending);
}

#[test]
fn a_pending_session_is_claimed_once() {
    let store = store();
    let id = store.begin_session("owner").expect("begin");
    store.end_session(id, true).expect("end");

    let (claimed, _) = store
        .claim_pending("writer")
        .expect("claim")
        .expect("a session");
    assert_eq!(claimed, id);
    assert_eq!(store.claim_pending("writer").expect("claim"), None);
    let record = store.session(id).expect("read").expect("exists");
    assert_eq!(record.status, SessionStatus::Analysing);
    assert_eq!(record.attempts, 1);
}

#[test]
fn recovery_ends_only_what_other_processes_left_behind() {
    let store = store();
    let old = store.begin_session("old-process").expect("begin");
    let current = store.begin_session("this-process").expect("begin");

    assert_eq!(store.recover("this-process", true).expect("recover"), 1);
    assert_eq!(status(&store, old), SessionStatus::Pending);
    assert_eq!(status(&store, current), SessionStatus::Active);

    let unanalysed = store.begin_session("old-process").expect("begin");
    store.recover("this-process", false).expect("recover");
    assert_eq!(status(&store, unanalysed), SessionStatus::Ended);
}

#[test]
fn a_session_that_keeps_failing_is_given_up_on() {
    let store = store();
    let id = store.begin_session("owner").expect("begin");
    store.end_session(id, true).expect("end");

    for attempt in 1..=MAX_ATTEMPTS {
        let (claimed, job) = store
            .claim_pending("writer")
            .expect("claim")
            .expect("a session");
        assert_eq!(claimed, id);
        let status = store
            .finish_session(id, job, Err("boom".into()), &Spend::default())
            .expect("finish");
        let expected = if attempt < MAX_ATTEMPTS {
            SessionStatus::Pending
        } else {
            SessionStatus::Failed
        };
        assert_eq!(status, expected);
    }

    let record = store.session(id).expect("read").expect("exists");
    assert_eq!(record.error.as_deref(), Some("boom"));
    assert_eq!(store.claim_pending("writer").expect("claim"), None);
}

#[test]
fn an_interrupted_analysis_is_retried_by_the_next_process() {
    let store = store();
    let id = store.begin_session("old").expect("begin");
    store.end_session(id, true).expect("end");
    store.claim_pending("old").expect("claim");

    store.recover("new", true).expect("recover");
    assert_eq!(status(&store, id), SessionStatus::Pending);

    let (_, job) = store
        .claim_pending("new")
        .expect("claim")
        .expect("a session");
    store
        .finish_session(id, job, Ok(SkillsSaved::default()), &Spend::default())
        .expect("finish");
    assert_eq!(status(&store, id), SessionStatus::Analysed);
}

/// The agent's thread and the writer each hold a connection; their writes
/// have to wait for one another rather than fail.
#[test]
fn two_connections_can_write_at_the_same_time() {
    let path = memory_path(&scratch_dir());
    let first = Arc::new(MemoryStore::create(&path).expect("create"));
    let second = MemoryStore::open_existing(&path)
        .expect("open")
        .expect("exists");

    let writer = {
        let first = Arc::clone(&first);
        thread::spawn(move || {
            for index in 0..50 {
                let id = first.begin_session("a").expect("begin");
                first
                    .append_messages(id, 0, &[Message::user(format!("a{}", index))])
                    .expect("append");
            }
        })
    };
    for index in 0..50 {
        second
            .upsert_skill(&skill(&format!("skill-{}", index), vec![1.0, 0.0]))
            .expect("save");
    }
    writer.join().expect("the other writer finished");

    assert_eq!(first.sessions().expect("sessions").len(), 50);
    assert_eq!(first.list_skills(100).expect("skills").len(), 50);
}

// Costs /////////////////////////////
//////////////////////////////////////

/// The schema as the first release wrote it, before costs were kept.
const SCHEMA_V1: &str = "
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE sessions (
  id INTEGER PRIMARY KEY,
  owner TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('active','ended','pending','analysing','analysed','failed')),
  started_at INTEGER NOT NULL,
  ended_at INTEGER,
  analysed_at INTEGER,
  attempts INTEGER NOT NULL DEFAULT 0,
  error TEXT
);
CREATE TABLE session_messages (
  session_id INTEGER NOT NULL REFERENCES sessions(id),
  seq INTEGER NOT NULL,
  message TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (session_id, seq)
);
CREATE TABLE memories (
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
INSERT INTO meta (key, value) VALUES ('schema_version', '1');
INSERT INTO sessions (owner, status, started_at) VALUES ('old', 'ended', 0);
";

/// The tables the second release added, before updated skills were counted
/// apart, with one finished job.
const SCHEMA_V2_ADDITIONS: &str = "
CREATE TABLE session_costs (
  session_id INTEGER PRIMARY KEY REFERENCES sessions(id),
  tokens INTEGER NOT NULL DEFAULT 0,
  cost_nanos INTEGER NOT NULL DEFAULT 0,
  unpriced INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE memory_jobs (
  id INTEGER PRIMARY KEY,
  session_id INTEGER NOT NULL REFERENCES sessions(id),
  owner TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('running','done','failed','interrupted')),
  skills_saved INTEGER NOT NULL DEFAULT 0,
  tokens INTEGER NOT NULL DEFAULT 0,
  cost_nanos INTEGER NOT NULL DEFAULT 0,
  unpriced INTEGER NOT NULL DEFAULT 0,
  error TEXT,
  started_at INTEGER NOT NULL,
  finished_at INTEGER
);
UPDATE meta SET value = '2' WHERE key = 'schema_version';
INSERT INTO memory_jobs (session_id, owner, status, skills_saved, tokens, started_at)
  VALUES (1, 'old', 'done', 2, 40, 0);
";

/// Writes a database the way an older release did, without going through
/// the store, which would upgrade it.
fn write_old_database(path: &Path, script: &str) {
    std::fs::create_dir_all(path.parent().expect("a parent")).expect("create dir");
    let io = Arc::new(PlatformIO::new().expect("io"));
    let database = Database::open(
        io,
        path.to_str().expect("utf-8"),
        OpenOptions::new(Arc::new(SqliteDialect)).flags(OpenFlags::Create),
    )
    .expect("open");
    let connection = database.connect().expect("connect");
    let mut rest = script;
    while let Some((mut statement, end)) = connection.consume_stmt(rest).expect("parse") {
        loop {
            match statement.step().expect("step") {
                StepResult::Done => break,
                StepResult::IO | StepResult::Yield => statement._io().step().expect("io"),
                StepResult::Row => {}
                other => panic!("unexpected step result: {:?}", other),
            }
        }
        rest = &rest[end..];
    }
}

#[test]
fn a_first_release_database_is_upgraded_in_place() {
    let path = memory_path(&scratch_dir());
    write_old_database(&path, SCHEMA_V1);

    let store = MemoryStore::open_existing(&path)
        .expect("open")
        .expect("exists");

    assert_eq!(
        store.schema_version().expect("version").as_deref(),
        Some("3")
    );
    assert_eq!(store.sessions().expect("sessions").len(), 1);
    assert_eq!(store.session_totals(&[]).expect("totals"), Spend::default());
    assert_eq!(store.memory_totals("me").expect("totals"), Spend::default());
    assert_eq!(store.last_job().expect("last job"), None);
    // a session an older binary began takes a cost all the same
    store
        .record(1, 0, &[Message::user("hi")], &Spend::new(5, 7, 0))
        .expect("record");
    assert_eq!(
        store.session_spend(1).expect("spend"),
        Some(Spend::new(5, 7, 0))
    );
}

#[test]
fn a_second_release_job_table_gains_the_updated_count() {
    let path = memory_path(&scratch_dir());
    write_old_database(&path, &format!("{}{}", SCHEMA_V1, SCHEMA_V2_ADDITIONS));

    let store = MemoryStore::open_existing(&path)
        .expect("open")
        .expect("exists");

    assert_eq!(
        store.schema_version().expect("version").as_deref(),
        Some("3")
    );
    // what the older release counted as saved is read back as added
    let old = store.last_job().expect("last job").expect("a job");
    assert_eq!(
        old.skills,
        SkillsSaved {
            added: 2,
            updated: 0
        }
    );
    assert_eq!(old.spend, Spend::new(40, 0, 0));

    let session = store.begin_session("me").expect("begin");
    store.end_session(session, true).expect("end");
    let (id, job) = store
        .claim_pending("me")
        .expect("claim")
        .expect("a session");
    let skills = SkillsSaved {
        added: 0,
        updated: 3,
    };
    store
        .finish_session(id, job, Ok(skills), &Spend::default())
        .expect("finish");
    assert_eq!(store.last_job().unwrap().unwrap().skills, skills);
    drop(store);

    // opening it again leaves the column it already has
    let again = MemoryStore::open_existing(&path)
        .expect("open again")
        .expect("exists");
    assert_eq!(again.last_job().unwrap().unwrap().skills, skills);
}

#[test]
fn recording_stores_the_total_so_far_rather_than_adding_to_it() {
    let store = store();
    let id = store.begin_session("owner").expect("begin");

    store
        .record(id, 0, &[Message::user("one")], &Spend::new(10, 100, 0))
        .expect("record");
    store
        .record(id, 1, &[Message::assistant("two")], &Spend::new(30, 250, 1))
        .expect("record");
    // the cost alone, with no new messages
    store
        .record(id, 2, &[], &Spend::new(40, 300, 1))
        .expect("record");

    assert_eq!(
        store.session_spend(id).expect("spend"),
        Some(Spend::new(40, 300, 1))
    );
    assert_eq!(store.session_messages(id).expect("messages").len(), 2);

    let other = store.begin_session("owner").expect("begin");
    store
        .record(other, 0, &[Message::user("x")], &Spend::new(1, 2, 0))
        .expect("record");
    assert_eq!(
        store.session_totals(&[]).expect("totals"),
        Spend::new(41, 302, 1)
    );
    assert_eq!(
        store.session_totals(&[id]).expect("totals"),
        Spend::new(1, 2, 0)
    );
}

#[test]
fn a_job_records_what_its_analysis_cost() {
    let store = store();
    let id = store.begin_session("owner").expect("begin");
    store.end_session(id, true).expect("end");

    let (claimed, job) = store
        .claim_pending("writer")
        .expect("claim")
        .expect("a session");
    assert_eq!(claimed, id);
    let running = store.last_job().expect("last job").expect("a job");
    assert_eq!(running.status, JobStatus::Running);
    assert_eq!(running.session_id, id);
    assert_eq!(running.owner, "writer");

    store
        .set_job_spend(job, &Spend::new(10, 20, 0))
        .expect("spend");
    assert_eq!(
        store.last_job().expect("last job").expect("a job").spend,
        Spend::new(10, 20, 0)
    );

    store
        .finish_session(
            id,
            job,
            Ok(SkillsSaved {
                added: 2,
                updated: 1,
            }),
            &Spend::new(15, 35, 1),
        )
        .expect("finish");
    let done = store.last_job().expect("last job").expect("a job");
    assert_eq!(done.status, JobStatus::Done);
    assert_eq!(
        done.skills,
        SkillsSaved {
            added: 2,
            updated: 1
        }
    );
    assert_eq!(done.spend, Spend::new(15, 35, 1));
    assert_eq!(done.error, None);
}

#[test]
fn a_failed_job_keeps_its_error_and_cost() {
    let store = store();
    let id = store.begin_session("owner").expect("begin");
    store.end_session(id, true).expect("end");
    let (_, job) = store
        .claim_pending("writer")
        .expect("claim")
        .expect("a session");

    store
        .finish_session(id, job, Err("boom".into()), &Spend::new(3, 4, 0))
        .expect("finish");

    let failed = store.last_job().expect("last job").expect("a job");
    assert_eq!(failed.status, JobStatus::Failed);
    assert_eq!(failed.error.as_deref(), Some("boom"));
    assert_eq!(failed.spend, Spend::new(3, 4, 0));
}

#[test]
fn memory_totals_leave_out_one_owners_jobs_and_last_job_is_the_newest() {
    let store = store();
    for owner in ["old", "this", "old"] {
        let id = store.begin_session("owner").expect("begin");
        store.end_session(id, true).expect("end");
        let (_, job) = store
            .claim_pending(owner)
            .expect("claim")
            .expect("a session");
        store
            .finish_session(id, job, Ok(SkillsSaved::default()), &Spend::new(1, 10, 0))
            .expect("finish");
    }

    assert_eq!(
        store.memory_totals("this").expect("totals"),
        Spend::new(2, 20, 0)
    );
    assert_eq!(
        store.memory_totals("nobody").expect("totals"),
        Spend::new(3, 30, 0)
    );
    let last = store.last_job().expect("last job").expect("a job");
    assert_eq!(last.owner, "old");
    assert_eq!(last.session_id, 3);
}

#[test]
fn recovery_marks_other_processes_running_jobs_interrupted_and_keeps_their_cost() {
    let store = store();
    let mut jobs = Vec::new();
    for owner in ["old", "this"] {
        let id = store.begin_session(owner).expect("begin");
        store.end_session(id, true).expect("end");
        let (_, job) = store
            .claim_pending(owner)
            .expect("claim")
            .expect("a session");
        store
            .set_job_spend(job, &Spend::new(5, 50, 0))
            .expect("spend");
        jobs.push(job);
    }

    store.recover("this", true).expect("recover");

    let status = |job: i64| store.job(job).expect("read").expect("exists").status;
    assert_eq!(status(jobs[0]), JobStatus::Interrupted);
    assert_eq!(status(jobs[1]), JobStatus::Running);
    assert_eq!(
        store.memory_totals("this").expect("totals"),
        Spend::new(5, 50, 0)
    );
}

#[test]
fn skills_are_counted() {
    let store = store();
    assert_eq!(store.count_skills().expect("count"), 0);

    store.upsert_skill(&skill("one", vec![1.0])).expect("save");
    assert_eq!(store.count_skills().expect("count"), 1);

    store
        .upsert_skill(&skill("one", vec![0.5]))
        .expect("save again");
    assert_eq!(store.count_skills().expect("count"), 1);
}
