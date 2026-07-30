use std::collections::HashMap;
use std::sync::Arc;

use super::{galaxy_mcp_args, request_user_query, GalaxyMcpTarget};
use crate::ai::agent::api::RequestParams;
use crate::ai::agent::{AIAgentInput, EntrypointType, RequestMetadata, UserQueryMode};

#[test]
fn galaxy_mcp_is_pinned_to_the_app_process_and_full_terminal_hierarchy() {
    let target = GalaxyMcpTarget {
        window_id: "WindowId(7)".to_owned(),
        tab_id: "EntityId(21)".to_owned(),
        pane_id: "Pane Terminal (42)".to_owned(),
    };
    let args = galaxy_mcp_args(&target, true, true);

    assert_eq!(
        args,
        vec![
            "--galaxyctrl",
            "mcp",
            "--pid",
            &std::process::id().to_string(),
            "--window",
            "WindowId(7)",
            "--tab",
            "EntityId(21)",
            "--pane",
            "Pane Terminal (42)",
            "--agent-safe",
            "--allow-terminal-execute",
            "--allow-terminal-interrupt",
        ]
    );
}

#[test]
fn galaxy_mcp_omits_terminal_mutations_without_profile_permission() {
    let target = GalaxyMcpTarget {
        window_id: "WindowId(7)".to_owned(),
        tab_id: "EntityId(21)".to_owned(),
        pane_id: "Pane Terminal (42)".to_owned(),
    };
    let args = galaxy_mcp_args(&target, false, false);

    assert_eq!(
        args,
        vec![
            "--galaxyctrl",
            "mcp",
            "--pid",
            &std::process::id().to_string(),
            "--window",
            "WindowId(7)",
            "--tab",
            "EntityId(21)",
            "--pane",
            "Pane Terminal (42)",
            "--agent-safe",
        ]
    );
}

fn request_with_entrypoint(entrypoint: EntrypointType) -> RequestParams {
    let mut params = RequestParams::new_for_test();
    params.input.push(AIAgentInput::UserQuery {
        query: "visible request".to_owned(),
        context: Arc::from([]),
        static_query_type: None,
        referenced_attachments: HashMap::new(),
        user_query_mode: UserQueryMode::Normal,
        running_command: None,
        intended_agent: None,
    });
    params.metadata = Some(RequestMetadata {
        is_autodetected_user_query: false,
        entrypoint,
        is_auto_resume_after_error: false,
    });
    params
}

#[test]
fn explicit_user_entrypoints_keep_the_initial_query_bubble() {
    let params = request_with_entrypoint(EntrypointType::UserInitiated);

    assert_eq!(
        request_user_query(&params).as_deref(),
        Some("visible request")
    );
}

#[test]
fn automatic_and_system_entrypoints_hide_the_synthetic_query_bubble() {
    for entrypoint in [
        EntrypointType::InitProjectRules,
        EntrypointType::TriggerPassiveSuggestion { trigger: None },
        EntrypointType::AgentInitiated,
        EntrypointType::ResumeConversation,
    ] {
        assert_eq!(
            request_user_query(&request_with_entrypoint(entrypoint)),
            None
        );
    }
}

#[test]
fn automatic_error_resume_does_not_duplicate_the_original_user_query() {
    let mut params = request_with_entrypoint(EntrypointType::UserInitiated);
    params.metadata.as_mut().unwrap().is_auto_resume_after_error = true;

    assert_eq!(request_user_query(&params), None);
}
