use std::collections::HashMap;

use galaxy_agent_core::{
    AgentEvent, ProviderRunOutcome, RuntimeActivity, RuntimeActivityStatus, RuntimeCapabilities,
    StopReason, Usage,
};
use uuid::Uuid;
use warp_multi_agent_api::response_event::stream_finished;
use warp_multi_agent_api::{self as api, ClientAction, ResponseEvent};

use super::provider_run_coordinator::ProviderRunProjection;
use crate::ai::agent::runtime_activity;
use crate::ai::bedrock::response_translator::{
    build_add_agent_output_message, build_append_text, build_create_task, build_stream_init,
    build_user_query_message,
};
use crate::ai::openai::response_translator::{build_stream_finished, StreamUsage};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct RuntimeResponseConfig {
    pub(crate) task_id: String,
    pub(crate) conversation_id: String,
    pub(crate) needs_create_task: bool,
    pub(crate) user_query: Option<String>,
    pub(crate) model_id: String,
    pub(crate) max_context_tokens: Option<u32>,
    pub(crate) capabilities: RuntimeCapabilities,
    pub(crate) empty_output_message: Option<String>,
}

/// Converts the provider-neutral runtime lifecycle into Galaxy's existing
/// transcript protocol. Runtime implementations never need to know about the
/// protobuf messages consumed by the UI.
pub(crate) struct RuntimeResponseTranslator {
    config: RuntimeResponseConfig,
    request_id: String,
    initialized: bool,
    text_message_id: Option<String>,
    reasoning_message_id: Option<String>,
    activity_message_ids: HashMap<String, String>,
    activities: HashMap<String, RuntimeActivity>,
    has_visible_output: bool,
    /// Usage for the most recent model call.
    usage: Usage,
    context_usage: Option<(u64, u64)>,
}

/// Projects a multi-turn provider run into one existing Galaxy response stream.
/// Intermediate model stops remain coordinator-internal; only the run outcome
/// emits the UI's terminal `Finished` event.
pub(crate) struct ProviderRunResponseProjector {
    translator: RuntimeResponseTranslator,
    has_started_model_turn: bool,
    finished: bool,
}

impl ProviderRunResponseProjector {
    pub(crate) fn new(config: RuntimeResponseConfig) -> Self {
        Self {
            translator: RuntimeResponseTranslator::new(config),
            has_started_model_turn: false,
            finished: false,
        }
    }

    pub(crate) fn restored(
        config: RuntimeResponseConfig,
        projection_was_initialized: bool,
    ) -> Self {
        Self {
            translator: RuntimeResponseTranslator::restored(config, projection_was_initialized),
            has_started_model_turn: false,
            finished: false,
        }
    }

    pub(crate) fn project(
        &mut self,
        projection: ProviderRunProjection,
    ) -> Result<Vec<ResponseEvent>, String> {
        if self.finished {
            return Err("provider run projection is already finished".to_string());
        }
        match projection {
            ProviderRunProjection::ModelTurnStarted { .. } => {
                if self.has_started_model_turn {
                    self.translator.begin_followup_turn();
                }
                self.has_started_model_turn = true;
                self.translator.translate(AgentEvent::TurnStarted {
                    runtime_request_id: String::new(),
                })
            }
            ProviderRunProjection::ModelEvent { event, .. } => self.translator.translate(event),
            ProviderRunProjection::ModelRetry { .. } => {
                Ok(self.translator.discard_failed_turn_output())
            }
            ProviderRunProjection::ModelTurnRequested { .. }
            | ProviderRunProjection::ModelTurnFinished { .. }
            | ProviderRunProjection::ToolBatchReady { .. } => Ok(Vec::new()),
        }
    }

    pub(crate) fn set_task_id(&mut self, task_id: impl Into<String>) {
        self.translator.set_task_id(task_id);
    }

    pub(crate) fn finish(
        &mut self,
        outcome: &ProviderRunOutcome,
        aggregate_usage: &Usage,
    ) -> Result<Vec<ResponseEvent>, String> {
        if self.finished {
            return Err("provider run projection is already finished".to_string());
        }
        self.finished = true;
        match outcome {
            ProviderRunOutcome::Completed(completion) => Ok(self
                .translator
                .finish_provider_run(completion.stop_reason.clone(), aggregate_usage)),
            ProviderRunOutcome::Failed(failure) => Ok(self
                .translator
                .provider_failure(&failure.message, aggregate_usage)),
            ProviderRunOutcome::Cancelled { .. } => Ok(self
                .translator
                .finish_provider_run(StopReason::Cancelled, aggregate_usage)),
        }
    }
}

impl RuntimeResponseTranslator {
    pub(crate) fn new(config: RuntimeResponseConfig) -> Self {
        Self::with_initialization(config, false)
    }

