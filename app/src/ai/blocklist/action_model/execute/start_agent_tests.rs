use ai::agent::action_result::StartAgentVersion;
use galaxy_core::features::FeatureFlag;
use warpui::{App, Entity, EntityId, ModelHandle};

use super::*;
use crate::ai::agent::conversation::ConversationStatus;
use crate::ai::agent::task::TaskId;
use crate::ai::agent::{
    AIAgentAction, AIAgentActionId, AIAgentActionResultType, AIAgentActionType, RenderableAIError,
    StartAgentExecutionMode, StartAgentResult,
};
use crate::ai::blocklist::orchestration_event_streamer::OrchestrationEventStreamer;
use crate::ai::blocklist::BlocklistAIHistoryModel;
use crate::server::server_api::ServerApiProvider;
use crate::test_util::settings::initialize_history_persistence_for_tests;

const FIRST_REQUEST_ID: StartAgentRequestId = StartAgentRequestId::from_raw_for_test(0);

/// Stable placeholder run_id assigned to the parent conversation in tests
/// that exercise the server-backed Oz child path. Tests without this id
/// exercise the direct-provider local child path instead.
const PARENT_RUN_ID: &str = "00000000-0000-0000-0000-000000000001";

#[derive(Default)]
struct CapturedDirectProviderChildLinks(Vec<(AIAgentActionId, AIConversationId, AIConversationId)>);

impl Entity for CapturedDirectProviderChildLinks {
    type Event = ();
}

#[derive(Default)]
struct CapturedStartAgentPrompts(Vec<String>);

impl Entity for CapturedStartAgentPrompts {
    type Event = ();
}

#[derive(Default)]
struct CapturedRunAgentsChildLinks(
    Vec<(AIAgentActionId, String, AIConversationId, AIConversationId)>,
);

impl Entity for CapturedRunAgentsChildLinks {
    type Event = ();
}

fn capture_start_agent_prompts(
    app: &mut App,
    executor: &ModelHandle<StartAgentExecutor>,
) -> ModelHandle<CapturedStartAgentPrompts> {
    let captured = app.add_model(|_| CapturedStartAgentPrompts::default());
    captured.update(app, |_, ctx| {
        ctx.subscribe_to_model(executor, |captured, _, event, _ctx| {
            if let StartAgentExecutorEvent::CreateAgent(request) = event {
                captured.0.push(request.prompt.clone());
            }
        });
    });
    captured
}

fn capture_direct_provider_child_links(
    app: &mut App,
    executor: &ModelHandle<StartAgentExecutor>,
) -> ModelHandle<CapturedDirectProviderChildLinks> {
    let captured = app.add_model(|_| CapturedDirectProviderChildLinks::default());
    captured.update(app, |captured, ctx| {
        ctx.subscribe_to_model(executor, |captured, _, event, _ctx| {
            if let StartAgentExecutorEvent::DirectProviderChildConversationCreated {
                action_id,
                parent_conversation_id,
                child_conversation_id,
            } = event
            {
                captured.0.push((
                    action_id.clone(),
                    *parent_conversation_id,
                    *child_conversation_id,
                ));
            }
        });
    });
    captured
}

fn capture_run_agents_child_links(
    app: &mut App,
    executor: &ModelHandle<StartAgentExecutor>,
) -> ModelHandle<CapturedRunAgentsChildLinks> {
    let captured = app.add_model(|_| CapturedRunAgentsChildLinks::default());
    captured.update(app, |_, ctx| {
        ctx.subscribe_to_model(executor, |captured, _, event, _ctx| {
            if let StartAgentExecutorEvent::RunAgentsChildConversationCreated {
                action_id,
                agent_name,
                parent_conversation_id,
                child_conversation_id,
            } = event
            {
                captured.0.push((
                    action_id.clone(),
                    agent_name.clone(),
                    *parent_conversation_id,
                    *child_conversation_id,
                ));
            }
        });
    });
    captured
}

fn build_start_agent_action(
    version: StartAgentVersion,
    execution_mode: StartAgentExecutionMode,
) -> AIAgentAction {
    build_start_agent_action_with_prompt(version, execution_mode, "Investigate the failure")
}

fn build_start_agent_action_with_prompt(
    version: StartAgentVersion,
    execution_mode: StartAgentExecutionMode,
    prompt: &str,
) -> AIAgentAction {
    AIAgentAction {
        id: AIAgentActionId::from("start-agent-action".to_string()),
        action: AIAgentActionType::StartAgent {
            version,
            name: "Agent 1".to_string(),
            prompt: prompt.to_string(),
            execution_mode,
            lifecycle_subscription: None,
        },
        task_id: TaskId::new("start-agent-task".to_string()),
        requires_result: false,
        tool_name: None,
    }
}

#[test]
fn execute_wraps_child_prompt_with_leaf_worker_contract() {
    App::test((), |mut app| async move {
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        let executor = app.add_model(StartAgentExecutor::new);
        let captured = capture_start_agent_prompts(&mut app, &executor);
        let root_conversation_id = history_model.update(&mut app, |history, ctx| {
            history.start_new_conversation(terminal_view_id, false, false, false, ctx)
        });
        let action = build_start_agent_action(
            StartAgentVersion::V1,
            StartAgentExecutionMode::local_with_defaults(),
        );

        let execution = executor.update(&mut app, |executor, ctx| {
            executor
                .execute(
                    ExecuteActionInput {
                        action: &action,
                        conversation_id: root_conversation_id,
                    },
                    ctx,
                )
                .into()
        });
        assert!(matches!(execution, AnyActionExecution::Async { .. }));
        captured.read(&app, |captured, _ctx| {
            assert_eq!(captured.0.len(), 1);
            assert!(captured.0[0].contains("You are a leaf worker"));
            assert!(
                captured.0[0].contains("Do not launch, delegate to, or create additional agents")
            );
            assert!(captured.0[0].ends_with("Assigned task:\nInvestigate the failure"));
        });
    });
}

#[test]
fn execute_denies_start_agent_from_child_conversation() {
    App::test((), |mut app| async move {
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        let executor = app.add_model(StartAgentExecutor::new);
        let child_conversation_id = history_model.update(&mut app, |history, ctx| {
            let conversation_id =
                history.start_new_conversation(terminal_view_id, false, false, false, ctx);
            history
                .conversation_mut(&conversation_id)
                .expect("conversation should exist")
                .set_parent_agent_id("parent-agent".to_string());
            conversation_id
        });
        let action = build_start_agent_action(
            StartAgentVersion::V1,
            StartAgentExecutionMode::local_with_defaults(),
        );

        let execution = executor.update(&mut app, |executor, ctx| {
            executor
                .execute(
                    ExecuteActionInput {
                        action: &action,
                        conversation_id: child_conversation_id,
                    },
                    ctx,
                )
                .into()
        });

        assert!(matches!(
            execution,
            AnyActionExecution::Sync(AIAgentActionResultType::StartAgent(
                StartAgentResult::Error { error, .. }
            )) if error.contains("leaf workers")
        ));
        executor.read(&app, |executor, _ctx| {
            assert!(executor.pending.is_empty());
        });
    });
}

