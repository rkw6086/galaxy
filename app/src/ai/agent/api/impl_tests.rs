use galaxy_core::features::FeatureFlag;
use galaxy_core::HostId;
use warp_multi_agent_api as api;

use super::{
    get_supported_cli_agent_tools, get_supported_tools, remove_orchestration_tools_if_disabled,
};
use crate::ai::agent::api::RequestParams;
use crate::ai::blocklist::SessionContext;
use crate::ai::llms::LLMId;
use crate::terminal::model::session::SessionType;

fn request_params_with_ask_user_question_enabled(ask_user_question_enabled: bool) -> RequestParams {
    let model = LLMId::from("test-model");

    RequestParams {
        terminal_view_id: None,
        input: vec![],
        tool_results: vec![],
        conversation_token: None,
        forked_from_conversation_token: None,
        ambient_agent_task_id: None,
        tasks: vec![],
        existing_suggestions: None,
        metadata: None,
        session_context: SessionContext::new_for_test(),
        model: model.clone(),
        coding_model: model.clone(),
        cli_agent_model: model.clone(),
        computer_use_model: model,
        is_memory_enabled: false,
        warp_drive_context_enabled: false,
        context_window_limit: None,
        model_start_timeout_secs: None,
        mcp_context: None,
        planning_enabled: true,
        should_redact_secrets: false,
        api_keys: None,
        custom_model_providers: None,
        custom_model_routers: None,
        permission_mode: Default::default(),
        command_allowlist: Vec::new(),
        autonomy_level: api::AutonomyLevel::Supervised,
        isolation_level: api::IsolationLevel::None,
        web_search_enabled: false,
        computer_use_enabled: false,
        ask_user_question_enabled,
        research_agent_enabled: false,
        orchestration_enabled: false,
        supported_tools_override: None,
        root_task_id: None,
        parent_agent_id: None,
        agent_name: None,
        message_history: Vec::new(),
        progressive_summary: None,
        tool_result_archive: Vec::new(),
        messages_sent: std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        global_rules: Vec::new(),
    }
}

fn request_params_for_remote(host_id: Option<HostId>) -> RequestParams {
    let mut params = request_params_with_ask_user_question_enabled(false);
    params.session_context =
        SessionContext::new_with_session_type_for_test(Some(SessionType::WormholedRemote {
            host_id,
        }));
    params
}

#[test]
fn supported_tools_expose_local_subagents_without_hosted_orchestration_tools() {
    let mut params = request_params_with_ask_user_question_enabled(false);
    params.orchestration_enabled = true;

    let supported_tools = get_supported_tools(&params);

    assert!(supported_tools.contains(&api::ToolType::Subagent));
    assert!(!supported_tools.contains(&api::ToolType::RunAgents));
    assert!(!supported_tools.contains(&api::ToolType::SendMessageToAgent));
    assert!(!supported_tools.contains(&api::ToolType::WaitForEvents));
    assert!(!supported_tools.contains(&api::ToolType::StartAgentV2));
}

#[test]
fn supported_tools_include_plan_document_capabilities() {
    let params = request_params_with_ask_user_question_enabled(false);
    let supported_tools = get_supported_tools(&params);

    assert!(supported_tools.contains(&api::ToolType::ReadDocuments));
    assert!(supported_tools.contains(&api::ToolType::CreateDocuments));
    assert!(supported_tools.contains(&api::ToolType::EditDocuments));
}

#[test]
fn computer_use_is_explicitly_enabled_and_kept_out_of_command_monitor_tools() {
    let mut params = request_params_with_ask_user_question_enabled(false);
    params.computer_use_enabled = true;
    let (normal, cli) = super::prepare_direct_provider_params(&mut params);
    assert!(normal.contains(&api::ToolType::RequestComputerUse));
    assert!(!normal.contains(&api::ToolType::UseComputer));
    assert!(!cli.contains(&api::ToolType::RequestComputerUse));
    assert!(!cli.contains(&api::ToolType::UseComputer));

    params.computer_use_enabled = false;
    let (normal, _) = super::prepare_direct_provider_params(&mut params);
    assert!(!normal.contains(&api::ToolType::RequestComputerUse));
}

