use agent_client_protocol::schema::v1::{ContentBlock, SessionId, StopReason, TextContent};

use super::*;

#[test]
fn session_started_exposes_agent_capabilities() {
    let event = AcpEvent::SessionStarted {
        session_id: SessionId::new("session-1"),
        can_load: true,
        can_steer: false,
    };

    assert_eq!(
        event,
        AcpEvent::SessionStarted {
            session_id: SessionId::new("session-1"),
            can_load: true,
            can_steer: false,
        }
    );
}

#[test]
fn finished_preserves_the_protocol_stop_reason() {
    let event = AcpEvent::Finished {
        stop_reason: StopReason::Cancelled,
    };

    assert_eq!(
        event,
        AcpEvent::Finished {
            stop_reason: StopReason::Cancelled,
        }
    );
}

#[test]
fn user_content_preserves_live_steering_input() {
    let content = ContentBlock::Text(TextContent::new("stop at 75 seconds"));
    let event = AcpEvent::UserContent {
        content: content.clone(),
    };

    assert_eq!(event, AcpEvent::UserContent { content });
}

#[test]
fn tool_events_preserve_display_safe_output() {
    let event = AcpEvent::ToolCall {
        id: "tool-1".into(),
        title: "Run tests".to_owned(),
        status: agent_client_protocol::schema::v1::ToolCallStatus::Completed,
        output: Some("42 tests passed".to_owned()),
    };

    assert_eq!(
        event,
        AcpEvent::ToolCall {
            id: "tool-1".into(),
            title: "Run tests".to_owned(),
            status: agent_client_protocol::schema::v1::ToolCallStatus::Completed,
            output: Some("42 tests passed".to_owned()),
        }
    );
}
