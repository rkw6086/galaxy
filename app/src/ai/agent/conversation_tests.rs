use std::collections::HashMap;

use galaxy_agent_core::{ContentPart, ConversationMessage, MessageContent, MessageRole};
use galaxy_core::features::FeatureFlag;
use warp_multi_agent_api as api;

use super::{
    artifact_from_fork_proto, AIConversation, AIConversationAutoexecuteMode, AIConversationId,
    ConversationStatus, RestoreConversationError,
};
use crate::ai::artifacts::Artifact;
use crate::persistence::model::{
    AcpConversationData, AgentBackend, AgentConversationData, ProviderConversationContext,
};

fn restored_conversation(conversation_data: Option<AgentConversationData>) -> AIConversation {
    AIConversation::new_restored(
        AIConversationId::new(),
        vec![api::Task {
            id: "root-task".to_string(),
            messages: vec![],
            dependencies: None,
            description: String::new(),
            summary: String::new(),
            server_data: String::new(),
        }],
        conversation_data,
    )
    .unwrap()
}

fn restored_conversation_with_root_description(description: &str) -> AIConversation {
    AIConversation::new_restored(
        AIConversationId::new(),
        vec![api::Task {
            id: "root-task".to_string(),
            messages: vec![],
            dependencies: None,
            description: description.to_string(),
            summary: String::new(),
            server_data: String::new(),
        }],
        None,
    )
    .unwrap()
}

fn user_query_message(id: &str, request_id: &str, query: &str) -> api::Message {
    api::Message {
        fetched_memories: vec![],
        id: id.to_string(),
        task_id: "root-task".to_string(),
        server_message_data: String::new(),
        citations: vec![],
        message: Some(api::message::Message::UserQuery(api::message::UserQuery {
            query: query.to_string(),
            context: None,
            referenced_attachments: HashMap::new(),
            mode: None,
            intended_agent: Default::default(),
        })),
        request_id: request_id.to_string(),
        timestamp: None,
    }
}

fn agent_output_message(id: &str, request_id: &str) -> api::Message {
    api::Message {
        fetched_memories: vec![],
        id: id.to_string(),
        task_id: "root-task".to_string(),
        server_message_data: String::new(),
        citations: vec![],
        message: Some(api::message::Message::AgentOutput(
            api::message::AgentOutput {
                text: "Done".to_string(),
            },
        )),
        request_id: request_id.to_string(),
        timestamp: None,
    }
}

fn restored_conversation_with_queries(queries: &[&str]) -> AIConversation {
    let messages = queries
        .iter()
        .enumerate()
        .flat_map(|(index, query)| {
            let request_id = format!("request-{index}");
            [
                user_query_message(&format!("user-{index}"), &request_id, query),
                agent_output_message(&format!("agent-{index}"), &request_id),
            ]
        })
        .collect();

    AIConversation::new_restored(
        AIConversationId::new(),
        vec![api::Task {
            id: "root-task".to_string(),
            messages,
            dependencies: None,
            description: String::new(),
            summary: String::new(),
            server_data: String::new(),
        }],
        None,
    )
    .unwrap()
}

#[test]
fn agent_backend_does_not_change_after_successful_output() {
    let mut conversation = restored_conversation_with_queries(&["Review this repository"]);

    assert!(!conversation
        .set_agent_backend_if_no_output(AgentBackend::Acp(AcpConversationData::default())));
    assert_eq!(conversation.agent_backend(), &AgentBackend::Provider);
}

#[test]
fn latest_user_query_returns_latest_non_empty_user_query() {
    let conversation =
        restored_conversation_with_queries(&["write unit tests", "fix the failing test"]);

    assert_eq!(
        conversation.latest_user_query(),
        Some("fix the failing test".to_string())
    );
}

#[test]
fn latest_user_query_trims_and_skips_empty_queries() {
    let conversation = restored_conversation_with_queries(&["  write unit tests  ", "  "]);

    assert_eq!(
        conversation.latest_user_query(),
        Some("write unit tests".to_string())
    );
}

#[test]
fn title_uses_root_task_description() {
    let conversation = restored_conversation_with_root_description("Root task title");

    assert_eq!(conversation.title().as_deref(), Some("Root task title"));
}

