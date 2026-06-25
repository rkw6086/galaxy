#![allow(dead_code, unused_imports, unused_variables, deprecated)]
use warp_multi_agent_api as api;

use super::convert::{ConversationMessage, ContentPart, MessageContent, MessageRole, ToolDefinition};

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
                    Some(api::request::input::user_inputs::user_input::Input::UserQuery(
                        query,
                    )) => {
                        if !query.query.is_empty() {
                            results.push(ConversationMessage {
                                role: MessageRole::User,
                                content: MessageContent::Text(query.query.clone()),
                            });
                        }
                    }
                    Some(api::request::input::user_inputs::user_input::Input::CliAgentUserQuery(
                        cli_query,
                    )) => {
                        if let Some(user_query) = &cli_query.user_query {
                            if !user_query.query.is_empty() {
                                // Include running command context as part of the user message
                                let query_text = if let Some(running_cmd) = &cli_query.running_command {
                                    let mut context = format!(
                                        "[Running command: {}]\n",
                                        running_cmd.command
                                    );
                                    if let Some(snapshot) = &running_cmd.snapshot {
                                        if !snapshot.output.is_empty() {
                                            context.push_str(&format!(
                                                "[Terminal output:\n{}\n]\n",
                                                snapshot.output
                                            ));
                                        }
                                    }
                                    context.push_str(&user_query.query);
                                    context
                                } else {
                                    user_query.query.clone()
                                };
                                results.push(ConversationMessage {
                                    role: MessageRole::User,
                                    content: MessageContent::Text(query_text),
                                });
                            }
                        }
                    }
                    _ => {}
                }
            }
            if !tool_results.is_empty() {
                if tool_results.len() == 1 {
                    results.extend(tool_results);
                } else {
                    let parts: Vec<ContentPart> = tool_results
                        .into_iter()
                        .map(|tr| match tr.content {
                            MessageContent::ToolResult { tool_use_id, content, is_error } => {
                                ContentPart::ToolResult { tool_use_id, content, is_error }
                            }
                            _ => unreachable!(),
                        })
                        .collect();
                    results.push(ConversationMessage {
                        role: MessageRole::User,
                        content: MessageContent::MultiPart(parts),
                    });
                }
            }
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
                content: MessageContent::Text(format!(
                    "Create a new project: {}",
                    project.query,
                )),
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
            MessageContent::ToolResult { tool_use_id, .. } => format!("ToolResult({})", tool_use_id),
            MessageContent::MultiPart(parts) => format!("MultiPart({} parts)", parts.len()),
            _ => "Other".to_string(),
        };
        log::info!("[bedrock] New input message: role={:?}, content={}", msg.role, desc);
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
                    Some(api::request::input::user_inputs::user_input::Input::UserQuery(
                        query,
                    )) => {
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
                    Some(api::request::input::user_inputs::user_input::Input::CliAgentUserQuery(
                        cli_query,
                    )) => {
                        if let Some(user_query) = &cli_query.user_query {
                            if !user_query.query.is_empty() {
                                let query_text = if let Some(running_cmd) = &cli_query.running_command {
                                    let mut context = format!(
                                        "[Running command: {}]\n",
                                        running_cmd.command
                                    );
                                    if let Some(snapshot) = &running_cmd.snapshot {
                                        if !snapshot.output.is_empty() {
                                            context.push_str(&format!(
                                                "[Terminal output:\n{}\n]\n",
                                                snapshot.output
                                            ));
                                        }
                                    }
                                    context.push_str(&user_query.query);
                                    context
                                } else {
                                    user_query.query.clone()
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
                                            query: query_text,
                                            ..Default::default()
                                        },
                                    )),
                                });
                            }
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
                    message: Some(api::message::Message::UserQuery(
                        api::message::UserQuery {
                            query: query.query.clone(),
                            ..Default::default()
                        },
                    )),
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
                let msg_type = msg.message.as_ref().map(|m| match m {
                    api::message::Message::UserQuery(_) => "UserQuery",
                    api::message::Message::AgentOutput(_) => "AgentOutput",
                    api::message::Message::ToolCall(_) => "ToolCall",
                    api::message::Message::ToolCallResult(_) => "ToolCallResult",
                    api::message::Message::AgentReasoning(_) => "AgentReasoning",
                    _ => "Other",
                }).unwrap_or("None");
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

