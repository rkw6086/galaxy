use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use ai::agent::action::{RunAgentsAgentRunConfig, RunAgentsExecutionMode, RunAgentsRequest};
use ai::agent::orchestration_config::{
    OrchestrationConfig, OrchestrationConfigStatus, OrchestrationExecutionMode,
};
use galaxy_core::execution_mode::ExecutionMode;
use galaxy_core::features::FeatureFlag;
use settings::Setting;
use warpui::{App, Entity, EntityId, ModelHandle};

use super::*;
use crate::ai::active_agent_views_model::ActiveAgentViewsModel;
use crate::ai::agent::task::TaskId;
use crate::ai::blocklist::{
    BlocklistAIHistoryModel, BlocklistAIPermissions, StartAgentExecutorEvent, StartAgentRequest,
};
use crate::ai::cloud_agent_settings::CloudAgentSettings;
use crate::ai::document::ai_document_model::{AIDocumentModel, AIDocumentSaveStatus};
use crate::ai::execution_profiles::profiles::AIExecutionProfilesModel;
use crate::ai::execution_profiles::RunAgentsPermission;
use crate::ai::mcp::templatable_manager::TemplatableMCPServerManager;
use crate::appearance::Appearance;
use crate::auth::AuthStateProvider;
use crate::cloud_object::model::persistence::CloudModel;
use crate::network::NetworkStatus;
use crate::server::cloud_objects::update_manager::UpdateManager;
use crate::server::ids::SyncId;
use crate::server::sync_queue::SyncQueue;
use crate::settings::PrivacySettings;
use crate::terminal::cli_agent_sessions::CLIAgentSessionsModel;
use crate::test_util::settings::initialize_settings_for_tests_with_mode;
use crate::workspaces::team_tester::TeamTesterStatus;
use crate::workspaces::user_workspaces::UserWorkspaces;
use crate::{
    AgentNotificationsModel, GlobalResourceHandles, GlobalResourceHandlesProvider, LaunchMode,
};

struct RunAgentsTestState {
    conversation_id: AIConversationId,
    executor: ModelHandle<RunAgentsExecutor>,
    start_agent_executor: ModelHandle<StartAgentExecutor>,
}

#[derive(Default)]
struct CapturedStartAgentRequests(Vec<StartAgentRequest>);

impl Entity for CapturedStartAgentRequests {
    type Event = ();
}
fn with_plan_id(mut action: AIAgentAction, plan_id: &str) -> AIAgentAction {
    let AIAgentActionType::RunAgents(request) = &mut action.action else {
        panic!("expected run_agents action");
    };
    request.plan_id = plan_id.to_string();
    action
}

fn persist_plan_config(
    app: &mut App,
    conversation_id: AIConversationId,
    plan_id: &str,
    status: OrchestrationConfigStatus,
) {
    persist_plan_config_with_harness(app, conversation_id, plan_id, "oz", status);
}

fn persist_plan_config_with_harness(
    app: &mut App,
    conversation_id: AIConversationId,
    plan_id: &str,
    harness_type: &str,
    status: OrchestrationConfigStatus,
) {
    BlocklistAIHistoryModel::handle(app).update(app, |history, _ctx| {
        history
            .conversation_mut(&conversation_id)
            .expect("conversation should exist")
            .set_orchestration_config_for_plan(
                plan_id.to_string(),
                OrchestrationConfig {
                    model_id: "auto".to_string(),
                    harness_type: harness_type.to_string(),
                    execution_mode: OrchestrationExecutionMode::Remote {
                        environment_id: "env-1".to_string(),
                        worker_host: "warp".to_string(),
                    },
                },
                status,
            );
    });
}

fn mark_conversation_as_child(app: &mut App, conversation_id: AIConversationId) {
    BlocklistAIHistoryModel::handle(app).update(app, |history, _ctx| {
        history
            .conversation_mut(&conversation_id)
            .expect("conversation should exist")
            .set_parent_agent_id("parent-agent".to_string());
    });
}

#[test]
fn should_autoexecute_duplicate_launched_agent_denial() {
    App::test((), |mut app| async move {
        let state = initialize_run_agents_test(&mut app, ExecutionMode::App);
        state.executor.update(&mut app, |executor, _ctx| {
            executor.record_launched_agents(
                state.conversation_id,
                &[RunAgentsAgentOutcome {
                    name: "child".to_string(),
                    kind: RunAgentsAgentOutcomeKind::Launched {
                        agent_id: "agent-123".to_string(),
                    },
                }],
            );
        });
        let action = remote_run_agents_action("oz");

        let should_autoexecute = state.executor.update(&mut app, |executor, ctx| {
            executor.should_autoexecute(
                ExecuteActionInput {
                    action: &action,
                    conversation_id: state.conversation_id,
                },
                ctx,
            )
        });

        assert!(should_autoexecute);
    });
}