#[test]
fn dispatch_denies_child_conversation_defense_in_depth() {
    App::test((), |mut app| async move {
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        let executor = app.add_model(StartAgentExecutor::new);
        let child_conversation_id = history_model.update(&mut app, |history, ctx| {
            let conversation_id =
                history.start_new_conversation(terminal_view_id, false, false, false, ctx);
            history
                .conversation_mut(&conversation_id)
                .expect("conversation should exist")
                .set_parent_agent_id("parent-agent".to_string());
            conversation_id
        });

        let dispatch = executor.update(&mut app, |executor, ctx| {
            executor.dispatch(
                AIAgentActionId::from("run-agents-action".to_string()),
                "grandchild".to_string(),
                "Do more work".to_string(),
                StartAgentExecutionMode::local_with_defaults(),
                None,
                child_conversation_id,
                None,
                ctx,
            )
        });

        assert!(matches!(
            dispatch.receiver.try_recv(),
            Ok(StartAgentOutcome::Error(error)) if error.contains("leaf workers")
        ));
        executor.read(&app, |executor, _ctx| {
            assert!(executor.pending.is_empty());
        });
    });
}

#[test]
fn local_execution_waits_for_completion() {
    assert_eq!(
        wait_policy_for_execution_mode(&StartAgentExecutionMode::local_with_defaults()),
        StartAgentWaitPolicy::Completion
    );
}

#[test]
fn detach_dispatch_rejects_late_child_callback() {
    App::test((), |mut app| async move {
        initialize_history_persistence_for_tests(&mut app);
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        let executor = app.add_model(StartAgentExecutor::new);
        let parent_conversation_id = history_model.update(&mut app, |history, ctx| {
            history.start_new_conversation(terminal_view_id, false, false, false, ctx)
        });
        let dispatch = executor.update(&mut app, |executor, ctx| {
            executor.dispatch(
                AIAgentActionId::from("run-agents".to_string()),
                "child".to_string(),
                "work".to_string(),
                StartAgentExecutionMode::Remote {
                    environment_id: "environment".to_string(),
                    skill_references: Vec::new(),
                    model_id: "model".to_string(),
                    computer_use_enabled: false,
                    worker_host: String::new(),
                    harness_type: "oz".to_string(),
                    title: String::new(),
                    auth_secret_name: None,
                },
                None,
                parent_conversation_id,
                Some(PARENT_RUN_ID.to_string()),
                ctx,
            )
        });
        assert!(executor.update(&mut app, |executor, _| {
            executor.detach_dispatch(dispatch.request_id)
        }));

        let child_conversation_id = history_model.update(&mut app, |history, ctx| {
            history.start_new_child_conversation(
                terminal_view_id,
                "child".to_string(),
                parent_conversation_id,
                None,
                ctx,
            )
        });
        history_model.update(&mut app, |history, ctx| {
            history.record_new_conversation_request_complete(
                dispatch.request_id,
                child_conversation_id,
                ctx,
            );
        });

        executor.read(&app, |executor, _| assert!(executor.pending.is_empty()));
        assert!(dispatch.receiver.try_recv().is_err());
    });
}

#[test]
fn legacy_local_codex_command_prompt_normalizes_to_local_harness() {
    let (prompt, execution_mode) = normalize_legacy_local_child_harness_command(
        "codex --dangerously-bypass-approvals-and-sandbox 'Investigate the failure'".to_string(),
        StartAgentExecutionMode::local_with_defaults(),
    );

    assert_eq!(prompt, "Investigate the failure");
    assert_eq!(
        execution_mode,
        StartAgentExecutionMode::local_harness("codex".to_string())
    );
}

#[test]
fn execute_normalizes_legacy_local_codex_command_before_validation() {
    App::test((), |mut app| async move {
        let _local_codex = FeatureFlag::LocalClaudeCodexChildHarnesses.override_enabled(true);
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        let executor = app.add_model(StartAgentExecutor::new);
        let parent_conversation_id = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_conversation(terminal_view_id, false, false, false, ctx)
        });
        let action = build_start_agent_action_with_prompt(
            StartAgentVersion::V1,
            StartAgentExecutionMode::local_with_defaults(),
            "codex --dangerously-bypass-approvals-and-sandbox 'Investigate the failure'",
        );

        let execution = executor.update(&mut app, |executor, ctx| {
            let input = ExecuteActionInput {
                action: &action,
                conversation_id: parent_conversation_id,
            };
            let result: AnyActionExecution = executor.execute(input, ctx).into();
            result
        });

        let AnyActionExecution::Sync(result) = execution else {
            panic!("expected sync execution");
        };

        assert!(matches!(
            result,
            AIAgentActionResultType::StartAgent(StartAgentResult::Error { error, version })
                if error
                    == "Local harness child agents require the parent run_id to be available."
                    && version == StartAgentVersion::V1
        ));
    });
}

#[test]
fn execute_returns_error_when_child_startup_is_blocked_before_initialization() {
    App::test((), |mut app| async move {
        initialize_history_persistence_for_tests(&mut app);
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        let executor = app.add_model(StartAgentExecutor::new);
        let parent_conversation_id = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_conversation(terminal_view_id, false, false, false, ctx)
        });
        history_model.update(&mut app, |model, ctx| {
            model.assign_run_id_for_conversation(
                parent_conversation_id,
                PARENT_RUN_ID.to_string(),
                None,
                terminal_view_id,
                ctx,
            );
        });
        let action = build_start_agent_action(
            StartAgentVersion::V1,
            StartAgentExecutionMode::local_with_defaults(),
        );

        let execution = executor.update(&mut app, |executor, ctx| {
            let input = ExecuteActionInput {
                action: &action,
                conversation_id: parent_conversation_id,
            };
            let result: AnyActionExecution = executor.execute(input, ctx).into();
            result
        });

        let AnyActionExecution::Async {
            execute_future,
            on_complete,
        } = execution
        else {
            panic!("expected async execution");
        };

        let child_conversation_id = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_child_conversation(
                terminal_view_id,
                "Agent 1".to_string(),
                parent_conversation_id,
                None,
                ctx,
            )
        });

        history_model.update(&mut app, |model, ctx| {
            model.record_new_conversation_request_complete(
                FIRST_REQUEST_ID,
                child_conversation_id,
                ctx,
            );
        });

        executor.read(&app, |executor, _| {
            assert_eq!(
                executor
                    .pending
                    .values()
                    .find_map(|pending| pending.child_conversation_id),
                Some(child_conversation_id)
            );
        });

        history_model.update(&mut app, |history_model, ctx| {
            history_model.update_conversation_status(
                terminal_view_id,
                child_conversation_id,
                ConversationStatus::Blocked {
                    blocked_action:
                        "GitHub authentication required before starting the child agent."
                            .to_string(),
                },
                ctx,
            );
        });

        let async_result = execute_future.await;
        let result = app.update(|ctx| on_complete(async_result, ctx));
        assert!(matches!(
            result,
            AIAgentActionResultType::StartAgent(StartAgentResult::Error { error, version })
                if error
                    == "GitHub authentication required before starting the child agent."
                    && version == StartAgentVersion::V1
        ));

        executor.read(&app, |executor, _| {
            assert!(executor.pending.is_empty());
        });
    });
}

