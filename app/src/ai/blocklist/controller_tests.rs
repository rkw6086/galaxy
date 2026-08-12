use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use ai::agent::action::{AskUserQuestionItem, AskUserQuestionType};
use chrono::Local;
use uuid::Uuid;
use warp_multi_agent_api::response_event;
use warpui::{App, EntityId, SingletonEntity};

use crate::ai::agent::conversation::{AIConversationId, ConversationStatus};
use crate::ai::agent::task::TaskId;
use crate::ai::agent::{
    AIAgentAction, AIAgentActionId, AIAgentActionType, AIAgentAttachment, AIAgentContext,
    AIAgentInput, CancellationReason, ImageContext, PassiveSuggestionTrigger, RunningCommand,
    UserQueryMode,
};
use crate::ai::ambient_agents::AmbientAgentTaskId;
use crate::ai::blocklist::{
    BlocklistAIHistoryEvent, BlocklistAIHistoryModel, PendingAttachment, PendingFile, RequestInput,
    ResponseStream, ResponseStreamId,
};
use crate::ai::llms::LLMId;
use crate::persistence::model::{AcpConversationData, AgentBackend};
use crate::terminal::model::block::BlockId;
use crate::test_util::settings::initialize_history_persistence_for_tests;
use crate::test_util::terminal::{add_window_with_terminal, initialize_app_for_terminal_view};

fn new_ambient_agent_task_id() -> AmbientAgentTaskId {
    Uuid::new_v4().to_string().parse().unwrap()
}

fn ask_user_question_action(action_id: &str) -> AIAgentAction {
    AIAgentAction {
        id: AIAgentActionId::from(action_id.to_string()),
        task_id: TaskId::new(format!("task-{action_id}")),
        action: AIAgentActionType::AskUserQuestion {
            questions: vec![AskUserQuestionItem {
                question_id: "q1".to_owned(),
                question: "Which path should the agent take?".to_owned(),
                question_type: AskUserQuestionType::MultipleChoice {
                    is_multiselect: false,
                    options: vec![],
                    supports_other: true,
                },
            }],
        },
        requires_result: true,
        tool_name: Some("ask_user_question".to_owned()),
    }
}

fn image_attachment(file_name: &str) -> PendingAttachment {
    PendingAttachment::Image(ImageContext {
        data: String::new(),
        mime_type: "image/png".to_owned(),
        file_name: file_name.to_owned(),
        is_figma: false,
    })
}

fn file_attachment(file_name: &str) -> PendingAttachment {
    PendingAttachment::File(PendingFile {
        file_name: file_name.to_owned(),
        file_path: file_name.into(),
        mime_type: "text/plain".to_owned(),
    })
}

fn live_steering_eligibility() -> super::LiveSteeringEligibility {
    super::LiveSteeringEligibility {
        is_user_initiated: true,
        has_shared_session_participant: false,
        is_queued_prompt: false,
        has_queued_query_id: false,
        has_additional_attachments: false,
        is_existing_task: true,
        is_active_conversation: true,
        has_plain_user_input: true,
        has_pending_context: false,
        has_action_context: false,
        has_pending_passive_results: false,
    }
}

#[test]
fn no_action_tool_error_recovery_detects_unfulfilled_tool_intent() {
    assert_eq!(
        super::no_action_tool_error_recovery_reason(
            true,
            "Let me recall earlier in the StateManager class: what I read:",
        ),
        Some("unfulfilled_tool_intent")
    );
    assert_eq!(
        super::no_action_tool_error_recovery_reason(
            true,
            "Now let me look at how manifests are currently stored and served:\n\
             Now let me check what writes them:",
        ),
        Some("unfulfilled_tool_intent")
    );
}

#[test]
fn no_action_tool_error_recovery_ignores_normal_answers_and_non_failed_tools() {
    assert_eq!(
        super::no_action_tool_error_recovery_reason(
            true,
            "The grep timed out, so I could not verify the file contents. Based on the \
             loaded manifest code, the likely fix is to narrow the search and update the \
             config watcher.",
        ),
        None
    );
    assert_eq!(
        super::no_action_tool_error_recovery_reason(
            false,
            "Let me look at the config watcher implementation:",
        ),
        None
    );
}

