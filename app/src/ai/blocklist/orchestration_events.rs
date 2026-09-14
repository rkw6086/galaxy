#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

use uuid::Uuid;
use warp_multi_agent_api as api;
use warpui::{Entity, ModelContext, SingletonEntity};

use super::history_model::{
    BlocklistAIHistoryEvent, BlocklistAIHistoryModel, ConversationStatusUpdate,
};
use crate::ai::agent::conversation::{AIConversationId, ConversationStatus};
use crate::ai::agent::task::TaskId;
use crate::ai::agent::{
    AIAgentExchangeId, AIAgentInput, AIAgentOutputMessageType, LifecycleEventType,
    ReceivedMessageInput,
};

const MAX_RETRY_ATTEMPTS: i32 = 3;
const MAX_PENDING_LIFECYCLE_EVENTS_PER_TARGET: usize = 200;
const MAX_SUBAGENT_QUESTION_DEPTH: u8 = 3;

/// Stage associated with a lifecycle error detail.
/// This keeps persisted/runtime metadata consistent across API payloads and DB rows.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum LifecycleEventDetailStage {
    Runtime,
}

#[derive(Debug, Clone, Default)]
pub(super) struct LifecycleEventDetailPayload {
    pub(crate) stage: Option<LifecycleEventDetailStage>,
    pub(crate) reason: Option<String>,
    pub(crate) error_message: Option<String>,
    pub(crate) blocked_action: Option<String>,
}

impl LifecycleEventDetailStage {
    /// Canonical lowercase representation used in persistence/API payloads.
    fn as_str(self) -> &'static str {
        match self {
            Self::Runtime => "runtime",
        }
    }
}

/// Type-specific queued data.
#[derive(Debug, Clone)]
pub enum PendingEventDetail {
    /// A locally authorized parent/child message; never sent through the hosted messaging API.
    LocalMessage {
        message_id: String,
        subject: String,
        message_body: String,
    },
    Message {
        message_id: String,
        addresses: Vec<String>,
        subject: String,
        message_body: String,
    },
    Lifecycle {
        event: api::AgentEvent,
    },
    /// A subagent is asking its parent a question (routed from AskUserQuestion).
    SubagentQuestion {
        source_conversation_id: AIConversationId,
        question_text: String,
        options: Vec<String>,
    },
    /// The parent's answer to a subagent's question.
    SubagentAnswer {
        answer_text: String,
    },
    /// A subagent reporting its completion summary to the parent.
    SubagentCompletionSummary,
}

/// A queued event consumed by the controller.
#[derive(Debug, Clone)]
pub struct PendingEvent {
    pub event_id: String,
    pub source_agent_id: String,
    pub attempt_count: i32,
    pub detail: PendingEventDetail,
}

pub enum OrchestrationEventServiceEvent {
    /// Signals that a conversation may have pending orchestration events
    /// ready to drain.
    EventsReady { conversation_id: AIConversationId },
}

/// Synchronous state manager for orchestration event queuing, delivery tracking, and readiness detection.
pub struct OrchestrationEventService {
    pending_events: HashMap<AIConversationId, Vec<PendingEvent>>,
    awaiting_server_echo_events: HashMap<AIConversationId, Vec<PendingEvent>>,
    conversation_statuses: HashMap<AIConversationId, ConversationStatus>,
    restarted_children: HashSet<AIConversationId>,
}

impl OrchestrationEventService {
    pub fn new(ctx: &mut ModelContext<Self>) -> Self {
        let history_model = BlocklistAIHistoryModel::handle(ctx);
        ctx.subscribe_to_model(&history_model, move |me, _, event, ctx| {
            me.handle_history_event(event, ctx);
        });
        Self::new_without_subscriptions()
    }

    fn new_without_subscriptions() -> Self {
        Self {
            pending_events: HashMap::new(),
            awaiting_server_echo_events: HashMap::new(),
            conversation_statuses: HashMap::new(),
            restarted_children: HashSet::new(),
        }
    }

