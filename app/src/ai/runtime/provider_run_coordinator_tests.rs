use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use galaxy_agent_core::{
    turn_control, AgentEventStream, ConversationMessage, MessageContent, MessageRole,
    PermissionDecision, PermissionKind, PermissionRequest, ProviderRunFailure, RuntimeCapabilities,
    RuntimeDescriptor, ToolCall, ToolDefinition, ToolResult, ToolResultStatus,
};
use warp_multi_agent_api::response_event;

use super::*;
use crate::ai::runtime::event_translator::{ProviderRunResponseProjector, RuntimeResponseConfig};

type ScriptedEvent = Result<AgentEvent, AgentError>;
type ScriptedTurn = Result<Vec<ScriptedEvent>, AgentError>;

struct ScriptedRuntime {
    descriptor: RuntimeDescriptor,
    turns: Mutex<VecDeque<ScriptedTurn>>,
    events_consumed: Arc<AtomicUsize>,
    requests: Mutex<Vec<TurnRequest>>,
}

impl ScriptedRuntime {
    fn new(turns: Vec<ScriptedTurn>) -> Self {
        Self::with_id("scripted", turns)
    }

    fn with_id(id: &str, turns: Vec<ScriptedTurn>) -> Self {
        Self {
            descriptor: RuntimeDescriptor {
                id: id.to_string(),
                display_name: "Scripted provider".to_string(),
                kind: RuntimeKind::Provider,
                capabilities: RuntimeCapabilities::provider(),
            },
            turns: Mutex::new(turns.into()),
            events_consumed: Arc::new(AtomicUsize::new(0)),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<TurnRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl AgentRuntime for ScriptedRuntime {
    fn descriptor(&self) -> &RuntimeDescriptor {
        &self.descriptor
    }

    async fn start_turn(
        &self,
        request: TurnRequest,
        _control: TurnControl,
    ) -> Result<AgentEventStream, AgentError> {
        self.requests.lock().unwrap().push(request);
        let turn = self
            .turns
            .lock()
            .unwrap()
            .pop_front()
            .expect("scripted runtime ran out of turns")?;
        let events_consumed = self.events_consumed.clone();
        Ok(Box::pin(futures::stream::iter(turn).inspect(move |_| {
            events_consumed.fetch_add(1, Ordering::Relaxed);
        })))
    }
}

#[derive(Clone, Copy)]
enum FirstAttemptStall {
    Start,
    Event,
}

struct StallingRuntime {
    descriptor: RuntimeDescriptor,
    first_attempt_stall: FirstAttemptStall,
    attempts: AtomicUsize,
    requests: Mutex<Vec<TurnRequest>>,
    controls: Mutex<Vec<TurnControl>>,
}

impl StallingRuntime {
    fn new(first_attempt_stall: FirstAttemptStall) -> Self {
        Self {
            descriptor: RuntimeDescriptor {
                id: "stalling".to_string(),
                display_name: "Stalling provider".to_string(),
                kind: RuntimeKind::Provider,
                capabilities: RuntimeCapabilities::provider(),
            },
            first_attempt_stall,
            attempts: AtomicUsize::new(0),
            requests: Mutex::new(Vec::new()),
            controls: Mutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<TurnRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn attempt_commands(&self) -> Vec<Option<TurnCommand>> {
        self.controls
            .lock()
            .unwrap()
            .iter()
            .map(|control| control.try_receive().ok())
            .collect()
    }
}

#[async_trait]
impl AgentRuntime for StallingRuntime {
    fn descriptor(&self) -> &RuntimeDescriptor {
        &self.descriptor
    }

    async fn start_turn(
        &self,
        request: TurnRequest,
        control: TurnControl,
    ) -> Result<AgentEventStream, AgentError> {
        self.requests.lock().unwrap().push(request);
        self.controls.lock().unwrap().push(control);
        let attempt = self.attempts.fetch_add(1, Ordering::Relaxed);
        if attempt == 0 {
            match self.first_attempt_stall {
                FirstAttemptStall::Start => return futures::future::pending().await,
                FirstAttemptStall::Event => {
                    let started = futures::stream::iter(vec![started("request-stalled")]);
                    return Ok(Box::pin(started.chain(futures::stream::pending())));
                }
            }
        }
        Ok(Box::pin(futures::stream::iter(answer_turn().unwrap())))
    }
}

fn request() -> TurnRequest {
    let mut request = TurnRequest::new(
        "test-model",
        vec![ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text("Inspect and fix the issue".to_string()),
        }],
    );
    request.conversation_id = Some("conversation".to_string());
    request.system_prompt = Some("Use tools and finish the task.".to_string());
    request.tools = vec![
        ToolDefinition {
            name: "read_files".to_string(),
            description: "Read files".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        },
        ToolDefinition {
            name: galaxy_agent_core::RECALL_TOOL_HISTORY_NAME.to_string(),
            description: "Recall tool results".to_string(),
            input_schema: serde_json::json!({"type": "object"}),
        },
    ];
    request
}

#[test]
fn estimates_turn_request_with_system_tools_messages_and_output() {
    let mut request = TurnRequest::new(
        "test-model",
        vec![ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text("12345678".to_string()),
        }],
    );
    request.system_prompt = Some("1234".to_string());
    request.tools = vec![ToolDefinition {
        name: "tool".to_string(),
        description: "description".to_string(),
        input_schema: serde_json::json!({"type": "object"}),
    }];
    request.max_output_tokens = Some(10);

    let expected_input = (request.system_prompt.as_deref().unwrap().len()
        + serde_json::to_string(&request.messages[0]).unwrap().len()
        + serde_json::to_string(&request.tools[0]).unwrap().len()) as u64;
    assert_eq!(
        estimate_turn_request_tokens(&request),
        expected_input.div_ceil(ESTIMATED_BYTES_PER_TOKEN) + ESTIMATED_REQUEST_OVERHEAD_TOKENS + 10
    );
}

#[test]
fn estimates_image_payloads_without_counting_raw_bytes_as_text() {
    let image_bytes = vec![255; 3_750_000];
    let serialized_text_tokens = serde_json::to_string(&image_bytes)
        .unwrap()
        .len()
        .div_ceil(ESTIMATED_BYTES_PER_TOKEN as usize) as u64;
    let request = TurnRequest::new(
        "test-model",
        vec![ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::MultiPart(vec![
                ContentPart::Text("what is in this screenshot?".to_string()),
                ContentPart::Image {
                    data: image_bytes,
                    mime_type: "image/jpeg".to_string(),
                },
            ]),
        }],
    );

    let estimate = estimate_turn_request_tokens(&request);

    assert!(estimate < 10_000);
    assert!(estimate > ESTIMATED_REQUEST_OVERHEAD_TOKENS + ESTIMATED_IMAGE_TOKENS);
    assert!(serialized_text_tokens > estimate);
}

#[tokio::test]
async fn proactively_compacts_before_starting_an_oversized_request() {
    let runtime = Arc::new(ScriptedRuntime::new(vec![answer_turn()]));
    let mut coordinator = coordinator(runtime.clone());
    coordinator.set_max_context_tokens(Some(1));
    let (_sender, control) = turn_control();

    let block = coordinator
        .drive_until_blocked(control, |_| Ok(()))
        .await
        .unwrap();

    assert_eq!(block, ProviderRunBlock::ContextWindowExceeded);
    assert!(runtime.requests().is_empty());
}

fn started(id: &str) -> ScriptedEvent {
    Ok(AgentEvent::TurnStarted {
        runtime_request_id: id.to_string(),
    })
}

fn usage(input_tokens: u64, output_tokens: u64) -> ScriptedEvent {
    Ok(AgentEvent::UsageUpdated {
        usage: Usage {
            input_tokens,
            output_tokens,
            ..Usage::default()
        },
    })
}

fn stopped(reason: StopReason) -> ScriptedEvent {
    Ok(AgentEvent::TurnStopped { reason })
}

fn tool_call(id: &str, name: &str) -> ToolCall {
    ToolCall {
        id: id.to_string(),
        name: name.to_string(),
        arguments: serde_json::json!({"path": "Cargo.toml"}),
    }
}

fn tool_turn() -> ScriptedTurn {
    Ok(vec![
        started("request-tools"),
        Ok(AgentEvent::TextDelta {
            text: "I will inspect it.".to_string(),
        }),
        Ok(AgentEvent::Tool {
            event: ToolEvent::Proposed {
                call: tool_call("read-1", "read_files"),
            },
        }),
        usage(10, 4),
        stopped(StopReason::Completed),
    ])
}

fn tool_turn_with_calls(calls: Vec<ToolCall>) -> ScriptedTurn {
    let mut events = vec![started("request-tools")];
    events.extend(calls.into_iter().map(|call| {
        Ok(AgentEvent::Tool {
            event: ToolEvent::Proposed { call },
        })
    }));
    events.push(usage(10, 4));
    events.push(stopped(StopReason::Completed));
    Ok(events)
}

fn answer_turn() -> ScriptedTurn {
    Ok(vec![
        started("request-answer"),
        Ok(AgentEvent::TextDelta {
            text: "The issue is fixed.".to_string(),
        }),
        usage(20, 5),
        stopped(StopReason::Completed),
    ])
}

#[tokio::test]
async fn tool_turn_text_is_streamed_and_retained_in_history() {
    let runtime = Arc::new(ScriptedRuntime::new(vec![tool_turn()]));
    let mut coordinator = coordinator(runtime);
    let mut projections = Vec::new();
    let (_sender, control) = turn_control();

    let block = coordinator
        .drive_until_blocked(control, collect_projection(&mut projections))
        .await
        .unwrap();

    assert!(matches!(block, ProviderRunBlock::Tools(_)));
    assert!(coordinator
        .run()
        .transcript()
        .iter()
        .any(|message| format!("{:?}", message.content).contains("I will inspect it.")));
    let text: Vec<_> = projections
        .iter()
        .filter_map(|projection| match projection {
            ProviderRunProjection::ModelEvent {
                event: AgentEvent::TextDelta { text },
                ..
            } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, ["I will inspect it."]);
}

#[tokio::test]
async fn text_and_reasoning_are_projected_before_reading_the_next_stream_event() {
    let runtime = Arc::new(ScriptedRuntime::new(vec![Ok(vec![
        started("request-streaming"),
        Ok(AgentEvent::TextDelta {
            text: "First ".to_string(),
        }),
        Ok(AgentEvent::ReasoningDelta {
            text: "Thinking".to_string(),
        }),
        Ok(AgentEvent::ReasoningCompleted {
            text: "Thinking".to_string(),
            signature: Some("signature".to_string()),
        }),
        Ok(AgentEvent::TextDelta {
            text: "second.".to_string(),
        }),
        usage(10, 4),
        stopped(StopReason::Completed),
    ])]));
    let events_consumed = runtime.events_consumed.clone();
    let mut coordinator = coordinator(runtime);
    let mut output = Vec::new();
    let (_sender, control) = turn_control();

    coordinator
        .drive_until_blocked(control, |projection| {
            if let ProviderRunProjection::ModelEvent { event, .. } = projection {
                if matches!(
                    event,
                    AgentEvent::TextDelta { .. }
                        | AgentEvent::ReasoningDelta { .. }
                        | AgentEvent::ReasoningCompleted { .. }
                ) {
                    output.push((events_consumed.load(Ordering::Relaxed), event));
                }
            }
            Ok(())
        })
        .await
        .unwrap();

    assert_eq!(
        output,
        vec![
            (
                2,
                AgentEvent::TextDelta {
                    text: "First ".to_string()
                }
            ),
            (
                3,
                AgentEvent::ReasoningDelta {
                    text: "Thinking".to_string()
                }
            ),
            (
                4,
                AgentEvent::ReasoningCompleted {
                    text: "Thinking".to_string(),
                    signature: Some("signature".to_string()),
                }
            ),
            (
                5,
                AgentEvent::TextDelta {
                    text: "second.".to_string()
                }
            ),
        ]
    );
}

#[tokio::test]
async fn completed_answer_is_still_projected() {
    let runtime = Arc::new(ScriptedRuntime::new(vec![answer_turn()]));
    let mut coordinator = coordinator(runtime);
    let mut projections = Vec::new();
    let (_sender, control) = turn_control();

    coordinator
        .drive_until_blocked(control, collect_projection(&mut projections))
        .await
        .unwrap();

    assert!(projections.iter().any(|projection| matches!(
        projection,
        ProviderRunProjection::ModelEvent {
            event: AgentEvent::TextDelta { text },
            ..
        } if text == "The issue is fixed."
    )));
}

fn coordinator(runtime: Arc<dyn AgentRuntime>) -> ProviderRunCoordinator {
    ProviderRunCoordinator::from_request(
        "run-1",
        ProviderRunProfile::new(runtime, request()),
        Vec::new(),
        ProviderRunLimits::default(),
    )
    .unwrap()
}

async fn coordinator_awaiting_tools(
    calls: Vec<ToolCall>,
) -> (ProviderRunCoordinator, PendingToolBatch) {
    let runtime = Arc::new(ScriptedRuntime::new(vec![tool_turn_with_calls(calls)]));
    let mut coordinator = coordinator(runtime);
    let (_sender, control) = turn_control();
    let block = coordinator
        .drive_until_blocked(control, |_| Ok(()))
        .await
        .unwrap();
    let ProviderRunBlock::Tools(batch) = block else {
        panic!("expected external tool work");
    };
    (coordinator, batch)
}

fn tool_result(call_id: &str, content: &str, status: ToolResultStatus) -> ToolEvent {
    ToolEvent::Completed {
        result: ToolResult {
            call_id: call_id.to_string(),
            content: content.to_string(),
            status,
        },
    }
}

fn collect_projection(
    events: &mut Vec<ProviderRunProjection>,
) -> impl FnMut(ProviderRunProjection) -> Result<(), String> + '_ {
    |event| {
        events.push(event);
        Ok(())
    }
}

#[tokio::test]
async fn one_run_drives_model_tool_and_followup_turns_with_atomic_history() {
    let runtime = Arc::new(ScriptedRuntime::new(vec![tool_turn(), answer_turn()]));
    let mut coordinator = coordinator(runtime.clone());
    let mut projections = Vec::new();
    let (_sender, control) = turn_control();

    let first_block = coordinator
        .drive_until_blocked(control, collect_projection(&mut projections))
        .await
        .unwrap();
    let ProviderRunBlock::Tools(batch) = first_block else {
        panic!("expected external tool work");
    };
    assert_eq!(batch.work_id.epoch.get(), 1);
    assert_eq!(batch.calls.len(), 1);
    assert!(projections.iter().all(|projection| !matches!(
        projection,
        ProviderRunProjection::ModelEvent {
            event: AgentEvent::Tool { .. },
            ..
        }
    )));
    assert!(matches!(
        projections.last(),
        Some(ProviderRunProjection::ToolBatchReady { .. })
    ));

    coordinator
        .run_mut()
        .start_tool(&batch.work_id, "read-1")
        .unwrap();
    coordinator
        .run_mut()
        .complete_tool(
            &batch.work_id,
            ToolResult {
                call_id: "read-1".to_string(),
                content: "manifest contents".to_string(),
                status: ToolResultStatus::Success,
            },
        )
        .unwrap();
    coordinator
        .run_mut()
        .commit_tool_batch(&batch.work_id)
        .unwrap();

    projections.clear();
    let (_sender, control) = turn_control();
    let second_block = coordinator
        .drive_until_blocked(control, collect_projection(&mut projections))
        .await
        .unwrap();
    let ProviderRunBlock::AwaitingDriver {
        work_id,
        stop_reason,
    } = second_block
    else {
        panic!("expected driver decision");
    };
    assert_eq!(stop_reason, StopReason::Completed);
    assert_eq!(work_id.epoch.get(), 3);
    assert_eq!(coordinator.run().usage().input_tokens, 30);
    assert_eq!(coordinator.run().usage().output_tokens, 9);

    let requests = runtime.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].messages.len(), 1);
    assert_eq!(requests[1].messages.len(), 3);
    let MessageContent::MultiPart(results) = &requests[1].messages[2].content else {
        panic!("expected atomic tool result message");
    };
    assert!(matches!(
        results.as_slice(),
        [ContentPart::ToolResult {
            tool_use_id,
            content,
            is_error: false,
        }] if tool_use_id == "read-1" && content == "manifest contents"
    ));
    assert!(projections.iter().any(|projection| matches!(
        projection,
        ProviderRunProjection::ModelEvent {
            event: AgentEvent::UsageUpdated { usage },
            ..
        } if usage.input_tokens == 20 && usage.output_tokens == 5
    )));

