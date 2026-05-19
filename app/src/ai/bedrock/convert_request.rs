#![allow(dead_code, unused_imports, unused_variables, deprecated)]
use warp_multi_agent_api as api;

use super::convert::{
    ContentPart, ConversationMessage, MessageContent, MessageRole, ToolDefinition,
};

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
                            let content = extract_tool_result_content(result);
                            tool_results.push(ConversationMessage {
                                role: MessageRole::User,
                                content: MessageContent::ToolResult {
                                    tool_use_id: result.tool_call_id.clone(),
                                    content,
                                    is_error: false,
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

    match input_type {
        api::request::input::Type::UserInputs(user_inputs) => {
            for user_input in &user_inputs.inputs {
                match &user_input.input {
                    Some(api::request::input::user_inputs::user_input::Input::ToolCallResult(
                        result,
                    )) => {
                        if !result.tool_call_id.is_empty() {
                            let content = extract_tool_result_content(result);
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
                let content = extract_tool_result_content(result);
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
        _ => {}
    }

    results
}

pub fn extract_messages_from_request(request: &api::Request) -> Vec<ConversationMessage> {
    let mut messages = Vec::new();

    if let Some(task_context) = &request.task_context {
        log::info!(
            "[bedrock-debug] extract_messages: {} tasks in task_context",
            task_context.tasks.len()
        );
        for task in &task_context.tasks {
            log::info!(
                "[bedrock-debug] extract_messages: task '{}' has {} messages",
                task.id,
                task.messages.len()
            );
            for msg in &task.messages {
                let msg_type = msg
                    .message
                    .as_ref()
                    .map(|m| match m {
                        api::message::Message::UserQuery(_) => "UserQuery",
                        api::message::Message::AgentOutput(_) => "AgentOutput",
                        api::message::Message::ToolCall(_) => "ToolCall",
                        api::message::Message::ToolCallResult(_) => "ToolCallResult",
                        api::message::Message::AgentReasoning(_) => "AgentReasoning",
                        _ => "Other",
                    })
                    .unwrap_or("None");
                log::info!(
                    "[bedrock-debug] extract_messages:   msg id='{}' type={}",
                    msg.id,
                    msg_type
                );
                if let Some(converted) = convert_proto_message(msg) {
                    messages.push(converted);
                }
            }
        }
    } else {
        log::warn!("[bedrock-debug] extract_messages: NO task_context in request!");
    }

    if let Some(input) = &request.input {
        if let Some(input_type) = &input.r#type {
            #[allow(deprecated)]
            match input_type {
                // UserInputs (UserQuery + ToolCallResult) are already injected
                // into task messages by inject_input_messages_into_task().
                api::request::input::Type::UserInputs(_) => {}
                api::request::input::Type::UserQuery(_) => {}
                api::request::input::Type::ToolCallResult(_) => {}
                api::request::input::Type::InitProjectRules(_) => {
                    messages.push(ConversationMessage {
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
                    messages.push(ConversationMessage {
                        role: MessageRole::User,
                        content: MessageContent::Text(format!(
                            "Create a development environment for this project. \
                             Set up necessary dependencies, configuration files, and tooling.{}",
                            repo_info
                        )),
                    });
                }
                api::request::input::Type::CreateNewProject(project) => {
                    messages.push(ConversationMessage {
                        role: MessageRole::User,
                        content: MessageContent::Text(format!(
                            "Create a new project: {}",
                            project.query
                        )),
                    });
                }
                api::request::input::Type::CloneRepository(repo) => {
                    messages.push(ConversationMessage {
                        role: MessageRole::User,
                        content: MessageContent::Text(format!(
                            "Clone the repository at {} and set it up for development.",
                            repo.url
                        )),
                    });
                }
                api::request::input::Type::AutoCodeDiffQuery(diff) => {
                    messages.push(ConversationMessage {
                        role: MessageRole::User,
                        content: MessageContent::Text(format!(
                            "Apply code changes: {}",
                            diff.query
                        )),
                    });
                }
                api::request::input::Type::ResumeConversation(_) => {
                    messages.push(ConversationMessage {
                        role: MessageRole::User,
                        content: MessageContent::Text(
                            "Continue where we left off. Review the conversation history and proceed with the next steps."
                                .to_string(),
                        ),
                    });
                }
                api::request::input::Type::QueryWithCannedResponse(canned) => {
                    messages.push(ConversationMessage {
                        role: MessageRole::User,
                        content: MessageContent::Text(canned.query.clone()),
                    });
                }
                api::request::input::Type::CodeReview(_) => {
                    messages.push(ConversationMessage {
                        role: MessageRole::User,
                        content: MessageContent::Text(
                            "Review the following code changes and provide detailed feedback on correctness, style, and potential issues."
                                .to_string(),
                        ),
                    });
                }
                _ => {}
            }
        }
    }

    ensure_starts_with_user_message(&mut messages);
    ensure_tool_results_paired(&mut messages);
    log::info!(
        "[bedrock-debug] extract_messages: TOTAL {} messages to send to Bedrock (User={}, Assistant={}, ToolResult={}, ToolUse={})",
        messages.len(),
        messages.iter().filter(|m| m.role == MessageRole::User && matches!(&m.content, MessageContent::Text(_))).count(),
        messages.iter().filter(|m| m.role == MessageRole::Assistant && matches!(&m.content, MessageContent::Text(_))).count(),
        messages.iter().filter(|m| matches!(&m.content, MessageContent::ToolResult { .. })).count(),
        messages.iter().filter(|m| matches!(&m.content, MessageContent::ToolUse { .. })).count(),
    );
    messages
}

/// Sanitizes a message list to satisfy Bedrock Converse API invariants:
/// 1. Messages must start with a user message.
/// 2. Every assistant tool_use must be immediately followed by a user
///    message containing the matching tool_result.
///
/// Call this on the combined (history + new input) messages before
/// sending to `build_converse_request`.
pub fn sanitize_messages_for_bedrock(messages: &mut Vec<ConversationMessage>) {
    ensure_starts_with_user_message(messages);
    ensure_tool_results_paired(messages);
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
            if let Some(ts) = &context.current_time {
                prompt.push_str(&format!("- Current time (UTC): {}\n", ts));
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

    prompt.push_str("## Tools\nYou have access to the following tools. Use them proactively to explore codebases and complete tasks:\n");
    prompt.push_str("- `run_shell_command`: Execute shell commands. Use absolute paths based on the working directory.\n");
    prompt.push_str(
        "- `read_files`: Read file contents. Pass all files you need in a single call.\n",
    );
    prompt.push_str("- `apply_file_diffs`: Apply search/replace edits to files.\n");
    prompt.push_str("- `grep`: Search for patterns in files. Pass all patterns in one call.\n");
    prompt.push_str(
        "- `file_glob`: Find files matching glob patterns. Pass all patterns in one call.\n",
    );
    prompt.push_str("- `get_tool_documentation`: Get detailed documentation for any tool or system capabilities.\n");
    prompt.push_str("- `suggest_next_prompt`: After completing a task, suggest a follow-up action the user might want.\n\n");

    prompt.push_str("## Guidelines\n");
    prompt.push_str("- You ALWAYS have access to run shell commands via `run_shell_command`. Never tell the user you cannot execute commands — use the tool directly.\n");
    prompt.push_str(
        "- ALWAYS use tools to explore the codebase before answering questions about code.\n",
    );
    prompt.push_str("- Use absolute paths based on the working directory shown above.\n");
    prompt.push_str("- When asked about a project, start by listing files with `file_glob` or `run_shell_command`.\n");
    prompt
        .push_str("- Read relevant files before making claims about code structure or behavior.\n");
    prompt.push_str("- Be concise and direct in responses.\n");
    prompt.push_str("- IMPORTANT: After EVERY response, you MUST call `suggest_next_prompt` to suggest a relevant follow-up action or question the user might want to take next.\n");

    Some(prompt)
}

pub fn extract_tools(request: &api::Request) -> Vec<ToolDefinition> {
    let mut tools = Vec::new();
    let mut seen_names = std::collections::HashSet::new();

    if let Some(task_context) = &request.task_context {
        for task in &task_context.tasks {
            for msg in &task.messages {
                if let Some(api::message::Message::ToolCall(tool_call)) = &msg.message {
                    let (name, _) = extract_tool_call_info(tool_call);
                    if name != "unknown_tool" && seen_names.insert(name.clone()) {
                        tools.push(tool_definition_for_name(&name));
                    }
                }
            }
        }
    }

    if let Some(input) = &request.input {
        if let Some(input_type) = &input.r#type {
            #[allow(deprecated)]
            match input_type {
                api::request::input::Type::UserInputs(user_inputs) => {
                    for user_input in &user_inputs.inputs {
                        if let Some(
                            api::request::input::user_inputs::user_input::Input::ToolCallResult(_),
                        ) = &user_input.input
                        {
                            break;
                        }
                    }
                }
                api::request::input::Type::ToolCallResult(_) => {}
                _ => {}
            }
        }
    }

    if tools.is_empty() {
        let messages = extract_messages_from_request(request);
        let has_tool_content = messages.iter().any(|m| {
            matches!(
                m.content,
                MessageContent::ToolUse { .. } | MessageContent::ToolResult { .. }
            )
        });
        if has_tool_content {
            tools = default_tool_definitions();
        }
    }

    if tools.is_empty() {
        tools = default_tool_definitions();
    }

    // Filter out suggest_next_prompt — its action executor waits on a oneshot
    // channel for UI interaction that never fires in the Bedrock path, causing
    // the conversation to stay InProgress forever.
    tools.retain(|t| t.name != "suggest_next_prompt");

    tools
}

fn tool_definition_for_name(name: &str) -> ToolDefinition {
    match name {
        "run_shell_command" => ToolDefinition {
            name: "run_shell_command".to_string(),
            description: "Execute a shell command and return its output.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The shell command to execute" }
                },
                "required": ["command"]
            }),
        },
        "read_files" => ToolDefinition {
            name: "read_files".to_string(),
            description: "Read the contents of one or more files. ALWAYS pass all files you need in a single call rather than making multiple separate calls.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "files": { "type": "array", "items": { "type": "string" }, "description": "File paths to read. Include ALL files you need in one call for efficiency." }
                },
                "required": ["files"]
            }),
        },
        "apply_file_diffs" => ToolDefinition {
            name: "apply_file_diffs".to_string(),
            description: "Apply search/replace diffs to files.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "diffs": { "type": "array", "items": { "type": "object", "properties": { "file_path": { "type": "string" }, "search": { "type": "string" }, "replace": { "type": "string" } }, "required": ["file_path", "search", "replace"] } }
                },
                "required": ["diffs"]
            }),
        },
        "grep" => ToolDefinition {
            name: "grep".to_string(),
            description: "Search for patterns in files. Pass all search patterns in one call.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "queries": { "type": "array", "items": { "type": "string" }, "description": "Search patterns. Include ALL patterns you need in one call." },
                    "path": { "type": "string", "description": "Directory to search in" }
                },
                "required": ["queries"]
            }),
        },
        "file_glob" => ToolDefinition {
            name: "file_glob".to_string(),
            description: "Find files matching glob patterns. Pass all patterns in one call.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "patterns": { "type": "array", "items": { "type": "string" }, "description": "Glob patterns to match" }
                },
                "required": ["patterns"]
            }),
        },
        "suggest_next_prompt" => ToolDefinition {
            name: "suggest_next_prompt".to_string(),
            description: "After completing a task, suggest a relevant follow-up prompt the user might want to try next. Use this to suggest a natural next step based on what was just accomplished.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "prompt": { "type": "string", "description": "The suggested prompt text that will be sent to the agent if the user accepts" },
                    "label": { "type": "string", "description": "Short display label for the suggestion chip (keep under 40 chars)" }
                },
                "required": ["prompt", "label"]
            }),
        },
        _ => ToolDefinition {
            name: name.to_string(),
            description: format!("Tool: {}", name),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {}
            }),
        },
    }
}

