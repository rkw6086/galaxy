use galaxy_core::features::FeatureFlag;
use warp_multi_agent_api as api;

use super::RequestParams;
use crate::ai::agent::redaction;
use crate::terminal::model::session::SessionType;

fn remove_orchestration_tools_if_disabled(
    supported_tools: &mut Vec<api::ToolType>,
    orchestration_enabled: bool,
) {
    if orchestration_enabled {
        return;
    }
    supported_tools.retain(|tool| {
        !matches!(
            tool,
            api::ToolType::Subagent | api::ToolType::RunAgents | api::ToolType::StartAgentV2
        )
    });
}

pub(crate) fn prepare_direct_provider_params(
    params: &mut RequestParams,
) -> (Vec<api::ToolType>, Vec<api::ToolType>) {
    let supported_tools_override = params.supported_tools_override.take();
    let mut supported_tools = supported_tools_override
        .clone()
        .unwrap_or_else(|| get_supported_tools(params));
    remove_orchestration_tools_if_disabled(&mut supported_tools, params.orchestration_enabled);
    let mut supported_cli_agent_tools =
        supported_tools_override.unwrap_or_else(|| get_supported_cli_agent_tools(params));
    remove_orchestration_tools_if_disabled(
        &mut supported_cli_agent_tools,
        params.orchestration_enabled,
    );
    if params.permission_mode == crate::ai::execution_profiles::PermissionMode::ReadOnly {
        supported_tools.retain(is_read_only_tool);
        supported_cli_agent_tools.retain(is_read_only_tool);
    }
    if params.should_redact_secrets {
        redaction::redact_inputs(&mut params.input);
    }
    (supported_tools, supported_cli_agent_tools)
}

fn is_read_only_tool(tool: &api::ToolType) -> bool {
    matches!(
        tool,
        api::ToolType::ReadFiles
            | api::ToolType::SearchCodebase
            | api::ToolType::Grep
            | api::ToolType::FileGlob
            | api::ToolType::FileGlobV2
            | api::ToolType::ReadSkill
            | api::ToolType::ReadDocuments
    )
}

fn get_supported_tools(params: &RequestParams) -> Vec<api::ToolType> {
    let mut supported_tools = vec![
        api::ToolType::Grep,
        api::ToolType::FileGlob,
        api::ToolType::FileGlobV2,
        api::ToolType::ReadMcpResource,
        api::ToolType::CallMcpTool,
        api::ToolType::RunShellCommand,
        api::ToolType::WriteToLongRunningShellCommand,
        api::ToolType::ReadShellCommandOutput,
        api::ToolType::ReadDocuments,
        api::ToolType::CreateDocuments,
        api::ToolType::EditDocuments,
    ];

    if params.orchestration_enabled {
        supported_tools.push(api::ToolType::Subagent);
    }

    if FeatureFlag::ConversationsAsContext.is_enabled() {
        supported_tools.push(api::ToolType::FetchConversation);
    }

    match params.session_context.session_type() {
        None | Some(SessionType::Local) => {
            supported_tools.extend(&[
                api::ToolType::ReadFiles,
                api::ToolType::ApplyFileDiffs,
                api::ToolType::SearchCodebase,
            ]);
        }
        Some(SessionType::WormholedRemote { host_id: Some(_) }) => {
            // Remote session with a known host — enable tools that route
            // through RemoteServerClient. The host_id is only populated
            // after a successful connection handshake, so its presence is a
            // sufficient proxy for client availability.
            supported_tools.extend(&[api::ToolType::ReadFiles, api::ToolType::ApplyFileDiffs]);
            if FeatureFlag::RemoteCodebaseIndexing.is_enabled() {
                supported_tools.push(api::ToolType::SearchCodebase);
            }
        }
        Some(SessionType::WormholedRemote { host_id: None }) => {}
    }

    if FeatureFlag::ListSkills.is_enabled() {
        supported_tools.push(api::ToolType::ReadSkill);
    }

    if FeatureFlag::AskUserQuestion.is_enabled() && params.ask_user_question_enabled {
        supported_tools.push(api::ToolType::AskUserQuestion);
    }

    supported_tools
}

fn get_supported_cli_agent_tools(params: &RequestParams) -> Vec<api::ToolType> {
    let mut supported_cli_agent_tools = vec![
        api::ToolType::WriteToLongRunningShellCommand,
        api::ToolType::ReadShellCommandOutput,
        api::ToolType::Grep,
        api::ToolType::FileGlob,
        api::ToolType::FileGlobV2,
    ];

    if FeatureFlag::TransferControlTool.is_enabled() {
        supported_cli_agent_tools.push(api::ToolType::TransferShellCommandControlToUser);
    }

    match params.session_context.session_type() {
        None | Some(SessionType::Local) => {
            supported_cli_agent_tools
                .extend(&[api::ToolType::ReadFiles, api::ToolType::SearchCodebase]);
        }
        Some(SessionType::WormholedRemote { host_id: Some(_) }) => {
            supported_cli_agent_tools.push(api::ToolType::ReadFiles);
            if FeatureFlag::RemoteCodebaseIndexing.is_enabled() {
                supported_cli_agent_tools.push(api::ToolType::SearchCodebase);
            }
        }
        Some(SessionType::WormholedRemote { host_id: None }) => {}
    }

    supported_cli_agent_tools
}

#[cfg(test)]
#[path = "impl_tests.rs"]
mod tests;
