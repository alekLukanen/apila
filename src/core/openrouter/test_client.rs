use std::time::Duration;

use crate::core::test_support::{Route, StubOpenRouter, StubReply};

use super::client::{OpenRouter, OpenRouterConfig, OpenRouterError};
use super::types::EmbeddingRequest;

fn client(server: &StubOpenRouter) -> OpenRouter {
    OpenRouter::new(OpenRouterConfig::new("sk-or-test".into()).set_base_url(server.base_url()))
        .expect("build client")
}

fn embeddings_reply(data: serde_json::Value) -> StubOpenRouter {
    StubOpenRouter::start_script(
        vec![StubReply::json(
            serde_json::json!({"model": "m", "data": data}),
        )],
        Duration::ZERO,
    )
}

fn request(inputs: &[&str]) -> EmbeddingRequest {
    EmbeddingRequest::new(
        "openai/text-embedding-3-small",
        inputs.iter().map(|input| input.to_string()).collect(),
    )
}

#[test]
fn embeddings_come_back_in_the_order_of_the_inputs() {
    let server = embeddings_reply(serde_json::json!([
        {"index": 1, "embedding": [0.0, 1.0]},
        {"index": 0, "embedding": [1.0, 0.0]},
    ]));

    let response = client(&server)
        .embeddings(request(&["first", "second"]))
        .expect("embed");

    assert_eq!(response.data[0].embedding, vec![1.0, 0.0]);
    assert_eq!(response.data[1].embedding, vec![0.0, 1.0]);
    assert_eq!(server.embedding_requests().len(), 1);
    let sent: serde_json::Value =
        serde_json::from_str(&server.embedding_requests()[0]).expect("json");
    assert_eq!(sent["input"], serde_json::json!(["first", "second"]));
}

#[test]
fn a_response_with_no_embeddings_is_an_error() {
    let server = embeddings_reply(serde_json::json!([]));

    let err = client(&server)
        .embeddings(request(&["first"]))
        .expect_err("no embeddings");

    assert!(matches!(err, OpenRouterError::NoEmbeddings), "{err:?}");
}

#[test]
fn unusable_embeddings_are_refused() {
    let cases = [
        // one vector for two inputs
        (serde_json::json!([{"index": 0, "embedding": [1.0]}]), 2),
        // two vectors for the same input
        (
            serde_json::json!([
                {"index": 0, "embedding": [1.0]},
                {"index": 0, "embedding": [0.5]},
            ]),
            2,
        ),
        (serde_json::json!([{"index": 0, "embedding": []}]), 1),
        (
            serde_json::json!([{"index": 0, "embedding": [0.0, 0.0]}]),
            1,
        ),
        (
            serde_json::json!([
                {"index": 0, "embedding": [1.0]},
                {"index": 1, "embedding": [1.0, 0.0]},
            ]),
            2,
        ),
    ];
    for (data, inputs) in cases {
        let server = embeddings_reply(data.clone());
        let texts: Vec<&str> = ["a", "b"].into_iter().take(inputs).collect();
        let err = client(&server)
            .embeddings(request(&texts))
            .expect_err("unusable");
        assert!(
            matches!(err, OpenRouterError::InvalidEmbedding(_)),
            "{data}: {err:?}"
        );
    }
}

#[test]
fn an_invalid_embedding_request_never_leaves_the_machine() {
    let server = embeddings_reply(serde_json::json!([]));

    let err = client(&server)
        .embeddings(request(&[]))
        .expect_err("invalid");

    assert!(matches!(err, OpenRouterError::InvalidRequest(_)), "{err:?}");
    assert!(server.requests().is_empty());
}

#[test]
fn an_error_status_carries_the_api_s_message() {
    // a routed stub with no routes answers everything with a 500
    let server = StubOpenRouter::start_routed(vec![(
        Route::Chat("nobody/asks-for-this".into()),
        vec![StubReply::text("unused")],
    )]);

    let err = client(&server)
        .embeddings(request(&["first"]))
        .expect_err("an error status");

    match err {
        OpenRouterError::Api { status, message } => {
            assert_eq!(status, 500);
            assert!(message.contains("no stub route"), "{message}");
        }
        err => panic!("unexpected error: {err:?}"),
    }
}
