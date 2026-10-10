use std::{fmt::Debug, time::Duration};

use reqwest::blocking::Client;
use thiserror::Error;

use crate::core::config::config::Config;

use super::types::{
    ApiErrorBody, ChatCompletionRequest, ChatCompletionResponse, EmbeddingRequest,
    EmbeddingResponse,
};

pub const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";
pub const DEFAULT_TIMEOUT_SECS: u64 = 60;

#[derive(Debug, Error)]
pub enum OpenRouterError {
    #[error("http request failed: {0}")]
    Http(#[from] reqwest::Error),

    #[error("failed to decode the response: {0}")]
    Decode(#[from] serde_json::Error),

    #[error("openrouter returned an error (status {status}): {message}")]
    Api { status: u16, message: String },

    #[error("unexpected response from openrouter (status {status}): {body}")]
    UnexpectedResponse { status: u16, body: String },

    #[error("invalid request: {0}")]
    InvalidRequest(String),

    #[error("the response contained no choices")]
    NoChoices,

    #[error("the response contained no embeddings")]
    NoEmbeddings,

    #[error("the response contained an unusable embedding: {0}")]
    InvalidEmbedding(String),
}

#[derive(Clone)]
pub struct OpenRouterConfig {
    api_key: String,
    base_url: String,
    http_referer: Option<String>,
    x_openrouter_title: Option<String>,
}

/// Hand written so the api key can never leak into a log or a panic message.
impl Debug for OpenRouterConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenRouterConfig")
            .field("api_key", &"<redacted>")
            .field("base_url", &self.base_url)
            .field("http_referer", &self.http_referer)
            .field("x_title", &self.x_openrouter_title)
            .finish()
    }
}

impl OpenRouterConfig {
    pub fn new(api_key: String) -> OpenRouterConfig {
        OpenRouterConfig {
            api_key,
            base_url: DEFAULT_BASE_URL.into(),
            http_referer: None,
            x_openrouter_title: None,
        }
    }

    /// Builds the client config from the project's `config.json`.
    pub fn from_config(config: &Config) -> OpenRouterConfig {
        OpenRouterConfig {
            api_key: config.openrouter_api_key.clone(),
            base_url: config
                .openrouter_base_url
                .clone()
                .unwrap_or_else(|| DEFAULT_BASE_URL.to_string()),
            http_referer: config.http_referer.clone(),
            x_openrouter_title: config.x_openrouter_title.clone(),
        }
    }

    pub fn base_url(&self) -> String {
        self.base_url.clone()
    }
    pub fn http_referer(&self) -> Option<String> {
        self.http_referer.clone()
    }
    pub fn x_openrouter_title(&self) -> Option<String> {
        self.x_openrouter_title.clone()
    }

    pub fn set_base_url(mut self, base_url: String) -> OpenRouterConfig {
        self.base_url = base_url;
        self
    }
    pub fn set_http_referer(mut self, http_referer: String) -> OpenRouterConfig {
        self.http_referer = Some(http_referer);
        self
    }
    pub fn set_x_title(mut self, x_title: String) -> OpenRouterConfig {
        self.x_openrouter_title = Some(x_title);
        self
    }
}

/// The interface used to talk to the OpenRouter json api.
pub struct OpenRouter {
    config: OpenRouterConfig,
    http: Client,
}

impl Debug for OpenRouter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OpenRouter<...>")
    }
}

impl OpenRouter {
    pub fn new(config: OpenRouterConfig) -> Result<OpenRouter, OpenRouterError> {
        let http = Client::builder()
            .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
            .build()?;
        Ok(OpenRouter { config, http })
    }

    pub fn config(&self) -> OpenRouterConfig {
        self.config.clone()
    }

    // Chat operations ///////////////////
    //////////////////////////////////////

    /// Sends a chat completion request and returns the decoded response.
    pub fn chat_completion(
        &self,
        req: ChatCompletionRequest,
    ) -> Result<ChatCompletionResponse, OpenRouterError> {
        req.validate().map_err(OpenRouterError::InvalidRequest)?;

        let body = self.post_json("chat/completions", &req)?;
        let resp: ChatCompletionResponse = serde_json::from_str(&body)?;
        if resp.choices.is_empty() {
            return Err(OpenRouterError::NoChoices);
        }

        Ok(resp)
    }

    // Embedding operations //////////////
    //////////////////////////////////////

    /// One vector per input, in the order of `req.input` whatever order the
    /// api listed them in.
    pub fn embeddings(&self, req: EmbeddingRequest) -> Result<EmbeddingResponse, OpenRouterError> {
        req.validate().map_err(OpenRouterError::InvalidRequest)?;

        let body = self.post_json("embeddings", &req)?;
        let mut resp: EmbeddingResponse = serde_json::from_str(&body)?;
        if resp.data.is_empty() {
            return Err(OpenRouterError::NoEmbeddings);
        }
        resp.data.sort_by_key(|data| data.index);
        check_embeddings(&resp, req.input.len()).map_err(OpenRouterError::InvalidEmbedding)?;

        Ok(resp)
    }

    /// Posts `req` to `{base_url}/{path}` and returns the body of a 2xx reply.
    fn post_json(
        &self,
        path: &str,
        req: &impl serde::Serialize,
    ) -> Result<String, OpenRouterError> {
        let url = format!("{}/{}", self.config.base_url.trim_end_matches('/'), path);

        let mut builder = self
            .http
            .post(url)
            .bearer_auth(&self.config.api_key)
            .header("Content-Type", "application/json");
        if let Some(http_referer) = &self.config.http_referer {
            builder = builder.header("HTTP-Referer", http_referer);
        }
        if let Some(x_title) = &self.config.x_openrouter_title {
            builder = builder.header("X-OpenRouter-Title", x_title);
        }

        let resp = builder.json(req).send()?;

        // The body carries the useful error message, so it is read as text
        // rather than thrown away by `error_for_status`.
        let status = resp.status().as_u16();
        let body = resp.text()?;

        if !(200..300).contains(&status) {
            return Err(match serde_json::from_str::<ApiErrorBody>(&body) {
                Ok(err) => OpenRouterError::Api {
                    status,
                    message: err.error.message,
                },
                Err(_) => OpenRouterError::UnexpectedResponse { status, body },
            });
        }

        Ok(body)
    }
}

/// A vector that is empty, not finite or all zeros has no direction, so no
/// similarity could ever be measured against it.
fn check_embeddings(resp: &EmbeddingResponse, inputs: usize) -> Result<(), String> {
    if resp.data.len() != inputs {
        return Err(format!(
            "{} inputs were sent and {} vectors came back",
            inputs,
            resp.data.len()
        ));
    }
    let dimensions = resp.data[0].embedding.len();
    for (position, data) in resp.data.iter().enumerate() {
        // sorted by index, so anything else is an input left out or repeated
        if data.index != position {
            return Err(format!("no vector came back for input {}", position));
        }
        let vector = &data.embedding;
        if vector.is_empty() {
            return Err("a vector is empty".into());
        }
        if vector.len() != dimensions {
            return Err("the vectors are not all the same length".into());
        }
        if vector.iter().any(|value| !value.is_finite()) {
            return Err("a vector holds a value that is not finite".into());
        }
        if vector.iter().all(|value| *value == 0.0) {
            return Err("a vector is all zeros".into());
        }
    }
    Ok(())
}
