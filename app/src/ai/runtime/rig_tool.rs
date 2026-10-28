use std::collections::{HashMap, HashSet};
use std::time::Duration;

use ai::diff_validation::ParsedDiff;
use ai::skills::{SkillPathOrigin, SkillReference};
use galaxy_agent_core::ToolCall;
use uuid::Uuid;

use super::rig_request::MCPToolTarget;
use crate::ai::agent::task::TaskId;
use crate::ai::agent::{
    AIAgentAction, AIAgentActionType, AIAgentPtyWriteMode, AskUserQuestionItem,
    AskUserQuestionOption, AskUserQuestionType, CreateDocumentsRequest, DocumentDiff,
    DocumentToCreate, EditDocumentsRequest, FileEdit, FileLocations, ReadDocumentsRequest,
    ReadFilesRequest, ReadSkillRequest, RunAgentsAgentRunConfig, RunAgentsExecutionMode,
    RunAgentsRequest, SearchCodebaseRequest, ShellCommandDelay,
};
use crate::ai::document::ai_document_model::AIDocumentId;

const MAX_BATCH_READ_ITEMS: usize = 6;
const MAX_BATCH_SEARCH_ITEMS: usize = 3;
const MAX_RUN_AGENTS: usize = 8;

pub(crate) fn action_from_tool_call(
    task_id: &str,
    call: &ToolCall,
    skill_path_origin: &SkillPathOrigin,
    mcp_tool_aliases: &HashMap<String, MCPToolTarget>,
) -> Result<AIAgentAction, String> {
    let input = &call.arguments;
    if !input.is_object() {
        return Err(format!("invalid {} input: expected an object", call.name));
    }
    let action = if let Some(target) = mcp_tool_aliases.get(&call.name) {
        AIAgentActionType::CallMCPTool {
            server_id: target.server_id,
            name: target.name.clone(),
            input: {
                let mut arguments = input.clone();
                if let Some(parameter) = &target.title_parameter {
                    arguments.as_object_mut().unwrap().remove(parameter);
                }
                arguments
            },
        }
    } else {
        match call.name.as_str() {
            "run_shell_command" => AIAgentActionType::RequestCommandOutput {
                command: required_nonempty_string(input, "command")?,
                is_read_only: Some(optional_boolean(input, "is_read_only")?.unwrap_or(false)),
                is_risky: Some(optional_boolean(input, "is_risky")?.unwrap_or(false)),
                wait_until_completion: optional_boolean(input, "wait_until_complete")?
                    .unwrap_or(false),
                uses_pager: Some(optional_boolean(input, "uses_pager")?.unwrap_or(false)),
                rationale: optional_string(input, "rationale")?,
                citations: Vec::new(),
            },
            "request_computer_use" => AIAgentActionType::RequestComputerUse(
                crate::ai::agent::RequestComputerUseRequest {
                    task_summary: required_nonempty_string(input, "task_summary")?,
                    screenshot_params: Some(computer_screenshot_params(input)?),
                },
            ),
            "use_computer" => AIAgentActionType::UseComputer(
                crate::ai::agent::UseComputerRequest {
                    action_summary: required_nonempty_string(input, "action_summary")?,
                    actions: computer_actions(input)?,
                    screenshot_params: if optional_boolean(input, "take_screenshot")?.unwrap_or(true) {
                        Some(computer_screenshot_params(input)?)
                    } else {
                        None
                    },
                },
            ),
            "read_files" => AIAgentActionType::ReadFiles(ReadFilesRequest {
                locations: limited_required_array(input, "files", MAX_BATCH_READ_ITEMS)?
                    .iter()
                    .enumerate()
                    .map(|(index, file)| file_location(file, index))
                    .collect::<Result<_, _>>()?,
            }),
            "apply_file_diffs" => AIAgentActionType::RequestFileEdits {
                file_edits: file_edits(input)?,
                title: Some(required_string(input, "summary")?),
            },
            "grep" => AIAgentActionType::Grep {
                queries: limited_required_strings(input, "queries", MAX_BATCH_SEARCH_ITEMS)?,
                path: optional_string(input, "path")?.unwrap_or_default(),
            },
            "file_glob" => AIAgentActionType::FileGlob {
                patterns: limited_required_strings(input, "patterns", MAX_BATCH_SEARCH_ITEMS)?,
                path: optional_string(input, "path")?.filter(|path| !path.is_empty()),
            },
            "search_codebase" => AIAgentActionType::SearchCodebase(SearchCodebaseRequest {
                query: required_string(input, "query")?,
                partial_paths: optional_strings(input, "path_filters")?
                    .filter(|paths| !paths.is_empty()),
                codebase_path: optional_string(input, "path")?.filter(|path| !path.is_empty()),
            }),
            "write_to_long_running_shell_command" => {
                AIAgentActionType::WriteToLongRunningShellCommand {
                    block_id: required_nonempty_string(input, "command_id")?.into(),
                    input: required_string(input, "input")?.into_bytes().into(),
                    mode: match optional_string(input, "mode")?.as_deref() {
                        Some("line") => AIAgentPtyWriteMode::Line,
                        Some("block") => AIAgentPtyWriteMode::Block,
                        Some("raw") | None => AIAgentPtyWriteMode::Raw,
                        Some(mode) => {
                            return Err(format!(
                            "invalid field \"mode\": expected \"raw\", \"line\", or \"block\", got {mode:?}"
                        ));
                        }
                    },
                }
            }
            "interrupt_shell_command" => AIAgentActionType::WriteToLongRunningShellCommand {
                block_id: required_nonempty_string(input, "command_id")?.into(),
                input: vec![galaxy_terminal::model::escape_sequences::C0::ETX].into(),
                mode: AIAgentPtyWriteMode::Raw,
            },
            "read_shell_command_output" => AIAgentActionType::ReadShellCommandOutput {
                block_id: required_nonempty_string(input, "command_id")?.into(),
                delay: Some(ShellCommandDelay::Duration(Duration::from_secs(
                    optional_bounded_u64(
                        input,
                        "wait_seconds",
                        crate::ai::provider::request_translator::COMMAND_MONITOR_MAX_POLL_SECONDS,
                    )?
                    .unwrap_or(2),
                ))),
            },
            "read_mcp_resource" => AIAgentActionType::ReadMCPResource {
                server_id: Some(required_uuid(input, "server_id")?),
                name: String::new(),
                uri: Some(required_string(input, "uri")?),
            },
            "read_plan" | "read_documents" | "read_notebook" => {
                AIAgentActionType::ReadDocuments(ReadDocumentsRequest {
                    document_ids: limited_required_strings(
                        input,
                        "document_ids",
                        MAX_BATCH_READ_ITEMS,
                    )?
                    .into_iter()
                    .map(|id| {
                        AIDocumentId::try_from(id.clone()).map_err(|_| {
                            format!("invalid document_ids entry: {id:?} is not a document ID")
                        })
                    })
                    .collect::<Result<_, _>>()?,
                })
            }
            "create_plan" | "create_documents" | "create_notebook" => {
                AIAgentActionType::CreateDocuments(CreateDocumentsRequest {
                    documents: limited_required_array(input, "documents", 1)?
                        .iter()
                        .enumerate()
                        .map(|(index, document)| {
                            require_object(document, &format!("documents[{index}]"))?;
                            Ok(DocumentToCreate {
                                title: required_string(document, "title")?,
                                content: required_string(document, "content")?,
                            })
                        })
                        .collect::<Result<_, String>>()?,
                })
            }
            "edit_plan" | "edit_documents" | "edit_notebook" => {
                AIAgentActionType::EditDocuments(EditDocumentsRequest {
                    diffs: limited_required_array(input, "diffs", 1)?
                        .iter()
                        .enumerate()
                        .map(|(index, diff)| {
                            require_object(diff, &format!("diffs[{index}]"))?;
                            let document_id = required_string(diff, "document_id")?;
                            Ok(DocumentDiff {
                                document_id: AIDocumentId::try_from(document_id.clone())
                                    .map_err(|_| format!("invalid document_id: {document_id:?}"))?,
                                search: required_string(diff, "search")?,
                                replace: required_string(diff, "replace")?,
                            })
                        })
                        .collect::<Result<_, String>>()?,
                })
            }
            "run_agents" => AIAgentActionType::RunAgents(RunAgentsRequest {
                summary: required_nonempty_string(input, "summary")?,
                base_prompt: optional_string(input, "base_prompt")?.unwrap_or_default(),
                skills: skill_references(input, skill_path_origin)?,
                model_id: optional_string(input, "model_id")?.unwrap_or_default(),
                harness_type: optional_string(input, "harness_type")?.unwrap_or_default(),
                execution_mode: run_agents_execution_mode(input)?,
                agent_run_configs: limited_nonempty_required_array(
                    input,
                    "agent_run_configs",
                    MAX_RUN_AGENTS,
                )?
                .iter()
                .enumerate()
                .map(|(index, config)| {
                    require_object(config, &format!("agent_run_configs[{index}]"))?;
                    Ok(RunAgentsAgentRunConfig {
                        name: required_nonempty_string(config, "name")?,
                        prompt: required_nonempty_string(config, "prompt")?,
                        title: optional_string(config, "title")?.unwrap_or_default(),
                        model_id: optional_string(config, "model_id")?.unwrap_or_default(),
                    })
                })
                .collect::<Result<_, String>>()?,
                plan_id: optional_string(input, "plan_id")?.unwrap_or_default(),
                harness_auth_secret_name: None,
            }),
            // Direct-provider single-child delegation uses the same durable group lifecycle.
            "start_agent" => AIAgentActionType::RunAgents(RunAgentsRequest {
                summary: required_nonempty_string(input, "name")?,
                base_prompt: String::new(),
                skills: Vec::new(),
                model_id: String::new(),
                harness_type: String::new(),
                execution_mode: RunAgentsExecutionMode::Local,
                agent_run_configs: vec![RunAgentsAgentRunConfig {
                    name: required_nonempty_string(input, "name")?,
                    prompt: required_nonempty_string(input, "prompt")?,
                    title: String::new(),
                    model_id: String::new(),
                }],
                plan_id: String::new(),
                harness_auth_secret_name: None,
            }),
            "send_message_to_agent" => AIAgentActionType::SendMessageToAgent {
                addresses: vec![required_string(input, "agent_id")?],
                subject: String::new(),
                message: required_string(input, "message")?,
            },
            "transfer_shell_command_control_to_user" => {
                AIAgentActionType::TransferShellCommandControlToUser {
                    reason: required_nonempty_string(input, "reason")?,
                }
            }
            "wait_for_events" => AIAgentActionType::WaitForEvents {
                tool_call_id: call.id.clone(),
                idle_timeout_seconds: optional_nonnegative_i32(input, "idle_timeout_seconds")?
                    .unwrap_or(0),
            },
            "ask_user_question" => AIAgentActionType::AskUserQuestion {
                questions: vec![AskUserQuestionItem {
                    question_id: Uuid::new_v4().to_string(),
                    question: required_string(input, "question")?,
                    question_type: AskUserQuestionType::MultipleChoice {
                        is_multiselect: false,
                        options: optional_strings(input, "options")?
                            .unwrap_or_default()
                            .into_iter()
                            .enumerate()
                            .map(|(index, label)| AskUserQuestionOption {
                                label,
                                recommended: index == 0,
                            })
                            .collect(),
                        supports_other: true,
                    },
                }],
            },
            "read_skill" => {
                let skill = required_string(input, "skill")?;
                let skill = match required_string(input, "reference_type")?.as_str() {
                    "bundled" => SkillReference::BundledSkillId(skill),
                    "path" => SkillReference::Path(
                        skill_path_origin
                            .location_for_path(skill)
                            .map_err(|error| error.to_string())?,
                    ),
                    reference_type => {
                        return Err(format!(
                        "invalid reference_type: expected \"path\" or \"bundled\", got {reference_type:?}"
                    ));
                    }
                };
                AIAgentActionType::ReadSkill(ReadSkillRequest { skill })
            }
            "fetch_conversation" => AIAgentActionType::FetchConversation {
                conversation_id: required_string(input, "conversation_id")?,
            },
            name if name.starts_with("mcp__") => {
                let mut parts = name.splitn(3, "__");
                let _prefix = parts.next();
                let server_id = parts.next().and_then(|value| Uuid::parse_str(value).ok());
                let name = parts
                    .next()
                    .unwrap_or_else(|| name.strip_prefix("mcp__").unwrap_or(name))
                    .to_string();
                AIAgentActionType::CallMCPTool {
                    server_id,
                    name,
                    input: input.clone(),
                }
            }
            name => return Err(format!("unsupported Rig tool proposal: {name}")),
        }
    };

    let tool_name = matches!(
        call.name.as_str(),
        "read_notebook" | "create_notebook" | "edit_notebook"
    )
    .then(|| "notebook".to_string());
    Ok(AIAgentAction {
        id: call.id.clone().into(),
        task_id: TaskId::new(task_id.to_string()),
        action,
        requires_result: true,
        tool_name,
    })
}