    pub fn handle_history_event(
        &mut self,
        event: &BlocklistAIHistoryEvent,
        ctx: &mut ModelContext<Self>,
    ) {
        match event {
            BlocklistAIHistoryEvent::UpdatedConversationStatus {
                conversation_id,
                update,
                ..
            } => {
                let is_restored = matches!(update, ConversationStatusUpdate::Restored);
                self.on_conversation_status_updated(*conversation_id, is_restored, ctx)
            }
            BlocklistAIHistoryEvent::UpdatedStreamingExchange {
                conversation_id,
                exchange_id,
                ..
            } => self.confirm_delivery_from_exchange(*conversation_id, *exchange_id, ctx),
            BlocklistAIHistoryEvent::StartedNewConversation {
                new_conversation_id,
                ..
            } => self.sync_conversation_status(*new_conversation_id, ctx),
            BlocklistAIHistoryEvent::RestoredConversations {
                conversation_ids, ..
            } => {
                for conversation_id in conversation_ids {
                    self.sync_conversation_status(*conversation_id, ctx);
                }
            }
            BlocklistAIHistoryEvent::RemoveConversation {
                conversation_id, ..
            }
            | BlocklistAIHistoryEvent::DeletedConversation {
                conversation_id, ..
            } => {
                self.pending_events.remove(conversation_id);
                self.awaiting_server_echo_events.remove(conversation_id);
                self.conversation_statuses.remove(conversation_id);
                self.restarted_children.remove(conversation_id);
            }
            _ => {}
        }
    }

    fn sync_conversation_status(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &ModelContext<Self>,
    ) {
        let Some(conversation) =
            BlocklistAIHistoryModel::as_ref(ctx).conversation(&conversation_id)
        else {
            self.conversation_statuses.remove(&conversation_id);
            return;
        };
        self.conversation_statuses
            .insert(conversation_id, conversation.status().clone());
    }

    fn on_conversation_status_updated(
        &mut self,
        conversation_id: AIConversationId,
        is_restored: bool,
        ctx: &mut ModelContext<Self>,
    ) {
        let current_status = {
            let Some(conversation) =
                BlocklistAIHistoryModel::as_ref(ctx).conversation(&conversation_id)
            else {
                self.conversation_statuses.remove(&conversation_id);
                return;
            };
            conversation.status().clone()
        };

        let previous = self
            .conversation_statuses
            .insert(conversation_id, current_status.clone());
        if !is_restored && previous.as_ref() != Some(&current_status) {
            self.report_restarted_child(conversation_id, previous.as_ref(), &current_status, ctx);
        }
        let has_pending = self
            .pending_events
            .get(&conversation_id)
            .is_some_and(|events| !events.is_empty());
        // Re-fire EventsReady whenever the conversation reaches a status
        // that `conversation_ready_for_pending_events` would accept, so
        // events queued while the stream was in flight drain as soon as
        // the stream finishes — either to `Success` or to
        // `WaitingForEvents` via a `wait_for_events` yield.
        if !is_restored
            && (matches!(
                &current_status,
                ConversationStatus::Success | ConversationStatus::WaitingForEvents
            ) || (matches!(current_status, ConversationStatus::Error)
                && self.has_pending_local_message(conversation_id)))
            && has_pending
        {
            ctx.emit(OrchestrationEventServiceEvent::EventsReady { conversation_id });
        }
    }

    pub fn has_pending_local_message(&self, conversation_id: AIConversationId) -> bool {
        self.pending_events
            .get(&conversation_id)
            .is_some_and(|events| {
                events
                    .iter()
                    .any(|event| matches!(event.detail, PendingEventDetail::LocalMessage { .. }))
            })
    }

    /// Local providers commit the input directly and do not emit a hosted message echo.
    pub fn confirm_local_messages(&mut self, conversation_id: AIConversationId) {
        if let Some(events) = self.awaiting_server_echo_events.get_mut(&conversation_id) {
            events.retain(|event| !matches!(event.detail, PendingEventDetail::LocalMessage { .. }));
            if events.is_empty() {
                self.awaiting_server_echo_events.remove(&conversation_id);
            }
        }
    }

    pub fn send_local_follow_up(
        &mut self,
        parent: AIConversationId,
        child: AIConversationId,
        message: String,
        ctx: &mut ModelContext<Self>,
    ) -> String {
        self.restarted_children.insert(child);
        self.enqueue_local_message(
            child,
            parent,
            "Follow-up from your orchestrator",
            message,
            ctx,
        )
    }

