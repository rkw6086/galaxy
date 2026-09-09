use serde_json::json;

use super::{normalize_endpoint_url, sanitize_error, tail_limited_payload_context};

#[test]
fn endpoint_accepts_base_or_logs_path() {
    assert_eq!(
        normalize_endpoint_url("https://logging.ryserve.net").as_deref(),
        Some("https://logging.ryserve.net/api/logs")
    );
    assert_eq!(
        normalize_endpoint_url("https://logging.ryserve.net/api/logs/").as_deref(),
        Some("https://logging.ryserve.net/api/logs")
    );
}

#[test]
fn errors_are_compacted_truncated_and_lightly_redacted() {
    let error = format!("failed\nAuthorization: Bearer sk-test {}", "x".repeat(700));
    let sanitized = sanitize_error(error);

    assert!(!sanitized.contains("sk-test"));
    assert!(!sanitized.contains('\n'));
    assert!(sanitized.chars().count() <= 501);
}

#[test]
fn raw_payload_cap_keeps_tail() {
    assert_eq!(
        tail_limited_payload_context("0123456789", 4),
        json!({
            "payload": "6789",
            "payload_total_chars": 10,
            "payload_included_chars": 4,
            "payload_max_chars": 4,
            "truncated": true,
            "truncation_strategy": "tail",
        })
    );
}
