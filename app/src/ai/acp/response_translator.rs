use std::collections::HashMap;

use galaxy_acp::{AcpEvent, ContentBlock, StopReason, ToolCallId, ToolCallStatus};
use uuid::Uuid;
use warp_multi_agent_api::response_event::stream_finished;
use warp_multi_agent_api::{self as api, ResponseEvent};

use crate::ai::bedrock::response_translator::{
    build_add_agent_output_message, build_append_text, build_create_task, build_stream_init,
    build_user_query_message,
};

/// Stateful translation from ACP session updates to Galaxy's existing agent UI
/// response protocol.
pub(super) struct AcpResponseTranslator {
    task_id: String,
    request_id: String,
    needs_create_task: bool,
    user_query: Option<String>,
    model_id: String,
    initialized: bool,
    message_id: Option<String>,
    tool_titles: HashMap<ToolCallId, String>,
    used_tokens: u64,
    context_size: u64,
    accept_next_user_content: bool,
}

impl AcpResponseTranslator {
    pub(super) fn new(
        task_id: String,
        needs_create_task: bool,
        user_query: Option<String>,
        model_id: String,
    ) -> Self {
        Self {
            task_id,
            request_id: Uuid::new_v4().to_string(),
            needs_create_task,
            user_query,
            model_id,
            initialized: false,
            message_id: None,
            tool_titles: HashMap::new(),
            used_tokens: 0,
            context_size: 0,
            accept_next_user_content: false,
        }
    }

    pub(super) fn translate(&mut self, event: AcpEvent) -> Result<Vec<ResponseEvent>, String> {
        let mut events = Vec::new();
        match event {
            AcpEvent::SessionStarted { .. } => self.initialize(&mut events),
            AcpEvent::AgentText { text } => {
                self.initialize(&mut events);
                self.add_or_append(&text, &mut events);
            }
            // Reasoning is deliberately not copied into the plain assistant
            // transcript. ACP agents can still expose plans and tool progress.
            AcpEvent::AgentThought { .. } => {}
            AcpEvent::AgentContent { content, thought } => {
                if !thought {
                    self.initialize(&mut events);
                    let description = match content {
                        ContentBlock::Text(text) => text.text,
                        ContentBlock::Image(_) => "[Agent returned an image.]".to_owned(),
                        ContentBlock::Audio(_) => "[Agent returned audio.]".to_owned(),
                        ContentBlock::ResourceLink(resource) => {
                            format!("[Agent referenced {}.]", resource.name)
                        }
                        ContentBlock::Resource(_) => {
                            "[Agent returned embedded resource content.]".to_owned()
                        }
                        _ => "[Agent returned unsupported content.]".to_owned(),
                    };
                    self.add_or_append(&description, &mut events);
                }
            }
            AcpEvent::UserContent { content } => {
                // Some ACP adapters replay user-message chunks while loading a
                // session or echo Galaxy's initial prompt, which also contains
                // hidden context. Only content explicitly authorized by the
                // live-steering path may enter the visible transcript.
                if self.accept_next_user_content {
                    self.accept_next_user_content = false;
                    self.initialize(&mut events);
                    if let ContentBlock::Text(text) = content {
                        events.push(build_user_query_message(&self.task_id, &text.text));
                        // Assistant output after steering belongs in a new chat
                        // bubble, not the message that preceded the follow-up.
                        self.message_id = None;
                    }
                }
            }
            AcpEvent::ToolCall {
                id,
                title,
                status,
                output,
            } => {
                self.initialize(&mut events);
                self.tool_titles.insert(id, title.clone());
                self.add_or_append(&tool_status_line(&title, status), &mut events);
                if let Some(output) = output {
                    self.add_or_append(&tool_output_block(&output), &mut events);
                }
            }
            AcpEvent::ToolCallUpdate {
                id,
                title,
                status,
                output,
            } => {
                self.initialize(&mut events);
                let title = title
                    .or_else(|| self.tool_titles.get(&id).cloned())
                    .unwrap_or_else(|| "tool".to_owned());
                self.tool_titles.insert(id, title.clone());
                if let Some(status) = status {
                    self.add_or_append(&tool_status_line(&title, status), &mut events);
                }
                if let Some(output) = output {
                    self.add_or_append(&tool_output_block(&output), &mut events);
                }
            }
            AcpEvent::Usage { used, size, .. } => {
                self.used_tokens = used;
                self.context_size = size;
            }
            AcpEvent::PermissionRequested { request } => {
                self.initialize(&mut events);
                self.add_or_append(
                    &format!(
                        "\n\n> Permission requested for: {}\n",
                        request.tool_call.fields.title.as_deref().unwrap_or("tool")
                    ),
                    &mut events,
                );
            }
            AcpEvent::PermissionResolved { decision, .. } => {
                self.initialize(&mut events);
                self.add_or_append(
                    &format!("\n\n> Permission decision: {decision:?}\n"),
                    &mut events,
                );
            }
            AcpEvent::Finished { stop_reason } => {
                self.initialize(&mut events);
                if self.message_id.is_none() && stop_reason != StopReason::Cancelled {
                    self.add_or_append(
                        "> ACP agent completed without a text response.",
                        &mut events,
                    );
                }
                events.push(self.finished(stop_reason));
            }
            AcpEvent::Error { message } => return Err(message),
            // ACP events are forward-compatible. Unknown events do not belong
            // in the user-visible transcript until Galaxy knows their meaning.
            _ => {}
        }
        Ok(events)
    }

    pub(super) fn translate_steered_user_content(
        &mut self,
        content: ContentBlock,
    ) -> Result<Vec<ResponseEvent>, String> {
        self.accept_next_user_content = true;
        self.translate(AcpEvent::UserContent { content })
    }