#[test]
fn execute_resolves_error_when_request_linkage_happens_after_child_already_failed() {
    App::test((), |mut app| async move {
        initialize_history_persistence_for_tests(&mut app);
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        let executor = app.add_model(StartAgentExecutor::new);
        let parent_conversation_id = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_conversation(terminal_view_id, false, false, false, ctx)
        });
        history_model.update(&mut app, |model, ctx| {
            model.assign_run_id_for_conversation(
                parent_conversation_id,
                PARENT_RUN_ID.to_string(),
                None,
                terminal_view_id,
                ctx,
            );
        });
        let action = build_start_agent_action(
            StartAgentVersion::V1,
            StartAgentExecutionMode::local_with_defaults(),
        );

        let execution = executor.update(&mut app, |executor, ctx| {
            let input = ExecuteActionInput {
                action: &action,
                conversation_id: parent_conversation_id,
            };
            let result: AnyActionExecution = executor.execute(input, ctx).into();
            result
        });

        let AnyActionExecution::Async {
            execute_future,
            on_complete,
        } = execution
        else {
            panic!("expected async execution");
        };

        let child_conversation_id = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_child_conversation(
                terminal_view_id,
                "Agent 1".to_string(),
                parent_conversation_id,
                None,
                ctx,
            )
        });

        history_model.update(&mut app, |history_model, ctx| {
            history_model.update_conversation_status_with_error(
                terminal_view_id,
                child_conversation_id,
                ConversationStatus::Error,
                Some(RenderableAIError::other(
                    "'codex' CLI not found on your machine.",
                    false,
                )),
                ctx,
            );
        });

        history_model.update(&mut app, |model, ctx| {
            model.record_new_conversation_request_complete(
                FIRST_REQUEST_ID,
                child_conversation_id,
                ctx,
            );
        });

        let async_result = execute_future.await;
        let result = app.update(|ctx| on_complete(async_result, ctx));
        assert!(matches!(
            result,
            AIAgentActionResultType::StartAgent(StartAgentResult::Error { error, version })
                if error == "'codex' CLI not found on your machine."
                    && version == StartAgentVersion::V1
        ));

        executor.read(&app, |executor, _| {
            assert!(executor.pending.is_empty());
        });
    });
}

#[test]
fn execute_resolves_success_when_request_linkage_happens_after_child_already_started() {
    App::test((), |mut app| async move {
        initialize_history_persistence_for_tests(&mut app);
        let terminal_view_id = EntityId::new();
        app.add_singleton_model(|_| ServerApiProvider::new_for_test());
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        app.add_singleton_model(OrchestrationEventStreamer::new);
        let executor = app.add_model(StartAgentExecutor::new);
        let parent_conversation_id = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_conversation(terminal_view_id, false, false, false, ctx)
        });
        history_model.update(&mut app, |model, ctx| {
            model.assign_run_id_for_conversation(
                parent_conversation_id,
                PARENT_RUN_ID.to_string(),
                None,
                terminal_view_id,
                ctx,
            );
        });
        let action = build_start_agent_action(
            StartAgentVersion::V1,
            StartAgentExecutionMode::local_with_defaults(),
        );

        let execution = executor.update(&mut app, |executor, ctx| {
            let input = ExecuteActionInput {
                action: &action,
                conversation_id: parent_conversation_id,
            };
            let result: AnyActionExecution = executor.execute(input, ctx).into();
            result
        });

        let AnyActionExecution::Async {
            execute_future,
            on_complete,
        } = execution
        else {
            panic!("expected async execution");
        };

        let child_conversation_id = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_child_conversation(
                terminal_view_id,
                "Agent 1".to_string(),
                parent_conversation_id,
                None,
                ctx,
            )
        });
        let run_id = uuid::Uuid::new_v4().to_string();

        history_model.update(&mut app, |history_model, ctx| {
            history_model.assign_run_id_for_conversation(
                child_conversation_id,
                run_id.clone(),
                None,
                terminal_view_id,
                ctx,
            );
        });

        history_model.update(&mut app, |model, ctx| {
            model.record_new_conversation_request_complete(
                FIRST_REQUEST_ID,
                child_conversation_id,
                ctx,
            );
        });

        let async_result = execute_future.await;
        let result = app.update(|ctx| on_complete(async_result, ctx));
        assert!(matches!(
            result,
            AIAgentActionResultType::StartAgent(StartAgentResult::Success {
                agent_id,
                version,
            }) if agent_id == run_id && version == StartAgentVersion::V1
        ));

        executor.read(&app, |executor, _| {
            assert!(executor.pending.is_empty());
        });
    });
}

#[test]
fn direct_provider_child_link_is_published_before_start_agent_completes() {
    App::test((), |mut app| async move {
        initialize_history_persistence_for_tests(&mut app);
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        let executor = app.add_model(StartAgentExecutor::new);
        let captured = capture_direct_provider_child_links(&mut app, &executor);
        let parent_conversation_id = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_conversation(terminal_view_id, false, false, false, ctx)
        });
        let action = build_start_agent_action(
            StartAgentVersion::V1,
            StartAgentExecutionMode::local_with_defaults(),
        );

        let execution = executor.update(&mut app, |executor, ctx| {
            let input = ExecuteActionInput {
                action: &action,
                conversation_id: parent_conversation_id,
            };
            let result: AnyActionExecution = executor.execute(input, ctx).into();
            result
        });
        let AnyActionExecution::Async {
            execute_future,
            on_complete,
        } = execution
        else {
            panic!("expected async execution");
        };

        let child_conversation_id = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_child_conversation(
                terminal_view_id,
                "Agent 1".to_string(),
                parent_conversation_id,
                None,
                ctx,
            )
        });
        history_model.update(&mut app, |model, ctx| {
            model.record_new_conversation_request_complete(
                FIRST_REQUEST_ID,
                child_conversation_id,
                ctx,
            );
        });

        captured.read(&app, |captured, _| {
            assert_eq!(
                captured.0,
                vec![(
                    action.id.clone(),
                    parent_conversation_id,
                    child_conversation_id,
                )]
            );
        });
        executor.read(&app, |executor, _| {
            assert!(
                executor.pending.contains_key(&FIRST_REQUEST_ID),
                "publishing the child link must not complete the StartAgent tool call"
            );
        });

        drop(execute_future);
        drop(on_complete);
    });
}

