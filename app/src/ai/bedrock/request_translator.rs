use warp_multi_agent_api as api;

use super::convert::{
    ContentPart, ConversationMessage, MessageContent, MessageRole, ToolDefinition,
};

/// Convert a prost_types::Struct to a serde_json::Value for tool input schemas.
fn prost_struct_to_json(s: &prost_types::Struct) -> serde_json::Value {
    struct_to_value(s)
}

fn struct_to_value(s: &prost_types::Struct) -> serde_json::Value {
    let map: serde_json::Map<String, serde_json::Value> = s
        .fields
        .iter()
        .map(|(k, v)| (k.clone(), prost_value_to_json(v)))
        .collect();
    serde_json::Value::Object(map)
}

fn prost_value_to_json(v: &prost_types::Value) -> serde_json::Value {
    use prost_types::value::Kind;
    match &v.kind {
        Some(Kind::NullValue(_)) => serde_json::Value::Null,
        Some(Kind::NumberValue(n)) => serde_json::json!(n),
        Some(Kind::StringValue(s)) => serde_json::Value::String(s.clone()),
        Some(Kind::BoolValue(b)) => serde_json::Value::Bool(*b),
        Some(Kind::StructValue(s)) => struct_to_value(s),
        Some(Kind::ListValue(l)) => {
            serde_json::Value::Array(l.values.iter().map(prost_value_to_json).collect())
        }
        None => serde_json::Value::Null,
    }
}

/// Extract new input messages from the current request and convert them directly
/// to ConversationMessage format for the Bedrock message history.
/// This extracts UserQuery and ToolCallResult from request.input only.
pub fn extract_new_input_messages(request: &api::Request) -> Vec<ConversationMessage> {
    let mut results = Vec::new();
    let Some(input) = &request.input else {
        return results;
    };
    let Some(input_type) = &input.r#type else {
        return results;
    };

    match input_type {
        api::request::input::Type::UserInputs(user_inputs) => {
            let mut tool_results: Vec<ConversationMessage> = Vec::new();
            let mut user_queries: Vec<ConversationMessage> = Vec::new();
            for user_input in &user_inputs.inputs {
                match &user_input.input {
                    Some(api::request::input::user_inputs::user_input::Input::ToolCallResult(
                        result,
                    )) => {
                        if !result.tool_call_id.is_empty() {
                            let (content, is_error) = extract_tool_result_content(result);
                            tool_results.push(ConversationMessage {
                                role: MessageRole::User,
                                content: MessageContent::ToolResult {
                                    tool_use_id: result.tool_call_id.clone(),
                                    content,
                                    is_error,
                                },
                            });
                        }
                    }
                    Some(api::request::input::user_inputs::user_input::Input::UserQuery(query)) => {
                        if !query.query.is_empty() {
                            user_queries.push(ConversationMessage {
                                role: MessageRole::User,
                                content: MessageContent::Text(query.query.clone()),
                            });
                        }
                    }
                    _ => {}
                }
            }
            // Tool results MUST come before user queries so they pair with
            // the preceding assistant tool_use messages (Bedrock requires
            // tool_result immediately after the corresponding tool_use).
            if !tool_results.is_empty() {
                if tool_results.len() == 1 {
                    results.extend(tool_results);
                } else {
                    let parts: Vec<ContentPart> = tool_results
                        .into_iter()
                        .map(|tr| match tr.content {
                            MessageContent::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                            } => ContentPart::ToolResult {
                                tool_use_id,
                                content,
                                is_error,
                            },
                            _ => unreachable!(),
                        })
                        .collect();
                    results.push(ConversationMessage {
                        role: MessageRole::User,
                        content: MessageContent::MultiPart(parts),
                    });
                }
            }
            results.extend(user_queries);
        }
        #[allow(deprecated)]
        api::request::input::Type::UserQuery(query) => {
            if !query.query.is_empty() {
                results.push(ConversationMessage {
                    role: MessageRole::User,
                    content: MessageContent::Text(query.query.clone()),
                });
            }
        }
        api::request::input::Type::InitProjectRules(_) => {
            results.push(ConversationMessage {
                role: MessageRole::User,
                content: MessageContent::Text(
                    "Initialize this project. Analyze the codebase structure and files, \
                     generate an AGENTS.md file documenting project conventions and setup \
                     instructions, and offer to create a development environment configuration. \
                     Use the available tools to inspect the project before responding."
                        .to_string(),
                ),
            });
        }
        api::request::input::Type::CreateEnvironment(env) => {
            let repo_info = if env.repo_paths.is_empty() {
                String::new()
            } else {
                format!(" Repositories: {}", env.repo_paths.join(", "))
            };
            results.push(ConversationMessage {
                role: MessageRole::User,
                content: MessageContent::Text(format!(
                    "Create a development environment for this project. \
                     Set up necessary dependencies, configuration files, and tooling.{repo_info}"
                )),
            });
        }
        api::request::input::Type::CreateNewProject(project) => {
            results.push(ConversationMessage {
                role: MessageRole::User,
                content: MessageContent::Text(format!("Create a new project: {}", project.query,)),
            });
        }
        api::request::input::Type::CloneRepository(repo) => {
            results.push(ConversationMessage {
                role: MessageRole::User,
                content: MessageContent::Text(format!(
                    "Clone the repository at {} and set it up for development.",
                    repo.url,
                )),
            });
        }
        api::request::input::Type::ResumeConversation(_) => {
            results.push(ConversationMessage {
                role: MessageRole::User,
                content: MessageContent::Text(
                    "Continue where we left off. Review the conversation history and proceed with the next steps."
                        .to_string(),
                ),
            });
        }
        api::request::input::Type::CodeReview(_) => {
            results.push(ConversationMessage {
                role: MessageRole::User,
                content: MessageContent::Text(
                    "Review the following code changes and provide detailed feedback on correctness, style, and potential issues."
                        .to_string(),
                ),
            });
        }
        api::request::input::Type::SummarizeConversation(summarize) => {
            let prompt = if summarize.prompt.is_empty() {
                "Please summarize this conversation so far, preserving key decisions, \
                 code changes, and important context. Be concise but retain all \
                 information needed to continue the work."
                    .to_string()
            } else {
                summarize.prompt.clone()
            };
            results.push(ConversationMessage {
                role: MessageRole::User,
                content: MessageContent::Text(prompt),
            });
        }
        _ => {}
    }

    for msg in &results {
        let desc = match &msg.content {
            MessageContent::Text(t) => format!("Text({}chars)", t.len()),
            MessageContent::ToolResult { tool_use_id, .. } => {
                format!("ToolResult({})", tool_use_id)
            }
            MessageContent::MultiPart(parts) => format!("MultiPart({} parts)", parts.len()),
            _ => "Other".to_string(),
        };
        log::info!(
            "[bedrock] New input message: role={:?}, content={}",
            msg.role,
            desc
        );
    }

    results
}

