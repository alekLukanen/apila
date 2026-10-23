use serde_json::json;

use super::types::{
    ApiErrorBody, ChatCompletionRequest, ChatCompletionResponse, EmbeddingRequest,
    EmbeddingResponse, FinishReason, FunctionCall, Message, Reasoning, ReasoningEffort, Spend,
    Tool, ToolCall, ToolChoice, Usage,
};

#[test]
fn request_serializes_every_message_role() {
    let req = ChatCompletionRequest::new(
        "openai/gpt-4o",
        vec![
            Message::system("you are a coding agent"),
            Message::user("what is in the repo?"),
            Message::assistant_tool_calls(vec![ToolCall {
                id: "call_1".into(),
                call_type: "function".into(),
                function: FunctionCall {
                    name: "list_files".into(),
                    arguments: r#"{"path":"."}"#.into(),
                },
            }]),
            Message::tool("call_1", "Cargo.toml\nsrc"),
        ],
    )
    .set_tools(vec![Tool::function(
        "list_files",
        "lists files in a directory",
        json!({
            "type": "object",
            "properties": { "path": { "type": "string" } },
            "required": ["path"]
        }),
    )])
    .set_tool_choice(ToolChoice::auto())
    .set_temperature(0.5)
    .set_max_tokens(1024);

    let got = serde_json::to_value(&req).expect("serialize request");

    assert_eq!(
        got,
        json!({
            "model": "openai/gpt-4o",
            "messages": [
                { "role": "system", "content": "you are a coding agent" },
                { "role": "user", "content": "what is in the repo?" },
                {
                    "role": "assistant",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "list_files", "arguments": "{\"path\":\".\"}" }
                    }]
                },
                { "role": "tool", "tool_call_id": "call_1", "content": "Cargo.toml\nsrc" }
            ],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "list_files",
                    "description": "lists files in a directory",
                    "parameters": {
                        "type": "object",
                        "properties": { "path": { "type": "string" } },
                        "required": ["path"]
                    }
                }
            }],
            "tool_choice": "auto",
            "temperature": 0.5,
            "max_tokens": 1024
        })
    );
}

#[test]
fn request_omits_unset_optional_fields() {
    let req = ChatCompletionRequest::new("openai/gpt-4o", vec![Message::user("hi")]);
    let got = serde_json::to_value(&req).expect("serialize request");

    assert_eq!(
        got,
        json!({
            "model": "openai/gpt-4o",
            "messages": [{ "role": "user", "content": "hi" }]
        })
    );
}

#[test]
fn a_reasoning_effort_serializes_under_reasoning() {
    let req = ChatCompletionRequest::new("openai/gpt-4o", vec![Message::user("hi")]).set_reasoning(
        Reasoning {
            effort: Some(ReasoningEffort::Xhigh),
        },
    );
    let got = serde_json::to_value(&req).expect("serialize request");

    assert_eq!(got["reasoning"], json!({ "effort": "xhigh" }));
}

#[test]
fn every_reasoning_effort_uses_the_openrouter_name() {
    let efforts = [
        (ReasoningEffort::Xhigh, "xhigh"),
        (ReasoningEffort::High, "high"),
        (ReasoningEffort::Medium, "medium"),
        (ReasoningEffort::Low, "low"),
        (ReasoningEffort::Minimal, "minimal"),
        (ReasoningEffort::None, "none"),
    ];

    for (effort, name) in efforts {
        assert_eq!(effort.as_str(), name);
        assert_eq!(
            serde_json::to_value(effort).expect("serialize"),
            json!(name)
        );
        let parsed: ReasoningEffort =
            serde_json::from_value(json!(name)).expect("deserialize effort");
        assert_eq!(parsed, effort);
    }
}

#[test]
fn named_tool_choice_serializes_as_an_object() {
    let got = serde_json::to_value(ToolChoice::function("list_files")).expect("serialize");
    assert_eq!(
        got,
        json!({ "type": "function", "function": { "name": "list_files" } })
    );
}

