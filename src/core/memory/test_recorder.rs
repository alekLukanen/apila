use std::sync::Mutex;

use crate::core::openrouter::types::{FunctionCall, Message, Spend, ToolCall};
use crate::core::test_support::scratch_dir;

use super::recorder::{JobState, MemoryJob, MemoryStatus, Messages, SessionRecorder};
use super::store::{memory_path, MemoryStore, SessionStatus, SkillsSaved};

fn recorder(dir: &std::path::Path) -> SessionRecorder {
    SessionRecorder::new(dir.to_path_buf(), "owner".into(), MemoryStatus::default())
}

#[test]
fn each_message_is_stored_once_in_order() {
    let dir = scratch_dir();
    let mut recorder = recorder(&dir);
    let messages = Mutex::new(vec![Message::user("one")]);

    recorder.sync(&messages);
    messages
        .lock()
        .unwrap()
        .extend([Message::assistant("two"), Message::user("three")]);
    recorder.sync(&messages);
    // nothing new: nothing written again
    recorder.sync(&messages);

    let store = MemoryStore::open_existing(&memory_path(&dir))
        .expect("open")
        .expect("the recorder created it");
    let id = recorder.session_id().expect("a session was begun");
    let stored = store.session_messages(id).expect("read");
    let contents: Vec<_> = stored.iter().map(|m| m.content().unwrap_or("")).collect();
    assert_eq!(contents, vec!["one", "two", "three"]);
}

#[test]
fn a_session_with_nothing_said_writes_nothing() {
    let dir = scratch_dir();
    let mut recorder = recorder(&dir);

    recorder.sync(&Mutex::new(Vec::<Message>::new()));
    recorder.end(true).expect("end");

    assert!(recorder.session_id().is_none());
    assert!(!memory_path(&dir).exists());
}

#[test]
fn a_tool_call_reads_back_as_it_was_recorded() {
    let dir = scratch_dir();
    let mut recorder = recorder(&dir);
    let call = ToolCall {
        id: "call-1".into(),
        call_type: "function".into(),
        function: FunctionCall {
            name: "sqlite".into(),
            arguments: r#"{"action":"list"}"#.into(),
        },
    };
    recorder.sync(&Mutex::new(vec![
        Message::user("list them"),
        Message::assistant_tool_calls(vec![call]),
        Message::tool("call-1", "(no databases)"),
    ]));

    let store = recorder.store().expect("open");
    let stored = store
        .session_messages(recorder.session_id().expect("a session"))
        .expect("read");
    assert_eq!(stored[1].tool_calls()[0].function.name, "sqlite");
    assert_eq!(
        stored[1].tool_calls()[0].function.arguments,
        r#"{"action":"list"}"#
    );
    assert!(matches!(&stored[2], Message::Tool { tool_call_id, .. } if tool_call_id == "call-1"));
}

#[test]
fn ending_marks_the_session_for_analysis_or_not() {
    let dir = scratch_dir();
    let mut analysed = recorder(&dir);
    analysed.sync(&Mutex::new(vec![Message::user("one")]));
    analysed.end(true).expect("end");

    let mut kept = recorder(&dir);
    kept.sync(&Mutex::new(vec![Message::user("two")]));
    kept.end(false).expect("end");

    let store = kept.store().expect("open");
    let sessions = store.sessions().expect("sessions");
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].status, SessionStatus::Pending);
    assert_eq!(sessions[1].status, SessionStatus::Ended);
    assert_eq!(sessions[0].owner, "owner");
}

#[test]
fn a_failure_is_shown_and_cleared_once_recording_works_again() {
    let dir = scratch_dir();
    // a file where the memory directory should be keeps the store from opening
    std::fs::write(dir.join("memory"), "not a directory").expect("write");
    let status = MemoryStatus::default();
    let mut recorder = SessionRecorder::new(dir.clone(), "owner".into(), status.clone());
    let messages = Mutex::new(vec![Message::user("one")]);

    recorder.sync(&messages);
    assert!(status
        .get()
        .is_some_and(|status| status.contains("recording the session failed")));

    std::fs::remove_file(dir.join("memory")).expect("remove");
    recorder.sync(&messages);
    assert_eq!(status.get(), None);
    assert!(recorder.session_id().is_some());
}