#[test]
fn execute_denies_duplicate_launched_agent() {
    App::test((), |mut app| async move {
        let state = initialize_run_agents_test(&mut app, ExecutionMode::App);
        state.executor.update(&mut app, |executor, _ctx| {
            executor.record_launched_agents(
                state.conversation_id,
                &[RunAgentsAgentOutcome {
                    name: "child".to_string(),
                    kind: RunAgentsAgentOutcomeKind::Launched {
                        agent_id: "agent-123".to_string(),
                    },
                }],
            );
        });
        let action = with_agent_name(remote_run_agents_action("oz"), "Child");

        let execution = state.executor.update(&mut app, |executor, ctx| {
            executor
                .execute(
                    ExecuteActionInput {
                        action: &action,
                        conversation_id: state.conversation_id,
                    },
                    ctx,
                )
                .into()
        });

        let AnyActionExecution::Sync(AIAgentActionResultType::RunAgents(RunAgentsResult::Denied {
            reason,
        })) = execution
        else {
            panic!("expected synchronous run_agents denial");
        };
        assert!(reason.contains("child (agent-123)"));
        assert!(reason.contains("send_message_to_agent"));
    });
}

#[test]
fn execute_denies_run_agents_from_child_conversation() {
    App::test((), |mut app| async move {
        let state = initialize_run_agents_test(&mut app, ExecutionMode::App);
        mark_conversation_as_child(&mut app, state.conversation_id);
        let action = remote_run_agents_action("oz");

        let should_autoexecute = state.executor.update(&mut app, |executor, ctx| {
            executor.should_autoexecute(
                ExecuteActionInput {
                    action: &action,
                    conversation_id: state.conversation_id,
                },
                ctx,
            )
        });
        assert!(
            should_autoexecute,
            "the denial should not require user approval"
        );

        let execution = state.executor.update(&mut app, |executor, ctx| {
            executor
                .execute(
                    ExecuteActionInput {
                        action: &action,
                        conversation_id: state.conversation_id,
                    },
                    ctx,
                )
                .into()
        });

        assert!(matches!(
            execution,
            AnyActionExecution::Sync(AIAgentActionResultType::RunAgents(
                RunAgentsResult::Denied { reason }
            )) if reason.contains("leaf workers")
        ));
    });
}

#[test]
fn autonomous_mode_still_denies_run_agents_from_child_conversation() {
    App::test((), |mut app| async move {
        let state = initialize_run_agents_test(&mut app, ExecutionMode::Sdk);
        mark_conversation_as_child(&mut app, state.conversation_id);
        let action = remote_run_agents_action("oz");

        let execution = state.executor.update(&mut app, |executor, ctx| {
            executor
                .execute(
                    ExecuteActionInput {
                        action: &action,
                        conversation_id: state.conversation_id,
                    },
                    ctx,
                )
                .into()
        });

        assert!(matches!(
            execution,
            AnyActionExecution::Sync(AIAgentActionResultType::RunAgents(
                RunAgentsResult::Denied { reason }
            )) if reason.contains("leaf workers")
        ));
    });
}

#[test]
fn execute_denies_mixed_batch_containing_launched_agent() {
    App::test((), |mut app| async move {
        let state = initialize_run_agents_test(&mut app, ExecutionMode::App);
        state.executor.update(&mut app, |executor, _ctx| {
            executor.record_launched_agents(
                state.conversation_id,
                &[RunAgentsAgentOutcome {
                    name: "child".to_string(),
                    kind: RunAgentsAgentOutcomeKind::Launched {
                        agent_id: "agent-123".to_string(),
                    },
                }],
            );
        });
        let mut action = remote_run_agents_action("oz");
        let AIAgentActionType::RunAgents(request) = &mut action.action else {
            panic!("expected run_agents action");
        };
        request.agent_run_configs.push(RunAgentsAgentRunConfig {
            name: "new-child".to_string(),
            prompt: "Do separate work".to_string(),
            title: String::new(),
            model_id: String::new(),
        });

        let execution = state.executor.update(&mut app, |executor, ctx| {
            executor
                .execute(
                    ExecuteActionInput {
                        action: &action,
                        conversation_id: state.conversation_id,
                    },
                    ctx,
                )
                .into()
        });

        assert!(matches!(
            execution,
            AnyActionExecution::Sync(AIAgentActionResultType::RunAgents(
                RunAgentsResult::Denied { reason }
            )) if reason.contains("child (agent-123)")
        ));
    });
}

#[test]
fn validate_request_rejects_blank_and_duplicate_agent_names() {
    let AIAgentActionType::RunAgents(mut request) = remote_run_agents_action("oz").action else {
        panic!("expected run_agents action");
    };
    normalize_request_for_local_execution(&mut request);
    request.agent_run_configs[0].name = "   ".to_string();
    assert_eq!(
        validate_request(&request),
        Err("orchestrate: agent names must not be empty".to_string())
    );

    request.agent_run_configs[0].name = "Child".to_string();
    request.agent_run_configs.push(RunAgentsAgentRunConfig {
        name: " child ".to_string(),
        prompt: "Do separate work".to_string(),
        title: String::new(),
        model_id: String::new(),
    });
    assert_eq!(
        validate_request(&request),
        Err("orchestrate: duplicate agent name 'child' in the same batch".to_string())
    );
}