fn default_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        tool_definition_for_name("run_shell_command"),
        tool_definition_for_name("read_files"),
        tool_definition_for_name("apply_file_diffs"),
        tool_definition_for_name("grep"),
        tool_definition_for_name("file_glob"),
    ]
}

fn convert_proto_message(msg: &api::Message) -> Option<ConversationMessage> {
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
            let content = format_tool_call_result(result);
            Some(ConversationMessage {
                role: MessageRole::User,
                content: MessageContent::ToolResult {
                    tool_use_id: result.tool_call_id.clone(),
                    content,
                    is_error: false,
                },
            })
        }
        api::message::Message::AgentReasoning(_) => None,
        _ => None,
    }
}

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
            #[allow(deprecated)]
            api::message::tool_call::Tool::FileGlob(glob) => (
                "file_glob".to_string(),
                serde_json::json!({ "patterns": glob.patterns }),
            ),
            api::message::tool_call::Tool::SuggestPrompt(sp) => {
                let (prompt, label) = match &sp.display_mode {
                    Some(api::message::tool_call::suggest_prompt::DisplayMode::PromptChip(
                        chip,
                    )) => (chip.prompt.clone(), chip.label.clone()),
                    _ => (String::new(), String::new()),
                };
                (
                    "suggest_next_prompt".to_string(),
                    serde_json::json!({ "prompt": prompt, "label": label }),
                )
            }
            _ => ("unknown_tool".to_string(), serde_json::json!({})),
        }
    } else {
        ("unknown_tool".to_string(), serde_json::json!({}))
    }
}

