use crate::ai::provider::types::{ContentPart, ConversationMessage, MessageContent, MessageRole};

/// Sanitizes messages for OpenAI API compatibility.
///
/// OpenAI is more lenient than Bedrock — it doesn't require strict user/assistant
/// alternation and allows system messages anywhere. The main constraints are:
/// - Tool results must reference a valid tool_call_id from a preceding assistant message
/// - Tool calls in assistant messages must eventually have matching tool results
pub fn sanitize_messages_for_openai(messages: &mut Vec<ConversationMessage>) {
    remove_orphaned_tool_results(messages);
    synthesize_missing_tool_results(messages);
}

/// Removes tool_result messages that reference tool_use_ids not found in any
/// preceding assistant message.
fn remove_orphaned_tool_results(messages: &mut Vec<ConversationMessage>) {
    let mut known_tool_use_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    // First pass: collect all tool_use_ids from assistant messages
    for msg in messages.iter() {
        if msg.role != MessageRole::Assistant {
            continue;
        }
        collect_tool_use_ids(&msg.content, &mut known_tool_use_ids);
    }

    // Second pass: remove tool_results that reference unknown IDs
    messages.retain(|msg| {
        if msg.role != MessageRole::User {
            return true;
        }
        match &msg.content {
            MessageContent::ToolResult { tool_use_id, .. } => {
                known_tool_use_ids.contains(tool_use_id)
            }
            MessageContent::MultiPart(parts) => {
                // Keep the message if it has at least one non-orphaned part
                parts.iter().any(|part| match part {
                    ContentPart::ToolResult { tool_use_id, .. } => {
                        known_tool_use_ids.contains(tool_use_id)
                    }
                    _ => true,
                })
            }
            _ => true,
        }
    });
}

/// For any assistant tool_use that doesn't have a matching tool_result in a
/// subsequent user message, synthesize an error result.
fn synthesize_missing_tool_results(messages: &mut Vec<ConversationMessage>) {
    let mut pending_tool_use_ids: Vec<(String, usize)> = Vec::new();
    let mut answered_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Collect all tool_use IDs and all answered IDs
    for (i, msg) in messages.iter().enumerate() {
        match msg.role {
            MessageRole::Assistant => {
                collect_tool_use_ids_with_index(&msg.content, i, &mut pending_tool_use_ids);
            }
            MessageRole::User => {
                collect_tool_result_ids(&msg.content, &mut answered_ids);
            }
        }
    }

    // Find unanswered tool_uses and synthesize results
    let mut synthetic_results: Vec<ConversationMessage> = Vec::new();
    for (tool_use_id, _) in pending_tool_use_ids {
        if !answered_ids.contains(&tool_use_id) {
            synthetic_results.push(ConversationMessage {
                role: MessageRole::User,
                content: MessageContent::ToolResult {
                    tool_use_id,
                    content: "Tool call result unavailable (conversation was interrupted)."
                        .to_string(),
                    is_error: true,
                },
            });
        }
    }

    if !synthetic_results.is_empty() {
        messages.extend(synthetic_results);
    }
}

fn collect_tool_use_ids(content: &MessageContent, ids: &mut std::collections::HashSet<String>) {
    match content {
        MessageContent::ToolUse { tool_use_id, .. } => {
            ids.insert(tool_use_id.clone());
        }
        MessageContent::MultiPart(parts) => {
            for part in parts {
                if let ContentPart::ToolUse { tool_use_id, .. } = part {
                    ids.insert(tool_use_id.clone());
                }
            }
        }
        _ => {}
    }
}

fn collect_tool_use_ids_with_index(
    content: &MessageContent,
    index: usize,
    ids: &mut Vec<(String, usize)>,
) {
    match content {
        MessageContent::ToolUse { tool_use_id, .. } => {
            ids.push((tool_use_id.clone(), index));
        }
        MessageContent::MultiPart(parts) => {
            for part in parts {
                if let ContentPart::ToolUse { tool_use_id, .. } = part {
                    ids.push((tool_use_id.clone(), index));
                }
            }
        }
        _ => {}
    }
}

fn collect_tool_result_ids(content: &MessageContent, ids: &mut std::collections::HashSet<String>) {
    match content {
        MessageContent::ToolResult { tool_use_id, .. } => {
            ids.insert(tool_use_id.clone());
        }
        MessageContent::MultiPart(parts) => {
            for part in parts {
                if let ContentPart::ToolResult { tool_use_id, .. } = part {
                    ids.insert(tool_use_id.clone());
                }
            }
        }
        _ => {}
    }
}
