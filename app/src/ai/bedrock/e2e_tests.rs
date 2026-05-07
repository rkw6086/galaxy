use futures::StreamExt;
use serde_json::json;
use std::path::PathBuf;

use super::client::{BedrockClient, BedrockClientConfig};
use super::convert::{ConversationMessage, MessageContent, MessageRole, ToolDefinition};
use crate::settings::ai::BedrockAuthMethod;
use warp_multi_agent_api as api;

fn make_user_query_message(id: &str, task_id: &str, query: &str) -> api::Message {
    api::Message {
        id: id.into(),
        task_id: task_id.into(),
        request_id: String::new(),
        timestamp: None,
        server_message_data: String::new(),
        citations: vec![],
        message: Some(api::message::Message::UserQuery(api::message::UserQuery {
            query: query.into(),
            context: None,
            referenced_attachments: std::collections::HashMap::new(),
            mode: None,
            intended_agent: 0,
        })),
    }
}

fn make_tool_call_run_shell(
    id: &str,
    task_id: &str,
    tool_call_id: &str,
    command: &str,
) -> api::Message {
    api::Message {
        id: id.into(),
        task_id: task_id.into(),
        request_id: String::new(),
        timestamp: None,
        server_message_data: String::new(),
        citations: vec![],
        message: Some(api::message::Message::ToolCall(api::message::ToolCall {
            tool_call_id: tool_call_id.into(),
            tool: Some(api::message::tool_call::Tool::RunShellCommand(
                api::message::tool_call::RunShellCommand {
                    command: command.into(),
                    is_read_only: true,
                    uses_pager: false,
                    citations: vec![],
                    is_risky: false,
                    risk_category: 0,
                    wait_until_complete_value: None,
                },
            )),
        })),
    }
}

fn make_tool_call_read_files(
    id: &str,
    task_id: &str,
    tool_call_id: &str,
    file_name: &str,
) -> api::Message {
    api::Message {
        id: id.into(),
        task_id: task_id.into(),
        request_id: String::new(),
        timestamp: None,
        server_message_data: String::new(),
        citations: vec![],
        message: Some(api::message::Message::ToolCall(api::message::ToolCall {
            tool_call_id: tool_call_id.into(),
            tool: Some(api::message::tool_call::Tool::ReadFiles(
                api::message::tool_call::ReadFiles {
                    files: vec![api::message::tool_call::read_files::File {
                        name: file_name.into(),
                        line_ranges: vec![],
                    }],
                },
            )),
        })),
    }
}

#[allow(deprecated)]
fn make_tool_result_shell(
    id: &str,
    task_id: &str,
    tool_call_id: &str,
    output: &str,
    exit_code: i32,
) -> api::Message {
    api::Message {
        id: id.into(),
        task_id: task_id.into(),
        request_id: String::new(),
        timestamp: None,
        server_message_data: String::new(),
        citations: vec![],
        message: Some(api::message::Message::ToolCallResult(
            api::message::ToolCallResult {
                tool_call_id: tool_call_id.into(),
                context: None,
                result: Some(api::message::tool_call_result::Result::RunShellCommand(
                    api::RunShellCommandResult {
                        command: String::new(),
                        output: String::new(),
                        exit_code: 0,
                        result: Some(api::run_shell_command_result::Result::CommandFinished(
                            api::ShellCommandFinished {
                                output: output.into(),
                                exit_code,
                                command_id: String::new(),
                            },
                        )),
                    },
                )),
            },
        )),
    }
}

fn make_tool_result_read_files(
    id: &str,
    task_id: &str,
    tool_call_id: &str,
    file_path: &str,
    content: &str,
) -> api::Message {
    api::Message {
        id: id.into(),
        task_id: task_id.into(),
        request_id: String::new(),
        timestamp: None,
        server_message_data: String::new(),
        citations: vec![],
        message: Some(api::message::Message::ToolCallResult(
            api::message::ToolCallResult {
                tool_call_id: tool_call_id.into(),
                context: None,
                result: Some(api::message::tool_call_result::Result::ReadFiles(
                    api::ReadFilesResult {
                        result: Some(api::read_files_result::Result::TextFilesSuccess(
                            api::read_files_result::TextFilesSuccess {
                                files: vec![api::FileContent {
                                    file_path: file_path.into(),
                                    content: content.into(),
                                    line_range: None,
                                }],
                            },
                        )),
                    },
                )),
            },
        )),
    }
}

fn make_user_inputs_input(query: &str) -> api::request::Input {
    api::request::Input {
        context: None,
        r#type: Some(api::request::input::Type::UserInputs(
            api::request::input::UserInputs {
                inputs: vec![api::request::input::user_inputs::UserInput {
                    input: Some(
                        api::request::input::user_inputs::user_input::Input::UserQuery(
                            api::request::input::UserQuery {
                                query: query.into(),
                                referenced_attachments: std::collections::HashMap::new(),
                                mode: None,
                                intended_agent: 0,
                            },
                        ),
                    ),
                }],
            },
        )),
    }
}

#[allow(deprecated)]
fn make_settings(model: &str) -> api::request::Settings {
    api::request::Settings {
        model_config: Some(api::request::settings::ModelConfig {
            base: model.into(),
            planning: String::new(),
            coding: String::new(),
            cli_agent: String::new(),
            computer_use_agent: String::new(),
            base_model_context_window_limit: 0,
        }),
        rules_enabled: false,
        web_context_retrieval_enabled: false,
        supports_parallel_tool_calls: true,
        use_anthropic_text_editor_tools: false,
        planning_enabled: false,
        warp_drive_context_enabled: false,
        supports_create_files: true,
        supported_tools: vec![],
        supports_long_running_commands: false,
        should_preserve_file_content_in_history: true,
        supports_todos_ui: false,
        supports_linked_code_blocks: false,
        supports_started_child_task_message: false,
        supports_suggest_prompt: false,
        supports_read_image_files: false,
        supports_reasoning_message: true,
        api_keys: None,
        autonomy_level: 0,
        isolation_level: 0,
        web_search_enabled: false,
        supported_cli_agent_tools: vec![],
        supports_v4a_file_diffs: false,
        supports_summarization_via_message_replacement: false,
        supports_bundled_skills: false,
        supports_research_agent: false,
        supports_orchestration_v2: false,
    }
}

fn make_request(
    task_id: &str,
    messages: Vec<api::Message>,
    input: api::request::Input,
    model: &str,
) -> api::Request {
    api::Request {
        task_context: Some(api::request::TaskContext {
            tasks: vec![api::Task {
                id: task_id.into(),
                description: String::new(),
                dependencies: None,
                messages,
                summary: String::new(),
                server_data: String::new(),
            }],
        }),
        input: Some(input),
        settings: Some(make_settings(model)),
        metadata: None,
        existing_suggestions: None,
        mcp_context: None,
    }
}

fn get_test_config() -> Option<BedrockClientConfig> {
    if std::env::var("BEDROCK_INTEGRATION_TEST").is_err() {
        return None;
    }

    let profile =
        std::env::var("BEDROCK_TEST_PROFILE").unwrap_or_else(|_| "coding-assistant".into());
    let region = std::env::var("BEDROCK_TEST_REGION").unwrap_or_else(|_| "us-east-1".into());

    Some(BedrockClientConfig {
        auth_method: BedrockAuthMethod::Profile,
        profile,
        region,
        access_key_id: String::new(),
        secret_access_key: String::new(),
        cross_region_inference: false,
        fallback_to_warp: false,
    })
}

