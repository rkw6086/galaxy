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
    let ask_user_question_enabled = FeatureFlag::AskUserQuestion.is_enabled()
        && params.ask_user_question_enabled
        && matches!(
            params.permission_mode,
            crate::ai::execution_profiles::PermissionMode::AskFirst
                | crate::ai::execution_profiles::PermissionMode::AllowAll
        );
    supported_tools
        .retain(|tool| *tool != api::ToolType::AskUserQuestion || ask_user_question_enabled);
    // Computer use controls this machine, even when the terminal is connected elsewhere.
    // Only expose the initial request here; the dedicated profile owns subsequent actions.
    supported_tools.retain(|tool| {
        *tool != api::ToolType::UseComputer
            && (*tool != api::ToolType::RequestComputerUse || params.computer_use_enabled)
    });
    let mut supported_cli_agent_tools =
        supported_tools_override.unwrap_or_else(|| get_supported_cli_agent_tools(params));
    remove_orchestration_tools_if_disabled(
        &mut supported_cli_agent_tools,
        params.orchestration_enabled,
    );
    supported_cli_agent_tools.retain(|tool| {
        !matches!(
            tool,
            api::ToolType::UseComputer | api::ToolType::RequestComputerUse
        )
    });
    supported_cli_agent_tools.retain(|tool| *tool != api::ToolType::AskUserQuestion);
    // Explicit overrides cannot restore retired terminal monitoring or PTY access.
    for tools in [&mut supported_tools, &mut supported_cli_agent_tools] {
        tools.retain(|tool| {
            !matches!(
                tool,
                api::ToolType::WriteToLongRunningShellCommand
                    | api::ToolType::ReadShellCommandOutput
                    | api::ToolType::TransferShellCommandControlToUser
            )
        });
    }
    if params.permission_mode == crate::ai::execution_profiles::PermissionMode::ReadOnly {
        let permitted = |tool: &api::ToolType| {
            is_read_only_tool(tool)
                || (!params.command_allowlist.is_empty()
                    && matches!(
                        tool,
                        api::ToolType::RunShellCommand
                            | api::ToolType::ReadShellCommandOutput
                            | api::ToolType::WriteToLongRunningShellCommand
                    ))
        };
        supported_tools.retain(permitted);
        supported_cli_agent_tools.retain(permitted);
    }
    if params.permission_mode == crate::ai::execution_profiles::PermissionMode::GalaxyDecides {
        // Do not spend model context or turns proposing operations that local policy always
        // denies. Exact command grants and the narrow inspection grammar still gate commands.
        let local_files = matches!(
            params.session_context.session_type(),
            None | Some(SessionType::Local)
        );
        let permitted = |tool: &api::ToolType| {
            is_read_only_tool(tool)
                || *tool == api::ToolType::RunShellCommand
                || (local_files && *tool == api::ToolType::ApplyFileDiffs)
        };
        supported_tools.retain(permitted);
        supported_cli_agent_tools.retain(permitted);
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
        api::ToolType::ReadDocuments,
        api::ToolType::CreateDocuments,
        api::ToolType::EditDocuments,
    ];
    if params.computer_use_enabled {
        supported_tools.push(api::ToolType::RequestComputerUse);
    }

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
        api::ToolType::Grep,
        api::ToolType::FileGlob,
        api::ToolType::FileGlobV2,
    ];

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
