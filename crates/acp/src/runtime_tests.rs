use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Duration;

use agent_client_protocol::schema::v1::{
    AuthMethod, AuthMethodAgent, AuthMethodId, ContentBlock, ContentChunk, ImageContent,
    InitializeResponse, McpServer, McpServerStdio, PromptCapabilities, SessionConfigOption,
    SessionConfigOptionValue, SessionConfigSelectOption, SessionId, SessionUpdate, TextContent,
    ToolCall, ToolCallStatus, ToolCallUpdate, ToolCallUpdateFields, UsageUpdate,
};
use agent_client_protocol::schema::ProtocolVersion;

use super::*;

fn pending_turn(turn_id: u64, cwd: &str, session_id: Option<&str>) -> PendingTurn {
    let mut request = AcpTurnRequest::text("conversation", PathBuf::from(cwd), "hello");
    request.session_id = session_id.map(SessionId::new);
    let (events, _receiver) = async_channel::unbounded();
    PendingTurn {
        turn_id,
        request,
        events,
    }
}

fn advertised_auth_method(id: &'static str) -> AuthMethod {
    AuthMethod::Agent(AuthMethodAgent::new(id, id))
}

#[test]
fn authentication_is_skipped_when_agent_advertises_no_methods() {
    let request = authentication_request(&[], None).unwrap();

    assert_eq!(request, None);
}

#[test]
fn authentication_uses_first_advertised_method_by_default() {
    let methods = [
        advertised_auth_method("recommended"),
        advertised_auth_method("alternative"),
    ];
    let request = authentication_request(&methods, None).unwrap().unwrap();

    assert_eq!(request.method_id, AuthMethodId::new("recommended"));
}

#[test]
fn authentication_uses_explicit_preference_instead_of_advertised_order() {
    let methods = [
        advertised_auth_method("api-key"),
        advertised_auth_method("chat-gpt"),
    ];
    let request = authentication_request(&methods, Some(&AuthMethodId::new("chat-gpt")))
        .unwrap()
        .unwrap();

    assert_eq!(request.method_id, AuthMethodId::new("chat-gpt"));
}

#[test]
fn authentication_rejects_preference_that_agent_did_not_advertise() {
    let methods = [
        advertised_auth_method("api-key"),
        advertised_auth_method("chat-gpt"),
    ];
    let error = authentication_request(&methods, Some(&AuthMethodId::new("missing"))).unwrap_err();

    assert!(error
        .to_string()
        .contains("preferred authentication method"));
    assert!(error.to_string().contains("api-key, chat-gpt"));
}

#[test]
fn turn_validation_requires_absolute_paths() {
    let request = AcpTurnRequest::text("conversation", "relative", "hello");

    assert!(matches!(
        request.validate(),
        Err(AcpRuntimeError::InvalidTurn(_))
    ));
}

#[test]
fn turn_validation_rejects_relative_mcp_commands() {
    let request = AcpTurnRequest::text("conversation", PathBuf::from("/workspace"), "hello")
        .mcp_servers(vec![McpServer::Stdio(McpServerStdio::new(
            "server",
            "relative-command",
        ))]);

    assert!(matches!(
        request.validate(),
        Err(AcpRuntimeError::InvalidTurn(_))
    ));
}

#[test]
fn prompt_media_uses_capabilities_advertised_by_the_connected_agent() {
    let prompt = vec![ContentBlock::Image(ImageContent::new(
        "aW1hZ2U=",
        "image/png",
    ))];

    let error = validate_prompt_capabilities(&prompt, &PromptCapabilities::default())
        .expect_err("image capability is required");
    assert!(error.to_string().contains("image prompt support"));

    validate_prompt_capabilities(&prompt, &PromptCapabilities::new().image(true))
        .expect("advertised image support");
}

