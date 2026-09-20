//! Durable display history for direct providers, independent of their compacted model transcript.
use galaxy_agent_core::ToolCall;
use serde::{Deserialize, Serialize};
use warp_multi_agent_api as api;

use super::{AIAgentAction, AIAgentActionResult};
use crate::ai::runtime::ProviderActionContext;

const TOOL_PREFIX: &str = "galaxy:provider-tool:v1:";

#[derive(Serialize, Deserialize)]
struct SavedTool {
    context: ProviderActionContext,
    call: ToolCall,
    #[serde(default)]
    delegated_base_prompt: Option<String>,
    #[serde(default)]
    question_ids: Vec<String>,
}

pub(crate) fn tool_message(
    context: &ProviderActionContext,
    call: &ToolCall,
    action: &AIAgentAction,
) -> anyhow::Result<api::Message> {
    // Keep the native call as well as the compatibility representation. The native
    // form retains MCP aliases and distinctions such as notebook vs. plan tools.
    let compatible = context.history_compatibility_call(call);
    let event = crate::ai::provider::response_translator::build_tool_call_message(
        context.task_id(),
        &call.id,
        &compatible.name,
        &compatible.arguments.to_string(),
    );
    let mut message = match event.r#type {
        Some(api::response_event::Type::ClientActions(actions)) => actions
            .actions
            .into_iter()
            .find_map(|action| match action.action {
                Some(api::client_action::Action::AddMessagesToTask(messages)) => {
                    messages.messages.into_iter().next()
                }
                _ => None,
            })
            .ok_or_else(|| anyhow::anyhow!("missing tool history message"))?,
        _ => anyhow::bail!("missing tool history event"),
    };
    // Native action IDs must remain identical across execution and restoration.
    message.id = call.id.clone();
    if let Some(api::message::Message::ToolCall(tool)) = &mut message.message {
        tool.tool_call_id = call.id.clone();
    } else {
        message.message = Some(api::message::Message::ToolCall(api::message::ToolCall {
            tool_call_id: call.id.clone(),
            tool: None,
        }));
    }
    message.server_message_data = format!(
        "{TOOL_PREFIX}{}",
        serde_json::to_string(&SavedTool {
            context: context.clone(),
            call: call.clone(),
            question_ids: match &action.action {
                super::AIAgentActionType::AskUserQuestion { questions } => questions
                    .iter()
                    .map(|question| question.question_id.clone())
                    .collect(),
                _ => Vec::new(),
            },
            delegated_base_prompt: match &action.action {
                super::AIAgentActionType::RunAgents(request) => Some(request.base_prompt.clone()),
                _ => None,
            },
        })?
    );
    Ok(message)
}

pub(crate) fn restore_tool(data: &str) -> Option<Result<AIAgentAction, String>> {
    let json = data.strip_prefix(TOOL_PREFIX)?;
    Some(
        serde_json::from_str::<SavedTool>(json)
            .map_err(|error| error.to_string())
            .and_then(|saved| {
                let mut action = saved.context.restored_action_from_tool_call(&saved.call)?;
                if let (super::AIAgentActionType::RunAgents(request), Some(base_prompt)) =
                    (&mut action.action, saved.delegated_base_prompt)
                {
                    request.base_prompt = base_prompt;
                }
                if let super::AIAgentActionType::AskUserQuestion { questions } = &mut action.action
                {
                    for (question, id) in questions.iter_mut().zip(saved.question_ids) {
                        question.question_id = id;
                    }
                }
                Ok(action)
            }),
    )
}

const RESULT_PREFIX: &str = "galaxy:provider-result:v1:";

#[derive(Default, Serialize, Deserialize)]
struct SavedResultDetails {
    cancelled: bool,
    file_edit: Option<(String, usize, usize)>,
    command_error: Option<(String, String)>,
}

/// Supplement fields that the server's request/result protocol does not round-trip.
/// This is display metadata and is never used to execute an action.
pub(crate) fn restore_result_details(
    input: &mut super::AIAgentInput,
    data: &str,
    proposal_data: Option<&str>,
) {
    let Some(json) = data.strip_prefix(RESULT_PREFIX) else {
        return;
    };
    let Ok(details) = serde_json::from_str::<SavedResultDetails>(json) else {
        return;
    };
    let super::AIAgentInput::ActionResult { result, .. } = input else {
        return;
    };
    if details.cancelled {
        if let Some(Ok(action)) = proposal_data.and_then(restore_tool) {
            result.result = action.action.cancelled_result();
        }
    }
    if let (
        super::AIAgentActionResultType::RequestFileEdits(super::RequestFileEditsResult::Success {
            diff,
            lines_added,
            lines_removed,
            ..
        }),
        Some((saved_diff, added, removed)),
    ) = (&mut result.result, details.file_edit)
    {
        *diff = saved_diff;
        *lines_added = added;
        *lines_removed = removed;
    }
    if let Some((command, message)) = details.command_error {
        result.result = super::AIAgentActionResultType::RequestCommandOutput(
            super::RequestCommandOutputResult::ExecutionError { command, message },
        );
    }
}