fn require_object(input: &serde_json::Value, field: &str) -> Result<(), String> {
    input
        .is_object()
        .then_some(())
        .ok_or_else(|| format!("invalid {field}: expected an object"))
}

fn required_string(input: &serde_json::Value, key: &str) -> Result<String, String> {
    input
        .get(key)
        .ok_or_else(|| format!("missing required field {key:?}"))?
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("invalid field {key:?}: expected a string"))
}

fn required_nonempty_string(input: &serde_json::Value, key: &str) -> Result<String, String> {
    let value = required_string(input, key)?;
    if value.trim().is_empty() {
        return Err(format!(
            "invalid field {key:?}: expected a non-empty string"
        ));
    }
    Ok(value)
}

fn optional_string(input: &serde_json::Value, key: &str) -> Result<Option<String>, String> {
    input
        .get(key)
        .map(|value| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| format!("invalid field {key:?}: expected a string"))
        })
        .transpose()
}

fn required_array<'a>(
    input: &'a serde_json::Value,
    key: &str,
) -> Result<&'a Vec<serde_json::Value>, String> {
    input
        .get(key)
        .ok_or_else(|| format!("missing required field {key:?}"))?
        .as_array()
        .ok_or_else(|| format!("invalid field {key:?}: expected an array"))
}