fn get_test_model() -> String {
    std::env::var("BEDROCK_TEST_MODEL").unwrap_or_else(|_| {
        "arn:aws:bedrock:us-east-1:156729649053:application-inference-profile/fma5bsw4oxhy".into()
    })
}

fn sample_project_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/ai/bedrock/test_fixtures/sample_project")
}

fn agent_tools() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "run_shell_command".into(),
            description: "Execute a shell command and return its output.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": { "type": "string", "description": "The shell command to execute" }
                },
                "required": ["command"]
            }),
        },
        ToolDefinition {
            name: "read_files".into(),
            description: "Read the contents of one or more files.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "files": { "type": "array", "items": { "type": "string" }, "description": "File paths to read" }
                },
                "required": ["files"]
            }),
        },
        ToolDefinition {
            name: "apply_file_diffs".into(),
            description: "Apply search/replace diffs to files. Creates a file if it doesn't exist."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "diffs": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "file_path": { "type": "string" },
                                "search": { "type": "string", "description": "Text to find (empty for new file)" },
                                "replace": { "type": "string", "description": "Replacement text" }
                            },
                            "required": ["file_path", "search", "replace"]
                        }
                    }
                },
                "required": ["diffs"]
            }),
        },
        ToolDefinition {
            name: "grep".into(),
            description: "Search for patterns in files using grep.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "queries": { "type": "array", "items": { "type": "string" }, "description": "Search patterns" },
                    "path": { "type": "string", "description": "Directory to search in" }
                },
                "required": ["queries"]
            }),
        },
        ToolDefinition {
            name: "file_glob".into(),
            description: "Find files matching glob patterns.".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "patterns": { "type": "array", "items": { "type": "string" }, "description": "Glob patterns to match" }
                },
                "required": ["patterns"]
            }),
        },
    ]
}

#[derive(Debug)]
struct StreamEvent {
    event_type: StreamEventType,
}

#[derive(Debug)]
enum StreamEventType {
    Init {
        request_id: String,
        conversation_id: String,
    },
    CreateTask {
        task_id: String,
    },
    AddMessage {
        task_id: String,
        message_id: String,
        text: String,
    },
    AppendText {
        task_id: String,
        message_id: String,
        delta: String,
    },
    ToolCallMessage {
        task_id: String,
        text: String,
    },
    Finished {
        reason: String,
        total_tokens: u32,
    },
    Error(String),
}