    fn enqueue_local_message(
        &mut self,
        target: AIConversationId,
        source: AIConversationId,
        subject: &str,
        message_body: String,
        ctx: &mut ModelContext<Self>,
    ) -> String {
        let message_id = Uuid::new_v4().to_string();
        self.enqueue_event_batch(
            target,
            vec![PendingEvent {
                event_id: message_id.clone(),
                source_agent_id: source.to_string(),
                attempt_count: 0,
                detail: PendingEventDetail::LocalMessage {
                    message_id: message_id.clone(),
                    subject: subject.to_string(),
                    message_body,
                },
            }],
            ctx,
        );
        message_id
    }

    fn report_restarted_child(
        &mut self,
        child: AIConversationId,
        previous: Option<&ConversationStatus>,
        status: &ConversationStatus,
        ctx: &mut ModelContext<Self>,
    ) {
        let history = BlocklistAIHistoryModel::as_ref(ctx);
        let Some(conversation) = history.conversation(&child) else {
            return;
        };
        if conversation.is_remote_child() {
            return;
        }
        let Some(parent) = conversation.parent_conversation_id() else {
            return;
        };
        let restarting = matches!(
            previous,
            Some(
                ConversationStatus::Error
                    | ConversationStatus::Cancelled
                    | ConversationStatus::Success
            )
        ) && matches!(
            status,
            ConversationStatus::InProgress | ConversationStatus::TransientError
        );
        if restarting {
            self.restarted_children.insert(child);
        }
        if !self.restarted_children.contains(&child) {
            return;
        }
        let name = conversation.agent_name().unwrap_or("child agent");
        let message = if restarting {
            format!("Child {name} ({child}) has restarted. Its earlier terminal result is no longer the final outcome. Keep tracking this child; do not spawn a replacement. A completion update will follow.")
        } else if matches!(status, ConversationStatus::Success) {
            let output = conversation
                .all_exchanges()
                .into_iter()
                .rev()
                .filter_map(|exchange| exchange.output_status.output())
                .map(|output| output.get().format_for_copy(None))
                .find(|text| !text.is_empty())
                .unwrap_or_default();
            let mut end = output.len().min(16_000);
            while !output.is_char_boundary(end) {
                end -= 1;
            }
            format!("Restarted child {name} ({child}) completed successfully. Incorporate this result into your task, even if an earlier attempt failed.\n{}{}", &output[..end], if end < output.len() { "\n[Output excerpt truncated; inspect the child conversation for more.]" } else { "" })
        } else if matches!(
            status,
            ConversationStatus::Error | ConversationStatus::Cancelled
        ) {
            format!("Restarted child {name} ({child}) ended with {status:?}: {}. Diagnose the failure before retrying; do not loop or override a user cancellation.", conversation.status_error_message().unwrap_or_default())
        } else {
            return;
        };
        if matches!(
            status,
            ConversationStatus::Success | ConversationStatus::Error | ConversationStatus::Cancelled
        ) {
            self.restarted_children.remove(&child);
        }
        self.enqueue_local_message(parent, child, "Child agent progress update", message, ctx);
    }

    fn enqueue_lifecycle_event(
        &mut self,
        target_conversation_id: AIConversationId,
        pending: PendingEvent,
    ) {
        // Lifecycle queues are maintained separately per target conversation.
        // Coalescing and cap enforcement happen before queue insertion.
        let dropped_for_cap = {
            let queue = self
                .pending_events
                .entry(target_conversation_id)
                .or_default();
            let _ = coalesce_lifecycle_events(queue, &pending);
            queue.push(pending);
            enforce_lifecycle_queue_cap(queue, MAX_PENDING_LIFECYCLE_EVENTS_PER_TARGET)
        };
        if !dropped_for_cap.is_empty() {
            log::warn!(
                "Dropped {} coalescable lifecycle events due to queue cap for target conversation {target_conversation_id:?}",
                dropped_for_cap.len()
            );
        }
    }

    /// Accepts pre-built events from the v2 streamer and enqueues them
    /// for drain by the controller via the normal injection path.
    /// Lifecycle events go through coalescing and cap enforcement.
    pub fn enqueue_event_batch(
        &mut self,
        conversation_id: AIConversationId,
        events: Vec<PendingEvent>,
        ctx: &mut ModelContext<Self>,
    ) {
        if events.is_empty() {
            return;
        }
        for event in events {
            if matches!(event.detail, PendingEventDetail::Lifecycle { .. }) {
                self.enqueue_lifecycle_event(conversation_id, event);
            } else {
                self.pending_events
                    .entry(conversation_id)
                    .or_default()
                    .push(event);
            }
        }
        ctx.emit(OrchestrationEventServiceEvent::EventsReady { conversation_id });
    }