#[test]
fn persisted_session_restore_is_rejected_when_not_advertised() {
    let requested = SessionId::new("persisted");

    assert_eq!(
        restorable_session_id(Some(&requested), true),
        Ok(Some(requested.clone()))
    );
    let error = restorable_session_id(Some(&requested), false).unwrap_err();
    assert!(error
        .to_string()
        .contains("does not advertise session/load"));
    assert_eq!(restorable_session_id(None, false), Ok(None));
}

#[test]
fn successful_session_load_does_not_create_a_replacement() {
    let load_calls = Arc::new(AtomicUsize::new(0));
    let create_calls = Arc::new(AtomicUsize::new(0));
    let load_count = Arc::clone(&load_calls);
    let create_count = Arc::clone(&create_calls);

    let result = futures::executor::block_on(open_session(
        Some(SessionId::new("persisted")),
        move |_| {
            load_count.fetch_add(1, Ordering::Relaxed);
            futures::future::ready(Ok(()))
        },
        move || {
            create_count.fetch_add(1, Ordering::Relaxed);
            futures::future::ready(Ok(SessionId::new("replacement")))
        },
    ))
    .unwrap();

    assert_eq!(result, SessionId::new("persisted"));
    assert_eq!(load_calls.load(Ordering::Relaxed), 1);
    assert_eq!(create_calls.load(Ordering::Relaxed), 0);
}

#[test]
fn stale_config_values_are_dropped_before_session_application() {
    let options = vec![SessionConfigOption::select(
        "model",
        "Model",
        "fast",
        vec![SessionConfigSelectOption::new("fast", "Fast")],
    )];
    let values = std::collections::BTreeMap::from([
        (
            "model".to_owned(),
            SessionConfigOptionValue::value_id("missing"),
        ),
        (
            "unknown".to_owned(),
            SessionConfigOptionValue::value_id("value"),
        ),
    ]);

    assert!(valid_config_values(&options, &values).is_empty());
}

#[test]
fn advertised_config_values_are_retained() {
    let options = vec![SessionConfigOption::select(
        "model",
        "Model",
        "fast",
        vec![SessionConfigSelectOption::new("fast", "Fast")],
    )];
    let values = std::collections::BTreeMap::from([(
        "model".to_owned(),
        SessionConfigOptionValue::value_id("fast"),
    )]);

    assert_eq!(valid_config_values(&options, &values), values);
}

#[test]
fn failed_session_load_is_visible_and_does_not_create_a_replacement() {
    let load_calls = Arc::new(AtomicUsize::new(0));
    let create_calls = Arc::new(AtomicUsize::new(0));
    let load_count = Arc::clone(&load_calls);
    let create_count = Arc::clone(&create_calls);

    let error = futures::executor::block_on(open_session(
        Some(SessionId::new("expired")),
        move |_| {
            load_count.fetch_add(1, Ordering::Relaxed);
            futures::future::ready(Err(agent_client_protocol::Error::new(
                -32000,
                "unknown session",
            )))
        },
        move || {
            create_count.fetch_add(1, Ordering::Relaxed);
            futures::future::ready(Ok(SessionId::new("fresh")))
        },
    ))
    .unwrap_err();

    assert!(error.to_string().contains("unknown session"));
    assert_eq!(load_calls.load(Ordering::Relaxed), 1);
    assert_eq!(create_calls.load(Ordering::Relaxed), 0);
}

#[test]
fn new_session_failure_is_returned_when_no_persisted_session_exists() {
    let error = futures::executor::block_on(open_session(
        None,
        |_| futures::future::ready(Ok(())),
        || {
            futures::future::ready(Err(agent_client_protocol::Error::new(
                -32001,
                "new session failed",
            )))
        },
    ))
    .unwrap_err();

    assert!(error.to_string().contains("new session failed"));
}