    coordinator.run_mut().complete(&work_id).unwrap();
    let (_sender, control) = turn_control();
    assert_eq!(
        coordinator
            .drive_until_blocked(control, |_| Ok(()))
            .await
            .unwrap(),
        ProviderRunBlock::Done(ProviderRunOutcome::Completed(
            galaxy_agent_core::ProviderRunCompletion {
                stop_reason: StopReason::Completed,
            }
        ))
    );
}

#[tokio::test]
async fn request_profiles_route_through_their_own_runtime_and_template() {
    let base_runtime = Arc::new(ScriptedRuntime::with_id(
        "base-runtime",
        vec![answer_turn()],
    ));
    let cli_runtime = Arc::new(ScriptedRuntime::with_id("cli-runtime", vec![answer_turn()]));
    let mut base_request = request();
    base_request.model = "base-model".into();
    base_request.system_prompt = Some("base prompt".to_string());
    let mut cli_request = request();
    cli_request.model = "cli-model".into();
    cli_request.system_prompt = Some("cli prompt".to_string());
    let run = ProviderRun::new(
        "run-1",
        base_request.messages.clone(),
        BASE_PROVIDER_PROFILE,
        ProviderRunLimits::default(),
    );
    let mut profiles = BTreeMap::new();
    profiles.insert(
        BASE_PROVIDER_PROFILE.to_string(),
        ProviderRunProfile::new(base_runtime.clone(), base_request),
    );
    profiles.insert(
        CLI_MONITOR_PROVIDER_PROFILE.to_string(),
        ProviderRunProfile::new(cli_runtime.clone(), cli_request),
    );
    let mut coordinator = ProviderRunCoordinator::new(run, profiles).unwrap();

    let (_sender, control) = turn_control();
    let ProviderRunBlock::AwaitingDriver { work_id, .. } = coordinator
        .drive_until_blocked(control, |_| Ok(()))
        .await
        .unwrap()
    else {
        panic!("expected base driver decision");
    };
    coordinator
        .run_mut()
        .continue_with_observation(
            &work_id,
            MessageContent::Text("command is still running".to_string()),
            CLI_MONITOR_PROVIDER_PROFILE,
        )
        .unwrap();
    let (_sender, control) = turn_control();
    assert!(matches!(
        coordinator
            .drive_until_blocked(control, |_| Ok(()))
            .await
            .unwrap(),
        ProviderRunBlock::AwaitingDriver { .. }
    ));

    let base_requests = base_runtime.requests();
    let cli_requests = cli_runtime.requests();
    assert_eq!(base_requests.len(), 1);
    assert_eq!(base_requests[0].model.as_str(), "base-model");
    assert_eq!(
        base_requests[0].system_prompt.as_deref(),
        Some("base prompt")
    );
    assert_eq!(cli_requests.len(), 1);
    assert_eq!(cli_requests[0].model.as_str(), "cli-model");
    assert_eq!(cli_requests[0].system_prompt.as_deref(), Some("cli prompt"));
    assert!(matches!(
        cli_requests[0].messages.last(),
        Some(ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text(text),
        }) if text == "command is still running"
    ));
}

