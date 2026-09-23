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

#[test]
fn streamed_payload_matches_existing_tail_for_unicode_and_split_writes() {
    let payload = "start \"escaped\" é日本🙂 ending".repeat(100);
    for limit in [0, 1, 3, 17, payload.chars().count(), payload.len()] {
        for chunk_size in [1, 2, 7, payload.len()] {
            let actual = super::bounded_payload_context(limit, |writer| {
                for chunk in payload.as_bytes().chunks(chunk_size) {
                    writer.write_all(chunk).unwrap();
                }
                Ok(())
            });
            assert_eq!(actual, tail_limited_payload_context(&payload, limit));
        }
    }
}

#[test]
fn streamed_json_matches_serialized_payload_without_materializing_the_archive() {
    let payload = json!({ "archive": ["large tool output".repeat(1000)], "input": "steer 🙂" });
    let expected = serde_json::to_string_pretty(&payload).unwrap();
    let actual = super::bounded_payload_context(100, |writer| {
        serde_json::to_writer_pretty(writer, &payload)
    });
    assert_eq!(actual, tail_limited_payload_context(&expected, 100));
}
