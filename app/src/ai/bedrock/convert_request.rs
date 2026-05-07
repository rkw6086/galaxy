use warp_multi_agent_api as api;

use super::convert::{ConversationMessage, MessageContent, MessageRole, ToolDefinition};

pub fn extract_messages_from_request(request: &api::Request) -> Vec<ConversationMessage> {
    let mut messages = Vec::new();

    if let Some(task_context) = &request.task_context {
        for task in &task_context.tasks {
            for msg in &task.messages {
                if let Some(converted) = convert_proto_message(msg) {
                    messages.push(converted);
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
                        if let Some(input_variant) = &user_input.input {
                            match input_variant {
                                api::request::input::user_inputs::user_input::Input::UserQuery(
                                    query,
                                ) => {
                                    if !query.query.is_empty() {
                                        messages.push(ConversationMessage {
                                            role: MessageRole::User,
                                            content: MessageContent::Text(query.query.clone()),
                                        });
                                    }
                                }
                                api::request::input::user_inputs::user_input::Input::ToolCallResult(
                                    result,
                                ) => {
                                    let content = extract_tool_result_content(result);
                                    if !result.tool_call_id.is_empty() {
                                        messages.push(ConversationMessage {
                                            role: MessageRole::User,
                                            content: MessageContent::ToolResult {
                                                tool_use_id: result.tool_call_id.clone(),
                                                content,
                                                is_error: false,
                                            },
                                        });
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                api::request::input::Type::UserQuery(query) => {
                    if !query.query.is_empty() {
                        messages.push(ConversationMessage {
                            role: MessageRole::User,
                            content: MessageContent::Text(query.query.clone()),
                        });
                    }
                }
                api::request::input::Type::ToolCallResult(result) => {
                    let content = extract_tool_result_content(result);
                    if !result.tool_call_id.is_empty() {
                        messages.push(ConversationMessage {
                            role: MessageRole::User,
                            content: MessageContent::ToolResult {
                                tool_use_id: result.tool_call_id.clone(),
                                content,
                                is_error: false,
                            },
                        });
                    }
                }
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

pub fn extract_system_prompt(_request: &api::Request) -> Option<String> {
    Some("You are a helpful AI coding assistant. You help users with software engineering tasks including writing code, debugging, and explaining concepts.".to_string())
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
            description: "Read the contents of one or more files.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "files": { "type": "array", "items": { "type": "string" }, "description": "File paths to read" }
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
            description: "Search for patterns in files using grep.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "queries": { "type": "array", "items": { "type": "string" }, "description": "Search patterns" },
                    "path": { "type": "string", "description": "Directory to search in" }
                },
                "required": ["queries"]
            }),
        },
        "file_glob" => ToolDefinition {
            name: "file_glob".to_string(),
            description: "Find files matching glob patterns.".to_string(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "patterns": { "type": "array", "items": { "type": "string" }, "description": "Glob patterns to match" }
                },
                "required": ["patterns"]
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
                        finished.output.clone()
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
                        format!(
                            "Exit code: {}\nOutput: {}",
                            finished.exit_code, finished.output
                        )
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
            _ => "Tool completed successfully.".to_string(),
        }
    } else {
        "Tool completed.".to_string()
    }
}
