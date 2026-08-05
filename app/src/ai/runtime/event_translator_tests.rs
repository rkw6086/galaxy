use galaxy_agent_core::{
    AgentEvent, RuntimeActivity, RuntimeActivityStatus, RuntimeCapabilities, StopReason, Usage,
};
use warp_multi_agent_api::{client_action, message, response_event};

use super::{RuntimeResponseConfig, RuntimeResponseTranslator};
use crate::ai::agent::runtime_activity;

fn provider_translator() -> RuntimeResponseTranslator {
    RuntimeResponseTranslator::new(RuntimeResponseConfig {
        task_id: "task".to_owned(),
        conversation_id: "conversation".to_owned(),
        needs_create_task: false,
        user_query: None,
        model_id: "model".to_owned(),
        max_context_tokens: Some(1_000),
        capabilities: RuntimeCapabilities::provider(),
        empty_output_message: None,
    })
}

fn session_translator() -> RuntimeResponseTranslator {
    RuntimeResponseTranslator::new(RuntimeResponseConfig {
        task_id: "task".to_owned(),
        conversation_id: String::new(),
        needs_create_task: false,
        user_query: None,
        model_id: "session-runtime".to_owned(),
        max_context_tokens: None,
        capabilities: RuntimeCapabilities::session_runtime(),
        empty_output_message: Some("> runtime completed without text".to_owned()),
    })
}

#[test]
fn provider_and_session_runtimes_share_text_translation() {
    for mut translator in [provider_translator(), session_translator()] {
        let first = translator
            .translate(AgentEvent::TextDelta {
                text: "one".to_owned(),
            })
            .expect("first delta");
        let second = translator
            .translate(AgentEvent::TextDelta {
                text: " two".to_owned(),
            })
            .expect("second delta");

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
}

#[test]
fn reasoning_uses_the_native_reasoning_message_contract() {
    let mut translator = provider_translator();
    let first = translator
        .translate(AgentEvent::ReasoningDelta {
            text: "think".to_owned(),
        })
        .expect("reasoning");
    let second = translator
        .translate(AgentEvent::ReasoningDelta {
            text: " more".to_owned(),
        })
        .expect("reasoning append");

    let Some(response_event::Type::ClientActions(actions)) = &first[1].r#type else {
        panic!("expected reasoning action");
    };
    let Some(client_action::Action::AddMessagesToTask(add)) = &actions.actions[0].action else {
        panic!("expected reasoning message");
    };
    assert!(matches!(
        add.messages[0].message,
        Some(message::Message::AgentReasoning(_))
    ));
    let Some(response_event::Type::ClientActions(actions)) = &second[0].r#type else {
        panic!("expected reasoning append");
    };
    assert!(matches!(
        actions.actions[0].action,
        Some(client_action::Action::AppendToMessageContent(_))
    ));
}

#[test]
fn session_activity_updates_the_same_structured_message() {
    let mut translator = session_translator();
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
    let Some(response_event::Type::ClientActions(actions)) = &started[1].r#type else {
        panic!("expected activity action");
    };
    let Some(client_action::Action::AddMessagesToTask(add)) = &actions.actions[0].action else {
        panic!("expected activity message");
    };
    let message_id = add.messages[0].id.clone();

    let completed = translator
        .translate(AgentEvent::RuntimeActivityUpdated {
            activity: RuntimeActivity {
                id: "tool-1".to_owned(),
                title: String::new(),
                status: Some(RuntimeActivityStatus::Completed),
                output: Some("ok".to_owned()),
            },
        })
        .expect("completed");
    let Some(response_event::Type::ClientActions(actions)) = &completed[0].r#type else {
        panic!("expected activity update");
    };
    let Some(client_action::Action::UpdateTaskMessage(update)) = &actions.actions[0].action else {
        panic!("expected in-place update");
    };
    let message = update.message.as_ref().expect("updated message");
    assert_eq!(message.id, message_id);
    assert_eq!(
        runtime_activity::decode(&message.server_message_data),
        Some(RuntimeActivity {
            id: "tool-1".to_owned(),
            title: "Run tests".to_owned(),
            status: Some(RuntimeActivityStatus::Completed),
            output: Some("ok".to_owned()),
        })
    );
}

#[test]
fn usage_shape_follows_history_ownership_capability() {
    let mut provider = provider_translator();
    provider
        .translate(AgentEvent::UsageUpdated {
            usage: Usage {
                input_tokens: 250,
                output_tokens: 10,
                ..Usage::default()
            },
        })
        .expect("provider usage");
    let provider_finished = provider
        .translate(AgentEvent::TurnStopped {
            reason: StopReason::Completed,
        })
        .expect("provider finished");
    let Some(response_event::Type::Finished(finished)) = &provider_finished[1].r#type else {
        panic!("expected provider finish");
    };
    assert_eq!(finished.token_usage[0].total_input, 250);

    let mut session = session_translator();
    session
        .translate(AgentEvent::ContextUsageUpdated {
            used_tokens: 25,
            context_size: 100,
        })
        .expect("context usage");
    let session_finished = session
        .translate(AgentEvent::TurnStopped {
            reason: StopReason::Completed,
        })
        .expect("session finished");
    let Some(response_event::Type::Finished(finished)) = &session_finished[2].r#type else {
        panic!("expected session finish");
    };
    assert_eq!(finished.token_usage[0].total_input, 0);
    assert_eq!(
        finished
            .conversation_usage_metadata
            .as_ref()
            .expect("context metadata")
            .context_window_usage,
        0.25
    );
}

#[test]
fn capabilities_reject_events_owned_by_the_other_runtime_shape() {
    assert!(provider_translator()
        .translate(AgentEvent::ContextUsageUpdated {
            used_tokens: 1,
            context_size: 2,
        })
        .is_err());
    assert!(session_translator()
        .translate(AgentEvent::UsageUpdated {
            usage: Usage::default(),
        })
        .is_err());
}