#[test]
fn steering_support_uses_codex_extension_metadata() {
    let mut steering = serde_json::Map::new();
    steering.insert("supported".to_owned(), serde_json::Value::Bool(true));
    let mut meta = serde_json::Map::new();
    meta.insert("steering".to_owned(), serde_json::Value::Object(steering));
    let response = InitializeResponse::new(ProtocolVersion::V1).meta(meta);

    assert!(supports_steering(&response));
    assert!(!supports_steering(&InitializeResponse::new(
        ProtocolVersion::V1
    )));
}

#[test]
fn streamed_text_and_thoughts_are_visible_events() {
    let text = event_from_session_update(SessionUpdate::AgentMessageChunk(ContentChunk::new(
        ContentBlock::Text(TextContent::new("answer")),
    )));
    let thought = event_from_session_update(SessionUpdate::AgentThoughtChunk(ContentChunk::new(
        ContentBlock::Text(TextContent::new("reasoning")),
    )));

    assert_eq!(
        text,
        Some(AcpEvent::AgentText {
            text: "answer".to_owned()
        })
    );
    assert_eq!(
        thought,
        Some(AcpEvent::AgentThought {
            text: "reasoning".to_owned()
        })
    );
}

#[test]
fn streamed_user_content_is_a_visible_steering_event() {
    let content = ContentBlock::Text(TextContent::new("stop after this step"));
    let event = event_from_session_update(SessionUpdate::UserMessageChunk(ContentChunk::new(
        content.clone(),
    )));

    assert_eq!(event, Some(AcpEvent::UserContent { content }));
}

#[test]
fn tool_and_usage_updates_are_visible_events() {
    let tool = event_from_session_update(SessionUpdate::ToolCall(
        ToolCall::new("tool-1", "Run tests").status(ToolCallStatus::InProgress),
    ));
    let update = event_from_session_update(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
        "tool-1",
        ToolCallUpdateFields::new()
            .title("Tests passed")
            .status(ToolCallStatus::Completed),
    )));
    let usage =
        event_from_session_update(SessionUpdate::UsageUpdate(UsageUpdate::new(400, 200_000)));

    assert_eq!(
        tool,
        Some(AcpEvent::ToolCall {
            id: "tool-1".into(),
            title: "Run tests".to_owned(),
            status: ToolCallStatus::InProgress,
            output: None,
        })
    );
    assert_eq!(
        update,
        Some(AcpEvent::ToolCallUpdate {
            id: "tool-1".into(),
            title: Some("Tests passed".to_owned()),
            status: Some(ToolCallStatus::Completed),
            output: None,
        })
    );
    assert_eq!(
        usage,
        Some(AcpEvent::Usage {
            used: 400,
            size: 200_000,
            cost: None,
        })
    );
}

#[test]
fn terminal_command_from_raw_input_becomes_the_visible_title() {
    let tool = event_from_session_update(SessionUpdate::ToolCall(
        ToolCall::new("tool-1", "shell")
            .raw_input(serde_json::json!({ "cmd": "git status --short" })),
    ));

    assert_eq!(
        tool,
        Some(AcpEvent::ToolCall {
            id: "tool-1".into(),
            title: "git status --short".to_owned(),
            status: ToolCallStatus::Pending,
            output: None,
        })
    );
}

#[test]
fn tool_content_is_sanitized_before_becoming_visible_output() {
    let tool = event_from_session_update(SessionUpdate::ToolCall(
        ToolCall::new("tool-1", "Run tests").content(vec![ToolCallContent::from(
            ContentBlock::Text(TextContent::new(
                "\u{1b}[31m42 tests passed\u{1b}[0m\0\u{202e}",
            )),
        )]),
    ));

    assert_eq!(
        tool,
        Some(AcpEvent::ToolCall {
            id: "tool-1".into(),
            title: "Run tests".to_owned(),
            status: ToolCallStatus::Pending,
            output: Some("42 tests passed".to_owned()),
        })
    );
}