#[tokio::test]
async fn correlated_parallel_lifecycle_commits_results_in_original_call_order() {
    let (mut coordinator, batch) = coordinator_awaiting_tools(vec![
        tool_call("first", "read_files"),
        tool_call("second", "read_files"),
    ])
    .await;
    let conversation_id = AIConversationId::new();
    let first = ProviderToolExecutionRef::new(conversation_id, &batch.work_id, "first");
    let second = ProviderToolExecutionRef::new(conversation_id, &batch.work_id, "second");

    assert_eq!(
        coordinator
            .apply_tool_lifecycle(
                &first,
                &ToolEvent::Started {
                    call_id: "first".to_string(),
                },
            )
            .unwrap(),
        ProviderToolLifecycleOutcome::Pending
    );
    coordinator
        .apply_tool_lifecycle(
            &second,
            &ToolEvent::Started {
                call_id: "second".to_string(),
            },
        )
        .unwrap();
    assert_eq!(
        coordinator
            .apply_tool_lifecycle(
                &second,
                &tool_result("second", "second result", ToolResultStatus::Success),
            )
            .unwrap(),
        ProviderToolLifecycleOutcome::Pending
    );
    assert_eq!(coordinator.run().transcript().len(), 2);
    assert_eq!(
        coordinator
            .apply_tool_lifecycle(
                &first,
                &tool_result("first", "first result", ToolResultStatus::Success),
            )
            .unwrap(),
        ProviderToolLifecycleOutcome::BatchCommitted
    );

    let MessageContent::MultiPart(parts) = &coordinator.run().transcript().last().unwrap().content
    else {
        panic!("expected atomic tool result message");
    };
    let ids = parts
        .iter()
        .map(|part| match part {
            ContentPart::ToolResult { tool_use_id, .. } => tool_use_id.as_str(),
            ContentPart::Text(_)
            | ContentPart::Reasoning { .. }
            | ContentPart::Image { .. }
            | ContentPart::ToolUse { .. } => panic!("expected only tool results"),
        })
        .collect::<Vec<_>>();
    assert_eq!(ids, vec!["first", "second"]);
}