#[test]
fn response_with_tool_calls_deserializes() {
    // A response body shaped the way the api documents it, including fields
    // this client does not model.
    let body = r#"{
        "id": "gen-123",
        "object": "chat.completion",
        "created": 1700000000,
        "model": "openai/gpt-4o",
        "system_fingerprint": null,
        "choices": [{
            "index": 0,
            "logprobs": null,
            "finish_reason": "tool_calls",
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call_abc",
                    "type": "function",
                    "function": {
                        "name": "list_files",
                        "arguments": "{\"path\":\"src\",\"depth\":2}"
                    }
                }]
            }
        }],
        "usage": {
            "prompt_tokens": 31,
            "completion_tokens": 12,
            "total_tokens": 43,
            "prompt_tokens_details": { "cached_tokens": 0 }
        }
    }"#;

    let resp: ChatCompletionResponse = serde_json::from_str(body).expect("deserialize response");

    assert_eq!(resp.id, "gen-123");
    assert_eq!(resp.model, "openai/gpt-4o");
    assert_eq!(resp.created, 1700000000);

    let choice = resp.first_choice().expect("a choice");
    assert_eq!(choice.finish_reason, Some(FinishReason::ToolCalls));
    assert_eq!(choice.message.content(), None);

    let tool_calls = choice.message.tool_calls();
    assert_eq!(tool_calls.len(), 1);
    assert_eq!(tool_calls[0].id, "call_abc");
    assert_eq!(tool_calls[0].function.name, "list_files");
    assert_eq!(
        tool_calls[0]
            .function
            .parsed_arguments()
            .expect("parse arguments"),
        json!({ "path": "src", "depth": 2 })
    );

    let usage = resp.usage.expect("usage");
    assert_eq!(usage.prompt_tokens, 31);
    assert_eq!(usage.completion_tokens, 12);
    assert_eq!(usage.total_tokens, 43);
}

#[test]
fn response_with_plain_content_deserializes() {
    let body = r#"{
        "id": "gen-456",
        "model": "openai/gpt-4o",
        "choices": [{
            "index": 0,
            "finish_reason": "stop",
            "message": { "role": "assistant", "content": "hello there" }
        }]
    }"#;

    let resp: ChatCompletionResponse = serde_json::from_str(body).expect("deserialize response");
    let choice = resp.first_choice().expect("a choice");

    assert_eq!(choice.finish_reason, Some(FinishReason::Stop));
    assert_eq!(choice.message.content(), Some("hello there"));
    assert!(choice.message.tool_calls().is_empty());
}

#[test]
fn an_unknown_finish_reason_does_not_fail_the_deserialize() {
    let body = r#"{
        "id": "gen-789",
        "model": "openai/gpt-4o",
        "choices": [{
            "index": 0,
            "finish_reason": "something_new",
            "message": { "role": "assistant", "content": "ok" }
        }]
    }"#;

    let resp: ChatCompletionResponse = serde_json::from_str(body).expect("deserialize response");
    assert_eq!(
        resp.first_choice().expect("a choice").finish_reason,
        Some(FinishReason::Other)
    );
}

#[test]
fn error_body_deserializes() {
    let body = r#"{
        "error": {
            "code": 402,
            "message": "Insufficient credits",
            "metadata": { "provider_name": "OpenAI" }
        },
        "user_id": null
    }"#;

    let err: ApiErrorBody = serde_json::from_str(body).expect("deserialize error");
    assert_eq!(err.error.code, Some(402));
    assert_eq!(err.error.message, "Insufficient credits");
    assert!(err.error.metadata.is_some());
}

#[test]
fn validate_rejects_bad_requests() {
    let no_messages = ChatCompletionRequest::new("openai/gpt-4o", vec![]);
    assert!(no_messages.validate().is_err());

    let no_model = ChatCompletionRequest::new("", vec![Message::user("hi")]);
    assert!(no_model.validate().is_err());

    let too_many_stops = ChatCompletionRequest::new("openai/gpt-4o", vec![Message::user("hi")])
        .set_stop(vec![
            "a".into(),
            "b".into(),
            "c".into(),
            "d".into(),
            "e".into(),
        ]);
    assert!(too_many_stops.validate().is_err());

    let bad_temperature =
        ChatCompletionRequest::new("openai/gpt-4o", vec![Message::user("hi")]).set_temperature(3.0);
    assert!(bad_temperature.validate().is_err());

    let bad_tool_name = ChatCompletionRequest::new("openai/gpt-4o", vec![Message::user("hi")])
        .set_tools(vec![Tool::function("list files!", "nope", json!({}))]);
    assert!(bad_tool_name.validate().is_err());

    let ok = ChatCompletionRequest::new("openai/gpt-4o", vec![Message::user("hi")])
        .set_temperature(0.7)
        .set_stop(vec!["\n\n".into()])
        .set_tools(vec![Tool::function("list_files", "ok", json!({}))]);
    assert!(ok.validate().is_ok());
}

#[test]
fn validate_rejects_a_request_with_no_user_message() {
    let req = ChatCompletionRequest::new("openai/gpt-4o", vec![Message::system("guidelines")]);

    let err = req
        .validate()
        .expect_err("a system message is not a conversation");

    assert!(err.contains("user message"));
}

