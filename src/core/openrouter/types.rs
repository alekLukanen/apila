use serde::{Deserialize, Serialize};

/// The maximum number of stop sequences the api accepts.
pub const MAX_STOP_SEQUENCES: usize = 4;
/// The maximum length of a tool function name.
pub const MAX_FUNCTION_NAME_LEN: usize = 64;

// Messages //////////////////////////
//////////////////////////////////////

/// A single message in a conversation. Serialized with a `role` discriminant,
/// matching the OpenAI compatible schema OpenRouter exposes.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase")]
pub enum Message {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        /// Null when the model only returned tool calls.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}

impl Message {
    pub fn system(content: impl Into<String>) -> Message {
        Message::System {
            content: content.into(),
        }
    }
    pub fn user(content: impl Into<String>) -> Message {
        Message::User {
            content: content.into(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Message {
        Message::Assistant {
            content: Some(content.into()),
            tool_calls: Vec::new(),
        }
    }
    pub fn assistant_tool_calls(tool_calls: Vec<ToolCall>) -> Message {
        Message::Assistant {
            content: None,
            tool_calls,
        }
    }
    /// The result of running a tool, replying to the tool call with `tool_call_id`.
    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Message {
        Message::Tool {
            tool_call_id: tool_call_id.into(),
            content: content.into(),
        }
    }

    /// The text content of the message, if it has any.
    pub fn content(&self) -> Option<&str> {
        match self {
            Message::System { content } | Message::User { content } => Some(content),
            Message::Tool { content, .. } => Some(content),
            Message::Assistant { content, .. } => content.as_deref(),
        }
    }

    /// The tool calls the model requested. Empty for every other role.
    pub fn tool_calls(&self) -> &[ToolCall] {
        match self {
            Message::Assistant { tool_calls, .. } => tool_calls,
            _ => &[],
        }
    }
}

// Tools /////////////////////////////
//////////////////////////////////////

/// A tool the model may call. Only function tools are supported.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tool {
    #[serde(rename = "type")]
    pub tool_type: String,
    pub function: FunctionDef,
}

impl Tool {
    /// `parameters` is a json schema object describing the function's arguments.
    pub fn function(
        name: impl Into<String>,
        description: impl Into<String>,
        parameters: serde_json::Value,
    ) -> Tool {
        Tool {
            tool_type: "function".into(),
            function: FunctionDef {
                name: name.into(),
                description: Some(description.into()),
                parameters,
            },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionDef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub parameters: serde_json::Value,
}

/// How the model should pick between the provided tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ToolChoice {
    /// "auto" | "none" | "required"
    Mode(String),
    Function {
        #[serde(rename = "type")]
        choice_type: String,
        function: FunctionChoice,
    },
}

impl ToolChoice {
    pub fn auto() -> ToolChoice {
        ToolChoice::Mode("auto".into())
    }
    pub fn none() -> ToolChoice {
        ToolChoice::Mode("none".into())
    }
    pub fn required() -> ToolChoice {
        ToolChoice::Mode("required".into())
    }
    /// Force a call to a single named function.
    pub fn function(name: impl Into<String>) -> ToolChoice {
        ToolChoice::Function {
            choice_type: "function".into(),
            function: FunctionChoice { name: name.into() },
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionChoice {
    pub name: String,
}

/// A tool call requested by the model.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// A json document encoded as a string, not a json object.
    pub arguments: String,
}

impl FunctionCall {
    /// Parses `arguments` into a json value.
    pub fn parsed_arguments(&self) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::from_str(&self.arguments)
    }
}

// Request ///////////////////////////
//////////////////////////////////////

#[derive(Debug, Clone, Serialize)]
pub struct ChatCompletionRequest {
    pub model: String,
    pub messages: Vec<Message>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<Tool>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_choice: Option<ToolChoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop: Option<Vec<String>>,
}

impl ChatCompletionRequest {
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> ChatCompletionRequest {
        ChatCompletionRequest {
            model: model.into(),
            messages,
            tools: None,
            tool_choice: None,
            temperature: None,
            max_tokens: None,
            stop: None,
        }
    }

    pub fn set_tools(mut self, tools: Vec<Tool>) -> ChatCompletionRequest {
        self.tools = Some(tools);
        self
    }
    pub fn set_tool_choice(mut self, tool_choice: ToolChoice) -> ChatCompletionRequest {
        self.tool_choice = Some(tool_choice);
        self
    }
    pub fn set_temperature(mut self, temperature: f32) -> ChatCompletionRequest {
        self.temperature = Some(temperature);
        self
    }
    pub fn set_max_tokens(mut self, max_tokens: u32) -> ChatCompletionRequest {
        self.max_tokens = Some(max_tokens);
        self
    }
    pub fn set_stop(mut self, stop: Vec<String>) -> ChatCompletionRequest {
        self.stop = Some(stop);
        self
    }

    /// Checks the constraints the api documents so an obviously bad request
    /// never costs a round trip. Returns a description of the first problem.
    pub fn validate(&self) -> Result<(), String> {
        if self.model.trim() == "" {
            return Err("model must not be empty".into());
        }
        if self.messages.is_empty() {
            return Err("messages must contain at least one message".into());
        }
        // providers reject a conversation that never reaches a user turn, so
        // it is caught here rather than by a 400
        if !self
            .messages
            .iter()
            .any(|message| matches!(message, Message::User { .. }))
        {
            return Err("messages must contain at least one user message".into());
        }
        if let Some(stop) = &self.stop {
            if stop.len() > MAX_STOP_SEQUENCES {
                return Err(format!(
                    "stop accepts at most {} sequences, got {}",
                    MAX_STOP_SEQUENCES,
                    stop.len()
                ));
            }
        }
        if let Some(temperature) = self.temperature {
            if !(0.0..=2.0).contains(&temperature) {
                return Err(format!(
                    "temperature must be between 0 and 2, got {}",
                    temperature
                ));
            }
        }
        for tool in self.tools.iter().flatten() {
            validate_function_name(&tool.function.name)?;
        }
        if let Some(ToolChoice::Function { function, .. }) = &self.tool_choice {
            validate_function_name(&function.name)?;
        }
        Ok(())
    }
}

/// Tool names are limited to 64 chars of `[a-zA-Z0-9_-]`.
fn validate_function_name(name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err("tool function name must not be empty".into());
    }
    if name.len() > MAX_FUNCTION_NAME_LEN {
        return Err(format!(
            "tool function name `{}` exceeds {} characters",
            name, MAX_FUNCTION_NAME_LEN
        ));
    }
    if let Some(bad) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || *c == '_' || *c == '-'))
    {
        return Err(format!(
            "tool function name `{}` contains an invalid character `{}`",
            name, bad
        ));
    }
    Ok(())
}

// Response //////////////////////////
//////////////////////////////////////

/// Note that no response type denies unknown fields; OpenRouter and the
/// underlying providers regularly return more than what is modeled here.
#[derive(Debug, Clone, Deserialize)]
pub struct ChatCompletionResponse {
    pub id: String,
    pub model: String,
    #[serde(default)]
    pub created: i64,
    #[serde(default)]
    pub object: String,
    #[serde(default)]
    pub choices: Vec<Choice>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

impl ChatCompletionResponse {
    pub fn first_choice(&self) -> Option<&Choice> {
        self.choices.first()
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Choice {
    #[serde(default)]
    pub index: u32,
    pub message: Message,
    #[serde(default)]
    pub finish_reason: Option<FinishReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    ToolCalls,
    Length,
    ContentFilter,
    /// Any value the api adds that isn't modeled above.
    #[serde(other)]
    Other,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u32,
    #[serde(default)]
    pub completion_tokens: u32,
    #[serde(default)]
    pub total_tokens: u32,
}

/// The body returned alongside a non 2xx status.
#[derive(Debug, Clone, Deserialize)]
pub struct ApiErrorBody {
    pub error: ApiError,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ApiError {
    #[serde(default)]
    pub code: Option<i64>,
    pub message: String,
    #[serde(default)]
    pub metadata: Option<serde_json::Value>,
}