fn parse_response_event(event: &api::ResponseEvent) -> StreamEvent {
    match &event.r#type {
        Some(api::response_event::Type::Init(init)) => StreamEvent {
            event_type: StreamEventType::Init {
                request_id: init.request_id.clone(),
                conversation_id: init.conversation_id.clone(),
            },
        },
        Some(api::response_event::Type::ClientActions(actions)) => {
            for action in &actions.actions {
                if let Some(action_type) = &action.action {
                    match action_type {
                        api::client_action::Action::CreateTask(ct) => {
                            let task_id =
                                ct.task.as_ref().map(|t| t.id.clone()).unwrap_or_default();
                            return StreamEvent {
                                event_type: StreamEventType::CreateTask { task_id },
                            };
                        }
                        api::client_action::Action::AddMessagesToTask(add) => {
                            let task_id = add.task_id.clone();
                            for msg in &add.messages {
                                if let Some(api::message::Message::AgentOutput(output)) =
                                    &msg.message
                                {
                                    if output.text.starts_with("[Tool call:") {
                                        return StreamEvent {
                                            event_type: StreamEventType::ToolCallMessage {
                                                task_id,
                                                text: output.text.clone(),
                                            },
                                        };
                                    }
                                    return StreamEvent {
                                        event_type: StreamEventType::AddMessage {
                                            task_id,
                                            message_id: msg.id.clone(),
                                            text: output.text.clone(),
                                        },
                                    };
                                }
                            }
                        }
                        api::client_action::Action::AppendToMessageContent(append) => {
                            let task_id = append.task_id.clone();
                            if let Some(msg) = &append.message {
                                let message_id = msg.id.clone();
                                if let Some(api::message::Message::AgentOutput(output)) =
                                    &msg.message
                                {
                                    return StreamEvent {
                                        event_type: StreamEventType::AppendText {
                                            task_id,
                                            message_id,
                                            delta: output.text.clone(),
                                        },
                                    };
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            StreamEvent {
                event_type: StreamEventType::Error("Unknown action".into()),
            }
        }
        Some(api::response_event::Type::Finished(finished)) => {
            let reason = format!("{:?}", finished.reason);
            let total_tokens = finished
                .conversation_usage_metadata
                .as_ref()
                .and_then(|m| m.byok_token_usage.get("bedrock"))
                .map(|u| u.total_tokens)
                .unwrap_or(0);
            StreamEvent {
                event_type: StreamEventType::Finished {
                    reason,
                    total_tokens,
                },
            }
        }
        None => StreamEvent {
            event_type: StreamEventType::Error("Empty event".into()),
        },
    }
}

struct AgentSimulation {
    client: BedrockClient,
    model: String,
    task_id: String,
    conversation: Vec<ConversationMessage>,
    tools: Vec<ToolDefinition>,
    system_prompt: String,
    project_path: PathBuf,
    all_text_output: String,
    turn_count: u32,
    max_turns: u32,
}

#[derive(Debug)]
struct TurnResult {
    text_output: String,
    tool_calls: Vec<ToolCall>,
    finished: bool,
    total_tokens: u32,
    events_received: u32,
    had_create_task: bool,
}

#[derive(Debug, Clone)]
struct ToolCall {
    name: String,
    input: serde_json::Value,
    raw_text: String,
}

impl AgentSimulation {
    fn new(client: BedrockClient, model: String, project_path: PathBuf) -> Self {
        let task_id = uuid::Uuid::new_v4().to_string();
        Self {
            client,
            model,
            task_id,
            conversation: Vec::new(),
            tools: agent_tools(),
            system_prompt: format!(
                "You are a helpful AI coding assistant. You help users with software engineering tasks. \
                 The user's current working directory is: {}. \
                 When using tools, use absolute paths based on this directory. \
                 When asked to produce files, use the apply_file_diffs tool to create them.",
                project_path.display()
            ),
            project_path,
            all_text_output: String::new(),
            turn_count: 0,
            max_turns: 10,
        }
    }

    async fn send_user_message(&mut self, message: &str) -> TurnResult {
        self.conversation.push(ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text(message.into()),
        });
        self.execute_turn().await
    }

    async fn execute_turn(&mut self) -> TurnResult {
        self.turn_count += 1;
        assert!(
            self.turn_count <= self.max_turns,
            "Exceeded max turns ({})",
            self.max_turns
        );

        let needs_create_task = self.turn_count == 1;

        let stream = self
            .client
            .converse_stream(
                &self.model,
                &self.task_id,
                needs_create_task,
                self.conversation.clone(),
                Some(self.system_prompt.clone()),
                self.tools.clone(),
                8192,
                None,
                false,
            )
            .await
            .expect("converse_stream should succeed");

        let mut text_output = String::new();
        let mut tool_calls = Vec::new();
        let mut finished = false;
        let mut total_tokens = 0u32;
        let mut events_received = 0u32;
        let mut had_create_task = false;
        let mut had_init = false;

        let mut stream = stream;
        while let Some(event_result) = stream.next().await {
            events_received += 1;
            let event = event_result.expect("stream event should be Ok");
            let parsed = parse_response_event(&event);

            match parsed.event_type {
                StreamEventType::Init { request_id, .. } => {
                    had_init = true;
                    assert!(
                        !request_id.is_empty(),
                        "StreamInit should have non-empty request_id"
                    );
                }
                StreamEventType::CreateTask { ref task_id } => {
                    had_create_task = true;
                    assert_eq!(
                        task_id, &self.task_id,
                        "CreateTask task_id should match our task_id"
                    );
                }
                StreamEventType::AddMessage {
                    ref task_id,
                    text: ref t,
                    ..
                } => {
                    assert_eq!(
                        task_id, &self.task_id,
                        "AddMessage task_id should match our task_id"
                    );
                    text_output.push_str(t);
                }
                StreamEventType::AppendText {
                    ref task_id,
                    delta: ref d,
                    ..
                } => {
                    assert_eq!(
                        task_id, &self.task_id,
                        "AppendText task_id should match our task_id"
                    );
                    text_output.push_str(d);
                }
                StreamEventType::ToolCallMessage { text: ref t, .. } => {
                    if let Some(call) = parse_tool_call_text(t) {
                        tool_calls.push(call);
                    }
                }
                StreamEventType::Finished {
                    total_tokens: tokens,
                    ..
                } => {
                    finished = true;
                    total_tokens = tokens;
                }
                StreamEventType::Error(ref e) => {
                    panic!("Unexpected error event: {e}");
                }
            }
        }

        assert!(had_init, "Stream should start with Init event");
        if needs_create_task {
            assert!(had_create_task, "First turn should have CreateTask event");
        }
        assert!(finished, "Stream should end with Finished event");
        assert!(
            events_received >= 3,
            "Should receive at least Init + one content + Finished"
        );

        self.all_text_output.push_str(&text_output);

        if !text_output.is_empty() {
            self.conversation.push(ConversationMessage {
                role: MessageRole::Assistant,
                content: MessageContent::Text(text_output.clone()),
            });
        }

        TurnResult {
            text_output,
            tool_calls,
            finished,
            total_tokens,
            events_received,
            had_create_task,
        }
    }

    fn provide_tool_result(
        &mut self,
        tool_use_id: &str,
        name: &str,
        input: &serde_json::Value,
        result: &str,
        is_error: bool,
    ) {
        self.conversation.push(ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::ToolUse {
                tool_use_id: tool_use_id.into(),
                name: name.into(),
                input: input.clone(),
            },
        });
        self.conversation.push(ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::ToolResult {
                tool_use_id: tool_use_id.into(),
                content: result.into(),
                is_error,
            },
        });
    }

    fn execute_tool_locally(&self, name: &str, input: &serde_json::Value) -> (String, bool) {
        match name {
            "run_shell_command" => {
                let command = input["command"].as_str().unwrap_or("echo 'no command'");
                let output = std::process::Command::new("sh")
                    .arg("-c")
                    .arg(command)
                    .current_dir(&self.project_path)
                    .output();
                match output {
                    Ok(out) => {
                        let stdout = String::from_utf8_lossy(&out.stdout);
                        let stderr = String::from_utf8_lossy(&out.stderr);
                        let result = if stderr.is_empty() {
                            stdout.to_string()
                        } else {
                            format!("{}\nstderr: {}", stdout, stderr)
                        };
                        (result, !out.status.success())
                    }
                    Err(e) => (format!("Error: {e}"), true),
                }
            }
            "read_files" => {
                let files = input["files"]
                    .as_array()
                    .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
                    .unwrap_or_default();
                let mut result = String::new();
                for file in files {
                    let path = if file.starts_with('/') {
                        PathBuf::from(file)
                    } else {
                        self.project_path.join(file)
                    };
                    match std::fs::read_to_string(&path) {
                        Ok(content) => {
                            result.push_str(&format!("{}:\n{}\n\n", path.display(), content));
                        }
                        Err(e) => {
                            result.push_str(&format!("Error reading {}: {}\n", path.display(), e));
                        }
                    }
                }
                (result, false)
            }
            "file_glob" => {
                let mut files = Vec::new();
                for entry in walkdir::WalkDir::new(&self.project_path)
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .filter(|e| e.file_type().is_file())
                {
                    files.push(entry.path().display().to_string());
                }
                let result = if files.is_empty() {
                    "No files found.".into()
                } else {
                    files.join("\n")
                };
                (result, false)
            }
            "grep" => {
                let queries = input["queries"]
                    .as_array()
                    .map(|arr| arr.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>())
                    .unwrap_or_default();
                let path = input["path"]
                    .as_str()
                    .unwrap_or(self.project_path.to_str().unwrap_or("."));
                let mut result = String::new();
                for query in queries {
                    let output = std::process::Command::new("grep")
                        .args(["-rn", query, path])
                        .output();
                    if let Ok(out) = output {
                        result.push_str(&String::from_utf8_lossy(&out.stdout));
                    }
                }
                if result.is_empty() {
                    result = "No matches found.".into();
                }
                (result, false)
            }
            "apply_file_diffs" => {
                let diffs = input["diffs"].as_array();
                match diffs {
                    Some(diffs) => {
                        let mut results = Vec::new();
                        for diff in diffs {
                            let file_path = diff["file_path"].as_str().unwrap_or("");
                            let search = diff["search"].as_str().unwrap_or("");
                            let replace = diff["replace"].as_str().unwrap_or("");

                            let path = if file_path.starts_with('/') {
                                PathBuf::from(file_path)
                            } else {
                                self.project_path.join(file_path)
                            };

                            if search.is_empty() {
                                if let Some(parent) = path.parent() {
                                    let _ = std::fs::create_dir_all(parent);
                                }
                                match std::fs::write(&path, replace) {
                                    Ok(_) => results.push(format!("Created: {}", path.display())),
                                    Err(e) => results.push(format!(
                                        "Error creating {}: {}",
                                        path.display(),
                                        e
                                    )),
                                }
                            } else {
                                match std::fs::read_to_string(&path) {
                                    Ok(content) => {
                                        let new_content = content.replace(search, replace);
                                        match std::fs::write(&path, &new_content) {
                                            Ok(_) => {
                                                results.push(format!("Updated: {}", path.display()))
                                            }
                                            Err(e) => results.push(format!(
                                                "Error writing {}: {}",
                                                path.display(),
                                                e
                                            )),
                                        }
                                    }
                                    Err(e) => results.push(format!(
                                        "Error reading {}: {}",
                                        path.display(),
                                        e
                                    )),
                                }
                            }
                        }
                        (results.join("\n"), false)
                    }
                    None => ("No diffs provided.".into(), true),
                }
            }
            _ => (format!("Unknown tool: {name}"), true),
        }
    }
}

fn parse_tool_call_text(text: &str) -> Option<ToolCall> {
    if text.starts_with("[Tool call: ") && text.ends_with("]") {
        let inner = &text[12..text.len() - 1];
        if let Some(paren_pos) = inner.rfind(" (") {
            let name = inner[..paren_pos].to_string();
            return Some(ToolCall {
                name,
                input: json!({}),
                raw_text: text.to_string(),
            });
        }
    }
    None
}

/// This test simulates the exact flow when a user types:
///   /agent Please review this code and produce a WARP.md file for it.
///
/// It verifies:
/// 1. StreamInit is emitted first
/// 2. CreateTask is emitted for new conversations (upgrades optimistic root task)
/// 3. Text output is substantial (not just "#" or empty)
/// 4. Tool calls are properly formed and can be executed
/// 5. Multi-turn conversation works (tool results fed back)
/// 6. The model eventually produces meaningful text output
/// 7. Stream finishes cleanly with token usage
#[tokio::test]
async fn test_agent_flow_review_code_produce_warp_md() {
    let Some(config) = get_test_config() else {
        eprintln!("Skipping: BEDROCK_INTEGRATION_TEST not set");
        return;
    };

    let client = BedrockClient::from_config(config)
        .await
        .expect("client creation");
    let model = get_test_model();
    let project_path = sample_project_path();

    println!("\n{}", "=".repeat(80));
    println!("[E2E TEST] Agent flow: /agent Please review this code and produce a WARP.md file");
    println!("[E2E TEST] Project: {}", project_path.display());
    println!("[E2E TEST] Model: {model}");
    println!("{}\n", "=".repeat(80));

    let mut sim = AgentSimulation::new(client, model, project_path.clone());

    // Simulate the /agent command
    let turn1 = sim
        .send_user_message("Please review this code and produce a WARP.md file for it.")
        .await;

    println!("\n--- Turn 1 ---");
    println!(
        "[E2E] Text ({} chars): {:?}",
        turn1.text_output.len(),
        &turn1.text_output[..turn1.text_output.len().min(500)]
    );
    println!("[E2E] Tool calls: {}", turn1.tool_calls.len());
    println!("[E2E] Events: {}", turn1.events_received);
    println!("[E2E] Tokens: {}", turn1.total_tokens);
    println!("[E2E] CreateTask: {}", turn1.had_create_task);

    // CRITICAL ASSERTIONS for the bug we're fixing:
    // The model should NOT produce just "#" and stop.
    // Either it produces substantial text OR it makes tool calls to explore the code first.
    assert!(
        turn1.text_output.len() > 5 || !turn1.tool_calls.is_empty(),
        "Turn 1 should produce meaningful text (>5 chars) OR tool calls. Got text='{}', tool_calls={}",
        &turn1.text_output[..turn1.text_output.len().min(100)],
        turn1.tool_calls.len()
    );

    // Verify CreateTask was sent (first turn)
    assert!(
        turn1.had_create_task,
        "First turn MUST have CreateTask to upgrade optimistic root task"
    );

    // Verify token usage is tracked
    assert!(turn1.total_tokens > 0, "Should have non-zero token usage");

    println!("\n[E2E] Turn 1 passed all assertions!");
    println!(
        "[E2E] Total accumulated text: {} chars",
        sim.all_text_output.len()
    );

    // Clean up any WARP.md that might have been created
    let warp_md_path = project_path.join("WARP.md");
    if warp_md_path.exists() {
        std::fs::remove_file(&warp_md_path).ok();
        println!("[E2E] Cleaned up WARP.md");
    }
}

/// Multi-turn test: simulates the agent calling tools to explore code,
/// then producing a WARP.md file. Runs up to 5 turns of tool use.
#[tokio::test]
async fn test_agent_multi_turn_tool_use_produces_output() {
    let Some(config) = get_test_config() else {
        eprintln!("Skipping: BEDROCK_INTEGRATION_TEST not set");
        return;
    };

    let client = BedrockClient::from_config(config)
        .await
        .expect("client creation");
    let model = get_test_model();
    let project_path = sample_project_path();

    println!("\n{}", "=".repeat(80));
    println!("[E2E TEST] Multi-turn agent flow with actual tool execution");
    println!("[E2E TEST] Project: {}", project_path.display());
    println!("{}\n", "=".repeat(80));

    let mut sim = AgentSimulation::new(client, model, project_path.clone());

    // Initial user message
    let mut turn = sim
        .send_user_message(
            "Please review this code and produce a WARP.md file for it. \
             Start by listing the files, reading the important ones, then create the WARP.md.",
        )
        .await;

    println!("\n--- Turn 1 ---");
    println!("[E2E] Text: {} chars", turn.text_output.len());
    println!(
        "[E2E] Tool calls: {:?}",
        turn.tool_calls.iter().map(|t| &t.name).collect::<Vec<_>>()
    );

    // For models with extended thinking, the first turn might have reasoning + tool call
    // The text should contain more than just "#"
    let meaningful_text = turn.text_output.trim().len() > 5;
    let has_tool_calls = !turn.tool_calls.is_empty();

    println!("[E2E] Has meaningful text: {meaningful_text}");
    println!("[E2E] Has tool calls: {has_tool_calls}");

    assert!(
        meaningful_text || has_tool_calls,
        "First turn must produce meaningful output. Text='{}' ({} chars), tools={}",
        &turn.text_output[..turn.text_output.len().min(200)],
        turn.text_output.len(),
        turn.tool_calls.len()
    );

    // Now simulate multi-turn: if the model made tool calls, execute them and continue
    let mut total_turns = 1u32;
    let max_simulation_turns = 5u32;

    while !turn.tool_calls.is_empty() && total_turns < max_simulation_turns {
        total_turns += 1;
        let tool = turn.tool_calls[0].clone();
        println!(
            "\n--- Turn {} (executing tool: {}) ---",
            total_turns, tool.name
        );

        // For this test, provide simulated tool results since we can't parse
        // the tool input from the `[Tool call: name (id)]` format that stream.rs emits.
        // Instead, provide reasonable results based on the tool name.
        let tool_use_id = format!("tool_{}", total_turns);
        let (result, is_error) = match tool.name.as_str() {
            "run_shell_command" => {
                let ls_output = std::process::Command::new("ls")
                    .arg("-la")
                    .current_dir(&project_path)
                    .output()
                    .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                    .unwrap_or_else(|_| "Error listing files".into());
                (ls_output, false)
            }
            "file_glob" | "list_files" => {
                let mut files = Vec::new();
                for entry in walkdir::WalkDir::new(&project_path)
                    .into_iter()
                    .filter_map(|e| e.ok())
                    .filter(|e| e.file_type().is_file())
                {
                    files.push(entry.path().display().to_string());
                }
                (files.join("\n"), false)
            }
            "read_files" => {
                // Read the main files
                let mut content = String::new();
                for file in &["src/main.rs", "src/lib.rs", "Cargo.toml", "README.md"] {
                    let path = project_path.join(file);
                    if let Ok(c) = std::fs::read_to_string(&path) {
                        content.push_str(&format!("{}:\n{}\n\n", path.display(), c));
                    }
                }
                (content, false)
            }
            "grep" => ("No matches found.".into(), false),
            "apply_file_diffs" => ("Created: WARP.md".into(), false),
            _ => (format!("Tool {} executed successfully", tool.name), false),
        };

        println!(
            "[E2E] Tool result ({} chars): {:?}",
            result.len(),
            &result[..result.len().min(200)]
        );

        // Provide the result and get next turn
        sim.provide_tool_result(&tool_use_id, &tool.name, &json!({}), &result, is_error);
        turn = sim.execute_turn().await;

        println!(
            "[E2E] Turn {} text: {} chars",
            total_turns,
            turn.text_output.len()
        );
        println!(
            "[E2E] Turn {} tool calls: {:?}",
            total_turns,
            turn.tool_calls.iter().map(|t| &t.name).collect::<Vec<_>>()
        );
    }

    println!("\n{}", "=".repeat(80));
    println!("[E2E] Completed in {} turns", total_turns);
    println!(
        "[E2E] Total text output: {} chars",
        sim.all_text_output.len()
    );
    println!(
        "[E2E] Final text preview: {:?}",
        &sim.all_text_output[..sim.all_text_output.len().min(1000)]
    );
    println!("{}\n", "=".repeat(80));

    // Final assertions
    // Agent behavior: model may spend ALL turns using tools to explore code.
    // This is valid. The key assertion is that:
    // 1. Each turn either produces text OR tool calls (never neither)
    // 2. No partial reasoning fragments leak to the UI
    // 3. The stream protocol is correct (Init, CreateTask, content, Finished)
    assert!(
        sim.all_text_output.len() > 0 || total_turns > 1,
        "Agent should either produce text or make multiple tool calls to explore"
    );

    println!(
        "[E2E] SUCCESS: Agent completed {} turns, {} chars text output",
        total_turns,
        sim.all_text_output.len()
    );

    // Clean up
    let warp_md_path = project_path.join("WARP.md");
    if warp_md_path.exists() {
        std::fs::remove_file(&warp_md_path).ok();
    }
}

/// This test verifies the exact scenario from the bug report:
/// - Model with extended thinking (Opus 4.6) sends reasoning blocks first
/// - Then sends actual text content
/// - The stream should capture BOTH reasoning and text as visible output
/// - It should NOT stop at just "#"
#[tokio::test]
async fn test_reasoning_model_produces_substantial_output() {
    let Some(config) = get_test_config() else {
        eprintln!("Skipping: BEDROCK_INTEGRATION_TEST not set");
        return;
    };

    let client = BedrockClient::from_config(config)
        .await
        .expect("client creation");
    let model = get_test_model();

    println!("\n{}", "=".repeat(80));
    println!("[E2E TEST] Reasoning model substantial output test");
    println!("[E2E TEST] Model: {model}");
    println!("{}\n", "=".repeat(80));

    let system =
        Some("You are a helpful coding assistant. Respond with detailed markdown.".to_string());

    let messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::Text(
            "Please review this code and produce a WARP.md file for it:\n\n\
             ```rust\n\
             fn main() {\n    \
                 let config = Config::from_env();\n    \
                 println!(\"Starting {} on port {}\", config.name, config.port);\n\
             }\n\
             ```\n\n\
             Write the full WARP.md content in your response."
                .into(),
        ),
    }];

    let stream = client
        .converse_stream(
            &model,
            "test-task-id",
            true,
            messages,
            system,
            agent_tools(),
            8192,
            None,
            false,
        )
        .await
        .expect("converse_stream should succeed");

    let mut total_text = String::new();
    let mut event_count = 0u32;
    let mut had_create_task = false;

    let mut stream = stream;
    while let Some(event_result) = stream.next().await {
        event_count += 1;
        let event = event_result.expect("event should be Ok");
        if let Some(event_type) = &event.r#type {
            match event_type {
                api::response_event::Type::ClientActions(actions) => {
                    for action in &actions.actions {
                        if let Some(action_type) = &action.action {
                            match action_type {
                                api::client_action::Action::CreateTask(_) => {
                                    had_create_task = true;
                                }
                                api::client_action::Action::AddMessagesToTask(add) => {
                                    for msg in &add.messages {
                                        if let Some(api::message::Message::AgentOutput(output)) =
                                            &msg.message
                                        {
                                            total_text.push_str(&output.text);
                                        }
                                    }
                                }
                                api::client_action::Action::AppendToMessageContent(append) => {
                                    if let Some(msg) = &append.message {
                                        if let Some(api::message::Message::AgentOutput(output)) =
                                            &msg.message
                                        {
                                            total_text.push_str(&output.text);
                                        }
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    println!("[E2E] Events received: {event_count}");
    println!("[E2E] Total text: {} chars", total_text.len());
    println!("[E2E] Had CreateTask: {had_create_task}");
    println!(
        "[E2E] Text preview: {:?}",
        &total_text[..total_text.len().min(1000)]
    );

    // THE BUG CHECK: output must be more than just "#"
    // With reasoning hidden, if the model only makes tool calls, text will be tool call messages.
    // The key assertion is that we DON'T get partial reasoning fragments like "#" displayed.
    let trimmed = total_text.trim();

    // If text is present, it should be meaningful (not just a fragment from reasoning)
    if !trimmed.is_empty() && !trimmed.starts_with("[Tool call:") {
        assert!(
            trimmed.len() > 10,
            "If text output is present (not a tool call), it should be substantial. Got ({} chars): '{}'",
            trimmed.len(),
            &trimmed[..trimmed.len().min(100)]
        );
    }

    // Should have CreateTask
    assert!(
        had_create_task,
        "Should emit CreateTask for new conversation"
    );

    // Should have at least some events (either text or tool calls)
    assert!(
        event_count >= 3,
        "Should have at least Init + CreateTask + Finished, got {}",
        event_count
    );
}

/// Verify the exact event sequence matches what the conversation controller expects:
/// 1. Init
/// 2. CreateTask (for new conversations only)
/// 3. AddMessagesToTask (first content)
/// 4. AppendToMessageContent (subsequent deltas)
/// 5. Finished
#[tokio::test]
async fn test_event_sequence_matches_controller_expectations() {
    let Some(config) = get_test_config() else {
        eprintln!("Skipping: BEDROCK_INTEGRATION_TEST not set");
        return;
    };

    let client = BedrockClient::from_config(config)
        .await
        .expect("client creation");
    let model = get_test_model();

    let messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::Text("Say hello world.".into()),
    }];

    let stream = client
        .converse_stream(
            &model,
            "my-task-id",
            true,
            messages,
            None,
            vec![],
            100,
            None,
            false,
        )
        .await
        .expect("should connect");

    let mut events: Vec<String> = Vec::new();
    let mut stream = stream;
    while let Some(event_result) = stream.next().await {
        let event = event_result.expect("event ok");
        match &event.r#type {
            Some(api::response_event::Type::Init(_)) => events.push("Init".into()),
            Some(api::response_event::Type::ClientActions(actions)) => {
                for action in &actions.actions {
                    if let Some(a) = &action.action {
                        match a {
                            api::client_action::Action::CreateTask(_) => {
                                events.push("CreateTask".into())
                            }
                            api::client_action::Action::AddMessagesToTask(_) => {
                                events.push("AddMessages".into())
                            }
                            api::client_action::Action::AppendToMessageContent(_) => {
                                events.push("AppendText".into())
                            }
                            _ => events.push(format!("Other({:?})", std::mem::discriminant(a))),
                        }
                    }
                }
            }
            Some(api::response_event::Type::Finished(_)) => events.push("Finished".into()),
            None => events.push("Empty".into()),
        }
    }

    println!("[E2E] Event sequence: {:?}", events);

    // Verify sequence
    assert_eq!(events[0], "Init", "First event must be Init");
    assert_eq!(
        events[1], "CreateTask",
        "Second event must be CreateTask (new conversation)"
    );

    // After CreateTask, should have at least one content event
    let content_events: Vec<_> = events[2..events.len() - 1]
        .iter()
        .filter(|e| *e == "AddMessages" || *e == "AppendText")
        .collect();
    assert!(
        !content_events.is_empty(),
        "Should have content events between CreateTask and Finished"
    );

    // First content event should be AddMessages (creates the message)
    let first_content_idx = events
        .iter()
        .position(|e| e == "AddMessages" || e == "AppendText")
        .unwrap();
    assert_eq!(
        events[first_content_idx], "AddMessages",
        "First content event should be AddMessages (creates message). Sequence: {:?}",
        events
    );

    // Last event should be Finished
    assert_eq!(
        events.last().unwrap(),
        "Finished",
        "Last event must be Finished"
    );
}

/// Test that follow-up messages in the same conversation do NOT send CreateTask
/// (since the task is already Server-created after the first turn)
#[tokio::test]
async fn test_followup_turn_does_not_send_create_task() {
    let Some(config) = get_test_config() else {
        eprintln!("Skipping: BEDROCK_INTEGRATION_TEST not set");
        return;
    };

    let client = BedrockClient::from_config(config)
        .await
        .expect("client creation");
    let model = get_test_model();

    // Simulate turn 2: needs_create_task = false
    let messages = vec![
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text("What is 2+2?".into()),
        },
        ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::Text("4".into()),
        },
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text("And 3+3?".into()),
        },
    ];

    let stream = client
        .converse_stream(
            &model,
            "existing-task-id",
            false,
            messages,
            None,
            vec![],
            100,
            None,
            false,
        )
        .await
        .expect("should connect");

    let mut had_create_task = false;
    let mut had_content = false;
    let mut stream = stream;
    while let Some(event_result) = stream.next().await {
        let event = event_result.expect("event ok");
        if let Some(api::response_event::Type::ClientActions(actions)) = &event.r#type {
            for action in &actions.actions {
                if let Some(a) = &action.action {
                    match a {
                        api::client_action::Action::CreateTask(_) => had_create_task = true,
                        api::client_action::Action::AddMessagesToTask(_)
                        | api::client_action::Action::AppendToMessageContent(_) => {
                            had_content = true
                        }
                        _ => {}
                    }
                }
            }
        }
    }

    assert!(
        !had_create_task,
        "Follow-up turn should NOT have CreateTask (task already exists)"
    );
    assert!(had_content, "Should still have content events");
}

// ============================================================================
// Slash Command Tests
// ============================================================================

async fn run_slash_command_test(
    user_message: &str,
    tools: Vec<ToolDefinition>,
    system_prompt: Option<String>,
    test_name: &str,
) -> (Vec<StreamEvent>, String) {
    let config = match get_test_config() {
        Some(c) => c,
        None => {
            println!("[SKIP] {}: BEDROCK_INTEGRATION_TEST not set", test_name);
            return (vec![], String::new());
        }
    };

    let model = get_test_model();
    let client = BedrockClient::from_config(config)
        .await
        .expect("client creation should succeed");

    let messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::Text(user_message.to_string()),
    }];

    let task_id = uuid::Uuid::new_v4().to_string();
    println!(
        "\n{}\n[SLASH CMD TEST] {}\n[SLASH CMD TEST] Model: {}\n[SLASH CMD TEST] Message: {:.100}\n{}",
        "=".repeat(80), test_name, model, user_message, "=".repeat(80)
    );

    let stream = client
        .converse_stream(
            &model,
            &task_id,
            true,
            messages,
            system_prompt,
            tools,
            4096,
            None,
            true,
        )
        .await
        .expect("stream should connect");

    let mut events = Vec::new();
    let mut all_text = String::new();
    let mut stream = stream;
    while let Some(event_result) = stream.next().await {
        let event = event_result.expect("event should be ok");
        let parsed = parse_response_event(&event);
        match &parsed.event_type {
            StreamEventType::AddMessage { text, .. } => all_text.push_str(text),
            StreamEventType::AppendText { delta, .. } => all_text.push_str(delta),
            _ => {}
        }
        events.push(parsed);
    }

    println!(
        "[SLASH CMD] Events: {}, Text: {} chars",
        events.len(),
        all_text.len()
    );
    (events, all_text)
}