#[test]
fn tool_queue_decision_blocks_failed_tool_proposal_before_snapshot_fallback() {
    assert_eq!(
        super::tool_queue_decision(false, false, true, false, 1, 1,),
        super::ToolQueueDecision::BlockedFailedToolProposal
    );
}

#[test]
fn tool_queue_decision_blocks_parent_tools_while_child_agents_are_active() {
    assert_eq!(
        super::tool_queue_decision(false, false, false, true, 2, 0,),
        super::ToolQueueDecision::BlockedActiveChildAgents
    );
}

#[test]
fn tool_queue_decision_preserves_existing_terminal_precedence() {
    assert_eq!(
        super::tool_queue_decision(true, false, true, true, 1, 1,),
        super::ToolQueueDecision::Cancelled
    );
    assert_eq!(
        super::tool_queue_decision(false, true, true, true, 1, 1,),
        super::ToolQueueDecision::UnfinishedExchange
    );
}

#[test]
fn tool_queue_decision_uses_snapshot_fallback_only_when_unblocked() {
    let decision = super::tool_queue_decision(false, false, false, false, 1, 1);

    assert_eq!(
        decision,
        super::ToolQueueDecision::QueueActionsWithStreamSnapshotFallback
    );
    assert!(decision.will_queue_actions());
}

#[test]
fn query_targets_existing_conversation_extracts_existing_task_id() {
    let conversation_id = AIConversationId::new();
    let task_id = TaskId::new("task".to_owned());

    assert_eq!(
        super::query_targets_existing_conversation(&super::InputQuery {
            which_task: super::WhichTask::Task {
                conversation_id,
                task_id,
            },
            input_query: super::InputQueryType::UserSubmittedQueryFromInput {
                query: "Continue".to_owned(),
                static_query_type: None,
                running_command: None,
            },
            additional_attachments: HashMap::new(),
            queued_query_id: None,
        }),
        Some(conversation_id)
    );
    assert_eq!(
        super::query_targets_existing_conversation(&super::InputQuery {
            which_task: super::WhichTask::NewConversation,
            input_query: super::InputQueryType::UserSubmittedQueryFromInput {
                query: "new task".to_owned(),
                static_query_type: None,
                running_command: None,
            },
            additional_attachments: HashMap::new(),
            queued_query_id: None,
        }),
        None
    );
}

#[test]
fn active_descendant_conversation_ids_filters_done_children() {
    App::test((), |mut app| async move {
        initialize_history_persistence_for_tests(&mut app);
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());

        let orchestrator_id = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_conversation(terminal_view_id, false, false, false, ctx)
        });
        let child_id = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_child_conversation(
                terminal_view_id,
                "manifest-owner".to_string(),
                orchestrator_id,
                None,
                ctx,
            )
        });

        history_model.read(&app, |history_model, _| {
            assert_eq!(
                super::active_descendant_conversation_ids(history_model, orchestrator_id),
                vec![child_id]
            );
        });

        history_model.update(&mut app, |history_model, ctx| {
            history_model.update_conversation_status(
                terminal_view_id,
                child_id,
                ConversationStatus::Success,
                ctx,
            );
        });

        history_model.read(&app, |history_model, _| {
            assert_eq!(
                super::active_descendant_conversation_ids(history_model, orchestrator_id),
                Vec::<AIConversationId>::new()
            );
        });
    });
}

#[test]
fn acp_backend_model_identity_does_not_claim_a_provider_model() {
    assert_eq!(super::acp_backend_model_id(&AgentBackend::Provider), None);
    assert_eq!(
        super::acp_backend_model_id(&AgentBackend::Acp(AcpConversationData {
            provider_id: String::new(),
            agent_id: " Codex ".to_owned(),
            launch_fingerprint: "launch-123".to_owned(),
            session_id: None,
            config_values: std::collections::BTreeMap::from([(
                "model".to_owned(),
                serde_json::json!("fast"),
            )]),
        })),
        Some(LLMId::from("acp:codex:model=\"fast\""))
    );
    assert_eq!(
        super::acp_backend_model_id(&AgentBackend::Acp(AcpConversationData {
            provider_id: "work".to_owned(),
            agent_id: " Codex ".to_owned(),
            launch_fingerprint: "launch-123".to_owned(),
            session_id: None,
            config_values: std::collections::BTreeMap::from([(
                "model".to_owned(),
                serde_json::json!("fast"),
            )]),
        })),
        Some(LLMId::from("acp:work:codex:model=\"fast\""))
    );
}

