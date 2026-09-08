use crate::core::openrouter::types::{Choice, FinishReason};

enum Turn {
    Continue,
    Done,
    Failed(String),
}

fn classify(choice: &Choice) -> Turn {
    if !choice.message.tool_calls().is_empty() {
        return Turn::Continue;
    }
    match choice.finish_reason {
        None | Some(FinishReason::Stop) => Turn::Done,
        Some(FinishReason::ToolCalls) => Turn::Continue,
        Some(FinishReason::Length) => Turn::Failed("response was truncated".into()),
        Some(FinishReason::ContentFilter) => Turn::Failed("stopped by content filter".into()),
        Some(FinishReason::Other) => Turn::Failed("stopped for an unknown reason".into()),
    }
}