/// Extract the user's query text from the request input (if present).
/// Used to emit a UserQuery proto message in the stream for persistence.
pub fn extract_user_query_text(request: &api::Request) -> Option<String> {
    let input = request.input.as_ref()?;
    let input_type = input.r#type.as_ref()?;

    match input_type {
        api::request::input::Type::UserInputs(user_inputs) => {
            for user_input in &user_inputs.inputs {
                if let Some(api::request::input::user_inputs::user_input::Input::UserQuery(query)) =
                    &user_input.input
                {
                    if !query.query.is_empty() {
                        return Some(query.query.clone());
                    }
                }
            }
            None
        }
        #[allow(deprecated)]
        api::request::input::Type::UserQuery(query) => {
            if !query.query.is_empty() {
                Some(query.query.clone())
            } else {
                None
            }
        }
        _ => None,
    }
}

/// For the Bedrock direct path: inject all input messages (user queries and tool call
/// results) into the task's messages so they persist in conversation history for future
/// requests. Without this, inputs are lost after the current request cycle because they
/// only exist in `request.input` and are never stored in `task_context.tasks[].messages`.
pub fn inject_input_messages_into_task(request: &mut api::Request) {
    let input_messages: Vec<api::Message> = extract_input_messages(request);
    if input_messages.is_empty() {
        return;
    }

    log::info!(
        "[bedrock] Injecting {} input messages into task history",
        input_messages.len()
    );

    if let Some(task_context) = &mut request.task_context {
        if let Some(task) = task_context.tasks.first_mut() {
            task.messages.extend(input_messages);
        } else {
            // No task exists yet — create one to hold the messages
            let task_id = uuid::Uuid::new_v4().to_string();
            task_context.tasks.push(api::Task {
                id: task_id,
                messages: input_messages,
                ..Default::default()
            });
        }
    } else {
        let task_id = uuid::Uuid::new_v4().to_string();
        request.task_context = Some(api::request::TaskContext {
            tasks: vec![api::Task {
                id: task_id,
                messages: input_messages,
                ..Default::default()
            }],
        });
    }
}

fn extract_input_messages(request: &api::Request) -> Vec<api::Message> {
    let mut results = Vec::new();
    let Some(input) = &request.input else {
        return results;
    };
    let Some(input_type) = &input.r#type else {
        return results;
    };

    let task_id = request
        .task_context
        .as_ref()
        .and_then(|tc| tc.tasks.first())
        .map(|t| t.id.clone())
        .unwrap_or_default();

    #[allow(deprecated)]
    match input_type {
        api::request::input::Type::UserInputs(user_inputs) => {
            for user_input in &user_inputs.inputs {
                match &user_input.input {
                    Some(api::request::input::user_inputs::user_input::Input::ToolCallResult(
                        result,
                    )) => {
                        if !result.tool_call_id.is_empty() {
                            let (content, _is_error) = extract_tool_result_content(result);
                            results.push(api::Message {
                                id: uuid::Uuid::new_v4().to_string(),
                                task_id: task_id.clone(),
                                request_id: String::new(),
                                timestamp: None,
                                server_message_data: String::new(),
                                citations: vec![],
                                message: Some(api::message::Message::ToolCallResult(
                                    api::message::ToolCallResult {
                                        tool_call_id: result.tool_call_id.clone(),
                                        context: None,
                                        result: Some(
                                            api::message::tool_call_result::Result::Server(
                                                api::message::tool_call_result::ServerResult {
                                                    serialized_result: content,
                                                },
                                            ),
                                        ),
                                    },
                                )),
                            });
                        }
                    }
                    Some(api::request::input::user_inputs::user_input::Input::UserQuery(query)) => {
                        if !query.query.is_empty() {
                            results.push(api::Message {
                                id: uuid::Uuid::new_v4().to_string(),
                                task_id: task_id.clone(),
                                request_id: String::new(),
                                timestamp: None,
                                server_message_data: String::new(),
                                citations: vec![],
                                message: Some(api::message::Message::UserQuery(
                                    api::message::UserQuery {
                                        query: query.query.clone(),
                                        ..Default::default()
                                    },
                                )),
                            });
                        }
                    }
                    _ => {}
                }
            }
        }
        api::request::input::Type::UserQuery(query) => {
            if !query.query.is_empty() {
                results.push(api::Message {
                    id: uuid::Uuid::new_v4().to_string(),
                    task_id: task_id.clone(),
                    request_id: String::new(),
                    timestamp: None,
                    server_message_data: String::new(),
                    citations: vec![],
                    message: Some(api::message::Message::UserQuery(api::message::UserQuery {
                        query: query.query.clone(),
                        ..Default::default()
                    })),
                });
            }
        }
        api::request::input::Type::ToolCallResult(result) => {
            if !result.tool_call_id.is_empty() {
                let (content, _is_error) = extract_tool_result_content(result);
                results.push(api::Message {
                    id: uuid::Uuid::new_v4().to_string(),
                    task_id: task_id.clone(),
                    request_id: String::new(),
                    timestamp: None,
                    server_message_data: String::new(),
                    citations: vec![],
                    message: Some(api::message::Message::ToolCallResult(
                        api::message::ToolCallResult {
                            tool_call_id: result.tool_call_id.clone(),
                            context: None,
                            result: Some(api::message::tool_call_result::Result::Server(
                                api::message::tool_call_result::ServerResult {
                                    serialized_result: content,
                                },
                            )),
                        },
                    )),
                });
            }
        }
        api::request::input::Type::SummarizeConversation(summarize) => {
            let prompt = if summarize.prompt.is_empty() {
                "Please summarize this conversation so far, preserving key decisions, \
                 code changes, and important context. Be concise but retain all \
                 information needed to continue the work."
                    .to_string()
            } else {
                summarize.prompt.clone()
            };
            results.push(api::Message {
                id: uuid::Uuid::new_v4().to_string(),
                task_id: task_id.clone(),
                request_id: String::new(),
                timestamp: None,
                server_message_data: String::new(),
                citations: vec![],
                message: Some(api::message::Message::UserQuery(
                    api::message::UserQuery {
                        query: prompt,
                        ..Default::default()
                    },
                )),
            });
        }
        _ => {}
    }

    results
}