#[test]
fn live_steering_accepts_plain_input_for_the_existing_command_monitor() {
    let input = super::InputQueryType::UserSubmittedQueryFromInput {
        query: "Stop the command now.".to_owned(),
        static_query_type: None,
        running_command: Some(RunningCommand {
            command: "script/soak-test".to_owned(),
            block_id: BlockId::new(),
            grid_contents: "elapsed: 75s".to_owned(),
            cursor: String::new(),
            requested_command_id: None,
            is_alt_screen_active: false,
        }),
    };

    assert!(!super::is_plain_live_steering_input(&input, false));
    assert!(super::is_plain_live_steering_input(&input, true));
}

#[test]
fn running_command_monitor_identity_requires_the_same_conversation_and_block() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);
        let conversation_id = AIConversationId::new();

        terminal.update(&mut app, |terminal, _ctx| {
            let mut terminal_model = terminal.model.lock();
            terminal_model.simulate_long_running_block("sleep 100", "running");
            let task_id = TaskId::new("monitor-task".to_owned());
            let active_block = terminal_model.block_list_mut().active_block_mut();
            active_block.set_is_agent_tagged_in(true);
            active_block
                .set_agent_interaction_mode_for_agent_monitored_command(&task_id, conversation_id)
                .expect("tagged command should transition to agent monitoring");

            let running_command = super::running_command_snapshot(&terminal_model);
            assert!(super::running_command_belongs_to_monitor(
                &terminal_model,
                conversation_id,
                &running_command,
            ));
            assert!(!super::running_command_belongs_to_monitor(
                &terminal_model,
                AIConversationId::new(),
                &running_command,
            ));

            let mut other_block = running_command;
            other_block.block_id = BlockId::new();
            assert!(!super::running_command_belongs_to_monitor(
                &terminal_model,
                conversation_id,
                &other_block,
            ));
        });
    });
}

#[test]
fn live_steering_retains_attachment_context_and_action_guards() {
    let eligible = live_steering_eligibility();
    assert!(eligible.can_attempt());

    assert!(!super::LiveSteeringEligibility {
        has_additional_attachments: true,
        ..eligible
    }
    .can_attempt());
    assert!(!super::LiveSteeringEligibility {
        has_pending_context: true,
        ..eligible
    }
    .can_attempt());
    assert!(!super::LiveSteeringEligibility {
        has_action_context: true,
        ..eligible
    }
    .can_attempt());
}

#[test]
fn passive_suggestions_request_params_omit_ambient_agent_task_id() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);

        terminal.update(&mut app, |terminal, ctx| {
            let task_id = new_ambient_agent_task_id();
            let conversation_id =
                BlocklistAIHistoryModel::handle(ctx).update(ctx, |history_model, ctx| {
                    history_model.start_new_conversation(terminal.id(), false, false, false, ctx)
                });

            terminal.ai_controller().update(ctx, |controller, ctx| {
                controller.set_ambient_agent_task_id(Some(task_id), ctx);

                assert_eq!(controller.get_ambient_agent_task_id(), Some(task_id));
                assert_eq!(
                    controller
                        .build_passive_suggestions_request_params(
                            Some(conversation_id),
                            PassiveSuggestionTrigger::FilesChanged,
                            vec![],
                            ctx,
                        )
                        .expect("existing conversation should build passive suggestion params")
                        .1
                        .ambient_agent_task_id,
                    None
                );
                assert_eq!(
                    controller
                        .build_passive_suggestions_request_params(
                            None,
                            PassiveSuggestionTrigger::FilesChanged,
                            vec![],
                            ctx,
                        )
                        .expect("new conversation should build passive suggestion params")
                        .1
                        .ambient_agent_task_id,
                    None
                );
            });
        });
    });
}