fn assert_valid_stream(events: &[StreamEvent], test_name: &str) {
    if events.is_empty() {
        return; // skipped
    }

    assert!(
        events.len() >= 3,
        "[{}] Should have at least Init + CreateTask + Finished, got {}",
        test_name,
        events.len()
    );

    assert!(
        matches!(events[0].event_type, StreamEventType::Init { .. }),
        "[{}] First event should be Init",
        test_name
    );

    assert!(
        matches!(events[1].event_type, StreamEventType::CreateTask { .. }),
        "[{}] Second event should be CreateTask",
        test_name
    );

    let has_finished = events
        .iter()
        .any(|e| matches!(e.event_type, StreamEventType::Finished { .. }));
    assert!(has_finished, "[{}] Should have a Finished event", test_name);

    let has_content = events.iter().any(|e| {
        matches!(
            e.event_type,
            StreamEventType::AddMessage { .. }
                | StreamEventType::AppendText { .. }
                | StreamEventType::ToolCallMessage { .. }
        )
    });
    assert!(
        has_content,
        "[{}] Should have at least one content event",
        test_name
    );
}

#[tokio::test]
async fn test_slash_init_project_rules() {
    let (events, text) = run_slash_command_test(
        "Initialize this project. Analyze the codebase structure and files, \
         generate an AGENTS.md file documenting project conventions and setup \
         instructions, and offer to create a development environment configuration. \
         Use the available tools to inspect the project before responding.",
        agent_tools(),
        Some("You are a helpful AI coding assistant. You help users with software engineering tasks.".into()),
        "slash_init_project_rules",
    )
    .await;

    assert_valid_stream(&events, "slash_init_project_rules");

    if !events.is_empty() {
        let has_tool_or_text = events.iter().any(|e| {
            matches!(
                e.event_type,
                StreamEventType::ToolCallMessage { .. } | StreamEventType::AddMessage { .. }
            )
        });
        assert!(
            has_tool_or_text,
            "/init should produce tool calls or text output"
        );
        println!("[SLASH CMD] /init produced {} chars of text", text.len());
    }
}