#[test]
fn hosted_child_link_does_not_publish_direct_provider_panel_event() {
    App::test((), |mut app| async move {
        initialize_history_persistence_for_tests(&mut app);
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        let executor = app.add_model(StartAgentExecutor::new);
        let captured = capture_direct_provider_child_links(&mut app, &executor);
        let parent_conversation_id = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_conversation(terminal_view_id, false, false, false, ctx)
        });
        history_model.update(&mut app, |model, ctx| {
            model.assign_run_id_for_conversation(
                parent_conversation_id,
                PARENT_RUN_ID.to_string(),
                None,
                terminal_view_id,
                ctx,
            );
        });
        let action = build_start_agent_action(
            StartAgentVersion::V1,
            StartAgentExecutionMode::local_with_defaults(),
        );

        let execution = executor.update(&mut app, |executor, ctx| {
            let input = ExecuteActionInput {
                action: &action,
                conversation_id: parent_conversation_id,
            };
            let result: AnyActionExecution = executor.execute(input, ctx).into();
            result
        });
        let AnyActionExecution::Async {
            execute_future,
            on_complete,
        } = execution
        else {
            panic!("expected async execution");
        };

        let child_conversation_id = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_child_conversation(
                terminal_view_id,
                "Agent 1".to_string(),
                parent_conversation_id,
                None,
                ctx,
            )
        });
        history_model.update(&mut app, |model, ctx| {
            model.record_new_conversation_request_complete(
                FIRST_REQUEST_ID,
                child_conversation_id,
                ctx,
            );
        });

        captured.read(&app, |captured, _| {
            assert_eq!(captured.0, Vec::new());
        });

        drop(execute_future);
        drop(on_complete);
    });
}

struct PendingDirectProviderChild {
    action_id: AIAgentActionId,
    parent_conversation_id: AIConversationId,
    history_model: ModelHandle<BlocklistAIHistoryModel>,
    executor: ModelHandle<StartAgentExecutor>,
    captured_cleanup: ModelHandle<CapturedCleanupEvents>,
    direct_links: ModelHandle<CapturedDirectProviderChildLinks>,
    run_agents_links: ModelHandle<CapturedRunAgentsChildLinks>,
    terminal_view_id: EntityId,
    child_conversation_id: AIConversationId,
    dispatch: StartAgentDispatch,
}

fn dispatch_pending_direct_provider_child(app: &mut App) -> PendingDirectProviderChild {
    initialize_history_persistence_for_tests(app);
    let terminal_view_id = EntityId::new();
    let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
    let executor = app.add_model(StartAgentExecutor::new);
    let captured_cleanup = app.add_model(|_| CapturedCleanupEvents::default());
    captured_cleanup.update(app, |_, ctx| {
        ctx.subscribe_to_model(&executor, |captured, _, event, _ctx| {
            if let StartAgentExecutorEvent::CleanupFailedChildLaunch { conversation_id } = event {
                captured.0.push(*conversation_id);
            }
        });
    });
    let direct_links = capture_direct_provider_child_links(app, &executor);
    let run_agents_links = capture_run_agents_child_links(app, &executor);
    let parent_conversation_id = history_model.update(app, |history_model, ctx| {
        history_model.start_new_conversation(terminal_view_id, false, false, false, ctx)
    });
    let action_id = AIAgentActionId::from("run-agents-action".to_string());
    let dispatch = executor.update(app, |executor, ctx| {
        executor.dispatch(
            action_id.clone(),
            "child".to_string(),
            "Investigate the failure".to_string(),
            StartAgentExecutionMode::local_with_defaults(),
            None,
            parent_conversation_id,
            None,
            ctx,
        )
    });
    let child_conversation_id = history_model.update(app, |history_model, ctx| {
        history_model.start_new_child_conversation(
            terminal_view_id,
            "child".to_string(),
            parent_conversation_id,
            None,
            ctx,
        )
    });
    history_model.update(app, |history_model, ctx| {
        history_model.record_new_conversation_request_complete(
            FIRST_REQUEST_ID,
            child_conversation_id,
            ctx,
        );
    });

    PendingDirectProviderChild {
        action_id,
        parent_conversation_id,
        history_model,
        executor,
        captured_cleanup,
        direct_links,
        run_agents_links,
        terminal_view_id,
        child_conversation_id,
        dispatch,
    }
}

#[test]
fn direct_provider_nonterminal_states_remain_pending_until_cancelled() {
    App::test((), |mut app| async move {
        let state = dispatch_pending_direct_provider_child(&mut app);
        assert_eq!(state.dispatch.wait_policy, StartAgentWaitPolicy::Completion);

        for status in [
            ConversationStatus::Blocked {
                blocked_action: "Waiting for user input".to_string(),
            },
            ConversationStatus::TransientError,
            ConversationStatus::WaitingForEvents,
        ] {
            state.history_model.update(&mut app, |history_model, ctx| {
                history_model.update_conversation_status(
                    state.terminal_view_id,
                    state.child_conversation_id,
                    status,
                    ctx,
                );
            });
            assert!(matches!(
                state.dispatch.receiver.try_recv(),
                Err(async_channel::TryRecvError::Empty)
            ));
            state.executor.read(&app, |executor, _| {
                assert!(executor.pending.contains_key(&FIRST_REQUEST_ID));
            });
        }

        state.history_model.update(&mut app, |history_model, ctx| {
            history_model.update_conversation_status(
                state.terminal_view_id,
                state.child_conversation_id,
                ConversationStatus::Cancelled,
                ctx,
            );
        });

        assert!(matches!(
            state.dispatch.receiver.try_recv(),
            Ok(StartAgentOutcome::Error(error))
                if error == "Child agent was cancelled by the user."
        ));
        state.executor.read(&app, |executor, _| {
            assert!(executor.pending.is_empty());
        });
        state.captured_cleanup.read(&app, |captured, _| {
            assert!(captured.0.is_empty());
        });
    });
}

#[test]
fn direct_provider_error_preserves_child_for_inspection() {
    App::test((), |mut app| async move {
        let state = dispatch_pending_direct_provider_child(&mut app);
        state.history_model.update(&mut app, |history_model, ctx| {
            history_model.update_conversation_status_with_error(
                state.terminal_view_id,
                state.child_conversation_id,
                ConversationStatus::Error,
                Some(RenderableAIError::other("Child execution failed", false)),
                ctx,
            );
        });

        assert!(matches!(
            state.dispatch.receiver.try_recv(),
            Ok(StartAgentOutcome::Error(error)) if error == "Child execution failed"
        ));
        state.captured_cleanup.read(&app, |captured, _| {
            assert!(captured.0.is_empty());
        });
        state.history_model.read(&app, |history_model, _| {
            assert!(history_model
                .conversation(&state.child_conversation_id)
                .is_some());
        });
    });
}

#[test]
fn cancelling_standalone_start_agent_drops_dispatch_but_keeps_child_running() {
    App::test((), |mut app| async move {
        initialize_history_persistence_for_tests(&mut app);
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        let executor = app.add_model(StartAgentExecutor::new);
        let parent_conversation_id = history_model.update(&mut app, |history, ctx| {
            history.start_new_conversation(terminal_view_id, false, false, false, ctx)
        });
        let action = build_start_agent_action(
            StartAgentVersion::V1,
            StartAgentExecutionMode::local_with_defaults(),
        );
        let execution = executor.update(&mut app, |executor, ctx| {
            executor
                .execute(
                    ExecuteActionInput {
                        action: &action,
                        conversation_id: parent_conversation_id,
                    },
                    ctx,
                )
                .into()
        });
        let child_conversation_id = history_model.update(&mut app, |history, ctx| {
            history.start_new_child_conversation(
                terminal_view_id,
                "child".to_string(),
                parent_conversation_id,
                None,
                ctx,
            )
        });
        history_model.update(&mut app, |history, ctx| {
            history.record_new_conversation_request_complete(
                FIRST_REQUEST_ID,
                child_conversation_id,
                ctx,
            );
        });

        executor.update(&mut app, |executor, _| {
            executor.cancel_execution(parent_conversation_id, &action.id);
        });

        executor.read(&app, |executor, _| assert!(executor.pending.is_empty()));
        history_model.read(&app, |history, _| {
            assert_eq!(
                history
                    .conversation(&child_conversation_id)
                    .expect("child should remain in history")
                    .status(),
                &ConversationStatus::InProgress
            );
        });
        let AnyActionExecution::Async { execute_future, .. } = execution else {
            panic!("expected async StartAgent execution");
        };
        let _ = execute_future.await;
    });
}

