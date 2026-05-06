use warp_multi_agent_api::{self as api, response_event::stream_finished};

use super::stream::*;

#[test]
fn test_build_stream_init_has_valid_ids() {
    let event = build_stream_init("req-123", "conv-456");

    match event.r#type {
        Some(api::response_event::Type::Init(init)) => {
            assert_eq!(init.request_id, "req-123");
            assert_eq!(init.conversation_id, "conv-456");
            assert_eq!(init.run_id, "");
        }
        other => panic!("Expected Init event, got {:?}", other),
    }
}

#[test]
fn test_build_stream_finished_done_reason() {
    let reason = stream_finished::Reason::Done(stream_finished::Done {});
    let event = build_stream_finished(reason, 100, 50);

    match event.r#type {
        Some(api::response_event::Type::Finished(finished)) => {
            assert!(matches!(
                finished.reason,
                Some(stream_finished::Reason::Done(_))
            ));
            assert!(!finished.should_refresh_model_config);
            let metadata = finished.conversation_usage_metadata.unwrap();
            assert_eq!(metadata.byok_token_usage.get("bedrock").unwrap().total_tokens, 150);
        }
        other => panic!("Expected Finished event, got {:?}", other),
    }
}

#[test]
fn test_build_stream_finished_max_token_limit() {
    let reason =
        stream_finished::Reason::MaxTokenLimit(stream_finished::ReachedMaxTokenLimit {});
    let event = build_stream_finished(reason, 200, 100);

    match event.r#type {
        Some(api::response_event::Type::Finished(finished)) => {
            assert!(matches!(
                finished.reason,
                Some(stream_finished::Reason::MaxTokenLimit(_))
            ));
        }
        other => panic!("Expected Finished event, got {:?}", other),
    }
}

#[test]
fn test_build_stream_finished_other_reason() {
    let reason = stream_finished::Reason::Other(stream_finished::Other {});
    let event = build_stream_finished(reason, 0, 0);

    match event.r#type {
        Some(api::response_event::Type::Finished(finished)) => {
            assert!(matches!(
                finished.reason,
                Some(stream_finished::Reason::Other(_))
            ));
        }
        other => panic!("Expected Finished event, got {:?}", other),
    }
}
