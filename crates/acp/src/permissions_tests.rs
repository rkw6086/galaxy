use agent_client_protocol::schema::v1::{
    PermissionOption, PermissionOptionKind, RequestPermissionOutcome, RequestPermissionRequest,
    ToolCallUpdate, ToolCallUpdateFields, ToolKind,
};

use super::*;

fn permission_request() -> RequestPermissionRequest {
    permission_request_for(ToolKind::Other)
}

fn permission_request_for(kind: ToolKind) -> RequestPermissionRequest {
    RequestPermissionRequest::new(
        "session-1",
        ToolCallUpdate::new(
            "tool-1",
            ToolCallUpdateFields::new().title("Run command").kind(kind),
        ),
        vec![
            PermissionOption::new(
                "allow-always",
                "Always allow",
                PermissionOptionKind::AllowAlways,
            ),
            PermissionOption::new("allow-once", "Allow once", PermissionOptionKind::AllowOnce),
            PermissionOption::new(
                "reject-always",
                "Always reject",
                PermissionOptionKind::RejectAlways,
            ),
            PermissionOption::new(
                "reject-once",
                "Reject once",
                PermissionOptionKind::RejectOnce,
            ),
        ],
    )
}

#[test]
fn default_handler_denies_interactive_turns() {
    let request = permission_request();
    let decision =
        futures::executor::block_on(DenyByDefaultPermissionHandler.decide(PermissionContext {
            request,
            auto_approve: false,
            policy: AcpPermissionPolicy::default(),
        }));

    assert_eq!(decision, PermissionDecision::Deny);
}

#[test]
fn default_handler_allows_only_explicit_autonomous_turns() {
    let request = permission_request();
    let decision =
        futures::executor::block_on(DenyByDefaultPermissionHandler.decide(PermissionContext {
            request,
            auto_approve: true,
            policy: AcpPermissionPolicy::default(),
        }));

    assert_eq!(decision, PermissionDecision::Allow);
}

#[test]
fn default_handler_honors_only_the_matching_profile_category() {
    let policy = AcpPermissionPolicy {
        read: true,
        ..Default::default()
    };
    let read =
        futures::executor::block_on(DenyByDefaultPermissionHandler.decide(PermissionContext {
            request: permission_request_for(ToolKind::Read),
            auto_approve: false,
            policy,
        }));
    let execute =
        futures::executor::block_on(DenyByDefaultPermissionHandler.decide(PermissionContext {
            request: permission_request_for(ToolKind::Execute),
            auto_approve: false,
            policy,
        }));

    assert_eq!(read, PermissionDecision::Allow);
    assert_eq!(execute, PermissionDecision::Deny);
}

#[test]
fn allow_and_deny_prefer_one_shot_options() {
    let request = permission_request();

    let allow = outcome_for_decision(&request, PermissionDecision::Allow);
    let deny = outcome_for_decision(&request, PermissionDecision::Deny);

    assert_eq!(
        allow,
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new("allow-once"))
    );
    assert_eq!(
        deny,
        RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new("reject-once"))
    );
}

#[test]
fn allow_never_silently_selects_a_persistent_option() {
    let mut request = permission_request();
    request
        .options
        .retain(|option| option.kind != PermissionOptionKind::AllowOnce);

    let outcome = outcome_for_decision(&request, PermissionDecision::Allow);

    assert_eq!(outcome, RequestPermissionOutcome::Cancelled);
}

#[test]
fn unknown_selected_option_is_cancelled() {
    let outcome = outcome_for_decision(
        &permission_request(),
        PermissionDecision::Select(PermissionOptionId::new("not-advertised")),
    );

    assert_eq!(outcome, RequestPermissionOutcome::Cancelled);
}