#[test]
fn input_for_query_converts_prompt_attachments_and_ignores_live_staging() {
    // `input_for_query` builds its image/file context purely from the explicitly-provided
    // attachment set (resolved by `send_query` from either the queued row or live staging),
    // never from the context model's pending attachments.
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);

        terminal.update(&mut app, |terminal, ctx| {
            let conversation_id =
                BlocklistAIHistoryModel::handle(ctx).update(ctx, |history_model, ctx| {
                    history_model.start_new_conversation(terminal.id(), false, false, false, ctx)
                });

            let controller = terminal.ai_controller();
            let context_model = controller.as_ref(ctx).context_model.clone();
            let active_session = controller.as_ref(ctx).active_session.clone();

            // Stage *live* attachments that must NOT leak into a query built from a different,
            // explicitly-provided attachment set.
            context_model.update(ctx, |m, ctx| {
                m.append_pending_attachments(
                    vec![image_attachment("live.png"), file_attachment("live.txt")],
                    ctx,
                );
            });

            let task_id = TaskId::new("test-task".to_owned());
            // Two files sharing a basename to exercise duplicate-basename suffixing.
            let prompt_attachments = vec![
                image_attachment("queued.png"),
                file_attachment("notes.txt"),
                file_attachment("notes.txt"),
            ];

            let input = super::input_for_query(
                "build a query".to_owned(),
                &task_id,
                conversation_id,
                None,
                UserQueryMode::Normal,
                None,
                HashMap::new(),
                prompt_attachments,
                context_model.as_ref(ctx),
                active_session.as_ref(ctx),
                ctx,
            );

            let AIAgentInput::UserQuery {
                context,
                referenced_attachments,
                ..
            } = input
            else {
                panic!("expected UserQuery");
            };

            // The provided image is attached as image context; the live-staged image is not.
            let image_names: Vec<&str> = context
                .iter()
                .filter_map(|c| match c {
                    AIAgentContext::Image(img) => Some(img.file_name.as_str()),
                    _ => None,
                })
                .collect();
            assert_eq!(image_names, vec!["queued.png"]);

            // The provided files are attached as FilePathReference with duplicate-basename
            // suffixing; the live-staged file is not.
            let mut file_names: Vec<String> = referenced_attachments
                .values()
                .filter_map(|a| match a {
                    AIAgentAttachment::FilePathReference { file_name, .. } => {
                        Some(file_name.clone())
                    }
                    _ => None,
                })
                .collect();
            file_names.sort();
            assert_eq!(
                file_names,
                vec!["notes.txt".to_owned(), "notes.txt".to_owned()]
            );
            assert!(referenced_attachments.contains_key("notes.txt"));
            assert!(referenced_attachments.contains_key("notes.txt (1)"));
            assert!(!referenced_attachments.contains_key("live.txt"));
        });
    });
}

#[test]
fn cancelling_conversation_aborts_pending_auto_resume() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);

        // An ID with no backing conversation: if the scheduled wait ever
        // completes, the resume is a harmless no-op.
        let conversation_id = AIConversationId::new();

        terminal.update(&mut app, |terminal, ctx| {
            terminal.ai_controller().update(ctx, |controller, ctx| {
                controller.schedule_auto_resume_after_error(conversation_id, ctx);
                assert!(controller
                    .pending_auto_resume_handles
                    .contains_key(&conversation_id));

                controller.cancel_conversation_progress(
                    conversation_id,
                    CancellationReason::ManuallyCancelled,
                    ctx,
                );
                assert!(!controller
                    .pending_auto_resume_handles
                    .contains_key(&conversation_id));
            });
        });
    });
}

