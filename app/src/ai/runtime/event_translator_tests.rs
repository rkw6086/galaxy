use galaxy_agent_core::{
    AgentEvent, RuntimeActivity, RuntimeActivityStatus, RuntimeCapabilities, StopReason, Usage,
};
use warp_multi_agent_api::{client_action, message, response_event};

use super::{ProviderRunResponseProjector, RuntimeResponseConfig, RuntimeResponseTranslator};
use crate::ai::agent::runtime_activity;
use crate::ai::runtime::provider_run_coordinator::ProviderRunProjection;

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
fn restored_provider_projection_skips_stream_initialization() {
    let config = RuntimeResponseConfig {
        task_id: "task".to_owned(),
        conversation_id: "conversation".to_owned(),
        needs_create_task: true,
        user_query: Some("do not duplicate".to_owned()),
        model_id: "model".to_owned(),
        max_context_tokens: Some(1_000),
        capabilities: RuntimeCapabilities::provider(),
        empty_output_message: None,
    };
    let mut projector = ProviderRunResponseProjector::restored(config, true);
    let work_id = galaxy_agent_core::ExternalWorkId {
        run_id: galaxy_agent_core::ProviderRunId::new("run"),
        epoch: galaxy_agent_core::RunEpoch::new(2),
    };

    assert!(projector
        .project(ProviderRunProjection::ModelTurnStarted {
            work_id: work_id.clone(),
            profile: galaxy_agent_core::ProviderRequestProfile::new("base"),
            runtime_id: "runtime".to_owned(),
            model_id: "model".to_owned(),
            runtime_request_id: "request".to_owned(),
            retry_attempt: 0,
            elapsed_ms: 1,
        })
        .unwrap()
        .is_empty());
    let events = projector
        .project(ProviderRunProjection::ModelEvent {
            work_id,
            event: AgentEvent::TextDelta {
                text: "continued".to_owned(),
            },
        })
        .unwrap();

    assert_eq!(events.len(), 1);
    let Some(response_event::Type::ClientActions(actions)) = &events[0].r#type else {
        panic!("restored output should append through a client action");
    };
    assert!(matches!(
        actions.actions[0].action,
        Some(client_action::Action::AddMessagesToTask(_))
    ));
}

#[test]
fn restored_task_progress_replay_does_not_duplicate_the_persisted_checklist() {
    use galaxy_agent_core::{TaskListItem, TaskListSnapshot, TaskListStatus};
    let snapshot = TaskListSnapshot { tasks: vec![
        TaskListItem { id: "inspect".to_owned(), title: "Inspect".to_owned(), status: TaskListStatus::Completed },
        TaskListItem { id: "verify".to_owned(), title: "Verify".to_owned(), status: TaskListStatus::InProgress },
    ] };
    let mut projector = ProviderRunResponseProjector::restored(RuntimeResponseConfig {
        task_id: "task".to_owned(), conversation_id: "conversation".to_owned(), needs_create_task: false,
        user_query: None, model_id: "model".to_owned(), max_context_tokens: Some(1000),
        capabilities: RuntimeCapabilities::provider(), empty_output_message: None,
    }, true).with_task_list(Some(snapshot.clone()));
    let batch = galaxy_agent_core::PendingToolBatch {
        work_id: galaxy_agent_core::ExternalWorkId {
            run_id: galaxy_agent_core::ProviderRunId::new("run"), epoch: galaxy_agent_core::RunEpoch::new(1),
        },
        calls: vec![galaxy_agent_core::PendingToolCall {
            call: galaxy_agent_core::ToolCall {
                id: "progress".to_owned(), name: galaxy_agent_core::UPDATE_TASK_LIST_NAME.to_owned(),
                arguments: serde_json::to_value(&snapshot).unwrap(),
            },
            state: galaxy_agent_core::PendingToolCallState::Resolved { result: galaxy_agent_core::ToolResult {
                images: Vec::new(),
                call_id: "progress".to_owned(), status: galaxy_agent_core::ToolResultStatus::Success,
                content: snapshot.to_tool_result_content(),
            }},
        }],
    };
    assert!(projector.project(ProviderRunProjection::ToolBatchReady { batch }).unwrap().is_empty());
}

