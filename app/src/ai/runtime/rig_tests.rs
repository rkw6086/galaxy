use galaxy_agent_core::StopReason;
use warp_multi_agent_api::response_event::stream_finished;

use super::{build_add_reasoning, build_append_reasoning, map_stop_reason, saturating_i32};

#[test]
fn stop_reasons_map_to_the_existing_ui_contract() {
    assert!(matches!(
        map_stop_reason(StopReason::Completed),
        stream_finished::Reason::Done(_)
    ));
    assert!(matches!(
        map_stop_reason(StopReason::MaxTokens),
        stream_finished::Reason::MaxTokenLimit(_)
    ));
    assert!(matches!(
        map_stop_reason(StopReason::Cancelled),
        stream_finished::Reason::Other(_)
    ));
}

#[test]
fn token_counts_saturate_at_the_proto_limit() {
    assert_eq!(saturating_i32(u64::MAX), i32::MAX);
}

#[test]
fn reasoning_events_match_the_existing_ui_message_contract() {
    let add = build_add_reasoning("task", "message", "think");
    let append = build_append_reasoning("task", "message", " more");

    let Some(warp_multi_agent_api::response_event::Type::ClientActions(add)) = add.r#type else {
        panic!("expected client actions");
    };
    let Some(warp_multi_agent_api::client_action::Action::AddMessagesToTask(add)) =
        &add.actions[0].action
    else {
        panic!("expected add-message action");
    };
    assert!(matches!(
        add.messages[0].message.as_ref(),
        Some(warp_multi_agent_api::message::Message::AgentReasoning(reasoning))
            if reasoning.reasoning == "think"
    ));

    let Some(warp_multi_agent_api::response_event::Type::ClientActions(append)) = append.r#type
    else {
        panic!("expected client actions");
    };
    let Some(warp_multi_agent_api::client_action::Action::AppendToMessageContent(append)) =
        &append.actions[0].action
    else {
        panic!("expected append-message action");
    };
    assert_eq!(
        append.mask.as_ref().unwrap().paths,
        ["agent_reasoning.reasoning"]
    );
}