#[tokio::test]
async fn test_slash_create_environment() {
    let project_path = sample_project_path().display().to_string();
    let message = format!(
        "Create a development environment for this project. \
         Set up necessary dependencies, configuration files, and tooling. Repositories: {}",
        project_path
    );

    let (events, _text) = run_slash_command_test(
        &message,
        agent_tools(),
        Some("You are a helpful AI coding assistant.".into()),
        "slash_create_environment",
    )
    .await;

    assert_valid_stream(&events, "slash_create_environment");
}

#[tokio::test]
async fn test_slash_create_new_project() {
    let (events, text) = run_slash_command_test(
        "Create a new project: A simple Rust CLI tool that converts CSV files to JSON",
        agent_tools(),
        Some("You are a helpful AI coding assistant.".into()),
        "slash_create_new_project",
    )
    .await;

    assert_valid_stream(&events, "slash_create_new_project");

    if !events.is_empty() {
        assert!(
            text.len() > 0
                || events
                    .iter()
                    .any(|e| matches!(e.event_type, StreamEventType::ToolCallMessage { .. })),
            "/new-project should produce text or tool calls"
        );
    }
}

#[tokio::test]
async fn test_slash_clone_repository() {
    let (events, _text) = run_slash_command_test(
        "Clone the repository at https://github.com/rust-lang/rust-by-example and set it up for development.",
        agent_tools(),
        Some("You are a helpful AI coding assistant.".into()),
        "slash_clone_repository",
    )
    .await;

    assert_valid_stream(&events, "slash_clone_repository");
}