#[test]
fn title_falls_back_to_initial_query_when_root_description_is_empty() {
    let conversation = restored_conversation_with_queries(&["Initial query"]);

    assert_eq!(conversation.title().as_deref(), Some("Initial query"));
}

#[test]
fn reassign_exchange_ids_keeps_exchange_lookup_consistent() {
    let mut conversation = restored_conversation_with_queries(&["one", "two"]);

    let old_ids: Vec<_> = conversation.all_exchanges().iter().map(|e| e.id).collect();
    assert!(!old_ids.is_empty());

    // Pre-condition: every original id resolves via the exchange-id index.
    for id in &old_ids {
        assert!(conversation.exchange_with_id(*id).is_some());
    }

    conversation.reassign_exchange_ids();

    // Reassigning regenerates ids without changing the exchange count, so
    // `modify_task` does not rebuild the index; correctness relies on the
    // explicit `rebuild_exchange_id_index()` call. The stale ids must be gone.
    for id in &old_ids {
        assert!(conversation.exchange_with_id(*id).is_none());
    }

    // Every current id resolves via the rebuilt index.
    let new_ids: Vec<_> = conversation.all_exchanges().iter().map(|e| e.id).collect();
    assert_eq!(new_ids.len(), old_ids.len());
    for id in &new_ids {
        assert!(conversation.exchange_with_id(*id).is_some());
    }
}

