use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, Mutex};

use ai::agent::action::{AskUserQuestionItem, AskUserQuestionType};
use chrono::Local;
use galaxy_agent_core::{
    CompletedModelTurn, ContentPart, ConversationMessage, ExternalWorkId, MessageContent,
    MessageRole, PermissionKind, PermissionRequest, ProviderRun, ProviderRunId, ProviderRunLimits,
    ProviderRunState, ProviderRunStep, RunEpoch, RuntimeCapabilities, StopReason, ToolCall,
    TurnRequest, Usage,
};
use galaxy_core::command::ExitCode;
use uuid::Uuid;
use warp_multi_agent_api::response_event;
use warpui::{App, EntityId, SingletonEntity};

use crate::ai::agent::conversation::{AIConversationId, ConversationStatus};
use crate::ai::agent::task::TaskId;
use crate::ai::agent::{
    AIAgentAction, AIAgentActionId, AIAgentActionResultType, AIAgentActionType, AIAgentAttachment,
    AIAgentContext, AIAgentExchangeId, AIAgentInput, CancellationReason, ImageContext,
    PassiveSuggestionTrigger, ReadShellCommandOutputResult, RequestCommandOutputResult,
    RunningCommand, ShellCommandError, TransferShellCommandControlToUserResult, UserQueryMode,
    WriteToLongRunningShellCommandResult,
};
use crate::ai::ambient_agents::AmbientAgentTaskId;
use crate::ai::blocklist::{
    BlocklistAIHistoryEvent, BlocklistAIHistoryModel, PendingAttachment, PendingFile, RequestInput,
    ResponseStream, ResponseStreamId,
};
use crate::ai::llms::LLMId;
use crate::persistence::model::{AcpConversationData, AgentBackend};
use crate::terminal::model::block::{BlockId, BlockState};
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

fn provider_execution_ref(
    conversation_id: AIConversationId,
    run_id: &str,
    epoch: u64,
) -> crate::ai::runtime::ProviderToolExecutionRef {
    crate::ai::runtime::ProviderToolExecutionRef {
        conversation_id,
        run_id: ProviderRunId::new(run_id),
        epoch: RunEpoch::new(epoch),
        call_id: "call".to_owned(),
    }
}

fn provider_snapshot(conversation_id: AIConversationId) -> super::ActiveProviderRunSnapshot {
    let task_id = TaskId::new("root-task".to_owned());
    let messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::Text("Finish the task".to_owned()),
    }];
    super::ActiveProviderRunSnapshot {
        version: super::ACTIVE_PROVIDER_RUN_SNAPSHOT_VERSION,
        run: ProviderRun::new(
            "restored-run",
            messages.clone(),
            crate::ai::runtime::BASE_PROVIDER_PROFILE,
            ProviderRunLimits::default(),
        ),
        base_request: TurnRequest::new("provider-model", messages),
        cli_monitor_request: None,
        response_config: crate::ai::runtime::RuntimeResponseConfig {
            task_id: task_id.to_string(),
            conversation_id: conversation_id.to_string(),
            needs_create_task: false,
            user_query: None,
            model_id: "provider-model".to_owned(),
            max_context_tokens: Some(128_000),
            capabilities: RuntimeCapabilities::provider(),
            empty_output_message: None,
        },
        action_context: crate::ai::runtime::ProviderActionContext::new_for_test(
            task_id.to_string(),
        ),
        projection_target: super::ProviderProjectionTarget {
            task_id: task_id.clone(),
            exchange_id: AIAgentExchangeId::new(),
        },
        root_task_id: task_id,
        did_input_contain_user_query: true,
        persistence_offset: 0,
        committed_provider_batch: None,
        command_action_refs: HashMap::new(),
        command_monitor: None,
        pending_monitor_observation: None,
        pending_command_completion: None,
        monitor_prose_continuations: 0,
    }
}