fn extract_tool_result_content(result: &api::request::input::ToolCallResult) -> String {
    if let Some(result_type) = &result.result {
        match result_type {
            api::request::input::tool_call_result::Result::RunShellCommand(cmd_result) => {
                match &cmd_result.result {
                    Some(api::run_shell_command_result::Result::CommandFinished(finished)) => {
                        if finished.output.is_empty() {
                            format!("Exit code: {}\n(no output)", finished.exit_code)
                        } else {
                            format!("Exit code: {}\n{}", finished.exit_code, finished.output)
                        }
                    }
                    Some(api::run_shell_command_result::Result::LongRunningCommandSnapshot(
                        snapshot,
                    )) => snapshot.output.clone(),
                    _ => "Command completed.".to_string(),
                }
            }
            api::request::input::tool_call_result::Result::ReadFiles(read_result) => {
                match &read_result.result {
                    Some(api::read_files_result::Result::TextFilesSuccess(success)) => success
                        .files
                        .iter()
                        .map(|f| format!("{}:\n{}", f.file_path, f.content))
                        .collect::<Vec<_>>()
                        .join("\n\n"),
                    Some(api::read_files_result::Result::AnyFilesSuccess(success)) => success
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
                    _ => "Failed to read files.".to_string(),
                }
            }
            api::request::input::tool_call_result::Result::Grep(grep_result) => {
                match &grep_result.result {
                    Some(api::grep_result::Result::Success(success)) => {
                        if success.matched_files.is_empty() {
                            "No matches found.".to_string()
                        } else {
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
                                .join("\n")
                        }
                    }
                    Some(api::grep_result::Result::Error(error)) => {
                        format!("Grep error: {}", error.message)
                    }
                    None => "Grep completed (no result).".to_string(),
                }
            }
            api::request::input::tool_call_result::Result::FileGlobV2(glob_result) => {
                match &glob_result.result {
                    Some(api::file_glob_v2_result::Result::Success(success)) => {
                        if success.matched_files.is_empty() {
                            "No files matched.".to_string()
                        } else {
                            success
                                .matched_files
                                .iter()
                                .map(|f| f.file_path.as_str())
                                .collect::<Vec<_>>()
                                .join("\n")
                        }
                    }
                    Some(api::file_glob_v2_result::Result::Error(error)) => {
                        format!("File glob error: {}", error.message)
                    }
                    None => "File glob completed (no result).".to_string(),
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
                            "Diffs applied successfully.".to_string()
                        } else {
                            parts.join("\n")
                        }
                    }
                    Some(api::apply_file_diffs_result::Result::Error(error)) => {
                        format!("Apply diffs error: {}", error.message)
                    }
                    None => "Apply diffs completed.".to_string(),
                }
            }
            _ => "Tool completed successfully.".to_string(),
        }
    } else {
        "Tool completed.".to_string()
    }
}