fn limited_required_array<'a>(
    input: &'a serde_json::Value,
    key: &str,
    maximum: usize,
) -> Result<&'a Vec<serde_json::Value>, String> {
    let values = required_array(input, key)?;
    if values.len() > maximum {
        return Err(format!(
            "invalid field {key:?}: expected no more than {maximum} items, got {}",
            values.len()
        ));
    }
    Ok(values)
}

fn nonempty_required_array<'a>(
    input: &'a serde_json::Value,
    key: &str,
) -> Result<&'a Vec<serde_json::Value>, String> {
    let values = required_array(input, key)?;
    if values.is_empty() {
        return Err(format!("invalid field {key:?}: expected at least one item"));
    }
    Ok(values)
}

fn limited_nonempty_required_array<'a>(
    input: &'a serde_json::Value,
    key: &str,
    maximum: usize,
) -> Result<&'a Vec<serde_json::Value>, String> {
    let values = limited_required_array(input, key, maximum)?;
    if values.is_empty() {
        return Err(format!("invalid field {key:?}: expected at least one item"));
    }
    Ok(values)
}

fn required_strings(input: &serde_json::Value, key: &str) -> Result<Vec<String>, String> {
    strings_from_array(required_array(input, key)?, key)
}

fn limited_required_strings(
    input: &serde_json::Value,
    key: &str,
    maximum: usize,
) -> Result<Vec<String>, String> {
    strings_from_array(limited_required_array(input, key, maximum)?, key)
}