fn start_snapshot_tool(
    snapshot: &mut super::ActiveProviderRunSnapshot,
    call_id: &str,
) -> ExternalWorkId {
    let Some(ProviderRunStep::CallModel(call)) = snapshot.run.next_step().unwrap() else {
        panic!("expected provider model call");
    };
    snapshot
        .run
        .accept_model_turn(
            &call.work_id,
            CompletedModelTurn {
                assistant_content: vec![ContentPart::Text("I will run a command.".to_owned())],
                tool_calls: vec![ToolCall {
                    id: call_id.to_owned(),
                    name: "run_shell_command".to_owned(),
                    arguments: serde_json::json!({"command": "sleep 10"}),
                }],
                usage: Usage::default(),
                stop_reason: StopReason::Completed,
                advertised_tools: BTreeSet::from(["run_shell_command".to_owned()]),
            },
        )
        .unwrap();
    let Some(ProviderRunStep::DispatchTools(batch)) = snapshot.run.next_step().unwrap() else {
        panic!("expected provider tool batch");
    };
    batch.work_id
}

fn attach_snapshot_command_monitor(
    snapshot: &mut super::ActiveProviderRunSnapshot,
    conversation_id: AIConversationId,
) -> (AIAgentActionId, BlockId, TaskId) {
    let action_id = AIAgentActionId::from("command-call".to_owned());
    let block_id = BlockId::new();
    let cli_task_id = TaskId::new("cli-task".to_owned());
    let work_id = snapshot.run.ready_work_id().expect("ready work identity");
    snapshot.cli_monitor_request = Some(TurnRequest::new("provider-model", Vec::new()));
    snapshot.command_action_refs.insert(
        action_id.clone(),
        crate::ai::runtime::ProviderToolExecutionRef::new(
            conversation_id,
            &work_id,
            action_id.to_string(),
        ),
    );
    snapshot.command_monitor = Some(super::ProviderCommandMonitorState {
        run_id: snapshot.run.id().clone(),
        originating_work_id: work_id,
        originating_call_id: action_id.to_string(),
        initial_requested_command_action_id: action_id.clone(),
        block_id: block_id.clone(),
        command: "sleep 10".to_owned(),
        cli_task_id: cli_task_id.clone(),
    });
    snapshot.action_context.set_task_id(cli_task_id.to_string());
    snapshot.response_config.task_id = cli_task_id.to_string();
    (action_id, block_id, cli_task_id)
}

#[test]
fn provider_snapshot_parse_and_validation_reject_corrupt_restore_identity() {
    let conversation_id = AIConversationId::new();
    let snapshot = provider_snapshot(conversation_id);
    let json = serde_json::to_string(&snapshot).unwrap();
    assert!(super::ActiveProviderRunSnapshot::parse(&json)
        .unwrap()
        .validate(conversation_id)
        .is_ok());

    let mut unsupported_version = serde_json::to_value(&snapshot).unwrap();
    unsupported_version["version"] = serde_json::json!(99);
    assert!(
        super::ActiveProviderRunSnapshot::parse(&unsupported_version.to_string())
            .unwrap_err()
            .contains("unsupported active provider run snapshot version")
    );

    let mut invalid_offset = snapshot.clone();
    invalid_offset.persistence_offset = invalid_offset.run.transcript().len() + 1;
    assert_eq!(
        invalid_offset.validate(conversation_id).unwrap_err(),
        "provider run persistence offset exceeds transcript length"
    );

    let mut mismatched_model = snapshot.clone();
    mismatched_model.response_config.model_id = "different-model".to_owned();
    assert_eq!(
        mismatched_model.validate(conversation_id).unwrap_err(),
        "provider run base model does not match response projection"
    );

    let mut orphaned_task = snapshot;
    orphaned_task.action_context.set_task_id("orphan-task");
    orphaned_task.response_config.task_id = "orphan-task".to_owned();
    assert_eq!(
        orphaned_task.validate(conversation_id).unwrap_err(),
        "provider run current task is not owned by its projection or monitor"
    );
}

