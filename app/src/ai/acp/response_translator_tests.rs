use galaxy_agent_core::{AgentEvent, RuntimeActivity, RuntimeActivityStatus, StopReason};
use warp_multi_agent_api::{client_action, message, response_event};

use super::AcpResponseTranslator;
use crate::ai::agent::runtime_activity;

#[test]
fn initializes_the_existing_chat_exchange_and_persists_user_text() {
    let mut translator = AcpResponseTranslator::new(
        "task".to_owned(),
        true,
        Some("hello".to_owned()),
        "acp:codex".to_owned(),
    );
    let events = translator
        .translate(AgentEvent::TurnStarted {
            runtime_request_id: "session".to_owned(),
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
        .translate(AgentEvent::TextDelta {
            text: "one".to_owned(),
        })
        .expect("first");
    let second = translator
        .translate(AgentEvent::TextDelta {
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
fn renders_acp_tool_progress_as_structured_non_executable_activity() {
    let mut translator =
        AcpResponseTranslator::new("task".to_owned(), false, None, "acp:codex".to_owned());
    let events = translator
        .translate(AgentEvent::RuntimeActivityUpdated {
            activity: RuntimeActivity {
                id: "tool-1".to_owned(),
                title: "Read file".to_owned(),
                status: Some(RuntimeActivityStatus::InProgress),
                output: None,
            },
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
    assert_eq!(
        runtime_activity::decode(&add.messages[0].server_message_data),
        Some(RuntimeActivity {
            id: "tool-1".to_owned(),
            title: "Read file".to_owned(),
            status: Some(RuntimeActivityStatus::InProgress),
            output: None,
        })
    );
}

#[test]
fn maps_usage_and_successful_completion() {
    let mut translator =
        AcpResponseTranslator::new("task".to_owned(), false, None, "acp:codex".to_owned());
    translator
        .translate(AgentEvent::ContextUsageUpdated {
            used_tokens: 25,
            context_size: 100,
        })
        .expect("usage");
    let events = translator
        .translate(AgentEvent::TurnStopped {
            reason: StopReason::Completed,
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
fn updates_the_existing_activity_card_with_bounded_tool_output() {
    let mut translator =
        AcpResponseTranslator::new("task".to_owned(), false, None, "acp:codex".to_owned());
    let started = translator
        .translate(AgentEvent::RuntimeActivityUpdated {
            activity: RuntimeActivity {
                id: "tool-1".to_owned(),
                title: "Run tests".to_owned(),
                status: Some(RuntimeActivityStatus::InProgress),
                output: None,
            },
        })
        .expect("started");
    let Some(response_event::Type::ClientActions(start_actions)) = &started[1].r#type else {
        panic!("expected initial activity action");
    };
    let Some(client_action::Action::AddMessagesToTask(add)) = &start_actions.actions[0].action
    else {
        panic!("expected initial activity message");
    };
    let message_id = add.messages[0].id.clone();

    let events = translator
        .translate(AgentEvent::RuntimeActivityUpdated {
            activity: RuntimeActivity {
                id: "tool-1".to_owned(),
                title: "Run tests".to_owned(),
                status: Some(RuntimeActivityStatus::Completed),
                output: Some("test one ... ok\ntest two ... ok".to_owned()),
            },
        })
        .expect("tool");

    let Some(response_event::Type::ClientActions(update_actions)) = &events[0].r#type else {
        panic!("expected update action");
    };
    let Some(client_action::Action::UpdateTaskMessage(update)) = &update_actions.actions[0].action
    else {
        panic!("expected in-place activity update");
    };
    let updated_message = update.message.as_ref().expect("updated message");
    assert_eq!(updated_message.id, message_id);
    assert_eq!(
        runtime_activity::decode(&updated_message.server_message_data),
        Some(RuntimeActivity {
            id: "tool-1".to_owned(),
            title: "Run tests".to_owned(),
            status: Some(RuntimeActivityStatus::Completed),
            output: Some("test one ... ok\ntest two ... ok".to_owned()),
        })
    );
}

#[test]
fn activity_only_turn_does_not_claim_the_agent_returned_no_output() {
    let mut translator =
        AcpResponseTranslator::new("task".to_owned(), false, None, "acp:codex".to_owned());
    translator
        .translate(AgentEvent::RuntimeActivityUpdated {
            activity: RuntimeActivity {
                id: "tool-1".to_owned(),
                title: "Inspect repository".to_owned(),
                status: Some(RuntimeActivityStatus::Completed),
                output: Some("done".to_owned()),
            },
        })
        .expect("activity");

    let events = translator
        .translate(AgentEvent::TurnStopped {
            reason: StopReason::Completed,
        })
        .expect("finished");

    assert_eq!(events.len(), 1);
    assert!(matches!(
        events[0].r#type,
        Some(response_event::Type::Finished(_))
    ));
}

#[test]
fn successful_turn_without_agent_output_is_still_visible() {
    let mut translator =
        AcpResponseTranslator::new("task".to_owned(), false, None, "acp:codex".to_owned());
    let events = translator
        .translate(AgentEvent::TurnStopped {
            reason: StopReason::Completed,
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
fn reasoning_is_not_flattened_into_the_plain_answer_transcript() {
    let mut translator =
        AcpResponseTranslator::new("task".to_owned(), false, None, "acp:codex".to_owned());

    let events = translator
        .translate(AgentEvent::ReasoningDelta {
            text: "private chain of thought".to_owned(),
        })
        .expect("translate");

    assert!(events.is_empty());
}

#[test]
fn live_steering_adds_a_user_bubble_and_starts_a_new_assistant_bubble() {
    let mut translator =
        AcpResponseTranslator::new("task".to_owned(), false, None, "acp:codex".to_owned());
    translator
        .translate(AgentEvent::TextDelta {
            text: "original response".to_owned(),
        })
        .expect("initial output");

    let steered = translator
        .translate(AgentEvent::UserInputAccepted {
            text: "stop at 75s".to_owned(),
        })
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
        .translate(AgentEvent::TextDelta {
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
        .translate(AgentEvent::TurnStarted {
            runtime_request_id: "session".to_owned(),
        })
        .expect("initialize");

    let events = translator
        .translate(AgentEvent::RuntimeNotice {
            message: "Galaxy couldn't confirm that live steering message: turn is no longer active. The agent may not have received it; check the current terminal and file state before retrying.".to_owned(),
        })
        .expect("notice");

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

    let events = translator
        .translate(AgentEvent::RuntimeNotice {
            message: "The ACP adapter started that steering message as a separate turn instead of injecting it into the active one. Galaxy terminated the adapter process immediately, but the turn may have begun acting; check the current terminal and file state before retrying.".to_owned(),
        })
        .expect("notice");

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