fn optional_strings(input: &serde_json::Value, key: &str) -> Result<Option<Vec<String>>, String> {
    input
        .get(key)
        .map(|value| {
            let values = value
                .as_array()
                .ok_or_else(|| format!("invalid field {key:?}: expected an array"))?;
            strings_from_array(values, key)
        })
        .transpose()
}

fn strings_from_array(values: &[serde_json::Value], key: &str) -> Result<Vec<String>, String> {
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            value
                .as_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| format!("invalid {key}[{index}]: expected a string"))
        })
        .collect()
}

fn required_uuid(input: &serde_json::Value, key: &str) -> Result<Uuid, String> {
    let value = required_string(input, key)?;
    Uuid::parse_str(&value).map_err(|_| format!("invalid field {key:?}: expected a UUID"))
}

fn optional_boolean(input: &serde_json::Value, key: &str) -> Result<Option<bool>, String> {
    input
        .get(key)
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| format!("invalid field {key:?}: expected a boolean"))
        })
        .transpose()
}

fn optional_bounded_u64(
    input: &serde_json::Value,
    key: &str,
    maximum: u64,
) -> Result<Option<u64>, String> {
    input
        .get(key)
        .map(|value| {
            let value = value
                .as_u64()
                .ok_or_else(|| format!("invalid field {key:?}: expected a non-negative integer"))?;
            if value > maximum {
                return Err(format!(
                    "invalid field {key:?}: expected an integer no greater than {maximum}"
                ));
            }
            Ok(value)
        })
        .transpose()
}