#[test]
fn computer_use_never_expands_explicit_tool_overrides_or_read_only_permissions() {
    let mut params = request_params_with_ask_user_question_enabled(false);
    params.computer_use_enabled = true;
    params.supported_tools_override = Some(Vec::new());
    let (normal, cli) = super::prepare_direct_provider_params(&mut params);
    assert!(normal.is_empty());
    assert!(cli.is_empty());

    params.permission_mode = crate::ai::execution_profiles::PermissionMode::ReadOnly;
    params.supported_tools_override = Some(vec![
        api::ToolType::RequestComputerUse,
        api::ToolType::UseComputer,
    ]);
    let (normal, cli) = super::prepare_direct_provider_params(&mut params);
    assert!(normal.is_empty());
    assert!(cli.is_empty());
}

#[test]
fn supported_tools_omit_subagents_when_orchestration_is_disabled() {
    let params = request_params_with_ask_user_question_enabled(false);
    let supported_tools = get_supported_tools(&params);

    assert!(!supported_tools.contains(&api::ToolType::Subagent));
}

#[test]
fn supported_tool_override_cannot_restore_leaf_orchestration_tools() {
    let mut supported_tools = vec![
        api::ToolType::Grep,
        api::ToolType::Subagent,
        api::ToolType::RunAgents,
        api::ToolType::StartAgentV2,
    ];

    remove_orchestration_tools_if_disabled(&mut supported_tools, false);

    assert_eq!(supported_tools, vec![api::ToolType::Grep]);
}

#[test]
fn enabled_orchestration_preserves_supported_tool_override() {
    let mut supported_tools = vec![api::ToolType::Grep, api::ToolType::Subagent];

    remove_orchestration_tools_if_disabled(&mut supported_tools, true);

    assert_eq!(
        supported_tools,
        vec![api::ToolType::Grep, api::ToolType::Subagent]
    );
}

#[test]
fn supported_tools_omit_hosted_only_capabilities() {
    let params = request_params_with_ask_user_question_enabled(false);
    let supported_tools = get_supported_tools(&params);

    for hosted_only_tool in [
        api::ToolType::InitProject,
        api::ToolType::OpenCodeReview,
        api::ToolType::SuggestNewConversation,
        api::ToolType::SuggestPrompt,
        api::ToolType::UploadFileArtifact,
        api::ToolType::UseComputer,
        api::ToolType::RequestComputerUse,
        api::ToolType::InsertReviewComments,
        api::ToolType::RunAgents,
        api::ToolType::SendMessageToAgent,
        api::ToolType::WaitForEvents,
    ] {
        assert!(
            !supported_tools.contains(&hosted_only_tool),
            "{hosted_only_tool:?} has no direct-provider tool schema"
        );
    }
}

#[test]
fn supported_tools_omits_ask_user_question_when_disabled() {
    let params = request_params_with_ask_user_question_enabled(false);
    let supported_tools = get_supported_tools(&params);

    assert!(!supported_tools.contains(&api::ToolType::AskUserQuestion));
}

#[test]
fn supported_tools_includes_ask_user_question_when_enabled_and_feature_flag_is_enabled() {
    let _flag = FeatureFlag::AskUserQuestion.override_enabled(true);

    let params = request_params_with_ask_user_question_enabled(true);
    let supported_tools = get_supported_tools(&params);

    assert!(supported_tools.contains(&api::ToolType::AskUserQuestion));
}

#[test]
fn question_overrides_respect_mode_capability_and_feature_without_enabling_cli_pauses() {
    use crate::ai::execution_profiles::PermissionMode;

    for (mode, enabled, feature_enabled, expected) in [
        (PermissionMode::AskFirst, true, true, true),
        (PermissionMode::AllowAll, true, true, true),
        (PermissionMode::AllowAll, false, true, false),
        (PermissionMode::AllowAll, true, false, false),
        (PermissionMode::GalaxyDecides, true, true, false),
        (PermissionMode::ReadOnly, true, true, false),
    ] {
        let _flag = FeatureFlag::AskUserQuestion.override_enabled(feature_enabled);
        let mut params = request_params_with_ask_user_question_enabled(enabled);
        params.permission_mode = mode;
        params.supported_tools_override = Some(vec![api::ToolType::AskUserQuestion]);

        let (normal, cli) = super::prepare_direct_provider_params(&mut params);

        assert_eq!(
            normal.contains(&api::ToolType::AskUserQuestion),
            expected,
            "{mode:?}"
        );
        assert!(!cli.contains(&api::ToolType::AskUserQuestion), "{mode:?}");
    }
}

