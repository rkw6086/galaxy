use std::fmt;

use bytes::Bytes;
use futures::Stream;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};

use crate::settings::OpenAIProviderKind;

#[derive(Clone, Debug)]
pub struct OpenAIClientConfig {
    pub kind: OpenAIProviderKind,
    pub base_url: String,
    pub api_key: Option<String>,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub max_input_tokens: Option<u32>,
    pub max_output_tokens: Option<u32>,
    pub use_rig: bool,
    pub supports_system_messages: bool,
}

pub struct OpenAIClient {
    http: reqwest::Client,
    base_url: String,
    api_key: Option<String>,
}

#[derive(Debug)]
pub enum OpenAIError {
    ConnectionFailed(String),
    AuthenticationFailed(String),
    RateLimited(String),
    BadRequest(String),
    ServerError(String),
    #[allow(dead_code)]
    StreamError(String),
}

impl fmt::Display for OpenAIError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConnectionFailed(msg) => write!(f, "Connection failed: {msg}"),
            Self::AuthenticationFailed(msg) => write!(f, "Authentication failed: {msg}"),
            Self::RateLimited(msg) => write!(f, "Rate limited: {msg}"),
            Self::BadRequest(msg) => write!(f, "Bad request: {msg}"),
            Self::ServerError(msg) => write!(f, "Server error: {msg}"),
            Self::StreamError(msg) => write!(f, "Stream error: {msg}"),
        }
    }
}

impl OpenAIClient {
    pub fn from_config(config: OpenAIClientConfig) -> Self {
        let http = reqwest::Client::new();
        Self {
            http,
            base_url: config.base_url,
            api_key: config.api_key,
        }
    }

    pub async fn chat_completions_stream(
        &self,
        request_body: serde_json::Value,
    ) -> Result<impl Stream<Item = Result<Bytes, reqwest::Error>>, OpenAIError> {
        let url = format!("{}/chat/completions", self.base_url.trim_end_matches('/'));

        let mut headers = HeaderMap::new();
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some(ref key) = self.api_key {
            headers.insert(
                AUTHORIZATION,
                HeaderValue::from_str(&format!("Bearer {key}"))
                    .map_err(|e| OpenAIError::BadRequest(format!("Invalid API key header: {e}")))?,
            );
        }

        let response = self
            .http
            .post(&url)
            .headers(headers)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| OpenAIError::ConnectionFailed(e.to_string()))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(match status.as_u16() {
                401 => OpenAIError::AuthenticationFailed(body),
                429 => OpenAIError::RateLimited(body),
                400 => OpenAIError::BadRequest(body),
                _ => OpenAIError::ServerError(format!("HTTP {status}: {body}")),
            });
        }

        Ok(response.bytes_stream())
    }
}