#[test]
fn cancelling_duplicate_action_id_detaches_only_the_matching_conversation() {
    App::test((), |mut app| async move {
        initialize_history_persistence_for_tests(&mut app);
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        let executor = app.add_model(StartAgentExecutor::new);
        let terminal_view_id = EntityId::new();
        let first_conversation = history_model.update(&mut app, |history, ctx| {
            history.start_new_conversation(terminal_view_id, false, false, false, ctx)
        });
        let second_conversation = history_model.update(&mut app, |history, ctx| {
            history.start_new_conversation(terminal_view_id, false, false, false, ctx)
        });
        let action = build_start_agent_action(
            StartAgentVersion::V1,
            StartAgentExecutionMode::local_with_defaults(),
        );

        let first = executor.update(&mut app, |executor, ctx| {
            executor
                .execute(
                    ExecuteActionInput {
                        action: &action,
                        conversation_id: first_conversation,
                    },
                    ctx,
                )
                .into()
        });
        let second = executor.update(&mut app, |executor, ctx| {
            executor
                .execute(
                    ExecuteActionInput {
                        action: &action,
                        conversation_id: second_conversation,
                    },
                    ctx,
                )
                .into()
        });
        assert!(matches!(first, AnyActionExecution::Async { .. }));
        assert!(matches!(second, AnyActionExecution::Async { .. }));

        executor.update(&mut app, |executor, _| {
            executor.cancel_execution(first_conversation, &action.id);
            assert_eq!(executor.pending.len(), 1);
            assert_eq!(
                executor
                    .pending
                    .values()
                    .next()
                    .unwrap()
                    .parent_conversation_id,
                second_conversation
            );
        });
    });
}

#[test]
fn removing_direct_provider_child_resolves_pending_wait() {
    App::test((), |mut app| async move {
        let state = dispatch_pending_direct_provider_child(&mut app);
        state.history_model.update(&mut app, |history_model, ctx| {
            history_model.remove_conversation(
                state.child_conversation_id,
                state.terminal_view_id,
                ctx,
            );
        });

        assert!(matches!(
            state.dispatch.receiver.try_recv(),
            Ok(StartAgentOutcome::Error(error))
                if error == "Child agent conversation was removed by the user."
        ));
        state.executor.read(&app, |executor, _| {
            assert!(executor.pending.is_empty());
        });
    });
}

#[test]
fn deleting_direct_provider_child_resolves_pending_wait() {
    App::test((), |mut app| async move {
        let state = dispatch_pending_direct_provider_child(&mut app);
        state.history_model.update(&mut app, |history_model, ctx| {
            history_model.delete_conversation(
                state.child_conversation_id,
                Some(state.terminal_view_id),
                ctx,
            );
        });

        assert!(matches!(
            state.dispatch.receiver.try_recv(),
            Ok(StartAgentOutcome::Error(error))
                if error == "Child agent conversation was removed by the user."
        ));
        state.executor.read(&app, |executor, _| {
            assert!(executor.pending.is_empty());
        });
    });
}

#[test]
fn run_agents_dispatch_publishes_only_run_agents_child_link() {
    App::test((), |mut app| async move {
        let state = dispatch_pending_direct_provider_child(&mut app);

        state.direct_links.read(&app, |captured, _| {
            assert!(captured.0.is_empty());
        });
        state.run_agents_links.read(&app, |captured, _| {
            assert_eq!(
                captured.0,
                vec![(
                    state.action_id.clone(),
                    "child".to_string(),
                    state.parent_conversation_id,
                    state.child_conversation_id,
                )]
            );
        });
    });
}

#[test]
fn reattach_reuses_persisted_child_without_launching_another_agent() {
    App::test((), |mut app| async move {
        initialize_history_persistence_for_tests(&mut app);
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        let executor = app.add_model(StartAgentExecutor::new);
        let captured_prompts = capture_start_agent_prompts(&mut app, &executor);
        let captured_links = capture_run_agents_child_links(&mut app, &executor);
        let parent_conversation_id = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_conversation(terminal_view_id, false, false, false, ctx)
        });
        let child_conversation_id = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_child_conversation(
                terminal_view_id,
                "child".to_string(),
                parent_conversation_id,
                None,
                ctx,
            )
        });
        let action_id = AIAgentActionId::from("run-agents-action".to_string());

        let dispatch = executor.update(&mut app, |executor, ctx| {
            executor.reattach(
                action_id.clone(),
                "child".to_string(),
                parent_conversation_id,
                child_conversation_id,
                StartAgentWaitPolicy::Completion,
                ctx,
            )
        });

        assert_eq!(dispatch.wait_policy, StartAgentWaitPolicy::Completion);
        assert!(matches!(
            dispatch.receiver.try_recv(),
            Err(async_channel::TryRecvError::Empty)
        ));
        captured_prompts.read(&app, |captured, _| {
            assert!(captured.0.is_empty());
        });
        captured_links.read(&app, |captured, _| {
            assert_eq!(
                captured.0,
                vec![(
                    action_id,
                    "child".to_string(),
                    parent_conversation_id,
                    child_conversation_id,
                )]
            );
        });

        history_model.update(&mut app, |history_model, ctx| {
            history_model.update_conversation_status(
                terminal_view_id,
                child_conversation_id,
                ConversationStatus::Success,
                ctx,
            );
        });
        assert!(matches!(
            dispatch.receiver.try_recv(),
            Ok(StartAgentOutcome::Completed { agent_id, .. })
                if agent_id == child_conversation_id.to_string()
        ));
        executor.read(&app, |executor, _| {
            assert!(executor.pending.is_empty());
        });
    });
}

