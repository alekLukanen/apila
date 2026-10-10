use crate::core::tools::tool::{Tool, ToolContext, ToolError, ToolOutput, ToolState};

/// Ending the turn is a call because nothing in the model's text can be read
/// as deciding it. Every agent has it; it is not something the user grants.
pub struct EndTurnTool;

impl EndTurnTool {
    pub fn new() -> EndTurnTool {
        EndTurnTool
    }
}

impl Tool for EndTurnTool {
    fn name(&self) -> String {
        "end_turn".into()
    }

    fn description(&self) -> String {
        "Declare that you have finished the work you were asked to do and the \
         turn is over. Call this once the task is complete, or once you need \
         something from the user before you can carry on. Do not call it while \
         you still have work left to run."
            .into()
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "summary": {
                    "type": "string",
                    "description": "A short account of what you did, or of what you are waiting on."
                }
            },
            "required": []
        })
    }

    fn always_enabled(&self) -> bool {
        true
    }

    fn new_state(&self, _context: &ToolContext) -> Result<Box<dyn ToolState>, ToolError> {
        Ok(Box::new(EndTurnState))
    }
}

/// Ending a turn needs nothing kept between calls, so this is the simplest a
/// state gets — and the shape every stateless tool takes.
pub struct EndTurnState;

impl ToolState for EndTurnState {
    fn run(
        &mut self,
        _context: &ToolContext,
        arguments: &serde_json::Value,
    ) -> Result<ToolOutput, ToolError> {
        // the summary is optional on purpose: this tool's whole job is to stop
        // the loop, and refusing the call over a missing argument would keep
        // the agent running for no reason
        let summary = arguments
            .get("summary")
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .trim();

        let content = if summary == "" {
            "turn ended".to_string()
        } else {
            format!("turn ended: {}", summary)
        };

        Ok(ToolOutput::new(content).set_ends_turn(true))
    }
}