#[test]
fn an_embedding_request_serializes_its_model_and_inputs() {
    let request = EmbeddingRequest::new("openai/text-embedding-3-small", vec!["a".into()]);

    assert_eq!(
        serde_json::to_value(&request).expect("serialize"),
        json!({"model": "openai/text-embedding-3-small", "input": ["a"]})
    );
}

#[test]
fn an_embedding_request_needs_a_model_and_text() {
    assert!(EmbeddingRequest::new("m/e", vec!["a".into()])
        .validate()
        .is_ok());
    assert!(EmbeddingRequest::new(" ", vec!["a".into()])
        .validate()
        .is_err());
    assert!(EmbeddingRequest::new("m/e", Vec::new()).validate().is_err());
    assert!(EmbeddingRequest::new("m/e", vec!["  ".into()])
        .validate()
        .is_err());
}

#[test]
fn an_embedding_response_deserializes_with_extra_fields() {
    let response: EmbeddingResponse = serde_json::from_value(json!({
        "object": "list",
        "model": "openai/text-embedding-3-small",
        "data": [{"object": "embedding", "index": 0, "embedding": [0.25, -0.5]}],
        "usage": {"prompt_tokens": 3, "total_tokens": 3},
    }))
    .expect("deserialize");

    assert_eq!(response.data.len(), 1);
    assert_eq!(response.data[0].embedding, vec![0.25, -0.5]);
    assert_eq!(response.usage.expect("usage").prompt_tokens, 3);
}

fn usage(total_tokens: u32, cost: Option<f64>) -> Usage {
    Usage {
        prompt_tokens: 0,
        completion_tokens: 0,
        total_tokens,
        cost,
    }
}

#[test]
fn a_chat_usage_cost_parses_and_is_none_when_absent() {
    let priced: ChatCompletionResponse = serde_json::from_value(json!({
        "id": "gen-1",
        "model": "m",
        "choices": [],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15, "cost": 0.0025},
    }))
    .expect("deserialize");
    assert_eq!(priced.usage.expect("usage").cost, Some(0.0025));

    let unpriced: ChatCompletionResponse = serde_json::from_value(json!({
        "id": "gen-1",
        "model": "m",
        "choices": [],
        "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15},
    }))
    .expect("deserialize");
    assert_eq!(unpriced.usage.expect("usage").cost, None);
}

#[test]
fn an_embedding_usage_cost_parses_and_is_none_when_absent() {
    let priced: EmbeddingResponse = serde_json::from_value(json!({
        "data": [],
        "usage": {"prompt_tokens": 3, "total_tokens": 3, "cost": 0.000001},
    }))
    .expect("deserialize");
    assert_eq!(priced.usage.expect("usage").cost, Some(0.000001));

    let unpriced: EmbeddingResponse = serde_json::from_value(json!({
        "data": [],
        "usage": {"prompt_tokens": 3, "total_tokens": 3},
    }))
    .expect("deserialize");
    assert_eq!(unpriced.usage.expect("usage").cost, None);
}

#[test]
fn spend_counts_a_missing_usage_or_cost_as_unpriced() {
    let mut spend = Spend::default();
    spend.add_response(None);
    assert_eq!(spend, Spend::new(0, 0, 1));

    spend.add_response(Some(&usage(40, None)));
    assert_eq!(spend, Spend::new(40, 0, 2));
    assert!(!spend.priced());

    spend.add_response(Some(&usage(10, Some(f64::NAN))));
    assert_eq!(spend, Spend::new(50, 0, 3));
}

#[test]
fn spend_sums_priced_responses() {
    let mut spend = Spend::default();
    spend
        .add_response(Some(&usage(100, Some(0.0001))))
        .add_response(Some(&usage(20, Some(0.0002))));

    assert_eq!(spend, Spend::new(120, 300_000, 0));
    assert!(spend.priced());
    assert!((spend.cost_usd() - 0.0003).abs() < 1e-12);
}

#[test]
fn spend_rounds_to_the_nearest_nanodollar_and_clamps() {
    let mut spend = Spend::default();
    spend.add_response(Some(&usage(0, Some(0.0000000016))));
    assert_eq!(spend.cost_nanos, 2);

    let mut huge = Spend::default();
    huge.add_response(Some(&usage(0, Some(1e30))));
    assert_eq!(huge.cost_nanos, i64::MAX);

    let mut negative = Spend::default();
    negative.add_response(Some(&usage(0, Some(-1e30))));
    assert_eq!(negative.cost_nanos, i64::MIN);
}

#[test]
fn spend_merge_adds_every_field() {
    let mut total = Spend::new(10, 5, 1);
    total.merge(&Spend::new(3, 7, 2));
    assert_eq!(total, Spend::new(13, 12, 3));
}