// Memory status /////////////////////
//////////////////////////////////////

#[test]
fn a_job_moves_from_queued_to_running_to_done() {
    let status = MemoryStatus::default();
    let shared = status.clone();

    status.session_queued();
    assert_eq!(shared.snapshot().pending, Some(1));

    status.job_started(7);
    let view = shared.snapshot();
    assert_eq!(view.pending, Some(0));
    assert!(view.job_running());
    assert_eq!(view.last_job.expect("a job").session_id, 7);

    let done = JobState::Done {
        skills: SkillsSaved {
            added: 1,
            updated: 1,
        },
    };
    status.job_finished(done.clone());
    let view = shared.snapshot();
    assert!(!view.job_running());
    assert!(!view.stalled());
    assert_eq!(view.last_job.expect("a job").state, done);

    status.job_started(8);
    status.job_finished(JobState::Failed("boom".into()));
    assert!(shared.snapshot().stalled());
}

#[test]
fn spent_deltas_add_up_on_the_job_and_on_the_process_total() {
    let status = MemoryStatus::default();
    status.job_started(1);
    status.job_spent(&Spend::new(10, 100, 0));
    status.job_spent(&Spend::new(5, 50, 1));
    status.job_finished(JobState::Done {
        skills: SkillsSaved::default(),
    });
    status.job_started(2);
    status.job_spent(&Spend::new(1, 1, 0));

    let view = status.snapshot();
    assert_eq!(view.last_job.expect("a job").spend, Spend::new(1, 1, 0));
    assert_eq!(view.jobs_live, Spend::new(16, 151, 1));
}

#[test]
fn loading_keeps_what_this_process_already_set() {
    let status = MemoryStatus::default();
    status.set_skills(4);
    status.set_pending(1);
    status.job_started(9);
    status.job_spent(&Spend::new(1, 2, 0));

    let stored = MemoryJob {
        session_id: 3,
        state: JobState::Interrupted,
        spend: Spend::new(50, 500, 0),
    };
    status.load(Spend::new(100, 1000, 0), 2, Some(stored), Some(5));

    let view = status.snapshot();
    assert!(view.loaded);
    assert_eq!(view.skills, Some(4));
    assert_eq!(view.last_job.as_ref().expect("a job").session_id, 9);
    // the job that started took the one session pending before the load
    assert_eq!(view.pending, Some(0));
    assert_eq!(view.memory_spend(), Spend::new(101, 1002, 0));
}

#[test]
fn loading_an_untouched_status_takes_everything() {
    let status = MemoryStatus::default();
    let stored = MemoryJob {
        session_id: 3,
        state: JobState::Interrupted,
        spend: Spend::new(50, 500, 0),
    };
    status.load(Spend::new(100, 1000, 0), 2, Some(stored.clone()), Some(5));

    let view = status.snapshot();
    assert_eq!(view.skills, Some(2));
    assert_eq!(view.pending, Some(5));
    assert_eq!(view.last_job, Some(stored));
    assert_eq!(view.memory_spend(), Spend::new(100, 1000, 0));
}

// Recording costs ///////////////////
//////////////////////////////////////

/// A conversation that carries a cost, as an agent's does.
struct Costed {
    messages: Vec<Message>,
    spend: Spend,
}

impl Messages for Costed {
    fn messages(&self) -> &[Message] {
        &self.messages
    }
    fn spend(&self) -> Spend {
        self.spend
    }
}

fn costed(messages: Vec<Message>, spend: Spend) -> Mutex<Costed> {
    Mutex::new(Costed { messages, spend })
}