#[test]
fn raw_tool_output_uses_display_text_and_agent_truncation_metadata() {
    let update = event_from_session_update(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
        "tool-1",
        ToolCallUpdateFields::new().raw_output(serde_json::json!({
            "output": "first lines",
            "metadata": {
                "truncated": true
            }
        })),
    )));

    assert_eq!(
        update,
        Some(AcpEvent::ToolCallUpdate {
            id: "tool-1".into(),
            title: None,
            status: None,
            output: Some("first lines\n[output truncated by ACP agent]".to_owned()),
        })
    );
}

#[test]
fn terminal_output_extension_metadata_becomes_visible_output() {
    let meta: Meta = serde_json::from_value(serde_json::json!({
        "terminal_output": {
            "data": "\u{1b}[32mApplying migrations\u{1b}[0m\n",
            "terminal_id": "terminal-1"
        }
    }))
    .unwrap();
    let update = event_from_session_update(SessionUpdate::ToolCallUpdate(
        ToolCallUpdate::new("tool-1", ToolCallUpdateFields::new()).meta(meta),
    ));

    assert_eq!(
        update,
        Some(AcpEvent::ToolCallUpdate {
            id: "tool-1".into(),
            title: None,
            status: None,
            output: Some("Applying migrations\n".to_owned()),
        })
    );
}

#[test]
fn terminal_exit_metadata_avoids_replaying_aggregated_raw_output() {
    let meta: Meta = serde_json::from_value(serde_json::json!({
        "terminal_exit": {
            "exit_code": 0,
            "signal": null,
            "terminal_id": "terminal-1"
        }
    }))
    .unwrap();
    let update = event_from_session_update(SessionUpdate::ToolCallUpdate(
        ToolCallUpdate::new(
            "tool-1",
            ToolCallUpdateFields::new()
                .status(ToolCallStatus::Completed)
                .raw_output(serde_json::json!({
                    "formatted_output": "already streamed",
                    "exit_code": 0
                })),
        )
        .meta(meta),
    ));

    assert_eq!(
        update,
        Some(AcpEvent::ToolCallUpdate {
            id: "tool-1".into(),
            title: None,
            status: Some(ToolCallStatus::Completed),
            output: Some("[terminal exited: code 0]".to_owned()),
        })
    );
}

#[test]
fn visible_tool_output_is_utf8_safe_and_bounded() {
    let long_output = "🚀".repeat(MAX_VISIBLE_TOOL_OUTPUT_BYTES);
    let update = event_from_session_update(SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
        "tool-1",
        ToolCallUpdateFields::new().content(vec![ToolCallContent::from(ContentBlock::Text(
            TextContent::new(long_output),
        ))]),
    )));
    let Some(AcpEvent::ToolCallUpdate {
        output: Some(output),
        ..
    }) = update
    else {
        panic!("expected a visible tool-call update");
    };

    assert!(output.is_char_boundary(output.len()));
    assert!(output.len() <= MAX_VISIBLE_TOOL_OUTPUT_BYTES);
    assert!(output.ends_with(TOOL_OUTPUT_TRUNCATION_MARKER));
}

#[test]
fn load_replay_is_suppressed_until_the_session_is_ready() {
    let router = EventRouter::default();
    let session_id = SessionId::new("persisted");
    let (events, receiver) = async_channel::unbounded();
    router.set_route(
        session_id.clone(),
        EventRoute {
            turn_id: 1,
            events,
            auto_approve: false,
            permission_policy: AcpPermissionPolicy::default(),
        },
    );
    router.suppress_replay(session_id.clone());

    router.on_session_notification(SessionNotification::new(
        session_id.clone(),
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
            "old answer",
        )))),
    ));
    assert!(matches!(
        receiver.try_recv(),
        Err(async_channel::TryRecvError::Empty)
    ));

    router.finish_replay(&session_id);
    router.on_session_notification(SessionNotification::new(
        session_id,
        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(TextContent::new(
            "new answer",
        )))),
    ));
    assert_eq!(
        receiver.try_recv(),
        Ok(AcpEvent::AgentText {
            text: "new answer".to_owned(),
        })
    );
}