    pub(crate) fn restored(
        mut config: RuntimeResponseConfig,
        projection_was_initialized: bool,
    ) -> Self {
        // The task and exchange already exist in restored history. If its output was never
        // initialized, replay only the stream Init rather than duplicating task/input messages.
        config.needs_create_task = false;
        config.user_query = None;
        Self::with_initialization(config, projection_was_initialized)
    }

    fn with_initialization(config: RuntimeResponseConfig, initialized: bool) -> Self {
        Self {
            config,
            request_id: Uuid::new_v4().to_string(),
            initialized,
            text_message_id: None,
            reasoning_message_id: None,
            activity_message_ids: HashMap::new(),
            activities: HashMap::new(),
            has_visible_output: false,
            usage: Usage::default(),
            context_usage: None,
        }
    }

    pub(crate) fn translate(&mut self, event: AgentEvent) -> Result<Vec<ResponseEvent>, String> {
        let mut events = Vec::new();
        match event {
            AgentEvent::TurnStarted { .. } => self.initialize(&mut events),
            AgentEvent::KeepAlive | AgentEvent::ToolCallProgress { .. } => {}
            AgentEvent::TextDelta { text } => {
                self.initialize(&mut events);
                self.add_or_append_text(&text, &mut events);
            }
            AgentEvent::ReasoningDelta { text } => {
                self.initialize(&mut events);
                self.add_or_append_reasoning(&text, &mut events);
            }
            AgentEvent::ReasoningCompleted { text, .. } => {
                self.initialize(&mut events);
                self.complete_reasoning(&text, &mut events);
            }
            AgentEvent::RuntimeActivityUpdated { activity } => {
                if self.config.capabilities.host_tool_execution {
                    return Err(
                        "a host-tool runtime emitted runtime-owned tool activity".to_owned()
                    );
                }
                self.initialize(&mut events);
                self.text_message_id = None;
                self.upsert_runtime_activity(activity, &mut events)?;
            }
            AgentEvent::ContextUsageUpdated {
                used_tokens,
                context_size,
            } => {
                if self.config.capabilities.host_managed_history {
                    return Err(
                        "a host-history runtime reported session context occupancy".to_owned()
                    );
                }
                self.context_usage = Some((used_tokens, context_size));
            }
            AgentEvent::UserInputAccepted { text } => {
                if !self.config.capabilities.steering {
                    return Err("a non-steerable runtime accepted live user input".to_owned());
                }
                self.initialize(&mut events);
                events.push(build_user_query_message(&self.config.task_id, &text));
                self.text_message_id = None;
                self.reasoning_message_id = None;
            }
            AgentEvent::RuntimeNotice { message } => {
                self.initialize(&mut events);
                self.text_message_id = None;
                self.add_or_append_text(&message, &mut events);
                self.text_message_id = None;
            }
            AgentEvent::UsageUpdated { usage } => {
                if !self.config.capabilities.host_managed_history {
                    return Err("a session runtime reported provider request usage".to_owned());
                }
                self.usage = usage;
            }
            AgentEvent::TurnStopped { reason } => {
                self.initialize(&mut events);
                if !self.has_visible_output && reason != StopReason::Cancelled {
                    if let Some(message) = self.config.empty_output_message.clone() {
                        self.add_or_append_text(&message, &mut events);
                    }
                }
                events.push(self.finished(reason));
            }
            AgentEvent::Tool { .. } => {
                let owner = if self.config.capabilities.host_tool_execution {
                    "host-tool runtime emitted an unhandled tool lifecycle event"
                } else {
                    "session runtime handed runtime-owned tool execution to Galaxy"
                };
                return Err(owner.to_owned());
            }
        }
        Ok(events)
    }

    pub(crate) fn startup_error(&mut self, message: &str) -> Vec<ResponseEvent> {
        let mut events = Vec::new();
        self.initialize(&mut events);
        self.text_message_id = None;
        self.add_or_append_text(message, &mut events);
        events.push(self.finished(StopReason::Refusal));
        events
    }

    fn finish_provider_run(
        &mut self,
        reason: StopReason,
        aggregate_usage: &Usage,
    ) -> Vec<ResponseEvent> {
        let mut events = Vec::new();
        self.initialize(&mut events);
        if !self.has_visible_output && reason != StopReason::Cancelled {
            if let Some(message) = self.config.empty_output_message.clone() {
                self.add_or_append_text(&message, &mut events);
            }
        }
        events.push(self.finished_with_usage(map_stop_reason(reason), aggregate_usage));
        events
    }

    pub(crate) fn begin_followup_turn(&mut self) {
        self.text_message_id = None;
        self.reasoning_message_id = None;
        self.usage = Usage::default();
    }