#[tokio::test]
async fn correlated_lifecycle_rejects_wrong_run_epoch_call_and_duplicate_without_mutation() {
    let (mut coordinator, batch) = coordinator_awaiting_tools(vec![
        tool_call("first", "read_files"),
        tool_call("second", "read_files"),
    ])
    .await;
    let conversation_id = AIConversationId::new();
    let first = ProviderToolExecutionRef::new(conversation_id, &batch.work_id, "first");
    let second = ProviderToolExecutionRef::new(conversation_id, &batch.work_id, "second");
    let initial = coordinator.run().clone();

    let mut wrong_run = first.clone();
    wrong_run.run_id = ProviderRunId::new("wrong-run");
    assert!(matches!(
        coordinator
            .apply_tool_lifecycle(
                &wrong_run,
                &ToolEvent::Started {
                    call_id: "first".to_string(),
                },
            )
            .unwrap_err(),
        ProviderRunCoordinatorError::Core(ProviderRunProtocolError::WorkMismatch { .. })
    ));
    assert_eq!(coordinator.run(), &initial);

    let mut stale = first.clone();
    stale.epoch = RunEpoch::new(stale.epoch.get() + 1);
    assert!(matches!(
        coordinator
            .apply_tool_lifecycle(
                &stale,
                &ToolEvent::Started {
                    call_id: "first".to_string(),
                },
            )
            .unwrap_err(),
        ProviderRunCoordinatorError::Core(ProviderRunProtocolError::WorkMismatch { .. })
    ));
    assert_eq!(coordinator.run(), &initial);

    assert!(matches!(
        coordinator
            .apply_tool_lifecycle(
                &first,
                &ToolEvent::Started {
                    call_id: "other".to_string(),
                },
            )
            .unwrap_err(),
        ProviderRunCoordinatorError::InvalidToolLifecycle(_)
    ));
    assert_eq!(coordinator.run(), &initial);

    coordinator
        .apply_tool_lifecycle(
            &second,
            &tool_result("second", "done", ToolResultStatus::Success),
        )
        .unwrap();
    let after_completion = coordinator.run().clone();
    assert!(matches!(
        coordinator
            .apply_tool_lifecycle(
                &second,
                &tool_result("second", "duplicate", ToolResultStatus::Success),
            )
            .unwrap_err(),
        ProviderRunCoordinatorError::Core(ProviderRunProtocolError::DuplicateToolUpdate { .. })
    ));
    assert_eq!(coordinator.run(), &after_completion);
}

#[tokio::test]
async fn permission_denial_is_the_only_correlated_terminal_result() {
    let (mut coordinator, batch) =
        coordinator_awaiting_tools(vec![tool_call("shell", "read_files")]).await;
    let execution_ref =
        ProviderToolExecutionRef::new(AIConversationId::new(), &batch.work_id, "shell");
    let request = PermissionRequest {
        id: "permission-shell".to_string(),
        call_id: "shell".to_string(),
        kind: PermissionKind::Execute,
        reason: Some("run a command".to_string()),
    };

    assert_eq!(
        coordinator
            .apply_tool_lifecycle(
                &execution_ref,
                &ToolEvent::PermissionRequested {
                    request: request.clone(),
                },
            )
            .unwrap(),
        ProviderToolLifecycleOutcome::Pending
    );
    assert_eq!(
        coordinator
            .apply_tool_lifecycle(
                &execution_ref,
                &ToolEvent::PermissionResolved {
                    request_id: request.id,
                    call_id: request.call_id,
                    decision: PermissionDecision::Denied {
                        reason: Some("not allowed".to_string()),
                    },
                },
            )
            .unwrap(),
        ProviderToolLifecycleOutcome::BatchCommitted
    );
    let committed = coordinator.run().clone();
    assert!(matches!(
        &committed.transcript().last().unwrap().content,
        MessageContent::MultiPart(parts)
            if matches!(parts.as_slice(), [ContentPart::ToolResult {
                tool_use_id,
                content,
                is_error: true,
            }] if tool_use_id == "shell" && content == "not allowed")
    ));

    assert!(coordinator
        .apply_tool_lifecycle(
            &execution_ref,
            &tool_result("shell", "duplicate denial", ToolResultStatus::Denied),
        )
        .is_err());
    assert_eq!(coordinator.run(), &committed);
}

#[tokio::test]
async fn execution_failure_commits_one_correlated_error_result() {
    let (mut coordinator, batch) =
        coordinator_awaiting_tools(vec![tool_call("read", "read_files")]).await;
    let execution_ref =
        ProviderToolExecutionRef::new(AIConversationId::new(), &batch.work_id, "read");

    coordinator
        .apply_tool_lifecycle(
            &execution_ref,
            &ToolEvent::Started {
                call_id: "read".to_string(),
            },
        )
        .unwrap();
    assert_eq!(
        coordinator
            .apply_tool_lifecycle(
                &execution_ref,
                &tool_result("read", "file missing", ToolResultStatus::Error),
            )
            .unwrap(),
        ProviderToolLifecycleOutcome::BatchCommitted
    );
    assert!(matches!(
        &coordinator.run().transcript().last().unwrap().content,
        MessageContent::MultiPart(parts)
            if matches!(parts.as_slice(), [ContentPart::ToolResult {
                tool_use_id,
                content,
                is_error: true,
            }] if tool_use_id == "read" && content == "file missing")
    ));
}

#[tokio::test]
async fn inline_tool_batches_commit_before_the_next_model_boundary() {
    let recall_turn = Ok(vec![
        started("request-recall"),
        Ok(AgentEvent::Tool {
            event: ToolEvent::Proposed {
                call: ToolCall {
                    id: "recall-1".to_string(),
                    name: galaxy_agent_core::RECALL_TOOL_HISTORY_NAME.to_string(),
                    arguments: serde_json::json!({"search_query": "missing"}),
                },
            },
        }),
        usage(10, 1),
        stopped(StopReason::Completed),
    ]);
    let runtime = Arc::new(ScriptedRuntime::new(vec![recall_turn, answer_turn()]));
    let mut coordinator = coordinator(runtime.clone());
    let mut projections = Vec::new();
    let (_sender, control) = turn_control();

    let block = coordinator
        .drive_until_blocked(control, collect_projection(&mut projections))
        .await
        .unwrap();

    assert!(matches!(block, ProviderRunBlock::ReadyToCallModel));
    assert_eq!(runtime.requests().len(), 1);
    let (_sender, control) = turn_control();
    let block = coordinator
        .drive_until_blocked(control, collect_projection(&mut projections))
        .await
        .unwrap();
    assert!(matches!(block, ProviderRunBlock::AwaitingDriver { .. }));
    assert_eq!(runtime.requests().len(), 2);
    assert!(projections.iter().any(|projection| matches!(
        projection,
        ProviderRunProjection::ToolBatchReady { batch } if batch.is_complete()
    )));
    let requests = runtime.requests();
    assert_eq!(requests[1].messages.len(), 3);
    let MessageContent::MultiPart(results) = &requests[1].messages[2].content else {
        panic!("expected inline result batch");
    };
    assert!(matches!(
        results.as_slice(),
        [ContentPart::ToolResult {
            tool_use_id,
            is_error: false,
            ..
        }] if tool_use_id == "recall-1"
    ));
}

