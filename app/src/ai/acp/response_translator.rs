use std::collections::HashMap;

use galaxy_agent_core::{AgentEvent, RuntimeActivity, RuntimeActivityStatus, StopReason};
use uuid::Uuid;
use warp_multi_agent_api::response_event::stream_finished;
use warp_multi_agent_api::{self as api, ClientAction, ResponseEvent};

use crate::ai::agent::runtime_activity;
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
    activity_message_ids: HashMap<String, String>,
    activities: HashMap<String, RuntimeActivity>,
    has_visible_output: bool,
    used_tokens: u64,
    context_size: u64,
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
            activity_message_ids: HashMap::new(),
            activities: HashMap::new(),
            has_visible_output: false,
            used_tokens: 0,
            context_size: 0,
        }
    }

    pub(super) fn translate(&mut self, event: AgentEvent) -> Result<Vec<ResponseEvent>, String> {
        let mut events = Vec::new();
        match event {
            AgentEvent::TurnStarted { .. } => self.initialize(&mut events),
            AgentEvent::TextDelta { text } => {
                self.initialize(&mut events);
                self.add_or_append(&text, &mut events);
            }
            // The legacy transcript has no dedicated reasoning surface on this
            // path yet. The shared runtime event remains available for the UI
            // convergence phase instead of being flattened into answer text.
            AgentEvent::ReasoningDelta { .. } | AgentEvent::ReasoningCompleted { .. } => {}
            AgentEvent::RuntimeActivityUpdated { activity } => {
                self.initialize(&mut events);
                self.message_id = None;
                self.upsert_runtime_activity(activity, &mut events)?;
            }
            AgentEvent::ContextUsageUpdated {
                used_tokens,
                context_size,
            } => {
                self.used_tokens = used_tokens;
                self.context_size = context_size;
            }
            AgentEvent::UserInputAccepted { text } => {
                self.initialize(&mut events);
                events.push(build_user_query_message(&self.task_id, &text));
                self.message_id = None;
            }
            AgentEvent::RuntimeNotice { message } => {
                self.initialize(&mut events);
                self.message_id = None;
                self.add_or_append(&message, &mut events);
                self.message_id = None;
            }
            AgentEvent::TurnStopped { reason } => {
                self.initialize(&mut events);
                if !self.has_visible_output && reason != StopReason::Cancelled {
                    self.add_or_append(
                        "> ACP agent completed without a text response.",
                        &mut events,
                    );
                }
                events.push(self.finished(reason));
            }
            AgentEvent::Tool { .. } => {
                return Err(
                    "ACP runtime attempted to hand agent-owned tool execution to Galaxy".to_owned(),
                );
            }
            AgentEvent::UsageUpdated { .. } => {
                return Err("ACP runtime reported provider-style request usage".to_owned());
            }
        }
        Ok(events)
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
        self.has_visible_output = true;
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

    fn upsert_runtime_activity(
        &mut self,
        activity: RuntimeActivity,
        events: &mut Vec<ResponseEvent>,
    ) -> Result<(), String> {
        let activity_id = activity.id.clone();
        let merged_activity = self
            .activities
            .entry(activity_id.clone())
            .or_insert_with(|| activity.clone());
        if !activity.title.trim().is_empty() {
            merged_activity.title = activity.title;
        }
        if activity.status.is_some() {
            merged_activity.status = activity.status;
        }
        if activity.output.is_some() {
            merged_activity.output = activity.output;
        }

        let server_message_data = runtime_activity::encode(merged_activity)
            .map_err(|error| format!("failed to encode ACP runtime activity: {error}"))?;
        let fallback_text = runtime_activity_fallback_text(merged_activity);
        if let Some(message_id) = self.activity_message_ids.get(&activity_id) {
            events.push(build_update_runtime_activity_message(
                &self.task_id,
                message_id,
                &fallback_text,
                &server_message_data,
            ));
        } else {
            let message_id = Uuid::new_v4().to_string();
            events.push(build_add_runtime_activity_message(
                &self.task_id,
                &message_id,
                &fallback_text,
                &server_message_data,
            ));
            self.activity_message_ids.insert(activity_id, message_id);
        }
        self.has_visible_output = true;
        Ok(())
    }

    fn finished(&self, stop_reason: StopReason) -> ResponseEvent {
        let reason = match stop_reason {
            StopReason::Completed | StopReason::Cancelled => {
                stream_finished::Reason::Done(stream_finished::Done {})
            }
            StopReason::MaxTokens => {
                stream_finished::Reason::MaxTokenLimit(stream_finished::ReachedMaxTokenLimit {})
            }
            StopReason::ContextWindowExceeded => stream_finished::Reason::ContextWindowExceeded(
                stream_finished::ContextWindowExceeded {},
            ),
            StopReason::Refusal | StopReason::ToolLoopLimit | StopReason::Other(_) => {
                stream_finished::Reason::Other(stream_finished::Other {})
            }
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

fn runtime_activity_fallback_text(activity: &RuntimeActivity) -> String {
    let title = &activity.title;
    let status = activity.status.as_ref().map(|status| match status {
        RuntimeActivityStatus::Pending => "waiting",
        RuntimeActivityStatus::InProgress => "running",
        RuntimeActivityStatus::Completed => "completed",
        RuntimeActivityStatus::Failed => "failed",
        RuntimeActivityStatus::Other(_) => "updated",
    });
    let mut text = match status {
        Some(status) => format!("> **{title}** — {status}"),
        None => format!("> **{title}**"),
    };
    if let Some(output) = &activity.output {
        text.push_str("\n\n");
        for line in output.lines() {
            text.push_str("    ");
            text.push_str(line);
            text.push('\n');
        }
    }
    text
}

fn build_add_runtime_activity_message(
    task_id: &str,
    message_id: &str,
    fallback_text: &str,
    server_message_data: &str,
) -> ResponseEvent {
    let message = runtime_activity_message(task_id, message_id, fallback_text, server_message_data);
    runtime_activity_client_action(api::client_action::Action::AddMessagesToTask(
        api::client_action::AddMessagesToTask {
            task_id: task_id.to_owned(),
            messages: vec![message],
        },
    ))
}

fn build_update_runtime_activity_message(
    task_id: &str,
    message_id: &str,
    fallback_text: &str,
    server_message_data: &str,
) -> ResponseEvent {
    let message = runtime_activity_message(task_id, message_id, fallback_text, server_message_data);
    runtime_activity_client_action(api::client_action::Action::UpdateTaskMessage(
        api::client_action::UpdateTaskMessage {
            task_id: task_id.to_owned(),
            message: Some(message),
            mask: Some(prost_types::FieldMask {
                paths: vec![
                    "agent_output.text".to_owned(),
                    "server_message_data".to_owned(),
                ],
            }),
        },
    ))
}

fn runtime_activity_message(
    task_id: &str,
    message_id: &str,
    fallback_text: &str,
    server_message_data: &str,
) -> api::Message {
    api::Message {
        id: message_id.to_owned(),
        task_id: task_id.to_owned(),
        request_id: String::new(),
        timestamp: None,
        server_message_data: server_message_data.to_owned(),
        citations: Vec::new(),
        fetched_memories: Vec::new(),
        message: Some(api::message::Message::AgentOutput(
            api::message::AgentOutput {
                text: fallback_text.to_owned(),
            },
        )),
    }
}

fn runtime_activity_client_action(action: api::client_action::Action) -> ResponseEvent {
    ResponseEvent {
        r#type: Some(api::response_event::Type::ClientActions(
            api::response_event::ClientActions {
                actions: vec![ClientAction {
                    action: Some(action),
                }],
            },
        )),
    }
}

#[cfg(test)]
#[path = "response_translator_tests.rs"]
mod tests;