#[test]
fn restored_committed_command_requires_durable_terminal_owner() {
    let conversation_id = AIConversationId::new();
    let mut snapshot = provider_snapshot(conversation_id);
    let action_id = AIAgentActionId::from("command-call".to_owned());
    let work_id = snapshot.run.ready_work_id().expect("ready work identity");
    snapshot.committed_provider_batch = Some(work_id.clone());
    snapshot.command_action_refs.insert(
        action_id.clone(),
        crate::ai::runtime::ProviderToolExecutionRef::new(
            conversation_id,
            &work_id,
            action_id.to_string(),
        ),
    );

    assert_eq!(
        super::normalize_restored_provider_snapshot(&mut snapshot).unwrap_err(),
        "restored provider command batch completed without durable terminal evidence"
    );
}

#[test]
fn restore_normalization_removes_interrupted_command_correlation() {
    let conversation_id = AIConversationId::new();
    let mut snapshot = provider_snapshot(conversation_id);
    let call_id = "command-call";
    let work_id = start_snapshot_tool(&mut snapshot, call_id);
    snapshot.run.start_tool(&work_id, call_id).unwrap();
    let action_id = AIAgentActionId::from(call_id.to_owned());
    snapshot.command_action_refs.insert(
        action_id.clone(),
        crate::ai::runtime::ProviderToolExecutionRef::new(
            conversation_id,
            &work_id,
            action_id.to_string(),
        ),
    );

    super::normalize_restored_provider_snapshot(&mut snapshot).unwrap();

    assert!(!snapshot.command_action_refs.contains_key(&action_id));
    assert!(matches!(
        snapshot.run.state(),
        ProviderRunState::ReadyToCallModel
    ));
    let MessageContent::MultiPart(parts) = &snapshot.run.transcript().last().unwrap().content
    else {
        panic!("interrupted command result should be committed");
    };
    assert!(matches!(
        &parts[0],
        ContentPart::ToolResult {
            tool_use_id,
            is_error: true,
            ..
        } if tool_use_id == call_id
    ));
}

#[test]
fn restore_normalization_reproposes_permission_without_losing_correlation() {
    let conversation_id = AIConversationId::new();
    let mut snapshot = provider_snapshot(conversation_id);
    let call_id = "command-call";
    let work_id = start_snapshot_tool(&mut snapshot, call_id);
    snapshot
        .run
        .request_tool_permission(
            &work_id,
            PermissionRequest {
                id: "permission-1".to_owned(),
                call_id: call_id.to_owned(),
                kind: PermissionKind::Execute,
                reason: None,
            },
        )
        .unwrap();
    let action_id = AIAgentActionId::from(call_id.to_owned());
    snapshot.command_action_refs.insert(
        action_id.clone(),
        crate::ai::runtime::ProviderToolExecutionRef::new(
            conversation_id,
            &work_id,
            action_id.to_string(),
        ),
    );

    super::normalize_restored_provider_snapshot(&mut snapshot).unwrap();

    assert!(snapshot.command_action_refs.contains_key(&action_id));
    let ProviderRunState::AwaitingTools { batch } = snapshot.run.state() else {
        panic!("permission reset should keep the tool batch pending");
    };
    assert!(matches!(
        batch.calls[0].state,
        galaxy_agent_core::PendingToolCallState::Proposed
    ));
}

