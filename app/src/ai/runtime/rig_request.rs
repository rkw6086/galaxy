use std::collections::HashSet;
use std::sync::{Arc, Mutex};

use ai::agent::action_result::AnyFileContent;
use ai::skills::SkillReference;
use base64::engine::general_purpose;
use base64::Engine as _;
use galaxy_agent_core::{
    ContentPart, ConversationMessage, MessageContent, MessageRole, ToolDefinition, ToolResult,
    TurnRequest,
};
use warp_multi_agent_api::ToolType;

use crate::ai::agent::api::RequestParams;
use crate::ai::agent::{AIAgentContext, AIAgentInput, MCPContext, UserQueryMode};
use crate::ai::bedrock::request_translator::{default_tool_definitions, tool_name_is_supported};
use crate::ai::openai::client::OpenAIClientConfig;
use crate::ai::openai::request_translator::sanitize_messages_for_openai;

pub(crate) struct PreparedRigTurn {
    pub task_id: String,
    pub needs_create_task: bool,
    pub user_query: Option<String>,
    pub request: TurnRequest,
    pub persistent_messages: Vec<ConversationMessage>,
    pub tool_result_archive: Vec<ConversationMessage>,
    pub messages_sent: Arc<Mutex<Vec<ConversationMessage>>>,
}

pub(crate) fn prepare_rig_turn(
    config: &OpenAIClientConfig,
    params: RequestParams,
    supported_tools: Vec<ToolType>,
    supported_cli_agent_tools: Vec<ToolType>,
) -> PreparedRigTurn {
    let RequestParams {
        input,
        tool_results,
        conversation_token,
        tasks,
        model,
        root_task_id,
        message_history,
        progressive_summary,
        tool_result_archive,
        messages_sent,
        global_rules,
        mcp_context,
        ..
    } = params;

    let task_id = root_task_id
        .or_else(|| tasks.first().map(|task| task.id.clone()))
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let needs_create_task = tasks.is_empty();
    let user_query = input.iter().find_map(input_user_query);
    let mode = request_mode(&input);
    let available_tools = match mode {
        RigRequestMode::Cli => supported_cli_agent_tools,
        RigRequestMode::Normal | RigRequestMode::Plan | RigRequestMode::Orchestrate => {
            supported_tools
        }
    };
    let tools = tool_definitions(&available_tools, mcp_context.as_ref());
    let system_prompt = build_system_prompt(&input, &tools, &global_rules, mode);

    let mut new_messages = input_messages(input, tool_results);
    let mut persistent_messages = message_history;
    persistent_messages.append(&mut new_messages);
    for message in &mut persistent_messages {
        message.truncate_tool_results_for_provider_request();
    }
    sanitize_messages_for_openai(&mut persistent_messages);

    let mut turn_messages = Vec::new();
    if let Some(summary) = progressive_summary {
        turn_messages.push(ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::Text(format!(
                "<conversation-history-summary>\n{summary}\n</conversation-history-summary>\n\n\
                 The above summarizes earlier conversation history. The detailed messages below are the most recent exchanges."
            )),
        });
        turn_messages.push(ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::Text(
                "Understood, I have the prior context. Continuing with the recent conversation."
                    .to_string(),
            ),
        });
    }
    turn_messages.extend(persistent_messages.clone());

    let model_id = config
        .model
        .clone()
        .filter(|model| !model.is_empty() && model != "auto")
        .unwrap_or_else(|| model.as_str().to_string());
    let mut request = TurnRequest::new(model_id, turn_messages);
    request.conversation_id = conversation_token.map(|token| token.as_str().to_string());
    request.system_prompt = Some(system_prompt);
    request.tools = tools;
    request.max_output_tokens = config.max_output_tokens.map(u64::from);

    PreparedRigTurn {
        task_id,
        needs_create_task,
        user_query,
        request,
        persistent_messages,
        tool_result_archive,
        messages_sent,
    }
}