#[test]
fn validate_request_allows_unique_sibling_names() {
    let AIAgentActionType::RunAgents(mut request) = remote_run_agents_action("oz").action else {
        panic!("expected run_agents action");
    };
    normalize_request_for_local_execution(&mut request);
    request.agent_run_configs.push(RunAgentsAgentRunConfig {
        name: "second-child".to_string(),
        prompt: "Do separate work".to_string(),
        title: String::new(),
        model_id: String::new(),
    });

    assert_eq!(validate_request(&request), Ok(()));
}

#[test]
fn local_normalization_clears_remote_only_fields_and_disabled_harness() {
    let AIAgentActionType::RunAgents(mut request) = remote_run_agents_action("codex").action else {
        panic!("expected run_agents action");
    };
    request.model_id = "gpt-5".to_string();
    request.harness_auth_secret_name = Some("remote-secret".to_string());

    normalize_request_for_local_execution(&mut request);

    assert!(matches!(
        request.execution_mode,
        RunAgentsExecutionMode::Local
    ));
    assert_eq!(request.harness_type, "oz");
    assert_eq!(request.model_id, "");
    assert_eq!(request.harness_auth_secret_name, None);
    assert_eq!(validate_request(&request), Ok(()));
}

#[test]
fn validate_request_rejects_remote_dispatch() {
    let AIAgentActionType::RunAgents(request) = remote_run_agents_action("oz").action else {
        panic!("expected run_agents action");
    };

    assert_eq!(
        validate_request(&request),
        Err("Galaxy only supports local child-agent orchestration.".to_string())
    );
}

#[test]
fn recovered_run_agents_reattaches_existing_child_and_launches_only_missing_child() {
    App::test((), |mut app| async move {
        let state = initialize_run_agents_test(&mut app, ExecutionMode::Sdk);
        let terminal_view_id = EntityId::new();
        let history = BlocklistAIHistoryModel::handle(&app);
        let existing_child_id = history.update(&mut app, |history, ctx| {
            history.start_new_child_conversation(
                terminal_view_id,
                "child".to_string(),
                state.conversation_id,
                None,
                ctx,
            )
        });
        let captured = subscribe_to_start_agent_requests(&mut app, &state.start_agent_executor);
        let mut action = remote_run_agents_action("oz");
        let AIAgentActionType::RunAgents(request) = &mut action.action else {
            panic!("expected run_agents action");
        };
        request.agent_run_configs.push(RunAgentsAgentRunConfig {
            name: "missing-child".to_string(),
            prompt: "Do separate work".to_string(),
            title: String::new(),
            model_id: String::new(),
        });
        state.executor.update(&mut app, |executor, _| {
            executor
                .mark_recovery_actions(state.conversation_id, &HashSet::from([action.id.clone()]));
        });

        let execution = state.executor.update(&mut app, |executor, ctx| {
            executor
                .execute(
                    ExecuteActionInput {
                        action: &action,
                        conversation_id: state.conversation_id,
                    },
                    ctx,
                )
                .into()
        });
        let AnyActionExecution::Async {
            execute_future,
            on_complete,
        } = execution
        else {
            panic!("expected async recovery execution");
        };
        let missing_request = captured.read(&app, |captured, _| {
            assert_eq!(captured.0.len(), 1);
            assert_eq!(captured.0[0].name, "missing-child");
            captured.0[0].clone()
        });

        history.update(&mut app, |history, ctx| {
            history.update_conversation_status(
                terminal_view_id,
                existing_child_id,
                crate::ai::agent::conversation::ConversationStatus::Success,
                ctx,
            );
        });
        let missing_child_id = history.update(&mut app, |history, ctx| {
            history.start_new_child_conversation(
                terminal_view_id,
                "missing-child".to_string(),
                state.conversation_id,
                None,
                ctx,
            )
        });
        history.update(&mut app, |history, ctx| {
            history.record_new_conversation_request_complete(
                missing_request.id,
                missing_child_id,
                ctx,
            );
            history.update_conversation_status(
                terminal_view_id,
                missing_child_id,
                crate::ai::agent::conversation::ConversationStatus::Success,
                ctx,
            );
        });

        let async_result = execute_future.await;
        let result = app.update(|ctx| on_complete(async_result, ctx));
        let AIAgentActionResultType::RunAgents(RunAgentsResult::Launched { agents, .. }) = result
        else {
            panic!("expected recovered RunAgents result");
        };
        assert_eq!(agents.len(), 2);
        assert!(matches!(
            &agents[0].kind,
            RunAgentsAgentOutcomeKind::Launched { agent_id }
                if agent_id == &existing_child_id.to_string()
        ));
        assert!(matches!(
            &agents[1].kind,
            RunAgentsAgentOutcomeKind::Launched { agent_id }
                if agent_id == &missing_child_id.to_string()
        ));
    });
}