#[test]
fn user_follow_up_does_not_cancel_unresolved_ask_user_question() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);

        let sent_request_count = Arc::new(Mutex::new(0));
        let controller = terminal.read(&app, |terminal, _| terminal.ai_controller().clone());
        let sent_request_count_for_subscription = Arc::clone(&sent_request_count);
        app.update(|ctx| {
            ctx.subscribe_to_model(&controller, move |_, event, _| {
                if matches!(event, super::BlocklistAIControllerEvent::SentRequest { .. }) {
                    *sent_request_count_for_subscription.lock().unwrap() += 1;
                }
            });
        });

        let conversation_id = terminal.update(&mut app, |terminal, ctx| {
            let terminal_surface_id = terminal.id();
            let conversation_id =
                BlocklistAIHistoryModel::handle(ctx).update(ctx, |history_model, ctx| {
                    let conversation_id = history_model.start_new_conversation(
                        terminal_surface_id,
                        false,
                        false,
                        false,
                        ctx,
                    );
                    history_model.mark_active_conversation_id(
                        conversation_id,
                        terminal_surface_id,
                        ctx,
                    );
                    history_model.update_conversation_status(
                        terminal_surface_id,
                        conversation_id,
                        ConversationStatus::Blocked {
                            blocked_action: "ask_user_question".to_owned(),
                        },
                        ctx,
                    );
                    conversation_id
                });

            terminal.ai_controller().update(ctx, |controller, ctx| {
                controller.action_model.update(ctx, |action_model, _| {
                    action_model.push_pending_action_for_test(
                        conversation_id,
                        ask_user_question_action("ask-1"),
                    );
                });
            });

            conversation_id
        });

        terminal.update(&mut app, |terminal, ctx| {
            terminal.ai_controller().update(ctx, |controller, ctx| {
                controller.send_user_query_in_conversation(
                    "Continue".to_owned(),
                    conversation_id,
                    None,
                    ctx,
                );
            });
        });

        assert_eq!(*sent_request_count.lock().unwrap(), 0);
        controller.read(&app, |controller, ctx| {
            assert!(controller
                .action_model
                .as_ref(ctx)
                .has_unresolved_ask_user_question_for_conversation(conversation_id, ctx));
        });
    });
}

#[test]
fn new_conversation_submission_does_not_cancel_active_unresolved_ask_user_question() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);

        let (conversation_id, initial_conversation_count) =
            terminal.update(&mut app, |terminal, ctx| {
                let terminal_surface_id = terminal.id();
                let conversation_id =
                    BlocklistAIHistoryModel::handle(ctx).update(ctx, |history_model, ctx| {
                        let conversation_id = history_model.start_new_conversation(
                            terminal_surface_id,
                            false,
                            false,
                            false,
                            ctx,
                        );
                        history_model.mark_active_conversation_id(
                            conversation_id,
                            terminal_surface_id,
                            ctx,
                        );
                        history_model.update_conversation_status(
                            terminal_surface_id,
                            conversation_id,
                            ConversationStatus::Blocked {
                                blocked_action: "ask_user_question".to_owned(),
                            },
                            ctx,
                        );
                        conversation_id
                    });

                terminal.ai_controller().update(ctx, |controller, ctx| {
                    controller.action_model.update(ctx, |action_model, _| {
                        action_model.push_pending_action_for_test(
                            conversation_id,
                            ask_user_question_action("ask-new-task"),
                        );
                    });
                });

                let initial_conversation_count = BlocklistAIHistoryModel::as_ref(ctx)
                    .all_live_conversations()
                    .len();
                (conversation_id, initial_conversation_count)
            });

        terminal.update(&mut app, |terminal, ctx| {
            terminal.ai_controller().update(ctx, |controller, ctx| {
                controller.send_user_query_in_new_conversation(
                    "Start another task".to_owned(),
                    None,
                    crate::ai::agent::EntrypointType::UserInitiated,
                    None,
                    ctx,
                );
            });
        });

        terminal.read(&app, |terminal, ctx| {
            let history_model = BlocklistAIHistoryModel::as_ref(ctx);
            assert_eq!(
                history_model.all_live_conversations().len(),
                initial_conversation_count
            );
            assert_eq!(
                history_model
                    .conversation(&conversation_id)
                    .map(|c| c.status()),
                Some(&ConversationStatus::Blocked {
                    blocked_action: "ask_user_question".to_owned()
                })
            );
            assert!(terminal
                .ai_controller()
                .as_ref(ctx)
                .action_model
                .as_ref(ctx)
                .has_unresolved_ask_user_question_for_conversation(conversation_id, ctx));
        });
    });
}

