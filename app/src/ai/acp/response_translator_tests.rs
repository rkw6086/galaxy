use galaxy_acp::{
    AcpEvent, AgentCapabilities, ContentBlock, SessionId, StopReason, TextContent, ToolCallId,
    ToolCallStatus,
};
use warp_multi_agent_api::{client_action, message, response_event};

use super::AcpResponseTranslator;

#[test]
fn initializes_the_existing_chat_exchange_and_persists_user_text() {
    let mut translator = AcpResponseTranslator::new(
        "task".to_owned(),
        true,
        Some("hello".to_owned()),
        "acp:codex".to_owned(),
    );
    let events = translator
        .translate(AcpEvent::SessionStarted {
            session_id: SessionId::from("session"),
            agent_info: None,
            capabilities: AgentCapabilities::default(),
            can_load: true,
            can_steer: true,
        })
        .expect("translate");

    assert!(matches!(
        events[0].r#type,
        Some(response_event::Type::Init(_))
    ));
    assert!(matches!(
        events[1].r#type,
        Some(response_event::Type::ClientActions(_))
    ));
    assert!(matches!(
        events[2].r#type,
        Some(response_event::Type::ClientActions(_))
    ));
}

#[test]
fn streams_agent_text_as_add_then_append() {
    let mut translator =
        AcpResponseTranslator::new("task".to_owned(), false, None, "acp:codex".to_owned());
    let first = translator
        .translate(AcpEvent::AgentText {
            text: "one".to_owned(),
        })
        .expect("first");
    let second = translator
        .translate(AcpEvent::AgentText {
            text: " two".to_owned(),
        })
        .expect("second");

    let Some(response_event::Type::ClientActions(first_actions)) = &first[1].r#type else {
        panic!("expected first client action");
    };
    assert!(matches!(
        first_actions.actions[0].action,
        Some(client_action::Action::AddMessagesToTask(_))
    ));
    let Some(response_event::Type::ClientActions(second_actions)) = &second[0].r#type else {
        panic!("expected append client action");
    };
    assert!(matches!(
        second_actions.actions[0].action,
        Some(client_action::Action::AppendToMessageContent(_))
    ));
}

#[test]
fn renders_acp_tool_progress_as_text_not_an_executable_galaxy_action() {
    let mut translator =
        AcpResponseTranslator::new("task".to_owned(), false, None, "acp:codex".to_owned());
    let events = translator
        .translate(AcpEvent::ToolCall {
            id: ToolCallId::from("tool-1"),
            title: "Read file".to_owned(),
            status: ToolCallStatus::InProgress,
            output: None,
        })
        .expect("tool");
    let Some(response_event::Type::ClientActions(actions)) = &events[1].r#type else {
        panic!("expected client action");
    };
    let Some(client_action::Action::AddMessagesToTask(add)) = &actions.actions[0].action else {
        panic!("expected display-only message");
    };
    assert!(matches!(
        add.messages[0].message,
        Some(message::Message::AgentOutput(_))
    ));
}

#[test]
fn maps_usage_and_successful_completion() {
    let mut translator =
        AcpResponseTranslator::new("task".to_owned(), false, None, "acp:codex".to_owned());
    translator
        .translate(AcpEvent::Usage {
            used: 25,
            size: 100,
            cost: None,
        })
        .expect("usage");
    let events = translator
        .translate(AcpEvent::Finished {
            stop_reason: StopReason::EndTurn,
        })
        .expect("finished");
    let Some(finished) = events.iter().find_map(|event| {
        let Some(response_event::Type::Finished(finished)) = &event.r#type else {
            return None;
        };
        Some(finished)
    }) else {
        panic!("expected finished");
    };
    assert_eq!(finished.token_usage[0].total_input, 0);
    assert_eq!(
        finished
            .conversation_usage_metadata
            .as_ref()
            .expect("metadata")
            .context_window_usage,
        0.25
    );
}

#[test]
fn renders_bounded_tool_output_in_the_agent_transcript() {
    let mut translator =
        AcpResponseTranslator::new("task".to_owned(), false, None, "acp:codex".to_owned());
    let events = translator
        .translate(AcpEvent::ToolCall {
            id: ToolCallId::from("tool-1"),
            title: "Run tests".to_owned(),
            status: ToolCallStatus::Completed,
            output: Some("test one ... ok\ntest two ... ok".to_owned()),
        })
        .expect("tool");

    let Some(response_event::Type::ClientActions(status_actions)) = &events[1].r#type else {
        panic!("expected status action");
    };
    let Some(client_action::Action::AddMessagesToTask(add_status)) =
        &status_actions.actions[0].action
    else {
        panic!("expected status message");
    };
    let Some(message::Message::AgentOutput(status)) = &add_status.messages[0].message else {
        panic!("expected agent output");
    };
    assert!(status.text.contains("Run tests"));

    let Some(response_event::Type::ClientActions(output_actions)) = &events[2].r#type else {
        panic!("expected output action");
    };
    assert!(matches!(
        output_actions.actions[0].action,
        Some(client_action::Action::AppendToMessageContent(_))
    ));
}

#[test]
fn successful_turn_without_agent_output_is_still_visible() {
    let mut translator =
        AcpResponseTranslator::new("task".to_owned(), false, None, "acp:codex".to_owned());
    let events = translator
        .translate(AcpEvent::Finished {
            stop_reason: StopReason::EndTurn,
        })
        .expect("finished");

    assert!(events.iter().any(|event| {
        let Some(response_event::Type::ClientActions(actions)) = &event.r#type else {
            return false;
        };
        let Some(client_action::Action::AddMessagesToTask(add)) = &actions.actions[0].action else {
            return false;
        };
        let Some(message::Message::AgentOutput(output)) = &add.messages[0].message else {
            return false;
        };
        output.text.contains("completed without a text response")
    }));
}