/// Sanitizes a message list to satisfy Bedrock Converse API invariants:
/// 1. Messages must start with a user message.
/// 2. Every assistant tool_use must be immediately followed by a user
///    message containing the matching tool_result.
///
/// Call this on the combined (history + new input) messages before
/// sending to `build_converse_request`.
pub fn sanitize_messages_for_bedrock(messages: &mut Vec<ConversationMessage>) {
    remove_orphaned_tool_results(messages);
    ensure_starts_with_user_message(messages);
    ensure_tool_results_paired(messages);
    ensure_ends_with_user_message(messages);
}

/// Bedrock requires the conversation to end with a user message.
/// If the last message is an assistant message (e.g. after compaction),
/// append a continuation prompt.
fn ensure_ends_with_user_message(messages: &mut Vec<ConversationMessage>) {
    if messages.last().is_some_and(|m| m.role == MessageRole::Assistant) {
        log::info!("[bedrock] Appending continuation prompt (conversation ended with assistant message)");
        messages.push(ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text("Continue.".to_string()),
        });
    }
}

/// Removes tool_result content that references tool_use IDs not present in any
/// preceding assistant message. This happens after compaction when the history
/// is replaced with a summary but the next request still carries tool_results
/// from the old (now-discarded) exchanges.
fn remove_orphaned_tool_results(messages: &mut Vec<ConversationMessage>) {
    use std::collections::HashSet;

    // Collect all tool_use IDs from assistant messages.
    let mut valid_tool_use_ids = HashSet::new();
    for msg in messages.iter() {
        if msg.role == MessageRole::Assistant {
            collect_tool_use_ids_into(&msg.content, &mut valid_tool_use_ids);
        }
    }

    if valid_tool_use_ids.is_empty() {
        // No tool_use in history — remove ALL tool_results from user messages.
        let before_count = messages.len();
        messages.retain(|msg| {
            if msg.role != MessageRole::User {
                return true;
            }
            !is_pure_tool_result(&msg.content)
        });

        // Also strip tool_result parts from MultiPart user messages.
        for msg in messages.iter_mut() {
            if msg.role != MessageRole::User {
                continue;
            }
            strip_tool_result_parts(&mut msg.content);
        }

        if messages.len() != before_count {
            log::info!(
                "[bedrock] Removed {} orphaned tool_result message(s) (no tool_use in history)",
                before_count - messages.len()
            );
        }
        return;
    }

    // Remove tool_results whose IDs aren't in valid_tool_use_ids.
    for msg in messages.iter_mut() {
        if msg.role != MessageRole::User {
            continue;
        }
        strip_orphaned_tool_result_parts(&mut msg.content, &valid_tool_use_ids);
    }

    // Remove messages that became empty after stripping.
    messages.retain(|msg| !is_empty_content(&msg.content));
}

fn is_pure_tool_result(content: &MessageContent) -> bool {
    matches!(content, MessageContent::ToolResult { .. })
}

fn strip_tool_result_parts(content: &mut MessageContent) {
    if let MessageContent::MultiPart(parts) = content {
        parts.retain(|p| !matches!(p, ContentPart::ToolResult { .. }));
        if parts.len() == 1 {
            let part = parts.remove(0);
            *content = match part {
                ContentPart::Text(t) => MessageContent::Text(t),
                ContentPart::ToolUse { tool_use_id, name, input } => {
                    MessageContent::ToolUse { tool_use_id, name, input }
                }
                ContentPart::ToolResult { tool_use_id, content: c, is_error } => {
                    MessageContent::ToolResult { tool_use_id, content: c, is_error }
                }
            };
        }
    }
}

fn strip_orphaned_tool_result_parts(
    content: &mut MessageContent,
    valid_ids: &std::collections::HashSet<String>,
) {
    match content {
        MessageContent::ToolResult { tool_use_id, .. } => {
            if !valid_ids.contains(tool_use_id) {
                *content = MessageContent::Text(String::new());
            }
        }
        MessageContent::MultiPart(parts) => {
            parts.retain(|p| match p {
                ContentPart::ToolResult { tool_use_id, .. } => valid_ids.contains(tool_use_id),
                _ => true,
            });
            if parts.len() == 1 {
                let part = parts.remove(0);
                *content = match part {
                    ContentPart::Text(t) => MessageContent::Text(t),
                    ContentPart::ToolUse { tool_use_id, name, input } => {
                        MessageContent::ToolUse { tool_use_id, name, input }
                    }
                    ContentPart::ToolResult { tool_use_id, content: c, is_error } => {
                        MessageContent::ToolResult { tool_use_id, content: c, is_error }
                    }
                };
            }
        }
        _ => {}
    }
}

fn is_empty_content(content: &MessageContent) -> bool {
    match content {
        MessageContent::Text(t) => t.is_empty(),
        MessageContent::MultiPart(parts) => parts.is_empty(),
        _ => false,
    }
}

fn collect_tool_use_ids_into(content: &MessageContent, ids: &mut std::collections::HashSet<String>) {
    match content {
        MessageContent::ToolUse { tool_use_id, .. } => {
            ids.insert(tool_use_id.clone());
        }
        MessageContent::MultiPart(parts) => {
            for p in parts {
                if let ContentPart::ToolUse { tool_use_id, .. } = p {
                    ids.insert(tool_use_id.clone());
                }
            }
        }
        _ => {}
    }
}

fn ensure_starts_with_user_message(messages: &mut Vec<ConversationMessage>) {
    if messages.is_empty() {
        messages.push(ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text("Please proceed with the requested task.".to_string()),
        });
        return;
    }

    if messages[0].role != MessageRole::User {
        messages.insert(
            0,
            ConversationMessage {
                role: MessageRole::User,
                content: MessageContent::Text(
                    "Please proceed with the requested task.".to_string(),
                ),
            },
        );
    }
}