fn optional_nonnegative_i32(input: &serde_json::Value, key: &str) -> Result<Option<i32>, String> {
    input
        .get(key)
        .map(|value| {
            value
                .as_i64()
                .and_then(|value| i32::try_from(value).ok())
                .filter(|value| *value >= 0)
                .ok_or_else(|| {
                    format!("invalid field {key:?}: expected a non-negative 32-bit integer")
                })
        })
        .transpose()
}

fn computer_i32(input: &serde_json::Value, key: &str) -> Result<i32, String> {
    input.get(key).and_then(serde_json::Value::as_i64)
        .and_then(|value| i32::try_from(value).ok())
        .ok_or_else(|| format!("invalid field {key:?}: expected a 32-bit integer"))
}

fn computer_target(input: &serde_json::Value) -> Result<computer_use::Target, String> {
    let Some(target) = input.get("target") else { return Ok(computer_use::Target::Screen); };
    require_object(target, "target")?;
    match required_string(target, "type")?.as_str() {
        "screen" => Ok(computer_use::Target::Screen),
        "window" => {
            let window_id = optional_bounded_u64(target, "window_id", u64::from(u32::MAX))?
                .filter(|id| *id > 0).ok_or("window target requires a non-zero window_id")? as u32;
            let pid = computer_i32(target, "pid")?;
            if pid <= 0 { return Err("window target requires a positive pid".to_string()); }
            if !crate::features::FeatureFlag::BackgroundComputerUse.is_enabled()
                || !computer_use::background_supported()
            {
                return Err("Background window targeting is unavailable; use the screen target.".to_string());
            }
            Ok(computer_use::Target::Window { window_id, pid })
        }
        _ => Err("target.type must be screen or window".to_string()),
    }
}

fn computer_screenshot_params(input: &serde_json::Value) -> Result<computer_use::ScreenshotParams, String> {
    let empty = serde_json::json!({});
    let params = input.get("screenshot_params").unwrap_or(&empty);
    require_object(params, "screenshot_params")?;
    let positive = |key, maximum, default| -> Result<usize, String> {
        let value = optional_bounded_u64(params, key, maximum)?.unwrap_or(default);
        if value == 0 { return Err(format!("{key} must be positive")); }
        Ok(value as usize)
    };
    Ok(computer_use::ScreenshotParams {
        max_long_edge_px: Some(positive("max_long_edge_px", 4096, 1568)?),
        max_total_px: Some(positive("max_total_px", 4_194_304, 1_150_000)?),
        region: None,
        target: computer_target(params)?,
    })
}