#[tokio::test]
async fn recoverable_start_failure_retries_the_same_work_identity() {
    let mut recoverable = AgentError::new(AgentErrorKind::Transport, "temporary network error");
    recoverable.recoverable = true;
    let runtime = Arc::new(ScriptedRuntime::new(vec![Err(recoverable), answer_turn()]));
    let mut coordinator = coordinator(runtime.clone());
    let mut projections = Vec::new();
    let (_sender, control) = turn_control();

    let block = coordinator
        .drive_until_blocked(control, collect_projection(&mut projections))
        .await
        .unwrap();

    assert!(matches!(block, ProviderRunBlock::AwaitingDriver { .. }));
    assert_eq!(runtime.requests().len(), 2);
    assert_eq!(coordinator.run().model_retries(), 1);
    let retry = projections
        .iter()
        .find_map(|projection| match projection {
            ProviderRunProjection::ModelRetry {
                work_id,
                retry_attempt,
                max_retries,
                ..
            } => Some((work_id.clone(), *retry_attempt, *max_retries)),
            ProviderRunProjection::ModelTurnRequested { .. }
            | ProviderRunProjection::ModelTurnStarted { .. }
            | ProviderRunProjection::ModelTurnFinished { .. }
            | ProviderRunProjection::ModelEvent { .. }
            | ProviderRunProjection::ToolBatchReady { .. } => None,
        })
        .expect("retry projection");
    let started = projections
        .iter()
        .find_map(|projection| match projection {
            ProviderRunProjection::ModelTurnStarted {
                work_id,
                retry_attempt: 1,
                ..
            } => Some(work_id.clone()),
            ProviderRunProjection::ModelTurnRequested { .. }
            | ProviderRunProjection::ModelTurnStarted { .. }
            | ProviderRunProjection::ModelTurnFinished { .. }
            | ProviderRunProjection::ModelEvent { .. }
            | ProviderRunProjection::ModelRetry { .. }
            | ProviderRunProjection::ToolBatchReady { .. } => None,
        })
        .expect("retried model start");
    assert_eq!(retry.0, started);
    assert_eq!(retry.1, 1);
    assert_eq!(retry.2, 3);
}

#[tokio::test]
async fn model_progress_waits_for_each_projection_acknowledgement() {
    let runtime = Arc::new(ScriptedRuntime::new(vec![answer_turn()]));
    let mut coordinator = coordinator(runtime.clone());
    let (acknowledgement_sender, acknowledgement_receiver) = async_channel::unbounded();
    let (observed_sender, observed_receiver) = async_channel::unbounded();
    let (_sender, control) = turn_control();

    let drive = async {
        coordinator
            .drive_until_blocked_with_acknowledgements(
                control,
                move |projection| {
                    let acknowledgement_receiver = acknowledgement_receiver.clone();
                    let observed_sender = observed_sender.clone();
                    Box::pin(async move {
                        observed_sender.send(projection).await.unwrap();
                        acknowledgement_receiver.recv().await.unwrap()
                    })
                },
                |_| Box::pin(async { Ok(()) }),
            )
            .await
            .unwrap()
    };
    let driver = async {
        let first = observed_receiver.recv().await.unwrap();
        assert!(matches!(
            first,
            ProviderRunProjection::ModelTurnRequested { .. }
        ));
        assert_eq!(runtime.requests().len(), 0);
        assert!(observed_receiver.try_recv().is_err());
        acknowledgement_sender.send(Ok(())).await.unwrap();

        let second = observed_receiver.recv().await.unwrap();
        assert!(matches!(
            second,
            ProviderRunProjection::ModelTurnStarted { .. }
        ));
        assert_eq!(runtime.requests().len(), 1);
        assert!(observed_receiver.try_recv().is_err());
        acknowledgement_sender.send(Ok(())).await.unwrap();

        loop {
            let projection = observed_receiver.recv().await.unwrap();
            let finished = matches!(projection, ProviderRunProjection::ModelTurnFinished { .. });
            acknowledgement_sender.send(Ok(())).await.unwrap();
            if finished {
                break;
            }
        }
    };

    let (block, ()) = futures::join!(drive, driver);
    assert!(matches!(block, ProviderRunBlock::AwaitingDriver { .. }));
}

fn assert_single_retry_lifecycle(
    projections: &[ProviderRunProjection],
    expected_timeout_stage: &str,
    expected_initial_start: bool,
) {
    let mut work_ids = Vec::new();
    let mut phases = Vec::new();
    let mut retry_error = None;
    for projection in projections {
        match projection {
            ProviderRunProjection::ModelTurnRequested {
                work_id,
                retry_attempt,
                ..
            } => {
                work_ids.push(work_id.clone());
                phases.push(format!("requested:{retry_attempt}"));
            }
            ProviderRunProjection::ModelTurnStarted {
                work_id,
                retry_attempt,
                ..
            } => {
                work_ids.push(work_id.clone());
                phases.push(format!("started:{retry_attempt}"));
            }
            ProviderRunProjection::ModelRetry {
                work_id,
                retry_attempt,
                error,
                ..
            } => {
                work_ids.push(work_id.clone());
                phases.push(format!("retry:{retry_attempt}"));
                retry_error = Some(error);
            }
            ProviderRunProjection::ModelTurnFinished {
                work_id,
                retry_attempt,
                ..
            } => {
                work_ids.push(work_id.clone());
                phases.push(format!("finished:{retry_attempt}"));
            }
            ProviderRunProjection::ModelEvent { .. }
            | ProviderRunProjection::ToolBatchReady { .. } => {}
        }
    }

    let expected = if expected_initial_start {
        vec![
            "requested:0",
            "started:0",
            "retry:1",
            "requested:1",
            "started:1",
            "finished:1",
        ]
    } else {
        vec![
            "requested:0",
            "retry:1",
            "requested:1",
            "started:1",
            "finished:1",
        ]
    };
    assert_eq!(phases, expected);
    assert!(work_ids.windows(2).all(|ids| ids[0] == ids[1]));
    let retry_error = retry_error.expect("timeout retry error");
    assert_eq!(retry_error.kind, AgentErrorKind::Transport);
    assert!(retry_error.recoverable);
    assert!(retry_error.message.contains(expected_timeout_stage));
}

#[tokio::test]
async fn model_start_timeout_retries_the_same_work_identity() {
    let runtime = Arc::new(StallingRuntime::new(FirstAttemptStall::Start));
    let mut coordinator = coordinator(runtime.clone());
    coordinator.set_model_timeouts(Duration::from_millis(10), Duration::from_secs(1));
    let mut projections = Vec::new();
    let (_sender, control) = turn_control();

    let block = coordinator
        .drive_until_blocked(control, collect_projection(&mut projections))
        .await
        .unwrap();

    assert!(matches!(block, ProviderRunBlock::AwaitingDriver { .. }));
    assert_eq!(runtime.requests().len(), 2);
    assert_eq!(
        runtime.attempt_commands(),
        vec![Some(TurnCommand::Cancel), None]
    );
    assert_eq!(coordinator.run().model_retries(), 1);
    assert_single_retry_lifecycle(&projections, "start timed out", false);
}