#[test]
fn restored_committed_task_snapshot_repairs_missing_projection_even_on_terminal_cleanup() {
    use galaxy_agent_core::{TaskListItem, TaskListSnapshot, TaskListStatus};
    let snapshot = TaskListSnapshot { tasks: vec![TaskListItem {
        id: "verify".to_owned(), title: "Verify".to_owned(), status: TaskListStatus::Completed,
    }] };
    let mut projector = ProviderRunResponseProjector::restored(RuntimeResponseConfig {
        task_id: "task".to_owned(), conversation_id: "conversation".to_owned(), needs_create_task: false,
        user_query: None, model_id: "model".to_owned(), max_context_tokens: Some(1000),
        capabilities: RuntimeCapabilities::provider(), empty_output_message: None,
    }, true).with_restored_task_lists(None, Some(snapshot));
    let events = projector.finish(&galaxy_agent_core::ProviderRunOutcome::Cancelled {
        reason: "restored cancellation".to_owned(),
    }, &Usage::default()).unwrap();
    let operations = events.into_iter().filter_map(|event| {
        let Some(response_event::Type::ClientActions(actions)) = event.r#type else { return None };
        Some(actions.actions.into_iter().filter_map(|action| {
            let Some(client_action::Action::AddMessagesToTask(add)) = action.action else { return None };
            Some(add.messages.into_iter().filter_map(|message| {
                let Some(message::Message::UpdateTodos(update)) = message.message else { return None };
                update.operation
            }).collect::<Vec<_>>())
        }).flatten().collect::<Vec<_>>())
    }).flatten().collect::<Vec<_>>();
    assert!(matches!(&operations[0], message::update_todos::Operation::CreateTodoList(_)));
    assert!(matches!(&operations[1], message::update_todos::Operation::MarkTodosCompleted(mark) if mark.todo_ids == ["verify"]));
}

#[test]
fn provider_activity_does_not_create_or_complete_task_lists() {
    let mut projector = ProviderRunResponseProjector::new(RuntimeResponseConfig {
        task_id: "task".to_owned(),
        conversation_id: "conversation".to_owned(),
        needs_create_task: false,
        user_query: None,
        model_id: "model".to_owned(),
        max_context_tokens: Some(1_000),
        capabilities: RuntimeCapabilities::provider(),
        empty_output_message: None,
    });
    let work_id = galaxy_agent_core::ExternalWorkId {
        run_id: galaxy_agent_core::ProviderRunId::new("run"),
        epoch: galaxy_agent_core::RunEpoch::new(1),
    };
    let mut events = projector
        .project(ProviderRunProjection::ModelTurnStarted {
            work_id: work_id.clone(),
            profile: galaxy_agent_core::ProviderRequestProfile::new("base"),
            runtime_id: "runtime".to_owned(),
            model_id: "model".to_owned(),
            runtime_request_id: "request".to_owned(),
            retry_attempt: 0,
            elapsed_ms: 1,
        })
        .unwrap();
    assert_eq!(events.len(), 1);
    let tool_events = projector
        .project(ProviderRunProjection::ToolBatchReady {
            batch: galaxy_agent_core::PendingToolBatch {
                work_id,
                calls: Vec::new(),
            },
        })
        .unwrap();
    assert!(tool_events.is_empty());
    events.extend(
        projector
            .finish(
                &galaxy_agent_core::ProviderRunOutcome::Completed(
                    galaxy_agent_core::ProviderRunCompletion {
                        stop_reason: StopReason::Completed,
                    },
                ),
                &Usage::default(),
            )
            .unwrap(),
    );
    for event in events {
        if let Some(response_event::Type::ClientActions(actions)) = event.r#type {
            for action in actions.actions {
                if let Some(client_action::Action::AddMessagesToTask(add)) = action.action {
                    assert!(add.messages.iter().all(|message| !matches!(
                        message.message,
                        Some(message::Message::UpdateTodos(_))
                    )));
                }
            }
        }
    }
}

