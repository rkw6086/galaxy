use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use ai::skills::SkillPathOrigin;
use async_trait::async_trait;
use futures::channel::oneshot;
use futures::StreamExt;
use galaxy_agent_core::{
    AgentError, AgentErrorKind, AgentEvent, AgentEventStream, AgentRuntime, ContentPart,
    ConversationMessage, MessageContent, MessageRole, RuntimeCapabilities, RuntimeDescriptor,
    RuntimeKind, StopReason, ToolCall, ToolDefinition, ToolEvent, ToolResult, ToolResultStatus,
    TurnControl, TurnRequest, Usage, RECALL_TOOL_HISTORY_NAME,
};
use warp_multi_agent_api::{client_action, message, response_event};

use super::{
    append_tool_result, build_tool_proposed, rig_response_stream, sync_assistant_turn,
    PreparedRigTurn, INLINE_TOOL_LOOP_MESSAGE, MAX_INLINE_TOOL_CONTINUATIONS,
};
use crate::ai::agent::api::StreamEvent;

#[test]
fn tool_proposal_matches_the_domain_permission_contract() {
    let action = build_tool_proposed(
        "task",
        &ToolCall {
            id: "call-1".to_string(),
            name: "run_shell_command".to_string(),
            arguments: serde_json::json!({
                "command": "cargo test",
                "is_read_only": true
            }),
        },
        &SkillPathOrigin::Local,
        &HashMap::new(),
    )
    .unwrap();

    assert_eq!(action.id.to_string(), "call-1");
    assert!(matches!(
        action.action,
        crate::ai::agent::AIAgentActionType::RequestCommandOutput {
            command,
            is_read_only: Some(true),
            ..
        } if command == "cargo test"
    ));
}

#[test]
fn mcp_tool_proposal_routes_directly_to_the_mcp_executor_contract() {
    let action = build_tool_proposed(
        "task",
        &ToolCall {
            id: "call-mcp".to_string(),
            name: "mcp__11111111-1111-4111-8111-111111111111__read_file".to_string(),
            arguments: serde_json::json!({"path": "Cargo.toml"}),
        },
        &SkillPathOrigin::Local,
        &HashMap::new(),
    )
    .unwrap();

    assert!(matches!(
        action.action,
        crate::ai::agent::AIAgentActionType::CallMCPTool {
            server_id: Some(server_id),
            name,
            ..
        } if server_id.to_string() == "11111111-1111-4111-8111-111111111111"
            && name == "read_file"
    ));
}

#[test]
fn assistant_history_is_updated_before_fast_tool_execution_can_continue() {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let mut history_index = None;
    let first_call = ToolCall {
        id: "call-1".to_string(),
        name: "read_files".to_string(),
        arguments: serde_json::json!({"files": ["Cargo.toml"]}),
    };
    let second_call = ToolCall {
        id: "call-2".to_string(),
        name: "grep".to_string(),
        arguments: serde_json::json!({"queries": ["rig"]}),
    };

    sync_assistant_turn(
        &messages,
        "",
        None,
        "I'll inspect both.",
        std::slice::from_ref(&first_call),
        &mut history_index,
    );
    sync_assistant_turn(
        &messages,
        "",
        None,
        "I'll inspect both.",
        &[first_call, second_call],
        &mut history_index,
    );

    let messages = messages.lock().unwrap();
    assert_eq!(messages.len(), 1);
    let MessageContent::MultiPart(parts) = &messages[0].content else {
        panic!("expected combined assistant content");
    };
    assert_eq!(parts.len(), 3);
    assert!(
        matches!(&parts[0], galaxy_agent_core::ContentPart::Text(text) if text == "I'll inspect both.")
    );
    assert!(
        matches!(&parts[1], galaxy_agent_core::ContentPart::ToolUse { tool_use_id, .. } if tool_use_id == "call-1")
    );
    assert!(
        matches!(&parts[2], galaxy_agent_core::ContentPart::ToolUse { tool_use_id, .. } if tool_use_id == "call-2")
    );
}

#[test]
fn signed_reasoning_is_persisted_before_the_tool_call() {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let mut history_index = None;
    let call = ToolCall {
        id: "call-1".to_string(),
        name: "read_files".to_string(),
        arguments: serde_json::json!({"files": ["Cargo.toml"]}),
    };

    sync_assistant_turn(
        &messages,
        "I should inspect the manifest.",
        Some("signed-reasoning"),
        "",
        std::slice::from_ref(&call),
        &mut history_index,
    );

    let messages = messages.lock().unwrap();
    let MessageContent::MultiPart(parts) = &messages[0].content else {
        panic!("expected reasoning and tool call parts");
    };
    assert!(matches!(
        parts.as_slice(),
        [
            ContentPart::Reasoning {
                text,
                signature: Some(signature),
            },
            ContentPart::ToolUse { tool_use_id, .. },
        ] if text == "I should inspect the manifest."
            && signature == "signed-reasoning"
            && tool_use_id == "call-1"
    ));
}

