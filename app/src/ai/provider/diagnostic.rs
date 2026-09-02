#![allow(dead_code)]

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use chrono::{Local, Utc};
use serde_json::Value as JsonValue;

use super::convert::{ConversationMessage, MessageContent, MessageRole, ToolDefinition};

const ENV_VAR: &str = "GALAXY_BEDROCK_DIAGNOSTICS";
const LOG_FILENAME: &str = "bedrock-diagnostics.log";
const MAX_ROTATIONS: usize = 5;
const ERROR_DUMP_PREFIX: &str = "Error_";
const MAX_CAPTURED_LINES: usize = 2_000;
const LOG_TAIL_BYTES: u64 = 200 * 1024;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Layer {
    Protobuf,
    Bedrock,
}

impl std::fmt::Display for Layer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Layer::Protobuf => write!(f, "PROTOBUF"),
            Layer::Bedrock => write!(f, "BEDROCK"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Direction {
    Input,
    Stream,
    Result,
}

impl std::fmt::Display for Direction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Direction::Input => write!(f, "INPUT"),
            Direction::Stream => write!(f, "STREAM"),
            Direction::Result => write!(f, "RESULT"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Status {
    Pending,
    Success,
    Fail,
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Status::Pending => write!(f, "PENDING"),
            Status::Success => write!(f, "SUCCESS"),
            Status::Fail => write!(f, "FAIL"),
        }
    }
}

pub struct BedrockDiagnosticLogger {
    writer: Option<Mutex<BufWriter<File>>>,
    log_path: Option<PathBuf>,
    model_id: String,
    conversation_id: Mutex<String>,
    request_id: Mutex<String>,
    task_id: String,
    protobuf_input: Mutex<Option<String>>,
    bedrock_input: Mutex<Option<JsonValue>>,
    captured_lines: Mutex<Vec<String>>,
}

impl BedrockDiagnosticLogger {
    pub fn try_new(
        model_id: &str,
        conversation_id: &str,
        request_id: &str,
        task_id: &str,
    ) -> Option<Self> {
        let mut writer = None;
        let mut log_path = None;

        if is_enabled() {
            if let Some(path) = diagnostic_log_file_path() {
                if let Some(parent) = path.parent() {
                    let _ = fs::create_dir_all(parent);
                }

                rotate_if_needed(&path);

                match OpenOptions::new().create(true).append(true).open(&path) {
                    Ok(file) => {
                        writer = Some(Mutex::new(BufWriter::new(file)));
                        log_path = Some(path.clone());
                        log::info!("[bedrock-diag] Diagnostic logging enabled -> {path:?}");
                    }
                    Err(e) => {
                        log::warn!("[bedrock-diag] Failed to open log file {path:?}: {e}");
                    }
                }
            } else {
                log::warn!("[bedrock-diag] Could not determine log directory");
            }
        }

        Some(Self {
            writer,
            log_path,
            model_id: model_id.to_string(),
            conversation_id: Mutex::new(conversation_id.to_string()),
            request_id: Mutex::new(request_id.to_string()),
            task_id: task_id.to_string(),
            protobuf_input: Mutex::new(None),
            bedrock_input: Mutex::new(None),
            captured_lines: Mutex::new(Vec::new()),
        })
    }

    pub fn set_ids(&self, conversation_id: &str, request_id: &str) {
        if let Ok(mut cid) = self.conversation_id.lock() {
            *cid = conversation_id.to_string();
        }
        if let Ok(mut rid) = self.request_id.lock() {
            *rid = request_id.to_string();
        }
    }

    pub fn log_protobuf_input(&self, request: &warp_multi_agent_api::Request) {
        let payload = format!("{:?}", request);
        if let Ok(mut protobuf_input) = self.protobuf_input.lock() {
            *protobuf_input = Some(payload.clone());
        }
        self.write_line(Layer::Protobuf, Direction::Input, Status::Pending, &payload);
    }

    pub fn log_bedrock_input(
        &self,
        messages: &[ConversationMessage],
        system_prompt: &Option<String>,
        tools: &[ToolDefinition],
        max_tokens: i32,
        temperature: Option<f32>,
        cross_region_inference: bool,
    ) {
        let messages_json = serialize_messages(messages);
        let tools_json = serialize_tools(tools);

        let payload = serde_json::json!({
            "model_id": self.model_id,
            "cross_region_inference": cross_region_inference,
            "max_tokens": max_tokens,
            "temperature": temperature,
            "system_prompt": system_prompt,
            "messages": messages_json,
            "tools": tools_json,
        });
        if let Ok(mut bedrock_input) = self.bedrock_input.lock() {
            *bedrock_input = Some(payload.clone());
        }

        self.write_line(
            Layer::Bedrock,
            Direction::Input,
            Status::Pending,
            &payload.to_string(),
        );
    }

    pub fn log_stream_event(&self, event_description: &str) {
        self.write_line(
            Layer::Bedrock,
            Direction::Stream,
            Status::Success,
            event_description,
        );
    }