    pub(super) fn steering_failed(&mut self, error: &str) -> Vec<ResponseEvent> {
        let mut events = Vec::new();
        self.initialize(&mut events);
        self.message_id = None;
        self.add_or_append(
            &format!(
                "Galaxy couldn't confirm that live steering message: {error}. \
                 The agent may not have received it; check the current terminal and file state \
                 before retrying."
            ),
            &mut events,
        );
        // Any output still arriving from the original turn should not be
        // appended to Galaxy's steering-failure notice.
        self.message_id = None;
        events
    }

    pub(super) fn steering_started_new_turn(&mut self) -> Vec<ResponseEvent> {
        let mut events = Vec::new();
        self.initialize(&mut events);
        self.message_id = None;
        self.add_or_append(
            "The ACP adapter started that steering message as a separate turn instead of \
             injecting it into the active one. Galaxy terminated the adapter process immediately, \
             but the turn may have begun acting; check the current terminal and file state before \
             retrying.",
            &mut events,
        );
        self.message_id = None;
        events
    }

    pub(super) fn startup_error(&mut self, error: &str) -> Vec<ResponseEvent> {
        let mut events = Vec::new();
        self.initialize(&mut events);
        self.message_id = None;
        self.add_or_append(
            &format!("Galaxy couldn't start the ACP agent: {error}"),
            &mut events,
        );
        events.push(self.finished(StopReason::Refusal));
        events
    }

    fn initialize(&mut self, events: &mut Vec<ResponseEvent>) {
        if self.initialized {
            return;
        }
        // The ACP session ID is persisted separately. An empty conversation ID
        // keeps this synthetic Init event out of Galaxy cloud token paths.
        events.push(build_stream_init(&self.request_id, ""));
        if self.needs_create_task {
            events.push(build_create_task(&self.task_id));
        }
        if let Some(user_query) = &self.user_query {
            events.push(build_user_query_message(&self.task_id, user_query));
        }
        self.initialized = true;
    }

    fn add_or_append(&mut self, text: &str, events: &mut Vec<ResponseEvent>) {
        if text.is_empty() {
            return;
        }
        if let Some(message_id) = &self.message_id {
            events.push(build_append_text(&self.task_id, message_id, text));
        } else {
            let message_id = Uuid::new_v4().to_string();
            events.push(build_add_agent_output_message(
                &self.task_id,
                &message_id,
                text,
            ));
            self.message_id = Some(message_id);
        }
    }

    fn finished(&self, stop_reason: StopReason) -> ResponseEvent {
        let reason = match stop_reason {
            StopReason::EndTurn | StopReason::Cancelled => {
                stream_finished::Reason::Done(stream_finished::Done {})
            }
            StopReason::MaxTokens | StopReason::MaxTurnRequests => {
                stream_finished::Reason::MaxTokenLimit(stream_finished::ReachedMaxTokenLimit {})
            }
            StopReason::Refusal => stream_finished::Reason::Other(stream_finished::Other {}),
            // ACP marks this enum non-exhaustive so newer agents can add stop reasons
            // without breaking older clients.
            _ => stream_finished::Reason::Other(stream_finished::Other {}),
        };
        let used_tokens = u32::try_from(self.used_tokens).unwrap_or(u32::MAX);
        let context_usage = if self.context_size == 0 {
            0.0
        } else {
            (self.used_tokens as f32 / self.context_size as f32).clamp(0.0, 1.0)
        };
        #[allow(deprecated)]
        let usage_metadata = stream_finished::ConversationUsageMetadata {
            context_window_usage: context_usage,
            summarized: false,
            credits_spent: 0.0,
            platform_credits_spent: 0.0,
            total_input_tokens: used_tokens,
            token_usage: Vec::new(),
            tool_usage_metadata: None,
            warp_token_usage: HashMap::new(),
            byok_token_usage: HashMap::new(),
            custom_endpoint_token_usage: HashMap::new(),
            context_window_segments: Vec::new(),
        };
        ResponseEvent {
            r#type: Some(api::response_event::Type::Finished(
                api::response_event::StreamFinished {
                    reason: Some(reason),
                    token_usage: vec![stream_finished::TokenUsage {
                        model_id: self.model_id.clone(),
                        // ACP reports current context occupancy, not the input
                        // consumed by this individual request. Galaxy separately
                        // accumulates per-request token usage, so counting it
                        // here would grow the total again on every turn.
                        total_input: 0,
                        output: 0,
                        input_cache_read: 0,
                        input_cache_write: 0,
                        cost_in_cents: 0.0,
                    }],
                    should_refresh_model_config: false,
                    request_cost: None,
                    conversation_usage_metadata: Some(usage_metadata),
                },
            )),
        }
    }
}

fn tool_status_line(title: &str, status: ToolCallStatus) -> String {
    let status = match status {
        ToolCallStatus::Pending => "waiting",
        ToolCallStatus::InProgress => "running",
        ToolCallStatus::Completed => "completed",
        ToolCallStatus::Failed => "failed",
        // ACP marks this enum non-exhaustive. Preserve a useful transcript if a
        // newer agent reports a status this client does not recognize yet.
        _ => "updated",
    };
    format!("\n\n> **{title}** — {status}\n")
}

fn tool_output_block(output: &str) -> String {
    let mut block = String::from("\n");
    for line in output.lines() {
        block.push_str("    ");
        block.push_str(line);
        block.push('\n');
    }
    block
}

#[cfg(test)]
#[path = "response_translator_tests.rs"]
mod tests;
