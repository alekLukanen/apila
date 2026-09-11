use crate::core::openrouter::types::{Choice, FinishReason};

pub(super) enum Turn {
    Continue,
    Done,
    Failed(String),
}

pub(super) fn classify(choice: &Choice) -> Turn {
    if !choice.message.tool_calls().is_empty() {
        return Turn::Continue;
    }
    match choice.finish_reason {
        None | Some(FinishReason::Stop) => Turn::Done,
        // the model said it wanted tools and then sent none; sending the same
        // conversation back would only get the same answer, so it is a failure
        // rather than an iteration burned
        Some(FinishReason::ToolCalls) => {
            Turn::Failed("the model asked for tool calls but sent none".into())
        }
        Some(FinishReason::Length) => Turn::Failed("response was truncated".into()),
        Some(FinishReason::ContentFilter) => Turn::Failed("stopped by content filter".into()),
        Some(FinishReason::Other) => Turn::Failed("stopped for an unknown reason".into()),
    }
}