#[tokio::test]
async fn model_event_idle_timeout_retries_the_same_work_identity() {
    let runtime = Arc::new(StallingRuntime::new(FirstAttemptStall::Event));
    let mut coordinator = coordinator(runtime.clone());
    coordinator.set_model_timeouts(Duration::from_secs(1), Duration::from_millis(10));
    let mut projections = Vec::new();
    let (_sender, control) = turn_control();

    let block = coordinator
        .drive_until_blocked(control, collect_projection(&mut projections))
        .await
        .unwrap();

    assert!(matches!(block, ProviderRunBlock::AwaitingDriver { .. }));
    assert_eq!(runtime.requests().len(), 2);
    assert_eq!(
        runtime.attempt_commands(),
        vec![Some(TurnCommand::Cancel), None]
    );
    assert_eq!(coordinator.run().model_retries(), 1);
    assert_single_retry_lifecycle(&projections, "event timed out", true);
}

#[tokio::test]
async fn model_keepalive_preserves_the_active_turn_without_rendering_output() {
    let runtime = Arc::new(ScriptedRuntime::new(vec![Ok(vec![
        started("request-keepalive"),
        Ok(AgentEvent::KeepAlive),
        Ok(AgentEvent::TextDelta {
            text: "finished".to_string(),
        }),
        stopped(StopReason::Completed),
    ])]));
    let mut coordinator = coordinator(runtime);
    let mut projections = Vec::new();
    let (_sender, control) = turn_control();

    let block = coordinator
        .drive_until_blocked(control, collect_projection(&mut projections))
        .await
        .unwrap();

    assert!(matches!(block, ProviderRunBlock::AwaitingDriver { .. }));
    assert_eq!(coordinator.run().model_retries(), 0);
    assert!(!projections.iter().any(|projection| {
        matches!(
            projection,
            ProviderRunProjection::ModelEvent {
                event: AgentEvent::KeepAlive,
                ..
            }
        )
    }));
}

#[tokio::test]
async fn persistent_checkpoint_failure_terminates_without_redrive() {
    let runtime = Arc::new(ScriptedRuntime::new(vec![answer_turn()]));
    let mut coordinator = coordinator(runtime.clone());
    let checkpoint_attempts = Arc::new(AtomicUsize::new(0));
    let attempts = checkpoint_attempts.clone();
    let (_sender, control) = turn_control();

    let block = coordinator
        .drive_until_blocked_with_checkpoint(
            control,
            |_| Ok(()),
            move |_| {
                attempts.fetch_add(1, Ordering::Relaxed);
                Box::pin(async { Err("database unavailable".to_string()) })
            },
        )
        .await
        .unwrap();

    let ProviderRunBlock::Done(ProviderRunOutcome::Failed(failure)) = block else {
        panic!("checkpoint failure must terminate the drive");
    };
    assert_eq!(failure.kind, ProviderRunFailureKind::ExternalWork);
    assert!(failure.message.contains("database unavailable"));
    assert_eq!(checkpoint_attempts.load(Ordering::Relaxed), 1);
    assert!(runtime.requests().is_empty());
}

#[tokio::test]
async fn tool_projection_failure_synthesizes_a_result_and_fails_the_run() {
    let runtime = Arc::new(ScriptedRuntime::new(vec![tool_turn()]));
    let mut coordinator = coordinator(runtime);
    let (_sender, control) = turn_control();

    let block = coordinator
        .drive_until_blocked(control, |event| match event {
            ProviderRunProjection::ToolBatchReady { .. } => {
                Err("task projection disappeared".to_string())
            }
            ProviderRunProjection::ModelTurnRequested { .. }
            | ProviderRunProjection::ModelTurnStarted { .. }
            | ProviderRunProjection::ModelTurnFinished { .. }
            | ProviderRunProjection::ModelEvent { .. }
            | ProviderRunProjection::ModelRetry { .. } => Ok(()),
        })
        .await
        .unwrap();

    let ProviderRunBlock::Done(ProviderRunOutcome::Failed(failure)) = block else {
        panic!("projection failure must terminate the run visibly");
    };
    assert_eq!(failure.kind, ProviderRunFailureKind::Projection);
    let MessageContent::MultiPart(results) =
        &coordinator.run().transcript().last().unwrap().content
    else {
        panic!("pending tool must receive a synthetic error result");
    };
    assert!(matches!(
        results.as_slice(),
        [ContentPart::ToolResult {
            tool_use_id,
            is_error: true,
            ..
        }] if tool_use_id == "read-1"
    ));
}

#[tokio::test]
async fn stream_without_terminal_event_fails_instead_of_committing_partial_output() {
    let runtime = Arc::new(ScriptedRuntime::new(vec![Ok(vec![
        started("request-eof"),
        Ok(AgentEvent::TextDelta {
            text: "partial".to_string(),
        }),
    ])]));
    let mut coordinator = coordinator(runtime);
    let (_sender, control) = turn_control();

    let block = coordinator
        .drive_until_blocked(control, |_| Ok(()))
        .await
        .unwrap();

    let ProviderRunBlock::Done(ProviderRunOutcome::Failed(failure)) = block else {
        panic!("unexpected EOF must fail the run");
    };
    assert_eq!(failure.kind, ProviderRunFailureKind::ModelCall);
    assert_eq!(coordinator.run().transcript().len(), 1);
}

#[tokio::test]
async fn reasoning_completed_authoritatively_replaces_canonical_reasoning() {
    let runtime = Arc::new(ScriptedRuntime::new(vec![Ok(vec![
        started("request-reasoning"),
        Ok(AgentEvent::ReasoningDelta {
            text: "draft".to_string(),
        }),
        Ok(AgentEvent::ReasoningCompleted {
            text: "final".to_string(),
            signature: Some("signature".to_string()),
        }),
        stopped(StopReason::Completed),
    ])]));
    let mut coordinator = coordinator(runtime);
    let (_sender, control) = turn_control();

    let block = coordinator
        .drive_until_blocked(control, |_| Ok(()))
        .await
        .unwrap();

    assert!(matches!(block, ProviderRunBlock::AwaitingDriver { .. }));
    let MessageContent::MultiPart(parts) = &coordinator.run().transcript()[1].content else {
        panic!("expected canonical reasoning content");
    };
    assert!(matches!(
        parts.as_slice(),
        [ContentPart::Reasoning { text, signature }]
            if text == "final" && signature.as_deref() == Some("signature")
    ));
}

#[tokio::test]
async fn provider_cancellation_does_not_commit_partial_assistant_content() {
    let expected_transcript = request().messages;
    let runtime = Arc::new(ScriptedRuntime::new(vec![Ok(vec![
        started("request-cancelled"),
        Ok(AgentEvent::TextDelta {
            text: "partial".to_string(),
        }),
        stopped(StopReason::Cancelled),
    ])]));
    let mut coordinator = coordinator(runtime);
    let (_sender, control) = turn_control();

    let block = coordinator
        .drive_until_blocked(control, |_| Ok(()))
        .await
        .unwrap();

    assert_eq!(
        block,
        ProviderRunBlock::Done(ProviderRunOutcome::Cancelled {
            reason: "provider model call was cancelled".to_string(),
        })
    );
    assert_eq!(coordinator.run().transcript(), expected_transcript);
}