#[test]
fn restored_active_command_rebuilds_monitor_observation() {
    let conversation_id = AIConversationId::new();
    let mut snapshot = provider_snapshot(conversation_id);
    let (action_id, block_id, cli_task_id) =
        attach_snapshot_command_monitor(&mut snapshot, conversation_id);
    snapshot.pending_command_completion = Some(super::PendingProviderCommandCompletion {
        block_id: block_id.clone(),
        initial_requested_command_action_id: Some(action_id.clone()),
        command: "stale".to_owned(),
        output: "stale".to_owned(),
        exit_code: 1,
    });

    super::apply_restored_provider_command_evidence(
        conversation_id,
        &mut snapshot,
        super::RestoredProviderCommandEvidence {
            conversation_id: Some(conversation_id),
            requested_command_action_id: Some(action_id),
            cli_task_id: Some(cli_task_id.clone()),
            command: "sleep 10".to_owned(),
            state: BlockState::Executing,
            output: "running".to_owned(),
            exit_code: 0,
        },
    )
    .unwrap();

    assert!(snapshot.pending_command_completion.is_none());
    let observation = snapshot
        .pending_monitor_observation
        .expect("active command should restore monitoring");
    assert_eq!(observation.block_id, block_id);
    assert_eq!(observation.cli_task_id, cli_task_id);
}

#[test]
fn restored_completed_command_rebuilds_exact_completion_evidence() {
    let conversation_id = AIConversationId::new();
    let mut snapshot = provider_snapshot(conversation_id);
    let (action_id, block_id, cli_task_id) =
        attach_snapshot_command_monitor(&mut snapshot, conversation_id);
    snapshot.pending_monitor_observation = Some(super::PendingProviderMonitorObservation {
        block_id: block_id.clone(),
        cli_task_id: cli_task_id.clone(),
    });

    super::apply_restored_provider_command_evidence(
        conversation_id,
        &mut snapshot,
        super::RestoredProviderCommandEvidence {
            conversation_id: Some(conversation_id),
            requested_command_action_id: Some(action_id.clone()),
            cli_task_id: Some(cli_task_id),
            command: "sleep 10".to_owned(),
            state: BlockState::DoneWithExecution,
            output: "done".to_owned(),
            exit_code: 17,
        },
    )
    .unwrap();

    assert!(snapshot.pending_monitor_observation.is_none());
    let completion = snapshot
        .pending_command_completion
        .expect("completed command should restore final evidence");
    assert_eq!(completion.block_id, block_id);
    assert_eq!(
        completion.initial_requested_command_action_id,
        Some(action_id)
    );
    assert_eq!(completion.command, "sleep 10");
    assert_eq!(completion.output, "done");
    assert_eq!(completion.exit_code, 17);
}

#[test]
fn restored_command_rejects_mismatched_or_invalid_terminal_evidence() {
    let conversation_id = AIConversationId::new();
    let mut snapshot = provider_snapshot(conversation_id);
    let (action_id, _block_id, cli_task_id) =
        attach_snapshot_command_monitor(&mut snapshot, conversation_id);

    assert_eq!(
        super::apply_restored_provider_command_evidence(
            conversation_id,
            &mut snapshot,
            super::RestoredProviderCommandEvidence {
                conversation_id: Some(AIConversationId::new()),
                requested_command_action_id: Some(action_id.clone()),
                cli_task_id: Some(cli_task_id.clone()),
                command: "sleep 10".to_owned(),
                state: BlockState::Executing,
                output: String::new(),
                exit_code: 0,
            },
        )
        .unwrap_err(),
        "restored provider command block identity does not match"
    );
    assert_eq!(
        super::apply_restored_provider_command_evidence(
            conversation_id,
            &mut snapshot,
            super::RestoredProviderCommandEvidence {
                conversation_id: Some(conversation_id),
                requested_command_action_id: Some(action_id),
                cli_task_id: Some(cli_task_id),
                command: "sleep 10".to_owned(),
                state: BlockState::Background,
                output: String::new(),
                exit_code: 0,
            },
        )
        .unwrap_err(),
        "restored provider command block has an invalid state"
    );
}