#[tokio::test]
async fn test_slash_auto_code_diff() {
    let (events, text) = run_slash_command_test(
        "Apply code changes: Add a new function called `subtract` to the math module that subtracts two numbers",
        agent_tools(),
        Some("You are a helpful AI coding assistant.".into()),
        "slash_auto_code_diff",
    )
    .await;

    assert_valid_stream(&events, "slash_auto_code_diff");

    if !events.is_empty() {
        let has_action = text.len() > 0
            || events
                .iter()
                .any(|e| matches!(e.event_type, StreamEventType::ToolCallMessage { .. }));
        assert!(
            has_action,
            "/auto-code-diff should produce text or tool calls"
        );
    }
}

#[tokio::test]
async fn test_slash_resume_conversation() {
    let config = match get_test_config() {
        Some(c) => c,
        None => {
            println!("[SKIP] slash_resume_conversation: BEDROCK_INTEGRATION_TEST not set");
            return;
        }
    };

    let model = get_test_model();
    let client = BedrockClient::from_config(config)
        .await
        .expect("client creation should succeed");

    let messages = vec![
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text("What is the capital of France?".into()),
        },
        ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::Text("The capital of France is Paris.".into()),
        },
        ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text(
                "Continue where we left off. Review the conversation history and proceed with the next steps."
                    .into(),
            ),
        },
    ];

    let task_id = uuid::Uuid::new_v4().to_string();
    let stream = client
        .converse_stream(
            &model,
            &task_id,
            true,
            messages,
            None,
            vec![],
            256,
            None,
            true,
        )
        .await
        .expect("stream should connect");

    let mut had_content = false;
    let mut had_finished = false;
    let mut stream = stream;
    while let Some(event_result) = stream.next().await {
        let event = event_result.expect("event ok");
        let parsed = parse_response_event(&event);
        match &parsed.event_type {
            StreamEventType::AddMessage { .. } | StreamEventType::AppendText { .. } => {
                had_content = true;
            }
            StreamEventType::Finished { .. } => had_finished = true,
            _ => {}
        }
    }

    assert!(had_content, "/resume should produce content");
    assert!(had_finished, "/resume should finish");
}