#[test]
fn restored_uninitialized_projection_replays_init_before_live_delta() {
    let config = RuntimeResponseConfig {
        task_id: "task".to_owned(),
        conversation_id: "conversation".to_owned(),
        needs_create_task: true,
        user_query: Some("already persisted".to_owned()),
        model_id: "model".to_owned(),
        max_context_tokens: Some(1_000),
        capabilities: RuntimeCapabilities::provider(),
        empty_output_message: None,
    };
    let mut projector = ProviderRunResponseProjector::restored(config, false);
    let work_id = galaxy_agent_core::ExternalWorkId {
        run_id: galaxy_agent_core::ProviderRunId::new("run"),
        epoch: galaxy_agent_core::RunEpoch::new(2),
    };

    let started = projector
        .project(ProviderRunProjection::ModelTurnStarted {
            work_id: work_id.clone(),
            profile: galaxy_agent_core::ProviderRequestProfile::new("base"),
            runtime_id: "runtime".to_owned(),
            model_id: "model".to_owned(),
            runtime_request_id: "request".to_owned(),
            retry_attempt: 0,
            elapsed_ms: 1,
        })
        .unwrap();
    let delta = projector
        .project(ProviderRunProjection::ModelEvent {
            work_id,
            event: AgentEvent::TextDelta {
                text: "continued".to_owned(),
            },
        })
        .unwrap();

    assert_eq!(started.len(), 1);
    assert!(matches!(
        started[0].r#type,
        Some(response_event::Type::Init(_))
    ));
    assert_eq!(delta.len(), 1);
    assert!(matches!(
        delta[0].r#type,
        Some(response_event::Type::ClientActions(_))
    ));
}

#[test]
fn provider_followup_turn_starts_a_distinct_text_message() {
    let mut projector = ProviderRunResponseProjector::new(RuntimeResponseConfig {
        task_id: "task".to_owned(),
        conversation_id: "conversation".to_owned(),
        needs_create_task: false,
        user_query: None,
        model_id: "model".to_owned(),
        max_context_tokens: Some(1_000),
        capabilities: RuntimeCapabilities::provider(),
        empty_output_message: None,
    });
    let first_work_id = galaxy_agent_core::ExternalWorkId {
        run_id: galaxy_agent_core::ProviderRunId::new("run"),
        epoch: galaxy_agent_core::RunEpoch::new(1),
    };
    projector
        .project(ProviderRunProjection::ModelTurnStarted {
            work_id: first_work_id.clone(),
            profile: galaxy_agent_core::ProviderRequestProfile::new("base"),
            runtime_id: "runtime".to_owned(),
            model_id: "model".to_owned(),
            runtime_request_id: "request-1".to_owned(),
            retry_attempt: 0,
            elapsed_ms: 1,
        })
        .unwrap();
    let first_text = projector
        .project(ProviderRunProjection::ModelEvent {
            work_id: first_work_id,
            event: AgentEvent::TextDelta {
                text: "before tool".to_owned(),
            },
        })
        .unwrap();
    let Some(response_event::Type::ClientActions(first_actions)) = &first_text[0].r#type else {
        panic!("expected first text action");
    };
    let Some(client_action::Action::AddMessagesToTask(first_add)) =
        &first_actions.actions[0].action
    else {
        panic!("first turn should add a text message");
    };
    let first_message_id = first_add.messages[0].id.clone();

    let second_work_id = galaxy_agent_core::ExternalWorkId {
        run_id: galaxy_agent_core::ProviderRunId::new("run"),
        epoch: galaxy_agent_core::RunEpoch::new(2),
    };
    projector
        .project(ProviderRunProjection::ModelTurnStarted {
            work_id: second_work_id.clone(),
            profile: galaxy_agent_core::ProviderRequestProfile::new("base"),
            runtime_id: "runtime".to_owned(),
            model_id: "model".to_owned(),
            runtime_request_id: "request-2".to_owned(),
            retry_attempt: 0,
            elapsed_ms: 1,
        })
        .unwrap();
    let second_text = projector
        .project(ProviderRunProjection::ModelEvent {
            work_id: second_work_id,
            event: AgentEvent::TextDelta {
                text: "after tool".to_owned(),
            },
        })
        .unwrap();
    let Some(response_event::Type::ClientActions(second_actions)) = &second_text[0].r#type else {
        panic!("expected follow-up text action");
    };
    let Some(client_action::Action::AddMessagesToTask(second_add)) =
        &second_actions.actions[0].action
    else {
        panic!("follow-up turn should add a text message");
    };

    assert_ne!(first_message_id, second_add.messages[0].id);
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
fn retargeting_starts_new_text_and_reasoning_messages_on_the_new_task() {
    let mut translator = provider_translator();
    translator
        .translate(AgentEvent::TextDelta {
            text: "root text".to_owned(),
        })
        .expect("root text");
    translator
        .translate(AgentEvent::ReasoningDelta {
            text: "root reasoning".to_owned(),
        })
        .expect("root reasoning");

    translator.set_task_id("cli-task");
    for event in [
        AgentEvent::TextDelta {
            text: "cli text".to_owned(),
        },
        AgentEvent::ReasoningDelta {
            text: "cli reasoning".to_owned(),
        },
    ] {
        let translated = translator.translate(event).expect("retargeted output");
        let Some(response_event::Type::ClientActions(actions)) = &translated[0].r#type else {
            panic!("expected retargeted client action");
        };
        let Some(client_action::Action::AddMessagesToTask(add)) = &actions.actions[0].action else {
            panic!("retargeted output must start a new message");
        };
        assert_eq!(add.task_id, "cli-task");
        assert_eq!(add.messages[0].task_id, "cli-task");
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
fn reasoning_completed_authoritatively_replaces_streamed_reasoning() {
    let mut translator = provider_translator();
    let streamed = translator
        .translate(AgentEvent::ReasoningDelta {
            text: "draft reasoning".to_owned(),
        })
        .expect("reasoning delta");
    let completed = translator
        .translate(AgentEvent::ReasoningCompleted {
            text: "authoritative reasoning".to_owned(),
            signature: Some("signature".to_owned()),
        })
        .expect("reasoning completion");

    let Some(response_event::Type::ClientActions(streamed_actions)) = &streamed[1].r#type else {
        panic!("expected streamed reasoning action");
    };
    let Some(client_action::Action::AddMessagesToTask(add)) = &streamed_actions.actions[0].action
    else {
        panic!("expected streamed reasoning message");
    };
    let message_id = add.messages[0].id.clone();
    let Some(response_event::Type::ClientActions(completed_actions)) = &completed[0].r#type else {
        panic!("expected completed reasoning action");
    };
    let Some(client_action::Action::UpdateTaskMessage(update)) =
        &completed_actions.actions[0].action
    else {
        panic!("completed reasoning must replace the streamed value");
    };
    let message = update.message.as_ref().expect("replacement message");
    assert_eq!(message.id, message_id);
    assert!(matches!(
        &message.message,
        Some(message::Message::AgentReasoning(reasoning))
            if reasoning.reasoning == "authoritative reasoning"
    ));
    assert_eq!(
        update.mask.as_ref().expect("replacement mask").paths,
        ["agent_reasoning.reasoning"]
    );
}