fn format_tool_call_result(result: &api::message::ToolCallResult) -> String {
    if let Some(result_type) = &result.result {
        match result_type {
            api::message::tool_call_result::Result::RunShellCommand(cmd_result) => {
                match &cmd_result.result {
                    Some(api::run_shell_command_result::Result::CommandFinished(finished)) => {
                        if finished.output.is_empty() {
                            format!("Exit code: {}\n(no output)", finished.exit_code)
                        } else {
                            format!("Exit code: {}\n{}", finished.exit_code, finished.output)
                        }
                    }
                    Some(api::run_shell_command_result::Result::LongRunningCommandSnapshot(
                        snapshot,
                    )) => {
                        format!("Output (running): {}", snapshot.output)
                    }
                    _ => "Command completed.".to_string(),
                }
            }
            api::message::tool_call_result::Result::ReadFiles(read_result) => {
                match &read_result.result {
                    Some(api::read_files_result::Result::TextFilesSuccess(success)) => success
                        .files
                        .iter()
                        .map(|f| format!("{}:\n{}", f.file_path, f.content))
                        .collect::<Vec<_>>()
                        .join("\n\n"),
                    _ => "Read files completed.".to_string(),
                }
            }
            api::message::tool_call_result::Result::Grep(grep_result) => {
                match &grep_result.result {
                    Some(api::grep_result::Result::Success(success)) => {
                        if success.matched_files.is_empty() {
                            "No matches found.".to_string()
                        } else {
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
                                .join("\n")
                        }
                    }
                    Some(api::grep_result::Result::Error(error)) => {
                        format!("Grep error: {}", error.message)
                    }
                    None => "Grep completed (no result).".to_string(),
                }
            }
            api::message::tool_call_result::Result::FileGlobV2(glob_result) => {
                match &glob_result.result {
                    Some(api::file_glob_v2_result::Result::Success(success)) => {
                        if success.matched_files.is_empty() {
                            "No files matched.".to_string()
                        } else {
                            success
                                .matched_files
                                .iter()
                                .map(|f| f.file_path.as_str())
                                .collect::<Vec<_>>()
                                .join("\n")
                        }
                    }
                    Some(api::file_glob_v2_result::Result::Error(error)) => {
                        format!("File glob error: {}", error.message)
                    }
                    None => "File glob completed (no result).".to_string(),
                }
            }
            api::message::tool_call_result::Result::ApplyFileDiffs(diff_result) => {
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
                            "Diffs applied successfully.".to_string()
                        } else {
                            parts.join("\n")
                        }
                    }
                    Some(api::apply_file_diffs_result::Result::Error(error)) => {
                        format!("Apply diffs error: {}", error.message)
                    }
                    None => "Apply diffs completed.".to_string(),
                }
            }
            api::message::tool_call_result::Result::Server(server_result) => {
                server_result.serialized_result.clone()
            }
            _ => "Tool completed successfully.".to_string(),
        }
    } else {
        "Tool completed.".to_string()
    }
}

#[cfg(test)]
#[path = "convert_request_tests.rs"]
mod tests;
