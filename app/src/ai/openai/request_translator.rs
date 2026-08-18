use std::collections::HashSet;

use crate::ai::provider::types::{ContentPart, ConversationMessage, MessageContent, MessageRole};

/// Sanitizes messages for OpenAI API compatibility.
///
/// OpenAI is more lenient than Bedrock — it doesn't require strict user/assistant
/// alternation and allows system messages anywhere. The main constraints are:
/// - Tool results must reference a valid tool_call_id from a preceding assistant message
/// - Tool calls in assistant messages must eventually have matching tool results
/// - When routed to Bedrock via LiteLLM, the conversation must end with a user message
pub fn sanitize_messages_for_openai(messages: &mut Vec<ConversationMessage>) {
    remove_orphaned_tool_results(messages);
    remove_misplaced_tool_results(messages);
    synthesize_missing_tool_results(messages);
    ensure_ends_with_user_message(messages);
}

/// Removes tool_result messages that reference tool_use_ids not found in any
/// preceding assistant message.
fn remove_orphaned_tool_results(messages: &mut Vec<ConversationMessage>) {
    let mut known_tool_use_ids = HashSet::new();

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

/// LiteLLM may route OpenAI-compatible requests to Bedrock, which requires a
/// user turn containing tool_result blocks to directly answer the tool_use
/// blocks from the immediately previous assistant turn. Late results from
/// cancelled or superseded actions are valid history globally, but invalid in
/// that later user turn, so drop them before request conversion.
fn remove_misplaced_tool_results(messages: &mut Vec<ConversationMessage>) {
    let mut i = 0;
    while i < messages.len() {
        if messages[i].role != MessageRole::User {
            i += 1;
            continue;
        }

        let mut allowed_tool_use_ids = if i > 0 && messages[i - 1].role == MessageRole::Assistant {
            let mut ids = HashSet::new();
            collect_tool_use_ids(&messages[i - 1].content, &mut ids);
            ids
        } else {
            HashSet::new()
        };

        if retain_allowed_tool_results(&mut messages[i].content, &mut allowed_tool_use_ids) {
            i += 1;
        } else {
            messages.remove(i);
        }
    }
}

fn retain_allowed_tool_results(
    content: &mut MessageContent,
    allowed_tool_use_ids: &mut HashSet<String>,
) -> bool {
    match content {
        MessageContent::Text(_) | MessageContent::ToolUse { .. } => true,
        MessageContent::ToolResult { tool_use_id, .. } => allowed_tool_use_ids.remove(tool_use_id),
        MessageContent::MultiPart(parts) => {
            parts.retain(|part| match part {
                ContentPart::ToolResult { tool_use_id, .. } => {
                    allowed_tool_use_ids.remove(tool_use_id)
                }
                ContentPart::Text(_)
                | ContentPart::Reasoning { .. }
                | ContentPart::Image { .. }
                | ContentPart::ToolUse { .. } => true,
            });
            !parts.is_empty()
        }
    }
}

/// For any assistant tool_use that doesn't have a matching tool_result in a
/// subsequent user message, synthesize a result immediately after the tool_use.
/// This satisfies Bedrock's requirement (via LiteLLM) that tool_result blocks
/// appear immediately after the corresponding tool_use message.
fn synthesize_missing_tool_results(messages: &mut Vec<ConversationMessage>) {
    let mut answered_ids = HashSet::new();

    // First pass: collect all existing tool_result IDs
    for msg in messages.iter() {
        if msg.role == MessageRole::User {
            collect_tool_result_ids(&msg.content, &mut answered_ids);
        }
    }

    // Second pass: walk through messages and insert synthetic results after
    // assistant tool_use messages that have unanswered IDs.
    let mut i = 0;
    while i < messages.len() {
        if messages[i].role != MessageRole::Assistant {
            i += 1;
            continue;
        }

        let mut unanswered: Vec<String> = Vec::new();
        collect_tool_use_ids_vec(&messages[i].content, &mut unanswered);
        unanswered.retain(|id| !answered_ids.contains(id));

        if unanswered.is_empty() {
            i += 1;
            continue;
        }

        log::warn!(
            "[openai] Synthesizing {} missing tool_result(s) after message {} for IDs: {:?}",
            unanswered.len(),
            i,
            unanswered
        );

        let synthetic_parts: Vec<ContentPart> = unanswered
            .iter()
            .map(|id| ContentPart::ToolResult {
                tool_use_id: id.clone(),
                content: "Tool call result unavailable (conversation was interrupted).".to_string(),
                is_error: true,
            })
            .collect();

        let insert_idx = i + 1;

        // If next message is a user message, merge synthetic results into it
        if insert_idx < messages.len() && messages[insert_idx].role == MessageRole::User {
            match &mut messages[insert_idx].content {
                MessageContent::MultiPart(parts) => {
                    let existing = std::mem::take(parts);
                    parts.extend(synthetic_parts);
                    parts.extend(existing);
                }
                existing => {
                    let existing_part =
                        match std::mem::replace(existing, MessageContent::Text(String::new())) {
                            MessageContent::Text(t) => ContentPart::Text(t),
                            MessageContent::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                            } => ContentPart::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                            },
                            MessageContent::ToolUse {
                                tool_use_id,
                                name,
                                input,
                            } => ContentPart::ToolUse {
                                tool_use_id,
                                name,
                                input,
                            },
                            MessageContent::MultiPart(_) => unreachable!(),
                        };
                    let mut parts = synthetic_parts;
                    parts.push(existing_part);
                    *existing = MessageContent::MultiPart(parts);
                }
            }
        } else {
            // No user message follows — insert a new one
            let content = if synthetic_parts.len() == 1 {
                match synthetic_parts.into_iter().next().unwrap() {
                    ContentPart::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => MessageContent::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    },
                    _ => unreachable!(),
                }
            } else {
                MessageContent::MultiPart(synthetic_parts)
            };
            messages.insert(
                insert_idx,
                ConversationMessage {
                    role: MessageRole::User,
                    content,
                },
            );
        }

        // Mark these as answered so we don't double-synthesize
        for id in unanswered {
            answered_ids.insert(id);
        }

        i += 2; // Skip past the inserted/modified message
    }
}

fn collect_tool_use_ids(content: &MessageContent, ids: &mut HashSet<String>) {
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

fn collect_tool_use_ids_vec(content: &MessageContent, ids: &mut Vec<String>) {
    match content {
        MessageContent::ToolUse { tool_use_id, .. } => {
            ids.push(tool_use_id.clone());
        }
        MessageContent::MultiPart(parts) => {
            for part in parts {
                if let ContentPart::ToolUse { tool_use_id, .. } = part {
                    ids.push(tool_use_id.clone());
                }
            }
        }
        _ => {}
    }
}

fn collect_tool_result_ids(content: &MessageContent, ids: &mut HashSet<String>) {
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

/// Ensures the message array ends with a user message. Some providers (e.g. Bedrock
/// via LiteLLM) reject requests where the conversation ends with an assistant message
/// ("assistant message prefill"). If the last message is from the assistant, append a
/// minimal user message to satisfy this constraint.
fn ensure_ends_with_user_message(messages: &mut Vec<ConversationMessage>) {
    if messages.is_empty() {
        return;
    }
    if let Some(last) = messages.last() {
        if last.role == MessageRole::Assistant {
            log::info!(
                "[openai] Conversation ends with assistant message — appending user continuation message"
            );
            messages.push(ConversationMessage {
                role: MessageRole::User,
                content: MessageContent::Text("Continue.".to_string()),
            });
        }
    }
}
