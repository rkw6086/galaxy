use prost::Message;

use super::*;
use crate::proto::{
    client_message, remote_skill_proto, server_message, session_scoped_request,
    BundledSkillMetadata, ClientMessage, HomeSkillMetadata, Initialize, InitializeResponse,
    RemoteAgentContextSnapshot, RemoteContextFileProto, RemoteSkillProto, ServerMessage,
};

#[tokio::test]
async fn background_job_requests_preserve_identity_and_byte_log_cursors() {
    let request = ClientMessage::host_scoped(
        "background-status".to_owned(),
        crate::proto::host_scoped_request::Message::BackgroundJob(
            crate::proto::BackgroundJobRequest {
                operation: crate::proto::background_job_request::Operation::Status as i32,
                job_id: "2ce65749-03d4-4482-910f-b9224575e9bd".to_owned(),
                conversation_id: "43131875-abf9-4f98-b20a-b607d1b99125".to_owned(),
                action_id: "job-action".to_owned(),
                log_offset: 128 * 1024 + 3,
                has_environment: true,
                ..Default::default()
            },
        ),
    );
    let mut buffer = Vec::new();
    write_message(&mut buffer, &request).await.unwrap();
    let decoded: ClientMessage = read_message(&mut buffer.as_slice()).await.unwrap();
    assert_eq!(decoded, request);

    let response = ServerMessage {
        request_id: request.request_id,
        message: Some(server_message::Message::BackgroundJobResponse(
            crate::proto::BackgroundJobResponse {
                job_json: "{}".to_owned(),
                log_chunk: vec![0xf0, 0x9f, 0x92, 0xa1, 0, 0xff],
                next_log_offset: 128 * 1024 + 9,
                total_log_bytes: 1024 * 1024,
                logs_available: true,
                error: String::new(),
                completion_log_tail: "retained completion output".as_bytes().to_vec(),
                completion_log_bytes: 1024 * 1024 + 7,
                completion_log_tail_available: true,
                job_not_found: false,
            },
        )),
    };
    let mut buffer = Vec::new();
    write_message(&mut buffer, &response).await.unwrap();
    let decoded: ServerMessage = read_message(&mut buffer.as_slice()).await.unwrap();
    assert_eq!(decoded, response);
}

#[tokio::test]
async fn round_trip_client_message() {
    let msg = ClientMessage::session_scoped(
        "test-123".to_string(),
        session_scoped_request::Message::Initialize(Initialize {
            auth_token: String::new(),
            user_id: String::new(),
            user_email: String::new(),
            crash_reporting_enabled: true,
            codebase_index_limits: None,
        }),
    );

    let mut buf = Vec::new();
    write_client_message(&mut buf, &msg).await.unwrap();

    let mut cursor = &buf[..];
    let decoded: ClientMessage = read_client_message(&mut cursor).await.unwrap();

    assert_eq!(decoded.request_id, "test-123");
    match decoded.message {
        Some(client_message::Message::SessionScoped(_)) => {}
        other => panic!("unexpected message variant: {other:?}"),
    }
}