#[tokio::test]
async fn transcript_projector_emits_one_ui_stream_for_the_whole_run() {
    let runtime = Arc::new(ScriptedRuntime::new(vec![answer_turn()]));
    let mut coordinator = coordinator(runtime);
    let mut projector = ProviderRunResponseProjector::new(RuntimeResponseConfig {
        task_id: "task".to_string(),
        conversation_id: "conversation".to_string(),
        needs_create_task: true,
        user_query: Some("Inspect and fix the issue".to_string()),
        model_id: "test-model".to_string(),
        max_context_tokens: Some(100_000),
        capabilities: RuntimeCapabilities::provider(),
        empty_output_message: None,
    });
    let mut ui_events = Vec::new();
    let (_sender, control) = turn_control();

    let block = coordinator
        .drive_until_blocked(control, |projection| {
            ui_events.extend(projector.project(projection)?);
            Ok(())
        })
        .await
        .unwrap();
    let ProviderRunBlock::AwaitingDriver { work_id, .. } = block else {
        panic!("expected driver decision");
    };

    assert_eq!(
        count_response_events(&ui_events, ResponseEventKind::Init),
        1
    );
    assert_eq!(
        count_response_events(&ui_events, ResponseEventKind::Finished),
        0
    );
    assert!(ui_events
        .iter()
        .any(|event| matches!(event.r#type, Some(response_event::Type::ClientActions(_)))));

    coordinator.run_mut().complete(&work_id).unwrap();
    let (_sender, control) = turn_control();
    let ProviderRunBlock::Done(outcome) = coordinator
        .drive_until_blocked(control, |_| Ok(()))
        .await
        .unwrap()
    else {
        panic!("expected terminal run");
    };
    ui_events.extend(
        projector
            .finish(&outcome, coordinator.run().usage())
            .unwrap(),
    );
    assert_eq!(
        count_response_events(&ui_events, ResponseEventKind::Finished),
        1
    );
}

#[test]
fn transcript_projector_preserves_provider_failure_message() {
    let mut projector = ProviderRunResponseProjector::new(RuntimeResponseConfig {
        task_id: "task".to_string(),
        conversation_id: "conversation".to_string(),
        needs_create_task: false,
        user_query: None,
        model_id: "test-model".to_string(),
        max_context_tokens: Some(100_000),
        capabilities: RuntimeCapabilities::provider(),
        empty_output_message: None,
    });
    let events = projector
        .finish(
            &ProviderRunOutcome::Failed(ProviderRunFailure {
                kind: ProviderRunFailureKind::ModelCall,
                message: "upstream provider rejected the request".to_string(),
                source: None,
            }),
            &Usage::default(),
        )
        .unwrap();

    let finished = events
        .iter()
        .find_map(|event| match &event.r#type {
            Some(response_event::Type::Finished(finished)) => Some(finished),
            Some(response_event::Type::Init(_))
            | Some(response_event::Type::ClientActions(_))
            | None => None,
        })
        .expect("terminal provider failure");
    assert!(matches!(
        &finished.reason,
        Some(response_event::stream_finished::Reason::InternalError(error))
            if error.message == "upstream provider rejected the request"
    ));
}

#[derive(Clone, Copy)]
enum ResponseEventKind {
    Init,
    Finished,
}

fn count_response_events(
    events: &[warp_multi_agent_api::ResponseEvent],
    kind: ResponseEventKind,
) -> usize {
    events
        .iter()
        .filter(|event| match (&event.r#type, kind) {
            (Some(response_event::Type::Init(_)), ResponseEventKind::Init)
            | (Some(response_event::Type::Finished(_)), ResponseEventKind::Finished) => true,
            (Some(response_event::Type::ClientActions(_)), ResponseEventKind::Init)
            | (Some(response_event::Type::ClientActions(_)), ResponseEventKind::Finished)
            | (Some(response_event::Type::Init(_)), ResponseEventKind::Finished)
            | (Some(response_event::Type::Finished(_)), ResponseEventKind::Init)
            | (None, ResponseEventKind::Init)
            | (None, ResponseEventKind::Finished) => false,
        })
        .count()
}

#[test]
fn session_runtime_is_rejected_before_any_turn_can_start() {
    struct SessionRuntime {
        descriptor: RuntimeDescriptor,
    }

    #[async_trait]
    impl AgentRuntime for SessionRuntime {
        fn descriptor(&self) -> &RuntimeDescriptor {
            &self.descriptor
        }

        async fn start_turn(
            &self,
            _request: TurnRequest,
            _control: TurnControl,
        ) -> Result<AgentEventStream, AgentError> {
            unreachable!()
        }
    }

    let runtime = Arc::new(SessionRuntime {
        descriptor: RuntimeDescriptor {
            id: "session".to_string(),
            display_name: "Session runtime".to_string(),
            kind: RuntimeKind::Acp,
            capabilities: RuntimeCapabilities::session_runtime(),
        },
    });
    let error = ProviderRunCoordinator::from_request(
        "run-1",
        ProviderRunProfile::new(runtime, request()),
        Vec::new(),
        ProviderRunLimits::default(),
    )
    .err()
    .expect("session runtime must be rejected");

    assert!(matches!(
        error,
        ProviderRunCoordinatorError::InvalidRuntime(_)
    ));
}

#[test]
fn context_estimate_tracks_live_transcript_and_reserves_fixed_context() {
    let runtime = Arc::new(ScriptedRuntime::new(vec![]));
    let mut request = request();
    request.system_prompt = Some("project instructions".repeat(100));
    request.max_output_tokens = Some(4_000);
    let mut coordinator = ProviderRunCoordinator::from_request(
        "context-budget",
        ProviderRunProfile::new(runtime, request),
        Vec::new(),
        ProviderRunLimits::default(),
    )
    .unwrap();
    let (before, fixed_before) = coordinator.estimated_context_tokens().unwrap();
    assert!(fixed_before > 4_000);
    coordinator
        .run_mut()
        .compact_transcript_at_model_boundary(
            1,
            vec![ConversationMessage {
                role: MessageRole::User,
                content: MessageContent::Text("new large tool output".repeat(1_000)),
            }],
        )
        .unwrap();
    let (after, fixed_after) = coordinator.estimated_context_tokens().unwrap();
    assert!(after > before + 4_000);
    assert_eq!(fixed_before, fixed_after);
}

#[tokio::test]
async fn monitor_request_uses_its_profile_context_limit() {
    let base_runtime = Arc::new(ScriptedRuntime::new(vec![answer_turn()]));
    let cli_runtime = Arc::new(ScriptedRuntime::new(vec![answer_turn()]));
    let original_content = format!("start:{}:end", "x".repeat(20_000));
    let large_result = ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::ToolResult {
            tool_use_id: "large-result".to_string(),
            content: original_content.clone(),
            is_error: false,
        },
    };
    let run = ProviderRun::new(
        "profile-limit-run",
        vec![large_result],
        CLI_MONITOR_PROVIDER_PROFILE,
        ProviderRunLimits::default(),
    );
    let mut profiles = BTreeMap::new();
    profiles.insert(
        BASE_PROVIDER_PROFILE.to_string(),
        ProviderRunProfile::new(base_runtime, request()).with_max_context_tokens(Some(100_000)),
    );
    profiles.insert(
        CLI_MONITOR_PROVIDER_PROFILE.to_string(),
        ProviderRunProfile::new(cli_runtime.clone(), request())
            .with_max_context_tokens(Some(8_000)),
    );
    let mut coordinator = ProviderRunCoordinator::new(run, profiles).unwrap();
    coordinator.set_max_context_tokens(Some(100_000));

    let (_sender, control) = turn_control();
    let block = coordinator
        .drive_until_blocked(control, |_| Ok(()))
        .await
        .unwrap();

    assert!(matches!(block, ProviderRunBlock::AwaitingDriver { .. }));
    let requests = cli_runtime.requests();
    let MessageContent::ToolResult { content, .. } = &requests[0].messages[0].content else {
        panic!("expected bounded monitor tool result");
    };
    assert!(content.chars().count() <= galaxy_agent_core::provider_output_char_limit(Some(8_000)));
    assert!(content.contains("tool result truncated"));
    let MessageContent::ToolResult { content, .. } = &coordinator.run().transcript()[0].content
    else {
        panic!("expected original monitor tool result");
    };
    assert_eq!(content, &original_content);
}

#[tokio::test]
async fn final_model_request_bounds_restored_tagged_terminal_text() {
    let runtime = Arc::new(ScriptedRuntime::new(vec![answer_turn()]));
    let original_output = format!("first:{}:last", "x".repeat(20_000));
    let restored_message = ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::Text(format!(
            "[Running command: verbose]\n[Terminal output:\n{original_output}\n]\nContinue"
        )),
    };
    let mut turn_request = request();
    turn_request.messages = vec![restored_message];
    let mut coordinator = ProviderRunCoordinator::from_request(
        "restored-terminal-run",
        ProviderRunProfile::new(runtime.clone(), turn_request).with_max_context_tokens(Some(8_000)),
        Vec::new(),
        ProviderRunLimits::default(),
    )
    .unwrap();

    let (_sender, control) = turn_control();
    let block = coordinator
        .drive_until_blocked(control, |_| Ok(()))
        .await
        .unwrap();

    assert!(matches!(block, ProviderRunBlock::AwaitingDriver { .. }));
    let requests = runtime.requests();
    let MessageContent::Text(content) = &requests[0].messages[0].content else {
        panic!("expected bounded restored terminal text");
    };
    assert!(content.contains("terminal output truncated for model context"));
    assert!(!content.contains(&original_output));
    let MessageContent::Text(content) = &coordinator.run().transcript()[0].content else {
        panic!("expected original restored terminal text");
    };
    assert!(content.contains(&original_output));
}

