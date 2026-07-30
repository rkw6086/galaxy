use galaxy_acp::{ContentBlock, ImageContent, TextContent};

use crate::ai::agent::api::RequestParams;
use crate::ai::agent::{AIAgentAttachment, AIAgentContext, AIAgentInput, MarkdownActionResult};

const CONTEXT_HEADER: &str = "\n\n<galaxy_context hidden_from_transcript=\"true\">\n";
const CONTEXT_FOOTER: &str = "\n</galaxy_context>";
const SYSTEM_REQUEST_PLACEHOLDER: &str =
    "Handle the Galaxy system request in the hidden context below.";
const MAX_RUNNING_COMMAND_OUTPUT_CHARS: usize = 32_000;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct GalaxyTerminalTools {
    pub(super) status: bool,
    pub(super) interrupt: bool,
}

/// Converts Galaxy's rich request input into ACP prompt content.
///
/// The first text block contains the user's visible request. Context and global
/// rules are appended in a clearly delimited block that is sent to the agent but
/// is not copied into Galaxy's visible user bubble. Images stay as native ACP
/// image blocks so adapters can forward them to multimodal models.
pub(super) fn prompt_content(
    params: &RequestParams,
    terminal_tools: GalaxyTerminalTools,
) -> Result<Vec<ContentBlock>, String> {
    let visible_query = params
        .input
        .iter()
        .rev()
        .find_map(AIAgentInput::display_query);

    let mut hidden_context = Vec::new();
    let mut images = Vec::new();

    for input in &params.input {
        append_hidden_input(input, terminal_tools, &mut hidden_context)?;

        if let Some(context) = input.context() {
            for item in context {
                match item {
                    AIAgentContext::Image(image) => {
                        let mut file_name = image.file_name.clone();
                        params.redact_text_for_model(&mut file_name);
                        images.push(ContentBlock::Image(
                            ImageContent::new(image.data.clone(), image.mime_type.clone())
                                .uri(format!("attachment://{file_name}")),
                        ));
                    }
                    AIAgentContext::SelectedText(text) => {
                        hidden_context.push(format!("Selected text:\n{text}"));
                    }
                    AIAgentContext::File(file) => {
                        hidden_context.push(format!("File context:\n{}", serialize_context(file)?));
                    }
                    AIAgentContext::Directory { pwd, .. } => {
                        if let Some(pwd) = pwd {
                            hidden_context.push(format!("Working directory: {pwd}"));
                        }
                    }
                    AIAgentContext::ExecutionEnvironment(environment) => {
                        hidden_context.push(format!(
                            "Execution environment:\n{}",
                            serialize_context(environment)?
                        ))
                    }
                    AIAgentContext::CurrentTime { current_time } => {
                        hidden_context.push(format!("Current time: {current_time}"));
                    }
                    AIAgentContext::Codebase { path, name } => {
                        hidden_context.push(format!("Codebase: {name} ({path})"));
                    }
                    AIAgentContext::ProjectRules { .. }
                    | AIAgentContext::Git { .. }
                    | AIAgentContext::Repository { .. }
                    | AIAgentContext::PullRequest { .. }
                    | AIAgentContext::Skills { .. }
                    | AIAgentContext::Block(_) => {
                        hidden_context.push(format!(
                            "Additional Galaxy context:\n{}",
                            serialize_context(item)?
                        ));
                    }
                }
            }
        }

        if let AIAgentInput::UserQuery {
            referenced_attachments,
            ..
        } = input
        {
            for (name, attachment) in referenced_attachments {
                hidden_context.push(attachment_text(name, attachment)?);
            }
        }
    }

    if !params.global_rules.is_empty() {
        let rules = params
            .global_rules
            .iter()
            .map(|(name, content)| format!("Rule: {name}\n{content}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        hidden_context.push(format!("Galaxy rules:\n{rules}"));
    }

    let mut text = visible_query.unwrap_or_else(|| SYSTEM_REQUEST_PLACEHOLDER.to_owned());
    if !hidden_context.is_empty() {
        text.push_str(CONTEXT_HEADER);
        text.push_str(&hidden_context.join("\n\n"));
        text.push_str(CONTEXT_FOOTER);
    }
    params.redact_text_for_model(&mut text);

    let mut prompt = vec![ContentBlock::Text(TextContent::new(text))];
    prompt.extend(images);
    Ok(prompt)
}

fn append_hidden_input(
    input: &AIAgentInput,
    terminal_tools: GalaxyTerminalTools,
    hidden_context: &mut Vec<String>,
) -> Result<(), String> {
    match input {
        AIAgentInput::UserQuery {
            running_command: Some(command),
            ..
        } => {
            hidden_context.push(format!(
                "A command is running in the user's visible Galaxy terminal.\n\
                 Command: {}\n\
                 Galaxy block_id: {}\n\
                 Alternate screen: {}\n\
                 Current output:\n{}\n\n{}",
                command.command,
                command.block_id,
                command.is_alt_screen_active,
                tail_chars(&command.grid_contents, MAX_RUNNING_COMMAND_OUTPUT_CHARS),
                running_command_tool_guidance(terminal_tools),
            ));
        }
        AIAgentInput::UserQuery { .. } | AIAgentInput::CreateNewProject { .. } => {}
        AIAgentInput::AutoCodeDiffQuery { query, .. } => {
            hidden_context.push(format!(
                "Galaxy system request: create a code diff.\n{query}"
            ));
        }
        AIAgentInput::ResumeConversation { .. } => {
            hidden_context.push(
                "Galaxy system request: resume the current conversation and continue the task."
                    .to_owned(),
            );
        }
        AIAgentInput::InitProjectRules { .. } => {
            hidden_context.push(
                "Galaxy system request: initialize appropriate project rules for this workspace."
                    .to_owned(),
            );
        }
        AIAgentInput::CreateEnvironment { repo_paths, .. } => {
            hidden_context.push(format!(
                "Galaxy system request: create a development environment for these repositories:\n{}",
                repo_paths.join("\n")
            ));
        }
        AIAgentInput::TriggerPassiveSuggestion {
            attachments,
            trigger,
            ..
        } => {
            hidden_context.push(format!(
                "Galaxy background request: generate a concise useful suggestion for this event:\n{trigger:#?}"
            ));
            for (index, attachment) in attachments.iter().enumerate() {
                hidden_context.push(attachment_text(
                    &format!("background attachment {}", index + 1),
                    attachment,
                )?);
            }
        }
        AIAgentInput::CloneRepository { .. } => {
            // The display query already contains the requested repository URL.
        }
        AIAgentInput::CodeReview {
            review_comments, ..
        } => {
            hidden_context.push(format!(
                "Galaxy system request: address this code-review batch:\n{review_comments:#?}"
            ));
        }
        AIAgentInput::FetchReviewComments { repo_path, .. } => {
            hidden_context.push(format!(
                "Galaxy system request: fetch and address review comments for {repo_path}."
            ));
        }
        AIAgentInput::SummarizeConversation { prompt, .. } => {
            hidden_context.push(format!(
                "Galaxy system request: summarize the conversation for future continuation.{}",
                prompt
                    .as_deref()
                    .map(|prompt| format!("\nAdditional instructions: {prompt}"))
                    .unwrap_or_default()
            ));
        }
        AIAgentInput::InvokeSkill {
            skill, user_query, ..
        } => {
            hidden_context.push(format!(
                "Galaxy skill instructions for {}:\n{}",
                skill.name, skill.content
            ));
            if let Some(user_query) = user_query {
                for (name, attachment) in &user_query.referenced_attachments {
                    hidden_context.push(attachment_text(name, attachment)?);
                }
            }
        }
        AIAgentInput::StartFromAmbientRunPrompt {
            ambient_run_id,
            runtime_skill,
            attachments_dir,
            ..
        } => {
            hidden_context.push(format!(
                "Galaxy system request: continue ambient run {ambient_run_id}.{}{}",
                runtime_skill
                    .as_ref()
                    .map(|skill| format!("\nRuntime skill:\n{}", skill.content))
                    .unwrap_or_default(),
                attachments_dir
                    .as_deref()
                    .map(|path| format!("\nDownloaded attachment directory: {path}"))
                    .unwrap_or_default(),
            ));
        }
        AIAgentInput::ActionResult { result, .. } => {
            hidden_context.push(format!(
                "Galaxy tool result for {}:\n{}",
                result.id,
                MarkdownActionResult(&result.result)
            ));
        }
        AIAgentInput::MessagesReceivedFromAgents { messages } => {
            hidden_context.push(format!(
                "Messages received from other Galaxy agents:\n{messages:#?}"
            ));
        }
        AIAgentInput::EventsFromAgents { events } => {
            hidden_context.push(format!(
                "Events received from other Galaxy agents:\n{events:#?}"
            ));
        }
        AIAgentInput::PassiveSuggestionResult {
            trigger,
            suggestion,
            ..
        } => {
            hidden_context.push(format!(
                "Galaxy passive-suggestion feedback.\nTrigger: {trigger:#?}\nResult: {suggestion:#?}"
            ));
        }
        AIAgentInput::OrchestrationConfigUpdate {
            plan_id,
            config,
            status,
        } => {
            hidden_context.push(format!(
                "Galaxy orchestration configuration changed for plan {plan_id}.\n\
                 Status: {status:#?}\nConfiguration: {config:#?}"
            ));
        }
    }
    Ok(())
}

fn running_command_tool_guidance(terminal_tools: GalaxyTerminalTools) -> &'static str {
    match (terminal_tools.status, terminal_tools.interrupt) {
        (true, true) => {
            "Use `galaxy_terminal_status` to inspect this exact pane. For a deadline, call \
             `galaxy_terminal_interrupt_at` once with this exact block_id and the target \
             `running_for_ms`; Galaxy performs the wait outside the model loop and refuses to \
             interrupt a replacement block. Use `galaxy_terminal_interrupt` only when an \
             immediate stop is requested."
        }
        (true, false) => {
            "The read-only `galaxy_terminal_status` tool can inspect this exact pane, but no \
             Galaxy terminal mutation tool is available under the active execution profile. Do \
             not claim that you can stop this command or enforce a deadline."
        }
        (false, false) => {
            "Galaxy terminal control tools are unavailable for this turn. Do not claim that you \
             can monitor, stop, or enforce a deadline on this existing command."
        }
        (false, true) => {
            "Galaxy exposed an inconsistent terminal tool configuration. Do not attempt to \
             monitor or interrupt this existing command."
        }
    }
}

fn tail_chars(text: &str, max_chars: usize) -> &str {
    let Some((start, _)) = text.char_indices().rev().nth(max_chars.saturating_sub(1)) else {
        return text;
    };
    &text[start..]
}

fn attachment_text(name: &str, attachment: &AIAgentAttachment) -> Result<String, String> {
    let content = match attachment {
        AIAgentAttachment::PlainText(text) => text.clone(),
        AIAgentAttachment::DocumentContent { content, .. } => content.clone(),
        AIAgentAttachment::DiffHunk { diff_content, .. } => diff_content.clone(),
        AIAgentAttachment::FilePathReference { file_path, .. } => {
            format!("Local file reference: {file_path}")
        }
        AIAgentAttachment::DriveObject { .. }
        | AIAgentAttachment::DiffSet { .. }
        | AIAgentAttachment::Block(_) => serialize_context(attachment)?,
    };
    Ok(format!("Attachment {name}:\n{content}"))
}

fn serialize_context(value: &impl serde::Serialize) -> Result<String, String> {
    serde_json::to_string_pretty(value)
        .map_err(|error| format!("failed to serialize ACP prompt context: {error}"))
}

#[cfg(test)]
#[path = "prompt_tests.rs"]
mod tests;