#[tokio::test]
async fn round_trip_remote_agent_context_snapshot() {
    let mut buf = Vec::new();
    write_server_message(
        &mut buf,
        &ServerMessage {
            request_id: String::new(),
            message: Some(server_message::Message::RemoteAgentContextSnapshot(
                RemoteAgentContextSnapshot {
                    revision: 7,
                    home_dir: "/home/user".to_string(),
                    skills: vec![
                        RemoteSkillProto {
                            path: "/bundled/pr-comments/SKILL.md".to_string(),
                            content: "bundled content".to_string(),
                            source: Some(remote_skill_proto::Source::Bundled(
                                BundledSkillMetadata {
                                    id: "pr-comments".to_string(),
                                    requires_mcp: Some("figma".to_string()),
                                },
                            )),
                        },
                        RemoteSkillProto {
                            path: "/home/user/.agents/skills/test/SKILL.md".to_string(),
                            content: "home skill content".to_string(),
                            source: Some(remote_skill_proto::Source::Home(HomeSkillMetadata {})),
                        },
                    ],
                    global_rules: vec![RemoteContextFileProto {
                        path: "/home/user/.agents/AGENTS.md".to_string(),
                        content: "rule content".to_string(),
                    }],
                },
            )),
        },
    )
    .await
    .unwrap();

    let decoded = read_server_message(&mut &buf[..]).await.unwrap();
    match decoded.message {
        Some(server_message::Message::RemoteAgentContextSnapshot(snapshot)) => {
            assert_eq!(snapshot.revision, 7);
            assert_eq!(snapshot.home_dir, "/home/user");
            assert_eq!(snapshot.skills.len(), 2);
            let Some(remote_skill_proto::Source::Bundled(bundled)) =
                snapshot.skills[0].source.as_ref()
            else {
                panic!("expected bundled skill source");
            };
            assert_eq!(bundled.id, "pr-comments");
            assert_eq!(bundled.requires_mcp.as_deref(), Some("figma"));
            assert!(matches!(
                snapshot.skills[1].source,
                Some(remote_skill_proto::Source::Home(_))
            ));
            assert_eq!(snapshot.skills[1].content, "home skill content");
            assert_eq!(
                snapshot.global_rules[0].path,
                "/home/user/.agents/AGENTS.md"
            );
            assert_eq!(snapshot.global_rules[0].content, "rule content");
        }
        other => panic!("unexpected message variant: {other:?}"),
    }
}

#[tokio::test]
async fn round_trip_server_message() {
    let msg = ServerMessage {
        request_id: "resp-456".to_string(),
        message: Some(server_message::Message::InitializeResponse(
            InitializeResponse {
                server_version: "0.1.0".to_string(),
                host_id: "test-host".to_string(),
                supports_background_jobs: true,
            },
        )),
    };

    let mut buf = Vec::new();
    write_server_message(&mut buf, &msg).await.unwrap();

    let mut cursor = &buf[..];
    let decoded: ServerMessage = read_server_message(&mut cursor).await.unwrap();

    assert_eq!(decoded.request_id, "resp-456");
    match decoded.message {
        Some(server_message::Message::InitializeResponse(resp)) => {
            assert_eq!(resp.server_version, "0.1.0");
            assert_eq!(resp.host_id, "test-host");
        }
        other => panic!("unexpected message variant: {other:?}"),
    }
}

#[tokio::test]
async fn read_unexpected_eof_on_empty_input() {
    let mut cursor: &[u8] = &[];
    let result = read_client_message(&mut cursor).await;
    assert!(matches!(result, Err(ProtocolError::UnexpectedEof)));
}

#[tokio::test]
async fn read_truncated_payload() {
    // Write a length prefix claiming 100 bytes, but only provide 4.
    let mut buf = Vec::new();
    buf.extend_from_slice(&100u32.to_le_bytes());
    buf.extend_from_slice(&[0u8; 4]);

    let mut cursor = &buf[..];
    let result = read_client_message(&mut cursor).await;
    assert!(matches!(result, Err(ProtocolError::UnexpectedEof)));
}

#[tokio::test]
async fn round_trip_zero_length_message() {
    // A default ClientMessage with no fields set encodes to zero bytes.
    let msg = ClientMessage::default();

    let mut buf = Vec::new();
    write_client_message(&mut buf, &msg).await.unwrap();

    // The first 4 bytes should be the length (0).
    assert_eq!(&buf[..4], &0u32.to_le_bytes());

    let mut cursor = &buf[..];
    let decoded: ClientMessage = read_client_message(&mut cursor).await.unwrap();
    assert_eq!(decoded.request_id, "");
    assert!(decoded.message.is_none());
}

#[tokio::test]
async fn read_message_too_large() {
    // Write a length prefix exceeding MAX_MESSAGE_SIZE.
    let oversized_len = (MAX_MESSAGE_SIZE as u32) + 1;
    let buf = oversized_len.to_le_bytes();

    let mut cursor = &buf[..];
    let result = read_client_message(&mut cursor).await;
    assert!(matches!(result, Err(ProtocolError::MessageTooLarge { .. })));
}