#[test]
fn cancelling_recovered_run_agents_keeps_persisted_child_running() {
    App::test((), |mut app| async move {
        let state = initialize_run_agents_test(&mut app, ExecutionMode::Sdk);
        let terminal_view_id = EntityId::new();
        let history = BlocklistAIHistoryModel::handle(&app);
        let child_id = history.update(&mut app, |history, ctx| {
            history.start_new_child_conversation(
                terminal_view_id,
                "child".to_string(),
                state.conversation_id,
                None,
                ctx,
            )
        });
        let action = remote_run_agents_action("oz");
        state.executor.update(&mut app, |executor, _| {
            executor
                .mark_recovery_actions(state.conversation_id, &HashSet::from([action.id.clone()]));
        });
        let execution = state.executor.update(&mut app, |executor, ctx| {
            executor
                .execute(
                    ExecuteActionInput {
                        action: &action,
                        conversation_id: state.conversation_id,
                    },
                    ctx,
                )
                .into()
        });
        let AnyActionExecution::Async {
            execute_future,
            on_complete,
        } = execution
        else {
            panic!("expected async recovery execution");
        };

        state.executor.update(&mut app, |executor, ctx| {
            executor.cancel_execution(state.conversation_id, &action.id, ctx);
        });
        let async_result = execute_future.await;
        let result = app.update(|ctx| on_complete(async_result, ctx));
        assert!(matches!(
            result,
            AIAgentActionResultType::RunAgents(RunAgentsResult::Cancelled)
        ));
        history.read(&app, |history, _| {
            assert!(matches!(
                history.conversation(&child_id).map(|child| child.status()),
                Some(crate::ai::agent::conversation::ConversationStatus::InProgress)
            ));
        });
    });
}

#[test]
fn completion_slots_are_polled_concurrently_and_preserve_request_order() {
    App::test((), |_app| async move {
        let (first_sender, first_receiver) = async_channel::bounded(1);
        let (second_sender, second_receiver) = async_channel::bounded(1);
        let slots = vec![
            ChildSlot::Pending(StartAgentDispatch {
                request_id: StartAgentRequestId::from_raw_for_test(1),
                receiver: first_receiver,
                wait_policy: StartAgentWaitPolicy::Completion,
                detached: Arc::new(AtomicBool::new(false)),
            }),
            ChildSlot::Pending(StartAgentDispatch {
                request_id: StartAgentRequestId::from_raw_for_test(2),
                receiver: second_receiver,
                wait_policy: StartAgentWaitPolicy::Completion,
                detached: Arc::new(AtomicBool::new(false)),
            }),
            ChildSlot::Failed("prelaunch failure".to_string()),
        ];
        let mut outcomes =
            Box::pin(join_all(slots.into_iter().map(|slot| {
                resolve_child_slot_with_timeout(slot, Duration::from_millis(1))
            })));

        second_sender
            .try_send(StartAgentOutcome::Completed {
                agent_id: "second-agent".to_string(),
                output: "done".to_string(),
            })
            .unwrap();
        assert!(futures::poll!(&mut outcomes).is_pending());
        assert!(
            !second_sender.is_full(),
            "join_all should poll and drain the second slot while the first is pending"
        );

        first_sender
            .try_send(StartAgentOutcome::Error("first failed".to_string()))
            .unwrap();
        let outcomes = outcomes.await;

        assert!(matches!(
            &outcomes[0].outcome,
            RunAgentsAgentOutcomeKind::Failed { error } if error == "first failed"
        ));
        assert!(matches!(
            &outcomes[1].outcome,
            RunAgentsAgentOutcomeKind::Completed { agent_id, output }
                if agent_id == "second-agent" && output == "done"
        ));
        assert!(matches!(
            &outcomes[2].outcome,
            RunAgentsAgentOutcomeKind::Failed { error } if error == "prelaunch failure"
        ));
    });
}

#[test]
fn completion_wait_ignores_spawn_timeout() {
    App::test((), |_app| async move {
        let (sender, receiver) = async_channel::bounded(1);
        let completion = Box::pin(resolve_child_slot_with_timeout(
            ChildSlot::Pending(StartAgentDispatch {
                request_id: StartAgentRequestId::from_raw_for_test(1),
                receiver,
                wait_policy: StartAgentWaitPolicy::Completion,
                detached: Arc::new(AtomicBool::new(false)),
            }),
            Duration::from_millis(1),
        ));
        let wait = warpui::r#async::Timer::after(Duration::from_millis(20));

        let completion = match futures::future::select(completion, Box::pin(wait)).await {
            futures::future::Either::Left((outcome, _)) => {
                panic!("completion wait unexpectedly resolved before child completion: {outcome:?}")
            }
            futures::future::Either::Right((_, completion)) => completion,
        };
        sender
            .try_send(StartAgentOutcome::Completed {
                agent_id: "child-agent".to_string(),
                output: "done".to_string(),
            })
            .unwrap();

        assert!(matches!(
            completion.await.outcome,
            RunAgentsAgentOutcomeKind::Completed { agent_id, output }
                if agent_id == "child-agent" && output == "done"
        ));
    });
}

#[test]
fn startup_wait_retains_spawn_timeout() {
    App::test((), |_app| async move {
        let (_sender, receiver) = async_channel::bounded(1);
        let outcome = resolve_child_slot_with_timeout(
            ChildSlot::Pending(StartAgentDispatch {
                request_id: StartAgentRequestId::from_raw_for_test(1),
                receiver,
                wait_policy: StartAgentWaitPolicy::Startup,
                detached: Arc::new(AtomicBool::new(false)),
            }),
            Duration::from_millis(1),
        )
        .await;

        assert!(outcome.timed_out_request_id.is_some());
        assert_eq!(
            outcome.timed_out_request_id,
            Some(StartAgentRequestId::from_raw_for_test(1))
        );
        assert!(matches!(
            outcome.outcome,
            RunAgentsAgentOutcomeKind::Failed { error }
                if error.contains("Agent failed to start within")
        ));
    });
}

