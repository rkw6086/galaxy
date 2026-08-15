use std::collections::HashMap;
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
    RunAgentsRequest, SearchCodebaseRequest, ShellCommandDelay, StartAgentExecutionMode,
    StartAgentVersion,
};
use crate::ai::document::ai_document_model::AIDocumentId;

pub(crate) fn action_from_tool_call(
    task_id: &str,
    call: &ToolCall,
    skill_path_origin: &SkillPathOrigin,
    mcp_tool_aliases: &HashMap<String, MCPToolTarget>,
) -> Result<AIAgentAction, String> {
    let input = &call.arguments;
    let action = if let Some(target) = mcp_tool_aliases.get(&call.name) {
        AIAgentActionType::CallMCPTool {
            server_id: target.server_id,
            name: target.name.clone(),
            input: input.clone(),
        }
    } else {
        match call.name.as_str() {
        "run_shell_command" => AIAgentActionType::RequestCommandOutput {
            command: string(input, "command"),
            is_read_only: Some(boolean(input, "is_read_only")),
            is_risky: Some(boolean(input, "is_risky")),
            wait_until_completion: boolean(input, "wait_until_complete"),
            uses_pager: Some(boolean(input, "uses_pager")),
            rationale: None,
            citations: Vec::new(),
        },
        "read_files" => AIAgentActionType::ReadFiles(ReadFilesRequest {
            locations: input
                .get("files")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(file_location)
                .collect(),
        }),
        "apply_file_diffs" => AIAgentActionType::RequestFileEdits {
            file_edits: file_edits(input),
            title: nonempty_string(input, "summary"),
        },
        "grep" => AIAgentActionType::Grep {
            queries: strings(input, "queries"),
            path: string(input, "path"),
        },
        "file_glob" => AIAgentActionType::FileGlob {
            patterns: strings(input, "patterns"),
            path: nonempty_string(input, "path"),
        },
        "search_codebase" => AIAgentActionType::SearchCodebase(SearchCodebaseRequest {
            query: string(input, "query"),
            partial_paths: nonempty_strings(input, "path_filters"),
            codebase_path: nonempty_string(input, "path"),
        }),
        "write_to_long_running_shell_command" => {
            AIAgentActionType::WriteToLongRunningShellCommand {
                block_id: string(input, "command_id").into(),
                input: string(input, "input").into_bytes().into(),
                mode: match input.get("mode").and_then(serde_json::Value::as_str) {
                    Some("line") => AIAgentPtyWriteMode::Line,
                    Some("block") => AIAgentPtyWriteMode::Block,
                    Some("raw") | Some(_) | None => AIAgentPtyWriteMode::Raw,
                },
            }
        }
        "interrupt_shell_command" => AIAgentActionType::WriteToLongRunningShellCommand {
            block_id: string(input, "command_id").into(),
            input: vec![galaxy_terminal::model::escape_sequences::C0::ETX].into(),
            mode: AIAgentPtyWriteMode::Raw,
        },
        "read_shell_command_output" => AIAgentActionType::ReadShellCommandOutput {
            block_id: string(input, "command_id").into(),
            delay: Some(ShellCommandDelay::Duration(Duration::from_secs(
                input
                    .get("wait_seconds")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(2)
                    .min(crate::ai::bedrock::request_translator::COMMAND_MONITOR_MAX_POLL_SECONDS),
            ))),
        },
        "read_mcp_resource" => AIAgentActionType::ReadMCPResource {
            server_id: uuid(input, "server_id"),
            name: String::new(),
            uri: nonempty_string(input, "uri"),
        },
        "read_plan" | "read_documents" | "read_notebook" => {
            AIAgentActionType::ReadDocuments(ReadDocumentsRequest {
                document_ids: strings(input, "document_ids")
                    .into_iter()
                    .filter_map(|id| AIDocumentId::try_from(id).ok())
                    .collect(),
            })
        }
        "create_plan" | "create_documents" | "create_notebook" => {
            AIAgentActionType::CreateDocuments(CreateDocumentsRequest {
                documents: input
                    .get("documents")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|document| {
                        Some(DocumentToCreate {
                            title: document.get("title")?.as_str()?.to_string(),
                            content: document.get("content")?.as_str()?.to_string(),
                        })
                    })
                    .collect(),
            })
        }
        "edit_plan" | "edit_documents" | "edit_notebook" => {
            AIAgentActionType::EditDocuments(EditDocumentsRequest {
                diffs: input
                    .get("diffs")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|diff| {
                        Some(DocumentDiff {
                            document_id: AIDocumentId::try_from(diff.get("document_id")?.as_str()?)
                                .ok()?,
                            search: string(diff, "search"),
                            replace: string(diff, "replace"),
                        })
                    })
                    .collect(),
            })
        }
        "run_agents" => AIAgentActionType::RunAgents(RunAgentsRequest {
            summary: string(input, "summary"),
            base_prompt: string(input, "base_prompt"),
            skills: skill_references(input, skill_path_origin),
            model_id: string(input, "model_id"),
            harness_type: string(input, "harness_type"),
            execution_mode: run_agents_execution_mode(input),
            agent_run_configs: input
                .get("agent_run_configs")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .map(|config| RunAgentsAgentRunConfig {
                    name: string(config, "name"),
                    prompt: string(config, "prompt"),
                    title: string(config, "title"),
                })
                .collect(),
            plan_id: string(input, "plan_id"),
            harness_auth_secret_name: None,
        }),
        "start_agent" => AIAgentActionType::StartAgent {
            version: StartAgentVersion::V1,
            name: string(input, "name"),
            prompt: string(input, "prompt"),
            execution_mode: StartAgentExecutionMode::local_with_defaults(),
            lifecycle_subscription: None,
        },
        "send_message_to_agent" => AIAgentActionType::SendMessageToAgent {
            addresses: vec![string(input, "agent_id")],
            subject: String::new(),
            message: string(input, "message"),
        },
        "ask_user_question" => AIAgentActionType::AskUserQuestion {
            questions: vec![AskUserQuestionItem {
                question_id: Uuid::new_v4().to_string(),
                question: string(input, "question"),
                question_type: AskUserQuestionType::MultipleChoice {
                    is_multiselect: false,
                    options: strings(input, "options")
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
            let skill = string(input, "skill");
            let skill = match input
                .get("reference_type")
                .and_then(serde_json::Value::as_str)
            {
                Some("bundled") => SkillReference::BundledSkillId(skill),
                Some("path") | Some(_) | None => SkillReference::Path(
                    skill_path_origin
                        .location_for_path(skill)
                        .map_err(|error| error.to_string())?,
                ),
            };
            AIAgentActionType::ReadSkill(ReadSkillRequest { skill })
        }
        "fetch_conversation" => AIAgentActionType::FetchConversation {
            conversation_id: string(input, "conversation_id"),
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

fn string(input: &serde_json::Value, key: &str) -> String {
    input
        .get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn nonempty_string(input: &serde_json::Value, key: &str) -> Option<String> {
    let value = string(input, key);
    (!value.is_empty()).then_some(value)
}

fn boolean(input: &serde_json::Value, key: &str) -> bool {
    input
        .get(key)
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

fn strings(input: &serde_json::Value, key: &str) -> Vec<String> {
    input
        .get(key)
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(ToOwned::to_owned)
        .collect()
}

fn nonempty_strings(input: &serde_json::Value, key: &str) -> Option<Vec<String>> {
    let values = strings(input, key);
    (!values.is_empty()).then_some(values)
}

fn uuid(input: &serde_json::Value, key: &str) -> Option<Uuid> {
    input
        .get(key)
        .and_then(serde_json::Value::as_str)
        .and_then(|value| Uuid::parse_str(value).ok())
}

fn skill_references(
    input: &serde_json::Value,
    skill_path_origin: &SkillPathOrigin,
) -> Vec<SkillReference> {
    input
        .get("skills")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|skill| {
            let reference = string(skill, "skill");
            if reference.is_empty() {
                return None;
            }
            match skill
                .get("reference_type")
                .and_then(serde_json::Value::as_str)
            {
                Some("bundled") => Some(SkillReference::BundledSkillId(reference)),
                Some("path") | Some(_) | None => skill_path_origin
                    .location_for_path(reference)
                    .ok()
                    .map(SkillReference::Path),
            }
        })
        .collect()
}

fn run_agents_execution_mode(input: &serde_json::Value) -> RunAgentsExecutionMode {
    let Some(execution_mode) = input.get("execution_mode") else {
        return RunAgentsExecutionMode::Local;
    };
    let mode_type = execution_mode
        .get("type")
        .and_then(serde_json::Value::as_str)
        .or_else(|| execution_mode.as_str());
    match mode_type {
        Some("remote") => RunAgentsExecutionMode::Remote {
            environment_id: string(execution_mode, "environment_id"),
            worker_host: string(execution_mode, "worker_host"),
            computer_use_enabled: boolean(execution_mode, "computer_use_enabled"),
        },
        Some("local") | Some(_) | None => RunAgentsExecutionMode::Local,
    }
}

fn file_location(file: &serde_json::Value) -> Option<FileLocations> {
    if let Some(name) = file.as_str() {
        return Some(FileLocations {
            name: name.to_string(),
            lines: Vec::new(),
        });
    }
    let name = file
        .get("path")
        .or_else(|| file.get("name"))?
        .as_str()?
        .to_string();
    let lines = file
        .get("line_ranges")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|range| {
            let start = usize::try_from(range.get("start")?.as_u64()?).ok()?;
            let end = usize::try_from(range.get("end")?.as_u64()?).ok()?;
            (start > 0 && end >= start).then_some(start..end)
        })
        .collect();
    Some(FileLocations { name, lines })
}

fn file_edits(input: &serde_json::Value) -> Vec<FileEdit> {
    let diffs = input
        .get("diffs")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .map(|diff| {
            FileEdit::Edit(ParsedDiff::StrReplaceEdit {
                file: nonempty_string(diff, "file_path"),
                search: nonempty_string(diff, "search"),
                replace: nonempty_string(diff, "replace"),
            })
        });
    let creates = input
        .get("new_files")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .map(|file| FileEdit::Create {
            file: nonempty_string(file, "file_path"),
            content: nonempty_string(file, "content"),
        });
    let deletes = input
        .get("deleted_files")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .map(|file| FileEdit::Delete {
            file: file
                .as_str()
                .map(ToOwned::to_owned)
                .or_else(|| nonempty_string(file, "file_path")),
        });
    diffs.chain(creates).chain(deletes).collect()
}

#[cfg(test)]
#[path = "rig_tool_tests.rs"]
mod tests;
