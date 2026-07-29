use std::sync::{Arc, Mutex};

use bytes::Bytes;
use futures::{stream, StreamExt};
use serde_json::json;
use warp_multi_agent_api as api;

use super::response_translator::{
    is_known_tool, openai_stream_to_response_events, OpenAIStreamContext,
};
use crate::ai::provider::types::{ConversationMessage, MessageContent, MessageRole};

async fn run_recall_tool_call() -> (Vec<api::ResponseEvent>, Vec<ConversationMessage>) {
    let arguments = json!({"tool_use_id": "previous-tool-use"}).to_string();
    let chunk = json!({
        "choices": [{
            "delta": {
                "tool_calls": [{
                    "index": 0,
                    "id": "recall-tool-use",
                    "function": {
                        "name": "recall_tool_history",
                        "arguments": arguments,
                    },
                }],
            },
            "finish_reason": "tool_calls",
        }],
    });
    let sse = format!("data: {chunk}\n\ndata: [DONE]\n\n");
    let byte_stream = stream::iter(vec![Ok::<Bytes, reqwest::Error>(Bytes::from(sse))]);
    let messages_sent = Arc::new(Mutex::new(Vec::new()));
    let archive = vec![
        ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::ToolUse {
                tool_use_id: "previous-tool-use".to_string(),
                name: "run_shell_command".to_string(),
                input: json!({"command": "cargo test"}),
            },
        },
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::ToolResult {
                tool_use_id: "previous-tool-use".to_string(),
                content: "all tests passed".to_string(),
                is_error: false,
            },
        },
    ];

    let events = openai_stream_to_response_events(
        byte_stream,
        OpenAIStreamContext {
            task_id: "task-1".to_string(),
            needs_create_task: false,
            user_query: None,
            messages_sent: messages_sent.clone(),
            model_id: "test-model".to_string(),
            max_context_tokens: Some(100_000),
            tool_result_archive: archive,
        },
    )
    .collect::<Vec<_>>()
    .await
    .into_iter()
    .collect::<Result<Vec<_>, _>>()
    .expect("stream should succeed");
    let history = messages_sent
        .lock()
        .expect("history lock should not be poisoned")
        .clone();

    (events, history)
}

fn has_tool_call(event: &api::ResponseEvent) -> bool {
    let Some(api::response_event::Type::ClientActions(client_actions)) = &event.r#type else {
        return false;
    };

    client_actions.actions.iter().any(|action| {
        let Some(api::client_action::Action::AddMessagesToTask(add_messages)) = &action.action
        else {
            return false;
        };
        add_messages
            .messages
            .iter()
            .any(|message| matches!(&message.message, Some(api::message::Message::ToolCall(_))))
    })
}

#[tokio::test]
async fn recall_tool_history_uses_archive_and_stores_paired_result() {
    let (_, history) = run_recall_tool_call().await;

    assert_eq!(history.len(), 2);
    let MessageContent::ToolUse {
        tool_use_id,
        name,
        input,
    } = &history[0].content
    else {
        panic!("expected assistant tool use");
    };
    assert_eq!(history[0].role, MessageRole::Assistant);
    assert_eq!(tool_use_id, "recall-tool-use");
    assert_eq!(name, "recall_tool_history");
    assert_eq!(input, &json!({"tool_use_id": "previous-tool-use"}));

    let MessageContent::ToolResult {
        tool_use_id,
        content,
        is_error,
    } = &history[1].content
    else {
        panic!("expected paired user tool result");
    };
    assert_eq!(history[1].role, MessageRole::User);
    assert_eq!(tool_use_id, "recall-tool-use");
    assert!(!is_error);
    assert!(content.contains("Tool: run_shell_command"));
    assert!(content.contains("Tool Use ID: previous-tool-use"));
    assert!(content.contains("all tests passed"));
}

#[tokio::test]
async fn recall_tool_history_does_not_emit_a_client_tool_call() {
    let (events, _) = run_recall_tool_call().await;

    assert!(!events.iter().any(has_tool_call));
}

#[test]
fn direct_provider_known_tools_exclude_hosted_only_tools() {
    assert!(!is_known_tool("send_message_to_agent"));
    assert!(!is_known_tool("suggest_next_prompt"));
    assert!(is_known_tool("recall_tool_history"));
    assert!(is_known_tool("interrupt_shell_command"));
}