#[test]
fn suppresses_unsolicited_user_content_so_initial_hidden_context_cannot_leak() {
    let mut translator =
        AcpResponseTranslator::new("task".to_owned(), false, None, "acp:codex".to_owned());

    let events = translator
        .translate(AcpEvent::UserContent {
            content: ContentBlock::Text(TextContent::new(
                "hidden initial prompt and system context",
            )),
        })
        .expect("translate");

    assert!(events.is_empty());
}

#[test]
fn live_steering_adds_a_user_bubble_and_starts_a_new_assistant_bubble() {
    let mut translator =
        AcpResponseTranslator::new("task".to_owned(), false, None, "acp:codex".to_owned());
    translator
        .translate(AcpEvent::AgentText {
            text: "original response".to_owned(),
        })
        .expect("initial output");

    let steered = translator
        .translate_steered_user_content(ContentBlock::Text(TextContent::new("stop at 75s")))
        .expect("steering");
    let Some(response_event::Type::ClientActions(user_actions)) = &steered[0].r#type else {
        panic!("expected user client action");
    };
    let Some(client_action::Action::AddMessagesToTask(add_user)) = &user_actions.actions[0].action
    else {
        panic!("expected user message");
    };
    assert!(matches!(
        add_user.messages[0].message,
        Some(message::Message::UserQuery(_))
    ));

    let resumed = translator
        .translate(AcpEvent::AgentText {
            text: "steered response".to_owned(),
        })
        .expect("resumed output");
    let Some(response_event::Type::ClientActions(agent_actions)) = &resumed[0].r#type else {
        panic!("expected agent client action");
    };
    assert!(matches!(
        agent_actions.actions[0].action,
        Some(client_action::Action::AddMessagesToTask(_))
    ));
}

#[test]
fn steering_failure_surfaces_an_indeterminate_delivery_warning() {
    let mut translator =
        AcpResponseTranslator::new("task".to_owned(), false, None, "acp:codex".to_owned());
    translator
        .translate(AcpEvent::SessionStarted {
            session_id: SessionId::from("session"),
            agent_info: None,
            capabilities: AgentCapabilities::default(),
            can_load: true,
            can_steer: true,
        })
        .expect("initialize");

    let events = translator.steering_failed("turn is no longer active");

    assert_eq!(events.len(), 1);
    let Some(response_event::Type::ClientActions(error_actions)) = &events[0].r#type else {
        panic!("expected visible error action");
    };
    let Some(client_action::Action::AddMessagesToTask(add_error)) =
        &error_actions.actions[0].action
    else {
        panic!("expected visible error message");
    };
    let Some(message::Message::AgentOutput(output)) = &add_error.messages[0].message else {
        panic!("expected agent output");
    };
    assert!(output.text.contains("couldn't confirm"));
    assert!(output.text.contains("before retrying"));
}

#[test]
fn implicit_steering_turn_warning_does_not_recommend_a_blind_retry() {
    let mut translator =
        AcpResponseTranslator::new("task".to_owned(), false, None, "acp:codex".to_owned());

    let events = translator.steering_started_new_turn();

    let text =
        events
            .iter()
            .filter_map(|event| match &event.r#type {
                Some(response_event::Type::ClientActions(actions)) => actions
                    .actions
                    .iter()
                    .find_map(|action| match &action.action {
                        Some(client_action::Action::AddMessagesToTask(add)) => add
                            .messages
                            .iter()
                            .find_map(|message| match &message.message {
                                Some(message::Message::AgentOutput(output)) => {
                                    Some(output.text.as_str())
                                }
                                _ => None,
                            }),
                        _ => None,
                    }),
                _ => None,
            })
            .collect::<String>();
    assert!(text.contains("started"));
    assert!(text.contains("terminated"));
    assert!(text.contains("immediately"));
    assert!(text.contains("may have begun acting"));
}

#[test]
fn startup_error_keeps_the_user_request_and_finishes_visibly() {
    let mut translator = AcpResponseTranslator::new(
        "task".to_owned(),
        false,
        Some("help me".to_owned()),
        "acp:codex".to_owned(),
    );

    let events = translator.startup_error("adapter missing");

    assert_eq!(events.len(), 4);
    assert!(matches!(
        events[0].r#type,
        Some(response_event::Type::Init(_))
    ));
    let Some(response_event::Type::ClientActions(user_actions)) = &events[1].r#type else {
        panic!("expected visible user request");
    };
    let Some(client_action::Action::AddMessagesToTask(user_messages)) =
        &user_actions.actions[0].action
    else {
        panic!("expected user message");
    };
    assert!(matches!(
        user_messages.messages[0].message,
        Some(message::Message::UserQuery(_))
    ));
    let Some(response_event::Type::ClientActions(error_actions)) = &events[2].r#type else {
        panic!("expected visible startup error");
    };
    let Some(client_action::Action::AddMessagesToTask(error_messages)) =
        &error_actions.actions[0].action
    else {
        panic!("expected error message");
    };
    let Some(message::Message::AgentOutput(output)) = &error_messages.messages[0].message else {
        panic!("expected agent output");
    };
    assert!(output.text.contains("adapter missing"));
    assert!(matches!(
        events[3].r#type,
        Some(response_event::Type::Finished(_))
    ));
}