#[test]
fn mock_response_stream_updates_history_through_controller() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);
        let captured_events = Arc::new(Mutex::new(Vec::new()));
        let events_for_subscription = Arc::clone(&captured_events);
        app.update(|ctx| {
            ctx.subscribe_to_model(&BlocklistAIHistoryModel::handle(ctx), move |_, event, _| {
                events_for_subscription.lock().unwrap().push(event.clone())
            });
        });

        let (conversation_id, stream) = terminal.update(&mut app, |view, ctx| {
            let terminal_surface_id = view.id();
            let stream_id = ResponseStreamId::new_for_test();
            let conversation_id =
                BlocklistAIHistoryModel::handle(ctx).update(ctx, |history, ctx| {
                    let conversation_id = history.start_new_conversation(
                        terminal_surface_id,
                        false,
                        false,
                        false,
                        ctx,
                    );
                    let task_id = history
                        .conversation(&conversation_id)
                        .unwrap()
                        .get_root_task_id()
                        .clone();
                    history
                        .update_conversation_for_new_request_input(
                            RequestInput {
                                conversation_id,
                                input_messages: HashMap::from([(task_id, vec![])]),
                                working_directory: None,
                                model_id: LLMId::from("test-model"),
                                coding_model_id: LLMId::from("test-coding-model"),
                                cli_agent_model_id: LLMId::from("test-cli-agent-model"),
                                computer_use_model_id: LLMId::from("test-computer-use-model"),
                                shared_session_response_initiator: None,
                                request_start_ts: Local::now(),
                                supported_tools_override: None,
                            },
                            stream_id.clone(),
                            terminal_surface_id,
                            ctx,
                        )
                        .unwrap();
                    conversation_id
                });
            let stream = ctx.add_model(|_| ResponseStream::new_for_test(stream_id.clone()));
            view.ai_controller().update(ctx, |controller, ctx| {
                controller.register_mock_stream_for_test(
                    stream_id,
                    conversation_id,
                    stream.clone(),
                    ctx,
                );
            });
            (conversation_id, stream)
        });

        stream.update(&mut app, |stream, ctx| {
            stream.emit_response_event_for_test(
                warp_multi_agent_api::ResponseEvent {
                    r#type: Some(response_event::Type::Init(response_event::StreamInit {
                        request_id: "test-request".to_string(),
                        conversation_id: "test-server-conversation".to_string(),
                        run_id: String::new(),
                    })),
                },
                ctx,
            );
            stream.emit_response_event_for_test(
                warp_multi_agent_api::ResponseEvent {
                    r#type: Some(response_event::Type::Finished(
                        response_event::StreamFinished {
                            reason: Some(response_event::stream_finished::Reason::Done(
                                response_event::stream_finished::Done {},
                            )),
                            conversation_usage_metadata: None,
                            token_usage: vec![],
                            should_refresh_model_config: false,
                            request_cost: None,
                        },
                    )),
                },
                ctx,
            );
        });

        BlocklistAIHistoryModel::handle(&app).read(&app, |history, _| {
            assert_eq!(
                history.conversation(&conversation_id).map(|c| c.status()),
                Some(&crate::ai::agent::conversation::ConversationStatus::Success)
            );
        });
        let events = captured_events.lock().unwrap();
        assert!(events.iter().any(|event| matches!(
            event,
            BlocklistAIHistoryEvent::ConversationServerTokenAssigned {
                conversation_id: id,
                ..
            } if *id == conversation_id
        )));
        assert!(events.iter().any(|event| matches!(
            event,
            BlocklistAIHistoryEvent::UpdatedStreamingExchange {
                conversation_id: id,
                ..
            } if *id == conversation_id
        )));
    });
}