#[test]
fn queued_spec_changes_rotate_sessions_without_losing_fifo_order() {
    let first = pending_turn(1, "/workspace/first", None);
    let mut state = ConversationState::new(first);
    state.ready = true;
    state.session_id = Some(SessionId::new("old-session"));

    let mut second = pending_turn(2, "/workspace/second", Some("old-session"));
    second.request.mcp_servers = vec![McpServer::Stdio(McpServerStdio::new(
        "galaxy",
        "/usr/bin/galaxy",
    ))];
    let mut third = pending_turn(3, "/workspace/second", Some("old-session"));
    third.request.mcp_servers = second.request.mcp_servers.clone();
    state.queued.push_back(second);
    state.queued.push_back(third);

    state.active.take();
    assert!(state.activate_next());
    assert_eq!(state.session_id, None);
    assert!(!state.ready);
    let second = state.active.as_ref().unwrap();
    assert_eq!(second.pending.turn_id, 2);
    assert_eq!(second.pending.request.session_id, None);
    assert_eq!(second.phase, TurnPhase::Opening);

    state.ready = true;
    state.session_id = Some(SessionId::new("new-session"));
    state.active.take();
    assert!(state.activate_next());
    assert_eq!(state.session_id, Some(SessionId::new("new-session")));
    let third = state.active.as_ref().unwrap();
    assert_eq!(third.pending.turn_id, 3);
    assert_eq!(third.phase, TurnPhase::Prompting);
    assert!(state.queued.is_empty());
}

#[test]
fn terminal_actor_errors_are_visible_to_active_and_queued_turns() {
    let (active_events, active_receiver) = async_channel::unbounded();
    let mut active = pending_turn(1, "/workspace", None);
    active.events = active_events;
    let (queued_events, queued_receiver) = async_channel::unbounded();
    let mut queued = pending_turn(2, "/workspace", None);
    queued.events = queued_events;
    let mut state = ConversationState::new(active);
    state.queued.push_back(queued);
    let conversations = HashMap::from([("conversation".to_owned(), state)]);

    fail_conversations(&conversations, "protocol dispatch failed");

    assert_eq!(
        active_receiver.try_recv(),
        Ok(AcpEvent::Error {
            message: "protocol dispatch failed".to_owned(),
        })
    );
    assert_eq!(
        queued_receiver.try_recv(),
        Ok(AcpEvent::Error {
            message: "protocol dispatch failed".to_owned(),
        })
    );
}

#[test]
fn steering_outcome_uses_the_codex_wire_values() {
    assert_eq!(
        serde_json::to_value(AcpSteeringOutcome::Injected).unwrap(),
        serde_json::json!("injected")
    );
    assert_eq!(
        serde_json::from_value::<AcpSteeringOutcome>(serde_json::json!("startedNewTurn")).unwrap(),
        AcpSteeringOutcome::StartedNewTurn
    );
}

#[test]
fn implicit_steering_turn_requires_immediate_teardown() {
    assert!(!steering_became_untracked(AcpSteeringOutcome::Injected));
    assert!(steering_became_untracked(
        AcpSteeringOutcome::StartedNewTurn
    ));
    assert!(!steering_became_untracked(AcpSteeringOutcome::Failed));
}

#[test]
fn untracked_steering_command_preserves_the_result_acknowledgement() {
    let (ack, acknowledgement) = oneshot::channel();
    let command = Command::AbortUntrackedSteering {
        conversation_key: "conversation-1".to_owned(),
        turn_id: 42,
        result: Ok(AcpSteeringOutcome::StartedNewTurn),
        ack,
    };
    let Command::AbortUntrackedSteering { result, ack, .. } = command else {
        panic!("expected an immediate untracked-steering abort");
    };
    let _ = ack.send(result);

    assert_eq!(
        futures::executor::block_on(acknowledgement)
            .unwrap()
            .unwrap(),
        AcpSteeringOutcome::StartedNewTurn
    );
}