    pub fn log_stream_error(&self, error: &str) {
        self.write_line(Layer::Bedrock, Direction::Stream, Status::Fail, error);
    }

    pub fn log_result_success(&self, input_tokens: i32, output_tokens: i32, stop_reason: &str) {
        let payload = serde_json::json!({
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "stop_reason": stop_reason,
        });
        self.write_line(
            Layer::Bedrock,
            Direction::Result,
            Status::Success,
            &payload.to_string(),
        );
    }

    pub fn log_result_fail(&self, error: &str) {
        let payload = serde_json::json!({
            "error": error,
        });
        self.write_line(
            Layer::Bedrock,
            Direction::Result,
            Status::Fail,
            &payload.to_string(),
        );
    }

    pub fn dump_error_snapshot(&self, error: &str, debug_error: &str) -> Option<PathBuf> {
        if !is_enabled() {
            return None;
        }
        let conversation_id = self
            .conversation_id
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default();
        let request_id = self
            .request_id
            .lock()
            .map(|value| value.clone())
            .unwrap_or_default();
        let protobuf_input = self
            .protobuf_input
            .lock()
            .ok()
            .and_then(|value| value.clone());
        let bedrock_input = self
            .bedrock_input
            .lock()
            .ok()
            .and_then(|value| value.clone());
        let captured_lines = self
            .captured_lines
            .lock()
            .map(|lines| lines.clone())
            .unwrap_or_default();

        let current_log_path = galaxy_logging::log_file_path().ok();
        let current_log_tail = current_log_path
            .as_ref()
            .and_then(|path| read_file_tail(path, LOG_TAIL_BYTES));
        let diagnostics_log_tail = self
            .log_path
            .as_ref()
            .and_then(|path| read_file_tail(path, LOG_TAIL_BYTES));
        let context_window = bedrock_input
            .as_ref()
            .and_then(|value| value.get("messages").cloned())
            .unwrap_or(JsonValue::Null);
        let tools = bedrock_input
            .as_ref()
            .and_then(|value| value.get("tools").cloned())
            .unwrap_or(JsonValue::Null);

        let payload = serde_json::json!({
            "timestamp_local": Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "timestamp_utc": Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
            "error": {
                "display": error,
                "debug": debug_error,
            },
            "bedrock": {
                "model_id": &self.model_id,
                "task_id": &self.task_id,
                "conversation_id": conversation_id,
                "request_id": request_id,
                "input": bedrock_input,
                "context_window": context_window,
                "tools": tools,
            },
            "protobuf_request_debug": protobuf_input,
            "captured_bedrock_lines": captured_lines,
            "logs": {
                "warp_log_path": current_log_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
                "warp_log_tail": current_log_tail,
                "bedrock_diagnostics_log_path": self
                    .log_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
                "bedrock_diagnostics_log_tail": diagnostics_log_tail,
            }
        });

        let file_name = format!(
            "{ERROR_DUMP_PREFIX}{}.txt",
            Local::now().format("%Y%m%d_%H%M%S_%3f")
        );
        let serialized_payload = match serde_json::to_string_pretty(&payload) {
            Ok(value) => value,
            Err(e) => {
                log::error!("[bedrock] Failed to serialize Bedrock error snapshot: {e}");
                return None;
            }
        };

        let mut attempted_paths = Vec::new();
        for dump_path in error_dump_paths(&file_name) {
            match fs::write(&dump_path, &serialized_payload) {
                Ok(()) => return Some(dump_path),
                Err(e) => {
                    attempted_paths.push(format!("{} ({e})", dump_path.display()));
                }
            }
        }
        log::error!(
            "[bedrock] Failed to write Bedrock error snapshot. Attempted paths: {}",
            attempted_paths.join(", ")
        );
        None
    }

    fn write_line(&self, layer: Layer, direction: Direction, status: Status, payload: &str) {
        let timestamp = Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let conversation_id = self
            .conversation_id
            .lock()
            .map(|c| c.clone())
            .unwrap_or_default();
        let request_id = self
            .request_id
            .lock()
            .map(|r| r.clone())
            .unwrap_or_default();
        let line = format!(
            "[{}][{}][{}][{}][{}][{}][{}][{}] {}\n",
            timestamp,
            layer,
            direction,
            self.model_id,
            status,
            conversation_id,
            request_id,
            self.task_id,
            payload,
        );

        if let Ok(mut captured_lines) = self.captured_lines.lock() {
            captured_lines.push(line.trim_end().to_string());
            if captured_lines.len() > MAX_CAPTURED_LINES {
                let overflow = captured_lines.len() - MAX_CAPTURED_LINES;
                captured_lines.drain(0..overflow);
            }
        }

        if let Some(writer) = &self.writer {
            if let Ok(mut writer) = writer.lock() {
                let _ = writer.write_all(line.as_bytes());
                let _ = writer.flush();
            }
        }
    }
}