#[test]
fn startup_timeout_detaches_exact_pending_request() {
    App::test((), |mut app| async move {
        let state = initialize_run_agents_test(&mut app, ExecutionMode::App);
        let start_agent_executor = state.start_agent_executor;
        let parent_conversation_id = state.conversation_id;
        let dispatch = start_agent_executor.update(&mut app, |executor, ctx| {
            executor.dispatch(
                AIAgentActionId::from("run-agents-timeout".to_string()),
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
                Some("parent-run".to_string()),
                ctx,
            )
        });
        let request_id = dispatch.request_id;

        let resolved =
            resolve_child_slot_with_timeout(ChildSlot::Pending(dispatch), Duration::from_millis(1))
                .await;
        let timed_out_request_id = resolved
            .timed_out_request_id
            .expect("startup timeout should expose request identity");
        start_agent_executor.update(&mut app, |executor, _| {
            assert!(executor.detach_dispatch(timed_out_request_id));
        });

        start_agent_executor.read(&app, |executor, _| {
            assert!(!executor.has_pending_dispatch_for_test(request_id));
        });
    });
}

fn initialize_run_agents_test(app: &mut App, mode: ExecutionMode) -> RunAgentsTestState {
    initialize_settings_for_tests_with_mode(app, mode, false);
    let global_resource_handles = GlobalResourceHandles::mock(app);
    app.add_singleton_model(|_| GlobalResourceHandlesProvider::new(global_resource_handles));
    let history = app.add_singleton_model(|_| BlocklistAIHistoryModel::new(vec![], &[]));
    app.add_singleton_model(OrchestrationEventService::new);
    app.add_singleton_model(|_| CLIAgentSessionsModel::new());
    app.add_singleton_model(|_| ActiveAgentViewsModel::new());
    app.add_singleton_model(AgentNotificationsModel::new);
    app.add_singleton_model(BlocklistAIPermissions::new);
    let terminal_view_id = EntityId::new();
    app.add_singleton_model(|_| AuthStateProvider::new_for_test());
    app.add_singleton_model(SyncQueue::mock);
    app.add_singleton_model(|_| NetworkStatus::new());
    app.add_singleton_model(TeamTesterStatus::mock);
    app.add_singleton_model(UpdateManager::mock);
    app.add_singleton_model(CloudModel::mock);
    app.add_singleton_model(|ctx| {
        crate::local_object_repository::LocalObjectRepository::new(None, None, ctx)
    });
    app.add_singleton_model(|_| Appearance::mock());
    app.add_singleton_model(|_| AIDocumentModel::new_for_test());
    app.add_singleton_model(|_| TemplatableMCPServerManager::default());
    app.add_singleton_model(|ctx| {
        AIExecutionProfilesModel::new(&LaunchMode::new_for_unit_test(), ctx)
    });
    app.add_singleton_model(PrivacySettings::mock);
    app.add_singleton_model(UserWorkspaces::default_mock);
    let conversation_id = history.update(app, |history_model, ctx| {
        history_model.start_new_conversation(terminal_view_id, false, false, false, ctx)
    });
    let start_agent_executor = app.add_model(StartAgentExecutor::new);
    let executor = app.add_model(|ctx| {
        RunAgentsExecutor::new(start_agent_executor.clone(), terminal_view_id, ctx)
    });

    RunAgentsTestState {
        conversation_id,
        executor,
        start_agent_executor,
    }
}

fn subscribe_to_start_agent_requests(
    app: &mut App,
    start_agent_executor: &ModelHandle<StartAgentExecutor>,
) -> ModelHandle<CapturedStartAgentRequests> {
    let captured = app.add_model(|_| CapturedStartAgentRequests::default());
    captured.update(app, |_, ctx| {
        ctx.subscribe_to_model(start_agent_executor, |captured, _, event, _ctx| {
            if let StartAgentExecutorEvent::CreateAgent(request) = event {
                captured.0.push(request.as_ref().clone());
            }
        });
    });
    captured
}

fn remote_run_agents_action(harness_type: &str) -> AIAgentAction {
    AIAgentAction {
        id: AIAgentActionId::from("run-agents-action".to_string()),
        task_id: TaskId::new("run-agents-task".to_string()),
        requires_result: true,
        action: AIAgentActionType::RunAgents(RunAgentsRequest {
            summary: "Run child agent".to_string(),
            base_prompt: "Help".to_string(),
            skills: vec![],
            model_id: String::new(),
            harness_type: harness_type.to_string(),
            execution_mode: RunAgentsExecutionMode::Remote {
                environment_id: "env-1".to_string(),
                worker_host: "warp".to_string(),
                computer_use_enabled: false,
            },
            agent_run_configs: vec![RunAgentsAgentRunConfig {
                name: "child".to_string(),
                prompt: "Help".to_string(),
                title: String::new(),
                model_id: String::new(),
            }],
            plan_id: String::new(),
            harness_auth_secret_name: None,
        }),
        tool_name: None,
    }
}