fn computer_actions(input: &serde_json::Value) -> Result<Vec<computer_use::TargetedAction>, String> {
    use computer_use::{Action, Key, MouseButton, ScrollDirection, ScrollDistance, TargetedAction, Vector2I};
    let mut wait_seconds = 0.0;
    limited_required_array(input, "actions", 20)?.iter().map(|input| {
        require_object(input, "action")?;
        let position = || -> Result<Vector2I, String> {
            Ok(Vector2I::new(computer_i32(input, "x")?, computer_i32(input, "y")?))
        };
        let button = || -> Result<MouseButton, String> {
            match required_string(input, "button")?.as_str() {
                "left" => Ok(MouseButton::Left), "right" => Ok(MouseButton::Right),
                "middle" => Ok(MouseButton::Middle), "back" => Ok(MouseButton::Back),
                "forward" => Ok(MouseButton::Forward),
                _ => Err("button must be left, right, middle, back, or forward".to_string()),
            }
        };
        let key = || -> Result<Key, String> {
            match (input.get("key"), input.get("keycode")) {
                (Some(_), None) => {
                    let text = required_string(input, "key")?;
                    let mut chars = text.chars();
                    let key = chars.next().ok_or("key must contain exactly one character")?;
                    if chars.next().is_some() { return Err("key must contain exactly one character".to_string()); }
                    Ok(Key::Char(key))
                }
                (None, Some(_)) => Ok(Key::Keycode(computer_i32(input, "keycode")?)),
                _ => Err("provide exactly one of key or keycode".to_string()),
            }
        };
        let action = match required_string(input, "type")?.as_str() {
            "mouse_move" => Action::MouseMove { to: position()? },
            "mouse_down" => Action::MouseDown { button: button()?, at: position()? },
            "mouse_up" => Action::MouseUp { button: button()? },
            "mouse_wheel" => Action::MouseWheel {
                at: position()?,
                direction: match required_string(input, "direction")?.as_str() {
                    "up" => ScrollDirection::Up, "down" => ScrollDirection::Down,
                    "left" => ScrollDirection::Left, "right" => ScrollDirection::Right,
                    _ => return Err("direction must be up, down, left, or right".to_string()),
                },
                distance: ScrollDistance::Pixels(optional_nonnegative_i32(input, "pixels")?.ok_or("mouse_wheel requires pixels")?),
            },
            "type_text" => {
                let text = required_string(input, "text")?;
                if text.len() > 100_000 { return Err("text must be no longer than 100000 bytes".to_string()); }
                Action::TypeText { text }
            }
            "key_down" => Action::KeyDown { key: key()? },
            "key_up" => Action::KeyUp { key: key()? },
            "wait" => {
                let seconds = input.get("seconds").and_then(serde_json::Value::as_f64)
                    .filter(|seconds| seconds.is_finite() && *seconds >= 0.0 && *seconds <= 10.0)
                    .ok_or("seconds must be between 0 and 10")?;
                wait_seconds += seconds;
                if wait_seconds > 10.0 { return Err("a computer-use batch may wait at most 10 seconds".to_string()); }
                Action::Wait(Duration::from_secs_f64(seconds))
            }
            _ => return Err("unknown computer action type".to_string()),
        };
        Ok(TargetedAction { action, target: computer_target(input)? })
    }).collect()
}

fn skill_references(
    input: &serde_json::Value,
    skill_path_origin: &SkillPathOrigin,
) -> Result<Vec<SkillReference>, String> {
    let Some(skills) = optional_array(input, "skills")? else {
        return Ok(Vec::new());
    };
    skills
        .iter()
        .enumerate()
        .map(|(index, skill)| {
            require_object(skill, &format!("skills[{index}]"))?;
            let reference = required_string(skill, "skill")?;
            match required_string(skill, "reference_type")?.as_str() {
                "bundled" => Ok(SkillReference::BundledSkillId(reference)),
                "path" => skill_path_origin
                    .location_for_path(reference)
                    .map(SkillReference::Path)
                    .map_err(|error| error.to_string()),
                reference_type => Err(format!(
                    "invalid skills[{index}].reference_type: expected \"path\" or \"bundled\", got {reference_type:?}"
                )),
            }
        })
        .collect()
}

fn run_agents_execution_mode(input: &serde_json::Value) -> Result<RunAgentsExecutionMode, String> {
    let Some(execution_mode) = input.get("execution_mode") else {
        return Ok(RunAgentsExecutionMode::Local);
    };
    require_object(execution_mode, "execution_mode")?;
    match optional_string(execution_mode, "type")?.as_deref() {
        Some("remote") => Ok(RunAgentsExecutionMode::Remote {
            environment_id: optional_string(execution_mode, "environment_id")?.unwrap_or_default(),
            worker_host: optional_string(execution_mode, "worker_host")?.unwrap_or_default(),
            computer_use_enabled: optional_boolean(execution_mode, "computer_use_enabled")?
                .unwrap_or(false),
        }),
        Some("local") | None => {
            optional_string(execution_mode, "environment_id")?;
            optional_string(execution_mode, "worker_host")?;
            optional_boolean(execution_mode, "computer_use_enabled")?;
            Ok(RunAgentsExecutionMode::Local)
        }
        Some(mode_type) => Err(format!(
            "invalid execution_mode.type: expected \"local\" or \"remote\", got {mode_type:?}"
        )),
    }
}

