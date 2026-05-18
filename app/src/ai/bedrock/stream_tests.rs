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
    let event = build_stream_finished(reason, 100, 50, 20, 10);

    match event.r#type {
        Some(api::response_event::Type::Finished(finished)) => {
            assert!(matches!(
                finished.reason,
                Some(stream_finished::Reason::Done(_))
            ));
            assert!(!finished.should_refresh_model_config);
            let metadata = finished.conversation_usage_metadata.unwrap();
            assert_eq!(
                metadata
                    .byok_token_usage
                    .get("bedrock")
                    .unwrap()
                    .total_tokens,
                180
            );
            // Verify token_usage includes cache breakdown
            assert_eq!(finished.token_usage.len(), 1);
            let usage = &finished.token_usage[0];
            assert_eq!(usage.total_input, 100);
            assert_eq!(usage.output, 50);
            assert_eq!(usage.input_cache_read, 20);
            assert_eq!(usage.input_cache_write, 10);
            assert!(usage.cost_in_cents > 0.0);
        }
        other => panic!("Expected Finished event, got {:?}", other),
    }
}

#[test]
fn test_build_stream_finished_max_token_limit() {
    let reason = stream_finished::Reason::MaxTokenLimit(stream_finished::ReachedMaxTokenLimit {});
    let event = build_stream_finished(reason, 200, 100, 0, 0);

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
    let event = build_stream_finished(reason, 0, 0, 0, 0);

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

#[test]
fn test_build_create_task_event() {
    let event = super::stream::build_create_task("task-abc-123");

    match event.r#type {
        Some(api::response_event::Type::ClientActions(actions)) => {
            assert_eq!(actions.actions.len(), 1);
            match &actions.actions[0].action {
                Some(api::client_action::Action::CreateTask(create)) => {
                    let task = create.task.as_ref().unwrap();
                    assert_eq!(task.id, "task-abc-123");
                    assert!(task.messages.is_empty());
                    assert!(task.description.is_empty());
                    assert!(task.dependencies.is_none());
                }
                other => panic!("Expected CreateTask action, got {:?}", other),
            }
        }
        other => panic!("Expected ClientActions event, got {:?}", other),
    }
}

#[test]
fn test_build_create_task_has_no_parent() {
    let event = super::stream::build_create_task("root-task-id");

    if let Some(api::response_event::Type::ClientActions(actions)) = event.r#type {
        if let Some(api::client_action::Action::CreateTask(create)) = &actions.actions[0].action {
            let task = create.task.as_ref().unwrap();
            assert!(
                task.dependencies.is_none(),
                "Root task CreateTask must have no dependencies (no parent_id)"
            );
        }
    }
}