#[test]
fn execute_waits_for_direct_provider_child_and_returns_its_output() {
    App::test((), |mut app| async move {
        initialize_history_persistence_for_tests(&mut app);
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        let executor = app.add_model(StartAgentExecutor::new);
        let parent_conversation_id = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_conversation(terminal_view_id, false, false, false, ctx)
        });
        let action = build_start_agent_action(
            StartAgentVersion::V1,
            StartAgentExecutionMode::local_with_defaults(),
        );

        let execution = executor.update(&mut app, |executor, ctx| {
            let input = ExecuteActionInput {
                action: &action,
                conversation_id: parent_conversation_id,
            };
            let result: AnyActionExecution = executor.execute(input, ctx).into();
            result
        });
        let AnyActionExecution::Async {
            execute_future,
            on_complete,
        } = execution
        else {
            panic!("expected async execution");
        };

        let child_conversation_id = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_child_conversation(
                terminal_view_id,
                "Agent 1".to_string(),
                parent_conversation_id,
                None,
                ctx,
            )
        });
        history_model.update(&mut app, |model, ctx| {
            model.record_new_conversation_request_complete(
                FIRST_REQUEST_ID,
                child_conversation_id,
                ctx,
            );
        });

        executor.read(&app, |executor, _| {
            let pending = executor
                .pending
                .get(&FIRST_REQUEST_ID)
                .expect("direct child should remain pending until completion");
            assert_eq!(pending.wait_policy, StartAgentWaitPolicy::Completion);
        });

        history_model.update(&mut app, |history_model, ctx| {
            history_model.update_conversation_status(
                terminal_view_id,
                child_conversation_id,
                ConversationStatus::Success,
                ctx,
            );
        });

        let async_result = execute_future.await;
        let result = app.update(|ctx| on_complete(async_result, ctx));
        assert!(matches!(
            result,
            AIAgentActionResultType::StartAgent(StartAgentResult::Success {
                agent_id,
                version,
            }) if agent_id.contains(&child_conversation_id.to_string())
                && agent_id.contains("Agent output:")
                && agent_id.contains("Agent completed but produced no text output.")
                && version == StartAgentVersion::V1
        ));
        executor.read(&app, |executor, _| {
            assert!(executor.pending.is_empty());
        });
    });
}

#[test]
fn execute_returns_detailed_error_when_child_startup_fails_before_initialization() {
    App::test((), |mut app| async move {
        initialize_history_persistence_for_tests(&mut app);
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        let executor = app.add_model(StartAgentExecutor::new);
        let parent_conversation_id = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_conversation(terminal_view_id, false, false, false, ctx)
        });
        history_model.update(&mut app, |model, ctx| {
            model.assign_run_id_for_conversation(
                parent_conversation_id,
                PARENT_RUN_ID.to_string(),
                None,
                terminal_view_id,
                ctx,
            );
        });
        let action = build_start_agent_action(
            StartAgentVersion::V1,
            StartAgentExecutionMode::local_with_defaults(),
        );

        let execution = executor.update(&mut app, |executor, ctx| {
            let input = ExecuteActionInput {
                action: &action,
                conversation_id: parent_conversation_id,
            };
            let result: AnyActionExecution = executor.execute(input, ctx).into();
            result
        });

        let AnyActionExecution::Async {
            execute_future,
            on_complete,
        } = execution
        else {
            panic!("expected async execution");
        };

        let child_conversation_id = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_child_conversation(
                terminal_view_id,
                "Agent 1".to_string(),
                parent_conversation_id,
                None,
                ctx,
            )
        });

        history_model.update(&mut app, |model, ctx| {
            model.record_new_conversation_request_complete(
                FIRST_REQUEST_ID,
                child_conversation_id,
                ctx,
            );
        });

        history_model.update(&mut app, |history_model, ctx| {
            history_model.update_conversation_status_with_error(
                terminal_view_id,
                child_conversation_id,
                ConversationStatus::Error,
                Some(RenderableAIError::other(
                    "Failed to resolve child agent skills: review-comments",
                    false,
                )),
                ctx,
            );
        });

        let async_result = execute_future.await;
        let result = app.update(|ctx| on_complete(async_result, ctx));
        assert!(matches!(
            result,
            AIAgentActionResultType::StartAgent(StartAgentResult::Error { error, version })
                if error == "Failed to resolve child agent skills: review-comments"
                    && version == StartAgentVersion::V1
        ));
    });
}

#[test]
fn execute_accepts_local_harness_child_when_parent_run_id_is_available() {
    App::test((), |mut app| async move {
        initialize_history_persistence_for_tests(&mut app);
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        let executor = app.add_model(StartAgentExecutor::new);
        let parent_conversation_id = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_conversation(terminal_view_id, false, false, false, ctx)
        });
        history_model.update(&mut app, |model, ctx| {
            model.assign_run_id_for_conversation(
                parent_conversation_id,
                PARENT_RUN_ID.to_string(),
                None,
                terminal_view_id,
                ctx,
            );
        });
        let action = build_start_agent_action(
            StartAgentVersion::V2,
            StartAgentExecutionMode::local_harness("claude".to_string()),
        );

        let execution = executor.update(&mut app, |executor, ctx| {
            let input = ExecuteActionInput {
                action: &action,
                conversation_id: parent_conversation_id,
            };
            let result: AnyActionExecution = executor.execute(input, ctx).into();
            result
        });
        let AnyActionExecution::Async {
            execute_future,
            on_complete,
        } = execution
        else {
            panic!("expected async execution");
        };

        executor.read(&app, |executor, _| {
            assert!(executor.pending.contains_key(&FIRST_REQUEST_ID));
        });

        drop(execute_future);
        drop(on_complete);
    });
}

#[test]
fn execute_returns_error_when_local_harness_child_missing_parent_run_id() {
    App::test((), |mut app| async move {
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        let executor = app.add_model(StartAgentExecutor::new);
        let parent_conversation_id = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_conversation(terminal_view_id, false, false, false, ctx)
        });
        let action = build_start_agent_action(
            StartAgentVersion::V2,
            StartAgentExecutionMode::local_harness("claude".to_string()),
        );

        let execution = executor.update(&mut app, |executor, ctx| {
            let input = ExecuteActionInput {
                action: &action,
                conversation_id: parent_conversation_id,
            };
            let result: AnyActionExecution = executor.execute(input, ctx).into();
            result
        });

        let AnyActionExecution::Sync(result) = execution else {
            panic!("expected sync execution");
        };

        assert!(matches!(
            result,
            AIAgentActionResultType::StartAgent(StartAgentResult::Error { error, version })
                if error
                    == "Local harness child agents require the parent run_id to be available."
                    && version == StartAgentVersion::V2
        ));
    });
}

#[test]
fn execute_rejects_invalid_local_harness_names_before_pane_creation() {
    App::test((), |mut app| async move {
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        let executor = app.add_model(StartAgentExecutor::new);
        let parent_conversation_id = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_conversation(terminal_view_id, false, false, false, ctx)
        });
        let action = build_start_agent_action(
            StartAgentVersion::V2,
            StartAgentExecutionMode::local_harness("gemini".to_string()),
        );

        let execution = executor.update(&mut app, |executor, ctx| {
            let input = ExecuteActionInput {
                action: &action,
                conversation_id: parent_conversation_id,
            };
            let result: AnyActionExecution = executor.execute(input, ctx).into();
            result
        });

        let AnyActionExecution::Sync(result) = execution else {
            panic!("expected sync execution");
        };

        assert!(matches!(
            result,
            AIAgentActionResultType::StartAgent(StartAgentResult::Error { error, version })
                if error == "Unsupported local child harness 'gemini'."
                    && version == StartAgentVersion::V2
        ));
    });
}