fn with_agent_name(mut action: AIAgentAction, name: &str) -> AIAgentAction {
    let AIAgentActionType::RunAgents(request) = &mut action.action else {
        panic!("expected run_agents action");
    };
    request.agent_run_configs[0].name = name.to_string();
    action
}

#[test]
fn local_codex_run_agents_maps_to_local_harness_mode_when_flag_enabled() {
    let _local_codex = FeatureFlag::LocalClaudeCodexChildHarnesses.override_enabled(true);
    let cfg = RunAgentsAgentRunConfig {
        name: "child".to_string(),
        prompt: "Investigate the failure".to_string(),
        title: String::new(),
        model_id: String::new(),
    };

    let mode = run_agents_to_start_agent_mode(
        &RunAgentsExecutionMode::Local,
        "codex",
        "",
        &[],
        None,
        &cfg,
    )
    .expect("local Codex should be accepted when the feature flag is enabled");

    assert_eq!(
        mode,
        StartAgentExecutionMode::Local {
            harness_type: Some("codex".to_string()),
            model_id: None,
        }
    );
}

#[test]
fn child_model_overrides_the_run_wide_model() {
    let cfg = RunAgentsAgentRunConfig {
        name: "child".to_string(),
        prompt: "Investigate".to_string(),
        title: String::new(),
        model_id: "child-model".to_string(),
    };

    let mode = run_agents_to_start_agent_mode(
        &RunAgentsExecutionMode::Local,
        "oz",
        "fallback-model",
        &[],
        None,
        &cfg,
    )
    .unwrap();

    assert_eq!(
        mode,
        StartAgentExecutionMode::Local {
            harness_type: None,
            model_id: Some("child-model".to_string()),
        }
    );
}

fn persist_default_auth_secret(app: &mut App, harness_config_name: &str, secret_name: &str) {
    CloudAgentSettings::handle(app).update(app, |settings, ctx| {
        let mut secrets = settings.last_selected_auth_secret.value().clone();
        secrets.insert(harness_config_name.to_string(), secret_name.to_string());
        settings
            .last_selected_auth_secret
            .set_value(secrets, ctx)
            .unwrap();
        settings
            .inherit_auth_secret_harnesses
            .set_value(HashMap::new(), ctx)
            .unwrap();
    });
}

#[test]
fn should_autoexecute_when_plan_has_approved_orchestration_config() {
    App::test((), |mut app| async move {
        let state = initialize_run_agents_test(&mut app, ExecutionMode::App);
        persist_plan_config(
            &mut app,
            state.conversation_id,
            "plan-1",
            OrchestrationConfigStatus::Approved,
        );
        let action = with_plan_id(remote_run_agents_action("oz"), "plan-1");

        let should_autoexecute = state.executor.update(&mut app, |executor, ctx| {
            executor.should_autoexecute(
                ExecuteActionInput {
                    action: &action,
                    conversation_id: state.conversation_id,
                },
                ctx,
            )
        });

        assert!(should_autoexecute);
    });
}

#[test]
fn approved_remote_plan_is_normalized_and_can_autoexecute_locally() {
    App::test((), |mut app| async move {
        let state = initialize_run_agents_test(&mut app, ExecutionMode::App);
        persist_plan_config_with_harness(
            &mut app,
            state.conversation_id,
            "plan-1",
            "codex",
            OrchestrationConfigStatus::Approved,
        );
        let action = with_plan_id(remote_run_agents_action("oz"), "plan-1");

        let should_autoexecute = state.executor.update(&mut app, |executor, ctx| {
            executor.should_autoexecute(
                ExecuteActionInput {
                    action: &action,
                    conversation_id: state.conversation_id,
                },
                ctx,
            )
        });

        assert!(should_autoexecute);
    });
}

#[test]
fn execute_denies_disapproved_plan_config() {
    App::test((), |mut app| async move {
        let state = initialize_run_agents_test(&mut app, ExecutionMode::App);
        persist_plan_config(
            &mut app,
            state.conversation_id,
            "plan-1",
            OrchestrationConfigStatus::Disapproved,
        );
        let action = with_plan_id(remote_run_agents_action("oz"), "plan-1");

        let execution = state.executor.update(&mut app, |executor, ctx| {
            executor
                .execute(
                    ExecuteActionInput {
                        action: &action,
                        conversation_id: state.conversation_id,
                    },
                    ctx,
                )
                .into()
        });

        let AnyActionExecution::Sync(AIAgentActionResultType::RunAgents(RunAgentsResult::Denied {
            reason,
        })) = execution
        else {
            panic!("expected synchronous run_agents denial");
        };
        assert_eq!(reason, "Orchestration config was disapproved");
    });
}

