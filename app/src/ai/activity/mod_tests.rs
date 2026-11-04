use super::*;

fn recording_store() -> ActivityStore {
    let mut store = ActivityStore::default();
    store.set_recording(true);
    store
}

fn capture(store: &mut ActivityStore, context: Value, detail: String) {
    store.push_captured(
        store.capture_generation().unwrap(),
        "model_response",
        "Model completed".to_owned(),
        &context,
        detail,
        Utc::now(),
    );
}

#[test]
fn clear_and_pause_resume_invalidate_in_flight_captures() {
    let mut store = recording_store();
    let first_generation = store.capture_generation().unwrap();
    store.clear();
    store.push_captured(
        first_generation,
        "tool_result",
        "Late".to_owned(),
        &json!({}),
        "old".to_owned(),
        Utc::now(),
    );
    assert!(store.entries.is_empty());

    let second_generation = store.capture_generation().unwrap();
    store.set_recording(false);
    assert!(store.capture_generation().is_none());
    store.set_recording(true);
    store.push_captured(
        second_generation,
        "tool_result",
        "Late".to_owned(),
        &json!({}),
        "old".to_owned(),
        Utc::now(),
    );
    assert!(store.entries.is_empty());
    capture(&mut store, json!({}), "new".to_owned());
    assert_eq!(store.entries.len(), 1);
}

#[test]
fn retention_evicts_oldest_entries_by_count_and_allocated_size() {
    let mut store = recording_store();
    for _ in 0..MAX_ITEMS + 3 {
        capture(&mut store, json!({}), String::new());
    }
    assert_eq!(store.entries.len(), MAX_ITEMS);
    assert_eq!(store.dropped, 3);
    assert_eq!(store.entries.front().unwrap().id, 3);

    store.clear();
    let detail = "x".repeat(1024 * 1024);
    for _ in 0..20 {
        capture(&mut store, json!({}), detail.clone());
    }
    assert!(store.retained_bytes <= MAX_RETAINED_BYTES);
    assert_eq!(
        store.retained_bytes,
        store
            .entries
            .iter()
            .map(|entry| entry_size(entry))
            .sum::<usize>()
    );
    assert!(store.dropped > 0);
    // Metadata also consumes the budget, so sixteen 1 MiB payloads do not fit.
    assert!(store.entries.len() < 16);
}

#[test]
fn session_names_propagate_to_prior_entries_and_correlated_responses() {
    let mut store = recording_store();
    capture(
        &mut store,
        json!({"conversation_id": "session-12345", "stream_id": "stream-1"}),
        String::new(),
    );
    store.register_session("session-12345".to_owned(), "Inspect build".to_owned());
    let previous_snapshot = store.entries.front().unwrap().clone();
    capture(&mut store, json!({"stream_id": "stream-1"}), String::new());
    let response = store.entries.back().unwrap();
    assert_eq!(response.session_id.as_deref(), Some("session-12345"));
    assert_eq!(response.session_label, "Inspect build · session-");
    assert_eq!(previous_snapshot.session_label, response.session_label);

    let bytes_before_rename = store.retained_bytes;
    store.register_session(
        "session-12345".to_owned(),
        "Longer build inspection title".to_owned(),
    );
    assert!(store.retained_bytes > bytes_before_rename);
    assert_eq!(
        store.retained_bytes,
        store
            .entries
            .iter()
            .map(|entry| entry_size(entry))
            .sum::<usize>()
    );
}

#[test]
fn correlation_rejects_oversized_ids_and_preserves_explicit_ownership() {
    let mut store = recording_store();
    let overlong = "s".repeat(100_000);
    capture(
        &mut store,
        json!({"conversation_id": "session-a", "stream_id": overlong}),
        String::new(),
    );
    assert!(store.sessions.is_empty());
    capture(&mut store, json!({"stream_id": overlong}), String::new());
    assert!(store.entries.back().unwrap().session_id.is_none());

    capture(
        &mut store,
        json!({"conversation_id": "session-a", "request_id": "request-1"}),
        String::new(),
    );
    capture(
        &mut store,
        json!({"conversation_id": "session-b", "request_id": "request-1"}),
        String::new(),
    );
    assert_eq!(
        store.entries.back().unwrap().session_id.as_deref(),
        Some("session-b")
    );
    assert_eq!(
        session_id(&json!({"conversation_id": "", "tool": {"conversation_id": "owner"}})),
        Some("owner".to_owned())
    );
}

#[test]
fn risk_assessments_share_the_permissions_filter() {
    assert_eq!(category("tool_risk_assessment_finished"), "Permissions");
    assert_eq!(category("risk_review_request"), "Permissions");
    assert_eq!(category("provider_compaction_requested"), "Compaction");
    assert_eq!(
        category("shell_command_terminal_completed"),
        "Background jobs"
    );
}

#[test]
fn long_recordings_keep_accepting_new_session_and_request_identities() {
    let mut store = recording_store();
    for index in 0..MAX_LABELS + 2 {
        let owner = format!("session-{index}");
        store.register_session(owner.clone(), format!("Task {index}"));
        capture(
            &mut store,
            json!({"conversation_id": owner, "request_id": format!("request-{index}")}),
            String::new(),
        );
    }
    assert_eq!(store.labels.len(), MAX_LABELS);
    assert_eq!(store.label_order.len(), MAX_LABELS);
    assert_eq!(store.sessions.len(), MAX_LABELS);
    assert_eq!(store.correlation_order.len(), MAX_LABELS);
    assert!(!store.labels.contains_key("session-0"));
    assert!(!store.sessions.contains_key("request_id:request-0"));

    let latest = MAX_LABELS + 1;
    capture(
        &mut store,
        json!({"request_id": format!("request-{latest}")}),
        String::new(),
    );
    let response = store.entries.back().unwrap();
    assert_eq!(
        response.session_id.as_deref(),
        Some(format!("session-{latest}").as_str())
    );
    assert!(response
        .session_label
        .starts_with(&format!("Task {latest}")));
}