    #[cfg(any(test, not(target_family = "wasm")))]
    pub fn has_pending_events(&self, conversation_id: AIConversationId) -> bool {
        self.pending_events
            .get(&conversation_id)
            .is_some_and(|events| !events.is_empty())
    }

    /// Drain and return all pending events for a conversation.
    fn drain_pending_events(&mut self, conversation_id: &AIConversationId) -> Vec<PendingEvent> {
        self.pending_events
            .remove(conversation_id)
            .unwrap_or_default()
    }

    /// Drains pending events for a conversation, resolves the root task ID,
    /// and converts them to AIAgentInput variants ready for injection.
    /// Returns None if there are no events or the conversation cannot be found
    /// (in which case events are requeued automatically).
    pub fn drain_events_for_request(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) -> Option<(Vec<AIAgentInput>, TaskId)> {
        let inputs = self.drain_and_convert_events(conversation_id);
        if inputs.is_empty() {
            return None;
        }
        let Some(conversation) =
            BlocklistAIHistoryModel::as_ref(ctx).conversation(&conversation_id)
        else {
            self.requeue_awaiting_events(conversation_id, ctx);
            return None;
        };
        Some((inputs, conversation.get_root_task_id().clone()))
    }

    /// Drains pending events for a conversation and converts them to
    /// AIAgentInput variants ready for injection. Moves the drained events
    /// to awaiting_server_echo_events for delivery confirmation.
    fn drain_and_convert_events(&mut self, conversation_id: AIConversationId) -> Vec<AIAgentInput> {
        let deliverable = self.drain_pending_events(&conversation_id);
        if deliverable.is_empty() {
            return vec![];
        }

        let mut messages = Vec::new();
        let mut lifecycle_events = Vec::new();
        let mut server_bound_events = Vec::new();
        for event in deliverable {
            match &event.detail {
                PendingEventDetail::LocalMessage {
                    message_id,
                    subject,
                    message_body,
                } => {
                    messages.push(ReceivedMessageInput {
                        message_id: message_id.clone(),
                        sender_agent_id: event.source_agent_id.clone(),
                        addresses: vec![conversation_id.to_string()],
                        subject: subject.clone(),
                        message_body: message_body.clone(),
                    });
                    server_bound_events.push(event);
                }
                PendingEventDetail::Message {
                    message_id,
                    addresses,
                    subject,
                    message_body,
                } => {
                    messages.push(ReceivedMessageInput {
                        message_id: message_id.clone(),
                        sender_agent_id: event.source_agent_id.clone(),
                        addresses: addresses.clone(),
                        subject: subject.clone(),
                        message_body: message_body.clone(),
                    });
                    server_bound_events.push(event);
                }
                PendingEventDetail::Lifecycle { event: _ } => {
                    lifecycle_events.push(
                        if let PendingEventDetail::Lifecycle { event: e } = &event.detail {
                            e.clone()
                        } else {
                            unreachable!()
                        },
                    );
                    server_bound_events.push(event);
                }
                // Local-only subagent events are consumed directly by the controller,
                // not converted to AIAgentInput or awaited for server echo.
                PendingEventDetail::SubagentQuestion { .. }
                | PendingEventDetail::SubagentAnswer { .. }
                | PendingEventDetail::SubagentCompletionSummary => {}
            }
        }

        // Only server-bound events need echo confirmation.
        if !server_bound_events.is_empty() {
            self.awaiting_server_echo_events
                .entry(conversation_id)
                .or_default()
                .extend(server_bound_events);
        }

        let mut inputs = Vec::new();
        if !messages.is_empty() {
            inputs.push(AIAgentInput::MessagesReceivedFromAgents { messages });
        }
        if !lifecycle_events.is_empty() {
            inputs.push(AIAgentInput::EventsFromAgents {
                events: lifecycle_events,
            });
        }
        inputs
    }

