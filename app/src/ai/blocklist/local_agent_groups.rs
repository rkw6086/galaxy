//! Local delegation membership survives parent turns; only complete groups wake the parent.
use galaxyui::AppContext;
use persistence::model::LocalAgentGroup;

use super::*;

fn group_event_id(group: &LocalAgentGroup) -> String {
    format!("local-group/{}/{}", group.id, group.revision)
}

fn terminal_child(status: &ConversationStatus) -> bool {
    matches!(
        status,
        ConversationStatus::Success | ConversationStatus::Error | ConversationStatus::Cancelled
    )
}

impl OrchestrationEventService {
    pub(crate) fn has_waiting_local_groups(
        &self,
        parent: AIConversationId,
        ctx: &AppContext,
    ) -> bool {
        BlocklistAIHistoryModel::as_ref(ctx)
            .conversation(&parent)
            .is_some_and(|conversation| {
                conversation
                    .local_agent_groups()
                    .iter()
                    .any(|group| !group.notified && !group.children.is_empty())
            })
    }

    pub(crate) fn begin_local_group(
        &mut self,
        parent: AIConversationId,
        id: &str,
        ctx: &mut ModelContext<Self>,
    ) {
        self.update_local_group(parent, id, ctx, |_| {});
    }

    pub(crate) fn register_local_group_child(
        &mut self,
        parent: AIConversationId,
        id: &str,
        child: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        self.update_local_group(parent, id, ctx, |group| {
            let child = child.to_string();
            if !group.children.contains(&child) {
                group.children.push(child);
            }
        });
    }

    pub(crate) fn seal_local_group(
        &mut self,
        parent: AIConversationId,
        id: &str,
        errors: Vec<String>,
        ctx: &mut ModelContext<Self>,
    ) {
        self.update_local_group(parent, id, ctx, |group| {
            group.sealed = true;
            group.launch_errors.extend(errors);
        });
        self.reconcile_local_groups(parent, ctx);
    }

    fn update_local_group(
        &mut self,
        parent: AIConversationId,
        id: &str,
        ctx: &mut ModelContext<Self>,
        edit: impl FnOnce(&mut LocalAgentGroup),
    ) {
        let Some(conversation) = BlocklistAIHistoryModel::as_ref(ctx).conversation(&parent) else {
            return;
        };
        let mut groups = conversation.local_agent_groups().to_vec();
        if !groups.iter().any(|group| group.id == id) {
            groups.push(LocalAgentGroup {
                id: id.to_string(),
                ..Default::default()
            });
        }
        edit(
            groups
                .iter_mut()
                .find(|group| group.id == id)
                .expect("group was inserted"),
        );
        BlocklistAIHistoryModel::handle(ctx).update(ctx, |history, ctx| {
            history.set_local_agent_groups(parent, groups, ctx)
        });
    }

    pub(super) fn confirm_group_messages(
        &mut self,
        parent: AIConversationId,
        event_ids: &[String],
        ctx: &mut ModelContext<Self>,
    ) {
        let Some(conversation) = BlocklistAIHistoryModel::as_ref(ctx).conversation(&parent) else {
            return;
        };
        let mut groups = conversation.local_agent_groups().to_vec();
        let mut changed = false;
        for group in &mut groups {
            if event_ids.contains(&group_event_id(group)) && !group.notified {
                group.notified = true;
                changed = true;
            }
        }
        if changed {
            BlocklistAIHistoryModel::handle(ctx).update(ctx, |history, ctx| {
                history.set_local_agent_groups(parent, groups, ctx)
            });
        }
    }

    /// Re-evaluate current statuses, rather than counting completion callbacks. A restarted
    /// child becomes pending again and invalidates any undelivered earlier group result.
    pub(crate) fn reconcile_local_groups_for_child(
        &mut self,
        child: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) -> bool {
        let parents = BlocklistAIHistoryModel::as_ref(ctx)
            .all_live_conversations()
            .iter()
            .filter_map(|(_, conversation)| {
                conversation
                    .local_agent_groups()
                    .iter()
                    .any(|group| group.children.contains(&child.to_string()))
                    .then_some(conversation.id())
            })
            .collect::<Vec<_>>();
        let grouped = !parents.is_empty();
        for parent in parents {
            self.reconcile_local_groups(parent, ctx);
        }
        grouped
    }