/// Ensures every `tool_use` block in an assistant message is immediately followed
/// by a user message containing the matching `tool_result`. The Bedrock/Anthropic
/// Converse API rejects requests where this invariant is violated.
///
/// This function:
/// 1. Walks messages sequentially and collects tool_use IDs from each assistant message.
/// 2. Checks the immediately-next user message for matching tool_results.
/// 3. Synthesizes missing tool_results in the correct position.
/// 4. Removes trailing assistant tool_use messages that have no following user message.
/// 5. Ensures strict user/assistant role alternation.
fn ensure_tool_results_paired(messages: &mut Vec<ConversationMessage>) {
    use std::collections::HashSet;

    // Collect all tool_result IDs that exist anywhere in the conversation.
    let mut all_result_ids = HashSet::new();
    for msg in messages.iter() {
        collect_tool_result_ids(&msg.content, &mut all_result_ids);
    }

    // Walk forward: for each assistant message with tool_use blocks,
    // ensure the next message is a user message with matching tool_results.
    let mut i = 0;
    while i < messages.len() {
        let tool_use_ids = collect_tool_use_ids(&messages[i].content);
        if tool_use_ids.is_empty() || messages[i].role != MessageRole::Assistant {
            i += 1;
            continue;
        }

        // Find which tool_use IDs are missing results in the next message.
        let next_result_ids = messages
            .get(i + 1)
            .filter(|m| m.role == MessageRole::User)
            .map(|m| {
                let mut ids = HashSet::new();
                collect_tool_result_ids(&m.content, &mut ids);
                ids
            })
            .unwrap_or_default();

        // Also check global results — if a result exists later, that's still
        // structurally broken (not immediately after), but we still need to
        // synthesize one in the right position.
        let missing: Vec<String> = tool_use_ids
            .into_iter()
            .filter(|id| !next_result_ids.contains(id))
            .collect();

        if missing.is_empty() {
            i += 1;
            continue;
        }

        log::warn!(
            "[bedrock] Synthesizing {} missing tool_result(s) after message {} for tool_use IDs: {:?}",
            missing.len(),
            i,
            missing
        );

        // Build synthetic tool_result messages.
        let synthetic_results: Vec<ContentPart> = missing
            .iter()
            .map(|id| ContentPart::ToolResult {
                tool_use_id: id.clone(),
                content: "Tool call result unavailable (conversation was interrupted).".to_string(),
                is_error: false,
            })
            .collect();

        let insert_idx = i + 1;

        // If the next message is already a user message, merge synthetic results into it.
        // IMPORTANT: ToolResult blocks must come BEFORE text content in a user message
        // that follows an assistant tool_use. The Bedrock/Anthropic API validates this
        // ordering and rejects requests where text precedes tool_result.
        if insert_idx < messages.len() && messages[insert_idx].role == MessageRole::User {
            match &mut messages[insert_idx].content {
                MessageContent::MultiPart(parts) => {
                    // Prepend synthetic results before existing parts so
                    // tool_result blocks appear first in the content.
                    let existing = std::mem::take(parts);
                    parts.extend(synthetic_results);
                    parts.extend(existing);
                }
                existing => {
                    // Convert existing single content + synthetic results into MultiPart.
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
                    // Synthetic tool_result blocks come first, then the
                    // original content (text), matching the Bedrock API
                    // requirement that tool_result precedes other content.
                    let mut parts = synthetic_results;
                    parts.push(existing_part);
                    *existing = MessageContent::MultiPart(parts);
                }
            }
            log::info!(
                "[bedrock] Merged synthetic tool_result(s) into existing user message at index {}",
                insert_idx
            );
        } else {
            // No user message follows — insert a new one.
            let content = if synthetic_results.len() == 1 {
                match synthetic_results.into_iter().next().unwrap() {
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
                MessageContent::MultiPart(synthetic_results)
            };
            messages.insert(
                insert_idx,
                ConversationMessage {
                    role: MessageRole::User,
                    content,
                },
            );
        }

        // Advance past both the assistant message and the (now-valid) user message.
        i += 2;
    }

    // Final pass: ensure no trailing assistant tool_use without a following user message.
    if let Some(last) = messages.last() {
        if last.role == MessageRole::Assistant {
            let trailing_ids = collect_tool_use_ids(&last.content);
            if !trailing_ids.is_empty() {
                log::warn!(
                    "[bedrock] Removing {} trailing tool_use IDs from final assistant message",
                    trailing_ids.len()
                );
                // Synthesize a trailing user message with all the results.
                let parts: Vec<ContentPart> = trailing_ids
                    .into_iter()
                    .map(|id| ContentPart::ToolResult {
                        tool_use_id: id,
                        content: "Tool call result unavailable (conversation was interrupted)."
                            .to_string(),
                        is_error: false,
                    })
                    .collect();
                let content = if parts.len() == 1 {
                    match parts.into_iter().next().unwrap() {
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
                    MessageContent::MultiPart(parts)
                };
                messages.push(ConversationMessage {
                    role: MessageRole::User,
                    content,
                });
            }
        }
    }
}

/// Extracts all tool_use IDs from a message's content.
fn collect_tool_use_ids(content: &MessageContent) -> Vec<String> {
    match content {
        MessageContent::ToolUse { tool_use_id, .. } => vec![tool_use_id.clone()],
        MessageContent::MultiPart(parts) => parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::ToolUse { tool_use_id, .. } => Some(tool_use_id.clone()),
                _ => None,
            })
            .collect(),
        _ => vec![],
    }
}

/// Collects all tool_result IDs from a message's content into the provided set.
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