fn file_location(file: &serde_json::Value, file_index: usize) -> Result<FileLocations, String> {
    if let Some(name) = file.as_str() {
        return Ok(FileLocations {
            name: name.to_string(),
            lines: Vec::new(),
        });
    }
    require_object(file, &format!("files[{file_index}]"))?;
    let name = required_string(file, "path")?;
    let lines = match file.get("line_ranges") {
        None => Vec::new(),
        Some(value) => value
            .as_array()
            .ok_or_else(|| format!("invalid files[{file_index}].line_ranges: expected an array"))?
            .iter()
            .enumerate()
            .map(|(range_index, range)| {
                require_object(
                    range,
                    &format!("files[{file_index}].line_ranges[{range_index}]"),
                )?;
                let start = required_line_number(range, "start", file_index, range_index)?;
                let inclusive_end = required_line_number(range, "end", file_index, range_index)?;
                if inclusive_end < start {
                    return Err(format!(
                        "invalid files[{file_index}].line_ranges[{range_index}]: end must be greater than or equal to start"
                    ));
                }
                let exclusive_end = inclusive_end.checked_add(1).ok_or_else(|| {
                    format!(
                        "invalid files[{file_index}].line_ranges[{range_index}].end: inclusive end is too large"
                    )
                })?;
                Ok(start..exclusive_end)
            })
            .collect::<Result<_, String>>()?,
    };
    Ok(FileLocations { name, lines })
}

fn required_line_number(
    range: &serde_json::Value,
    key: &str,
    file_index: usize,
    range_index: usize,
) -> Result<usize, String> {
    let value = range
        .get(key)
        .ok_or_else(|| format!("missing required field {key:?}"))?
        .as_u64()
        .and_then(|value| usize::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| {
            format!(
                "invalid files[{file_index}].line_ranges[{range_index}].{key}: expected a positive integer"
            )
        })?;
    Ok(value)
}

fn file_edits(input: &serde_json::Value) -> Result<Vec<FileEdit>, String> {
    let mut edits = Vec::new();
    if let Some(diffs) = optional_array(input, "diffs")? {
        for (index, diff) in diffs.iter().enumerate() {
            require_object(diff, &format!("diffs[{index}]"))?;
            edits.push(FileEdit::Edit(ParsedDiff::StrReplaceEdit {
                file: Some(required_string(diff, "file_path")?),
                search: Some(required_string(diff, "search")?),
                replace: Some(required_string(diff, "replace")?),
            }));
        }
    }
    if let Some(files) = optional_array(input, "new_files")? {
        for (index, file) in files.iter().enumerate() {
            require_object(file, &format!("new_files[{index}]"))?;
            edits.push(FileEdit::Create {
                file: Some(required_string(file, "file_path")?),
                content: Some(required_string(file, "content")?),
                creation_scope: None,
            });
        }
    }
    if let Some(files) = optional_array(input, "deleted_files")? {
        for (index, file) in files.iter().enumerate() {
            let path = file
                .as_str()
                .ok_or_else(|| format!("invalid deleted_files[{index}]: expected a string"))?;
            edits.push(FileEdit::Delete {
                file: Some(path.to_owned()),
            });
        }
    }
    if edits.is_empty() {
        return Err(
            "invalid file edits: expected at least one diff, new file, or deleted file".to_string(),
        );
    }
    let distinct_files = edits
        .iter()
        .filter_map(FileEdit::file)
        .collect::<HashSet<_>>();
    if distinct_files.len() > 1 {
        return Err(format!(
            "apply_file_diffs accepts one file per call; received {} distinct files. Apply each file in a separate call.",
            distinct_files.len()
        ));
    }
    Ok(edits)
}

fn optional_array<'a>(
    input: &'a serde_json::Value,
    key: &str,
) -> Result<Option<&'a Vec<serde_json::Value>>, String> {
    input
        .get(key)
        .map(|value| {
            value
                .as_array()
                .ok_or_else(|| format!("invalid field {key:?}: expected an array"))
        })
        .transpose()
}

#[cfg(test)]
#[path = "rig_tool_tests.rs"]
mod tests;