    pub(super) fn reconcile_local_groups(
        &mut self,
        parent: AIConversationId,
        ctx: &mut ModelContext<Self>,
    ) {
        let history = BlocklistAIHistoryModel::as_ref(ctx);
        let Some(conversation) = history.conversation(&parent) else {
            return;
        };
        let mut groups = conversation.local_agent_groups().to_vec();
        let original = groups.clone();
        let mut ready = Vec::new();
        let mut stale = Vec::new();
        for group in &mut groups {
            if group.children.is_empty() {
                continue;
            }
            let children = group
                .children
                .iter()
                .map(|id| {
                    AIConversationId::try_from(id.clone())
                        .ok()
                        .and_then(|id| history.conversation(&id))
                        .filter(|child| {
                            child.parent_conversation_id() == Some(parent)
                                && !child.is_remote_child()
                        })
                })
                .collect::<Vec<_>>();
            let finished = children.iter().all(|child| {
                child.is_none_or(|child| {
                    terminal_child(child.status()) && child.active_provider_run_json().is_none()
                })
            });
            let event_id = group_event_id(group);
            let queued = self
                .pending_events
                .get(&parent)
                .into_iter()
                .chain(self.awaiting_server_echo_events.get(&parent))
                .flatten()
                .any(|event| event.event_id == event_id);
            if !finished {
                if group.notified || queued {
                    stale.push(event_id);
                    group.revision = group.revision.saturating_add(1);
                    group.notified = false;
                }
                continue;
            }
            if !group.sealed || group.notified || queued {
                continue;
            }
            let mut result = format!("All children in delegation group {} have finished. Process their results together now. Failed or cancelled children are explicit blockers, not successful results. Use a corrective follow-up to the same child for recoverable errors; do not override cancellation.\n", group.id);
            for (id, child) in group.children.iter().zip(children) {
                let Some(child) = child else {
                    result.push_str(&format!("\nChild {id}: removed or unavailable.\n"));
                    continue;
                };
                result.push_str(&format!(
                    "\n{} ({id}): {:?}\n",
                    child.agent_name().unwrap_or("Child"),
                    child.status()
                ));
                if matches!(child.status(), ConversationStatus::Success) {
                    let output = child
                        .all_exchanges()
                        .into_iter()
                        .rev()
                        .filter_map(|exchange| exchange.output_status.output())
                        .map(|output| output.get().format_for_copy(None))
                        .find(|output| !output.is_empty())
                        .unwrap_or_default();
                    // Bound the whole group, including large fan-outs, to roughly 32 KB.
                    let limit = 32_000 / group.children.len().max(1);
                    let mut end = output.len().min(limit);
                    while !output.is_char_boundary(end) {
                        end -= 1;
                    }
                    result.push_str(&output[..end]);
                    if end < output.len() {
                        result.push_str("\n[Result excerpt truncated; inspect the child conversation for more.]");
                    }
                } else if let Some(error) = child.status_error_message() {
                    result.push_str(&error);
                }
            }
            for error in &group.launch_errors {
                result.push_str(&format!("\nLaunch failure: {error}\n"));
            }
            if result.len() > 48_000 {
                let mut end = 48_000;
                while !result.is_char_boundary(end) {
                    end -= 1;
                }
                result.truncate(end);
                result.push_str(
                    "\n[Group result truncated; inspect the child conversations for full details.]",
                );
            }
            ready.push(PendingEvent {
                event_id: event_id.clone(),
                source_agent_id: parent.to_string(),
                attempt_count: 0,
                detail: PendingEventDetail::LocalMessage {
                    message_id: event_id,
                    subject: "Child agent group completed".to_string(),
                    message_body: result,
                },
            });
        }
        for events in [
            self.pending_events.get_mut(&parent),
            self.awaiting_server_echo_events.get_mut(&parent),
        ]
        .into_iter()
        .flatten()
        {
            events.retain(|event| !stale.contains(&event.event_id));
        }
        if groups != original {
            BlocklistAIHistoryModel::handle(ctx).update(ctx, |history, ctx| {
                history.set_local_agent_groups(parent, groups, ctx)
            });
        }
        if !ready.is_empty() {
            self.enqueue_event_batch(parent, ready, ctx);
        }
    }
}