pub fn extract_system_prompt(request: &api::Request) -> Option<String> {
    let mut prompt = String::with_capacity(2048);

    prompt.push_str("You are Galaxy, an AI coding assistant embedded in a terminal application. You help users with software engineering tasks including writing code, debugging, explaining concepts, and navigating codebases.\n\n");

    if let Some(input) = &request.input {
        if let Some(context) = &input.context {
            prompt.push_str("## Environment\n");
            if let Some(dir) = &context.directory {
                if !dir.pwd.is_empty() {
                    prompt.push_str(&format!("- Working directory: {}\n", dir.pwd));
                }
                if !dir.home.is_empty() {
                    prompt.push_str(&format!("- Home directory: {}\n", dir.home));
                }
            }
            if let Some(os) = &context.operating_system {
                if !os.platform.is_empty() {
                    prompt.push_str(&format!("- OS: {}\n", os.platform));
                }
            }
            if let Some(shell) = &context.shell {
                if !shell.name.is_empty() {
                    prompt.push_str(&format!("- Shell: {}", shell.name));
                    if !shell.version.is_empty() {
                        prompt.push_str(&format!(" {}", shell.version));
                    }
                    prompt.push('\n');
                }
            }
            if let Some(git) = &context.git {
                if !git.branch.is_empty() {
                    prompt.push_str(&format!("- Git branch: {}\n", git.branch));
                }
            }
            prompt.push('\n');

            if !context.project_rules.is_empty() {
                prompt.push_str("## Project Rules\n");
                for rules in &context.project_rules {
                    if !rules.root_path.is_empty() {
                        prompt.push_str(&format!("### Rules from {}\n", rules.root_path));
                    }
                    for file in &rules.active_rule_files {
                        if !file.content.is_empty() {
                            prompt.push_str(&file.content);
                            prompt.push('\n');
                        }
                    }
                }
                prompt.push('\n');
            }
        }
    }

    prompt.push_str("## Tool Usage\n");
    prompt.push_str("You have been given every tool you need to complete your tasks. Use them to achieve results with as few calls and as little back-and-forth as possible.\n\n");
    prompt.push_str("**How to choose tools:**\n");
    prompt.push_str("- For reading, writing, searching, and navigating files on the local filesystem, use your filesystem tools (`read_files`, `file_glob`, `grep`, `apply_file_diffs`).\n");
    prompt.push_str("- For running commands, installing packages, building, testing, and any shell operation, use `run_shell_command`.\n");
    prompt.push_str("- For tasks that require interacting with external services, web UIs, or capabilities not covered by your filesystem and shell tools, use your MCP tools.\n");
    prompt.push_str("- For complex multi-step tasks where a single script would replace many tool calls, write code (Python, Node, bash) via `run_shell_command` to reduce round-trips. But never use scripts for simple operations that a single command handles.\n\n");
    prompt.push_str("**Critical rules:**\n");
    prompt.push_str("- Use ONLY the tools in your tool configuration. Never invent or guess tool names.\n");
    prompt.push_str("- ALWAYS pass `--no-pager` (or equivalent) flags to CLI tools like git, less, man, etc. Tools that lock stdin will freeze the session.\n");
    prompt.push_str("- Output text directly in your response instead of using `echo` — echo requires user approval and adds unnecessary friction.\n");
    prompt.push_str("- Use absolute paths based on the working directory shown above.\n");
    prompt.push_str("- Be logical in your tool choices. Read files before making claims about code. List files before assuming project structure.\n");
    prompt.push_str("- Be concise and direct.\n");

    Some(prompt)
}

pub fn extract_tools(request: &api::Request) -> Vec<ToolDefinition> {
    // Always start with the full set of default tools.
    let mut tools = default_tool_definitions();
    let mut seen_names: std::collections::HashSet<String> =
        tools.iter().map(|t| t.name.clone()).collect();

    // Include MCP tools from connected servers so the model can invoke them.
    if let Some(mcp_context) = &request.mcp_context {
        for server in &mcp_context.servers {
            for tool in &server.tools {
                let name = format!("mcp__{}__{}", server.name, tool.name);
                if seen_names.insert(name.clone()) {
                    let input_schema = tool
                        .input_schema
                        .as_ref()
                        .map(|s| prost_struct_to_json(s))
                        .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));
                    tools.push(ToolDefinition {
                        name,
                        description: if tool.description.is_empty() {
                            format!("MCP tool from {} server", server.name)
                        } else {
                            tool.description.clone()
                        },
                        input_schema,
                    });
                }
            }
        }
        // Also handle flat (deprecated) tool list
        #[allow(deprecated)]
        for tool in &mcp_context.tools {
            let name = format!("mcp__{}", tool.name);
            if seen_names.insert(name.clone()) {
                let input_schema = tool
                    .input_schema
                    .as_ref()
                    .map(|s| prost_struct_to_json(s))
                    .unwrap_or_else(|| serde_json::json!({"type": "object", "properties": {}}));
                tools.push(ToolDefinition {
                    name,
                    description: if tool.description.is_empty() {
                        "MCP tool".to_string()
                    } else {
                        tool.description.clone()
                    },
                    input_schema,
                });
            }
        }
    }

    // Filter out suggest_next_prompt — its action executor waits on a oneshot
    // channel for UI interaction that never fires in the Bedrock path, causing
    // the conversation to stay InProgress forever.
    tools.retain(|t| t.name != "suggest_next_prompt");

    tools
}