    /// Drain only the local subagent events (Question/Answer/Summary) for a conversation.
    /// These are not sent to the server and are consumed directly by the controller.
    pub fn drain_subagent_events(
        &mut self,
        conversation_id: &AIConversationId,
    ) -> Vec<PendingEvent> {
        let Some(pending) = self.pending_events.get_mut(conversation_id) else {
            return vec![];
        };

        let mut subagent_events = Vec::new();
        pending.retain(|event| match &event.detail {
            PendingEventDetail::SubagentQuestion { .. }
            | PendingEventDetail::SubagentAnswer { .. }
            | PendingEventDetail::SubagentCompletionSummary => {
                subagent_events.push(event.clone());
                false
            }
            _ => true,
        });

        if pending.is_empty() {
            self.pending_events.remove(conversation_id);
        }

        subagent_events
    }

    /// Moves all awaiting events back to pending for retry after a failed
    /// send attempt. Increments attempt counts and drops events that have
    /// exhausted their retry limit.
    pub fn requeue_awaiting_events(
        &mut self,
        conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        let events = self
            .awaiting_server_echo_events
            .remove(&conversation_id)
            .unwrap_or_default();
        if events.is_empty() {
            return;
        }

        let (retryable, exhausted) =
            increment_attempt_and_partition_by_retry_limit(events, MAX_RETRY_ATTEMPTS);

        if !exhausted.is_empty() {
            log::warn!(
                "Dropping {} orchestration events after exhausting retries",
                exhausted.len()
            );
        }

        if !retryable.is_empty() {
            let queue = self.pending_events.entry(conversation_id).or_default();
            let mut combined = retryable;
            combined.append(queue);
            *queue = combined;

            ctx.emit(OrchestrationEventServiceEvent::EventsReady { conversation_id });
        }
    }

    /// Scans the exchange output for orchestration IDs echoed back by the
    /// server, then clears matching entries from awaiting_server_echo_events.
    fn confirm_delivery_from_exchange(
        &mut self,
        conversation_id: AIConversationId,
        exchange_id: AIAgentExchangeId,
        ctx: &ModelContext<Self>,
    ) {
        if !self
            .awaiting_server_echo_events
            .contains_key(&conversation_id)
        {
            return;
        }

        let Some(conversation) =
            BlocklistAIHistoryModel::as_ref(ctx).conversation(&conversation_id)
        else {
            return;
        };
        let Some(exchange) = conversation.exchange_with_id(exchange_id) else {
            return;
        };

        let mut echoed_message_ids = Vec::new();
        let mut echoed_lifecycle_event_ids = Vec::new();
        if let Some(output) = exchange.output_status.output() {
            for msg in &output.get().messages {
                match &msg.message {
                    AIAgentOutputMessageType::MessagesReceivedFromAgents { messages } => {
                        for received in messages {
                            if !received.message_id.is_empty() {
                                echoed_message_ids.push(received.message_id.clone());
                            }
                        }
                    }
                    AIAgentOutputMessageType::EventsFromAgents { event_ids } => {
                        for id in event_ids {
                            if !id.is_empty() {
                                echoed_lifecycle_event_ids.push(id.clone());
                            }
                        }
                    }
                    AIAgentOutputMessageType::Text(_)
                    | AIAgentOutputMessageType::Reasoning { .. }
                    | AIAgentOutputMessageType::Summarization { .. }
                    | AIAgentOutputMessageType::Subagent(_)
                    | AIAgentOutputMessageType::RuntimeActivity(_)
                    | AIAgentOutputMessageType::Action(_)
                    | AIAgentOutputMessageType::TodoOperation(_)
                    | AIAgentOutputMessageType::WebSearch(_)
                    | AIAgentOutputMessageType::WebFetch(_)
                    | AIAgentOutputMessageType::CommentsAddressed { .. }
                    | AIAgentOutputMessageType::DebugOutput { .. }
                    | AIAgentOutputMessageType::ArtifactCreated(_)
                    | AIAgentOutputMessageType::SkillInvoked(_) => {}
                }
            }
        }

        if !echoed_message_ids.is_empty() || !echoed_lifecycle_event_ids.is_empty() {
            self.acknowledge_delivery_from_server_echo(
                conversation_id,
                &echoed_message_ids,
                &echoed_lifecycle_event_ids,
            );
        }
    }