fn stored_spend(dir: &std::path::Path, recorder: &SessionRecorder) -> Option<Spend> {
    MemoryStore::open_existing(&memory_path(dir))
        .expect("open")
        .expect("exists")
        .session_spend(recorder.session_id().expect("a session"))
        .expect("read")
}

#[test]
fn a_cost_with_no_new_message_is_still_stored() {
    let dir = scratch_dir();
    let mut recorder = recorder(&dir);
    let conversation = costed(vec![Message::user("one")], Spend::new(10, 100, 0));
    recorder.sync(&conversation);
    assert_eq!(stored_spend(&dir, &recorder), Some(Spend::new(10, 100, 0)));

    // a reply with no choices costs something and adds no message
    conversation.lock().unwrap().spend = Spend::new(20, 200, 0);
    recorder.sync(&conversation);
    assert_eq!(stored_spend(&dir, &recorder), Some(Spend::new(20, 200, 0)));
}

#[test]
fn a_cost_alone_never_begins_a_session() {
    let dir = scratch_dir();
    let mut recorder = recorder(&dir);

    recorder.sync(&costed(Vec::new(), Spend::new(10, 100, 0)));

    assert!(recorder.session_id().is_none());
    assert!(!memory_path(&dir).exists());
}

#[test]
fn an_unchanged_conversation_is_not_written_again() {
    let dir = scratch_dir();
    let mut recorder = recorder(&dir);
    let conversation = costed(vec![Message::user("one")], Spend::new(10, 100, 0));
    recorder.sync(&conversation);

    // overwritten behind the recorder's back, so a second write would show
    let id = recorder.session_id().expect("a session");
    let other = MemoryStore::open_existing(&memory_path(&dir))
        .expect("open")
        .expect("exists");
    other
        .record(id, 1, &[], &Spend::new(99, 999, 0))
        .expect("record");

    recorder.sync(&conversation);
    assert_eq!(stored_spend(&dir, &recorder), Some(Spend::new(99, 999, 0)));
}

#[test]
fn a_sync_after_a_failed_write_stores_the_whole_total() {
    let dir = scratch_dir();
    std::fs::write(dir.join("memory"), "not a directory").expect("write");
    let status = MemoryStatus::default();
    let mut recorder = SessionRecorder::new(dir.clone(), "owner".into(), status.clone());
    let conversation = costed(vec![Message::user("one")], Spend::new(10, 100, 0));

    recorder.sync(&conversation);
    assert!(status.get().is_some());

    std::fs::remove_file(dir.join("memory")).expect("remove");
    {
        let mut conversation = conversation.lock().unwrap();
        conversation.messages.push(Message::assistant("two"));
        conversation.spend = Spend::new(30, 300, 1);
    }
    recorder.sync(&conversation);

    assert_eq!(status.get(), None);
    assert_eq!(stored_spend(&dir, &recorder), Some(Spend::new(30, 300, 1)));
}

#[test]
fn clearing_by_prefix_leaves_another_parts_error() {
    let status = MemoryStatus::default();
    status.set("memory failed: boom");

    status.clear_prefixed("recording the session failed");
    assert_eq!(status.get().as_deref(), Some("memory failed: boom"));

    status.clear_prefixed("memory failed");
    assert_eq!(status.get(), None);
}

#[test]
fn a_session_queued_after_a_failure_is_no_longer_stalled() {
    let status = MemoryStatus::default();
    status.set_pending(1);
    status.job_started(1);
    status.job_finished(JobState::Failed("boom".into()));
    status.set_pending(1);
    assert!(status.snapshot().stalled());

    status.session_queued();
    let view = status.snapshot();
    assert!(view.queued);
    assert!(!view.stalled());
    assert_eq!(view.pending, Some(2));

    status.job_started(1);
    assert!(!status.snapshot().queued);
    status.job_finished(JobState::Failed("again".into()));
    assert!(status.snapshot().stalled());
}