pub fn default_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "run_shell_command".to_string(),
            description: "Execute a shell command in the user's terminal and return its output. Use for running builds, tests, git operations, installing packages, or any shell operation. Commands run in the user's actual shell with their environment. Set is_read_only=true for read-only commands (ls, cat, git status) to enable auto-execution. Always use --no-pager for git commands.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The shell command to execute" },
                    "is_read_only": { "type": "boolean", "description": "True if command only reads data and makes no changes" },
                    "is_risky": { "type": "boolean", "description": "True if command is destructive or irreversible (rm -rf, git push --force)" }
                },
                "required": ["command"]
            }),
        },
        ToolDefinition {
            name: "read_files".to_string(),
            description: "Read the contents of one or more files. Pass ALL file paths you need in a single call for efficiency. Returns file contents with path headers. Binary files are detected and skipped. Use absolute paths.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "files": { "type": "array", "items": { "type": "string" }, "description": "Absolute file paths to read" }
                },
                "required": ["files"]
            }),
        },
        ToolDefinition {
            name: "apply_file_diffs".to_string(),
            description: "Apply search/replace edits to files. Creates files if they don't exist (use empty search string). The search string must uniquely match one location in the file. Include enough surrounding context for uniqueness. For new files, use search=\"\" and put full content in replace.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "summary": { "type": "string", "description": "A brief summary of what these edits accomplish (e.g. 'Add error handling to parse_config')" },
                    "diffs": { "type": "array", "items": { "type": "object", "properties": { "file_path": { "type": "string", "description": "Absolute path to the file" }, "search": { "type": "string", "description": "Exact text to find (must match uniquely). Empty string to create a new file." }, "replace": { "type": "string", "description": "Text to replace with" } }, "required": ["file_path", "search", "replace"] }, "description": "Array of file edits to apply" }
                },
                "required": ["summary", "diffs"]
            }),
        },
        ToolDefinition {
            name: "grep".to_string(),
            description: "Search for regex patterns in files. Uses git grep in git repos (respects .gitignore) or ripgrep otherwise. Returns file paths and matching line numbers. Use read_files afterward to see context around matches. Pass ALL patterns you need in one call.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "queries": { "type": "array", "items": { "type": "string" }, "description": "Regex patterns to search for" },
                    "path": { "type": "string", "description": "Directory to scope the search to" }
                },
                "required": ["queries"]
            }),
        },
        ToolDefinition {
            name: "file_glob".to_string(),
            description: "Find files matching glob patterns. Uses git ls-files in git repos. Returns absolute file paths of matches. Common patterns: '**/*.rs', 'src/**/*.ts', '**/Cargo.toml'. Pass ALL patterns in one call.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "patterns": { "type": "array", "items": { "type": "string" }, "description": "Glob patterns to match files" },
                    "path": { "type": "string", "description": "Directory to search from" }
                },
                "required": ["patterns"]
            }),
        },
        ToolDefinition {
            name: "search_codebase".to_string(),
            description: "Semantic code search across the indexed codebase. Use for finding relevant code by meaning rather than exact text match. Better than grep for conceptual queries like 'authentication logic' or 'error handling for database connections'.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Natural language search query describing what you're looking for" },
                    "path": { "type": "string", "description": "Optional directory path to narrow search scope" }
                },
                "required": ["query"]
            }),
        },
        ToolDefinition {
            name: "write_to_long_running_shell_command".to_string(),
            description: "Send input (stdin) to a currently running shell command. Use this to interact with commands that are waiting for input, like interactive prompts, REPLs, or commands that accept piped input.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "input": { "type": "string", "description": "Text to send as stdin to the running command" }
                },
                "required": ["input"]
            }),
        },
        ToolDefinition {
            name: "read_shell_command_output".to_string(),
            description: "Read the latest output from a previously started long-running shell command. Use to check progress or get results from commands that are still running.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {},
                "required": []
            }),
        },
        ToolDefinition {
            name: "read_mcp_resource".to_string(),
            description: "Read a resource from a connected MCP (Model Context Protocol) server. Resources provide context like database schemas, API docs, or live system state.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "server_id": { "type": "string", "description": "MCP server identifier" },
                    "uri": { "type": "string", "description": "Resource URI to read" }
                },
                "required": ["server_id", "uri"]
            }),
        },
        ToolDefinition {
            name: "read_documents".to_string(),
            description: "Read the contents of one or more Galaxy notebook documents by their IDs.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "document_ids": { "type": "array", "items": { "type": "string" }, "description": "Document IDs to read" }
                },
                "required": ["document_ids"]
            }),
        },
        ToolDefinition {
            name: "create_documents".to_string(),
            description: "Create new Galaxy notebook documents with the specified title and content.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "documents": { "type": "array", "items": { "type": "object", "properties": { "title": { "type": "string" }, "content": { "type": "string" } }, "required": ["title", "content"] }, "description": "Documents to create" }
                },
                "required": ["documents"]
            }),
        },
        ToolDefinition {
            name: "edit_documents".to_string(),
            description: "Edit existing Galaxy notebook documents using search/replace diffs.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "diffs": { "type": "array", "items": { "type": "object", "properties": { "document_id": { "type": "string" }, "search": { "type": "string" }, "replace": { "type": "string" } }, "required": ["document_id", "search", "replace"] }, "description": "Edits to apply to documents" }
                },
                "required": ["diffs"]
            }),
        },
        ToolDefinition {
            name: "start_agent".to_string(),
            description: "Start a sub-agent to handle a specific task autonomously. Use for delegating independent work that can run in parallel. The agent gets its own conversation context and tool access. IMPORTANT: Only use this for the initial investigation or when genuinely new research is needed. Do NOT re-spawn agents for follow-up questions if you already have their output in context — just answer from the information you already have.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string", "description": "Name for the sub-agent (used for identification)" },
                    "prompt": { "type": "string", "description": "The task/instructions for the sub-agent to execute" }
                },
                "required": ["name", "prompt"]
            }),
        },
        // send_message_to_agent removed — child agents in local/Bedrock mode
        // run autonomously and cannot receive messages. Exposing this tool
        // causes the model to poll in a loop.
        ToolDefinition {
            name: "ask_user_question".to_string(),
            description: "Ask the user a question when you need clarification or a decision. Present clear options when possible. Use sparingly — prefer making reasonable assumptions.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "question": { "type": "string", "description": "The question to ask the user" },
                    "options": { "type": "array", "items": { "type": "string" }, "description": "Optional multiple-choice options to present" }
                },
                "required": ["question"]
            }),
        },
        // suggest_next_prompt removed — its executor hangs waiting for UI
        // interaction that doesn't exist in the Bedrock path. The stream-level
        // skip (response_translator) prevents deadlocks, but removing the tool
        // definition avoids wasting output tokens on calls that will be discarded.
        ToolDefinition {
            name: "read_skill".to_string(),
            description: "Read a skill definition to understand available capabilities and how to use them.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "skill": { "type": "string", "description": "Skill identifier to read" }
                },
                "required": ["skill"]
            }),
        },
        ToolDefinition {
            name: "fetch_conversation".to_string(),
            description: "Fetch the contents of a previous conversation for context. Use when the user references prior work or you need history from another session.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "conversation_id": { "type": "string", "description": "ID of the conversation to fetch" }
                },
                "required": ["conversation_id"]
            }),
        },
    ]
}