    /// Clears awaiting_server_echo_events entries that match the given IDs.
    fn acknowledge_delivery_from_server_echo(
        &mut self,
        conversation_id: AIConversationId,
        echoed_message_ids: &[String],
        echoed_lifecycle_event_ids: &[String],
    ) {
        if echoed_message_ids.is_empty() && echoed_lifecycle_event_ids.is_empty() {
            return;
        }

        let echoed_message_ids: HashSet<&str> =
            echoed_message_ids.iter().map(String::as_str).collect();
        let echoed_lifecycle_event_ids: HashSet<&str> = echoed_lifecycle_event_ids
            .iter()
            .map(String::as_str)
            .collect();
        let should_remove_entry = {
            let Some(awaiting_events) = self.awaiting_server_echo_events.get_mut(&conversation_id)
            else {
                return;
            };

            awaiting_events.retain(|pending_event| {
                let was_echoed = did_event_round_trip_through_server(
                    pending_event,
                    &echoed_message_ids,
                    &echoed_lifecycle_event_ids,
                );
                !was_echoed
            });

            awaiting_events.is_empty()
        };

        if should_remove_entry {
            self.awaiting_server_echo_events.remove(&conversation_id);
        }
    }

    /// Route a subagent's AskUserQuestion to the parent conversation for silent auto-answer.
    pub fn route_subagent_question_to_parent(
        &mut self,
        child_conversation_id: AIConversationId,
        parent_conversation_id: AIConversationId,
        question_text: String,
        options: Vec<String>,
        depth: u8,
        ctx: &mut ModelContext<Self>,
    ) {
        if depth >= MAX_SUBAGENT_QUESTION_DEPTH {
            log::warn!(
                "Subagent question depth limit reached for conversation {:?}",
                child_conversation_id
            );
            return;
        }

        let event = PendingEvent {
            event_id: Uuid::new_v4().to_string(),
            source_agent_id: child_conversation_id.to_string(),
            attempt_count: 0,
            detail: PendingEventDetail::SubagentQuestion {
                source_conversation_id: child_conversation_id,
                question_text,
                options,
            },
        };

        self.pending_events
            .entry(parent_conversation_id)
            .or_default()
            .push(event);

        ctx.emit(OrchestrationEventServiceEvent::EventsReady {
            conversation_id: parent_conversation_id,
        });
    }

    /// Route the parent's answer back to the child subagent.
    pub fn route_answer_to_subagent(
        &mut self,
        child_conversation_id: AIConversationId,
        answer_text: String,
        ctx: &mut ModelContext<Self>,
    ) {
        let event = PendingEvent {
            event_id: Uuid::new_v4().to_string(),
            source_agent_id: "parent".to_string(),
            attempt_count: 0,
            detail: PendingEventDetail::SubagentAnswer { answer_text },
        };

        self.pending_events
            .entry(child_conversation_id)
            .or_default()
            .push(event);

        ctx.emit(OrchestrationEventServiceEvent::EventsReady {
            conversation_id: child_conversation_id,
        });
    }

    /// Route a subagent's completion summary to the parent conversation.
    pub fn route_subagent_completion_summary(
        &mut self,
        child_conversation_id: AIConversationId,
        parent_conversation_id: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        let event = PendingEvent {
            event_id: Uuid::new_v4().to_string(),
            source_agent_id: child_conversation_id.to_string(),
            attempt_count: 0,
            detail: PendingEventDetail::SubagentCompletionSummary,
        };

        self.pending_events
            .entry(parent_conversation_id)
            .or_default()
            .push(event);

        ctx.emit(OrchestrationEventServiceEvent::EventsReady {
            conversation_id: parent_conversation_id,
        });
    }
}

fn did_event_round_trip_through_server(
    pending_event: &PendingEvent,
    echoed_message_ids: &HashSet<&str>,
    echoed_lifecycle_event_ids: &HashSet<&str>,
) -> bool {
    match &pending_event.detail {
        PendingEventDetail::Message { message_id, .. }
        | PendingEventDetail::LocalMessage { message_id, .. } => {
            echoed_message_ids.contains(message_id.as_str())
        }
        PendingEventDetail::Lifecycle { event } => {
            echoed_lifecycle_event_ids.contains(event.event_id.as_str())
        }
        // Local-only events never round-trip through the server.
        PendingEventDetail::SubagentQuestion { .. }
        | PendingEventDetail::SubagentAnswer { .. }
        | PendingEventDetail::SubagentCompletionSummary => false,
    }
}

