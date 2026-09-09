use super::*;

#[test]
fn stop_takeover_does_not_request_a_completion_assessment() {
    let state = LongRunningCommandControlState::User {
        reason: UserTakeOverReason::Stop,
    };

    assert!(!should_request_completion_assessment(Some(&state)));
}

#[test]
fn non_stop_control_states_can_request_a_completion_assessment() {
    let agent_state = LongRunningCommandControlState::Agent {
        is_blocked: false,
        should_hide_responses: false,
    };
    let transfer_state = LongRunningCommandControlState::User {
        reason: UserTakeOverReason::TransferFromAgent {
            reason: "needs user input".to_owned(),
        },
    };

    assert!(should_request_completion_assessment(None));
    assert!(should_request_completion_assessment(Some(&agent_state)));
    assert!(should_request_completion_assessment(Some(&transfer_state)));
}

#[test]
fn prose_monitor_turn_is_nudged_once_until_a_tool_action_runs() {
    assert!(should_nudge_monitor_turn(false, false));
    assert!(!should_nudge_monitor_turn(false, true));
    assert!(!should_nudge_monitor_turn(true, false));
}

#[test]
fn shell_control_event_must_match_conversation_and_requested_command() {
    let active_conversation_id = AIConversationId::new();
    let other_conversation_id = AIConversationId::new();
    let active_action_id = AIAgentActionId::from("same-action".to_owned());
    let other_action_id = AIAgentActionId::from("other-action".to_owned());

    assert!(matches_active_requested_command(
        active_conversation_id,
        &active_action_id,
        Some(active_conversation_id),
        Some(&active_action_id),
    ));
    assert!(!matches_active_requested_command(
        other_conversation_id,
        &active_action_id,
        Some(active_conversation_id),
        Some(&active_action_id),
    ));
    assert!(!matches_active_requested_command(
        active_conversation_id,
        &other_action_id,
        Some(active_conversation_id),
        Some(&active_action_id),
    ));
}

#[test]
fn requested_command_identity_rejects_duplicate_id_from_another_conversation() {
    let active_conversation_id = AIConversationId::new();
    let other_conversation_id = AIConversationId::new();
    let duplicate_action_id = AIAgentActionId::from("duplicate-action".to_owned());

    assert!(matches_requested_command_identity(
        active_conversation_id,
        &duplicate_action_id,
        Some(active_conversation_id),
        Some(&duplicate_action_id),
    ));
    assert!(!matches_requested_command_identity(
        other_conversation_id,
        &duplicate_action_id,
        Some(active_conversation_id),
        Some(&duplicate_action_id),
    ));
}