#[test]
fn rejected_tool_result_is_paired_with_the_assistant_call_in_history() {
    let messages = Arc::new(Mutex::new(Vec::new()));
    let mut history_index = None;
    let call = ToolCall {
        id: "call-unknown".to_string(),
        name: "invented_tool".to_string(),
        arguments: serde_json::json!({}),
    };
    sync_assistant_turn(
        &messages,
        "",
        None,
        "",
        std::slice::from_ref(&call),
        &mut history_index,
    );
    append_tool_result(
        &messages,
        ToolResult {
            call_id: call.id.clone(),
            content: "tool is unavailable".to_string(),
            status: ToolResultStatus::Error,
        },
    );

    let messages = messages.lock().unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(messages[0].role, MessageRole::Assistant);
    assert!(matches!(
        &messages[0].content,
        MessageContent::ToolUse {
            tool_use_id,
            name,
            ..
        } if tool_use_id == "call-unknown" && name == "invented_tool"
    ));
    assert_eq!(messages[1].role, MessageRole::User);
    assert!(matches!(
        &messages[1].content,
        MessageContent::ToolResult {
            tool_use_id,
            content,
            is_error: true,
        } if tool_use_id == "call-unknown" && content == "tool is unavailable"
    ));
}

struct ScriptedRuntime {
    descriptor: RuntimeDescriptor,
    turns: Mutex<VecDeque<Vec<AgentEvent>>>,
    requests: Arc<Mutex<Vec<TurnRequest>>>,
}

impl ScriptedRuntime {
    fn new(turns: Vec<Vec<AgentEvent>>, requests: Arc<Mutex<Vec<TurnRequest>>>) -> Self {
        Self {
            descriptor: RuntimeDescriptor {
                id: "scripted-provider".to_string(),
                display_name: "Scripted provider".to_string(),
                kind: RuntimeKind::Provider,
                capabilities: RuntimeCapabilities::provider(),
            },
            turns: Mutex::new(turns.into()),
            requests,
        }
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
        let events = self.turns.lock().unwrap().pop_front().ok_or_else(|| {
            AgentError::new(
                AgentErrorKind::Protocol,
                "scripted provider ran out of turns",
            )
        })?;
        Ok(Box::pin(futures::stream::iter(events.into_iter().map(Ok))))
    }
}

fn recall_turn(index: usize) -> Vec<AgentEvent> {
    vec![
        AgentEvent::TurnStarted {
            runtime_request_id: format!("request-{index}"),
        },
        AgentEvent::Tool {
            event: ToolEvent::Proposed {
                call: ToolCall {
                    id: format!("recall-{index}"),
                    name: RECALL_TOOL_HISTORY_NAME.to_string(),
                    arguments: serde_json::json!({"search_query": "missing"}),
                },
            },
        },
        AgentEvent::UsageUpdated {
            usage: Usage {
                input_tokens: 10,
                output_tokens: 1,
                ..Usage::default()
            },
        },
        AgentEvent::TurnStopped {
            reason: StopReason::Completed,
        },
    ]
}

fn answer_turn() -> Vec<AgentEvent> {
    vec![
        AgentEvent::TurnStarted {
            runtime_request_id: "request-answer".to_string(),
        },
        AgentEvent::TextDelta {
            text: "Continuing with the answer.".to_string(),
        },
        AgentEvent::UsageUpdated {
            usage: Usage {
                input_tokens: 20,
                output_tokens: 3,
                ..Usage::default()
            },
        },
        AgentEvent::TurnStopped {
            reason: StopReason::Completed,
        },
    ]
}

fn prepared_turn(messages_sent: Arc<Mutex<Vec<ConversationMessage>>>) -> PreparedRigTurn {
    let initial_messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::Text("Inspect the issue.".to_string()),
    }];
    let mut request = TurnRequest::new("test-model", initial_messages.clone());
    request.conversation_id = Some("conversation".to_string());
    request.tools = vec![ToolDefinition {
        name: RECALL_TOOL_HISTORY_NAME.to_string(),
        description: "Recall prior tool output".to_string(),
        input_schema: serde_json::json!({"type": "object"}),
    }];
    PreparedRigTurn {
        task_id: "task".to_string(),
        needs_create_task: false,
        user_query: None,
        request,
        persistent_messages: initial_messages,
        tool_result_archive: Vec::new(),
        messages_sent,
        mcp_tool_aliases: HashMap::new(),
    }
}

