use std::sync::Arc;

use crate::core::memory::store::{memory_path, MemoryStore, NewSkill};
use crate::core::openrouter::client::{OpenRouter, OpenRouterConfig};
use crate::core::test_support::{scratch_dir, Route, StubOpenRouter, StubReply};

use super::skills::{GetSkillTool, SearchSkillsTool};
use super::tool::{Tool, ToolContext, ToolError};

const MODEL: &str = "openai/text-embedding-3-small";

/// A store holding three skills along the axes of a 2d space.
fn store() -> Arc<MemoryStore> {
    let store = MemoryStore::create(&memory_path(&scratch_dir())).expect("create the store");
    for (name, embedding) in [
        ("fetch-webpage-into-sqlite", vec![1.0, 0.0]),
        ("summarise-a-table", vec![0.8, 0.6]),
        ("write-a-poem", vec![0.0, 1.0]),
    ] {
        store
            .upsert_skill(&NewSkill {
                name: name.into(),
                when_to_use: format!("The user asks to {}.", name.replace('-', " ")),
                body: format!("## Steps\n1. {}", name),
                embedding,
                embedding_model: MODEL.into(),
                source_session: None,
            })
            .expect("save a skill");
    }
    Arc::new(store)
}

/// A context whose embeddings all come back as `query`.
fn context(
    store: &Arc<MemoryStore>,
    query: Vec<f32>,
    config: serde_json::Value,
) -> (ToolContext, StubOpenRouter) {
    let server =
        StubOpenRouter::start_routed(vec![(Route::Embeddings, vec![StubReply::embedding(query)])]);
    let openrouter =
        OpenRouter::new(OpenRouterConfig::new("sk-or-test".into()).set_base_url(server.base_url()))
            .expect("build client");
    let context = ToolContext::new(scratch_dir(), config)
        .set_memory(Some(Arc::clone(store)))
        .set_openrouter(Some(Arc::new(openrouter)));
    (context, server)
}

fn search_config() -> serde_json::Value {
    serde_json::json!({"embedding_model": MODEL, "min_similarity": 0.25})
}

fn search(context: &ToolContext, arguments: serde_json::Value) -> Result<String, ToolError> {
    let mut state = SearchSkillsTool::new()
        .new_state(context)
        .expect("start the tool");
    state
        .run(context, &arguments)
        .map(|output| output.content())
}

fn get(context: &ToolContext, arguments: serde_json::Value) -> Result<String, ToolError> {
    let mut state = GetSkillTool::new()
        .new_state(context)
        .expect("start the tool");
    state
        .run(context, &arguments)
        .map(|output| output.content())
}

#[test]
fn search_lists_the_closest_skills_first() {
    let store = store();
    let (context, server) = context(&store, vec![1.0, 0.0], search_config());

    let content = search(
        &context,
        serde_json::json!({"request": "save hacker news into a database"}),
    )
    .expect("search");

    assert_eq!(
        content,
        "id | name | similarity | when to use\n\
         1 | fetch-webpage-into-sqlite | 1.00 | The user asks to fetch webpage into sqlite.\n\
         2 | summarise-a-table | 0.80 | The user asks to summarise a table.\n\
         2 skills at or above similarity 0.25; read one with get_skill"
    );
    let sent: serde_json::Value =
        serde_json::from_str(&server.embedding_requests()[0]).expect("json");
    assert_eq!(sent["model"], MODEL);
    assert_eq!(sent["input"][0], "save hacker news into a database");
}

#[test]
fn a_search_with_nothing_close_says_so() {
    let store = store();
    let (context, _server) = context(&store, vec![-1.0, -1.0], search_config());

    let content = search(&context, serde_json::json!({"request": "anything"})).expect("search");

    assert_eq!(content, "no skills at or above similarity 0.25");
}

#[test]
fn a_search_says_when_skills_were_stored_with_another_model() {
    let store = store();
    let (context, _server) = context(
        &store,
        vec![1.0, 0.0],
        serde_json::json!({"embedding_model": "someone/else"}),
    );

    let content = search(&context, serde_json::json!({"request": "anything"})).expect("search");

    assert_eq!(
        content,
        "no skills at or above similarity 0.25; 3 skills stored with another embedding model"
    );
}