pub fn is_enabled() -> bool {
    std::env::var(ENV_VAR)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn diagnostic_log_file_path() -> Option<PathBuf> {
    galaxy_logging::log_directory()
        .ok()
        .map(|dir| dir.join(LOG_FILENAME))
}

fn error_dump_directory() -> PathBuf {
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or_else(|| Path::new(env!("CARGO_MANIFEST_DIR")))
        .to_path_buf();

    if source_root.is_dir() {
        source_root
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }
}

fn error_dump_paths(file_name: &str) -> Vec<PathBuf> {
    let mut directories = Vec::new();
    push_unique_directory(&mut directories, error_dump_directory());
    if let Ok(current_dir) = std::env::current_dir() {
        push_unique_directory(&mut directories, current_dir);
    }
    push_unique_directory(&mut directories, std::env::temp_dir());

    directories
        .into_iter()
        .map(|directory| directory.join(file_name))
        .collect()
}

fn push_unique_directory(directories: &mut Vec<PathBuf>, directory: PathBuf) {
    if directory.is_dir() && !directories.iter().any(|existing| existing == &directory) {
        directories.push(directory);
    }
}
fn rotate_if_needed(path: &Path) {
    let metadata = match fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return,
    };

    const TEN_MB: u64 = 10 * 1024 * 1024;
    if metadata.len() < TEN_MB {
        return;
    }

    for i in (0..MAX_ROTATIONS - 1).rev() {
        let from = if i == 0 {
            path.to_path_buf()
        } else {
            path.with_extension(format!("log.{}", i))
        };
        let to = path.with_extension(format!("log.{}", i + 1));
        let _ = fs::rename(&from, &to);
    }

    let first_rotation = path.with_extension("log.1");
    let _ = fs::rename(path, &first_rotation);
}

fn read_file_tail(path: &Path, max_bytes: u64) -> Option<String> {
    let mut file = File::open(path).ok()?;
    let file_len = file.metadata().ok()?.len();
    let start = file_len.saturating_sub(max_bytes);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return None;
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return None;
    }

    let mut tail = String::from_utf8_lossy(&bytes).to_string();
    if start > 0 {
        tail = format!("... log tail truncated to last {max_bytes} bytes ...\n{tail}");
    }
    Some(tail)
}

fn serialize_messages(messages: &[ConversationMessage]) -> JsonValue {
    let entries: Vec<JsonValue> = messages
        .iter()
        .map(|msg| {
            let role = match msg.role {
                MessageRole::User => "user",
                MessageRole::Assistant => "assistant",
            };
            let content = match &msg.content {
                MessageContent::Text(t) => serde_json::json!({"type": "text", "text": t}),
                MessageContent::ToolUse {
                    tool_use_id,
                    name,
                    input,
                } => {
                    serde_json::json!({
                        "type": "tool_use",
                        "tool_use_id": tool_use_id,
                        "name": name,
                        "input": input,
                    })
                }
                MessageContent::ToolResult {
                    tool_use_id,
                    content,
                    is_error,
                } => {
                    serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": content,
                        "is_error": is_error,
                    })
                }
                MessageContent::MultiPart(parts) => {
                    let part_values: Vec<JsonValue> = parts
                        .iter()
                        .map(|p| match p {
                            super::convert::ContentPart::Text(t) => {
                                serde_json::json!({"type": "text", "text": t})
                            }
                            super::convert::ContentPart::Reasoning { text, signature } => {
                                serde_json::json!({
                                    "type": "reasoning",
                                    "char_length": text.len(),
                                    "has_signature": signature.is_some(),
                                    "text": "REDACTED",
                                })
                            }
                            super::convert::ContentPart::Image { data, mime_type } => {
                                serde_json::json!({
                                    "type": "image",
                                    "mime_type": mime_type,
                                    "byte_length": data.len(),
                                    "data": "REDACTED",
                                })
                            }
                            super::convert::ContentPart::ToolUse {
                                tool_use_id,
                                name,
                                input,
                            } => {
                                serde_json::json!({
                                    "type": "tool_use",
                                    "tool_use_id": tool_use_id,
                                    "name": name,
                                    "input": input,
                                })
                            }
                            super::convert::ContentPart::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                            } => {
                                serde_json::json!({
                                    "type": "tool_result",
                                    "tool_use_id": tool_use_id,
                                    "content": content,
                                    "is_error": is_error,
                                })
                            }
                        })
                        .collect();
                    serde_json::json!({"type": "multi_part", "parts": part_values})
                }
            };
            serde_json::json!({"role": role, "content": content})
        })
        .collect();
    JsonValue::Array(entries)
}

fn serialize_tools(tools: &[ToolDefinition]) -> JsonValue {
    let entries: Vec<JsonValue> = tools
        .iter()
        .map(|t| {
            serde_json::json!({
                "name": t.name,
                "description": t.description,
                "input_schema": t.input_schema,
            })
        })
        .collect();
    JsonValue::Array(entries)
}
