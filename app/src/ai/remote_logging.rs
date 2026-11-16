//! Opt-in remote application and AI diagnostics logger.
//!
//! This is intentionally separate from product telemetry. It is controlled
//! exclusively by local settings and should only receive operational metadata:
//! provider/model IDs, lifecycle states, counts, timings, and sanitized errors.
//! Native application warnings and errors are captured independently of AI use.

#[cfg(not(target_family = "wasm"))]
mod global_errors;

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
use galaxyui::{Entity, ModelContext, SingletonEntity, View, ViewContext};
use regex::Regex;
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
const MAX_ERROR_INPUT_CHARS: usize = 16 * 1024;
static ERROR_URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)[a-z][a-z0-9+.-]*://[^\s\"'<>]+"#).expect("valid error URL pattern")
});
static ERROR_CREDENTIAL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)[\"'\\]*\b(?:[a-z0-9]+[_-])*(?:authorization|proxy-authorization|x-api-key|api[_-]?key|access[_-]?key(?:[_-]?id)?|secret[_-]?access[_-]?key|client[_-]?secret|private[_-]?key|access[_-]?token|refresh[_-]?token|session[_-]?token|id[_-]?token|cookie|set-cookie|password|secret)\b[\"'\\\s]*[:=]\s*"#)
        .expect("valid error credential pattern")
});
static ERROR_PRIVATE_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?(?:-----END [A-Z ]*PRIVATE KEY-----|$)")
        .expect("valid private key pattern")
});
static ERROR_BEARER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?i)\bbearer\s+[^\s,\"';}\]]+"#).expect("valid bearer pattern"));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RemoteLogLevel {
    Info,
    Warn,
    Error,
}

impl RemoteLogLevel {
    pub(crate) fn as_str(self) -> &'static str {
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
        Self::from_values(
            *settings.remote_logging_enabled.value(),
            settings.remote_logging_endpoint.value(),
            settings.remote_logging_api_key.value(),
        )
    }

    fn from_values(enabled: bool, endpoint: &str, api_key: &str) -> Option<Self> {
        if !enabled {
            return None;
        }

        let api_key = api_key.trim();
        if api_key.is_empty() {
            return None;
        }

        let endpoint = normalize_endpoint_url(endpoint)?;
        Some(Self {
            endpoint,
            api_key: api_key.to_string(),
        })
    }
}

/// Configure before the native logger starts, using the same local settings
/// backend as the app. Startup errors then use the existing diagnostics opt-in.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn initialize_application_logging(
    preferences: &dyn galaxyui_extras::user_preferences::UserPreferences,
) {
    use crate::settings::{RemoteLoggingApiKey, RemoteLoggingEnabled, RemoteLoggingEndpoint};

    let enabled = RemoteLoggingEnabled::read_from_preferences(preferences)
        .unwrap_or_else(RemoteLoggingEnabled::default_value);
    let endpoint = RemoteLoggingEndpoint::read_from_preferences(preferences)
        .unwrap_or_else(RemoteLoggingEndpoint::default_value);
    let api_key = RemoteLoggingApiKey::read_from_preferences(preferences)
        .unwrap_or_else(RemoteLoggingApiKey::default_value);
    global_errors::configure(RemoteLogConfig::from_values(enabled, &endpoint, &api_key));
    galaxy_logging::set_diagnostic_log_sink(global_errors::capture);
}

/// Keep the thread-safe uploader configuration synchronized without reading
/// settings from the logger, where the UI model may already be borrowed.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn register_application_logging(ctx: &mut galaxyui::AppContext) {
    use crate::settings::AISettingsChangedEvent;

    global_errors::configure(RemoteLogConfig::from_settings(AISettings::as_ref(ctx)));
    ctx.subscribe_to_model(&AISettings::handle(ctx), |_, event, ctx| {
        if matches!(
            event,
            AISettingsChangedEvent::RemoteLoggingEnabled { .. }
                | AISettingsChangedEvent::RemoteLoggingEndpoint { .. }
                | AISettingsChangedEvent::RemoteLoggingApiKey { .. }
        ) {
            global_errors::configure(RemoteLogConfig::from_settings(AISettings::as_ref(ctx)));
        }
    });
}