#[test]
fn provider_retry_clears_failed_attempt_output_before_new_messages() {
    let mut projector = ProviderRunResponseProjector::new(RuntimeResponseConfig {
        task_id: "task".to_owned(),
        conversation_id: "conversation".to_owned(),
        needs_create_task: false,
        user_query: None,
        model_id: "model".to_owned(),
        max_context_tokens: Some(1_000),
        capabilities: RuntimeCapabilities::provider(),
        empty_output_message: None,
    });
    let work_id = galaxy_agent_core::ExternalWorkId {
        run_id: galaxy_agent_core::ProviderRunId::new("run"),
        epoch: galaxy_agent_core::RunEpoch::new(1),
    };
    projector
        .project(ProviderRunProjection::ModelTurnStarted {
            work_id: work_id.clone(),
            profile: galaxy_agent_core::ProviderRequestProfile::new("base"),
            runtime_id: "runtime".to_owned(),
            model_id: "model".to_owned(),
            runtime_request_id: "request".to_owned(),
            retry_attempt: 0,
            elapsed_ms: 1,
        })
        .unwrap();
    for event in [
        AgentEvent::TextDelta {
            text: "failed text".to_owned(),
        },
        AgentEvent::ReasoningDelta {
            text: "failed reasoning".to_owned(),
        },
    ] {
        projector
            .project(ProviderRunProjection::ModelEvent {
                work_id: work_id.clone(),
                event,
            })
            .unwrap();
    }
    let retry = projector
        .project(ProviderRunProjection::ModelRetry {
            work_id,
            profile: galaxy_agent_core::ProviderRequestProfile::new("base"),
            runtime_id: "runtime".to_owned(),
            model_id: "model".to_owned(),
            retry_attempt: 1,
            max_retries: 3,
            elapsed_ms: 2,
            error: galaxy_agent_core::AgentError::new(
                galaxy_agent_core::AgentErrorKind::Transport,
                "retry",
            ),
        })
        .unwrap();

    assert_eq!(retry.len(), 2);
    for event in retry {
        let Some(response_event::Type::ClientActions(actions)) = event.r#type else {
            panic!("cleanup must use a client action");
        };
        let Some(client_action::Action::UpdateTaskMessage(update)) = &actions.actions[0].action
        else {
            panic!("cleanup must replace failed output");
        };
        let message = update.message.as_ref().expect("cleanup message");
        match &message.message {
            Some(message::Message::AgentOutput(output)) => assert!(output.text.is_empty()),
            Some(message::Message::AgentReasoning(reasoning)) => {
                assert!(reasoning.reasoning.is_empty())
            }
            _ => panic!("cleanup must target visible text or reasoning"),
        }
    }
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