#[test]
fn the_limit_has_a_default_a_cap_and_must_be_positive() {
    let store = store();
    let (context, _server) = context(
        &store,
        vec![1.0, 0.0],
        serde_json::json!({"embedding_model": MODEL, "min_similarity": -1.0}),
    );

    let rows = |content: String| content.lines().count() - 2;
    let all = search(&context, serde_json::json!({"request": "x"})).expect("search");
    assert_eq!(rows(all), 3);
    let one = search(&context, serde_json::json!({"request": "x", "limit": 1})).expect("search");
    assert_eq!(rows(one), 1);
    let capped =
        search(&context, serde_json::json!({"request": "x", "limit": 500})).expect("search");
    assert_eq!(rows(capped), 3);

    for limit in [
        serde_json::json!(0),
        serde_json::json!(-2),
        serde_json::json!(1.5),
        serde_json::json!("3"),
    ] {
        let err = search(
            &context,
            serde_json::json!({"request": "x", "limit": limit}),
        )
        .expect_err("a bad limit");
        assert!(
            matches!(err, ToolError::InvalidArgument { ref argument, .. } if argument == "limit"),
            "{limit}: {err:?}"
        );
    }
}

#[test]
fn a_search_needs_a_request() {
    let store = store();
    let (context, server) = context(&store, vec![1.0, 0.0], search_config());

    let err = search(&context, serde_json::json!({})).expect_err("no request");

    assert!(matches!(err, ToolError::MissingArgument { ref argument } if argument == "request"));
    assert!(server.requests().is_empty());
}

#[test]
fn search_requires_an_embedding_model() {
    let tool = SearchSkillsTool::new();

    assert!(tool.validate_config(&search_config()).is_ok());
    assert!(tool
        .validate_config(&serde_json::json!({"embedding_model": MODEL}))
        .is_ok());
    assert!(tool.validate_config(&serde_json::json!({})).is_err());
    assert!(tool
        .validate_config(&serde_json::json!({"embedding_model": " "}))
        .is_err());
    assert!(tool
        .validate_config(&serde_json::json!({"embedding_model": MODEL, "typo": 1}))
        .is_err());
}

#[test]
fn get_skill_returns_the_whole_skill_framed_as_notes() {
    let store = store();
    let (context, _server) = context(&store, vec![1.0, 0.0], serde_json::json!({}));

    let content = get(&context, serde_json::json!({"id": 1})).expect("get");

    assert!(
        content.starts_with("# fetch-webpage-into-sqlite (id 1)\n"),
        "{content}"
    );
    assert!(
        content.contains("not instructions from the user"),
        "{content}"
    );
    assert!(content.contains("when to use: The user asks to fetch webpage into sqlite."));
    assert!(
        content.ends_with("## Steps\n1. fetch-webpage-into-sqlite"),
        "{content}"
    );
}

#[test]
fn get_skill_with_an_unknown_id_names_the_ones_there_are() {
    let store = store();
    let (context, _server) = context(&store, vec![1.0, 0.0], serde_json::json!({}));

    let err = get(&context, serde_json::json!({"id": 42})).expect_err("unknown");

    assert!(matches!(err, ToolError::InvalidArgument { ref argument, .. } if argument == "id"));
    let message = err.to_string();
    for id in ["1", "2", "3"] {
        assert!(message.contains(id), "{message}");
    }
}

#[test]
fn get_skill_needs_an_integer_id() {
    let store = store();
    let (context, _server) = context(&store, vec![1.0, 0.0], serde_json::json!({}));

    assert!(matches!(
        get(&context, serde_json::json!({})).expect_err("missing"),
        ToolError::MissingArgument { ref argument } if argument == "id"
    ));
    for id in [serde_json::json!("1"), serde_json::json!(1.5)] {
        assert!(matches!(
            get(&context, serde_json::json!({"id": id})).expect_err("not an integer"),
            ToolError::InvalidArgument { ref argument, .. } if argument == "id"
        ));
    }
}

#[test]
fn without_a_memory_database_the_tools_say_so() {
    let context = ToolContext::new(scratch_dir(), search_config());

    let err = search(&context, serde_json::json!({"request": "x"})).expect_err("no store");
    assert!(matches!(err, ToolError::NotStarted { .. }), "{err:?}");
    let err = get(&context, serde_json::json!({"id": 1})).expect_err("no store");
    assert!(matches!(err, ToolError::NotStarted { .. }), "{err:?}");
}