#[test]
fn provider_restore_failure_is_visible_and_clears_persisted_run() {
    App::test((), |mut app| async move {
        initialize_app_for_terminal_view(&mut app);
        let terminal = add_window_with_terminal(&mut app, None);

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
                    history_model
                        .persist_active_provider_run_json(
                            conversation_id,
                            Some("corrupt snapshot".to_owned()),
                            ctx,
                        )
                        .unwrap();
                    conversation_id
                });

            terminal.ai_controller().update(ctx, |controller, ctx| {
                controller.restoring_provider_runs.insert(conversation_id);
                controller.fail_restored_provider_run(
                    conversation_id,
                    "snapshot identity mismatch".to_owned(),
                    ctx,
                );
                assert!(!controller
                    .restoring_provider_runs
                    .contains(&conversation_id));
            });

            let history_model = BlocklistAIHistoryModel::handle(ctx);
            let conversation = history_model
                .as_ref(ctx)
                .conversation(&conversation_id)
                .expect("failed restored conversation should remain visible");
            assert_eq!(conversation.status(), &ConversationStatus::Error);
            assert!(conversation.active_provider_run_json().is_none());
            let error = conversation
                .status_error()
                .expect("restore failure should retain a structured error");
            assert!(error
                .to_string()
                .contains("Failed to restore active provider run: snapshot identity mismatch"));
            assert!(matches!(
                error,
                crate::ai::agent::RenderableAIError::Other {
                    will_attempt_resume: false,
                    waiting_for_network: false,
                    is_user_error: false,
                    ..
                }
            ));
        });
    });
}

#[test]
fn provider_lifecycle_requires_exact_active_work_identity() {
    let conversation_id = AIConversationId::new();
    let active_work = ExternalWorkId {
        run_id: ProviderRunId::new("current"),
        epoch: RunEpoch::new(3),
    };

    assert!(super::provider_execution_matches_active_work(
        &active_work.run_id,
        Some(&active_work),
        &provider_execution_ref(conversation_id, "current", 3),
    ));
    assert!(!super::provider_execution_matches_active_work(
        &active_work.run_id,
        Some(&active_work),
        &provider_execution_ref(conversation_id, "current", 2),
    ));
    assert!(!super::provider_execution_matches_active_work(
        &active_work.run_id,
        Some(&active_work),
        &provider_execution_ref(conversation_id, "old", 3),
    ));
}

#[test]
fn provider_finished_action_only_resumes_its_committed_batch() {
    let conversation_id = AIConversationId::new();
    let run_id = ProviderRunId::new("current");
    let work_id = ExternalWorkId {
        run_id: run_id.clone(),
        epoch: RunEpoch::new(3),
    };
    let current = provider_execution_ref(conversation_id, "current", 3);

    assert_eq!(
        super::provider_finished_action_disposition(&run_id, Some(&work_id), None, &current),
        super::ProviderFinishedActionDisposition::AwaitBatchCommit,
    );
    assert_eq!(
        super::provider_finished_action_disposition(&run_id, None, Some(&work_id), &current),
        super::ProviderFinishedActionDisposition::Resume,
    );
    assert_eq!(
        super::provider_finished_action_disposition(
            &run_id,
            None,
            Some(&work_id),
            &provider_execution_ref(conversation_id, "current", 2),
        ),
        super::ProviderFinishedActionDisposition::Ignore,
    );
    assert_eq!(
        super::provider_finished_action_disposition(
            &run_id,
            None,
            Some(&work_id),
            &provider_execution_ref(conversation_id, "old", 3),
        ),
        super::ProviderFinishedActionDisposition::Ignore,
    );
}

#[test]
fn provider_boundary_prioritizes_completion_and_waits_for_committed_results() {
    assert_eq!(
        super::provider_boundary_intent(
            super::ProviderBoundaryPhase::Ready,
            true,
            true,
            true,
            true,
            true,
            0,
        ),
        super::ProviderBoundaryIntent::Park
    );
    assert_eq!(
        super::provider_boundary_intent(
            super::ProviderBoundaryPhase::Unsafe,
            false,
            true,
            true,
            true,
            true,
            0,
        ),
        super::ProviderBoundaryIntent::Park
    );
    assert_eq!(
        super::provider_boundary_intent(
            super::ProviderBoundaryPhase::Ready,
            false,
            true,
            true,
            true,
            true,
            0,
        ),
        super::ProviderBoundaryIntent::ApplyCompletion
    );
    assert_eq!(
        super::provider_boundary_intent(
            super::ProviderBoundaryPhase::Ready,
            false,
            false,
            true,
            false,
            true,
            0,
        ),
        super::ProviderBoundaryIntent::ApplyMonitorObservation
    );
}