#[test]
fn question_availability_does_not_expand_empty_tool_overrides() {
    let _flag = FeatureFlag::AskUserQuestion.override_enabled(true);
    let mut params = request_params_with_ask_user_question_enabled(true);
    params.permission_mode = crate::ai::execution_profiles::PermissionMode::AllowAll;
    params.supported_tools_override = Some(Vec::new());

    let (normal, cli) = super::prepare_direct_provider_params(&mut params);

    assert!(normal.is_empty());
    assert!(cli.is_empty());
}

#[test]
fn remote_supported_tools_include_search_codebase_when_connected_and_feature_flag_is_enabled() {
    let _flag = FeatureFlag::RemoteCodebaseIndexing.override_enabled(true);
    let params = request_params_for_remote(Some(HostId::new("host".to_string())));
    let supported_tools = get_supported_tools(&params);
    let supported_cli_agent_tools = get_supported_cli_agent_tools(&params);

    assert!(supported_tools.contains(&api::ToolType::SearchCodebase));
    assert!(supported_cli_agent_tools.contains(&api::ToolType::SearchCodebase));
}
#[test]
fn remote_supported_tools_omit_search_codebase_when_feature_flag_is_disabled() {
    let _flag = FeatureFlag::RemoteCodebaseIndexing.override_enabled(false);
    let params = request_params_for_remote(Some(HostId::new("host".to_string())));
    let supported_tools = get_supported_tools(&params);
    let supported_cli_agent_tools = get_supported_cli_agent_tools(&params);

    assert!(!supported_tools.contains(&api::ToolType::SearchCodebase));
    assert!(!supported_cli_agent_tools.contains(&api::ToolType::SearchCodebase));
}

#[test]
fn remote_supported_tools_omit_search_codebase_when_remote_is_not_connected() {
    let _flag = FeatureFlag::RemoteCodebaseIndexing.override_enabled(true);
    let params = request_params_for_remote(None);
    let supported_tools = get_supported_tools(&params);
    let supported_cli_agent_tools = get_supported_cli_agent_tools(&params);

    assert!(!supported_tools.contains(&api::ToolType::SearchCodebase));
    assert!(!supported_cli_agent_tools.contains(&api::ToolType::SearchCodebase));
}

#[test]
fn read_only_mode_filters_even_explicit_tool_overrides() {
    let mut params = request_params_with_ask_user_question_enabled(true);
    params.permission_mode = crate::ai::execution_profiles::PermissionMode::ReadOnly;
    params.supported_tools_override = Some(vec![
        api::ToolType::Grep,
        api::ToolType::ReadFiles,
        api::ToolType::RunShellCommand,
        api::ToolType::ApplyFileDiffs,
        api::ToolType::CallMcpTool,
        api::ToolType::RunAgents,
    ]);
    let (base, cli) = super::prepare_direct_provider_params(&mut params);
    assert_eq!(base, vec![api::ToolType::Grep, api::ToolType::ReadFiles]);
    assert_eq!(cli, base);
}

#[test]
fn read_only_advertises_shell_only_when_explicit_command_approvals_exist() {
    let mut params = request_params_with_ask_user_question_enabled(true);
    params.permission_mode = crate::ai::execution_profiles::PermissionMode::ReadOnly;
    params.command_allowlist = vec!["^cargo test$".to_string()];
    let (base, _) = super::prepare_direct_provider_params(&mut params);
    assert!(base.contains(&api::ToolType::RunShellCommand));
    assert!(!base.contains(&api::ToolType::ApplyFileDiffs));
    assert!(!base.contains(&api::ToolType::RunAgents));
}

#[test]
fn explicit_overrides_cannot_restore_interactive_shell_tools() {
    let mut params = request_params_with_ask_user_question_enabled(false);
    params.supported_tools_override = Some(vec![
        api::ToolType::RunShellCommand,
        api::ToolType::WriteToLongRunningShellCommand,
        api::ToolType::ReadShellCommandOutput,
        api::ToolType::TransferShellCommandControlToUser,
    ]);
    let (normal, cli) = super::prepare_direct_provider_params(&mut params);
    for tools in [normal, cli] {
        assert_eq!(tools, vec![api::ToolType::RunShellCommand]);
    }
}