/// Give queued diagnostics a bounded chance to finish during normal exit.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn flush_application_logging(timeout: Duration) {
    global_errors::flush(timeout);
}

#[cfg(not(target_family = "wasm"))]
pub(crate) fn report_application_exit_error(error: &anyhow::Error) {
    if log::log_enabled!(target: "galaxy::startup", log::Level::Error) {
        log::error!(target: "galaxy::startup", "Galaxy stopped with an error: {error:#}");
    } else {
        // Logger setup itself can fail after the opt-in was read. Capture that
        // error directly rather than relying on an uninitialized log facade.
        global_errors::capture(
            &log::Record::builder()
                .level(log::Level::Error)
                .target("galaxy::startup")
                .args(format_args!("Galaxy stopped with an error: {error:#}"))
                .build(),
        );
    }
}

/// Retain the existing panic reporting hook, then give the uploader a short,
/// bounded chance to deliver its diagnostic before unwinding or aborting.
#[cfg(not(target_family = "wasm"))]
pub(crate) fn install_application_panic_flush() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            previous(info);
            global_errors::flush(Duration::from_millis(500));
        }));
    });
}

pub(crate) fn log_model_event<M>(ctx: &mut ModelContext<M>, record: RemoteLogRecord)
where
    M: Entity,
{
    super::activity::record_log(&record);
    if is_raw_payload(&record) && !remote_payloads_enabled(AISettings::as_ref(ctx)) {
        return;
    }
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

/// Views must log without updating AISettings: its singleton cannot be read
/// while it is temporarily borrowed by a model update.
pub(crate) fn log_view_event<V: View>(ctx: &mut ViewContext<V>, record: RemoteLogRecord) {
    super::activity::record_log(&record);
    if is_raw_payload(&record) && !remote_payloads_enabled(AISettings::as_ref(ctx)) {
        return;
    }
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
    // Stop formatting before scanning/redacting an arbitrarily large upstream
    // error body. A truncated diagnostic must never traverse the entire body.
    struct ErrorPrefix {
        text: String,
        remaining: usize,
    }
    impl std::fmt::Write for ErrorPrefix {
        fn write_str(&mut self, value: &str) -> std::fmt::Result {
            if let Some((boundary, _)) = value.char_indices().nth(self.remaining) {
                self.text.push_str(&value[..boundary]);
                self.remaining = 0;
                Err(std::fmt::Error)
            } else {
                self.text.push_str(value);
                self.remaining -= value.chars().count();
                Ok(())
            }
        }
    }
    let mut prefix = ErrorPrefix {
        text: String::new(),
        remaining: MAX_ERROR_INPUT_CHARS,
    };
    let _ = std::fmt::write(&mut prefix, format_args!("{error}"));
    let text = ERROR_PRIVATE_KEY.replace_all(&prefix.text, "[redacted private key]");
    let safe_urls = ERROR_URL.replace_all(&text, |captures: &regex::Captures<'_>| {
        let matched = &captures[0];
        let url_text = matched.trim_end_matches([')', ']', '}', ',', ';']);
        let suffix = &matched[url_text.len()..];
        match reqwest::Url::parse(url_text) {
            Ok(mut url) => {
                let _ = url.set_username("");
                let _ = url.set_password(None);
                url.set_query(None);
                url.set_fragment(None);
                format!("{url}{suffix}")
            }
            Err(_) => format!("[redacted URL]{suffix}"),
        }
    });
    let safe_credentials = redact_error_credentials(&safe_urls);
    let safe_bearers = ERROR_BEARER.replace_all(&safe_credentials, "Bearer [redacted]");
    let compact = safe_bearers
        .split_whitespace()
        .map(redact_sensitive_token)
        .collect::<Vec<_>>()
        .join(" ");
    truncate_chars(&compact, MAX_ERROR_CHARS)
}