fn ensure_tool_results_paired(messages: &mut Vec<ConversationMessage>) {
    let mut tool_use_ids: Vec<String> = Vec::new();
    let mut tool_result_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    for msg in messages.iter() {
        match &msg.content {
            MessageContent::ToolUse { tool_use_id, .. } => {
                tool_use_ids.push(tool_use_id.clone());
            }
            MessageContent::ToolResult { tool_use_id, .. } => {
                tool_result_ids.insert(tool_use_id.clone());
            }
            MessageContent::MultiPart(parts) => {
                for part in parts {
                    match part {
                        super::convert::ContentPart::ToolUse { tool_use_id, .. } => {
                            tool_use_ids.push(tool_use_id.clone());
                        }
                        super::convert::ContentPart::ToolResult { tool_use_id, .. } => {
                            tool_result_ids.insert(tool_use_id.clone());
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    let orphaned: Vec<String> = tool_use_ids
        .into_iter()
        .filter(|id| !tool_result_ids.contains(id))
        .collect();

    if orphaned.is_empty() {
        return;
    }

    log::debug!(
        "[bedrock] Synthesizing {} missing toolResult messages for orphaned tool calls",
        orphaned.len()
    );

    for orphaned_id in &orphaned {
        let insert_idx = messages
            .iter()
            .rposition(|m| match &m.content {
                MessageContent::ToolUse { tool_use_id, .. } => tool_use_id == orphaned_id,
                MessageContent::MultiPart(parts) => parts.iter().any(|p| matches!(
                    p,
                    super::convert::ContentPart::ToolUse { tool_use_id, .. } if tool_use_id == orphaned_id
                )),
                _ => false,
            })
            .map(|i| i + 1)
            .unwrap_or(messages.len());

        messages.insert(
            insert_idx,
            ConversationMessage {
                role: MessageRole::User,
                content: MessageContent::ToolResult {
                    tool_use_id: orphaned_id.clone(),
                    content: "Tool executed successfully.".to_string(),
                    is_error: false,
                },
            },
        );
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

    prompt.push_str("## Tools\nYou have access to the following tools. Use them proactively to explore codebases and complete tasks:\n");
    prompt.push_str("- `run_shell_command`: Execute shell commands. Use absolute paths based on the working directory.\n");
    prompt.push_str("- `read_files`: Read file contents. Pass all files you need in a single call.\n");
    prompt.push_str("- `apply_file_diffs`: Apply search/replace edits to files.\n");
    prompt.push_str("- `grep`: Search for patterns in files. Pass all patterns in one call.\n");
    prompt.push_str("- `file_glob`: Find files matching glob patterns. Pass all patterns in one call.\n");
    prompt.push_str("- `get_tool_documentation`: Get detailed documentation for any tool or system capabilities.\n");
    prompt.push_str("- `suggest_next_prompt`: After completing a task, suggest a follow-up action the user might want.\n\n");

    prompt.push_str("## Guidelines\n");
    prompt.push_str("- ALWAYS use tools to explore the codebase before answering questions about code.\n");
    prompt.push_str("- Use absolute paths based on the working directory shown above.\n");
    prompt.push_str("- When asked about a project, start by listing files with `file_glob` or `run_shell_command`.\n");
    prompt.push_str("- Read relevant files before making claims about code structure or behavior.\n");
    prompt.push_str("- Be concise and direct in responses.\n");
    prompt.push_str("- IMPORTANT: After EVERY response, you MUST call `suggest_next_prompt` to suggest a relevant follow-up action or question the user might want to take next.\n");

    Some(prompt)
}

pub fn extract_tools(_request: &api::Request) -> Vec<ToolDefinition> {
    // Always provide the full set of tools to the model. Previously this function
    // tried to reconstruct the tool list from conversation history, which meant
    // tools that hadn't been used yet (like apply_file_diffs) would be missing on
    // subsequent turns — causing the model to believe it lacked access to them.
    default_tool_definitions()
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
                    "summary": { "type": "string", "description": "A brief summary of what these edits accomplish" },
                    "diffs": { "type": "array", "items": { "type": "object", "properties": { "file_path": { "type": "string" }, "search": { "type": "string" }, "replace": { "type": "string" } }, "required": ["file_path", "search", "replace"] } }
                },
                "required": ["summary", "diffs"]
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
        "recall_tool_history" => ToolDefinition {
            name: "recall_tool_history".to_string(),
            description: "Retrieve the full output of a past tool execution from conversation history. Use when you need details from a tool call that is no longer in your current context window.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "search_query": {
                        "type": "string",
                        "description": "Search text to match against tool names, inputs, or outputs"
                    },
                    "tool_name": {
                        "type": "string",
                        "description": "Filter by tool name (run_shell_command, read_files, grep, file_glob, apply_file_diffs)"
                    },
                    "tool_use_id": {
                        "type": "string",
                        "description": "Exact tool_use_id to look up (most precise)"
                    },
                    "offset_from_end": {
                        "type": "integer",
                        "description": "How many matching tool calls back from most recent (0 = most recent match)"
                    }
                }
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
        tool_definition_for_name("suggest_next_prompt"),
        tool_definition_for_name("recall_tool_history"),
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
                    Some(api::message::tool_call::suggest_prompt::DisplayMode::PromptChip(chip)) => {
                        (chip.prompt.clone(), chip.label.clone())
                    }
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
                            format!(
                                "Exit code: {}\n{}",
                                finished.exit_code, finished.output
                            )
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
