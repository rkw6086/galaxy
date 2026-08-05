use agent_client_protocol::schema::v1::{
    AgentCapabilities, ContentBlock, SessionId, TextContent, ToolCallId, ToolCallStatus,
};
use galaxy_agent_core::{
    AgentEvent, ContentPart, MessageContent, RuntimeActivity, RuntimeActivityStatus,
};

use super::*;

#[test]
fn shared_prompt_converts_once_to_official_acp_content() {
    let prompt = MessageContent::MultiPart(vec![
        ContentPart::Text("describe this".to_owned()),
        ContentPart::Image {
            data: b"image".to_vec(),
            mime_type: "image/png".to_owned(),
        },
    ]);

    let content = prompt_content(prompt).expect("prompt conversion");

    assert!(matches!(
        &content[0],
        ContentBlock::Text(text) if text.text == "describe this"
    ));
    assert!(matches!(
        &content[1],
        ContentBlock::Image(image)
            if image.data == "aW1hZ2U="
                && image.mime_type == "image/png"
                && image.uri.is_none()
    ));
}

#[test]
fn provider_tool_history_is_rejected_as_an_acp_prompt() {
    let error = prompt_content(MessageContent::ToolUse {
        tool_use_id: "tool-1".to_owned(),
        name: "shell".to_owned(),
        input: serde_json::json!({}),
    })
    .expect_err("tool history must be rejected");

    assert_eq!(error.kind, AgentErrorKind::InvalidRequest);
}

#[test]
fn session_start_updates_state_before_emitting_turn_started() {
    let state = Arc::new(Mutex::new(AcpRuntimeState::default()));
    let mut tool_titles = HashMap::new();

    let event = map_event(
        AcpEvent::SessionStarted {
            session_id: SessionId::new("session-42"),
            agent_info: None,
            capabilities: AgentCapabilities::default(),
            can_load: true,
            can_steer: true,
        },
        &state,
        &mut tool_titles,
    )
    .expect("event mapping");

    assert_eq!(
        event,
        Some(AgentEvent::TurnStarted {
            runtime_request_id: "session-42".to_owned(),
        })
    );
    assert_eq!(
        *state.lock().expect("state"),
        AcpRuntimeState {
            session_id: Some("session-42".to_owned()),
            can_load: true,
            can_steer: true,
            config_options: Vec::new(),
        }
    );
}

#[test]
fn acp_tool_progress_is_runtime_activity_not_a_host_tool_proposal() {
    let state = Arc::new(Mutex::new(AcpRuntimeState::default()));
    let mut tool_titles = HashMap::new();

    let event = map_event(
        AcpEvent::ToolCall {
            id: ToolCallId::new("tool-1"),
            title: "Run tests".to_owned(),
            status: ToolCallStatus::InProgress,
            output: Some("test_a ... ok".to_owned()),
        },
        &state,
        &mut tool_titles,
    )
    .expect("event mapping");

    assert_eq!(
        event,
        Some(AgentEvent::RuntimeActivityUpdated {
            activity: RuntimeActivity {
                id: "tool-1".to_owned(),
                title: "Run tests".to_owned(),
                status: Some(RuntimeActivityStatus::InProgress),
                output: Some("test_a ... ok".to_owned()),
            },
        })
    );
}

#[test]
fn unsolicited_user_echo_is_not_exposed_as_visible_input() {
    let state = Arc::new(Mutex::new(AcpRuntimeState::default()));
    let mut tool_titles = HashMap::new();

    let event = map_event(
        AcpEvent::UserContent {
            content: ContentBlock::Text(TextContent::new("hidden Galaxy context")),
        },
        &state,
        &mut tool_titles,
    )
    .expect("event mapping");

    assert_eq!(event, None);
}