#[test]
fn restored_conversation_defaults_autoexecute_override_when_not_persisted() {
    let _flag = FeatureFlag::RememberFastForwardState.override_enabled(true);
    let conversation_data: AgentConversationData =
        serde_json::from_str(r#"{"server_conversation_token":null}"#).unwrap();

    let conversation = restored_conversation(Some(conversation_data));

    assert_eq!(
        conversation.autoexecute_override(),
        AIConversationAutoexecuteMode::RespectUserSettings
    );
}

#[test]
fn restored_conversation_retains_active_provider_run_json() {
    let snapshot = r#"{"version":1,"run":{"state":"awaiting_model"}}"#;
    let conversation_data = AgentConversationData {
        active_provider_run_json: Some(snapshot.to_string()),
        ..Default::default()
    };

    let conversation = restored_conversation(Some(conversation_data));

    assert_eq!(conversation.active_provider_run_json(), Some(snapshot));
}

#[test]
fn restored_conversation_uses_persisted_last_event_sequence() {
    let conversation_data: AgentConversationData =
        serde_json::from_str(r#"{"server_conversation_token":null,"last_event_sequence":42}"#)
            .unwrap();

    let conversation = restored_conversation(Some(conversation_data));

    assert_eq!(conversation.last_event_sequence(), Some(42));
}

#[test]
fn restored_conversation_uses_persisted_remote_child_marker() {
    let conversation_data: AgentConversationData =
        serde_json::from_str(r#"{"server_conversation_token":null,"is_remote_child":true}"#)
            .unwrap();

    let conversation = restored_conversation(Some(conversation_data));

    assert!(conversation.is_remote_child());
}

#[test]
fn new_conversations_default_to_provider_backend() {
    let conversation = AIConversation::new(false, false);

    assert_eq!(conversation.agent_backend(), &AgentBackend::Provider);
}

#[test]
fn acp_session_id_is_updated_only_for_acp_conversations() {
    let mut acp_conversation = AIConversation::new_with_agent_backend(
        false,
        false,
        AgentBackend::Acp(AcpConversationData {
            provider_id: "provider-1".to_string(),
            agent_id: "codex-acp".to_string(),
            launch_fingerprint: "launch-123".to_string(),
            session_id: None,
            config_values: Default::default(),
        }),
    );
    assert!(acp_conversation.set_acp_session_id("session-123"));
    assert_eq!(
        acp_conversation.agent_backend(),
        &AgentBackend::Acp(AcpConversationData {
            provider_id: "provider-1".to_string(),
            agent_id: "codex-acp".to_string(),
            launch_fingerprint: "launch-123".to_string(),
            session_id: Some("session-123".to_string()),
            config_values: Default::default(),
        })
    );

    let mut provider_conversation = AIConversation::new(false, false);
    assert!(!provider_conversation.set_acp_session_id("ignored"));
    assert_eq!(
        provider_conversation.agent_backend(),
        &AgentBackend::Provider
    );
}

#[test]
fn restored_conversation_uses_persisted_acp_backend() {
    let backend = AgentBackend::Acp(AcpConversationData {
        provider_id: "provider-1".to_string(),
        agent_id: "codex-acp".to_string(),
        launch_fingerprint: "launch-123".to_string(),
        session_id: Some("session-123".to_string()),
        config_values: Default::default(),
    });
    let conversation = restored_conversation(Some(AgentConversationData {
        agent_backend: backend.clone(),
        ..Default::default()
    }));

    assert_eq!(conversation.agent_backend(), &backend);
}

#[test]
fn child_conversation_detection_uses_parent_agent_id() {
    let conversation_data: AgentConversationData = serde_json::from_str(
        r#"{"server_conversation_token":null,"parent_agent_id":"parent-run-id"}"#,
    )
    .unwrap();

    let conversation = restored_conversation(Some(conversation_data));

    assert!(conversation.is_child_agent_conversation());
    assert_eq!(conversation.parent_conversation_id(), None);
}

/// When the persisted task list is empty (e.g. a child conversation persisted
/// before any server response), restoring via `new_restored_synthesizing_on_empty`
/// must produce a fresh in-progress optimistic root, mirroring
/// `AIConversation::new()`.
#[test]
fn restored_conversation_with_empty_task_list_creates_in_progress_optimistic_root() {
    let conversation =
        AIConversation::new_restored_synthesizing_on_empty(AIConversationId::new(), vec![], None)
            .expect("empty task list must synthesize an optimistic root");

    let root_task = conversation
        .get_root_task()
        .expect("synthesized root task should exist");
    assert!(root_task.is_root_task());
    assert!(
        root_task.source().is_none(),
        "synthesized root is optimistic and has no api::Task source"
    );
    assert!(
        !root_task.id().to_string().is_empty(),
        "synthesized optimistic root must have a non-empty UUID id"
    );
    assert_eq!(conversation.status(), &ConversationStatus::InProgress);
    assert!(conversation.status_error_message().is_none());
}

/// The legacy `AgentConversationData.root_task_is_optimistic` flag must be
/// ignored on restore. A non-empty task list always produces a real
/// server-backed root regardless of whether the flag is set.
#[test]
fn restored_conversation_ignores_legacy_root_task_is_optimistic_flag_with_non_empty_tasks() {
    let conversation_data: AgentConversationData = serde_json::from_str(
        r#"{"server_conversation_token":null,"root_task_is_optimistic":true}"#,
    )
    .unwrap();

    let conversation = restored_conversation(Some(conversation_data));
    let root_task = conversation
        .get_root_task()
        .expect("root task should exist");

    assert_eq!(root_task.id().to_string(), "root-task");
    assert!(root_task.is_root_task());
    assert!(
        root_task.source().is_some(),
        "with a real task list, the legacy optimistic flag must be ignored",
    );
}

/// The legacy `root_task_is_optimistic` flag is ignored when restoring an
/// empty task list via `new_restored_synthesizing_on_empty`.
#[test]
fn restored_conversation_ignores_legacy_root_task_is_optimistic_flag_with_empty_tasks() {
    let conversation_data: AgentConversationData = serde_json::from_str(
        r#"{"server_conversation_token":null,"root_task_is_optimistic":true}"#,
    )
    .unwrap();

    let conversation = AIConversation::new_restored_synthesizing_on_empty(
        AIConversationId::new(),
        vec![],
        Some(conversation_data),
    )
    .expect("empty task list must synthesize an optimistic root regardless of legacy flag");

    let root_task = conversation
        .get_root_task()
        .expect("synthesized root task should exist");
    assert!(root_task.is_root_task());
    assert!(root_task.source().is_none());
    assert_eq!(conversation.status(), &ConversationStatus::InProgress);
}

/// Strict `new_restored` returns `NoRootTask` for an empty task list.
#[test]
fn new_restored_with_empty_task_list_returns_no_root_task_error() {
    let result = AIConversation::new_restored(AIConversationId::new(), vec![], None);
    assert!(
        matches!(result, Err(RestoreConversationError::NoRootTask)),
        "empty task list via strict new_restored must return NoRootTask; got {result:?}",
    );
}

/// When multiple parentless tasks exist (e.g. a legacy orphan optimistic
/// stub alongside the real server root), `new_restored` must prefer the
/// candidate whose `messages` is non-empty. Each ordering runs in a loop to
/// surface any nondeterminism in candidate selection.
#[test]
fn test_new_restored_prefers_parentless_task_with_messages_over_empty_stub() {
    let stub = api::Task {
        id: "optimistic-stub-uuid".to_string(),
        messages: vec![],
        dependencies: None,
        description: String::new(),
        summary: String::new(),
        server_data: String::new(),
    };
    let real = api::Task {
        id: "server-root-id".to_string(),
        messages: vec![user_query_message("user-msg", "request-1", "real query")],
        dependencies: None,
        description: String::new(),
        summary: String::new(),
        server_data: String::new(),
    };

    // Stub appears first in the vec.
    for _ in 0..50 {
        let conversation = AIConversation::new_restored(
            AIConversationId::new(),
            vec![stub.clone(), real.clone()],
            None,
        )
        .expect("restore with stub + real parentless tasks must succeed");
        let root_task = conversation
            .get_root_task()
            .expect("restored conversation must have a root task");
        assert_eq!(
            root_task.id().to_string(),
            "server-root-id",
            "expected the real (non-empty) parentless task to win when stub is first",
        );
        let source = root_task
            .source()
            .expect("chosen root must have api::Task source");
        assert!(
            !source.messages.is_empty(),
            "chosen root must have non-empty messages",
        );
    }

    // Real appears first in the vec.
    for _ in 0..50 {
        let conversation = AIConversation::new_restored(
            AIConversationId::new(),
            vec![real.clone(), stub.clone()],
            None,
        )
        .expect("restore with real + stub parentless tasks must succeed");
        let root_task = conversation
            .get_root_task()
            .expect("restored conversation must have a root task");
        assert_eq!(
            root_task.id().to_string(),
            "server-root-id",
            "expected the real (non-empty) parentless task to win when real is first",
        );
        let source = root_task
            .source()
            .expect("chosen root must have api::Task source");
        assert!(
            !source.messages.is_empty(),
            "chosen root must have non-empty messages",
        );
    }
}

#[test]
fn cli_agent_transcript_vehicle_is_excluded_from_navigation() {
    let conversation = AIConversation::new(false, true);

    assert!(conversation.should_exclude_from_navigation());
}

#[test]
fn restored_conversation_defaults_unknown_persisted_autoexecute_override() {
    let _flag = FeatureFlag::RememberFastForwardState.override_enabled(true);
    let conversation_data: AgentConversationData = serde_json::from_str(
        r#"{"server_conversation_token":null,"autoexecute_override":"UnexpectedValue"}"#,
    )
    .unwrap();

    let conversation = restored_conversation(Some(conversation_data));

    assert_eq!(
        conversation.autoexecute_override(),
        AIConversationAutoexecuteMode::RespectUserSettings
    );
}

#[test]
fn restored_conversation_uses_persisted_autoexecute_override_when_enabled() {
    let _flag = FeatureFlag::RememberFastForwardState.override_enabled(true);
    let conversation_data: AgentConversationData = serde_json::from_str(
        r#"{"server_conversation_token":null,"autoexecute_override":"RunToCompletion"}"#,
    )
    .unwrap();

    let conversation = restored_conversation(Some(conversation_data));

    assert_eq!(
        conversation.autoexecute_override(),
        AIConversationAutoexecuteMode::RunToCompletion
    );
}

#[test]
fn restored_conversation_ignores_persisted_autoexecute_override_when_disabled() {
    let _flag = FeatureFlag::RememberFastForwardState.override_enabled(false);
    let conversation_data: AgentConversationData = serde_json::from_str(
        r#"{"server_conversation_token":null,"autoexecute_override":"RunToCompletion"}"#,
    )
    .unwrap();

    let conversation = restored_conversation(Some(conversation_data));

    assert_eq!(
        conversation.autoexecute_override(),
        AIConversationAutoexecuteMode::RespectUserSettings
    );
}

#[test]
fn fork_artifacts_adds_file_artifacts_to_conversation() {
    let proto_artifact = api::message::artifact_event::ConversationArtifact {
        artifact: Some(
            api::message::artifact_event::conversation_artifact::Artifact::File(
                api::message::artifact_event::FileArtifact {
                    artifact_uid: "artifact-file-1".to_string(),
                    filepath: "outputs/report.txt".to_string(),
                    mime_type: "text/plain".to_string(),
                    size_bytes: 42,
                    description: "Daily summary".to_string(),
                },
            ),
        ),
    };

    assert_eq!(
        artifact_from_fork_proto(&proto_artifact),
        Some(Artifact::File {
            artifact_uid: "artifact-file-1".to_string(),
            filepath: "outputs/report.txt".to_string(),
            filename: "report.txt".to_string(),
            mime_type: "text/plain".to_string(),
            description: Some("Daily summary".to_string()),
            size_bytes: Some(42),
        })
    );
}

#[test]
fn waiting_for_events_display_label_is_waiting() {
    assert_eq!(
        format!("{}", ConversationStatus::WaitingForEvents),
        "Waiting"
    );
}

/// `is_done` returns true only for `Success | Error | Cancelled`;
/// `WaitingForEvents` and `Blocked` are not done because the run can still
/// resume on its own.
#[test]
fn is_done_only_includes_success_error_cancelled() {
    assert!(ConversationStatus::Success.is_done());
    assert!(ConversationStatus::Error.is_done());
    assert!(ConversationStatus::Cancelled.is_done());

    assert!(!ConversationStatus::InProgress.is_done());
    assert!(!ConversationStatus::Blocked {
        blocked_action: "approve".to_string()
    }
    .is_done());
    assert!(!ConversationStatus::WaitingForEvents.is_done());
}

/// `is_waiting_for_events` is true only for the new variant.
#[test]
fn is_waiting_for_events_returns_true_only_for_waiting_for_events_variant() {
    assert!(ConversationStatus::WaitingForEvents.is_waiting_for_events());

    assert!(!ConversationStatus::InProgress.is_waiting_for_events());
    assert!(!ConversationStatus::Success.is_waiting_for_events());
    assert!(!ConversationStatus::Error.is_waiting_for_events());
    assert!(!ConversationStatus::Cancelled.is_waiting_for_events());
    assert!(!ConversationStatus::Blocked {
        blocked_action: "approve".to_string()
    }
    .is_waiting_for_events());
}

/// A conversation that was yielded via `wait_for_events` at shutdown
/// restores as whatever `derive_status_from_root_task` returns (Success
/// for a cleanly-streamed last exchange). The unresolved tool call stays
/// in the transcript as an orphan; the next outbound request triggers
/// the server's existing supersede mechanism to synthesize the matching
/// `Cancel`. The waiting state itself is not durable across restart.
#[test]
fn restored_conversation_does_not_re_enter_waiting_for_events() {
    let conversation_data: AgentConversationData =
        serde_json::from_str(r#"{"server_conversation_token":null}"#).unwrap();

    let conversation = restored_conversation(Some(conversation_data));

    assert_eq!(conversation.status(), &ConversationStatus::Success);
}

fn fetched_memory(
    memory_id: &str,
    content: &str,
    memory_store_id: &str,
    source: Option<api::message::fetched_memory::Source>,
) -> api::message::FetchedMemory {
    api::message::FetchedMemory {
        memory_id: memory_id.to_string(),
        content: content.to_string(),
        memory_store_id: memory_store_id.to_string(),
        source,
    }
}

fn conversation_source(conversation_id: &str) -> Option<api::message::fetched_memory::Source> {
    Some(api::message::fetched_memory::Source::Conversation(
        api::message::fetched_memory::Conversation {
            conversation_id: conversation_id.to_string(),
        },
    ))
}

fn restored_conversation_with_memories_per_query(
    memories_per_query: Vec<Vec<api::message::FetchedMemory>>,
) -> AIConversation {
    let messages = memories_per_query
        .into_iter()
        .enumerate()
        .flat_map(|(index, memories)| {
            let request_id = format!("request-{index}");
            let query = api::Message {
                fetched_memories: memories,
                ..user_query_message(&format!("user-{index}"), &request_id, "query")
            };
            [
                query,
                agent_output_message(&format!("agent-{index}"), &request_id),
            ]
        })
        .collect();

    AIConversation::new_restored(
        AIConversationId::new(),
        vec![api::Task {
            id: "root-task".to_string(),
            messages,
            ..Default::default()
        }],
        None,
    )
    .unwrap()
}

#[test]
fn fetched_memories_is_empty_when_no_message_has_memories() {
    let conversation = restored_conversation_with_memories_per_query(vec![vec![]]);

    assert_eq!(conversation.fetched_memories(), vec![]);
}

#[test]
fn fetched_memories_preserves_order_across_and_within_messages() {
    let conversation = restored_conversation_with_memories_per_query(vec![
        vec![
            fetched_memory("m1", "first", "store-1", None),
            fetched_memory("m2", "second", "store-1", None),
        ],
        vec![fetched_memory("m3", "third", "store-2", None)],
    ]);

    let ids: Vec<String> = conversation
        .fetched_memories()
        .into_iter()
        .map(|memory| memory.memory_id)
        .collect();
    assert_eq!(ids, vec!["m1", "m2", "m3"]);
}

#[test]
fn fetched_memories_dedupes_keeping_first_position_and_latest_data() {
    let conversation = restored_conversation_with_memories_per_query(vec![
        vec![
            fetched_memory("m1", "old content", "store-1", None),
            fetched_memory("m2", "other", "store-1", None),
        ],
        vec![
            fetched_memory(
                "m1",
                "new content",
                "store-1",
                conversation_source("conversation-1"),
            ),
            fetched_memory("m1", "same memory id different store", "store-2", None),
        ],
    ]);

    let memories = conversation.fetched_memories();
    assert_eq!(
        memories,
        vec![
            fetched_memory(
                "m1",
                "new content",
                "store-1",
                conversation_source("conversation-1"),
            ),
            fetched_memory("m2", "other", "store-1", None),
            fetched_memory("m1", "same memory id different store", "store-2", None),
        ]
    );
}

#[test]
fn restored_provider_context_preserves_compacted_history_and_tool_archive() {
    let context = ProviderConversationContext {
        messages: vec![ConversationMessage {
            role: MessageRole::Assistant,
            content: MessageContent::MultiPart(vec![
                ContentPart::Reasoning {
                    text: "retained reasoning".into(),
                    signature: Some("signature".into()),
                },
                ContentPart::Text("retained response".into()),
            ]),
        }],
        tool_result_archive: vec![ConversationMessage {
            role: MessageRole::User,
            content: MessageContent::ToolResult {
                tool_use_id: "old-tool".into(),
                content: "original output".into(),
                is_error: false,
            },
        }],
    };
    let data = AgentConversationData {
        provider_context: Some(context.clone()),
        progressive_summary: Some("Older history".into()),
        ..Default::default()
    };
    let data = serde_json::from_str(&serde_json::to_string(&data).unwrap()).unwrap();
    let restored = restored_conversation(Some(data));
    assert_eq!(restored.provider_context(), context);
    assert_eq!(restored.progressive_summary(), Some("Older history"));
    assert!(restored.active_provider_run_json().is_none());
}

#[test]
fn explicit_empty_provider_context_does_not_reconstruct_old_task_messages() {
    let data = AgentConversationData {
        provider_context: Some(ProviderConversationContext {
            messages: Vec::new(),
            tool_result_archive: Vec::new(),
        }),
        ..Default::default()
    };
    let task = api::Task {
        id: "root-task".into(),
        messages: vec![user_query_message("query", "request", "old query")],
        dependencies: None,
        description: String::new(),
        summary: String::new(),
        server_data: String::new(),
    };
    let legacy =
        AIConversation::new_restored(AIConversationId::new(), vec![task.clone()], None).unwrap();
    assert!(!legacy.bedrock_message_history().is_empty());
    let restored =
        AIConversation::new_restored(AIConversationId::new(), vec![task], Some(data)).unwrap();
    assert!(restored.bedrock_message_history().is_empty());
}