#[test]
fn provider_monitor_prose_retry_is_bounded_then_parks() {
    assert_eq!(
        super::provider_boundary_intent(
            super::ProviderBoundaryPhase::AwaitingDriver,
            false,
            false,
            false,
            true,
            true,
            0,
        ),
        super::ProviderBoundaryIntent::RetryMonitor
    );
    assert_eq!(
        super::provider_boundary_intent(
            super::ProviderBoundaryPhase::AwaitingDriver,
            false,
            false,
            false,
            true,
            true,
            super::MAX_PROVIDER_MONITOR_PROSE_CONTINUATIONS,
        ),
        super::ProviderBoundaryIntent::Park
    );
    assert_eq!(
        super::provider_boundary_intent(
            super::ProviderBoundaryPhase::AwaitingDriver,
            false,
            false,
            false,
            false,
            false,
            0,
        ),
        super::ProviderBoundaryIntent::CompleteRun
    );
}

#[test]
fn provider_completion_requires_current_run_and_exact_monitor_identity() {
    let conversation_id = AIConversationId::new();
    let run_id = ProviderRunId::new("current");
    let block_id = BlockId::new();
    let other_block_id = BlockId::new();
    let action_id = AIAgentActionId::from("command-1".to_owned());
    let other_action_id = AIAgentActionId::from("command-2".to_owned());
    let execution_ref = provider_execution_ref(conversation_id, "current", 3);
    let command_action_refs = HashMap::from([(action_id.clone(), execution_ref.clone())]);

    assert!(super::provider_command_completion_matches(
        &run_id,
        &command_action_refs,
        None,
        &block_id,
        Some(&action_id),
    ));
    assert!(!super::provider_command_completion_matches(
        &run_id,
        &command_action_refs,
        None,
        &block_id,
        Some(&other_action_id),
    ));

    let monitor = super::ProviderCommandMonitorState {
        run_id: run_id.clone(),
        originating_work_id: execution_ref.work_id(),
        originating_call_id: action_id.to_string(),
        initial_requested_command_action_id: action_id.clone(),
        block_id: block_id.clone(),
        command: "sleep 10".to_owned(),
        cli_task_id: TaskId::new("cli-task".to_owned()),
    };
    assert!(super::provider_command_completion_matches(
        &run_id,
        &command_action_refs,
        Some(&monitor),
        &block_id,
        Some(&action_id),
    ));
    assert!(super::provider_command_completion_matches(
        &run_id,
        &command_action_refs,
        Some(&monitor),
        &block_id,
        None,
    ));
    assert!(!super::provider_command_completion_matches(
        &run_id,
        &command_action_refs,
        Some(&monitor),
        &other_block_id,
        Some(&action_id),
    ));
    assert!(!super::provider_command_completion_matches(
        &ProviderRunId::new("replacement"),
        &command_action_refs,
        Some(&monitor),
        &block_id,
        Some(&action_id),
    ));
}

#[test]
fn pending_provider_completion_reconciles_with_its_committed_snapshot() {
    let block_id = BlockId::new();
    let action_id = AIAgentActionId::from("command-1".to_owned());
    let mut completion = super::PendingProviderCommandCompletion {
        block_id: block_id.clone(),
        initial_requested_command_action_id: Some(action_id.clone()),
        command: String::new(),
        output: "done".to_owned(),
        exit_code: 0,
    };

    assert!(super::reconcile_provider_completion_with_snapshot(
        Some(&mut completion),
        &block_id,
        &action_id,
        Some("sleep 10"),
        None,
    )
    .unwrap());
    assert_eq!(completion.command, "sleep 10");
    assert!(super::reconcile_provider_completion_with_snapshot(
        None,
        &block_id,
        &action_id,
        Some("sleep 10"),
        None,
    )
    .is_ok_and(|matched| !matched));
}