pub(super) fn build_lifecycle_event(
    event_id: String,
    sender_agent_id: String,
    event_type: LifecycleEventType,
    occurred_at: prost_types::Timestamp,
    detail_payload: &LifecycleEventDetailPayload,
) -> api::AgentEvent {
    // Build the API envelope that is forwarded to recipients and stored in memory.
    // This keeps `occurred_at` attached
    // to the event itself (not inferred at formatting time).
    let detail = lifecycle_event_detail_from_type(event_type, detail_payload);
    api::AgentEvent {
        event_id,
        occurred_at: Some(occurred_at),
        event: Some(api::agent_event::Event::LifecycleEvent(
            api::agent_event::LifecycleEvent {
                sender_agent_id,
                detail,
            },
        )),
    }
}

#[allow(deprecated)]
fn lifecycle_event_detail_from_type(
    event_type: LifecycleEventType,
    detail_payload: &LifecycleEventDetailPayload,
) -> Option<api::agent_event::lifecycle_event::Detail> {
    match event_type {
        LifecycleEventType::InProgress => {
            Some(api::agent_event::lifecycle_event::Detail::InProgress(()))
        }
        LifecycleEventType::Succeeded => {
            Some(api::agent_event::lifecycle_event::Detail::Succeeded(()))
        }
        LifecycleEventType::Failed => Some(api::agent_event::lifecycle_event::Detail::Failed(
            api::agent_event::lifecycle_event::Failed {
                reason: detail_payload.reason.clone().unwrap_or_default(),
                error_message: detail_payload.error_message.clone().unwrap_or_default(),
            },
        )),
        // Legacy variants delegate to their new equivalents.
        LifecycleEventType::Started => {
            Some(api::agent_event::lifecycle_event::Detail::InProgress(()))
        }
        LifecycleEventType::Idle => Some(api::agent_event::lifecycle_event::Detail::Succeeded(())),
        LifecycleEventType::Restarted => {
            Some(api::agent_event::lifecycle_event::Detail::InProgress(()))
        }
        LifecycleEventType::Cancelled => {
            Some(api::agent_event::lifecycle_event::Detail::Cancelled(()))
        }
        LifecycleEventType::Blocked => Some(api::agent_event::lifecycle_event::Detail::Blocked(
            api::agent_event::lifecycle_event::Blocked {
                blocked_action: detail_payload.blocked_action.clone().unwrap_or_default(),
            },
        )),
        LifecycleEventType::Errored => Some(api::agent_event::lifecycle_event::Detail::Errored(
            api::agent_event::lifecycle_event::Errored {
                stage: detail_payload
                    .stage
                    .map(|stage| stage.as_str().to_string())
                    .unwrap_or_default(),
                reason: detail_payload.reason.clone().unwrap_or_default(),
                error_message: detail_payload.error_message.clone().unwrap_or_default(),
            },
        )),
        LifecycleEventType::Unspecified => None,
    }
}

#[allow(deprecated)]
pub(super) fn lifecycle_event_type_from_proto(
    lifecycle_event: &api::agent_event::LifecycleEvent,
) -> api::LifecycleEventType {
    match lifecycle_event.detail.as_ref() {
        Some(api::agent_event::lifecycle_event::Detail::InProgress(_)) => {
            api::LifecycleEventType::InProgress
        }
        Some(api::agent_event::lifecycle_event::Detail::Succeeded(_)) => {
            api::LifecycleEventType::Succeeded
        }
        Some(api::agent_event::lifecycle_event::Detail::Failed(_)) => {
            api::LifecycleEventType::Failed
        }
        // Legacy detail variants map to new types.
        Some(api::agent_event::lifecycle_event::Detail::Started(_)) => {
            api::LifecycleEventType::InProgress
        }
        Some(api::agent_event::lifecycle_event::Detail::Idle(_)) => {
            api::LifecycleEventType::Succeeded
        }
        Some(api::agent_event::lifecycle_event::Detail::Restarted(_)) => {
            api::LifecycleEventType::InProgress
        }
        Some(api::agent_event::lifecycle_event::Detail::Cancelled(_)) => {
            api::LifecycleEventType::Cancelled
        }
        Some(api::agent_event::lifecycle_event::Detail::Blocked(_)) => {
            api::LifecycleEventType::Blocked
        }
        Some(api::agent_event::lifecycle_event::Detail::Errored(_)) => {
            api::LifecycleEventType::Errored
        }
        None => api::LifecycleEventType::Unspecified,
    }
}