struct PendingConnection {
    dropped: Arc<AtomicBool>,
}

impl Future for PendingConnection {
    type Output = Result<(), AcpRuntimeError>;

    fn poll(self: Pin<&mut Self>, _ctx: &mut Context<'_>) -> Poll<Self::Output> {
        Poll::Pending
    }
}

impl Drop for PendingConnection {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::Release);
    }
}

#[test]
fn initialization_timeout_cancels_the_connection_future() {
    let dropped = Arc::new(AtomicBool::new(false));
    let (_initialized_tx, initialized_rx) = oneshot::channel();
    let (_authenticated_tx, authenticated_rx) = oneshot::channel();
    let result = futures::executor::block_on(supervise_connection_readiness(
        PendingConnection {
            dropped: Arc::clone(&dropped),
        },
        initialized_rx,
        authenticated_rx,
        Duration::from_millis(10),
        Duration::from_secs(1),
    ));

    assert!(matches!(
        result,
        Err(AcpRuntimeError::InitializationTimeout(_))
    ));
    assert!(dropped.load(Ordering::Acquire));
}

#[test]
fn authentication_timeout_cancels_the_initialized_connection_future() {
    let dropped = Arc::new(AtomicBool::new(false));
    let (initialized_tx, initialized_rx) = oneshot::channel();
    let (_authenticated_tx, authenticated_rx) = oneshot::channel();
    let _ = initialized_tx.send(());
    let result = futures::executor::block_on(supervise_connection_readiness(
        PendingConnection {
            dropped: Arc::clone(&dropped),
        },
        initialized_rx,
        authenticated_rx,
        Duration::from_secs(1),
        Duration::from_millis(10),
    ));

    assert!(matches!(
        result,
        Err(AcpRuntimeError::AuthenticationTimeout(_))
    ));
    assert!(dropped.load(Ordering::Acquire));
}

#[cfg(unix)]
#[test]
fn hung_agent_initialization_closes_queued_turns_and_cancellation() {
    let manager = AcpSessionManager::spawn(
        AcpManagerConfig::new(AcpLaunchConfig::new("/bin/sh").args(["-c", "exec sleep 30"]))
            .initialization_timeout(Duration::from_millis(50)),
    )
    .unwrap();
    let (handle, events) = manager
        .run_turn(AcpTurnRequest::text(
            "conversation",
            PathBuf::from("/workspace"),
            "hello",
        ))
        .unwrap();

    let cancellation = futures::executor::block_on(async {
        match future::select(
            Box::pin(handle.cancel()),
            Box::pin(async_io::Timer::after(Duration::from_secs(2))),
        )
        .await
        {
            Either::Left((result, _)) => result,
            Either::Right((_, _)) => panic!("cancellation remained blocked after init timeout"),
        }
    });
    let event = futures::executor::block_on(async {
        match future::select(
            Box::pin(events.recv()),
            Box::pin(async_io::Timer::after(Duration::from_secs(2))),
        )
        .await
        {
            Either::Left((result, _)) => result.unwrap(),
            Either::Right((_, _)) => panic!("queued turn was not failed after init timeout"),
        }
    });

    assert!(matches!(
        cancellation,
        Err(AcpRuntimeError::RuntimeClosed(_))
    ));
    assert!(matches!(
        event,
        AcpEvent::Error { message } if message.contains("did not initialize")
    ));
    assert!(!manager.is_alive());
}

#[test]
fn manager_liveness_is_observable() {
    let (command_tx, _command_rx) = async_channel::unbounded();
    let manager = AcpSessionManager {
        inner: Arc::new(ManagerInner {
            command_tx,
            agent_info: Mutex::new(None),
            agent_capabilities: Mutex::new(None),
            launch: AcpLaunchConfig::new("agent"),
            alive: AtomicBool::new(true),
            terminal_error: Mutex::new(None),
        }),
    };

    assert!(manager.is_alive());
    manager.inner.alive.store(false, Ordering::Release);
    assert!(!manager.is_alive());
}