#[test]
fn pending_provider_completion_rejects_a_different_snapshot() {
    let block_id = BlockId::new();
    let action_id = AIAgentActionId::from("command-1".to_owned());
    let mut completion = super::PendingProviderCommandCompletion {
        block_id,
        initial_requested_command_action_id: Some(action_id.clone()),
        command: String::new(),
        output: "done".to_owned(),
        exit_code: 0,
    };

    assert!(super::reconcile_provider_completion_with_snapshot(
        Some(&mut completion),
        &BlockId::new(),
        &action_id,
        Some("sleep 10"),
        None,
    )
    .is_err());
    let completion_block_id = completion.block_id.clone();
    assert!(super::reconcile_provider_completion_with_snapshot(
        Some(&mut completion),
        &completion_block_id,
        &AIAgentActionId::from("command-2".to_owned()),
        Some("sleep 10"),
        None,
    )
    .is_err());
}

#[test]
fn nonzero_provider_completion_is_continuation_evidence() {
    let completion = super::PendingProviderCommandCompletion {
        block_id: BlockId::new(),
        initial_requested_command_action_id: None,
        command: "cargo test".to_owned(),
        output: "one test failed".to_owned(),
        exit_code: 17,
    };
    let galaxy_agent_core::MessageContent::Text(observation) = completion.observation() else {
        panic!("command completion must be text evidence");
    };
    assert!(observation.contains("exit code 17"));
    assert!(observation.contains("nonzero exit is not automatic run completion"));
    assert!(observation.contains("Continue the original objective"));
}

#[test]
fn provider_command_result_classifier_covers_snapshot_and_finished_variants() {
    let block_id = BlockId::new();
    let exit_code = ExitCode::from(17);
    let expected_snapshot = super::ProviderCommandResult::Snapshot {
        block_id: block_id.clone(),
        command: Some("sleep 10".to_owned()),
    };
    assert_eq!(
        super::classify_provider_command_result(&AIAgentActionResultType::RequestCommandOutput(
            RequestCommandOutputResult::LongRunningCommandSnapshot {
                block_id: block_id.clone(),
                command: "sleep 10".to_owned(),
                grid_contents: "running".to_owned(),
                cursor: String::new(),
                is_alt_screen_active: false,
            },
        ),),
        Some(expected_snapshot.clone())
    );
    assert_eq!(
        super::classify_provider_command_result(&AIAgentActionResultType::ReadShellCommandOutput(
            ReadShellCommandOutputResult::LongRunningCommandSnapshot {
                command: "sleep 10".to_owned(),
                block_id: block_id.clone(),
                grid_contents: "running".to_owned(),
                cursor: String::new(),
                is_alt_screen_active: false,
                is_preempted: false,
            },
        ),),
        Some(expected_snapshot)
    );
    for result in [
        AIAgentActionResultType::WriteToLongRunningShellCommand(
            WriteToLongRunningShellCommandResult::Snapshot {
                block_id: block_id.clone(),
                grid_contents: "running".to_owned(),
                cursor: String::new(),
                is_alt_screen_active: false,
                is_preempted: false,
            },
        ),
        AIAgentActionResultType::TransferShellCommandControlToUser(
            TransferShellCommandControlToUserResult::Snapshot {
                block_id: block_id.clone(),
                grid_contents: "running".to_owned(),
                cursor: String::new(),
                is_alt_screen_active: false,
                is_preempted: false,
            },
        ),
    ] {
        assert_eq!(
            super::classify_provider_command_result(&result),
            Some(super::ProviderCommandResult::Snapshot {
                block_id: block_id.clone(),
                command: None,
            })
        );
    }

    let expected_finished = super::ProviderCommandResult::Finished {
        block_id: block_id.clone(),
        command: Some("sleep 10".to_owned()),
        output: "failed".to_owned(),
        exit_code: 17,
    };
    assert_eq!(
        super::classify_provider_command_result(&AIAgentActionResultType::RequestCommandOutput(
            RequestCommandOutputResult::Completed {
                block_id: block_id.clone(),
                command: "sleep 10".to_owned(),
                output: "failed".to_owned(),
                exit_code,
                start_ts: None,
                completed_ts: None,
            },
        ),),
        Some(expected_finished.clone())
    );
    assert_eq!(
        super::classify_provider_command_result(&AIAgentActionResultType::ReadShellCommandOutput(
            ReadShellCommandOutputResult::CommandFinished {
                command: "sleep 10".to_owned(),
                block_id: block_id.clone(),
                output: "failed".to_owned(),
                exit_code,
                start_ts: None,
                completed_ts: None,
            },
        ),),
        Some(expected_finished)
    );
    for result in [
        AIAgentActionResultType::WriteToLongRunningShellCommand(
            WriteToLongRunningShellCommandResult::CommandFinished {
                block_id: block_id.clone(),
                output: "failed".to_owned(),
                exit_code,
                start_ts: None,
                completed_ts: None,
            },
        ),
        AIAgentActionResultType::TransferShellCommandControlToUser(
            TransferShellCommandControlToUserResult::CommandFinished {
                block_id: block_id.clone(),
                output: "failed".to_owned(),
                exit_code,
                start_ts: None,
                completed_ts: None,
            },
        ),
    ] {
        assert_eq!(
            super::classify_provider_command_result(&result),
            Some(super::ProviderCommandResult::Finished {
                block_id: block_id.clone(),
                command: None,
                output: "failed".to_owned(),
                exit_code: 17,
            })
        );
    }
}