async fn run_scripted_turn(
    turns: Vec<Vec<AgentEvent>>,
) -> (Vec<StreamEvent>, Vec<TurnRequest>, Vec<ConversationMessage>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let messages_sent = Arc::new(Mutex::new(Vec::new()));
    let runtime = ScriptedRuntime::new(turns, requests.clone());
    let (cancel_tx, cancellation_rx) = oneshot::channel();
    let events = rig_response_stream(
        runtime,
        prepared_turn(messages_sent.clone()),
        SkillPathOrigin::Local,
        Some(100_000),
        "scripted",
        cancellation_rx,
    )
    .collect::<Vec<_>>()
    .await
    .into_iter()
    .collect::<Result<Vec<_>, _>>()
    .expect("scripted response should succeed");
    drop(cancel_tx);
    let requests = requests.lock().unwrap().clone();
    let messages_sent = messages_sent.lock().unwrap().clone();
    (events, requests, messages_sent)
}

fn agent_output_texts(events: &[StreamEvent]) -> Vec<&str> {
    let mut texts = Vec::new();
    for event in events {
        let StreamEvent::Response(response) = event else {
            continue;
        };
        let Some(response_event::Type::ClientActions(actions)) = &response.r#type else {
            continue;
        };
        for action in &actions.actions {
            let Some(client_action::Action::AddMessagesToTask(add)) = &action.action else {
                continue;
            };
            for message in &add.messages {
                if let Some(message::Message::AgentOutput(output)) = &message.message {
                    texts.push(output.text.as_str());
                }
            }
        }
    }
    texts
}

#[tokio::test]
async fn inline_recall_starts_a_followup_provider_turn_with_the_paired_result() {
    let (events, requests, messages_sent) =
        run_scripted_turn(vec![recall_turn(1), answer_turn()]).await;

    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].messages.len(), 3);
    assert!(matches!(
        &requests[1].messages[1].content,
        MessageContent::ToolUse {
            tool_use_id,
            name,
            ..
        } if tool_use_id == "recall-1" && name == RECALL_TOOL_HISTORY_NAME
    ));
    assert!(matches!(
        &requests[1].messages[2].content,
        MessageContent::ToolResult {
            tool_use_id,
            content,
            is_error: false,
        } if tool_use_id == "recall-1"
            && content == "No matching tool calls found in conversation history."
    ));
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                StreamEvent::Response(response)
                    if matches!(response.r#type, Some(response_event::Type::Init(_)))
            ))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(
                event,
                StreamEvent::Response(response)
                    if matches!(response.r#type, Some(response_event::Type::Finished(_)))
            ))
            .count(),
        1
    );
    assert!(events
        .iter()
        .all(|event| !matches!(event, StreamEvent::ToolProposed(_))));
    assert_eq!(agent_output_texts(&events), ["Continuing with the answer."]);

    let finished = events.iter().find_map(|event| {
        let StreamEvent::Response(response) = event else {
            return None;
        };
        let Some(response_event::Type::Finished(finished)) = &response.r#type else {
            return None;
        };
        Some(finished)
    });
    let finished = finished.expect("stream should finish");
    assert_eq!(finished.token_usage[0].total_input, 30);
    assert_eq!(finished.token_usage[0].output, 4);
    assert!(matches!(
        messages_sent.last().map(|message| &message.content),
        Some(MessageContent::Text(text)) if text == "Continuing with the answer."
    ));
}

#[tokio::test]
async fn repeated_inline_recall_stops_with_a_visible_loop_limit_message() {
    let turns = (0..=MAX_INLINE_TOOL_CONTINUATIONS)
        .map(recall_turn)
        .collect();
    let (events, requests, messages_sent) = run_scripted_turn(turns).await;

    assert_eq!(requests.len(), MAX_INLINE_TOOL_CONTINUATIONS + 1);
    assert!(agent_output_texts(&events).contains(&INLINE_TOOL_LOOP_MESSAGE));
    assert!(matches!(
        messages_sent.last().map(|message| &message.content),
        Some(MessageContent::Text(text)) if text == INLINE_TOOL_LOOP_MESSAGE
    ));
    let finished = events.iter().find_map(|event| {
        let StreamEvent::Response(response) = event else {
            return None;
        };
        let Some(response_event::Type::Finished(finished)) = &response.r#type else {
            return None;
        };
        Some(finished)
    });
    assert!(matches!(
        finished.and_then(|finished| finished.reason.as_ref()),
        Some(response_event::stream_finished::Reason::Other(_))
    ));
}