#[test]
fn execute_denies_never_allow_profile_setting() {
    App::test((), |mut app| async move {
        let state = initialize_run_agents_test(&mut app, ExecutionMode::App);
        set_run_agents_permission(&mut app, RunAgentsPermission::NeverAllow);
        let action = remote_run_agents_action("oz");

        let execution = state.executor.update(&mut app, |executor, ctx| {
            executor
                .execute(
                    ExecuteActionInput {
                        action: &action,
                        conversation_id: state.conversation_id,
                    },
                    ctx,
                )
                .into()
        });

        let AnyActionExecution::Sync(AIAgentActionResultType::RunAgents(RunAgentsResult::Denied {
            reason,
        })) = execution
        else {
            panic!("expected synchronous run_agents denial");
        };
        assert_eq!(
            reason,
            "Running child agents is disabled by the active execution profile."
        );
    });
}

#[test]
fn autonomous_mode_autoexecutes_and_does_not_deny_missing_api_key() {
    App::test((), |mut app| async move {
        let state = initialize_run_agents_test(&mut app, ExecutionMode::Sdk);
        set_run_agents_permission(&mut app, RunAgentsPermission::NeverAllow);
        let action = remote_run_agents_action("codex");

        let should_autoexecute = state.executor.update(&mut app, |executor, ctx| {
            executor.should_autoexecute(
                ExecuteActionInput {
                    action: &action,
                    conversation_id: state.conversation_id,
                },
                ctx,
            )
        });
        assert!(should_autoexecute);

        let execution = state.executor.update(&mut app, |executor, ctx| {
            executor
                .execute(
                    ExecuteActionInput {
                        action: &action,
                        conversation_id: state.conversation_id,
                    },
                    ctx,
                )
                .into()
        });
        assert!(matches!(execution, AnyActionExecution::Async { .. }));
    });
}

#[test]
fn execute_publishes_every_parent_owned_plan_before_dispatch() {
    App::test((), |mut app| async move {
        let state = initialize_run_agents_test(&mut app, ExecutionMode::Sdk);
        BlocklistAIHistoryModel::handle(&app).update(&mut app, |model, ctx| {
            model.assign_run_id_for_conversation(
                state.conversation_id,
                "00000000-0000-0000-0000-000000000001".to_string(),
                None,
                EntityId::new(),
                ctx,
            );
        });
        let unrelated_conversation_id = AIConversationId::new();
        let (first_plan_id, second_plan_id, unrelated_plan_id) = AIDocumentModel::handle(&app)
            .update(&mut app, |model, ctx| {
                (
                    model.create_document(
                        "First plan",
                        "# First",
                        state.conversation_id,
                        None,
                        ctx,
                    ),
                    model.create_document(
                        "Second plan",
                        "# Second",
                        state.conversation_id,
                        None,
                        ctx,
                    ),
                    model.create_document(
                        "Unrelated plan",
                        "# Unrelated",
                        unrelated_conversation_id,
                        None,
                        ctx,
                    ),
                )
            });
        let captured = subscribe_to_start_agent_requests(&mut app, &state.start_agent_executor);
        let action = remote_run_agents_action("oz");

        let execution = state.executor.update(&mut app, |executor, ctx| {
            executor
                .execute(
                    ExecuteActionInput {
                        action: &action,
                        conversation_id: state.conversation_id,
                    },
                    ctx,
                )
                .into()
        });

        assert!(matches!(execution, AnyActionExecution::Async { .. }));
        captured.read(&app, |captured, _ctx| {
            assert!(captured.0.is_empty());
        });
        AIDocumentModel::handle(&app).read(&app, |model, _ctx| {
            assert!(matches!(
                model.get_document_save_status(&first_plan_id),
                AIDocumentSaveStatus::Saving
            ));
            assert!(matches!(
                model.get_document_save_status(&second_plan_id),
                AIDocumentSaveStatus::Saving
            ));
            assert!(matches!(
                model.get_document_save_status(&unrelated_plan_id),
                AIDocumentSaveStatus::NotSaved
            ));
        });
    });
}

/// A run_agents call holds in the `Publishing` state while it waits for the parent's
/// plans to become server-backed, then dispatches children. This verifies that
/// cancelling mid-publication prevents fan-out: even when the plan finishes publishing
/// afterwards (resolving the wait), the post-wait dispatch is skipped because
/// `cancel_execution` cleared the pending marker that `is_pending` guards on.
#[test]
fn cancel_during_plan_publication_does_not_dispatch_children() {
    App::test((), |mut app| async move {
        let state = initialize_run_agents_test(&mut app, ExecutionMode::Sdk);
        BlocklistAIHistoryModel::handle(&app).update(&mut app, |model, ctx| {
            model.assign_run_id_for_conversation(
                state.conversation_id,
                "00000000-0000-0000-0000-000000000001".to_string(),
                None,
                EntityId::new(),
                ctx,
            );
        });
        let plan_id = AIDocumentModel::handle(&app).update(&mut app, |model, ctx| {
            model.create_document("Plan", "# Plan", state.conversation_id, None, ctx)
        });
        let captured = subscribe_to_start_agent_requests(&mut app, &state.start_agent_executor);
        let action = remote_run_agents_action("oz");
        let action_id = action.id.clone();

        let execution = state.executor.update(&mut app, |executor, ctx| {
            executor
                .execute(
                    ExecuteActionInput {
                        action: &action,
                        conversation_id: state.conversation_id,
                    },
                    ctx,
                )
                .into()
        });
        // The action is awaiting plan publication, so it's pending but no children dispatched yet.
        assert!(matches!(execution, AnyActionExecution::Async { .. }));
        state.executor.update(&mut app, |executor, ctx| {
            assert!(executor.is_pending(state.conversation_id, &action_id));
            executor.cancel_execution(state.conversation_id, &action_id, ctx);
            assert!(!executor.is_pending(state.conversation_id, &action_id));
        });

        // Finish publishing the plan, which resolves the wait the dispatch was blocked on.
        AIDocumentModel::handle(&app).update(&mut app, |model, ctx| {
            model.create_document_from_notebook(
                plan_id,
                SyncId::ServerId(123.into()),
                "Plan",
                "# Plan",
                state.conversation_id,
                None,
                ctx,
            );
        });
        for _ in 0..3 {
            futures_lite::future::yield_now().await;
        }

        // Cancellation won the race: the resolved wait does not fan out children.
        captured.read(&app, |captured, _ctx| {
            assert!(captured.0.is_empty());
        });
    });
}