#[test]
fn provider_command_result_classifier_ignores_cancelled_and_error_variants() {
    let results = [
        AIAgentActionResultType::RequestCommandOutput(
            RequestCommandOutputResult::CancelledBeforeExecution,
        ),
        AIAgentActionResultType::RequestCommandOutput(RequestCommandOutputResult::Denylisted {
            command: "blocked".to_owned(),
        }),
        AIAgentActionResultType::WriteToLongRunningShellCommand(
            WriteToLongRunningShellCommandResult::Cancelled,
        ),
        AIAgentActionResultType::WriteToLongRunningShellCommand(
            WriteToLongRunningShellCommandResult::Error(ShellCommandError::BlockNotFound),
        ),
        AIAgentActionResultType::ReadShellCommandOutput(ReadShellCommandOutputResult::Cancelled),
        AIAgentActionResultType::ReadShellCommandOutput(ReadShellCommandOutputResult::Error(
            ShellCommandError::BlockNotFound,
        )),
        AIAgentActionResultType::TransferShellCommandControlToUser(
            TransferShellCommandControlToUserResult::Cancelled,
        ),
        AIAgentActionResultType::TransferShellCommandControlToUser(
            TransferShellCommandControlToUserResult::Error(ShellCommandError::BlockNotFound),
        ),
    ];
    assert!(results
        .iter()
        .all(|result| super::classify_provider_command_result(result).is_none()));
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
fn tool_queue_decision_blocks_parent_tools_while_child_agents_are_active() {
    assert_eq!(
        super::tool_queue_decision(false, false, true, 2),
        super::ToolQueueDecision::BlockedActiveChildAgents
    );
}

#[test]
fn tool_queue_decision_preserves_existing_terminal_precedence() {
    assert_eq!(
        super::tool_queue_decision(true, false, true, 1),
        super::ToolQueueDecision::Cancelled
    );
    assert_eq!(
        super::tool_queue_decision(false, true, true, 1),
        super::ToolQueueDecision::UnfinishedExchange
    );
}

#[test]
fn tool_queue_decision_queues_actions_when_unblocked() {
    let decision = super::tool_queue_decision(false, false, false, 1);

    assert_eq!(decision, super::ToolQueueDecision::QueueActions);
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