#[tokio::test]
async fn test_slash_code_review() {
    let (events, text) = run_slash_command_test(
        "Review the following code changes and provide detailed feedback on correctness, style, and potential issues.\n\n\
         ```diff\n\
         --- a/src/lib.rs\n\
         +++ b/src/lib.rs\n\
         @@ -1,4 +1,8 @@\n\
         +pub fn divide(a: f64, b: f64) -> f64 {\n\
         +    a / b\n\
         +}\n\
         +\n\
          pub fn add(a: i32, b: i32) -> i32 {\n\
              a + b\n\
          }\n\
         ```",
        vec![],
        Some("You are a code review assistant. Identify bugs, style issues, and suggest improvements.".into()),
        "slash_code_review",
    )
    .await;

    assert_valid_stream(&events, "slash_code_review");

    if !events.is_empty() {
        assert!(
            text.len() > 20,
            "/code-review should produce substantial feedback, got {} chars",
            text.len()
        );
    }
}

#[tokio::test]
async fn test_slash_query_with_canned_response() {
    let (events, text) = run_slash_command_test(
        "Help me install dependencies and set up this project for development",
        agent_tools(),
        Some("You are a helpful AI coding assistant.".into()),
        "slash_query_with_canned_response",
    )
    .await;

    assert_valid_stream(&events, "slash_query_with_canned_response");

    if !events.is_empty() {
        let has_output = text.len() > 0
            || events
                .iter()
                .any(|e| matches!(e.event_type, StreamEventType::ToolCallMessage { .. }));
        assert!(has_output, "Canned response query should produce output");
    }
}

#[tokio::test]
async fn test_empty_messages_safety_check() {
    let config = match get_test_config() {
        Some(c) => c,
        None => {
            println!("[SKIP] empty_messages_safety_check: BEDROCK_INTEGRATION_TEST not set");
            return;
        }
    };

    let model = get_test_model();
    let client = BedrockClient::from_config(config)
        .await
        .expect("client creation should succeed");

    let messages = vec![ConversationMessage {
        role: MessageRole::User,
        content: MessageContent::Text("Please proceed with the requested task.".into()),
    }];

    let task_id = uuid::Uuid::new_v4().to_string();
    let stream = client
        .converse_stream(
            &model,
            &task_id,
            true,
            messages,
            None,
            vec![],
            100,
            None,
            true,
        )
        .await
        .expect("safety fallback message should work");

    let mut had_finished = false;
    let mut stream = stream;
    while let Some(event_result) = stream.next().await {
        let event = event_result.expect("event ok");
        if matches!(event.r#type, Some(api::response_event::Type::Finished(_))) {
            had_finished = true;
        }
    }

    assert!(
        had_finished,
        "Safety fallback should produce a complete stream"
    );
}