// FileGlob remains part of saved history even though the server deprecated it.
#[allow(deprecated)]
pub(crate) fn result_message(result: &AIAgentActionResult) -> anyhow::Result<api::Message> {
    use api::message::tool_call_result::Result as Output;
    use api::request::input::tool_call_result::Result as Input;
    let details = SavedResultDetails {
        cancelled: result.result.is_cancelled(),
        file_edit: match &result.result {
            super::AIAgentActionResultType::RequestFileEdits(
                super::RequestFileEditsResult::Success {
                    diff,
                    lines_added,
                    lines_removed,
                    ..
                },
            ) => Some((diff.clone(), *lines_added, *lines_removed)),
            _ => None,
        },
        command_error: match &result.result {
            super::AIAgentActionResultType::RequestCommandOutput(
                super::RequestCommandOutputResult::ExecutionError { command, message },
            ) => Some((command.clone(), message.clone())),
            _ => None,
        },
    };
    let input: api::request::input::user_inputs::user_input::Input = result.clone().try_into()?;
    let api::request::input::user_inputs::user_input::Input::ToolCallResult(input) = input else {
        anyhow::bail!("expected tool result");
    };
    // These protobuf oneofs use different field numbers; map variants explicitly.
    let payload = input.result.map(|result| match result {
        Input::RunShellCommand(value) => Output::RunShellCommand(value),
        Input::ReadFiles(value) => Output::ReadFiles(value),
        Input::SearchCodebase(value) => Output::SearchCodebase(value),
        Input::ApplyFileDiffs(value) => Output::ApplyFileDiffs(value),
        Input::SuggestPlan(value) => Output::SuggestPlan(value),
        Input::SuggestCreatePlan(value) => Output::SuggestCreatePlan(value),
        Input::Grep(value) => Output::Grep(value),
        Input::FileGlob(value) => Output::FileGlob(value),
        Input::ReadMcpResource(value) => Output::ReadMcpResource(value),
        Input::CallMcpTool(value) => Output::CallMcpTool(value),
        Input::WriteToLongRunningShellCommand(value) => {
            Output::WriteToLongRunningShellCommand(value)
        }
        Input::SuggestNewConversation(value) => Output::SuggestNewConversation(value),
        Input::FileGlobV2(value) => Output::FileGlobV2(value),
        Input::SuggestPrompt(value) => Output::SuggestPrompt(value),
        Input::OpenCodeReview(value) => Output::OpenCodeReview(value),
        Input::InitProject(value) => Output::InitProject(value),
        Input::ReadDocuments(value) => Output::ReadDocuments(value),
        Input::EditDocuments(value) => Output::EditDocuments(value),
        Input::CreateDocuments(value) => Output::CreateDocuments(value),
        Input::ReadShellCommandOutput(value) => Output::ReadShellCommandOutput(value),
        Input::UseComputer(value) => Output::UseComputer(value),
        Input::InsertReviewComments(value) => Output::InsertReviewComments(value),
        Input::RequestComputerUse(value) => Output::RequestComputerUseResult(value),
        Input::ReadSkill(value) => Output::ReadSkill(value),
        Input::FetchConversation(value) => Output::FetchConversation(value),
        Input::StartAgent(value) => Output::StartAgent(value),
        Input::SendMessageToAgent(value) => Output::SendMessageToAgent(value),
        Input::TransferShellCommandControlToUser(value) => {
            Output::TransferShellCommandControlToUser(value)
        }
        Input::AskUserQuestion(value) => Output::AskUserQuestion(value),
        Input::StartAgentV2(value) => Output::StartAgentV2(value),
        Input::UploadFileArtifact(value) => Output::UploadFileArtifact(value),
        Input::RunAgentsResult(value) => Output::RunAgentsResult(value),
        Input::WaitForEvents(value) => Output::WaitForEvents(value),
    });
    Ok(api::Message {
        id: format!("galaxy-tool-result:{}", result.id),
        server_message_data: format!("{RESULT_PREFIX}{}", serde_json::to_string(&details)?),
        task_id: result.task_id.to_string(),
        message: Some(api::message::Message::ToolCallResult(
            api::message::ToolCallResult {
                tool_call_id: input.tool_call_id,
                context: None,
                result: payload,
            },
        )),
        ..Default::default()
    })
}

#[cfg(test)]
#[path = "provider_history_tests.rs"]
mod tests;
