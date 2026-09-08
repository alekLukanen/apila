use std::path::PathBuf;

use apila::core::{
    config::config::Config,
    openrouter::{
        client::{OpenRouter, OpenRouterConfig},
        types::{ChatCompletionRequest, Message, Tool, ToolChoice},
    },
};

/// The model this smoke test runs on unless one is passed in.
const SMOKE_TEST_MODEL: &str = "openai/gpt-4o-mini";

/// A live smoke test for the OpenRouter client. Reads the api key from
/// `<project_dir>/config.json`; pass the project dir as the first argument
/// (defaults to ./test_env) and the model as the second.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("--- scratch main ---");

    let project_dir: PathBuf = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "test_env".to_string())
        .into();

    let model = std::env::args()
        .nth(2)
        .unwrap_or_else(|| SMOKE_TEST_MODEL.to_string());

    let config = Config::load(&project_dir)?;
    let client = OpenRouter::new(OpenRouterConfig::from_config(&config))?;

    // plain chat ////
    println!("\n--- chat completion ---");
    let resp = client.chat_completion(ChatCompletionRequest::new(
        model.clone(),
        vec![
            Message::system("You are terse. Answer in one short sentence."),
            Message::user("What is the capital of France?"),
        ],
    ))?;
    let choice = resp.first_choice().expect("a choice");
    println!("finish_reason: {:?}", choice.finish_reason);
    println!("content: {:?}", choice.message.content());
    println!("usage: {:?}", resp.usage);

    // tool calling ////
    println!("\n--- tool call ---");
    let resp = client.chat_completion(
        ChatCompletionRequest::new(
            model,
            vec![Message::user("What is the weather in Denver, Colorado?")],
        )
        .set_tools(vec![Tool::function(
            "get_weather",
            "Get the current weather for a location",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "location": { "type": "string", "description": "City and state" }
                },
                "required": ["location"]
            }),
        )])
        .set_tool_choice(ToolChoice::auto()),
    )?;
    let choice = resp.first_choice().expect("a choice");
    println!("finish_reason: {:?}", choice.finish_reason);
    for tool_call in choice.message.tool_calls() {
        println!(
            "tool call {} -> {} {:?}",
            tool_call.id,
            tool_call.function.name,
            tool_call.function.parsed_arguments()?
        );
    }

    Ok(())
}