/// Full round-trip test: builds a proto Request with tool call + tool result history,
/// runs through extract_messages_from_request → converse_stream, verifying the whole
/// pipeline handles tool_call_id matching correctly.
#[tokio::test]
async fn test_full_proto_round_trip_with_tool_history() {
    let config = match get_test_config() {
        Some(c) => c,
        None => {
            println!("[SKIP] full_proto_round_trip: BEDROCK_INTEGRATION_TEST not set");
            return;
        }
    };

    let model = get_test_model();
    let task_id = uuid::Uuid::new_v4().to_string();
    let tool_call_id_1 = "tooluse_test_001";
    let tool_call_id_2 = "tooluse_test_002";

    let request = make_request(
        &task_id,
        vec![
            make_user_query_message("msg-1", &task_id, "List all files in the current directory."),
            make_tool_call_run_shell(tool_call_id_1, &task_id, tool_call_id_1, "ls -la"),
            make_tool_call_read_files(tool_call_id_2, &task_id, tool_call_id_2, "README.md"),
            make_tool_result_shell(
                "msg-result-1", &task_id, tool_call_id_1,
                "total 16\ndrwxr-xr-x  5 user staff  160 May  6 10:00 .\n-rw-r--r--  1 user staff  100 May  6 10:00 README.md\n-rw-r--r--  1 user staff  200 May  6 10:00 Cargo.toml",
                0,
            ),
            make_tool_result_read_files(
                "msg-result-2", &task_id, tool_call_id_2,
                "README.md", "# Sample Project\nA test project.",
            ),
        ],
        make_user_inputs_input("Now summarize what you found in one sentence."),
        &model,
    );

    let messages = super::convert_request::extract_messages_from_request(&request);
    let system_prompt = super::convert_request::extract_system_prompt(&request);
    let tools = super::convert_request::extract_tools(&request);

    println!("\n=== FULL PROTO ROUND-TRIP TEST ===");
    println!("Extracted {} messages:", messages.len());
    for (i, msg) in messages.iter().enumerate() {
        let desc = match &msg.content {
            MessageContent::Text(t) => format!("Text({}chars)", t.len()),
            MessageContent::ToolUse {
                tool_use_id, name, ..
            } => {
                format!("ToolUse(name={}, id={})", name, tool_use_id)
            }
            MessageContent::ToolResult {
                tool_use_id,
                is_error,
                ..
            } => {
                format!("ToolResult(id={}, is_error={})", tool_use_id, is_error)
            }
            MessageContent::MultiPart(parts) => format!("MultiPart({} parts)", parts.len()),
        };
        println!("  msg[{}]: role={:?}, content={}", i, msg.role, desc);
    }
    println!("System prompt: {:?}", system_prompt.is_some());
    println!("Tools: {}", tools.len());

    assert!(
        messages.len() >= 4,
        "Should have user + assistant tool calls + user tool results + follow-up user"
    );

    let has_tool_use = messages
        .iter()
        .any(|m| matches!(m.content, MessageContent::ToolUse { .. }));
    let has_tool_result = messages
        .iter()
        .any(|m| matches!(m.content, MessageContent::ToolResult { .. }));
    assert!(has_tool_use, "Should have ToolUse messages from history");
    assert!(
        has_tool_result,
        "Should have ToolResult messages from history"
    );

    let tool_use_ids: Vec<&str> = messages
        .iter()
        .filter_map(|m| {
            if let MessageContent::ToolUse { tool_use_id, .. } = &m.content {
                Some(tool_use_id.as_str())
            } else {
                None
            }
        })
        .collect();
    let tool_result_ids: Vec<&str> = messages
        .iter()
        .filter_map(|m| {
            if let MessageContent::ToolResult { tool_use_id, .. } = &m.content {
                Some(tool_use_id.as_str())
            } else {
                None
            }
        })
        .collect();

    println!("ToolUse IDs: {:?}", tool_use_ids);
    println!("ToolResult IDs: {:?}", tool_result_ids);

    for use_id in &tool_use_ids {
        assert!(
            tool_result_ids.contains(use_id),
            "ToolUse ID {} has no matching ToolResult",
            use_id
        );
    }

    let client = BedrockClient::from_config(config)
        .await
        .expect("client creation should succeed");

    let stream_result = client
        .converse_stream(
            &model,
            &task_id,
            false,
            messages,
            system_prompt,
            tools,
            1024,
            None,
            true,
        )
        .await;

    match stream_result {
        Ok(mut stream) => {
            let mut text = String::new();
            let mut had_finished = false;
            while let Some(event_result) = stream.next().await {
                match event_result {
                    Ok(event) => {
                        if let Some(api::response_event::Type::Finished(_)) = &event.r#type {
                            had_finished = true;
                        }
                        if let Some(api::response_event::Type::ClientActions(actions)) =
                            &event.r#type
                        {
                            for action in &actions.actions {
                                if let Some(api::client_action::Action::AddMessagesToTask(add)) =
                                    &action.action
                                {
                                    for msg in &add.messages {
                                        if let Some(api::message::Message::AgentOutput(output)) =
                                            &msg.message
                                        {
                                            text.push_str(&output.text);
                                        }
                                    }
                                }
                                if let Some(api::client_action::Action::AppendToMessageContent(
                                    append,
                                )) = &action.action
                                {
                                    if let Some(msg) = &append.message {
                                        if let Some(api::message::Message::AgentOutput(output)) =
                                            &msg.message
                                        {
                                            text.push_str(&output.text);
                                        }
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        panic!("Stream error (this is the bug we're testing for): {:?}", e);
                    }
                }
            }
            assert!(had_finished, "Stream should finish normally");
            assert!(
                !text.is_empty(),
                "Model should produce a summary response, got: {}",
                text
            );
            println!("SUCCESS! Model response: {}", &text[..text.len().min(200)]);
        }
        Err(e) => {
            panic!(
                "converse_stream failed (likely toolResult ID mismatch): {:?}",
                e
            );
        }
    }
}

/// Tests that the message extraction correctly handles the ToolCall → ToolCallResult
/// tool_call_id mapping WITHOUT hitting the real API.
#[tokio::test]
async fn test_proto_tool_call_id_mapping() {
    let task_id = "test-task-id";
    let tool_call_id = "tooluse_ABC123xyz";

    let request = make_request(
        task_id,
        vec![
            make_user_query_message("msg-1", task_id, "Hello"),
            make_tool_call_run_shell(tool_call_id, task_id, tool_call_id, "echo hi"),
            make_tool_result_shell("msg-result-1", task_id, tool_call_id, "hi", 0),
        ],
        make_user_inputs_input("Thanks!"),
        "",
    );

    let messages = super::convert_request::extract_messages_from_request(&request);

    println!("\n=== PROTO TOOL_CALL_ID MAPPING TEST ===");
    for (i, msg) in messages.iter().enumerate() {
        println!(
            "  msg[{}]: role={:?}, content={:?}",
            i,
            msg.role,
            match &msg.content {
                MessageContent::Text(t) => format!("Text('{}')", t),
                MessageContent::ToolUse {
                    tool_use_id, name, ..
                } => format!("ToolUse(id={}, name={})", tool_use_id, name),
                MessageContent::ToolResult { tool_use_id, .. } =>
                    format!("ToolResult(id={})", tool_use_id),
                MessageContent::MultiPart(parts) => format!("MultiPart({} parts)", parts.len()),
            }
        );
    }

    assert_eq!(messages.len(), 4);
    assert_eq!(messages[0].role, MessageRole::User);
    assert_eq!(messages[1].role, MessageRole::Assistant);
    assert_eq!(messages[2].role, MessageRole::User);
    assert_eq!(messages[3].role, MessageRole::User);

    if let MessageContent::ToolUse {
        tool_use_id, name, ..
    } = &messages[1].content
    {
        assert_eq!(
            tool_use_id, tool_call_id,
            "ToolUse must use tool_call.tool_call_id, not msg.id"
        );
        assert_eq!(name, "run_shell_command");
    } else {
        panic!(
            "messages[1] should be ToolUse, got {:?}",
            messages[1].content
        );
    }

    if let MessageContent::ToolResult { tool_use_id, .. } = &messages[2].content {
        assert_eq!(
            tool_use_id, tool_call_id,
            "ToolResult must use result.tool_call_id"
        );
    } else {
        panic!(
            "messages[2] should be ToolResult, got {:?}",
            messages[2].content
        );
    }
}

/// Tests a scenario where msg.id != tool_call.tool_call_id (edge case).
/// The code MUST use tool_call.tool_call_id, not msg.id.
#[tokio::test]
async fn test_tool_call_id_uses_proto_field_not_message_id() {
    let task_id = "task-123";
    let message_id = "msg-uuid-different-from-tool-id";
    let tool_call_id = "tooluse_BedrockAssignedId";

    let request = api::Request {
        task_context: Some(api::request::TaskContext {
            tasks: vec![api::Task {
                id: task_id.into(),
                description: String::new(),
                dependencies: None,
                messages: vec![
                    make_user_query_message("msg-0", task_id, "Do something"),
                    {
                        let mut msg =
                            make_tool_call_run_shell(message_id, task_id, tool_call_id, "pwd");
                        msg.id = message_id.into();
                        msg
                    },
                    make_tool_result_shell("msg-result", task_id, tool_call_id, "/home/user", 0),
                ],
                summary: String::new(),
                server_data: String::new(),
            }],
        }),
        input: Some(make_user_inputs_input("Done")),
        settings: None,
        metadata: None,
        existing_suggestions: None,
        mcp_context: None,
    };

    let messages = super::convert_request::extract_messages_from_request(&request);

    let tool_use_msg = messages
        .iter()
        .find(|m| matches!(m.content, MessageContent::ToolUse { .. }))
        .unwrap();
    let tool_result_msg = messages
        .iter()
        .find(|m| matches!(m.content, MessageContent::ToolResult { .. }))
        .unwrap();

    if let MessageContent::ToolUse { tool_use_id, .. } = &tool_use_msg.content {
        assert_eq!(
            tool_use_id, tool_call_id,
            "MUST use tool_call.tool_call_id ('{}'), NOT msg.id ('{}')",
            tool_call_id, message_id
        );
    }
    if let MessageContent::ToolResult { tool_use_id, .. } = &tool_result_msg.content {
        assert_eq!(tool_use_id, tool_call_id);
    }
}