#[test]
fn execute_rejects_disabled_local_codex_before_other_local_harness_validation() {
    App::test((), |mut app| async move {
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        let executor = app.add_model(StartAgentExecutor::new);
        let parent_conversation_id = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_conversation(terminal_view_id, false, false, false, ctx)
        });
        let action = build_start_agent_action(
            StartAgentVersion::V2,
            StartAgentExecutionMode::local_harness("codex".to_string()),
        );

        let execution = executor.update(&mut app, |executor, ctx| {
            let input = ExecuteActionInput {
                action: &action,
                conversation_id: parent_conversation_id,
            };
            let result: AnyActionExecution = executor.execute(input, ctx).into();
            result
        });

        let AnyActionExecution::Sync(result) = execution else {
            panic!("expected sync execution");
        };

        assert!(matches!(
            result,
            AIAgentActionResultType::StartAgent(StartAgentResult::Error { error, version })
                if error == "Local Codex child agents are temporarily disabled."
                    && version == StartAgentVersion::V2
        ));
    });
}

#[test]
fn execute_allows_local_codex_when_flag_is_enabled() {
    App::test((), |mut app| async move {
        let _local_codex = FeatureFlag::LocalClaudeCodexChildHarnesses.override_enabled(true);
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        let executor = app.add_model(StartAgentExecutor::new);
        let parent_conversation_id = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_conversation(terminal_view_id, false, false, false, ctx)
        });
        let action = build_start_agent_action(
            StartAgentVersion::V2,
            StartAgentExecutionMode::local_harness("codex".to_string()),
        );

        let execution = executor.update(&mut app, |executor, ctx| {
            let input = ExecuteActionInput {
                action: &action,
                conversation_id: parent_conversation_id,
            };
            let result: AnyActionExecution = executor.execute(input, ctx).into();
            result
        });

        let AnyActionExecution::Sync(result) = execution else {
            panic!("expected sync execution");
        };

        assert!(matches!(
            result,
            AIAgentActionResultType::StartAgent(StartAgentResult::Error { error, version })
                if error
                    == "Local harness child agents require the parent run_id to be available."
                    && version == StartAgentVersion::V2
        ));
    });
}

#[test]
fn parallel_dispatch_keeps_two_pendings_distinguishable_by_request_id() {
    App::test((), |mut app| async move {
        initialize_history_persistence_for_tests(&mut app);
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        let executor = app.add_model(StartAgentExecutor::new);
        let parent_conversation_id = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_conversation(terminal_view_id, false, false, false, ctx)
        });
        history_model.update(&mut app, |model, ctx| {
            model.assign_run_id_for_conversation(
                parent_conversation_id,
                PARENT_RUN_ID.to_string(),
                None,
                terminal_view_id,
                ctx,
            );
        });

        let action_a = build_start_agent_action(
            StartAgentVersion::V1,
            StartAgentExecutionMode::local_with_defaults(),
        );
        let action_b = build_start_agent_action(
            StartAgentVersion::V1,
            StartAgentExecutionMode::local_with_defaults(),
        );
        executor.update(&mut app, |executor, ctx| {
            let _: AnyActionExecution = executor
                .execute(
                    ExecuteActionInput {
                        action: &action_a,
                        conversation_id: parent_conversation_id,
                    },
                    ctx,
                )
                .into();
            let _: AnyActionExecution = executor
                .execute(
                    ExecuteActionInput {
                        action: &action_b,
                        conversation_id: parent_conversation_id,
                    },
                    ctx,
                )
                .into();
        });

        executor.read(&app, |executor, _| {
            assert_eq!(executor.pending.len(), 2, "both pendings should be live");
            assert!(executor.pending.contains_key(&FIRST_REQUEST_ID));
            assert!(executor
                .pending
                .contains_key(&StartAgentRequestId::from_raw_for_test(1)));
        });
    });
}

#[test]
fn parallel_pendings_each_resolve_independently_via_recorded_child_id() {
    App::test((), |mut app| async move {
        initialize_history_persistence_for_tests(&mut app);
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        let executor = app.add_model(StartAgentExecutor::new);
        let parent_conversation_id = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_conversation(terminal_view_id, false, false, false, ctx)
        });
        history_model.update(&mut app, |model, ctx| {
            model.assign_run_id_for_conversation(
                parent_conversation_id,
                PARENT_RUN_ID.to_string(),
                None,
                terminal_view_id,
                ctx,
            );
        });

        let action_a = build_start_agent_action(
            StartAgentVersion::V1,
            StartAgentExecutionMode::local_with_defaults(),
        );
        let action_b = build_start_agent_action(
            StartAgentVersion::V1,
            StartAgentExecutionMode::local_with_defaults(),
        );
        let exec_a = executor.update(&mut app, |executor, ctx| {
            let result: AnyActionExecution = executor
                .execute(
                    ExecuteActionInput {
                        action: &action_a,
                        conversation_id: parent_conversation_id,
                    },
                    ctx,
                )
                .into();
            result
        });
        let exec_b = executor.update(&mut app, |executor, ctx| {
            let result: AnyActionExecution = executor
                .execute(
                    ExecuteActionInput {
                        action: &action_b,
                        conversation_id: parent_conversation_id,
                    },
                    ctx,
                )
                .into();
            result
        });
        let (
            AnyActionExecution::Async {
                execute_future: future_a,
                on_complete: complete_a,
            },
            AnyActionExecution::Async {
                execute_future: future_b,
                on_complete: complete_b,
            },
        ) = (exec_a, exec_b)
        else {
            panic!("expected async executions");
        };

        let child_a = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_child_conversation(
                terminal_view_id,
                "Agent A".to_string(),
                parent_conversation_id,
                None,
                ctx,
            )
        });
        let child_b = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_child_conversation(
                terminal_view_id,
                "Agent B".to_string(),
                parent_conversation_id,
                None,
                ctx,
            )
        });

        history_model.update(&mut app, |model, ctx| {
            model.record_new_conversation_request_complete(FIRST_REQUEST_ID, child_a, ctx);
            model.record_new_conversation_request_complete(
                StartAgentRequestId::from_raw_for_test(1),
                child_b,
                ctx,
            );
        });

        history_model.update(&mut app, |history_model, ctx| {
            history_model.update_conversation_status_with_error(
                terminal_view_id,
                child_b,
                ConversationStatus::Error,
                Some(RenderableAIError::other("Agent B init failed", false)),
                ctx,
            );
        });

        let async_b = future_b.await;
        let result_b = app.update(|ctx| complete_b(async_b, ctx));
        assert!(matches!(
            result_b,
            AIAgentActionResultType::StartAgent(StartAgentResult::Error { error, .. })
                if error == "Agent B init failed"
        ));

        executor.read(&app, |executor, _| {
            assert_eq!(
                executor.pending.len(),
                1,
                "only child_b's pending should have been removed"
            );
            assert!(executor.pending.contains_key(&FIRST_REQUEST_ID));
        });

        drop(future_a);
        drop(complete_a);
    });
}

#[derive(Default)]
struct CapturedCleanupEvents(Vec<AIConversationId>);

