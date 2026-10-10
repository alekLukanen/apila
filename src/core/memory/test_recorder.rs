use std::sync::Mutex;

use crate::core::openrouter::types::{FunctionCall, Message, ToolCall};
use crate::core::test_support::scratch_dir;

use super::recorder::{MemoryStatus, SessionRecorder};
use super::store::{memory_path, MemoryStore, SessionStatus};

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