#[test]
fn plan_tasks_require_multiple_explicit_checklist_items_in_a_task_section() {
    let batch = |content: &str| galaxy_agent_core::PendingToolBatch {
        work_id: galaxy_agent_core::ExternalWorkId {
            run_id: galaxy_agent_core::ProviderRunId::new("run"),
            epoch: galaxy_agent_core::RunEpoch::new(1),
        },
        calls: vec![galaxy_agent_core::PendingToolCall {
            call: galaxy_agent_core::ToolCall {
                id: "plan-call".to_string(),
                name: "create_plan".to_string(),
                arguments: serde_json::json!({"documents": [{"content": content}]}),
            },
            state: galaxy_agent_core::PendingToolCallState::Proposed,
        }],
    };
    for content in [
        "## Risks\n- [ ] Risk one\n- [ ] Risk two",
        "## Tasks\n- [ ] Update the parser",
        "## Tasks\n- Requirement one\n- Requirement two",
    ] {
        assert!(super::todos_from_plan_batch(&batch(content)).is_none());
    }
    let todos = super::todos_from_plan_batch(&batch(
        "## Tasks\n- [ ] Add retry correlation\n- [ ] Test stale callbacks\n## Risks\n- [ ] Slow shell startup",
    )).unwrap();
    assert_eq!(todos.len(), 2);
    assert_eq!(todos[0].title, "Add retry correlation");
    assert_eq!(todos[1].title, "Test stale callbacks");
    assert_ne!(todos[0].id, todos[1].id);
}

#[test]
fn authentication_failures_identify_the_requested_transport_before_stream_start() {
    for (runtime, expected) in [
        (
            "rig-openai-compatible",
            warp_multi_agent_api::LlmProvider::Openai,
        ),
        (
            "rig-chatgpt-subscription",
            warp_multi_agent_api::LlmProvider::Openai,
        ),
        ("rig-bedrock", warp_multi_agent_api::LlmProvider::AwsBedrock),
        (
            "rig-anthropic",
            warp_multi_agent_api::LlmProvider::Anthropic,
        ),
        ("rig-gemini", warp_multi_agent_api::LlmProvider::Google),
        ("rig-vertex-ai", warp_multi_agent_api::LlmProvider::Google),
        ("unrecognized", warp_multi_agent_api::LlmProvider::Unknown),
    ] {
        let mut projector = ProviderRunResponseProjector::new(provider_translator().config);
        // A previous Bedrock turn must not leak its identity into the next request.
        for runtime_id in [
            "rig-bedrock:old-model".to_owned(),
            format!("{runtime}:new-model"),
        ] {
            projector
                .project(ProviderRunProjection::ModelTurnRequested {
                    work_id: galaxy_agent_core::ExternalWorkId {
                        run_id: galaxy_agent_core::ProviderRunId::new("run"),
                        epoch: galaxy_agent_core::RunEpoch::new(1),
                    },
                    profile: galaxy_agent_core::ProviderRequestProfile::new("cli"),
                    runtime_id,
                    model_id: "new-model".to_owned(),
                    retry_attempt: 0,
                })
                .expect("requested turn projection");
        }
        let error = galaxy_agent_core::AgentError::new(
            galaxy_agent_core::AgentErrorKind::Authentication,
            "Unauthorized",
        );
        let events =
            projector
                .translator
                .provider_failure("Unauthorized", Some(&error), &Usage::default());
        let Some(response_event::Type::Finished(finished)) = &events.last().unwrap().r#type else {
            panic!("expected a finished event");
        };
        let Some(response_event::stream_finished::Reason::InvalidApiKey(details)) =
            &finished.reason
        else {
            panic!("expected authentication failure");
        };
        assert_eq!(details.provider, expected as i32, "{runtime}");
        assert_eq!(details.model_name, "new-model");
    }
}