    fn discard_failed_turn_output(&mut self) -> Vec<ResponseEvent> {
        let mut events = Vec::new();
        self.usage = Usage::default();
        if let Some(message_id) = self.text_message_id.take() {
            events.push(build_replace_text_message(
                &self.config.task_id,
                &message_id,
                "",
            ));
        }
        if let Some(message_id) = self.reasoning_message_id.take() {
            events.push(build_replace_reasoning_message(
                &self.config.task_id,
                &message_id,
                "",
            ));
        }
        events
    }

    pub(crate) fn set_task_id(&mut self, task_id: impl Into<String>) {
        let task_id = task_id.into();
        if self.config.task_id == task_id {
            return;
        }
        self.config.task_id = task_id;
        self.text_message_id = None;
        self.reasoning_message_id = None;
        self.activity_message_ids.clear();
        self.activities.clear();
    }

    fn initialize(&mut self, events: &mut Vec<ResponseEvent>) {
        if self.initialized {
            return;
        }
        events.push(build_stream_init(
            &self.request_id,
            &self.config.conversation_id,
        ));
        if self.config.needs_create_task {
            events.push(build_create_task(&self.config.task_id));
        }
        if let Some(user_query) = &self.config.user_query {
            events.push(build_user_query_message(&self.config.task_id, user_query));
        }
        self.initialized = true;
    }

    fn add_or_append_text(&mut self, text: &str, events: &mut Vec<ResponseEvent>) {
        if text.is_empty() {
            return;
        }
        self.has_visible_output = true;
        if let Some(message_id) = &self.text_message_id {
            events.push(build_append_text(&self.config.task_id, message_id, text));
        } else {
            let message_id = Uuid::new_v4().to_string();
            events.push(build_add_agent_output_message(
                &self.config.task_id,
                &message_id,
                text,
            ));
            self.text_message_id = Some(message_id);
        }
    }

    fn add_or_append_reasoning(&mut self, text: &str, events: &mut Vec<ResponseEvent>) {
        if text.is_empty() {
            return;
        }
        self.has_visible_output = true;
        if let Some(message_id) = &self.reasoning_message_id {
            events.push(build_reasoning_message(
                &self.config.task_id,
                message_id,
                text,
                true,
            ));
        } else {
            let message_id = Uuid::new_v4().to_string();
            events.push(build_reasoning_message(
                &self.config.task_id,
                &message_id,
                text,
                false,
            ));
            self.reasoning_message_id = Some(message_id);
        }
    }

