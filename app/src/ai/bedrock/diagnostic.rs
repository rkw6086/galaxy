use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::Mutex;

use chrono::Utc;
use serde_json::Value as JsonValue;

use super::convert::{ConversationMessage, MessageContent, MessageRole, ToolDefinition};

const ENV_VAR: &str = "GALAXY_BEDROCK_DIAGNOSTICS";
const LOG_FILENAME: &str = "bedrock-diagnostics.log";
const MAX_ROTATIONS: usize = 5;

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
    writer: Mutex<BufWriter<File>>,
    model_id: String,
    conversation_id: Mutex<String>,
    request_id: Mutex<String>,
    task_id: String,
}

impl BedrockDiagnosticLogger {
    pub fn try_new(
        model_id: &str,
        conversation_id: &str,
        request_id: &str,
        task_id: &str,
    ) -> Option<Self> {
        if !is_enabled() {
            return None;
        }

        let log_path = match log_file_path() {
            Some(path) => path,
            None => {
                log::warn!("[bedrock-diag] Could not determine log directory");
                return None;
            }
        };

        if let Some(parent) = log_path.parent() {
            let _ = fs::create_dir_all(parent);
        }

        rotate_if_needed(&log_path);

        let file = match OpenOptions::new().create(true).append(true).open(&log_path) {
            Ok(f) => f,
            Err(e) => {
                log::warn!(
                    "[bedrock-diag] Failed to open log file {:?}: {}",
                    log_path,
                    e
                );
                return None;
            }
        };

        log::info!(
            "[bedrock-diag] Diagnostic logging enabled -> {:?}",
            log_path
        );

        Some(Self {
            writer: Mutex::new(BufWriter::new(file)),
            model_id: model_id.to_string(),
            conversation_id: Mutex::new(conversation_id.to_string()),
            request_id: Mutex::new(request_id.to_string()),
            task_id: task_id.to_string(),
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

        if let Ok(mut writer) = self.writer.lock() {
            let _ = writer.write_all(line.as_bytes());
            let _ = writer.flush();
        }
    }
}

pub fn is_enabled() -> bool {
    std::env::var(ENV_VAR)
        .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn log_file_path() -> Option<PathBuf> {
    galaxy_logging::log_directory()
        .ok()
        .map(|dir| dir.join(LOG_FILENAME))
}

fn rotate_if_needed(path: &PathBuf) {
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
            path.clone()
        } else {
            path.with_extension(format!("log.{}", i))
        };
        let to = path.with_extension(format!("log.{}", i + 1));
        let _ = fs::rename(&from, &to);
    }

    let first_rotation = path.with_extension("log.1");
    let _ = fs::rename(path, &first_rotation);
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
