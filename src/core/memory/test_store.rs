use std::sync::Arc;
use std::thread;

use crate::core::openrouter::types::Message;
use crate::core::test_support::scratch_dir;

use super::store::{
    cosine_similarity, decode_vector, encode_vector, memory_path, MemoryStore, NewSkill,
    SessionStatus, MAX_ATTEMPTS,
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

    assert_eq!(store.claim_pending("writer").expect("claim"), Some(id));
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
        assert_eq!(store.claim_pending("writer").expect("claim"), Some(id));
        let status = store
            .finish_session(id, Err("boom".into()))
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

    store.finish_session(id, Ok(())).expect("finish");
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