#[tokio::test]
async fn final_model_request_bounds_new_tool_results_to_the_active_context() {
    let runtime = Arc::new(ScriptedRuntime::new(vec![answer_turn()]));
    let original_content = format!("start:{}:end", "界".repeat(20_000));
    let large_result = ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::ToolResult {
            tool_use_id: "large-result".to_string(),
            content: original_content.clone(),
            is_error: false,
        },
    };
    let mut turn_request = request();
    turn_request.messages = vec![large_result];
    let mut coordinator = ProviderRunCoordinator::from_request(
        "large-result-run",
        ProviderRunProfile::new(runtime.clone(), turn_request),
        Vec::new(),
        ProviderRunLimits::default(),
    )
    .unwrap();
    coordinator.set_max_context_tokens(Some(8_000));

    let (_sender, control) = turn_control();
    let block = coordinator
        .drive_until_blocked(control, |_| Ok(()))
        .await
        .unwrap();

    assert!(matches!(block, ProviderRunBlock::AwaitingDriver { .. }));
    let requests = runtime.requests();
    let MessageContent::ToolResult { content, .. } = &requests[0].messages[0].content else {
        panic!("expected bounded tool result");
    };
    assert!(content.chars().count() <= galaxy_agent_core::provider_output_char_limit(Some(8_000)));
    assert!(content.contains("tool result truncated"));
    let MessageContent::ToolResult { content, .. } = &coordinator.run().transcript()[0].content
    else {
        panic!("expected original tool result");
    };
    assert_eq!(content, &original_content);
}

#[tokio::test]
async fn preflight_leaves_headroom_before_the_advertised_context_limit() {
    let runtime = Arc::new(ScriptedRuntime::new(vec![answer_turn()]));
    let mut coordinator = coordinator(runtime.clone());
    let (estimated_tokens, _) = coordinator.estimated_context_tokens().unwrap();
    coordinator.set_max_context_tokens(Some(estimated_tokens + 1));
    let (_sender, control) = turn_control();
    let block = coordinator
        .drive_until_blocked(control, |_| Ok(()))
        .await
        .unwrap();
    assert_eq!(block, ProviderRunBlock::ContextWindowExceeded);
    assert!(runtime.requests().is_empty());
}

#[test]
fn request_estimate_accounts_for_multibyte_text() {
    let mut request = request();
    request.system_prompt = Some("a".repeat(1_000));
    let ascii_tokens = estimate_turn_request_tokens(&request);
    request.system_prompt = Some("界".repeat(1_000));
    assert!(estimate_turn_request_tokens(&request) > ascii_tokens + 600);
}

#[test]
fn discovery_reminder_counts_batches_and_resets_on_mutation_or_user_input() {
    let read = ConversationMessage {
        role: MessageRole::Assistant,
        content: MessageContent::MultiPart(vec![
            ContentPart::ToolUse {
                tool_use_id: "read".into(),
                name: "read_files".into(),
                input: serde_json::json!({}),
            },
            ContentPart::ToolUse {
                tool_use_id: "search".into(),
                name: "search_codebase".into(),
                input: serde_json::json!({}),
            },
        ]),
    };
    let mut messages = vec![read.clone(); 4];
    assert_eq!(consecutive_discovery_batches(&messages), 4);
    messages.push(ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::ToolResult {
            tool_use_id: "read".into(),
            content: "contents".into(),
            is_error: false,
        },
    });
    assert_eq!(consecutive_discovery_batches(&messages), 4);
    messages.push(ConversationMessage {
        role: MessageRole::Assistant,
        content: MessageContent::ToolUse {
            tool_use_id: "edit".into(),
            name: "edit_files".into(),
            input: serde_json::json!({}),
        },
    });
    messages.push(read);
    assert_eq!(consecutive_discovery_batches(&messages), 1);
    messages.push(ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::Text("New request".into()),
    });
    assert_eq!(consecutive_discovery_batches(&messages), 0);
}

#[test]
fn discovery_reminder_is_advisory_periodic_and_excludes_other_modes() {
    let mut request = request();
    request.messages = vec![
        ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::ToolUse {
                tool_use_id: "read".into(),
                name: "read_files".into(),
                input: serde_json::json!({})
            },
        };
        4
    ];
    let unchanged = request.clone();
    add_exploration_reminder(&mut request);
    assert_eq!(request, unchanged);
    request.system_prompt =
        Some(crate::ai::provider::request_translator::IMPLEMENTATION_PROGRESS_GUIDANCE.into());
    let original = request.clone();
    add_exploration_reminder(&mut request);
    assert!(request
        .system_prompt
        .as_ref()
        .unwrap()
        .contains("Implementation progress checkpoint"));
    assert_eq!(request.messages, original.messages);
    assert_eq!(request.tools, original.tools);
    let mut fifth = original;
    fifth.messages.push(fifth.messages[0].clone());
    let expected = fifth.clone();
    add_exploration_reminder(&mut fifth);
    assert_eq!(fifth, expected);
}