impl Entity for CapturedCleanupEvents {
    type Event = ();
}

/// Per-test handles for the launch-cleanup tests, returned by
/// [`dispatch_pending_child_launch`].
struct PendingChildLaunch {
    history_model: ModelHandle<BlocklistAIHistoryModel>,
    captured: ModelHandle<CapturedCleanupEvents>,
    terminal_view_id: EntityId,
    child_conversation_id: AIConversationId,
}

/// Dispatches a local child launch and creates (but does not yet link) its
/// child conversation, leaving one in-flight pending in the executor with a
/// model subscribed to capture `CleanupFailedChildLaunch` events. Tests link
/// the child and drive it to a terminal state, then assert on cleanup. The
/// returned executor handle must be kept alive for the duration of the test so
/// the executor (and its history subscription) is not dropped before the child
/// status change is processed.
fn dispatch_pending_child_launch(
    app: &mut App,
) -> (PendingChildLaunch, ModelHandle<StartAgentExecutor>) {
    initialize_history_persistence_for_tests(app);
    let terminal_view_id = EntityId::new();
    let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
    let executor = app.add_model(StartAgentExecutor::new);
    let captured = app.add_model(|_| CapturedCleanupEvents::default());
    captured.update(app, |_, ctx| {
        ctx.subscribe_to_model(&executor, |captured, _, event, _ctx| {
            if let StartAgentExecutorEvent::CleanupFailedChildLaunch { conversation_id } = event {
                captured.0.push(*conversation_id);
            }
        });
    });
    let parent_conversation_id = history_model.update(app, |history_model, ctx| {
        history_model.start_new_conversation(terminal_view_id, false, false, false, ctx)
    });
    history_model.update(app, |model, ctx| {
        model.assign_run_id_for_conversation(
            parent_conversation_id,
            PARENT_RUN_ID.to_string(),
            None,
            terminal_view_id,
            ctx,
        );
    });
    let action = build_start_agent_action(
        StartAgentVersion::V1,
        StartAgentExecutionMode::local_with_defaults(),
    );
    // The pending lives in the executor regardless of the returned execution,
    // and cleanup is emitted synchronously from the child status update, so the
    // action-result plumbing is discarded.
    executor.update(app, |executor, ctx| {
        let _: AnyActionExecution = executor
            .execute(
                ExecuteActionInput {
                    action: &action,
                    conversation_id: parent_conversation_id,
                },
                ctx,
            )
            .into();
    });
    let child_conversation_id = history_model.update(app, |history_model, ctx| {
        history_model.start_new_child_conversation(
            terminal_view_id,
            "Agent 1".to_string(),
            parent_conversation_id,
            None,
            ctx,
        )
    });
    (
        PendingChildLaunch {
            history_model,
            captured,
            terminal_view_id,
            child_conversation_id,
        },
        executor,
    )
}

/// Links the dispatched child conversation back to its pending request,
/// which lets subsequent child status changes resolve the pending.
fn link_pending_child(state: &PendingChildLaunch, app: &mut App) {
    state.history_model.update(app, |model, ctx| {
        model.record_new_conversation_request_complete(
            FIRST_REQUEST_ID,
            state.child_conversation_id,
            ctx,
        );
    });
}

#[test]
fn errored_child_launch_emits_cleanup_event() {
    App::test((), |mut app| async move {
        let (state, _executor) = dispatch_pending_child_launch(&mut app);
        link_pending_child(&state, &mut app);
        state.history_model.update(&mut app, |model, ctx| {
            model.update_conversation_status_with_error(
                state.terminal_view_id,
                state.child_conversation_id,
                ConversationStatus::Error,
                Some(RenderableAIError::other(
                    "Child agent failed to spawn",
                    false,
                )),
                ctx,
            );
        });

        state.captured.read(&app, |captured, _| {
            assert_eq!(captured.0, vec![state.child_conversation_id]);
        });
    });
}

#[test]
fn blocked_child_launch_does_not_emit_cleanup_event() {
    App::test((), |mut app| async move {
        let (state, _executor) = dispatch_pending_child_launch(&mut app);
        link_pending_child(&state, &mut app);
        // A recoverable startup block (e.g. awaiting GitHub auth) keeps its
        // chip so the user can resolve it.
        state.history_model.update(&mut app, |model, ctx| {
            model.update_conversation_status(
                state.terminal_view_id,
                state.child_conversation_id,
                ConversationStatus::Blocked {
                    blocked_action: "GitHub authentication required.".to_string(),
                },
                ctx,
            );
        });

        state.captured.read(&app, |captured, _| {
            assert!(
                captured.0.is_empty(),
                "blocked startup should not schedule chip cleanup"
            );
        });
    });
}

#[test]
fn successfully_started_child_does_not_emit_cleanup_event() {
    App::test((), |mut app| async move {
        let (state, _executor) = dispatch_pending_child_launch(&mut app);
        // `OrchestrationEventStreamer::new` reads the history singleton, so it
        // must be registered after the helper registers it.
        app.add_singleton_model(|_| ServerApiProvider::new_for_test());
        app.add_singleton_model(OrchestrationEventStreamer::new);
        // Assigning a run id before linkage marks the child as started, so the
        // executor resolves the pending as a success, not a launch failure.
        state.history_model.update(&mut app, |model, ctx| {
            model.assign_run_id_for_conversation(
                state.child_conversation_id,
                uuid::Uuid::new_v4().to_string(),
                None,
                state.terminal_view_id,
                ctx,
            );
        });
        link_pending_child(&state, &mut app);

        state.captured.read(&app, |captured, _| {
            assert!(
                captured.0.is_empty(),
                "a child that started successfully should not be cleaned up"
            );
        });
    });
}

#[test]
fn execute_returns_error_when_remote_opencode_harness_is_requested() {
    App::test((), |mut app| async move {
        let terminal_view_id = EntityId::new();
        let history_model = app.add_singleton_model(|_| BlocklistAIHistoryModel::new_for_test());
        let executor = app.add_model(StartAgentExecutor::new);
        let parent_conversation_id = history_model.update(&mut app, |history_model, ctx| {
            history_model.start_new_conversation(terminal_view_id, false, false, false, ctx)
        });
        let action = build_start_agent_action(
            StartAgentVersion::V2,
            StartAgentExecutionMode::Remote {
                environment_id: "env-123".to_string(),
                skill_references: vec![],
                model_id: String::new(),
                computer_use_enabled: false,
                worker_host: String::new(),
                harness_type: "opencode".to_string(),
                title: String::new(),
                auth_secret_name: None,
            },
        );

        let execution = executor.update(&mut app, |executor, ctx| {
            let input = ExecuteActionInput {
                action: &action,
                conversation_id: parent_conversation_id,
            };
            let result: AnyActionExecution = executor.execute(input, ctx).into();
            result
        });

        let AnyActionExecution::Sync(result) = execution else {
            panic!("expected sync execution");
        };

        assert!(matches!(
            result,
            AIAgentActionResultType::StartAgent(StartAgentResult::Error { error, version })
                if error == "Remote child agents do not support the opencode harness yet."
                    && version == StartAgentVersion::V2
        ));
    });
}
