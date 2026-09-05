use std::path::PathBuf;

use apila::core::{
    config::config::Config,
    openrouter::{
        client::{OpenRouter, OpenRouterConfig},
        types::{ChatCompletionRequest, Message, Tool, ToolChoice},
    },
};

/// A live smoke test for the OpenRouter client. Reads the api key from
/// `<project_dir>/config.json`; pass the project dir as the first argument
/// (defaults to ./test_env).
fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("--- scratch main ---");

    let project_dir: PathBuf = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "test_env".to_string())
        .into();

    let config = Config::load(&project_dir)?;
    let model = config
        .default_model
        .clone()
        .unwrap_or_else(|| "openai/gpt-4o-mini".to_string());
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