/// When an agent command exits the shell, the conversation must be finalized as
/// `Error` (not `Cancelled`), and a subsequent `ManuallyCancelled` (as fired by
/// the pane-close path) must not overwrite that failure.
#[test]
fn fail_conversation_due_to_shell_exit_reports_error_and_survives_manual_cancel() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);

        let conversation_id = terminal.update(&mut app, |view, ctx| {
            let terminal_surface_id = view.id();
            let stream_id = ResponseStreamId::new_for_test();
            let conversation_id =
                BlocklistAIHistoryModel::handle(ctx).update(ctx, |history, ctx| {
                    let conversation_id = history.start_new_conversation(
                        terminal_surface_id,
                        false,
                        false,
                        false,
                        ctx,
                    );
                    let task_id = history
                        .conversation(&conversation_id)
                        .unwrap()
                        .get_root_task_id()
                        .clone();
                    history
                        .update_conversation_for_new_request_input(
                            RequestInput {
                                conversation_id,
                                input_messages: HashMap::from([(task_id, vec![])]),
                                working_directory: None,
                                model_id: LLMId::from("test-model"),
                                coding_model_id: LLMId::from("test-coding-model"),
                                cli_agent_model_id: LLMId::from("test-cli-agent-model"),
                                computer_use_model_id: LLMId::from("test-computer-use-model"),
                                shared_session_response_initiator: None,
                                request_start_ts: Local::now(),
                                supported_tools_override: None,
                            },
                            stream_id.clone(),
                            terminal_surface_id,
                            ctx,
                        )
                        .unwrap();
                    conversation_id
                });
            let stream = ctx.add_model(|_| ResponseStream::new_for_test(stream_id.clone()));
            view.ai_controller().update(ctx, |controller, ctx| {
                controller.register_mock_stream_for_test(stream_id, conversation_id, stream, ctx);
                controller.fail_conversation_due_to_shell_exit(conversation_id, ctx);
            });
            conversation_id
        });

        // The in-flight request is finalized as Error (with the shell-exit error
        // on its exchange), not Cancelled.
        BlocklistAIHistoryModel::handle(&app).read(&app, |history, _| {
            assert_eq!(
                history.conversation(&conversation_id).map(|c| c.status()),
                Some(&crate::ai::agent::conversation::ConversationStatus::Error)
            );
        });

        // The pane-close cancellation path must be a no-op now that the
        // conversation is terminal.
        terminal.update(&mut app, |view, ctx| {
            view.ai_controller().update(ctx, |controller, ctx| {
                controller.cancel_conversation_progress(
                    conversation_id,
                    CancellationReason::ManuallyCancelled,
                    ctx,
                );
            });
        });
        BlocklistAIHistoryModel::handle(&app).read(&app, |history, _| {
            assert_eq!(
                history.conversation(&conversation_id).map(|c| c.status()),
                Some(&crate::ai::agent::conversation::ConversationStatus::Error)
            );
        });
    });
}

/// An optimistic long-running-command completion that cancels an in-flight
/// stream must finalize the conversation as `Success`, not `Cancelled`. This is
/// a regression test for the reason -> status mapping living in a single place
/// (`CancellationReason::conversation_outcome`).
#[test]
fn optimistic_cli_subagent_completion_with_in_flight_stream_reports_success() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);

        let conversation_id = terminal.update(&mut app, |view, ctx| {
            let terminal_surface_id = view.id();
            let stream_id = ResponseStreamId::new_for_test();
            let conversation_id =
                BlocklistAIHistoryModel::handle(ctx).update(ctx, |history, ctx| {
                    let conversation_id = history.start_new_conversation(
                        terminal_surface_id,
                        false,
                        false,
                        false,
                        ctx,
                    );
                    let task_id = history
                        .conversation(&conversation_id)
                        .unwrap()
                        .get_root_task_id()
                        .clone();
                    history
                        .update_conversation_for_new_request_input(
                            RequestInput {
                                conversation_id,
                                input_messages: HashMap::from([(task_id, vec![])]),
                                working_directory: None,
                                model_id: LLMId::from("test-model"),
                                coding_model_id: LLMId::from("test-coding-model"),
                                cli_agent_model_id: LLMId::from("test-cli-agent-model"),
                                computer_use_model_id: LLMId::from("test-computer-use-model"),
                                shared_session_response_initiator: None,
                                request_start_ts: Local::now(),
                                supported_tools_override: None,
                            },
                            stream_id.clone(),
                            terminal_surface_id,
                            ctx,
                        )
                        .unwrap();
                    conversation_id
                });
            let stream = ctx.add_model(|_| ResponseStream::new_for_test(stream_id.clone()));
            view.ai_controller().update(ctx, |controller, ctx| {
                controller.register_mock_stream_for_test(stream_id, conversation_id, stream, ctx);
                // The long-running command finished while the agent was still
                // streaming, cancelling the in-flight stream optimistically.
                controller.cancel_conversation_progress(
                    conversation_id,
                    CancellationReason::CommandFinishedDuringInlineAgentView,
                    ctx,
                );
            });
            conversation_id
        });

        BlocklistAIHistoryModel::handle(&app).read(&app, |history, _| {
            assert_eq!(
                history.conversation(&conversation_id).map(|c| c.status()),
                Some(&crate::ai::agent::conversation::ConversationStatus::Success)
            );
        });
    });
}