#[test]
fn process_tree_teardown_allows_supported_platforms() {
    assert!(validate_process_tree_teardown(true).is_ok());
}

#[test]
fn process_tree_teardown_fails_closed_on_unsupported_platforms() {
    assert!(matches!(
        validate_process_tree_teardown(false),
        Err(AcpRuntimeError::ProcessTreeTeardownUnsupported)
    ));
}

#[test]
fn dropping_the_last_manager_requests_worker_shutdown() {
    let (command_tx, command_rx) = async_channel::unbounded();
    let manager = AcpSessionManager {
        inner: Arc::new(ManagerInner {
            command_tx,
            agent_info: Mutex::new(None),
            agent_capabilities: Mutex::new(None),
            launch: AcpLaunchConfig::new("agent"),
            alive: AtomicBool::new(true),
            terminal_error: Mutex::new(None),
        }),
    };

    drop(manager);

    assert!(matches!(command_rx.try_recv(), Ok(Command::Shutdown)));
}

#[test]
fn session_handle_cancel_targets_its_exact_turn_and_waits_for_ack() {
    let (command_tx, command_rx) = async_channel::unbounded();
    let manager = AcpSessionManager {
        inner: Arc::new(ManagerInner {
            command_tx,
            agent_info: Mutex::new(None),
            agent_capabilities: Mutex::new(None),
            launch: AcpLaunchConfig::new("agent"),
            alive: AtomicBool::new(true),
            terminal_error: Mutex::new(None),
        }),
    };
    let handle = AcpSessionHandle {
        manager,
        conversation_key: "conversation-1".to_owned(),
        turn_id: 42,
    };

    let acknowledge = async move {
        let command = command_rx.recv().await.unwrap();
        let Command::Cancel {
            conversation_key,
            turn_id,
            ack,
        } = command
        else {
            panic!("cancel must not be translated into another command");
        };
        assert_eq!(conversation_key, "conversation-1");
        assert_eq!(turn_id, 42);
        let _ = ack.send(Ok(()));
    };
    let (result, ()) =
        futures::executor::block_on(futures::future::join(handle.cancel(), acknowledge));

    assert!(result.is_ok());
}

#[test]
fn steering_uses_its_typed_command_and_preserves_unsupported_error() {
    let (command_tx, command_rx) = async_channel::unbounded();
    let manager = AcpSessionManager {
        inner: Arc::new(ManagerInner {
            command_tx,
            agent_info: Mutex::new(None),
            agent_capabilities: Mutex::new(None),
            launch: AcpLaunchConfig::new("agent"),
            alive: AtomicBool::new(true),
            terminal_error: Mutex::new(None),
        }),
    };
    let handle = AcpSessionHandle {
        manager,
        conversation_key: "conversation-1".to_owned(),
        turn_id: 7,
    };
    let prompt = vec![ContentBlock::Text(TextContent::new("stop after this step"))];

    let respond = async move {
        let command = command_rx.recv().await.unwrap();
        let Command::Steer {
            conversation_key,
            turn_id,
            prompt,
            ack,
        } = command
        else {
            panic!("steering must never fall back to a concurrent prompt");
        };
        assert_eq!(conversation_key, "conversation-1");
        assert_eq!(turn_id, 7);
        assert_eq!(
            prompt,
            vec![ContentBlock::Text(TextContent::new("stop after this step"))]
        );
        let _ = ack.send(Err(AcpRuntimeError::SteeringUnsupported));
    };
    let (result, ()) =
        futures::executor::block_on(futures::future::join(handle.steer(prompt), respond));

    assert!(matches!(result, Err(AcpRuntimeError::SteeringUnsupported)));
}