fn input_messages(
    inputs: Vec<AIAgentInput>,
    tool_results: Vec<ToolResult>,
) -> Vec<ConversationMessage> {
    let mut messages = Vec::new();
    if !tool_results.is_empty() {
        let mut parts = tool_results
            .into_iter()
            .map(|result| {
                let is_error = result.is_error();
                ContentPart::ToolResult {
                    tool_use_id: result.call_id,
                    content: result.content,
                    is_error,
                }
            })
            .collect::<Vec<_>>();
        let content = if parts.len() == 1 {
            let ContentPart::ToolResult {
                tool_use_id,
                content,
                is_error,
            } = parts.pop().expect("one tool result exists")
            else {
                unreachable!()
            };
            MessageContent::ToolResult {
                tool_use_id,
                content,
                is_error,
            }
        } else {
            MessageContent::MultiPart(parts)
        };
        messages.push(ConversationMessage {
            role: MessageRole::User,
            content,
        });
    }

    messages.extend(inputs.into_iter().filter_map(input_message));
    messages
}

fn input_message(input: AIAgentInput) -> Option<ConversationMessage> {
    let (text, images) = match input {
        AIAgentInput::UserQuery {
            query,
            context,
            running_command,
            ..
        } => {
            let text = if let Some(command) = running_command {
                format!(
                    "[Running command: {}]\n[Command ID: {}]\n[Terminal output:\n{}\n]\n{}",
                    command.command, command.block_id, command.grid_contents, query
                )
            } else {
                query
            };
            (text, image_parts(&context))
        }
        AIAgentInput::ActionResult { .. } => return None,
        AIAgentInput::AutoCodeDiffQuery { query, .. } => (query, Vec::new()),
        AIAgentInput::ResumeConversation { .. } => (
            "Continue where we left off. Review the conversation history and proceed with the next steps."
                .to_string(),
            Vec::new(),
        ),
        AIAgentInput::InitProjectRules { .. } => (
            "Initialize this project. Analyze the codebase structure and files, generate an AGENTS.md file documenting project conventions and setup instructions, and offer to create a development environment configuration. Use the available tools to inspect the project before responding."
                .to_string(),
            Vec::new(),
        ),
        AIAgentInput::CreateEnvironment { repo_paths, .. } => (
            format!(
                "Create a development environment for this project. Set up necessary dependencies, configuration files, and tooling. Repositories: {}",
                repo_paths.join(", ")
            ),
            Vec::new(),
        ),
        AIAgentInput::TriggerPassiveSuggestion { .. } => (
            "Suggest a useful next action based on the current project context.".to_string(),
            Vec::new(),
        ),
        AIAgentInput::CreateNewProject { query, .. } => {
            (format!("Create a new project: {query}"), Vec::new())
        }
        AIAgentInput::CloneRepository {
            clone_repo_url, ..
        } => (
            format!(
                "Clone the repository at {} and set it up for development.",
                clone_repo_url.into_url()
            ),
            Vec::new(),
        ),
        AIAgentInput::CodeReview { .. } => (
            "Review the provided code changes and address the review comments.".to_string(),
            Vec::new(),
        ),
        AIAgentInput::FetchReviewComments { repo_path, .. } => (
            format!("Fetch and review the pull-request comments for {repo_path}."),
            Vec::new(),
        ),
        AIAgentInput::SummarizeConversation { prompt, .. } => (
            prompt.unwrap_or_else(|| {
                "Summarize this conversation, preserving decisions, changes, and context needed to continue."
                    .to_string()
            }),
            Vec::new(),
        ),
        AIAgentInput::InvokeSkill {
            skill, user_query, ..
        } => {
            let suffix = user_query
                .map(|query| query.query)
                .filter(|query| !query.is_empty())
                .map(|query| format!("\n\nAdditional context from user: {query}"))
                .unwrap_or_default();
            (
                format!(
                    "Execute the following skill: {}\n\n<skill-instructions>\n{}\n</skill-instructions>{suffix}",
                    skill.name, skill.content
                ),
                Vec::new(),
            )
        }
        AIAgentInput::StartFromAmbientRunPrompt { ambient_run_id, .. } => (
            format!("Continue the configured ambient-agent run {ambient_run_id}."),
            Vec::new(),
        ),
        AIAgentInput::MessagesReceivedFromAgents { messages } => (
            messages
                .into_iter()
                .map(|message| {
                    format!(
                        "Message from {} ({})\nSubject: {}\n{}",
                        message.sender_agent_id,
                        message.addresses.join(", "),
                        message.subject,
                        message.message_body
                    )
                })
                .collect::<Vec<_>>()
                .join("\n\n"),
            Vec::new(),
        ),
        AIAgentInput::EventsFromAgents { events } => (
            format!("Agent lifecycle events:\n{events:#?}"),
            Vec::new(),
        ),
        AIAgentInput::PassiveSuggestionResult { suggestion, .. } => (
            format!("The user responded to a passive suggestion: {suggestion:?}"),
            Vec::new(),
        ),
        AIAgentInput::OrchestrationConfigUpdate {
            plan_id,
            config,
            status,
        } => (
            format!(
                "Orchestration configuration updated for plan {plan_id}: status={status:?}, config={config:?}"
            ),
            Vec::new(),
        ),
    };

    let content = if images.is_empty() {
        MessageContent::Text(text)
    } else {
        let mut parts = Vec::with_capacity(images.len() + 1);
        parts.push(ContentPart::Text(text));
        parts.extend(images);
        MessageContent::MultiPart(parts)
    };
    Some(ConversationMessage {
        role: MessageRole::User,
        content,
    })
}