/// Returns `(content_text, is_error)` for a tool call result.
fn extract_tool_result_content(result: &api::request::input::ToolCallResult) -> (String, bool) {
    if let Some(result_type) = &result.result {
        match result_type {
            api::request::input::tool_call_result::Result::RunShellCommand(cmd_result) => {
                match &cmd_result.result {
                    Some(api::run_shell_command_result::Result::CommandFinished(finished)) => {
                        let content = if finished.output.is_empty() {
                            format!("Exit code: {}\n(no output)", finished.exit_code)
                        } else {
                            format!("Exit code: {}\n{}", finished.exit_code, finished.output)
                        };
                        let is_error = finished.exit_code != 0;
                        (content, is_error)
                    }
                    Some(api::run_shell_command_result::Result::LongRunningCommandSnapshot(
                        snapshot,
                    )) => (snapshot.output.clone(), false),
                    Some(api::run_shell_command_result::Result::PermissionDenied(denied)) => {
                        let reason = match &denied.reason {
                            Some(api::permission_denied::Reason::DenylistedCommand(())) => {
                                "command is on the deny list"
                            }
                            _ => "permission denied",
                        };
                        (format!("Error: Command not executed — {reason}."), true)
                    }
                    _ => ("Command completed.".to_string(), false),
                }
            }
            api::request::input::tool_call_result::Result::ReadFiles(read_result) => {
                match &read_result.result {
                    Some(api::read_files_result::Result::TextFilesSuccess(success)) => (
                        success
                            .files
                            .iter()
                            .map(|f| format!("{}:\n{}", f.file_path, f.content))
                            .collect::<Vec<_>>()
                            .join("\n\n"),
                        false,
                    ),
                    Some(api::read_files_result::Result::AnyFilesSuccess(success)) => (
                        success
                            .files
                            .iter()
                            .filter_map(|f| match &f.content {
                                Some(api::any_file_content::Content::TextContent(t)) => {
                                    Some(format!("{}:\n{}", t.file_path, t.content))
                                }
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n\n"),
                        false,
                    ),
                    Some(api::read_files_result::Result::Error(error)) => {
                        (format!("Error reading files: {}", error.message), true)
                    }
                    _ => ("Failed to read files.".to_string(), true),
                }
            }
            api::request::input::tool_call_result::Result::Grep(grep_result) => {
                match &grep_result.result {
                    Some(api::grep_result::Result::Success(success)) => {
                        if success.matched_files.is_empty() {
                            ("No matches found.".to_string(), false)
                        } else {
                            (
                                success
                                    .matched_files
                                    .iter()
                                    .map(|f| {
                                        let lines: String = f
                                            .matched_lines
                                            .iter()
                                            .map(|l| format!("  line {}", l.line_number))
                                            .collect::<Vec<_>>()
                                            .join(", ");
                                        format!("{} (matches at: {})", f.file_path, lines)
                                    })
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                                false,
                            )
                        }
                    }
                    Some(api::grep_result::Result::Error(error)) => {
                        (format!("Grep error: {}", error.message), true)
                    }
                    None => ("Grep completed (no result).".to_string(), false),
                }
            }
            api::request::input::tool_call_result::Result::FileGlobV2(glob_result) => {
                match &glob_result.result {
                    Some(api::file_glob_v2_result::Result::Success(success)) => {
                        if success.matched_files.is_empty() {
                            ("No files matched.".to_string(), false)
                        } else {
                            (
                                success
                                    .matched_files
                                    .iter()
                                    .map(|f| f.file_path.as_str())
                                    .collect::<Vec<_>>()
                                    .join("\n"),
                                false,
                            )
                        }
                    }
                    Some(api::file_glob_v2_result::Result::Error(error)) => {
                        (format!("File glob error: {}", error.message), true)
                    }
                    None => ("File glob completed (no result).".to_string(), false),
                }
            }
            api::request::input::tool_call_result::Result::ApplyFileDiffs(diff_result) => {
                match &diff_result.result {
                    Some(api::apply_file_diffs_result::Result::Success(success)) => {
                        let mut parts = Vec::new();
                        for f in &success.updated_files_v2 {
                            if let Some(file) = &f.file {
                                parts.push(format!("Updated: {}", file.file_path));
                            }
                        }
                        for f in &success.deleted_files {
                            parts.push(format!("Deleted: {}", f.file_path));
                        }
                        if parts.is_empty() {
                            ("Diffs applied successfully.".to_string(), false)
                        } else {
                            (parts.join("\n"), false)
                        }
                    }
                    Some(api::apply_file_diffs_result::Result::Error(error)) => {
                        (format!("Apply diffs error: {}", error.message), true)
                    }
                    None => ("Apply diffs completed.".to_string(), false),
                }
            }
            api::request::input::tool_call_result::Result::FileGlob(glob_result) => {
                match &glob_result.result {
                    Some(api::file_glob_result::Result::Success(success)) => {
                        if success.matched_files.is_empty() {
                            ("No files matched.".to_string(), false)
                        } else {
                            (success.matched_files.clone(), false)
                        }
                    }
                    Some(api::file_glob_result::Result::Error(error)) => {
                        (format!("File glob error: {}", error.message), true)
                    }
                    None => ("File glob completed (no result).".to_string(), false),
                }
            }
            api::request::input::tool_call_result::Result::CallMcpTool(mcp_result) => {
                match &mcp_result.result {
                    Some(api::call_mcp_tool_result::Result::Success(success)) => (
                        success
                            .results
                            .iter()
                            .filter_map(|item| match &item.result {
                                Some(api::call_mcp_tool_result::success::result::Result::Text(t)) => {
                                    Some(t.text.clone())
                                }
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                        false,
                    ),
                    Some(api::call_mcp_tool_result::Result::Error(error)) => {
                        (format!("MCP tool error: {}", error.message), true)
                    }
                    None => ("MCP tool completed.".to_string(), false),
                }
            }
            api::request::input::tool_call_result::Result::SearchCodebase(search_result) => {
                match &search_result.result {
                    Some(api::search_codebase_result::Result::Success(success)) => (
                        success
                            .files
                            .iter()
                            .map(|f| format!("{}:\n{}", f.file_path, f.content))
                            .collect::<Vec<_>>()
                            .join("\n\n"),
                        false,
                    ),
                    Some(api::search_codebase_result::Result::Error(error)) => {
                        (format!("Search codebase error: {}", error.message), true)
                    }
                    None => ("Search codebase completed (no result).".to_string(), false),
                }
            }
            api::request::input::tool_call_result::Result::WriteToLongRunningShellCommand(
                write_result,
            ) => match &write_result.result {
                Some(
                    api::write_to_long_running_shell_command_result::Result::LongRunningCommandSnapshot(
                        snapshot,
                    ),
                ) => (snapshot.output.clone(), false),
                Some(
                    api::write_to_long_running_shell_command_result::Result::CommandFinished(
                        finished,
                    ),
                ) => {
                    let content = format!(
                        "Exit code: {}\n{}",
                        finished.exit_code, finished.output
                    );
                    let is_error = finished.exit_code != 0;
                    (content, is_error)
                }
                Some(api::write_to_long_running_shell_command_result::Result::Error(_)) => {
                    ("Error: shell command not found.".to_string(), true)
                }
                None => ("Write to shell command completed.".to_string(), false),
            },
            api::request::input::tool_call_result::Result::StartAgent(start_agent_result) => {
                match &start_agent_result.result {
                    Some(api::start_agent_result::Result::Success(success)) => {
                        (success.agent_id.clone(), false)
                    }
                    Some(api::start_agent_result::Result::Error(error)) => {
                        (format!("Agent error: {}", error.error), true)
                    }
                    None => ("Agent completed.".to_string(), false),
                }
            }
            api::request::input::tool_call_result::Result::StartAgentV2(start_agent_result) => {
                match &start_agent_result.result {
                    Some(api::start_agent_v2_result::Result::Success(success)) => {
                        (success.agent_id.clone(), false)
                    }
                    Some(api::start_agent_v2_result::Result::Error(error)) => {
                        (format!("Agent error: {}", error.error), true)
                    }
                    None => ("Agent completed.".to_string(), false),
                }
            }
            _ => ("Tool completed successfully.".to_string(), false),
        }
    } else {
        ("Tool completed.".to_string(), false)
    }
}

#[cfg(test)]
pub fn extract_messages_from_request(request: &api::Request) -> Vec<ConversationMessage> {
    let mut messages = Vec::new();

    if let Some(task_context) = &request.task_context {
        for task in &task_context.tasks {
            for msg in &task.messages {
                if let Some(converted) = convert_proto_message_for_test(msg) {
                    messages.push(converted);
                }
            }
        }
    }

    let new_inputs = extract_new_input_messages(request);
    messages.extend(new_inputs);

    ensure_starts_with_user_message(&mut messages);
    ensure_tool_results_paired(&mut messages);
    messages
}

/// Heuristic to detect whether a serialized `ServerResult` text represents an error.
/// Used during session restoration where the structured result type is lost and we only
/// have the serialized text to inspect.
fn is_server_result_error(text: &str) -> bool {
    let lower = text.to_lowercase();
    lower.starts_with("error:")
        || lower.starts_with("error reading")
        || lower.starts_with("grep error:")
        || lower.starts_with("file glob error:")
        || lower.starts_with("apply diffs error:")
        || lower.starts_with("mcp tool error:")
        || lower.starts_with("search codebase error:")
        || lower.starts_with("failed to read files")
        || (lower.starts_with("exit code:") && !lower.starts_with("exit code: 0"))
}

/// Converts a proto `api::Message` into a `ConversationMessage` for the Bedrock message history.
/// Used to rebuild the message history from persisted task messages on session restore.
pub fn convert_proto_message(msg: &api::Message) -> Option<ConversationMessage> {
    let message_content = msg.message.as_ref()?;
    match message_content {
        api::message::Message::UserQuery(query) => Some(ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text(query.query.clone()),
        }),
        api::message::Message::AgentOutput(output) => Some(ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::Text(output.text.clone()),
        }),
        api::message::Message::ToolCall(tool_call) => {
            let (name, input) = extract_tool_call_info(tool_call);
            Some(ConversationMessage {
                role: MessageRole::Assistant,
                content: MessageContent::ToolUse {
                    tool_use_id: tool_call.tool_call_id.clone(),
                    name,
                    input,
                },
            })
        }
        api::message::Message::ToolCallResult(result) => {
            let (content, is_error) = match &result.result {
                Some(api::message::tool_call_result::Result::Server(s)) => {
                    let text = s.serialized_result.clone();
                    let is_err = is_server_result_error(&text);
                    (text, is_err)
                }
                _ => ("Tool completed.".to_string(), false),
            };
            Some(ConversationMessage {
                role: MessageRole::User,
                content: MessageContent::ToolResult {
                    tool_use_id: result.tool_call_id.clone(),
                    content,
                    is_error,
                },
            })
        }
        _ => None,
    }
}

#[allow(deprecated)]
fn extract_tool_call_info(tool_call: &api::message::ToolCall) -> (String, serde_json::Value) {
    if let Some(tool) = &tool_call.tool {
        match tool {
            api::message::tool_call::Tool::RunShellCommand(cmd) => (
                "run_shell_command".to_string(),
                serde_json::json!({ "command": cmd.command }),
            ),
            api::message::tool_call::Tool::ReadFiles(read) => (
                "read_files".to_string(),
                serde_json::json!({ "files": read.files.iter().map(|f| &f.name).collect::<Vec<_>>() }),
            ),
            api::message::tool_call::Tool::ApplyFileDiffs(diffs) => (
                "apply_file_diffs".to_string(),
                serde_json::json!({ "diffs": diffs.diffs.iter().map(|d| {
                    serde_json::json!({
                        "file_path": d.file_path,
                        "search": d.search,
                        "replace": d.replace
                    })
                }).collect::<Vec<_>>() }),
            ),
            api::message::tool_call::Tool::Grep(grep) => (
                "grep".to_string(),
                serde_json::json!({ "queries": grep.queries, "path": grep.path }),
            ),
            api::message::tool_call::Tool::FileGlob(glob) => (
                "file_glob".to_string(),
                serde_json::json!({ "patterns": glob.patterns }),
            ),
            _ => ("unknown_tool".to_string(), serde_json::json!({})),
        }
    } else {
        ("unknown_tool".to_string(), serde_json::json!({}))
    }
}

#[cfg(test)]
fn convert_proto_message_for_test(msg: &api::Message) -> Option<ConversationMessage> {
    convert_proto_message(msg)
}


#[cfg(test)]
#[path = "request_translator_tests.rs"]
mod tests;
