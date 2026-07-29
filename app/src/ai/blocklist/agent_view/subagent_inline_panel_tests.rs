use warp_multi_agent_api as api;

use super::{
    collect_visible_transcript, completion_summary_from_messages, extract_message_text,
    truncate_with_ellipsis, SubagentPanelState,
};
use crate::ai::agent::conversation::AIConversationId;

fn message(content: api::message::Message) -> api::Message {
    api::Message {
        message: Some(content),
        ..Default::default()
    }
}

fn user_query(text: &str) -> api::Message {
    message(api::message::Message::UserQuery(api::message::UserQuery {
        query: text.to_string(),
        ..Default::default()
    }))
}

fn system_query() -> api::Message {
    message(api::message::Message::SystemQuery(
        api::message::SystemQuery::default(),
    ))
}

fn agent_output(text: &str) -> api::Message {
    message(api::message::Message::AgentOutput(
        api::message::AgentOutput {
            text: text.to_string(),
        },
    ))
}

fn message_refs(messages: &[api::Message]) -> Vec<&api::Message> {
    messages.iter().collect()
}

#[test]
fn truncate_with_ellipsis_preserves_short_text() {
    assert_eq!(
        truncate_with_ellipsis("Galaxy terminal", 20),
        "Galaxy terminal"
    );
}

#[test]
fn truncate_with_ellipsis_is_unicode_safe() {
    let truncated = truncate_with_ellipsis("🚀🚀🚀🚀🚀 Galaxy", 8);

    assert_eq!(truncated.chars().count(), 8);
    assert!(truncated.ends_with("..."));
}

#[test]
fn truncate_with_ellipsis_handles_tiny_limits() {
    assert_eq!(truncate_with_ellipsis("Galaxy", 2), "..");
}

#[test]
fn new_panel_starts_expanded_so_agent_chat_is_visible() {
    let state = SubagentPanelState::new(AIConversationId::new());

    assert!(state.is_expanded);
}

#[test]
fn transcript_shows_user_and_agent_messages_but_hides_system_queries() {
    let system = system_query();
    let user = user_query("Please check the build");
    let agent = agent_output("The build is still running.");

    assert_eq!(extract_message_text(&system), None);
    assert_eq!(
        extract_message_text(&user).as_deref(),
        Some("Please check the build")
    );
    assert_eq!(
        extract_message_text(&agent).as_deref(),
        Some("The build is still running.")
    );
    assert_eq!(extract_message_text(&user_query("")), None);
    assert_eq!(extract_message_text(&agent_output("")), None);
}

#[test]
fn hidden_system_messages_do_not_displace_agent_responses() {
    let mut messages = vec![agent_output("Visible response to a system request")];
    messages.extend((0..32).map(|_| system_query()));
    let refs = message_refs(&messages);

    assert_eq!(
        collect_visible_transcript(&refs, 8),
        vec!["Visible response to a system request"]
    );
}

#[test]
fn transcript_limits_visible_messages_and_keeps_chronological_order() {
    let messages = (0..10)
        .map(|index| agent_output(&format!("response {index}")))
        .collect::<Vec<_>>();
    let refs = message_refs(&messages);

    assert_eq!(
        collect_visible_transcript(&refs, 8),
        (2..10)
            .map(|index| format!("response {index}"))
            .collect::<Vec<_>>()
    );
}

#[test]
fn completion_summary_uses_latest_agent_response() {
    let messages = vec![
        agent_output("Final assistant answer"),
        user_query("A trailing user message"),
        system_query(),
    ];
    let refs = message_refs(&messages);

    assert_eq!(
        completion_summary_from_messages(&refs).as_deref(),
        Some("Final assistant answer")
    );
}