    fn complete_reasoning(&mut self, text: &str, events: &mut Vec<ResponseEvent>) {
        if let Some(message_id) = &self.reasoning_message_id {
            events.push(build_replace_reasoning_message(
                &self.config.task_id,
                message_id,
                text,
            ));
        } else if !text.is_empty() {
            self.add_or_append_reasoning(text, events);
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
            .map_err(|error| format!("failed to encode runtime activity: {error}"))?;
        let fallback_text = runtime_activity_fallback_text(merged_activity);
        if let Some(message_id) = self.activity_message_ids.get(&activity_id) {
            events.push(build_update_runtime_activity_message(
                &self.config.task_id,
                message_id,
                &fallback_text,
                &server_message_data,
            ));
        } else {
            let message_id = Uuid::new_v4().to_string();
            events.push(build_add_runtime_activity_message(
                &self.config.task_id,
                &message_id,
                &fallback_text,
                &server_message_data,
            ));
            self.activity_message_ids.insert(activity_id, message_id);
        }
        self.has_visible_output = true;
        Ok(())
    }

    fn finished(&self, reason: StopReason) -> ResponseEvent {
        self.finished_with_reason(map_stop_reason(reason))
    }

    fn provider_failure(&mut self, message: &str, aggregate_usage: &Usage) -> Vec<ResponseEvent> {
        let mut events = Vec::new();
        self.initialize(&mut events);
        events.push(self.finished_with_usage(
            stream_finished::Reason::InternalError(stream_finished::InternalError {
                message: message.to_owned(),
            }),
            aggregate_usage,
        ));
        events
    }

    fn finished_with_reason(&self, reason: stream_finished::Reason) -> ResponseEvent {
        self.finished_with_usage(reason, &self.usage)
    }

    fn finished_with_usage(
        &self,
        reason: stream_finished::Reason,
        aggregate_usage: &Usage,
    ) -> ResponseEvent {
        if !self.config.capabilities.host_managed_history {
            let (used_tokens, context_size) = self.context_usage.unwrap_or_default();
            return build_context_finished(
                reason,
                &self.config.model_id,
                used_tokens,
                context_size,
            );
        }
        build_stream_finished(
            reason,
            StreamUsage {
                input_tokens: saturating_i32(aggregate_usage.input_tokens),
                output_tokens: saturating_i32(aggregate_usage.output_tokens),
                cache_read_tokens: saturating_i32(aggregate_usage.cached_input_tokens),
                cache_write_tokens: saturating_i32(aggregate_usage.cache_creation_input_tokens),
                current_context_tokens: Some(saturating_i32(
                    self.usage
                        .input_tokens
                        .saturating_add(self.usage.cached_input_tokens)
                        .saturating_add(self.usage.cache_creation_input_tokens),
                )),
                cost_in_cents: 0.0,
                model_id: self.config.model_id.clone(),
                max_context_tokens: self.config.max_context_tokens,
            },
        )
    }
}

fn build_reasoning_message(
    task_id: &str,
    message_id: &str,
    text: &str,
    append: bool,
) -> ResponseEvent {
    let message = api::Message {
        id: message_id.to_owned(),
        task_id: task_id.to_owned(),
        request_id: String::new(),
        timestamp: None,
        server_message_data: String::new(),
        citations: Vec::new(),
        fetched_memories: Vec::new(),
        message: Some(api::message::Message::AgentReasoning(
            api::message::AgentReasoning {
                reasoning: text.to_owned(),
                finished_duration: None,
            },
        )),
    };
    let action = if append {
        api::client_action::Action::AppendToMessageContent(
            api::client_action::AppendToMessageContent {
                task_id: task_id.to_owned(),
                message: Some(message),
                mask: Some(prost_types::FieldMask {
                    paths: vec!["agent_reasoning.reasoning".to_owned()],
                }),
            },
        )
    } else {
        api::client_action::Action::AddMessagesToTask(api::client_action::AddMessagesToTask {
            task_id: task_id.to_owned(),
            messages: vec![message],
        })
    };
    runtime_client_action(action)
}

fn build_replace_reasoning_message(task_id: &str, message_id: &str, text: &str) -> ResponseEvent {
    let message = api::Message {
        id: message_id.to_owned(),
        task_id: task_id.to_owned(),
        request_id: String::new(),
        timestamp: None,
        server_message_data: String::new(),
        citations: Vec::new(),
        fetched_memories: Vec::new(),
        message: Some(api::message::Message::AgentReasoning(
            api::message::AgentReasoning {
                reasoning: text.to_owned(),
                finished_duration: None,
            },
        )),
    };
    runtime_client_action(api::client_action::Action::UpdateTaskMessage(
        api::client_action::UpdateTaskMessage {
            task_id: task_id.to_owned(),
            message: Some(message),
            mask: Some(prost_types::FieldMask {
                paths: vec!["agent_reasoning.reasoning".to_owned()],
            }),
        },
    ))
}

fn build_replace_text_message(task_id: &str, message_id: &str, text: &str) -> ResponseEvent {
    runtime_client_action(api::client_action::Action::UpdateTaskMessage(
        api::client_action::UpdateTaskMessage {
            task_id: task_id.to_owned(),
            message: Some(api::Message {
                id: message_id.to_owned(),
                task_id: task_id.to_owned(),
                request_id: String::new(),
                timestamp: None,
                server_message_data: String::new(),
                citations: Vec::new(),
                fetched_memories: Vec::new(),
                message: Some(api::message::Message::AgentOutput(
                    api::message::AgentOutput {
                        text: text.to_owned(),
                    },
                )),
            }),
            mask: Some(prost_types::FieldMask {
                paths: vec!["agent_output.text".to_owned()],
            }),
        },
    ))
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
    runtime_client_action(api::client_action::Action::AddMessagesToTask(
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
    runtime_client_action(api::client_action::Action::UpdateTaskMessage(
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

fn runtime_client_action(action: api::client_action::Action) -> ResponseEvent {
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

fn build_context_finished(
    reason: stream_finished::Reason,
    model_id: &str,
    used_tokens: u64,
    context_size: u64,
) -> ResponseEvent {
    let total_input_tokens = u32::try_from(used_tokens).unwrap_or(u32::MAX);
    let context_window_usage = if context_size == 0 {
        0.0
    } else {
        (used_tokens as f32 / context_size as f32).clamp(0.0, 1.0)
    };
    #[allow(deprecated)]
    let usage_metadata = stream_finished::ConversationUsageMetadata {
        context_window_usage,
        summarized: false,
        credits_spent: 0.0,
        platform_credits_spent: 0.0,
        total_input_tokens,
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
                    model_id: model_id.to_owned(),
                    // Session runtimes report current occupancy, not tokens
                    // consumed by this individual request.
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

pub(crate) fn map_stop_reason(reason: StopReason) -> stream_finished::Reason {
    match reason {
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
    }
}

pub(crate) fn saturating_i32(value: u64) -> i32 {
    i32::try_from(value).unwrap_or(i32::MAX)
}

#[cfg(test)]
#[path = "event_translator_tests.rs"]
mod tests;