fn redact_error_credentials(text: &str) -> String {
    let mut redacted = String::with_capacity(text.len());
    let mut cursor = 0;
    for assignment in ERROR_CREDENTIAL.find_iter(text) {
        // A match inside an already-redacted credential is credential content,
        // not a new assignment. Do not copy any of that content back out.
        if assignment.start() < cursor {
            continue;
        }
        redacted.push_str(&text[cursor..assignment.end()]);
        redacted.push_str("[redacted]");
        cursor = assignment.end() + error_credential_value_end(&text[assignment.end()..]);
    }
    redacted.push_str(&text[cursor..]);
    redacted
}

fn error_credential_value_end(value: &str) -> usize {
    let escapes = value.bytes().take_while(|byte| *byte == b'\\').count();
    if let Some(quote @ (b'"' | b'\'')) = value.as_bytes().get(escapes).copied() {
        // JSON strings can themselves contain escaped JSON. A delimiter at
        // nesting depth N has 2^N-1 preceding slashes; an escaped quote inside
        // its value has a different residue. Preserve the original escaping,
        // so a quoted password cannot be broken open by global unescaping.
        let span = escapes + 1;
        if !span.is_power_of_two() {
            return value.len();
        }
        let mut slashes = 0;
        let start = escapes + 1;
        for (index, character) in value[start..].char_indices() {
            if character == '\\' {
                slashes += 1;
                continue;
            }
            if character as u32 == u32::from(quote) && slashes % (2 * span) == escapes {
                return start + index + 1;
            }
            slashes = 0;
        }
        // An incomplete quoted value may be a truncated provider error body.
        return value.len();
    }
    if value.starts_with('{') || value.starts_with('[') {
        let mut values = serde_json::Deserializer::from_str(value).into_iter::<Value>();
        return if values.next().is_some_and(|result| result.is_ok()) {
            values.byte_offset()
        } else {
            value.len()
        };
    }
    // An unquoted credential/header has no reliable value terminator. It may
    // contain spaces, an authentication scheme, or multiple signed fields.
    // Redact through the line boundary instead of guessing and exposing a suffix.
    value.find(['\r', '\n']).unwrap_or(value.len())
}

pub(crate) fn raw_model_payload_context<M>(
    ctx: &ModelContext<M>,
    build_payload: impl FnOnce(usize) -> Value,
) -> Option<Value>
where
    M: Entity,
{
    let settings = AISettings::as_ref(ctx);
    let remote_enabled = remote_payloads_enabled(settings);
    let local_enabled = super::activity::is_recording();
    if !remote_enabled && !local_enabled {
        return None;
    }

    let remote_max = (*settings.remote_logging_model_payload_max_chars.value()).max(1);
    let max_chars = if remote_enabled {
        remote_max
    } else {
        super::activity::MAX_PAYLOAD_CHARS
    };
    Some(build_payload(max_chars))
}

fn remote_payloads_enabled(settings: &AISettings) -> bool {
    *settings.remote_logging_enabled.value()
        && *settings.remote_logging_log_model_payloads.value()
        && !settings.remote_logging_api_key.value().trim().is_empty()
}

fn is_raw_payload(record: &RemoteLogRecord) -> bool {
    matches!(
        record.context.get("event").and_then(Value::as_str),
        Some("raw_model_request" | "raw_model_response")
    )
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
    send_remote_log_with_client(client, config, payload).await
}