/// True when a pending event is a lifecycle succeeded/in_progress event and
/// therefore eligible to be superseded by a newer lifecycle transition from
/// the same sender.
fn is_coalescable_lifecycle_pending_event(event: &PendingEvent) -> bool {
    let PendingEventDetail::Lifecycle { event: agent_event } = &event.detail else {
        return false;
    };
    let Some(api::agent_event::Event::LifecycleEvent(lifecycle_event)) = &agent_event.event else {
        return false;
    };
    matches!(
        lifecycle_event_type_from_proto(lifecycle_event),
        api::LifecycleEventType::Succeeded | api::LifecycleEventType::InProgress
    )
}

fn coalesce_lifecycle_events(
    queue: &mut Vec<PendingEvent>,
    new_event: &PendingEvent,
) -> Vec<String> {
    // Remove older supersedable lifecycle events for the same sender as `new_event`.
    // Returns the removed event IDs for callers that need observability/debug assertions.
    // Only coalesce supersedable lifecycle states (succeeded/in_progress). Critical states
    // like errored/cancelled/blocked are retained so recipients don't lose important
    // transitions.
    let PendingEventDetail::Lifecycle {
        event: new_agent_event,
    } = &new_event.detail
    else {
        return vec![];
    };
    let Some(api::agent_event::Event::LifecycleEvent(new_lifecycle_event)) = &new_agent_event.event
    else {
        return vec![];
    };
    let new_type = lifecycle_event_type_from_proto(new_lifecycle_event);
    if !matches!(
        new_type,
        api::LifecycleEventType::Succeeded | api::LifecycleEventType::InProgress
    ) {
        return vec![];
    }

    let mut removed_event_ids = Vec::new();
    queue.retain(|existing| {
        let PendingEventDetail::Lifecycle {
            event: existing_agent_event,
        } = &existing.detail
        else {
            return true;
        };
        let Some(api::agent_event::Event::LifecycleEvent(existing_lifecycle_event)) =
            &existing_agent_event.event
        else {
            return true;
        };
        let existing_type = lifecycle_event_type_from_proto(existing_lifecycle_event);
        let should_remove = existing_lifecycle_event.sender_agent_id
            == new_lifecycle_event.sender_agent_id
            && matches!(
                existing_type,
                api::LifecycleEventType::Succeeded | api::LifecycleEventType::InProgress
            );
        if should_remove {
            removed_event_ids.push(existing.event_id.clone());
        }
        !should_remove
    });
    removed_event_ids
}

/// Enforce an upper bound on pending lifecycle events while preferentially dropping
/// supersedable lifecycle states first, preserving critical transitions.
fn enforce_lifecycle_queue_cap(
    queue: &mut Vec<PendingEvent>,
    max_pending_lifecycle_events: usize,
) -> Vec<String> {
    let mut dropped_event_ids = Vec::new();
    while count_pending_lifecycle_events(queue) > max_pending_lifecycle_events {
        if let Some(index) = queue
            .iter()
            .position(is_coalescable_lifecycle_pending_event)
        {
            dropped_event_ids.push(queue.remove(index).event_id);
        } else {
            // No coalescable items remain; keep critical events.
            break;
        }
    }
    dropped_event_ids
}

/// Count lifecycle entries in a mixed pending queue (message + lifecycle).
fn count_pending_lifecycle_events(queue: &[PendingEvent]) -> usize {
    queue
        .iter()
        .filter(|event| matches!(event.detail, PendingEventDetail::Lifecycle { .. }))
        .count()
}

/// Increment attempt counts and split events into retryable vs exhausted buckets.
/// Exhaustion is based on `max_retry_attempts` after incrementing this attempt.
fn increment_attempt_and_partition_by_retry_limit(
    mut attempted_events: Vec<PendingEvent>,
    max_retry_attempts: i32,
) -> (Vec<PendingEvent>, Vec<PendingEvent>) {
    for event in &mut attempted_events {
        event.attempt_count += 1;
    }
    attempted_events
        .into_iter()
        .partition(|event| event.attempt_count < max_retry_attempts)
}

impl Default for OrchestrationEventService {
    fn default() -> Self {
        Self::new_without_subscriptions()
    }
}

impl Entity for OrchestrationEventService {
    type Event = OrchestrationEventServiceEvent;
}

impl SingletonEntity for OrchestrationEventService {}

#[cfg(test)]
#[path = "orchestration_events_tests.rs"]
mod tests;