fn image_parts(context: &[AIAgentContext]) -> Vec<ContentPart> {
    context
        .iter()
        .filter_map(|context| {
            let AIAgentContext::Image(image) = context else {
                return None;
            };
            let data = match general_purpose::STANDARD.decode(&image.data) {
                Ok(data) => data,
                Err(error) => {
                    log::warn!("Skipping invalid base64 image supplied to Rig: {error}");
                    return None;
                }
            };
            Some(ContentPart::Image {
                data,
                mime_type: image.mime_type.clone(),
            })
        })
        .collect()
}

fn input_user_query(input: &AIAgentInput) -> Option<String> {
    match input {
        AIAgentInput::UserQuery { query, .. } => Some(query.clone()),
        AIAgentInput::InvokeSkill { skill, .. } => Some(format!("/{}", skill.name)),
        AIAgentInput::AutoCodeDiffQuery { .. }
        | AIAgentInput::ResumeConversation { .. }
        | AIAgentInput::InitProjectRules { .. }
        | AIAgentInput::CreateEnvironment { .. }
        | AIAgentInput::TriggerPassiveSuggestion { .. }
        | AIAgentInput::CreateNewProject { .. }
        | AIAgentInput::CloneRepository { .. }
        | AIAgentInput::CodeReview { .. }
        | AIAgentInput::FetchReviewComments { .. }
        | AIAgentInput::SummarizeConversation { .. }
        | AIAgentInput::StartFromAmbientRunPrompt { .. }
        | AIAgentInput::ActionResult { .. }
        | AIAgentInput::MessagesReceivedFromAgents { .. }
        | AIAgentInput::EventsFromAgents { .. }
        | AIAgentInput::PassiveSuggestionResult { .. }
        | AIAgentInput::OrchestrationConfigUpdate { .. } => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RigRequestMode {
    Normal,
    Plan,
    Orchestrate,
    Cli,
}

fn request_mode(inputs: &[AIAgentInput]) -> RigRequestMode {
    for input in inputs {
        if matches!(
            input,
            AIAgentInput::UserQuery {
                running_command: Some(_),
                ..
            }
        ) {
            return RigRequestMode::Cli;
        }
        if let AIAgentInput::UserQuery {
            user_query_mode, ..
        } = input
        {
            match user_query_mode {
                UserQueryMode::Normal => {}
                UserQueryMode::Plan => return RigRequestMode::Plan,
                UserQueryMode::Orchestrate => return RigRequestMode::Orchestrate,
            }
        }
    }
    RigRequestMode::Normal
}

fn tool_definitions(
    supported_tools: &[ToolType],
    mcp_context: Option<&MCPContext>,
) -> Vec<ToolDefinition> {
    let supported = supported_tools.iter().copied().collect::<HashSet<_>>();
    let mut tools = default_tool_definitions()
        .into_iter()
        .filter(|tool| tool_name_is_supported(&tool.name, &supported))
        .collect::<Vec<_>>();

    if !supported.contains(&ToolType::CallMcpTool) {
        return tools;
    }
    let Some(mcp_context) = mcp_context else {
        return tools;
    };
    let mut seen = tools
        .iter()
        .map(|tool| tool.name.clone())
        .collect::<HashSet<_>>();
    for server in &mcp_context.servers {
        for tool in &server.tools {
            let name = format!("mcp__{}__{}", server.id, tool.name);
            if seen.insert(name.clone()) {
                tools.push(ToolDefinition {
                    name,
                    description: tool
                        .description
                        .as_deref()
                        .map(str::to_string)
                        .unwrap_or_else(|| format!("MCP tool from {} server", server.name)),
                    input_schema: serde_json::Value::Object(tool.input_schema.as_ref().clone()),
                });
            }
        }
    }
    #[allow(deprecated)]
    for tool in &mcp_context.tools {
        let name = format!("mcp__{}", tool.name);
        if seen.insert(name.clone()) {
            tools.push(ToolDefinition {
                name,
                description: tool
                    .description
                    .as_deref()
                    .map(str::to_string)
                    .unwrap_or_else(|| "MCP tool".to_string()),
                input_schema: serde_json::Value::Object(tool.input_schema.as_ref().clone()),
            });
        }
    }
    tools
}

fn build_system_prompt(
    inputs: &[AIAgentInput],
    tools: &[ToolDefinition],
    global_rules: &[(String, String)],
    mode: RigRequestMode,
) -> String {
    let mut prompt = String::from(
        "You are Galaxy, a local-first software-engineering and terminal agent. Complete the user's task through inspection, implementation, and proportionate validation. Galaxy owns tool permissions and execution; use only the tools advertised in this request and treat every result as authoritative evidence.\n\n",
    );
    let contexts = inputs.iter().filter_map(AIAgentInput::context).flatten();
    let mut environment = Vec::new();
    let mut project_rules = Vec::new();
    let mut available_skills = Vec::new();
    let mut attached_context = Vec::new();
    for context in contexts {
        match context {
            AIAgentContext::Directory {
                pwd,
                home_dir,
                are_file_symbols_indexed,
            } => {
                if let Some(pwd) = pwd {
                    environment.push(format!("Working directory: {pwd}"));
                }
                if let Some(home_dir) = home_dir {
                    environment.push(format!("Home directory: {home_dir}"));
                }
                environment.push(format!(
                    "Working-directory file symbols indexed: {are_file_symbols_indexed}"
                ));
            }
            AIAgentContext::ExecutionEnvironment(execution) => {
                let shell_version = execution
                    .shell_version
                    .as_deref()
                    .map(|version| format!(" {version}"))
                    .unwrap_or_default();
                environment.push(format!("Shell: {}{shell_version}", execution.shell_name));
                if let Some(os) = &execution.os.category {
                    environment.push(format!("OS: {os}"));
                }
                if let Some(distribution) = &execution.os.distribution {
                    environment.push(format!("OS distribution: {distribution}"));
                }
            }
            AIAgentContext::ProjectRules {
                root_path,
                active_rules,
                additional_rule_paths,
            } => {
                for rule in active_rules {
                    if let AnyFileContent::StringContent(content) = &rule.content {
                        project_rules.push((root_path.clone(), content.clone()));
                    }
                }
                if !additional_rule_paths.is_empty() {
                    environment.push(format!(
                        "Additional project rule paths: {}",
                        additional_rule_paths.join(", ")
                    ));
                }
            }
            AIAgentContext::Git { head, branch } => {
                environment.push(format!("Git HEAD: {head}"));
                if let Some(branch) = branch {
                    environment.push(format!("Git branch: {branch}"));
                }
            }
            AIAgentContext::Skills { skills } => {
                for skill in skills {
                    let (reference_type, reference) = match &skill.reference {
                        SkillReference::Path(path) => ("path", path.display_path()),
                        SkillReference::BundledSkillId(id) => ("bundled", id.clone()),
                    };
                    available_skills.push(format!(
                        "- name={:?}; reference_type={reference_type:?}; skill={reference:?}; description={:?}",
                        skill.name, skill.description
                    ));
                }
            }
            AIAgentContext::SelectedText(text) => {
                attached_context.push(("Selected text".to_string(), text.clone()));
            }
            AIAgentContext::CurrentTime { current_time } => {
                environment.push(format!("Current time: {current_time}"));
            }
            AIAgentContext::Codebase { path, name } => {
                environment.push(format!("Indexed codebase: {name} ({path})"));
            }
            AIAgentContext::File(file) => match &file.content {
                AnyFileContent::StringContent(content) => {
                    attached_context.push((format!("Attached file: {file}"), content.clone()));
                }
                AnyFileContent::BinaryContent(_) => {
                    environment.push(format!("Attached binary file (content omitted): {file}"));
                }
            },
            AIAgentContext::Repository { name, owner } => {
                let owner = owner
                    .as_deref()
                    .map(|owner| format!("{owner}/"))
                    .unwrap_or_default();
                environment.push(format!("Repository: {owner}{name}"));
            }
            AIAgentContext::PullRequest {
                number,
                state,
                draft,
                base_branch,
            } => {
                environment.push(format!(
                    "Pull request: #{number}; state={state}; draft={draft}; base={base_branch}"
                ));
            }
            AIAgentContext::Block(block) => {
                let details = format!(
                    "Command: {}\nExit code: {}\nOutput:\n{}",
                    block.command, block.exit_code, block.output
                );
                attached_context.push((format!("Terminal block {}", block.id), details));
            }
            AIAgentContext::Image(_) => {}
        }
    }
    if !environment.is_empty() {
        prompt.push_str("## Environment\n");
        for item in environment {
            prompt.push_str("- ");
            prompt.push_str(&item);
            prompt.push('\n');
        }
        prompt.push('\n');
    }
    if !project_rules.is_empty() {
        prompt.push_str("## Project Rules\n");
        for (root, content) in project_rules {
            prompt.push_str(&format!("### Rules from {root}\n{content}\n"));
        }
        prompt.push('\n');
    }
    if !attached_context.is_empty() {
        prompt.push_str("## Attached Context\n");
        for (label, content) in attached_context {
            prompt.push_str(&format!(
                "<context label={label:?}>\n{content}\n</context>\n"
            ));
        }
        prompt.push('\n');
    }
    if !available_skills.is_empty() && tools.iter().any(|tool| tool.name == "read_skill") {
        prompt.push_str("## Available Skills\n");
        prompt.push_str(&available_skills.join("\n"));
        prompt.push_str("\n\n");
    }
    if !global_rules.is_empty() {
        prompt.push_str("## Global Rules\n");
        for (name, content) in global_rules {
            if !name.is_empty() {
                prompt.push_str(&format!("### {name}\n"));
            }
            prompt.push_str(content);
            prompt.push_str("\n\n");
        }
    }
    match mode {
        RigRequestMode::Normal => {}
        RigRequestMode::Plan => prompt.push_str(
            "## Plan Mode\nInspect and produce an implementation-ready plan. Do not edit files or perform state-changing actions.\n\n",
        ),
        RigRequestMode::Orchestrate => prompt.push_str(
            "## Orchestration Mode\nDelegate only independent, bounded work where parallelism materially helps, then synthesize the results.\n\n",
        ),
        RigRequestMode::Cli => prompt.push_str(
            "## Running Command Monitor\nMonitor the existing command by its command ID. Never start a duplicate command. Poll briefly, respect stop conditions, and report only verified outcomes.\n\n",
        ),
    }
    prompt.push_str("## Available Tools\n");
    if tools.is_empty() {
        prompt.push_str("No tools are available. Do not invent tool calls.\n");
    } else {
        prompt.push_str("Use only these tools: ");
        prompt.push_str(
            &tools
                .iter()
                .map(|tool| tool.name.as_str())
                .collect::<Vec<_>>()
                .join(", "),
        );
        prompt.push_str(".\nNever invent tool names or parameters. Check command exit codes and tool error results before claiming success.\n");
    }
    prompt
}

#[cfg(test)]
#[path = "rig_request_tests.rs"]
mod tests;
