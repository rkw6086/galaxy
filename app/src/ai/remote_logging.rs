//! Opt-in remote AI diagnostics logger.
//!
//! This is intentionally separate from product telemetry. It is controlled
//! exclusively by local settings and should only receive operational metadata:
//! provider/model IDs, lifecycle states, counts, timings, and sanitized errors.

use std::sync::LazyLock;
use std::time::Duration;
#[cfg(all(feature = "remote_logs_local_file", not(target_family = "wasm")))]
use std::{
    fs::OpenOptions,
    io::Write,
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

use chrono::Utc;
use galaxy_core::channel::ChannelState;
use galaxyui::{Entity, ModelContext, SingletonEntity};
use serde::Serialize;
use serde_json::{json, Value};
use settings::Setting;

use crate::AISettings;

const DEFAULT_LOGS_PATH: &str = "/api/logs";
const REMOTE_LOG_SERVICE: &str = "galaxy-ai";
const REMOTE_LOG_TIMEOUT: Duration = Duration::from_secs(5);
// Authentication and endpoint selection remain scoped to each request.
static REMOTE_LOG_CLIENT: LazyLock<Result<reqwest::Client, String>> = LazyLock::new(|| {
    reqwest::Client::builder()
        .timeout(REMOTE_LOG_TIMEOUT)
        .build()
        .map_err(|error| format!("could not create HTTP client: {error}"))
});
const MAX_ERROR_CHARS: usize = 500;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RemoteLogLevel {
    Info,
    Warn,
    Error,
}

impl RemoteLogLevel {
    fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warn => "warn",
            Self::Error => "error",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RemoteLogRecord {
    pub(crate) level: RemoteLogLevel,
    pub(crate) message: String,
    pub(crate) context: Value,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RemoteLogConfig {
    endpoint: String,
    api_key: String,
}

#[derive(Serialize)]
struct RemoteLogPayload {
    level: &'static str,
    message: String,
    service: &'static str,
    context: Value,
}

#[cfg(all(feature = "remote_logs_local_file", not(target_family = "wasm")))]
static LOCAL_LOG_FILE: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

impl RemoteLogConfig {
    fn from_settings(settings: &AISettings) -> Option<Self> {
        if !*settings.remote_logging_enabled.value() {
            return None;
        }

        let api_key = settings.remote_logging_api_key.value().trim();
        if api_key.is_empty() {
            return None;
        }

        let endpoint = normalize_endpoint_url(settings.remote_logging_endpoint.value())?;
        Some(Self {
            endpoint,
            api_key: api_key.to_string(),
        })
    }
}

pub(crate) fn log_model_event<M>(ctx: &mut ModelContext<M>, record: RemoteLogRecord)
where
    M: Entity,
{
    let Some(config) = RemoteLogConfig::from_settings(AISettings::as_ref(ctx)) else {
        return;
    };

    let payload = RemoteLogPayload {
        level: record.level.as_str(),
        message: record.message,
        service: REMOTE_LOG_SERVICE,
        context: enrich_context(record.context),
    };

    #[cfg(all(feature = "remote_logs_local_file", not(target_family = "wasm")))]
    write_local_log(&payload);

    let _ = ctx.spawn(
        async move { send_remote_log(config, payload).await },
        |_, result, _| {
            if let Err(error) = result {
                log::warn!("[remote-logging] Failed to send remote AI log: {error}");
            }
        },
    );
}

#[cfg(all(feature = "remote_logs_local_file", not(target_family = "wasm")))]
fn write_local_log(payload: &RemoteLogPayload) {
    let state = LOCAL_LOG_FILE.get_or_init(|| Mutex::new(None));
    let mut path = match state.lock() {
        Ok(path) => path,
        Err(error) => {
            log::error!("[remote-logging] local log lock is poisoned: {error}");
            return;
        }
    };

    if path.is_none() {
        let directory = std::env::temp_dir().join("galaxy-remote-logs");
        if let Err(error) = std::fs::create_dir_all(&directory) {
            log::error!(
                "[remote-logging] failed to create local log directory {}: {error}",
                directory.display()
            );
            return;
        }
        let file_path = directory.join(format!("galaxy-remote-{}.jsonl", std::process::id()));
        *path = Some(file_path.clone());
        log::warn!(
            "[remote-logging] local diagnostic log enabled: {}",
            file_path.display()
        );
    }

    let Some(file_path) = path.as_ref() else {
        return;
    };
    let line = match serde_json::to_string(payload) {
        Ok(line) => line,
        Err(error) => {
            log::error!("[remote-logging] failed to serialize local log record: {error}");
            return;
        }
    };
    match OpenOptions::new().create(true).append(true).open(file_path) {
        Ok(mut file) => {
            if let Err(error) = writeln!(file, "{line}") {
                log::error!(
                    "[remote-logging] failed to write local log {}: {error}",
                    file_path.display()
                );
            }
        }
        Err(error) => log::error!(
            "[remote-logging] failed to open local log {}: {error}",
            file_path.display()
        ),
    }
}

pub(crate) fn sanitize_error(error: impl std::fmt::Display) -> String {
    let compact = error
        .to_string()
        .split_whitespace()
        .map(redact_sensitive_token)
        .collect::<Vec<_>>()
        .join(" ");
    truncate_chars(&compact, MAX_ERROR_CHARS)
}

pub(crate) fn raw_model_payload_context<M>(
    ctx: &ModelContext<M>,
    raw_payload: impl AsRef<str>,
) -> Option<Value>
where
    M: Entity,
{
    let settings = AISettings::as_ref(ctx);
    if !*settings.remote_logging_enabled.value()
        || !*settings.remote_logging_log_model_payloads.value()
        || settings.remote_logging_api_key.value().trim().is_empty()
    {
        return None;
    }

    let max_chars = (*settings.remote_logging_model_payload_max_chars.value()).max(1);
    Some(tail_limited_payload_context(
        raw_payload.as_ref(),
        max_chars,
    ))
}

fn enrich_context(mut context: Value) -> Value {
    let Value::Object(ref mut map) = context else {
        return json!({
            "timestamp": Utc::now().to_rfc3339(),
            "app": app_context(),
            "details": context,
        });
    };

    map.insert("timestamp".to_string(), json!(Utc::now().to_rfc3339()));
    map.insert("app".to_string(), app_context());
    context
}

fn app_context() -> Value {
    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "channel": ChannelState::channel().to_string(),
        "source_commit": crate::build_info::SOURCE_COMMIT,
        "source_modified": crate::build_info::source_is_dirty(),
    })
}

async fn send_remote_log(config: RemoteLogConfig, payload: RemoteLogPayload) -> Result<(), String> {
    let client = REMOTE_LOG_CLIENT.as_ref().map_err(Clone::clone)?;

    let response = client
        .post(&config.endpoint)
        .header("x-api-key", config.api_key)
        .json(&payload)
        .send()
        .await
        .map_err(|error| format!("request failed: {error}"))?;

    if !response.status().is_success() {
        return Err(format!("server returned HTTP {}", response.status()));
    }

    Ok(())
}

fn normalize_endpoint_url(endpoint: &str) -> Option<String> {
    let endpoint = endpoint.trim().trim_end_matches('/');
    if endpoint.is_empty() {
        return None;
    }
    if endpoint.ends_with(DEFAULT_LOGS_PATH) {
        Some(endpoint.to_string())
    } else {
        Some(format!("{endpoint}{DEFAULT_LOGS_PATH}"))
    }
}

fn redact_sensitive_token(token: &str) -> &str {
    let trimmed = token.trim_matches(|character: char| {
        matches!(character, '"' | '\'' | ',' | ';' | ')' | ']' | '}')
    });
    let lower = trimmed.to_ascii_lowercase();
    if trimmed.starts_with("sk-")
        || trimmed.starts_with("log_sk_")
        || lower.starts_with("bearer.")
        || lower.starts_with("bearer:")
        || lower == "bearer"
        || lower == "authorization:"
        || lower == "x-api-key:"
    {
        "[redacted]"
    } else {
        token
    }
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    let mut truncated = value.chars().take(max_chars).collect::<String>();
    truncated.push('…');
    truncated
}

fn tail_limited_payload_context(payload: &str, max_chars: usize) -> Value {
    let total_chars = payload.chars().count();
    let truncated = total_chars > max_chars;
    let payload = if truncated {
        payload
            .chars()
            .skip(total_chars - max_chars)
            .collect::<String>()
    } else {
        payload.to_string()
    };

    json!({
        "payload": payload,
        "payload_total_chars": total_chars,
        "payload_included_chars": if truncated { max_chars } else { total_chars },
        "payload_max_chars": max_chars,
        "truncated": truncated,
        "truncation_strategy": if truncated { Some("tail") } else { None },
    })
}

#[cfg(test)]
#[path = "remote_logging_tests.rs"]
mod tests;