fn set_run_agents_permission(app: &mut App, permission: RunAgentsPermission) {
    AIExecutionProfilesModel::handle(app).update(app, |profiles, ctx| {
        let profile_id = *profiles.active_profile(None, ctx).id();
        profiles.set_run_agents(profile_id, permission, ctx);
    });
}

#[test]
fn should_not_autoexecute_without_approved_plan_or_always_allow_profile() {
    App::test((), |mut app| async move {
        let state = initialize_run_agents_test(&mut app, ExecutionMode::App);
        let action = remote_run_agents_action("oz");

        let should_autoexecute = state.executor.update(&mut app, |executor, ctx| {
            executor.should_autoexecute(
                ExecuteActionInput {
                    action: &action,
                    conversation_id: state.conversation_id,
                },
                ctx,
            )
        });

        assert!(!should_autoexecute);
    });
}

#[test]
fn execute_normalizes_remote_non_oz_harness_without_requiring_remote_auth() {
    App::test((), |mut app| async move {
        let state = initialize_run_agents_test(&mut app, ExecutionMode::App);
        let action = remote_run_agents_action("codex");

        let execution = state.executor.update(&mut app, |executor, ctx| {
            executor
                .execute(
                    ExecuteActionInput {
                        action: &action,
                        conversation_id: state.conversation_id,
                    },
                    ctx,
                )
                .into()
        });

        assert!(matches!(execution, AnyActionExecution::Async { .. }));
    });
}

#[test]
fn normalized_remote_non_oz_harness_autoexecutes_with_always_allow() {
    App::test((), |mut app| async move {
        let state = initialize_run_agents_test(&mut app, ExecutionMode::App);
        set_run_agents_permission(&mut app, RunAgentsPermission::AlwaysAllow);
        let action = remote_run_agents_action("codex");

        let should_autoexecute = state.executor.update(&mut app, |executor, ctx| {
            executor.should_autoexecute(
                ExecuteActionInput {
                    action: &action,
                    conversation_id: state.conversation_id,
                },
                ctx,
            )
        });

        assert!(should_autoexecute);
    });
}

#[test]
fn normalized_remote_non_oz_harness_ignores_default_auth_secret() {
    App::test((), |mut app| async move {
        let state = initialize_run_agents_test(&mut app, ExecutionMode::App);
        set_run_agents_permission(&mut app, RunAgentsPermission::AlwaysAllow);
        persist_default_auth_secret(&mut app, "codex", "default-openai-key");
        let action = remote_run_agents_action("codex");

        let should_autoexecute = state.executor.update(&mut app, |executor, ctx| {
            executor.should_autoexecute(
                ExecuteActionInput {
                    action: &action,
                    conversation_id: state.conversation_id,
                },
                ctx,
            )
        });

        assert!(should_autoexecute);
    });
}

#[test]
fn normalized_remote_oz_harness_autoexecutes_without_default_auth_secret() {
    App::test((), |mut app| async move {
        let state = initialize_run_agents_test(&mut app, ExecutionMode::App);
        set_run_agents_permission(&mut app, RunAgentsPermission::AlwaysAllow);
        let action = remote_run_agents_action("oz");

        let should_autoexecute = state.executor.update(&mut app, |executor, ctx| {
            executor.should_autoexecute(
                ExecuteActionInput {
                    action: &action,
                    conversation_id: state.conversation_id,
                },
                ctx,
            )
        });

        assert!(should_autoexecute);
    });
}

#[test]
fn populate_default_auth_secret_for_autoexecute_uses_persisted_secret() {
    App::test((), |mut app| async move {
        let state = initialize_run_agents_test(&mut app, ExecutionMode::App);
        persist_default_auth_secret(&mut app, "claude", "default-anthropic-key");
        let AIAgentActionType::RunAgents(mut request) = remote_run_agents_action("claude").action
        else {
            panic!("expected run_agents action");
        };

        state.executor.update(&mut app, |_, ctx| {
            populate_default_auth_secret_for_execution(&mut request, ctx);
        });

        assert_eq!(
            request.harness_auth_secret_name.as_deref(),
            Some("default-anthropic-key")
        );
    });
}