async fn send_remote_log_with_client(
    client: &reqwest::Client,
    config: RemoteLogConfig,
    payload: RemoteLogPayload,
) -> Result<(), String> {
    let response = client
        .post(&config.endpoint)
        .header("x-api-key", config.api_key)
        .json(&payload)
        .send()
        .await
        .map_err(|error| format!("request failed: {}", sanitize_error(error)))?;

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
        || trimmed.starts_with("AKIA")
        || trimmed.starts_with("ASIA")
        || trimmed.starts_with("ghp_")
        || trimmed.starts_with("github_pat_")
        || trimmed.starts_with("glpat-")
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

/// Stop serialization once the diagnostic prefix is full. Unlike the tail
/// collector below, this does not walk the remaining history or tool archive.
/// A truncated prefix is diagnostic text, not a complete serialized request.
pub(crate) fn prefix_bounded_payload_context(
    max_chars: usize,
    serialize: impl FnOnce(&mut dyn std::io::Write) -> serde_json::Result<()>,
) -> Value {
    struct PrefixWriter {
        bytes: Vec<u8>,
        max_chars: usize,
        max_bytes: usize,
        chars: usize,
        truncated: bool,
    }

    impl std::io::Write for PrefixWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.truncated {
                return Err(std::io::Error::other("diagnostic prefix limit reached"));
            }
            let mut included = 0;
            for byte in bytes {
                let starts_character = byte & 0xc0 != 0x80;
                if (starts_character && self.chars == self.max_chars)
                    || self.bytes.len().saturating_add(included) == self.max_bytes
                {
                    self.bytes.extend_from_slice(&bytes[..included]);
                    self.truncated = true;
                    return if included == 0 {
                        Err(std::io::Error::other("diagnostic prefix limit reached"))
                    } else {
                        // Respect Write's short-write contract. serde's write_all
                        // immediately retries the remainder and receives the error.
                        Ok(included)
                    };
                }
                if starts_character {
                    self.chars += 1;
                }
                included += 1;
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut writer = PrefixWriter {
        bytes: Vec::new(),
        max_chars,
        max_bytes: max_chars.saturating_mul(4),
        chars: 0,
        truncated: false,
    };
    let result = serialize(&mut writer);
    if let Err(error) = result {
        if !writer.truncated {
            return json!({ "serialization_error": sanitize_error(error) });
        }
    }
    // Writers may split a UTF-8 character across calls. Keep only a complete
    // prefix if serialization stopped before the last character was finished.
    let valid_length = match std::str::from_utf8(&writer.bytes) {
        Ok(_) => writer.bytes.len(),
        Err(error) => error.valid_up_to(),
    };
    writer.bytes.truncate(valid_length);
    let payload = String::from_utf8(writer.bytes).expect("validated diagnostic UTF-8 prefix");
    let included_chars = payload.chars().count();
    json!({
        "payload": payload,
        "payload_total_chars": if writer.truncated { None } else { Some(included_chars) },
        "payload_included_chars": included_chars,
        "payload_max_chars": max_chars,
        "truncated": writer.truncated,
        "truncation_strategy": if writer.truncated { Some("prefix") } else { None },
        "serialization_stopped_early": writer.truncated,
    })
}

/// Serialize directly into a bounded UTF-8 tail instead of materializing the full
/// request and its archive as a JSON Value and then another large String.
pub(crate) fn bounded_payload_context(
    max_chars: usize,
    serialize: impl FnOnce(&mut dyn std::io::Write) -> serde_json::Result<()>,
) -> Value {
    struct TailWriter {
        bytes: std::collections::VecDeque<u8>,
        max_bytes: usize,
        total_chars: usize,
    }

    impl std::io::Write for TailWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.total_chars += bytes.iter().filter(|byte| **byte & 0xc0 != 0x80).count();
            let retained = &bytes[bytes.len().saturating_sub(self.max_bytes)..];
            let discard = (self.bytes.len() + retained.len()).saturating_sub(self.max_bytes);
            self.bytes.drain(..discard);
            self.bytes.extend(retained);
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let mut writer = TailWriter {
        bytes: std::collections::VecDeque::new(),
        max_bytes: max_chars.saturating_mul(4),
        total_chars: 0,
    };
    if let Err(error) = serialize(&mut writer) {
        return json!({ "serialization_error": sanitize_error(error) });
    }
    // A byte-limited tail can begin in the middle of a multibyte character.
    while writer.bytes.front().is_some_and(|byte| byte & 0xc0 == 0x80) {
        writer.bytes.pop_front();
    }
    let bytes = writer.bytes.into_iter().collect::<Vec<_>>();
    let payload = String::from_utf8_lossy(&bytes);
    let mut context = tail_limited_payload_context(&payload, max_chars);
    context["payload_total_chars"] = json!(writer.total_chars);
    context["truncated"] = json!(writer.total_chars > max_chars);
    context["truncation_strategy"] = json!(if writer.total_chars > max_chars {
        Some("tail")
    } else {
        None
    });
    context
}

pub(crate) fn tail_limited_payload_context(payload: &str, max_chars: usize) -> Value {
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