#[tokio::test]
async fn write_message_too_large() {
    // Build a ClientMessage whose encoded size exceeds MAX_MESSAGE_SIZE.
    let msg = ClientMessage {
        request_id: "x".repeat(MAX_MESSAGE_SIZE + 1),
        message: None,
    };

    let mut buf = Vec::new();
    let result = write_client_message(&mut buf, &msg).await;
    assert!(matches!(result, Err(ProtocolError::MessageTooLarge { .. })));
    // Nothing should have been written to the stream.
    assert!(buf.is_empty());
}

#[test]
fn try_extract_request_id_from_valid_message() {
    let msg = ClientMessage::session_scoped(
        "abc-123".to_string(),
        session_scoped_request::Message::Initialize(Initialize {
            auth_token: String::new(),
            user_id: String::new(),
            user_email: String::new(),
            crash_reporting_enabled: true,
            codebase_index_limits: None,
        }),
    );
    let buf = msg.encode_to_vec();
    assert_eq!(try_extract_request_id(&buf), Some("abc-123".to_string()));
}

#[test]
fn try_extract_request_id_from_corrupted_payload_with_valid_id() {
    // Manually construct bytes: valid request_id field followed by
    // corrupt trailing bytes (unterminated varint that would crash
    // a full prost decode but doesn't affect our field-1 extraction).
    let mut buf = Vec::new();
    // Field 1 (string): tag=0x0a, length=7, "req-456"
    buf.push(0x0a);
    buf.push(7);
    buf.extend_from_slice(b"req-456");
    // Corrupt trailing bytes: unterminated varint (all continuation bits set).
    buf.extend_from_slice(&[0xFF, 0xFF, 0xFF, 0xFF]);

    // request_id should still be extractable despite trailing corruption.
    assert_eq!(try_extract_request_id(&buf), Some("req-456".to_string()));
}
#[test]
fn try_extract_request_id_from_empty_bytes() {
    assert_eq!(try_extract_request_id(&[]), None);
}

#[test]
fn try_extract_request_id_from_garbage_bytes() {
    // Completely random bytes that don't form a valid protobuf.
    // This may or may not decode depending on what prost makes of it,
    // but should not panic. If it decodes to an empty request_id, we
    // return None.
    let result = try_extract_request_id(&[0xFF, 0xFF, 0xFF, 0xFF]);
    // We don't assert a specific value — just that it doesn't panic.
    // If prost happens to decode something, it'll be empty or garbage.
    let _ = result;
}

#[tokio::test]
async fn decode_error_extracts_request_id() {
    // Construct a corrupted message with a valid request_id field.
    let mut payload = Vec::new();
    // Field 1 (string): tag=0x0a, length=6, "req-42"
    payload.push(0x0a);
    payload.push(6);
    payload.extend_from_slice(b"req-42");
    // Invalid trailing bytes that cause prost decode failure.
    payload.extend_from_slice(&[0x0F, 0x01]);

    let mut buf = Vec::new();
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(&payload);

    let mut cursor = &buf[..];
    let result = read_client_message(&mut cursor).await;
    match result {
        Err(ProtocolError::Decode(_, Some(id))) => {
            assert_eq!(id.to_string(), "req-42");
        }
        other => panic!("expected Decode error with request_id, got: {other:?}"),
    }
}

#[tokio::test]
async fn decode_error_none_when_no_request_id() {
    // Completely invalid protobuf bytes with no valid field 1.
    let garbage = vec![0xFF, 0xFE, 0xFD, 0xFC];
    let mut buf = Vec::new();
    buf.extend_from_slice(&(garbage.len() as u32).to_le_bytes());
    buf.extend_from_slice(&garbage);

    let mut cursor = &buf[..];
    let result = read_client_message(&mut cursor).await;
    match result {
        Err(ProtocolError::Decode(_, None)) => {}
        other => panic!("expected Decode error with None request_id, got: {other:?}"),
    }
}
